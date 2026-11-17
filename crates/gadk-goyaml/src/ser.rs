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

//! The serializer: a serde `Serializer` for the value dispatch (`marshal`, `mapv`, `structv`,
//! `slicev`) emitting events through `encode.rs`, plus the
//! `Serialize` impls of [`Document`] (routed to `nodev`) and [`GoValue`].
//!
//! yaml.v3 reflects over a value it can walk twice; serde hands each value over once, in serde
//! order. So a value is first captured into a [`Content`] tree (struct keys named by
//! [`Options::key_for`], map entries sorted by `sorter.rs`, the inline map checked against the
//! struct's keys), then [`marshal`] walks that tree as yaml.v3's encoder walks a value,
//! through `encode.rs`'s `stringv`/`intv`/…/`mappingv`/`slicev`/`nodev`.
//!
//! Three values cross the serde boundary through private newtype names instead of their serde
//! form (the `serde_json::RawValue` technique): a [`Document`] ([`DOCUMENT_MARKER`], encoded
//! by `nodev`), a [`GoValue::Timestamp`] ([`TIMESTAMP_MARKER`], `timev`)
//! and a [`GoValue::Bytes`] ([`GO_STRING_MARKER`], `stringv` of a string that is not UTF-8). The
//! value itself is handed over in a thread-local slot while the codec is "armed"; through any
//! other serializer they serialize as unit, an RFC3339Nano string and bytes respectively.

use std::cell::Cell;
use std::collections::BTreeSet;

use chrono::{DateTime, FixedOffset};
use serde::ser::{
    Serialize, SerializeMap, SerializeSeq, SerializeStruct, SerializeStructVariant, SerializeTuple,
    SerializeTupleStruct, SerializeTupleVariant, Serializer,
};

use crate::Options;
use crate::encode::Encoder;
use crate::error::{Error, MAX_WALK_DEPTH, walk_depth_exceeded};
use crate::inline::{INLINE_FIELD, INLINE_MAP_MARKER};
use crate::node::{DOCUMENT_MARKER, Document, Kind};
use crate::sorter::{ReflectKind, SortKey, sort_keys};
use crate::value::GoValue;

/// The newtype name a [`GoValue::Timestamp`] serializes through (→ `timev`).
pub(crate) const TIMESTAMP_MARKER: &str = "$gadk_goyaml::Timestamp";

/// The newtype name a [`GoValue::Bytes`] serializes through (a string holding bytes that
/// are not UTF-8 → `stringv`'s `!!binary`).
pub(crate) const GO_STRING_MARKER: &str = "$gadk_goyaml::GoString";

/// The error a `serialize_map(None)` call returns (serde derive's `#[serde(flatten)]` path).
const FLATTEN_UNSUPPORTED: &str = "serde flatten / maps of unknown length are unsupported";

/// A value handed over through a marker newtype.
enum Handed {
    Document(Document),
    Time(DateTime<FixedOffset>),
    GoString(Vec<u8>),
}

thread_local! {
    /// Set by the codec right before it serializes a marker newtype's payload, cleared by the
    /// payload when it hands itself over (so other serializers never see it set).
    static ARMED: Cell<bool> = const { Cell::new(false) };
    /// The handed-over value.
    static SLOT: Cell<Option<Handed>> = const { Cell::new(None) };
}

/// The payload of a marker newtype.
enum Handoff<'a> {
    Document(&'a Document),
    Time(&'a DateTime<FixedOffset>),
    GoString(&'a [u8]),
}

impl Serialize for Handoff<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if ARMED.replace(false) {
            SLOT.set(Some(match self {
                Handoff::Document(d) => Handed::Document((*d).clone()),
                Handoff::Time(t) => Handed::Time(**t),
                Handoff::GoString(b) => Handed::GoString(b.to_vec()),
            }));
            return serializer.serialize_unit();
        }
        // Another serde format.
        match self {
            Handoff::Document(_) => serializer.serialize_unit(),
            Handoff::Time(t) => serializer.serialize_str(&gadk_platform::go_format_rfc3339_nano(t)),
            Handoff::GoString(b) => serializer.serialize_bytes(b),
        }
    }
}

impl Serialize for Document {
    /// A node is encoded through `nodev`; through gadk-goyaml's serializer the
    /// subtree is handed over by `crate::node::DOCUMENT_MARKER`. Other serde formats see
    /// unit.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_newtype_struct(DOCUMENT_MARKER, &Handoff::Document(self))
    }
}

impl Serialize for GoValue {
    /// Encodes the dynamic value (`Timestamp` via `timev`, `Bytes` via
    /// `stringv`'s `!!binary`, maps sorted by `sorter.rs`).
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            GoValue::Null => serializer.serialize_unit(),
            GoValue::Bool(b) => serializer.serialize_bool(*b),
            GoValue::Int(i) => serializer.serialize_i64(*i),
            GoValue::Uint(u) => serializer.serialize_u64(*u),
            GoValue::Float(f) => serializer.serialize_f64(*f),
            GoValue::String(s) => serializer.serialize_str(s),
            GoValue::Bytes(b) => {
                serializer.serialize_newtype_struct(GO_STRING_MARKER, &Handoff::GoString(b))
            }
            GoValue::Timestamp(t) => {
                serializer.serialize_newtype_struct(TIMESTAMP_MARKER, &Handoff::Time(t))
            }
            GoValue::Seq(items) => {
                let mut seq = serializer.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(item)?;
                }
                seq.end()
            }
            GoValue::Map(entries) => {
                let mut map = serializer.serialize_map(Some(entries.len()))?;
                for (k, v) in entries {
                    map.serialize_entry(k, v)?;
                }
                map.end()
            }
            GoValue::MapAny(entries) => {
                let mut map = serializer.serialize_map(Some(entries.len()))?;
                for (k, v) in entries {
                    map.serialize_entry(k, v)?;
                }
                map.end()
            }
        }
    }
}

