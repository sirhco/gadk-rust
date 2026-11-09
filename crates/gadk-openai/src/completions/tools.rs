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

//! Chat Completions tools (port of ADK-Go `internal/completions/tools.go`).

use gadk_genai::{
    FunctionCallingConfigMode, FunctionDeclaration, GenerateContentConfig, ToolConfig,
};
use gadk_platform::go_quote;
use serde_json::{Map, Value};

use crate::completions::wire::{
    AllowedToolsTag, ChatCompletionAllowedToolChoiceParam, ChatCompletionAllowedToolsMode,
    ChatCompletionAllowedToolsParam, ChatCompletionFunctionToolParam,
    ChatCompletionNamedToolChoiceFunctionParam, ChatCompletionNamedToolChoiceParam,
    ChatCompletionToolChoiceOption, ChatCompletionToolParam, FunctionTag,
};
use crate::error::OpenAiError;
use crate::oaishared::FunctionDefinitionParam;
use crate::shared::schema::{normalize_schema, schema_to_map};
use crate::shared::tools::ensure_function_tool_only;

/// Go `convertTools`: each function declaration of `cfg.tools` as a Chat Completions function
/// tool (`{"function":{…},"type":"function"}`), after
/// [`ensure_function_tool_only`](crate::shared::tools::ensure_function_tool_only); `None` or no
/// tools is empty.
pub(crate) fn convert_tools(
    cfg: Option<&GenerateContentConfig>,
) -> Result<Vec<ChatCompletionToolParam>, OpenAiError> {
    let Some(cfg) = cfg else {
        return Ok(Vec::new());
    };
    let mut tools = Vec::new();
    for (i, tool) in cfg.tools.iter().enumerate() {
        ensure_function_tool_only(i, Some(tool))?;
        for decl in &tool.function_declarations {
            tools.push(ChatCompletionToolParam::Function(
                convert_function_declaration(decl)?,
            ));
        }
    }
    Ok(tools)
}

/// Go `convertFunctionDeclaration`: one declaration as a Chat Completions function tool,
/// whose declaration nests under `function`. The parameters are the genai schema, else the
/// JSON schema (not strict-rewritten: optional properties stay optional), else an empty object
/// schema. Go's nil-declaration error has no counterpart (a `Vec` holds no nil).
fn convert_function_declaration(
    fn_decl: &FunctionDeclaration,
) -> Result<ChatCompletionFunctionToolParam, OpenAiError> {
    if fn_decl.name.is_empty() {
        return Err(OpenAiError::plain(
            "openai: function declaration missing name",
        ));
    }
    let mut params_map = schema_to_map(fn_decl.parameters.as_ref())?;
    if params_map.is_none()
        && let Some(json_schema) = &fn_decl.parameters_json_schema
    {
        params_map = normalize_schema(Some(json_schema))?;
    }
    let params_map = params_map.unwrap_or_else(|| {
        let mut empty = Map::new();
        empty.insert("type".to_string(), Value::from("object"));
        empty.insert("properties".to_string(), Value::Object(Map::new()));
        empty
    });
    Ok(ChatCompletionFunctionToolParam {
        function: FunctionDefinitionParam {
            name: fn_decl.name.clone(),
            strict: None,
            description: (!fn_decl.description.is_empty()).then(|| fn_decl.description.clone()),
            parameters: Some(params_map),
        },
        kind: FunctionTag,
    })
}

/// Go `convertToolChoice`: the Chat Completions `tool_choice` for a tool config; `Ok(None)`
/// when it names nothing (the provider's default stands).
pub(crate) fn convert_tool_choice(
    tool_cfg: Option<&ToolConfig>,
) -> Result<Option<ChatCompletionToolChoiceOption>, OpenAiError> {
    let Some(cfg) = tool_cfg.and_then(|t| t.function_calling_config.as_ref()) else {
        return Ok(None);
    };
    let names = &cfg.allowed_function_names;
    // Go's `""` mode is `None` or an empty unknown value.
    let mode = cfg.mode.as_ref().filter(|mode| !mode.as_str().is_empty());
    let choice = match mode {
        None | Some(FunctionCallingConfigMode::Unspecified | FunctionCallingConfigMode::Auto) => {
            if names.is_empty() {
                // Nothing named, so the provider's own default stands.
                return Ok(None);
            }
            allowed_tool_param(names, ChatCompletionAllowedToolsMode::Auto)
                .map(ChatCompletionToolChoiceOption::AllowedTools)
        }
        Some(FunctionCallingConfigMode::None) => {
            Some(ChatCompletionToolChoiceOption::Auto("none".to_string()))
        }
        Some(FunctionCallingConfigMode::Any) => {
            if names.is_empty() {
                Some(ChatCompletionToolChoiceOption::Auto("required".to_string()))
            } else if let Some(name) = sole_function_name(names) {
                // Naming the one function says the same as allowed_tools, in the older form
                // that compatible providers accept and many that do not know allowed_tools
                // reject.
                Some(ChatCompletionToolChoiceOption::FunctionToolChoice(
                    ChatCompletionNamedToolChoiceParam {
                        function: ChatCompletionNamedToolChoiceFunctionParam {
                            name: name.to_string(),
                        },
                        kind: FunctionTag,
                    },
                ))
            } else {
                allowed_tool_param(names, ChatCompletionAllowedToolsMode::Required)
                    .map(ChatCompletionToolChoiceOption::AllowedTools)
            }
        }
        Some(other) => {
            return Err(OpenAiError::plain(format!(
                "openai: unsupported tool calling mode {}",
                go_quote(other.as_str())
            )));
        }
    };
    Ok(choice)
}

/// Go `soleFunctionName`: the one name `names` allows, skipping empty entries; `None` for
/// none or several.
fn sole_function_name(names: &[String]) -> Option<&str> {
    let mut sole: Option<&str> = None;
    for name in names {
        if name.is_empty() {
            continue;
        }
        if sole.is_some() {
            return None;
        }
        sole = Some(name);
    }
    sole
}

/// Go `allowedToolParam`: the allowed-tools choice, each entry nesting its name under
/// `function` (`{"function":{"name":…},"type":"function"}`, Go map key order); `None` when
/// every name is empty.
fn allowed_tool_param(
    names: &[String],
    mode: ChatCompletionAllowedToolsMode,
) -> Option<ChatCompletionAllowedToolChoiceParam> {
    let tools: Vec<Map<String, Value>> = names
        .iter()
        .filter(|name| !name.is_empty())
        .map(|name| {
            let mut function = Map::new();
            function.insert("name".to_string(), Value::from(name.as_str()));
            let mut entry = Map::new();
            entry.insert("type".to_string(), Value::from("function"));
            entry.insert("function".to_string(), Value::Object(function));
            entry
        })
        .collect();
    if tools.is_empty() {
        return None;
    }
    Some(ChatCompletionAllowedToolChoiceParam {
        allowed_tools: ChatCompletionAllowedToolsParam { mode, tools },
        kind: AllowedToolsTag,
    })
}
