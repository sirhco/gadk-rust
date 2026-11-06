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

//! Test-only fakes for the live flow: a Gemini Live wire-protocol server (port of Go
//! startFakeLiveServer / startClosableLiveServer in internal/llminternal/base_flow_live_test.go),
//! a scripted in-memory live model, and the helpers both live parity targets share
//! (Go newLiveInvocationContext, liveConfigProcessor, testLiveReconnectPolicy, startDrain, …).

use std::collections::VecDeque;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use futures::{SinkExt, StreamExt};
use gadk_agent::{
    AgentConfig, AgentError, EventStream, InvocationContext, LiveRunConfig, LiveSession, RunConfig,
    StreamingMode, agent,
};
use gadk_gemini::{ClientConfig, GeminiModel, MapEnv};
use gadk_genai::{Content, GenerateContentConfig, LiveConnectConfig};
use gadk_model::{
    Backend, HttpOptions, LiveConnError, LiveConnErrorKind, LiveConnection, Llm, LlmRequest,
    LlmResponse, ModelError, RealtimeInput,
};
use gadk_session::Event;
use parking_lot::Mutex;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tokio::task::{AbortHandle, JoinHandle};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_util::sync::CancellationToken;

use crate::llm::flow::{Flow, RequestProcessor};
use crate::llm::live::reconnect::LiveReconnectPolicy;
use crate::llm::live::session::{LiveProbe, LiveSessionImpl};

/// The server's reply to the client's setup frame.
pub(crate) const SETUP_COMPLETE: &str = r#"{"setupComplete":{}}"#;
/// Go serverContentPong: one complete model turn.
pub(crate) const SERVER_CONTENT_PONG: &str = r#"{"serverContent":{"modelTurn":{"parts":[{"text":"pong"}],"role":"model"},"turnComplete":true}}"#;
/// Go drainTimeout: every reconnect test drives a bounded policy, so a stream that has not
/// drained by now is not terminating at all.
pub(crate) const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

/// Go's `serveConn func(connNum int, conn *websocket.Conn)`: the per-connection script, run after
/// the setup exchange with a 1-based connection number. Returning drops the `FakeConn`, which
/// closes the TCP socket without a close handshake (Go: `defer conn.Close()`).
pub(crate) type ServeConn = Arc<dyn Fn(usize, FakeConn) -> BoxFuture<'static, ()> + Send + Sync>;

/// Boxes a closure as a [`ServeConn`].
pub(crate) fn serve<F, Fut>(f: F) -> ServeConn
where
    F: Fn(usize, FakeConn) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    Arc::new(move |n, conn| Box::pin(f(n, conn)))
}

/// The server side of one accepted Live connection (Go `*websocket.Conn` in the fake handler).
pub(crate) struct FakeConn {
    ws: WebSocketStream<TcpStream>,
}

impl FakeConn {
    /// Go `conn.WriteMessage(websocket.TextMessage, …)`; false when the write failed.
    pub(crate) async fn write_text(&mut self, text: &str) -> bool {
        self.ws.send(Message::text(text.to_owned())).await.is_ok()
    }

    /// Go `conn.WriteControl(websocket.CloseMessage, websocket.FormatCloseMessage(code, reason), …)`.
    pub(crate) async fn write_close(&mut self, code: u16, reason: &str) {
        let frame = CloseFrame {
            code: CloseCode::from(code),
            reason: reason.to_owned().into(),
        };
        let _ = self.ws.send(Message::Close(Some(frame))).await;
    }

    /// Go `conn.ReadMessage()`: the next data frame; `None` once the client closed, the socket
    /// failed or the stream ended. Pings and pongs are skipped (tungstenite answers pings).
    pub(crate) async fn read_data(&mut self) -> Option<Message> {
        loop {
            match self.ws.next().await {
                Some(Ok(m @ (Message::Text(_) | Message::Binary(_)))) => return Some(m),
                Some(Ok(Message::Close(_)) | Err(_)) | None => return None,
                Some(Ok(_)) => continue,
            }
        }
    }

    /// Go blockUntilClientCloses: parks until the client tears the connection down, so an idle
    /// server never triggers a spurious reconnect.
    pub(crate) async fn block_until_client_closes(&mut self) {
        while self.read_data().await.is_some() {}
    }
}

