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

//! Port of ADK-Go workflow/agent_node_mode_placement_test.go (external→internal).

use std::any::Any;
use std::sync::Arc;

use futures::StreamExt;
use futures::stream::BoxStream;
use gadk_agent::{
    Agent, AgentConfig, AgentError, Context, ContextDelta, Mode, ParentMap, RunConfig,
};
use gadk_genai::{Content, FunctionCall};
use gadk_model::{Llm, LlmRequest, LlmResponse, ModelError};
use gadk_platform::Platform;
use gadk_session::{CreateRequest, Event, GetRequest, InMemorySessionService, SessionService};
use serde_json::{Value, json};

use crate::agents::sequential_agent::{self, SequentialAgentConfig};
use crate::agents::workflow_agent::{self, WorkflowAgentConfig};
use crate::llm::agent as llm_agent;
use crate::llm::{IncludeContents, LlmAgentConfig};
use crate::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use crate::testutil::MockModel;
use crate::workflow::{AgentNode, Edge, Node, NodeConfig, START, Workflow, WorkflowOptions};

/// Go `for ev, err := range seq { if err != nil { t.Fatal(err) } }`: every event (with or
/// without content), stopping at the first error.
async fn drain_events(s: gadk_agent::EventStream) -> Result<Vec<Event>, gadk_agent::AgentError> {
    futures::TryStreamExt::try_collect(s).await
}

/// Port of Go `capturingLLM`: records the first request, always answers "answer".
struct CapturingLlm {
    got: parking_lot::Mutex<Option<LlmRequest>>,
}

impl CapturingLlm {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            got: parking_lot::Mutex::new(None),
        })
    }
    fn got(&self) -> Option<LlmRequest> {
        self.got.lock().clone()
    }
    /// Go `systemInstruction()`: concatenated system-instruction text, "" when absent.
    fn system_instruction(&self) -> String {
        self.got()
            .map(|r| system_instruction_of(&r))
            .unwrap_or_default()
    }
}

