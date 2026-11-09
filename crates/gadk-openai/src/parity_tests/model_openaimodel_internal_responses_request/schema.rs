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

//! `responses/request_test.go`: the schema tests (see `mod.rs`).

use std::collections::BTreeMap;

use gadk_genai::{Content, GenerateContentConfig, Schema, Type};
use gadk_model::LlmRequest;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use crate::parity_tests::model_openaimodel_internal_responses_tools::{wire, wire_value};
use crate::responses::request::build_params;
use crate::responses::schema::new_json_schema_format;
use crate::shared::schema::DEFAULT_RESPONSE_FORMAT_NAME;

/// The encoded `ResponseFormatTextJSONSchemaConfigParam` Go's want struct stands for, in
/// openai-go's field order (`name`, `schema`, `strict`, `type`).
fn want_format(name: &str, schema: Value) -> String {
    format!(
        r#"{{"name":{},"schema":{},"strict":true,"type":"json_schema"}}"#,
        wire(&name),
        wire(&schema)
    )
}

fn json_schema(schema: Value) -> GenerateContentConfig {
    GenerateContentConfig {
        response_json_schema: Some(schema),
        ..GenerateContentConfig::default()
    }
}

fn typed(t: Type) -> Schema {
    Schema {
        r#type: Some(t),
        ..Schema::default()
    }
}

fn titled_object(title: &str, properties: BTreeMap<String, Schema>) -> Schema {
    Schema {
        title: title.to_string(),
        r#type: Some(Type::Object),
        properties,
        ..Schema::default()
    }
}

/// `{"type":"object"}` made strict: what every property-less object becomes.
fn strict_empty_object() -> Value {
    json!({
        "type": "object",
        "properties": {},
        "additionalProperties": false,
        "required": [],
    })
}

