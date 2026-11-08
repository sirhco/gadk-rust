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

//! Function tools and tool choice (port of ADK-Go `tools.go`, except `toolUseIDSanitizer`,
//! which is [`crate::tool_id`]).
//!
//! Placeholder signatures written by Task 1; Task 3 implements them.

use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;

use gadk_genai::{FunctionDeclaration, GenerateContentConfig, ThinkingConfig, Tool, ToolConfig};
use serde_json::Value;

use crate::error::AnthropicError;
use crate::wire::{ToolChoiceParam, ToolInputSchema, ToolParam};

/// `LlmRequest.tools` (Go `req.Tools`, `map[string]any`); only whether it is empty matters.
pub(crate) type ReqTools = HashMap<String, Arc<dyn Any + Send + Sync>>;

/// Go `validateTools`: `Ok(true)` when some tool has function declarations; Go's errors for
/// every non-function tool (`ErrInvalidConfig` + `ErrUnsupportedToolType`), an entry without
/// declarations, and a declaration with a blank name.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn validate_tools(tools: &[Tool]) -> Result<bool, AnthropicError> {
    todo!("Task 3")
}

/// Go `validateToolConfig`, with Go's texts per field and per mode (`AUTO`, `ANY`, `NONE`, an
/// unknown mode).
#[allow(unused_variables, clippy::todo)]
pub(crate) fn validate_tool_config(
    tc: Option<&ToolConfig>,
    has_tools: bool,
    thinking: Option<&ThinkingConfig>,
) -> Result<(), AnthropicError> {
    todo!("Task 3")
}

/// Go `buildToolsAndChoice`: one [`ToolParam`] per declaration, and the tool choice
/// (`none` only with tools, `tool` for one allowed name in `ANY`, `any`, else `auto` when
/// there are declared tools).
#[allow(unused_variables, clippy::todo)]
pub(crate) fn build_tools_and_choice(
    cfg: Option<&GenerateContentConfig>,
    req_tools: &ReqTools,
) -> Result<(Vec<ToolParam>, Option<ToolChoiceParam>), AnthropicError> {
    todo!("Task 3")
}

/// Go `convertFunctionDeclaration`: trimmed name, the input schema, the description when
/// non-empty.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn convert_function_declaration(
    decl: &FunctionDeclaration,
) -> Result<ToolParam, AnthropicError> {
    todo!("Task 3")
}

/// Go `buildToolInputSchema`: `parameters_json_schema`, else `parameters` (marshalled as genai
/// JSON), else `{"properties":{}}`; types lowercased; `properties`, string `required` entries,
/// and every other key except `type` as extras.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn build_tool_input_schema(
    decl: &FunctionDeclaration,
) -> Result<ToolInputSchema, AnthropicError> {
    todo!("Task 3")
}

/// Go `lowercaseSchemaTypes`: lowercases every string `type` (and the strings of an array
/// `type`) at any depth.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn lowercase_schema_types(node: &mut Value) {
    todo!("Task 3")
}
