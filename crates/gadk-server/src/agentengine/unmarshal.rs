// Copyright 2026 Chris Olson
// Portions derived from ADK-Go (https://github.com/google/adk-go), Copyright 2026 Google LLC.
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

//! Go `encoding/json.Unmarshal` for the agentengine request models (`go_unmarshal`): Go's key
//! matching and error texts, then serde.

use std::fmt;
use std::marker::PhantomData;

use base64::Engine as _;
use serde::de::{DeserializeOwned, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::value::RawValue;
use serde_json::{Map, Number, Value};

use super::encode::{GoKind, GoStruct};
use super::encode_shapes;

/// The genai types reachable from the request models that have their own `UnmarshalJSON`
/// (`genai.Content` → `Part.VideoMetadata`, `Part.AudioTranscription`): Go type, Go's type text
/// of the anonymous aux struct they decode into, and whether that struct declares the
/// `time.Duration` fields as `string` (`VideoMetadata`; `Transcription` uses genai's
/// `*durationJSON`, taken verbatim here and parsed by gadk-genai's serde). Go reports their
/// errors relative to the aux struct (`Go struct field .fps`, `Go value of type struct { … }`).
const CUSTOM_UNMARSHALERS: &[(&str, &str, bool)] = &[
    (
        "genai.Transcription",
        "struct { StartOffset *genai.durationJSON \"json:\\\"startOffset,omitempty\\\"\"; EndOffset *genai.durationJSON \"json:\\\"endOffset,omitempty\\\"\"; *genai.Alias }",
        false,
    ),
    (
        "genai.VideoMetadata",
        "struct { EndOffset string \"json:\\\"endOffset,omitempty\\\"\"; StartOffset string \"json:\\\"startOffset,omitempty\\\"\"; *genai.Alias }",
        true,
    ),
];

/// How a custom unmarshaler's aux struct declares the `time.Duration` fields.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Durations {
    /// Not inside a custom unmarshaler: `time.Duration` is an integer.
    Int,
    /// Declared `string`.
    String,
    /// Declared with genai's own unmarshaler: taken verbatim.
    Verbatim,
}

/// Go `json.Unmarshal(payload, &v)` into the Go struct `shape` (a request model from
/// [`encode_shapes`](super::encode_shapes), e.g. `MODELS_QUERY`), producing the Rust model `T`:
///
/// 1. syntax: the whole input must be one JSON value (Go `checkValid`); an empty or truncated
///    input is `unexpected end of JSON input`; any other syntax error keeps serde_json's text
///    (ledgered, as in 5a);
/// 2. keys: in each JSON object decoded into a struct (`GoKind::Struct`, also through
///    `Pointer`/`Slice`/`Map` elements and into genai structs), a key matches the field whose
///    json name (tag name, else Go name) equals it exactly, else case-insensitively (Go
///    `foldName`); an unmatched key is ignored; a later duplicate wins (Go decodes into the
///    existing value: a repeated struct field or map merges, a repeated slice reuses its
///    elements); values of
///    `Interface` and `Map(Interface)` fields (`input`, `state`, `FunctionCall.args`) are taken
///    verbatim; a JSON `null` leaves a non-interface field at its zero value (a pointer, slice,
///    map or interface becomes nil);
/// 3. types: a value whose JSON kind does not fit the field's `GoKind` is Go 1.27's
///    `json: cannot unmarshal <JSON kind> into Go struct field <Struct>.<path> of type
///    <go_type>` (`<Struct>` the top-level type name without package, `<path>` the object keys
///    as written in the input and the array indexes from the top level, joined by `.`); when the
///    value is an array element the text is `… into <Struct>.<path> of type <elem type>`
///    (no `Go struct field`), at the top level `… into Go value of type <pkg.Type>`; a number
///    that does not fit an integer or float field is `number <its text>`; an invalid base64
///    string for `[]byte` appends `: illegal base64 data at input byte <n>`; the first error in
///    input order wins;
/// 4. the canonicalized value is deserialized into `T` (its serde names are the json names).
pub(crate) fn go_unmarshal<T: DeserializeOwned>(
    payload: &[u8],
    shape: &'static GoStruct,
) -> Result<T, String> {
    let raw: &RawValue = serde_json::from_slice(payload).map_err(|e| {
        if e.is_eof() {
            "unexpected end of JSON input".to_owned()
        } else {
            e.to_string()
        }
    })?;
    let top_name = shape
        .go_type
        .rsplit_once('.')
        .map_or(shape.go_type, |(_, name)| name);
    let mut decoder = Decoder {
        top_name: top_name.to_owned(),
        top_type: shape.go_type.to_owned(),
        path: Vec::new(),
        durations: Durations::Int,
    };
    let mut out = Map::new();
    decoder.decode_struct(raw, shape, shape.go_type, &mut out)?;
    serde_json::from_value(Value::Object(out)).map_err(|e| e.to_string())
}

/// One element of the error path.
enum Seg {
    /// An object key as written in the input.
    Key(String),
    /// An array index.
    Index(usize),
}

/// The decode state of one `json.Unmarshal` call (Go's `decodeState` error context).
struct Decoder {
    /// The top-level struct name in `Go struct field <name>.<path>` (empty inside a custom
    /// unmarshaler, whose own `json.Unmarshal` call reports `.fps`).
    top_name: String,
    /// The type in `Go value of type <type>` for a top-level kind mismatch.
    top_type: String,
    /// The path from the top-level value to the value being decoded.
    path: Vec<Seg>,
    /// How `time.Duration` fields decode here.
    durations: Durations,
}