/// A Gemini Live wire-protocol fake on `127.0.0.1:0` (Go startClosableLiveServer): upgrades,
/// counts the connection (before the setup exchange, so a client that saw setupComplete always
/// observes it in the count), consumes the setup frame, replies `{"setupComplete":{}}`, then
/// runs the per-connection script.
pub(crate) struct FakeLiveServer {
    port: u16,
    conn_count: Arc<AtomicUsize>,
    /// The accept task; taken (and awaited) by `close_listener`.
    accept: Mutex<Option<JoinHandle<()>>>,
    conns: Arc<Mutex<Vec<AbortHandle>>>,
}

impl FakeLiveServer {
    pub(crate) async fn start(serve: ServeConn) -> FakeLiveServer {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let conn_count = Arc::new(AtomicUsize::new(0));
        let conns: Arc<Mutex<Vec<AbortHandle>>> = Arc::new(Mutex::new(Vec::new()));
        let (count, handles) = (conn_count.clone(), conns.clone());
        let accept = tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    return;
                };
                let (count, serve) = (count.clone(), serve.clone());
                let task = tokio::spawn(async move {
                    let Ok(ws) = tokio_tungstenite::accept_async(tcp).await else {
                        return;
                    };
                    let n = count.fetch_add(1, Ordering::SeqCst) + 1;
                    let mut conn = FakeConn { ws };
                    if conn.read_data().await.is_none() {
                        return;
                    }
                    if !conn.write_text(SETUP_COMPLETE).await {
                        return;
                    }
                    serve(n, conn).await;
                });
                handles.lock().push(task.abort_handle());
            }
        });
        FakeLiveServer {
            port,
            conn_count,
            accept: Mutex::new(Some(accept)),
            conns,
        }
    }

    /// The `ws://` base URL the model dials (the 1a Live tests use the same form).
    pub(crate) fn base_url(&self) -> String {
        format!("ws://127.0.0.1:{}/", self.port)
    }

    /// Go `connCount.Load()`.
    pub(crate) fn conn_count(&self) -> usize {
        self.conn_count.load(Ordering::SeqCst)
    }

    /// Go waitForConns: waits (up to 10 s) until at least `n` connections were accepted.
    pub(crate) async fn wait_for_conns(&self, n: usize) {
        for _ in 0..2000 {
            if self.conn_count() >= n {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!(
            "server accepted {} connections, want at least {n}",
            self.conn_count()
        );
    }

    /// Go `closeServer()` (closes the listener, not the server): every later dial is refused;
    /// connections already accepted keep running. Aborting the accept task drops the listener,
    /// and awaiting it means the listener is gone (the port refuses dials) once this returns.
    pub(crate) async fn close_listener(&self) {
        let accept = self.accept.lock().take();
        if let Some(accept) = accept {
            accept.abort();
            let _ = accept.await;
        }
    }

    /// Go `genai.NewClient(…Backend: BackendGeminiAPI, APIKey: "test-api-key", BaseURL: ws://…)`
    /// wrapped in fakeLiveModel (name "test-live-model"): a real `GeminiModel` pointed at the fake.
    pub(crate) async fn model(&self) -> Arc<dyn Llm> {
        GeminiModel::new(
            "test-live-model",
            ClientConfig {
                backend: Some(Backend::GeminiApi),
                api_key: Some("test-api-key".into()),
                http_options: HttpOptions {
                    base_url: Some(self.base_url()),
                    ..Default::default()
                },
                env: Arc::new(MapEnv::default()),
                ..Default::default()
            },
        )
        .await
        .unwrap()
    }
}

impl Drop for FakeLiveServer {
    fn drop(&mut self) {
        if let Some(accept) = self.accept.lock().take() {
            accept.abort();
        }
        for h in self.conns.lock().drain(..) {
            h.abort();
        }
    }
}

/// Shared state of a [`ScriptedLiveLlm`] and every connection it opened.
struct ScriptedState {
    responses: Mutex<VecDeque<LlmResponse>>,
    sent: Mutex<Vec<Content>>,
    closed: AtomicBool,
    close_notify: Notify,
    fail_history: bool,
    connects: AtomicUsize,
}

/// An [`Llm`] whose `connect_live` returns an in-memory connection that yields `responses` in
/// order and records what the flow sends. For tests that need no socket.
pub(crate) struct ScriptedLiveLlm {
    name: String,
    state: Arc<ScriptedState>,
}

impl ScriptedLiveLlm {
    fn build(name: &str, responses: Vec<LlmResponse>, fail_history: bool) -> Arc<ScriptedLiveLlm> {
        Arc::new(ScriptedLiveLlm {
            name: name.to_owned(),
            state: Arc::new(ScriptedState {
                responses: Mutex::new(responses.into()),
                sent: Mutex::new(Vec::new()),
                closed: AtomicBool::new(false),
                close_notify: Notify::new(),
                fail_history,
                connects: AtomicUsize::new(0),
            }),
        })
    }

    pub(crate) fn new(name: &str, responses: Vec<LlmResponse>) -> Arc<ScriptedLiveLlm> {
        Self::build(name, responses, false)
    }

    /// `send_history` fails client-side, as Go's NaN history does (`json: unsupported value: NaN`).
    pub(crate) fn failing_history(name: &str) -> Arc<ScriptedLiveLlm> {
        Self::build(name, Vec::new(), true)
    }

    #[expect(dead_code, reason = "for the later live tasks' tests (contract C4)")]
    pub(crate) fn sent_contents(&self) -> Vec<Content> {
        self.state.sent.lock().clone()
    }

    pub(crate) fn closed(&self) -> bool {
        self.state.closed.load(Ordering::SeqCst)
    }

    pub(crate) fn connects(&self) -> usize {
        self.state.connects.load(Ordering::SeqCst)
    }
}

impl Llm for ScriptedLiveLlm {
    fn name(&self) -> &str {
        &self.name
    }

    fn generate_content(
        &self,
        _req: LlmRequest,
        _stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        futures::stream::empty().boxed()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn connect_live<'a>(
        &'a self,
        _cfg: LiveConnectConfig,
    ) -> BoxFuture<'a, Result<Box<dyn LiveConnection>, ModelError>> {
        self.state.connects.fetch_add(1, Ordering::SeqCst);
        let conn: Box<dyn LiveConnection> = Box::new(ScriptedConn(self.state.clone()));
        Box::pin(async move { Ok(conn) })
    }
}

