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

//! The decoder: yaml.v3 v3.0.1 `decode.go`'s reflection path (`decoder`) as a serde
//! `Deserializer` over a node of a [`Document`] — alias expansion and the alias-ratio limit,
//! `<<` merges, type-error collection, `Document` fields and the `InlineMap` catch-all.
//!
//! Go fills a value in place; serde builds it from visitor calls. Where Go leaves a target
//! untouched after a type error, the deserializer hands the visitor the zero value of the
//! entry point (`0`, `false`, `""`, an empty map, an empty sequence for a slice, `len` zero
//! elements for an array or tuple) and records the error; Go's
//! `good` result travels in [`DecodeState::good`] so sequences and maps drop the entries Go
//! drops. Go type names in `TypeError` texts come from the serde entry point (`int64`,
//! `[]uint8`, the serde struct name, `[]interface {}`, `map[string]interface {}`).

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashSet;
use std::marker::PhantomData;

use base64::Engine as _;
use base64::engine::DecodePaddingMode;
use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig};
use serde::de::value::{BorrowedStrDeserializer, StringDeserializer};
use serde::de::{
    self, Deserialize, DeserializeOwned, DeserializeSeed, Deserializer, EnumAccess, Expected,
    IntoDeserializer, MapAccess, SeqAccess, Unexpected, VariantAccess, Visitor,
};

use crate::Options;
use crate::error::{Error, MAX_WALK_DEPTH, TypeError, failf, walk_depth_exceeded};
use crate::inline::{INLINE_FIELD, INLINE_MAP_MARKER};
use crate::node::{DOCUMENT_MARKER, Document, Kind, NodeData, NodeId};
use crate::resolve::{
    BINARY_TAG, MAP_TAG, MERGE_TAG, NULL_TAG, SEQ_TAG, STR_TAG, resolve, short_tag,
};
use crate::value::{
    GO_VALUE_MARKER, GoValue, GoValueDeserializer, InlineEntries, go_sharp_v, hand_over,
    unmarshal_value, unmarshal_value_entered,
};

/// Go: `alias_ratio_range_low` — 400,000 decode operations is ~500kb of dense object
/// declarations, or ~5kb of dense object declarations with 10000% alias expansion.
pub(crate) const ALIAS_RATIO_RANGE_LOW: usize = 400_000;

/// Go: `alias_ratio_range_high` — 4,000,000 decode operations is ~5MB of dense object
/// declarations, or ~4.5MB of dense object declarations with 10% alias expansion.
pub(crate) const ALIAS_RATIO_RANGE_HIGH: usize = 4_000_000;

/// Go: `alias_ratio_range` — the range over which we scale allowed alias ratios.
pub(crate) const ALIAS_RATIO_RANGE: f64 = (ALIAS_RATIO_RANGE_HIGH - ALIAS_RATIO_RANGE_LOW) as f64;

/// Go: decode.go `decoder` — the state shared by one decode (`de.rs` and `value.rs`).
pub(crate) struct DecodeState<'o> {
    /// The key-naming options.
    pub(crate) opts: &'o Options,
    /// Go's `doc`: the document node being decoded.
    #[allow(dead_code)] // Go's decoder.doc is written but never read.
    pub(crate) doc: Option<NodeId>,
    /// Go's `aliases`: alias nodes being expanded (`anchor '…' value contains itself`).
    pub(crate) aliases: HashSet<NodeId>,
    /// Go's `terrors`: the collected `line N: …` type errors.
    pub(crate) terrors: Vec<String>,
    /// Go's `uniqueKeys` (always true: `newDecoder` sets it).
    pub(crate) unique_keys: bool,
    /// Go's `decodeCount`.
    pub(crate) decode_count: usize,
    /// Go's `aliasCount`.
    pub(crate) alias_count: usize,
    /// Go's `aliasDepth`.
    pub(crate) alias_depth: usize,
    /// Go's `mergedFields` (`map[interface{}]bool`, nil = `None`): the keys already set while
    /// a `<<` merge is applied, compared with Go's interface equality (`GoValue` `==`).
    pub(crate) merged_fields: Option<Vec<GoValue>>,
    /// Current nesting of the recursive walk: collections and alias expansions (plan
    /// decision D7; no Go field).
    pub(crate) depth: usize,
    /// Go's `good` result of the last `unmarshal` (serde visitors return values, not flags).
    pub(crate) good: bool,
    /// Go's `k.Interface()` of the last map key decoded through a key cursor, when the Go
    /// key type compares equal to the `interface{}` keys a merge records.
    pub(crate) last_key: Option<GoValue>,
}

/// Go: decode.go `newDecoder`.
pub(crate) fn new_decoder(opts: &Options) -> DecodeState<'_> {
    // Go: decode.go newDecoder
    DecodeState {
        opts,
        doc: None,
        aliases: HashSet::new(),
        terrors: Vec::new(),
        unique_keys: true,
        decode_count: 0,
        alias_count: 0,
        alias_depth: 0,
        merged_fields: None,
        depth: 0,
        good: false,
        last_key: None,
    }
}

/// What [`DecodeState::enter`] resolved; hand it back to [`DecodeState::leave`].
pub(crate) struct Entered {
    /// The node to decode (document content and alias targets followed), or `None` for a
    /// document without exactly one content node (Go's `document` returning false).
    pub(crate) node: Option<NodeId>,
    /// Whether a document node was unwrapped (Go's `document` reports success whatever its
    /// content's result).
    pub(crate) document: bool,
    /// The alias nodes being expanded, outermost first.
    aliases: Vec<NodeId>,
    /// The nesting depth to restore.
    depth: usize,
}

impl DecodeState<'_> {
    /// Go: `(*decoder).terror` — record `line N: cannot unmarshal <tag>[ `value`] into
    /// <out_type>`, `out_type` being Go's type name for the target.
    pub(crate) fn terror(&mut self, n: &NodeData, tag: &str, out_type: &str) {
        // Go: decode.go terror
        let tag = if n.tag.is_empty() {
            tag
        } else {
            n.tag.as_str()
        };
        let value = if tag != SEQ_TAG && tag != MAP_TAG {
            if n.value.len() > 10 {
                // Divergence (ledger 6c-1): Go cuts the first 7 bytes even inside a UTF-8
                // sequence and prints the raw bytes; a Rust `String` shows U+FFFD there.
                let head = String::from_utf8_lossy(&n.value.as_bytes()[..7]);
                format!(" `{head}...`")
            } else {
                format!(" `{}`", n.value)
            }
        } else {
            // Go appends a `!!seq`/`!!map` node's value as is (`!!mapfoo` for `!!map foo`).
            n.value.clone()
        };
        self.terrors.push(format!(
            "line {}: cannot unmarshal {}{} into {}",
            n.line,
            short_tag(tag),
            value,
            out_type
        ));
    }

    /// Go: the counters at the head of `(*decoder).unmarshal` and the alias-ratio check.
    pub(crate) fn count(&mut self) -> Result<(), Error> {
        // Go: decode.go unmarshal (head)
        self.decode_count += 1;
        if self.alias_depth > 0 {
            self.alias_count += 1;
        }
        if self.alias_count > 100
            && self.decode_count > 1000
            && self.alias_count as f64 / self.decode_count as f64
                > allowed_alias_ratio(self.decode_count)
        {
            return Err(failf("document contains excessive aliasing"));
        }
        Ok(())
    }

    /// One more level of nesting (plan decision D7).
    fn descend(&mut self) -> Result<(), Error> {
        // Divergence (ledger 6c-1): plan decision D7 — collections and alias expansions
        // nested deeper than MAX_WALK_DEPTH fail instead of overflowing the stack (Go has no
        // limit here).
        if self.depth >= MAX_WALK_DEPTH {
            return Err(walk_depth_exceeded());
        }
        self.depth += 1;
        Ok(())
    }

    /// Go: `(*decoder).unmarshal` up to its kind switch — the counters (`count`), then
    /// `document` and `alias` (each a nested `unmarshal`, counted again) until a node of
    /// another kind; a sequence, mapping or alias also counts one level of nesting (D7).
    pub(crate) fn enter(&mut self, doc: &Document, mut n: NodeId) -> Result<Entered, Error> {
        let mut e = Entered {
            node: None,
            document: false,
            aliases: Vec::new(),
            depth: self.depth,
        };
        loop {
            self.count()?;
            let node = &doc[n];
            match node.kind {
                Kind::Document => {
                    // Go: decode.go document
                    if node.content.len() != 1 {
                        return Ok(e);
                    }
                    self.doc = Some(n);
                    e.document = true;
                    n = node.content[0];
                }
                Kind::Alias => {
                    // Go: decode.go alias
                    if self.aliases.contains(&n) {
                        // TODO this could actually be allowed in some circumstances.
                        return Err(failf(format_args!(
                            "anchor '{}' value contains itself",
                            node.value
                        )));
                    }
                    // Go dereferences a nil `Alias` (a panic); only hand-built nodes lack one.
                    let Some(target) = node.alias else {
                        return Err(failf(format_args!(
                            "unknown anchor '{}' referenced",
                            node.value
                        )));
                    };
                    self.descend()?;
                    self.aliases.insert(n);
                    self.alias_depth += 1;
                    e.aliases.push(n);
                    n = target;
                }
                Kind::Sequence | Kind::Mapping => {
                    self.descend()?;
                    e.node = Some(n);
                    return Ok(e);
                }
                Kind::Scalar | Kind::Zero => {
                    e.node = Some(n);
                    return Ok(e);
                }
            }
        }
    }

    /// The end of the `unmarshal` calls [`DecodeState::enter`] began: Go's `alias` deletes
    /// each expanded alias and decrements `aliasDepth`.
    pub(crate) fn leave(&mut self, e: Entered) {
        for a in e.aliases.iter().rev() {
            self.aliases.remove(a);
            self.alias_depth -= 1;
        }
        self.depth = e.depth;
    }
}

