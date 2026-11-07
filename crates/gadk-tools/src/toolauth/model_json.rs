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

//! Go-compatible JSON for the toolauth models (port of ADK-Go `tool/toolauth/model_json.go` and
//! `digest`'s tree). Task 1 placeholder: Task 6 replaces every `todo!` body.

#![allow(dead_code, unused_variables, clippy::todo)]

use serde_json::value::RawValue;

use super::Extra;

/// Go's `any` after a `UseNumber` decode: numbers keep their literal text; object keys sort
/// bytewise (Go map order).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GoJson {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<GoJson>),
    Object(std::collections::BTreeMap<String, GoJson>),
}

impl GoJson {
    pub(crate) fn parse(text: &str) -> Result<GoJson, serde_json::Error> {
        todo!("Task 6")
    }

    /// Infallible: `RawValue` holds valid JSON.
    pub(crate) fn from_raw(raw: &RawValue) -> GoJson {
        todo!("Task 6")
    }

    /// Compact, sorted keys, no trailing newline. `escape_html = true` is `json.Marshal`
    /// (`<` `>` `&` as `\u003c` `\u003e` `\u0026`); `false` is an Encoder with `SetEscapeHTML(false)`.
    /// U+2028/U+2029 are always escaped; other non-ASCII is raw.
    pub(crate) fn encode(&self, escape_html: bool) -> String {
        todo!("Task 6")
    }
}

/// Go `cmp` equality of two `Extra` maps (each value compared as `GoJson`).
pub(crate) fn extra_eq(a: &Extra, b: &Extra) -> bool {
    todo!("Task 6")
}

/// Go `json.Marshal` of one string (HTML-escaped).
pub(crate) fn go_string(s: &str) -> String {
    todo!("Task 6")
}

/// Go `pythonName` (model_json.go): "type" → "type_", camelCase → snake_case.
pub(crate) fn python_name(alias: &str) -> String {
    todo!("Task 6")
}

/// Go `decode` (model_json.go): one JSON object split into known fields (by Go alias, the
/// camelCase spelling winning over its Python name) and extras. `Err` is Go's message text.
pub(crate) struct Decoded {
    pub(crate) known: std::collections::BTreeMap<String, Box<RawValue>>,
    pub(crate) extra: Extra,
}

pub(crate) fn decode_object(
    raw: &RawValue,
    type_name: &str,
    aliases: &[&str],
    python_names: bool,
    required: &[&str],
) -> Result<Decoded, String> {
    todo!("Task 6")
}

impl Decoded {
    /// Decode one known field; a type mismatch is Go's
    /// "toolauth: <type_name>: field <alias>: cannot use a JSON <kind> as <go_type>".
    pub(crate) fn take<T: serde::de::DeserializeOwned>(
        &mut self,
        alias: &str,
        type_name: &str,
        go_type: &str,
    ) -> Result<Option<T>, String> {
        todo!("Task 6")
    }
}

/// Go `encode` (model_json.go): known fields in Go struct order, then extras sorted, skipping an
/// extra whose key is a field alias (or, with `python_names`, a field's Python name).
pub(crate) struct ObjectWriter {
    buf: String,
}

impl ObjectWriter {
    pub(crate) fn new() -> Self {
        todo!("Task 6")
    }

    /// Append `"alias":<json>`; `json` is already Go-encoded.
    pub(crate) fn field(&mut self, alias: &str, json: &str) {
        todo!("Task 6")
    }

    pub(crate) fn finish(self, extra: &Extra, aliases: &[&str], python_names: bool) -> String {
        todo!("Task 6")
    }
}

/// Every model's Go `MarshalJSON` bytes (HTML-escaped, Go field order, extras last).
pub(crate) trait GoMarshal {
    fn go_marshal(&self) -> String;
}

/// `Serialize` body shared by every model: emits `go_marshal()` as a `RawValue`, so a
/// serde_json serializer writes Go's bytes verbatim.
pub(crate) fn serialize_go<T: GoMarshal, S: serde::Serializer>(
    v: &T,
    s: S,
) -> Result<S::Ok, S::Error> {
    todo!("Task 6")
}
