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

//! The node tree: yaml.v3's node, kind and style (as an arena [`Document`] of [`NodeData`]
//! addressed by [`NodeId`]) with the zero check, long/short tags, indicated-string check and
//! `set_string`, and the composer that builds the tree from the event stream (incl. the
//! `textless` mode).
//!
//! A node graph may share nodes (an alias points at its anchored node) and may contain cycles;
//! here identity is `NodeId` equality and an alias is `alias: Some(id)`.

use std::collections::HashMap;
use std::io::Read;
use std::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, Index, IndexMut};

use crate::api::{
    yaml_event_delete, yaml_parser_delete, yaml_parser_initialize, yaml_parser_set_input_reader,
    yaml_parser_set_input_string,
};
use crate::error::{Error, failf};
use crate::parser::yaml_parser_parse;
use crate::resolve::{
    MAP_TAG, MERGE_TAG, NULL_TAG, SEQ_TAG, STR_TAG, long_tag, resolve, short_tag,
};
use crate::yamlh::{
    YamlErrorType, YamlEvent, YamlEventType, YamlMappingStyle, YamlParser, YamlScalarStyle,
    YamlSequenceStyle, YamlStyle,
};

/// The serde newtype-struct name through which a [`Document`] crosses the serde boundary
/// (the `serde_json::RawValue` technique): `ser.rs` and `de.rs` recognise it and hand the
/// subtree over through a thread-local slot instead of serializing it.
pub(crate) const DOCUMENT_MARKER: &str = "$gadk_goyaml::Document";

/// A node handle inside a [`Document`] (equality is node identity).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub(crate) u32);

impl NodeId {
    /// The node's index in its document's arena.
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// Whether a node is a document, a mapping, a sequence, a scalar value, or an alias to another
/// node. The numeric values match yaml.v3's (`Document = 1`, `Sequence = 2`, …); `Zero` is
/// kind 0, the kind of the zero node.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Kind {
    /// An unset node.
    #[default]
    Zero = 0,
    /// A document.
    Document = 1,
    /// A sequence.
    Sequence = 2,
    /// A mapping.
    Mapping = 4,
    /// A scalar.
    Scalar = 8,
    /// An alias to another node.
    Alias = 16,
}

impl Kind {
    /// The numeric value (`%d` in `cannot encode node with unknown kind %d`).
    pub fn bits(self) -> u32 {
        self as u32
    }
}

/// Node style bit flags.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Style(u32);

impl Style {
    /// The tag is written explicitly.
    pub const TAGGED: Style = Style(1);
    /// Double-quoted scalar.
    pub const DOUBLE_QUOTED: Style = Style(2);
    /// Single-quoted scalar.
    pub const SINGLE_QUOTED: Style = Style(4);
    /// Literal block scalar (`|`).
    pub const LITERAL: Style = Style(8);
    /// Folded block scalar (`>`).
    pub const FOLDED: Style = Style(16);
    /// Flow collection (`[...]` / `{...}`).
    pub const FLOW: Style = Style(32);

    /// No flags.
    pub const fn empty() -> Style {
        Style(0)
    }

    /// The raw bits.
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// A style from raw bits (any value).
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

    /// Some flag of `other` is set (`self & other != 0`).
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

/// One node: its kind, style, tag, value, comments, position and links. `alias` and
/// `content` refer to nodes of the same [`Document`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NodeData {
    /// Whether the node is a document, a mapping, a sequence, a scalar value, or an alias.
    pub kind: Kind,
    /// Customizes the appearance of the node in the tree.
    pub style: Style,
    /// The YAML tag defining the data type for the value (always the resolved tag on decode).
    pub tag: String,
    /// The unescaped and unquoted representation of the value.
    // A string may hold any bytes in yaml.v3, but parsed values are always UTF-8 (the reader
    // rejects invalid input) and a Rust `String` must be UTF-8.
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
    /// Whether the node has all of its fields unset.
    pub fn is_zero(&self) -> bool {
        // An empty `content` counts as unset (there is no distinction between an absent and an
        // empty list).
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

    /// Whether the node is a scalar explicitly tagged `!!str`, or untagged and
    /// quoted/literal/folded.
    pub(crate) fn indicated_string(&self) -> bool {
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

    /// Makes the node a `!!str` scalar holding `s`, literal style when
    /// it contains a newline.
    pub fn set_string(&mut self, s: &str) {
        // There is no `!!binary` base64 branch for a string that is not valid UTF-8: a Rust
        // `&str` is always valid UTF-8.
        self.kind = Kind::Scalar;
        self.value = s.to_string();
        self.tag = STR_TAG.to_string();
        if self.value.contains('\n') {
            self.style = Style::LITERAL;
        }
    }
}

/// An arena of nodes with a root: what parsing fills (`root` is the
/// document node) or any detached subtree (`root` of another kind).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Document {
    pub(crate) nodes: Vec<NodeData>,
    pub(crate) root: NodeId,
}

impl Default for Document {
    /// A document whose root is the zero node (an unset node).
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