struct ScriptedConn(Arc<ScriptedState>);

#[async_trait]
impl LiveConnection for ScriptedConn {
    async fn send_history(&self, _history: &[Content]) -> Result<(), LiveConnError> {
        if self.0.fail_history {
            return Err(LiveConnError::new(
                LiveConnErrorKind::Other,
                "failed to send history: json: unsupported value: NaN",
            ));
        }
        Ok(())
    }

    async fn send_content(&self, content: Content) -> Result<(), LiveConnError> {
        self.0.sent.lock().push(content);
        Ok(())
    }

    async fn send_realtime(&self, _input: RealtimeInput) -> Result<(), LiveConnError> {
        Ok(())
    }

    /// The scripted responses in order, then parks until `close`, then a non-resumable error.
    async fn recv(&self) -> Result<Option<LlmResponse>, LiveConnError> {
        loop {
            let notified = self.0.close_notify.notified();
            if let Some(r) = self.0.responses.lock().pop_front() {
                return Ok(Some(r));
            }
            if self.0.closed.load(Ordering::SeqCst) {
                return Err(LiveConnError::new(
                    LiveConnErrorKind::Other,
                    "scripted connection closed",
                ));
            }
            notified.await;
        }
    }

    async fn close(&self) -> Result<(), LiveConnError> {
        self.0.closed.store(true, Ordering::SeqCst);
        self.0.close_notify.notify_waiters();
        Ok(())
    }
}

