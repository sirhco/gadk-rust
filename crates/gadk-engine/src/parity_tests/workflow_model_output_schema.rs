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

//! Port of Go workflow/model_output_schema_test.go (be529e7b, #2194).

use std::collections::BTreeMap;

use gadk_genai::{Schema, Type};
use serde_json::{Value, json};

use crate::workflow::model_output_schema::model_output_schema;

fn t(typ: Type) -> Schema {
    Schema {
        r#type: Some(typ),
        ..Schema::default()
    }
}

fn nullable(mut s: Schema) -> Schema {
    s.nullable = Some(true);
    s
}

fn array(items: Schema) -> Schema {
    Schema {
        r#type: Some(Type::Array),
        items: Some(Box::new(items)),
        ..Schema::default()
    }
}

fn object(props: Vec<(&str, Schema)>, required: &[&str]) -> Schema {
    Schema {
        r#type: Some(Type::Object),
        properties: props
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect::<BTreeMap<_, _>>(),
        required: required.iter().map(|r| r.to_string()).collect(),
        ..Schema::default()
    }
}

/// Go `closedObject`: an object schema with the given properties and no other keys allowed.
fn closed_object(props: Value) -> Value {
    json!({"type": "object", "properties": props, "additionalProperties": false})
}

/// The schema jsonschema-go's `For[schemaShapes]` resolves (Go derives it from the struct).
fn schema_shapes() -> Value {
    let inner = json!({
        "type": "object",
        "properties": {"n": {"type": "integer"}},
        "required": ["n"],
        "additionalProperties": false
    });
    let mut maybe = inner.clone();
    maybe["type"] = json!(["null", "object"]);
    json!({
        "type": "object",
        "properties": {
            "name": {"type": "string", "description": "the name"},
            "score": {"type": "number"},
            "ok": {"type": "boolean"},
            "note": {"type": ["null", "string"]},
            "tags": {"type": ["null", "array"], "items": {"type": "string"}},
            "inner": inner,
            "maybe": maybe,
            "many": {"type": ["null", "array"], "items": inner},
            "count": {"type": "integer", "minimum": 0},
            "Opaque": {"type": ["null", "array"], "items": {"type": "integer", "minimum": 0, "maximum": 255}}
        },
        "required": ["name", "ok", "note", "tags", "inner", "maybe", "many", "count", "Opaque"],
        "additionalProperties": false
    })
}

