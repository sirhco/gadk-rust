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

//! Port of Go internal/utils/schema_test.go.

use std::collections::BTreeMap;

use gadk_genai::{Schema, Type};
use serde_json::{Map, Value, json};

use crate::tool::utils::{match_type, validate_map_on_schema, validate_output_schema};

fn ty(t: Type) -> Schema {
    Schema {
        r#type: Some(t),
        ..Default::default()
    }
}

fn obj(v: Value) -> Map<String, Value> {
    v.as_object().cloned().expect("object")
}

#[test]
fn test_match_type() {
    let string_items = Schema {
        r#type: Some(Type::Array),
        items: Some(Box::new(ty(Type::String))),
        ..Default::default()
    };
    let foo_object = Schema {
        r#type: Some(Type::Object),
        properties: BTreeMap::from([("foo".to_string(), ty(Type::String))]),
        ..Default::default()
    };
    // (name, value, schema, want_match, want_err) — every Go case, in Go order; is_input is always true.
    let cases: Vec<(&str, Value, Option<Schema>, bool, bool)> = vec![
        ("nil schema", json!("test"), None, false, true),
        (
            "nil value",
            Value::Null,
            Some(ty(Type::String)),
            false,
            false,
        ),
        (
            "nil value nullable",
            Value::Null,
            Some(Schema {
                nullable: Some(true),
                ..ty(Type::String)
            }),
            true,
            false,
        ),
        (
            "string match",
            json!("test"),
            Some(ty(Type::String)),
            true,
            false,
        ),
        (
            "string mismatch",
            json!(123.0),
            Some(ty(Type::String)),
            false,
            false,
        ),
        (
            "integer match",
            json!(123.0),
            Some(ty(Type::Integer)),
            true,
            false,
        ),
        (
            "integer mismatch float",
            json!(123.45),
            Some(ty(Type::Integer)),
            false,
            false,
        ),
        (
            "integer mismatch type",
            json!("123"),
            Some(ty(Type::Integer)),
            false,
            false,
        ),
        (
            "number match",
            json!(123.45),
            Some(ty(Type::Number)),
            true,
            false,
        ),
        (
            "number mismatch",
            json!("123.45"),
            Some(ty(Type::Number)),
            false,
            false,
        ),
        (
            "boolean match",
            json!(true),
            Some(ty(Type::Boolean)),
            true,
            false,
        ),
        (
            "boolean mismatch",
            json!("true"),
            Some(ty(Type::Boolean)),
            false,
            false,
        ),
        (
            "array match",
            json!(["a", "b"]),
            Some(string_items.clone()),
            true,
            false,
        ),
        (
            "array mismatch type",
            json!("not an array"),
            Some(string_items.clone()),
            false,
            false,
        ),
        (
            "array mismatch item type",
            json!(["a", 1.0]),
            Some(string_items.clone()),
            false,
            false,
        ),
        (
            "array missing items",
            json!(["a", "b"]),
            Some(ty(Type::Array)),
            false,
            true,
        ),
        (
            "object match",
            json!({"foo": "bar"}),
            Some(foo_object.clone()),
            true,
            false,
        ),
        (
            "object mismatch type",
            json!("not an object"),
            Some(foo_object.clone()),
            false,
            false,
        ),
        (
            "object mismatch property type",
            json!({"foo": 123.0}),
            Some(foo_object.clone()),
            false,
            true,
        ),
        (
            "unsupported type",
            json!(123),
            Some(ty(Type::Unknown("UNSUPPORTED".into()))),
            false,
            true,
        ),
        (
            "lowercase type in schema",
            json!("test"),
            Some(ty(Type::Unknown("string".into()))),
            true,
            false,
        ),
    ];
    for (name, value, schema, want_match, want_err) in cases {
        let got = match_type(&value, schema.as_ref(), true);
        assert_eq!(
            got.is_err(),
            want_err,
            "{name}: matchType() error = {got:?}, wantErr {want_err}"
        );
        if let Ok(m) = got {
            assert_eq!(m, want_match, "{name}");
        }
    }
}

// parity: adapted: schema is &Schema (D5); Go's nil-schema case is unrepresentable and dropped
#[test]
fn test_validate_map_on_schema() {
    let schema = Schema {
        r#type: Some(Type::Object),
        properties: BTreeMap::from([
            ("str_field".to_string(), ty(Type::String)),
            ("int_field".to_string(), ty(Type::Integer)),
        ]),
        required: vec!["str_field".to_string()],
        ..Default::default()
    };
    let schema_nil_props = ty(Type::Object);
    let cases: Vec<(&str, Value, &Schema, bool)> = vec![
        (
            "valid map",
            json!({"str_field": "hello", "int_field": 123.0}),
            &schema,
            false,
        ),
        (
            "valid map with only required fields",
            json!({"str_field": "hello"}),
            &schema,
            false,
        ),
        (
            "missing required field",
            json!({"int_field": 123.0}),
            &schema,
            true,
        ),
        (
            "extra field",
            json!({"str_field": "hello", "extra": "field"}),
            &schema,
            true,
        ),
        ("type mismatch", json!({"str_field": 123.0}), &schema, true),
        (
            "nil properties and no args",
            json!({}),
            &schema_nil_props,
            false,
        ),
        (
            "nil properties and some args",
            json!({"some": "arg"}),
            &schema_nil_props,
            true,
        ),
    ];
    for (name, args, schema, want_err) in cases {
        let got = validate_map_on_schema(&obj(args), schema, true);
        assert_eq!(
            got.is_err(),
            want_err,
            "{name}: ValidateMapOnSchema() error = {got:?}, wantErr {want_err}"
        );
    }
}

// parity: adapted: schema is &Schema (D5); Go's nil-schema case is unrepresentable and dropped
#[test]
fn test_validate_output_schema() {
    let schema = Schema {
        r#type: Some(Type::Object),
        properties: BTreeMap::from([("result".to_string(), ty(Type::String))]),
        required: vec!["result".to_string()],
        ..Default::default()
    };
    assert_eq!(
        validate_output_schema(r#"{"result": "success"}"#, &schema),
        Ok(obj(json!({"result": "success"}))),
        "valid output"
    );
    assert!(
        validate_output_schema(r#"{"result": "success""#, &schema).is_err(),
        "invalid json"
    );
    assert!(
        validate_output_schema(r#"{"wrong_key": "failure"}"#, &schema).is_err(),
        "schema mismatch"
    );
}