    /// The short form of the tag, computed from the node's properties
    /// when `Tag` is unset (`!!map`, `!!seq`, the alias target's, the resolved scalar tag, or
    /// `!!null` for the zero node).
    pub fn short_tag(&self, id: NodeId) -> String {
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

    /// The long form of [`Document::short_tag`].
    pub fn long_tag(&self, id: NodeId) -> String {
        long_tag(&self.short_tag(id))
    }

    /// A new document holding a deep copy of the subtree at `at` (content and alias targets,
    /// followed transitively, identity and sharing preserved); its root is the copy of `at`.
    /// This is how a subtree becomes a value of its own (for example a node held in a
    /// struct field).
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
// Parser: produces a node tree out of a libyaml event stream.
// ---------------------------------------------------------------------------------------------

/// The composer. Every node it builds lives in `arena`, so anchors stay valid across the
/// documents of a stream (`anchors` is created once, in `init`).
pub(crate) struct Parser<'r> {
    /// The libyaml parser.
    pub(crate) parser: YamlParser<'r>,
    /// The current event (`YamlEventType::No` when consumed).
    pub(crate) event: YamlEvent,
    /// The document node being parsed (nothing reads it).
    #[allow(dead_code)] // written but never read
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

/// A collection the composer has opened and not yet closed: the explicit-stack form of
/// the recursive document / sequence / mapping parsing (see [`Parser::compose`]).
pub(crate) enum Frame {
    /// A document node awaiting its single child.
    Document(NodeId),
    /// A sequence node.
    Sequence(NodeId),
    /// A mapping node; `key` is the key of the pair whose value is being parsed.
    Mapping {
        n: NodeId,
        block: bool,
        key: Option<NodeId>,
    },
}

/// Event bytes as a string. The reader only lets valid UTF-8 through and the scanner
/// writes escapes as UTF-8, so this is lossless except for a tag URI whose `%`-escapes pass
/// the scanner's octet checks without being UTF-8 (e.g. an encoded surrogate).
// Such a tag's invalid sequences become U+FFFD (yaml.v3 keeps the raw bytes).
fn go_string(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// A parser around an initialized libyaml parser (set up with
/// `yaml_parser_initialize`).
fn new_parser_state<'r>() -> Parser<'r> {
    let mut p = Parser {
        parser: YamlParser::default(),
        event: YamlEvent::default(),
        doc: None,
        arena: Document {
            nodes: Vec::new(),
            root: NodeId(0),
        },
        anchors: HashMap::new(),
        done_init: false,
        textless: false,
    };
    if !yaml_parser_initialize(&mut p.parser) {
        panic!("failed to initialize YAML emitter");
    }
    p
}

/// A parser over `b` (empty input is parsed as `"\n"`).
pub(crate) fn new_parser(b: &[u8]) -> Parser<'_> {
    let mut p = new_parser_state();
    let b: &[u8] = if b.is_empty() { b"\n" } else { b };
    yaml_parser_set_input_string(&mut p.parser, b);
    p
}

/// A parser reading from `r`.
pub(crate) fn new_parser_from_reader<'r, R: Read + 'r>(r: R) -> Parser<'r> {
    let mut p = new_parser_state();
    yaml_parser_set_input_reader(&mut p.parser, Box::new(r));
    p
}

impl<'r> Parser<'r> {
    /// Initializes the parser (once).
    pub(crate) fn init(&mut self) -> Result<(), Error> {
        if self.done_init {
            return Ok(());
        }
        self.anchors = HashMap::new();
        self.expect(YamlEventType::StreamStart)?;
        self.done_init = true;
        Ok(())
    }

