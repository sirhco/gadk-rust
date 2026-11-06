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

//! Port of Go internal/toolinternal/tool_test.go.

use std::sync::Arc;

use gadk_agent::{Context, Mode, Tool, ToolError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::llm::LlmAgentConfig;
use crate::llm::agent as llm_agent;
use crate::tool::task_agent_tool::TaskAgentTool;
use crate::tool::{FunctionTool, FunctionToolConfig};

#[derive(Deserialize, Serialize, JsonSchema)]
struct NoArgs {}

// parity: adapted: Go's toolinternal.ResponseDeferrer interface assertion → Tool::defers_response() (default false); Go's nonDeferringTool stub is a plain FunctionTool
#[test]
fn test_response_deferrer_contract() {
    // TaskAgentTool implements ResponseDeferrer and returns true
    let a = llm_agent::new(LlmAgentConfig {
        name: "doer".into(),
        mode: Mode::Task,
        ..Default::default()
    })
    .unwrap();
    let tool: Arc<dyn Tool> = TaskAgentTool::new(a);
    assert!(
        tool.defers_response(),
        "TaskAgentTool.defers_response() = false, want true"
    );

    // plain non-deferring tool does NOT implement ResponseDeferrer
    let plain = FunctionTool::new(
        FunctionToolConfig::new("non_deferring", ""),
        |_ctx: Context, _a: NoArgs| async { Ok::<_, ToolError>(NoArgs {}) },
    )
    .unwrap();
    assert!(
        !plain.defers_response(),
        "non_deferring unexpectedly defers its response"
    );
}