/// Go: decode.go `allowedAliasRatio`.
pub(crate) fn allowed_alias_ratio(decode_count: usize) -> f64 {
    // Go: decode.go allowedAliasRatio
    if decode_count <= ALIAS_RATIO_RANGE_LOW {
        // allow 99% to come from alias expansion for small-to-medium documents
        0.99
    } else if decode_count >= ALIAS_RATIO_RANGE_HIGH {
        // allow 10% to come from alias expansion for very large documents
        0.10
    } else {
        // scale smoothly from 99% down to 10% over the range.
        // this maps to 396,000 - 400,000 allowed alias-driven decodes over the range.
        // 400,000 decode operations is ~100MB of allocations in worst-case scenarios (single-item maps).
        0.99 - 0.89 * ((decode_count - ALIAS_RATIO_RANGE_LOW) as f64 / ALIAS_RATIO_RANGE)
    }
}

/// Go: decode.go `isStringMap` — every key of the mapping is `!!str` or `!!merge`.
pub(crate) fn is_string_map(doc: &Document, n: NodeId) -> bool {
    // Go: decode.go isStringMap
    let node = &doc[n];
    if node.kind != Kind::Mapping {
        return false;
    }
    node.content.iter().step_by(2).all(|&k| {
        let tag = doc.short_tag(k);
        tag == STR_TAG || tag == MERGE_TAG
    })
}

/// Go: decode.go `failWantMap`.
pub(crate) fn fail_want_map() -> Error {
    // Go: decode.go failWantMap
    failf("map merge requires map or sequence of maps as the value")
}

/// Go: decode.go `isMerge` — an untagged or `!!merge` scalar `<<`.
pub(crate) fn is_merge(n: &NodeData) -> bool {
    // Go: decode.go isMerge
    n.kind == Kind::Scalar
        && n.value == "<<"
        && (n.tag.is_empty() || n.tag == "!" || short_tag(&n.tag) == MERGE_TAG)
}

/// Go: the unique-keys check at the head of `(*decoder).mapping`: records `line N: mapping
/// key "k" already defined at line M` for every repeated key (same kind and text) and
/// returns whether there was none.
pub(crate) fn check_unique_keys(d: &mut DecodeState<'_>, doc: &Document, n: NodeId) -> bool {
    // Go: decode.go mapping (uniqueKeys)
    if !d.unique_keys {
        return true;
    }
    let content = &doc[n].content;
    let l = content.len();
    let nerrs = d.terrors.len();
    for i in (0..l).step_by(2) {
        let ni = &doc[content[i]];
        for j in (i + 2..l).step_by(2) {
            let nj = &doc[content[j]];
            if ni.kind == nj.kind && ni.value == nj.value {
                d.terrors.push(format!(
                    "line {}: mapping key {} already defined at line {}",
                    nj.line,
                    gadk_platform::go_quote(&nj.value),
                    ni.line
                ));
            }
        }
    }
    d.terrors.len() == nerrs
}

/// Go's `base64.StdEncoding`: padded, CR/LF ignored, non-zero trailing bits accepted.
const GO_STD_ENCODING: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new()
        .with_decode_allow_trailing_bits(true)
        .with_decode_padding_mode(DecodePaddingMode::RequireCanonical),
);

/// Go's `base64.StdEncoding.DecodeString`.
fn go_base64_decode(s: &str) -> Option<Vec<u8>> {
    let clean: Vec<u8> = s.bytes().filter(|&b| b != b'\r' && b != b'\n').collect();
    GO_STD_ENCODING.decode(clean).ok()
}

/// Go: the head of `(*decoder).scalar` — the tag and value of a scalar node: a quoted or
/// `!!str` scalar is its text, else `resolve`'s result, with `!!binary` base64-decoded (a
/// Go string: [`GoValue::Bytes`] when not UTF-8).
pub(crate) fn resolve_scalar(n: &NodeData) -> Result<(String, GoValue), Error> {
    // Go: decode.go scalar (resolution)
    if n.indicated_string() {
        return Ok((STR_TAG.to_string(), GoValue::String(n.value.clone())));
    }
    let (tag, resolved) = resolve(&n.tag, &n.value)?;
    if tag == BINARY_TAG {
        let text = match &resolved {
            GoValue::String(s) => s.as_str(),
            _ => n.value.as_str(),
        };
        let data = go_base64_decode(text)
            .ok_or_else(|| failf("!!binary value contains invalid base64 data"))?;
        let value = match String::from_utf8(data) {
            Ok(s) => GoValue::String(s),
            Err(e) => GoValue::Bytes(e.into_bytes()),
        };
        return Ok((tag, value));
    }
    Ok((tag, resolved))
}

/// The text Go stores in a `string` target from a resolved scalar: the decoded `!!binary`
/// data, else the node's text.
fn go_string_of(tag: &str, resolved: &GoValue, n: &NodeData) -> String {
    if tag == BINARY_TAG {
        match resolved {
            GoValue::String(s) => return s.clone(),
            // Divergence (ledger 6c-1): Go stores the raw bytes in the string; a Rust
            // `String` holds them with U+FFFD for invalid sequences.
            GoValue::Bytes(b) => return String::from_utf8_lossy(b).into_owned(),
            _ => {}
        }
    }
    n.value.clone()
}

/// Go: `(*decoder).unmarshal(n, out)` with `out` a `string`: the text, or `None` where Go's
/// `good` is false (a null, or a collection — recorded as a type error).
pub(crate) fn unmarshal_string(
    d: &mut DecodeState<'_>,
    doc: &Document,
    n: NodeId,
) -> Result<Option<String>, Error> {
    // Go: decode.go unmarshal (string target)
    let entered = d.enter(doc, n)?;
    let result = match entered.node {
        Some(n) => string_entered(d, doc, n),
        None => Ok(None),
    };
    d.leave(entered);
    result
}

/// [`unmarshal_string`] after [`DecodeState::enter`].
fn string_entered(
    d: &mut DecodeState<'_>,
    doc: &Document,
    n: NodeId,
) -> Result<Option<String>, Error> {
    let node = &doc[n];
    match node.kind {
        Kind::Scalar => {
            let (tag, resolved) = resolve_scalar(node)?;
            if resolved == GoValue::Null {
                return Ok(None);
            }
            Ok(Some(go_string_of(&tag, &resolved, node)))
        }
        Kind::Mapping => {
            if check_unique_keys(d, doc, n) {
                d.terror(node, MAP_TAG, "string");
            }
            Ok(None)
        }
        Kind::Sequence => {
            d.terror(node, SEQ_TAG, "string");
            Ok(None)
        }
        Kind::Zero if node.is_zero() => Ok(None),
        _ => Err(unknown_kind(node)),
    }
}

/// Go's `failf("cannot decode node with unknown kind %d", n.Kind)`.
fn unknown_kind(node: &NodeData) -> Error {
    failf(format_args!(
        "cannot decode node with unknown kind {}",
        node.kind.bits()
    ))
}

/// Go's `(*Node).Decode(&v)` for node `at` of `doc`: `Err(Error::Type)` when type errors were
/// collected.
pub(crate) fn from_node<T: DeserializeOwned>(
    doc: &Document,
    at: NodeId,
    opts: &Options,
) -> Result<T, Error> {
    // Go: yaml.go (*Node).Decode
    let mut d = new_decoder(opts);
    let v = T::deserialize(NodeDe::new(&mut d, doc, at))?;
    if !d.terrors.is_empty() {
        return Err(Error::Type(TypeError { errors: d.terrors }));
    }
    Ok(v)
}

/// Go's `(*Node).Decode(&v)` keeping Go's partly filled value next to the `*TypeError`.
pub(crate) fn from_node_partial<T: DeserializeOwned + Default>(
    doc: &Document,
    at: NodeId,
    opts: &Options,
) -> Result<(T, Option<TypeError>), Error> {
    // Go: yaml.go (*Node).Decode
    let mut d = new_decoder(opts);
    match T::deserialize(NodeDe::new(&mut d, doc, at)) {
        Ok(v) => {
            let terr = (!d.terrors.is_empty()).then_some(TypeError { errors: d.terrors });
            Ok((v, terr))
        }
        // A type error serde cannot answer with a zero value (an enum) ends the decode;
        // `T::default()` stands for Go's untouched value.
        Err(Error::Type(e)) => Ok((T::default(), Some(e))),
        Err(e) => Err(e),
    }
}

thread_local! {
    /// The subtree handed from the decoder to `Document`'s visitor (see
    /// [`crate::node::DOCUMENT_MARKER`]).
    static DOCUMENT_SLOT: RefCell<Option<Document>> = const { RefCell::new(None) };
}

/// Builds a [`Document`] from the hand-over slot.
struct DocumentVisitor;

impl<'de> Visitor<'de> for DocumentVisitor {
    type Value = Document;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a YAML node (only gadk-goyaml decodes a Document)")
    }

    fn visit_newtype_struct<D: Deserializer<'de>>(self, _d: D) -> Result<Document, D::Error> {
        match DOCUMENT_SLOT.with(|s| s.borrow_mut().take()) {
            Some(doc) => Ok(doc),
            None => Err(de::Error::invalid_type(Unexpected::NewtypeStruct, &self)),
        }
    }
}

impl<'de> Deserialize<'de> for Document {
    /// A `Document` field receives the raw sub-node (Go's `yaml.Node` field) through
    /// [`crate::node::DOCUMENT_MARKER`].
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_newtype_struct(DOCUMENT_MARKER, DocumentVisitor)
    }
}

/// A Go scalar kind a serde entry point stands for.
#[derive(Clone, Copy)]
enum Prim {
    Bool,
    Int(u32),
    Uint(u32),
    Float(u32),
    Str,
}