/// A captured value: what [`marshal`] dispatches on (the scalar and collection kinds plus the
/// node and timestamp special cases).
enum Content {
    /// Null (serde `None`/unit) → `nilv`.
    Null,
    /// `bool` → `boolv`.
    Bool(bool),
    /// A signed integer of the given kind → `intv`.
    Int(ReflectKind, i64),
    /// An unsigned integer of the given kind → `uintv`.
    Uint(ReflectKind, u64),
    /// `float32` (value widened) or `float64` → `floatv`.
    Float(ReflectKind, f64),
    /// A string (any bytes) → `stringv`.
    Str(Vec<u8>),
    /// timestamp → `timev`.
    Time(DateTime<FixedOffset>),
    /// A slice → `slicev`.
    Seq(Vec<Content>),
    /// A map, entries sorted by `keyList` → `mapv`.
    Map(Vec<MapEntry>),
    /// An [`crate::InlineMap`] (a map; inside its struct, the `,inline` entries).
    InlineMap(Vec<MapEntry>),
    /// A struct → `structv`.
    Struct(StructContent),
    /// A node → `nodev`.
    Node(Document),
}

/// One captured map entry with its `keyList` sort key.
struct MapEntry {
    sort: SortKey,
    key: Content,
    value: Content,
}

/// A captured struct: the fields in serde order and the `,inline` map's sorted entries.
struct StructContent {
    fields: Vec<(String, Content)>,
    inline: Vec<MapEntry>,
}

impl Content {
    /// The key as the map-key ordering sees it (looking through interfaces/pointers).
    fn sort_key(&self) -> SortKey {
        match self {
            Content::Null => SortKey::Other(ReflectKind::Interface),
            Content::Bool(b) => SortKey::Bool(*b),
            Content::Int(k, i) => SortKey::Int(*k, *i),
            Content::Uint(k, u) => SortKey::Uint(*k, *u),
            Content::Float(k, f) => SortKey::Float(*k, *f),
            Content::Str(b) => match std::str::from_utf8(b) {
                Ok(s) => SortKey::String(s.to_owned()),
                Err(_) => SortKey::from_go_value(&GoValue::Bytes(b.clone())),
            },
            Content::Seq(_) => SortKey::Other(ReflectKind::Slice),
            Content::Map(_) | Content::InlineMap(_) => SortKey::Other(ReflectKind::Map),
            Content::Time(_) | Content::Struct(_) | Content::Node(_) => {
                SortKey::Other(ReflectKind::Struct)
            }
        }
    }
}

/// One more level of collection nesting below `depth` (plan decision D7).
fn enter(depth: usize) -> Result<usize, Error> {
    // Plan decision D7: values nested deeper than MAX_WALK_DEPTH collections fail instead of
    // overflowing the stack.
    if depth >= MAX_WALK_DEPTH {
        return Err(walk_depth_exceeded());
    }
    Ok(depth + 1)
}

/// Encodes a value that is not a `Document` node as an implicit document wrapping it
/// (`e.init()` included).
pub(crate) fn marshal_doc<T: Serialize + ?Sized>(
    e: &mut Encoder<'_>,
    v: &T,
    opts: &Options,
) -> Result<(), Error> {
    e.init()?;
    // The value is captured before its first event is emitted, so a value that fails while
    // being captured (inline-map conflict, flatten, a serde value nested too deep) leaves no
    // partial document behind in an Encoder's stream. A failure during the walk that follows
    // (e.g. a `Document` field whose nodes nest too deep: its depth is counted only on the walk)
    // comes after
    // events were already emitted, so the stream keeps that partial document and the Encoder's
    // later calls fail.
    let content = v.serialize(Capture { opts, depth: 0 })?;
    if let Content::Node(doc) = &content
        && doc[doc.root()].kind == Kind::Document
    {
        // A reference to a document node is encoded as that document.
        return e.marshal_doc_node(doc, doc.root());
    }
    crate::api::yaml_document_start_event_initialize(&mut e.event, None, Vec::new(), true);
    e.emit()?;
    marshal(e, &content)?;
    crate::api::yaml_document_end_event_initialize(&mut e.event, true);
    e.emit()
}

