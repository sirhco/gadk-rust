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

#![allow(dead_code, unused_variables, clippy::todo)] // D1: Task 4 fills the bodies.

use gadk_genai::{GenerateContentConfig, ToolConfig};

use crate::completions::wire::{ChatCompletionToolChoiceOption, ChatCompletionToolParam};
use crate::error::OpenAiError;

/// Go `convertTools`: each function declaration of `cfg.tools` as a Chat Completions function
/// tool (`{"function":{…},"type":"function"}`), after
/// [`ensure_function_tool_only`](crate::shared::tools::ensure_function_tool_only); `None` or no
/// tools is empty.
pub(crate) fn convert_tools(
    cfg: Option<&GenerateContentConfig>,
) -> Result<Vec<ChatCompletionToolParam>, OpenAiError> {
    todo!("Task 4")
}

/// Go `convertToolChoice`: the Chat Completions `tool_choice` for a tool config; `Ok(None)`
/// when it names nothing (the provider's default stands).
pub(crate) fn convert_tool_choice(
    tool_cfg: Option<&ToolConfig>,
) -> Result<Option<ChatCompletionToolChoiceOption>, OpenAiError> {
    todo!("Task 4")
}
