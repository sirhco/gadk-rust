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

//! Thinking, effort and structured output (port of ADK-Go `thinking.go`).

use gadk_genai::{GenerateContentConfig, ThinkingConfig};
use gadk_platform::go_to_lower;
use serde_json::{Map, Value};

use crate::config::Effort;
use crate::error::{AnthropicError, ErrorKind, go_quote};
use crate::tools::lowercase_schema_types;
use crate::wire::{JsonOutputFormat, JsonSchemaTag, OutputConfig, ThinkingConfigParam};

/// The property a non-object output schema is wrapped in (Go `wrappedOutputSchemaKey`).
pub(crate) const WRAPPED_OUTPUT_SCHEMA_KEY: &str = "result";

/// A normalized output schema (Go `buildOutputFormatSchema`'s `map[string]any, bool`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OutputSchema {
    /// The schema sent as `output_config.format.schema`.
    pub(crate) schema: Map<String, Value>,
    /// Whether the schema was wrapped in `{"result": …}` (a non-object root), so text
    /// responses must be unwrapped.
    pub(crate) unwrap: bool,
}

/// Go `fmt.Errorf("%w: <detail>", ErrInvalidConfig)`.
fn invalid_config(detail: impl std::fmt::Display) -> AnthropicError {
    AnthropicError::new(ErrorKind::InvalidConfig, detail)
}

/// Go `validateThinkingConfig`: rejects `thinking_level` and a missing `thinking_budget`.
/// `include_thoughts` is ignored by design (thought parts are always returned when thinking is
/// active).
pub(crate) fn validate_thinking_config(tc: Option<&ThinkingConfig>) -> Result<(), AnthropicError> {
    let Some(tc) = tc else {
        return Ok(());
    };
    if tc
        .thinking_level
        .as_ref()
        .is_some_and(|level| !level.as_str().is_empty())
    {
        return Err(invalid_config(
            "ThinkingConfig.ThinkingLevel is not supported on Claude; configure ClientConfig.Effort instead",
        ));
    }
    if tc.thinking_budget.is_none() {
        return Err(invalid_config(
            "ThinkingConfig.ThinkingBudget must be set explicitly when ThinkingConfig is provided (0 to disable, >0 for manual budget, <0 for adaptive thinking)",
        ));
    }
    Ok(())
}

/// Go `isThinkingEnabled`: a budget is set and non-zero.
pub(crate) fn is_thinking_enabled(tc: Option<&ThinkingConfig>) -> bool {
    tc.and_then(|tc| tc.thinking_budget)
        .is_some_and(|budget| budget != 0)
}

/// Go `buildThinkingParam`: budget 0 → disabled, < 0 → adaptive (summarized), > 0 → enabled
/// with that budget; `None` without a budget.
pub(crate) fn build_thinking_param(
    cfg: Option<&GenerateContentConfig>,
) -> Option<ThinkingConfigParam> {
    let budget = cfg?.thinking_config.as_ref()?.thinking_budget?;
    Some(match budget {
        0 => ThinkingConfigParam::Disabled,
        b if b < 0 => ThinkingConfigParam::AdaptiveSummarized,
        b => ThinkingConfigParam::Enabled {
            budget_tokens: i64::from(b),
        },
    })
}

