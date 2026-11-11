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

//! Go `server/agentengine/internal/helper/encode.go` (`ConvertSnake`) and `emit_json.go`
//! (`EmitJSON`, `EmitJSONError`).
//!
//! Go's `ConvertSnake` walks a value by reflection; serde output cannot recover what it reads
//! (pointer vs value, `omitempty`, named string types, `time.Time`, `[]byte`, `float32`), so the
//! walk is driven by Go's struct shapes ([`GoStruct`], generated into
//! [`encode_shapes`](super::encode_shapes) by `scripts/gen-go-agentengine --shapes`) and each
//! Rust type that reaches an NDJSON line implements [`GoSnake`] over its shape.
//!
//! Each [`GoSnake`] impl walks its Go shape over the Rust value's serde output (Go's JSON keys,
//! the same names the shapes' tags give): a field the output lacks is Go's zero value for its
//! kind. The five genai `time.Duration` fields are the exception serde cannot carry (genai
//! writes whole seconds), so an event's output is patched with their nanoseconds first.

use std::collections::BTreeMap;

use bytes::Bytes;
use chrono::TimeDelta;
use gadk_genai::{Content, GroundingMetadata, Transcription};
use gadk_model::LlmResponse;
use serde::Serialize;
use serde_json::{Map, Value};

use super::Log;
use super::encode_shapes;
use super::models::StreamingAgentRunWithEventsResponse;

/// A Go type as `ConvertSnake` (and `json.Unmarshal`) sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GoKind {
    /// `bool` (or a defined bool type): `v.Bool()`.
    Bool,
    /// `int`, `int8`…`int64` or a defined int type (`time.Duration`, `time.Month`): `v.Int()`.
    Int,
    /// `uint*`: `ConvertSnake` fails (`unsupported type: uint…`).
    Uint,
    /// `float32`: widened to float64 (`v.Float()`).
    Float32,
    /// `float64`.
    Float64,
    /// `string`.
    String,
    /// A defined type over `string` (`genai.Role`, `genai.FinishReason`): returned as is, so
    /// `addIfNotEmpty` never drops it (an empty one is written `""`).
    NamedString,
    /// `[]byte`: standard base64 text.
    Bytes,
    /// `json.RawMessage`: empty → nil (dropped); otherwise the JSON text verbatim.
    RawMessage,
    /// `time.Time`: integer Unix seconds (`UnixMilli() / 1000`).
    Time,
    /// A struct value, by Go type name (`pkg.Type`, a [`GoStruct::go_type`]).
    Struct(&'static str),
    /// `*T`: nil → nil (always dropped), else `T` at `path + "*"`.
    Pointer(&'static GoKind),
    /// `[]T`: nil or empty → `[]`; elements at `path + ".[]"`.
    Slice(&'static GoKind),
    /// `map[string]T`: nil or empty → `{}`; values at `path + "->"`, keys unchanged.
    Map(&'static GoKind),
    /// `any` / an interface: nil → nil (dropped); else the dynamic value.
    Interface,
}

/// One Go struct field (`reflect.StructField`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GoField {
    /// Go field name (`LongRunningToolIDs`).
    pub(crate) name: &'static str,
    /// The raw `json` tag (`"longRunningToolIds,omitempty"`, `"-"`, or `""` when none), parsed
    /// by Go `fieldName`/`parseTag` rules at encode time.
    pub(crate) tag: &'static str,
    /// `reflect.StructField.Anonymous`: an embedded struct, flattened into its parent.
    pub(crate) embedded: bool,
    /// Exported (`IsExported`); an unexported field goes through Go `convertValue` / `omitzero`.
    pub(crate) exported: bool,
    /// `reflect.Type.String()` of the field (`"[]string"`, `"*genai.Content"`,
    /// `"map[string]interface {}"`): `json.Unmarshal` type-error text.
    pub(crate) go_type: &'static str,
    /// The field's kind.
    pub(crate) kind: GoKind,
}

/// One Go struct's shape: its fields in declaration order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GoStruct {
    /// `reflect.Type.String()` (`"session.Event"`).
    pub(crate) go_type: &'static str,
    /// The fields in Go declaration order.
    pub(crate) fields: &'static [GoField],
}

/// The shape named `go_type` among [`encode_shapes::ALL`](super::encode_shapes::ALL).
pub(crate) fn shape(go_type: &str) -> Option<&'static GoStruct> {
    let all = encode_shapes::ALL;
    all.binary_search_by(|s| s.go_type.cmp(go_type))
        .ok()
        .and_then(|i| all.get(i).copied())
}

/// `ConvertSnake`'s output tree: what `json.Encoder` then writes (objects with sorted keys).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum GoValue {
    /// Go nil (`null` when it reaches the encoder inside a map or slice).
    Null,
    /// `bool`.
    Bool(bool),
    /// `int64` (Go `v.Int()`).
    Int(i64),
    /// `float64` (Go `v.Float()`), written with Go's float text.
    Float(f64),
    /// `string`.
    String(String),
    /// `[]any`.
    Array(Vec<GoValue>),
    /// `map[string]any`: keys sorted.
    Object(BTreeMap<String, GoValue>),
    /// `json.RawMessage`: valid JSON text, written compacted with Go's HTML escaping and its
    /// number text unchanged.
    Raw(String),
}

impl Serialize for GoValue {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::Error as _;
        match self {
            GoValue::Null => serializer.serialize_unit(),
            GoValue::Bool(b) => serializer.serialize_bool(*b),
            GoValue::Int(i) => serializer.serialize_i64(*i),
            GoValue::Float(f) if f.is_finite() => serializer.serialize_f64(*f),
            // Go `encoding/json`: `json: unsupported value: NaN` (`+Inf`, `-Inf`).
            GoValue::Float(f) => Err(S::Error::custom(format!(
                "json: unsupported value: {}",
                if f.is_nan() {
                    "NaN"
                } else if *f > 0.0 {
                    "+Inf"
                } else {
                    "-Inf"
                }
            ))),
            GoValue::String(s) => serializer.serialize_str(s),
            GoValue::Array(a) => a.serialize(serializer),
            GoValue::Object(m) => m.serialize(serializer),
            GoValue::Raw(text) => serde_json::value::RawValue::from_string(compact_html(text))
                .map_err(S::Error::custom)?
                .serialize(serializer),
        }
    }
}

