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

//! Port of ADK-Go `plugin/agentanalytics/events_test.go` (ledger `docs/parity/7b-3a.md`): 18
//! Go tests, split into [`direct`] (tests that call the plugin's callbacks and helpers
//! directly) and [`runner`] (tests that run agents through a gadk `Runner`). Go's load-memory tool
//! is a fake tool reporting `ToolOriginKind::Local` (gadk-tools is a `gadk`-only dependency;
//! adapted).
//!
//! The Go test helpers both halves share are here: Go `textContent`, `checkJSON` /
//! `checkJSONAt`, `fakeTool`, the callback calls of Go's `callbackErrors` pattern (each callback
//! result is checked as it returns; Go collects the errors and fails at `errs.check(t)`), and
//! `requireInvocationCompletedOK` (`trace_manager_test.go`; `text_content` and it are shared
//! with the `late_callbacks_test.go` port).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod direct;
mod runner;

use std::any::Any;
use std::sync::Arc;

use gadk_agent::{
    Agent, AgentConfig, AgentError, AgentState, AgentType, ReadonlyContext, Tool, ToolError,
    Toolset,
};
use gadk_engine::llm::LlmAgentConfig;
use gadk_engine::tool::function::{FunctionTool, FunctionToolConfig};
use gadk_genai::Content;
use gadk_session::Event;
use serde_json::{Map, Value};

use crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::{
    FakeTool, MockCtx, TestPlugin, check_json as try_check_json, check_json_at, text_content,
};

/// The JSON object `v` (Go's `map[string]any` literals).
fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        other => panic!("not a JSON object: {other}"),
    }
}

/// Go `checkJSON(t, name, got, want)`.
fn check_json(name: &str, got: &Value, want: &str) {
    if let Err(diff) = try_check_json(name, got, want) {
        panic!("{diff}");
    }
}

/// Go `p.OnEventCallback()(ctx, ev)`, checked.
async fn on_event(tp: &TestPlugin, ctx: &MockCtx, ev: Event) {
    let cb = tp.plugin.on_event_callback().expect("on-event callback");
    cb(ctx.invocation(), ev).await.expect("OnEventCallback");
}

/// Go `p.OnUserMessageCallback()(ctx, msg)`, checked.
async fn on_user_message(tp: &TestPlugin, ctx: &MockCtx, msg: Content) {
    let cb = tp
        .plugin
        .on_user_message_callback()
        .expect("on-user-message callback");
    cb(ctx.invocation(), msg)
        .await
        .expect("OnUserMessageCallback");
}

/// Go `p.BeforeRunCallback()(ctx)`, checked.
async fn before_run(tp: &TestPlugin, ctx: &MockCtx) {
    let cb = tp
        .plugin
        .before_run_callback()
        .expect("before-run callback");
    cb(ctx.invocation()).await.expect("BeforeRunCallback");
}

/// Go `p.AfterRunCallback()(ctx)`.
async fn after_run(tp: &TestPlugin, ctx: &MockCtx) {
    let cb = tp.plugin.after_run_callback().expect("after-run callback");
    cb(ctx.invocation()).await;
}

/// Go `p.BeforeToolCallback()(ctx, t, args)`, checked.
///
/// The tool callbacks take the ADK context itself: a test builds it once from its `MockCtx`
/// (`MockCtx::context`) and passes the same context to every callback of a call, as Go passes
/// the same `mCtx` (each `MockCtx::context()` is a new context of a new activation, and the
/// plugin pairs a call's rows by activation).
async fn before_tool(
    tp: &TestPlugin,
    ctx: &gadk_agent::Context,
    t: &Arc<dyn Tool>,
    args: &Map<String, Value>,
) {
    let cb = tp
        .plugin
        .before_tool_callback()
        .expect("before-tool callback");
    cb(ctx.clone(), Arc::clone(t), args.clone())
        .await
        .1
        .expect("BeforeToolCallback");
}