// parity: adapted: Go derives the "struct shapes" schema from a Go struct with jsonschema.For; the
// port spells out that resolved schema as JSON (gadk's schemas are JSON values). Go's falseSchema
// `{"not": {}}` is written as JSON `false`, which jsonschema-go marshals it to.
#[test]
fn test_model_output_schema() {
    let inner = object(vec![("n", t(Type::Integer))], &["n"]);
    let nullable_inner = nullable(inner.clone());
    let mut named = t(Type::String);
    named.description = "the name".into();
    let cases: Vec<(&str, Value, Option<Schema>)> = vec![
        (
            // Pointer and slice fields keep Nullable, so set_model_response accepts a null for
            // them as ValidateOutput does.
            "struct shapes",
            schema_shapes(),
            Some(object(
                vec![
                    ("name", named),
                    ("score", t(Type::Number)),
                    ("ok", t(Type::Boolean)),
                    ("note", nullable(t(Type::String))),
                    ("tags", nullable(array(t(Type::String)))),
                    ("inner", inner.clone()),
                    ("maybe", nullable_inner),
                    ("many", nullable(array(inner.clone()))),
                    // uint's minimum: 0 is dropped; ValidateOutput still checks it.
                    ("count", t(Type::Integer)),
                    // json.RawMessage is a byte array with bounds on each item.
                    ("Opaque", nullable(array(t(Type::Integer)))),
                ],
                &[
                    "name", "ok", "note", "tags", "inner", "maybe", "many", "count", "Opaque",
                ],
            )),
        ),
        (
            "string enum and titles",
            json!({
                "type": "object", "title": "Report", "additionalProperties": false,
                "properties": {"level": {"type": "string", "title": "Level", "enum": ["low", "high"]}}
            }),
            Some({
                let mut level = t(Type::String);
                level.title = "Level".into();
                level.r#enum = vec!["low".into(), "high".into()];
                let mut s = object(vec![("level", level)], &[]);
                s.title = "Report".into();
                s
            }),
        ),
        (
            "single-element type lists",
            json!({
                "type": ["object"], "additionalProperties": false,
                "properties": {"city": {"type": ["string"]}}
            }),
            Some(object(vec![("city", t(Type::String))], &[])),
        ),
        (
            "value constraints and annotations dropped",
            json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "$id": "https://example.com/report",
                "$anchor": "report",
                "type": "object",
                "additionalProperties": false,
                "minProperties": 1,
                "maxProperties": 5,
                "properties": {
                    "code": {
                        "type": "string", "pattern": "^[A-Z]+$", "format": "email",
                        "minLength": 1, "maxLength": 9,
                        "default": "X", "examples": ["AB"],
                        "deprecated": true, "readOnly": true, "writeOnly": true, "$comment": "c"
                    },
                    "list": {
                        "type": "array",
                        "items": {"type": "number", "multipleOf": 0.5, "exclusiveMinimum": 0.0, "exclusiveMaximum": 9.0},
                        "minItems": 1, "maxItems": 3, "uniqueItems": true
                    }
                }
            }),
            Some(object(
                vec![("code", t(Type::String)), ("list", array(t(Type::Number)))],
                &[],
            )),
        ),
        // set_model_response rejects keys its schema does not list, so an object that accepts
        // extra keys cannot be sent.
        (
            "open root object",
            json!({"type": "object", "properties": {"city": {"type": "string"}}}),
            None,
        ),
        (
            "open nested object",
            closed_object(json!({
                "place": {"type": "object", "properties": {"city": {"type": "string"}}}
            })),
            None,
        ),
        (
            "properties with a schema for extra keys",
            json!({
                "type": "object",
                "properties": {"name": {"type": "string"}},
                "additionalProperties": {"type": "string"}
            }),
            None,
        ),
        (
            "required name with no property",
            json!({
                "type": "object", "additionalProperties": false, "required": ["city", "country"],
                "properties": {"city": {"type": "string"}}
            }),
            None,
        ),
        (
            // A pointer Output: set_model_response always answers with an object, so a null
            // answer would have no way through.
            "nullable root object",
            json!({
                "type": ["null", "object"], "additionalProperties": false,
                "properties": {"city": {"type": "string"}}
            }),
            None,
        ),
        (
            "integer enum",
            closed_object(json!({"level": {"type": "integer", "enum": [1, 2]}})),
            None,
        ),
        (
            "non-string value in a string enum",
            closed_object(json!({"level": {"type": "string", "enum": ["low", 2]}})),
            None,
        ),
        (
            "string enum on an integer",
            closed_object(json!({"level": {"type": "integer", "enum": ["1", "2"]}})),
            None,
        ),
        ("untyped property", closed_object(json!({"any": {}})), None),
        (
            "map property",
            closed_object(
                json!({"m": {"type": "object", "additionalProperties": {"type": "integer"}}}),
            ),
            None,
        ),
        (
            "anyOf property",
            closed_object(
                json!({"v": {"type": "string", "anyOf": [{"type": "string"}, {"type": "string", "minLength": 2}]}}),
            ),
            None,
        ),
        (
            "const property",
            closed_object(json!({"v": {"type": "string", "const": "x"}})),
            None,
        ),
        (
            "two non-null types",
            closed_object(json!({"v": {"type": ["string", "integer"]}})),
            None,
        ),
        (
            "array without items",
            closed_object(json!({"v": {"type": "array"}})),
            None,
        ),
        (
            "nested object without properties",
            closed_object(json!({"v": {"type": "object", "additionalProperties": false}})),
            None,
        ),
        (
            "root object without properties",
            json!({"type": "object", "additionalProperties": false}),
            None,
        ),
        ("root string", json!({"type": "string"}), None),
        (
            "root array of objects",
            json!({"type": "array", "items": closed_object(json!({"n": {"type": "integer"}}))}),
            None,
        ),
    ];
    for (name, schema, want) in cases {
        let got = model_output_schema(Some(&schema));
        assert_eq!(got, want, "{name}: modelOutputSchema mismatch");
    }
}

#[test]
fn test_model_output_schema_nil() {
    assert_eq!(model_output_schema(None), None);
}