/// Go `validateOutputFormatConfig`.
pub(crate) fn validate_output_format_config(
    cfg: Option<&GenerateContentConfig>,
) -> Result<(), AnthropicError> {
    let Some(cfg) = cfg else {
        return Ok(());
    };
    let has_schema = cfg.response_schema.is_some() || cfg.response_json_schema.is_some();
    if cfg.response_schema.is_some() && cfg.response_json_schema.is_some() {
        return Err(invalid_config(
            "ResponseSchema and ResponseJsonSchema cannot both be set",
        ));
    }
    let mime = go_to_lower(cfg.response_mime_type.trim());
    if !mime.is_empty() && mime != "text/plain" && mime != "application/json" {
        return Err(invalid_config(format!(
            "ResponseMIMEType {} is not supported",
            go_quote(&cfg.response_mime_type)
        )));
    }
    if mime == "text/plain" && has_schema {
        return Err(invalid_config(format!(
            "ResponseMIMEType {} cannot be combined with ResponseSchema or ResponseJsonSchema",
            go_quote(&cfg.response_mime_type)
        )));
    }
    if mime == "application/json" && !has_schema {
        return Err(invalid_config(
            "ResponseMIMEType \"application/json\" requires ResponseSchema or ResponseJsonSchema",
        ));
    }
    if has_schema {
        build_output_format_schema(Some(cfg))?;
    }
    Ok(())
}

/// Go `buildOutputFormatSchema`: `None` without a response schema (`response_json_schema`
/// wins over `response_schema`); errors name the field Go names.
pub(crate) fn build_output_format_schema(
    cfg: Option<&GenerateContentConfig>,
) -> Result<Option<OutputSchema>, AnthropicError> {
    let Some(cfg) = cfg else {
        return Ok(None);
    };
    let (marshalled, field_name) = if let Some(raw) = cfg.response_json_schema.as_ref() {
        (serde_json::to_value(raw), "ResponseJsonSchema")
    } else if let Some(schema) = cfg.response_schema.as_ref() {
        (serde_json::to_value(schema), "ResponseSchema")
    } else {
        return Ok(None);
    };

    let value = marshalled
        .map_err(|err| invalid_config(format!("failed to marshal {field_name}: {err}")))?;
    // Go unmarshals into a map[string]any and rejects a failure or an empty map.
    let mut schema = match value {
        Value::Object(map) if !map.is_empty() => map,
        _ => {
            return Err(invalid_config(format!(
                "{field_name} must be a non-empty JSON schema"
            )));
        }
    };

    lowercase_schema_map(&mut schema);
    if !schema.contains_key("type") && schema.contains_key("properties") {
        schema.insert("type".into(), Value::String("object".into()));
    }
    normalize_output_schema_map(&mut schema);
    if schema.get("type").and_then(Value::as_str) == Some("object") {
        return Ok(Some(OutputSchema {
            schema,
            unwrap: false,
        }));
    }

    rewrite_wrapped_schema_map(&mut schema);
    let mut wrapped = Map::new();
    wrapped.insert("type".into(), Value::String("object".into()));
    wrapped.insert(
        "required".into(),
        Value::from(vec![WRAPPED_OUTPUT_SCHEMA_KEY]),
    );
    wrapped.insert("additionalProperties".into(), Value::Bool(false));
    // $defs and definitions are hoisted to the wrapper root.
    for defs_key in ["$defs", "definitions"] {
        if let Some(defs) = schema.remove(defs_key) {
            wrapped.insert(defs_key.into(), defs);
        }
    }
    let mut properties = Map::new();
    properties.insert(WRAPPED_OUTPUT_SCHEMA_KEY.into(), Value::Object(schema));
    wrapped.insert("properties".into(), Value::Object(properties));
    Ok(Some(OutputSchema {
        schema: wrapped,
        unwrap: true,
    }))
}

/// Go `rewriteWrappedSchemaRefs`: local `$ref`s point into `#/properties/result`, except
/// `$defs`/`definitions`, which are hoisted to the wrapper.
pub(crate) fn rewrite_wrapped_schema_refs(node: &mut Value) {
    match node {
        Value::Object(map) => rewrite_wrapped_schema_map(map),
        Value::Array(items) => items.iter_mut().for_each(rewrite_wrapped_schema_refs),
        _ => {}
    }
}