/// Go `p.AfterToolCallback()(ctx, t, args, result, err)`, checked (Go's nil result is an
/// empty map).
async fn after_tool(
    tp: &TestPlugin,
    ctx: &gadk_agent::Context,
    t: &Arc<dyn Tool>,
    args: &Map<String, Value>,
    result: Map<String, Value>,
    err: Option<Arc<ToolError>>,
) {
    let cb = tp
        .plugin
        .after_tool_callback()
        .expect("after-tool callback");
    cb(ctx.clone(), Arc::clone(t), args.clone(), result, err)
        .await
        .expect("AfterToolCallback");
}

/// Go `p.OnToolErrorCallback()(ctx, t, args, err)`.
async fn on_tool_error(
    tp: &TestPlugin,
    ctx: &gadk_agent::Context,
    t: &Arc<dyn Tool>,
    args: &Map<String, Value>,
    err: Arc<ToolError>,
) -> Result<Option<Map<String, Value>>, AgentError> {
    let cb = tp
        .plugin
        .on_tool_error_callback()
        .expect("on-tool-error callback");
    cb(ctx.clone(), Arc::clone(t), args.clone(), err).await
}

/// Go `agent.New(agent.Config{Name: name, Description: description})`.
fn local_agent(name: &str, description: &str, sub_agents: Vec<Arc<dyn Agent>>) -> Arc<dyn Agent> {
    gadk_agent::new_agent(AgentConfig {
        name: name.into(),
        description: description.into(),
        sub_agents,
        ..AgentConfig::default()
    })
    .expect("agent")
}

/// Go's remote A2A agent: `agent.New(…)` with its `AgentType` set to `"RemoteAgent"` by
/// reflection (gadk: an agent built with `AgentType::RemoteAgent`).
fn remote_agent(name: &str, description: &str) -> Arc<dyn Agent> {
    gadk_agent::new_with_state(
        AgentConfig {
            name: name.into(),
            description: description.into(),
            ..AgentConfig::default()
        },
        AgentState {
            agent_type: AgentType::RemoteAgent,
            config: None,
            on_before_agent_callback_event: None,
            live: None,
        },
    )
    .expect("remote agent")
}

/// Go `llmagent.New(cfg)`.
fn llm(cfg: LlmAgentConfig) -> Arc<dyn Agent> {
    gadk_engine::llm_agent::new(cfg).expect("llm agent")
}

/// Go `functiontool.New(functiontool.Config{…}, func(agent.Context, struct{}) (struct{},
/// error))`: a function tool whose arguments and result are JSON objects.
fn function_tool(cfg: FunctionToolConfig) -> Arc<dyn Tool> {
    FunctionTool::new(cfg, |_ctx, _args: Map<String, Value>| async {
        Ok::<_, ToolError>(Map::<String, Value>::new())
    })
    .expect("function tool")
}

/// Go `singleToolset`: a toolset of one tool.
struct SingleToolset {
    name: String,
    tool: Arc<dyn Tool>,
}

#[async_trait::async_trait]
impl Toolset for SingleToolset {
    fn name(&self) -> &str {
        &self.name
    }
    async fn tools(&self, _ctx: &ReadonlyContext) -> Result<Vec<Arc<dyn Tool>>, ToolError> {
        Ok(vec![Arc::clone(&self.tool)])
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Go `tool.WithConfirmation(singleToolset{name, t}, true, nil).Tools(nil)`: the one wrapped
/// tool.
async fn confirmed(name: &str, t: Arc<dyn Tool>) -> Arc<dyn Tool> {
    let ts = gadk_agent::toolset::with_confirmation(
        Arc::new(SingleToolset {
            name: name.into(),
            tool: t,
        }),
        true,
        None,
    );
    let ctx = MockCtx::new().invocation();
    let mut tools = ts.tools(ctx.as_readonly()).await.expect("tools");
    assert_eq!(tools.len(), 1, "tool.WithConfirmation.Tools(): want 1 tool");
    tools.remove(0)
}