impl Prim {
    /// Go's type name in `TypeError` texts.
    fn go_name(self) -> &'static str {
        match self {
            Prim::Bool => "bool",
            Prim::Int(8) => "int8",
            Prim::Int(16) => "int16",
            Prim::Int(32) => "int32",
            Prim::Int(_) => "int64",
            Prim::Uint(8) => "uint8",
            Prim::Uint(16) => "uint16",
            Prim::Uint(32) => "uint32",
            Prim::Uint(_) => "uint64",
            Prim::Float(32) => "float32",
            Prim::Float(_) => "float64",
            Prim::Str => "string",
        }
    }

    /// Go: the kind switch of `(*decoder).scalar` for a non-nil resolved value.
    fn convert(self, tag: &str, resolved: &GoValue, n: &NodeData) -> Option<GoValue> {
        // Go: decode.go scalar (kind switch)
        match self {
            Prim::Str => Some(GoValue::String(go_string_of(tag, resolved, n))),
            Prim::Bool => match resolved {
                GoValue::Bool(b) => Some(GoValue::Bool(*b)),
                // This offers some compatibility with the 1.1 spec (https://yaml.org/type/bool.html).
                // It only works if explicitly attempting to unmarshal into a typed bool value.
                GoValue::String(s) => match s.as_str() {
                    "y" | "Y" | "yes" | "Yes" | "YES" | "on" | "On" | "ON" => {
                        Some(GoValue::Bool(true))
                    }
                    "n" | "N" | "no" | "No" | "NO" | "off" | "Off" | "OFF" => {
                        Some(GoValue::Bool(false))
                    }
                    _ => None,
                },
                _ => None,
            },
            Prim::Int(bits) => {
                let v = match *resolved {
                    GoValue::Int(i) => i,
                    GoValue::Uint(u) => i64::try_from(u).ok()?,
                    // Divergence (ledger 6c-1): Go's `int64(f)` of an out-of-range float is
                    // platform-defined; Rust saturates, as Go does on arm64.
                    GoValue::Float(f) if f <= i64::MAX as f64 => f as i64,
                    _ => return None,
                };
                (bits >= 64 || (v >= -(1i64 << (bits - 1)) && v < (1i64 << (bits - 1))))
                    .then_some(GoValue::Int(v))
            }
            Prim::Uint(bits) => {
                let v = match *resolved {
                    GoValue::Int(i) => u64::try_from(i).ok()?,
                    GoValue::Uint(u) => u,
                    // Divergence (ledger 6c-1): as above, `uint64(f)` saturates (Go on arm64).
                    GoValue::Float(f) if f <= u64::MAX as f64 => f as u64,
                    _ => return None,
                };
                (bits >= 64 || v < (1u64 << bits)).then_some(GoValue::Uint(v))
            }
            Prim::Float(_) => match *resolved {
                GoValue::Int(i) => Some(GoValue::Float(i as f64)),
                GoValue::Uint(u) => Some(GoValue::Float(u as f64)),
                GoValue::Float(f) => Some(GoValue::Float(f)),
                _ => None,
            },
        }
    }

    /// Go's `k.Interface()` of a key decoded into this kind, when it can equal one of the
    /// `interface{}` keys a merge records (Go compares type and value: `string`, `bool`,
    /// `float64`, and `uint64` above `math.MaxInt64` match; sized ints and `int64` never do).
    fn key_identity(self, v: &GoValue) -> Option<GoValue> {
        match (self, v) {
            (Prim::Str | Prim::Bool, _) | (Prim::Float(64), _) => Some(v.clone()),
            (Prim::Uint(64), GoValue::Uint(u)) if i64::try_from(*u).is_err() => Some(v.clone()),
            _ => None,
        }
    }

    fn visit<'de, V: Visitor<'de>>(self, v: GoValue, visitor: V) -> Result<V::Value, Error> {
        match (self, v) {
            (_, GoValue::Bool(b)) => visitor.visit_bool(b),
            (Prim::Int(8), GoValue::Int(i)) => visitor.visit_i8(i as i8),
            (Prim::Int(16), GoValue::Int(i)) => visitor.visit_i16(i as i16),
            (Prim::Int(32), GoValue::Int(i)) => visitor.visit_i32(i as i32),
            (_, GoValue::Int(i)) => visitor.visit_i64(i),
            (Prim::Uint(8), GoValue::Uint(u)) => visitor.visit_u8(u as u8),
            (Prim::Uint(16), GoValue::Uint(u)) => visitor.visit_u16(u as u16),
            (Prim::Uint(32), GoValue::Uint(u)) => visitor.visit_u32(u as u32),
            (_, GoValue::Uint(u)) => visitor.visit_u64(u),
            (Prim::Float(32), GoValue::Float(f)) => visitor.visit_f32(f as f32),
            (_, GoValue::Float(f)) => visitor.visit_f64(f),
            (_, GoValue::String(s)) => visitor.visit_string(s),
            (_, other) => unreachable!(
                "Prim::convert yields only bool, int, uint, float or string values, not {other:?}"
            ),
        }
    }

    /// The zero value Go leaves in a target it did not set.
    fn zero<'de, V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self {
            Prim::Bool => visitor.visit_bool(false),
            Prim::Int(8) => visitor.visit_i8(0),
            Prim::Int(16) => visitor.visit_i16(0),
            Prim::Int(32) => visitor.visit_i32(0),
            Prim::Int(_) => visitor.visit_i64(0),
            Prim::Uint(8) => visitor.visit_u8(0),
            Prim::Uint(16) => visitor.visit_u16(0),
            Prim::Uint(32) => visitor.visit_u32(0),
            Prim::Uint(_) => visitor.visit_u64(0),
            Prim::Float(32) => visitor.visit_f32(0.0),
            Prim::Float(_) => visitor.visit_f64(0.0),
            Prim::Str => visitor.visit_str(""),
        }
    }
}

/// The sequence-shaped serde entry points.
#[derive(Clone, Copy)]
enum SeqWant {
    /// `deserialize_seq`: Go `[]interface {}`.
    Seq,
    /// `deserialize_bytes` / `deserialize_byte_buf`: Go `[]uint8`.
    Bytes,
    /// `deserialize_tuple` / `deserialize_tuple_struct`: a Go array of this length.
    Tuple(usize),
}

impl SeqWant {
    fn go_name(self) -> &'static str {
        match self {
            SeqWant::Bytes => "[]uint8",
            // Divergence (ledger 6c-1): serde exposes no element type; Go names it
            // (`[]int64`, `[]yaml.Node`, `[2]int`).
            SeqWant::Seq | SeqWant::Tuple(_) => "[]interface {}",
        }
    }

    fn zero<'de, V: Visitor<'de>>(
        self,
        d: &mut DecodeState<'_>,
        doc: &Document,
        visitor: V,
    ) -> Result<V::Value, Error> {
        match self {
            SeqWant::Bytes => visitor.visit_byte_buf(Vec::new()),
            // A Go array keeps its length: `len` zero elements.
            SeqWant::Seq => visitor.visit_seq(SeqWalk::new(d, doc, &[], 0)),
            SeqWant::Tuple(len) => visitor.visit_seq(SeqWalk::new(d, doc, &[], len)),
        }
    }
}

/// The map-shaped serde entry points.
#[derive(Clone, Copy)]
enum Mode {
    /// `deserialize_map`: Go `map[string]interface {}` (any key type).
    Map,
    /// `deserialize_struct`: Go's struct of that name.
    Struct {
        name: &'static str,
        fields: &'static [&'static str],
    },
}

impl Mode {
    fn go_name(self) -> &'static str {
        match self {
            // Divergence (ledger 6c-1): serde exposes no key or element type; Go names them
            // (`map[string]int64`).
            Mode::Map => "map[string]interface {}",
            // Divergence (ledger 6c-1): Go prints the package-qualified type (`main.Inner`);
            // serde gives the bare struct name.
            Mode::Struct { name, .. } => name,
        }
    }
}

/// The serde `Deserializer` over node `n` of a [`Document`]: one Go `unmarshal(n, out)` call
/// whose `out` type is the serde entry point used.
pub(crate) struct NodeDe<'a, 'o> {
    d: &'a mut DecodeState<'o>,
    doc: &'a Document,
    n: NodeId,
    /// Whether Go's `unmarshal` head already ran for `n` (a cursor handed on by `Option` or
    /// a newtype, Go's `prepare` loop over pointers).
    entered: bool,
    /// Whether this cursor decodes a map key (records [`DecodeState::last_key`]).
    key: bool,
}

impl<'a, 'o> NodeDe<'a, 'o> {
    pub(crate) fn new(d: &'a mut DecodeState<'o>, doc: &'a Document, n: NodeId) -> Self {
        NodeDe {
            d,
            doc,
            n,
            entered: false,
            key: false,
        }
    }

    /// The cursor for the same node after `enter` (Go's `prepare` dereferencing a pointer).
    fn same(&mut self) -> NodeDe<'_, 'o> {
        NodeDe {
            d: &mut *self.d,
            doc: self.doc,
            n: self.n,
            entered: true,
            key: self.key,
        }
    }

    /// Runs `f` on the node Go's `unmarshal` reaches after `document`/`alias` (`None` for a
    /// document without one content node), then undoes the alias bookkeeping.
    fn run<R>(
        mut self,
        f: impl FnOnce(&mut Self, Option<NodeId>) -> Result<R, Error>,
    ) -> Result<R, Error> {
        if self.entered {
            let n = self.n;
            return f(&mut self, Some(n));
        }
        let e = self.d.enter(self.doc, self.n)?;
        if let Some(n) = e.node {
            self.n = n;
        }
        self.entered = true;
        let r = f(&mut self, e.node);
        if e.document {
            // Go's `document` returns true whatever its content's result.
            self.d.good = true;
        }
        self.d.leave(e);
        r
    }

