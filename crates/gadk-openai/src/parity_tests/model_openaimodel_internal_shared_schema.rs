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

//! Port of ADK-Go `model/openaimodel/internal/shared/schema_test.go`.

use gadk_genai::{Schema, Type};
use pretty_assertions::assert_eq;
use serde::Serialize;
use serde_json::{Value, json};

use crate::error::ErrorKind;
use crate::shared::schema::{
    DEFAULT_RESPONSE_FORMAT_NAME, normalize_schema, response_format_name, schema_to_map,
};

// parity: adapted: Go's "invalid type" schema carries a channel in Example, which json.Marshal rejects; a gadk Schema's Example is a serde_json::Value, which always marshals, so that case asserts the conversion succeeds and leaves no type
#[test]
fn test_schema_to_map() {
    // (name, schema, wantErr, want["type"])
    let tests: Vec<(&str, Option<Schema>, bool, Option<Value>)> = vec![
        ("nil schema", None, false, None),
        (
            "string type",
            Some(Schema {
                r#type: Some(Type::String),
                ..Schema::default()
            }),
            false,
            // genai marshals "STRING"; SchemaToMap lowers it.
            Some(json!("string")),
        ),
        (
            "invalid type",
            Some(Schema {
                example: Some(json!({"unmarshalable": ["in", "Go", "only"]})),
                ..Schema::default()
            }),
            false,
            None,
        ),
    ];
    for (name, schema, want_err, want_type) in tests {
        let result = schema_to_map(schema.as_ref());
        assert_eq!(
            result.is_err(),
            want_err,
            "{name}: SchemaToMap() error = {result:?}, wantErr {want_err}"
        );
        let got = result.unwrap_or_default();
        assert_eq!(
            got.as_ref().and_then(|m| m.get("type")).cloned(),
            want_type,
            "{name}: unexpected map: {got:?}"
        );
    }
}

/// Go's `struct{ Type string `json:"type"` }` schema.
#[derive(Serialize)]
struct TypeOnly {
    #[serde(rename = "type")]
    kind: &'static str,
}

// parity: adapted: normalize_schema takes a JSON value, so the "struct schema" is the struct serialized to one and the "invalid schema" (Go: a func, which json.Marshal rejects) is a JSON array, which the map decode rejects; both still fail with an error
#[test]
fn test_normalize_schema() {
    // (name, schema, want["type"], wantErr)
    let tests: Vec<(&str, Option<Value>, Option<Value>, bool)> = vec![
        ("nil schema", None, None, true),
        (
            "map schema",
            Some(json!({"type": "object"})),
            Some(json!("object")),
            false,
        ),
        (
            "struct schema",
            Some(serde_json::to_value(TypeOnly { kind: "array" }).unwrap()),
            Some(json!("array")),
            false,
        ),
        ("invalid schema", Some(json!([])), None, true),
    ];
    for (name, schema, want_type, want_err) in tests {
        let result = normalize_schema(schema.as_ref());
        assert_eq!(
            result.is_err(),
            want_err,
            "{name}: NormalizeSchema() error = {result:?}, wantErr {want_err}"
        );
        if let Ok(got) = result {
            assert_eq!(
                got.as_ref().and_then(|m| m.get("type")).cloned(),
                want_type,
                "{name}: NormalizeSchema() = {got:?}"
            );
        }
    }
    // The nil schema is the bare sentinel.
    let err = normalize_schema(None).unwrap_err();
    assert!(err.is(ErrorKind::EmptyJsonSchema));
    assert_eq!(err.to_string(), "openai: empty json schema");
}

#[test]
fn test_response_format_name() {
    let titled = |title: &str| {
        Some(Schema {
            title: title.to_string(),
            ..Schema::default()
        })
    };
    let a64 = "a".repeat(64);
    let a65 = "a".repeat(65);
    let tests: Vec<(&str, Option<Schema>, &str)> = vec![
        ("nil schema", None, DEFAULT_RESPONSE_FORMAT_NAME),
        (
            "no title",
            Some(Schema {
                r#type: Some(Type::Object),
                ..Schema::default()
            }),
            DEFAULT_RESPONSE_FORMAT_NAME,
        ),
        (
            "title already a legal name",
            titled("Custom-Title_2"),
            "Custom-Title_2",
        ),
        ("title of exactly 64 characters is kept", titled(&a64), &a64),
        (
            "title with a space falls back to the default",
            titled("Weather Report"),
            DEFAULT_RESPONSE_FORMAT_NAME,
        ),
        (
            "title with a trailing newline falls back to the default",
            titled("Weather\n"),
            DEFAULT_RESPONSE_FORMAT_NAME,
        ),
        (
            "non-ascii title falls back to the default",
            titled("\u{5929}\u{6c17}\u{4e88}\u{5831}"),
            DEFAULT_RESPONSE_FORMAT_NAME,
        ),
        (
            "title longer than 64 characters falls back to the default",
            titled(&a65),
            DEFAULT_RESPONSE_FORMAT_NAME,
        ),
    ];
    for (name, schema, want) in tests {
        assert_eq!(response_format_name(schema.as_ref()), want, "{name}");
    }
}
