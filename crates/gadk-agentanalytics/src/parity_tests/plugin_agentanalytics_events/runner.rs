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

//! Part of the port of ADK-Go `plugin/agentanalytics/events_test.go` (owner: Task 11): the 4 Go
//! tests from `TestResolveToolOrigin` to `TestAgentTool_SkipSummarizationLogsFinalAgentResponse` in
//! the Go file's order that run a gadk `Runner` (the exact list is in the 7b-3a plan's contracts,
//! Appendix A).
//!
//! Go's `scriptedLLM` and `sseStreamLLM` are gadk-engine's `MockModel` (`new` /
//! `with_stream`); Go's load-memory tool is a [`FakeTool`] declaring `ToolOriginKind::Local`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Weak};

use futures::FutureExt;
use gadk_agent::{
    BeforeModelCallback, Context, RunConfig, StreamingMode, Tool, ToolError, ToolOriginKind,
};
use gadk_engine::llm::LlmAgentConfig;
use gadk_engine::testutil::MockModel;
use gadk_engine::tool::function::{FunctionTool, FunctionToolConfig};
use gadk_engine::tool::{AgentTool, AgentToolConfig};
use gadk_genai::{Content, FunctionCall, Part};
use gadk_model::{LlmResponse, ModelError};
use serde_json::{Map, Value, json};

use super::{
    FakeTool, check_json, confirmed, function_tool, llm, local_agent, obj, remote_agent,
    require_invocation_completed_ok, text_content,
};
use crate::event_types::{
    EVENT_AGENT_RESPONSE, EVENT_AGENT_STARTING, EVENT_STATE_DELTA, EVENT_TOOL_PAUSED,
    EVENT_TOOL_STARTING,
};
use crate::events::{
    TOOL_ORIGIN_A2A, TOOL_ORIGIN_LOCAL, TOOL_ORIGIN_MCP, TOOL_ORIGIN_SKILL, TOOL_ORIGIN_SUB_AGENT,
    TOOL_ORIGIN_TOOLBOX, TOOL_ORIGIN_TRANSFER_A2A, TOOL_ORIGIN_TRANSFER_AGENT, TOOL_ORIGIN_UNKNOWN,
    tool_origin_of,
};
use crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::{
    MockCtx, event_types, function_call_id_of, json_object_column, lookup, new_test_runner,
    only_row_of_type, rows_of_type, run_turn, setup_test_plugin, until_invocation_end,
};
use crate::plugin::AnalyticsPlugin;
use crate::plugin::rows::{identity_of, runner_agent_of};
use crate::trace::{CallbackCtx, TraceManager};

/// A user tool that holds another tool without exposing it (Go's `exportedWrapperTool`,
/// `confirmationWrapperTool` and `valueWrapperTool`: no `Unwrap`, so the inner tool is never
/// read).
struct HoldingTool {
    name: String,
    _inner: Arc<dyn Tool>,
}

impl Tool for HoldingTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        ""
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Go `unwrapMethodTool` / `valueUnwrapTool`: a user wrapper whose `Unwrap` returns `inner`.
struct UnwrapTool {
    name: String,
    inner: Arc<dyn Tool>,
}