    /// Go: `unmarshal` into a scalar kind (`scalar`, or a type error for a collection).
    fn primitive<'de, V: Visitor<'de>>(
        &mut self,
        n: Option<NodeId>,
        want: Prim,
        visitor: V,
    ) -> Result<V::Value, Error> {
        let Some(n) = n else {
            self.d.good = false;
            return want.zero(visitor);
        };
        let node = &self.doc[n];
        match node.kind {
            Kind::Scalar => {
                let (tag, resolved) = resolve_scalar(node)?;
                if resolved == GoValue::Null {
                    // Go's `null(out)` leaves a non-nillable target untouched.
                    self.d.good = false;
                    return want.zero(visitor);
                }
                match want.convert(&tag, &resolved, node) {
                    Some(v) => {
                        if self.key {
                            self.d.last_key = want.key_identity(&v);
                        }
                        self.d.good = true;
                        want.visit(v, visitor)
                    }
                    None => {
                        self.d.terror(node, &tag, want.go_name());
                        self.d.good = false;
                        want.zero(visitor)
                    }
                }
            }
            Kind::Mapping => {
                if check_unique_keys(self.d, self.doc, n) {
                    self.d.terror(node, MAP_TAG, want.go_name());
                }
                self.d.good = false;
                want.zero(visitor)
            }
            Kind::Sequence => {
                self.d.terror(node, SEQ_TAG, want.go_name());
                self.d.good = false;
                want.zero(visitor)
            }
            Kind::Zero if node.is_zero() => {
                self.d.good = false;
                want.zero(visitor)
            }
            _ => Err(unknown_kind(node)),
        }
    }

    /// Go: `unmarshal` into a slice or array (`sequence`; a scalar or mapping is a type
    /// error, a null sets the nil slice).
    fn sequence<'de, V: Visitor<'de>>(
        &mut self,
        n: Option<NodeId>,
        want: SeqWant,
        visitor: V,
    ) -> Result<V::Value, Error> {
        let Some(n) = n else {
            self.d.good = false;
            return want.zero(self.d, self.doc, visitor);
        };
        let doc = self.doc;
        let node = &doc[n];
        match node.kind {
            Kind::Sequence => {
                // Go: decode.go sequence
                if let SeqWant::Tuple(len) = want
                    && node.content.len() != len
                {
                    return Err(failf(format_args!(
                        "invalid array: want {len} elements but got {}",
                        node.content.len()
                    )));
                }
                let len = match want {
                    SeqWant::Tuple(len) => len,
                    _ => 0,
                };
                let r = visitor.visit_seq(SeqWalk::new(&mut *self.d, doc, &node.content, len));
                self.d.good = true;
                r
            }
            Kind::Scalar => {
                let (tag, resolved) = resolve_scalar(node)?;
                if resolved == GoValue::Null {
                    // Go's `null(out)` sets a slice to nil (an array is left untouched).
                    self.d.good = !matches!(want, SeqWant::Tuple(_));
                } else {
                    self.d.terror(node, &tag, want.go_name());
                    self.d.good = false;
                }
                let good = self.d.good;
                let r = want.zero(self.d, doc, visitor);
                self.d.good = good;
                r
            }
            Kind::Mapping => {
                if check_unique_keys(self.d, doc, n) {
                    self.d.terror(node, MAP_TAG, want.go_name());
                }
                let r = want.zero(self.d, doc, visitor);
                self.d.good = false;
                r
            }
            Kind::Zero if node.is_zero() => {
                let r = want.zero(self.d, doc, visitor);
                self.d.good = !matches!(want, SeqWant::Tuple(_));
                r
            }
            _ => Err(unknown_kind(node)),
        }
    }

    /// Go: `unmarshal` into a map or struct (`mapping` / `mappingStruct`; a scalar or
    /// sequence is a type error, a null sets a nil map and leaves a struct untouched).
    fn mapping<'de, V: Visitor<'de>>(
        &mut self,
        n: Option<NodeId>,
        mode: Mode,
        visitor: V,
    ) -> Result<V::Value, Error> {
        let doc = self.doc;
        let Some(n) = n else {
            let r = visitor.visit_map(MapWalk::empty(&mut *self.d, doc, mode));
            self.d.good = false;
            return r;
        };
        let node = &doc[n];
        let good = match node.kind {
            Kind::Mapping => {
                if check_unique_keys(self.d, doc, n) {
                    let r = visitor.visit_map(MapWalk::new(&mut *self.d, doc, n, mode));
                    self.d.good = true;
                    return r;
                }
                false
            }
            Kind::Scalar => {
                let (tag, resolved) = resolve_scalar(node)?;
                if resolved == GoValue::Null {
                    matches!(mode, Mode::Map)
                } else {
                    self.d.terror(node, &tag, mode.go_name());
                    false
                }
            }
            Kind::Sequence => {
                self.d.terror(node, SEQ_TAG, mode.go_name());
                false
            }
            Kind::Zero if node.is_zero() => matches!(mode, Mode::Map),
            _ => return Err(unknown_kind(node)),
        };
        let r = visitor.visit_map(MapWalk::empty(&mut *self.d, doc, mode));
        self.d.good = good;
        r
    }

    /// Go: `unmarshal` into `struct {}` (serde's unit and unit structs).
    fn unit<'de, V: Visitor<'de>>(
        &mut self,
        n: Option<NodeId>,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Error> {
        self.d.good = false;
        if let Some(n) = n {
            let node = &self.doc[n];
            match node.kind {
                Kind::Mapping => self.d.good = check_unique_keys(self.d, self.doc, n),
                Kind::Scalar => {
                    let (tag, resolved) = resolve_scalar(node)?;
                    if resolved != GoValue::Null {
                        self.d.terror(node, &tag, name);
                    }
                }
                Kind::Sequence => self.d.terror(node, SEQ_TAG, name),
                Kind::Zero if node.is_zero() => {}
                _ => return Err(unknown_kind(node)),
            }
        }
        visitor.visit_unit()
    }

    /// An enum: a scalar names a unit variant, a one-pair mapping `{variant: value}` any
    /// variant (R6's encoding); anything else is a type error, returned at once since serde
    /// has no zero enum value.
    fn enumeration<'de, V: Visitor<'de>>(
        &mut self,
        n: Option<NodeId>,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        let doc = self.doc;
        let Some(n) = n else {
            return Err(de::Error::invalid_type(Unexpected::Unit, &visitor));
        };
        let node = &doc[n];
        match node.kind {
            Kind::Scalar => {
                let (tag, resolved) = resolve_scalar(node)?;
                if resolved == GoValue::Null {
                    return Err(de::Error::invalid_type(Unexpected::Unit, &visitor));
                }
                self.d.good = true;
                visitor.visit_enum(UnitVariant(go_string_of(&tag, &resolved, node)))
            }
            Kind::Mapping if node.content.len() == 2 => {
                let terrors = self.d.terrors.len();
                let Some(variant) = unmarshal_string(self.d, doc, node.content[0])? else {
                    if self.d.terrors.len() == terrors {
                        // A null key: Go's null into a string is "not good" with no type
                        // error, so report serde's, as for a null enum scalar.
                        return Err(de::Error::invalid_type(Unexpected::Unit, &visitor));
                    }
                    return Err(Error::Type(TypeError {
                        errors: self.d.terrors.clone(),
                    }));
                };
                // A struct variant's fields are keyed under the variant's name, as on encode.
                let struct_name = variants
                    .iter()
                    .copied()
                    .find(|v| *v == variant)
                    .unwrap_or(name);
                let r = visitor.visit_enum(MapVariant {
                    d: &mut *self.d,
                    doc,
                    variant,
                    value: node.content[1],
                    struct_name,
                });
                self.d.good = true;
                r
            }
            Kind::Zero if node.is_zero() => {
                Err(de::Error::invalid_type(Unexpected::Unit, &visitor))
            }
            Kind::Mapping | Kind::Sequence => {
                let tag = if node.kind == Kind::Sequence {
                    SEQ_TAG
                } else {
                    MAP_TAG
                };
                self.d.terror(node, tag, name);
                Err(Error::Type(TypeError {
                    errors: self.d.terrors.clone(),
                }))
            }
            _ => Err(unknown_kind(node)),
        }
    }
}

impl<'de> Deserializer<'de> for NodeDe<'_, '_> {
    type Error = Error;

    /// Go's `interface{}` target: the [`GoValue`] rules (R5), handed to the visitor by shape.
    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        let v = if self.entered {
            unmarshal_value_entered(self.d, self.doc, self.n)?
        } else {
            unmarshal_value(self.d, self.doc, self.n)?
        };
        if self.key {
            self.d.last_key = v.clone();
        }
        let good = v.is_some();
        let r = GoValueDeserializer(v.unwrap_or_default()).deserialize_any(visitor);
        self.d.good = good;
        r
    }