/// Encodes a captured value (the tag is always empty here).
fn marshal(e: &mut Encoder<'_>, c: &Content) -> Result<(), Error> {
    match c {
        Content::Null => e.nilv(),
        Content::Bool(b) => e.boolv("", *b),
        Content::Int(_, i) => e.intv("", *i),
        Content::Uint(_, u) => e.uintv("", *u),
        Content::Float(k, f) => e.floatv("", *f, if *k == ReflectKind::Float32 { 32 } else { 64 }),
        Content::Str(s) => e.stringv("", s),
        Content::Time(t) => e.timev("", t),
        Content::Seq(items) => nested(e, |e| slicev(e, items)),
        Content::Map(entries) | Content::InlineMap(entries) => nested(e, |e| mapv(e, entries)),
        Content::Struct(s) => nested(e, |e| structv(e, s)),
        // A node goes to nodev (its own D7 guard counts on from e.depth).
        Content::Node(doc) => e.nodev(doc, doc.root()),
    }
}

/// Runs `f` one collection level deeper (plan decision D7, on `Encoder::depth` like
/// `Encoder::node`).
fn nested(
    e: &mut Encoder<'_>,
    f: impl FnOnce(&mut Encoder<'_>) -> Result<(), Error>,
) -> Result<(), Error> {
    // Plan decision D7 (see `enter`).
    if e.depth >= MAX_WALK_DEPTH {
        return Err(walk_depth_exceeded());
    }
    e.depth += 1;
    let result = f(e);
    e.depth -= 1;
    result
}

/// Encodes a map; the entries were sorted when captured.
fn mapv(e: &mut Encoder<'_>, entries: &[MapEntry]) -> Result<(), Error> {
    e.mappingv("", |e| {
        for entry in entries {
            marshal(e, &entry.key)?;
            marshal(e, &entry.value)?;
        }
        Ok(())
    })
}

/// Encodes a struct: the fields in order (omitempty fields were skipped by serde),
/// then the `,inline` map's entries, sorted (its key conflicts were checked when captured).
fn structv(e: &mut Encoder<'_>, s: &StructContent) -> Result<(), Error> {
    e.mappingv("", |e| {
        for (key, value) in &s.fields {
            e.stringv("", key.as_bytes())?;
            e.flow = false;
            marshal(e, value)?;
        }
        if !s.inline.is_empty() {
            e.flow = false;
            for entry in &s.inline {
                marshal(e, &entry.key)?;
                e.flow = false;
                marshal(e, &entry.value)?;
            }
        }
        Ok(())
    })
}

/// Encodes a sequence.
fn slicev(e: &mut Encoder<'_>, items: &[Content]) -> Result<(), Error> {
    e.slicev_start("")?;
    for item in items {
        marshal(e, item)?;
    }
    e.slicev_end()
}

/// Serializes a marker newtype's payload with the codec armed and takes the handed-over value.
fn take_handoff<T: Serialize + ?Sized>(
    name: &'static str,
    value: &T,
    c: Capture<'_>,
) -> Result<Handed, Error> {
    ARMED.set(true);
    let result = value.serialize(c);
    ARMED.set(false);
    result?;
    match SLOT.take() {
        Some(handed) => Ok(handed),
        None => Err(Error::Unsupported(format!(
            "serde newtype name {name:?} is reserved by gadk_goyaml"
        ))),
    }
}

/// The serde `Serializer` that captures a value into a [`Content`]. `depth` is the number of
/// collections enclosing the value (plan decision D7).
#[derive(Clone, Copy)]
struct Capture<'o> {
    opts: &'o Options,
    depth: usize,
}

impl<'o> Serializer for Capture<'o> {
    type Ok = Content;
    type Error = Error;
    type SerializeSeq = SeqCapture<'o>;
    type SerializeTuple = SeqCapture<'o>;
    type SerializeTupleStruct = SeqCapture<'o>;
    type SerializeTupleVariant = TupleVariantCapture<'o>;
    type SerializeMap = MapCapture<'o>;
    type SerializeStruct = StructCapture<'o>;
    type SerializeStructVariant = StructVariantCapture<'o>;

    fn serialize_bool(self, v: bool) -> Result<Content, Error> {
        Ok(Content::Bool(v))
    }

    fn serialize_i8(self, v: i8) -> Result<Content, Error> {
        Ok(Content::Int(ReflectKind::Int8, i64::from(v)))
    }

    fn serialize_i16(self, v: i16) -> Result<Content, Error> {
        Ok(Content::Int(ReflectKind::Int16, i64::from(v)))
    }

    fn serialize_i32(self, v: i32) -> Result<Content, Error> {
        Ok(Content::Int(ReflectKind::Int32, i64::from(v)))
    }

    fn serialize_i64(self, v: i64) -> Result<Content, Error> {
        Ok(Content::Int(ReflectKind::Int64, v))
    }

    fn serialize_u8(self, v: u8) -> Result<Content, Error> {
        Ok(Content::Uint(ReflectKind::Uint8, u64::from(v)))
    }

    fn serialize_u16(self, v: u16) -> Result<Content, Error> {
        Ok(Content::Uint(ReflectKind::Uint16, u64::from(v)))
    }

    fn serialize_u32(self, v: u32) -> Result<Content, Error> {
        Ok(Content::Uint(ReflectKind::Uint32, u64::from(v)))
    }

    fn serialize_u64(self, v: u64) -> Result<Content, Error> {
        Ok(Content::Uint(ReflectKind::Uint64, v))
    }

    fn serialize_f32(self, v: f32) -> Result<Content, Error> {
        Ok(Content::Float(ReflectKind::Float32, f64::from(v)))
    }

