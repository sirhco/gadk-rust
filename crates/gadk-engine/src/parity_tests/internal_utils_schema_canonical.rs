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

//! Port of Go internal/utils/schema_canonical_test.go.

use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};

use crate::tool::utils::{canonical_schema_json, canonicalize};

/// Builds `{"type":"object","properties":{…}}` inserting properties in `order`.
fn object_schema(order: &[&str]) -> Value {
    let mut props = Map::new();
    for name in order {
        let ty = if *name == "foo" { "string" } else { "integer" };
        props.insert((*name).to_string(), json!({ "type": ty }));
    }
    json!({ "type": "object", "properties": Value::Object(props) })
}

// parity: adapted: schemas are serde_json Values; jsonschema-go's non-serialized PropertyOrder field becomes property insertion order.
#[test]
fn test_canonical_schema_json_property_order() {
    let out1 = canonical_schema_json(&object_schema(&["foo", "bar"]));
    let out2 = canonical_schema_json(&object_schema(&["bar", "foo"]));
    let out3 = canonical_schema_json(
        &json!({"type": "object", "properties": {"foo": {"type": "string"}, "bar": {"type": "integer"}}}),
    );
    assert_eq!(out1, out2);
    assert_eq!(out1, out3);
}

#[test]
fn test_canonical_schema_json_required_order() {
    let mk = |required: &[&str]| {
        json!({
            "type": "object",
            "properties": {"foo": {"type": "string"}, "bar": {"type": "integer"}},
            "required": required,
        })
    };
    assert_eq!(
        canonical_schema_json(&mk(&["foo", "bar"])),
        canonical_schema_json(&mk(&["bar", "foo"]))
    );
}

#[test]
fn test_canonical_schema_json_type_union_order() {
    let out1 = canonical_schema_json(&json!({"type": ["string", "null"]}));
    let out2 = canonical_schema_json(&json!({"type": ["null", "string"]}));
    assert_eq!(out1, out2);
}

#[test]
fn test_canonicalize_deeply_nested() {
    let input = json!({"z": {"y": {"x": "val", "a": 123.0}, "b": true}, "a": "top"});
    assert_eq!(
        canonicalize(&input),
        r#"{"a":"top","z":{"b":true,"y":{"a":123,"x":"val"}}}"#
    );
}

#[test]
fn test_canonicalize_arrays_preserved() {
    let input = json!([{"b": 2, "a": 1}, [4, 3, 5], "string"]);
    assert_eq!(canonicalize(&input), r#"[{"a":1,"b":2},[4,3,5],"string"]"#);
}

#[test]
fn test_canonicalize_primitives() {
    let cases = [
        ("string", json!("hello world"), "\"hello world\""),
        ("number", json!(42.5), "42.5"),
        ("bool", json!(true), "true"),
        ("nil", Value::Null, "null"),
    ];
    for (name, input, want) in cases {
        assert_eq!(canonicalize(&input), want, "{name}");
    }
}