/// A JSON value with its raw children (numbers keep their input text).
enum Node<'a> {
    Null,
    Bool(bool),
    Number(&'a str),
    String(String),
    Array(Vec<&'a RawValue>),
    Object(Vec<(String, &'a RawValue)>),
}

impl Node<'_> {
    /// Go's JSON kind word in `cannot unmarshal <kind>`.
    fn kind(&self) -> &'static str {
        match self {
            Node::Null => "null",
            Node::Bool(_) => "bool",
            Node::Number(_) => "number",
            Node::String(_) => "string",
            Node::Array(_) => "array",
            Node::Object(_) => "object",
        }
    }
}

/// An object's members in input order, duplicates kept.
struct Pairs<'a>(Vec<(String, &'a RawValue)>);

impl<'de: 'a, 'a> Deserialize<'de> for Pairs<'a> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct PairsVisitor<'a>(PhantomData<&'a ()>);

        impl<'de: 'a, 'a> Visitor<'de> for PairsVisitor<'a> {
            type Value = Pairs<'a>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a JSON object")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Pairs<'a>, A::Error> {
                let mut out = Vec::new();
                while let Some((key, value)) = map.next_entry::<String, &'de RawValue>()? {
                    out.push((key, value));
                }
                Ok(Pairs(out))
            }
        }

        deserializer.deserialize_map(PairsVisitor(PhantomData))
    }
}

/// Splits an already-validated raw value one level.
fn node(raw: &RawValue) -> Result<Node<'_>, String> {
    let text = raw.get();
    match text.as_bytes().first() {
        Some(b'n') => Ok(Node::Null),
        Some(b't') => Ok(Node::Bool(true)),
        Some(b'f') => Ok(Node::Bool(false)),
        Some(b'"') => serde_json::from_str(text)
            .map(Node::String)
            .map_err(|e| e.to_string()),
        Some(b'[') => serde_json::from_str::<Vec<&RawValue>>(text)
            .map(Node::Array)
            .map_err(|e| e.to_string()),
        Some(b'{') => serde_json::from_str::<Pairs<'_>>(text)
            .map(|p| Node::Object(p.0))
            .map_err(|e| e.to_string()),
        _ => Ok(Node::Number(text)),
    }
}

/// The element type of a slice, array or map type string (`[]*genai.Part` → `*genai.Part`,
/// `map[string]interface {}` → `interface {}`).
fn elem_type(go_type: &str) -> &str {
    go_type.split_once(']').map_or(go_type, |(_, elem)| elem)
}

/// Go `foldName` equality: equal under Unicode simple case folding (underscores significant).
fn fold_eq(a: &str, b: &str) -> bool {
    let mut x = a.chars();
    let mut y = b.chars();
    loop {
        match (x.next(), y.next()) {
            (None, None) => return true,
            (Some(c), Some(d)) => {
                if c != d
                    && !c.to_lowercase().eq(d.to_lowercase())
                    && !c.to_uppercase().eq(d.to_uppercase())
                {
                    return false;
                }
            }
            _ => return false,
        }
    }
}

/// The decodable fields of `shape` with their json names (Go `cachedTypeFields`): `-`-tagged
/// and unexported fields excluded, an untagged embedded struct's fields promoted.
fn fields_of(shape: &'static GoStruct) -> Vec<(String, &'static super::encode::GoField)> {
    let mut out = Vec::new();
    for field in shape.fields {
        if field.tag == "-" {
            continue;
        }
        let tag_name = field.tag.split(',').next().unwrap_or_default();
        if field.embedded && tag_name.is_empty() {
            let inner = match field.kind {
                GoKind::Struct(name) => Some(name),
                GoKind::Pointer(GoKind::Struct(name)) => Some(*name),
                _ => None,
            };
            if let Some(inner) = inner.and_then(shape_named) {
                out.extend(fields_of(inner));
                continue;
            }
        }
        if !field.exported {
            continue;
        }
        let name = if tag_name.is_empty() {
            field.name
        } else {
            tag_name
        };
        out.push((name.to_owned(), field));
    }
    out
}

/// The shape named `go_type` among [`encode_shapes::ALL`].
fn shape_named(go_type: &str) -> Option<&'static GoStruct> {
    encode_shapes::ALL
        .iter()
        .copied()
        .find(|s| s.go_type == go_type)
}

/// Go's `encoding/base64.StdEncoding.Decode` (`\r` and `\n` ignored, padding required, trailing
/// bits not checked); `Err` is the `CorruptInputError` offset.
fn go_base64_decode(s: &str) -> Result<Vec<u8>, usize> {
    let src = s.as_bytes();
    let mut out = Vec::with_capacity(src.len() / 4 * 3);
    let mut si = 0;
    loop {
        let mut dbuf = [0u8; 4];
        let mut dlen = 4;
        let mut j = 0;
        while j < 4 {
            if si == src.len() {
                if j == 0 {
                    return Ok(out);
                }
                return Err(si - j);
            }
            let c = src[si];
            si += 1;
            if let Some(v) = base64_value(c) {
                dbuf[j] = v;
                j += 1;
                continue;
            }
            if c == b'\n' || c == b'\r' {
                continue;
            }
            if c != b'=' {
                return Err(si - 1);
            }
            match j {
                0 | 1 => return Err(si - 1),
                2 => {
                    while si < src.len() && (src[si] == b'\n' || src[si] == b'\r') {
                        si += 1;
                    }
                    if si == src.len() {
                        return Err(src.len());
                    }
                    if src[si] != b'=' {
                        return Err(si - 1);
                    }
                    si += 1;
                }
                _ => {}
            }
            while si < src.len() && (src[si] == b'\n' || src[si] == b'\r') {
                si += 1;
            }
            if si < src.len() {
                return Err(si);
            }
            dlen = j;
            break;
        }
        let val = (u32::from(dbuf[0]) << 18)
            | (u32::from(dbuf[1]) << 12)
            | (u32::from(dbuf[2]) << 6)
            | u32::from(dbuf[3]);
        let bytes = val.to_be_bytes();
        match dlen {
            4 => out.extend_from_slice(&bytes[1..4]),
            3 => out.extend_from_slice(&bytes[1..3]),
            2 => out.push(bytes[1]),
            _ => {}
        }
        if dlen < 4 {
            return Ok(out);
        }
    }
}

