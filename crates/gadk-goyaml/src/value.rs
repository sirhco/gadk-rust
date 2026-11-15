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

//! [`GoValue`]: what yaml.v3 v3.0.1 decodes into a Go `interface{}` (`decode.go` `scalar`,
//! `sequence` and `mapping` with an interface target), and the decode of a node into it,
//! sharing `de.rs`'s [`DecodeState`] (alias limits, merges, type errors). Its `Serialize` impl
//! (Go's `encoder.marshal` of those values) is in `ser.rs`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;

use chrono::{DateTime, Datelike, FixedOffset, Timelike};
use serde::de::value::StringDeserializer;
use serde::de::{
    self, Deserialize, DeserializeSeed, Deserializer, IntoDeserializer, MapAccess, SeqAccess,
    Visitor,
};

use crate::de::{
    DecodeState, check_unique_keys, fail_want_map, is_merge, is_string_map, resolve_scalar,
    unmarshal_string,
};
use crate::error::{Error, failf};
use crate::node::{Document, Kind, NodeId};
use crate::resolve::NULL_TAG;

/// The serde newtype-struct name [`GoValue`]'s `Deserialize` impl asks for: gadk-goyaml's
/// deserializer answers it with the exact value decoded by Go's rules, handed over through a
/// thread-local slot (the `serde_json::RawValue` technique); other deserializers ignore the
/// name and the value is built from their data model.
pub(crate) const GO_VALUE_MARKER: &str = "$gadk_goyaml::GoValue";

/// A Go value of dynamic type, as yaml.v3 decodes into `interface{}`.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum GoValue {
    /// `nil`.
    #[default]
    Null,
    /// `bool`.
    Bool(bool),
    /// `int` (64-bit).
    Int(i64),
    /// `uint64` (integers above `math.MaxInt64`).
    Uint(u64),
    /// `float64`.
    Float(f64),
    /// `string` (valid UTF-8), also for `!!binary` whose decoded bytes are valid UTF-8.
    String(String),
    /// A Go `string` that is not valid UTF-8 (only from `!!binary`).
    // Divergence (ledger 6c-1): Go yields a `string`; a Rust `String` must be UTF-8.
    Bytes(Vec<u8>),
    /// `time.Time` (an instant with nanoseconds and its offset).
    Timestamp(DateTime<FixedOffset>),
    /// `[]interface{}`.
    Seq(Vec<GoValue>),
    /// `map[string]interface{}` (every key a string), in document order (encode sorts).
    Map(Vec<(String, GoValue)>),
    /// `map[interface{}]interface{}`, in document order (encode sorts).
    MapAny(Vec<(GoValue, GoValue)>),
}

thread_local! {
    /// The value handed from the decoder to [`GoValue`]'s visitor (see [`GO_VALUE_MARKER`]).
    static GO_VALUE_SLOT: RefCell<Option<GoValue>> = const { RefCell::new(None) };
}

/// Calls `visitor.visit_newtype_struct` with `v` waiting in the hand-over slot; the slot is
/// cleared afterwards whether or not the visitor took it.
pub(crate) fn hand_over<'de, V: Visitor<'de>>(v: GoValue, visitor: V) -> Result<V::Value, Error> {
    GO_VALUE_SLOT.with(|s| *s.borrow_mut() = Some(v));
    let r = visitor.visit_newtype_struct(GoValueDeserializer(GoValue::Null));
    GO_VALUE_SLOT.with(|s| s.borrow_mut().take());
    r
}

/// Go: `(*decoder).unmarshal(n, out)` with `out` an `interface{}`: the decoded value, or
/// `None` where Go's `unmarshal` returns `good == false` (the `interface{}` stays nil and
/// callers skip it, as Go's `sequence`/`mapping` do).
pub(crate) fn unmarshal_value(
    d: &mut DecodeState<'_>,
    doc: &Document,
    n: NodeId,
) -> Result<Option<GoValue>, Error> {
    // Go: decode.go unmarshal (interface target)
    let entered = d.enter(doc, n)?;
    let result = match entered.node {
        Some(n) => unmarshal_value_entered(d, doc, n),
        // Go's `document` with other than one content node returns false.
        None => Ok(None),
    };
    let document = entered.document;
    d.leave(entered);
    // Go's `document` reports success whatever its content's result.
    result.map(|v| {
        if document {
            Some(v.unwrap_or_default())
        } else {
            v
        }
    })
}

