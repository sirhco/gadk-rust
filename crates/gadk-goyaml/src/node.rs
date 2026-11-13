// Copyright 2026 Chris Olson
// Portions derived from gopkg.in/yaml.v3 (https://github.com/go-yaml/yaml), Copyright 2011-2019 Canonical Ltd.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! The node tree: yaml.v3 v3.0.1 `yaml.go`'s `Node`, `Kind` and `Style` (as an arena
//! [`Document`] of [`NodeData`] addressed by [`NodeId`]) with `IsZero`, `LongTag`, `ShortTag`,
//! `indicatedString` and `SetString`, and `decode.go`'s `parser` (the composer that builds the
//! tree from the event stream, incl. the `textless` mode).
//!
//! Go's `*Node` graph may share nodes (an alias points at its anchored node) and may contain
//! cycles; here identity is `NodeId` equality and an alias is `alias: Some(id)`.
// Composer bodies are Task 6's; Task 6 removes this line.
#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::collections::HashMap;
use std::io::Read;
use std::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, Index, IndexMut};

use crate::error::Error;
use crate::resolve::{MAP_TAG, NULL_TAG, SEQ_TAG, STR_TAG, long_tag, resolve, short_tag};
use crate::yamlh::{YamlEvent, YamlEventType, YamlParser};

/// The serde newtype-struct name through which a [`Document`] crosses the serde boundary
/// (the `serde_json::RawValue` technique): `ser.rs` and `de.rs` recognise it and hand the
/// subtree over through a thread-local slot instead of serializing it.
pub(crate) const DOCUMENT_MARKER: &str = "$gadk_goyaml::Document";

/// A node handle inside a [`Document`] (Go: a `*Node`; equality is Go's pointer identity).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub(crate) u32);

impl NodeId {
    /// The node's index in its document's arena.
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// Go's `yaml.Kind`: whether a node is a document, a mapping, a sequence, a scalar value, or
/// an alias to another node. Values are Go's (`DocumentNode = 1 << 0` …); `Zero` is Go's
/// `Kind(0)`, the kind of the zero `Node`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Go's zero kind (an unset node).
    #[default]
    Zero = 0,
    /// Go's `DocumentNode`.
    Document = 1,
    /// Go's `SequenceNode`.
    Sequence = 2,
    /// Go's `MappingNode`.
    Mapping = 4,
    /// Go's `ScalarNode`.
    Scalar = 8,
    /// Go's `AliasNode`.
    Alias = 16,
}

impl Kind {
    /// Go's numeric value (`%d` in `cannot encode node with unknown kind %d`).
    pub fn bits(self) -> u32 {
        self as u32
    }
}

/// Go's `yaml.Style` bit flags.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Style(u32);

impl Style {
    /// Go's `TaggedStyle`.
    pub const TAGGED: Style = Style(1);
    /// Go's `DoubleQuotedStyle`.
    pub const DOUBLE_QUOTED: Style = Style(2);
    /// Go's `SingleQuotedStyle`.
    pub const SINGLE_QUOTED: Style = Style(4);
    /// Go's `LiteralStyle`.
    pub const LITERAL: Style = Style(8);
    /// Go's `FoldedStyle`.
    pub const FOLDED: Style = Style(16);
    /// Go's `FlowStyle`.
    pub const FLOW: Style = Style(32);

    /// No flags (Go's `Style(0)`).
    pub const fn empty() -> Style {
        Style(0)
    }

    /// The raw bits.
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// A style from raw bits (any value, as Go's `Style(n)`).
    pub const fn from_bits(bits: u32) -> Style {
        Style(bits)
    }

    /// No flag is set.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Every flag of `other` is set.
    pub const fn contains(self, other: Style) -> bool {
        self.0 & other.0 == other.0
    }

    /// Some flag of `other` is set (Go's `s&other != 0`).
    pub const fn intersects(self, other: Style) -> bool {
        self.0 & other.0 != 0
    }
}

