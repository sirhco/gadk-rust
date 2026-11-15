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

//! Port of Go runner/live_runner_test.go.

use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use futures::StreamExt;
use gadk_agent::{
    Agent, AgentConfig, AgentError, AgentState, AgentType, EventStream, InvocationContext,
    LiveAgent, LiveRequest, LiveRunConfig, LiveSession, Plugin, new_with_state,
};
use gadk_genai::{
    Blob, Content, FunctionCall, FunctionResponse, Part, ROLE_MODEL, ROLE_USER, Transcription,
};
use gadk_model::RealtimeInput;
use gadk_platform::Platform;
use gadk_session::{
    CreateRequest, DeleteRequest, Event, GetRequest, InMemorySessionService, ListRequest, Session,
    SessionError, SessionService,
};
use parking_lot::Mutex;
use serde_json::json;

use crate::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};

const APP: &str = "testApp";
const USER: &str = "testUser";

/// The producer of a mock agent's live event stream (Go `runLiveFn`'s iterator).
type Sequence = Arc<dyn Fn(InvocationContext) -> EventStream + Send + Sync>;

/// Go `mockLiveAgent.RunLive`: the live hook of a plain custom agent.
struct MockLive {
    session: Arc<dyn LiveSession>,
    sequence: Sequence,
    called: Arc<AtomicBool>,
}

#[async_trait]
impl LiveAgent for MockLive {
    async fn run_live(
        &self,
        ctx: InvocationContext,
    ) -> Result<(Arc<dyn LiveSession>, EventStream), AgentError> {
        self.called.store(true, Ordering::SeqCst);
        Ok((self.session.clone(), (self.sequence)(ctx)))
    }
}

/// Go `&mockLiveAgent{Agent: agent.New(agent.Config{Name: name}), runLiveFn: …}`.
fn mock_live_agent(
    name: &str,
    session: Arc<dyn LiveSession>,
    sequence: Sequence,
    called: Arc<AtomicBool>,
) -> Arc<dyn Agent> {
    new_with_state(
        AgentConfig {
            name: name.into(),
            ..Default::default()
        },
        AgentState {
            agent_type: AgentType::CustomAgent,
            config: None,
            on_before_agent_callback_event: None,
            live: Some(Arc::new(MockLive {
                session,
                sequence,
                called,
            })),
        },
    )
    .unwrap()
}

/// Go `dummyLiveSession`.
struct DummyLiveSession;

