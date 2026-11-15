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

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Port of ADK-Go runner/run_node_test.go.

use std::collections::BTreeMap;
use std::sync::Arc;

use futures::StreamExt;
use gadk_agent::{
    Agent, AgentError, BeforeAgentCallback, Context, EventStream, RunConfig, ToolError,
};
use gadk_engine::agents::workflow_agent::{self, WorkflowAgentConfig};
use gadk_engine::llm::LlmAgentConfig;
use gadk_engine::llm::agent as llm_agent;
use gadk_engine::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use gadk_engine::tool::{FunctionTool, FunctionToolConfig};
use gadk_engine::workflow::{
    AgentNode, Edge, Node, NodeConfig, NodeStatus, RunState, START, Workflow, WorkflowOptions,
    chain, function_node,
};
use gadk_genai::{Content, FunctionCall, FunctionResponse, Part, Schema, Type};
use gadk_model::Llm;
use gadk_platform::Platform;
use gadk_session::{
    CreateRequest, Event, GetRequest, InMemorySessionService, Session, SessionService,
};
use parking_lot::Mutex;
use serde_json::{Value, json};

mod workflow_support;
use workflow_support::ScriptedModel;

const NODE_TEST_APP: &str = "node_test_app";
const NODE_TEST_USER: &str = "u";
const NODE_TEST_SESSION: &str = "s";

fn start() -> Arc<dyn Node> {
    Arc::clone(&START)
}

fn edge(from: &Arc<dyn Node>, to: &Arc<dyn Node>) -> Edge {
    Edge {
        from: from.clone(),
        to: to.clone(),
        route: None,
    }
}

fn user_text(text: &str) -> Content {
    Content::from_text(text, "user")
}

fn model_text(text: &str) -> Content {
    Content::from_text(text, "model")
}

fn model_call(name: &str, args: Value) -> Content {
    Content::from_function_call(
        FunctionCall {
            name: name.into(),
            args: args.as_object().cloned().unwrap_or_default(),
            ..Default::default()
        },
        "model",
    )
}

fn function_response_content(id: &str, name: &str, response: Value) -> Content {
    Content::from_function_response(
        FunctionResponse {
            id: id.into(),
            name: name.into(),
            response: response.as_object().cloned().unwrap_or_default(),
            ..Default::default()
        },
        "user",
    )
}

/// Go `for ev, err := range r.Run(...) { if err != nil { t.Fatal(err) } }`: every event.
async fn drain(s: EventStream) -> Vec<Event> {
    futures::TryStreamExt::try_collect(s)
        .await
        .expect("Run() error")
}

/// Every event and every error, in order (Go loops that record `err` and continue).
async fn run_all(mut s: EventStream) -> (Vec<Event>, Vec<AgentError>) {
    let (mut events, mut errs) = (Vec::new(), Vec::new());
    while let Some(item) = s.next().await {
        match item {
            Ok(ev) => events.push(ev),
            Err(e) => errs.push(e),
        }
    }
    (events, errs)
}

/// Concatenated text of an event's parts.
fn event_text(ev: &Event) -> String {
    ev.response
        .content
        .as_ref()
        .map(|c| c.parts.iter().map(|p| p.text.as_str()).collect())
        .unwrap_or_default()
}

/// True when some part of some event's content has exactly `text`.
fn saw_text(events: &[Event], text: &str) -> bool {
    events.iter().any(|e| {
        e.response
            .content
            .as_ref()
            .is_some_and(|c| c.parts.iter().any(|p| p.text == text))
    })
}

/// The non-null outputs of `events`, in order.
fn outputs(events: &[Event]) -> Vec<Value> {
    events.iter().filter_map(|e| e.output.clone()).collect()
}

async fn new_node_test_session(svc: &Arc<dyn SessionService>) {
    svc.create(CreateRequest {
        app_name: NODE_TEST_APP.into(),
        user_id: NODE_TEST_USER.into(),
        session_id: Some(NODE_TEST_SESSION.into()),
        state: None,
    })
    .await
    .expect("sessionService.Create()");
}

async fn get_session(svc: &Arc<dyn SessionService>) -> Arc<dyn Session> {
    svc.get(GetRequest {
        app_name: NODE_TEST_APP.into(),
        user_id: NODE_TEST_USER.into(),
        session_id: NODE_TEST_SESSION.into(),
        ..Default::default()
    })
    .await
    .expect("Get()")
}

/// Go `newNodeTestRunner`.
fn new_node_test_runner(agent: Arc<dyn Agent>, svc: Arc<dyn SessionService>) -> Runner {
    Runner::new(RunnerConfig {
        app_name: NODE_TEST_APP.into(),
        agent,
        session_service: svc,
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig::default(),
        auto_create_session: false,
        platform: Platform::default(),
        compaction: None,
    })
    .expect("runner.New()")
}

/// A fresh in-memory service with the test session created, and a runner over `agent`.
async fn node_test_setup(agent: Arc<dyn Agent>) -> (Arc<dyn SessionService>, Runner) {
    let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    new_node_test_session(&svc).await;
    let r = new_node_test_runner(agent, svc.clone());
    (svc, r)
}

