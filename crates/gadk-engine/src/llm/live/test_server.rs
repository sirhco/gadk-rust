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
//! the scripted in-memory live model (re-exported from `testutil::live`, where it is public
//! under the `testutil` feature), and the helpers both live parity targets share
//! (Go newLiveInvocationContext, liveConfigProcessor, testLiveReconnectPolicy, startDrain, …).

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures::future::BoxFuture;
use futures::{SinkExt, StreamExt};
use gadk_agent::{
    AgentConfig, AgentError, EventStream, InvocationContext, LiveRunConfig, LiveSession, RunConfig,
    StreamingMode, agent,
};
use gadk_gemini::{ClientConfig, GeminiModel, MapEnv};
use gadk_genai::GenerateContentConfig;
use gadk_model::{Backend, HttpOptions, Llm, LlmRequest};
use gadk_session::Event;
use parking_lot::Mutex;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{AbortHandle, JoinHandle};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_util::sync::CancellationToken;

use crate::llm::flow::{Flow, RequestProcessor};
use crate::llm::live::reconnect::LiveReconnectPolicy;
use crate::llm::live::session::{LiveProbe, LiveSessionImpl};
pub(crate) use crate::testutil::live::ScriptedLiveLlm;

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

/// Go startLiveServerWithSetup's `onSetup func(connNum int, frame []byte)`: receives each
/// connection's setup frame before setupComplete is sent.
pub(crate) type OnSetup = Arc<dyn Fn(usize, String) + Send + Sync>;

impl FakeLiveServer {
    pub(crate) async fn start(serve: ServeConn) -> FakeLiveServer {
        Self::start_with_setup(Arc::new(|_, _| {}), serve).await
    }

    /// Go startLiveServerWithSetup: [`start`](Self::start) plus `on_setup`, which receives each
    /// connection's setup frame before setupComplete is sent.
    pub(crate) async fn start_with_setup(on_setup: OnSetup, serve: ServeConn) -> FakeLiveServer {
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
                let (count, serve, on_setup) = (count.clone(), serve.clone(), on_setup.clone());
                let task = tokio::spawn(async move {
                    let Ok(ws) = tokio_tungstenite::accept_async(tcp).await else {
                        return;
                    };
                    let n = count.fetch_add(1, Ordering::SeqCst) + 1;
                    let mut conn = FakeConn { ws };
                    let Some(setup) = conn.read_data().await else {
                        return;
                    };
                    on_setup(
                        n,
                        setup.into_text().map(|t| t.to_string()).unwrap_or_default(),
                    );
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
        self.model_named("test-live-model").await
    }

    /// [`model`](Self::model) with the given model name (Go namedFakeLiveModel).
    pub(crate) async fn model_named(&self, name: &str) -> Arc<dyn Llm> {
        GeminiModel::new(
            name,
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