    /// Releases the libyaml parser.
    pub(crate) fn destroy(&mut self) {
        if self.event.typ != YamlEventType::No {
            yaml_event_delete(&mut self.event);
        }
        yaml_parser_delete(&mut self.parser);
    }

    /// Consume an event of type `e`.
    pub(crate) fn expect(&mut self, e: YamlEventType) -> Result<(), Error> {
        if self.event.typ == YamlEventType::No
            && !yaml_parser_parse(&mut self.parser, &mut self.event)
        {
            return Err(self.fail());
        }
        if self.event.typ == YamlEventType::StreamEnd {
            return Err(failf(
                "attempted to go past the end of stream; corrupted value?",
            ));
        }
        if self.event.typ != e {
            self.parser.problem = format!("expected {} event but got {}", e, self.event.typ);
            return Err(self.fail());
        }
        yaml_event_delete(&mut self.event);
        self.event.typ = YamlEventType::No;
        Ok(())
    }

    /// The next event's type, parsing it into `event` if needed.
    pub(crate) fn peek(&mut self) -> Result<YamlEventType, Error> {
        if self.event.typ != YamlEventType::No {
            return Ok(self.event.typ);
        }
        // It's curious choice from the underlying API to generally return a
        // positive result on success, but on this case return true in an error
        // scenario. This was the source of bugs in the past (issue #666).
        if !yaml_parser_parse(&mut self.parser, &mut self.event)
            || self.parser.error != YamlErrorType::No
        {
            return Err(self.fail());
        }
        Ok(self.event.typ)
    }

    /// The error for the libyaml parser's current problem
    /// (`yaml: line N: …`).
    pub(crate) fn fail(&self) -> Error {
        let mut line = 0;
        if self.parser.context_mark.line != 0 {
            line = self.parser.context_mark.line;
            // Scanner errors don't iterate line before returning error
            if self.parser.error == YamlErrorType::Scanner {
                line += 1;
            }
        } else if self.parser.problem_mark.line != 0 {
            line = self.parser.problem_mark.line;
            // Scanner errors don't iterate line before returning error
            if self.parser.error == YamlErrorType::Scanner {
                line += 1;
            }
        }
        let where_ = if line != 0 {
            format!("line {line}: ")
        } else {
            String::new()
        };
        let msg = if !self.parser.problem.is_empty() {
            self.parser.problem.as_str()
        } else {
            "unknown problem parsing YAML content"
        };
        failf(format_args!("{where_}{msg}"))
    }

    /// Record `anchor` (empty = none) for node `n`.
    pub(crate) fn anchor(&mut self, n: NodeId, anchor: Vec<u8>) {
        // An ANCHOR token's value is never empty.
        if !anchor.is_empty() {
            let name = go_string(&anchor);
            self.arena[n].anchor = name.clone();
            self.anchors.insert(name, n);
        }
    }

    /// The next node, `None` at the end of the stream.
    pub(crate) fn parse(&mut self) -> Result<Option<NodeId>, Error> {
        self.init()?;
        match self.peek()? {
            YamlEventType::Scalar => self.scalar().map(Some),
            YamlEventType::Alias => self.alias().map(Some),
            YamlEventType::MappingStart => {
                let frame = self.mapping()?;
                self.compose(frame).map(Some)
            }
            YamlEventType::SequenceStart => {
                let frame = self.sequence()?;
                self.compose(frame).map(Some)
            }
            YamlEventType::DocumentStart => {
                let frame = self.document()?;
                self.compose(frame).map(Some)
            }
            YamlEventType::StreamEnd => {
                // Happens when attempting to decode an empty buffer.
                Ok(None)
            }
            YamlEventType::TailComment => {
                panic!("internal error: unexpected tail comment event (please report)")
            }
            other => {
                panic!("internal error: attempted to parse unknown event (please report): {other}")
            }
        }
    }

