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

//! Rust-only tests for the tool helpers: `tool_from_request`, filter and confirmation wrappers.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use gadk_agent::{
    Context, ContextDelta, InvocationContext, ReadonlyContext, RunnableTool, Tool, ToolError,
    Toolset, allowed_tools_predicate, filter_toolset, pack_tool, tool_from_request,
    with_confirmation,
};
use gadk_model::LlmRequest;
use gadk_session::ToolConfirmation;
use serde_json::{Map, Value, json};

struct Echo {
    name: &'static str,
    calls: AtomicUsize,
}
impl Tool for Echo {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "echo"
    }
    fn as_runnable(&self) -> Option<&dyn RunnableTool> {
        Some(self)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
#[async_trait]
impl RunnableTool for Echo {
    async fn run(&self, _ctx: Context, args: Value) -> Result<Map<String, Value>, ToolError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"echo": args}).as_object().cloned().unwrap())
    }
}

/// A tool with neither the runnable nor the streaming capability.
struct Inert;
impl Tool for Inert {
    fn name(&self) -> &str {
        "inert"
    }
    fn description(&self) -> &str {
        "inert"
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct Static(Vec<Arc<dyn Tool>>);
#[async_trait]
impl Toolset for Static {
    fn name(&self) -> &str {
        "static"
    }
    async fn tools(&self, _ctx: &ReadonlyContext) -> Result<Vec<Arc<dyn Tool>>, ToolError> {
        Ok(self.0.clone())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
fn echo(name: &'static str) -> Arc<dyn Tool> {
    Arc::new(Echo {
        name,
        calls: AtomicUsize::new(0),
    })
}
fn tool_ctx(confirmation: Option<ToolConfirmation>) -> Context {
    InvocationContext::builder()
        .build()
        .with_delta(ContextDelta {
            function_call_id: Some("call-1".into()),
            tool_confirmation: Some(confirmation),
            ..Default::default()
        })
}
async fn first_tool(ts: &Arc<dyn Toolset>) -> Arc<dyn Tool> {
    ts.tools(
        InvocationContext::builder()
            .build_invocation()
            .as_readonly(),
    )
    .await
    .unwrap()
    .remove(0)
}

#[test]
fn tool_from_request_round_trip() {
    let t = echo("a");
    let mut req = LlmRequest::default();
    pack_tool(&mut req, &t).unwrap();
    let back = tool_from_request(&req, "a").unwrap();
    assert_eq!(
        Arc::as_ptr(&back).cast::<()>(),
        Arc::as_ptr(&t).cast::<()>()
    );
    assert!(tool_from_request(&req, "missing").is_none());
    // The D3 path through tool.rs (spec R5).
    assert!(gadk_agent::tool::tool_from_request(&req, "a").is_some());
}

#[tokio::test]
async fn filter_toolset_keeps_allowed_tools_only() {
    let ts = filter_toolset(
        Arc::new(Static(vec![echo("a"), echo("b"), echo("c")])),
        allowed_tools_predicate(&["a", "c"]),
    );
    assert_eq!(ts.name(), "static");
    let ctx = InvocationContext::builder().build_invocation();
    let names: Vec<String> = ts
        .tools(ctx.as_readonly())
        .await
        .unwrap()
        .iter()
        .map(|t| t.name().to_string())
        .collect();
    assert_eq!(names, ["a", "c"]);
}

#[tokio::test]
async fn with_confirmation_requests_rejects_and_runs() {
    let inner = echo("danger");
    let ts = with_confirmation(Arc::new(Static(vec![inner.clone()])), true, None);
    let wrapped = first_tool(&ts).await;
    let runnable = wrapped.as_runnable().unwrap();
    // No confirmation yet: request one and stop.
    let ctx = tool_ctx(None);
    assert!(matches!(
        runnable.run(ctx.clone(), json!({})).await,
        Err(ToolError::ConfirmationRequired)
    ));
    let actions = ctx.actions();
    assert!(actions.skip_summarization);
    assert_eq!(
        actions.requested_tool_confirmations["call-1"].hint,
        "Please approve or reject the tool call danger() by responding with a FunctionResponse with an expected ToolConfirmation payload."
    );
    // Rejected.
    let rejected = tool_ctx(Some(ToolConfirmation {
        confirmed: false,
        ..Default::default()
    }));
    assert!(matches!(
        runnable.run(rejected, json!({})).await,
        Err(ToolError::ConfirmationRejected)
    ));
    // Approved: the inner tool runs.
    let approved = tool_ctx(Some(ToolConfirmation {
        confirmed: true,
        ..Default::default()
    }));
    assert_eq!(
        runnable.run(approved, json!({"x": 1})).await.unwrap()["echo"],
        json!({"x": 1})
    );
}

#[tokio::test]
async fn with_confirmation_provider_overrides_flag() {
    // require_confirmation = true, but the provider says no: the tool runs immediately.
    let ts = with_confirmation(
        Arc::new(Static(vec![echo("danger")])),
        true,
        Some(Arc::new(|_name: &str, _args: &Value| false)),
    );
    let wrapped = first_tool(&ts).await;
    let ctx = tool_ctx(None);
    let out = wrapped
        .as_runnable()
        .unwrap()
        .run(ctx.clone(), json!({"y": 2}))
        .await
        .unwrap();
    assert_eq!(out["echo"], json!({"y": 2}));
    assert!(ctx.actions().requested_tool_confirmations.is_empty());

    // require_confirmation = false, but the provider says yes for "danger": confirmation is requested.
    let ts = with_confirmation(
        Arc::new(Static(vec![echo("danger")])),
        false,
        Some(Arc::new(|name: &str, _args: &Value| name == "danger")),
    );
    let wrapped = first_tool(&ts).await;
    let ctx = tool_ctx(None);
    assert!(matches!(
        wrapped
            .as_runnable()
            .unwrap()
            .run(ctx.clone(), json!({}))
            .await,
        Err(ToolError::ConfirmationRequired)
    ));
    let actions = ctx.actions();
    assert!(actions.skip_summarization);
    assert!(actions.requested_tool_confirmations.contains_key("call-1"));
}

#[tokio::test]
async fn with_confirmation_passes_non_runnable_tools_through() {
    let inert: Arc<dyn Tool> = Arc::new(Inert);
    let ts = with_confirmation(
        Arc::new(Static(vec![inert.clone(), echo("danger")])),
        true,
        None,
    );
    let tools = ts
        .tools(
            InvocationContext::builder()
                .build_invocation()
                .as_readonly(),
        )
        .await
        .unwrap();
    assert_eq!(
        Arc::as_ptr(&tools[0]).cast::<()>(),
        Arc::as_ptr(&inert).cast::<()>()
    );
    // The runnable tool is wrapped (a new allocation), and the wrapper hides defers_response.
    assert!(tools[1].as_runnable().is_some());
    assert!(!tools[1].defers_response());
}