/// Go newLiveInvocationContext: an agent named "test-live-agent", `RunConfig{Bidi, Live: {}}`,
/// and a cancellable invocation (the returned token is Go's `cancel`).
pub(crate) fn live_ctx() -> (InvocationContext, CancellationToken) {
    let cancel = CancellationToken::new();
    let ctx = InvocationContext::builder()
        .agent(
            agent::new(AgentConfig {
                name: "test-live-agent".into(),
                ..Default::default()
            })
            .unwrap(),
        )
        .run_config(RunConfig {
            streaming_mode: StreamingMode::Bidi,
            live: Some(LiveRunConfig::default()),
            ..Default::default()
        })
        .cancellation_token(cancel.clone())
        .build_invocation();
    (ctx, cancel)
}

/// Go liveConfigProcessor: the minimal request processor `run_live` needs (it populates
/// `req.config`, which the connect config is built from).
pub(crate) fn live_config_processor<'a>(
    _ctx: &'a InvocationContext,
    req: &'a mut LlmRequest,
    _f: &'a Flow,
) -> BoxFuture<'a, Result<Vec<Event>, AgentError>> {
    req.config = Some(GenerateContentConfig::default());
    Box::pin(async { Ok(Vec::new()) })
}

/// Go newReconnectFlow / the base_flow_live tests' Flow literal.
pub(crate) fn live_flow(model: Arc<dyn Llm>, policy: Option<LiveReconnectPolicy>) -> Arc<Flow> {
    Arc::new(Flow {
        model: Some(model),
        request_processors: vec![live_config_processor as RequestProcessor],
        live_reconnect: policy,
        ..Default::default()
    })
}

/// Go testLiveReconnectPolicy: the defaults with the delays shrunk.
pub(crate) fn test_live_reconnect_policy(
    max_attempts: usize,
    max_total: usize,
) -> LiveReconnectPolicy {
    LiveReconnectPolicy {
        initial_backoff: Duration::from_millis(1),
        max_backoff: Duration::from_millis(10),
        jitter: 0.0,
        max_attempts,
        max_total,
        // Every connection a fake server makes dies in milliseconds, so this only has to exceed
        // that for them all to count as short-lived.
        healthy_uptime: Duration::from_secs(1),
    }
}

/// The session's task probe (the flow's session is always a `LiveSessionImpl`).
pub(crate) fn probe_of(sess: &Arc<dyn LiveSession>) -> Arc<LiveProbe> {
    sess.as_any()
        .downcast_ref::<LiveSessionImpl>()
        .expect("run_live returns a LiveSessionImpl")
        .probe()
        .clone()
}

/// Go assertNoRunLiveLeak, adapted: waits (up to 5 s) until every task the flow spawned has
/// ended. Go counts RunLive goroutines in `runtime.Stack`; Rust counts live `LiveTaskGuard`s.
pub(crate) async fn assert_no_run_live_leak(probe: &LiveProbe) {
    for _ in 0..250 {
        if probe.live_tasks.load(Ordering::SeqCst) == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "leaked {} live flow tasks",
        probe.live_tasks.load(Ordering::SeqCst)
    );
}

/// Go waitForReconnectBackoff: blocks (up to 10 s) until the flow is parked in the reconnect
/// wait, so a teardown test observes the wait and not an earlier point in the loop.
pub(crate) async fn wait_for_backoff(probe: &LiveProbe) {
    for _ in 0..2000 {
        if probe.in_backoff.load(Ordering::SeqCst) > 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("the live flow never reached the reconnect backoff");
}

/// Go drainResult.
pub(crate) struct Drained {
    pub(crate) events: usize,
    pub(crate) last_err: Option<AgentError>,
}

/// Go startDrain: consumes the stream to exhaustion on its own task. Draining must begin before
/// the flow is expected to make progress, because pushes are rendezvous.
pub(crate) fn start_drain(mut stream: EventStream) -> JoinHandle<Drained> {
    tokio::spawn(async move {
        let mut d = Drained {
            events: 0,
            last_err: None,
        };
        while let Some(item) = stream.next().await {
            match item {
                Ok(_) => d.events += 1,
                Err(e) => d.last_err = Some(e),
            }
        }
        d
    })
}

/// Go awaitDrain: fails rather than hanging when the stream never ends.
pub(crate) async fn await_drain(h: JoinHandle<Drained>) -> Drained {
    tokio::time::timeout(DRAIN_TIMEOUT, h)
        .await
        .expect("stream never drained: the flow is not terminating")
        .unwrap()
}
