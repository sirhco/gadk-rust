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

//! Port of Go agent/workflowagents/sequentialagent/agent_test.go.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::{self, BoxStream, StreamExt};
use gadk_agent::{
    Agent, AgentConfig, AgentError, AgentState, AgentType, EventStream, InvocationContext,
    LiveAgent, LiveRequest, LiveSession, RunConfig,
};
use gadk_genai::{Content, ROLE_MODEL, ROLE_USER};
use gadk_model::{Llm, LlmRequest, LlmResponse, ModelError};
use gadk_platform::Platform;
use gadk_session::{CreateRequest, Event, EventActions, InMemorySessionService, SessionService};
use pretty_assertions::assert_eq;
use serde_json::Map;
use tokio::sync::mpsc;

use crate::agents::sequential_agent::{self, SequentialAgentConfig};
use crate::llm::state::reveal;
use crate::llm::{LlmAgentConfig, agent as llm_agent};
use crate::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};

/// Go FakeLLM: replies "hello <id>".
pub(super) struct FakeLlm {
    pub(super) id: usize,
    pub(super) calls: AtomicUsize,
}

impl Llm for FakeLlm {
    fn name(&self) -> &str {
        "fake-llm"
    }
    fn generate_content(
        &self,
        _req: LlmRequest,
        _stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        stream::iter(vec![Ok(LlmResponse {
            content: Some(Content::from_text(format!("hello {}", self.id), ROLE_MODEL)),
            ..Default::default()
        })])
        .boxed()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Go failingLLM: fails every call, as a model does on a quota or auth error.
struct FailingLlm {
    calls: AtomicUsize,
}

impl Llm for FailingLlm {
    fn name(&self) -> &str {
        "failing-llm"
    }
    fn generate_content(
        &self,
        _req: LlmRequest,
        _stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        stream::iter(vec![Err(ModelError::Other("model unavailable".into()))]).boxed()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Go newCustomAgent.
pub(super) fn new_custom_agent(id: usize) -> Arc<dyn Agent> {
    llm_agent::new(LlmAgentConfig {
        name: format!("custom_agent_{id}"),
        model: Some(Arc::new(FakeLlm {
            id,
            calls: AtomicUsize::new(0),
        })),
        ..Default::default()
    })
    .unwrap()
}

/// Go newSequentialAgent.
pub(super) fn new_sequential_agent(sub_agents: Vec<Arc<dyn Agent>>, name: &str) -> Arc<dyn Agent> {
    sequential_agent::new(SequentialAgentConfig {
        agent: AgentConfig {
            name: name.into(),
            sub_agents,
            ..Default::default()
        },
    })
    .unwrap_or_else(|e| panic!("NewSequentialAgent() error = {e}"))
}

/// An expected LLM event: author + model text, empty (non-nil) state and artifact deltas.
fn want_event(id: usize) -> Event {
    let mut ev = Event {
        author: format!("custom_agent_{id}"),
        ..Default::default()
    };
    ev.response.content = Some(Content::from_text(format!("hello {id}"), ROLE_MODEL));
    ev.actions = EventActions {
        state_delta: Some(Map::new()),
        artifact_delta: Some(BTreeMap::new()),
        ..Default::default()
    };
    ev
}

/// Go cmpopts.IgnoreFields(session.Event{}, "ID", "Timestamp", "InvocationID").
fn normalize(mut ev: Event) -> Event {
    ev.id.clear();
    ev.invocation_id.clear();
    ev.timestamp = gadk_session::zero_time();
    ev
}

struct NewCase {
    name: &'static str,
    sub_agents: Vec<Arc<dyn Agent>>,
    want_events: Vec<Event>,
    want_err_message: Option<&'static str>,
}

#[tokio::test]
async fn test_new_sequential_agent() {
    let max_iterations = 0;
    let same_agent =
        new_sequential_agent(vec![new_custom_agent(1), new_custom_agent(2)], "same_agent");
    let tests = vec![
        NewCase {
            name: "ok",
            sub_agents: vec![new_custom_agent(0), new_custom_agent(1)],
            want_events: vec![want_event(0), want_event(1)],
            want_err_message: None,
        },
        NewCase {
            name: "ok with inner sequential",
            sub_agents: vec![
                new_custom_agent(0),
                new_sequential_agent(
                    vec![new_custom_agent(1), new_custom_agent(2)],
                    "test_agent1",
                ),
                new_custom_agent(3),
            ],
            want_events: vec![want_event(0), want_event(1), want_event(2), want_event(3)],
            want_err_message: None,
        },
        // As in Go, this case builds a valid tree and expects no events, so it passes vacuously.
        NewCase {
            name: "err with inner sequential with same name as root",
            sub_agents: vec![
                new_custom_agent(0),
                new_sequential_agent(
                    vec![new_custom_agent(1), new_custom_agent(2)],
                    "test_agent1",
                ),
                new_custom_agent(3),
            ],
            want_events: vec![],
            want_err_message: Some(
                r#"failed to create agent tree: agent names must be unique in the agent tree, found duplicate: "test_agent""#,
            ),
        },
        NewCase {
            name: "err with 2 levels of inner sequential with same name as root ",
            sub_agents: vec![
                new_custom_agent(0),
                new_sequential_agent(
                    vec![new_sequential_agent(
                        vec![new_custom_agent(1), new_custom_agent(2)],
                        "test_agent1",
                    )],
                    "test_agent",
                ),
                new_custom_agent(3),
            ],
            want_events: vec![],
            want_err_message: Some(
                r#"failed to create agent tree: agent names must be unique in the agent tree, found duplicate: "test_agent""#,
            ),
        },
        NewCase {
            name: "err with 2 levels of inner sequential with same name as parent ",
            sub_agents: vec![
                new_custom_agent(0),
                new_sequential_agent(
                    vec![new_sequential_agent(
                        vec![new_custom_agent(1), new_custom_agent(2)],
                        "test_agent1",
                    )],
                    "test_agent1",
                ),
                new_custom_agent(3),
            ],
            want_events: vec![],
            want_err_message: Some(
                r#"failed to create agent tree: agent names must be unique in the agent tree, found duplicate: "test_agent1""#,
            ),
        },
        NewCase {
            name: "err with repeated inner sequential",
            sub_agents: vec![
                new_custom_agent(0),
                same_agent.clone(),
                same_agent.clone(),
                new_custom_agent(3),
            ],
            want_events: vec![],
            want_err_message: Some(
                r#"failed to create base agent: error creating agent: subagent "same_agent" appears multiple times in subAgents"#,
            ),
        },
        NewCase {
            name: "err with repeated inner sequential in two levels",
            sub_agents: vec![
                new_custom_agent(0),
                new_sequential_agent(vec![same_agent.clone()], "test_agent1"),
                same_agent.clone(),
                new_custom_agent(3),
            ],
            want_events: vec![],
            want_err_message: Some(
                r#"failed to create agent tree: "same_agent" agent cannot have >1 parents, found: "test_agent1", "test_agent""#,
            ),
        },
    ];

    for tt in tests {
        let name = tt.name;
        let sequential = match sequential_agent::new(SequentialAgentConfig {
            agent: AgentConfig {
                name: "test_agent".into(),
                sub_agents: tt.sub_agents,
                ..Default::default()
            },
        }) {
            Ok(a) => a,
            Err(e) => {
                let want = tt
                    .want_err_message
                    .unwrap_or_else(|| panic!("{name}: NewSequentialAgent() error = {e}"));
                assert_eq!(want, e.to_string(), "{name}: err message mismatch");
                continue;
            }
        };

        let session_service = Arc::new(InMemorySessionService::new());
        let agent_runner = match Runner::new(RunnerConfig {
            app_name: "test_app".into(),
            agent: sequential,
            session_service: session_service.clone(),
            artifact_service: None,
            memory_service: None,
            plugin_config: PluginConfig::default(),
            auto_create_session: false,
            platform: Platform::default(),
            compaction: None,
        }) {
            Ok(r) => r,
            Err(e) => {
                let want = tt
                    .want_err_message
                    .unwrap_or_else(|| panic!("{name}: NewSequentialAgent() error = {e}"));
                assert_eq!(want, e.to_string(), "{name}: err message mismatch");
                continue;
            }
        };

        session_service
            .create(CreateRequest {
                app_name: "test_app".into(),
                user_id: "user_id".into(),
                session_id: Some("session_id".into()),
                state: None,
            })
            .await
            .unwrap();

        // Run twice: the second time the runner must determine which agent to use, and the result
        // must be the same. As in Go, the collected events carry over between the two runs.
        let mut got_events: Vec<Event> = Vec::new();
        for _ in 0..2 {
            let mut s = agent_runner.run(
                "user_id",
                "session_id",
                Some(Content::from_text("user input", ROLE_USER)),
                RunConfig::default(),
                RunOptions::default(),
            );
            while let Some(item) = s.next().await {
                let event = item.unwrap_or_else(|e| panic!("{name}: got unexpected error: {e}"));
                if max_iterations == 0 && got_events.len() == tt.want_events.len() {
                    break;
                }
                got_events.push(event);
            }
            drop(s);

            assert_eq!(
                got_events.len(),
                tt.want_events.len(),
                "{name}: unexpected event length"
            );
            for (i, got) in got_events.iter().enumerate() {
                assert_eq!(
                    tt.want_events[i],
                    normalize(got.clone()),
                    "{name}: event[{i}] mismatch"
                );
            }
        }
    }
}

type RunLiveFn = Arc<
    dyn Fn(InvocationContext) -> Result<(Arc<dyn LiveSession>, EventStream), AgentError>
        + Send
        + Sync,
>;

/// The LiveAgent half of Go mockLiveAgent: RunLive delegates to `run_live_fn`.
struct FnLive(RunLiveFn);

#[async_trait]
impl LiveAgent for FnLive {
    async fn run_live(
        &self,
        ctx: InvocationContext,
    ) -> Result<(Arc<dyn LiveSession>, EventStream), AgentError> {
        (self.0)(ctx)
    }
}

/// Go mockLiveAgent{Agent: agent.New(Config{Name}), runLiveFn}: a custom agent whose
/// `as_live()` is the closure.
fn mock_live_agent(name: &str, run_live_fn: RunLiveFn) -> Arc<dyn Agent> {
    gadk_agent::new_with_state(
        AgentConfig {
            name: name.into(),
            ..Default::default()
        },
        AgentState {
            agent_type: AgentType::CustomAgent,
            config: None,
            on_before_agent_callback_event: None,
            live: Some(Arc::new(FnLive(run_live_fn))),
        },
    )
    .unwrap()
}

/// Go dummyLiveSession: forwards every request to a channel and records Close.
struct DummyLiveSession {
    send_tx: mpsc::UnboundedSender<LiveRequest>,
    closed: AtomicBool,
}

impl DummyLiveSession {
    fn new() -> (Arc<DummyLiveSession>, mpsc::UnboundedReceiver<LiveRequest>) {
        let (send_tx, rx) = mpsc::unbounded_channel();
        (
            Arc::new(DummyLiveSession {
                send_tx,
                closed: AtomicBool::new(false),
            }),
            rx,
        )
    }
    fn closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl LiveSession for DummyLiveSession {
    async fn send(&self, req: LiveRequest) -> Result<(), AgentError> {
        let _ = self.send_tx.send(req);
        Ok(())
    }
    async fn close(&self) -> Result<(), AgentError> {
        self.closed.store(true, Ordering::SeqCst);
        Ok(())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A live agent whose session is `sess` and whose stream yields one event authored by `name`.
fn one_event_live_agent(name: &'static str, sess: Arc<DummyLiveSession>) -> Arc<dyn Agent> {
    mock_live_agent(
        name,
        Arc::new(move |ctx: InvocationContext| {
            let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
            ev.author = name.to_string();
            let s: EventStream = stream::iter(vec![Ok(ev)]).boxed();
            Ok((sess.clone() as Arc<dyn LiveSession>, s))
        }),
    )
}

/// Go mockInvocationContext{agent, invocationID}.
fn mock_ctx(agent: Arc<dyn Agent>, invocation_id: &str) -> InvocationContext {
    InvocationContext::builder()
        .agent(agent)
        .invocation_id(invocation_id)
        .build_invocation()
}

fn has_task_completed(agent: &Arc<dyn Agent>) -> bool {
    reveal(&**agent)
        .expect("an LLM agent")
        .tools
        .read()
        .iter()
        .any(|t| t.name() == "task_completed")
}

#[tokio::test]
async fn test_sequential_agent_run_live_injection() {
    let sub_agent1 = new_custom_agent(1);
    let sub_agent2 = new_custom_agent(2);
    let seq = sequential_agent::new(SequentialAgentConfig {
        agent: AgentConfig {
            name: "seq_agent".into(),
            sub_agents: vec![sub_agent1.clone(), sub_agent2],
            ..Default::default()
        },
    })
    .unwrap();

    // Before RunLive, sub-agents do not have the task_completed tool.
    assert!(
        !has_task_completed(&sub_agent1),
        "sub-agent 1 already has task_completed tool before RunLive"
    );

    let live = seq
        .as_live()
        .expect("sequential agent does not implement RunLive");
    // The injection happens before any sub-agent runs; the stream is never polled.
    let _ = live.run_live(mock_ctx(seq.clone(), "test_id")).await;

    assert!(
        has_task_completed(&sub_agent1),
        "sub-agent 1 does not have task_completed tool injected after RunLive"
    );
}

#[tokio::test]
async fn test_sequential_agent_run_live_nested_sequential_injection() {
    let sub_agent_a = new_custom_agent(1);
    let sub_agent_b = new_custom_agent(2);
    let inner_seq = new_sequential_agent(vec![sub_agent_b.clone()], "inner_seq");
    let outer_seq = new_sequential_agent(vec![sub_agent_a, inner_seq], "outer_seq");

    let live = outer_seq
        .as_live()
        .expect("sequential agent does not implement RunLive");
    let _ = live.run_live(mock_ctx(outer_seq.clone(), "test_id")).await;

    assert!(
        has_task_completed(&sub_agent_b),
        "nested sub-agent B does not have task_completed tool injected after RunLive"
    );
}

#[tokio::test]
async fn test_sequential_agent_run_live_non_live_composite_not_mutated() {
    let sub_agent_a = new_custom_agent(1);
    let sub_agent_b = new_custom_agent(2);
    // A composite agent that is not live-capable (a plain agent::new agent).
    let non_live_composite = gadk_agent::agent::new(AgentConfig {
        name: "non_live_composite".into(),
        sub_agents: vec![sub_agent_b.clone()],
        ..Default::default()
    })
    .unwrap();
    assert!(non_live_composite.as_live().is_none());
    let outer_seq = new_sequential_agent(vec![sub_agent_a, non_live_composite], "outer_seq");

    let live = outer_seq
        .as_live()
        .expect("sequential agent does not implement RunLive");
    let _ = live.run_live(mock_ctx(outer_seq.clone(), "test_id")).await;

    assert!(
        !has_task_completed(&sub_agent_b),
        "nested sub-agent B inside non-live composite must not have task_completed tool injected"
    );
}

/// Waits (up to 5 s, Go: the test context) for the request routed to a dummy session.
async fn recv_routed(rx: &mut mpsc::UnboundedReceiver<LiveRequest>) -> LiveRequest {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timeout waiting for the routed request")
        .expect("channel closed")
}

fn text_request(text: &str) -> LiveRequest {
    LiveRequest {
        content: Some(Content::from_text(text, "")),
        ..Default::default()
    }
}

#[tokio::test]
async fn test_sequential_agent_run_live_sequential_orchestration() {
    let (sub_sess1, mut rx1) = DummyLiveSession::new();
    let (sub_sess2, mut rx2) = DummyLiveSession::new();
    let live_agent1 = one_event_live_agent("sub_agent_1", sub_sess1.clone());
    let live_agent2 = one_event_live_agent("sub_agent_2", sub_sess2.clone());
    let seq_agent = new_sequential_agent(vec![live_agent1, live_agent2], "seq_agent");

    let live = seq_agent
        .as_live()
        .expect("sequential agent does not implement RunLive");
    let (sess, mut events) = live
        .run_live(mock_ctx(seq_agent.clone(), "test_inv_id"))
        .await
        .unwrap_or_else(|e| panic!("RunLive failed: {e}"));

    // Consume the first sub-agent event.
    let ev1 = events.next().await.expect("expected first event").unwrap();
    assert_eq!(ev1.author, "sub_agent_1");

    // Now the sequential session routes to sub-session 1.
    sess.send(text_request("to agent 1")).await.unwrap();
    let got1 = recv_routed(&mut rx1).await;
    assert_eq!(got1.content.unwrap().parts[0].text, "to agent 1");

    // Sub-session 1 completes, transitioning to agent 2.
    let ev2 = events.next().await.expect("expected second event").unwrap();
    assert_eq!(ev2.author, "sub_agent_2");

    sess.send(text_request("to agent 2")).await.unwrap();
    let got2 = recv_routed(&mut rx2).await;
    assert_eq!(got2.content.unwrap().parts[0].text, "to agent 2");

    assert!(
        sub_sess1.closed(),
        "expected sub_agent_1 session to be closed after transition"
    );

    // Sub-session 2 completes.
    assert!(
        events.next().await.is_none(),
        "expected iterator to be exhausted"
    );
    assert!(
        sub_sess2.closed(),
        "expected sub_agent_2 session to be closed at the end"
    );
}

#[tokio::test]
async fn test_sequential_agent_run_live_nested_sequential_orchestration() {
    let (sub_sess1, mut rx1) = DummyLiveSession::new();
    let (sub_sess2, mut rx2) = DummyLiveSession::new();
    let live_agent1 = one_event_live_agent("sub_agent_1", sub_sess1.clone());
    let live_agent2 = one_event_live_agent("sub_agent_2", sub_sess2.clone());
    let inner_seq = new_sequential_agent(vec![live_agent2], "inner_seq");
    let outer_seq = new_sequential_agent(vec![live_agent1, inner_seq], "outer_seq");

    let live = outer_seq
        .as_live()
        .expect("sequential agent does not implement RunLive");
    let (sess, mut events) = live
        .run_live(mock_ctx(outer_seq.clone(), "test_inv_id"))
        .await
        .unwrap_or_else(|e| panic!("RunLive failed: {e}"));

    let ev1 = events.next().await.expect("expected first event").unwrap();
    assert_eq!(ev1.author, "sub_agent_1");

    sess.send(text_request("to agent 1")).await.unwrap();
    let got1 = recv_routed(&mut rx1).await;
    assert_eq!(got1.content.unwrap().parts[0].text, "to agent 1");

    // Sub-session 1 completes, transitioning to the nested inner_seq → agent 2.
    let ev2 = events.next().await.expect("expected second event").unwrap();
    assert_eq!(ev2.author, "sub_agent_2");

    // Routed outer → inner sequential session → sub-session 2.
    sess.send(text_request("to agent 2")).await.unwrap();
    let got2 = recv_routed(&mut rx2).await;
    assert_eq!(got2.content.unwrap().parts[0].text, "to agent 2");

    assert!(
        sub_sess1.closed(),
        "expected sub_agent_1 session to be closed after transition"
    );

    assert!(
        events.next().await.is_none(),
        "expected iterator to be exhausted"
    );
    assert!(
        sub_sess2.closed(),
        "expected sub_agent_2 session to be closed at the end"
    );
}

#[tokio::test]
async fn test_sequential_agent_stops_on_sub_agent_error() {
    let first_llm = Arc::new(FailingLlm {
        calls: AtomicUsize::new(0),
    });
    let first = llm_agent::new(LlmAgentConfig {
        name: "first".into(),
        model: Some(first_llm.clone()),
        ..Default::default()
    })
    .unwrap();
    let second_llm = Arc::new(FakeLlm {
        id: 1,
        calls: AtomicUsize::new(0),
    });
    let second = llm_agent::new(LlmAgentConfig {
        name: "second".into(),
        model: Some(second_llm.clone()),
        ..Default::default()
    })
    .unwrap();
    let r = Runner::new(RunnerConfig {
        app_name: "test_app".into(),
        agent: new_sequential_agent(vec![first, second], "pipeline"),
        session_service: Arc::new(InMemorySessionService::new()),
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig::default(),
        auto_create_session: true,
        platform: Platform::default(),
        compaction: None,
    })
    .unwrap();

    // Keep reading after an error, as the run_sse handler and the console launcher do.
    let mut errs = Vec::new();
    let mut s = r.run(
        "user_id",
        "session_id",
        Some(Content::from_text("go", ROLE_USER)),
        RunConfig::default(),
        RunOptions::default(),
    );
    while let Some(item) = s.next().await {
        if let Err(e) = item {
            errs.push(e.to_string());
        }
    }

    assert!(
        errs.len() == 1 && errs[0].contains("model unavailable"),
        "got errors {errs:?}, want exactly one from the first agent's model"
    );
    assert_eq!(
        first_llm.calls.load(Ordering::SeqCst),
        1,
        "first agent's model calls"
    );
    assert_eq!(
        second_llm.calls.load(Ordering::SeqCst),
        0,
        "second agent's model called after the first agent failed"
    );
}

/// The live counterpart of `test_sequential_agent_stops_on_sub_agent_error`: a sub-agent whose
/// live stream yields an error ends the sequence, and the next sub-agent is never started.
#[tokio::test]
#[ignore = "8a bug: sequential RunLive keeps running the next sub-agent after a sub-agent's live stream yields an error (Go #2151 stops, as Run does)"]
async fn test_sequential_agent_run_live_stops_on_sub_agent_error() {
    let (first_sess, _first_rx) = DummyLiveSession::new();
    let first_sess2 = first_sess.clone();
    let first = mock_live_agent(
        "first",
        Arc::new(move |_ctx: InvocationContext| {
            let s: EventStream =
                stream::iter(vec![Err(AgentError::other("live connection failed"))]).boxed();
            Ok((first_sess2.clone() as Arc<dyn LiveSession>, s))
        }),
    );
    let second_started = Arc::new(AtomicBool::new(false));
    let second_started2 = second_started.clone();
    let second = mock_live_agent(
        "second",
        Arc::new(move |ctx: InvocationContext| {
            second_started2.store(true, Ordering::SeqCst);
            let (sess, _rx) = DummyLiveSession::new();
            let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
            ev.author = "second".into();
            let s: EventStream = stream::iter(vec![Ok(ev)]).boxed();
            Ok((sess as Arc<dyn LiveSession>, s))
        }),
    );

    let seq = new_sequential_agent(vec![first, second], "pipeline");
    let live = seq
        .as_live()
        .expect("sequential agent does not implement RunLive");
    let (_sess, mut seq_stream) = live
        .run_live(mock_ctx(seq.clone(), "inv"))
        .await
        .unwrap_or_else(|e| panic!("RunLive failed: {e}"));

    // Keep reading after an error, as the runner does.
    let mut errs = Vec::new();
    let mut authors = Vec::new();
    while let Some(item) = seq_stream.next().await {
        match item {
            Err(e) => errs.push(e.to_string()),
            Ok(ev) => authors.push(ev.author),
        }
    }

    assert!(
        errs.len() == 1 && errs[0].contains("live connection failed"),
        "got errors {errs:?}, want exactly one from the first sub-agent"
    );
    assert!(
        !second_started.load(Ordering::SeqCst),
        "second sub-agent was started after the first one failed; events from: {authors:?}"
    );
    assert!(
        first_sess.closed(),
        "first sub-agent's live session was not closed after its error"
    );
}

/// An ordinary turn does not read the session to look for a resume target.
#[tokio::test]
async fn test_sequential_agent_run_without_session_or_function_response() {
    let ran = Arc::new(AtomicBool::new(false));
    let ran2 = ran.clone();
    let sub = gadk_agent::new_agent(AgentConfig {
        name: "sub".into(),
        run: Some(Arc::new(move |_ctx: InvocationContext| {
            ran2.store(true, Ordering::SeqCst);
            stream::empty().boxed()
        })),
        ..Default::default()
    })
    .unwrap();
    let root = new_sequential_agent(vec![sub], "root");
    // Go icontext.NewInvocationContext(ctx, {Agent: root}): no session, no user content.
    let ctx = InvocationContext::builder()
        .agent(root.clone())
        .build_invocation();
    let mut s = root.run(ctx);
    while let Some(item) = s.next().await {
        if let Err(e) = item {
            panic!("{e}");
        }
    }
    assert!(ran.load(Ordering::SeqCst), "sub-agent did not run");
}

/// A FindAgent case: (name, root, target name, want).
type FindAgentCase<'a> = (
    &'a str,
    &'a Arc<dyn Agent>,
    &'a str,
    Option<&'a Arc<dyn Agent>>,
);

#[test]
fn test_sequential_agent_find_agent_returns_sequential_agent() {
    let child = new_custom_agent(0);
    let seq = new_sequential_agent(vec![child.clone()], "seq");
    let llm_parent = llm_agent::new(LlmAgentConfig {
        name: "llm_parent".into(),
        sub_agents: vec![seq.clone()],
        ..Default::default()
    })
    .unwrap();
    let base_parent = gadk_agent::new_agent(AgentConfig {
        name: "base_parent".into(),
        sub_agents: vec![seq.clone()],
        ..Default::default()
    })
    .unwrap();

    let addr = |a: &Arc<dyn Agent>| Arc::as_ptr(a).cast::<()>();
    let cases: Vec<FindAgentCase<'_>> = vec![
        ("own name", &seq, "seq", Some(&seq)),
        ("through llm agent parent", &llm_parent, "seq", Some(&seq)),
        ("through base agent parent", &base_parent, "seq", Some(&seq)),
        ("sub-agent", &seq, "custom_agent_0", Some(&child)),
        ("unknown name", &seq, "unknown", None),
    ];
    for (name, root, target, want) in cases {
        let got = root.find_agent(target);
        assert_eq!(
            got.as_ref().map(addr),
            want.map(addr),
            "{name}: FindAgent({target:?})"
        );
    }
}