/// [`rewrite_wrapped_schema_refs`] on an object node.
fn rewrite_wrapped_schema_map(map: &mut Map<String, Value>) {
    if let Some(Value::String(r)) = map.get_mut("$ref") {
        if r == "#" {
            *r = format!("#/properties/{WRAPPED_OUTPUT_SCHEMA_KEY}");
        } else if r == "#/$defs"
            || r.starts_with("#/$defs/")
            || r == "#/definitions"
            || r.starts_with("#/definitions/")
        {
            // $defs and definitions are hoisted to the wrapper root.
        } else if r.starts_with("#/") {
            *r = format!("#/properties/{WRAPPED_OUTPUT_SCHEMA_KEY}{}", &r[1..]);
        }
    }
    map.values_mut().for_each(rewrite_wrapped_schema_refs);
}

/// Go `normalizeOutputSchemaObjects`: every `type: object` gets `properties: {}` and
/// `additionalProperties: false` when missing.
pub(crate) fn normalize_output_schema_objects(node: &mut Value) {
    match node {
        Value::Object(map) => normalize_output_schema_map(map),
        Value::Array(items) => items.iter_mut().for_each(normalize_output_schema_objects),
        _ => {}
    }
}

/// [`normalize_output_schema_objects`] on an object node.
fn normalize_output_schema_map(map: &mut Map<String, Value>) {
    if map.get("type").and_then(Value::as_str) == Some("object") {
        map.entry("properties")
            .or_insert_with(|| Value::Object(Map::new()));
        map.entry("additionalProperties")
            .or_insert(Value::Bool(false));
    }
    map.values_mut().for_each(normalize_output_schema_objects);
}

/// [`lowercase_schema_types`] on an object node (the root of an output schema).
fn lowercase_schema_map(map: &mut Map<String, Value>) {
    let mut node = Value::Object(std::mem::take(map));
    lowercase_schema_types(&mut node);
    if let Value::Object(lowered) = node {
        *map = lowered;
    }
}

