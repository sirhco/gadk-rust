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

//! Function tools and tool choice; tool-use ID sanitizing lives in [`crate::tool_id`].

use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;

use gadk_genai::{FunctionDeclaration, GenerateContentConfig, ThinkingConfig, Tool, ToolConfig};
use gadk_platform::{go_to_lower, go_to_upper};
use serde_json::{Map, Value};

use crate::error::{AnthropicError, ErrorKind, go_quote};
use crate::thinking::is_thinking_enabled;
use crate::validate::invalid_config;
use crate::wire::{ObjectTag, ToolChoiceParam, ToolInputSchema, ToolParam};

/// `LlmRequest.tools`; only whether it is empty matters.
pub(crate) type ReqTools = HashMap<String, Arc<dyn Any + Send + Sync>>;

/// An `InvalidConfig` + `UnsupportedToolType` error with `<detail>`.
fn unsupported_tool(detail: &str) -> AnthropicError {
    AnthropicError::wrap2(
        ErrorKind::InvalidConfig,
        ErrorKind::UnsupportedToolType,
        detail,
    )
}

/// The mode of a function-calling config, trimmed and upper-cased (unset is `""`).
fn normalized_mode(raw: &str) -> String {
    go_to_upper(raw.trim())
}

/// Validates the tools: `Ok(true)` when some tool has function declarations; errors for
/// every non-function tool (`InvalidConfig` + `UnsupportedToolType`), an entry without
/// declarations, and a declaration with a blank name.
pub(crate) fn validate_tools(tools: &[Tool]) -> Result<bool, AnthropicError> {
    let mut has_decls = false;
    for t in tools {
        if t.google_search.is_some() {
            return Err(unsupported_tool(
                "tools.google_search is not supported by Claude",
            ));
        }
        if t.google_search_retrieval.is_some() {
            return Err(unsupported_tool(
                "tools.google_search_retrieval is not supported by Claude",
            ));
        }
        if t.retrieval.is_some() {
            return Err(unsupported_tool(
                "tools.retrieval is not supported by Claude",
            ));
        }
        if t.code_execution.is_some() {
            return Err(unsupported_tool(
                "tools.code_execution is not supported by Claude",
            ));
        }
        if t.enterprise_web_search.is_some() {
            return Err(unsupported_tool(
                "tools.enterprise_web_search is not supported by Claude",
            ));
        }
        if t.google_maps.is_some() {
            return Err(unsupported_tool(
                "tools.google_maps is not supported by Claude",
            ));
        }
        if t.computer_use.is_some() {
            return Err(unsupported_tool(
                "tools.computer_use is not supported by Claude",
            ));
        }
        if t.url_context.is_some() {
            return Err(unsupported_tool(
                "tools.url_context is not supported by Claude",
            ));
        }
        if t.file_search.is_some() {
            return Err(unsupported_tool(
                "tools.file_search is not supported by Claude",
            ));
        }
        if t.parallel_ai_search.is_some() {
            return Err(unsupported_tool(
                "tools.parallel_ai_search is not supported by Claude",
            ));
        }
        if !t.mcp_servers.is_empty() {
            return Err(unsupported_tool(
                "tools.mcp_servers is not supported by Claude",
            ));
        }
        if t.exa_ai_search.is_some() {
            return Err(unsupported_tool(
                "tools.exa_ai_search is not supported by Claude",
            ));
        }
        if t.function_declarations.is_empty() {
            return Err(unsupported_tool(
                "tools entry contains no function_declarations",
            ));
        }
        for decl in &t.function_declarations {
            if decl.name.trim().is_empty() {
                return Err(invalid_config(
                    "tools.function_declarations entry has an empty name",
                ));
            }
            has_decls = true;
        }
    }
    Ok(has_decls)
}

