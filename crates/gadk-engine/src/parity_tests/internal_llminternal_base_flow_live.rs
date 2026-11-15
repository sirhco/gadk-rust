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

//! Parity port of Go internal/llminternal/base_flow_live_test.go.

use std::any::Any;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::future::BoxFuture;
use gadk_agent::{
    AgentError, Context, EventStream, InvocationContext, LiveRequest, LiveSession, RunnableTool,
    Tool, ToolError,
};
use gadk_genai::{Blob, Content, GenerateContentConfig};
use gadk_model::{Llm, LlmRequest, RealtimeInput};
use gadk_session::Event;
use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use crate::llm::flow::{Flow, RequestProcessor};
use crate::llm::live::flow::run_live;
use crate::llm::live::test_server::{
    DRAIN_TIMEOUT, FakeConn, FakeLiveServer, SERVER_CONTENT_PONG, ScriptedLiveLlm, ServeConn,
    assert_no_run_live_leak, live_ctx, live_flow, probe_of, serve,
};
use crate::tool::utils::function_calls;

#[derive(Clone, Copy, Debug)]
enum LeakCase {
    /// Connection 1 dies with an abrupt close (resumable); RunLive must reconnect, serve a turn
    /// on connection 2, and tear down without stranding either connection's tasks.
    ReaderResumable,
    /// Connection 1 dies before the client sends; the sender that picks up the queued send may
    /// hit the dead connection, and a retried send must succeed on connection 2.
    SenderContent,
    /// As SenderContent, but the request carries RealtimeInput (the third error-send site).
    SenderRealtime,
    /// A close frame with code 1008 is resumable and must reconnect.
    PolicyViolation1008,
    /// A close frame with a non-resumable code surfaces as an error after one connection.
    NonResumable1002,
    /// Cancelling the invocation while the reader is parked in recv exits cleanly.
    ParentCancel,
}

fn leak_serve(case: LeakCase) -> ServeConn {
    match case {
        LeakCase::ReaderResumable | LeakCase::PolicyViolation1008 => {
            serve(move |n: usize, mut conn: FakeConn| async move {
                if n == 1 {
                    if matches!(case, LeakCase::PolicyViolation1008) {
                        conn.write_close(1008, "policy violation").await;
                    }
                    return; // hard close right after setup => resumable on the client
                }
                if !conn.write_text(SERVER_CONTENT_PONG).await {
                    return;
                }
                conn.block_until_client_closes().await;
            })
        }
        LeakCase::SenderContent | LeakCase::SenderRealtime => {
            serve(|n: usize, mut conn: FakeConn| async move {
                if n == 1 {
                    return;
                }
                while conn.read_data().await.is_some() {
                    if !conn.write_text(SERVER_CONTENT_PONG).await {
                        return;
                    }
                }
            })
        }
        LeakCase::NonResumable1002 => serve(|_n: usize, mut conn: FakeConn| async move {
            conn.write_close(1002, "fatal").await;
        }),
        LeakCase::ParentCancel => serve(|_n: usize, mut conn: FakeConn| async move {
            if !conn.write_text(SERVER_CONTENT_PONG).await {
                return;
            }
            conn.block_until_client_closes().await;
        }),
    }
}

/// Drives until the post-reconnect model turn, then cancels; true when a turn was seen.
async fn drive_cancel_on_turn(mut stream: EventStream, cancel: &CancellationToken) -> bool {
    let mut saw_turn = false;
    while let Some(item) = stream.next().await {
        // The post-cancel `context canceled` error is expected.
        let Ok(ev) = item else { continue };
        if ev.response.content.is_some() {
            saw_turn = true;
            cancel.cancel();
        }
    }
    saw_turn
}