/// The body of Go's `unmarshal` with an `interface{}` target once `n` is neither a document
/// nor an alias (its counters, aliases and depth already accounted by
/// [`DecodeState::enter`]).
pub(crate) fn unmarshal_value_entered(
    d: &mut DecodeState<'_>,
    doc: &Document,
    n: NodeId,
) -> Result<Option<GoValue>, Error> {
    // Go: decode.go unmarshal (kind switch, interface target)
    let node = &doc[n];
    match node.kind {
        // Go's `scalar`: a nil resolved value is `null(out)`, which sets the interface to nil
        // and succeeds; anything else is stored as resolved.
        Kind::Scalar => Ok(Some(resolve_scalar(node)?.1)),
        Kind::Mapping => mapping_value(d, doc, n),
        Kind::Sequence => sequence_value(d, doc, n),
        Kind::Zero if node.is_zero() => Ok(Some(GoValue::Null)),
        _ => Err(failf(format_args!(
            "cannot decode node with unknown kind {}",
            node.kind.bits()
        ))),
    }
}

/// Go: `(*decoder).sequence` with an `interface{}` target: a `[]interface{}` of the items
/// that decoded.
fn sequence_value(
    d: &mut DecodeState<'_>,
    doc: &Document,
    n: NodeId,
) -> Result<Option<GoValue>, Error> {
    // Go: decode.go sequence
    let content = &doc[n].content;
    let mut out = Vec::with_capacity(content.len());
    for &item in content {
        if let Some(v) = unmarshal_value(d, doc, item)? {
            out.push(v);
        }
    }
    Ok(Some(GoValue::Seq(out)))
}

/// A Go map being filled: `map[string]interface{}` or `map[interface{}]interface{}`.
enum MapOut {
    Str(Vec<(String, GoValue)>, HashMap<String, usize>),
    Any(Vec<(GoValue, GoValue)>),
}

impl MapOut {
    fn new(string_map: bool) -> MapOut {
        if string_map {
            MapOut::Str(Vec::new(), HashMap::new())
        } else {
            MapOut::Any(Vec::new())
        }
    }

    fn is_string_map(&self) -> bool {
        matches!(self, MapOut::Str(..))
    }

    /// Go's `out.MapIndex(k).IsValid()`.
    fn contains(&self, k: &GoValue) -> bool {
        match (self, k) {
            (MapOut::Str(_, index), GoValue::String(s)) => index.contains_key(s),
            (MapOut::Str(..), _) => false,
            (MapOut::Any(entries), k) => entries.iter().any(|(e, _)| e == k),
        }
    }

    /// Go's `out.SetMapIndex(k, v)`: an existing equal key keeps its position.
    fn insert(&mut self, k: GoValue, v: GoValue) {
        match self {
            MapOut::Str(entries, index) => {
                let GoValue::String(k) = k else {
                    return;
                };
                match index.get(&k) {
                    Some(&i) => entries[i].1 = v,
                    None => {
                        index.insert(k.clone(), entries.len());
                        entries.push((k, v));
                    }
                }
            }
            MapOut::Any(entries) => match entries.iter_mut().find(|(e, _)| *e == k) {
                Some(entry) => entry.1 = v,
                None => entries.push((k, v)),
            },
        }
    }

    fn into_value(self) -> GoValue {
        match self {
            MapOut::Str(entries, _) => GoValue::Map(entries),
            MapOut::Any(entries) => GoValue::MapAny(entries),
        }
    }
}