/// Validates the tool config, with texts per field and per mode (`AUTO`, `ANY`, `NONE`, an
/// unknown mode).
pub(crate) fn validate_tool_config(
    tc: Option<&ToolConfig>,
    has_tools: bool,
    thinking: Option<&ThinkingConfig>,
) -> Result<(), AnthropicError> {
    let Some(tc) = tc else {
        return Ok(());
    };
    if tc.retrieval_config.is_some() {
        return Err(invalid_config(
            "tool_config.retrieval_config is not supported",
        ));
    }
    if tc.include_server_side_tool_invocations == Some(true) {
        return Err(invalid_config(
            "tool_config.include_server_side_tool_invocations is not supported",
        ));
    }
    let Some(fcc) = tc.function_calling_config.as_ref() else {
        return Ok(());
    };
    if fcc.stream_function_call_arguments == Some(true) {
        return Err(invalid_config(
            "tool_config.function_calling_config.stream_function_call_arguments is not supported",
        ));
    }

    let raw_mode = fcc.mode.as_ref().map_or("", |m| m.as_str());
    let allowed = &fcc.allowed_function_names;
    match normalized_mode(raw_mode).as_str() {
        "" | "AUTO" => {
            if !allowed.is_empty() {
                return Err(invalid_config(
                    "tool_config.function_calling_config.allowed_function_names is not supported with mode AUTO",
                ));
            }
        }
        "ANY" => {
            if !has_tools {
                return Err(invalid_config(
                    "tool_config.function_calling_config.mode ANY requires at least one declared function tool",
                ));
            }
            if is_thinking_enabled(thinking) {
                return Err(invalid_config(
                    "tool_config.function_calling_config.mode ANY cannot be combined with an enabled thinking_config on Claude",
                ));
            }
            if allowed.len() > 1 {
                return Err(invalid_config(
                    "tool_config.function_calling_config.allowed_function_names with multiple names is not supported by Claude",
                ));
            }
            if allowed.len() == 1 && allowed[0].trim().is_empty() {
                return Err(invalid_config(
                    "tool_config.function_calling_config.allowed_function_names must not contain an empty name",
                ));
            }
        }
        "NONE" => {
            if !allowed.is_empty() {
                return Err(invalid_config(
                    "tool_config.function_calling_config.allowed_function_names is not supported with mode NONE",
                ));
            }
        }
        _ => {
            return Err(invalid_config(format!(
                "unsupported tool_config.function_calling_config.mode {}",
                go_quote(raw_mode)
            )));
        }
    }
    Ok(())
}

/// Builds one [`ToolParam`] per declaration, and the tool choice
/// (`none` only with tools, `tool` for one allowed name in `ANY`, `any`, else `auto` when
/// there are declared tools).
pub(crate) fn build_tools_and_choice(
    cfg: Option<&GenerateContentConfig>,
    req_tools: &ReqTools,
) -> Result<(Vec<ToolParam>, Option<ToolChoiceParam>), AnthropicError> {
    let mut tools = Vec::new();
    if let Some(cfg) = cfg {
        for t in &cfg.tools {
            for decl in &t.function_declarations {
                tools.push(convert_function_declaration(decl)?);
            }
        }
    }

    let has_tools = !tools.is_empty() || !req_tools.is_empty();
    let fcc = cfg
        .and_then(|c| c.tool_config.as_ref())
        .and_then(|tc| tc.function_calling_config.as_ref());

    let choice = match fcc {
        Some(fcc) => {
            let raw_mode = fcc.mode.as_ref().map_or("", |m| m.as_str());
            match normalized_mode(raw_mode).as_str() {
                "NONE" => (!tools.is_empty()).then_some(ToolChoiceParam::None),
                "ANY" => {
                    if fcc.allowed_function_names.len() == 1 {
                        Some(ToolChoiceParam::Tool {
                            name: fcc.allowed_function_names[0].trim().to_string(),
                        })
                    } else {
                        Some(ToolChoiceParam::Any)
                    }
                }
                _ => (has_tools && !tools.is_empty()).then_some(ToolChoiceParam::Auto),
            }
        }
        None => (has_tools && !tools.is_empty()).then_some(ToolChoiceParam::Auto),
    };

    Ok((tools, choice))
}