    /// A new node from the current event.
    pub(crate) fn node(
        &mut self,
        kind: Kind,
        default_tag: &str,
        tag: &str,
        value: &str,
    ) -> Result<NodeId, Error> {
        let mut style = Style::empty();
        let tag = if !tag.is_empty() && tag != "!" {
            style = Style::TAGGED;
            short_tag(tag)
        } else if !default_tag.is_empty() {
            default_tag.to_string()
        } else if kind == Kind::Scalar {
            resolve("", value)?.0
        } else {
            tag.to_string()
        };
        let mut n = NodeData {
            kind,
            tag,
            value: value.to_string(),
            style,
            ..NodeData::default()
        };
        if !self.textless {
            n.line = self.event.start_mark.line + 1;
            n.column = self.event.start_mark.column + 1;
            n.head_comment = go_string(&self.event.head_comment);
            n.line_comment = go_string(&self.event.line_comment);
            n.foot_comment = go_string(&self.event.foot_comment);
        }
        Ok(self.arena.push(n))
    }

    /// The recursion document/sequence/mapping → child → parse → …, run with an explicit stack of
    /// open collections starting at `first`; returns `first`'s node once its end event is
    /// consumed. A native thread stack does not grow, and the scanner lets collections nest
    /// 10000 deep before `exceeded max depth of 10000`.
    pub(crate) fn compose(&mut self, first: Frame) -> Result<NodeId, Error> {
        let mut stack = vec![first];
        let mut child = None;
        loop {
            let Some(top) = stack.last_mut() else {
                panic!("composer stack underflow");
            };
            if let Some(done) = self.resume(top, child.take())? {
                stack.pop();
                if stack.is_empty() {
                    return Ok(done);
                }
                child = Some(done);
                continue;
            }
            // The next child (init is done; `parse` re-checks it).
            match self.peek()? {
                YamlEventType::Scalar => child = Some(self.scalar()?),
                YamlEventType::Alias => child = Some(self.alias()?),
                YamlEventType::MappingStart => stack.push(self.mapping()?),
                YamlEventType::SequenceStart => stack.push(self.sequence()?),
                YamlEventType::DocumentStart => stack.push(self.document()?),
                // The event grammar puts a node (or an empty scalar) wherever a child is parsed,
                // so STREAM-END cannot happen here.
                YamlEventType::StreamEnd => {
                    panic!("internal error: missing child node (please report)")
                }
                YamlEventType::TailComment => {
                    panic!("internal error: unexpected tail comment event (please report)")
                }
                other => panic!(
                    "internal error: attempted to parse unknown event (please report): {other}"
                ),
            }
        }
    }

    /// The rest of a document / sequence / mapping body after its start event: `child` is the
    /// node just parsed for `frame` (appended here),
    /// `None` right after the collection opened. Returns the collection's node once its end
    /// event is consumed, `None` when it needs its next child.
    pub(crate) fn resume(
        &mut self,
        frame: &mut Frame,
        child: Option<NodeId>,
    ) -> Result<Option<NodeId>, Error> {
        match frame {
            Frame::Document(n) => {
                let n = *n;
                let Some(child) = child else {
                    return Ok(None); // p.parseChild(n)
                };
                self.arena[n].content.push(child);
                if self.peek()? == YamlEventType::DocumentEnd {
                    self.arena[n].foot_comment = go_string(&self.event.foot_comment);
                }
                self.expect(YamlEventType::DocumentEnd)?;
                Ok(Some(n))
            }
            Frame::Sequence(n) => {
                let n = *n;
                if let Some(child) = child {
                    self.arena[n].content.push(child);
                }
                if self.peek()? != YamlEventType::SequenceEnd {
                    return Ok(None); // p.parseChild(n)
                }
                self.arena[n].line_comment = go_string(&self.event.line_comment);
                self.arena[n].foot_comment = go_string(&self.event.foot_comment);
                self.expect(YamlEventType::SequenceEnd)?;
                Ok(Some(n))
            }
            Frame::Mapping { n, block, key } => {
                let (n, block) = (*n, *block);
                if let Some(child) = child {
                    self.arena[n].content.push(child);
                    match key.take() {
                        None => {
                            let k = child; // k := p.parseChild(n)
                            if block && !self.arena[k].foot_comment.is_empty() {
                                // Must be a foot comment for the prior value when being dedented.
                                let content = &self.arena[n].content;
                                if content.len() > 2 {
                                    let prior = content[content.len() - 3];
                                    self.arena[prior].foot_comment =
                                        self.arena[k].foot_comment.clone();
                                    self.arena[k].foot_comment = String::new();
                                }
                            }
                            *key = Some(k);
                            return Ok(None); // v := p.parseChild(n)
                        }
                        Some(k) => {
                            let v = child;
                            if self.arena[k].foot_comment.is_empty()
                                && !self.arena[v].foot_comment.is_empty()
                            {
                                self.arena[k].foot_comment =
                                    std::mem::take(&mut self.arena[v].foot_comment);
                            }
                            if self.peek()? == YamlEventType::TailComment {
                                if self.arena[k].foot_comment.is_empty() {
                                    self.arena[k].foot_comment =
                                        go_string(&self.event.foot_comment);
                                }
                                self.expect(YamlEventType::TailComment)?;
                            }
                        }
                    }
                }
                if self.peek()? != YamlEventType::MappingEnd {
                    return Ok(None); // k := p.parseChild(n)
                }
                self.arena[n].line_comment = go_string(&self.event.line_comment);
                self.arena[n].foot_comment = go_string(&self.event.foot_comment);
                let node = &self.arena[n];
                if !node.style.intersects(Style::FLOW)
                    && !node.foot_comment.is_empty()
                    && node.content.len() > 1
                {
                    let prior = node.content[node.content.len() - 2];
                    self.arena[prior].foot_comment =
                        std::mem::take(&mut self.arena[n].foot_comment);
                }
                self.expect(YamlEventType::MappingEnd)?;
                Ok(Some(n))
            }
        }
    }

