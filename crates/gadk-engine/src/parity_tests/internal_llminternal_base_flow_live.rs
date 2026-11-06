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
                    // Cancel only after the first event (the setup-complete acknowledgement), so
                    // the reader is parked in recv when cleanup tears the connection down.
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
            conn.block_until_client_closes().await; // an idle server: no unsolicited traffic
            let _ = gone.try_send(());
        }
    }))
    .await;
    let flow = live_flow(server.model().await, None);
    // The invocation is deliberately left alive: this test is about close() on its own.
    let (ctx, _cancel) = live_ctx();
    let (sess, mut stream) = run_live(flow, ctx).unwrap();
    let probe = probe_of(&sess);

    // The setup-complete acknowledgement: the connection is up.
    let first = tokio::time::timeout(Duration::from_secs(10), stream.next())
        .await
        .expect("no setup-complete event");
    assert!(
        first.is_some(),
        "stream ended before the setup-complete event; the connection never came up"
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