impl Tool for UnwrapTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        ""
    }
    fn unwrap(&self) -> Option<Arc<dyn Tool>> {
        Some(Arc::clone(&self.inner))
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// A tool declaring `kind` (and, for agent tools, `agent`): Go's ADK tool types that
/// `classifyADKToolType` classifies by package and type name.
struct KindTool {
    kind: ToolOriginKind,
    agent: Option<Arc<dyn gadk_agent::Agent>>,
}

impl Tool for KindTool {
    fn name(&self) -> &str {
        "kind_tool"
    }
    fn description(&self) -> &str {
        ""
    }
    fn origin_kind(&self) -> ToolOriginKind {
        self.kind
    }
    fn origin_agent(&self) -> Option<Arc<dyn gadk_agent::Agent>> {
        self.agent.clone()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Go `selfRefUnwrapTool`: `Unwrap` returns the tool itself.
struct SelfRefUnwrapTool {
    me: Weak<SelfRefUnwrapTool>,
    calls: AtomicUsize,
}

impl Tool for SelfRefUnwrapTool {
    fn name(&self) -> &str {
        "self_ref"
    }
    fn description(&self) -> &str {
        ""
    }
    fn unwrap(&self) -> Option<Arc<dyn Tool>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.me.upgrade().map(|t| t as Arc<dyn Tool>)
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Go `mutualUnwrapTool`: `Unwrap` returns the peer.
struct MutualUnwrapTool {
    name: String,
    peer: parking_lot::Mutex<Option<Weak<MutualUnwrapTool>>>,
    calls: Arc<AtomicUsize>,
}

impl Tool for MutualUnwrapTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        ""
    }
    fn unwrap(&self) -> Option<Arc<dyn Tool>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let peer = self.peer.lock().clone()?;
        peer.upgrade().map(|t| t as Arc<dyn Tool>)
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Go `allocUnwrapTool`: `Unwrap` returns a new wrapper each time.
struct AllocUnwrapTool {
    calls: Arc<AtomicUsize>,
}

impl Tool for AllocUnwrapTool {
    fn name(&self) -> &str {
        "alloc_ref"
    }
    fn description(&self) -> &str {
        ""
    }
    fn unwrap(&self) -> Option<Arc<dyn Tool>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Some(Arc::new(AllocUnwrapTool {
            calls: Arc::clone(&self.calls),
        }))
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Go `&model.LLMResponse{Content: textContent("model", text), TurnComplete: true}`.
fn final_text(text: &str) -> LlmResponse {
    LlmResponse {
        content: Some(text_content("model", text)),
        turn_complete: true,
        ..LlmResponse::default()
    }
}

// Go's type-name and package cases (`classifyADKToolType`) are tools declaring each kind, the
// load-memory tool a fake `Local` tool, the generic-type-name case a plain function tool;
// gadk has no nil tool (Go's nil case is dropped) and no log output to check; gadk's
// transfer tool exposes no agents, so its target is found only through the trace manager or
// the context's agent tree.
// parity: adapted: classification is each tool's declared `origin_kind` (R1(b))
#[tokio::test]
async fn test_resolve_tool_origin() {
    let local_agent = local_agent("local_helper", "Local helper agent", Vec::new());
    let remote_a2a_agent = remote_agent("remote_specialist", "Remote A2A specialist");
    let root_with_sub_agents = local_agent_tree(&local_agent, &remote_a2a_agent);

    let tp = setup_test_plugin().await;
    let base_ctx = tp.ctx.clone();
    let real_transfer_tool: Arc<parking_lot::Mutex<Option<Arc<dyn Tool>>>> =
        Arc::new(parking_lot::Mutex::new(None));
    let capture: BeforeModelCallback = {
        let slot = Arc::clone(&real_transfer_tool);
        Arc::new(move |_ctx: Context, req: gadk_model::LlmRequest| {
            let t = req
                .tools
                .get("transfer_to_agent")
                .and_then(|t| t.downcast_ref::<Arc<dyn Tool>>())
                .cloned();
            *slot.lock() = t;
            async move { (req, Ok(Some(final_text("done")))) }.boxed()
        })
    };
    let transfer_router = llm(LlmAgentConfig {
        name: "transfer_router".into(),
        description: "Router".into(),
        model: Some(MockModel::new(Vec::new())),
        sub_agents: vec![Arc::clone(&local_agent), Arc::clone(&remote_a2a_agent)],
        before_model_callbacks: vec![capture],
        ..LlmAgentConfig::default()
    });
    let r = new_test_runner(transfer_router, vec![tp.plugin.clone()]);
    let (_, errs) = run_turn(&r, "u1", "s1", "route", RunConfig::default()).await;
    assert!(
        errs.is_empty(),
        "newTestRunner(transferRouter).Run() error: {errs:?}"
    );
    let real_transfer_tool = real_transfer_tool
        .lock()
        .clone()
        .expect("realTransferTool = nil, want ADK transfer_to_agent tool");

    let fn_tool = function_tool(FunctionToolConfig::new("fn_lookup", "Function tool"));
    let fn_tool_with_mcp_and_agent_generic_args = function_tool(FunctionToolConfig::new(
        "fn_generic_names",
        "Function tool with MCP and Agent in generic type args",
    ));
    let confirmed_fn_tool = function_tool(FunctionToolConfig {
        require_confirmation: true,
        ..FunctionToolConfig::new("fn_confirmed", "Confirmed function tool")
    });
    // Wrap fnTool using the real tool.WithConfirmation toolset wrapper.
    let real_confirmed_local_tool = confirmed("local_ts", Arc::clone(&fn_tool)).await;
    let local_agent_tool = AgentTool::new(Arc::clone(&local_agent), None);
    let remote_a2a_agent_tool = AgentTool::new(Arc::clone(&remote_a2a_agent), None);
    let real_confirmed_a2a_tool = confirmed("a2a_ts", Arc::clone(&remote_a2a_agent_tool)).await;
    let transfer_tool: Arc<dyn Tool> = Arc::new(FakeTool {
        name: "transfer_to_agent".into(),
        description: "Transfer to another agent".into(),
        ..FakeTool::default()
    });
    let load_memory_tool: Arc<dyn Tool> = Arc::new(FakeTool {
        name: "load_memory".into(),
        origin: ToolOriginKind::Local,
        ..FakeTool::default()
    });

    let ap = AnalyticsPlugin {
        config: crate::config::Config::default().apply_defaults(),
        traces: Some(Arc::new(TraceManager::new())),
        ..AnalyticsPlugin::default()
    };
    let ctx_with_hierarchy = MockCtx {
        agent: Some(Arc::clone(&root_with_sub_agents)),
        ..base_ctx.clone()
    };
    // Register rootWithSubAgents in ap.traces via invocationSpan so ap.traces.agentNamed
    // resolves sub-agents.
    {
        let cctx = CallbackCtx::from_invocation(&ctx_with_hierarchy.invocation());
        let traces = ap.traces.as_ref().unwrap();
        traces.invocation_span(&cctx, &identity_of(&cctx), runner_agent_of(&cctx).as_ref());
    }
    let real_tool_ctx = ctx_with_hierarchy.with_function_call("call-tx-1");
    // ctxWithoutTraces shares no invocation ID with ctxWithHierarchy, so ap.traces has no
    // cached agent hierarchy for it.
    let ctx_without_traces = MockCtx {
        agent_name: "test-agent".into(),
        session_id: "sess-id-no-traces".into(),
        invocation_id: "inv-id-no-traces".into(),
        user_id: "user-id".into(),
        ..MockCtx::default()
    };

    let agent_args = |name: &str| obj(json!({ "agent_name": name }));
    // (name, context (None: baseCtx), tool, args, want)
    type OriginCase<'a> = (
        &'a str,
        Option<&'a MockCtx>,
        Arc<dyn Tool>,
        Map<String, Value>,
        &'a str,
    );
    let cases: Vec<OriginCase<'_>> = vec![
        (
            "custom tool without classification returns UNKNOWN",
            None,
            FakeTool::named("custom_unclassified"),
            Map::new(),
            TOOL_ORIGIN_UNKNOWN,
        ),
        (
            "user exported wrapper without Unwrap is not read via reflection",
            None,
            Arc::new(HoldingTool {
                name: "wrap_custom".into(),
                _inner: Arc::clone(&fn_tool),
            }),
            Map::new(),
            TOOL_ORIGIN_UNKNOWN,
        ),
        (
            "real tool.WithConfirmation wrapper preserves inner origin",
            None,
            Arc::clone(&real_confirmed_local_tool),
            Map::new(),
            TOOL_ORIGIN_LOCAL,
        ),
        (
            "real tool.WithConfirmation wrapper around A2A agenttool unwraps to A2A",
            None,
            Arc::clone(&real_confirmed_a2a_tool),
            Map::new(),
            TOOL_ORIGIN_A2A,
        ),
        (
            "user unexported confirmationWrapperTool without Unwrap is not read via reflection",
            None,
            Arc::new(HoldingTool {
                name: "confirm_val".into(),
                _inner: Arc::clone(&fn_tool),
            }),
            Map::new(),
            TOOL_ORIGIN_UNKNOWN,
        ),
        (
            "Unwrap() method wrapper preserves inner origin",
            None,
            Arc::new(UnwrapTool {
                name: "unwrap_custom".into(),
                inner: Arc::clone(&fn_tool),
            }),
            Map::new(),
            TOOL_ORIGIN_LOCAL,
        ),
        (
            "value-receiver Unwrap() method wrapper preserves inner origin",
            None,
            Arc::new(UnwrapTool {
                name: "val_unwrap".into(),
                inner: Arc::clone(&remote_a2a_agent_tool),
            }),
            Map::new(),
            TOOL_ORIGIN_A2A,
        ),
        (
            "functiontool is LOCAL",
            None,
            Arc::clone(&fn_tool),
            Map::new(),
            TOOL_ORIGIN_LOCAL,
        ),
        (
            "functiontool with MCP/Agent in generic type parameters is still LOCAL",
            None,
            Arc::clone(&fn_tool_with_mcp_and_agent_generic_args),
            Map::new(),
            TOOL_ORIGIN_LOCAL,
        ),
        (
            "functiontool with RequireConfirmation unwraps confirmationTool to LOCAL",
            None,
            Arc::clone(&confirmed_fn_tool),
            Map::new(),
            TOOL_ORIGIN_LOCAL,
        ),
        (
            "custom FinishTaskTool is UNKNOWN",
            None,
            FakeTool::named("finish_task"),
            Map::new(),
            TOOL_ORIGIN_UNKNOWN,
        ),
        (
            "agenttool wrapping local agent is SUB_AGENT",
            None,
            Arc::clone(&local_agent_tool),
            Map::new(),
            TOOL_ORIGIN_SUB_AGENT,
        ),
        (
            "agenttool wrapping remoteagent.NewA2A is A2A",
            None,
            Arc::clone(&remote_a2a_agent_tool),
            Map::new(),
            TOOL_ORIGIN_A2A,
        ),
        (
            "transfer_to_agent without args is TRANSFER_AGENT",
            Some(&ctx_with_hierarchy),
            Arc::clone(&transfer_tool),
            Map::new(),
            TOOL_ORIGIN_TRANSFER_AGENT,
        ),
        (
            "transfer_to_agent targeting local sub-agent via traceManager is TRANSFER_AGENT",
            Some(&ctx_with_hierarchy),
            Arc::clone(&transfer_tool),
            agent_args("local_helper"),
            TOOL_ORIGIN_TRANSFER_AGENT,
        ),
        (
            "transfer_to_agent targeting remote A2A sub-agent via traceManager is TRANSFER_A2A",
            Some(&ctx_with_hierarchy),
            Arc::clone(&transfer_tool),
            agent_args("remote_specialist"),
            TOOL_ORIGIN_TRANSFER_A2A,
        ),
        (
            "ADK TransferToAgentTool with supportedAgents and real NewToolContext targeting local agent is TRANSFER_AGENT",
            Some(&real_tool_ctx),
            Arc::clone(&real_transfer_tool),
            agent_args("local_helper"),
            TOOL_ORIGIN_TRANSFER_AGENT,
        ),
        (
            "ADK TransferToAgentTool with supportedAgents and real NewToolContext targeting remote A2A agent is TRANSFER_A2A",
            Some(&real_tool_ctx),
            Arc::clone(&real_transfer_tool),
            agent_args("remote_specialist"),
            TOOL_ORIGIN_TRANSFER_A2A,
        ),
        (
            "transfer_to_agent without supportedAgents under real NewToolContext resolves via traceManager to TRANSFER_A2A without log spam",
            Some(&real_tool_ctx),
            Arc::clone(&transfer_tool),
            agent_args("remote_specialist"),
            TOOL_ORIGIN_TRANSFER_A2A,
        ),
        (
            // Go reads the ADK tool's supportedAgents (findAgentA2AByName); gadk's transfer
            // tool exposes no agents (`origin_agent` is None), and neither the trace manager
            // nor this context's agent knows the target, so the call stays TRANSFER_AGENT.
            "ADK TransferToAgentTool with supportedAgents resolves A2A via tool struct when traces is empty",
            Some(&ctx_without_traces),
            Arc::clone(&real_transfer_tool),
            agent_args("remote_specialist"),
            TOOL_ORIGIN_TRANSFER_AGENT,
        ),
        (
            "loadmemorytool.New() is LOCAL",
            None,
            Arc::clone(&load_memory_tool),
            Map::new(),
            TOOL_ORIGIN_LOCAL,
        ),
        (
            "non-addressable user struct value wrapper without Unwrap is not read via reflection",
            None,
            Arc::new(HoldingTool {
                name: "val_wrap".into(),
                _inner: Arc::clone(&remote_a2a_agent_tool),
            }),
            Map::new(),
            TOOL_ORIGIN_UNKNOWN,
        ),
    ];
    for (name, ctx, tool, args, want) in cases {
        let call_ctx = ctx.unwrap_or(&base_ctx);
        let cctx = CallbackCtx::from_context(&call_ctx.context());
        let got = ap.resolve_tool_origin(&cctx, &identity_of(&cctx), &tool, &args);
        assert_eq!(got, want, "{name}: resolveToolOrigin()");
    }

    // classifyADKToolType exact package and baseName matching (gadk: the kind each ADK tool
    // declares).
    let kind_cases = [
        (
            "loadmemorytool package is LOCAL",
            ToolOriginKind::Local,
            None,
            TOOL_ORIGIN_LOCAL,
        ),
        (
            "mcptoolset package is MCP",
            ToolOriginKind::Mcp,
            None,
            TOOL_ORIGIN_MCP,
        ),
        (
            "workflowinternal TaskAgentTool wrapping local agent is SUB_AGENT",
            ToolOriginKind::SubAgent,
            Some(Arc::clone(&local_agent)),
            TOOL_ORIGIN_SUB_AGENT,
        ),
        (
            "workflowinternal SingleTurnTool wrapping remote agent is A2A",
            ToolOriginKind::SubAgent,
            Some(Arc::clone(&remote_a2a_agent)),
            TOOL_ORIGIN_A2A,
        ),
        (
            "workflowinternal FinishTaskTool is UNKNOWN",
            ToolOriginKind::Unknown,
            None,
            TOOL_ORIGIN_UNKNOWN,
        ),
        (
            "internal TransferToAgentTool is TRANSFER_AGENT",
            ToolOriginKind::TransferAgent,
            None,
            TOOL_ORIGIN_TRANSFER_AGENT,
        ),
        (
            "toolbox in package is TOOLBOX",
            ToolOriginKind::Toolbox,
            None,
            TOOL_ORIGIN_TOOLBOX,
        ),
        (
            "skilltoolset package is SKILL",
            ToolOriginKind::Skill,
            None,
            TOOL_ORIGIN_SKILL,
        ),
    ];
    for (name, kind, agent, want) in kind_cases {
        let t: Arc<dyn Tool> = Arc::new(KindTool { kind, agent });
        assert_eq!(tool_origin_of(&t), want, "{name}: classifyADKToolType");
    }

    // unwrapToolValue cycle detection and 8-hop limit on pointer Unwrap
    let base_cctx = CallbackCtx::from_context(&base_ctx.context());
    let base_id = identity_of(&base_cctx);
    let self_ref = Arc::new_cyclic(|me| SelfRefUnwrapTool {
        me: me.clone(),
        calls: AtomicUsize::new(0),
    });
    let self_ref_tool: Arc<dyn Tool> = self_ref.clone();
    assert_eq!(
        ap.resolve_tool_origin(&base_cctx, &base_id, &self_ref_tool, &Map::new()),
        TOOL_ORIGIN_UNKNOWN,
        "resolveToolOrigin(selfRef)"
    );
    let calls = self_ref.calls.load(Ordering::SeqCst);
    assert!(calls <= 8, "selfRef.calls = {calls}, want <= 8");

    let mutual_calls = Arc::new(AtomicUsize::new(0));
    let w1 = Arc::new(MutualUnwrapTool {
        name: "mutual_1".into(),
        peer: parking_lot::Mutex::new(None),
        calls: Arc::clone(&mutual_calls),
    });
    let w2 = Arc::new(MutualUnwrapTool {
        name: "mutual_2".into(),
        peer: parking_lot::Mutex::new(Some(Arc::downgrade(&w1))),
        calls: Arc::clone(&mutual_calls),
    });
    *w1.peer.lock() = Some(Arc::downgrade(&w2));
    let w1_tool: Arc<dyn Tool> = w1.clone();
    assert_eq!(
        ap.resolve_tool_origin(&base_cctx, &base_id, &w1_tool, &Map::new()),
        TOOL_ORIGIN_UNKNOWN,
        "resolveToolOrigin(mutual)"
    );
    let calls = mutual_calls.load(Ordering::SeqCst);
    assert!(calls <= 8, "mutualCalls = {calls}, want <= 8");

    let alloc_calls = Arc::new(AtomicUsize::new(0));
    let alloc_ref: Arc<dyn Tool> = Arc::new(AllocUnwrapTool {
        calls: Arc::clone(&alloc_calls),
    });
    assert_eq!(
        ap.resolve_tool_origin(&base_cctx, &base_id, &alloc_ref, &Map::new()),
        TOOL_ORIGIN_UNKNOWN,
        "resolveToolOrigin(allocRef)"
    );
    assert_eq!(alloc_calls.load(Ordering::SeqCst), 8, "allocCalls");
}

/// Go `agent.New(agent.Config{Name: "root_router", SubAgents: [local, remote]})`.
fn local_agent_tree(
    local: &Arc<dyn gadk_agent::Agent>,
    remote: &Arc<dyn gadk_agent::Agent>,
) -> Arc<dyn gadk_agent::Agent> {
    local_agent(
        "root_router",
        "Root router",
        vec![Arc::clone(local), Arc::clone(remote)],
    )
}

/// Go's `wait_approval` long-running function tool (`approvalArgs{Item string}`), counting
/// its runs in `runs`.
fn wait_approval_tool(runs: &Arc<AtomicUsize>) -> Arc<dyn Tool> {
    let runs = Arc::clone(runs);
    FunctionTool::new(
        FunctionToolConfig {
            is_long_running: true,
            input_schema: Some(json!({
                "type": "object",
                "properties": {"item": {"type": "string"}},
                "required": ["item"],
                "additionalProperties": false,
            })),
            ..FunctionToolConfig::new("wait_approval", "Waits for external approval")
        },
        move |_ctx, _args: Map<String, Value>| {
            runs.fetch_add(1, Ordering::SeqCst);
            async { Ok::<_, ToolError>(Map::<String, Value>::new()) }
        },
    )
    .expect("functiontool.New(wait_approval)")
}

/// A model response of `parts` (Go `&genai.Content{Role: genai.RoleModel, Parts: parts}`).
fn model_content(parts: Vec<Part>) -> Content {
    Content {
        role: "model".into(),
        parts,
    }
}

/// A function-call part (Go `&genai.Part{FunctionCall: &genai.FunctionCall{…}}`).
fn call_part(id: &str, name: &str, args: Value) -> Part {
    Part {
        function_call: Some(FunctionCall {
            id: id.into(),
            name: name.into(),
            args: obj(args),
            ..FunctionCall::default()
        }),
        ..Part::default()
    }
}

#[tokio::test]
async fn test_sse_streaming_pause_and_state_delta_emitted_once() {
    let tp = setup_test_plugin().await;
    let tool_runs = Arc::new(AtomicUsize::new(0));
    let wait_tool = wait_approval_tool(&tool_runs);
    let call = call_part("call-lr-1", "wait_approval", json!({"item": "cluster-1"}));
    let llm_model = MockModel::with_stream(vec![vec![
        Ok(LlmResponse {
            partial: true,
            content: Some(model_content(vec![call.clone()])),
            ..LlmResponse::default()
        }),
        Ok(LlmResponse {
            partial: false,
            turn_complete: true,
            content: Some(model_content(vec![call])),
            ..LlmResponse::default()
        }),
    ]]);
    let state_set = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let set_state: BeforeModelCallback = {
        let state_set = Arc::clone(&state_set);
        Arc::new(move |ctx: Context, req: gadk_model::LlmRequest| {
            let mut res = Ok(None);
            if !state_set.swap(true, Ordering::SeqCst)
                && let Err(err) = ctx.state().set("user:model_calls", json!(1))
            {
                res = Err(gadk_agent::AgentError::Other(Box::new(err)));
            }
            async move { (req, res) }.boxed()
        })
    };
    let root = llm(LlmAgentConfig {
        name: "sse_root".into(),
        model: Some(llm_model),
        tools: vec![wait_tool],
        before_model_callbacks: vec![set_state],
        ..LlmAgentConfig::default()
    });
    let r = new_test_runner(root, vec![tp.plugin.clone()]);
    let cfg = RunConfig {
        streaming_mode: StreamingMode::Sse,
        ..RunConfig::default()
    };
    let (_, errs) = run_turn(&r, "u1", "s1", "deploy", cfg).await;
    assert!(errs.is_empty(), "r.Run() error: {errs:?}");

    let rows = tp.collect_rows(until_invocation_end).await;
    require_invocation_completed_ok(&rows);
    assert_eq!(
        tool_runs.load(Ordering::SeqCst),
        1,
        "long-running tool runs"
    );
    assert_eq!(
        rows_of_type(&rows, EVENT_STATE_DELTA).len(),
        1,
        "STATE_DELTA rows; event types: {:?}",
        event_types(&rows)
    );
    let paused_rows = rows_of_type(&rows, EVENT_TOOL_PAUSED);
    assert_eq!(
        paused_rows.len(),
        1,
        "TOOL_PAUSED rows; event types: {:?}",
        event_types(&rows)
    );
    let agent_start = only_row_of_type(&rows, EVENT_AGENT_STARTING);
    assert_eq!(
        paused_rows[0].get("span_id"),
        agent_start.get("span_id"),
        "TOOL_PAUSED span_id, want AGENT_STARTING span_id"
    );
    assert_eq!(
        function_call_id_of(&paused_rows[0]),
        "call-lr-1",
        "TOOL_PAUSED function_call_id"
    );
}

#[tokio::test]
async fn test_sse_streaming_aborted_stream_does_not_emit_pause() {
    let tp = setup_test_plugin().await;
    let tool_runs = Arc::new(AtomicUsize::new(0));
    let wait_tool = wait_approval_tool(&tool_runs);
    let llm_model = MockModel::with_stream(vec![vec![
        Ok(LlmResponse {
            partial: true,
            content: Some(model_content(vec![call_part(
                "call-lr-abort",
                "wait_approval",
                json!({"item": "cluster-1"}),
            )])),
            ..LlmResponse::default()
        }),
        Err(ModelError::Other("stream reset by peer".into())),
    ]]);
    let root = llm(LlmAgentConfig {
        name: "sse_abort_root".into(),
        model: Some(llm_model),
        tools: vec![wait_tool],
        ..LlmAgentConfig::default()
    });
    let r = new_test_runner(root, vec![tp.plugin.clone()]);
    let cfg = RunConfig {
        streaming_mode: StreamingMode::Sse,
        ..RunConfig::default()
    };
    let (_, errs) = run_turn(&r, "u1", "s1", "deploy", cfg).await;
    assert!(!errs.is_empty(), "expected r.Run() to yield stream error");

    let rows = tp.collect_rows(until_invocation_end).await;
    assert_eq!(
        tool_runs.load(Ordering::SeqCst),
        0,
        "long-running tool runs on aborted stream"
    );
    assert!(
        rows_of_type(&rows, EVENT_TOOL_STARTING).is_empty(),
        "TOOL_STARTING rows on aborted stream; event types: {:?}",
        event_types(&rows)
    );
    assert!(
        rows_of_type(&rows, EVENT_TOOL_PAUSED).is_empty(),
        "TOOL_PAUSED rows on aborted stream; event types: {:?}",
        event_types(&rows)
    );
}

#[tokio::test]
async fn test_agent_tool_skip_summarization_logs_final_agent_response() {
    let tp = setup_test_plugin().await;
    let sub_agent = llm(LlmAgentConfig {
        name: "sub_agent".into(),
        description: "Answers delegate questions".into(),
        model: Some(MockModel::new(vec![Ok(final_text(
            "sub-agent final answer",
        ))])),
        ..LlmAgentConfig::default()
    });
    let sub_tool = AgentTool::new(
        sub_agent,
        Some(AgentToolConfig {
            skip_summarization: true,
        }),
    );
    assert!(
        sub_tool.display_result_on_skip_summarization(),
        "agenttool displays its result on skip summarization"
    );
    let root_agent = llm(LlmAgentConfig {
        name: "root_agent".into(),
        tools: vec![sub_tool],
        model: Some(MockModel::new(vec![Ok(LlmResponse {
            content: Some(model_content(vec![call_part(
                "",
                "sub_agent",
                json!({"request": "help me"}),
            )])),
            ..LlmResponse::default()
        })])),
        ..LlmAgentConfig::default()
    });
    let r = new_test_runner(root_agent, vec![tp.plugin.clone()]);
    let (_, errs) = run_turn(&r, "u1", "s1", "ask sub-agent", RunConfig::default()).await;
    assert!(errs.is_empty(), "r.Run() error: {errs:?}");

    let rows = tp.collect_rows(until_invocation_end).await;
    require_invocation_completed_ok(&rows);
    let mut found_skip_summary_response = false;
    for row in rows_of_type(&rows, EVENT_AGENT_RESPONSE) {
        let attrs = json_object_column(&row, "attributes");
        if lookup(&attrs, &["adk", "skip_summarization"]) == Some(&Value::Bool(true)) {
            found_skip_summary_response = true;
            check_json(
                "SkipSummarization AGENT_RESPONSE content",
                &json_object_column(&row, "content"),
                r#"{"response":"text: 'sub-agent final answer'"}"#,
            );
        }
    }
    assert!(
        found_skip_summary_response,
        "no AGENT_RESPONSE row emitted for SkipSummarization agenttool display event; event types: {:?}",
        event_types(&rows)
    );
}