/// Go `json.Compact` with `escapeHTML` (what `json.Encoder` does to a `json.RawMessage`):
/// whitespace outside strings removed, `<`, `>`, `&`, U+2028 and U+2029 escaped, everything
/// else (number text, string escapes) unchanged.
fn compact_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_string = false;
    let mut escaped = false;
    for c in text.chars() {
        if in_string {
            if escaped {
                escaped = false;
                out.push(c);
                continue;
            }
            match c {
                '\\' => {
                    escaped = true;
                    out.push(c);
                }
                '"' => {
                    in_string = false;
                    out.push(c);
                }
                '<' => out.push_str("\\u003c"),
                '>' => out.push_str("\\u003e"),
                '&' => out.push_str("\\u0026"),
                '\u{2028}' => out.push_str("\\u2028"),
                '\u{2029}' => out.push_str("\\u2029"),
                _ => out.push(c),
            }
        } else {
            match c {
                ' ' | '\t' | '\n' | '\r' => {}
                '"' => {
                    in_string = true;
                    out.push(c);
                }
                _ => out.push(c),
            }
        }
    }
    out
}

/// `ConvertSnake`'s failure (Go's wrapped error text, e.g. `failed to convert regular struct
/// field with path: .X err: unsupported type: uint32`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub(crate) struct ConvertError(pub(crate) String);

/// A Rust value that reaches an NDJSON line, converted as Go's `convertSnake(path, "", o)`
/// converts its Go counterpart.
pub(crate) trait GoSnake {
    /// Go `convertSnake(path, …, o)`: `Ok(GoValue::Null)` is Go's nil result.
    fn go_snake(&self, path: &str) -> Result<GoValue, ConvertError>;
}

/// Go `ConvertSnake(o)`: `o.go_snake("")`.
pub(crate) fn convert_snake<T: GoSnake + ?Sized>(o: &T) -> Result<GoValue, ConvertError> {
    o.go_snake("")
}

/// Go `pathToName`: the paths whose name is not `convertName`'s.
const PATH_TO_NAME: [(&str, &str); 1] = [(".LongRunningToolIDs", "long_running_tool_ids")];

/// Go `convertName(path, name)`: the snake-case field name, with Go's `pathToName` exception
/// (`.LongRunningToolIDs` → `long_running_tool_ids`); byte-exact port (ASCII case rules).
pub(crate) fn convert_name(path: &str, name: &str) -> String {
    if let Some((_, res)) = PATH_TO_NAME.iter().find(|(p, _)| *p == path) {
        return (*res).to_owned();
    }
    let n = name.as_bytes();
    let lower = name.to_ascii_lowercase();
    let l = lower.as_bytes();
    let mut b = String::with_capacity(n.len() + 4);
    let mut after_underscore = true;
    for i in 0..n.len() {
        // Ab  => _ab
        if !after_underscore && i > 0 && i + 1 < n.len() && n[i] != l[i] && n[i + 1] == l[i + 1] {
            b.push('_');
            b.push(char::from(l[i]));
            after_underscore = true;
            continue;
        }
        // aB  => a_b
        if !after_underscore && i + 1 < n.len() && n[i] == l[i] && n[i + 1] != l[i + 1] {
            b.push(char::from(l[i]));
            b.push('_');
            after_underscore = true;
            continue;
        }
        after_underscore = false;
        b.push(char::from(l[i]));
    }
    b
}

/// Go `parseTag(tag)`: `(name, omitempty, omitzero, skip)`.
fn parse_tag(tag: &str) -> Result<(String, bool, bool, bool), ConvertError> {
    if tag.is_empty() {
        return Ok((String::new(), false, false, false));
    }
    if tag == "-" {
        return Ok((String::new(), false, false, true));
    }
    let mut name = String::new();
    let mut omit_empty = false;
    let mut omit_zero = false;
    for val in tag.split(',') {
        // ignore empty values
        match val {
            "" => {}
            "omitempty" => {
                if omit_empty {
                    return Err(ConvertError("duplicate omitempty".to_owned()));
                }
                omit_empty = true;
            }
            "omitzero" => {
                if omit_zero {
                    return Err(ConvertError("duplicate omitzero".to_owned()));
                }
                omit_zero = true;
            }
            _ => {
                if !name.is_empty() {
                    return Err(ConvertError("duplicate name".to_owned()));
                }
                name = val.to_owned();
            }
        }
    }
    // allow empty name
    Ok((name, omit_empty, omit_zero, false))
}

/// Go `fieldName(name, tag)` / `parseTag`: `(name, omitempty, omitzero, skip)`; a duplicate
/// `omitempty`/`omitzero`/name is `Err` (`duplicate omitempty` / `duplicate omitzero` /
/// `duplicate name`).
pub(crate) fn field_name(
    name: &str,
    tag: &str,
) -> Result<(String, bool, bool, bool), ConvertError> {
    let (new_name, omit_empty, omit_zero, skip) = parse_tag(tag)?;
    let new_name = if new_name.is_empty() {
        name.to_owned()
    } else {
        new_name
    };
    Ok((new_name, omit_empty, omit_zero, skip))
}

/// Unix seconds of Go's zero `time.Time` (`0001-01-01T00:00:00Z`).
const ZERO_TIME_SECS: i64 = -62_135_596_800;

/// Where `GoKind::Struct` names resolve: [`shape`], then `extra` (test-local shapes).
#[derive(Clone, Copy)]
struct Shapes<'a> {
    extra: &'a [&'static GoStruct],
}