/// Go: `(*decoder).mapping` with an `interface{}` target: the unique-keys check, then a
/// `map[string]interface{}` when every key is `!!str`/`!!merge` (`isStringMap`), else a
/// `map[interface{}]interface{}`.
fn mapping_value(
    d: &mut DecodeState<'_>,
    doc: &Document,
    n: NodeId,
) -> Result<Option<GoValue>, Error> {
    // Go: decode.go mapping (interface target)
    if !check_unique_keys(d, doc, n) {
        return Ok(None);
    }
    let mut out = MapOut::new(is_string_map(doc, n));
    mapping_loop(d, doc, n, &mut out)?;
    Ok(Some(out.into_value()))
}

/// Go: the pair loop of `(*decoder).mapping` with a map target (after the unique-keys check),
/// then `merge` for a `<<` key.
fn mapping_loop(
    d: &mut DecodeState<'_>,
    doc: &Document,
    n: NodeId,
    out: &mut MapOut,
) -> Result<(), Error> {
    // Go: decode.go mapping (map path)
    let mut merged_fields = d.merged_fields.take();
    let mut merge_node = None;
    for pair in doc[n].content.as_chunks::<2>().0 {
        let (key, value) = (pair[0], pair[1]);
        if is_merge(&doc[key]) {
            merge_node = Some(value);
            continue;
        }
        let k = if out.is_string_map() {
            unmarshal_string(d, doc, key)?.map(GoValue::String)
        } else {
            unmarshal_value(d, doc, key)?
        };
        let Some(k) = k else {
            continue;
        };
        if let Some(mf) = &mut merged_fields {
            if mf.contains(&k) {
                continue;
            }
            mf.push(k.clone());
        }
        if matches!(k, GoValue::Seq(_) | GoValue::Map(_) | GoValue::MapAny(_)) {
            return Err(failf(format_args!("invalid map key: {}", go_sharp_v(&k))));
        }
        match unmarshal_value(d, doc, value)? {
            Some(e) => out.insert(k, e),
            // Go's `mapIsNew` is false for a map made for an interface target.
            None if doc.short_tag(value) == NULL_TAG && !out.contains(&k) => {
                out.insert(k, GoValue::Null)
            }
            None => {}
        }
    }
    d.merged_fields = merged_fields;
    if let Some(m) = merge_node {
        merge(d, doc, n, m, out)?;
    }
    Ok(())
}

/// Go: `(*decoder).merge` into a map: the keys of `parent` are recorded (decoded as
/// `interface{}`) unless an enclosing merge already records them, then each merged mapping
/// is decoded into the same map, skipping recorded keys.
fn merge(
    d: &mut DecodeState<'_>,
    doc: &Document,
    parent: NodeId,
    merge: NodeId,
    out: &mut MapOut,
) -> Result<(), Error> {
    // Go: decode.go merge
    let restore_none = d.merged_fields.is_none();
    if restore_none {
        d.merged_fields = Some(Vec::new());
        for pair in doc[parent].content.as_chunks::<2>().0 {
            if let Some(k) = unmarshal_value(d, doc, pair[0])?
                && let Some(mf) = &mut d.merged_fields
            {
                mf.push(k);
            }
        }
    }

    let m = &doc[merge];
    match m.kind {
        Kind::Mapping => {
            unmarshal_into_map(d, doc, merge, out)?;
        }
        Kind::Alias => {
            if let Some(target) = m.alias
                && doc[target].kind != Kind::Mapping
            {
                return Err(fail_want_map());
            }
            unmarshal_into_map(d, doc, merge, out)?;
        }
        Kind::Sequence => {
            for &ni in &m.content {
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
                unmarshal_into_map(d, doc, ni, out)?;
            }
        }
        _ => return Err(fail_want_map()),
    }

    if restore_none {
        d.merged_fields = None;
    }
    Ok(())
}

/// Go: `d.unmarshal(n, out)` with `out` a map value (a merged mapping or an alias of one).
fn unmarshal_into_map(
    d: &mut DecodeState<'_>,
    doc: &Document,
    n: NodeId,
    out: &mut MapOut,
) -> Result<bool, Error> {
    // Go: decode.go unmarshal (map target)
    let entered = d.enter(doc, n)?;
    let result = match entered.node {
        Some(m) if doc[m].kind == Kind::Mapping => {
            if check_unique_keys(d, doc, m) {
                mapping_loop(d, doc, m, out).map(|()| true)
            } else {
                Ok(false)
            }
        }
        // `merge` only passes mappings and aliases of mappings.
        Some(_) => Err(fail_want_map()),
        None => Ok(false),
    };
    d.leave(entered);
    result
}