// parity: adapted: Go's "with invalid json schema" passes a func, which json.Marshal rejects; a gadk JSON schema is a serde_json::Value, which always marshals, so that case is a JSON array, which the map decode rejects
#[test]
fn test_new_json_schema_format() {
    let tests: Vec<(&str, GenerateContentConfig, Option<String>)> = vec![
        ("no schema", GenerateContentConfig::default(), None),
        (
            "with response schema",
            GenerateContentConfig {
                response_schema: Some(titled_object("CustomTitle", BTreeMap::new())),
                ..GenerateContentConfig::default()
            },
            Some(want_format(
                "CustomTitle",
                json!({
                    "title": "CustomTitle",
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false,
                    "required": [],
                }),
            )),
        ),
        // OpenAI accepts only ^[a-zA-Z0-9_-]{1,64}$ in the name, so an illegal title falls back
        // to the default name and the body keeps the title the caller wrote.
        (
            "response schema title that is not a legal format name",
            GenerateContentConfig {
                response_schema: Some(titled_object("Weather Report", BTreeMap::new())),
                ..GenerateContentConfig::default()
            },
            Some(want_format(
                DEFAULT_RESPONSE_FORMAT_NAME,
                json!({
                    "title": "Weather Report",
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false,
                    "required": [],
                }),
            )),
        ),
        (
            "with json schema",
            json_schema(json!({"type": "object"})),
            Some(want_format("adk_response", strict_empty_object())),
        ),
        (
            "property-less nested object gets the strict keys",
            json_schema(json!({
                "type": "object",
                "properties": {
                    "city": {"type": "string"},
                    "audit": {"type": "object"},
                },
            })),
            Some(want_format(
                "adk_response",
                json!({
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["audit", "city"],
                    "properties": {
                        "city": {"type": "string"},
                        "audit": strict_empty_object(),
                    },
                }),
            )),
        ),
        (
            "property-less object inside array items and $defs",
            json_schema(json!({
                "type": "object",
                "properties": {
                    "rows": {"type": "array", "items": {"type": "object"}},
                    "bag": {"$ref": "#/$defs/bag"},
                },
                "$defs": {"bag": {"type": "object"}},
            })),
            Some(want_format(
                "adk_response",
                json!({
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["bag", "rows"],
                    "properties": {
                        "rows": {"type": "array", "items": strict_empty_object()},
                        "bag": {"$ref": "#/$defs/bag"},
                    },
                    "$defs": {"bag": strict_empty_object()},
                }),
            )),
        ),
        (
            "non-object properties are left alone",
            json_schema(json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "tags": {"type": "array", "items": {"type": "string"}},
                },
            })),
            Some(want_format(
                "adk_response",
                json!({
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["name", "tags"],
                    "properties": {
                        "name": {"type": "string"},
                        "tags": {"type": "array", "items": {"type": "string"}},
                    },
                }),
            )),
        ),
        // The one case where a caller's value is replaced rather than added to: a non-object
        // properties is invalid JSON Schema.
        (
            "properties that is not an object becomes empty",
            json_schema(json!({"type": "object", "properties": "garbage"})),
            Some(want_format("adk_response", strict_empty_object())),
        ),
        (
            "properties that is null becomes empty",
            json_schema(json!({"type": "object", "properties": null})),
            Some(want_format("adk_response", strict_empty_object())),
        ),
        (
            "property-less object in an anyOf branch",
            json_schema(json!({
                "type": "object",
                "properties": {
                    "m": {"anyOf": [{"type": "object"}, {"type": "string"}]},
                },
            })),
            Some(want_format(
                "adk_response",
                json!({
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["m"],
                    "properties": {
                        "m": {"anyOf": [strict_empty_object(), {"type": "string"}]},
                    },
                }),
            )),
        ),
        // A node carrying properties without declaring type: object is not rewritten itself,
        // but the walk still descends into it.
        (
            "properties are walked even when the parent declares no type",
            json_schema(json!({"properties": {"a": {"type": "object"}}})),
            Some(want_format(
                "adk_response",
                json!({"properties": {"a": strict_empty_object()}}),
            )),
        ),
        // Strict mode accepts only additionalProperties=false, so a map cannot survive.
        (
            "a map written as additionalProperties becomes an empty object",
            json_schema(json!({
                "type": "object",
                "additionalProperties": {"type": "string"},
            })),
            Some(want_format("adk_response", strict_empty_object())),
        ),
        // Sibling stripping runs before the object rewrite.
        (
            "a $ref keeps no siblings even when it declares type object",
            json_schema(json!({
                "$defs": {"x": {"type": "object"}},
                "$ref": "#/$defs/x",
                "type": "object",
            })),
            Some(want_format("adk_response", json!({"$ref": "#/$defs/x"}))),
        ),
        (
            "with nested response schema",
            GenerateContentConfig {
                response_schema: Some(titled_object(
                    "NestedTitle",
                    BTreeMap::from([
                        ("b_string".to_string(), typed(Type::String)),
                        (
                            "a_object".to_string(),
                            titled_object(
                                "",
                                BTreeMap::from([
                                    ("d_int".to_string(), typed(Type::Integer)),
                                    ("c_bool".to_string(), typed(Type::Boolean)),
                                ]),
                            ),
                        ),
                    ]),
                )),
                ..GenerateContentConfig::default()
            },
            Some(want_format(
                "NestedTitle",
                json!({
                    "title": "NestedTitle",
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["a_object", "b_string"],
                    "properties": {
                        "b_string": {"type": "string"},
                        "a_object": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["c_bool", "d_int"],
                            "properties": {
                                "d_int": {"type": "integer"},
                                "c_bool": {"type": "boolean"},
                            },
                        },
                    },
                }),
            )),
        ),
        (
            "with complex json schema for strict output",
            json_schema(json!({
                "title": "NestedTitle",
                "type": "object",
                "properties": {
                    "b_string": {"type": "string"},
                    "a_object": {
                        "type": "object",
                        "properties": {
                            "d_int": {"type": "integer"},
                            "c_bool": {"type": "boolean"},
                        },
                    },
                    "c_array": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {"e_float": {"type": "number"}},
                        },
                    },
                    "d_ref": {"$ref": "#/$defs/my_def", "description": "this should be deleted"},
                },
                "$defs": {
                    "my_def": {
                        "type": "object",
                        "properties": {"f_string": {"type": "string"}},
                    },
                },
                "anyOf": [
                    {"type": "object", "properties": {"g_string": {"type": "string"}}},
                ],
            })),
            Some(want_format(
                "adk_response",
                json!({
                    "title": "NestedTitle",
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["a_object", "b_string", "c_array", "d_ref"],
                    "properties": {
                        "b_string": {"type": "string"},
                        "a_object": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["c_bool", "d_int"],
                            "properties": {
                                "d_int": {"type": "integer"},
                                "c_bool": {"type": "boolean"},
                            },
                        },
                        "c_array": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "additionalProperties": false,
                                "required": ["e_float"],
                                "properties": {"e_float": {"type": "number"}},
                            },
                        },
                        "d_ref": {"$ref": "#/$defs/my_def"},
                    },
                    "$defs": {
                        "my_def": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["f_string"],
                            "properties": {"f_string": {"type": "string"}},
                        },
                    },
                    "anyOf": [
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["g_string"],
                            "properties": {"g_string": {"type": "string"}},
                        },
                    ],
                }),
            )),
        ),
        ("with invalid json schema", json_schema(json!([])), None),
    ];
    for (name, cfg, want) in tests {
        let got = new_json_schema_format(&cfg);
        assert_eq!(
            got.is_err(),
            want.is_none(),
            "{name}: newJSONSchemaFormat() = {got:?}, wantErr {}",
            want.is_none()
        );
        if let (Ok(got), Some(want)) = (got, want) {
            assert_eq!(wire(&got), want, "{name}: newJSONSchemaFormat()");
        }
    }
    let err = new_json_schema_format(&GenerateContentConfig::default()).unwrap_err();
    assert_eq!(
        err.to_string(),
        "openai: json schema requested without schema"
    );
}