/// Sends up to 200 requests 5 ms apart until a model turn arrives, then cancels.
async fn drive_sending(
    sess: Arc<dyn LiveSession>,
    mut stream: EventStream,
    cancel: &CancellationToken,
    realtime: bool,
) -> bool {
    let stop = CancellationToken::new();
    let sender = {
        let (sess, stop) = (sess.clone(), stop.clone());
        tokio::spawn(async move {
            for _ in 0..200 {
                if stop.is_cancelled() {
                    return;
                }
                let req = if realtime {
                    // A fresh Blob per send, as Go does.
                    LiveRequest {
                        realtime_input: Some(RealtimeInput::Blob(Blob {
                            data: vec![0x00u8, 0x01, 0x02, 0x03].into(),
                            mime_type: "audio/pcm".into(),
                            ..Default::default()
                        })),
                        content: None,
                    }
                } else {
                    LiveRequest {
                        realtime_input: None,
                        content: Some(Content::from_text("ping", "user")),
                    }
                };
                if sess.send(req).await.is_err() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
    };
    let mut saw_turn = false;
    while let Some(item) = stream.next().await {
        let Ok(ev) = item else { continue };
        if ev.response.content.is_some() && !saw_turn {
            saw_turn = true;
            stop.cancel();
            cancel.cancel();
        }
    }
    let _ = sender.await;
    saw_turn
}

// parity: adapted: Go counts RunLive goroutines in runtime.Stack; Rust asserts the session's LiveProbe task counter returns to zero, and the six table cases run in one fn
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_run_live_no_goroutine_leak() {
    let cases = [
        (LeakCase::ReaderResumable, 2),
        (LeakCase::SenderContent, 2),
        (LeakCase::SenderRealtime, 2),
        (LeakCase::PolicyViolation1008, 2),
        (LeakCase::NonResumable1002, 1),
        (LeakCase::ParentCancel, 1),
    ];
    for (case, want_conns) in cases {
        let server = FakeLiveServer::start(leak_serve(case)).await;
        let flow = live_flow(server.model().await, None);
        let (ctx, cancel) = live_ctx();
        let (sess, mut stream) = run_live(flow, ctx).unwrap();
        let probe = probe_of(&sess);
        let drive = async {
            match case {
                LeakCase::ReaderResumable | LeakCase::PolicyViolation1008 => {
                    assert!(
                        drive_cancel_on_turn(stream, &cancel).await,
                        "{case:?}: never received the post-reconnect model turn"
                    );
                }
                LeakCase::SenderContent => assert!(
                    drive_sending(sess.clone(), stream, &cancel, false).await,
                    "{case:?}: never received a model turn for the retried send"
                ),
                LeakCase::SenderRealtime => assert!(
                    drive_sending(sess.clone(), stream, &cancel, true).await,
                    "{case:?}: never received a model turn for the retried realtime send"
                ),
                LeakCase::NonResumable1002 => {
                    let mut saw_err = false;
                    while let Some(item) = stream.next().await {
                        if let Err(e) = item
                            && e.to_string().contains("1002")
                        {
                            saw_err = true;
                        }
                    }
                    assert!(
                        saw_err,
                        "expected the close-1002 error to surface through the stream"
                    );
                }
                LeakCase::ParentCancel => {
                    // Cancel only after the first event (the server's only turn), so the reader
                    // is parked in recv when cleanup tears the connection down.
                    let mut first = true;
                    while stream.next().await.is_some() {
                        if first {
                            first = false;
                            cancel.cancel();
                        }
                    }
                }
            }
        };
        tokio::time::timeout(DRAIN_TIMEOUT, drive)
            .await
            .unwrap_or_else(|_| panic!("{case:?}: the stream never ended"));
        assert_eq!(
            server.conn_count(),
            want_conns,
            "{case:?}: connection count"
        );
        assert_no_run_live_leak(&probe).await;
    }
}

/// Go liveHistoryProcessor: a history the connection fails to send client-side.
fn live_history_processor<'a>(
    _ctx: &'a InvocationContext,
    req: &'a mut LlmRequest,
    _f: &'a Flow,
) -> BoxFuture<'a, Result<Vec<Event>, AgentError>> {
    req.config = Some(GenerateContentConfig::default());
    req.contents = vec![Content::from_text("unmarshalable", "user")];
    Box::pin(async { Ok(Vec::new()) })
}

// parity: adapted: serde_json cannot fail to encode NaN (Value has no NaN), so the client-side send_history failure comes from ScriptedLiveLlm::failing_history and "the client closed the socket" is ScriptedLiveLlm::closed()
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_run_live_closes_connection_on_send_history_failure() {
    let llm = ScriptedLiveLlm::failing_history("test-live-model");
    let flow = Arc::new(Flow {
        model: Some(llm.clone() as Arc<dyn Llm>),
        request_processors: vec![live_history_processor as RequestProcessor],
        ..Default::default()
    });
    let (ctx, _cancel) = live_ctx();
    let (sess, mut stream) = run_live(flow, ctx).unwrap();
    let probe = probe_of(&sess);

    // Stop at the first error.
    let got = tokio::time::timeout(DRAIN_TIMEOUT, async {
        while let Some(item) = stream.next().await {
            if let Err(e) = item {
                return Some(e);
            }
        }
        None
    })
    .await
    .expect("the stream never surfaced the history failure");
    let err = got.expect("expected the SendHistory failure to surface through the stream");
    assert!(
        err.to_string().contains("history"),
        "surfaced error = {err}, want it to mention the history send"
    );
    assert_eq!(llm.connects(), 1, "connection count");
    assert!(
        llm.closed(),
        "run_live returned without closing the live connection (missing cleanup on the send_history path)"
    );
    drop(stream);
    assert_no_run_live_leak(&probe).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_run_live_close_stops_idle_session() {
    // conn_gone fires once the client has torn the socket down.
    let (gone_tx, mut gone_rx) = tokio::sync::mpsc::channel::<()>(1);
    let server = FakeLiveServer::start(serve(move |_n: usize, mut conn: FakeConn| {
        let gone = gone_tx.clone();
        async move {
            // One turn to show the connection is up, then an idle server: no further
            // unsolicited traffic.
            assert!(
                conn.write_text(SERVER_CONTENT_PONG).await,
                "writing the model turn failed"
            );
            conn.block_until_client_closes().await;
            let _ = gone.try_send(());
        }
    }))
    .await;
    let flow = live_flow(server.model().await, None);
    // The invocation is deliberately left alive: this test is about close() on its own.
    let (ctx, _cancel) = live_ctx();
    let (sess, mut stream) = run_live(flow, ctx).unwrap();
    let probe = probe_of(&sess);

    // The server's only turn. Once it lands the connection is up and the flow is settling
    // into the idle consumer select.
    let first = tokio::time::timeout(Duration::from_secs(10), stream.next())
        .await
        .expect("no model turn");
    assert!(
        first.is_some(),
        "stream ended before the model turn; the connection never came up"
    );
    assert_eq!(server.conn_count(), 1, "connection count");

    sess.close().await.unwrap();

    let after = tokio::time::timeout(Duration::from_secs(10), stream.next())
        .await
        .expect("the stream did not end after close");
    assert!(after.is_none(), "stream produced an item after close");

    tokio::time::timeout(Duration::from_secs(10), gone_rx.recv())
        .await
        .expect("close did not release the live connection: the websocket is still open");
    assert_eq!(
        server.conn_count(),
        1,
        "connection count (close must not trigger a reconnect)"
    );
    assert_no_run_live_leak(&probe).await;
}

// parity: skipped TestRunLiveSkipsNilRealtimeInput: Go sends typed nil pointers (*genai.Blob, *genai.ActivityStart, *genai.ActivityEnd) as RealtimeInput; gadk's RealtimeInput is an enum with no nil value
// parity: skipped TestHasRealtimeInput: Go hasRealtimeInput inspects an `any` for typed nil pointers; gadk's LiveRequest.realtime_input is an Option of an enum, so there is nothing to inspect

/// Which server messages become events. The fake server always sends setupComplete first,
/// which carries nothing and so must never surface as an event.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "8a bug: run_live yields an event for a live response that carries nothing (setupComplete, empty serverContent); Go 8cddb9d4 skips it"]
async fn test_run_live_skips_empty_responses() {
    let cases: Vec<(&str, &'static str, Option<gadk_model::LlmResponse>)> = vec![
        ("empty server content", r#"{"serverContent":{}}"#, None),
        (
            "turn complete",
            r#"{"serverContent":{"turnComplete":true}}"#,
            Some(gadk_model::LlmResponse {
                turn_complete: true,
                ..Default::default()
            }),
        ),
        (
            "interrupted",
            r#"{"serverContent":{"interrupted":true}}"#,
            Some(gadk_model::LlmResponse {
                interrupted: true,
                ..Default::default()
            }),
        ),
        (
            "input transcription",
            r#"{"serverContent":{"inputTranscription":{"text":"hi"}}}"#,
            Some(gadk_model::LlmResponse {
                input_transcription: Some(gadk_genai::Transcription {
                    text: "hi".into(),
                    ..Default::default()
                }),
                partial: true,
                ..Default::default()
            }),
        ),
        (
            "output transcription",
            r#"{"serverContent":{"outputTranscription":{"text":"hello"}}}"#,
            Some(gadk_model::LlmResponse {
                output_transcription: Some(gadk_genai::Transcription {
                    text: "hello".into(),
                    ..Default::default()
                }),
                partial: true,
                ..Default::default()
            }),
        ),
        (
            "usage metadata",
            r#"{"usageMetadata":{"promptTokenCount":3}}"#,
            Some(gadk_model::LlmResponse {
                usage_metadata: Some(gadk_genai::GenerateContentResponseUsageMetadata {
                    prompt_token_count: 3,
                    ..Default::default()
                }),
                ..Default::default()
            }),
        ),
        (
            "session resumption update",
            r#"{"sessionResumptionUpdate":{"newHandle":"handle-1","resumable":true}}"#,
            Some(gadk_model::LlmResponse {
                session_resumption_handle: "handle-1".into(),
                ..Default::default()
            }),
        ),
    ];
    // The end marker carries no turnComplete, which would flush the transcription cases'
    // accumulated text as an extra event.
    const END_MARKER: &str =
        r#"{"serverContent":{"modelTurn":{"parts":[{"text":"end"}],"role":"model"}}}"#;
    for (name, msg, want) in cases {
        let server =
            FakeLiveServer::start(serve(move |_n: usize, mut conn: FakeConn| async move {
                if !conn.write_text(msg).await || !conn.write_text(END_MARKER).await {
                    return;
                }
                conn.block_until_client_closes().await;
            }))
            .await;
        let flow = live_flow(server.model().await, None);
        let (ctx, _cancel) = live_ctx();
        let (sess, mut stream) = run_live(flow, ctx).unwrap();
        let got = tokio::time::timeout(Duration::from_secs(10), async {
            let mut got = Vec::new();
            while let Some(item) = stream.next().await {
                let ev = item.unwrap_or_else(|e| panic!("{name}: iterator error: {e}"));
                if ev.response.content.is_some() {
                    break;
                }
                got.push(ev.response);
            }
            got
        })
        .await
        .unwrap_or_else(|_| panic!("{name}: the model turn never arrived"));
        let _ = sess.close().await;
        assert_eq!(
            got,
            want.into_iter().collect::<Vec<_>>(),
            "{name}: events before the model turn"
        );
    }
}

/// Go mockFunctionTool / longRunningMockTool.
struct MockTool {
    name: &'static str,
    long_running: bool,
    result: Map<String, Value>,
}

impl Tool for MockTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "mock"
    }
    fn is_long_running(&self) -> bool {
        self.long_running
    }
    fn as_runnable(&self) -> Option<&dyn RunnableTool> {
        Some(self)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[async_trait]
impl RunnableTool for MockTool {
    async fn run(&self, _ctx: Context, _args: Value) -> Result<Map<String, Value>, ToolError> {
        Ok(self.result.clone())
    }
}

/// A tool whose run is driven by the test (Go's slow_tool / counted_tool `runFunc`).
struct HookTool {
    name: &'static str,
    run: Arc<dyn Fn() -> BoxFuture<'static, Map<String, Value>> + Send + Sync>,
}

impl Tool for HookTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "mock"
    }
    fn as_runnable(&self) -> Option<&dyn RunnableTool> {
        Some(self)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[async_trait]
impl RunnableTool for HookTool {
    async fn run(&self, _ctx: Context, _args: Value) -> Result<Map<String, Value>, ToolError> {
        Ok((self.run)().await)
    }
}

/// `live_flow` with tools.
fn live_flow_with_tools(model: Arc<dyn Llm>, tools: Vec<Arc<dyn Tool>>) -> Arc<Flow> {
    Arc::new(Flow {
        model: Some(model),
        request_processors: vec![
            crate::llm::live::test_server::live_config_processor as RequestProcessor,
        ],
        tools: parking_lot::RwLock::new(tools),
        ..Default::default()
    })
}

/// Pulls events until one carries a function response, failing on an error or the end.
async fn next_function_response(stream: &mut EventStream) {
    loop {
        let item = tokio::time::timeout(Duration::from_secs(10), stream.next())
            .await
            .expect("no function response event");
        match item {
            None => panic!("iterator ended before the tool response event"),
            Some(Err(e)) => panic!("RunLive yielded an error: {e}"),
            Some(Ok(ev)) => {
                if ev
                    .response
                    .content
                    .as_ref()
                    .is_some_and(|c| c.parts.iter().any(|p| p.function_response.is_some()))
                {
                    return;
                }
            }
        }
    }
}

/// A tool response reaches the model through the sender task, the same single writer that
/// sends user input.
// parity: adapted: Go pins a single websocket writer (gorilla panics on two, the race detector
// reports them); gadk's connection serializes writes through one sink, so the port keeps Go's
// observable checks: the user send and the tool response both reach the wire, response after
// input, on one connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "8a bug: run_live's consumer loop writes a tool response on the connection itself, so it can overtake user input queued on the sender task (intermittent); Go 33a587e2 sends it through the sender"]
async fn test_run_live_tool_response_shares_sender_goroutine() {
    const TOOL_CALL: &str =
        r#"{"toolCall":{"functionCalls":[{"id":"t1","name":"slow_tool","args":{}}]}}"#;
    let tool_started = Arc::new(tokio::sync::Notify::new());
    let tool_release = Arc::new(tokio::sync::Notify::new());
    let payloads = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
    let p = payloads.clone();
    let server = FakeLiveServer::start(serve(move |_n: usize, mut conn: FakeConn| {
        let p = p.clone();
        async move {
            assert!(conn.write_text(TOOL_CALL).await, "writing toolCall failed");
            while let Some(m) = conn.read_data().await {
                p.lock()
                    .push(m.into_text().map(|t| t.to_string()).unwrap_or_default());
            }
        }
    }))
    .await;
    let (started, release) = (tool_started.clone(), tool_release.clone());
    let tool: Arc<dyn Tool> = Arc::new(HookTool {
        name: "slow_tool",
        run: Arc::new(move || {
            let (started, release) = (started.clone(), release.clone());
            Box::pin(async move {
                started.notify_one();
                release.notified().await;
                json!({"ok": true}).as_object().unwrap().clone()
            })
        }),
    });
    let flow = live_flow_with_tools(server.model().await, vec![tool]);
    let (ctx, cancel) = live_ctx();
    let (sess, mut stream) = run_live(flow, ctx).unwrap();
    let probe = probe_of(&sess);

    loop {
        let item = tokio::time::timeout(Duration::from_secs(10), stream.next())
            .await
            .expect("no function call event");
        match item {
            None => panic!("iterator ended before the function call event"),
            Some(Err(e)) => panic!("RunLive yielded an error: {e}"),
            Some(Ok(ev)) if !function_calls(ev.response.content.as_ref()).is_empty() => break,
            Some(Ok(_)) => {}
        }
    }
    tokio::time::timeout(Duration::from_secs(10), tool_started.notified())
        .await
        .expect("slow_tool never started");

    // User input while the tool runs; then the tool returns and its response is written.
    sess.send(LiveRequest {
        content: Some(Content::from_text("ping", "user")),
        realtime_input: None,
    })
    .await
    .expect("Send failed");
    tool_release.notify_one();
    next_function_response(&mut stream).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !payloads
        .lock()
        .iter()
        .any(|p| p.contains("functionResponse"))
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "tool response never reached the wire; payloads: {:?}",
            payloads.lock()
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    cancel.cancel();
    while stream.next().await.is_some() {}

    let payloads = payloads.lock().clone();
    let user_idx = payloads.iter().position(|p| p.contains(r#""text":"ping""#));
    let resp_idx = payloads.iter().position(|p| p.contains("functionResponse"));
    let (Some(user_idx), Some(resp_idx)) = (user_idx, resp_idx) else {
        panic!("user send or tool response never reached the wire; payloads: {payloads:?}");
    };
    assert!(
        resp_idx > user_idx,
        "tool response (payload {resp_idx}) arrived before the user send (payload {user_idx})"
    );
    assert_eq!(
        server.conn_count(),
        1,
        "connection count (scenario needs no reconnect)"
    );
    assert_no_run_live_leak(&probe).await;
}

const TOOL_CALL: &str = r#"{"toolCall":{"functionCalls":[{"id":"lr_1","name":"long_job","args":{}},{"id":"plain_1","name":"plain_job","args":{}}]}}"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_run_live_marks_long_running_calls() {
    let server = FakeLiveServer::start(serve(|_n: usize, mut conn: FakeConn| async move {
        assert!(conn.write_text(TOOL_CALL).await, "writing toolCall failed");
        conn.block_until_client_closes().await;
    }))
    .await;
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(MockTool {
            name: "long_job",
            long_running: true,
            result: Map::new(),
        }),
        Arc::new(MockTool {
            name: "plain_job",
            long_running: false,
            result: json!({"ok": true}).as_object().unwrap().clone(),
        }),
    ];
    let flow = Arc::new(Flow {
        model: Some(server.model().await),
        request_processors: vec![
            crate::llm::live::test_server::live_config_processor as RequestProcessor,
        ],
        tools: parking_lot::RwLock::new(tools),
        ..Default::default()
    });
    let (ctx, _cancel) = live_ctx();
    let (sess, mut stream) = run_live(flow, ctx).unwrap();

    loop {
        let item = tokio::time::timeout(Duration::from_secs(10), stream.next())
            .await
            .expect("no function call event");
        let ev = match item {
            None => panic!("stream ended before the function call event"),
            Some(Err(e)) => panic!("run_live yielded an error: {e}"),
            Some(Ok(ev)) => ev,
        };
        if function_calls(ev.response.content.as_ref()).is_empty() {
            continue;
        }
        assert_eq!(ev.long_running_tool_ids, vec!["lr_1".to_owned()]);
        break;
    }
    sess.close().await.unwrap();
}

// parity: skipped TestSendAfterToolWriteFailure: the failing function-response write comes from json.Marshal rejecting a NaN result; a serde_json Value cannot hold NaN, so a gadk tool result always encodes

/// After the server acknowledges one functionResponse it pushes a model frame; the consumer
/// receives that frame, and no further functionResponse reaches the wire.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_run_live_tool_response_clears_pending_send() {
    const TOOL_CALL: &str =
        r#"{"toolCall":{"functionCalls":[{"id":"t1","name":"counted_tool","args":{}}]}}"#;
    const SERVER_CONTENT_THANKS: &str = r#"{"serverContent":{"modelTurn":{"parts":[{"text":"thanks"}],"role":"model"},"turnComplete":true}}"#;
    let function_responses = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = function_responses.clone();
    let server = FakeLiveServer::start(serve(move |_n: usize, mut conn: FakeConn| {
        let count = count.clone();
        async move {
            assert!(conn.write_text(TOOL_CALL).await, "writing toolCall failed");
            while let Some(m) = conn.read_data().await {
                if m.to_text().is_ok_and(|t| t.contains("functionResponse")) {
                    count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    if !conn.write_text(SERVER_CONTENT_THANKS).await {
                        return;
                    }
                }
            }
        }
    }))
    .await;
    let tool: Arc<dyn Tool> = Arc::new(HookTool {
        name: "counted_tool",
        run: Arc::new(|| Box::pin(async { json!({"ok": true}).as_object().unwrap().clone() })),
    });
    let flow = live_flow_with_tools(server.model().await, vec![tool]);
    let (ctx, cancel) = live_ctx();
    let (_sess, mut stream) = run_live(flow, ctx).unwrap();

    next_function_response(&mut stream).await;
    // The next event must be the model frame, not another response.
    loop {
        let item = tokio::time::timeout(Duration::from_secs(10), stream.next())
            .await
            .expect("no model frame");
        let ev = match item {
            None => panic!("iterator ended before the model frame"),
            Some(Err(e)) => panic!("RunLive yielded an error: {e}"),
            Some(Ok(ev)) => ev,
        };
        let text = ev
            .response
            .content
            .iter()
            .flat_map(|c| c.parts.iter())
            .map(|p| p.text.as_str())
            .rfind(|t| !t.is_empty());
        if text == Some("thanks") {
            break;
        }
    }

    // Give a hypothetical stuck sender a window to show its behaviour.
    let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
    while tokio::time::Instant::now() < deadline {
        let got = function_responses.load(std::sync::atomic::Ordering::SeqCst);
        assert!(
            got <= 1,
            "functionResponse reached the wire {got} times, want exactly 1"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    cancel.cancel();
    while stream.next().await.is_some() {}
    assert_eq!(
        function_responses.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "functionResponse wire count"
    );
    assert_eq!(
        server.conn_count(),
        1,
        "connection count (scenario needs no reconnect)"
    );
}

/// The token usage the Live API reports reaches the event RunLive yields, with the Live API's
/// response token count mapped to CandidatesTokenCount.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "8a bug: the live connection drops a server message's usageMetadata, so run_live events carry no usage (Go f2dc5d3a maps it, responseTokenCount to candidatesTokenCount)"]
async fn test_run_live_events_carry_usage_metadata() {
    const TURN_WITH_USAGE: &str = r#"{"serverContent":{"turnComplete":true},"usageMetadata":{"promptTokenCount":174,"responseTokenCount":146,"totalTokenCount":320}}"#;
    let server = FakeLiveServer::start(serve(|_n: usize, mut conn: FakeConn| async move {
        assert!(
            conn.write_text(TURN_WITH_USAGE).await,
            "writing usage message failed"
        );
        conn.block_until_client_closes().await;
    }))
    .await;
    let flow = live_flow(server.model().await, None);
    let (ctx, _cancel) = live_ctx();
    let (sess, mut stream) = run_live(flow, ctx).unwrap();
    loop {
        let item = tokio::time::timeout(Duration::from_secs(10), stream.next())
            .await
            .expect("no turn-complete event");
        let ev = match item {
            None => panic!("iterator ended before the turn-complete event"),
            Some(Err(e)) => panic!("RunLive yielded an error: {e}"),
            Some(Ok(ev)) => ev,
        };
        if !ev.response.turn_complete {
            continue;
        }
        assert_eq!(
            ev.response.usage_metadata,
            Some(gadk_genai::GenerateContentResponseUsageMetadata {
                prompt_token_count: 174,
                candidates_token_count: 146,
                total_token_count: 320,
                ..Default::default()
            }),
            "UsageMetadata"
        );
        break;
    }
    let _ = sess.close().await;
}

/// A reconnect sends the latest resumption handle in its setup frame, and that handle stays
/// out of the LiveRunConfig the caller passed in, which the caller may reuse for another
/// session.
// parity: adapted: Go shares the caller's *SessionResumptionConfig with the flow; a gadk
// RunConfig is cloned into the invocation, so the port checks the caller's copy and the
// setup frames.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_run_live_keeps_resumption_handle_out_of_caller_config() {
    const HANDLE_UPDATE: &str =
        r#"{"sessionResumptionUpdate":{"newHandle":"server-handle","resumable":true}}"#;
    let (setup_tx, mut setup_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let server = FakeLiveServer::start_with_setup(
        Arc::new(move |_n, frame| {
            let _ = setup_tx.send(frame);
        }),
        serve(|n: usize, mut conn: FakeConn| async move {
            if n > 1 {
                conn.block_until_client_closes().await;
                return;
            }
            assert!(
                conn.write_text(HANDLE_UPDATE).await,
                "writing resumption update failed"
            );
            // Returning hard-closes the connection, which the flow resumes from.
        }),
    )
    .await;
    let flow = live_flow(server.model().await, None);
    let caller_config = gadk_agent::RunConfig {
        streaming_mode: gadk_agent::StreamingMode::Bidi,
        live: Some(gadk_agent::LiveRunConfig {
            session_resumption: Some(gadk_genai::SessionResumptionConfig::default()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let (base, cancel) = live_ctx();
    let ctx = InvocationContext::builder()
        .agent(base.agent())
        .run_config(caller_config.clone())
        .cancellation_token(cancel.clone())
        .build_invocation();
    let (sess, stream) = run_live(flow, ctx).unwrap();
    let drain = tokio::spawn(async move {
        let mut stream = stream;
        while stream.next().await.is_some() {}
    });

    let mut got = Vec::new();
    for i in 0..2 {
        let frame = tokio::time::timeout(Duration::from_secs(10), setup_rx.recv())
            .await
            .unwrap_or_else(|_| panic!("connection {} never sent its setup frame", i + 1))
            .expect("setup channel closed");
        let msg: Value = serde_json::from_str(&frame)
            .unwrap_or_else(|e| panic!("decoding setup frame {frame:?}: {e}"));
        got.push(
            msg.pointer("/setup/sessionResumption/handle")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        );
    }
    let _ = sess.close().await;
    cancel.cancel();
    let _ = drain.await;

    assert_eq!(
        got,
        vec!["".to_owned(), "server-handle".to_owned()],
        "setup handles"
    );
    let handle = caller_config
        .live
        .as_ref()
        .and_then(|l| l.session_resumption.as_ref())
        .map(|r| r.handle.clone())
        .unwrap_or_default();
    assert_eq!(
        handle, "",
        "caller's SessionResumption.Handle, want it left empty"
    );
}
