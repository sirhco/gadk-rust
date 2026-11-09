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

//! Responses tools (port of ADK-Go `internal/responses/tools.go`).

#![allow(dead_code, unused_variables, clippy::todo)] // D1: Task 5 fills the bodies.

use gadk_genai::{FunctionDeclaration, GenerateContentConfig, ToolConfig};

use crate::error::OpenAiError;
use crate::responses::wire::{FunctionToolParam, ResponseToolChoice, ToolParam};

/// Go `convertTools`: each function declaration of `cfg.tools` as a Responses function tool,
/// after [`ensure_function_tool_only`](crate::shared::tools::ensure_function_tool_only);
/// `None` or no tools is empty.
pub(crate) fn convert_tools(
    cfg: Option<&GenerateContentConfig>,
) -> Result<Vec<ToolParam>, OpenAiError> {
    todo!("Task 5")
}

/// Go `convertFunctionDeclaration`: `None` is `openai: nil function declaration`, an empty
/// name `openai: function declaration missing name` (plain); parameters from `parameters`,
/// else `parameters_json_schema`, else `{"type":"object","properties":{}}`; `strict` pinned
/// `false`.
pub(crate) fn convert_function_declaration(
    decl: Option<&FunctionDeclaration>,
) -> Result<FunctionToolParam, OpenAiError> {
    todo!("Task 5")
}

/// Go `convertToolChoice`: the Responses `tool_choice` for a tool config; `Ok(None)` when it
/// names nothing.
pub(crate) fn convert_tool_choice(
    tool_cfg: Option<&ToolConfig>,
) -> Result<Option<ResponseToolChoice>, OpenAiError> {
    todo!("Task 5")
}