#[async_trait]
impl LiveSession for DummyLiveSession {
    async fn send(&self, _req: LiveRequest) -> Result<(), AgentError> {
        Ok(())
    }
    async fn close(&self) -> Result<(), AgentError> {
        Ok(())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Go `recordingLiveSession`.
#[derive(Default)]
struct RecordingLiveSession {
    requests: Mutex<Vec<LiveRequest>>,
}

#[async_trait]
impl LiveSession for RecordingLiveSession {
    async fn send(&self, req: LiveRequest) -> Result<(), AgentError> {
        self.requests.lock().push(req);
        Ok(())
    }
    async fn close(&self) -> Result<(), AgentError> {
        Ok(())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Go `failingLiveAppendService`: fails the append of the event whose ID is `fail_id`.
struct FailingLiveAppendService {
    inner: Arc<dyn SessionService>,
    fail_id: Option<String>,
}

#[async_trait]
impl SessionService for FailingLiveAppendService {
    async fn create(&self, req: CreateRequest) -> Result<Arc<dyn Session>, SessionError> {
        self.inner.create(req).await
    }
    async fn get(&self, req: GetRequest) -> Result<Arc<dyn Session>, SessionError> {
        self.inner.get(req).await
    }
    async fn list(&self, req: ListRequest) -> Result<Vec<Arc<dyn Session>>, SessionError> {
        self.inner.list(req).await
    }
    async fn delete(&self, req: DeleteRequest) -> Result<(), SessionError> {
        self.inner.delete(req).await
    }
    async fn append_event(
        &self,
        session: &dyn Session,
        event: &mut Event,
    ) -> Result<(), SessionError> {
        if self.fail_id.as_deref() == Some(event.id.as_str()) {
            return Err(SessionError::Backend("session storage unavailable".into()));
        }
        self.inner.append_event(session, event).await
    }
}

async fn create_session(svc: &dyn SessionService, session_id: &str) {
    svc.create(CreateRequest {
        app_name: APP.into(),
        user_id: USER.into(),
        session_id: Some(session_id.into()),
        state: None,
    })
    .await
    .unwrap();
}

fn new_runner(agent: Arc<dyn Agent>, svc: Arc<dyn SessionService>, plugins: Vec<Plugin>) -> Runner {
    Runner::new(RunnerConfig {
        app_name: APP.into(),
        agent,
        session_service: svc,
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig {
            plugins,
            ..Default::default()
        },
        auto_create_session: false,
        platform: Platform::default(),
        compaction: None,
    })
    .unwrap()
}

/// Go `newRunLiveTestRunner`: a created session and a runner over a `test_agent` mock live agent.
async fn new_run_live_test_runner(
    session_id: &str,
    sequence: Sequence,
) -> (Runner, Arc<dyn SessionService>) {
    let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    create_session(&*svc, session_id).await;
    let agent = mock_live_agent(
        "test_agent",
        Arc::new(DummyLiveSession),
        sequence,
        Arc::new(AtomicBool::new(false)),
    );
    (new_runner(agent, svc.clone(), vec![]), svc)
}

/// Go `getRunLiveTestEvents`.
async fn get_run_live_test_events(svc: &dyn SessionService, session_id: &str) -> Vec<Event> {
    svc.get(GetRequest {
        app_name: APP.into(),
        user_id: USER.into(),
        session_id: session_id.into(),
        ..Default::default()
    })
    .await
    .unwrap()
    .events()
}

fn new_event(ctx: &InvocationContext) -> Event {
    Event::new(ctx.platform(), ctx.invocation_id())
}

fn transcription(text: &str) -> Option<Transcription> {
    Some(Transcription {
        text: text.into(),
        ..Default::default()
    })
}

fn function_call_content(name: &str) -> Option<Content> {
    Some(Content {
        role: String::new(),
        parts: vec![Part {
            function_call: Some(FunctionCall {
                name: name.into(),
                ..Default::default()
            }),
            ..Default::default()
        }],
    })
}

fn has_function_call(ev: &Event) -> bool {
    ev.response
        .content
        .as_ref()
        .is_some_and(|c| c.parts.iter().any(|p| p.function_call.is_some()))
}

/// Sets its flag when dropped: detects that the upstream stream was dropped mid-way.
struct SetOnDrop(Arc<AtomicBool>);

impl Drop for SetOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

// parity: skipped TestRunner_RunLive_NilEventYieldedTerminatesRun: an EventStream item is Result<Event, AgentError>, so a live agent cannot yield a nil event (as 1b skipped TestRunner_NilEventYieldedTerminatesRun)

#[tokio::test]
async fn test_runner_run_live_callbacks() {
    let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    create_session(&*svc, "testSession").await;

    let before_run_called = Arc::new(AtomicBool::new(false));
    let after_run_called = Arc::new(AtomicBool::new(false));
    let (b, a) = (before_run_called.clone(), after_run_called.clone());
    let plugin = Plugin::new(gadk_agent::PluginConfig {
        name: "test_plugin".into(),
        before_run_callback: Some(Arc::new(move |_ctx| {
            b.store(true, Ordering::SeqCst);
            Box::pin(async { Ok::<Option<Content>, AgentError>(None) })
        })),
        after_run_callback: Some(Arc::new(move |_ctx| {
            a.store(true, Ordering::SeqCst);
            Box::pin(async {})
        })),
        ..Default::default()
    });

    let sequence: Sequence =
        Arc::new(|ctx: InvocationContext| futures::stream::iter(vec![Ok(new_event(&ctx))]).boxed());
    let agent = mock_live_agent(
        "test_agent",
        Arc::new(DummyLiveSession),
        sequence,
        Arc::new(AtomicBool::new(false)),
    );
    let r = new_runner(agent, svc, vec![plugin]);

    let (_sess, mut stream) = r
        .run_live(
            USER,
            "testSession",
            LiveRunConfig::default(),
            RunOptions::default(),
        )
        .await
        .expect("RunLive failed");

    assert!(
        before_run_called.load(Ordering::SeqCst),
        "BeforeRunCallback was not called before starting RunLive"
    );
    assert!(
        !after_run_called.load(Ordering::SeqCst),
        "AfterRunCallback should not be called until iterator is consumed"
    );
    while let Some(item) = stream.next().await {
        item.unwrap();
    }
    assert!(
        after_run_called.load(Ordering::SeqCst),
        "AfterRunCallback was not called after iterator was consumed"
    );
}

#[tokio::test]
async fn test_runner_run_live_early_exit() {
    let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    create_session(&*svc, "testSession2").await;

    let expected_content = Content::from_text("early exit content", "");
    let reply = expected_content.clone();
    let plugin = Plugin::new(gadk_agent::PluginConfig {
        name: "test_plugin".into(),
        before_run_callback: Some(Arc::new(move |_ctx| {
            let out = Ok(Some(reply.clone()));
            Box::pin(async move { out })
        })),
        ..Default::default()
    });

    let run_live_called = Arc::new(AtomicBool::new(false));
    let sequence: Sequence = Arc::new(|_ctx: InvocationContext| futures::stream::empty().boxed());
    let agent = mock_live_agent(
        "test_agent",
        Arc::new(DummyLiveSession),
        sequence,
        run_live_called.clone(),
    );
    let r = new_runner(agent, svc.clone(), vec![plugin]);

    let (sess, stream) = r
        .run_live(
            USER,
            "testSession2",
            LiveRunConfig::default(),
            RunOptions::default(),
        )
        .await
        .expect("RunLive failed");
    assert!(
        !run_live_called.load(Ordering::SeqCst),
        "RunLive should not have been called on the agent due to early exit"
    );

    let events: Vec<Event> = stream.map(|r| r.unwrap()).collect().await;
    assert_eq!(events.len(), 1, "expected 1 event");
    assert_eq!(events[0].response.content.as_ref(), Some(&expected_content));
    assert_eq!(events[0].author, "test_agent");
    // The early-exit event is persisted.
    assert_eq!(
        get_run_live_test_events(&*svc, "testSession2").await.len(),
        1
    );

    let err = sess
        .send(LiveRequest::default())
        .await
        .expect_err("send to an early-exited session must fail");
    assert!(
        err.to_string().contains("session is closed"),
        "expected error 'session is closed' when sending to early exited session, got {err}"
    );
    sess.close().await.expect("Close() failed");
}

#[tokio::test]
async fn test_runner_run_live_chronological_buffering() {
    let sequence: Sequence = Arc::new(|ctx: InvocationContext| {
        Box::pin(async_stream::stream! {
            // 1. Partial transcription.
            let mut ev1 = new_event(&ctx);
            ev1.response.partial = true;
            ev1.response.output_transcription = transcription("Hello");
            yield Ok(ev1);
            // 2. Function call (happening during transcription).
            let mut ev2 = new_event(&ctx);
            ev2.response.content = function_call_content("test_func");
            yield Ok(ev2);
            // 3. Final transcription.
            let mut ev3 = new_event(&ctx);
            ev3.response.output_transcription = transcription("Hello there.");
            yield Ok(ev3);
        })
    });
    let (r, svc) = new_run_live_test_runner("testSession3", sequence).await;

    let (_sess, mut stream) = r
        .run_live(
            USER,
            "testSession3",
            LiveRunConfig::default(),
            RunOptions::default(),
        )
        .await
        .expect("RunLive failed");
    while let Some(item) = stream.next().await {
        item.unwrap();
    }

    let events = get_run_live_test_events(&*svc, "testSession3").await;
    // Final transcription first, function call second; the partial transcription is not saved.
    assert_eq!(events.len(), 2, "expected 2 saved events in session");
    assert_eq!(
        events[0]
            .response
            .output_transcription
            .as_ref()
            .map(|t| t.text.as_str()),
        Some("Hello there."),
        "first saved event should be the final transcription"
    );
    assert!(
        has_function_call(&events[1]),
        "second saved event should be the function call, got {:?}",
        events[1]
    );
}

#[tokio::test]
async fn test_runner_run_live_flushes_buffered_events_at_eof() {
    const SESSION_ID: &str = "testSessionBufferedEOF";
    let sequence: Sequence = Arc::new(|ctx: InvocationContext| {
        Box::pin(async_stream::stream! {
            let mut partial = new_event(&ctx);
            partial.response.partial = true;
            partial.response.input_transcription = transcription("book me a ");
            yield Ok(partial);
            let mut buffered_call = new_event(&ctx);
            buffered_call.response.content = function_call_content("book_flight");
            yield Ok(buffered_call);
        })
    });
    let (r, svc) = new_run_live_test_runner(SESSION_ID, sequence).await;

    let (_sess, mut stream) = r
        .run_live(
            USER,
            SESSION_ID,
            LiveRunConfig::default(),
            RunOptions::default(),
        )
        .await
        .expect("RunLive failed");
    let mut received = Vec::new();
    while let Some(item) = stream.next().await {
        let event = item.unwrap();
        if has_function_call(&event) {
            let got = get_run_live_test_events(&*svc, SESSION_ID).await.len();
            assert_eq!(got, 1, "persisted events while yielding buffered call");
        }
        received.push(event);
    }

    assert_eq!(
        received.len(),
        2,
        "RunLive() yielded {} events, want partial transcription and buffered call",
        received.len()
    );
    assert!(
        has_function_call(&received[1]),
        "RunLive() second event is not the buffered function call"
    );
    let events = get_run_live_test_events(&*svc, SESSION_ID).await;
    assert_eq!(events.len(), 1, "persisted events after EOF");
    let content = events[0].response.content.as_ref().expect("content");
    assert_eq!(content.parts.len(), 1);
    assert_eq!(
        content.parts[0]
            .function_call
            .as_ref()
            .map(|fc| fc.name.as_str()),
        Some("book_flight"),
        "persisted event does not contain the expected function call"
    );
}

// parity: adapted: Go observes the downstream stop through yield returning false; a Rust upstream observes it by being dropped before it finishes (a drop flag set while the completion flag is unset)
#[tokio::test]
async fn test_runner_run_live_does_not_flush_buffered_events_after_consumer_stops() {
    const SESSION_ID: &str = "testSessionBufferedStop";
    let dropped = Arc::new(AtomicBool::new(false));
    let completed = Arc::new(AtomicBool::new(false));
    let (d, c) = (dropped.clone(), completed.clone());
    let sequence: Sequence = Arc::new(move |ctx: InvocationContext| {
        let (d, c) = (d.clone(), c.clone());
        Box::pin(async_stream::stream! {
            let _on_drop = SetOnDrop(d);
            let mut partial = new_event(&ctx);
            partial.response.partial = true;
            partial.response.input_transcription = transcription("book me a ");
            yield Ok(partial);
            let mut call = new_event(&ctx);
            call.response.content = function_call_content("book_flight");
            yield Ok(call);
            let mut continued_partial = new_event(&ctx);
            continued_partial.response.partial = true;
            continued_partial.response.input_transcription = transcription("book me a flight");
            yield Ok(continued_partial);
            c.store(true, Ordering::SeqCst);
        })
    });
    let (r, svc) = new_run_live_test_runner(SESSION_ID, sequence).await;

    let (_sess, mut stream) = r
        .run_live(
            USER,
            SESSION_ID,
            LiveRunConfig::default(),
            RunOptions::default(),
        )
        .await
        .expect("RunLive failed");
    let mut yield_calls = 0;
    while yield_calls < 2 {
        let Some(item) = stream.next().await else {
            break;
        };
        item.unwrap();
        yield_calls += 1;
    }
    drop(stream);

    assert_eq!(yield_calls, 2, "downstream yield calls");
    assert!(
        dropped.load(Ordering::SeqCst) && !completed.load(Ordering::SeqCst),
        "upstream iterator did not observe the downstream stop"
    );
    assert_eq!(
        get_run_live_test_events(&*svc, SESSION_ID).await.len(),
        0,
        "persisted events after consumer stop"
    );
}

// parity: adapted: Rust event streams cannot carry nil events, so the two nil-event cases are dropped; SessionService::append_event takes no context, so Go's contextBoundLiveAppendService has no counterpart and the cancel cases cancel the invocation token and yield AgentError::Cancelled
#[tokio::test]
async fn test_runner_run_live_buffered_event_exits() {
    struct Case {
        name: &'static str,
        cancel_run: bool,
        fail_append: bool,
        stop_on_error: bool,
        stop_on_call: bool,
        want_ids: &'static [&'static str],
        want_saved: &'static [&'static str],
        want_errors: usize,
    }
    let cases = [
        Case {
            name: "cancelled context preserves buffered events",
            cancel_run: true,
            fail_append: false,
            stop_on_error: false,
            stop_on_call: false,
            want_ids: &["partial", "later", "call", "response"],
            want_saved: &["later", "call", "response"],
            want_errors: 1,
        },
        Case {
            name: "cancelled context consumer stops on error",
            cancel_run: true,
            fail_append: false,
            stop_on_error: true,
            stop_on_call: false,
            want_ids: &["partial", "later"],
            want_saved: &["later"],
            want_errors: 1,
        },
        Case {
            name: "cancelled context consumer stops during flush",
            cancel_run: true,
            fail_append: false,
            stop_on_error: false,
            stop_on_call: true,
            want_ids: &["partial", "later", "call"],
            want_saved: &["later", "call"],
            want_errors: 1,
        },
        Case {
            name: "EOF preserves order and timestamps",
            cancel_run: false,
            fail_append: false,
            stop_on_error: false,
            stop_on_call: false,
            want_ids: &["partial", "later", "call", "response"],
            want_saved: &["later", "call", "response"],
            want_errors: 0,
        },
        Case {
            name: "append failure continues flush",
            cancel_run: false,
            fail_append: true,
            stop_on_error: false,
            stop_on_call: false,
            want_ids: &["partial", "later", "response"],
            want_saved: &["later", "response"],
            want_errors: 1,
        },
        Case {
            name: "append failure consumer stops",
            cancel_run: false,
            fail_append: true,
            stop_on_error: true,
            stop_on_call: false,
            want_ids: &["partial", "later"],
            want_saved: &["later"],
            want_errors: 1,
        },
        Case {
            name: "consumer stops during flush",
            cancel_run: false,
            fail_append: false,
            stop_on_error: false,
            stop_on_call: true,
            want_ids: &["partial", "later", "call"],
            want_saved: &["later", "call"],
            want_errors: 0,
        },
    ];
    for tc in cases {
        const SESSION_ID: &str = "bufferedExits";
        let start: DateTime<Utc> = Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap();
        let timestamp = move |id: &str| -> DateTime<Utc> {
            let secs = match id {
                "partial" => 0,
                "call" => 1,
                "response" => 2,
                "later" => 3,
                other => panic!("unexpected event id {other}"),
            };
            start + chrono::Duration::seconds(secs)
        };
        let cancel_run = tc.cancel_run;
        let sequence: Sequence = Arc::new(move |ctx: InvocationContext| {
            Box::pin(async_stream::stream! {
                let mut partial = new_event(&ctx);
                partial.id = "partial".into();
                partial.response.partial = true;
                partial.response.input_transcription = transcription("book a ");
                let mut call = new_event(&ctx);
                call.id = "call".into();
                call.response.content = function_call_content("book_flight");
                let mut response = new_event(&ctx);
                response.id = "response".into();
                response.response.content = Some(Content {
                    role: String::new(),
                    parts: vec![Part {
                        function_response: Some(FunctionResponse {
                            name: "book_flight".into(),
                            response: json!({"ok": true}).as_object().unwrap().clone(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }],
                });
                let mut later = new_event(&ctx);
                later.id = "later".into();
                later.response.content = Some(Content::from_text("unrelated message", ROLE_MODEL));
                for mut event in [partial, call, response, later] {
                    event.timestamp = timestamp(&event.id);
                    yield Ok(event);
                }
                if cancel_run {
                    // The live producer reports cancellation before closing its stream.
                    ctx.cancellation_token().cancel();
                    yield Err(AgentError::Cancelled);
                }
            })
        });
        let base: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
        create_session(&*base, SESSION_ID).await;
        let svc: Arc<dyn SessionService> = Arc::new(FailingLiveAppendService {
            inner: base.clone(),
            fail_id: tc.fail_append.then(|| "call".to_string()),
        });
        let agent = mock_live_agent(
            "test_agent",
            Arc::new(DummyLiveSession),
            sequence,
            Arc::new(AtomicBool::new(false)),
        );
        let r = new_runner(agent, svc, vec![]);
        let (_sess, mut stream) = r
            .run_live(
                USER,
                SESSION_ID,
                LiveRunConfig::default(),
                RunOptions::default(),
            )
            .await
            .unwrap();

        let mut got_ids: Vec<String> = Vec::new();
        let mut error_count = 0;
        while let Some(item) = stream.next().await {
            match item {
                Err(err) => {
                    error_count += 1;
                    if tc.cancel_run {
                        assert!(
                            matches!(err, AgentError::Cancelled),
                            "{}: error = {err}, want context cancellation",
                            tc.name
                        );
                    } else {
                        assert_eq!(
                            err.to_string(),
                            "failed to add event to session: session storage unavailable",
                            "{}: want wrapped append error",
                            tc.name
                        );
                    }
                    if tc.stop_on_error {
                        break;
                    }
                }
                Ok(event) => {
                    got_ids.push(event.id.clone());
                    assert_eq!(
                        event.timestamp,
                        timestamp(&event.id),
                        "{}: timestamp changed for yielded event {}",
                        tc.name,
                        event.id
                    );
                    if !event.response.partial {
                        let saved = get_run_live_test_events(&*base, SESSION_ID).await;
                        assert_eq!(
                            saved.last().map(|e| e.id.as_str()),
                            Some(event.id.as_str()),
                            "{}: event {} was yielded before it was saved",
                            tc.name,
                            event.id
                        );
                    }
                    if tc.stop_on_call && event.id == "call" {
                        break;
                    }
                }
            }
        }
        drop(stream);

        assert_eq!(got_ids, tc.want_ids, "{}: yielded IDs", tc.name);
        assert_eq!(error_count, tc.want_errors, "{}: errors", tc.name);
        let saved = get_run_live_test_events(&*base, SESSION_ID).await;
        for event in &saved {
            assert_eq!(
                event.timestamp,
                timestamp(&event.id),
                "{}: timestamp changed for saved event {}",
                tc.name,
                event.id
            );
        }
        let saved_ids: Vec<&str> = saved.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(saved_ids, tc.want_saved, "{}: saved IDs", tc.name);
    }
}

// parity: adapted: a Rust Part cannot be nil, so there is nothing to drop; the cases keep Go's persistence rule (text saved, function response and empty content not saved, realtime input forwarded) without the nil entries
#[tokio::test]
async fn test_runner_run_live_send_drops_nil_parts() {
    let text = Part::from_text("hi");
    let text2 = Part::from_text("there");
    let func_resp = Part {
        function_response: Some(FunctionResponse {
            name: "book_flight".into(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let blob = RealtimeInput::Blob(Blob {
        mime_type: "audio/pcm".into(),
        data: bytes::Bytes::from_static(&[0x00, 0x01]),
        ..Default::default()
    });
    struct Case {
        name: &'static str,
        parts: Vec<Part>,
        realtime_input: Option<RealtimeInput>,
        want_saved: bool,
    }
    let cases = [
        Case {
            name: "text",
            parts: vec![text.clone()],
            realtime_input: None,
            want_saved: true,
        },
        Case {
            name: "two texts",
            parts: vec![text.clone(), text2.clone()],
            realtime_input: None,
            want_saved: true,
        },
        Case {
            name: "function response",
            parts: vec![func_resp.clone()],
            realtime_input: None,
            want_saved: false,
        },
        Case {
            name: "no parts",
            parts: vec![],
            realtime_input: None,
            want_saved: false,
        },
        Case {
            name: "text with realtime input",
            parts: vec![text.clone()],
            realtime_input: Some(blob.clone()),
            want_saved: true,
        },
    ];
    for tc in cases {
        const SESSION_ID: &str = "testSessionNilParts";
        let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
        create_session(&*svc, SESSION_ID).await;
        let agent_sess = Arc::new(RecordingLiveSession::default());
        let sequence: Sequence =
            Arc::new(|_ctx: InvocationContext| futures::stream::empty().boxed());
        let agent = mock_live_agent(
            "test_agent",
            agent_sess.clone(),
            sequence,
            Arc::new(AtomicBool::new(false)),
        );
        let r = new_runner(agent, svc.clone(), vec![]);
        let (sess, _stream) = r
            .run_live(
                USER,
                SESSION_ID,
                LiveRunConfig::default(),
                RunOptions::default(),
            )
            .await
            .expect("RunLive failed");

        let content = Content {
            role: ROLE_USER.into(),
            parts: tc.parts.clone(),
        };
        sess.send(LiveRequest {
            content: Some(content.clone()),
            realtime_input: tc.realtime_input.clone(),
        })
        .await
        .unwrap_or_else(|e| panic!("{}: Send() error = {e}", tc.name));

        let requests = agent_sess.requests.lock().clone();
        assert_eq!(requests.len(), 1, "{}: forwarded requests", tc.name);
        assert_eq!(
            requests[0].content.as_ref(),
            Some(&content),
            "{}: forwarded content",
            tc.name
        );
        assert_eq!(
            requests[0].realtime_input, tc.realtime_input,
            "{}: forwarded RealtimeInput",
            tc.name
        );

        let events = get_run_live_test_events(&*svc, SESSION_ID).await;
        if !tc.want_saved {
            assert_eq!(events.len(), 0, "{}: saved events", tc.name);
            continue;
        }
        assert_eq!(events.len(), 1, "{}: saved events", tc.name);
        assert_eq!(events[0].author, "user", "{}: author", tc.name);
        assert_eq!(
            events[0].response.content.as_ref(),
            Some(&content),
            "{}: saved content",
            tc.name
        );
    }
}

/// Go `time.Date(2026, time.March, d, h, 0, 0, 0, time.UTC)`.
fn march(day: u32, hour: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 3, day, hour, 0, 0).unwrap()
}

/// A runner over `agent` whose platform clock always reads `run_time` (Go
/// `platform.WithTimeProvider(ctx, func() time.Time { return runTime })`).
fn new_timed_runner(
    agent: Arc<dyn Agent>,
    svc: Arc<dyn SessionService>,
    plugins: Vec<Plugin>,
    run_time: DateTime<Utc>,
) -> Runner {
    Runner::new(RunnerConfig {
        app_name: APP.into(),
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
    .unwrap()
}

// parity: adapted: on_event receives the event by value, so Go's in-place rewrite (`*ev = …; return nil`) only changes the plugin's own copy; the "rewrite" case then stores the agent's original event, which carries no timestamp either
#[tokio::test]
async fn test_runner_run_live_stamps_events_yielded_without_timestamp() {
    let run_time = march(2, 10);
    let own_time = march(1, 9);
    struct Case {
        name: &'static str,
        own: DateTime<Utc>,
        replace: bool,
        rewrite: bool,
        want: DateTime<Utc>,
    }
    let zero = gadk_session::zero_time();
    let cases = [
        Case {
            name: "agent event",
            own: zero,
            replace: false,
            rewrite: false,
            want: run_time,
        },
        Case {
            name: "plugin replacement",
            own: zero,
            replace: true,
            rewrite: false,
            want: run_time,
        },
        Case {
            name: "plugin rewrites in place",
            own: zero,
            replace: false,
            rewrite: true,
            want: run_time,
        },
        Case {
            name: "own timestamp",
            own: own_time,
            replace: false,
            rewrite: false,
            want: own_time,
        },
    ];
    for tc in cases {
        const SESSION_ID: &str = "testSession";
        let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
        create_session(&*svc, SESSION_ID).await;
        let saw_unstamped = Arc::new(AtomicBool::new(false));
        let saw = saw_unstamped.clone();
        let (replace, rewrite) = (tc.replace, tc.rewrite);
        let plugin = Plugin::new(gadk_agent::PluginConfig {
            name: "observe_and_replace".into(),
            on_event_callback: Some(Arc::new(move |_ctx, mut ev: Event| {
                if ev.timestamp == gadk_session::zero_time() {
                    saw.store(true, Ordering::SeqCst);
                }
                let out = if rewrite {
                    ev = Event {
                        author: ev.author.clone(),
                        response: ev.response.clone(),
                        ..Default::default()
                    };
                    let _ = ev;
                    None
                } else if replace {
                    Some(Event {
                        author: ev.author.clone(),
                        response: ev.response.clone(),
                        ..Default::default()
                    })
                } else {
                    None
                };
                Box::pin(async move { Ok(out) })
            })),
            ..Default::default()
        });
        let own = tc.own;
        let sequence: Sequence = Arc::new(move |_ctx: InvocationContext| {
            let mut ev = Event {
                author: "test_agent".into(),
                timestamp: own,
                ..Default::default()
            };
            ev.response.content = Some(Content::from_text("reply", ROLE_MODEL));
            futures::stream::iter(vec![Ok(ev)]).boxed()
        });
        let agent = mock_live_agent(
            "test_agent",
            Arc::new(DummyLiveSession),
            sequence,
            Arc::new(AtomicBool::new(false)),
        );
        let r = new_timed_runner(agent, svc.clone(), vec![plugin], run_time);
        let (_sess, mut stream) = r
            .run_live(
                USER,
                SESSION_ID,
                LiveRunConfig::default(),
                RunOptions::default(),
            )
            .await
            .unwrap_or_else(|e| panic!("{}: RunLive() error = {e}", tc.name));
        while let Some(item) = stream.next().await {
            item.unwrap_or_else(|e| panic!("{}: {e}", tc.name));
        }

        let mut saw_reply = false;
        for ev in get_run_live_test_events(&*svc, SESSION_ID).await {
            if ev.author != "test_agent" {
                continue;
            }
            saw_reply = true;
            assert_eq!(ev.timestamp, tc.want, "{}: stored event timestamp", tc.name);
        }
        assert!(
            saw_reply,
            "{}: no event from the live agent was stored",
            tc.name
        );
        assert!(
            !saw_unstamped.load(Ordering::SeqCst),
            "{}: OnEventCallback saw the live agent's event with a zero timestamp",
            tc.name
        );
    }
}

#[tokio::test]
async fn test_runner_run_live_plugin_replacement_keeps_event_identity() {
    const SESSION_ID: &str = "testSession";
    let run_time = march(2, 10);
    let agent_time = march(1, 9);
    let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    create_session(&*svc, SESSION_ID).await;
    let original_invocation_id: Arc<Mutex<String>> = Arc::default();
    let seen = original_invocation_id.clone();
    let plugin = Plugin::new(gadk_agent::PluginConfig {
        name: "replace".into(),
        on_event_callback: Some(Arc::new(move |_ctx, ev: Event| {
            *seen.lock() = ev.invocation_id.clone();
            let mut out = Event {
                id: "plugin-id".into(),
                invocation_id: "plugin-invocation".into(),
                timestamp: march(3, 11),
                ..Default::default()
            };
            out.response.content = Some(Content::from_text("replaced", ROLE_MODEL));
            out.response
                .custom_metadata
                .insert("modified".into(), json!(true));
            Box::pin(async move { Ok(Some(out)) })
        })),
        ..Default::default()
    });
    let sequence: Sequence = Arc::new(move |ctx: InvocationContext| {
        let mut ev = Event {
            id: "agent-event".into(),
            invocation_id: ctx.invocation_id().to_string(),
            timestamp: agent_time,
            author: "test_agent".into(),
            ..Default::default()
        };
        ev.response.content = Some(Content::from_text("reply", ROLE_MODEL));
        futures::stream::iter(vec![Ok(ev)]).boxed()
    });
    let agent = mock_live_agent(
        "test_agent",
        Arc::new(DummyLiveSession),
        sequence,
        Arc::new(AtomicBool::new(false)),
    );
    let r = new_timed_runner(agent, svc.clone(), vec![plugin], run_time);
    let (_sess, stream) = r
        .run_live(
            USER,
            SESSION_ID,
            LiveRunConfig::default(),
            RunOptions::default(),
        )
        .await
        .expect("RunLive() error");
    let streamed: Vec<Event> = stream.map(|r| r.unwrap()).collect().await;
    let stored = get_run_live_test_events(&*svc, SESSION_ID).await;
    let original_invocation_id = original_invocation_id.lock().clone();
    assert!(
        !original_invocation_id.is_empty(),
        "the plugin never saw the live agent's event"
    );

    // Go cmp.Diff ignoring Actions: identity from the original, content and metadata from the
    // replacement.
    let check = |where_: &str, evs: &[Event]| {
        assert_eq!(evs.len(), 1, "{where_} events");
        let ev = &evs[0];
        assert_eq!(ev.id, "agent-event", "{where_} ID");
        assert_eq!(
            ev.invocation_id, original_invocation_id,
            "{where_} InvocationID"
        );
        assert_eq!(ev.timestamp, agent_time, "{where_} Timestamp");
        assert_eq!(ev.author, "test_agent", "{where_} Author");
        assert_eq!(
            ev.response.content,
            Some(Content::from_text("replaced", ROLE_MODEL)),
            "{where_} Content"
        );
        assert_eq!(
            ev.response.custom_metadata.get("modified"),
            Some(&json!(true)),
            "{where_} CustomMetadata"
        );
        assert_eq!(
            ev.response.custom_metadata.len(),
            1,
            "{where_} CustomMetadata"
        );
    };
    check("streamed", &streamed);
    check("stored", &stored);
}

#[tokio::test]
async fn test_runner_run_live_early_exit_runs_on_event_and_after_run() {
    const SESSION_ID: &str = "testSession";
    let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    create_session(&*svc, SESSION_ID).await;

    let cached_reply = Content::from_text("cached answer", ROLE_MODEL);
    let redacted_reply = Content::from_text("redacted answer", ROLE_MODEL);
    let on_event_calls = Arc::new(Mutex::new(0usize));
    let after_run_calls = Arc::new(Mutex::new(0usize));
    let (oe, ar, cached, redacted) = (
        on_event_calls.clone(),
        after_run_calls.clone(),
        cached_reply.clone(),
        redacted_reply.clone(),
    );
    let plugin = Plugin::new(gadk_agent::PluginConfig {
        name: "cache".into(),
        before_run_callback: Some(Arc::new(move |_ctx| {
            let out = Ok(Some(cached.clone()));
            Box::pin(async move { out })
        })),
        on_event_callback: Some(Arc::new(move |_ctx, mut ev: Event| {
            *oe.lock() += 1;
            ev.response.content = Some(redacted.clone());
            Box::pin(async move { Ok(Some(ev)) })
        })),
        after_run_callback: Some(Arc::new(move |_ctx| {
            *ar.lock() += 1;
            Box::pin(async {})
        })),
        ..Default::default()
    });

    let run_live_called = Arc::new(AtomicBool::new(false));
    let sequence: Sequence = Arc::new(|_ctx: InvocationContext| futures::stream::empty().boxed());
    let agent = mock_live_agent(
        "test_agent",
        Arc::new(DummyLiveSession),
        sequence,
        run_live_called.clone(),
    );
    let r = new_runner(agent, svc.clone(), vec![plugin]);
    let (_sess, stream) = r
        .run_live(
            USER,
            SESSION_ID,
            LiveRunConfig::default(),
            RunOptions::default(),
        )
        .await
        .expect("RunLive failed");
    let got: Vec<Event> = stream.map(|r| r.unwrap()).collect().await;

    assert!(
        !run_live_called.load(Ordering::SeqCst),
        "the agent ran although BeforeRun replied"
    );
    assert_eq!(*on_event_calls.lock(), 1, "OnEvent calls");
    assert_eq!(*after_run_calls.lock(), 1, "AfterRun calls");
    assert_eq!(got.len(), 1, "yielded events");
    assert_eq!(
        got[0].response.content.as_ref(),
        Some(&redacted_reply),
        "yielded reply"
    );
    let stored = get_run_live_test_events(&*svc, SESSION_ID).await;
    assert_eq!(stored.len(), 1, "stored events");
    assert_eq!(
        stored[0].response.content.as_ref(),
        Some(&redacted_reply),
        "stored reply"
    );
}

#[tokio::test]
async fn test_runner_run_live_send_rejects_user_function_call() {
    const SESSION_ID: &str = "testSessionUserFunctionCall";
    let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    create_session(&*svc, SESSION_ID).await;
    let agent_sess = Arc::new(RecordingLiveSession::default());
    let sequence: Sequence = Arc::new(|_ctx: InvocationContext| futures::stream::empty().boxed());
    let agent = mock_live_agent(
        "test_agent",
        agent_sess.clone(),
        sequence,
        Arc::new(AtomicBool::new(false)),
    );
    let r = new_runner(agent, svc.clone(), vec![]);
    let (sess, _stream) = r
        .run_live(
            USER,
            SESSION_ID,
            LiveRunConfig::default(),
            RunOptions::default(),
        )
        .await
        .expect("RunLive failed");

    let content = Content {
        role: ROLE_USER.into(),
        parts: vec![
            Part::from_text("hi"),
            Part {
                function_call: Some(FunctionCall {
                    name: "some_tool".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ],
    };
    let err = sess
        .send(LiveRequest {
            content: Some(content),
            realtime_input: None,
        })
        .await;
    match err {
        Err(e) => assert_eq!(e.to_string(), "user message cannot contain function calls"),
        Ok(()) => panic!("Send() error = nil, want \"user message cannot contain function calls\""),
    }
    assert_eq!(agent_sess.requests.lock().len(), 0, "forwarded requests");
    assert_eq!(
        get_run_live_test_events(&*svc, SESSION_ID).await.len(),
        0,
        "saved events"
    );
}