/// Go's `fmt.Sprintf("%#v", v)` of a decoded `interface{}` value (used by
/// `invalid map key: %#v`).
pub(crate) fn go_sharp_v(v: &GoValue) -> String {
    let mut out = String::new();
    write_sharp_v(&mut out, v, true);
    out
}

/// Go's `%#v` of `v`; `top` is false for an element held in an `interface{}` slot (`nil`
/// prints `interface {}(nil)` there).
fn write_sharp_v(out: &mut String, v: &GoValue, top: bool) {
    match v {
        GoValue::Null if top => out.push_str("<nil>"),
        GoValue::Null => out.push_str("interface {}(nil)"),
        GoValue::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        GoValue::Int(i) => out.push_str(&i.to_string()),
        GoValue::Uint(u) => out.push_str(&format!("0x{u:x}")),
        GoValue::Float(f) => out.push_str(&gadk_platform::go_format_float_g(*f, 64)),
        GoValue::String(s) => out.push_str(&gadk_platform::go_quote(s)),
        GoValue::Bytes(b) => out.push_str(&gadk_platform::go_quote_bytes(b)),
        GoValue::Timestamp(t) => out.push_str(&go_time_string(t)),
        GoValue::Seq(items) => {
            out.push_str("[]interface {}{");
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_sharp_v(out, item, false);
            }
            out.push('}');
        }
        GoValue::Map(entries) => {
            // Go's fmt prints maps sorted by key.
            let mut sorted: Vec<&(String, GoValue)> = entries.iter().collect();
            sorted.sort_by(|a, b| a.0.cmp(&b.0));
            out.push_str("map[string]interface {}{");
            for (i, (k, v)) in sorted.into_iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&gadk_platform::go_quote(k));
                out.push(':');
                write_sharp_v(out, v, false);
            }
            out.push('}');
        }
        GoValue::MapAny(entries) => {
            // Go's fmtsort orders interface keys by their dynamic type's descriptor address
            // first, which no port can reproduce; keys are ordered by kind, then by text.
            let mut sorted: Vec<(String, String)> = entries
                .iter()
                .map(|(k, v)| {
                    let mut ks = String::new();
                    write_sharp_v(&mut ks, k, false);
                    let mut vs = String::new();
                    write_sharp_v(&mut vs, v, false);
                    (format!("{}{ks}", kind_rank(k)), format!("{ks}:{vs}"))
                })
                .collect();
            sorted.sort();
            out.push_str("map[interface {}]interface {}{");
            for (i, (_, entry)) in sorted.into_iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&entry);
            }
            out.push('}');
        }
    }
}

/// A sort rank of a key's Go kind for [`write_sharp_v`].
fn kind_rank(v: &GoValue) -> char {
    match v {
        GoValue::Null => '0',
        GoValue::Bool(_) => '1',
        GoValue::Int(_) => '2',
        GoValue::Uint(_) => '3',
        GoValue::Float(_) => '4',
        GoValue::String(_) | GoValue::Bytes(_) => '5',
        GoValue::Timestamp(_) => '6',
        GoValue::Seq(_) => '7',
        GoValue::Map(_) | GoValue::MapAny(_) => '8',
    }
}

/// Go's `time.Time.GoString` (`time.Date(2001, time.December, 14, 21, 59, 43, 0, time.UTC)`).
/// Go's parser yields `time.UTC` for `Z` and a fixed zone named `""` otherwise (or
/// `time.Local` when the offset is the machine's; not reproduced).
fn go_time_string(t: &DateTime<FixedOffset>) -> String {
    const MONTHS: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    let loc = if t.offset().local_minus_utc() == 0 {
        "time.UTC"
    } else {
        "time.Location(\"\")"
    };
    format!(
        "time.Date({}, time.{}, {}, {}, {}, {}, {}, {loc})",
        t.year(),
        MONTHS[t.month0() as usize],
        t.day(),
        t.hour(),
        t.minute(),
        t.second(),
        t.nanosecond()
    )
}

