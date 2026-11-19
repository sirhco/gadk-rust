// Copyright 2026 Chris Olson
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

//! Wording for JSON type-mismatch errors: the kind of a JSON value (`an array`, `a string`) and
//! the `expected <shape>, found <kind>` text built from it.

use serde_json::Value;

/// The kind of a JSON value as a bare noun: `null`, `boolean`, `number`, `string`, `array` or
/// `object`.
pub fn json_kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// The kind of a JSON value with its article, as an error names what it found: `null`,
/// `a boolean`, `a number`, `a string`, `an array` or `an object`.
pub fn json_kind_article(v: &Value) -> &'static str {
    article(json_kind(v))
}

/// [`json_kind_article`] for raw JSON text, judged by its first non-whitespace byte (the text is
/// assumed to be one valid JSON value; anything unrecognised counts as a number).
pub fn json_text_kind_article(text: &str) -> &'static str {
    match text.trim_start().as_bytes().first() {
        Some(b'n') => "null",
        Some(b't' | b'f') => "a boolean",
        Some(b'"') => "a string",
        Some(b'[') => "an array",
        Some(b'{') => "an object",
        _ => "a number",
    }
}

/// `expected <expected>, found <kind of found>`, e.g. `expected a JSON object, found an array`.
pub fn json_expected_found(expected: &str, found: &Value) -> String {
    format!("expected {expected}, found {}", json_kind_article(found))
}

/// The article form of a bare kind noun from [`json_kind`].
fn article(kind: &'static str) -> &'static str {
    match kind {
        "boolean" => "a boolean",
        "number" => "a number",
        "string" => "a string",
        "array" => "an array",
        "object" => "an object",
        _ => "null",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn kinds_and_articles() {
        for (v, kind, with_article, text) in [
            (json!(null), "null", "null", "null"),
            (json!(true), "boolean", "a boolean", " false"),
            (json!(1.5), "number", "a number", "-1"),
            (json!("s"), "string", "a string", "\"s\""),
            (json!([1]), "array", "an array", "[1]"),
            (json!({"a": 1}), "object", "an object", "{}"),
        ] {
            assert_eq!(json_kind(&v), kind);
            assert_eq!(json_kind_article(&v), with_article);
            assert_eq!(json_text_kind_article(text), with_article);
        }
    }

    #[test]
    fn expected_found_text() {
        assert_eq!(
            json_expected_found("a JSON object", &json!([])),
            "expected a JSON object, found an array"
        );
    }
}
