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

//! Port of Go tool/agenttool/agent_tool_limits_test.go.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures::stream::{self, BoxStream, StreamExt};
use gadk_agent::run_limits::{self, LlmCallLimiter};
use gadk_agent::{AgentError, Context, ContextDelta, ToolError};
use gadk_genai::{Content, Part};
use gadk_model::{Llm, LlmRequest, LlmResponse, ModelError};
use gadk_session::{CreateRequest, InMemorySessionService, SessionService};
use serde_json::{Map, Value, json};

use crate::llm::{LlmAgentConfig, agent as llm_agent};
use crate::tool::{AgentTool, FunctionTool, FunctionToolConfig};

/// Go slowModel: a tool call per turn until called `finish_after` times, then "done".
struct SlowModel {
    finish_after: usize,
    calls: AtomicUsize,
}

impl Llm for SlowModel {
    fn name(&self) -> &str {
        "slow-mock"
    }

    fn generate_content(
        &self,
        _req: LlmRequest,
        _stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let part = if n >= self.finish_after {
            Part::from_text("done")
        } else {
            let mut p = Part::from_function_call("noop", Map::new());
            if let Some(fc) = p.function_call.as_mut() {
                fc.id = "fc".into();
            }
            p
        };
        stream::iter(vec![Ok(LlmResponse {
            content: Some(Content::from_parts(vec![part], "model")),
            ..Default::default()
        })])
        .boxed()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[derive(serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
struct Empty {}

/// Go runChildUnderBudget: a tool context whose invocation carries the resolved budget, as the
/// runner sets it up.
async fn run_child_under_budget(
    m: Arc<SlowModel>,
    max_llm_calls: i32,
) -> Result<Map<String, Value>, ToolError> {
    let noop = FunctionTool::new(
        FunctionToolConfig::new("noop", ""),
        |_ctx: Context, _in: Empty| async { Ok::<Empty, ToolError>(Empty {}) },
    )
    .expect("functiontool.New() failed");
    let child = llm_agent::new(LlmAgentConfig {
        name: "child".into(),
        model: Some(m),
        tools: vec![noop],
        ..Default::default()
    })
    .expect("llmagent.New() failed");
    let svc = InMemorySessionService::new();
    let sess = svc
        .create(CreateRequest {
            app_name: "testApp".into(),
            user_id: "testUser".into(),
            ..Default::default()
        })
        .await
        .expect("session Create() failed");
    // Go runconfig.ToContext + icontext.NewInvocationContext + agent.NewToolContext(ictx, "", ..).
    let ctx = Context::builder()
        .session(sess)
        .llm_call_limiter(Arc::new(LlmCallLimiter::new(max_llm_calls)))
        .build()
        .with_delta(ContextDelta {
            function_call_id: Some(String::new()),
            ..Default::default()
        });
    AgentTool::new(child, None)
        .as_runnable()
        .expect("agentTool does not implement FunctionTool")
        .run(ctx, json!({"request": "go"}))
        .await
}

/// Go `errors.Is(err, agent.ErrLLMCallsLimitExceeded)`.
fn chain_has_limit_error(err: &ToolError) -> bool {
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(e) = cur {
        if matches!(
            e.downcast_ref::<AgentError>(),
            Some(AgentError::LlmCallsLimitExceeded { .. })
        ) {
            return true;
        }
        cur = e.source();
    }
    false
}

// parity: adapted: Go shrinks the default through t.Setenv(ADK_MAX_LLM_CALLS); std::env::set_var needs unsafe (forbidden), so the real default is used
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_agent_tool_run_forwards_max_llm_calls() {
    // "no limit lets a child past the default finish"
    let need = (run_limits::resolve_max_llm_calls(0) + 1) as usize;
    let m = Arc::new(SlowModel {
        finish_after: need,
        calls: AtomicUsize::new(0),
    });
    let result = run_child_under_budget(m.clone(), -1)
        .await
        .expect("Run() failed");
    assert_eq!(
        result.get("result"),
        Some(&json!("done")),
        "Run() result = {result:?}, want the child's final answer"
    );
    assert_eq!(m.calls.load(Ordering::SeqCst), need, "child model calls");

    // "a small limit bounds the child too"
    let m = Arc::new(SlowModel {
        finish_after: 10,
        calls: AtomicUsize::new(0),
    });
    let err = run_child_under_budget(m.clone(), 2).await.unwrap_err();
    assert!(
        chain_has_limit_error(&err),
        "Run() error = {err}, want it to wrap LlmCallsLimitExceeded"
    );
    assert_eq!(m.calls.load(Ordering::SeqCst), 2, "child model calls");
}
