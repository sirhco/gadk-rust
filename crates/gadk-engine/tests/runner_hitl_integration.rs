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
//! Port of ADK-Go runner/hitl_integration_test.go.

use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering::SeqCst};

use futures::StreamExt;
use futures::stream::BoxStream;
use gadk_agent::{Agent, Context, EventStream, RunConfig};
use gadk_engine::WorkflowError;
use gadk_engine::agents::workflow_agent::{self, WorkflowAgentConfig};
use gadk_engine::llm::LlmAgentConfig;
use gadk_engine::llm::agent as llm_agent;
use gadk_engine::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use gadk_engine::workflow::{
    BaseNode, Edge, Emit, Node, NodeConfig, RunNodeOptions, START,
    WORKFLOW_INPUT_FUNCTION_CALL_NAME, chain, dynamic_node, function_node, new_request_input_event,
    run_node,
};
use gadk_genai::{Content, FunctionCall, FunctionResponse};
use gadk_model::{Llm, LlmRequest, LlmResponse, ModelError};
use gadk_platform::Platform;
use gadk_session::{CreateRequest, Event, InMemorySessionService, RequestInput, SessionService};
use parking_lot::Mutex;
use serde_json::{Value, json};

/// Go `for ev, err := range seq { if err != nil { t.Fatal(err) } }`: every event (with or
/// without content), stopping at the first error.
async fn drain_events(s: gadk_agent::EventStream) -> Result<Vec<Event>, gadk_agent::AgentError> {
    futures::TryStreamExt::try_collect(s).await
}

const NODE_TEST_APP: &str = "node_test_app";
const NODE_TEST_USER: &str = "u";
const NODE_TEST_SESSION: &str = "s";

fn start() -> Arc<dyn Node> {
    Arc::clone(&START)
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

/// Port of Go `resumeContent`: one FunctionResponse with the payload under "payload".
fn resume_content(call_id: &str, call_name: &str, payload: Value) -> Content {
    Content::from_function_response(
        FunctionResponse {
            id: call_id.into(),
            name: call_name.into(),
            response: json!({ "payload": payload })
                .as_object()
                .cloned()
                .unwrap_or_default(),
            ..Default::default()
        },
        "user",
    )
}

/// Port of Go `findLongRunningInterrupt`.
fn find_long_running_interrupt(events: &[Event]) -> (String, String) {
    for ev in events {
        let Some(content) = &ev.response.content else {
            continue;
        };
        for p in &content.parts {
            if let Some(fc) = &p.function_call
                && ev.long_running_tool_ids.contains(&fc.id)
            {
                return (fc.id.clone(), fc.name.clone());
            }
        }
    }
    (String::new(), String::new())
}

/// Port of Go `drainRunner`: every event, panicking on the first error.
async fn drain(s: EventStream) -> Vec<Event> {
    drain_events(s).await.expect("runner yielded error")
}

/// Port of Go `debugEvents`.
fn debug_events(events: &[Event]) -> String {
    events
        .iter()
        .enumerate()
        .map(|(i, e)| {
            format!(
                "  [{i:02}] author={} lrt={:?} requested_input={:?} output={:?} content={:?}\n",
                e.author,
                e.long_running_tool_ids,
                e.requested_input.as_ref().map(|r| r.interrupt_id.clone()),
                e.output,
                e.response.content
            )
        })
        .collect()
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
    })
    .expect("runner.New()")
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

/// Go `workflowagent.New(workflowagent.Config{Name, SubAgents, Edges})`.
fn workflow_agent_of(
    name: &str,
    edges: Vec<Edge>,
    sub_agents: Vec<Arc<dyn Agent>>,
) -> Arc<dyn Agent> {
    workflow_agent::new(WorkflowAgentConfig {
        name: name.into(),
        sub_agents,
        edges,
        ..Default::default()
    })
    .expect("workflowagent.New()")
}

/// Port of Go `scriptedModel`: one response per call; past the end, repeats the last.
struct ScriptedModel {
    responses: Vec<Content>,
    call: Mutex<usize>,
}

