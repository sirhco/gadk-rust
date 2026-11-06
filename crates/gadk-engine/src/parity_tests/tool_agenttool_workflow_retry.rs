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

//! Port of Go tool/agenttool/workflow_retry_test.go.

use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering::SeqCst};
use std::time::Duration;

use gadk_agent::{AgentError, Context};
use gadk_genai::Content;
use gadk_session::{CreateRequest, Event, InMemorySessionService, SessionService};
use serde_json::{Value, json};

use crate::agents::workflow_agent::{self, WorkflowAgentConfig};
use crate::tool::AgentTool;
use crate::workflow::{
    Edge, Emit, NodeConfig, RetryConfig, RunNodeOptions, START, default_retry_config, dynamic_node,
    function_node, run_node,
};

const FAILURE: &str = "scripted failure";

fn err_chain(e: &(dyn std::error::Error + 'static)) -> String {
    let mut s = e.to_string();
    let mut cur = e.source();
    while let Some(c) = cur {
        s.push_str(" | ");
        s.push_str(&c.to_string());
        cur = c.source();
    }
    s
}

/// Port of Go createToolContext (agent_tool_test.go): a tool context over a fresh in-memory session.
async fn create_tool_context() -> Context {
    let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let session = svc
        .create(CreateRequest {
            app_name: "testApp".into(),
            user_id: "testUser".into(),
            session_id: Some("testSession".into()),
            state: None,
        })
        .await
        .unwrap();
    // Go NewToolContext: own actions via the function_call_id delta (Task 4 apply()).
    Context::builder()
        .session(session)
        .session_service(svc)
        .build()
        .with_delta(gadk_agent::ContextDelta {
            function_call_id: Some(String::new()),
            ..Default::default()
        })
}

#[tokio::test]
async fn test_agent_tool_workflow_recovered_failure() {
    for mode in ["retry", "fallback", "exhausted"] {
        let calls = Arc::new(AtomicI32::new(0));
        let c = calls.clone();
        let flaky = function_node(
            "flaky",
            move |_ctx: Context, _in: Value| {
                let n = c.fetch_add(1, SeqCst) + 1;
                async move {
                    if n == 1 || mode != "retry" {
                        Err::<String, _>(AgentError::other(FAILURE))
                    } else {
                        Ok("ok".to_string())
                    }
                }
            },
            NodeConfig {
                retry_config: Some(RetryConfig {
                    max_attempts: 2,
                    initial_delay: Duration::from_millis(1),
                    jitter: 0.0,
                    ..default_retry_config()
                }),
                ..Default::default()
            },
        );
        let node = if mode == "fallback" {
            let c = calls.clone();
            let child = function_node(
                "child",
                move |_ctx: Context, _in: Value| {
                    c.fetch_add(1, SeqCst);
                    async { Err::<String, _>(AgentError::other(FAILURE)) }
                },
                NodeConfig::default(),
            );
            dynamic_node(
                "parent",
                move |ctx: Context, input: Value, _emit: Emit| {
                    let child = child.clone();
                    async move {
                        match run_node(&ctx, child, input, RunNodeOptions::default()).await {
                            Err(e) if err_chain(&e).contains(FAILURE) => Ok("ok".to_string()),
                            _ => Err(AgentError::other("expected child failure")),
                        }
                    }
                },
                NodeConfig::default(),
            )
        } else {
            flaky
        };
        // AgentTool consumes content, not the workflow's typed output.
        let report = dynamic_node(
            "report",
            |ctx: Context, input: Value, emit: Emit| async move {
                if input != json!("ok") {
                    return Err(AgentError::other("unexpected recovered result"));
                }
                let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
                ev.response.content = Some(Content::from_text("ok", "model"));
                emit(ev).await?;
                Ok(Value::Null)
            },
            NodeConfig::default(),
        );
        let wf_agent = workflow_agent::new(WorkflowAgentConfig {
            name: "wf".into(),
            edges: vec![
                Edge {
                    from: START.clone(),
                    to: node.clone(),
                    route: None,
                },
                Edge {
                    from: node,
                    to: report,
                    route: None,
                },
            ],
            ..Default::default()
        })
        .unwrap_or_else(|e| panic!("{mode}: workflow construction failed: {e}"));
        let tool = AgentTool::new(wf_agent, None);
        let runnable = tool
            .as_runnable()
            .expect("agent tool did not implement toolinternal.FunctionTool");
        let result = runnable
            .run(create_tool_context().await, json!({"request": "start"}))
            .await;
        if mode == "exhausted" {
            let e = result.expect_err("exhausted retry did not preserve the final error");
            assert!(
                err_chain(&e).contains(FAILURE) && calls.load(SeqCst) == 2,
                "exhausted retry did not preserve the final error: {}",
                err_chain(&e)
            );
            continue;
        }
        let result = result.unwrap_or_else(|e| {
            panic!("{mode}: recovered workflow failed its agent tool call: {e}")
        });
        assert_eq!(
            Value::Object(result),
            json!({"result": "ok"}),
            "{mode}: recovered workflow result has unexpected shape"
        );
        let want_calls = if mode == "fallback" { 1 } else { 2 };
        assert_eq!(
            calls.load(SeqCst),
            want_calls,
            "{mode}: agent tool stopped consuming before recovery"
        );
    }
}