impl BitOr for Style {
    type Output = Style;
    fn bitor(self, rhs: Style) -> Style {
        Style(self.0 | rhs.0)
    }
}

impl BitOrAssign for Style {
    fn bitor_assign(&mut self, rhs: Style) {
        self.0 |= rhs.0;
    }
}

impl BitAnd for Style {
    type Output = Style;
    fn bitand(self, rhs: Style) -> Style {
        Style(self.0 & rhs.0)
    }
}

impl BitAndAssign for Style {
    fn bitand_assign(&mut self, rhs: Style) {
        self.0 &= rhs.0;
    }
}

/// One node: Go's `yaml.Node` exported fields. `alias` and `content` refer to nodes of the
/// same [`Document`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NodeData {
    /// Whether the node is a document, a mapping, a sequence, a scalar value, or an alias.
    pub kind: Kind,
    /// Customizes the appearance of the node in the tree.
    pub style: Style,
    /// The YAML tag defining the data type for the value (always the resolved tag on decode).
    pub tag: String,
    /// The unescaped and unquoted representation of the value.
    // Divergence (ledger 6c-1): a Go `string` may hold any bytes; parsed values are always
    // UTF-8 (the reader rejects invalid input), so only hand-built Go nodes differ.
    pub value: String,
    /// The anchor name for this node, which allows aliases to point to it.
    pub anchor: String,
    /// The node this alias points to (only for `Kind::Alias`).
    pub alias: Option<NodeId>,
    /// Contained nodes for documents, mappings, and sequences.
    pub content: Vec<NodeId>,
    /// Comments in the lines preceding the node and not separated by an empty line.
    pub head_comment: String,
    /// Comments at the end of the line where the node is in.
    pub line_comment: String,
    /// Comments following the node and before empty lines.
    pub foot_comment: String,
    /// The node's line in the decoded YAML text (1-based; 0 when unknown).
    pub line: usize,
    /// The node's column in the decoded YAML text (1-based; 0 when unknown).
    pub column: usize,
}

impl NodeData {
    /// Go's `(*Node).IsZero`: whether the node has all of its fields unset.
    pub fn is_zero(&self) -> bool {
        // Go: yaml.go IsZero
        // Divergence (ledger 6c-1): Go tells a nil `Content` from an empty non-nil one; an
        // empty `Vec` is Go's nil.
        self.kind == Kind::Zero
            && self.style.is_empty()
            && self.tag.is_empty()
            && self.value.is_empty()
            && self.anchor.is_empty()
            && self.alias.is_none()
            && self.content.is_empty()
            && self.head_comment.is_empty()
            && self.line_comment.is_empty()
            && self.foot_comment.is_empty()
            && self.line == 0
            && self.column == 0
    }

    /// Go's `(*Node).indicatedString`: a scalar explicitly tagged `!!str`, or untagged and
    /// quoted/literal/folded.
    pub(crate) fn indicated_string(&self) -> bool {
        // Go: yaml.go indicatedString
        self.kind == Kind::Scalar
            && (short_tag(&self.tag) == STR_TAG
                || (self.tag.is_empty() || self.tag == "!")
                    && self.style.intersects(
                        Style::SINGLE_QUOTED
                            | Style::DOUBLE_QUOTED
                            | Style::LITERAL
                            | Style::FOLDED,
                    ))
    }

    /// Go's `(*Node).SetString`: make the node a `!!str` scalar holding `s`, literal style when
    /// it contains a newline.
    pub fn set_string(&mut self, s: &str) {
        // Go: yaml.go SetString
        // Divergence (ledger 6c-1): Go base64-encodes a string that is not valid UTF-8 as
        // `!!binary`; a Rust `&str` is always valid UTF-8, so that branch cannot occur.
        self.kind = Kind::Scalar;
        self.value = s.to_string();
        self.tag = STR_TAG.to_string();
        if self.value.contains('\n') {
            self.style = Style::LITERAL;
        }
    }
}

