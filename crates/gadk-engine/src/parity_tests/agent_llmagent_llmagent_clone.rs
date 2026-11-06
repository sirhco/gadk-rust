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

//! Port of ADK-Go agent/llmagent/llmagent_clone_test.go.

use std::sync::Arc;

use gadk_agent::{Context, Tool, ToolError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::llm::agent;
use crate::llm::config::LlmAgentConfig;
use crate::llm::state::reveal;
use crate::tool::{FunctionTool, FunctionToolConfig};

#[derive(Deserialize, JsonSchema)]
struct DummyArgs {}

#[derive(Serialize, JsonSchema)]
struct DummyResult {}

fn dummy_tool(name: &str) -> Arc<dyn Tool> {
    FunctionTool::new(
        FunctionToolConfig::new(name, ""),
        |_ctx: Context, _args: DummyArgs| async { Ok::<_, ToolError>(DummyResult {}) },
    )
    .unwrap()
}

// parity: adapted: Rust has no slice aliasing; asserts the agent's tool list is a distinct Vec that a later push to the caller's Vec does not change
#[test]
fn test_new_clones_tools_slice() {
    let mut orig: Vec<Arc<dyn Tool>> = Vec::with_capacity(4);
    orig.push(dummy_tool("test_tool"));

    let a = agent::new(LlmAgentConfig {
        name: "test_agent".into(),
        tools: orig.clone(),
        ..Default::default()
    })
    .unwrap();
    let state = reveal(a.as_ref()).expect("an LlmAgent");
    {
        let tools = state.tools.read();
        assert_eq!(tools.len(), 1, "expected 1 tool");
        assert!(Arc::ptr_eq(&tools[0], &orig[0]), "same tool instance");
        assert_ne!(
            tools.as_ptr() as *const (),
            orig.as_ptr() as *const (),
            "expected Tools slice to be cloned, but it shares the same backing array with caller"
        );
    }

    orig.push(dummy_tool("extra_tool"));
    assert_eq!(orig.len(), 2, "expected caller slice to have 2 tools");
    assert_eq!(
        state.tools.read().len(),
        1,
        "agent Tools length must remain 1 after mutating original slice"
    );
}