    /// A document up to its first child (the rest is in [`Parser::resume`]).
    pub(crate) fn document(&mut self) -> Result<Frame, Error> {
        let n = self.node(Kind::Document, "", "", "")?;
        self.doc = Some(n);
        self.expect(YamlEventType::DocumentStart)?;
        Ok(Frame::Document(n))
    }

    /// An alias node.
    pub(crate) fn alias(&mut self) -> Result<NodeId, Error> {
        let value = go_string(&self.event.anchor);
        let n = self.node(Kind::Alias, "", "", &value)?;
        let target = self.anchors.get(&value).copied();
        self.arena[n].alias = target;
        if target.is_none() {
            return Err(failf(format_args!("unknown anchor '{value}' referenced")));
        }
        self.expect(YamlEventType::Alias)?;
        Ok(n)
    }

    /// A scalar node.
    pub(crate) fn scalar(&mut self) -> Result<NodeId, Error> {
        let parsed_style = self.event.scalar_style() as YamlStyle;
        let node_style = if parsed_style & YamlScalarStyle::DoubleQuoted as YamlStyle != 0 {
            Style::DOUBLE_QUOTED
        } else if parsed_style & YamlScalarStyle::SingleQuoted as YamlStyle != 0 {
            Style::SINGLE_QUOTED
        } else if parsed_style & YamlScalarStyle::Literal as YamlStyle != 0 {
            Style::LITERAL
        } else if parsed_style & YamlScalarStyle::Folded as YamlStyle != 0 {
            Style::FOLDED
        } else {
            Style::empty()
        };
        let node_value = go_string(&self.event.value);
        let node_tag = go_string(&self.event.tag);
        let default_tag = if node_style.is_empty() {
            if node_value == "<<" { MERGE_TAG } else { "" }
        } else {
            STR_TAG
        };
        let n = self.node(Kind::Scalar, default_tag, &node_tag, &node_value)?;
        self.arena[n].style |= node_style;
        let anchor = std::mem::take(&mut self.event.anchor);
        self.anchor(n, anchor);
        self.expect(YamlEventType::Scalar)?;
        Ok(n)
    }

    /// A sequence up to its child loop (the rest is in [`Parser::resume`]).
    pub(crate) fn sequence(&mut self) -> Result<Frame, Error> {
        let tag = go_string(&self.event.tag);
        let n = self.node(Kind::Sequence, SEQ_TAG, &tag, "")?;
        if self.event.sequence_style() as YamlStyle & YamlSequenceStyle::Flow as YamlStyle != 0 {
            self.arena[n].style |= Style::FLOW;
        }
        let anchor = std::mem::take(&mut self.event.anchor);
        self.anchor(n, anchor);
        self.expect(YamlEventType::SequenceStart)?;
        Ok(Frame::Sequence(n))
    }