    fn deserialize_bool<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.run(|c, n| c.primitive(n, Prim::Bool, visitor))
    }

    fn deserialize_i8<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.run(|c, n| c.primitive(n, Prim::Int(8), visitor))
    }

    fn deserialize_i16<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.run(|c, n| c.primitive(n, Prim::Int(16), visitor))
    }

    fn deserialize_i32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.run(|c, n| c.primitive(n, Prim::Int(32), visitor))
    }

    fn deserialize_i64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.run(|c, n| c.primitive(n, Prim::Int(64), visitor))
    }

    fn deserialize_u8<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.run(|c, n| c.primitive(n, Prim::Uint(8), visitor))
    }

    fn deserialize_u16<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.run(|c, n| c.primitive(n, Prim::Uint(16), visitor))
    }

    fn deserialize_u32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.run(|c, n| c.primitive(n, Prim::Uint(32), visitor))
    }

    fn deserialize_u64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.run(|c, n| c.primitive(n, Prim::Uint(64), visitor))
    }

    fn deserialize_f32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.run(|c, n| c.primitive(n, Prim::Float(32), visitor))
    }

    fn deserialize_f64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.run(|c, n| c.primitive(n, Prim::Float(64), visitor))
    }

    fn deserialize_char<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_string(visitor)
    }

    fn deserialize_str<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_string(visitor)
    }

    fn deserialize_string<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.run(|c, n| c.primitive(n, Prim::Str, visitor))
    }

    /// Go's `[]byte`: a sequence of `uint8`; v3.0.1 rejects a `!!str`/`!!binary` scalar
    /// (`cannot unmarshal !!binary `…` into []uint8`).
    fn deserialize_bytes<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.run(|c, n| c.sequence(n, SeqWant::Bytes, visitor))
    }

    fn deserialize_byte_buf<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.run(|c, n| c.sequence(n, SeqWant::Bytes, visitor))
    }

    /// Go's pointer: nil for a null node, else allocated and decoded (Go's `prepare`).
    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.run(|c, n| match n {
            Some(n) if c.doc.short_tag(n) != NULL_TAG => visitor.visit_some(c.same()),
            _ => {
                c.d.good = true;
                visitor.visit_none()
            }
        })
    }

    fn deserialize_unit<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.run(|c, n| c.unit(n, "struct {}", visitor))
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Error> {
        self.run(|c, n| c.unit(n, name, visitor))
    }

    /// Newtypes are transparent (Go's named types), except the private markers: a
    /// [`Document`] field receives the node (Go's `out.Type() == nodeType`, checked before
    /// `document`/`alias`), a [`GoValue`] the `interface{}` decode.
    fn deserialize_newtype_struct<V: Visitor<'de>>(
        mut self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Error> {
        if name == DOCUMENT_MARKER {
            // Divergence (ledger 6c-1): an `Option<Document>` (Go `*yaml.Node`) receives the
            // node too; Go checks for `yaml.Node` before `prepare` only, so a non-null value
            // into `*yaml.Node` is decoded as an ordinary struct there.
            if !self.entered {
                self.d.count()?;
            }
            let sub = self.doc.subtree(self.n);
            DOCUMENT_SLOT.with(|s| *s.borrow_mut() = Some(sub));
            let r = visitor.visit_newtype_struct(self.same());
            DOCUMENT_SLOT.with(|s| s.borrow_mut().take());
            self.d.good = true;
            return r;
        }
        if name == GO_VALUE_MARKER {
            let v = if self.entered {
                unmarshal_value_entered(self.d, self.doc, self.n)?
            } else {
                unmarshal_value(self.d, self.doc, self.n)?
            };
            if self.key {
                self.d.last_key = v.clone();
            }
            let good = v.is_some();
            let r = hand_over(v.unwrap_or_default(), visitor);
            self.d.good = good;
            return r;
        }
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.run(|c, n| c.sequence(n, SeqWant::Seq, visitor))
    }

    fn deserialize_tuple<V: Visitor<'de>>(self, len: usize, visitor: V) -> Result<V::Value, Error> {
        self.run(|c, n| c.sequence(n, SeqWant::Tuple(len), visitor))
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Error> {
        self.run(|c, n| c.sequence(n, SeqWant::Tuple(len), visitor))
    }

    /// Go's map target. serde derive decodes a struct with a `#[serde(flatten)]` field
    /// through `deserialize_map` with a visitor expecting `struct Name`; that is rejected
    /// (spec R6/R8).
    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        let expected = format!("{}", &visitor as &dyn Expected);
        if expected.starts_with("struct ") {
            // Divergence (ledger 6c-1): Go has no flatten; serde's flatten needs maps of
            // unknown shape, which yaml.v3's rules cannot express.
            return Err(Error::Unsupported(format!(
                "serde flatten is unsupported ({expected})"
            )));
        }
        self.run(|c, n| c.mapping(n, Mode::Map, visitor))
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        self.run(|c, n| c.mapping(n, Mode::Struct { name, fields }, visitor))
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        self.run(|c, n| c.enumeration(n, name, variants, visitor))
    }

    fn deserialize_identifier<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_string(visitor)
    }

    /// Go skips unknown keys without decoding their values.
    fn deserialize_ignored_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.d.good = true;
        visitor.visit_unit()
    }
}

/// The items of a sequence node as a serde sequence (Go's `sequence` loop): `next_element`
/// drops items whose decode was not good (Go stores only those, at index `j`, so a Go array's
/// good elements move toward the front), then yields zero elements until `len` were yielded
/// (a Go array's untouched tail; `len` is 0 for a slice).
struct SeqWalk<'a, 'o> {
    d: &'a mut DecodeState<'o>,
    doc: &'a Document,
    items: &'a [NodeId],
    idx: usize,
    len: usize,
    yielded: usize,
}

impl<'a, 'o> SeqWalk<'a, 'o> {
    fn new(d: &'a mut DecodeState<'o>, doc: &'a Document, items: &'a [NodeId], len: usize) -> Self {
        SeqWalk {
            d,
            doc,
            items,
            idx: 0,
            len,
            yielded: 0,
        }
    }

    /// The next zero element of a Go array's tail, decoded from the zero node without
    /// counting a Go `unmarshal` call (Go never visits it).
    fn zero_element<'de, T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Error> {
        if self.yielded >= self.len {
            return Ok(None);
        }
        self.yielded += 1;
        let zero = Document::default();
        let r = seed.deserialize(NodeDe {
            d: &mut *self.d,
            doc: &zero,
            n: zero.root(),
            entered: true,
            key: false,
        });
        self.d.good = false;
        r.map(Some)
    }
}

impl<'de> SeqAccess<'de> for SeqWalk<'_, '_> {
    type Error = Error;

    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Error> {
        let Some(&n) = self.items.get(self.idx) else {
            return self.zero_element(seed);
        };
        self.idx += 1;
        self.yielded += 1;
        seed.deserialize(NodeDe::new(self.d, self.doc, n)).map(Some)
    }

    fn next_element<T: Deserialize<'de>>(&mut self) -> Result<Option<T>, Error> {
        loop {
            let Some(&n) = self.items.get(self.idx) else {
                return self.zero_element(PhantomData);
            };
            self.idx += 1;
            let v = T::deserialize(NodeDe::new(self.d, self.doc, n))?;
            if self.d.good {
                self.yielded += 1;
                return Ok(Some(v));
            }
        }
    }

    fn size_hint(&self) -> Option<usize> {
        Some((self.items.len() - self.idx).max(self.len.saturating_sub(self.yielded)))
    }
}

/// A step of Go's `mapping`/`mappingStruct`/`merge` recursion, run iteratively so the pairs
/// of merged mappings reach the same visitor.
enum Task {
    /// The pair loop over mapping `node` (Go's local `mergedFields` is `mf`, `doneFields` is
    /// `done`; `merge` is the last `<<` value seen).
    Pairs {
        node: NodeId,
        i: usize,
        merge: Option<NodeId>,
        mf: Option<Vec<GoValue>>,
        done: Vec<bool>,
    },
    /// Go's `merge`: `items` decoded into the same target in turn (`restore_none`: Go's
    /// local `mergedFields` was nil).
    Merge {
        items: Vec<NodeId>,
        check_items: bool,
        idx: usize,
        restore_none: bool,
    },
    /// The end of the `unmarshal` call that entered a merged mapping.
    Leave(Entered),
}

/// What the walk yields next.
enum Yield {
    /// A struct field (serde name) and its value node.
    Field(&'static str, NodeId),
    /// A map pair.
    Pair(NodeId, NodeId),
    /// The inline catch-all's entries.
    Inline(Vec<(String, GoValue)>),
}

/// The value the visitor asks for after a key.
enum Pending {
    None,
    Node(NodeId),
    Inline(Vec<(String, GoValue)>),
}

/// The pairs of a mapping node (with `<<` merges applied) as a serde map: Go's `mapping` with
/// a map target or `mappingStruct`.
struct MapWalk<'a, 'o> {
    d: &'a mut DecodeState<'o>,
    doc: &'a Document,
    mode: Mode,
    /// Struct mode: each field's YAML key ([`Options::key_for`]) and index, without
    /// `INLINE_FIELD`.
    keys: Vec<(Cow<'static, str>, usize)>,
    /// Struct mode with an `INLINE_FIELD`: the unmatched pairs (Go's inline map), until handed
    /// over after every other pair.
    inline: Option<Vec<(String, GoValue)>>,
    /// Struct fields already handed to the visitor (serde accepts each once).
    given: Vec<bool>,
    stack: Vec<Task>,
    pending: Pending,
}

impl<'a, 'o> MapWalk<'a, 'o> {
    fn empty(d: &'a mut DecodeState<'o>, doc: &'a Document, mode: Mode) -> Self {
        MapWalk {
            d,
            doc,
            mode,
            keys: Vec::new(),
            inline: None,
            given: Vec::new(),
            stack: Vec::new(),
            pending: Pending::None,
        }
    }

    fn new(d: &'a mut DecodeState<'o>, doc: &'a Document, n: NodeId, mode: Mode) -> Self {
        let mut w = MapWalk::empty(d, doc, mode);
        if let Mode::Struct { name, fields } = mode {
            let opts = w.d.opts;
            w.keys = fields
                .iter()
                .enumerate()
                .filter(|(_, f)| **f != INLINE_FIELD)
                .map(|(i, f)| (opts.key_for(name, f), i))
                .collect();
            if fields.contains(&INLINE_FIELD) {
                w.inline = Some(Vec::new());
            }
            w.given = vec![false; fields.len()];
        }
        w.push_pairs(n);
        w
    }

    /// Go: the head of `mapping`/`mappingStruct` — take `mergedFields` for this loop.
    fn push_pairs(&mut self, node: NodeId) {
        let done = match self.mode {
            Mode::Struct { fields, .. } => vec![false; fields.len()],
            Mode::Map => Vec::new(),
        };
        self.stack.push(Task::Pairs {
            node,
            i: 0,
            merge: None,
            mf: self.d.merged_fields.take(),
            done,
        });
    }

    /// Go: `(*decoder).merge` up to its kind switch.
    fn start_merge(&mut self, parent: NodeId, merge: NodeId) -> Result<(), Error> {
        // Go: decode.go merge
        let doc = self.doc;
        let restore_none = self.d.merged_fields.is_none();
        if restore_none {
            self.d.merged_fields = Some(Vec::new());
            for pair in doc[parent].content.as_chunks::<2>().0 {
                if let Some(k) = unmarshal_value(self.d, doc, pair[0])?
                    && let Some(mf) = &mut self.d.merged_fields
                {
                    mf.push(k);
                }
            }
        }
        let m = &doc[merge];
        let (items, check_items) = match m.kind {
            Kind::Mapping => (vec![merge], false),
            Kind::Alias => {
                if let Some(target) = m.alias
                    && doc[target].kind != Kind::Mapping
                {
                    return Err(fail_want_map());
                }
                (vec![merge], false)
            }
            Kind::Sequence => (m.content.clone(), true),
            _ => return Err(fail_want_map()),
        };
        self.stack.push(Task::Merge {
            items,
            check_items,
            idx: 0,
            restore_none,
        });
        Ok(())
    }