/// The value of a standard-alphabet base64 character.
fn base64_value(c: u8) -> Option<u8> {
    match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// The bit size of a Go integer type (`int` is 64 bits, as on every target ADK-Go builds for).
fn int_bits(go_type: &str) -> u32 {
    match go_type {
        "int8" | "uint8" => 8,
        "int16" | "uint16" => 16,
        "int32" | "uint32" => 32,
        _ => 64,
    }
}

/// A JSON number literal as a Go integer of `bits` (signed or not), or `None` when it has a
/// fraction or exponent or is out of range (Go `strconv.ParseInt` / `ParseUint` on the text).
fn parse_int(text: &str, bits: u32, signed: bool) -> Option<Number> {
    let digits = text.strip_prefix('-').unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let v: i128 = text.parse().ok()?;
    let (min, max) = if signed {
        (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
    } else {
        (0, (1i128 << bits) - 1)
    };
    if v < min || v > max {
        return None;
    }
    if v < 0 {
        i64::try_from(v).ok().map(Number::from)
    } else {
        u64::try_from(v).ok().map(Number::from)
    }
}

/// A JSON number literal as a float (`f32` range for `float32`), keeping integers integral.
fn parse_float(text: &str, float32: bool) -> Option<Number> {
    let f: f64 = text.parse().ok()?;
    if !f.is_finite() || (float32 && f.abs() > f64::from(f32::MAX)) {
        return None;
    }
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Number(n)) => Some(n),
        _ => Number::from_f64(f),
    }
}

impl Decoder {
    /// Go's `UnmarshalTypeError` text for a `json` kind decoded into `go_type` here.
    fn type_error(&self, json: &str, go_type: &str) -> String {
        let Some(last) = self.path.last() else {
            return format!(
                "json: cannot unmarshal {json} into Go value of type {}",
                self.top_type
            );
        };
        let mut path = self.top_name.clone();
        for seg in &self.path {
            path.push('.');
            match seg {
                Seg::Key(k) => path.push_str(k),
                Seg::Index(i) => path.push_str(&i.to_string()),
            }
        }
        match last {
            Seg::Key(_) => format!(
                "json: cannot unmarshal {json} into Go struct field {path} of type {go_type}"
            ),
            Seg::Index(_) => format!("json: cannot unmarshal {json} into {path} of type {go_type}"),
        }
    }

    /// Decodes `raw` into the struct `shape` (Go type `go_type`), merging into `into`.
    fn decode_struct(
        &mut self,
        raw: &RawValue,
        shape: &'static GoStruct,
        go_type: &str,
        into: &mut Map<String, Value>,
    ) -> Result<(), String> {
        let pairs = match node(raw)? {
            Node::Null => return Ok(()),
            Node::Object(pairs) => pairs,
            other => return Err(self.type_error(other.kind(), go_type)),
        };
        let fields = fields_of(shape);
        for (key, value) in pairs {
            let field = fields
                .iter()
                .find(|(name, _)| *name == key)
                .or_else(|| fields.iter().find(|(name, _)| fold_eq(name, &key)));
            let Some((name, field)) = field else {
                continue;
            };
            let mut slot = into.remove(name);
            self.path.push(Seg::Key(key));
            let result = match self.durations {
                Durations::String if field.go_type == "time.Duration" => {
                    self.decode_value(value, &GoKind::String, "string", &mut slot)
                }
                Durations::Verbatim if field.go_type == "time.Duration" => {
                    serde_json::from_str::<Value>(value.get())
                        .map(|v| slot = Some(v))
                        .map_err(|e| e.to_string())
                }
                _ => self.decode_value(value, &field.kind, field.go_type, &mut slot),
            };
            self.path.pop();
            result?;
            if let Some(v) = slot {
                into.insert(name.clone(), v);
            }
        }
        Ok(())
    }

