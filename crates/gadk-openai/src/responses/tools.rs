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

//! Responses tools.

use gadk_genai::{
    FunctionCallingConfigMode, FunctionDeclaration, GenerateContentConfig, ToolConfig,
};
use gadk_platform::go_quote;
use serde_json::{Map, Value};

use crate::error::OpenAiError;
use crate::responses::wire::{
    AllowedToolsTag, FunctionTag, FunctionToolParam, ResponseToolChoice, ToolChoiceAllowedMode,
    ToolChoiceAllowedParam, ToolChoiceOptions, ToolParam,
};
use crate::shared::schema::{normalize_schema, schema_to_map};
use crate::shared::tools::ensure_function_tool_only;

/// Each function declaration of `cfg.tools` as a Responses function tool,
/// after [`ensure_function_tool_only`](crate::shared::tools::ensure_function_tool_only);
/// `None` or no tools is empty.
pub(crate) fn convert_tools(
    cfg: Option<&GenerateContentConfig>,
) -> Result<Vec<ToolParam>, OpenAiError> {
    let Some(cfg) = cfg else {
        return Ok(Vec::new());
    };
    let mut tools = Vec::new();
    for (i, tool) in cfg.tools.iter().enumerate() {
        ensure_function_tool_only(i, Some(tool))?;
        for decl in &tool.function_declarations {
            tools.push(ToolParam::Function(convert_function_declaration(Some(
                decl,
            ))?));
        }
    }
    Ok(tools)
}

/// Converts a function declaration: an empty
/// name `openai: function declaration missing name` (plain); parameters from `parameters`,
/// else `parameters_json_schema`, else `{"type":"object","properties":{}}`; `strict` pinned
/// `false`.
///
/// Strict is pinned off rather than left unset: an absent flag lets the API pick the mode from
/// the schema shape and make the caller's optional arguments required.
pub(crate) fn convert_function_declaration(
    decl: Option<&FunctionDeclaration>,
) -> Result<FunctionToolParam, OpenAiError> {
    let Some(decl) = decl else {
        return Err(OpenAiError::plain("openai: nil function declaration"));
    };
    if decl.name.is_empty() {
        return Err(OpenAiError::plain(
            "openai: function declaration missing name",
        ));
    }

    let mut parameters = schema_to_map(decl.parameters.as_ref())?;
    if parameters.is_none()
        && let Some(json_schema) = &decl.parameters_json_schema
    {
        parameters = normalize_schema(Some(json_schema))?;
    }
    // No parameters defined: an empty object schema.
    let parameters = parameters.unwrap_or_else(|| {
        Map::from_iter([
            ("type".to_string(), Value::String("object".to_string())),
            ("properties".to_string(), Value::Object(Map::new())),
        ])
    });

    Ok(FunctionToolParam {
        strict: Some(false),
        parameters: Some(parameters),
        name: decl.name.clone(),
        description: (!decl.description.is_empty()).then(|| decl.description.clone()),
        kind: FunctionTag,
    })
}

/// The Responses `tool_choice` for a tool config; `Ok(None)` when it
/// names nothing.
///
/// Mode unset, unspecified or `AUTO` is `None` (the API's own `auto`) unless function names
/// are allowed (`allowed_tools`, mode `auto`); `NONE` is `"none"`; `ANY` is `"required"`, or
/// `allowed_tools` with mode `required` when names are allowed. Another mode is
/// `openai: unsupported tool calling mode "<mode>"` (plain).
pub(crate) fn convert_tool_choice(
    tool_cfg: Option<&ToolConfig>,
) -> Result<Option<ResponseToolChoice>, OpenAiError> {
    let Some(cfg) = tool_cfg.and_then(|t| t.function_calling_config.as_ref()) else {
        return Ok(None);
    };
    let names = &cfg.allowed_function_names;
    let mode = cfg.mode.as_ref().map_or("", |mode| mode.as_str());
    let choice = match cfg.mode.as_ref() {
        _ if mode.is_empty() => auto_choice(names),
        Some(FunctionCallingConfigMode::Unspecified | FunctionCallingConfigMode::Auto) => {
            auto_choice(names)
        }
        Some(FunctionCallingConfigMode::None) => {
            Some(ResponseToolChoice::Mode(ToolChoiceOptions::None))
        }
        Some(FunctionCallingConfigMode::Any) => {
            if names.is_empty() {
                // Any tool may be called.
                Some(ResponseToolChoice::Mode(ToolChoiceOptions::Required))
            } else {
                allowed_tool_param(names, ToolChoiceAllowedMode::Required)
                    .map(ResponseToolChoice::AllowedTools)
            }
        }
        _ => {
            return Err(OpenAiError::plain(format!(
                "openai: unsupported tool calling mode {}",
                go_quote(mode)
            )));
        }
    };
    Ok(choice)
}

/// The auto arm of the tool choice: no choice (letting the API decide) unless function
/// names are allowed.
fn auto_choice(names: &[String]) -> Option<ResponseToolChoice> {
    if names.is_empty() {
        return None;
    }
    allowed_tool_param(names, ToolChoiceAllowedMode::Auto).map(ResponseToolChoice::AllowedTools)
}

/// An `allowed_tools` choice naming each non-empty function; `None`
/// when every name is empty.
fn allowed_tool_param(
    names: &[String],
    mode: ToolChoiceAllowedMode,
) -> Option<ToolChoiceAllowedParam> {
    let tools: Vec<Map<String, Value>> = names
        .iter()
        .filter(|name| !name.is_empty())
        .map(|name| {
            Map::from_iter([
                ("type".to_string(), Value::String("function".to_string())),
                ("name".to_string(), Value::String(name.clone())),
            ])
        })
        .collect();
    if tools.is_empty() {
        return None;
    }
    Some(ToolChoiceAllowedParam {
        mode,
        tools,
        kind: AllowedToolsTag,
    })
}