    /// Runs the loops until the next pair the visitor sees.
    fn next_yield(&mut self) -> Result<Option<Yield>, Error> {
        let doc = self.doc;
        loop {
            let Some(task) = self.stack.last_mut() else {
                return Ok(self.inline.take().map(Yield::Inline));
            };
            match task {
                Task::Leave(_) => {
                    if let Some(Task::Leave(e)) = self.stack.pop() {
                        self.d.leave(e);
                    }
                }
                Task::Merge {
                    items,
                    check_items,
                    idx,
                    restore_none,
                } => {
                    let Some(&ni) = items.get(*idx) else {
                        if *restore_none {
                            self.d.merged_fields = None;
                        }
                        self.stack.pop();
                        continue;
                    };
                    *idx += 1;
                    if *check_items {
                        let item = &doc[ni];
                        if item.kind == Kind::Alias {
                            if let Some(target) = item.alias
                                && doc[target].kind != Kind::Mapping
                            {
                                return Err(fail_want_map());
                            }
                        } else if item.kind != Kind::Mapping {
                            return Err(fail_want_map());
                        }
                    }
                    // Go: d.unmarshal(item, out) — a mapping into the same target.
                    let e = self.d.enter(doc, ni)?;
                    match e.node {
                        Some(m)
                            if doc[m].kind == Kind::Mapping
                                && check_unique_keys(self.d, doc, m) =>
                        {
                            self.stack.push(Task::Leave(e));
                            self.push_pairs(m);
                        }
                        _ => self.d.leave(e),
                    }
                }
                Task::Pairs {
                    node,
                    i,
                    merge,
                    mf,
                    done,
                } => {
                    let content = &doc[*node].content;
                    if *i + 1 >= content.len() {
                        // Go: the end of the loop, then `merge`.
                        let (node, merge, mf) = (*node, *merge, mf.take());
                        self.stack.pop();
                        self.d.merged_fields = mf;
                        if let Some(m) = merge {
                            self.start_merge(node, m)?;
                        }
                        continue;
                    }
                    let (kn, vn) = (content[*i], content[*i + 1]);
                    *i += 2;
                    if is_merge(&doc[kn]) {
                        *merge = Some(vn);
                        continue;
                    }
                    let Mode::Struct { name, fields } = self.mode else {
                        return Ok(Some(Yield::Pair(kn, vn)));
                    };
                    // Go: decode.go mappingStruct (pair)
                    let Some(sname) = unmarshal_string(self.d, doc, kn)? else {
                        continue;
                    };
                    if let Some(mf) = mf {
                        let k = GoValue::String(sname.clone());
                        if mf.contains(&k) {
                            continue;
                        }
                        mf.push(k);
                    }
                    if let Some(&(_, fi)) = self.keys.iter().find(|(k, _)| *k == sname) {
                        if self.d.unique_keys {
                            if done[fi] {
                                // Divergence (ledger 6c-1): Go names the type
                                // package-qualified (`main.T`).
                                self.d.terrors.push(format!(
                                    "line {}: field {sname} already set in type {name}",
                                    doc[kn].line
                                ));
                                continue;
                            }
                            done[fi] = true;
                        }
                        // serde takes each field once; Go would set it again only for a merged
                        // key whose recorded form differs in type (`1` vs `"1"`).
                        if self.given[fi] {
                            continue;
                        }
                        self.given[fi] = true;
                        return Ok(Some(Yield::Field(fields[fi], vn)));
                    }
                    if self.inline.is_some() {
                        let v = unmarshal_value(self.d, doc, vn)?.unwrap_or_default();
                        if let Some(inline) = &mut self.inline {
                            match inline.iter_mut().find(|(k, _)| *k == sname) {
                                Some(entry) => entry.1 = v,
                                None => inline.push((sname, v)),
                            }
                        }
                    }
                }
            }
        }
    }

    /// Go's handling of a decoded map key `k.Interface()` (from [`DecodeState::last_key`]):
    /// false when a merge already set it; `invalid map key` for a slice or map.
    fn check_key(&mut self) -> Result<bool, Error> {
        let Some(k) = self.d.last_key.take() else {
            return Ok(true);
        };
        if let Some(Task::Pairs { mf: Some(mf), .. }) = self.stack.last_mut() {
            if mf.contains(&k) {
                return Ok(false);
            }
            mf.push(k.clone());
        }
        if matches!(k, GoValue::Seq(_) | GoValue::Map(_) | GoValue::MapAny(_)) {
            return Err(failf(format_args!("invalid map key: {}", go_sharp_v(&k))));
        }
        Ok(true)
    }
}

impl<'de> MapAccess<'de> for MapWalk<'_, '_> {
    type Error = Error;

    /// For map visitors that do not use `next_entry`, every pair is handed over (a key
    /// cannot be taken back after the visitor has it).
    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Error> {
        match self.next_yield()? {
            None => Ok(None),
            Some(Yield::Field(f, v)) => {
                self.pending = Pending::Node(v);
                seed.deserialize(BorrowedStrDeserializer::new(f)).map(Some)
            }
            Some(Yield::Inline(entries)) => {
                self.pending = Pending::Inline(entries);
                seed.deserialize(BorrowedStrDeserializer::new(INLINE_FIELD))
                    .map(Some)
            }
            Some(Yield::Pair(k, v)) => {
                self.d.last_key = None;
                let mut key_de = NodeDe::new(self.d, self.doc, k);
                key_de.key = true;
                let key = seed.deserialize(key_de)?;
                self.check_key()?;
                self.pending = Pending::Node(v);
                Ok(Some(key))
            }
        }
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, Error> {
        match std::mem::replace(&mut self.pending, Pending::None) {
            Pending::Node(v) => seed.deserialize(NodeDe::new(self.d, self.doc, v)),
            Pending::Inline(entries) => {
                let name = match self.mode {
                    Mode::Struct { name, .. } => name,
                    Mode::Map => "",
                };
                seed.deserialize(InlineDe {
                    entries,
                    name,
                    marked: false,
                })
            }
            Pending::None => Err(Error::Custom("map value requested before its key".into())),
        }
    }

    /// Go's map pair loop: a key whose decode was not good, or that a merge already set, is
    /// skipped, as is a value whose decode was not good (unless it is null).
    fn next_entry<K: Deserialize<'de>, V: Deserialize<'de>>(
        &mut self,
    ) -> Result<Option<(K, V)>, Error> {
        if let Mode::Struct { .. } = self.mode {
            let Some(k) = self.next_key::<K>()? else {
                return Ok(None);
            };
            return Ok(Some((k, self.next_value::<V>()?)));
        }
        // Go: decode.go mapping (pair)
        loop {
            let Some(Yield::Pair(kn, vn)) = self.next_yield()? else {
                return Ok(None);
            };
            self.d.last_key = None;
            let mut key_de = NodeDe::new(self.d, self.doc, kn);
            key_de.key = true;
            let k = K::deserialize(key_de)?;
            if !self.d.good || !self.check_key()? {
                continue;
            }
            let v = V::deserialize(NodeDe::new(self.d, self.doc, vn))?;
            // Go's `mapIsNew` holds for every map serde builds.
            if self.d.good || self.doc.short_tag(vn) == NULL_TAG {
                return Ok(Some((k, v)));
            }
        }
    }
}

/// The value of the `INLINE_FIELD` key: only an [`crate::InlineMap`] accepts it (its
/// `INLINE_MAP_MARKER` newtype, then the map inside).
struct InlineDe {
    entries: Vec<(String, GoValue)>,
    name: &'static str,
    /// Whether the `INLINE_MAP_MARKER` newtype was asked for first.
    marked: bool,
}

impl InlineDe {
    fn not_inline_map(&self) -> Error {
        Error::Unsupported(format!(
            "field \"{INLINE_FIELD}\" of struct {} must be a gadk_goyaml::InlineMap",
            self.name
        ))
    }
}

impl<'de> Deserializer<'de> for InlineDe {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, _visitor: V) -> Result<V::Value, Error> {
        Err(self.not_inline_map())
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Error> {
        if name == INLINE_MAP_MARKER && !self.marked {
            return visitor.visit_newtype_struct(InlineDe {
                marked: true,
                ..self
            });
        }
        Err(self.not_inline_map())
    }

    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        if !self.marked {
            return Err(self.not_inline_map());
        }
        visitor.visit_map(InlineEntries {
            entries: self.entries.into_iter(),
            value: None,
        })
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes byte_buf
        option unit unit_struct seq tuple tuple_struct struct enum identifier ignored_any
    }
}

/// An enum given as a scalar: a unit variant.
struct UnitVariant(String);

impl<'de> EnumAccess<'de> for UnitVariant {
    type Error = Error;
    type Variant = Self;

    fn variant_seed<V: DeserializeSeed<'de>>(self, seed: V) -> Result<(V::Value, Self), Error> {
        let name: StringDeserializer<Error> = self.0.clone().into_deserializer();
        Ok((seed.deserialize(name)?, self))
    }
}

impl<'de> VariantAccess<'de> for UnitVariant {
    type Error = Error;

    fn unit_variant(self) -> Result<(), Error> {
        Ok(())
    }

    fn newtype_variant_seed<T: DeserializeSeed<'de>>(self, _seed: T) -> Result<T::Value, Error> {
        Err(de::Error::invalid_type(
            Unexpected::UnitVariant,
            &"a newtype variant",
        ))
    }

    fn tuple_variant<V: Visitor<'de>>(self, _len: usize, _visitor: V) -> Result<V::Value, Error> {
        Err(de::Error::invalid_type(
            Unexpected::UnitVariant,
            &"a tuple variant",
        ))
    }

    fn struct_variant<V: Visitor<'de>>(
        self,
        _fields: &'static [&'static str],
        _visitor: V,
    ) -> Result<V::Value, Error> {
        Err(de::Error::invalid_type(
            Unexpected::UnitVariant,
            &"a struct variant",
        ))
    }
}