impl Shapes<'_> {
    /// The generated shapes only.
    const GENERATED: Shapes<'static> = Shapes { extra: &[] };

    fn get(&self, go_type: &str) -> Result<&'static GoStruct, ConvertError> {
        shape(go_type)
            .or_else(|| self.extra.iter().copied().find(|s| s.go_type == go_type))
            .ok_or_else(|| ConvertError(format!("unsupported type: {go_type}")))
    }
}

/// [`convert_struct`] with test-local shapes `extra` also resolvable by name (Go's tests
/// convert structs declared inside the test function).
#[cfg(test)]
pub(crate) fn convert_struct_with(
    extra: &[&'static GoStruct],
    st: &GoStruct,
    value: &Value,
    path: &str,
) -> Result<GoValue, ConvertError> {
    convert_struct_in(Shapes { extra }, st, Some(value), path)
}

/// The value of the Go field `name` in `obj`: its exact key, else (as Go's `json.Unmarshal`
/// matches names) the first key equal to it ignoring ASCII case (`civil.Date`'s untagged
/// `Year`/`Month`/`Day` against genai's `year`/`month`/`day`).
fn lookup<'v>(obj: &'v Map<String, Value>, name: &str) -> Option<&'v Value> {
    obj.get(name).or_else(|| {
        obj.iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v)
    })
}

/// Go `convertSnake` of a struct: `value` holds the struct's fields keyed by json name (the
/// Rust value's serde output); a missing field is Go's zero value for its kind.
fn convert_struct_in(
    shapes: Shapes<'_>,
    st: &GoStruct,
    value: Option<&Value>,
    path: &str,
) -> Result<GoValue, ConvertError> {
    let obj = value.and_then(Value::as_object);
    let mut m = BTreeMap::new();
    for f in st.fields {
        let (name, omit_empty, _omit_zero, skip) = field_name(f.name, f.tag)
            .map_err(|e| ConvertError(format!("failed to parse tag ({}): {e}", f.tag)))?;
        // respect json "-"
        if skip {
            continue;
        }
        // handle embedded structs: flattened, so the parent's object holds their fields
        if f.embedded {
            let embed = convert_kind(shapes, &f.kind, f.go_type, value, &format!("{path}.{name}"))
                .map_err(|e| {
                    ConvertError(format!(
                        "failed to convert embedded struct with name:{name} o: {} {} err: {e}",
                        value.map(Value::to_string).unwrap_or_default(),
                        f.go_type
                    ))
                })?;
            if let GoValue::Object(embed) = embed {
                m.extend(embed);
            }
            continue;
        }
        // regular field
        let new_path = format!("{path}.{name}");
        let new_name = convert_name(&new_path, &name);
        let fv = obj.and_then(|o| lookup(o, &name));
        if f.exported {
            let val = convert_kind(shapes, &f.kind, f.go_type, fv, &new_path).map_err(|e| {
                ConvertError(format!(
                    "failed to convert regular struct field with path: {new_path} err: {e}"
                ))
            })?;
            if val == GoValue::Null {
                continue;
            }
            if !omit_empty || keeps_when_empty(&f.kind) || !is_empty(&val) {
                m.insert(new_name, val);
            }
        } else {
            // Go `val != 0 || !omitZero`: an int64/uint64/float64/string never equals the
            // untyped int 0, so omitzero never drops.
            m.insert(new_name, convert_value(&f.kind, fv));
        }
    }
    Ok(GoValue::Object(m))
}

/// Go `addIfNotEmpty`'s type switch: a named string type (through pointers) is not `string`,
/// so its default branch keeps it.
fn keeps_when_empty(kind: &GoKind) -> bool {
    match kind {
        GoKind::NamedString => true,
        GoKind::Pointer(inner) => keeps_when_empty(inner),
        _ => false,
    }
}

/// Go `addIfNotEmpty`'s empties: `map[string]any{}`, `[]any{}`, `false` and `""`.
fn is_empty(val: &GoValue) -> bool {
    match val {
        GoValue::Object(m) => m.is_empty(),
        GoValue::Array(a) => a.is_empty(),
        GoValue::Bool(b) => !*b,
        GoValue::String(s) => s.is_empty(),
        _ => false,
    }
}

/// Go `reflect.Value.Int()` of a field held in serde output: a number, a decimal string
/// (genai's `,string` int64s) or a Go duration string (genai's `time.Duration` text).
fn int_of(v: &Value) -> i64 {
    match v {
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_u64().map(|u| u as i64))
            .or_else(|| n.as_f64().map(|f| f as i64))
            .unwrap_or(0),
        Value::String(s) => s
            .parse::<i64>()
            .ok()
            .or_else(|| gadk_platform::go_parse_duration(s).ok().map(|d| d.0))
            .unwrap_or(0),
        _ => 0,
    }
}

/// Go `t.UnixMilli() / 1000` of a `time.Time` held as RFC 3339 text (Go's zero time when
/// absent).
fn time_secs(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::String(s)) => chrono::DateTime::parse_from_rfc3339(s)
            .map(|t| t.timestamp_millis() / 1000)
            .unwrap_or(ZERO_TIME_SECS),
        Some(Value::Number(n)) => n.as_i64().unwrap_or(ZERO_TIME_SECS),
        _ => ZERO_TIME_SECS,
    }
}

/// Go `reflect.Kind` text of a `uint*` field type (`unsupported type: uint32`).
fn uint_kind(go_type: &str) -> &str {
    match go_type.find("uint") {
        Some(i) => {
            let rest = &go_type[i..];
            let end = rest
                .find(|c: char| !c.is_ascii_alphanumeric())
                .unwrap_or(rest.len());
            &rest[..end]
        }
        None => "uint",
    }
}

