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

//! Port of Go agent/workflowagent/hitl_test.go (and the package's shared fixtures).

use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use chrono::{DateTime, Utc};
use futures::StreamExt;
use gadk_agent::{Agent, AgentError, Context, EventStream, InvocationContext};
use gadk_genai::{Content, FunctionResponse, Part};
use gadk_platform::Platform;
use gadk_session::{Event, RequestInput, Session, SessionError, State};
use parking_lot::Mutex;
use serde_json::{Map, Value, json};

use crate::agents::workflow_agent::{self, WorkflowAgentConfig};
/// Go workflow.Chain.
pub(super) use crate::workflow::chain;
use crate::workflow::{
    Edge, Node, NodeConfig, RunNodeOptions, START, WORKFLOW_INPUT_FUNCTION_CALL_NAME, dynamic_node,
    function_node, new_request_input_event, run_node,
};
use crate::{WorkflowError, WorkflowErrorKind, workflow_error};

/// Go fakeSession: records appended events and applies their state deltas.
pub(super) struct FakeSession {
    events: Mutex<Vec<Event>>,
    state: FakeState,
}

/// Go fakeSessionState.
pub(super) struct FakeState(Mutex<Map<String, Value>>);

impl State for FakeState {
    fn get(&self, key: &str) -> Result<Value, SessionError> {
        self.0
            .lock()
            .get(key)
            .cloned()
            .ok_or(SessionError::StateKeyNotExist)
    }

    /// Go: intentionally not persisted (real session services do not propagate a direct set from
    /// inside an invocation either).
    fn set(&self, _key: &str, _value: Value) -> Result<(), SessionError> {
        Ok(())
    }

