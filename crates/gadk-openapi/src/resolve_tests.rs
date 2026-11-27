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

//! Unit tests of `$ref` lookup, schema expansion and cleanup.

use serde_json::{Value, json};

use super::{MAX_EXPANDED_NODES, MAX_REF_DEPTH, lookup, object, schema};
use crate::error::Error;

fn doc() -> Value {
    json!({
        "components": {
            "schemas": {
                "Pet": {
                    "type": "object",
                    "properties": {
                        "name": {"type": "string", "example": "Rex"},
                        "example": {"type": "string", "xml": {"name": "ex"}},
                        "owner": {"$ref": "#/components/schemas/Owner"}
                    },
                    "discriminator": {"propertyName": "name"},
                    "externalDocs": {"url": "https://example.com"}
                },
                "Owner": {"type": "string", "nullable": true, "enum": ["a", null]},
                "a/b": {"type": "integer"},
                "a~b": {"type": "boolean"},
                "Node": {
                    "type": "object",
                    "properties": {"child": {"$ref": "#/components/schemas/Node"}}
                }
            },
            "parameters": {
                "Alias": {"$ref": "#/components/parameters/Limit"},
                "Limit": {"name": "limit", "in": "query", "schema": {"type": "integer"}}
            }
        }
    })
}

fn ref_error(result: Result<impl std::fmt::Debug, Error>) -> (String, String) {
    match result {
        Err(Error::Ref { reference, at }) => (reference, at),
        other => panic!("want Error::Ref, got {other:?}"),
    }
}

#[test]
fn lookup_follows_json_pointer_escapes() {
    let doc = doc();
    assert_eq!(
        lookup(&doc, "#/components/schemas/a~1b", "GET /x").unwrap(),
        &json!({"type": "integer"})
    );
    assert_eq!(
        lookup(&doc, "#/components/schemas/a~0b", "GET /x").unwrap(),
        &json!({"type": "boolean"})
    );
}

#[test]
fn remote_and_dangling_refs_are_errors() {
    let doc = doc();
    assert_eq!(
        ref_error(lookup(&doc, "other.yaml#/components/schemas/Pet", "GET /x")),
        (
            "other.yaml#/components/schemas/Pet".to_string(),
            "GET /x".to_string()
        )
    );
    assert_eq!(
        ref_error(lookup(&doc, "#/components/schemas/Missing", "POST /y")),
        (
            "#/components/schemas/Missing".to_string(),
            "POST /y".to_string()
        )
    );
    let err = schema(&doc, &json!({"$ref": "#/nowhere"}), "GET /x", false).unwrap_err();
    assert_eq!(
        err.to_string(),
        "cannot resolve $ref \"#/nowhere\" in GET /x"
    );
}

#[test]
fn a_parameter_ref_chain_is_followed() {
    let doc = doc();
    let alias = json!({"$ref": "#/components/parameters/Alias"});
    assert_eq!(
        object(&doc, &alias, "GET /x").unwrap(),
        &json!({"name": "limit", "in": "query", "schema": {"type": "integer"}})
    );
    let plain = json!({"name": "q", "in": "query"});
    assert_eq!(object(&doc, &plain, "GET /x").unwrap(), &plain);
}

#[test]
fn schemas_are_expanded_and_cleaned() {
    let doc = doc();
    let got = schema(
        &doc,
        &json!({"$ref": "#/components/schemas/Pet"}),
        "GET /x",
        true,
    )
    .unwrap();
    assert_eq!(
        got,
        json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                // A property named `example` is kept; its schema is cleaned.
                "example": {"type": "string"},
                "owner": {"type": ["string", "null"], "enum": ["a", null]}
            }
        })
    );
}