    fn serialize_f64(self, v: f64) -> Result<Content, Error> {
        Ok(Content::Float(ReflectKind::Float64, v))
    }

    fn serialize_char(self, v: char) -> Result<Content, Error> {
        Ok(Content::Str(v.to_string().into_bytes()))
    }

    fn serialize_str(self, v: &str) -> Result<Content, Error> {
        Ok(Content::Str(v.as_bytes().to_vec()))
    }

    /// Bytes: `slicev` of `u8`s (a `!!int` sequence).
    fn serialize_bytes(self, v: &[u8]) -> Result<Content, Error> {
        enter(self.depth)?;
        Ok(Content::Seq(
            v.iter()
                .map(|b| Content::Uint(ReflectKind::Uint8, u64::from(*b)))
                .collect(),
        ))
    }

    fn serialize_none(self) -> Result<Content, Error> {
        Ok(Content::Null)
    }

    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<Content, Error> {
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<Content, Error> {
        Ok(Content::Null)
    }

    /// A field-less struct encodes as `{}`.
    fn serialize_unit_struct(self, _name: &'static str) -> Result<Content, Error> {
        enter(self.depth)?;
        Ok(Content::Struct(StructContent {
            fields: Vec::new(),
            inline: Vec::new(),
        }))
    }

    /// A unit variant is its name, a string.
    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
    ) -> Result<Content, Error> {
        Ok(Content::Str(variant.as_bytes().to_vec()))
    }

    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        name: &'static str,
        value: &T,
    ) -> Result<Content, Error> {
        match name {
            DOCUMENT_MARKER | TIMESTAMP_MARKER | GO_STRING_MARKER => {
                match (name, take_handoff(name, value, self)?) {
                    (DOCUMENT_MARKER, Handed::Document(doc)) => Ok(Content::Node(doc)),
                    (TIMESTAMP_MARKER, Handed::Time(t)) => Ok(Content::Time(t)),
                    (GO_STRING_MARKER, Handed::GoString(b)) => Ok(Content::Str(b)),
                    _ => Err(Error::Unsupported(format!(
                        "serde newtype name {name:?} is reserved by gadk_goyaml"
                    ))),
                }
            }
            INLINE_MAP_MARKER => match value.serialize(self)? {
                Content::Map(entries) => Ok(Content::InlineMap(entries)),
                other => Ok(other),
            },
            _ => value.serialize(self),
        }
    }

    /// `{variant: value}`.
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<Content, Error> {
        let depth = enter(self.depth)?;
        let value = value.serialize(Capture {
            opts: self.opts,
            depth,
        })?;
        Ok(variant_map(variant, value))
    }

    fn serialize_seq(self, len: Option<usize>) -> Result<SeqCapture<'o>, Error> {
        Ok(SeqCapture {
            inner: Capture {
                opts: self.opts,
                depth: enter(self.depth)?,
            },
            items: Vec::with_capacity(len.unwrap_or(0)),
        })
    }

    fn serialize_tuple(self, len: usize) -> Result<SeqCapture<'o>, Error> {
        self.serialize_seq(Some(len))
    }

    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        len: usize,
    ) -> Result<SeqCapture<'o>, Error> {
        self.serialize_seq(Some(len))
    }

    /// `{variant: [fields…]}`.
    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<TupleVariantCapture<'o>, Error> {
        let outer = Capture {
            opts: self.opts,
            depth: enter(self.depth)?,
        };
        Ok(TupleVariantCapture {
            variant,
            seq: outer.serialize_seq(Some(len))?,
        })
    }

    /// A map; `None` (serde derive's `#[serde(flatten)]`) is rejected.
    fn serialize_map(self, len: Option<usize>) -> Result<MapCapture<'o>, Error> {
        let Some(len) = len else {
            // yaml.v3 has no flatten; serde derive serializes a struct with a
            // `#[serde(flatten)]` field as a map of unknown length.
            return Err(Error::Unsupported(FLATTEN_UNSUPPORTED.to_owned()));
        };
        Ok(MapCapture {
            inner: Capture {
                opts: self.opts,
                depth: enter(self.depth)?,
            },
            entries: Vec::with_capacity(len),
            key: None,
        })
    }

    fn serialize_struct(self, name: &'static str, len: usize) -> Result<StructCapture<'o>, Error> {
        Ok(StructCapture::new(self.opts, name, enter(self.depth)?, len))
    }

    /// `{variant: {fields…}}`; the fields are named by `key_for(variant, field)`.
    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<StructVariantCapture<'o>, Error> {
        let depth = enter(enter(self.depth)?)?;
        Ok(StructVariantCapture {
            variant,
            inner: StructCapture::new(self.opts, variant, depth, len),
        })
    }
}

/// `{variant: value}`: a one-entry map.
fn variant_map(variant: &'static str, value: Content) -> Content {
    Content::Map(vec![MapEntry {
        sort: SortKey::String(variant.to_owned()),
        key: Content::Str(variant.as_bytes().to_vec()),
        value,
    }])
}

/// Captures a sequence (also tuples and tuple structs).
struct SeqCapture<'o> {
    inner: Capture<'o>,
    items: Vec<Content>,
}