/// Go `convertSnake(path, …, v)` of a value of `kind`; `v` is the serde output (`None`: absent,
/// i.e. Go's zero value).
fn convert_kind(
    shapes: Shapes<'_>,
    kind: &GoKind,
    go_type: &str,
    v: Option<&Value>,
    path: &str,
) -> Result<GoValue, ConvertError> {
    if *kind == GoKind::RawMessage {
        // empty → nil; anything present (even `null`) is the JSON text. Divergence (ledger
        // 5c): the text is re-serialised from serde's `Value` (keys sorted, number text
        // normalised, e.g. a jsonschema `Default`); Go keeps the author's compacted text.
        return Ok(v.map_or(GoValue::Null, |v| GoValue::Raw(v.to_string())));
    }
    let v = v.filter(|v| !v.is_null());
    Ok(match kind {
        GoKind::Bool => GoValue::Bool(v.and_then(Value::as_bool).unwrap_or(false)),
        GoKind::Int => GoValue::Int(v.map_or(0, int_of)),
        GoKind::Uint => {
            return Err(ConvertError(format!(
                "unsupported type: {}",
                uint_kind(go_type)
            )));
        }
        // Divergence (ledger 5c): a NaN or ±Inf float is already `null` in serde's `to_value`
        // output, so it becomes `0` here (a pointer to it is nil, then dropped); Go fails the
        // encode with `json: unsupported value: NaN` and logs the error line.
        GoKind::Float32 => GoValue::Float(
            v.and_then(Value::as_f64)
                .map_or(0.0, |f| f64::from(f as f32)),
        ),
        GoKind::Float64 => GoValue::Float(v.and_then(Value::as_f64).unwrap_or(0.0)),
        GoKind::String | GoKind::NamedString | GoKind::Bytes => {
            GoValue::String(v.and_then(Value::as_str).unwrap_or_default().to_owned())
        }
        GoKind::RawMessage => GoValue::Null,
        GoKind::Time => GoValue::Int(time_secs(v)),
        GoKind::Struct(name) => return convert_struct_in(shapes, shapes.get(name)?, v, path),
        GoKind::Pointer(inner) => match v {
            None => GoValue::Null,
            Some(v) => {
                return convert_kind(
                    shapes,
                    inner,
                    go_type.strip_prefix('*').unwrap_or(go_type),
                    Some(v),
                    &format!("{path}*"),
                );
            }
        },
        GoKind::Slice(inner) => {
            let elem_path = format!("{path}.[]");
            let elem_type = go_type.strip_prefix("[]").unwrap_or(go_type);
            let mut res = Vec::new();
            for e in v.and_then(Value::as_array).into_iter().flatten() {
                let elem =
                    convert_kind(shapes, inner, elem_type, Some(e), &elem_path).map_err(|err| {
                        ConvertError(format!(
                            "failed to convert slice element with path: {elem_path} err: {err}"
                        ))
                    })?;
                res.push(elem);
            }
            GoValue::Array(res)
        }
        GoKind::Map(inner) => {
            let elem_path = format!("{path}->");
            let elem_type = go_type.find(']').map_or(go_type, |i| &go_type[i + 1..]);
            let mut res = BTreeMap::new();
            for (k, e) in v.and_then(Value::as_object).into_iter().flatten() {
                let elem =
                    convert_kind(shapes, inner, elem_type, Some(e), &elem_path).map_err(|err| {
                        ConvertError(format!(
                            "failed to convert map element with path: {elem_path} err: {err}"
                        ))
                    })?;
                res.insert(k.clone(), elem);
            }
            GoValue::Object(res)
        }
        GoKind::Interface => v.map_or(GoValue::Null, dynamic),
    })
}

/// Go `convertValue` of an unexported field: ints and uints as `Int`, floats as `Float`,
/// strings as `String`; any other kind is the `reflect.Value` itself, which encodes as `{}`.
fn convert_value(kind: &GoKind, v: Option<&Value>) -> GoValue {
    let v = v.filter(|v| !v.is_null());
    match kind {
        GoKind::Int | GoKind::Uint => GoValue::Int(v.map_or(0, int_of)),
        GoKind::Float32 => GoValue::Float(
            v.and_then(Value::as_f64)
                .map_or(0.0, |f| f64::from(f as f32)),
        ),
        GoKind::Float64 => GoValue::Float(v.and_then(Value::as_f64).unwrap_or(0.0)),
        GoKind::String | GoKind::NamedString => {
            GoValue::String(v.and_then(Value::as_str).unwrap_or_default().to_owned())
        }
        _ => GoValue::Object(BTreeMap::new()),
    }
}

/// Go `convertSnake` of a dynamic (`any`) value as `encoding/json` decodes it: objects and
/// arrays recurse, an integer is `Int`, any other number `Float`.
fn dynamic(v: &Value) -> GoValue {
    match v {
        Value::Null => GoValue::Null,
        Value::Bool(b) => GoValue::Bool(*b),
        Value::Number(n) => match n.as_i64() {
            Some(i) => GoValue::Int(i),
            None => GoValue::Float(n.as_f64().unwrap_or(0.0)),
        },
        Value::String(s) => GoValue::String(s.clone()),
        Value::Array(a) => GoValue::Array(a.iter().map(dynamic).collect()),
        Value::Object(m) => dynamic_map(m),
    }
}

/// [`dynamic`] of a `map[string]any`.
fn dynamic_map(m: &Map<String, Value>) -> GoValue {
    GoValue::Object(m.iter().map(|(k, v)| (k.clone(), dynamic(v))).collect())
}