#[test]
fn nullable_is_rewritten_only_for_3_0() {
    let doc = doc();
    let owner = json!({"$ref": "#/components/schemas/Owner"});
    assert_eq!(
        schema(&doc, &owner, "GET /x", true).unwrap(),
        json!({"type": ["string", "null"], "enum": ["a", null]})
    );
    assert_eq!(
        schema(&doc, &owner, "GET /x", false).unwrap(),
        json!({"type": "string", "enum": ["a", null]})
    );
    // `nullable: false` and `nullable` without a string `type` only lose the key.
    assert_eq!(
        schema(
            &doc,
            &json!({"type": "integer", "nullable": false}),
            "GET /x",
            true
        )
        .unwrap(),
        json!({"type": "integer"})
    );
    assert_eq!(
        schema(
            &doc,
            &json!({"nullable": true, "items": {"example": 1}}),
            "GET /x",
            true
        )
        .unwrap(),
        json!({"items": {}})
    );
}

#[test]
fn keys_next_to_a_ref_apply_on_top() {
    let doc = doc();
    let got = schema(
        &doc,
        &json!({"$ref": "#/components/schemas/a~1b", "description": "A count.", "example": 3}),
        "GET /x",
        false,
    )
    .unwrap();
    assert_eq!(got, json!({"type": "integer", "description": "A count."}));
}

#[test]
fn a_recursive_schema_stops_with_an_empty_schema_at_its_first_repeat() {
    let doc = doc();
    let got = schema(
        &doc,
        &json!({"$ref": "#/components/schemas/Node"}),
        "GET /x",
        false,
    )
    .unwrap();
    assert_eq!(got, json!({"type": "object", "properties": {"child": {}}}));
}

/// A document whose `L1` … `L<count>` each hold `fanout` properties `p0` … that refer to the
/// next level; the last level is a string.
fn levels(count: usize, fanout: usize) -> Value {
    let mut schemas = serde_json::Map::new();
    for level in 1..=count {
        let value = if level == count {
            json!({"type": "string"})
        } else {
            let next = json!({"$ref": format!("#/components/schemas/L{}", level + 1)});
            let properties: serde_json::Map<String, Value> = (0..fanout)
                .map(|i| (format!("p{i}"), next.clone()))
                .collect();
            json!({"type": "object", "properties": properties})
        };
        schemas.insert(format!("L{level}"), value);
    }
    json!({"components": {"schemas": schemas}})
}

#[test]
fn an_acyclic_chain_stops_with_an_empty_schema_at_level_nine() {
    let doc = levels(10, 1);
    let got = schema(
        &doc,
        &json!({"$ref": "#/components/schemas/L1"}),
        "GET /x",
        false,
    )
    .unwrap();
    let mut node = &got;
    for level in 1..=MAX_REF_DEPTH {
        assert_eq!(node["type"], json!("object"), "level {level}");
        node = &node["properties"]["p0"];
    }
    // The ninth `$ref` becomes `{}`.
    assert_eq!(node, &json!({}));
}

/// The objects and arrays in `value`.
fn nodes(value: &Value) -> usize {
    match value {
        Value::Object(map) => 1 + map.values().map(nodes).sum::<usize>(),
        Value::Array(items) => 1 + items.iter().map(nodes).sum::<usize>(),
        _ => 0,
    }
}

#[test]
fn an_expansion_past_the_node_budget_cuts_later_refs() {
    // Fully expanded this is about 2 * 10^6 objects.
    let doc = levels(7, 10);
    let got = schema(
        &doc,
        &json!({"$ref": "#/components/schemas/L1"}),
        "GET /x",
        false,
    )
    .unwrap();
    let total = nodes(&got);
    assert!(
        (MAX_EXPANDED_NODES..2 * MAX_EXPANDED_NODES).contains(&total),
        "{total} nodes"
    );
    // The first branch is expanded to the leaf; the last property of the root is cut.
    let mut node = &got;
    for _ in 1..7 {
        node = &node["properties"]["p0"];
    }
    assert_eq!(node, &json!({"type": "string"}));
    assert_eq!(got["properties"]["p9"], json!({}));
}
