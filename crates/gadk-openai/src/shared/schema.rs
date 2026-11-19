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

//! JSON-schema helpers both APIs share.

use gadk_genai::Schema;
use gadk_platform::go_to_lower;
use serde_json::{Map, Value};

use crate::error::{ErrorKind, OpenAiError};
use crate::shared::calls::go_map;

/// The structured-output format name when the schema's title
/// is not one OpenAI accepts.
pub(crate) const DEFAULT_RESPONSE_FORMAT_NAME: &str = "adk_response";

/// The format name: the schema's title when it matches `^[a-zA-Z0-9_-]{1,64}$`, else
/// [`DEFAULT_RESPONSE_FORMAT_NAME`].
pub(crate) fn response_format_name(schema: Option<&Schema>) -> String {
    match schema {
        Some(schema) if is_response_format_name(&schema.title) => schema.title.clone(),
        _ => DEFAULT_RESPONSE_FORMAT_NAME.to_string(),
    }
}

/// The pattern of an allowed format name (`^[a-zA-Z0-9_-]{1,64}$`; `$` is the end of the
/// text, so a trailing newline does not match). Every allowed character is ASCII, so the
/// byte length is the character count.
fn is_response_format_name(title: &str) -> bool {
    (1..=64).contains(&title.len())
        && title
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// A JSON schema of any shape as a map, numbers kept as decoded. `None`
/// is the bare [`ErrorKind::EmptyJsonSchema`](crate::error::ErrorKind::EmptyJsonSchema);
/// JSON `null` is `Ok(None)` (no map, no error); a non-object is
/// `openai: unmarshal json schema: <err>`.
pub(crate) fn normalize_schema(
    schema: Option<&Value>,
) -> Result<Option<Map<String, Value>>, OpenAiError> {
    let Some(schema) = schema else {
        return Err(OpenAiError::sentinel(ErrorKind::EmptyJsonSchema));
    };
    // A `serde_json::Value` already holds the numbers it was built with, so every number is
    // kept as written.
    go_map(schema.clone())
        .map_err(|err| OpenAiError::plain(format!("openai: unmarshal json schema: {err}")))
}

/// Makes a schema strict: every object type gets `properties` (added empty when
/// missing), `additionalProperties: false` and a sorted `required` naming every property; a
/// `$ref` keeps no siblings; recurses through `$defs`, `properties`, `anyOf`/`oneOf`/`allOf`
/// and an object `items`. Mutates in place.
pub(crate) fn enforce_strict_openai_schema(schema: &mut Map<String, Value>) {
    if schema.contains_key("$ref") {
        schema.retain(|key, _| key == "$ref");
        return;
    }

    if schema.get("type").and_then(Value::as_str) == Some("object") {
        if !matches!(schema.get("properties"), Some(Value::Object(_))) {
            schema.insert("properties".to_string(), Value::Object(Map::new()));
        }
        let required: Vec<Value> = match schema.get("properties") {
            // `Map` iterates in key order, so the names are sorted.
            Some(Value::Object(props)) => props.keys().cloned().map(Value::String).collect(),
            _ => Vec::new(),
        };
        schema.insert("additionalProperties".to_string(), Value::Bool(false));
        schema.insert("required".to_string(), Value::Array(required));
    }

    if let Some(Value::Object(defs)) = schema.get_mut("$defs") {
        defs.values_mut().for_each(enforce_strict_value);
    }
    if let Some(Value::Object(props)) = schema.get_mut("properties") {
        props.values_mut().for_each(enforce_strict_value);
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(Value::Array(items)) = schema.get_mut(key) {
            items.iter_mut().for_each(enforce_strict_value);
        }
    }
    if let Some(items @ Value::Object(_)) = schema.get_mut("items") {
        enforce_strict_value(items);
    }
}

/// [`enforce_strict_openai_schema`] on any JSON value: only an object is
/// touched.
fn enforce_strict_value(value: &mut Value) {
    if let Value::Object(schema) = value {
        enforce_strict_openai_schema(schema);
    }
}

/// A genai schema as the JSON-schema map both APIs take, type names
/// lowercased at every depth; `None` is `Ok(None)`. Failures are `openai: marshal schema: …` /
/// `openai: unmarshal schema: …`.
pub(crate) fn schema_to_map(
    schema: Option<&Schema>,
) -> Result<Option<Map<String, Value>>, OpenAiError> {
    let Some(schema) = schema else {
        return Ok(None);
    };
    let mut value = serde_json::to_value(schema)
        .map_err(|err| OpenAiError::plain(format!("openai: marshal schema: {err}")))?;
    lowercase_schema_types(&mut value);
    go_map(value).map_err(|err| OpenAiError::plain(format!("openai: unmarshal schema: {err}")))
}

/// Lowercases every `type` string (or string in a `type` array),
/// at every depth.
fn lowercase_schema_types(value: &mut Value) {
    match value {
        Value::Object(map) => {
            lowercase_type_field(map);
            map.values_mut().for_each(lowercase_schema_types);
        }
        Value::Array(items) => items.iter_mut().for_each(lowercase_schema_types),
        _ => {}
    }
}

/// The `type` member of one map, lowercased.
fn lowercase_type_field(map: &mut Map<String, Value>) {
    match map.get_mut("type") {
        Some(Value::String(t)) => *t = go_to_lower(t),
        Some(Value::Array(types)) => {
            for t in types {
                if let Value::String(s) = t {
                    *s = go_to_lower(s);
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use gadk_genai::Type;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn object(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            other => panic!("not an object: {other}"),
        }
    }

    #[test]
    fn normalize_schema_keeps_numbers_and_reads_null_as_none() {
        assert_eq!(normalize_schema(Some(&Value::Null)).unwrap(), None);
        let schema = json!({"type": "integer", "minimum": 9_007_199_254_740_993_i64});
        let got = normalize_schema(Some(&schema)).unwrap().unwrap();
        assert_eq!(
            String::from_utf8(gadk_model::to_go_json(&got).unwrap()).unwrap(),
            r#"{"minimum":9007199254740993,"type":"integer"}"#
        );
        let err = normalize_schema(Some(&json!("object"))).unwrap_err();
        assert_eq!(
            err.to_string(),
            "openai: unmarshal json schema: expected a JSON object, found a string"
        );
    }

    #[test]
    fn strict_schema_closes_every_object() {
        let mut schema = object(json!({
            "type": "object",
            "properties": {
                "b": {"type": "string"},
                "a": {"type": "object"},
                "list": {"type": "array", "items": {"type": "object", "properties": {"x": {}}}},
                "pick": {"anyOf": [{"type": "object"}, {"$ref": "#/$defs/D", "description": "gone"}]}
            },
            "$defs": {"D": {"type": "object", "properties": {"z": {"type": "integer"}}}}
        }));
        enforce_strict_openai_schema(&mut schema);
        assert_eq!(
            Value::Object(schema),
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["a", "b", "list", "pick"],
                "properties": {
                    "b": {"type": "string"},
                    "a": {"type": "object", "properties": {}, "additionalProperties": false, "required": []},
                    "list": {"type": "array", "items": {
                        "type": "object", "properties": {"x": {}}, "additionalProperties": false, "required": ["x"]
                    }},
                    "pick": {"anyOf": [
                        {"type": "object", "properties": {}, "additionalProperties": false, "required": []},
                        {"$ref": "#/$defs/D"}
                    ]}
                },
                "$defs": {"D": {
                    "type": "object", "properties": {"z": {"type": "integer"}},
                    "additionalProperties": false, "required": ["z"]
                }}
            })
        );
    }

    #[test]
    fn schema_to_map_lowercases_types_at_every_depth() {
        let schema = Schema {
            r#type: Some(Type::Object),
            title: "Report".to_string(),
            properties: BTreeMap::from([(
                "tags".to_string(),
                Schema {
                    r#type: Some(Type::Array),
                    items: Some(Box::new(Schema {
                        r#type: Some(Type::String),
                        ..Schema::default()
                    })),
                    max_items: Some(3),
                    ..Schema::default()
                },
            )]),
            minimum: Some(1.0),
            ..Schema::default()
        };
        let got = schema_to_map(Some(&schema)).unwrap().unwrap();
        assert_eq!(
            String::from_utf8(gadk_model::to_go_json(&got).unwrap()).unwrap(),
            r#"{"minimum":1,"properties":{"tags":{"items":{"type":"string"},"maxItems":3,"type":"array"}},"title":"Report","type":"object"}"#
        );
        assert_eq!(schema_to_map(None).unwrap(), None);
    }
}