    /// A mapping up to its pair loop (the rest is in [`Parser::resume`]).
    pub(crate) fn mapping(&mut self) -> Result<Frame, Error> {
        let tag = go_string(&self.event.tag);
        let n = self.node(Kind::Mapping, MAP_TAG, &tag, "")?;
        let mut block = true;
        if self.event.mapping_style() as YamlStyle & YamlMappingStyle::Flow as YamlStyle != 0 {
            block = false;
            self.arena[n].style |= Style::FLOW;
        }
        let anchor = std::mem::take(&mut self.event.anchor);
        self.anchor(n, anchor);
        self.expect(YamlEventType::MappingStart)?;
        Ok(Frame::Mapping {
            n,
            block,
            key: None,
        })
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

    /// Go-style dump of the tree at `id`: kind value tag style anchor line:col comments, one
    /// node per line, children indented, aliases as `-> <target value/anchor>`.
    fn tree(doc: &Document, id: NodeId) -> String {
        fn walk(doc: &Document, id: NodeId, depth: usize, out: &mut String) {
            let n = &doc[id];
            out.push_str(&format!(
                "{}{} {:?} tag={:?} style={} anchor={:?} {}:{}",
                "  ".repeat(depth),
                n.kind.bits(),
                n.value,
                n.tag,
                n.style.bits(),
                n.anchor,
                n.line,
                n.column
            ));
            for (k, c) in [
                ("h", &n.head_comment),
                ("l", &n.line_comment),
                ("f", &n.foot_comment),
            ] {
                if !c.is_empty() {
                    out.push_str(&format!(" {k}={c:?}"));
                }
            }
            if let Some(a) = n.alias {
                out.push_str(&format!(" -> {:?}", doc[a].anchor));
            }
            out.push('\n');
            for &c in &n.content {
                walk(doc, c, depth + 1, out);
            }
        }
        let mut out = String::new();
        walk(doc, id, 0, &mut out);
        out
    }

    fn parsed(input: &str, textless: bool) -> Result<String, String> {
        let mut p = new_parser(input.as_bytes());
        p.textless = textless;
        match p.parse() {
            Ok(Some(n)) => Ok(tree(&p.arena, n)),
            Ok(None) => Ok(String::new()),
            Err(e) => Err(e.to_string()),
        }
    }

    /// A reader that hands out `0` and then fails (Go: an `io.Reader` returning an error).
    struct Failing(&'static [u8]);

    impl Read for Failing {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.0.is_empty() {
                return Err(std::io::Error::other("boom"));
            }
            let n = self.0.len().min(buf.len());
            buf[..n].copy_from_slice(&self.0[..n]);
            self.0 = &self.0[n..];
            Ok(n)
        }
    }

    // The non-textless trees below were printed by yaml.v3 v3.0.1 (`yaml.Unmarshal` into a
    // `Node`); the textless ones follow decode.go (`Node.Encode` cannot re-parse comments).
    const COMMENTED: &str = "a: [1, 2] # line\nb: {c: 3} # map\n# foot\n";

    #[test]
    fn composes_nodes_with_positions_and_comments() {
        assert_eq!(
            parsed(COMMENTED, false).unwrap(),
            "1 \"\" tag=\"\" style=0 anchor=\"\" 1:1\n\
             \x20 4 \"\" tag=\"!!map\" style=0 anchor=\"\" 1:1\n\
             \x20   8 \"a\" tag=\"!!str\" style=0 anchor=\"\" 1:1\n\
             \x20   2 \"\" tag=\"!!seq\" style=32 anchor=\"\" 1:4 l=\"# line\"\n\
             \x20     8 \"1\" tag=\"!!int\" style=0 anchor=\"\" 1:5\n\
             \x20     8 \"2\" tag=\"!!int\" style=0 anchor=\"\" 1:8\n\
             \x20   8 \"b\" tag=\"!!str\" style=0 anchor=\"\" 2:1 f=\"# foot\"\n\
             \x20   4 \"\" tag=\"!!map\" style=32 anchor=\"\" 2:4 l=\"# map\"\n\
             \x20     8 \"c\" tag=\"!!str\" style=0 anchor=\"\" 2:5\n\
             \x20     8 \"3\" tag=\"!!int\" style=0 anchor=\"\" 2:8\n"
        );
        assert_eq!(
            parsed("# head\n\n# doc\nk: v # line\n# foot\n", false).unwrap(),
            "1 \"\" tag=\"\" style=0 anchor=\"\" 4:1 h=\"# head\"\n\
             \x20 4 \"\" tag=\"!!map\" style=0 anchor=\"\" 4:1\n\
             \x20   8 \"k\" tag=\"!!str\" style=0 anchor=\"\" 4:1 h=\"# doc\" f=\"# foot\"\n\
             \x20   8 \"v\" tag=\"!!str\" style=0 anchor=\"\" 4:4 l=\"# line\"\n"
        );
    }

    #[test]
    fn textless_drops_positions_and_node_comments_only() {
        // Go's `node` skips line/column and the event comments; `sequence`/`mapping` still
        // copy their end events' line/foot comments, and foot comments still move.
        assert_eq!(
            parsed(COMMENTED, true).unwrap(),
            "1 \"\" tag=\"\" style=0 anchor=\"\" 0:0\n\
             \x20 4 \"\" tag=\"!!map\" style=0 anchor=\"\" 0:0\n\
             \x20   8 \"a\" tag=\"!!str\" style=0 anchor=\"\" 0:0\n\
             \x20   2 \"\" tag=\"!!seq\" style=32 anchor=\"\" 0:0 l=\"# line\"\n\
             \x20     8 \"1\" tag=\"!!int\" style=0 anchor=\"\" 0:0\n\
             \x20     8 \"2\" tag=\"!!int\" style=0 anchor=\"\" 0:0\n\
             \x20   8 \"b\" tag=\"!!str\" style=0 anchor=\"\" 0:0 f=\"# foot\"\n\
             \x20   4 \"\" tag=\"!!map\" style=32 anchor=\"\" 0:0 l=\"# map\"\n\
             \x20     8 \"c\" tag=\"!!str\" style=0 anchor=\"\" 0:0\n\
             \x20     8 \"3\" tag=\"!!int\" style=0 anchor=\"\" 0:0\n"
        );
        assert_eq!(
            parsed("# head\n\n# doc\nk: v # line\n", true).unwrap(),
            "1 \"\" tag=\"\" style=0 anchor=\"\" 0:0\n\
             \x20 4 \"\" tag=\"!!map\" style=0 anchor=\"\" 0:0\n\
             \x20   8 \"k\" tag=\"!!str\" style=0 anchor=\"\" 0:0\n\
             \x20   8 \"v\" tag=\"!!str\" style=0 anchor=\"\" 0:0\n"
        );
    }

    #[test]
    fn tags_styles_and_merge_keys() {
        assert_eq!(
            parsed("<<: {a: 1}\n'<<': !!str 2\n! c: !x d\n", false).unwrap(),
            "1 \"\" tag=\"\" style=0 anchor=\"\" 1:1\n\
             \x20 4 \"\" tag=\"!!map\" style=0 anchor=\"\" 1:1\n\
             \x20   8 \"<<\" tag=\"!!merge\" style=0 anchor=\"\" 1:1\n\
             \x20   4 \"\" tag=\"!!map\" style=32 anchor=\"\" 1:5\n\
             \x20     8 \"a\" tag=\"!!str\" style=0 anchor=\"\" 1:6\n\
             \x20     8 \"1\" tag=\"!!int\" style=0 anchor=\"\" 1:9\n\
             \x20   8 \"<<\" tag=\"!!str\" style=4 anchor=\"\" 2:1\n\
             \x20   8 \"2\" tag=\"!!str\" style=1 anchor=\"\" 2:7\n\
             \x20   8 \"c\" tag=\"!!str\" style=0 anchor=\"\" 3:1\n\
             \x20   8 \"d\" tag=\"!x\" style=1 anchor=\"\" 3:6\n"
        );
    }

    #[test]
    fn anchors_and_aliases() {
        // The anchor is registered before the collection's children: an alias to an
        // enclosing node composes (a cycle), as in Go.
        let mut p = new_parser(b"&a [*a, &b x, *b]\n");
        let doc = p.parse().unwrap().unwrap();
        let arena = &p.arena;
        let seq = arena[doc].content[0];
        let items = &arena[seq].content;
        assert_eq!(arena[items[0]].alias, Some(seq));
        assert_eq!(arena[items[0]].value, "a");
        assert_eq!(arena[items[2]].alias, Some(items[1]));
        assert_eq!(p.anchors.len(), 2);

        assert_eq!(
            parsed("a: *nope\n", false).unwrap_err(),
            "yaml: unknown anchor 'nope' referenced"
        );
    }

    #[test]
    fn empty_input_and_end_of_stream() {
        // Go parses empty input as "\n": no document, `parse` returns nil.
        for input in ["", "\n", "# only a comment\n"] {
            let mut p = new_parser(input.as_bytes());
            assert_eq!(p.parse().unwrap(), None, "{input:?}");
            assert_eq!(p.parse().unwrap(), None, "{input:?}");
        }
        let mut p = new_parser(b"a\n--- b\n");
        assert!(p.parse().unwrap().is_some());
        assert!(p.parse().unwrap().is_some());
        assert_eq!(p.parse().unwrap(), None);
        assert_eq!(p.parse().unwrap(), None);
    }

    #[test]
    fn anchors_persist_across_documents() {
        let mut p = new_parser(b"a: &x 1\n---\nb: *x\n");
        let first = p.parse().unwrap().unwrap();
        let second = p.parse().unwrap().unwrap();
        let target = p.arena[p.arena[first].content[0]].content[1];
        let alias = p.arena[p.arena[second].content[0]].content[1];
        assert_eq!(p.arena[alias].alias, Some(target));
        assert_eq!((p.arena[second].line, p.arena[second].column), (2, 1));
    }

    #[test]
    fn expect_and_fail_messages() {
        let mut p = new_parser(b"a\n");
        p.init().unwrap();
        assert_eq!(
            p.expect(YamlEventType::Scalar).unwrap_err().to_string(),
            "yaml: expected scalar event but got document start"
        );
        let mut p = new_parser(b"a\n");
        while p.parse().unwrap().is_some() {}
        assert_eq!(
            p.expect(YamlEventType::Scalar).unwrap_err().to_string(),
            "yaml: attempted to go past the end of stream; corrupted value?"
        );

        let mut p = new_parser(b"a\n");
        assert_eq!(
            p.fail().to_string(),
            "yaml: unknown problem parsing YAML content"
        );
        p.parser.problem = "boom".into();
        p.parser.problem_mark.line = 2;
        assert_eq!(p.fail().to_string(), "yaml: line 2: boom");
        // The context mark wins; scanner errors count lines from 0.
        p.parser.context_mark.line = 4;
        assert_eq!(p.fail().to_string(), "yaml: line 4: boom");
        p.parser.error = YamlErrorType::Scanner;
        assert_eq!(p.fail().to_string(), "yaml: line 5: boom");
        p.parser.context_mark.line = 0;
        assert_eq!(p.fail().to_string(), "yaml: line 3: boom");
    }

    #[test]
    fn reader_errors_fail_the_parse() {
        // yaml.v3 issue #666: a reader error met while the scanner looks ahead for comments
        // leaves `parser.error` set although the parse call succeeded; `peek` checks it.
        for input in [&b"a: 1 #"[..], b"a: 1\n", b"a: 1\n# c", b"- a\n", b""] {
            let mut p = new_parser_from_reader(Failing(input));
            assert_eq!(
                p.parse().unwrap_err().to_string(),
                "yaml: input error: boom",
                "{:?}",
                String::from_utf8_lossy(input)
            );
        }
    }

    #[test]
    fn deep_nesting_does_not_overflow_the_stack() {
        // The scanner allows 10000 levels; the composer keeps them on the heap.
        let input = "- ".repeat(9_999) + "x\n";
        let mut p = new_parser(input.as_bytes());
        let doc = p.parse().unwrap().unwrap();
        let mut depth = 0;
        let mut id = p.arena[doc].content[0];
        while let Some(&child) = p.arena[id].content.first() {
            depth += 1;
            id = child;
        }
        assert_eq!((depth, p.arena[id].value.as_str()), (9_999, "x"));

        let input = "[".repeat(10_001);
        assert_eq!(
            parsed(&input, false).unwrap_err(),
            "yaml: exceeded max depth of 10000"
        );
    }

    #[test]
    fn destroy_resets_the_libyaml_parser() {
        let mut p = new_parser(b"a\n");
        p.init().unwrap();
        p.peek().unwrap();
        p.destroy();
        assert_eq!(p.event.typ, YamlEventType::No);
        assert!(p.parser.read_handler.is_none());
    }
}