    /// Decodes `raw` into a Go value of `kind` / `go_type`; `slot` is its current canonical
    /// value (`None`: Go's zero value, the key absent).
    fn decode_value(
        &mut self,
        raw: &RawValue,
        kind: &GoKind,
        go_type: &str,
        slot: &mut Option<Value>,
    ) -> Result<(), String> {
        let n = node(raw)?;
        if let Node::Null = n {
            // Go: null sets a pointer, slice, map or interface to nil; anything else is unchanged.
            if matches!(
                kind,
                GoKind::Pointer(_)
                    | GoKind::Slice(_)
                    | GoKind::Map(_)
                    | GoKind::Interface
                    | GoKind::Bytes
                    | GoKind::RawMessage
            ) {
                *slot = None;
            }
            return Ok(());
        }
        match kind {
            GoKind::Bool => match n {
                Node::Bool(b) => *slot = Some(Value::Bool(b)),
                other => return Err(self.type_error(other.kind(), go_type)),
            },
            GoKind::Int | GoKind::Uint => match n {
                Node::Number(text) => {
                    let signed = matches!(kind, GoKind::Int);
                    match parse_int(text, int_bits(go_type), signed) {
                        Some(num) => *slot = Some(Value::Number(num)),
                        None => return Err(self.type_error(&format!("number {text}"), go_type)),
                    }
                }
                other => return Err(self.type_error(other.kind(), go_type)),
            },
            GoKind::Float32 | GoKind::Float64 => match n {
                Node::Number(text) => match parse_float(text, matches!(kind, GoKind::Float32)) {
                    Some(num) => *slot = Some(Value::Number(num)),
                    None => return Err(self.type_error(&format!("number {text}"), go_type)),
                },
                other => return Err(self.type_error(other.kind(), go_type)),
            },
            GoKind::String | GoKind::NamedString | GoKind::Time => match n {
                Node::String(s) => *slot = Some(Value::String(s)),
                other => return Err(self.type_error(other.kind(), go_type)),
            },
            GoKind::Bytes => match n {
                Node::String(s) => match go_base64_decode(&s) {
                    Ok(bytes) => {
                        *slot = Some(Value::String(
                            base64::engine::general_purpose::STANDARD.encode(bytes),
                        ));
                    }
                    Err(at) => {
                        return Err(format!(
                            "{}: illegal base64 data at input byte {at}",
                            self.type_error("string", go_type)
                        ));
                    }
                },
                Node::Array(items) => {
                    let mut bytes = Vec::with_capacity(items.len());
                    for (i, item) in items.into_iter().enumerate() {
                        let mut b = None;
                        self.path.push(Seg::Index(i));
                        let result = self.decode_value(item, &GoKind::Uint, "uint8", &mut b);
                        self.path.pop();
                        result?;
                        let byte = b
                            .as_ref()
                            .and_then(Value::as_u64)
                            .and_then(|v| u8::try_from(v).ok())
                            .unwrap_or_default();
                        bytes.push(byte);
                    }
                    *slot = Some(Value::String(
                        base64::engine::general_purpose::STANDARD.encode(bytes),
                    ));
                }
                other => return Err(self.type_error(other.kind(), go_type)),
            },
            GoKind::RawMessage => {
                *slot = Some(serde_json::from_str(raw.get()).map_err(|e| e.to_string())?);
            }
            GoKind::Interface => *slot = self.interface_value(raw)?,
            GoKind::Struct(name) => {
                let Some(shape) = shape_named(name) else {
                    return Err(format!("json: no shape for Go type {name}"));
                };
                let mut obj = match slot.take() {
                    Some(Value::Object(m)) => m,
                    _ => Map::new(),
                };
                match CUSTOM_UNMARSHALERS.iter().find(|(t, _, _)| t == name) {
                    Some((_, aux_type, as_string)) => {
                        let mut inner = Decoder {
                            top_name: String::new(),
                            top_type: (*aux_type).to_owned(),
                            path: Vec::new(),
                            durations: if *as_string {
                                Durations::String
                            } else {
                                Durations::Verbatim
                            },
                        };
                        inner.decode_struct(raw, shape, aux_type, &mut obj)?;
                    }
                    None => self.decode_struct(raw, shape, go_type, &mut obj)?,
                }
                *slot = Some(Value::Object(obj));
            }
            GoKind::Pointer(inner) => {
                let elem = go_type.strip_prefix('*').unwrap_or(go_type);
                self.decode_value(raw, inner, elem, slot)?;
            }
            GoKind::Slice(inner) => match n {
                Node::Array(items) => {
                    // Go reuses the slice's existing elements (a struct element merges).
                    let existing = match slot.take() {
                        Some(Value::Array(a)) => a,
                        _ => Vec::new(),
                    };
                    let elem = elem_type(go_type);
                    let mut out = Vec::with_capacity(items.len());
                    for (i, item) in items.into_iter().enumerate() {
                        let mut v = existing.get(i).filter(|v| !v.is_null()).cloned();
                        self.path.push(Seg::Index(i));
                        let result = self.decode_value(item, inner, elem, &mut v);
                        self.path.pop();
                        result?;
                        out.push(v.unwrap_or(Value::Null));
                    }
                    *slot = Some(Value::Array(out));
                }
                other => return Err(self.type_error(other.kind(), go_type)),
            },
            GoKind::Map(inner) => match n {
                Node::Object(pairs) => {
                    let elem = elem_type(go_type);
                    let mut obj = match slot.take() {
                        Some(Value::Object(m)) => m,
                        _ => Map::new(),
                    };
                    for (key, item) in pairs {
                        // Go decodes each map value into a fresh zero value.
                        let mut v = None;
                        self.path.push(Seg::Key(key.clone()));
                        let result = self.decode_value(item, inner, elem, &mut v);
                        self.path.pop();
                        result?;
                        obj.insert(key, v.unwrap_or(Value::Null));
                    }
                    *slot = Some(Value::Object(obj));
                }
                other => return Err(self.type_error(other.kind(), go_type)),
            },
        }
        Ok(())
    }