impl Llm for CapturingLlm {
    fn name(&self) -> &str {
        "capturing"
    }
    fn generate_content(
        &self,
        req: LlmRequest,
        _stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        let mut got = self.got.lock();
        if got.is_none() {
            *got = Some(req);
        }
        let resp = LlmResponse {
            content: Some(Content::from_text("answer", "model")),
            ..Default::default()
        };
        futures::stream::iter([Ok(resp)]).boxed()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn system_instruction_of(r: &LlmRequest) -> String {
    r.config
        .as_ref()
        .and_then(|c| c.system_instruction.as_ref())
        .map(|si| si.parts.iter().map(|p| p.text.as_str()).collect())
        .unwrap_or_default()
}

/// True when any part of `contents` contains `needle`.
fn contents_contain(contents: &[Content], needle: &str) -> bool {
    contents
        .iter()
        .any(|c| c.parts.iter().any(|p| p.text.contains(needle)))
}

/// `&session.Event{Author: author, LLMResponse: {Content: NewContentFromText(text, role)}}`.
fn prior_event(author: &str, text: &str, role: &str) -> Event {
    let mut ev = Event {
        author: author.into(),
        ..Default::default()
    };
    ev.response.content = Some(Content::from_text(text, role));
    ev
}

/// Port of Go `newModeTestCtx`: a fresh in-memory session holding `prior`, user content
/// "current question", invocation "inv-mode-test", StreamingMode none.
async fn new_mode_test_ctx(a: &Arc<dyn Agent>, prior: Vec<Event>) -> Context {
    mode_ctx(a, prior, "inv-mode-test", Some("current question"), None).await
}

async fn mode_ctx(
    a: &Arc<dyn Agent>,
    prior: Vec<Event>,
    invocation_id: &str,
    user_content: Option<&str>,
    parents: Option<Arc<ParentMap>>,
) -> Context {
    let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let created = svc
        .create(CreateRequest {
            app_name: "app".into(),
            user_id: "u".into(),
            ..Default::default()
        })
        .await
        .expect("create session");
    for mut ev in prior {
        svc.append_event(created.as_ref(), &mut ev)
            .await
            .expect("AppendEvent");
    }
    let session = svc
        .get(GetRequest {
            app_name: "app".into(),
            user_id: "u".into(),
            session_id: created.id().to_string(),
            ..Default::default()
        })
        .await
        .expect("get session");
    let mut b = Context::builder()
        .session_service(svc)
        .session(session)
        .agent(a.clone())
        .invocation_id(invocation_id)
        .run_config(RunConfig::default());
    if let Some(text) = user_content {
        b = b.user_content(Content::from_text(text, "user"));
    }
    if let Some(p) = parents {
        b = b.parent_map(p);
    }
    b.build()
}

/// Drives `wf.run` (Go `wf.Run(ctx)`) to completion, panicking on any error.
async fn run_workflow(wf: &Arc<Workflow>, ctx: &Context) {
    drain_events(wf.run(ctx.as_invocation().clone()))
        .await
        .expect("workflow.Run");
}

fn llm(cfg: LlmAgentConfig) -> Arc<dyn Agent> {
    llm_agent::new(cfg).expect("llmagent.New")
}

fn node_of(a: &Arc<dyn Agent>) -> Arc<dyn Node> {
    AgentNode::new(a.clone(), NodeConfig::default()).expect("NewAgentNode")
}

fn single_node_workflow(a: &Arc<dyn Agent>) -> Arc<Workflow> {
    Workflow::new(
        "wf",
        vec![Edge {
            from: Arc::clone(&START),
            to: node_of(a),
            route: None,
        }],
        WorkflowOptions::default(),
    )
    .expect("workflow.New")
}

fn transfer_to(name: &str) -> Content {
    Content::from_function_call(
        FunctionCall {
            name: "transfer_to_agent".into(),
            args: json!({ "agent_name": name })
                .as_object()
                .cloned()
                .unwrap_or_default(),
            ..Default::default()
        },
        "model",
    )
}

fn auto_runner(agent: Arc<dyn Agent>) -> Runner {
    Runner::new(RunnerConfig {
        app_name: "app".into(),
        agent,
        session_service: Arc::new(InMemorySessionService::new()),
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig::default(),
        auto_create_session: true,
        platform: Platform::default(),
        compaction: None,
    })
    .expect("runner.New")
}

/// Go `for _, err := range r.Run(ctx, "u", "s1", NewContentFromText(text, "user"), RunConfig{})`.
async fn run_root(r: &Runner, text: &str) -> Vec<AgentError> {
    let mut s = r.run(
        "u",
        "s1",
        Some(Content::from_text(text, "user")),
        RunConfig::default(),
        RunOptions::default(),
    );
    let mut errs = Vec::new();
    while let Some(item) = s.next().await {
        if let Err(e) = item {
            errs.push(e);
        }
    }
    errs
}

/// Port of Go `roundTripLLM`: replies from a per-agent script (caller identified by the
/// MARKER_HELPER system-instruction marker) and records every request it served.
struct RoundTripLlm {
    script: std::collections::HashMap<String, Vec<Content>>,
    log: parking_lot::Mutex<RoundTripLog>,
}

/// Go `roundTripLLM`'s `seen`, `si` and `conts`: one entry per request served.
#[derive(Default)]
struct RoundTripLog {
    seen: Vec<String>,
    si: Vec<String>,
    conts: Vec<Vec<Content>>,
}

impl RoundTripLlm {
    fn new(script: Vec<(&str, Vec<Content>)>) -> Arc<Self> {
        Arc::new(Self {
            script: script
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
            log: parking_lot::Mutex::new(RoundTripLog::default()),
        })
    }
    /// The system instruction and contents of the second request served for "worker" (Go `idx[1]`).
    fn second_worker_call(&self) -> (String, Vec<Content>) {
        let log = self.log.lock();
        let idx: Vec<usize> = log
            .seen
            .iter()
            .enumerate()
            .filter(|(_, w)| *w == "worker")
            .map(|(i, _)| i)
            .collect();
        assert!(
            idx.len() >= 2,
            "worker was called {} time(s), want 2; call order = {:?}",
            idx.len(),
            log.seen
        );
        (log.si[idx[1]].clone(), log.conts[idx[1]].clone())
    }
}

impl Llm for RoundTripLlm {
    fn name(&self) -> &str {
        "scripted"
    }
    fn generate_content(
        &self,
        req: LlmRequest,
        _stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        let si = system_instruction_of(&req);
        let who = if si.contains("MARKER_HELPER") {
            "helper"
        } else {
            "worker"
        };
        let mut log = self.log.lock();
        let n = log.seen.iter().filter(|s| *s == who).count();
        log.seen.push(who.to_string());
        log.si.push(si);
        log.conts.push(req.contents.clone());
        let out = self
            .script
            .get(who)
            .and_then(|replies| replies.get(n).cloned())
            .unwrap_or_else(|| Content::from_text("done", "model"));
        futures::stream::iter([Ok(LlmResponse {
            content: Some(out),
            ..Default::default()
        })])
        .boxed()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

const IDENTITY_PREAMBLE: &str = "You are an agent. Your internal name is";

/// Go `workflowagent.New(workflowagent.Config{Name, Description, Edges: {Start → node}})`.
fn workflow_agent_over(name: &str, description: &str, node: Arc<dyn Node>) -> Arc<dyn Agent> {
    workflow_agent::new(WorkflowAgentConfig {
        name: name.into(),
        description: description.into(),
        edges: vec![Edge {
            from: Arc::clone(&START),
            to: node,
            route: None,
        }],
        ..Default::default()
    })
    .expect("workflowagent.New")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_agent_node_unset_mode_runs_as_single_turn_everywhere() {
    let model = CapturingLlm::new();
    let peer = llm(LlmAgentConfig {
        name: "peer".into(),
        model: Some(CapturingLlm::new() as Arc<dyn Llm>),
        ..Default::default()
    });
    // A sub-agent makes transfer wiring reachable, so its suppression is exercised.
    let a = llm(LlmAgentConfig {
        name: "worker".into(),
        description: "does the work".into(),
        model: Some(model.clone() as Arc<dyn Llm>),
        instruction: "WORKER_INSTRUCTION".into(),
        sub_agents: vec![peer],
        ..Default::default()
    });
    let wf = single_node_workflow(&a);
    let ctx = new_mode_test_ctx(&a, vec![prior_event("user", "EARLIER_TURN", "user")]).await;
    run_workflow(&wf, &ctx).await;

    let got = model.got().expect("model was never called");
    let si = model.system_instruction();
    assert!(
        !si.contains(IDENTITY_PREAMBLE),
        "single_turn node got the identity preamble; system instruction:\n{si}"
    );
    assert!(
        !si.contains("transfer_to_agent"),
        "single_turn node got transfer instructions; system instruction:\n{si}"
    );
    // The transfer_to_agent DECLARATION still ships (only the instruction is mode-gated), so it is not asserted.
    assert!(
        !contents_contain(&got.contents, "EARLIER_TURN"),
        "single_turn node saw conversation history; contents = {:?}",
        got.contents
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_agent_node_declared_chat_beats_node_default() {
    let model = CapturingLlm::new();
    let coord = llm(LlmAgentConfig {
        name: "coordinator".into(),
        model: Some(model.clone() as Arc<dyn Llm>),
        mode: Mode::Chat,
        instruction: "COORD_INSTRUCTION".into(),
        ..Default::default()
    });
    let wf = single_node_workflow(&coord);
    // History supplies the current turn and ends on a user turn: the context's user content is
    // not appended to the session here, so without the last event the chat request would end on a model turn.
    let prior = vec![
        prior_event("user", "EARLIER_TURN", "user"),
        prior_event("coordinator", "earlier answer", "model"),
        prior_event("user", "current question", "user"),
    ];
    let ctx = new_mode_test_ctx(&coord, prior).await;
    run_workflow(&wf, &ctx).await;

    let got = model.got().expect("model was never called");
    assert!(
        contents_contain(&got.contents, "EARLIER_TURN"),
        "chat-declared agent at a node lost its history; contents = {:?}",
        got.contents
    );
    let si = model.system_instruction();
    assert!(
        si.contains(IDENTITY_PREAMBLE),
        "chat-declared agent at a node lost its identity preamble; system instruction:\n{si}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_agent_node_declared_single_turn_drops_history() {
    let model = CapturingLlm::new();
    let a = llm(LlmAgentConfig {
        name: "worker".into(),
        model: Some(model.clone() as Arc<dyn Llm>),
        mode: Mode::SingleTurn,
        ..Default::default()
    });
    let wf = single_node_workflow(&a);
    run_workflow(
        &wf,
        &new_mode_test_ctx(&a, vec![prior_event("user", "EARLIER_TURN", "user")]).await,
    )
    .await;
    let got = model.got().expect("model was never called");
    assert!(
        !contents_contain(&got.contents, "EARLIER_TURN"),
        "single_turn node saw history; contents = {:?}",
        got.contents
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_agent_node_explicit_include_contents_beats_placement() {
    let model = CapturingLlm::new();
    let a = llm(LlmAgentConfig {
        name: "worker".into(),
        model: Some(model.clone() as Arc<dyn Llm>),
        include_contents: IncludeContents::Default,
        ..Default::default()
    });
    let wf = single_node_workflow(&a);
    run_workflow(
        &wf,
        &new_mode_test_ctx(&a, vec![prior_event("user", "EARLIER_TURN", "user")]).await,
    )
    .await;
    let got = model.got().expect("model was never called");
    assert!(
        contents_contain(&got.contents, "EARLIER_TURN"),
        "explicit IncludeContentsDefault was overridden by the placement; contents = {:?}",
        got.contents
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_agent_node_declared_single_turn_with_explicit_include_contents_keeps_history() {
    let model = CapturingLlm::new();
    let a = llm(LlmAgentConfig {
        name: "worker".into(),
        model: Some(model.clone() as Arc<dyn Llm>),
        mode: Mode::SingleTurn,
        include_contents: IncludeContents::Default,
        ..Default::default()
    });
    let wf = single_node_workflow(&a);
    run_workflow(
        &wf,
        &new_mode_test_ctx(&a, vec![prior_event("user", "EARLIER_TURN", "user")]).await,
    )
    .await;
    let got = model.got().expect("model was never called");
    assert!(
        contents_contain(&got.contents, "EARLIER_TURN"),
        "declared single_turn with explicit IncludeContentsDefault lost its history; contents = {:?}",
        got.contents
    );
}

// parity: skipped TestAgentNode_UnrecognisedIncludeContents_DoesNotDefeatThePlacement: Go IncludeContents is an unvalidated string; Rust's IncludeContents is a closed enum, so a misspelled value cannot be constructed

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_agent_node_unnamed_agent_is_still_placed_single_turn() {
    let model = CapturingLlm::new();
    let a = llm(LlmAgentConfig {
        model: Some(model.clone() as Arc<dyn Llm>),
        ..Default::default()
    });
    assert_eq!(a.name(), "", "precondition: agent name, want empty");
    let wf = single_node_workflow(&a);
    run_workflow(
        &wf,
        &new_mode_test_ctx(&a, vec![prior_event("user", "EARLIER_TURN", "user")]).await,
    )
    .await;
    let got = model.got().expect("model was never called");
    assert!(
        !contents_contain(&got.contents, "EARLIER_TURN"),
        "nameless agent at a single_turn node saw conversation history; contents = {:?}",
        got.contents
    );
}

#[tokio::test]
async fn test_agent_node_placement_survives_a_transfer_round_trip() {
    let m = RoundTripLlm::new(vec![
        ("worker", vec![transfer_to("helper")]),
        ("helper", vec![transfer_to("worker")]),
    ]);
    let helper = llm(LlmAgentConfig {
        name: "helper".into(),
        description: "helps".into(),
        model: Some(m.clone() as Arc<dyn Llm>),
        instruction: "MARKER_HELPER".into(),
        ..Default::default()
    });
    let worker = llm(LlmAgentConfig {
        name: "worker".into(),
        description: "works".into(),
        model: Some(m.clone() as Arc<dyn Llm>),
        instruction: "MARKER_WORKER".into(),
        sub_agents: vec![helper],
        ..Default::default()
    });
    let wf = single_node_workflow(&worker);
    let parents = Arc::new(ParentMap::new(&worker).expect("parentmap.New"));
    let ctx = mode_ctx(
        &worker,
        vec![prior_event("user", "EARLIER_TURN", "user")],
        "inv-1",
        Some("current question"),
        Some(parents),
    )
    .await;
    run_workflow(&wf, &ctx).await;

    let (si, contents) = m.second_worker_call();
    assert!(
        si.contains("MARKER_WORKER"),
        "worker's own instruction is missing, so the absences below prove nothing; si = {si:?}"
    );
    let identity = si.contains(IDENTITY_PREAMBLE);
    let transfer = si.contains("You have a list of other agents to transfer to");
    let history = contents_contain(&contents, "EARLIER_TURN");
    assert!(
        !identity && !transfer && !history,
        "after a transfer round trip the node's agent is no longer single_turn: identityPreamble={identity} transferInstructions={transfer} sawHistory={history}, want all false"
    );
}

// parity: adapted: Go's tool/callback context wrappers (NewToolContext/NewCallbackContext) are one Rust Context; the "tool" row adds a function_call_id + fresh actions delta, the "callback" row a fresh actions delta
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_agent_node_run_accepts_a_wrapper_context() {
    for row in ["tool", "callback"] {
        let model = CapturingLlm::new();
        let peer = llm(LlmAgentConfig {
            name: "peer".into(),
            model: Some(CapturingLlm::new() as Arc<dyn Llm>),
            description: "a peer".into(),
            ..Default::default()
        });
        let a = llm(LlmAgentConfig {
            name: "c".into(),
            description: "c".into(),
            model: Some(model.clone() as Arc<dyn Llm>),
            instruction: "OWN_INSTRUCTION".into(),
            sub_agents: vec![peer],
            ..Default::default()
        });
        let node = node_of(&a);
        let ctx = mode_ctx(&a, vec![], &format!("inv-{row}-ctx"), Some("hi"), None)
            .await
            .with_delta(ContextDelta {
                function_call_id: (row == "tool").then(|| "fc-1".to_string()),
                actions: Some(Arc::new(parking_lot::Mutex::new(Default::default()))),
                ..Default::default()
            });

        let events = drain_events(node.run(ctx, Value::Null))
            .await
            .unwrap_or_else(|e| panic!("{row}: node.Run: {e}"));
        assert!(
            !events.is_empty(),
            "node.Run over a {row} context produced no events"
        );

        let si = model.system_instruction();
        assert!(
            si.contains("OWN_INSTRUCTION"),
            "{row}: the agent's own instruction is missing, so the absences below prove nothing; si = {si:?}"
        );
        assert!(
            !si.contains("You are an agent"),
            "{row}: the node's agent got the identity preamble, so the single_turn placement was dropped"
        );
        assert!(
            !si.contains("transfer_to_agent"),
            "{row}: the node's agent got transfer instructions, so the single_turn placement was dropped"
        );
    }
}

// parity: skipped TestAgentNode_Run_SeededOverAToolContext_ErrorsRatherThanPanics: Go-only — Go tool contexts report a nil Session; every Rust Context carries a session (ContextBuilder default), so the missing-session path is unrepresentable

// parity: skipped TestAgentNode_Run_NilContext_Errors: Go nil-interface check; Node::run takes Context by value and cannot be null

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_agent_node_a_same_named_nested_agent_keeps_its_own_declaration() {
    let inner_model = CapturingLlm::new();
    let peer = llm(LlmAgentConfig {
        name: "peer".into(),
        model: Some(CapturingLlm::new() as Arc<dyn Llm>),
        description: "a peer".into(),
        ..Default::default()
    });
    let inner = llm(LlmAgentConfig {
        name: "coordinator".into(),
        model: Some(inner_model.clone() as Arc<dyn Llm>),
        mode: Mode::SingleTurn,
        instruction: "INNER_INSTRUCTION".into(),
        sub_agents: vec![peer],
        ..Default::default()
    });
    let seq = sequential_agent::new(SequentialAgentConfig {
        agent: AgentConfig {
            name: "seq".into(),
            sub_agents: vec![inner],
            ..Default::default()
        },
    })
    .expect("sequentialagent.New");
    let wf = workflow_agent_over("wf", "the workflow", node_of(&seq));
    let root = llm(LlmAgentConfig {
        name: "coordinator".into(),
        model: Some(MockModel::from_contents(vec![
            transfer_to("wf"),
            Content::from_text("done", "model"),
        ]) as Arc<dyn Llm>),
        sub_agents: vec![wf],
        ..Default::default()
    });

    let errs = run_root(&auto_runner(root), "please do the thing").await;
    assert!(errs.is_empty(), "Run: {errs:?}");

    assert!(
        inner_model.got().is_some(),
        "the nested agent was never reached, so this test pins nothing"
    );
    let si = inner_model.system_instruction();
    assert!(
        si.contains("INNER_INSTRUCTION"),
        "the agent's own instruction is missing, so the two absences below prove nothing; si = {si:?}"
    );
    assert!(
        !si.contains("You are an agent"),
        "the nested single_turn agent got the identity preamble; it read the root's chat binding"
    );
    assert!(
        !si.contains("transfer_to_agent"),
        "the nested single_turn agent got transfer instructions; it read the root's chat binding"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_agent_node_a_single_turn_placement_does_not_reach_a_same_named_undeclared_descendant()
{
    let inner_model = CapturingLlm::new();
    let inner_peer = llm(LlmAgentConfig {
        name: "innerpeer".into(),
        model: Some(CapturingLlm::new() as Arc<dyn Llm>),
        description: "inner peer".into(),
        ..Default::default()
    });
    let inner = llm(LlmAgentConfig {
        name: "worker".into(), // same name as the placed agent below, deliberately
        model: Some(inner_model.clone() as Arc<dyn Llm>),
        instruction: "INNER_INSTRUCTION".into(),
        sub_agents: vec![inner_peer],
        ..Default::default()
    });
    let seq = sequential_agent::new(SequentialAgentConfig {
        agent: AgentConfig {
            name: "seq".into(),
            sub_agents: vec![inner],
            ..Default::default()
        },
    })
    .expect("sequentialagent.New");
    let inner_wf = workflow_agent_over("innerwf", "inner workflow", node_of(&seq));
    let outer = llm(LlmAgentConfig {
        name: "worker".into(),
        model: Some(MockModel::from_contents(vec![
            transfer_to("innerwf"),
            Content::from_text("outer done", "model"),
        ]) as Arc<dyn Llm>),
        sub_agents: vec![inner_wf],
        ..Default::default()
    });
    let wf = workflow_agent_over("wf", "outer workflow", node_of(&outer));
    let root = llm(LlmAgentConfig {
        name: "coordinator".into(),
        model: Some(MockModel::from_contents(vec![
            transfer_to("wf"),
            Content::from_text("done", "model"),
        ]) as Arc<dyn Llm>),
        sub_agents: vec![wf],
        ..Default::default()
    });

    let errs = run_root(&auto_runner(root), "go").await;
    assert!(errs.is_empty(), "Run: {errs:?}");

    assert!(
        inner_model.got().is_some(),
        "the nested agent was never reached, so this test pins nothing"
    );
    let si = inner_model.system_instruction();
    assert!(
        si.contains("INNER_INSTRUCTION"),
        "the agent's own instruction is missing, so the checks below prove nothing; si = {si:?}"
    );
    assert!(
        si.contains("You are an agent"),
        "the nested undeclared agent lost its identity preamble; it read the outer agent's single_turn binding"
    );
    assert!(
        si.contains("transfer_to_agent"),
        "the nested undeclared agent lost its transfer instructions; it read the outer agent's single_turn binding"
    );
}

#[tokio::test]
async fn test_agent_node_a_composite_child_reentered_by_transfer_runs_chat() {
    let m = RoundTripLlm::new(vec![
        ("worker", vec![transfer_to("helper")]),
        ("helper", vec![transfer_to("worker")]),
    ]);
    let helper = llm(LlmAgentConfig {
        name: "helper".into(),
        description: "helps".into(),
        model: Some(m.clone() as Arc<dyn Llm>),
        instruction: "MARKER_HELPER".into(),
        ..Default::default()
    });
    let worker = llm(LlmAgentConfig {
        name: "worker".into(),
        description: "works".into(),
        model: Some(m.clone() as Arc<dyn Llm>),
        instruction: "MARKER_WORKER".into(),
        sub_agents: vec![helper],
        ..Default::default()
    });
    let seq = sequential_agent::new(SequentialAgentConfig {
        agent: AgentConfig {
            name: "seq".into(),
            description: "seq".into(),
            sub_agents: vec![worker],
            ..Default::default()
        },
    })
    .expect("sequentialagent.New");

    let errs = run_root(&auto_runner(seq), "EARLIER_TURN").await;
    assert!(errs.is_empty(), "r.Run: {errs:?}");

    let (si, contents) = m.second_worker_call();
    assert!(
        si.contains("MARKER_WORKER"),
        "worker's own instruction is missing, so the presences below prove nothing; si = {si:?}"
    );
    assert!(
        si.contains("You are an agent"),
        "no identity preamble on re-entry; an unbound undeclared agent must run chat here"
    );
    assert!(
        si.contains("transfer_to_agent"),
        "no transfer instructions on re-entry; an unbound undeclared agent must run chat here"
    );
    assert!(
        contents_contain(&contents, "EARLIER_TURN"),
        "no conversation history on re-entry; an unbound undeclared agent must run chat here"
    );
}