/// An arena of nodes with a root: what Go's `yaml.Unmarshal(data, &node)` fills (`root` is the
/// `DocumentNode`) or any detached subtree (`root` of another kind).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Document {
    pub(crate) nodes: Vec<NodeData>,
    pub(crate) root: NodeId,
}

impl Default for Document {
    /// A document whose root is the zero node (Go's `var node yaml.Node`).
    fn default() -> Self {
        Document::new(NodeData::default())
    }
}

impl Document {
    /// A document holding only `root`.
    pub fn new(root: NodeData) -> Document {
        Document {
            nodes: vec![root],
            root: NodeId(0),
        }
    }

    /// The root node.
    pub fn root(&self) -> NodeId {
        self.root
    }

    /// Make `id` the root.
    pub fn set_root(&mut self, id: NodeId) {
        self.root = id;
    }

    /// Add a node to the arena (not linked anywhere yet) and return its id.
    pub fn push(&mut self, node: NodeData) -> NodeId {
        let id = NodeId(u32::try_from(self.nodes.len()).unwrap_or(u32::MAX));
        self.nodes.push(node);
        id
    }

    /// Mutable access to a node (same as `&mut doc[id]`).
    pub fn get_mut(&mut self, id: NodeId) -> &mut NodeData {
        &mut self.nodes[id.index()]
    }

    /// The number of nodes in the arena (reachable or not).
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the arena is empty (never true for a constructed `Document`).
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Go's `(*Node).ShortTag`: the short form of the tag, computed from the node's properties
    /// when `Tag` is unset (`!!map`, `!!seq`, the alias target's, the resolved scalar tag, or
    /// `!!null` for the zero node).
    pub fn short_tag(&self, id: NodeId) -> String {
        // Go: yaml.go ShortTag
        let n = &self[id];
        if n.indicated_string() {
            return STR_TAG.to_string();
        }
        if n.tag.is_empty() || n.tag == "!" {
            match n.kind {
                Kind::Mapping => return MAP_TAG.to_string(),
                Kind::Sequence => return SEQ_TAG.to_string(),
                Kind::Alias => {
                    if let Some(target) = n.alias {
                        return self.short_tag(target);
                    }
                }
                Kind::Scalar => {
                    // resolve with an empty tag never fails.
                    return match resolve("", &n.value) {
                        Ok((tag, _)) => tag,
                        Err(_) => String::new(),
                    };
                }
                Kind::Zero => {
                    // Special case to make the zero value convenient.
                    if n.is_zero() {
                        return NULL_TAG.to_string();
                    }
                }
                Kind::Document => {}
            }
            return String::new();
        }
        short_tag(&n.tag)
    }

    /// Go's `(*Node).LongTag`: the long form of [`Document::short_tag`].
    pub fn long_tag(&self, id: NodeId) -> String {
        // Go: yaml.go LongTag
        long_tag(&self.short_tag(id))
    }

    /// A new document holding a deep copy of the subtree at `at` (content and alias targets,
    /// followed transitively, identity and sharing preserved); its root is the copy of `at`.
    /// This is how a Go `*Node` subtree becomes a value of its own (`*n = *doc.Content[0]`,
    /// `yaml.Node` struct fields).
    pub fn subtree(&self, at: NodeId) -> Document {
        let mut out = Document {
            nodes: Vec::new(),
            root: NodeId(0),
        };
        let mut map: HashMap<NodeId, NodeId> = HashMap::new();
        let mut stack = vec![at];
        // First pass: assign new ids in pre-order.
        while let Some(id) = stack.pop() {
            if map.contains_key(&id) {
                continue;
            }
            let new_id = out.push(self[id].clone());
            map.insert(id, new_id);
            let n = &self[id];
            if let Some(a) = n.alias {
                stack.push(a);
            }
            for &c in n.content.iter().rev() {
                stack.push(c);
            }
        }
        // Second pass: rewrite references.
        for n in &mut out.nodes {
            n.alias = n.alias.and_then(|a| map.get(&a).copied());
            for c in &mut n.content {
                if let Some(&m) = map.get(c) {
                    *c = m;
                }
            }
        }
        out.root = map.get(&at).copied().unwrap_or(NodeId(0));
        out
    }
}