    /// Go's `any` value: objects, arrays, strings, bools and numbers as JSON has them (`None`
    /// for null); a number outside the `float64` range is `number <text>` of type `float64`.
    fn interface_value(&mut self, raw: &RawValue) -> Result<Option<Value>, String> {
        Ok(Some(match node(raw)? {
            Node::Null => return Ok(None),
            Node::Bool(b) => Value::Bool(b),
            Node::String(s) => Value::String(s),
            Node::Number(text) => match parse_float(text, false) {
                Some(n) => Value::Number(n),
                None => return Err(self.type_error(&format!("number {text}"), "float64")),
            },
            Node::Array(items) => {
                let mut out = Vec::with_capacity(items.len());
                for (i, item) in items.into_iter().enumerate() {
                    self.path.push(Seg::Index(i));
                    let result = self.interface_value(item);
                    self.path.pop();
                    out.push(result?.unwrap_or(Value::Null));
                }
                Value::Array(out)
            }
            Node::Object(pairs) => {
                let mut out = Map::new();
                for (key, item) in pairs {
                    self.path.push(Seg::Key(key.clone()));
                    let result = self.interface_value(item);
                    self.path.pop();
                    out.insert(key, result?.unwrap_or(Value::Null));
                }
                Value::Object(out)
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use gadk_genai::{Content, Part};
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::super::encode_shapes::{
        MODELS_CREATE_SESSION_REQUEST, MODELS_GET_SESSION_REQUEST, MODELS_QUERY,
        MODELS_STREAM_QUERY_REQUEST, MODELS_STREAM_QUERY_TEXT_REQUEST,
        MODELS_STREAMING_AGENT_RUN_WITH_EVENTS_RUN_REQUEST,
    };
    use super::super::models::{
        CreateSessionInput, CreateSessionRequest, GetSessionRequest, Query, StreamQueryRequest,
        StreamQueryTextRequest, StreamingAgentRunWithEventsRunRequest,
    };
    use super::*;

    fn query(payload: &str) -> Query {
        go_unmarshal(payload.as_bytes(), &MODELS_QUERY).unwrap()
    }

    fn message(payload: &str) -> Content {
        go_unmarshal::<StreamQueryRequest>(payload.as_bytes(), &MODELS_STREAM_QUERY_REQUEST)
            .unwrap()
            .input
            .message
    }

    /// `go_unmarshal` into the Rust model of `shape`.
    fn decode(shape: &'static GoStruct, payload: &str) -> Result<(), String> {
        let p = payload.as_bytes();
        match shape.go_type {
            "models.Query" => go_unmarshal::<Query>(p, shape).map(drop),
            "models.CreateSessionRequest" => {
                go_unmarshal::<CreateSessionRequest>(p, shape).map(drop)
            }
            "models.GetSessionRequest" => go_unmarshal::<GetSessionRequest>(p, shape).map(drop),
            "models.StreamQueryRequest" => go_unmarshal::<StreamQueryRequest>(p, shape).map(drop),
            "models.StreamQueryTextRequest" => {
                go_unmarshal::<StreamQueryTextRequest>(p, shape).map(drop)
            }
            "models.StreamingAgentRunWithEventsRunRequest" => {
                go_unmarshal::<StreamingAgentRunWithEventsRunRequest>(p, shape).map(drop)
            }
            other => Err(format!("no model for {other}")),
        }
    }

    /// Go 1.27 `json.Unmarshal` error texts, recorded from ADK-Go's own models.
    #[test]
    fn go_error_texts() {
        let cases: &[(&GoStruct, &str, &str)] = &[
            (&MODELS_QUERY, r#""#, r#"unexpected end of JSON input"#),
            (&MODELS_QUERY, r#"{"#, r#"unexpected end of JSON input"#),
            (
                &MODELS_QUERY,
                r#"{"class_method":"#,
                r#"unexpected end of JSON input"#,
            ),
            (
                &MODELS_QUERY,
                r#"{"class_method":5}"#,
                r#"json: cannot unmarshal number into Go struct field Query.class_method of type string"#,
            ),
            (
                &MODELS_QUERY,
                r#"{"class_method":true}"#,
                r#"json: cannot unmarshal bool into Go struct field Query.class_method of type string"#,
            ),
            (
                &MODELS_QUERY,
                r#"{"class_method":[1]}"#,
                r#"json: cannot unmarshal array into Go struct field Query.class_method of type string"#,
            ),
            (
                &MODELS_QUERY,
                r#"{"class_method":{}}"#,
                r#"json: cannot unmarshal object into Go struct field Query.class_method of type string"#,
            ),
            (
                &MODELS_QUERY,
                r#"[]"#,
                r#"json: cannot unmarshal array into Go value of type models.Query"#,
            ),
            (
                &MODELS_QUERY,
                r#""x""#,
                r#"json: cannot unmarshal string into Go value of type models.Query"#,
            ),
            (
                &MODELS_QUERY,
                r#"5"#,
                r#"json: cannot unmarshal number into Go value of type models.Query"#,
            ),
            (
                &MODELS_QUERY,
                r#"{"class_method":5,"input":[1,2]}"#,
                r#"json: cannot unmarshal number into Go struct field Query.class_method of type string"#,
            ),
            (
                &MODELS_QUERY,
                r#"{"class_method":"x","class_method":7}"#,
                r#"json: cannot unmarshal number into Go struct field Query.class_method of type string"#,
            ),
            (
                &MODELS_CREATE_SESSION_REQUEST,
                r#"{"input":{"user_id":"u","state":[1]}}"#,
                r#"json: cannot unmarshal array into Go struct field CreateSessionRequest.input.state of type map[string]interface {}"#,
            ),
            (
                &MODELS_CREATE_SESSION_REQUEST,
                r#"{"input":5}"#,
                r#"json: cannot unmarshal number into Go struct field CreateSessionRequest.input of type models.CreateSessionInput"#,
            ),
            (
                &MODELS_CREATE_SESSION_REQUEST,
                r#"{"input":{"user_id":1,"state":5}}"#,
                r#"json: cannot unmarshal number into Go struct field CreateSessionRequest.input.user_id of type string"#,
            ),
            (
                &MODELS_GET_SESSION_REQUEST,
                r#"{"input":{"user_id":"u","session_id":false}}"#,
                r#"json: cannot unmarshal bool into Go struct field GetSessionRequest.input.session_id of type string"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"class_method":"async_stream_query","input":{"user_id":"u2","message":5}}"#,
                r#"json: cannot unmarshal number into Go struct field StreamQueryRequest.input.message of type genai.Content"#,
            ),
            (
                &MODELS_STREAM_QUERY_TEXT_REQUEST,
                r#"{"class_method":"async_stream_query","input":{"user_id":"u2","message":5}}"#,
                r#"json: cannot unmarshal number into Go struct field StreamQueryTextRequest.input.message of type string"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":"hello"}}"#,
                r#"json: cannot unmarshal string into Go struct field StreamQueryRequest.input.message of type genai.Content"#,
            ),
            (
                &MODELS_STREAM_QUERY_TEXT_REQUEST,
                r#"{"input":{"message":{"role":"user"}}}"#,
                r#"json: cannot unmarshal object into Go struct field StreamQueryTextRequest.input.message of type string"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":5}}}"#,
                r#"json: cannot unmarshal number into Go struct field StreamQueryRequest.input.message.parts of type []*genai.Part"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":[5]}}}"#,
                r#"json: cannot unmarshal number into StreamQueryRequest.input.message.parts.0 of type genai.Part"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":[{"text":5}]}}}"#,
                r#"json: cannot unmarshal number into Go struct field StreamQueryRequest.input.message.parts.0.text of type string"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":[{"functionCall":{"args":5}}]}}}"#,
                r#"json: cannot unmarshal number into Go struct field StreamQueryRequest.input.message.parts.0.functionCall.args of type map[string]interface {}"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":[{"inlineData":{"data":5}}]}}}"#,
                r#"json: cannot unmarshal number into Go struct field StreamQueryRequest.input.message.parts.0.inlineData.data of type []uint8"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":[{"thought":"yes"}]}}}"#,
                r#"json: cannot unmarshal string into Go struct field StreamQueryRequest.input.message.parts.0.thought of type bool"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"role":5,"parts":[{"text":5}]}}}"#,
                r#"json: cannot unmarshal number into Go struct field StreamQueryRequest.input.message.role of type string"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":[{"videoMetadata":{"fps":"x"}}]}}}"#,
                r#"json: cannot unmarshal string into Go struct field .fps of type float64"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":[{"executableCode":{"code":"x","language":7}}]}}}"#,
                r#"json: cannot unmarshal number into Go struct field StreamQueryRequest.input.message.parts.0.executableCode.language of type genai.Language"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":[{"functionResponse":{"name":"f","response":{"a":1},"parts":[{"inlineData":{"data":1}}]}}]}}}"#,
                r#"json: cannot unmarshal number into Go struct field StreamQueryRequest.input.message.parts.0.functionResponse.parts.0.inlineData.data of type []uint8"#,
            ),
            (
                &MODELS_STREAMING_AGENT_RUN_WITH_EVENTS_RUN_REQUEST,
                r#"{"user_id":"u3","session_id":5}"#,
                r#"json: cannot unmarshal number into Go struct field StreamingAgentRunWithEventsRunRequest.session_id of type string"#,
            ),
            (
                &MODELS_CREATE_SESSION_REQUEST,
                r#"{"Input":{"USER_ID":5}}"#,
                r#"json: cannot unmarshal number into Go struct field CreateSessionRequest.Input.USER_ID of type string"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"INPUT":{"Message":{"PARTS":[{},{"Text":5}]}}}"#,
                r#"json: cannot unmarshal number into Go struct field StreamQueryRequest.INPUT.Message.PARTS.1.Text of type string"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":[{"inlineData":{"data":"!!"}}]}}}"#,
                r#"json: cannot unmarshal string into Go struct field StreamQueryRequest.input.message.parts.0.inlineData.data of type []uint8: illegal base64 data at input byte 0"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":[{"inlineData":{"data":"aGk"}}]}}}"#,
                r#"json: cannot unmarshal string into Go struct field StreamQueryRequest.input.message.parts.0.inlineData.data of type []uint8: illegal base64 data at input byte 0"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":[{"mediaResolution":{"numTokens":1.5}}]}}}"#,
                r#"json: cannot unmarshal number 1.5 into Go struct field StreamQueryRequest.input.message.parts.0.mediaResolution.numTokens of type int32"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":[{"mediaResolution":{"numTokens":1e20}}]}}}"#,
                r#"json: cannot unmarshal number 1e20 into Go struct field StreamQueryRequest.input.message.parts.0.mediaResolution.numTokens of type int32"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":[{"mediaResolution":{"numTokens":"3"}}]}}}"#,
                r#"json: cannot unmarshal string into Go struct field StreamQueryRequest.input.message.parts.0.mediaResolution.numTokens of type int32"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":[{"mediaResolution":{"numTokens":3.0}}]}}}"#,
                r#"json: cannot unmarshal number 3.0 into Go struct field StreamQueryRequest.input.message.parts.0.mediaResolution.numTokens of type int32"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":[{"functionCall":{"partialArgs":[{"numberValue":true}]}}]}}}"#,
                r#"json: cannot unmarshal bool into Go struct field StreamQueryRequest.input.message.parts.0.functionCall.partialArgs.0.numberValue of type float64"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":[{"audioTranscription":{"finished":"x"}}]}}}"#,
                r#"json: cannot unmarshal string into Go struct field .finished of type bool"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":[{"videoMetadata":{"startOffset":5}}]}}}"#,
                r#"json: cannot unmarshal number into Go struct field .startOffset of type string"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":[{"videoMetadata":{"startOffset":"zz"}}]}}}"#,
                r#"time: invalid duration "zz""#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":{}}}}"#,
                r#"json: cannot unmarshal object into Go struct field StreamQueryRequest.input.message.parts of type []*genai.Part"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":[[1]]}}}"#,
                r#"json: cannot unmarshal array into StreamQueryRequest.input.message.parts.0 of type genai.Part"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":[{"text":5}]}},"class_method":7}"#,
                r#"json: cannot unmarshal number into Go struct field StreamQueryRequest.input.message.parts.0.text of type string"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":[{"text":5}]}},"class_method":"a" "#,
                r#"unexpected end of JSON input"#,
            ),
            (&MODELS_QUERY, r#" "#, r#"unexpected end of JSON input"#),
            (
                &MODELS_QUERY,
                r#"{"input":1e400}"#,
                r#"json: cannot unmarshal number 1e400 into Go struct field Query.input of type float64"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":[{"inlineData":{"data":"aGVsbG8gd29ybGQ!"}}]}}}"#,
                r#"json: cannot unmarshal string into Go struct field StreamQueryRequest.input.message.parts.0.inlineData.data of type []uint8: illegal base64 data at input byte 15"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":[{"inlineData":{"data":"aGVs bG8="}}]}}}"#,
                r#"json: cannot unmarshal string into Go struct field StreamQueryRequest.input.message.parts.0.inlineData.data of type []uint8: illegal base64 data at input byte 4"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":[{"inlineData":{"data":"aGVsbG8"}}]}}}"#,
                r#"json: cannot unmarshal string into Go struct field StreamQueryRequest.input.message.parts.0.inlineData.data of type []uint8: illegal base64 data at input byte 4"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":[{"inlineData":{"data":"aGVsbA"}}]}}}"#,
                r#"json: cannot unmarshal string into Go struct field StreamQueryRequest.input.message.parts.0.inlineData.data of type []uint8: illegal base64 data at input byte 4"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":[{"thoughtSignature":[256]}]}}}"#,
                r#"json: cannot unmarshal number 256 into StreamQueryRequest.input.message.parts.0.thoughtSignature.0 of type uint8"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":[{"thoughtSignature":["a"]}]}}}"#,
                r#"json: cannot unmarshal string into StreamQueryRequest.input.message.parts.0.thoughtSignature.0 of type uint8"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":[{"thoughtSignature":[1.5]}]}}}"#,
                r#"json: cannot unmarshal number 1.5 into StreamQueryRequest.input.message.parts.0.thoughtSignature.0 of type uint8"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":[{"mediaResolution":{"numTokens":99999999999}}]}}}"#,
                r#"json: cannot unmarshal number 99999999999 into Go struct field StreamQueryRequest.input.message.parts.0.mediaResolution.numTokens of type int32"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":[{"mediaResolution":{"numTokens":1.50}}]}}}"#,
                r#"json: cannot unmarshal number 1.50 into Go struct field StreamQueryRequest.input.message.parts.0.mediaResolution.numTokens of type int32"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":[{"mediaResolution":{"numTokens":1E2}}]}}}"#,
                r#"json: cannot unmarshal number 1E2 into Go struct field StreamQueryRequest.input.message.parts.0.mediaResolution.numTokens of type int32"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":[{"audioTranscription":{"words":[{"word":5}]}}]}}}"#,
                r#"json: cannot unmarshal number into Go struct field .words.0.word of type string"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":[{"audioTranscription":{"words":[5]}}]}}}"#,
                r#"json: cannot unmarshal number into .words.0 of type genai.WordInfo"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":[{"videoMetadata":5}]}}}"#,
                r#"json: cannot unmarshal number into Go value of type struct { EndOffset string "json:\"endOffset,omitempty\""; StartOffset string "json:\"startOffset,omitempty\""; *genai.Alias }"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":[{"videoMetadata":{"FPS":"x"}}]}}}"#,
                r#"json: cannot unmarshal string into Go struct field .FPS of type float64"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":[{"videoMetadata":{"endOffset":"1s","startOffset":"zz"}}]}}}"#,
                r#"time: invalid duration "zz""#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":[{"functionResponse":{"parts":[{"inlineData":5}]}}]}}}"#,
                r#"json: cannot unmarshal number into Go struct field StreamQueryRequest.input.message.parts.0.functionResponse.parts.0.inlineData of type genai.FunctionResponseBlob"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":[null, 5]}}}"#,
                r#"json: cannot unmarshal number into StreamQueryRequest.input.message.parts.1 of type genai.Part"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":[{"audioTranscription":5}]}}}"#,
                r#"json: cannot unmarshal number into Go value of type struct { StartOffset *genai.durationJSON "json:\"startOffset,omitempty\""; EndOffset *genai.durationJSON "json:\"endOffset,omitempty\""; *genai.Alias }"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":[{"videoMetadata":{"startOffset":"1s","fps":"x"}}]}}}"#,
                r#"json: cannot unmarshal string into Go struct field .fps of type float64"#,
            ),
            (
                &MODELS_STREAM_QUERY_REQUEST,
                r#"{"input":{"message":{"parts":[{"videoMetadata":{"startOffset":"zz","fps":"x"}}]}}}"#,
                r#"json: cannot unmarshal string into Go struct field .fps of type float64"#,
            ),
        ];
        for (shape, payload, want) in cases {
            assert_eq!(
                decode(shape, payload),
                Err((*want).to_owned()),
                "payload {payload}"
            );
        }
    }

    #[test]
    fn other_syntax_errors_keep_serde_text() {
        let got = go_unmarshal::<Query>(br#"{"class_method":"a"} x"#, &MODELS_QUERY).unwrap_err();
        assert_eq!(got, "trailing characters at line 1 column 22");
    }

    #[test]
    fn keys_match_exactly_then_case_insensitively_and_the_later_wins() {
        let q = query(r#"{"CLASS_METHOD":"a","Input":{"USER_ID":"u1"}}"#);
        assert_eq!(q.class_method, "a");
        assert_eq!(q.input, Some(json!({"USER_ID": "u1"})));
        assert_eq!(
            query(r#"{"class_method":"a","Class_Method":"b"}"#).class_method,
            "b"
        );
        assert_eq!(
            query(r#"{"Class_Method":"b","class_method":"a"}"#).class_method,
            "a"
        );
        assert_eq!(
            query(r#"{"CLASS_METHOD":"x","class_method":"y","Class_Method":"z"}"#).class_method,
            "z"
        );
        assert_eq!(query(r#"{"class_method":"esc"}"#).class_method, "esc");
        assert_eq!(query(r#"{"claſs_method":"x"}"#).class_method, "x");
        assert_eq!(query(r#"{"classmethod":"x"}"#).class_method, "");
        assert_eq!(
            query(r#"{"class_method":"x","unknown":1,"input":{"a":[1,{"b":null}]}}"#).input,
            Some(json!({"a": [1, {"b": null}]}))
        );
    }

    #[test]
    fn null_leaves_zero_values_and_clears_nilable_ones() {
        assert_eq!(query("null"), Query::default());
        assert_eq!(
            query(r#"{"class_method":null,"input":null}"#),
            Query::default()
        );
        assert_eq!(
            query(r#"{"class_method":"a","class_method":null}"#).class_method,
            "a"
        );
        let req: CreateSessionRequest = go_unmarshal(
            br#"{"input":{"user_id":"u","state":null}}"#,
            &MODELS_CREATE_SESSION_REQUEST,
        )
        .unwrap();
        assert_eq!(
            req.input,
            CreateSessionInput {
                user_id: "u".into(),
                state: None
            }
        );
    }

    #[test]
    fn repeated_values_merge_as_go_decodes_into_the_existing_value() {
        let req: CreateSessionRequest = go_unmarshal(
            br#"{"input":{"user_id":"u","state":{"a":1}},"input":{"state":{"b":2}}}"#,
            &MODELS_CREATE_SESSION_REQUEST,
        )
        .unwrap();
        assert_eq!(req.input.user_id, "u");
        assert_eq!(
            req.input.state,
            Some(json!({"a": 1, "b": 2}).as_object().unwrap().clone())
        );
        let m = message(
            r#"{"input":{"message":{"parts":[{"text":"a"},{"text":"b"}]}},"input":{"message":{"parts":[{"thought":true}]}}}"#,
        );
        assert_eq!(
            m.parts,
            vec![Part {
                text: "a".into(),
                thought: true,
                ..Default::default()
            }]
        );
    }

    #[test]
    fn create_session_state_keys_are_kept() {
        let req: CreateSessionRequest = go_unmarshal(
            br#"{"Input":{"User_ID":"u","STATE":{"A":1}}}"#,
            &MODELS_CREATE_SESSION_REQUEST,
        )
        .unwrap();
        assert_eq!(req.input.user_id, "u");
        assert_eq!(
            req.input.state,
            Some(json!({"A": 1}).as_object().unwrap().clone())
        );
    }

    #[test]
    fn genai_content_is_canonicalized_through_its_go_shape() {
        let m = message(
            r#"{"input":{"message":{"parts":[{"TEXT":"hi","Thought":true}],"ROLE":"user"}}}"#,
        );
        assert_eq!(m.role, "user");
        assert_eq!(
            m.parts,
            vec![Part {
                text: "hi".into(),
                thought: true,
                ..Default::default()
            }]
        );

        let m = message(
            r#"{"input":{"message":{"parts":[{"functionCall":{"NAME":"f","Args":{"Q":1}}}]}}}"#,
        );
        let call = m.parts[0].function_call.clone().unwrap();
        assert_eq!(call.name, "f");
        assert_eq!(call.args, json!({"Q": 1}).as_object().unwrap().clone());

        let m = message(
            r#"{"input":{"message":{"parts":[{"inlineData":{"data":"aGVsbG8=\n","MIMETYPE":"text/plain"}}]}}}"#,
        );
        let blob = m.parts[0].inline_data.clone().unwrap();
        assert_eq!(&blob.data[..], b"hello");
        assert_eq!(blob.mime_type, "text/plain");

        let m = message(r#"{"input":{"message":{"parts":[{"thoughtSignature":[1]}]}}}"#);
        assert_eq!(&m.parts[0].thought_signature[..], &[1u8]);

        let m = message(
            r#"{"input":{"message":{"parts":[{"videoMetadata":{"startOffset":"1.5s","FPS":2}}]}}}"#,
        );
        let video = m.parts[0].video_metadata.clone().unwrap();
        assert_eq!(video.fps, Some(2.0));
        assert_eq!(video.start_offset, chrono::TimeDelta::milliseconds(1500));

        let m = message(
            r#"{"input":{"message":{"parts":[{"mediaResolution":{"numTokens":-0}},{"mediaResolution":{"numTokens":7}}]}}}"#,
        );
        assert_eq!(
            m.parts[0].media_resolution.clone().unwrap().num_tokens,
            Some(0)
        );
        assert_eq!(
            m.parts[1].media_resolution.clone().unwrap().num_tokens,
            Some(7)
        );
    }

    #[test]
    fn text_and_run_requests() {
        let req: StreamQueryTextRequest = go_unmarshal(
            br#"{"class_method":"async_stream_query","input":{"user_id":"u2","message":"hello"}}"#,
            &MODELS_STREAM_QUERY_TEXT_REQUEST,
        )
        .unwrap();
        assert_eq!(req.input.message, "hello");
        assert_eq!(req.input.user_id, "u2");

        let run: StreamingAgentRunWithEventsRunRequest = go_unmarshal(
            br#"{"user_id":"u3","message":{"parts":[{"text":"a"}],"role":"user"}}"#,
            &MODELS_STREAMING_AGENT_RUN_WITH_EVENTS_RUN_REQUEST,
        )
        .unwrap();
        assert_eq!(run.user_id, "u3");
        assert_eq!(run.message, Content::from_text("a", "user"));
    }

    #[test]
    fn go_base64_offsets() {
        assert_eq!(go_base64_decode("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(go_base64_decode("aGVs\r\nbG8=\n").unwrap(), b"hello");
        assert_eq!(go_base64_decode("").unwrap(), b"");
        assert_eq!(go_base64_decode("aGVsbA==").unwrap(), b"hell");
        assert_eq!(go_base64_decode("aGVsbG8gd29ybGQ!"), Err(15));
        assert_eq!(go_base64_decode("aGVs bG8="), Err(4));
        assert_eq!(go_base64_decode("aGVsbG8"), Err(4));
        assert_eq!(go_base64_decode("aGVsbA"), Err(4));
        assert_eq!(go_base64_decode("aGk"), Err(0));
        assert_eq!(go_base64_decode("!!"), Err(0));
        assert_eq!(go_base64_decode("aGVsbA=x"), Err(6));
        assert_eq!(go_base64_decode("aGVsbA="), Err(7));
        assert_eq!(go_base64_decode("aGVsbG8=x"), Err(8));
        assert_eq!(go_base64_decode("aGVsbG8=\n\nx"), Err(10));
        assert_eq!(go_base64_decode("a==="), Err(1));
        assert_eq!(go_base64_decode("aG==").unwrap(), b"h");
    }
}
