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

//! Port of ADK-Go workflow/agent_node_mode_mutation_test.go (external→internal: reads `reveal`).

use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

use futures::StreamExt;
use futures::stream::BoxStream;
use gadk_agent::{Agent, Context, Mode, RunConfig};
use gadk_genai::Content;
use gadk_model::{Llm, LlmRequest, LlmResponse, ModelError};
use gadk_platform::Platform;
use gadk_session::{CreateRequest, Event, GetRequest, InMemorySessionService, SessionService};
use serde_json::Value;

use crate::llm::agent as llm_agent;
use crate::llm::state::reveal;
use crate::llm::{IncludeContents, LlmAgentConfig};
use crate::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
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

/// Port of Go `newModeTestCtx`: a fresh in-memory session holding `prior`, user content
/// "current question", invocation "inv-mode-test", StreamingMode none.
async fn new_mode_test_ctx(a: &Arc<dyn Agent>, prior: Vec<Event>) -> Context {
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
    Context::builder()
        .session_service(svc)
        .session(session)
        .agent(a.clone())
        .invocation_id("inv-mode-test")
        .user_content(Content::from_text("current question", "user"))
        .run_config(RunConfig::default())
        .build()
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
async fn run_root(r: &Runner, text: &str) -> Vec<gadk_agent::AgentError> {
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

/// Port of Go `raceFreeLLM`: one atomic counter, always answers "answer".
struct RaceFreeLlm {
    calls: AtomicUsize,
}

impl Llm for RaceFreeLlm {
    fn name(&self) -> &str {
        "race-free"
    }
    fn generate_content(
        &self,
        _req: LlmRequest,
        _stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        self.calls.fetch_add(1, SeqCst);
        futures::stream::iter([Ok(LlmResponse {
            content: Some(Content::from_text("answer", "model")),
            ..Default::default()
        })])
        .boxed()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Go `declaredMode`: `llminternal.Reveal(a).Mode`.
fn declared_mode(a: &Arc<dyn Agent>) -> Mode {
    reveal(a.as_ref())
        .unwrap_or_else(|| panic!("agent {:?} is not an LlmAgent", a.name()))
        .cfg
        .mode
}

/// Go `declaredIncludeContents`: `llminternal.Reveal(a).IncludeContents`.
fn declared_include_contents(a: &Arc<dyn Agent>) -> IncludeContents {
    reveal(a.as_ref())
        .unwrap_or_else(|| panic!("agent {:?} is not an LlmAgent", a.name()))
        .cfg
        .include_contents
}

/// Go `coordinatorToolNames`: the names of `Reveal(a).Tools`, sorted.
fn coordinator_tool_names(a: &Arc<dyn Agent>) -> Vec<String> {
    let st =
        reveal(a.as_ref()).unwrap_or_else(|| panic!("agent {:?} is not an LlmAgent", a.name()));
    let mut names: Vec<String> = st.tools().iter().map(|t| t.name().to_string()).collect();
    names.sort();
    names
}

#[test]
fn test_new_agent_node_does_not_mutate_agent_mode() {
    let a = llm(LlmAgentConfig {
        name: "worker".into(),
        ..Default::default()
    });
    assert_eq!(
        declared_mode(&a),
        Mode::Unset,
        "precondition: declared mode, want unset"
    );

    AgentNode::new(a.clone(), NodeConfig::default()).expect("NewAgentNode");

    assert_eq!(
        declared_mode(&a),
        Mode::Unset,
        "declared mode after NewAgentNode, want unset (wrapping must not mutate the agent)"
    );
}

#[test]
fn test_llm_agent_sub_agent_mode_resolution_is_construction_order_independent() {
    // Two sub-agents: an all-undeclared coordinator installs no tools at all, so the
    // declared one gives the comparison something to be wrong about.
    fn new_sub_agents() -> (Arc<dyn Agent>, Arc<dyn Agent>) {
        let sub = llm(LlmAgentConfig {
            name: "worker".into(),
            ..Default::default()
        });
        let dec = llm(LlmAgentConfig {
            name: "declared_worker".into(),
            mode: Mode::SingleTurn,
            ..Default::default()
        });
        (sub, dec)
    }
    fn coordinator(subs: Vec<Arc<dyn Agent>>) -> Arc<dyn Agent> {
        llm(LlmAgentConfig {
            name: "coordinator".into(),
            mode: Mode::Chat,
            sub_agents: subs,
            ..Default::default()
        })
    }

    // Order A: the coordinator is built first, then the same instance is placed in a graph.
    let (sub_a, dec_a) = new_sub_agents();
    let coord_a = coordinator(vec![sub_a.clone(), dec_a]);
    AgentNode::new(sub_a, NodeConfig::default()).expect("NewAgentNode");

    // Order B: the graph placement happens first.
    let (sub_b, dec_b) = new_sub_agents();
    AgentNode::new(sub_b.clone(), NodeConfig::default()).expect("NewAgentNode");
    let coord_b = coordinator(vec![sub_b, dec_b]);

    // The undeclared sub-agent is a chat peer in both orders (reached by transfer, no
    // delegation tool); the declared single_turn one gets a tool in both orders.
    let want = vec!["declared_worker".to_string()];
    let (got_a, got_b) = (
        coordinator_tool_names(&coord_a),
        coordinator_tool_names(&coord_b),
    );
    assert_eq!(
        got_a, got_b,
        "coordinator tools depend on construction order (coordinator-first vs node-first)"
    );
    assert_eq!(got_a, want, "coordinator tools");
}

#[test]
fn test_llm_agent_new_does_not_mutate_sub_agent_mode() {
    let sub = llm(LlmAgentConfig {
        name: "worker".into(),
        ..Default::default()
    });
    assert_eq!(
        declared_mode(&sub),
        Mode::Unset,
        "precondition: declared mode, want unset"
    );

    llm(LlmAgentConfig {
        name: "coordinator".into(),
        mode: Mode::Chat,
        sub_agents: vec![sub.clone()],
        ..Default::default()
    });

    assert_eq!(
        declared_mode(&sub),
        Mode::Unset,
        "declared mode after being adopted as a sub-agent, want unset (building a coordinator must not write to the sub-agent)"
    );
}

// Go's -race has no Rust equivalent; Send + Sync is enforced at compile time.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_llm_agent_new_concurrent_coordinators_over_one_sub_agent() {
    let sub = llm(LlmAgentConfig {
        name: "worker".into(),
        ..Default::default()
    });
    let handles: Vec<_> = (0..8)
        .map(|i| {
            let sub = sub.clone();
            tokio::spawn(async move {
                llm_agent::new(LlmAgentConfig {
                    name: format!("coordinator-{i}"),
                    mode: Mode::Chat,
                    sub_agents: vec![sub],
                    ..Default::default()
                })
            })
        })
        .collect();
    for h in handles {
        if let Err(e) = h.await.expect("join") {
            panic!("llmagent.New(coordinator): {e}");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_new_agent_node_concurrent_wrapping_is_race_free() {
    let a = llm(LlmAgentConfig {
        name: "worker".into(),
        ..Default::default()
    });
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let a = a.clone();
            tokio::spawn(async move { AgentNode::new(a, NodeConfig::default()).map(|_| ()) })
        })
        .collect();
    for h in handles {
        if let Err(e) = h.await.expect("join") {
            panic!("NewAgentNode: {e}");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_one_agent_instance_concurrent_invocations_are_race_free() {
    const RUNS: usize = 16;
    let model = Arc::new(RaceFreeLlm {
        calls: AtomicUsize::new(0),
    });
    let a = llm(LlmAgentConfig {
        name: "worker".into(),
        model: Some(model.clone() as Arc<dyn Llm>),
        ..Default::default()
    });
    let mut runs = Vec::new();
    for _ in 0..RUNS {
        runs.push((
            single_node_workflow(&a),
            new_mode_test_ctx(&a, vec![]).await,
        ));
    }
    let handles: Vec<_> = runs
        .into_iter()
        .map(|(wf, ctx)| {
            tokio::spawn(async move {
                drain_events(wf.run(ctx.as_invocation().clone()))
                    .await
                    .map(|_| ())
            })
        })
        .collect();
    for h in handles {
        if let Err(e) = h.await.expect("join") {
            panic!("workflow.Run: {e}");
        }
    }
    assert_eq!(
        model.calls.load(SeqCst),
        RUNS,
        "model call count — the runs did not reach the code this races"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_llm_agent_transfer_targets_are_construction_order_independent() {
    async fn coordinator_instruction(node_first: bool) -> String {
        let sub = llm(LlmAgentConfig {
            name: "worker".into(),
            description: "does the work".into(),
            ..Default::default()
        });
        if node_first {
            node_of(&sub);
        }
        let model = CapturingLlm::new();
        let coord = llm(LlmAgentConfig {
            name: "coordinator".into(),
            mode: Mode::Chat,
            model: Some(model.clone() as Arc<dyn Llm>),
            sub_agents: vec![sub.clone()],
            ..Default::default()
        });
        if !node_first {
            node_of(&sub);
        }
        let errs = run_root(&auto_runner(coord), "hi").await;
        assert!(errs.is_empty(), "Run: {errs:?}");
        assert!(
            model.got().is_some(),
            "the coordinator's model was never called, so this pins nothing"
        );
        model.system_instruction()
    }

    let coord_first = coordinator_instruction(false).await;
    let node_first = coordinator_instruction(true).await;

    assert!(
        coord_first.contains("transfer_to_agent"),
        "no transfer instructions in either order, so the comparison is vacuous; got {coord_first:?}"
    );
    assert!(
        coord_first.contains("worker"),
        "the undeclared sub-agent is not a transfer target when the coordinator is built first; got {coord_first:?}"
    );
    assert_eq!(
        coord_first, node_first,
        "the coordinator's transfer targets depend on construction order"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_agent_node_run_does_not_mutate_the_agents_include_contents() {
    let model = CapturingLlm::new();
    let a = llm(LlmAgentConfig {
        name: "worker".into(),
        model: Some(model.clone() as Arc<dyn Llm>),
        ..Default::default()
    });
    assert_eq!(
        declared_include_contents(&a),
        IncludeContents::Unset,
        "precondition: IncludeContents, want empty"
    );

    let wf = single_node_workflow(&a);
    let ctx = new_mode_test_ctx(&a, vec![]).await;
    drain_events(wf.run(ctx.as_invocation().clone()))
        .await
        .expect("wf.Run");
    assert!(
        model.got().is_some(),
        "the model was never called, so the single_turn placement was not exercised"
    );

    assert_eq!(
        declared_include_contents(&a),
        IncludeContents::Unset,
        "IncludeContents after a single_turn placement, want empty (a run must not mutate the agent)"
    );
    assert_eq!(
        declared_mode(&a),
        Mode::Unset,
        "declared mode after a single_turn placement, want unset"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_one_instance_a_node_run_does_not_decide_its_next_placement() {
    let model = Arc::new(RaceFreeLlm {
        calls: AtomicUsize::new(0),
    });
    let a = llm(LlmAgentConfig {
        name: "worker".into(),
        description: "w".into(),
        model: Some(model.clone() as Arc<dyn Llm>),
        instruction: "OWN".into(),
        ..Default::default()
    });

    let ctx = new_mode_test_ctx(&a, vec![]).await;
    drain_events(node_of(&a).run(ctx, Value::Null))
        .await
        .expect("node.Run");
    assert!(
        model.calls.load(SeqCst) > 0,
        "the model was never called, so the node run did not happen"
    );

    assert_eq!(
        declared_mode(&a),
        Mode::Unset,
        "Mode after a node run, want unset"
    );
    assert_eq!(
        declared_include_contents(&a),
        IncludeContents::Unset,
        "IncludeContents after a node run, want empty"
    );

    let errs = run_root(&auto_runner(a), "hello").await;
    assert!(
        errs.is_empty(),
        "running the same instance as a chat root after a node run: {errs:?}"
    );
}