impl Index<NodeId> for Document {
    type Output = NodeData;
    fn index(&self, id: NodeId) -> &NodeData {
        &self.nodes[id.index()]
    }
}

impl IndexMut<NodeId> for Document {
    fn index_mut(&mut self, id: NodeId) -> &mut NodeData {
        &mut self.nodes[id.index()]
    }
}

// ---------------------------------------------------------------------------------------------
// decode.go: Parser, produces a node tree out of a libyaml event stream.
// ---------------------------------------------------------------------------------------------

/// Go: decode.go `parser`. Every node it builds lives in `arena` (Go's heap), so anchors stay
/// valid across the documents of a stream as in Go (`anchors` is created once, in `init`).
pub(crate) struct Parser<'r> {
    /// The libyaml parser.
    pub(crate) parser: YamlParser<'r>,
    /// The current event (`YamlEventType::No` when consumed).
    pub(crate) event: YamlEvent,
    /// Go's `doc`: the document node being parsed.
    pub(crate) doc: Option<NodeId>,
    /// Holds every node built so far.
    pub(crate) arena: Document,
    /// Anchor name → anchored node.
    pub(crate) anchors: HashMap<String, NodeId>,
    /// Whether `init` ran.
    pub(crate) done_init: bool,
    /// Skip line/column/comments (`Node.Encode`'s re-parse).
    pub(crate) textless: bool,
}

/// Go: decode.go `newParser` (empty input is parsed as `"\n"`).
pub(crate) fn new_parser(b: &[u8]) -> Parser<'_> {
    // Go: decode.go newParser
    todo!("Task 6")
}

/// Go: decode.go `newParserFromReader`.
pub(crate) fn new_parser_from_reader<'r, R: Read + 'r>(r: R) -> Parser<'r> {
    // Go: decode.go newParserFromReader
    todo!("Task 6")
}

impl<'r> Parser<'r> {
    /// Go: `(*parser).init`.
    pub(crate) fn init(&mut self) -> Result<(), Error> {
        // Go: decode.go init
        todo!("Task 6")
    }

    /// Go: `(*parser).destroy`.
    pub(crate) fn destroy(&mut self) {
        // Go: decode.go destroy
        todo!("Task 6")
    }

    /// Go: `(*parser).expect` — consume an event of type `e`.
    pub(crate) fn expect(&mut self, e: YamlEventType) -> Result<(), Error> {
        // Go: decode.go expect
        todo!("Task 6")
    }

    /// Go: `(*parser).peek` — the next event's type, parsing it into `event` if needed.
    pub(crate) fn peek(&mut self) -> Result<YamlEventType, Error> {
        // Go: decode.go peek
        todo!("Task 6")
    }

    /// Go: `(*parser).fail` — the error for the libyaml parser's current problem
    /// (`yaml: line N: …`).
    pub(crate) fn fail(&self) -> Error {
        // Go: decode.go fail
        todo!("Task 6")
    }

    /// Go: `(*parser).anchor` — record `anchor` (empty = Go's nil) for node `n`.
    pub(crate) fn anchor(&mut self, n: NodeId, anchor: Vec<u8>) {
        // Go: decode.go anchor
        todo!("Task 6")
    }

    /// Go: `(*parser).parse` — the next node, `None` at the end of the stream.
    pub(crate) fn parse(&mut self) -> Result<Option<NodeId>, Error> {
        // Go: decode.go parse
        todo!("Task 6")
    }

