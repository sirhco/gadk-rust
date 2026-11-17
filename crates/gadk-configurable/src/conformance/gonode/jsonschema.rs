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

//! The reshaping of a JSON-schema `serde_json::Value` (what gadk holds where the recorded
//! format holds a JSON-schema pointer, jsonschema-go v0.4.3) into the yaml.v3 reflection of
//! jsonschema-go's `Schema` struct: field keys (`$ref` → `ref`, `$defs` → `defs`, …),
//! declaration field order, emitted zero fields, sorted `properties`, `default` as a byte
//! sequence, and the boolean schema `false` as `{not: {}}`. Nothing outside `gonode` uses its
//! items directly.
//!
//! [`reshape`] performs jsonschema-go's `(*Schema).UnmarshalJSON` field assignment: it returns
//! an object keyed by the `Schema` fields' YAML keys, which the `super::fixups::Builder` then
//! encodes with the `jsonschema.Schema` shape (declaration order, zero-fill); nested schemas
//! stay JSON and are reshaped when the builder reaches them.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Map, Value};

use super::fixups::JSONSCHEMA_FIELDS;

/// The `Schema` field values of JSON schema `v`, keyed by the fields' YAML keys.
///
/// - `true` is the empty `Schema{}`; `false` is `{Not: &Schema{}}`.
/// - `type` goes to `Type` (a string) or `Types` (an array); `items` to `Items` (a schema) or
///   `ItemsArray` (an array); each `dependencies` entry to `DependencyStrings` (an array) or
///   `DependencySchemas`.
/// - `default` (raw JSON) holds the value's compact JSON bytes, as the base64 string gadk uses
///   for a byte slice (the builder's `Bytes` class turns it into the `!!int` sequence). The
///   recorded format keeps the source text, so the bytes are equal only for a compact,
///   sorted-key, HTML-escaped source, as the fixture's are.
/// - Every other keyword of a field is copied under that field's key; keywords of no field go
///   to `Extra`.
pub fn reshape(v: &Value) -> Map<String, Value> {
    let mut out = Map::new();
    let obj = match v {
        Value::Bool(true) => return out,
        Value::Bool(false) => {
            out.insert("not".to_owned(), Value::Object(Map::new()));
            return out;
        }
        Value::Object(obj) => obj,
        _ => return out,
    };
    let mut extra = Map::new();
    for (k, val) in obj {
        match k.as_str() {
            "type" => match val {
                Value::String(_) => {
                    out.insert("type".to_owned(), val.clone());
                }
                Value::Array(_) => {
                    out.insert("types".to_owned(), val.clone());
                }
                _ => {}
            },
            "items" => {
                let key = if val.is_array() {
                    "itemsarray"
                } else {
                    "items"
                };
                out.insert(key.to_owned(), val.clone());
            }
            "dependencies" => {
                let Value::Object(deps) = val else { continue };
                let mut strings = Map::new();
                let mut schemas = Map::new();
                for (dk, dv) in deps {
                    if dv.is_array() {
                        strings.insert(dk.clone(), dv.clone());
                    } else {
                        schemas.insert(dk.clone(), dv.clone());
                    }
                }
                if !strings.is_empty() {
                    out.insert("dependencystrings".to_owned(), Value::Object(strings));
                }
                if !schemas.is_empty() {
                    out.insert("dependencyschemas".to_owned(), Value::Object(schemas));
                }
            }
            "default" => {
                let raw = gadk_model::to_go_json(val).unwrap_or_default();
                out.insert("default".to_owned(), Value::String(STANDARD.encode(raw)));
            }
            _ => match JSONSCHEMA_FIELDS
                .iter()
                .find(|f| f.json_tag != "-" && f.json_keyword == k.as_str())
            {
                Some(f) => {
                    out.insert(f.yaml_key.to_owned(), val.clone());
                }
                None => {
                    extra.insert(k.clone(), val.clone());
                }
            },
        }
    }
    if !extra.is_empty() {
        out.insert("extra".to_owned(), Value::Object(extra));
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn boolean_schemas_are_jsonschema_go_s() {
        assert!(reshape(&json!(true)).is_empty());
        assert_eq!(Value::Object(reshape(&json!(false))), json!({"not": {}}));
    }

    #[test]
    fn keywords_map_to_go_fields() {
        let got = reshape(&json!({
            "$schema": "s", "$ref": "#/$defs/A", "$defs": {"A": true},
            "type": ["null", "integer"], "items": [{"type": "string"}],
            "dependencies": {"a": ["b"], "c": {"type": "string"}},
            "additionalProperties": false, "default": {"z": 1, "a": "<"},
            "x-custom": 1
        }));
        assert_eq!(
            Value::Object(got),
            json!({
                "schema": "s", "ref": "#/$defs/A", "defs": {"A": true},
                "types": ["null", "integer"], "itemsarray": [{"type": "string"}],
                "dependencystrings": {"a": ["b"]}, "dependencyschemas": {"c": {"type": "string"}},
                "additionalproperties": false,
                "default": STANDARD.encode(br#"{"a":"\u003c","z":1}"#),
                "extra": {"x-custom": 1}
            })
        );
        let single = reshape(&json!({"type": "object", "items": {"type": "string"}}));
        assert_eq!(
            Value::Object(single),
            json!({"type": "object", "items": {"type": "string"}})
        );
    }
}