/// Go `EmitJSON(rw, o)`: one NDJSON line (`json.Encoder` output, i.e. JSON + `\n`, via
/// `gadk_model::to_go_json`) of `ConvertSnake(o)`. When the conversion fails, Go logs
/// `Failed to convert: %+v of type %T: %v` (ledgered: the dump is Rust `Debug`) and encodes the
/// original value: here `o`'s `Serialize`. An encoding failure is
/// `Err("failed to encode SSE response chunk: <err>")`.
pub(crate) fn emit_json<T>(o: &T, log: &Log) -> Result<Bytes, String>
where
    T: GoSnake + Serialize + std::fmt::Debug + ?Sized,
{
    let encoded = match convert_snake(o) {
        Ok(snake) => gadk_model::to_go_json(&snake),
        Err(err) => {
            log.print(&format!(
                "Failed to convert: {o:?} of type {}: {err}",
                std::any::type_name::<T>()
            ));
            // better to return an original version than nothing
            gadk_model::to_go_json(o)
        }
    };
    let mut line = encoded.map_err(|e| format!("failed to encode SSE response chunk: {e}"))?;
    line.push(b'\n');
    Ok(Bytes::from(line))
}

/// Go `EmitJSONError(rw, err)`: the line `{"error":"<text>"}` (through the same conversion and
/// encoder as [`emit_json`]). An encoding failure is `Err("failed to emit error: <err>")`.
pub(crate) fn emit_json_error(text: &str, log: &Log) -> Result<Bytes, String> {
    let mut json_err = Map::new();
    json_err.insert("error".to_owned(), Value::String(text.to_owned()));
    emit_json(&json_err, log).map_err(|e| format!("failed to emit error: {e}"))
}

impl GoSnake for Map<String, Value> {
    /// A Go `map[string]any`: keys unchanged, values as decoded JSON.
    fn go_snake(&self, _path: &str) -> Result<GoValue, ConvertError> {
        Ok(dynamic_map(self))
    }
}

/// Go `time.Duration` nanoseconds (saturating; Go's range is `i64`).
fn nanos(d: TimeDelta) -> i64 {
    d.num_nanoseconds().unwrap_or(if d < TimeDelta::zero() {
        i64::MIN
    } else {
        i64::MAX
    })
}

/// Writes `d`'s nanoseconds under `key` of the object `v` (genai's serde text is whole seconds;
/// Go's `ConvertSnake` reads the `time.Duration` itself).
fn set_nanos(v: &mut Value, key: &str, d: TimeDelta) {
    if let Some(o) = v.as_object_mut() {
        o.insert(key.to_owned(), Value::from(nanos(d)));
    }
}

fn patch_transcription(t: &Transcription, v: &mut Value) {
    set_nanos(v, "startOffset", t.start_offset);
    set_nanos(v, "endOffset", t.end_offset);
}

fn patch_content(c: &Content, v: &mut Value) {
    let Some(parts) = v.get_mut("parts").and_then(Value::as_array_mut) else {
        return;
    };
    for (p, pv) in c.parts.iter().zip(parts.iter_mut()) {
        if let (Some(vm), Some(vmv)) = (&p.video_metadata, pv.get_mut("videoMetadata")) {
            set_nanos(vmv, "startOffset", vm.start_offset);
            set_nanos(vmv, "endOffset", vm.end_offset);
        }
        if let (Some(t), Some(tv)) = (&p.audio_transcription, pv.get_mut("audioTranscription")) {
            patch_transcription(t, tv);
        }
    }
}

fn patch_grounding(g: &GroundingMetadata, v: &mut Value) {
    let Some(chunks) = v.get_mut("groundingChunks").and_then(Value::as_array_mut) else {
        return;
    };
    for (c, cv) in g.grounding_chunks.iter().zip(chunks.iter_mut()) {
        let route = c.maps.as_ref().and_then(|m| m.route.as_ref());
        let route_v = cv.get_mut("maps").and_then(|m| m.get_mut("route"));
        if let (Some(r), Some(rv)) = (route, route_v) {
            set_nanos(rv, "duration", r.duration);
        }
    }
}

/// The patches of the `model.LLMResponse` fields (flattened into `v`).
fn patch_response(r: &LlmResponse, v: &mut Value) {
    if let (Some(c), Some(cv)) = (&r.content, v.get_mut("content")) {
        patch_content(c, cv);
    }
    if let (Some(g), Some(gv)) = (&r.grounding_metadata, v.get_mut("groundingMetadata")) {
        patch_grounding(g, gv);
    }
    if let (Some(t), Some(tv)) = (&r.input_transcription, v.get_mut("inputTranscription")) {
        patch_transcription(t, tv);
    }
    if let (Some(t), Some(tv)) = (&r.output_transcription, v.get_mut("outputTranscription")) {
        patch_transcription(t, tv);
    }
}

/// `e`'s serde output (Go `session.Event`'s JSON keys) with the five genai `time.Duration`
/// fields as integer nanoseconds, which is what [`convert_struct_in`] walks.
fn event_value(e: &gadk_session::Event) -> Result<Value, ConvertError> {
    let mut v = serde_json::to_value(e).map_err(|err| ConvertError(err.to_string()))?;
    patch_response(&e.response, &mut v);
    let compacted = e
        .actions
        .compaction
        .as_ref()
        .and_then(|c| c.compacted_content.as_ref());
    let compacted_v = v
        .get_mut("actions")
        .and_then(|a| a.get_mut("compaction"))
        .and_then(|c| c.get_mut("compactedContent"));
    if let (Some(c), Some(cv)) = (compacted, compacted_v) {
        patch_content(c, cv);
    }
    Ok(v)
}

impl GoSnake for gadk_session::Event {
    /// `session.Event` over [`encode_shapes::SESSION_EVENT`](super::encode_shapes::SESSION_EVENT)
    /// (embedded `model.LLMResponse` flattened, `Timestamp` as Unix seconds, top-level
    /// `LongRunningToolIDs` → `long_running_tool_ids`).
    fn go_snake(&self, path: &str) -> Result<GoValue, ConvertError> {
        let v = event_value(self)?;
        convert_struct_in(
            Shapes::GENERATED,
            &encode_shapes::SESSION_EVENT,
            Some(&v),
            path,
        )
    }
}