impl ScriptedModel {
    fn new(responses: Vec<Content>) -> Arc<Self> {
        Arc::new(Self {
            responses,
            call: Mutex::new(0),
        })
    }
}

impl Llm for ScriptedModel {
    fn name(&self) -> &str {
        "scripted"
    }
    fn generate_content(
        &self,
        _req: LlmRequest,
        _stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        let mut call = self.call.lock();
        let i = (*call).min(self.responses.len() - 1);
        *call += 1;
        let resp = LlmResponse {
            content: Some(self.responses[i].clone()),
            ..Default::default()
        };
        futures::stream::iter([Ok(resp)]).boxed()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Go `workflowAgentName`; also the Author stamped on the agent's events.
const WORKFLOW_AGENT_NAME: &str = "test_workflow";

/// Go `newWorkflowRunner`: a workflow agent over `edges` with its session pre-created.
async fn new_workflow_runner(edges: Vec<Edge>) -> Runner {
    let wf_agent = workflow_agent_of(WORKFLOW_AGENT_NAME, edges, vec![]);
    let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    new_node_test_session(&svc).await;
    new_node_test_runner(wf_agent, svc)
}

/// Port of Go `hitlAskerNode`: requests input; on re-entry emits the resumed response as output.
struct HitlAsker {
    base: BaseNode,
    interrupt_id: String,
}

fn new_hitl_asker(name: &str, interrupt_id: &str, rerun_on_resume: bool) -> Arc<dyn Node> {
    let cfg = NodeConfig {
        rerun_on_resume: rerun_on_resume.then_some(true),
        ..Default::default()
    };
    Arc::new(HitlAsker {
        base: BaseNode::new(name, "", cfg),
        interrupt_id: interrupt_id.into(),
    })
}

impl Node for HitlAsker {
    fn name(&self) -> &str {
        self.base.name()
    }
    fn description(&self) -> &str {
        self.base.description()
    }
    fn config(&self) -> &NodeConfig {
        self.base.config()
    }
    fn input_schema(&self) -> Option<&Value> {
        self.base.input_schema()
    }
    fn output_schema(&self) -> Option<&Value> {
        self.base.output_schema()
    }
    fn validate_input(&self, v: Value) -> Result<Value, WorkflowError> {
        self.base.validate_input(v)
    }
    fn validate_output(&self, v: Value) -> Result<Value, WorkflowError> {
        self.base.validate_output(v)
    }
    fn run(&self, ctx: Context, input: Value) -> EventStream {
        let ev = match ctx.resumed_input(&self.interrupt_id) {
            Some(response) => {
                let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
                ev.output = Some(response);
                ev
            }
            None => new_request_input_event(
                &ctx,
                RequestInput {
                    interrupt_id: self.interrupt_id.clone(),
                    message: "please decide".into(),
                    payload: (!input.is_null()).then_some(input), // Go Payload: input (nil stays nil)
                    ..Default::default()
                },
            ),
        };
        futures::stream::iter([Ok(ev)]).boxed()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Go `handler` function node: records its input, returns "handled:" + input.
fn recording_handler(slot: &Arc<Mutex<Option<String>>>) -> Arc<dyn Node> {
    let slot = slot.clone();
    function_node::<String, String, _, _>(
        "handler",
        move |_ctx: Context, input: String| {
            let slot = slot.clone();
            async move {
                *slot.lock() = Some(input.clone());
                Ok(format!("handled:{input}"))
            }
        },
        NodeConfig::default(),
    )
}

#[tokio::test]
async fn test_runner_workflow_hitl_roundtrip_handoff() {
    let asker = new_hitl_asker("asker", "", false);
    let handler_input = Arc::new(Mutex::new(None::<String>));
    let handler = recording_handler(&handler_input);
    let r = new_workflow_runner(chain(&[start(), asker, handler])).await;

    // Turn 1: fresh message; the workflow pauses at the asker.
    let turn1 = drain(run(&r, user_text("draft"))).await;
    let (call_id, call_name) = find_long_running_interrupt(&turn1);
    assert!(
        !call_id.is_empty(),
        "turn 1 produced no LongRunning interrupt event; events:\n{}",
        debug_events(&turn1)
    );
    assert_eq!(
        call_name, WORKFLOW_INPUT_FUNCTION_CALL_NAME,
        "turn 1 interrupt FunctionCall name"
    );
    assert_eq!(*handler_input.lock(), None, "handler ran during turn 1");

    // Turn 2: resume with a FunctionResponse carrying the captured id.
    let turn2 = drain(run(
        &r,
        resume_content(&call_id, &call_name, json!("approve")),
    ))
    .await;
    let (id2, _) = find_long_running_interrupt(&turn2);
    assert!(
        id2.is_empty(),
        "turn 2 unexpectedly produced another interrupt: id={id2:?}"
    );
    assert_eq!(
        handler_input.lock().as_deref(),
        Some("approve"),
        "handler input"
    );
}

#[tokio::test]
async fn test_runner_workflow_hitl_roundtrip_re_entry() {
    let asker = new_hitl_asker("asker", "approval", true);
    let handler_input = Arc::new(Mutex::new(None::<String>));
    let handler = recording_handler(&handler_input);
    let r = new_workflow_runner(chain(&[start(), asker, handler])).await;

    let turn1 = drain(run(&r, user_text("draft"))).await;
    let (call_id, call_name) = find_long_running_interrupt(&turn1);
    assert_eq!(
        call_id, "approval",
        "turn 1 interrupt ID (asker supplied a stable InterruptID)"
    );

    drain(run(&r, resume_content(&call_id, &call_name, json!("yes")))).await;
    assert_eq!(
        handler_input.lock().as_deref(),
        Some("yes"),
        "handler input (re-entry asker should emit the response as its output)"
    );
}

#[tokio::test]
async fn test_runner_workflow_hitl_function_response_routed_by_id() {
    let asker = new_hitl_asker("asker", "", false);
    let r = new_workflow_runner(chain(&[start(), asker])).await;

    let turn1 = drain(run(&r, user_text("x"))).await;
    let (call_id, call_name) = find_long_running_interrupt(&turn1);
    assert!(!call_id.is_empty(), "no interrupt produced on turn 1");

    let authored_by = turn1
        .iter()
        .map(|e| e.author.as_str())
        .find(|a| !a.is_empty())
        .unwrap_or("");
    assert_eq!(
        authored_by, WORKFLOW_AGENT_NAME,
        "interrupt event Author (used to route the resume back to the right agent)"
    );

    let turn2 = drain(run(&r, resume_content(&call_id, &call_name, json!("ok")))).await;
    let (id2, _) = find_long_running_interrupt(&turn2);
    assert!(
        id2.is_empty(),
        "turn 2 produced a fresh interrupt instead of resuming; id={id2:?}"
    );
}

#[tokio::test]
async fn test_runner_nested_workflow_agent_under_llm_agent_resume() {
    const INTERRUPT_ID: &str = "sub_approval";
    let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    new_node_test_session(&svc).await;

    let asker = new_hitl_asker("asker", INTERRUPT_ID, true);
    let handler_input = Arc::new(Mutex::new(None::<String>));
    let handler = recording_handler(&handler_input);
    let sub = workflow_agent_of("subagent", chain(&[start(), asker, handler]), vec![]);

    let model = ScriptedModel::new(vec![
        model_call("transfer_to_agent", json!({"agent_name": "subagent"})),
        model_text("done"),
    ]);
    let orchestrator = llm_agent::new(LlmAgentConfig {
        name: "orchestrator".into(),
        model: Some(model as Arc<dyn Llm>),
        sub_agents: vec![sub],
        ..Default::default()
    })
    .expect("llmagent.New()");
    let r = new_node_test_runner(orchestrator, svc);

    let turn1 = drain(run(&r, user_text("ask the subagent"))).await;
    let (call_id, call_name) = find_long_running_interrupt(&turn1);
    assert_eq!(
        call_id,
        INTERRUPT_ID,
        "turn 1 interrupt ID; events:\n{}",
        debug_events(&turn1)
    );

    let turn2 = drain(run(
        &r,
        resume_content(&call_id, &call_name, json!("approve")),
    ))
    .await;
    let (id2, _) = find_long_running_interrupt(&turn2);
    assert!(
        id2.is_empty(),
        "sub-agent restarted with fresh interrupt {id2:?} on turn 2 instead of resuming; events:\n{}",
        debug_events(&turn2)
    );
    assert_eq!(
        handler_input.lock().as_deref(),
        Some("approve"),
        "handler input"
    );
}

#[tokio::test]
async fn test_runner_workflow_hitl_dynamic_orchestrator_resume() {
    const INTERRUPT_ID: &str = "approval";
    let first_child_runs = Arc::new(AtomicI32::new(0));
    let runs = first_child_runs.clone();
    let first_child = function_node::<String, String, _, _>(
        "first_child",
        move |_ctx: Context, input: String| {
            runs.fetch_add(1, SeqCst);
            async move { Ok(format!("X:{input}")) }
        },
        NodeConfig::default(),
    );
    let second_child = new_hitl_asker("second_child", INTERRUPT_ID, true);

    let parent_output = Arc::new(Mutex::new(None::<String>));
    let out_slot = parent_output.clone();
    let orchestrator = dynamic_node::<String, String, _, _>(
        "orchestrate",
        move |ctx: Context, input: String, _emit: Emit| {
            let (first, second, out_slot) =
                (first_child.clone(), second_child.clone(), out_slot.clone());
            async move {
                let x = run_node(
                    &ctx,
                    first,
                    json!(input),
                    RunNodeOptions {
                        run_id: Some("c1".into()),
                        ..Default::default()
                    },
                )
                .await?;
                let y = run_node(
                    &ctx,
                    second,
                    Value::Null,
                    RunNodeOptions {
                        run_id: Some("c2".into()),
                        ..Default::default()
                    },
                )
                .await?;
                let decision = y.as_str().unwrap_or("");
                let out = format!("{}|Y:{decision}", x.as_str().unwrap_or(""));
                *out_slot.lock() = Some(out.clone());
                Ok(out)
            }
        },
        NodeConfig::default(),
    );

    let r = new_workflow_runner(chain(&[start(), orchestrator])).await;

    let turn1 = drain(run(&r, user_text("draft"))).await;
    let (call_id, call_name) = find_long_running_interrupt(&turn1);
    assert_eq!(
        call_id,
        INTERRUPT_ID,
        "turn 1 interrupt ID; events:\n{}",
        debug_events(&turn1)
    );
    assert_eq!(
        first_child_runs.load(SeqCst),
        1,
        "first child runs on turn 1"
    );

    drain(run(&r, resume_content(&call_id, &call_name, json!("yes")))).await;

    assert_eq!(
        first_child_runs.load(SeqCst),
        2,
        "first child runs total (resume re-executes the orchestrator under a new invocation)"
    );
    assert_eq!(
        parent_output.lock().as_deref(),
        Some("X:draft|Y:yes"),
        "parent output (re-run first child + resumed second child)"
    );
}

#[tokio::test]
async fn test_runner_workflow_hitl_two_full_cycles_same_session() {
    let asker = new_hitl_asker("asker", "", false);
    let handler_input = Arc::new(Mutex::new(None::<String>));
    let handler = recording_handler(&handler_input);
    let r = new_workflow_runner(chain(&[start(), asker, handler])).await;

    let run_cycle = |answer: &'static str| {
        let (r, handler_input) = (&r, handler_input.clone());
        async move {
            let turn1 = drain(run(r, user_text("draft"))).await;
            let (call_id, call_name) = find_long_running_interrupt(&turn1);
            assert!(
                !call_id.is_empty(),
                "fresh turn produced no interrupt; events:\n{}",
                debug_events(&turn1)
            );
            drain(run(r, resume_content(&call_id, &call_name, json!(answer)))).await;
            assert_eq!(
                handler_input.lock().as_deref(),
                Some(answer),
                "handler input"
            );
        }
    };

    run_cycle("first").await;
    run_cycle("second").await;
}