/// An enum given as `{variant: value}`.
struct MapVariant<'a, 'o> {
    d: &'a mut DecodeState<'o>,
    doc: &'a Document,
    variant: String,
    value: NodeId,
    /// The matched variant's name (the enum's when no variant matches; serde then fails).
    struct_name: &'static str,
}

impl<'de, 'a, 'o> EnumAccess<'de> for MapVariant<'a, 'o> {
    type Error = Error;
    type Variant = VariantDe<'a, 'o>;

    fn variant_seed<V: DeserializeSeed<'de>>(
        self,
        seed: V,
    ) -> Result<(V::Value, VariantDe<'a, 'o>), Error> {
        let variant: StringDeserializer<Error> = self.variant.into_deserializer();
        let v = seed.deserialize(variant)?;
        Ok((
            v,
            VariantDe {
                value: NodeDe::new(self.d, self.doc, self.value),
                struct_name: self.struct_name,
            },
        ))
    }
}

/// The value of a `{variant: value}` enum.
struct VariantDe<'a, 'o> {
    value: NodeDe<'a, 'o>,
    /// The variant's name: a struct variant's fields are keyed under it
    /// ([`Options::key_for`]), as `serialize_struct_variant` keys them.
    struct_name: &'static str,
}

impl<'de> VariantAccess<'de> for VariantDe<'_, '_> {
    type Error = Error;

    fn unit_variant(self) -> Result<(), Error> {
        Ok(())
    }

    fn newtype_variant_seed<T: DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value, Error> {
        seed.deserialize(self.value)
    }