/// Converts a function declaration: trimmed name, the input schema, the description when
/// non-empty.
pub(crate) fn convert_function_declaration(
    decl: &FunctionDeclaration,
) -> Result<ToolParam, AnthropicError> {
    let input_schema = build_tool_input_schema(decl)?;
    Ok(ToolParam {
        input_schema,
        name: decl.name.trim().to_string(),
        description: (!decl.description.is_empty()).then(|| decl.description.clone()),
        cache_control: None,
    })
}

/// Builds the input schema: `parameters_json_schema`, else `parameters` (serialized as genai
/// JSON), else `{"properties":{}}`; types lowercased; `properties`, string `required` entries,
/// and every other key except `type` as extras.
pub(crate) fn build_tool_input_schema(
    decl: &FunctionDeclaration,
) -> Result<ToolInputSchema, AnthropicError> {
    let marshalled = if let Some(raw) = decl.parameters_json_schema.as_ref() {
        serde_json::to_value(raw)
    } else if let Some(params) = decl.parameters.as_ref() {
        serde_json::to_value(params)
    } else {
        return Ok(ToolInputSchema {
            properties: Value::Object(Map::new()),
            required: Vec::new(),
            kind: ObjectTag,
            extra: Map::new(),
        });
    };

    let value = marshalled.map_err(|_| {
        invalid_config(format!(
            "failed to marshal tool schema for {}",
            go_quote(&decl.name)
        ))
    })?;
    // The schema must be a JSON object: anything else (including null) fails.
    let Value::Object(mut schema) = value else {
        return Err(invalid_config(format!(
            "tool schema for {} must be a JSON object",
            go_quote(&decl.name)
        )));
    };

    lowercase_schema_map(&mut schema);

    let properties = match schema.remove("properties") {
        Some(props) if !props.is_null() => props,
        _ => Value::Object(Map::new()),
    };

    let required = match schema.remove("required") {
        Some(Value::Array(items)) => items
            .into_iter()
            .filter_map(|item| match item {
                Value::String(s) => Some(s),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };

    schema.remove("type");

    Ok(ToolInputSchema {
        properties,
        required,
        kind: ObjectTag,
        extra: schema,
    })
}

/// Lowercases every string `type` (and the strings of an array
/// `type`) at any depth.
pub(crate) fn lowercase_schema_types(node: &mut Value) {
    match node {
        Value::Object(map) => lowercase_schema_map(map),
        Value::Array(items) => items.iter_mut().for_each(lowercase_schema_types),
        _ => {}
    }
}

/// [`lowercase_schema_types`] on an object node (a tool or output schema root).
pub(crate) fn lowercase_schema_map(map: &mut Map<String, Value>) {
    match map.get_mut("type") {
        Some(Value::String(t)) => *t = go_to_lower(t),
        Some(Value::Array(elems)) => {
            for elem in elems.iter_mut() {
                if let Value::String(s) = elem {
                    *s = go_to_lower(s);
                }
            }
        }
        _ => {}
    }
    map.values_mut().for_each(lowercase_schema_types);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;
    use crate::wire::to_go_json;
    use gadk_genai::{
        ComputerUse, EnterpriseWebSearch, FileSearch, FunctionCallingConfig,
        FunctionCallingConfigMode, GoogleMaps, GoogleSearch, GoogleSearchRetrieval, MCPServer,
        Retrieval, RetrievalConfig, Schema, ToolCodeExecution, ToolExaAISearch,
        ToolParallelAISearch, Type, URLContext,
    };
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn json_of<T: serde::Serialize>(v: &T) -> String {
        String::from_utf8(to_go_json(v).unwrap()).unwrap()
    }

    fn decl(name: &str) -> FunctionDeclaration {
        FunctionDeclaration {
            name: name.into(),
            ..Default::default()
        }
    }

    fn fn_tool(names: &[&str]) -> Tool {
        Tool {
            function_declarations: names.iter().map(|n| decl(n)).collect(),
            ..Default::default()
        }
    }

    /// Go `TestToolsAndToolConfig`'s `weatherTools`.
    fn weather_tools() -> Vec<Tool> {
        let mut props = BTreeMap::new();
        props.insert(
            "city".to_string(),
            Schema {
                r#type: Some(Type::String),
                description: "City name".into(),
                ..Default::default()
            },
        );
        props.insert(
            "days".to_string(),
            Schema {
                r#type: Some(Type::Array),
                items: Some(Box::new(Schema {
                    r#type: Some(Type::Integer),
                    ..Default::default()
                })),
                ..Default::default()
            },
        );
        vec![Tool {
            function_declarations: vec![
                FunctionDeclaration {
                    name: "get_weather".into(),
                    description: "Look up weather.".into(),
                    parameters: Some(Schema {
                        r#type: Some(Type::Object),
                        properties: props,
                        required: vec!["city".into()],
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                FunctionDeclaration {
                    name: "get_time".into(),
                    description: "Look up time.".into(),
                    parameters_json_schema: Some(json!({
                        "type": "OBJECT",
                        "properties": {"tz": {"type": ["STRING", "NULL"]}}
                    })),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }]
    }

    fn fcc(mode: Option<FunctionCallingConfigMode>, allowed: &[&str]) -> ToolConfig {
        ToolConfig {
            function_calling_config: Some(FunctionCallingConfig {
                mode,
                allowed_function_names: allowed.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    // ------------------------------------------------------------------ validate_tools

    #[test]
    fn validate_tools_rejects_non_function_tools() {
        // Go TestToolsAndToolConfig/rejected_tools_and_tool_configs (tool cases).
        let with_fn = |mut t: Tool| {
            t.function_declarations = vec![decl("fn")];
            t
        };
        let cases: Vec<(&str, Tool, &str)> = vec![
            (
                "google_search",
                Tool {
                    google_search: Some(GoogleSearch::default()),
                    ..Default::default()
                },
                "google_search",
            ),
            (
                "google_search_retrieval",
                Tool {
                    google_search_retrieval: Some(GoogleSearchRetrieval::default()),
                    ..Default::default()
                },
                "google_search_retrieval",
            ),
            (
                "retrieval",
                Tool {
                    retrieval: Some(Retrieval::default()),
                    ..Default::default()
                },
                "retrieval",
            ),
            (
                "code_execution",
                Tool {
                    code_execution: Some(ToolCodeExecution::default()),
                    ..Default::default()
                },
                "code_execution",
            ),
            (
                "enterprise_web_search",
                Tool {
                    enterprise_web_search: Some(EnterpriseWebSearch::default()),
                    ..Default::default()
                },
                "enterprise_web_search",
            ),
            (
                "google_maps",
                Tool {
                    google_maps: Some(GoogleMaps::default()),
                    ..Default::default()
                },
                "google_maps",
            ),
            (
                "computer_use",
                Tool {
                    computer_use: Some(ComputerUse::default()),
                    ..Default::default()
                },
                "computer_use",
            ),
            (
                "url_context",
                Tool {
                    url_context: Some(URLContext::default()),
                    ..Default::default()
                },
                "url_context",
            ),
            (
                "file_search",
                Tool {
                    file_search: Some(FileSearch::default()),
                    ..Default::default()
                },
                "file_search",
            ),
            (
                "parallel_ai_search_with_function_declarations",
                with_fn(Tool {
                    parallel_ai_search: Some(ToolParallelAISearch::default()),
                    ..Default::default()
                }),
                "parallel_ai_search",
            ),
            (
                "mcp_servers_with_function_declarations",
                with_fn(Tool {
                    mcp_servers: vec![MCPServer {
                        name: "srv".into(),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                "mcp_servers",
            ),
            (
                "exa_ai_search_with_function_declarations",
                with_fn(Tool {
                    exa_ai_search: Some(ToolExaAISearch::default()),
                    ..Default::default()
                }),
                "exa_ai_search",
            ),
        ];
        for (name, tool, field) in cases {
            let err = validate_tools(&[tool]).expect_err(name);
            assert_eq!(
                err.to_string(),
                format!(
                    "anthropicmodel: invalid configuration: anthropicmodel: unsupported tool type: tools.{field} is not supported by Claude"
                ),
                "{name}"
            );
            assert_eq!(
                err.kinds(),
                &[ErrorKind::InvalidConfig, ErrorKind::UnsupportedToolType],
                "{name}"
            );
        }
    }

    #[test]
    fn validate_tools_rejects_an_empty_tool_entry() {
        // Go TestToolsAndToolConfig/rejected_tools_and_tool_configs/empty_tool.
        let err = validate_tools(&[Tool::default()]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "anthropicmodel: invalid configuration: anthropicmodel: unsupported tool type: tools entry contains no function_declarations"
        );
        assert!(err.is(ErrorKind::InvalidConfig) && err.is(ErrorKind::UnsupportedToolType));
    }

    #[test]
    fn validate_tools_rejects_a_blank_declaration_name() {
        let err = validate_tools(&[fn_tool(&["ok", "  "])]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "anthropicmodel: invalid configuration: tools.function_declarations entry has an empty name"
        );
        assert_eq!(err.kinds(), &[ErrorKind::InvalidConfig]);
    }

    #[test]
    fn validate_tools_reports_whether_declarations_exist() {
        assert!(!validate_tools(&[]).unwrap());
        assert!(validate_tools(&weather_tools()).unwrap());
    }

    // ------------------------------------------------------------- validate_tool_config

    #[test]
    fn validate_tool_config_rejections() {
        // Go TestToolsAndToolConfig/rejected_tools_and_tool_configs (tool-config cases), plus
        // the remaining branches of validateToolConfig.
        let budget = ThinkingConfig {
            thinking_budget: Some(1024),
            ..Default::default()
        };
        let cases: Vec<(&str, ToolConfig, bool, Option<&ThinkingConfig>, &str)> = vec![
            (
                "include_server_side_tool_invocations",
                ToolConfig {
                    include_server_side_tool_invocations: Some(true),
                    ..Default::default()
                },
                true,
                None,
                "tool_config.include_server_side_tool_invocations is not supported",
            ),
            (
                "retrieval_config",
                ToolConfig {
                    retrieval_config: Some(RetrievalConfig::default()),
                    ..Default::default()
                },
                true,
                None,
                "tool_config.retrieval_config is not supported",
            ),
            (
                "validated_mode",
                fcc(Some(FunctionCallingConfigMode::Validated), &[]),
                true,
                None,
                "unsupported tool_config.function_calling_config.mode \"VALIDATED\"",
            ),
            (
                "unspecified_mode",
                fcc(Some(FunctionCallingConfigMode::Unspecified), &[]),
                true,
                None,
                "unsupported tool_config.function_calling_config.mode \"MODE_UNSPECIFIED\"",
            ),
            (
                "any_with_multiple_allowed_names",
                fcc(
                    Some(FunctionCallingConfigMode::Any),
                    &["get_weather", "get_time"],
                ),
                true,
                None,
                "tool_config.function_calling_config.allowed_function_names with multiple names is not supported by Claude",
            ),
            (
                "any_with_blank_allowed_name",
                fcc(Some(FunctionCallingConfigMode::Any), &[" "]),
                true,
                None,
                "tool_config.function_calling_config.allowed_function_names must not contain an empty name",
            ),
            (
                "any_without_tools",
                fcc(Some(FunctionCallingConfigMode::Any), &[]),
                false,
                None,
                "tool_config.function_calling_config.mode ANY requires at least one declared function tool",
            ),
            (
                "any_with_thinking_enabled",
                fcc(Some(FunctionCallingConfigMode::Any), &[]),
                true,
                Some(&budget),
                "tool_config.function_calling_config.mode ANY cannot be combined with an enabled thinking_config on Claude",
            ),
            (
                "auto_with_allowed_names",
                fcc(Some(FunctionCallingConfigMode::Auto), &["get_weather"]),
                true,
                None,
                "tool_config.function_calling_config.allowed_function_names is not supported with mode AUTO",
            ),
            (
                "unset_mode_with_allowed_names",
                fcc(None, &["get_weather"]),
                true,
                None,
                "tool_config.function_calling_config.allowed_function_names is not supported with mode AUTO",
            ),
            (
                "none_with_allowed_names",
                fcc(Some(FunctionCallingConfigMode::None), &["get_weather"]),
                true,
                None,
                "tool_config.function_calling_config.allowed_function_names is not supported with mode NONE",
            ),
            (
                "stream_function_call_arguments",
                ToolConfig {
                    function_calling_config: Some(FunctionCallingConfig {
                        stream_function_call_arguments: Some(true),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                true,
                None,
                "tool_config.function_calling_config.stream_function_call_arguments is not supported",
            ),
            (
                "unknown_mode_quoted_as_given",
                fcc(
                    Some(FunctionCallingConfigMode::Unknown(" bogus".into())),
                    &[],
                ),
                true,
                None,
                "unsupported tool_config.function_calling_config.mode \" bogus\"",
            ),
        ];
        for (name, tc, has_tools, thinking, want) in cases {
            let err = validate_tool_config(Some(&tc), has_tools, thinking).expect_err(name);
            assert_eq!(
                err.to_string(),
                format!("anthropicmodel: invalid configuration: {want}"),
                "{name}"
            );
            assert_eq!(err.kinds(), &[ErrorKind::InvalidConfig], "{name}");
        }
    }

    #[test]
    fn validate_tool_config_accepts_supported_modes() {
        let disabled = ThinkingConfig {
            thinking_budget: Some(0),
            ..Default::default()
        };
        let ok: Vec<(&str, Option<ToolConfig>, bool, Option<&ThinkingConfig>)> = vec![
            ("nil", None, false, None),
            (
                "no_function_calling_config",
                Some(ToolConfig::default()),
                false,
                None,
            ),
            (
                "include_server_side_false",
                Some(ToolConfig {
                    include_server_side_tool_invocations: Some(false),
                    ..Default::default()
                }),
                false,
                None,
            ),
            (
                "auto",
                Some(fcc(Some(FunctionCallingConfigMode::Auto), &[])),
                true,
                None,
            ),
            ("unset_mode", Some(fcc(None, &[])), false, None),
            (
                "none",
                Some(fcc(Some(FunctionCallingConfigMode::None), &[])),
                true,
                None,
            ),
            (
                "any_one_name",
                Some(fcc(Some(FunctionCallingConfigMode::Any), &["get_weather"])),
                true,
                None,
            ),
            (
                "any_with_thinking_disabled",
                Some(fcc(Some(FunctionCallingConfigMode::Any), &[])),
                true,
                Some(&disabled),
            ),
            (
                "lowercase_padded_any",
                Some(fcc(
                    Some(FunctionCallingConfigMode::Unknown(" any ".into())),
                    &[],
                )),
                true,
                None,
            ),
        ];
        for (name, tc, has_tools, thinking) in ok {
            assert!(
                validate_tool_config(tc.as_ref(), has_tools, thinking).is_ok(),
                "{name}"
            );
        }
    }

    // ---------------------------------------------------------- build_tools_and_choice

    #[test]
    fn build_tools_normalizes_schemas() {
        // Go TestToolsAndToolConfig/schema_normalization_and_parallel_tool_calls_and_id_sanitization
        // (the tools half).
        let cfg = GenerateContentConfig {
            tools: weather_tools(),
            ..Default::default()
        };
        let (tools, choice) = build_tools_and_choice(Some(&cfg), &ReqTools::new()).unwrap();
        assert_eq!(
            json_of(&tools),
            concat!(
                r#"[{"input_schema":{"properties":{"city":{"description":"City name","type":"string"},"#,
                r#""days":{"items":{"type":"integer"},"type":"array"}},"required":["city"],"type":"object"},"#,
                r#""name":"get_weather","description":"Look up weather."},"#,
                r#"{"input_schema":{"properties":{"tz":{"type":["string","null"]}},"type":"object"},"#,
                r#""name":"get_time","description":"Look up time."}]"#
            )
        );
        assert_eq!(choice, Some(ToolChoiceParam::Auto));
    }

    #[test]
    fn build_tools_and_choice_modes() {
        // Go TestToolsAndToolConfig/tool_config_modes_auto_any_none and the ANY + one allowed
        // name case of schema_normalization_and_parallel_tool_calls_and_id_sanitization.
        let with = |tc: ToolConfig| GenerateContentConfig {
            tools: weather_tools(),
            tool_config: Some(tc),
            ..Default::default()
        };
        let cases: Vec<(&str, GenerateContentConfig, Option<ToolChoiceParam>)> = vec![
            (
                "auto",
                with(fcc(Some(FunctionCallingConfigMode::Auto), &[])),
                Some(ToolChoiceParam::Auto),
            ),
            (
                "any",
                with(fcc(Some(FunctionCallingConfigMode::Any), &[])),
                Some(ToolChoiceParam::Any),
            ),
            (
                "none",
                with(fcc(Some(FunctionCallingConfigMode::None), &[])),
                Some(ToolChoiceParam::None),
            ),
            (
                "any_one_allowed_name_trimmed",
                with(fcc(
                    Some(FunctionCallingConfigMode::Any),
                    &[" get_weather "],
                )),
                Some(ToolChoiceParam::Tool {
                    name: "get_weather".into(),
                }),
            ),
            (
                "unset_mode",
                with(fcc(None, &[])),
                Some(ToolChoiceParam::Auto),
            ),
            (
                "tool_config_without_function_calling_config",
                with(ToolConfig::default()),
                Some(ToolChoiceParam::Auto),
            ),
            (
                "none_without_tools",
                GenerateContentConfig {
                    tool_config: Some(fcc(Some(FunctionCallingConfigMode::None), &[])),
                    ..Default::default()
                },
                None,
            ),
            ("no_tools", GenerateContentConfig::default(), None),
        ];
        for (name, cfg, want) in cases {
            let (_, choice) = build_tools_and_choice(Some(&cfg), &ReqTools::new()).unwrap();
            assert_eq!(choice, want, "{name}");
        }
    }

    #[test]
    fn request_tools_alone_send_no_tool_choice() {
        // Go: hasTools counts req.Tools, but auto needs declared tools.
        let mut req_tools = ReqTools::new();
        req_tools.insert("t".into(), Arc::new(()) as Arc<dyn Any + Send + Sync>);
        let (tools, choice) = build_tools_and_choice(None, &req_tools).unwrap();
        assert!(tools.is_empty());
        assert_eq!(choice, None);
        let cfg = GenerateContentConfig {
            tool_config: Some(fcc(Some(FunctionCallingConfigMode::Auto), &[])),
            ..Default::default()
        };
        let (_, choice) = build_tools_and_choice(Some(&cfg), &req_tools).unwrap();
        assert_eq!(choice, None);
    }

    #[test]
    fn build_tools_propagates_schema_errors() {
        let cfg = GenerateContentConfig {
            tools: vec![Tool {
                function_declarations: vec![FunctionDeclaration {
                    name: "bad".into(),
                    parameters_json_schema: Some(json!([1, 2])),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let err = build_tools_and_choice(Some(&cfg), &ReqTools::new()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "anthropicmodel: invalid configuration: tool schema for \"bad\" must be a JSON object"
        );
    }

    // ------------------------------------------------------ convert / input schema

    #[test]
    fn convert_function_declaration_trims_name_and_omits_empty_description() {
        let tool = convert_function_declaration(&decl("  tool_a ")).unwrap();
        assert_eq!(
            json_of(&tool),
            r#"{"input_schema":{"properties":{},"type":"object"},"name":"tool_a"}"#
        );
    }

    #[test]
    fn input_schema_without_parameters_is_empty_object() {
        let schema = build_tool_input_schema(&decl("x")).unwrap();
        assert_eq!(json_of(&schema), r#"{"properties":{},"type":"object"}"#);
    }

    #[test]
    fn input_schema_prefers_parameters_json_schema() {
        let d = FunctionDeclaration {
            name: "x".into(),
            parameters: Some(Schema {
                r#type: Some(Type::Object),
                description: "from parameters".into(),
                ..Default::default()
            }),
            parameters_json_schema: Some(json!({"type": "object", "description": "from json"})),
            ..Default::default()
        };
        let schema = build_tool_input_schema(&d).unwrap();
        assert_eq!(
            json_of(&schema),
            r#"{"properties":{},"type":"object","description":"from json"}"#
        );
    }

    #[test]
    fn input_schema_keeps_extras_and_string_required_entries() {
        let d = FunctionDeclaration {
            name: "x".into(),
            parameters_json_schema: Some(json!({
                "type": "OBJECT",
                "properties": null,
                "required": ["a", 1, "b", null],
                "additionalProperties": false,
                "$defs": {"D": {"type": "STRING"}}
            })),
            ..Default::default()
        };
        let schema = build_tool_input_schema(&d).unwrap();
        assert_eq!(schema.properties, json!({}));
        assert_eq!(schema.required, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(
            json_of(&schema),
            r#"{"properties":{},"required":["a","b"],"type":"object","$defs":{"D":{"type":"string"}},"additionalProperties":false}"#
        );
    }

    #[test]
    fn input_schema_required_that_is_not_an_array_is_dropped() {
        let d = FunctionDeclaration {
            name: "x".into(),
            parameters_json_schema: Some(json!({"required": "a"})),
            ..Default::default()
        };
        let schema = build_tool_input_schema(&d).unwrap();
        assert!(schema.required.is_empty());
        assert!(schema.extra.is_empty());
    }

    #[test]
    fn input_schema_rejects_non_objects() {
        for (name, raw) in [
            ("array", json!([])),
            ("string", json!("s")),
            ("null", Value::Null),
        ] {
            let d = FunctionDeclaration {
                name: "my tool".into(),
                parameters_json_schema: Some(raw),
                ..Default::default()
            };
            let err = build_tool_input_schema(&d).expect_err(name);
            assert_eq!(
                err.to_string(),
                "anthropicmodel: invalid configuration: tool schema for \"my tool\" must be a JSON object",
                "{name}"
            );
            assert_eq!(err.kinds(), &[ErrorKind::InvalidConfig], "{name}");
        }
    }

    #[test]
    fn lowercase_schema_types_recurses() {
        let mut v = json!({
            "type": "OBJECT",
            "properties": {
                "a": {"type": ["STRING", "NULL", 3]},
                "b": {"anyOf": [{"type": "INTEGER"}, {"items": {"type": "BOOLEAN"}}]}
            },
            "enum": ["KEEP"]
        });
        lowercase_schema_types(&mut v);
        assert_eq!(
            v,
            json!({
                "type": "object",
                "properties": {
                    "a": {"type": ["string", "null", 3]},
                    "b": {"anyOf": [{"type": "integer"}, {"items": {"type": "boolean"}}]}
                },
                "enum": ["KEEP"]
            })
        );
        // Go strings.ToLower, rune by rune: U+0130 → `i`, and Σ is σ even when final.
        let mut v = json!({"type": ["\u{130}NTEGER", "\u{3a3}"]});
        lowercase_schema_types(&mut v);
        assert_eq!(v, json!({"type": ["integer", "\u{3c3}"]}));
    }
}