#[test]
fn test_new_json_schema_format_does_not_mutate_response_json_schema() {
    let schema = json!({
        "type": "object",
        "properties": {
            "nested": {
                "type": "object",
                "properties": {"value": {"type": "string"}},
            },
            "reference": {"$ref": "#/$defs/item", "description": "caller-owned metadata"},
        },
        "$defs": {
            "item": {
                "type": "object",
                "properties": {"id": {"type": "integer"}},
            },
        },
    });
    let want = wire(&schema);
    let cfg = json_schema(schema);

    let format = new_json_schema_format(&cfg).unwrap();
    let additional = format
        .schema
        .as_ref()
        .and_then(|schema| schema.get("additionalProperties"));
    assert_eq!(
        additional,
        Some(&json!(false)),
        "newJSONSchemaFormat() additionalProperties, want false"
    );

    assert_eq!(
        cfg.response_json_schema.as_ref().map(wire),
        Some(want),
        "newJSONSchemaFormat() mutated ResponseJsonSchema"
    );
}

#[test]
fn test_build_params_preserves_large_json_schema_integers() {
    const MINIMUM: i64 = 9_007_199_254_740_993;
    let req = LlmRequest {
        contents: vec![Content::from_text("return a count", "user")],
        config: Some(json_schema(json!({
            "type": "object",
            "properties": {
                "count": {"type": "integer", "minimum": MINIMUM},
            },
        }))),
        ..LlmRequest::default()
    };
    let params = build_params("gpt-4o-mini", &req).unwrap();
    let data = wire(&params);
    let want = r#""minimum":9007199254740993"#;
    assert!(
        data.contains(want),
        "encoded params = {data}, want exact integer constraint {want}"
    );
    assert_eq!(
        wire_value(&params)["text"]["format"]["schema"]["properties"]["count"]["minimum"],
        json!(MINIMUM)
    );
}