    /// Go: `(*parser).node` — a new node from the current event.
    pub(crate) fn node(
        &mut self,
        kind: Kind,
        default_tag: &str,
        tag: &str,
        value: &str,
    ) -> Result<NodeId, Error> {
        // Go: decode.go node
        todo!("Task 6")
    }

    /// Go: `(*parser).parseChild` — parse a node and append it to `parent`'s content.
    pub(crate) fn parse_child(&mut self, parent: NodeId) -> Result<NodeId, Error> {
        // Go: decode.go parseChild
        todo!("Task 6")
    }

    /// Go: `(*parser).document`.
    pub(crate) fn document(&mut self) -> Result<NodeId, Error> {
        // Go: decode.go document
        todo!("Task 6")
    }

    /// Go: `(*parser).alias`.
    pub(crate) fn alias(&mut self) -> Result<NodeId, Error> {
        // Go: decode.go alias
        todo!("Task 6")
    }

    /// Go: `(*parser).scalar`.
    pub(crate) fn scalar(&mut self) -> Result<NodeId, Error> {
        // Go: decode.go scalar
        todo!("Task 6")
    }

    /// Go: `(*parser).sequence`.
    pub(crate) fn sequence(&mut self) -> Result<NodeId, Error> {
        // Go: decode.go sequence
        todo!("Task 6")
    }