impl SerializeSeq for SeqCapture<'_> {
    type Ok = Content;
    type Error = Error;

    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.items.push(value.serialize(self.inner)?);
        Ok(())
    }

    fn end(self) -> Result<Content, Error> {
        Ok(Content::Seq(self.items))
    }
}

impl SerializeTuple for SeqCapture<'_> {
    type Ok = Content;
    type Error = Error;

    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        SerializeSeq::serialize_element(self, value)
    }

    fn end(self) -> Result<Content, Error> {
        SerializeSeq::end(self)
    }
}

impl SerializeTupleStruct for SeqCapture<'_> {
    type Ok = Content;
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        SerializeSeq::serialize_element(self, value)
    }

    fn end(self) -> Result<Content, Error> {
        SerializeSeq::end(self)
    }
}

/// Captures a tuple variant.
struct TupleVariantCapture<'o> {
    variant: &'static str,
    seq: SeqCapture<'o>,
}

impl SerializeTupleVariant for TupleVariantCapture<'_> {
    type Ok = Content;
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        SerializeSeq::serialize_element(&mut self.seq, value)
    }

    fn end(self) -> Result<Content, Error> {
        Ok(variant_map(self.variant, SerializeSeq::end(self.seq)?))
    }
}

/// Captures a map; `end` sorts the entries by key.
struct MapCapture<'o> {
    inner: Capture<'o>,
    entries: Vec<MapEntry>,
    key: Option<Content>,
}

impl SerializeMap for MapCapture<'_> {
    type Ok = Content;
    type Error = Error;

    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), Error> {
        self.key = Some(key.serialize(self.inner)?);
        Ok(())
    }

    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        let key = self.key.take().unwrap_or(Content::Null);
        let value = value.serialize(self.inner)?;
        self.entries.push(MapEntry {
            sort: key.sort_key(),
            key,
            value,
        });
        Ok(())
    }

    fn end(mut self) -> Result<Content, Error> {
        sort_keys(&mut self.entries, |entry| &entry.sort);
        Ok(Content::Map(self.entries))
    }
}

/// Captures a struct: its keys, the set of reported field keys, the `,inline` map and the
/// field values.
struct StructCapture<'o> {
    inner: Capture<'o>,
    name: &'static str,
    fields: Vec<(String, Content)>,
    /// The keys of every field serde reports (serialized or skipped by
    /// `skip_serializing_if`; `#[serde(skip)]` fields are never reported).
    keys: BTreeSet<String>,
    inline: Option<Vec<MapEntry>>,
}

impl<'o> StructCapture<'o> {
    fn new(opts: &'o Options, name: &'static str, depth: usize, len: usize) -> StructCapture<'o> {
        StructCapture {
            inner: Capture { opts, depth },
            name,
            fields: Vec::with_capacity(len),
            keys: BTreeSet::new(),
            inline: None,
        }
    }

    /// The key for a field, with a duplicate check.
    fn add_key(&mut self, field: &'static str) -> Result<String, Error> {
        let key = self.inner.opts.key_for(self.name, field).into_owned();
        if !self.keys.insert(key.clone()) {
            // yaml.v3 panics with this error; here it is returned. The struct is named by its
            // serde name.
            return Err(Error::Unsupported(format!(
                "duplicated key '{key}' in struct {}",
                self.name
            )));
        }
        Ok(key)
    }

    fn field<T: Serialize + ?Sized>(
        &mut self,
        field: &'static str,
        value: &T,
    ) -> Result<(), Error> {
        if field == INLINE_FIELD {
            if self.inline.is_some() {
                return Err(Error::Unsupported(format!(
                    "multiple ,inline maps in struct {}",
                    self.name
                )));
            }
            // The entries sit in the struct's own mapping, one level above the InlineMap's.
            let content = value.serialize(Capture {
                opts: self.inner.opts,
                depth: self.inner.depth - 1,
            })?;
            let Content::InlineMap(entries) = content else {
                return Err(Error::Unsupported(format!(
                    "field {INLINE_FIELD:?} of struct {} must be a gadk_goyaml::InlineMap",
                    self.name
                )));
            };
            self.inline = Some(entries);
            return Ok(());
        }
        let key = self.add_key(field)?;
        let value = value.serialize(self.inner)?;
        self.fields.push((key, value));
        Ok(())
    }

    fn skip(&mut self, field: &'static str) -> Result<(), Error> {
        if field != INLINE_FIELD {
            self.add_key(field)?;
        }
        Ok(())
    }

    /// The inline-map conflict check, in sorted key order.
    fn finish(self) -> Result<Content, Error> {
        let inline = self.inline.unwrap_or_default();
        for entry in &inline {
            let key = match &entry.key {
                Content::Str(b) => String::from_utf8_lossy(b).into_owned(),
                _ => continue,
            };
            if self.keys.contains(&key) {
                // yaml.v3 panics with this text; here it is returned.
                return Err(Error::Unsupported(format!(
                    "cannot have key {} in inlined map: conflicts with struct field",
                    gadk_platform::go_quote(&key)
                )));
            }
        }
        Ok(Content::Struct(StructContent {
            fields: self.fields,
            inline,
        }))
    }
}