    fn tuple_variant<V: Visitor<'de>>(self, len: usize, visitor: V) -> Result<V::Value, Error> {
        self.value.deserialize_tuple(len, visitor)
    }

    fn struct_variant<V: Visitor<'de>>(
        self,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        self.value
            .deserialize_struct(self.struct_name, fields, visitor)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde::Deserialize;

    use super::*;
    use crate::{Decoder, InlineMap, from_node, unmarshal, unmarshal_node};

    fn opts() -> Options {
        Options::default()
    }

    #[derive(Debug, Default, Deserialize, PartialEq)]
    #[serde(default)]
    struct Point {
        x: i64,
        y_pos: i64,
        label: String,
    }

    #[test]
    fn struct_keys_follow_key_for() {
        let p: Point =
            unmarshal(b"x: 1\nypos: 2\ny_pos: 9\nlabel: a\nother: [1]\n", &opts()).unwrap();
        assert_eq!(
            p,
            Point {
                x: 1,
                y_pos: 2,
                label: "a".into()
            }
        );
    }

    #[test]
    fn type_errors_are_collected_and_partial_value_kept() {
        let doc = unmarshal_node(b"x: a\nypos: 3\nlabel: [1]\n").unwrap();
        let err = from_node::<Point>(&doc, doc.root(), &opts()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "yaml: unmarshal errors:\n  line 1: cannot unmarshal !!str `a` into int64\n  line 3: cannot unmarshal !!seq into string"
        );
        let (p, terr) = from_node_partial::<Point>(&doc, doc.root(), &opts()).unwrap();
        assert_eq!(
            p,
            Point {
                y_pos: 3,
                ..Default::default()
            }
        );
        assert_eq!(terr.unwrap().errors.len(), 2);
        let (p, terr) = from_node_partial::<Point>(&doc, doc.content_root(), &opts()).unwrap();
        assert_eq!(p.y_pos, 3);
        assert!(terr.is_some());
    }

    trait ContentRoot {
        fn content_root(&self) -> NodeId;
    }

    impl ContentRoot for Document {
        fn content_root(&self) -> NodeId {
            self[self.root()].content[0]
        }
    }

    #[test]
    fn long_values_are_cut_in_type_errors() {
        let err = unmarshal::<i64>(b"abcdefghijklmnop", &opts()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "yaml: unmarshal errors:\n  line 1: cannot unmarshal !!str `abcdefg...` into int64"
        );
    }

    #[derive(Debug, Default, Deserialize, PartialEq)]
    #[serde(default)]
    struct Fixed {
        arr: [i64; 2],
        tup: (i64, i64),
        n: i64,
    }

    /// Go's array target: a null leaves it untouched (no error), a scalar is a type error,
    /// and only good elements are stored, compacted toward the front (decode.go `sequence`'s
    /// `j`). Go names the type `[2]int64` (ledgered: the port says `[]interface {}`).
    #[test]
    fn fixed_length_targets_take_null_mismatch_and_compact_like_go() {
        let partial = |src: &str| {
            let doc = unmarshal_node(src.as_bytes()).unwrap();
            let (v, te) = from_node_partial::<Fixed>(&doc, doc.root(), &opts()).unwrap();
            (v, te.map(|te| Error::Type(te).to_string()))
        };
        let want = |arr, tup| Fixed { arr, tup, n: 1 };

        assert_eq!(
            partial("arr: ~\ntup: ~\nn: 1\n"),
            (want([0, 0], (0, 0)), None)
        );
        assert_eq!(
            unmarshal::<Fixed>(b"arr: ~\ntup: ~\nn: 1\n", &opts()).unwrap(),
            want([0, 0], (0, 0))
        );
        assert_eq!(
            partial("arr: x\ntup: x\nn: 1\n"),
            (
                want([0, 0], (0, 0)),
                Some(
                    "yaml: unmarshal errors:\n  line 1: cannot unmarshal !!str `x` into []interface {}\n  line 2: cannot unmarshal !!str `x` into []interface {}"
                        .to_owned()
                )
            )
        );
        assert_eq!(
            partial("arr: [x, 1]\ntup: [x, 1]\nn: 1\n"),
            (
                want([1, 0], (1, 0)),
                Some(
                    "yaml: unmarshal errors:\n  line 1: cannot unmarshal !!str `x` into int64\n  line 2: cannot unmarshal !!str `x` into int64"
                        .to_owned()
                )
            )
        );
        assert_eq!(
            partial("arr: [1, ~]\ntup: [~, 1]\nn: 1\n"),
            (want([1, 0], (1, 0)), None)
        );
        assert_eq!(unmarshal::<[i64; 2]>(b"~", &opts()).unwrap(), [0, 0]);
        assert_eq!(
            unmarshal::<[[i64; 2]; 2]>(b"[~, [x, 3]]", &opts())
                .unwrap_err()
                .to_string(),
            "yaml: unmarshal errors:\n  line 1: cannot unmarshal !!str `x` into int64"
        );
        let doc = unmarshal_node(b"[~, [x, 3]]").unwrap();
        let (v, _) = from_node_partial::<[[i64; 2]; 2]>(&doc, doc.root(), &opts()).unwrap();
        assert_eq!(v, [[3, 0], [0, 0]]);
    }

    #[derive(Debug, Deserialize, PartialEq)]
    struct Strict {
        name: String,
        count: i64,
    }

    #[test]
    fn missing_field_without_serde_default_is_serdes_error() {
        // Go zero-fills absent keys; a serde struct without `#[serde(default)]` fails
        // (ledgered divergence, spec R7).
        let err = unmarshal::<Strict>(b"name: n\n", &opts()).unwrap_err();
        assert!(matches!(&err, Error::Custom(m) if m == "missing field `count`"));
        let s: Strict = unmarshal(b"name: n\ncount: 2\n", &opts()).unwrap();
        assert_eq!(s.count, 2);
    }

    #[derive(Debug, Default, Deserialize)]
    struct Flat {
        #[allow(dead_code)]
        a: i64,
        #[serde(flatten)]
        #[allow(dead_code)]
        rest: BTreeMap<String, i64>,
    }

    #[test]
    fn serde_flatten_is_rejected() {
        let err = unmarshal::<Flat>(b"a: 1\nb: 2\n", &opts()).unwrap_err();
        assert!(matches!(&err, Error::Unsupported(_)), "{err:?}");
        assert_eq!(
            err.to_string(),
            "serde flatten is unsupported (struct Flat)"
        );
        // An ordinary map still decodes.
        let m: BTreeMap<String, i64> = unmarshal(b"a: 1\nb: 2\n", &opts()).unwrap();
        assert_eq!(m.len(), 2);
    }

    #[derive(Debug, Default, Deserialize, PartialEq)]
    #[serde(default)]
    struct WithExtra {
        name: String,
        #[serde(rename = "$gadk_goyaml::inline")]
        extra: InlineMap,
    }

    #[derive(Debug, Default, Deserialize)]
    #[serde(default)]
    struct BadInline {
        #[serde(rename = "$gadk_goyaml::inline")]
        #[allow(dead_code)]
        extra: BTreeMap<String, i64>,
    }

    #[test]
    fn inline_map_takes_unmatched_keys() {
        let w: WithExtra = unmarshal(b"z: 1\nname: n\n<<: {a: x, name: m}\n", &opts()).unwrap();
        assert_eq!(w.name, "n");
        assert_eq!(
            w.extra.0,
            vec![
                ("z".to_string(), GoValue::Int(1)),
                ("a".to_string(), GoValue::String("x".into()))
            ]
        );
        let err = unmarshal::<BadInline>(b"a: 1\n", &opts()).unwrap_err();
        assert!(matches!(&err, Error::Unsupported(_)));
        assert_eq!(
            err.to_string(),
            "field \"$gadk_goyaml::inline\" of struct BadInline must be a gadk_goyaml::InlineMap"
        );
    }

    #[derive(Debug, Default, Deserialize)]
    #[serde(default)]
    struct WithNode {
        spec: Document,
        edges: Vec<Document>,
    }

    #[test]
    fn document_fields_receive_subtrees() {
        let w: WithNode = unmarshal(b"spec: &s {a: 1}\nedges: [*s, x]\n", &opts()).unwrap();
        assert_eq!(w.spec[w.spec.root()].kind, Kind::Mapping);
        assert_eq!(w.spec[w.spec.root()].anchor, "s");
        assert_eq!(w.edges.len(), 2);
        let alias = &w.edges[0][w.edges[0].root()];
        assert_eq!(alias.kind, Kind::Alias);
        let target = alias.alias.unwrap();
        assert_eq!(w.edges[0][target].kind, Kind::Mapping);
        assert_eq!(w.edges[1][w.edges[1].root()].value, "x");
        assert!(serde_json::from_str::<Document>("1").is_err());
    }

    #[derive(Debug, Deserialize, PartialEq)]
    enum Shape {
        Dot,
        Circle(f64),
        Rect { w: i64, h: i64 },
    }

    #[test]
    fn enums_from_scalars_and_one_key_maps() {
        let v: Vec<Shape> =
            unmarshal(b"- Dot\n- Circle: 1.5\n- Rect: {w: 2, h: 3}\n", &opts()).unwrap();
        assert_eq!(
            v,
            vec![Shape::Dot, Shape::Circle(1.5), Shape::Rect { w: 2, h: 3 }]
        );
        let err = unmarshal::<Shape>(b"[1]", &opts()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "yaml: unmarshal errors:\n  line 1: cannot unmarshal !!seq into Shape"
        );
    }

    #[derive(Debug, PartialEq, serde::Serialize, Deserialize)]
    enum SvEnum {
        Sv { my_field: i64 },
    }

    #[test]
    fn struct_variant_keys_round_trip_under_the_variant_name() {
        // Encode and decode both look a struct variant's keys up under the variant's name.
        let v = SvEnum::Sv { my_field: 3 };
        let mut verbatim = opts();
        verbatim.verbatim_structs.insert("Sv");
        let out = crate::marshal(&v, &verbatim).unwrap();
        assert_eq!(
            String::from_utf8(out.clone()).unwrap(),
            "Sv:\n    my_field: 3\n"
        );
        assert_eq!(unmarshal::<SvEnum>(&out, &verbatim).unwrap(), v);

        let mut keyed = opts();
        keyed.field_keys.insert(("Sv", "my_field"), "mf");
        let out = crate::marshal(&v, &keyed).unwrap();
        assert_eq!(String::from_utf8(out.clone()).unwrap(), "Sv:\n    mf: 3\n");
        assert_eq!(unmarshal::<SvEnum>(&out, &keyed).unwrap(), v);

        // The enum's name is not a key namespace for its variants' fields.
        let mut by_enum = opts();
        by_enum.verbatim_structs.insert("SvEnum");
        let out = crate::marshal(&v, &by_enum).unwrap();
        assert_eq!(
            String::from_utf8(out.clone()).unwrap(),
            "Sv:\n    myfield: 3\n"
        );
        assert_eq!(unmarshal::<SvEnum>(&out, &by_enum).unwrap(), v);
    }

    #[test]
    fn untagged_struct_variants_do_not_round_trip_non_default_keys() {
        // Pinned limitation (ledger 6c-1, Not annotated): serde decodes an untagged enum
        // through `deserialize_any` (raw YAML keys), while encode applies `key_for`.
        #[derive(Debug, PartialEq, serde::Serialize, Deserialize)]
        #[serde(untagged)]
        enum U {
            A { my_field: i64 },
        }
        let out = crate::marshal(&U::A { my_field: 1 }, &opts()).unwrap();
        assert_eq!(String::from_utf8(out.clone()).unwrap(), "myfield: 1\n");
        assert!(unmarshal::<U>(&out, &opts()).is_err());
        assert_eq!(
            unmarshal::<U>(b"my_field: 1\n", &opts()).unwrap(),
            U::A { my_field: 1 }
        );
    }

    #[test]
    fn terror_keeps_the_raw_value_of_seq_and_map_tags() {
        // Go: `cannot unmarshal !!mapfoo into int64` (value appended without backticks).
        let err = unmarshal::<BTreeMap<String, i64>>(b"a: !!map foo\n", &opts()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "yaml: unmarshal errors:\n  line 1: cannot unmarshal !!mapfoo into int64"
        );
        let err = unmarshal::<BTreeMap<String, i64>>(b"a: !!seq bar\n", &opts()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "yaml: unmarshal errors:\n  line 1: cannot unmarshal !!seqbar into int64"
        );
    }

    #[test]
    fn null_variant_key_is_a_serde_invalid_type() {
        // Go's null into a string position is "not good" with no type error (`d.null`), so a
        // null variant key reports serde's error, as a null enum scalar does.
        let err = unmarshal::<Shape>(b"{~: 1}", &opts()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid type: unit value, expected enum Shape"
        );
        // A variant key that records a type error keeps reporting it.
        let err = unmarshal::<Shape>(b"{[1]: 1}", &opts()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "yaml: unmarshal errors:\n  line 1: cannot unmarshal !!seq into string"
        );
    }

    #[test]
    fn from_node_partial_keeps_kept_entries() {
        let doc = unmarshal_node(b"[a, ~, b, [1]]").unwrap();
        let (v, terr) = from_node_partial::<Vec<String>>(&doc, doc.root(), &opts()).unwrap();
        assert_eq!(v, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(
            terr.unwrap().to_string(),
            "yaml: unmarshal errors:\n  line 1: cannot unmarshal !!seq into string"
        );
        let doc = unmarshal_node(b"{a: 1, b: ~, c: x}").unwrap();
        let (m, _) = from_node_partial::<BTreeMap<String, i64>>(&doc, doc.root(), &opts()).unwrap();
        // A null value is kept as the zero value; a value that fails is dropped.
        assert_eq!(m, BTreeMap::from([("a".into(), 1), ("b".into(), 0)]));
        let o: Option<i64> = unmarshal(b"~", &opts()).unwrap();
        assert_eq!(o, None);
    }

    #[test]
    fn decoder_decodes_each_document() {
        let mut dec = Decoder::new(&b"a: &x 1\n---\nb: *x\n---\n[1, 2]\n"[..]);
        let first: BTreeMap<String, i64> = dec.decode(&opts()).unwrap().unwrap();
        assert_eq!(first["a"], 1);
        let second: BTreeMap<String, i64> = dec.decode(&opts()).unwrap().unwrap();
        assert_eq!(second["b"], 1);
        let third: Vec<u8> = dec.decode(&opts()).unwrap().unwrap();
        assert_eq!(third, vec![1, 2]);
        assert!(dec.decode::<GoValue>(&opts()).unwrap().is_none());
    }

    #[test]
    fn bytes_reject_strings_like_go() {
        #[derive(Debug, Default, Deserialize)]
        #[serde(default)]
        struct B {
            #[serde(with = "bytes_buf")]
            b: Vec<u8>,
        }
        mod bytes_buf {
            pub fn deserialize<'de, D: serde::Deserializer<'de>>(
                d: D,
            ) -> Result<Vec<u8>, D::Error> {
                struct V;
                impl<'de> serde::de::Visitor<'de> for V {
                    type Value = Vec<u8>;
                    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        f.write_str("bytes")
                    }
                    fn visit_byte_buf<E>(self, v: Vec<u8>) -> Result<Vec<u8>, E> {
                        Ok(v)
                    }
                    fn visit_seq<A: serde::de::SeqAccess<'de>>(
                        self,
                        mut s: A,
                    ) -> Result<Vec<u8>, A::Error> {
                        let mut out = Vec::new();
                        while let Some(b) = s.next_element::<u8>()? {
                            out.push(b);
                        }
                        Ok(out)
                    }
                }
                d.deserialize_byte_buf(V)
            }
        }
        let err = unmarshal::<B>(b"b: !!binary aGVsbG8=\n", &opts()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "yaml: unmarshal errors:\n  line 1: cannot unmarshal !!binary `aGVsbG8=` into []uint8"
        );
        let b: B = unmarshal(b"b: [1, 255]\n", &opts()).unwrap();
        assert_eq!(b.b, vec![1, 255]);
    }

    #[derive(Debug, Deserialize)]
    struct Rec(Vec<Rec>);

    #[test]
    fn walk_depth_is_capped_at_127_nested_collections() {
        // Plan decision D7: 127 nested sequences decode into a recursive serde type, 128 fail
        // (Go has no limit here).
        let ok = format!("{}{}", "[".repeat(127), "]".repeat(127));
        let mut r: Rec = unmarshal(ok.as_bytes(), &opts()).unwrap();
        let mut levels = 1;
        while let Some(inner) = r.0.pop() {
            levels += 1;
            r = inner;
        }
        assert_eq!(levels, 127);
        let deep = format!("{}{}", "[".repeat(128), "]".repeat(128));
        let err = unmarshal::<Rec>(deep.as_bytes(), &opts()).unwrap_err();
        assert_eq!(err.to_string(), "yaml: exceeded max depth of 127");
    }

    #[test]
    fn helpers_match_go() {
        assert!((allowed_alias_ratio(0) - 0.99).abs() < 1e-12);
        assert!((allowed_alias_ratio(4_000_000) - 0.10).abs() < 1e-12);
        assert!((allowed_alias_ratio(2_200_000) - 0.545).abs() < 1e-12);
        let doc = unmarshal_node(b"{a: 1, <<: {}, !!str 2: 3}").unwrap();
        assert!(is_string_map(&doc, doc.content_root()));
        let doc = unmarshal_node(b"{a: 1, 2: 3}").unwrap();
        assert!(!is_string_map(&doc, doc.content_root()));
        let doc = unmarshal_node(b"[<<, !!merge <<, '<<', !!str <<]").unwrap();
        let merges: Vec<bool> = doc[doc.content_root()]
            .content
            .iter()
            .map(|&n| is_merge(&doc[n]))
            .collect();
        // A quoted `'<<'` is `!!str`, not a merge key.
        assert_eq!(merges, vec![true, true, false, false]);
        assert_eq!(
            fail_want_map().to_string(),
            "yaml: map merge requires map or sequence of maps as the value"
        );
        assert_eq!(go_base64_decode("aGVs\nbG8="), Some(b"hello".to_vec()));
        assert_eq!(go_base64_decode("aGVsbG8"), None);
    }
}