fn run(r: &Runner, msg: Content) -> EventStream {
    r.run(
        NODE_TEST_USER,
        NODE_TEST_SESSION,
        Some(msg),
        RunConfig::default(),
        RunOptions::default(),
    )
}

fn run_with(r: &Runner, msg: Content, opts: RunOptions) -> EventStream {
    r.run(
        NODE_TEST_USER,
        NODE_TEST_SESSION,
        Some(msg),
        RunConfig::default(),
        opts,
    )
}

fn state_delta() -> RunOptions {
    RunOptions {
        state_delta: Some(
            json!({"test_state": "must_change"})
                .as_object()
                .cloned()
                .unwrap(),
        ),
        ..Default::default()
    }
}

/// Go `workflowagent.New(workflowagent.Config{Name, Edges})`.
fn workflow_agent_of(name: &str, edges: Vec<Edge>) -> Arc<dyn Agent> {
    workflow_agent::new(WorkflowAgentConfig {
        name: name.into(),
        edges,
        ..Default::default()
    })
    .expect("workflowagent.New()")
}

fn scripted_agent(name: &str, text: &str) -> Arc<dyn Agent> {
    llm_agent::new(LlmAgentConfig {
        name: name.into(),
        model: Some(ScriptedModel::new(vec![model_text(text)]) as Arc<dyn Llm>),
        ..Default::default()
    })
    .expect("llmagent.New()")
}

/// Go `runStateForAgent`: rebuild a single-node workflow named "<app>/<agent>" and
/// reconstruct the paused RunState from session history (invocation id ""). `None` is Go's nil state.
async fn run_state_for_agent(
    svc: &Arc<dyn SessionService>,
    a: &Arc<dyn Agent>,
) -> Option<RunState> {
    let session = get_session(svc).await;
    let node: Arc<dyn Node> =
        AgentNode::new(a.clone(), NodeConfig::default()).expect("workflow.NewAgentNode()");
    let wf = Workflow::new(
        &format!("{NODE_TEST_APP}/{}", a.name()),
        vec![edge(&start(), &node)],
        WorkflowOptions::default(),
    )
    .expect("workflow.New()");
    wf.reconstruct_run_state(session.as_ref(), "")
        .expect("ReconstructRunState()")
}

/// Go `hasWaitingInterrupt`; a nil state has no waiting interrupt.
fn has_waiting_interrupt(state: Option<&RunState>, id: &str) -> bool {
    state.is_some_and(|st| {
        st.nodes
            .values()
            .any(|ns| ns.status == NodeStatus::Waiting && ns.interrupts.iter().any(|i| i == id))
    })
}

/// Go `sessionOutputs`.
fn session_outputs(sess: &dyn Session) -> Vec<Value> {
    sess.events().into_iter().filter_map(|e| e.output).collect()
}

/// Go `sessionHasUserText`.
fn session_has_user_text(sess: &dyn Session, want: &str) -> bool {
    sess.events().iter().any(|e| {
        e.author == "user"
            && e.response
                .content
                .as_ref()
                .is_some_and(|c| c.parts.iter().any(|p| p.text == want))
    })
}

/// Go `newUpperWorkflowAgent`: a workflow agent over one function node upper-casing its input.
fn new_upper_workflow_agent(name: &str) -> Arc<dyn Agent> {
    let upper = function_node::<String, String, _, _>(
        &format!("{name}_upper"),
        |_ctx: Context, input: String| async move { Ok(input.to_uppercase()) },
        NodeConfig::default(),
    );
    workflow_agent_of(name, chain(&[start(), upper]))
}

/// Go `newPermissiveAgent`.
fn new_permissive_agent() -> Arc<dyn Agent> {
    scripted_agent("approver", "handled")
}

#[tokio::test]
async fn test_runner_llm_agent_fresh_turn() {
    let (_svc, r) = node_test_setup(scripted_agent("greeter", "hello there")).await;

    let mut got_text = String::new();
    let mut saw_node_info = false;
    for ev in drain(run(&r, user_text("hi"))).await {
        // Stamped by the node runtime with the agent's name as its path.
        if let Some(ni) = &ev.node_info
            && (ni.path == "greeter" || ni.path.is_empty())
        {
            saw_node_info = true;
        }
        got_text.push_str(&event_text(&ev));
    }
    assert_eq!(got_text, "hello there", "model text");
    assert!(saw_node_info, "expected an event stamped with NodeInfo");
}