    fn all(&self) -> Vec<(String, Value)> {
        self.0
            .lock()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

impl Session for FakeSession {
    fn id(&self) -> &str {
        "test-session-id"
    }
    fn app_name(&self) -> &str {
        ""
    }
    fn user_id(&self) -> &str {
        ""
    }
    fn state(&self) -> &dyn State {
        &self.state
    }
    fn events(&self) -> Vec<Event> {
        self.events.lock().clone()
    }
    fn last_update_time(&self) -> DateTime<Utc> {
        gadk_session::zero_time()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl FakeSession {
    pub(super) fn new() -> Arc<FakeSession> {
        Arc::new(FakeSession {
            events: Mutex::new(Vec::new()),
            state: FakeState(Mutex::new(Map::new())),
        })
    }

    pub(super) fn append_event(&self, ev: Event) {
        if let Some(delta) = &ev.actions.state_delta {
            let mut st = self.state.0.lock();
            for (k, v) in delta {
                st.insert(k.clone(), v.clone());
            }
        }
        self.events.lock().push(ev);
    }

    /// Go appendUserMessage: the runner appends the inbound turn before the agent runs.
    pub(super) fn append_user_message(&self, msg: &Content) {
        let mut ev = Event::new(&Platform::default(), "test-invocation-id");
        ev.author = "user".into();
        ev.response.content = Some(msg.clone());
        self.append_event(ev);
    }
}

type HitlFn = Arc<dyn Fn(Context, Value) -> Vec<Result<Event, AgentError>> + Send + Sync>;

/// Go hitlNode / reentryNode: a custom node whose body the test supplies.
pub(super) struct HitlNode {
    name: String,
    cfg: NodeConfig,
    body: HitlFn,
}

impl HitlNode {
    /// Go newHitlNode (returns the node as `Arc<dyn Node>`, as Go returns the node pointer).
    #[allow(clippy::new_ret_no_self)]
    pub(super) fn new(
        name: &str,
        cfg: NodeConfig,
        body: impl Fn(Context, Value) -> Vec<Result<Event, AgentError>> + Send + Sync + 'static,
    ) -> Arc<dyn Node> {
        Arc::new(HitlNode {
            name: name.into(),
            cfg,
            body: Arc::new(body),
        })
    }
}

impl Node for HitlNode {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        ""
    }
    fn config(&self) -> &NodeConfig {
        &self.cfg
    }
    fn input_schema(&self) -> Option<&Value> {
        None
    }
    fn output_schema(&self) -> Option<&Value> {
        None
    }
    fn validate_input(&self, v: Value) -> Result<Value, WorkflowError> {
        Ok(v)
    }
    fn validate_output(&self, v: Value) -> Result<Value, WorkflowError> {
        Ok(v)
    }
    fn run(&self, ctx: Context, input: Value) -> EventStream {
        futures::stream::iter((self.body)(ctx, input)).boxed()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Go workflow.Start.
pub(super) fn start() -> Arc<dyn Node> {
    START.clone()
}

/// Go makeAgent: `New(Config{Name: "test_workflow", Edges: edges})`.
pub(super) fn make_agent(edges: Vec<Edge>) -> Arc<dyn Agent> {
    workflow_agent::new(WorkflowAgentConfig {
        name: "test_workflow".into(),
        edges,
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("workflowagent.New: {e}"))
}

/// Go newMockCtx: appends the inbound turn to history (as the runner does), then builds the
/// invocation context.
pub(super) fn new_mock_ctx(
    sess: &Arc<FakeSession>,
    agt: &Arc<dyn Agent>,
    msg: Option<Content>,
) -> InvocationContext {
    if let Some(m) = &msg {
        sess.append_user_message(m);
    }
    let mut b = InvocationContext::builder()
        .session(sess.clone() as Arc<dyn Session>)
        .agent(agt.clone())
        .invocation_id("test-invocation-id");
    if let Some(m) = msg {
        b = b.user_content(m);
    }
    b.build_invocation()
}

/// Go drainAgent: collects and appends every event; checks the first error against `want`.
pub(super) async fn drain_agent(
    sess: &Arc<FakeSession>,
    mut s: EventStream,
    want: Option<WorkflowErrorKind>,
) -> Vec<Event> {
    let mut got = Vec::new();
    let mut saw: Option<AgentError> = None;
    while let Some(item) = s.next().await {
        match item {
            Ok(ev) => {
                sess.append_event(ev.clone());
                got.push(ev);
            }
            Err(e) => {
                if saw.is_none() {
                    saw = Some(e);
                }
            }
        }
    }
    match (want, &saw) {
        (None, Some(e)) => panic!("unexpected error from agent: {e}"),
        (Some(k), None) => panic!("expected error {k:?}, got none"),
        (Some(k), Some(e)) => assert!(
            workflow_error(e).is_some_and(|w| w.is(k)),
            "expected error {k:?}, got {e}"
        ),
        (None, None) => {}
    }
    got
}

/// Go findRequest: the first requested input's interrupt id, or "".
pub(super) fn find_request(events: &[Event]) -> String {
    events
        .iter()
        .find_map(|e| e.requested_input.as_ref().map(|r| r.interrupt_id.clone()))
        .unwrap_or_default()
}

/// Go resumeMessage.
pub(super) fn resume_message(interrupt_id: &str, payload: Value) -> Content {
    let mut resp = Map::new();
    resp.insert("payload".into(), payload);
    Content {
        role: String::new(),
        parts: vec![Part {
            function_response: Some(FunctionResponse {
                id: interrupt_id.into(),
                name: WORKFLOW_INPUT_FUNCTION_CALL_NAME.into(),
                response: resp,
                ..Default::default()
            }),
            ..Default::default()
        }],
    }
}

/// Go newAskerNode (every asker is named "asker", as in Go).
pub(super) fn new_asker_node(
    interrupt_id: &str,
    message: &str,
    schema: Option<Value>,
) -> Arc<dyn Node> {
    let (id, msg) = (interrupt_id.to_string(), message.to_string());
    HitlNode::new("asker", NodeConfig::default(), move |ctx, _input| {
        vec![Ok(new_request_input_event(
            &ctx,
            RequestInput {
                interrupt_id: id.clone(),
                message: msg.clone(),
                response_schema: schema.clone(),
                ..Default::default()
            },
        ))]
    })
}

/// Go runFreshTurn.
pub(super) async fn run_fresh_turn(
    sess: &Arc<FakeSession>,
    a: &Arc<dyn Agent>,
    text: &str,
) -> Vec<Event> {
    let msg = Content {
        role: String::new(),
        parts: vec![Part::from_text(text)],
    };
    drain_agent(sess, a.run(new_mock_ctx(sess, a, Some(msg))), None).await
}

/// Go newStringHandlerNode.
pub(super) fn new_string_handler_node(
    name: &str,
    dst: Arc<Mutex<Option<String>>>,
) -> Arc<dyn Node> {
    function_node::<String, String, _, _>(
        name,
        move |_ctx: Context, input: String| {
            let dst = dst.clone();
            async move {
                *dst.lock() = Some(input.clone());
                Ok(format!("handled:{input}"))
            }
        },
        NodeConfig::default(),
    )
}

/// Go newMapHandlerNode.
fn new_map_handler_node(name: &str, dst: Arc<Mutex<Option<Map<String, Value>>>>) -> Arc<dyn Node> {
    function_node::<Map<String, Value>, Value, _, _>(
        name,
        move |_ctx: Context, input: Map<String, Value>| {
            let dst = dst.clone();
            async move {
                *dst.lock() = Some(input);
                Ok(Value::Null)
            }
        },
        NodeConfig::default(),
    )
}

/// Go newCountingHandlerNode.
pub(super) fn new_counting_handler_node(name: &str, counter: Arc<AtomicI32>) -> Arc<dyn Node> {
    function_node::<Value, Value, _, _>(
        name,
        move |_ctx: Context, _input: Value| {
            let c = counter.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Ok(Value::Null)
            }
        },
        NodeConfig::default(),
    )
}

/// Go newFlagHandlerNode.
fn new_flag_handler_node(name: &str, flag: Arc<AtomicBool>) -> Arc<dyn Node> {
    function_node::<Value, Value, _, _>(
        name,
        move |_ctx: Context, _input: Value| {
            let f = flag.clone();
            async move {
                f.store(true, Ordering::SeqCst);
                Ok(Value::Null)
            }
        },
        NodeConfig::default(),
    )
}

/// Go approvalSchema.
fn approval_schema() -> Value {
    json!({"type": "object", "properties": {"approved": {"type": "boolean"}}, "required": ["approved"]})
}

#[tokio::test]
async fn test_workflow_agent_run_then_resume_handoff() {
    let handler_input = Arc::new(Mutex::new(None));
    let asker = new_asker_node("approve_or_reject", "Please decide", None);
    let handler = new_string_handler_node("handler", handler_input.clone());
    let a = make_agent(chain(&[start(), asker, handler]));
    let sess = FakeSession::new();

    // Turn 1: fresh run; pauses with a requested input.
    let turn1 = run_fresh_turn(&sess, &a, "draft").await;
    assert_eq!(find_request(&turn1), "approve_or_reject");
    assert_eq!(*handler_input.lock(), None, "handler ran during turn 1");

    // Turn 2: resume with a payload; the handler receives it as its input.
    let turn2 = drain_agent(
        &sess,
        a.run(new_mock_ctx(
            &sess,
            &a,
            Some(resume_message("approve_or_reject", json!("approve"))),
        )),
        None,
    )
    .await;
    assert_eq!(
        find_request(&turn2),
        "",
        "turn 2 unexpectedly emitted a RequestedInput"
    );
    assert_eq!(handler_input.lock().as_deref(), Some("approve"));
}

#[tokio::test]
async fn test_workflow_agent_resume_restores_state_from_session() {
    let called = Arc::new(AtomicBool::new(false));
    // Fresh node instances per agent, so resume provably works from session history.
    let make_nodes = |called: Arc<AtomicBool>| {
        (
            new_asker_node("human_approval", "approve?", None),
            new_flag_handler_node("handler", called),
        )
    };
    let sess = FakeSession::new();

    let (asker1, handler1) = make_nodes(called.clone());
    let a1 = make_agent(chain(&[start(), asker1, handler1]));
    assert_eq!(
        find_request(&run_fresh_turn(&sess, &a1, "draft").await),
        "human_approval",
        "first agent did not pause as expected"
    );

    let (asker2, handler2) = make_nodes(called.clone());
    let a2 = make_agent(chain(&[start(), asker2, handler2]));
    drain_agent(
        &sess,
        a2.run(new_mock_ctx(
            &sess,
            &a2,
            Some(resume_message("human_approval", json!("yes"))),
        )),
        None,
    )
    .await;
    assert!(
        called.load(Ordering::SeqCst),
        "handler did not run after resume on a fresh agent instance"
    );
}

#[tokio::test]
async fn test_workflow_agent_resume_idempotent() {
    let runs = Arc::new(AtomicI32::new(0));
    let asker = new_asker_node("approve", "?", None);
    let handler = new_counting_handler_node("handler", runs.clone());
    let a = make_agent(chain(&[start(), asker, handler]));
    let sess = FakeSession::new();

    run_fresh_turn(&sess, &a, "x").await;
    // First resume: matches the waiting node, runs the handler.
    drain_agent(
        &sess,
        a.run(new_mock_ctx(
            &sess,
            &a,
            Some(resume_message("approve", json!("yes"))),
        )),
        None,
    )
    .await;
    // Second resume with the same payload: the response was already consumed, so no waiting node
    // matches.
    drain_agent(
        &sess,
        a.run(new_mock_ctx(
            &sess,
            &a,
            Some(resume_message("approve", json!("yes"))),
        )),
        Some(WorkflowErrorKind::NothingToResume),
    )
    .await;
    assert_eq!(
        runs.load(Ordering::SeqCst),
        1,
        "duplicate Resume must not re-run the handler"
    );
}

#[tokio::test]
async fn test_workflow_agent_resume_no_matching_response() {
    let asker = new_asker_node("real_id", "?", None);
    let a = make_agent(chain(&[start(), asker]));
    let sess = FakeSession::new();

    run_fresh_turn(&sess, &a, "x").await;
    let turn = drain_agent(
        &sess,
        a.run(new_mock_ctx(
            &sess,
            &a,
            Some(resume_message("unknown_id", json!("x"))),
        )),
        Some(WorkflowErrorKind::NothingToResume),
    )
    .await;
    assert_eq!(
        find_request(&turn),
        "",
        "unmatched resume produced a new RequestedInput"
    );
}

#[tokio::test]
async fn test_workflow_agent_resume_schema_validation_pass() {
    let handler_input = Arc::new(Mutex::new(None));
    let asker = new_asker_node("approval", "decide", Some(approval_schema()));
    let handler = new_map_handler_node("handler", handler_input.clone());
    let a = make_agent(chain(&[start(), asker, handler]));
    let sess = FakeSession::new();

    run_fresh_turn(&sess, &a, "x").await;
    drain_agent(
        &sess,
        a.run(new_mock_ctx(
            &sess,
            &a,
            Some(resume_message("approval", json!({"approved": true}))),
        )),
        None,
    )
    .await;
    let got = handler_input.lock().clone();
    assert!(
        got.as_ref()
            .is_some_and(|m| m.get("approved") == Some(&json!(true))),
        "handler input = {got:?}, want map with approved=true"
    );
}

#[tokio::test]
async fn test_workflow_agent_resume_schema_validation_fail() {
    let runs = Arc::new(AtomicI32::new(0));
    let asker = new_asker_node("approval", "decide", Some(approval_schema()));
    let handler = new_counting_handler_node("handler", runs.clone());
    let a = make_agent(chain(&[start(), asker, handler]));
    let sess = FakeSession::new();

    run_fresh_turn(&sess, &a, "x").await;
    // An invalid payload (a string instead of {approved: bool}).
    drain_agent(
        &sess,
        a.run(new_mock_ctx(
            &sess,
            &a,
            Some(resume_message("approval", json!("not an object"))),
        )),
        Some(WorkflowErrorKind::InvalidResumeResponse),
    )
    .await;
    assert_eq!(
        runs.load(Ordering::SeqCst),
        0,
        "handler ran despite schema validation failure"
    );

    // Retry with a valid payload.
    drain_agent(
        &sess,
        a.run(new_mock_ctx(
            &sess,
            &a,
            Some(resume_message("approval", json!({"approved": true}))),
        )),
        None,
    )
    .await;
    assert_eq!(runs.load(Ordering::SeqCst), 1, "handler runs after retry");
}

#[tokio::test]
async fn test_workflow_agent_resume_fan_out() {
    let hits = Arc::new(AtomicI32::new(0));
    let asker = new_asker_node("fan", "?", None);
    let h1 = new_counting_handler_node("h1", hits.clone());
    let h2 = new_counting_handler_node("h2", hits.clone());
    let h3 = new_counting_handler_node("h3", hits.clone());
    let edge = |from: &Arc<dyn Node>, to: &Arc<dyn Node>| Edge {
        from: from.clone(),
        to: to.clone(),
        route: None,
    };
    let a = make_agent(vec![
        edge(&start(), &asker),
        edge(&asker, &h1),
        edge(&asker, &h2),
        edge(&asker, &h3),
    ]);
    let sess = FakeSession::new();

    run_fresh_turn(&sess, &a, "x").await;
    drain_agent(
        &sess,
        a.run(new_mock_ctx(
            &sess,
            &a,
            Some(resume_message("fan", json!("go"))),
        )),
        None,
    )
    .await;
    assert_eq!(hits.load(Ordering::SeqCst), 3, "successor hits");
}

#[tokio::test]
async fn test_workflow_agent_fresh_turn_not_mistaken_for_resume() {
    let first_run = Arc::new(AtomicBool::new(false));
    let second_run = Arc::new(AtomicBool::new(false));
    // Each asker flips its own flag before the request is yielded.
    let make_asker = |flag: Arc<AtomicBool>| {
        HitlNode::new("asker", NodeConfig::default(), move |ctx, _input| {
            flag.store(true, Ordering::SeqCst);
            vec![Ok(new_request_input_event(
                &ctx,
                RequestInput {
                    interrupt_id: "ask".into(),
                    message: "?".into(),
                    ..Default::default()
                },
            ))]
        })
    };

    let a = make_agent(chain(&[start(), make_asker(first_run.clone())]));
    let sess = FakeSession::new();
    run_fresh_turn(&sess, &a, "x").await;
    assert!(
        first_run.load(Ordering::SeqCst),
        "asker did not run on turn 1"
    );

    // Turn 2: another fresh user message (no FunctionResponse) runs the workflow afresh.
    let a2 = make_agent(chain(&[start(), make_asker(second_run.clone())]));
    run_fresh_turn(&sess, &a2, "fresh").await;
    assert!(
        second_run.load(Ordering::SeqCst),
        "a fresh user message was misinterpreted as a resume; asker did not run on turn 2"
    );
}

#[tokio::test]
async fn test_workflow_agent_run_then_resume_dynamic_node_orchestrator() {
    const INTERRUPT_ID: &str = "ask_name_dyn";

    let asker = HitlNode::new("ask_name", NodeConfig::default(), |ctx, _input| {
        if let Some(resp) = ctx.resumed_input(INTERRUPT_ID) {
            let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
            ev.output = Some(resp);
            return vec![Ok(ev)];
        }
        vec![Ok(new_request_input_event(
            &ctx,
            RequestInput {
                interrupt_id: INTERRUPT_ID.into(),
                message: "What's your name?".into(),
                ..Default::default()
            },
        ))]
    });

    let orchestrator = dynamic_node::<String, String, _, _>(
        "hitl_demo",
        move |nc: Context, _in: String, _emit| {
            let asker = asker.clone();
            async move {
                // Pause: the error is NodeInterrupted (swallowed by the dynamic node's run).
                // Resume: out is the child's response.
                let out = run_node(&nc, asker, Value::Null, RunNodeOptions::default()).await?;
                let name = out
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .unwrap_or("stranger")
                    .to_string();
                Ok(format!("Hello, {name}!"))
            }
        },
        NodeConfig::default(),
    );

    let a = make_agent(chain(&[start(), orchestrator]));
    let sess = FakeSession::new();

    let turn1 = run_fresh_turn(&sess, &a, "start").await;
    assert_eq!(find_request(&turn1), INTERRUPT_ID);

    let turn2 = drain_agent(
        &sess,
        a.run(new_mock_ctx(
            &sess,
            &a,
            Some(resume_message(INTERRUPT_ID, json!("Wolo"))),
        )),
        None,
    )
    .await;
    assert_eq!(
        find_request(&turn2),
        "",
        "turn 2 unexpectedly emitted a RequestedInput"
    );
    let got = turn2
        .iter()
        .filter(|ev| {
            ev.output.is_some()
                && ev
                    .node_info
                    .as_ref()
                    .is_some_and(|ni| ni.path.contains("hitl_demo"))
        })
        .filter_map(|ev| ev.output.clone())
        .next_back();
    assert_eq!(got, Some(json!("Hello, Wolo!")), "orchestrator output");
}
