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

//! Parity port of Go internal/llminternal/tools_processor_test.go.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use gadk_agent::{InvocationContext, ReadonlyContext, Tool, ToolError, Toolset};
use pretty_assertions::assert_eq;

use crate::llm::{
    config::LlmAgentConfig,
    flow::Flow,
    processors::{TestTool, test_llm_agent, tools::tools_processor},
};

/// Go dynamicTestToolset: returns calls[index] once per Tools() call.
struct DynamicTestToolset {
    calls: Vec<Arc<dyn Tool>>,
    index: AtomicUsize,
}

#[async_trait]
impl Toolset for DynamicTestToolset {
    fn name(&self) -> &str {
        "dynamic_toolset"
    }
    async fn tools(&self, _: &ReadonlyContext) -> Result<Vec<Arc<dyn Tool>>, ToolError> {
        let i = self.index.fetch_add(1, Ordering::SeqCst);
        Ok(self.calls.get(i).cloned().into_iter().collect())
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

// parity: adapted: Rust Vec clones cannot alias spare capacity; the test still pins that each flow gets its own [base, toolset-tool] list and the agent's tools are unchanged
#[tokio::test]
async fn test_tool_processor_spare_capacity_not_corrupted() {
    let base = TestTool::named("base_tool");
    let ts = Arc::new(DynamicTestToolset {
        calls: vec![TestTool::named("tool_a"), TestTool::named("tool_b")],
        index: AtomicUsize::new(0),
    });
    let a = test_llm_agent(LlmAgentConfig {
        name: "test_agent".into(),
        tools: vec![base],
        toolsets: vec![ts],
        ..Default::default()
    });
    let ctx = InvocationContext::builder()
        .agent(a.clone())
        .build_invocation();
    let (flow1, flow2) = (Flow::default(), Flow::default());
    let mut req = gadk_model::LlmRequest::default();
    tools_processor(&ctx, &mut req, &flow1).await.unwrap();
    tools_processor(&ctx, &mut req, &flow2).await.unwrap();
    let names = |f: &Flow| {
        f.tools
            .read()
            .iter()
            .map(|t| t.name().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&flow1), ["base_tool", "tool_a"]);
    assert_eq!(names(&flow2), ["base_tool", "tool_b"]);
    let state = crate::llm::state::reveal(&*a).unwrap();
    assert_eq!(state.tools.read().len(), 1, "agent tools were modified");
}