/// Hands a [`GoValue`] to a visitor: through [`GO_VALUE_MARKER`] exactly, otherwise by its
/// data-model shape (`Timestamp` as its RFC 3339 text, `Bytes` as bytes, `MapAny` keys as
/// values).
pub(crate) struct GoValueDeserializer(pub(crate) GoValue);

impl<'de> Deserializer<'de> for GoValueDeserializer {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self.0 {
            GoValue::Null => visitor.visit_unit(),
            GoValue::Bool(b) => visitor.visit_bool(b),
            GoValue::Int(i) => visitor.visit_i64(i),
            GoValue::Uint(u) => visitor.visit_u64(u),
            GoValue::Float(f) => visitor.visit_f64(f),
            GoValue::String(s) => visitor.visit_string(s),
            GoValue::Bytes(b) => visitor.visit_byte_buf(b),
            GoValue::Timestamp(t) => {
                visitor.visit_string(gadk_platform::go_format_rfc3339_nano(&t))
            }
            GoValue::Seq(items) => visitor.visit_seq(GoSeq(items.into_iter())),
            GoValue::Map(entries) => visitor.visit_map(GoMap {
                entries: entries
                    .into_iter()
                    .map(|(k, v)| (GoValue::String(k), v))
                    .collect::<Vec<_>>()
                    .into_iter(),
                value: None,
            }),
            GoValue::MapAny(entries) => visitor.visit_map(GoMap {
                entries: entries.into_iter(),
                value: None,
            }),
        }
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self.0 {
            GoValue::Null => visitor.visit_none(),
            _ => visitor.visit_some(self),
        }
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Error> {
        if name == GO_VALUE_MARKER {
            return hand_over(self.0, visitor);
        }
        visitor.visit_newtype_struct(self)
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes byte_buf
        unit unit_struct seq tuple tuple_struct map struct enum identifier ignored_any
    }
}

/// The items of a [`GoValue::Seq`] as a serde sequence.
struct GoSeq(std::vec::IntoIter<GoValue>);

impl<'de> SeqAccess<'de> for GoSeq {
    type Error = Error;

    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Error> {
        self.0
            .next()
            .map(|v| seed.deserialize(GoValueDeserializer(v)))
            .transpose()
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.0.len())
    }
}

/// The entries of a [`GoValue::Map`] / [`GoValue::MapAny`] as a serde map.
struct GoMap {
    entries: std::vec::IntoIter<(GoValue, GoValue)>,
    value: Option<GoValue>,
}

impl<'de> MapAccess<'de> for GoMap {
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Error> {
        match self.entries.next() {
            Some((k, v)) => {
                self.value = Some(v);
                seed.deserialize(GoValueDeserializer(k)).map(Some)
            }
            None => Ok(None),
        }
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, Error> {
        seed.deserialize(GoValueDeserializer(self.value.take().unwrap_or_default()))
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.entries.len())
    }
}

/// The entries of an inline catch-all as a serde map of `String` → [`GoValue`].
pub(crate) struct InlineEntries {
    pub(crate) entries: std::vec::IntoIter<(String, GoValue)>,
    pub(crate) value: Option<GoValue>,
}

impl<'de> MapAccess<'de> for InlineEntries {
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Error> {
        match self.entries.next() {
            Some((k, v)) => {
                self.value = Some(v);
                let key: StringDeserializer<Error> = k.into_deserializer();
                seed.deserialize(key).map(Some)
            }
            None => Ok(None),
        }
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, Error> {
        seed.deserialize(GoValueDeserializer(self.value.take().unwrap_or_default()))
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.entries.len())
    }
}

/// Builds a [`GoValue`]: from the hand-over slot when gadk-goyaml decodes, otherwise from the
/// deserializer's data model (Go's `interface{}` kinds: integers that fit `int64` as `Int`,
/// maps with only string keys as `Map`).
struct GoValueVisitor;