impl GoSnake for StreamingAgentRunWithEventsResponse {
    /// `models.StreamingAgentRunWithEventsResponse` over
    /// [`encode_shapes::MODELS_STREAMING_AGENT_RUN_WITH_EVENTS_RESPONSE`](super::encode_shapes::MODELS_STREAMING_AGENT_RUN_WITH_EVENTS_RESPONSE)
    /// (its events at `.events.[]*`; the json name `longRunningToolIds` is what `convertName`
    /// sees, so it is `long_running_tool_ids` there too).
    fn go_snake(&self, path: &str) -> Result<GoValue, ConvertError> {
        let mut o = Map::new();
        if !self.events.is_empty() {
            let events = self
                .events
                .iter()
                .map(event_value)
                .collect::<Result<Vec<_>, _>>()?;
            o.insert("events".to_owned(), Value::Array(events));
        }
        if !self.artifacts.is_empty() {
            o.insert("artifacts".to_owned(), Value::Array(self.artifacts.clone()));
        }
        if !self.session_id.is_empty() {
            o.insert(
                "session_id".to_owned(),
                Value::String(self.session_id.clone()),
            );
        }
        convert_struct_in(
            Shapes::GENERATED,
            &encode_shapes::MODELS_STREAMING_AGENT_RUN_WITH_EVENTS_RESPONSE,
            Some(&Value::Object(o)),
            path,
        )
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use chrono::{DateTime, TimeDelta, Utc};
    use gadk_genai::{
        Citation, CitationMetadata, CivilDate, Content, FinishReason, GroundingChunk,
        GroundingChunkMaps, GroundingChunkMapsRoute, GroundingMetadata, LogprobsResult,
        LogprobsResultCandidate, Part, Transcription, VideoMetadata,
    };
    use gadk_session::{Event, EventActions, RequestInput};
    use parking_lot::Mutex;
    use pretty_assertions::assert_eq;
    use serde::Serialize;
    use serde_json::{Map, Value, json};

    use super::*;

    /// `shape` finds every generated table by its Go type (the probe writes `ALL` sorted by
    /// `go_type`, which the binary search relies on) and nothing else.
    #[test]
    fn shape_finds_every_table() {
        for s in encode_shapes::ALL {
            assert_eq!(shape(s.go_type), Some(*s), "{}", s.go_type);
        }
        assert!(shape("genai.Nope").is_none());
    }

    /// A [`Log`] that records every text it is given.
    fn recording_log() -> (Log, Arc<Mutex<Vec<String>>>) {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = lines.clone();
        let log = Log(Some(Arc::new(move |s: &str| {
            sink.lock().push(s.to_owned())
        })));
        (log, lines)
    }

    fn line(b: &Bytes) -> &str {
        std::str::from_utf8(b).unwrap()
    }

    /// The event of the Go probe below: every kind the encoder special-cases.
    fn go_truth_event() -> Event {
        let mut ev = Event {
            id: "e1".into(),
            timestamp: DateTime::<Utc>::from_timestamp(1_767_323_045, 999_000_000).unwrap(),
            invocation_id: "inv".into(),
            author: "a".into(),
            long_running_tool_ids: vec!["t1".into()],
            actions: EventActions {
                state_delta: Some(Map::from_iter([("k".to_owned(), json!("v"))])),
                artifact_delta: Some(BTreeMap::from([("f".to_owned(), 2)])),
                ..Default::default()
            },
            requested_input: Some(RequestInput {
                interrupt_id: "i".into(),
                response_schema: Some(json!({
                    "type": "object",
                    "default": {"a": "<b>"},
                    "items": {"type": "string"},
                    "properties": {"p": {"type": "string", "description": "d"}},
                    "required": ["p"],
                })),
                ..Default::default()
            }),
            ..Default::default()
        };
        let r = &mut ev.response;
        r.content = Some(Content {
            role: "model".into(),
            parts: vec![
                Part {
                    text: "x".into(),
                    video_metadata: Some(VideoMetadata {
                        start_offset: TimeDelta::milliseconds(1500),
                        end_offset: TimeDelta::seconds(2),
                        fps: Some(2.5),
                    }),
                    ..Default::default()
                },
                Part {
                    audio_transcription: Some(Transcription {
                        text: "a".into(),
                        end_offset: TimeDelta::milliseconds(250),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            ],
        });
        r.citation_metadata = Some(CitationMetadata {
            citations: vec![Citation {
                title: "t".into(),
                publication_date: CivilDate {
                    year: 2024,
                    month: 1,
                    day: 2,
                },
                ..Default::default()
            }],
        });
        r.logprobs_result = Some(LogprobsResult {
            chosen_candidates: vec![LogprobsResultCandidate {
                log_probability: 0.1,
                token: "tok".into(),
                ..Default::default()
            }],
            ..Default::default()
        });
        r.input_transcription = Some(Transcription {
            text: "hi".into(),
            start_offset: TimeDelta::milliseconds(1500),
            ..Default::default()
        });
        r.grounding_metadata = Some(GroundingMetadata {
            grounding_chunks: vec![GroundingChunk {
                maps: Some(GroundingChunkMaps {
                    route: Some(GroundingChunkMapsRoute {
                        duration: TimeDelta::milliseconds(90_500),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }],
            ..Default::default()
        });
        r.custom_metadata = Map::from_iter([
            ("n".to_owned(), json!(1)),
            ("f".to_owned(), json!(1.5)),
            ("s".to_owned(), json!("<x>")),
            ("nil".to_owned(), Value::Null),
        ]);
        r.finish_reason = Some(FinishReason::Stop);
        ev
    }

    /// The Go event line (ADK-Go `bb1fd51a`, `helper.EmitJSON` of the same `session.Event`):
    /// durations in nanoseconds, `civil.Date` as a struct, `float32` widened, `time.Time` as
    /// Unix seconds, zero numbers and named strings kept, jsonschema `type`/`items` dropped,
    /// `json.RawMessage` compacted and HTML-escaped, map `nil` kept as `null`. Go writes every
    /// `<` and `>` (here only inside strings) as `\u003c` / `\u003e`; [`go_event_line`] puts
    /// them back.
    const GO_EVENT_LINE: &str = r#"{"actions":{"artifact_delta":{"f":2},"state_delta":{"k":"v"}},"author":"a","avg_logprobs":0,"citation_metadata":{"citations":[{"end_index":0,"publication_date":{"day":2,"month":1,"year":2024},"start_index":0,"title":"t"}]},"content":{"parts":[{"media_processing":"","text":"x","video_metadata":{"end_offset":2000000000,"fps":2.5,"start_offset":1500000000}},{"audio_transcription":{"end_offset":250000000,"start_offset":0,"text":"a"},"media_processing":""}],"role":"model"},"custom_metadata":{"f":1.5,"n":1,"nil":null,"s":"<x>"},"finish_reason":"STOP","grounding_metadata":{"grounding_chunks":[{"maps":{"route":{"duration":90500000000}}}]},"id":"e1","input_transcription":{"end_offset":0,"start_offset":1500000000,"text":"hi"},"invocation_id":"inv","logprobs_result":{"chosen_candidates":[{"log_probability":0.10000000149011612,"token":"tok","token_id":0}]},"long_running_tool_ids":["t1"],"requested_input":{"interrupt_id":"i","response_schema":{"default":{"a":"<b>"},"properties":{"p":{"description":"d"}},"required":["p"]}},"timestamp":1767323045}"#;

    /// [`GO_EVENT_LINE`] with Go's HTML escaping.
    fn go_event_line() -> String {
        GO_EVENT_LINE
            .replace('<', "\\u003c")
            .replace('>', "\\u003e")
    }

    #[test]
    fn emit_json_matches_go_for_every_special_kind() {
        let (log, lines) = recording_log();
        let ev = go_truth_event();
        assert_eq!(
            line(&emit_json(&ev, &log).unwrap()),
            format!("{}\n", go_event_line())
        );
        // In the envelope the events sit at `.events.[]*`; `convertName` sees the json name
        // `longRunningToolIds`, so the key is `long_running_tool_ids` there too.
        let envelope = StreamingAgentRunWithEventsResponse {
            events: vec![ev],
            artifacts: vec![],
            session_id: "s".into(),
        };
        assert_eq!(
            line(&emit_json(&envelope, &log).unwrap()),
            format!(
                "{{\"events\":[{}],\"session_id\":\"s\"}}\n",
                go_event_line()
            )
        );
        assert_eq!(
            line(&emit_json(&StreamingAgentRunWithEventsResponse::default(), &log).unwrap()),
            "{}\n"
        );
        assert_eq!(
            line(&emit_json(&Event::default(), &log).unwrap()),
            "{\"actions\":{\"artifact_delta\":{},\"state_delta\":{}},\"author\":\"\",\
             \"avg_logprobs\":0,\"finish_reason\":\"\",\"id\":\"\",\"invocation_id\":\"\",\
             \"timestamp\":-62135596800}\n"
        );
        assert_eq!(
            line(&emit_json_error("a <b> & c", &log).unwrap()),
            "{\"error\":\"a \\u003cb\\u003e \\u0026 c\"}\n"
        );
        assert!(lines.lock().is_empty(), "{:?}", lines.lock());
    }

    #[test]
    fn convert_name_path_exception_is_path_exact() {
        assert_eq!(
            convert_name(".LongRunningToolIDs", "LongRunningToolIDs"),
            "long_running_tool_ids"
        );
        assert_eq!(
            convert_name(".events.[]*.LongRunningToolIDs", "LongRunningToolIDs"),
            "long_running_tool_i_ds"
        );
        assert_eq!(
            convert_name(".longRunningToolIds", "longRunningToolIds"),
            "long_running_tool_ids"
        );
    }

    #[test]
    fn field_name_follows_go_parse_tag() {
        let ok = |name: &str, tag: &str| field_name(name, tag).unwrap();
        assert_eq!(ok("X", ""), ("X".into(), false, false, false));
        assert_eq!(ok("X", "-"), ("X".into(), false, false, true));
        assert_eq!(ok("X", "-,"), ("-".into(), false, false, false));
        assert_eq!(ok("X", ",omitempty"), ("X".into(), true, false, false));
        assert_eq!(
            ok("X", "x,omitzero,omitempty"),
            ("x".into(), true, true, false)
        );
        let err = |tag: &str| field_name("X", tag).unwrap_err().0;
        assert_eq!(err("x,omitempty,omitempty"), "duplicate omitempty");
        assert_eq!(err("x,omitzero,omitzero"), "duplicate omitzero");
        assert_eq!(err("x,string"), "duplicate name");
    }

    /// A Go struct `struct { X uint32 }` (no `uint` field is reachable at `bb1fd51a`).
    const UINT_SHAPE: GoStruct = GoStruct {
        go_type: "helper.U",
        fields: &[GoField {
            name: "X",
            tag: "",
            embedded: false,
            exported: true,
            go_type: "uint32",
            kind: GoKind::Uint,
        }],
    };

    #[derive(Debug, Serialize)]
    #[serde(rename_all = "PascalCase")]
    struct Unsupported {
        x: u32,
    }

    impl GoSnake for Unsupported {
        fn go_snake(&self, path: &str) -> Result<GoValue, ConvertError> {
            convert_struct_with(&[], &UINT_SHAPE, &serde_json::to_value(self).unwrap(), path)
        }
    }

    #[test]
    fn convert_failure_logs_and_encodes_the_original() {
        let u = Unsupported { x: 7 };
        assert_eq!(
            convert_snake(&u).unwrap_err().0,
            "failed to convert regular struct field with path: .X err: unsupported type: uint32"
        );
        let (log, lines) = recording_log();
        assert_eq!(line(&emit_json(&u, &log).unwrap()), "{\"X\":7}\n");
        assert_eq!(
            *lines.lock(),
            vec![format!(
                "Failed to convert: Unsupported {{ x: 7 }} of type {}: failed to convert regular \
                 struct field with path: .X err: unsupported type: uint32",
                std::any::type_name::<Unsupported>()
            )]
        );
    }

    #[test]
    fn unsupported_float_is_an_encode_error() {
        let nan = GoValue::Object(BTreeMap::from([("f".to_owned(), GoValue::Float(f64::NAN))]));
        assert_eq!(
            gadk_model::to_go_json(&nan).unwrap_err().to_string(),
            "json: unsupported value: NaN"
        );
        let inf = GoValue::Float(f64::NEG_INFINITY);
        assert_eq!(
            gadk_model::to_go_json(&inf).unwrap_err().to_string(),
            "json: unsupported value: -Inf"
        );
    }

    #[test]
    fn raw_message_is_compacted_with_html_escaping() {
        let raw = GoValue::Raw("{ \"a\" : [1.50, \"<\\\"x>&\u{2028}\"] }".into());
        assert_eq!(
            String::from_utf8(gadk_model::to_go_json(&raw).unwrap()).unwrap(),
            "{\"a\":[1.50,\"\\u003c\\\"x\\u003e\\u0026\\u2028\"]}"
        );
    }

    /// The fixture's snake-case value of a Go struct back to its json names (Go's key for
    /// each field), with `time.Time` seconds as RFC 3339 and empty strings left out (the
    /// encoder writes Go's zero value for a missing field).
    fn unsnake_struct(st: &GoStruct, snake: &Map<String, Value>, path: &str) -> Map<String, Value> {
        let mut out = Map::new();
        for f in st.fields {
            let (name, _, _, skip) = field_name(f.name, f.tag).unwrap();
            if skip {
                continue;
            }
            let new_path = format!("{path}.{name}");
            if f.embedded {
                let GoKind::Struct(t) = f.kind else {
                    panic!("embedded {}", f.go_type)
                };
                out.extend(unsnake_struct(shape(t).unwrap(), snake, &new_path));
                continue;
            }
            if let Some(v) = snake.get(&convert_name(&new_path, &name))
                && let Some(v) = unsnake(&f.kind, v, &new_path)
            {
                out.insert(name, v);
            }
        }
        out
    }

    fn unsnake(kind: &GoKind, v: &Value, path: &str) -> Option<Value> {
        match kind {
            GoKind::String | GoKind::NamedString => {
                v.as_str().filter(|s| !s.is_empty()).map(|_| v.clone())
            }
            GoKind::Time => {
                let t = DateTime::<Utc>::from_timestamp(v.as_i64()?, 0)?;
                Some(Value::String(gadk_platform::go_format_rfc3339_nano(&t)))
            }
            GoKind::Struct(t) => Some(Value::Object(unsnake_struct(
                shape(t)?,
                v.as_object()?,
                path,
            ))),
            GoKind::Pointer(inner) => unsnake(inner, v, &format!("{path}*")),
            GoKind::Slice(inner) => Some(Value::Array(
                v.as_array()?
                    .iter()
                    .map(|e| unsnake(inner, e, &format!("{path}.[]")).unwrap_or(e.clone()))
                    .collect(),
            )),
            GoKind::Map(inner) => Some(Value::Object(
                v.as_object()?
                    .iter()
                    .map(|(k, e)| {
                        let e2 = unsnake(inner, e, &format!("{path}->")).unwrap_or(e.clone());
                        (k.clone(), e2)
                    })
                    .collect(),
            )),
            _ => Some(v.clone()),
        }
    }

    fn event_from_snake(snake: &Value, path: &str) -> Event {
        let camel = unsnake_struct(
            &encode_shapes::SESSION_EVENT,
            snake.as_object().unwrap(),
            path,
        );
        serde_json::from_value(Value::Object(camel)).unwrap()
    }

    /// Every NDJSON line Go's agentengine server wrote in the server fixtures (the
    /// scripted scenario's events, the `streaming_agent_run_with_events` envelopes and the
    /// model-error line) is reproduced byte for byte: each event is decoded back into a
    /// `gadk_session::Event` and re-encoded with `emit_json`.
    #[test]
    fn emit_json_reproduces_the_server_fixture_lines() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/agentengine");
        let mut names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .filter(|p| !p.to_string_lossy().ends_with(".deltas.json"))
            .collect();
        names.sort();
        let (log, lines) = recording_log();
        let mut checked = 0;
        for path in names {
            let fixture: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            if fixture["kind"] != "exchange"
                || fixture["request"]["target"] != "/stream_reasoning_engine"
                || fixture["response"]["status"] != 200
            {
                continue;
            }
            let body = fixture["response"]["body"].as_str().unwrap();
            for want in body.split_inclusive('\n') {
                let snake: Value = serde_json::from_str(want).unwrap();
                let got = if let Some(text) = snake.get("error").and_then(Value::as_str) {
                    emit_json_error(text, &log).unwrap()
                } else if let Some(events) = snake.get("events").and_then(Value::as_array) {
                    let resp = StreamingAgentRunWithEventsResponse {
                        events: events
                            .iter()
                            .map(|e| event_from_snake(e, ".events.[]*"))
                            .collect(),
                        artifacts: vec![],
                        session_id: snake["session_id"].as_str().unwrap_or_default().into(),
                    };
                    emit_json(&resp, &log).unwrap()
                } else {
                    emit_json(&event_from_snake(&snake, ""), &log).unwrap()
                };
                assert_eq!(line(&got), want, "{}", path.display());
                checked += 1;
            }
        }
        // main_09 (1), main_10 (1), main_11 (3), main_12 (error), main_14 (1), main_15 (1).
        assert_eq!(checked, 8);
        assert!(lines.lock().is_empty(), "{:?}", lines.lock());
    }
}