impl SerializeStruct for StructCapture<'_> {
    type Ok = Content;
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Error> {
        self.field(key, value)
    }

    fn skip_field(&mut self, key: &'static str) -> Result<(), Error> {
        self.skip(key)
    }

    fn end(self) -> Result<Content, Error> {
        self.finish()
    }
}

/// Captures a struct variant.
struct StructVariantCapture<'o> {
    variant: &'static str,
    inner: StructCapture<'o>,
}

impl SerializeStructVariant for StructVariantCapture<'_> {
    type Ok = Content;
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Error> {
        self.inner.field(key, value)
    }

    fn skip_field(&mut self, key: &'static str) -> Result<(), Error> {
        self.inner.skip(key)
    }

    fn end(self) -> Result<Content, Error> {
        Ok(variant_map(self.variant, self.inner.finish()?))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde::Serialize;

    use super::*;
    use crate::InlineMap;

    /// `marshal` with `opts`, as text.
    fn yaml_with<T: Serialize + ?Sized>(v: &T, opts: &Options) -> Result<String, String> {
        crate::marshal(v, opts)
            .map(|b| String::from_utf8(b).unwrap())
            .map_err(|e| e.to_string())
    }

    /// `marshal` with default options, as text (expected values printed by Go yaml.v3 v3.0.1).
    fn yaml<T: Serialize + ?Sized>(v: &T) -> String {
        yaml_with(v, &Options::default()).unwrap()
    }

    #[allow(non_snake_case)] // A Go-style field name.
    #[derive(Serialize)]
    struct Agent {
        user_message_index: i64,
        agentName: String,
    }

    #[test]
    fn struct_keys_follow_key_for() {
        let agent = Agent {
            user_message_index: 1,
            agentName: "x".into(),
        };
        // Go's default key: the field name lowercased (`UserMessageIndex`, `AgentName`).
        assert_eq!(yaml(&agent), "usermessageindex: 1\nagentname: x\n");
        let mut opts = Options::default();
        opts.verbatim_structs.insert("Agent");
        assert_eq!(
            yaml_with(&agent, &opts).unwrap(),
            "user_message_index: 1\nagentName: x\n"
        );
        opts.field_keys.insert(("Agent", "agentName"), "agent");
        assert_eq!(
            yaml_with(&agent, &opts).unwrap(),
            "user_message_index: 1\nagent: x\n"
        );
    }

    #[derive(Serialize)]
    struct WithExtra {
        name: String,
        #[serde(skip_serializing_if = "is_zero")]
        count: i64,
        #[serde(rename = "$gadk_goyaml::inline")]
        extra: InlineMap,
    }

    fn is_zero(v: &i64) -> bool {
        *v == 0
    }

    fn with_extra(extra: &[(&str, i64)]) -> WithExtra {
        WithExtra {
            name: "n".into(),
            count: 0,
            extra: InlineMap(
                extra
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), GoValue::Int(*v)))
                    .collect(),
            ),
        }
    }

    #[test]
    fn inline_map_entries_follow_the_fields_sorted() {
        assert_eq!(
            yaml(&with_extra(&[("b", 1), ("a", 2)])),
            "name: \"n\"\na: 2\nb: 1\n"
        );
        assert_eq!(yaml(&with_extra(&[])), "name: \"n\"\n");
    }

    #[test]
    fn inline_map_key_conflicting_with_a_field_fails() {
        let err = crate::marshal(&with_extra(&[("name", 1)]), &Options::default()).unwrap_err();
        assert!(matches!(&err, Error::Unsupported(_)));
        assert_eq!(
            err.to_string(),
            "cannot have key \"name\" in inlined map: conflicts with struct field"
        );
        // A field omitted by omitempty is still a struct field (Go's FieldsMap); the first
        // conflict in sorted key order is reported.
        assert_eq!(
            yaml_with(&with_extra(&[("zz", 2), ("count", 1)]), &Options::default()).unwrap_err(),
            "cannot have key \"count\" in inlined map: conflicts with struct field"
        );
    }

    #[derive(Serialize)]
    struct BadInline {
        #[serde(rename = "$gadk_goyaml::inline")]
        extra: BTreeMap<String, i64>,
    }

    /// Two fields with one key, and two inline maps: Go's getStructInfo errors.
    struct Twice(&'static str);

    impl Serialize for Twice {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let mut s = serializer.serialize_struct("Twice", 2)?;
            if self.0 == INLINE_FIELD {
                s.serialize_field(self.0, &InlineMap::new())?;
                s.serialize_field(self.0, &InlineMap::new())?;
            } else {
                s.serialize_field(self.0, &1)?;
                s.skip_field("a_b")?;
            }
            s.end()
        }
    }

    #[test]
    fn struct_info_errors() {
        assert_eq!(
            yaml_with(
                &BadInline {
                    extra: BTreeMap::new()
                },
                &Options::default()
            )
            .unwrap_err(),
            "field \"$gadk_goyaml::inline\" of struct BadInline must be a gadk_goyaml::InlineMap"
        );
        assert_eq!(
            yaml_with(&Twice("ab"), &Options::default()).unwrap_err(),
            "duplicated key 'ab' in struct Twice"
        );
        assert_eq!(
            yaml_with(&Twice(INLINE_FIELD), &Options::default()).unwrap_err(),
            "multiple ,inline maps in struct Twice"
        );
    }

    #[derive(Serialize)]
    struct Inner {
        a: i64,
    }

    #[derive(Serialize)]
    struct Flattened {
        name: String,
        #[serde(flatten)]
        inner: Inner,
    }

    #[test]
    fn flatten_is_rejected() {
        let v = Flattened {
            name: "n".into(),
            inner: Inner { a: 1 },
        };
        let err = crate::marshal(&v, &Options::default()).unwrap_err();
        assert!(matches!(&err, Error::Unsupported(_)));
        assert_eq!(err.to_string(), FLATTEN_UNSUPPORTED);
        assert!(crate::to_node(&v, &Options::default()).is_err());
        let mut out = Vec::new();
        let mut enc = crate::Encoder::new(&mut out);
        assert_eq!(
            enc.encode(&v, &Options::default()).unwrap_err().to_string(),
            "serde flatten / maps of unknown length are unsupported"
        );
    }

    #[test]
    fn non_utf8_go_string_is_binary() {
        assert_eq!(yaml(&GoValue::Bytes(vec![0xff, 0xfe])), "!!binary //4=\n");
        assert_eq!(
            yaml(&GoValue::Map(vec![(
                "x".into(),
                GoValue::Bytes(vec![0xff, 0xfe])
            )])),
            "x: !!binary //4=\n"
        );
        // Valid UTF-8 bytes are an ordinary string.
        assert_eq!(yaml(&GoValue::Bytes(b"yes".to_vec())), "\"yes\"\n");
        // Through other formats: bytes.
        assert_eq!(
            serde_json::to_string(&GoValue::Bytes(vec![1, 255])).unwrap(),
            "[1,255]"
        );
    }

    #[test]
    fn floats_use_go_format_float_g() {
        let f64s: [(f64, &str); 13] = [
            (0.1, "0.1\n"),
            (1e6, "1e+06\n"),
            (123456.0, "123456\n"),
            (1e-5, "1e-05\n"),
            (1e-4, "0.0001\n"),
            (-0.0, "-0\n"),
            (f64::INFINITY, ".inf\n"),
            (f64::NEG_INFINITY, "-.inf\n"),
            (f64::NAN, ".nan\n"),
            (1.0, "1\n"),
            (f64::MAX, "1.7976931348623157e+308\n"),
            (5e-324, "5e-324\n"),
            (1e21, "1e+21\n"),
        ];
        for (f, want) in f64s {
            assert_eq!(yaml(&f), want, "{f:?}");
            assert_eq!(yaml(&GoValue::Float(f)), want, "{f:?}");
        }
        let f32s: [(f32, &str); 4] = [
            (0.1, "0.1\n"),
            (16_777_216.0, "1.6777216e+07\n"),
            (f32::MAX, "3.4028235e+38\n"),
            (1e-7, "1e-07\n"),
        ];
        for (f, want) in f32s {
            assert_eq!(yaml(&f), want, "{f:?}");
        }
    }

    #[derive(Serialize)]
    enum Variant {
        Unit,
        New(i64),
        Tup(i64, String),
        Rec { a_b: i64 },
    }

    #[derive(Serialize)]
    struct Empty;

    #[derive(Serialize)]
    struct EmptyBraces {}

    #[test]
    fn serde_data_model_forms() {
        let variants = vec![
            Variant::Unit,
            Variant::New(1),
            Variant::Tup(2, "x".into()),
            Variant::Rec { a_b: 3 },
        ];
        assert_eq!(
            yaml(&variants),
            "- Unit\n- New: 1\n- Tup:\n    - 2\n    - x\n- Rec:\n    ab: 3\n"
        );
        // serialize_bytes: Go's []byte, a sequence of !!int.
        assert_eq!(yaml(&ByteSlice(&[1, 255])), "- 1\n- 255\n");
        let doc = crate::to_node(&ByteSlice(&[7]), &Options::default()).unwrap();
        let item = doc[doc.root()].content[0];
        assert_eq!(doc[item].tag, "!!int");
        // None / unit → null; Go's struct{} → {}; a char is a one-rune string.
        assert_eq!(yaml(&(None::<i64>, (), 'c')), "- null\n- null\n- c\n");
        assert_eq!(yaml(&Empty), "{}\n");
        assert_eq!(yaml(&EmptyBraces {}), "{}\n");
        assert_eq!(
            yaml(&vec!["multi\nline", "yes", "1.5", ""]),
            "- |-\n  multi\n  line\n- \"yes\"\n- \"1.5\"\n- \"\"\n"
        );
    }

    /// A `serialize_bytes` value.
    struct ByteSlice<'a>(&'a [u8]);

    impl Serialize for ByteSlice<'_> {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            serializer.serialize_bytes(self.0)
        }
    }

    #[test]
    fn map_keys_sort_like_go() {
        let m: BTreeMap<&str, i64> = [("a10", 1), ("a2", 2), ("b", 3), ("B", 4), ("1", 5)]
            .into_iter()
            .collect();
        assert_eq!(yaml(&m), "\"1\": 5\nB: 4\na2: 2\na10: 1\nb: 3\n");
        let any = GoValue::MapAny(vec![
            (GoValue::Int(2), GoValue::String("i".into())),
            (GoValue::String("a".into()), GoValue::String("s".into())),
            (GoValue::Bool(true), GoValue::String("b".into())),
            (GoValue::Float(1.5), GoValue::String("f".into())),
            (GoValue::Null, GoValue::String("n".into())),
            (GoValue::Uint(3), GoValue::String("u".into())),
        ]);
        assert_eq!(
            yaml(&any),
            "true: b\n1.5: f\n2: i\n3: u\nnull: \"n\"\na: s\n"
        );
    }

    #[test]
    fn timestamps_use_timev() {
        let t = DateTime::parse_from_rfc3339("2001-12-14T21:59:43.1-05:00").unwrap();
        assert_eq!(
            yaml(&GoValue::Timestamp(t)),
            "2001-12-14T21:59:43.1-05:00\n"
        );
        assert_eq!(
            yaml(&GoValue::Map(vec![("t".into(), GoValue::Timestamp(t))])),
            "t: 2001-12-14T21:59:43.1-05:00\n"
        );
        assert_eq!(
            serde_json::to_string(&GoValue::Timestamp(t)).unwrap(),
            "\"2001-12-14T21:59:43.1-05:00\""
        );
    }

    #[derive(Serialize)]
    struct WithDoc {
        spec: Document,
    }

    #[test]
    fn documents_go_through_nodev() {
        let doc = crate::unmarshal_node(b"# head\na: [1, 2] # line\n").unwrap();
        // marshal(&doc) of a document node is Marshal(&node).
        assert_eq!(
            crate::marshal(&doc, &Options::default()).unwrap(),
            crate::marshal_node(&doc, doc.root()).unwrap()
        );
        let first = doc[doc.root()].content[0];
        let field = WithDoc {
            spec: doc.subtree(first),
        };
        assert_eq!(yaml(&field), "spec:\n    # head\n    a: [1, 2] # line\n");
        assert_eq!(
            yaml(&WithDoc {
                spec: Document::default()
            }),
            "spec: null\n"
        );
        // Other formats see unit; the marker name is reserved.
        assert_eq!(serde_json::to_string(&doc).unwrap(), "null");
        struct Fake;
        impl Serialize for Fake {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_newtype_struct(DOCUMENT_MARKER, &1)
            }
        }
        assert_eq!(
            yaml_with(&Fake, &Options::default()).unwrap_err(),
            "serde newtype name \"$gadk_goyaml::Document\" is reserved by gadk_goyaml"
        );
    }

    #[test]
    fn to_node_and_encoder() {
        let doc = crate::to_node(&5i64, &Options::default()).unwrap();
        let n = &doc[doc.root()];
        assert_eq!(
            (n.kind, n.tag.as_str(), n.value.as_str()),
            (Kind::Scalar, "!!int", "5")
        );
        assert_eq!((n.line, n.column), (0, 0));
        let mut out = Vec::new();
        let mut enc = crate::Encoder::new(&mut out);
        enc.set_indent(2);
        enc.encode(&BTreeMap::from([("a", vec![1])]), &Options::default())
            .unwrap();
        enc.encode(&GoValue::Null, &Options::default()).unwrap();
        enc.close().unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "a:\n  - 1\n---\nnull\n");
    }

    #[derive(Serialize)]
    struct Nest {
        child: Option<Box<Nest>>,
        doc: Option<Document>,
    }

    /// `levels` nested `Nest` mappings; the innermost holds `doc`.
    fn nest(levels: usize, doc: Option<Document>) -> Nest {
        let mut n = Nest { child: None, doc };
        for _ in 1..levels {
            n = Nest {
                child: Some(Box::new(n)),
                doc: None,
            };
        }
        n
    }

    /// `levels` nested sequences.
    fn deep_seq(levels: usize) -> GoValue {
        let mut v = GoValue::Seq(Vec::new());
        for _ in 1..levels {
            v = GoValue::Seq(vec![v]);
        }
        v
    }

    #[test]
    fn depth_limit_127() {
        let depth_err = "yaml: exceeded max depth of 127";
        let opts = Options::default();
        assert!(yaml_with(&nest(127, None), &opts).is_ok());
        assert_eq!(yaml_with(&nest(128, None), &opts).unwrap_err(), depth_err);
        assert!(yaml_with(&deep_seq(127), &opts).is_ok());
        assert_eq!(yaml_with(&deep_seq(128), &opts).unwrap_err(), depth_err);
        // A Document field continues the count (nodev's guard on the same Encoder::depth).
        let one = crate::unmarshal_node(b"a: 1\n").unwrap();
        let one = one.subtree(one[one.root()].content[0]);
        let two = crate::unmarshal_node(b"a: {b: 1}\n").unwrap();
        let two = two.subtree(two[two.root()].content[0]);
        assert!(yaml_with(&nest(126, Some(one)), &opts).is_ok());
        assert_eq!(
            yaml_with(&nest(126, Some(two)), &opts).unwrap_err(),
            depth_err
        );
        // Variants: `{variant: [..]}` is two levels.
        #[derive(Serialize)]
        enum E {
            T(GoValue, i64),
        }
        assert!(yaml_with(&E::T(deep_seq(125), 0), &opts).is_ok());
        assert_eq!(
            yaml_with(&E::T(deep_seq(126), 0), &opts).unwrap_err(),
            depth_err
        );
    }
}