/// Go `buildOutputConfigParam`: `output_config` (`None` when neither effort nor a schema is
/// set) and whether text responses must be unwrapped.
pub(crate) fn build_output_config_param(
    effort: Option<Effort>,
    cfg: Option<&GenerateContentConfig>,
) -> Result<(Option<OutputConfig>, bool), AnthropicError> {
    let (format, unwrap) = match build_output_format_schema(cfg)? {
        Some(OutputSchema { schema, unwrap }) => (
            Some(JsonOutputFormat {
                schema,
                kind: JsonSchemaTag,
            }),
            unwrap,
        ),
        None => (None, false),
    };
    if effort.is_none() && format.is_none() {
        return Ok((None, unwrap));
    }
    Ok((Some(OutputConfig { effort, format }), unwrap))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;
    use crate::wire::to_go_json;
    use gadk_genai::{Schema, ThinkingLevel, Type};
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn json_of<T: serde::Serialize>(v: &T) -> String {
        String::from_utf8(to_go_json(v).unwrap()).unwrap()
    }

    fn budget(b: Option<i32>) -> ThinkingConfig {
        ThinkingConfig {
            thinking_budget: b,
            ..Default::default()
        }
    }

    fn with_thinking(tc: ThinkingConfig) -> GenerateContentConfig {
        GenerateContentConfig {
            thinking_config: Some(tc),
            ..Default::default()
        }
    }

    fn json_schema_cfg(mime: &str, schema: Value) -> GenerateContentConfig {
        GenerateContentConfig {
            response_mime_type: mime.into(),
            response_json_schema: Some(schema),
            ..Default::default()
        }
    }

    /// Go TestOutputSchemaAndStructuredOutput's `output_schema_processor_request_shape` schema.
    fn city_schema() -> Schema {
        let mut props = BTreeMap::new();
        props.insert(
            "city".to_string(),
            Schema {
                r#type: Some(Type::String),
                ..Default::default()
            },
        );
        props.insert(
            "temp_c".to_string(),
            Schema {
                r#type: Some(Type::Integer),
                ..Default::default()
            },
        );
        Schema {
            r#type: Some(Type::Object),
            properties: props,
            required: vec!["city".into(), "temp_c".into()],
            ..Default::default()
        }
    }

    // ------------------------------------------------------------------ thinking

    #[test]
    fn validate_thinking_config_cases() {
        // Go TestExtendedThinkingAndEffort/thinking_config_table, cases 5 and 6.
        assert!(validate_thinking_config(None).is_ok());
        for b in [0, 512, -1] {
            assert!(
                validate_thinking_config(Some(&budget(Some(b)))).is_ok(),
                "{b}"
            );
        }
        let include_only = ThinkingConfig {
            include_thoughts: true,
            ..Default::default()
        };
        let err = validate_thinking_config(Some(&include_only)).unwrap_err();
        assert_eq!(
            err.to_string(),
            "anthropicmodel: invalid configuration: ThinkingConfig.ThinkingBudget must be set explicitly when ThinkingConfig is provided (0 to disable, >0 for manual budget, <0 for adaptive thinking)"
        );
        assert_eq!(err.kinds(), &[ErrorKind::InvalidConfig]);

        let level = ThinkingConfig {
            thinking_budget: Some(512),
            thinking_level: Some(ThinkingLevel::High),
            ..Default::default()
        };
        let err = validate_thinking_config(Some(&level)).unwrap_err();
        assert_eq!(
            err.to_string(),
            "anthropicmodel: invalid configuration: ThinkingConfig.ThinkingLevel is not supported on Claude; configure ClientConfig.Effort instead"
        );
        // ThinkingLevel is checked first, as in Go.
        let level_no_budget = ThinkingConfig {
            thinking_level: Some(ThinkingLevel::Unspecified),
            ..Default::default()
        };
        assert!(
            validate_thinking_config(Some(&level_no_budget))
                .unwrap_err()
                .to_string()
                .contains("ThinkingLevel")
        );
        // An empty level is Go's unset "".
        let empty_level = ThinkingConfig {
            thinking_budget: Some(1),
            thinking_level: Some(ThinkingLevel::Unknown(String::new())),
            ..Default::default()
        };
        assert!(validate_thinking_config(Some(&empty_level)).is_ok());
    }

    #[test]
    fn is_thinking_enabled_cases() {
        assert!(!is_thinking_enabled(None));
        assert!(!is_thinking_enabled(Some(&budget(None))));
        assert!(!is_thinking_enabled(Some(&budget(Some(0)))));
        assert!(is_thinking_enabled(Some(&budget(Some(512)))));
        assert!(is_thinking_enabled(Some(&budget(Some(-1)))));
    }

    #[test]
    fn build_thinking_param_table() {
        // Go TestExtendedThinkingAndEffort/thinking_config_table, cases 1-4 (512 is not
        // clamped to the API minimum).
        assert_eq!(build_thinking_param(None), None);
        assert_eq!(
            build_thinking_param(Some(&GenerateContentConfig::default())),
            None
        );
        assert_eq!(
            build_thinking_param(Some(&with_thinking(budget(None)))),
            None
        );
        let cases = [
            (0, r#"{"type":"disabled"}"#),
            (512, r#"{"budget_tokens":512,"type":"enabled"}"#),
            (-1, r#"{"display":"summarized","type":"adaptive"}"#),
        ];
        for (b, want) in cases {
            let param = build_thinking_param(Some(&with_thinking(budget(Some(b))))).unwrap();
            assert_eq!(json_of(&param), want, "{b}");
        }
    }

    // ---------------------------------------------------------- output format: validate

    #[test]
    fn validate_output_format_config_rejections() {
        // Go TestGenerateContentConfigValidation (ResponseMIMEType / ResponseSchema cases) and
        // TestOutputSchemaAndStructuredOutput/empty_response_json_schema_rejected.
        let cases: Vec<(&str, GenerateContentConfig, &str)> = vec![
            (
                "response_mime_type_without_schema",
                GenerateContentConfig {
                    response_mime_type: "application/json".into(),
                    ..Default::default()
                },
                "ResponseMIMEType \"application/json\" requires ResponseSchema or ResponseJsonSchema",
            ),
            (
                "response_mime_type_non_json_with_schema",
                GenerateContentConfig {
                    response_mime_type: "text/plain".into(),
                    response_schema: Some(Schema {
                        r#type: Some(Type::Object),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                "ResponseMIMEType \"text/plain\" cannot be combined with ResponseSchema or ResponseJsonSchema",
            ),
            (
                "response_schema_and_response_json_schema_both_set",
                GenerateContentConfig {
                    response_schema: Some(Schema {
                        r#type: Some(Type::Object),
                        ..Default::default()
                    }),
                    response_json_schema: Some(json!({"type": "object"})),
                    ..Default::default()
                },
                "ResponseSchema and ResponseJsonSchema cannot both be set",
            ),
            (
                "unsupported_mime_type_quoted_as_given",
                GenerateContentConfig {
                    response_mime_type: " Text/CSV".into(),
                    ..Default::default()
                },
                "ResponseMIMEType \" Text/CSV\" is not supported",
            ),
            (
                "empty_response_json_schema_rejected",
                json_schema_cfg("", json!({})),
                "ResponseJsonSchema must be a non-empty JSON schema",
            ),
            (
                "non_object_response_json_schema",
                json_schema_cfg("", json!("string")),
                "ResponseJsonSchema must be a non-empty JSON schema",
            ),
            (
                "empty_response_schema",
                GenerateContentConfig {
                    response_schema: Some(Schema::default()),
                    ..Default::default()
                },
                "ResponseSchema must be a non-empty JSON schema",
            ),
        ];
        for (name, cfg, want) in cases {
            let err = validate_output_format_config(Some(&cfg)).expect_err(name);
            assert_eq!(
                err.to_string(),
                format!("anthropicmodel: invalid configuration: {want}"),
                "{name}"
            );
            assert_eq!(err.kinds(), &[ErrorKind::InvalidConfig], "{name}");
        }
    }

    #[test]
    fn validate_output_format_config_accepts() {
        let ok: Vec<(&str, Option<GenerateContentConfig>)> = vec![
            ("nil", None),
            ("default", Some(GenerateContentConfig::default())),
            (
                "text_plain_alone",
                Some(GenerateContentConfig {
                    response_mime_type: "text/plain".into(),
                    ..Default::default()
                }),
            ),
            (
                "json_mime_case_and_space_insensitive",
                Some(json_schema_cfg(
                    " APPLICATION/JSON ",
                    json!({"type": "string"}),
                )),
            ),
            (
                // Go strings.ToLower maps U+0130 to a single `i` (Rust's to_lowercase does not).
                "text_plain_dotted_capital_i",
                Some(GenerateContentConfig {
                    response_mime_type: "TEXT/PLA\u{130}N".into(),
                    ..Default::default()
                }),
            ),
            (
                "schema_without_mime",
                Some(GenerateContentConfig {
                    response_schema: Some(city_schema()),
                    ..Default::default()
                }),
            ),
        ];
        for (name, cfg) in ok {
            assert!(
                validate_output_format_config(cfg.as_ref()).is_ok(),
                "{name}"
            );
        }
    }

    // ---------------------------------------------------------- output format: build

    #[test]
    fn object_root_schema_is_normalized_not_wrapped() {
        // Go TestOutputSchemaAndStructuredOutput/output_schema_processor_request_shape.
        let cfg = GenerateContentConfig {
            response_mime_type: "application/json".into(),
            response_schema: Some(city_schema()),
            ..Default::default()
        };
        let out = build_output_format_schema(Some(&cfg)).unwrap().unwrap();
        assert!(!out.unwrap);
        assert_eq!(
            json_of(&out.schema),
            r#"{"additionalProperties":false,"properties":{"city":{"type":"string"},"temp_c":{"type":"integer"}},"required":["city","temp_c"],"type":"object"}"#
        );
    }

    #[test]
    fn nested_objects_are_normalized() {
        // Go TestOutputSchemaAndStructuredOutput/response_json_schema_with_nested_objects_and_effort.
        let cfg = json_schema_cfg(
            "",
            json!({
                "type": "OBJECT",
                "properties": {
                    "items": {
                        "type": "ARRAY",
                        "items": {
                            "type": "OBJECT",
                            "properties": {"name": {"type": "STRING"}}
                        }
                    }
                }
            }),
        );
        let out = build_output_format_schema(Some(&cfg)).unwrap().unwrap();
        assert!(!out.unwrap);
        assert_eq!(
            Value::Object(out.schema),
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "items": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {"name": {"type": "string"}}
                        }
                    }
                }
            })
        );
    }

    #[test]
    fn properties_without_type_become_an_object() {
        let cfg = json_schema_cfg("", json!({"properties": {"a": {"type": "object"}}}));
        let out = build_output_format_schema(Some(&cfg)).unwrap().unwrap();
        assert!(!out.unwrap);
        assert_eq!(
            Value::Object(out.schema),
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "a": {"type": "object", "properties": {}, "additionalProperties": false}
                }
            })
        );
    }

    #[test]
    fn existing_additional_properties_are_kept() {
        let cfg = json_schema_cfg(
            "",
            json!({"type": "object", "properties": {}, "additionalProperties": true}),
        );
        let out = build_output_format_schema(Some(&cfg)).unwrap().unwrap();
        assert_eq!(out.schema["additionalProperties"], json!(true));
    }

    #[test]
    fn response_json_schema_wins_over_response_schema() {
        let cfg = GenerateContentConfig {
            response_schema: Some(city_schema()),
            response_json_schema: Some(json!({"type": "string"})),
            ..Default::default()
        };
        let out = build_output_format_schema(Some(&cfg)).unwrap().unwrap();
        assert!(out.unwrap);
    }

    #[test]
    fn non_object_root_schema_is_wrapped() {
        // Go TestOutputSchemaAndStructuredOutput/non_object_root_schema_wrapped_and_unwrapped.
        let cfg = GenerateContentConfig {
            response_mime_type: "application/json".into(),
            response_schema: Some(Schema {
                r#type: Some(Type::Array),
                items: Some(Box::new(Schema {
                    r#type: Some(Type::String),
                    ..Default::default()
                })),
                ..Default::default()
            }),
            ..Default::default()
        };
        let out = build_output_format_schema(Some(&cfg)).unwrap().unwrap();
        assert!(out.unwrap);
        assert_eq!(
            json_of(&out.schema),
            r#"{"additionalProperties":false,"properties":{"result":{"items":{"type":"string"},"type":"array"}},"required":["result"],"type":"object"}"#
        );
    }

    #[test]
    fn wrapped_schema_hoists_defs_and_rewrites_refs() {
        // Go TestOutputSchemaAndStructuredOutput/non_object_root_schema_wrapped_and_unwrapped
        // ($defs / definitions / $ref part).
        let cfg = json_schema_cfg(
            "application/json",
            json!({
                "type": "array",
                "items": {
                    "anyOf": [
                        {"type": "string"},
                        {"$ref": "#/$defs/CityAlias"},
                        {"$ref": "#/definitions/LegacyCity"},
                        {"$ref": "#"}
                    ]
                },
                "additionalItems": {"$ref": "#/items"},
                "$defs": {"CityAlias": {"type": "string"}},
                "definitions": {"LegacyCity": {"type": "string"}}
            }),
        );
        let out = build_output_format_schema(Some(&cfg)).unwrap().unwrap();
        assert!(out.unwrap);
        assert_eq!(
            Value::Object(out.schema),
            json!({
                "type": "object",
                "required": ["result"],
                "additionalProperties": false,
                "$defs": {"CityAlias": {"type": "string"}},
                "definitions": {"LegacyCity": {"type": "string"}},
                "properties": {
                    "result": {
                        "type": "array",
                        "items": {
                            "anyOf": [
                                {"type": "string"},
                                {"$ref": "#/$defs/CityAlias"},
                                {"$ref": "#/definitions/LegacyCity"},
                                {"$ref": "#/properties/result"}
                            ]
                        },
                        "additionalItems": {"$ref": "#/properties/result/items"}
                    }
                }
            })
        );
    }

    #[test]
    fn rewrite_wrapped_schema_refs_cases() {
        let mut v = json!([
            {"$ref": "#"},
            {"$ref": "#/$defs"},
            {"$ref": "#/definitions"},
            {"$ref": "#/$defsX"},
            {"$ref": "#/a/b"},
            {"$ref": "other.json#/a"},
            {"$ref": 3},
            {"nested": {"$ref": "#/x"}}
        ]);
        rewrite_wrapped_schema_refs(&mut v);
        assert_eq!(
            v,
            json!([
                {"$ref": "#/properties/result"},
                {"$ref": "#/$defs"},
                {"$ref": "#/definitions"},
                {"$ref": "#/properties/result/$defsX"},
                {"$ref": "#/properties/result/a/b"},
                {"$ref": "other.json#/a"},
                {"$ref": 3},
                {"nested": {"$ref": "#/properties/result/x"}}
            ])
        );
    }

    #[test]
    fn normalize_output_schema_objects_cases() {
        let mut v = json!([
            {"type": "object"},
            {"type": ["object", "null"]},
            {"anyOf": [{"type": "object", "properties": {"a": {}}}]}
        ]);
        normalize_output_schema_objects(&mut v);
        assert_eq!(
            v,
            json!([
                {"type": "object", "properties": {}, "additionalProperties": false},
                {"type": ["object", "null"]},
                {"anyOf": [{"type": "object", "properties": {"a": {}}, "additionalProperties": false}]}
            ])
        );
    }

    // ------------------------------------------------------------ output config param

    #[test]
    fn build_output_config_param_cases() {
        // Go TestExtendedThinkingAndEffort/thinking_config_table case 1 (effort alone) and
        // TestOutputSchemaAndStructuredOutput (schema with and without effort).
        assert_eq!(
            build_output_config_param(None, None).unwrap(),
            (None, false)
        );
        assert_eq!(
            build_output_config_param(None, Some(&GenerateContentConfig::default())).unwrap(),
            (None, false)
        );

        let (out, unwrap) = build_output_config_param(Some(Effort::High), None).unwrap();
        assert!(!unwrap);
        assert_eq!(json_of(&out.unwrap()), r#"{"effort":"high"}"#);

        let cfg = json_schema_cfg("", json!({"type": "OBJECT", "properties": {}}));
        let (out, unwrap) = build_output_config_param(Some(Effort::XHigh), Some(&cfg)).unwrap();
        assert!(!unwrap);
        assert_eq!(
            json_of(&out.unwrap()),
            r#"{"effort":"xhigh","format":{"schema":{"additionalProperties":false,"properties":{},"type":"object"},"type":"json_schema"}}"#
        );

        let cfg = json_schema_cfg("", json!({"type": "string"}));
        let (out, unwrap) = build_output_config_param(None, Some(&cfg)).unwrap();
        assert!(unwrap);
        assert_eq!(
            json_of(&out.unwrap()),
            r#"{"format":{"schema":{"additionalProperties":false,"properties":{"result":{"type":"string"}},"required":["result"],"type":"object"},"type":"json_schema"}}"#
        );

        let cfg = json_schema_cfg("", json!({}));
        let err = build_output_config_param(Some(Effort::Low), Some(&cfg)).unwrap_err();
        assert_eq!(
            err.to_string(),
            "anthropicmodel: invalid configuration: ResponseJsonSchema must be a non-empty JSON schema"
        );
    }
}
