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

//! Tool validation both APIs share (port of ADK-Go `internal/shared/tools.go`).

#![allow(dead_code, unused_variables, clippy::todo)] // D1: Task 3 fills the bodies.

use gadk_genai::Tool;

use crate::error::OpenAiError;

/// Go `EnsureFunctionToolOnly`: rejects a nil tool (`None`: `openai: tool <idx> is nil`), one
/// carrying a non-function tool neither API is sent (`openai: non-function tools are not
/// supported (tool <idx>)`), and one declaring no functions (`openai: tool <idx> does not
/// declare any functions`). All three are plain errors (no sentinel).
pub(crate) fn ensure_function_tool_only(
    idx: usize,
    tool: Option<&Tool>,
) -> Result<(), OpenAiError> {
    todo!("Task 3")
}
