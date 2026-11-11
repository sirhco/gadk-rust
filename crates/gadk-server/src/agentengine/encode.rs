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
#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

//! Go `server/agentengine/internal/helper/encode.go` (`ConvertSnake`) and `emit_json.go`
//! (`EmitJSON`, `EmitJSONError`).
//!
//! Go's `ConvertSnake` walks a value by reflection; serde output cannot recover what it reads
//! (pointer vs value, `omitempty`, named string types, `time.Time`, `[]byte`, `float32`), so the
//! walk is driven by Go's struct shapes ([`GoStruct`], generated into
//! [`encode_shapes`](super::encode_shapes) by `scripts/gen-go-agentengine --shapes`) and each
//! Rust type that reaches an NDJSON line implements [`GoSnake`] over its shape.

use std::collections::BTreeMap;

use bytes::Bytes;
use serde::Serialize;

use super::Log;
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
    todo!("Task 6")
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
        todo!("Task 6")
    }
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
    todo!("Task 6")
}

/// Go `convertName(path, name)`: the snake-case field name, with Go's `pathToName` exception
/// (`.LongRunningToolIDs` → `long_running_tool_ids`); byte-exact port (ASCII case rules).
pub(crate) fn convert_name(path: &str, name: &str) -> String {
    todo!("Task 6")
}

/// Go `fieldName(name, tag)` / `parseTag`: `(name, omitempty, omitzero, skip)`; a duplicate
/// `omitempty`/`omitzero`/name is `Err` (`duplicate omitempty` / `duplicate omitzero` /
/// `duplicate name`).
pub(crate) fn field_name(
    name: &str,
    tag: &str,
) -> Result<(String, bool, bool, bool), ConvertError> {
    todo!("Task 6")
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
    todo!("Task 6")
}

/// Go `EmitJSONError(rw, err)`: the line `{"error":"<text>"}` (through the same conversion and
/// encoder as [`emit_json`]). An encoding failure is `Err("failed to emit error: <err>")`.
pub(crate) fn emit_json_error(text: &str, log: &Log) -> Result<Bytes, String> {
    todo!("Task 6")
}

impl GoSnake for gadk_session::Event {
    /// `session.Event` over [`encode_shapes::SESSION_EVENT`](super::encode_shapes::SESSION_EVENT)
    /// (embedded `model.LLMResponse` flattened, `Timestamp` as Unix seconds, top-level
    /// `LongRunningToolIDs` → `long_running_tool_ids`).
    fn go_snake(&self, path: &str) -> Result<GoValue, ConvertError> {
        todo!("Task 6")
    }
}

impl GoSnake for StreamingAgentRunWithEventsResponse {
    /// `models.StreamingAgentRunWithEventsResponse` over
    /// [`encode_shapes::MODELS_STREAMING_AGENT_RUN_WITH_EVENTS_RESPONSE`](super::encode_shapes::MODELS_STREAMING_AGENT_RUN_WITH_EVENTS_RESPONSE)
    /// (its events at `.events.[]*`, so their `LongRunningToolIDs` is `long_running_tool_i_ds`).
    fn go_snake(&self, path: &str) -> Result<GoValue, ConvertError> {
        todo!("Task 6")
    }
}