    /// Go: `(*parser).mapping`.
    pub(crate) fn mapping(&mut self) -> Result<NodeId, Error> {
        // Go: decode.go mapping
        todo!("Task 6")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scalar(v: &str) -> NodeData {
        NodeData {
            kind: Kind::Scalar,
            value: v.to_string(),
            ..NodeData::default()
        }
    }

    #[test]
    fn kind_and_style_values_match_go() {
        assert_eq!(
            [
                Kind::Zero,
                Kind::Document,
                Kind::Sequence,
                Kind::Mapping,
                Kind::Scalar,
                Kind::Alias
            ]
            .map(Kind::bits),
            [0, 1, 2, 4, 8, 16]
        );
        assert_eq!(
            [
                Style::TAGGED,
                Style::DOUBLE_QUOTED,
                Style::SINGLE_QUOTED,
                Style::LITERAL,
                Style::FOLDED,
                Style::FLOW
            ]
            .map(Style::bits),
            [1, 2, 4, 8, 16, 32]
        );
        let mut s = Style::TAGGED | Style::FLOW;
        assert!(s.contains(Style::FLOW) && s.intersects(Style::FLOW | Style::LITERAL));
        assert!(!s.contains(Style::FLOW | Style::LITERAL));
        s &= Style::FLOW;
        assert_eq!(s, Style::FLOW);
        s |= Style::LITERAL;
        assert_eq!((s & Style::LITERAL).bits(), 8);
        assert!(Style::empty().is_empty() && Style::from_bits(0x20) == Style::FLOW);
    }

    #[test]
    fn zero_node() {
        assert!(NodeData::default().is_zero());
        for n in [
            NodeData {
                kind: Kind::Scalar,
                ..NodeData::default()
            },
            NodeData {
                style: Style::FLOW,
                ..NodeData::default()
            },
            NodeData {
                tag: "!!str".into(),
                ..NodeData::default()
            },
            NodeData {
                value: "x".into(),
                ..NodeData::default()
            },
            NodeData {
                anchor: "a".into(),
                ..NodeData::default()
            },
            NodeData {
                alias: Some(NodeId(0)),
                ..NodeData::default()
            },
            NodeData {
                content: vec![NodeId(0)],
                ..NodeData::default()
            },
            NodeData {
                head_comment: "#".into(),
                ..NodeData::default()
            },
            NodeData {
                line_comment: "#".into(),
                ..NodeData::default()
            },
            NodeData {
                foot_comment: "#".into(),
                ..NodeData::default()
            },
            NodeData {
                line: 1,
                ..NodeData::default()
            },
            NodeData {
                column: 1,
                ..NodeData::default()
            },
        ] {
            assert!(!n.is_zero(), "{n:?}");
        }
        let doc = Document::default();
        assert_eq!(doc.len(), 1);
        assert!(!doc.is_empty());
        assert!(doc[doc.root()].is_zero());
    }

    #[test]
    fn set_string() {
        let mut n = NodeData {
            kind: Kind::Mapping,
            style: Style::FLOW,
            ..NodeData::default()
        };
        n.set_string("one line");
        assert_eq!(
            (n.kind, n.tag.as_str(), n.value.as_str()),
            (Kind::Scalar, "!!str", "one line")
        );
        assert_eq!(n.style, Style::FLOW); // Go only sets the style for multi-line text.
        n.set_string("a\nb");
        assert_eq!(n.style, Style::LITERAL);
    }

    #[test]
    fn push_index_and_mutate() {
        let mut doc = Document::new(NodeData {
            kind: Kind::Sequence,
            ..NodeData::default()
        });
        let a = doc.push(scalar("a"));
        let b = doc.push(scalar("b"));
        let root = doc.root();
        doc[root].content = vec![a, b];
        doc.get_mut(b).value = "B".into();
        assert_eq!((a.index(), b.index()), (1, 2));
        assert_eq!(doc[doc[root].content[1]].value, "B");
        doc.set_root(a);
        assert_eq!(doc.root(), a);
    }

    #[test]
    fn subtree_copies_reachable_nodes_and_keeps_sharing() {
        // root: mapping { k: &x seq [s], k2: *x } plus an unreachable node.
        let mut doc = Document::new(NodeData {
            kind: Kind::Document,
            ..NodeData::default()
        });
        let junk = doc.push(scalar("junk"));
        let map = doc.push(NodeData {
            kind: Kind::Mapping,
            ..NodeData::default()
        });
        let k = doc.push(scalar("k"));
        let s = doc.push(scalar("s"));
        let seq = doc.push(NodeData {
            kind: Kind::Sequence,
            anchor: "x".into(),
            content: vec![s],
            ..NodeData::default()
        });
        let k2 = doc.push(scalar("k2"));
        let al = doc.push(NodeData {
            kind: Kind::Alias,
            value: "x".into(),
            alias: Some(seq),
            ..NodeData::default()
        });
        doc[map].content = vec![k, seq, k2, al];
        let root = doc.root();
        doc[root].content = vec![map];
        let _ = junk;

        let sub = doc.subtree(map);
        assert_eq!(sub.len(), 6);
        let m = &sub[sub.root()];
        assert_eq!(m.kind, Kind::Mapping);
        let vals: Vec<&str> = m.content.iter().map(|&id| sub[id].value.as_str()).collect();
        assert_eq!(vals, ["k", "", "k2", "x"]);
        let seq_copy = m.content[1];
        assert_eq!(sub[m.content[3]].alias, Some(seq_copy));
        assert_eq!(sub[sub[seq_copy].content[0]].value, "s");

        // An alias whose target lies outside the subtree brings the target along.
        let only_alias = doc.subtree(al);
        assert_eq!(only_alias.len(), 3);
        let target = only_alias[only_alias.root()].alias.expect("alias kept");
        assert_eq!(only_alias[target].anchor, "x");

        // Self-containing structures terminate.
        let mut cyc = Document::new(NodeData {
            kind: Kind::Sequence,
            ..NodeData::default()
        });
        let r = cyc.root();
        let back = cyc.push(NodeData {
            kind: Kind::Alias,
            alias: Some(r),
            ..NodeData::default()
        });
        cyc[r].content = vec![back];
        let c = cyc.subtree(r);
        assert_eq!(c.len(), 2);
        assert_eq!(c[c[c.root()].content[0]].alias, Some(c.root()));
    }
}