#[tokio::test]
async fn test_runner_llm_agent_yield_user_message() {
    let (_svc, r) = node_test_setup(scripted_agent("greeter", "hi back")).await;

    let events = drain(run_with(
        &r,
        user_text("hi"),
        RunOptions {
            yield_user_message: true,
            ..Default::default()
        },
    ))
    .await;
    // Go keeps overwriting firstAuthor while it is "": the first non-empty author.
    let first_author = events
        .iter()
        .map(|e| e.author.as_str())
        .find(|a| !a.is_empty())
        .unwrap_or("");
    assert!(
        events.iter().any(|e| e.author == "user"),
        "expected a user message event to be yielded"
    );
    assert_eq!(
        first_author, "user",
        "first yielded event author; want the user message first"
    );
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct AskArgs {}

#[tokio::test]
async fn test_runner_llm_agent_long_running_tool_pauses_and_resumes() {
    let m = ScriptedModel::new(vec![
        model_call("ask_human", json!({})),
        model_text("waiting for approval"),
        model_text("all done"),
    ]);
    let long_tool = FunctionTool::new(
        FunctionToolConfig {
            is_long_running: true,
            ..FunctionToolConfig::new("ask_human", "asks a human and waits")
        },
        |_ctx: Context, _a: AskArgs| async {
            Ok::<_, ToolError>(BTreeMap::from([(
                "status".to_string(),
                "pending".to_string(),
            )]))
        },
    )
    .expect("functiontool.New()");
    let a = llm_agent::new(LlmAgentConfig {
        name: "approver".into(),
        model: Some(m.clone() as Arc<dyn Llm>),
        tools: vec![long_tool],
        ..Default::default()
    })
    .expect("llmagent.New()");
    let (svc, r) = node_test_setup(a.clone()).await;

    let mut long_running_id = String::new();
    for ev in drain(run(&r, user_text("please approve"))).await {
        if let Some(id) = ev.long_running_tool_ids.first() {
            long_running_id = id.clone();
        }
    }
    assert!(
        !long_running_id.is_empty(),
        "did not observe a long-running tool call from the LlmAgent"
    );
    assert_eq!(
        m.calls(),
        2,
        "turn 1 model calls (pending long-running result is fed back to the model once)"
    );

    let state = run_state_for_agent(&svc, &a).await;
    assert!(
        has_waiting_interrupt(state.as_ref(), &long_running_id),
        "expected a waiting node with InterruptID {long_running_id:?} in persisted state"
    );

    let reply =
        function_response_content(&long_running_id, "ask_human", json!({"output": "approved"}));
    let turn2 = drain(run(&r, reply)).await;
    assert!(
        saw_text(&turn2, "all done"),
        "did not observe the LlmAgent's final answer after resume"
    );
}

#[tokio::test]
async fn test_runner_message_as_output_clears_output() {
    let node: Arc<dyn Node> = AgentNode::new(
        scripted_agent("greeter", "the only answer"),
        NodeConfig::default(),
    )
    .expect("workflow.NewAgentNode()");
    let (_svc, r) = node_test_setup(workflow_agent_of("wf", chain(&[start(), node]))).await;

    let mut saw_message_as_output = false;
    for ev in drain(run(&r, user_text("hi"))).await {
        if ev.response.partial || !ev.node_info.as_ref().is_some_and(|n| n.message_as_output) {
            continue;
        }
        let Some(content) = &ev.response.content else {
            continue;
        };
        saw_message_as_output = true;
        assert_eq!(
            ev.output, None,
            "MessageAsOutput event Output; must be cleared to avoid double-rendering the model text"
        );
        assert!(
            !content.parts.is_empty(),
            "MessageAsOutput event lost its Content after Output was cleared"
        );
    }
    assert!(
        saw_message_as_output,
        "expected a non-partial event stamped with NodeInfo.MessageAsOutput"
    );
}

#[tokio::test]
async fn test_runner_llm_agent_output_key_schema_single_output() {
    const JSON_RESP: &str = r#"{"text":"ok"}"#;
    let a = llm_agent::new(LlmAgentConfig {
        name: "structured".into(),
        model: Some(ScriptedModel::new(vec![model_text(JSON_RESP)]) as Arc<dyn Llm>),
        output_key: "resp".into(),
        output_schema: Some(Schema {
            r#type: Some(Type::Object),
            properties: [(
                "text".to_string(),
                Schema {
                    r#type: Some(Type::String),
                    ..Default::default()
                },
            )]
            .into(),
            ..Default::default()
        }),
        ..Default::default()
    })
    .expect("llmagent.New()");
    let (svc, r) = node_test_setup(a).await;

    let events = drain(run(&r, user_text("hi"))).await;
    assert_eq!(events.len(), 1, "yielded events");

    let sess = get_session(&svc).await;
    let saved = sess.state().get("resp").expect("State().Get(resp)");
    assert_eq!(saved, json!(JSON_RESP), "state[resp]");
}

#[tokio::test]
async fn test_runner_workflow_node_output_and_persistence() {
    let (svc, r) = node_test_setup(new_upper_workflow_agent("wf")).await;

    let got_outputs = outputs(&drain(run(&r, user_text("hi"))).await);
    assert_eq!(got_outputs, vec![json!("HI")], "yielded outputs");

    let sess = get_session(&svc).await;
    assert!(
        session_outputs(sess.as_ref()).contains(&json!("HI")),
        "session does not contain the persisted output \"HI\""
    );
}

#[tokio::test]
async fn test_runner_workflow_node_error_propagates() {
    let boom = function_node::<String, String, _, _>(
        "boom",
        |_ctx: Context, _in: String| async { Err(AgentError::other("node failure")) },
        NodeConfig::default(),
    );
    let (_svc, r) = node_test_setup(workflow_agent_of("wf_err", chain(&[start(), boom]))).await;

    let (_, errs) = run_all(run(&r, user_text("hi"))).await;
    let got = errs
        .last()
        .expect("Run() yielded no error, want the node failure to propagate");
    assert!(
        got.to_string().contains("node failure"),
        "error = {got}, want it to contain \"node failure\""
    );
}

#[tokio::test]
async fn test_runner_workflow_node_multiple_invocations_accumulate() {
    let (svc, r) = node_test_setup(new_upper_workflow_agent("wf")).await;

    for input in ["first", "second", "third"] {
        drain(run(&r, user_text(input))).await;
    }

    let sess = get_session(&svc).await;
    assert_eq!(
        session_outputs(sess.as_ref()),
        vec![json!("FIRST"), json!("SECOND"), json!("THIRD")],
        "session outputs"
    );
}

#[tokio::test]
async fn test_runner_node_yield_user_message_false_by_default() {
    let (svc, r) = node_test_setup(scripted_agent("greeter", "hi back")).await;

    for ev in drain(run(&r, user_text("hi"))).await {
        assert_ne!(
            ev.author, "user",
            "yielded a user message event {ev:?}, want none by default"
        );
    }

    let sess = get_session(&svc).await;
    assert!(
        session_has_user_text(sess.as_ref(), "hi"),
        "session does not contain the persisted user message \"hi\""
    );
}

#[tokio::test]
async fn test_runner_node_state_delta_applied_before_node_runs() {
    let reader = function_node::<String, String, _, _>(
        "reader",
        |ctx: Context, _in: String| async move {
            let v = ctx.state().get("test_state")?;
            Ok(format!("state:{}", v.as_str().unwrap_or("")))
        },
        NodeConfig::default(),
    );
    let (svc, r) = node_test_setup(workflow_agent_of("wf_state", chain(&[start(), reader]))).await;

    let got_outputs = outputs(&drain(run_with(&r, user_text("go"), state_delta())).await);
    assert_eq!(got_outputs, vec![json!("state:must_change")], "outputs");

    let sess = get_session(&svc).await;
    let v = sess
        .state()
        .get("test_state")
        .expect("State().Get(test_state)");
    assert_eq!(v, json!("must_change"), "session state[test_state]");
}

#[tokio::test]
async fn test_runner_node_yields_user_event_with_state_delta() {
    let (_svc, r) = node_test_setup(scripted_agent("noop", "done")).await;

    let events = drain(run_with(
        &r,
        user_text("go"),
        RunOptions {
            yield_user_message: true,
            ..state_delta()
        },
    ))
    .await;
    let first = events.first().expect("Run() yielded no events");
    assert_eq!(first.author, "user", "first yielded event author");
    let delta = first
        .actions
        .state_delta
        .as_ref()
        .expect("user event state delta");
    assert_eq!(
        delta.get("test_state"),
        Some(&json!("must_change")),
        "user event state delta[test_state]"
    );
}

#[tokio::test]
async fn test_runner_node_state_delta_applied_before_llm_agent_runs() {
    let captured = Arc::new(Mutex::new(None::<Value>));
    let slot = captured.clone();
    let cb: BeforeAgentCallback = Arc::new(move |ctx: Context| {
        let slot = slot.clone();
        Box::pin(async move {
            let v = ctx.state().get("test_state")?;
            *slot.lock() = Some(v.clone());
            Ok(Some(Content::from_text(
                format!("state:{}", v.as_str().unwrap_or("")),
                "model",
            )))
        })
    });
    let a = llm_agent::new(LlmAgentConfig {
        name: "state_agent".into(),
        model: Some(ScriptedModel::new(vec![model_text("unused")]) as Arc<dyn Llm>),
        before_agent_callbacks: vec![cb],
        ..Default::default()
    })
    .expect("llmagent.New()");
    let (_svc, r) = node_test_setup(a).await;

    let events = drain(run_with(&r, user_text("go"), state_delta())).await;
    assert_eq!(
        *captured.lock(),
        Some(json!("must_change")),
        "before-agent callback saw state"
    );
    assert!(
        saw_text(&events, "state:must_change"),
        "did not observe the before-agent callback response carrying the state value"
    );
}

#[tokio::test]
async fn test_runner_node_plain_text_does_not_trigger_resume() {
    let (_svc, r) = node_test_setup(new_upper_workflow_agent("wf")).await;

    drain(run(&r, user_text("first"))).await;
    let got_outputs = outputs(&drain(run(&r, user_text("second"))).await);
    assert_eq!(got_outputs, vec![json!("SECOND")], "turn 2 outputs");
}

#[tokio::test]
async fn test_runner_node_allows_mixed_function_response_and_text() {
    let (_svc, r) = node_test_setup(new_permissive_agent()).await;

    let msg = Content {
        role: "user".into(),
        parts: vec![
            Part {
                text: "some text".into(),
                ..Default::default()
            },
            Part {
                function_response: Some(FunctionResponse {
                    id: "fc-1".into(),
                    name: "tool".into(),
                    response: json!({"v": 1}).as_object().cloned().unwrap(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ],
    };
    let (events, errs) = run_all(run(&r, msg)).await;
    assert!(
        errs.is_empty(),
        "Run() error = {errs:?}, want none (adk-go allows mixed function-response + text)"
    );
    assert!(
        saw_text(&events, "handled"),
        "expected a fresh run to consult the model for a mixed function-response + text message"
    );
}

#[tokio::test]
async fn test_runner_node_tolerates_unmatched_function_response() {
    let (_svc, r) = node_test_setup(new_permissive_agent()).await;

    let msg = function_response_content("no-such-fc", "unknown", json!({}));
    let (events, errs) = run_all(run(&r, msg)).await;
    assert!(
        errs.is_empty(),
        "Run() error = {errs:?}, want none (adk-go tolerates an unmatched function response)"
    );
    assert!(
        saw_text(&events, "handled"),
        "expected a fresh run to consult the model after an unmatched function response"
    );
}

/// Go `time.Date(2026, time.March, day, hour, 0, 0, 0, time.UTC)`.
fn march(day: u32, hour: u32) -> chrono::DateTime<chrono::Utc> {
    use chrono::TimeZone;
    chrono::Utc
        .with_ymd_and_hms(2026, 3, day, hour, 0, 0)
        .unwrap()
}

/// A runner over `agent` with `plugins` whose platform clock always reads `run_time` (Go
/// `platform.WithTimeProvider(ctx, func() time.Time { return runTime })`).
fn new_timed_plugin_runner(
    agent: Arc<dyn Agent>,
    svc: Arc<dyn SessionService>,
    plugins: Vec<gadk_agent::Plugin>,
    run_time: chrono::DateTime<chrono::Utc>,
) -> Runner {
    Runner::new(RunnerConfig {
        app_name: NODE_TEST_APP.into(),
        agent,
        session_service: svc,
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig {
            plugins,
            ..Default::default()
        },
        auto_create_session: false,
        platform: Platform::default().with_time(move || run_time),
        compaction: None,
    })
    .expect("runner.New()")
}

/// A custom agent that yields `ev` (with its `invocation_id` set to the run's when `own_inv`).
fn yielding_agent(name: &str, ev: Event, own_inv: bool) -> Arc<dyn Agent> {
    let run: gadk_agent::AgentRunFn = Arc::new(move |ctx: gadk_agent::InvocationContext| {
        let mut ev = ev.clone();
        if own_inv {
            ev.invocation_id = ctx.invocation_id().to_string();
        }
        futures::stream::iter([Ok(ev)]).boxed()
    });
    gadk_agent::new_agent(gadk_agent::AgentConfig {
        name: name.into(),
        run: Some(run),
        ..Default::default()
    })
    .expect("agent.New()")
}

fn reply_event(author: &str, text: &str) -> Event {
    let mut ev = Event {
        author: author.into(),
        ..Default::default()
    };
    ev.response.content = Some(model_text(text));
    ev
}

fn sequential_root(sub_agents: Vec<Arc<dyn Agent>>) -> Arc<dyn Agent> {
    gadk_engine::agents::sequential_agent::new(
        gadk_engine::agents::sequential_agent::SequentialAgentConfig {
            agent: gadk_agent::AgentConfig {
                name: "seq_root".into(),
                sub_agents,
                ..Default::default()
            },
        },
    )
    .expect("sequentialagent.New()")
}

/// Go `newLlmRoot`: an LlmAgent root that transfers to its only sub-agent "custom".
fn transferring_llm_root(sub: Arc<dyn Agent>) -> Arc<dyn Agent> {
    let call = Content {
        role: "model".into(),
        parts: vec![Part {
            function_call: Some(FunctionCall {
                id: "transfer-1".into(),
                name: "transfer_to_agent".into(),
                args: json!({"agent_name": "custom"})
                    .as_object()
                    .cloned()
                    .unwrap(),
                ..Default::default()
            }),
            ..Default::default()
        }],
    };
    llm_agent::new(LlmAgentConfig {
        name: "llm_root".into(),
        model: Some(ScriptedModel::new(vec![call]) as Arc<dyn Llm>),
        sub_agents: vec![sub],
        ..Default::default()
    })
    .expect("llmagent.New()")
}

fn event_has_text(ev: &Event, text: &str) -> bool {
    ev.response
        .content
        .as_ref()
        .is_some_and(|c| c.parts.len() == 1 && c.parts[0].text == text)
}

// parity: adapted: on_event receives the event by value, so Go's in-place rewrite (`*ev = …; return nil`) only changes the plugin's own copy and the agent's original event (also without a timestamp) is stored
#[tokio::test]
async fn test_runner_stamps_events_yielded_without_timestamp() {
    let run_time = march(2, 10);
    let own_time = march(1, 9);
    let zero = gadk_session::zero_time();
    // (name, llm root, own timestamp, replace, rewrite, want)
    let cases = [
        ("sequential agent root", false, zero, false, false, run_time),
        ("llm agent root", true, zero, false, false, run_time),
        (
            "sequential agent root, plugin replacement",
            false,
            zero,
            true,
            false,
            run_time,
        ),
        (
            "llm agent root, plugin replacement",
            true,
            zero,
            true,
            false,
            run_time,
        ),
        (
            "sequential agent root, plugin rewrites in place",
            false,
            zero,
            false,
            true,
            run_time,
        ),
        (
            "llm agent root, plugin rewrites in place",
            true,
            zero,
            false,
            true,
            run_time,
        ),
        (
            "sequential agent root, own timestamp",
            false,
            own_time,
            false,
            false,
            own_time,
        ),
        (
            "llm agent root, own timestamp",
            true,
            own_time,
            false,
            false,
            own_time,
        ),
    ];
    for (name, llm_root, own, replace, rewrite, want) in cases {
        let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
        new_node_test_session(&svc).await;
        let saw_unstamped = Arc::new(Mutex::new(false));
        let saw = saw_unstamped.clone();
        let p = gadk_agent::Plugin::new(gadk_agent::PluginConfig {
            name: "observe_and_replace".into(),
            on_event_callback: Some(Arc::new(move |_ctx, ev: Event| {
                let out = if ev.author != "custom" {
                    None
                } else {
                    if ev.timestamp == gadk_session::zero_time() {
                        *saw.lock() = true;
                    }
                    let copy = Event {
                        author: ev.author.clone(),
                        response: ev.response.clone(),
                        ..Default::default()
                    };
                    if rewrite {
                        drop(copy);
                        None
                    } else if replace {
                        Some(copy)
                    } else {
                        None
                    }
                };
                Box::pin(async move { Ok(out) })
            })),
            ..Default::default()
        });
        let mut ev = reply_event("custom", "reply");
        ev.timestamp = own;
        let custom = yielding_agent("custom", ev, false);
        let root = if llm_root {
            transferring_llm_root(custom)
        } else {
            sequential_root(vec![custom])
        };
        let r = new_timed_plugin_runner(root, svc.clone(), vec![p], run_time);
        let (_, errs) = run_all(run(&r, user_text("go"))).await;
        assert!(errs.is_empty(), "{name}: Run() error = {errs:?}");

        let got = get_session(&svc).await;
        let mut saw_reply = false;
        for ev in got.events() {
            if ev.author != "custom" {
                continue;
            }
            saw_reply = true;
            assert_eq!(ev.timestamp, want, "{name}: stored event timestamp");
        }
        assert!(
            saw_reply,
            "{name}: no event from the custom agent was stored"
        );
        assert_ne!(
            got.last_update_time(),
            gadk_session::zero_time(),
            "{name}: session LastUpdateTime() is zero after the run"
        );
        assert!(
            !*saw_unstamped.lock(),
            "{name}: OnEventCallback saw the custom agent's event with a zero timestamp"
        );
    }
}

#[tokio::test]
async fn test_runner_plugin_replacement_keeps_event_identity() {
    let run_time = march(2, 10);
    let plugin_time = march(3, 11);
    let agent_time = march(1, 9);
    // (name, llm root, replacement author, want author)
    let cases = [
        ("sequential agent root, no author", false, "", "custom"),
        ("llm agent root, no author", true, "", "custom"),
        (
            "sequential agent root, own author",
            false,
            "rewriter",
            "rewriter",
        ),
        ("llm agent root, own author", true, "rewriter", "rewriter"),
    ];
    for (name, llm_root, author, want_author) in cases {
        let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
        new_node_test_session(&svc).await;
        let original: Arc<Mutex<Option<Event>>> = Arc::default();
        let seen = original.clone();
        let p = gadk_agent::Plugin::new(gadk_agent::PluginConfig {
            name: "replace".into(),
            on_event_callback: Some(Arc::new(move |_ctx, ev: Event| {
                let out = if ev.author != "custom" {
                    None
                } else {
                    *seen.lock() = Some(ev.clone());
                    let mut rep = Event {
                        id: "plugin-id".into(),
                        invocation_id: "plugin-invocation".into(),
                        timestamp: plugin_time,
                        author: author.into(),
                        ..Default::default()
                    };
                    rep.response.content = Some(model_text("replaced"));
                    Some(rep)
                };
                Box::pin(async move { Ok(out) })
            })),
            ..Default::default()
        });
        let mut ev = reply_event("custom", "reply");
        ev.id = "agent-event".into();
        ev.timestamp = agent_time;
        let custom = yielding_agent("custom", ev, true);
        let root = if llm_root {
            transferring_llm_root(custom)
        } else {
            sequential_root(vec![custom])
        };
        let r = new_timed_plugin_runner(root, svc.clone(), vec![p], run_time);
        let events = drain(run(&r, user_text("go"))).await;
        let original = original
            .lock()
            .clone()
            .unwrap_or_else(|| panic!("{name}: the plugin never saw the custom agent's event"));
        let streamed = events
            .iter()
            .rfind(|e| event_has_text(e, "replaced"))
            .cloned()
            .unwrap_or_else(|| panic!("{name}: the replacement was not streamed"));
        let stored = get_session(&svc)
            .await
            .events()
            .into_iter()
            .rfind(|e| event_has_text(e, "replaced"))
            .unwrap_or_else(|| panic!("{name}: the replacement was not stored"));
        for (where_, ev) in [("streamed", &streamed), ("stored", &stored)] {
            assert_eq!(ev.id, original.id, "{name}: {where_} replacement ID");
            assert_eq!(
                ev.invocation_id, original.invocation_id,
                "{name}: {where_} replacement InvocationID"
            );
            assert_eq!(
                ev.timestamp, agent_time,
                "{name}: {where_} replacement Timestamp"
            );
            assert_eq!(
                ev.author, want_author,
                "{name}: {where_} replacement Author"
            );
        }
    }
}

// parity: adapted: Rust plugins hand back an owned Event, so the runner cannot write to the plugin's shared replacement; the guard checks the shared value and that each stored copy carries its original's identity
#[tokio::test]
async fn test_runner_plugin_shared_replacement_is_not_modified() {
    let run_time = march(2, 10);
    let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    new_node_test_session(&svc).await;

    let speaker = |name: &str| {
        let mut ev = reply_event(name, "reply");
        ev.id = format!("{name}-event");
        yielding_agent(name, ev, true)
    };
    let root = sequential_root(vec![speaker("custom"), speaker("other")]);
    let mut shared = Event::default();
    shared.response.content = Some(model_text("redacted"));
    let shared = Arc::new(Mutex::new(shared));
    let plugin_shared = shared.clone();
    let p = gadk_agent::Plugin::new(gadk_agent::PluginConfig {
        name: "redact".into(),
        on_event_callback: Some(Arc::new(move |_ctx, ev: Event| {
            let out = if ev.author != "custom" && ev.author != "other" {
                None
            } else {
                Some(plugin_shared.lock().clone())
            };
            Box::pin(async move { Ok(out) })
        })),
        ..Default::default()
    });
    let r = new_timed_plugin_runner(root, svc.clone(), vec![p], run_time);

    let ids = |evs: &[Event]| -> Vec<(String, String)> {
        evs.iter()
            .filter(|e| event_has_text(e, "redacted"))
            .map(|e| (e.id.clone(), e.author.clone()))
            .collect()
    };
    let streamed = ids(&drain(run(&r, user_text("go"))).await);
    let stored = ids(&get_session(&svc).await.events());
    let want = vec![
        ("custom-event".to_string(), "custom".to_string()),
        ("other-event".to_string(), "other".to_string()),
    ];
    assert_eq!(streamed, want, "streamed replacements");
    assert_eq!(stored, want, "stored replacements");
    let shared = shared.lock();
    assert!(
        shared.id.is_empty()
            && shared.invocation_id.is_empty()
            && shared.author.is_empty()
            && shared.timestamp == gadk_session::zero_time(),
        "shared replacement was modified: {:?}",
        *shared
    );
}

// parity: adapted: on_event receives the event by value, so Go's in-place rewrite only changes the plugin's own copy; that case then stores the runner's own reply, which already has a timestamp
#[tokio::test]
async fn test_runner_stamps_before_run_replacement() {
    let run_time = march(2, 10);
    let plugin_time = march(3, 11);
    for (name, rewrite) in [("replacement", false), ("rewrite in place", true)] {
        let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
        new_node_test_session(&svc).await;
        let a = gadk_agent::new_agent(gadk_agent::AgentConfig {
            name: "custom".into(),
            ..Default::default()
        })
        .expect("agent.New()");
        let original: Arc<Mutex<Option<Event>>> = Arc::default();
        let seen = original.clone();
        let p = gadk_agent::Plugin::new(gadk_agent::PluginConfig {
            name: "cache".into(),
            before_run_callback: Some(Arc::new(|_ctx| {
                Box::pin(async { Ok(Some(model_text("cached"))) })
            })),
            on_event_callback: Some(Arc::new(move |_ctx, ev: Event| {
                let out = if ev.author != "custom" {
                    None
                } else {
                    *seen.lock() = Some(ev.clone());
                    if rewrite {
                        None
                    } else {
                        Some(Event {
                            id: "plugin-id".into(),
                            invocation_id: "plugin-invocation".into(),
                            timestamp: plugin_time,
                            response: ev.response.clone(),
                            ..Default::default()
                        })
                    }
                };
                Box::pin(async move { Ok(out) })
            })),
            ..Default::default()
        });
        let r = new_timed_plugin_runner(a, svc.clone(), vec![p], run_time);
        let (_, errs) = run_all(run(&r, user_text("go"))).await;
        assert!(errs.is_empty(), "{name}: Run() error = {errs:?}");

        let got = get_session(&svc).await;
        let original = original.lock().clone().unwrap_or_default();
        let mut saw_reply = false;
        for ev in got.events() {
            if !event_has_text(&ev, "cached") {
                continue;
            }
            saw_reply = true;
            assert_eq!(ev.author, "custom", "{name}: stored BeforeRun reply Author");
            assert_eq!(
                ev.timestamp, run_time,
                "{name}: stored BeforeRun reply timestamp"
            );
            if rewrite {
                continue;
            }
            assert_eq!(ev.id, original.id, "{name}: stored BeforeRun reply ID");
            assert_eq!(
                ev.invocation_id, original.invocation_id,
                "{name}: stored BeforeRun reply InvocationID"
            );
        }
        assert!(saw_reply, "{name}: the BeforeRun reply was not stored");
        assert!(
            !original.id.is_empty() && !original.invocation_id.is_empty(),
            "{name}: the plugin saw a reply with ID {:?} and InvocationID {:?}, want both set",
            original.id,
            original.invocation_id
        );
        assert_ne!(
            got.last_update_time(),
            gadk_session::zero_time(),
            "{name}: session LastUpdateTime() is zero after the run"
        );
    }
}

/// Go `newCallerRoot`: a custom root whose sub-agent "approver" made the seeded calls; the
/// root yields one text event when it runs.
fn new_caller_root() -> Arc<dyn Agent> {
    let run: gadk_agent::AgentRunFn = Arc::new(|ctx: gadk_agent::InvocationContext| {
        let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
        ev.author = "root".into();
        ev.response.content = Some(model_text("root"));
        futures::stream::iter([Ok(ev)]).boxed()
    });
    gadk_agent::new_agent(gadk_agent::AgentConfig {
        name: "root".into(),
        sub_agents: vec![new_permissive_agent()],
        run: Some(run),
        ..Default::default()
    })
    .expect("agent.New()")
}

/// Go `seedFunctionCall`: a model event by "approver" carrying calls `call_ids`, as if
/// `invocation_id` had issued them.
async fn seed_function_call(svc: &Arc<dyn SessionService>, invocation_id: &str, call_ids: &[&str]) {
    let sess = get_session(svc).await;
    let mut ev = Event::new(&Platform::default(), invocation_id);
    ev.author = "approver".into();
    ev.response.content = Some(Content {
        role: "model".into(),
        parts: call_ids
            .iter()
            .map(|id| Part {
                function_call: Some(FunctionCall {
                    id: (*id).into(),
                    name: "tool".into(),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .collect(),
    });
    svc.append_event(&*sess, &mut ev)
        .await
        .expect("AppendEvent()");
}

/// Go `runFunctionResponses`: seeds fc-0 (no invocation ID), fc-1a/fc-1b (inv-1) and fc-2
/// (inv-2), runs `root` on a message answering `call_ids`, and returns every yielded event's
/// invocation ID.
async fn run_function_responses(root: Arc<dyn Agent>, call_ids: &[&str]) -> Vec<String> {
    let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    new_node_test_session(&svc).await;
    seed_function_call(&svc, "", &["fc-0"]).await;
    seed_function_call(&svc, "inv-1", &["fc-1a", "fc-1b"]).await;
    seed_function_call(&svc, "inv-2", &["fc-2"]).await;
    let r = new_node_test_runner(root, svc.clone());
    let msg = Content {
        role: "user".into(),
        parts: call_ids
            .iter()
            .map(|id| Part {
                function_response: Some(FunctionResponse {
                    id: (*id).into(),
                    name: "tool".into(),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .collect(),
    };
    let got: Vec<String> = drain(run(&r, msg))
        .await
        .into_iter()
        .map(|e| e.invocation_id)
        .collect();
    assert!(!got.is_empty(), "Run() yielded no events");
    got
}

#[rstest::rstest]
#[case::agent_path_calls_of_one_invocation(new_caller_root as fn() -> Arc<dyn Agent>, &["fc-1a", "fc-1b"])]
#[case::node_path_calls_of_one_invocation(new_permissive_agent as fn() -> Arc<dyn Agent>, &["fc-1a", "fc-1b"])]
#[case::node_path_calls_of_two_invocations(new_permissive_agent as fn() -> Arc<dyn Agent>, &["fc-1a", "fc-2"])]
#[tokio::test]
async fn test_runner_function_responses_resume_their_invocation(
    #[case] root: fn() -> Arc<dyn Agent>,
    #[case] ids: &[&str],
) {
    for id in run_function_responses(root(), ids).await {
        assert_eq!(id, "inv-1", "event InvocationID");
    }
}

#[rstest::rstest]
#[case::agent_path_calls_of_two_invocations(new_caller_root as fn() -> Arc<dyn Agent>, &["fc-1a", "fc-2"])]
#[case::agent_path_unknown_calls(new_caller_root as fn() -> Arc<dyn Agent>, &["no-such-fc"])]
#[case::node_path_unknown_calls(new_permissive_agent as fn() -> Arc<dyn Agent>, &["no-such-fc"])]
// fc-0's event has no invocation ID, and that counts as an invocation of its own.
#[case::agent_path_call_without_an_invocation_id_first(new_caller_root as fn() -> Arc<dyn Agent>, &["fc-0", "fc-2"])]
#[case::node_path_call_without_an_invocation_id_first(new_permissive_agent as fn() -> Arc<dyn Agent>, &["fc-0", "fc-2"])]
#[tokio::test]
async fn test_runner_function_responses_start_new_invocation(
    #[case] root: fn() -> Arc<dyn Agent>,
    #[case] ids: &[&str],
) {
    for id in run_function_responses(root(), ids).await {
        assert!(
            !id.is_empty() && id != "inv-1" && id != "inv-2",
            "event InvocationID = {id:?}, want a new invocation ID"
        );
    }
}