impl<'de> Visitor<'de> for GoValueVisitor {
    type Value = GoValue;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any YAML value")
    }

    fn visit_newtype_struct<D: Deserializer<'de>>(self, d: D) -> Result<GoValue, D::Error> {
        if let Some(v) = GO_VALUE_SLOT.with(|s| s.borrow_mut().take()) {
            return Ok(v);
        }
        d.deserialize_any(self)
    }

    fn visit_bool<E: de::Error>(self, v: bool) -> Result<GoValue, E> {
        Ok(GoValue::Bool(v))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<GoValue, E> {
        Ok(GoValue::Int(v))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<GoValue, E> {
        Ok(match i64::try_from(v) {
            Ok(i) => GoValue::Int(i),
            Err(_) => GoValue::Uint(v),
        })
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<GoValue, E> {
        Ok(GoValue::Float(v))
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<GoValue, E> {
        Ok(GoValue::String(v.to_string()))
    }

    fn visit_string<E: de::Error>(self, v: String) -> Result<GoValue, E> {
        Ok(GoValue::String(v))
    }

    fn visit_bytes<E: de::Error>(self, v: &[u8]) -> Result<GoValue, E> {
        self.visit_byte_buf(v.to_vec())
    }

    fn visit_byte_buf<E: de::Error>(self, v: Vec<u8>) -> Result<GoValue, E> {
        Ok(match String::from_utf8(v) {
            Ok(s) => GoValue::String(s),
            Err(e) => GoValue::Bytes(e.into_bytes()),
        })
    }

    fn visit_none<E: de::Error>(self) -> Result<GoValue, E> {
        Ok(GoValue::Null)
    }

    fn visit_unit<E: de::Error>(self) -> Result<GoValue, E> {
        Ok(GoValue::Null)
    }

    fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<GoValue, D::Error> {
        GoValue::deserialize(d)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<GoValue, A::Error> {
        let mut items = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(4096));
        while let Some(v) = seq.next_element::<GoValue>()? {
            items.push(v);
        }
        Ok(GoValue::Seq(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<GoValue, A::Error> {
        let mut entries = Vec::new();
        while let Some((k, v)) = map.next_entry::<GoValue, GoValue>()? {
            entries.push((k, v));
        }
        if entries.iter().all(|(k, _)| matches!(k, GoValue::String(_))) {
            Ok(GoValue::Map(
                entries
                    .into_iter()
                    .map(|(k, v)| match k {
                        GoValue::String(s) => (s, v),
                        _ => (String::new(), v),
                    })
                    .collect(),
            ))
        } else {
            Ok(GoValue::MapAny(entries))
        }
    }
}

impl<'de> Deserialize<'de> for GoValue {
    /// Through gadk-goyaml: exactly what yaml.v3 decodes into `interface{}` (via
    /// `GO_VALUE_MARKER`); through other formats: built from their data model.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_newtype_struct(GO_VALUE_MARKER, GoValueVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Options, unmarshal};

    fn decode(src: &str) -> Result<GoValue, Error> {
        unmarshal::<GoValue>(src.as_bytes(), &Options::default())
    }

    #[test]
    fn scalars_resolve_like_go() {
        assert_eq!(decode("1").unwrap(), GoValue::Int(1));
        assert_eq!(
            decode("18446744073709551615").unwrap(),
            GoValue::Uint(u64::MAX)
        );
        assert_eq!(decode("~").unwrap(), GoValue::Null);
        assert_eq!(decode("").unwrap(), GoValue::Null);
        assert_eq!(decode("'1'").unwrap(), GoValue::String("1".into()));
        assert_eq!(
            decode("!!binary aGVsbG8=").unwrap(),
            GoValue::String("hello".into())
        );
        assert_eq!(decode("!!binary /w==").unwrap(), GoValue::Bytes(vec![0xff]));
        assert_eq!(
            decode("!!binary '!'").unwrap_err().to_string(),
            "yaml: !!binary value contains invalid base64 data"
        );
    }

    #[test]
    fn maps_string_and_general() {
        assert_eq!(
            decode("{a: 1, b: [x]}").unwrap(),
            GoValue::Map(vec![
                ("a".into(), GoValue::Int(1)),
                ("b".into(), GoValue::Seq(vec![GoValue::String("x".into())])),
            ])
        );
        assert_eq!(
            decode("{1: a, 0x1: b}").unwrap(),
            GoValue::MapAny(vec![(GoValue::Int(1), GoValue::String("b".into()))])
        );
        assert_eq!(
            decode("? [a, b]\n: c\n").unwrap_err().to_string(),
            "yaml: invalid map key: []interface {}{\"a\", \"b\"}"
        );
    }

    #[test]
    fn merges_keep_own_keys_first() {
        assert_eq!(
            decode("a: &a {x: 1, y: 2}\nb:\n  <<: *a\n  x: 3\n").unwrap(),
            GoValue::Map(vec![
                (
                    "a".into(),
                    GoValue::Map(vec![
                        ("x".into(), GoValue::Int(1)),
                        ("y".into(), GoValue::Int(2))
                    ])
                ),
                (
                    "b".into(),
                    GoValue::Map(vec![
                        ("x".into(), GoValue::Int(3)),
                        ("y".into(), GoValue::Int(2))
                    ])
                ),
            ])
        );
        assert_eq!(
            decode("<<: 1\n").unwrap_err().to_string(),
            "yaml: map merge requires map or sequence of maps as the value"
        );
    }

    #[test]
    fn sharp_v_formats_like_go() {
        let v = GoValue::Map(vec![
            ("b".into(), GoValue::Null),
            (
                "a".into(),
                GoValue::Seq(vec![GoValue::Uint(255), GoValue::Float(1.5)]),
            ),
        ]);
        assert_eq!(
            go_sharp_v(&v),
            "map[string]interface {}{\"a\":[]interface {}{0xff, 1.5}, \"b\":interface {}(nil)}"
        );
        let t = DateTime::parse_from_rfc3339("2001-12-14T21:59:43.1Z").unwrap();
        assert_eq!(
            go_sharp_v(&GoValue::Timestamp(t)),
            "time.Date(2001, time.December, 14, 21, 59, 43, 100000000, time.UTC)"
        );
    }

    #[test]
    fn go_value_through_other_formats() {
        let v: GoValue = serde_json::from_str(r#"{"a": [1, 2.5, "x", null, true]}"#).unwrap();
        assert_eq!(
            v,
            GoValue::Map(vec![(
                "a".into(),
                GoValue::Seq(vec![
                    GoValue::Int(1),
                    GoValue::Float(2.5),
                    GoValue::String("x".into()),
                    GoValue::Null,
                    GoValue::Bool(true)
                ])
            )])
        );
    }

    #[test]
    fn walk_depth_is_capped_at_127_nested_collections() {
        // Plan decision D7: 127 nested sequences decode, 128 fail (Go has no limit here).
        let ok = format!("{}{}", "[".repeat(127), "]".repeat(127));
        let mut v = decode(&ok).unwrap();
        let mut levels = 0;
        while let GoValue::Seq(items) = v {
            levels += 1;
            v = items.into_iter().next().unwrap_or(GoValue::Null);
        }
        assert_eq!(levels, 127);
        let deep = format!("{}{}", "[".repeat(128), "]".repeat(128));
        assert_eq!(
            decode(&deep).unwrap_err().to_string(),
            "yaml: exceeded max depth of 127"
        );
        // An alias expansion counts as a level.
        // Outer sequence (1) + alias (1) + 125 nested sequences = 127 levels.
        let aliased = format!("- &a {}{}\n- *a\n", "[".repeat(125), "]".repeat(125));
        assert!(decode(&aliased).is_ok());
        let aliased = format!("- &a {}{}\n- *a\n", "[".repeat(126), "]".repeat(126));
        assert_eq!(
            decode(&aliased).unwrap_err().to_string(),
            "yaml: exceeded max depth of 127"
        );
    }
}
