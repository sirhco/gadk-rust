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

//! Ports of ADK-Go `server/adkrest/controllers/runtime_live_test.go`: `/run_live` served by a
//! real `axum::serve` on 127.0.0.1 and driven by a tokio-tungstenite client (and, where the
//! client must not answer a close frame, a raw frame client).
//!
//! Go's handler-exit channel becomes the server end of the TCP connection being dropped: the
//! test listener wraps each accepted stream and reports its drop, which happens once the live
//! session's last task has ended and its slot is released. Go's `stallingConn` becomes the same
//! wrapper with its writes held pending while stalled.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::any::Any;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use gadk_agent::{
    Agent, AgentConfig, AgentError, AgentState, AgentType, EventStream, InvocationContext,
    LiveAgent, LiveRequest, LiveSession, SingleLoader,
};
use gadk_genai::{Content, Part};
use gadk_model::RealtimeInput;
use gadk_platform::Platform;
use gadk_server::rest::Timeout;
use gadk_server::rest::controllers::{RuntimeApiController, RuntimeApiControllerConfig};
use gadk_server::rest::testutil::{FakeSessionService, SessionKey, TestSession};
use gadk_session::Event;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpListener, TcpSocket, TcpStream};
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::time::{Instant, sleep, timeout};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tokio_util::sync::CancellationToken;

const TEST_LIVE_APP_NAME: &str = "testApp";
const TEST_LIVE_USER_ID: &str = "testUser";
const TEST_LIVE_SESSION_ID: &str = "testSession";
const TEST_WEB_SOCKET_READ_TIMEOUT: Duration = Duration::from_secs(1);
/// Below the handler's one-second close drain, so the test sees it wait for the close reply.
const TEST_CLOSE_DRAIN_GRACE_PERIOD: Duration = Duration::from_millis(50);
const TEST_CLOSE_REPLY_EXIT_TIMEOUT: Duration = Duration::from_millis(100);
const DEFAULT_MAX_LIVE_MESSAGE_BYTES: usize = 16 << 20;

type Client = WebSocketStream<MaybeTlsStream<TcpStream>>;
type LiveRun = (Arc<dyn LiveSession>, EventStream);
type RunLiveFn = Arc<
    dyn Fn() -> Pin<Box<dyn Future<Output = Result<LiveRun, AgentError>> + Send>> + Send + Sync,
>;

// ---------------------------------------------------------------- the live agent double

/// Go `mockLiveAgent`: a custom agent whose live run is the test's.
struct MockLiveAgent(RunLiveFn);

#[async_trait]
impl LiveAgent for MockLiveAgent {
    async fn run_live(&self, _ctx: InvocationContext) -> Result<LiveRun, AgentError> {
        (self.0)().await
    }
}

fn mock_live_agent(run_live: RunLiveFn) -> Arc<dyn Agent> {
    gadk_agent::new_with_state(
        AgentConfig {
            name: TEST_LIVE_APP_NAME.into(),
            ..Default::default()
        },
        AgentState {
            agent_type: AgentType::CustomAgent,
            config: None,
            on_before_agent_callback_event: None,
            live: Some(Arc::new(MockLiveAgent(run_live))),
        },
    )
    .unwrap()
}

/// A [`RunLiveFn`] from an async closure.
fn run_live_fn<F, Fut>(f: F) -> RunLiveFn
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<LiveRun, AgentError>> + Send + 'static,
{
    Arc::new(move || Box::pin(f()))
}

/// Go `recordingLiveSession`: requests go to a one-slot channel; `closed` is cancelled once.
struct RecordingLiveSession {
    tx: mpsc::Sender<LiveRequest>,
    rx: tokio::sync::Mutex<mpsc::Receiver<LiveRequest>>,
    closed: CancellationToken,
}

impl RecordingLiveSession {
    fn new() -> Arc<Self> {
        let (tx, rx) = mpsc::channel(1);
        Arc::new(Self {
            tx,
            rx: tokio::sync::Mutex::new(rx),
            closed: CancellationToken::new(),
        })
    }
}

#[async_trait]
impl LiveSession for RecordingLiveSession {
    async fn send(&self, req: LiveRequest) -> Result<(), AgentError> {
        let _ = self.tx.send(req).await;
        Ok(())
    }
    async fn close(&self) -> Result<(), AgentError> {
        self.closed.cancel();
        Ok(())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Go's `yield`: hands one item to the consumer and returns once the consumer asks for the
/// next one (true) or has dropped the stream (false).
struct Yielder(mpsc::Sender<(Result<Event, AgentError>, oneshot::Sender<()>)>);

impl Yielder {
    async fn yield_(&self, item: Result<Event, AgentError>) -> bool {
        let (ack_tx, ack_rx) = oneshot::channel();
        if self.0.send((item, ack_tx)).await.is_err() {
            return false;
        }
        ack_rx.await.is_ok()
    }
}

/// Aborts the producer task when the stream is dropped.
struct AbortOnDrop(Option<tokio::task::JoinHandle<()>>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        if let Some(h) = self.0.take() {
            h.abort();
        }
    }
}

/// Go `iter.Seq2[*session.Event, error]`: a producer that runs when the stream is first polled
/// and is resumed only as the consumer asks for more.
fn event_stream<F, Fut>(producer: F) -> EventStream
where
    F: FnOnce(Yielder) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let (tx, rx) = mpsc::channel(1);
    let start: Option<Box<dyn FnOnce() -> tokio::task::JoinHandle<()> + Send>> =
        Some(Box::new(move || {
            tokio::spawn(async move {
                // The stream ends when the producer returns, not when it drops its yielder.
                let keep = tx.clone();
                producer(Yielder(tx)).await;
                drop(keep);
            })
        }));
    futures::stream::unfold(
        (rx, start, None::<oneshot::Sender<()>>, AbortOnDrop(None)),
        |(mut rx, mut start, prev, mut guard)| async move {
            if let Some(start) = start.take() {
                guard.0 = Some(start());
            }
            if let Some(ack) = prev {
                let _ = ack.send(());
            }
            let (item, ack) = rx.recv().await?;
            Some((item, (rx, start, Some(ack), guard)))
        },
    )
    .boxed()
}

/// Go `blockingLiveRun`: no events; the stream ends when the session is closed.
fn blocking_live_run(session: Arc<RecordingLiveSession>) -> RunLiveFn {
    run_live_fn(move || {
        let session = session.clone();
        async move {
            let closed = session.closed.clone();
            let s: Arc<dyn LiveSession> = session;
            Ok((
                s,
                event_stream(move |_| async move { closed.cancelled().await }),
            ))
        }
    })
}

/// [`blocking_live_run`] with a session of its own per connection; every session's requests
/// arrive on the returned receiver.
fn per_connection_live_run() -> (RunLiveFn, mpsc::UnboundedReceiver<LiveRequest>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let run = run_live_fn(move || {
        let tx = tx.clone();
        async move {
            let session = RecordingLiveSession::new();
            let s = session.clone();
            tokio::spawn(async move {
                let mut rx = s.rx.lock().await;
                while let Some(req) = rx.recv().await {
                    let _ = tx.send(req);
                }
            });
            let closed = session.closed.clone();
            let sess: Arc<dyn LiveSession> = session;
            Ok((
                sess,
                event_stream(move |_| async move { closed.cancelled().await }),
            ))
        }
    });
    (run, rx)
}

async fn next_request(rx: &mut mpsc::UnboundedReceiver<LiveRequest>) -> LiveRequest {
    timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("timed out waiting for request to reach the live session")
        .expect("request channel open")
}

/// Go `makeEvent`.
fn make_event(id: &str, author: &str, text: &str) -> Event {
    let mut e = Event::new(&Platform::default(), id);
    e.author = author.into();
    e.response.content = Some(Content {
        parts: vec![Part::from_text(text)],
        ..Default::default()
    });
    e
}

// ---------------------------------------------------------------- the server

/// Go `stallingConn`'s switch: while stalled, the server's writes stay pending.
#[derive(Default)]
struct Stall {
    state: Mutex<(bool, Option<Waker>)>,
    /// Signalled when a write starts waiting (Go `blocked`, one buffered signal).
    blocked: Notify,
}

impl Stall {
    fn stall(&self) {
        self.state.lock().0 = true;
    }
    fn unstall(&self) {
        let waker = {
            let mut s = self.state.lock();
            s.0 = false;
            s.1.take()
        };
        if let Some(w) = waker {
            w.wake();
        }
    }
}

/// The server end of one test connection: reports its drop on `exits` and holds writes while
/// its [`Stall`] is on.
struct ProbeConn {
    inner: TcpStream,
    stall: Arc<Stall>,
    exits: mpsc::UnboundedSender<()>,
}

impl Drop for ProbeConn {
    fn drop(&mut self) {
        let _ = self.exits.send(());
    }
}

impl AsyncRead for ProbeConn {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for ProbeConn {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        {
            let mut s = self.stall.state.lock();
            if s.0 {
                s.1 = Some(cx.waker().clone());
                drop(s);
                self.stall.blocked.notify_one();
                return Poll::Pending;
            }
        }
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

struct ProbeListener {
    inner: TcpListener,
    exits: mpsc::UnboundedSender<()>,
    stalls: mpsc::UnboundedSender<Arc<Stall>>,
}

impl axum::serve::Listener for ProbeListener {
    type Io = ProbeConn;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            if let Ok((inner, addr)) = self.inner.accept().await {
                let stall = Arc::new(Stall::default());
                let _ = self.stalls.send(stall.clone());
                let conn = ProbeConn {
                    inner,
                    stall,
                    exits: self.exits.clone(),
                };
                return (conn, addr);
            }
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }
}

struct TestServer {
    addr: SocketAddr,
    ws_url: String,
    /// One signal per connection whose server end was dropped.
    exits: mpsc::UnboundedReceiver<()>,
    /// The stall switch of each accepted connection, in accept order.
    stalls: mpsc::UnboundedReceiver<Arc<Stall>>,
}

/// Go `startRunLiveServer`: `run_live_handler` over `cfg` (with the shared session service and
/// agent loader) on 127.0.0.1. `send_buffer` sets the listening socket's `SO_SNDBUF`, which
/// accepted connections inherit (Go `smallBufferListener`).
async fn start_run_live_server_with(
    configure: impl FnOnce(&mut RuntimeApiControllerConfig),
    run_live: RunLiveFn,
    send_buffer: Option<u32>,
) -> TestServer {
    let id = SessionKey {
        app_name: TEST_LIVE_APP_NAME.into(),
        user_id: TEST_LIVE_USER_ID.into(),
        session_id: TEST_LIVE_SESSION_ID.into(),
    };
    let sessions = FakeSessionService::new([TestSession::new(
        id,
        serde_json::Map::new(),
        vec![],
        chrono::Utc::now(),
    )]);
    let mut cfg = RuntimeApiControllerConfig::new(Arc::new(sessions));
    cfg.agent_loader = Some(Arc::new(SingleLoader::new(mock_live_agent(run_live))));
    configure(&mut cfg);
    let controller = Arc::new(RuntimeApiController::new(cfg));

    let router = axum::Router::new().route(
        "/run_live",
        axum::routing::get(move |req: axum::extract::Request| {
            let c = controller.clone();
            async move { c.run_live_handler(req).await }
        }),
    );
    let socket = TcpSocket::new_v4().unwrap();
    if let Some(n) = send_buffer {
        socket.set_send_buffer_size(n).unwrap();
    }
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let listener = socket.listen(64).unwrap();
    let addr = listener.local_addr().unwrap();
    let (exits_tx, exits) = mpsc::unbounded_channel();
    let (stalls_tx, stalls) = mpsc::unbounded_channel();
    let probe = ProbeListener {
        inner: listener,
        exits: exits_tx,
        stalls: stalls_tx,
    };
    tokio::spawn(async move {
        let _ = axum::serve(probe, router).await;
    });
    TestServer {
        addr,
        ws_url: format!(
            "ws://{addr}/run_live?appName={TEST_LIVE_APP_NAME}&userId={TEST_LIVE_USER_ID}&sessionId={TEST_LIVE_SESSION_ID}"
        ),
        exits,
        stalls,
    }
}

async fn start_run_live_server(
    configure: impl FnOnce(&mut RuntimeApiControllerConfig),
    run_live: RunLiveFn,
) -> TestServer {
    start_run_live_server_with(configure, run_live, None).await
}

/// Go `dialLive`: one connection whose handshake must be accepted.
async fn dial_live(ws_url: &str) -> Client {
    let (client, resp) = tokio_tungstenite::connect_async(ws_url)
        .await
        .expect("websocket dial");
    assert_eq!(resp.status(), 101, "handshake status");
    client
}

/// Go `dialRunLiveHandler`.
async fn dial_run_live_handler(run_live: RunLiveFn) -> (Client, TestServer) {
    let server = start_run_live_server(|_| {}, run_live).await;
    (dial_live(&server.ws_url).await, server)
}

/// Go `waitForHandlerExit`.
async fn wait_for_handler_exit(exits: &mut mpsc::UnboundedReceiver<()>) {
    timeout(Duration::from_secs(1), exits.recv())
        .await
        .expect("RunLiveHandler did not exit")
        .expect("exit channel open");
}

/// The next data or close message, answering pings on the way (gorilla `ReadMessage`).
async fn read_message(client: &mut Client) -> Message {
    let deadline = Instant::now() + TEST_WEB_SOCKET_READ_TIMEOUT;
    loop {
        let msg = tokio::time::timeout_at(deadline, client.next())
            .await
            .expect("read timed out")
            .expect("connection ended")
            .expect("read failed");
        if !matches!(msg, Message::Ping(_) | Message::Pong(_)) {
            return msg;
        }
    }
}

/// Go `ReadJSON` of one event.
async fn read_event(client: &mut Client) -> Value {
    match read_message(client).await {
        Message::Text(t) => serde_json::from_str(t.as_str()).unwrap(),
        other => panic!("ReadJSON() got {other:?}, want a text frame"),
    }
}

/// Go `readCloseError`: the code and reason of the server's close frame.
async fn read_close_error(client: &mut Client) -> (u16, String) {
    match read_message(client).await {
        Message::Close(Some(CloseFrame { code, reason })) => (code.into(), reason.to_string()),
        other => panic!("ReadMessage() got {other:?}, want a close frame"),
    }
}

/// Go `waitForLiveRequest`.
async fn wait_for_live_request(session: &RecordingLiveSession) -> LiveRequest {
    timeout(Duration::from_secs(1), async {
        session.rx.lock().await.recv().await
    })
    .await
    .expect("timed out waiting for request to reach the live session")
    .expect("request channel open")
}

/// Go `closeLiveClient`.
async fn close_live_client(client: &mut Client, session: &RecordingLiveSession) {
    client
        .send(Message::text(json!({"close": true}).to_string()))
        .await
        .unwrap();
    timeout(Duration::from_secs(1), session.closed.cancelled())
        .await
        .expect("live session was not closed after the client close request");
}

fn close_frame(code: u16, reason: &str) -> Message {
    Message::Close(Some(CloseFrame {
        code: code.into(),
        reason: reason.into(),
    }))
}

/// Go `answerPings`: a client that reads (and so answers pings) but has nothing to say.
fn answer_pings(mut client: Client) {
    tokio::spawn(async move { while let Some(Ok(_)) = client.next().await {} });
}

/// Sends a pong every `every` until the connection fails or `stop` is cancelled.
fn send_pongs<S>(sink: Arc<tokio::sync::Mutex<S>>, every: Duration, stop: CancellationToken)
where
    S: futures::Sink<Message> + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval_at(Instant::now() + every, every);
        loop {
            tokio::select! {
                () = stop.cancelled() => return,
                _ = ticker.tick() => {}
            }
            if sink
                .lock()
                .await
                .send(Message::Pong(Bytes::new()))
                .await
                .is_err()
            {
                return;
            }
        }
    });
}

/// Go's log-capturing `synchronizedBuffer`: tracing output of this thread (the tests run on a
/// current-thread runtime, so the server's tasks log here too).
#[derive(Clone, Default)]
struct LogBuffer(Arc<Mutex<Vec<u8>>>);

impl io::Write for LogBuffer {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuffer {
    type Writer = LogBuffer;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

impl LogBuffer {
    fn capture(&self) -> tracing::subscriber::DefaultGuard {
        tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .with_writer(self.clone())
                .with_ansi(false)
                .finish(),
        )
    }
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock()).into_owned()
    }
}

/// A WebSocket client over a raw socket that answers nothing on its own (Go's test disables
/// gorilla's automatic close reply).
struct RawClient(TcpStream);

impl RawClient {
    async fn dial(addr: SocketAddr) -> Self {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let req = format!(
            "GET /run_live?appName={TEST_LIVE_APP_NAME}&userId={TEST_LIVE_USER_ID}&sessionId={TEST_LIVE_SESSION_ID} HTTP/1.1\r\nHost: {addr}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
        );
        s.write_all(req.as_bytes()).await.unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            head.push(s.read_u8().await.unwrap());
        }
        assert!(
            head.starts_with(b"HTTP/1.1 101"),
            "handshake: {}",
            String::from_utf8_lossy(&head)
        );
        Self(s)
    }

    /// One server frame: opcode and payload (server frames are unmasked).
    async fn read_frame(&mut self) -> (u8, Vec<u8>) {
        let b0 = self.0.read_u8().await.unwrap();
        let b1 = self.0.read_u8().await.unwrap();
        let len = match b1 & 0x7f {
            126 => u64::from(self.0.read_u16().await.unwrap()),
            127 => self.0.read_u64().await.unwrap(),
            n => u64::from(n),
        };
        let mut payload = vec![0; usize::try_from(len).unwrap()];
        self.0.read_exact(&mut payload).await.unwrap();
        (b0 & 0x0f, payload)
    }

    /// One masked client frame (a zero mask key leaves the payload as is).
    async fn write_frame(&mut self, opcode: u8, payload: &[u8]) {
        assert!(payload.len() < 126);
        let mut frame = vec![0x80 | opcode, 0x80 | u8::try_from(payload.len()).unwrap()];
        frame.extend_from_slice(&[0, 0, 0, 0]);
        frame.extend_from_slice(payload);
        self.0.write_all(&frame).await.unwrap();
    }
}

fn close_payload(code: u16, reason: &str) -> Vec<u8> {
    let mut p = code.to_be_bytes().to_vec();
    p.extend_from_slice(reason.as_bytes());
    p
}

fn first_text(event: &Value) -> Option<&str> {
    event["content"]["parts"][0]["text"].as_str()
}

fn blob(req: &LiveRequest) -> &gadk_genai::Blob {
    match &req.realtime_input {
        Some(RealtimeInput::Blob(b)) => b,
        other => panic!("RealtimeInput = {other:?}, want a blob"),
    }
}

// ---------------------------------------------------------------- the ports

#[tokio::test]
async fn test_run_live_handler_streams_events_over_web_socket() {
    let session = RecordingLiveSession::new();
    let s = session.clone();
    let (mut conn, _server) = dial_run_live_handler(run_live_fn(move || {
        let s = s.clone();
        async move {
            let sess: Arc<dyn LiveSession> = s;
            Ok((
                sess,
                event_stream(|y| async move {
                    y.yield_(Ok(make_event(
                        "invocation-1",
                        TEST_LIVE_APP_NAME,
                        "Hello from live agent",
                    )))
                    .await;
                }),
            ))
        }
    }))
    .await;

    let got = read_event(&mut conn).await;
    assert_eq!(got["invocationId"], "invocation-1", "InvocationID");
    assert_eq!(got["author"], TEST_LIVE_APP_NAME, "Author");
    assert_eq!(
        got["content"]["parts"].as_array().map(Vec::len),
        Some(1),
        "Content = {}",
        got["content"]
    );
    assert_eq!(first_text(&got), Some("Hello from live agent"));

    timeout(Duration::from_secs(1), session.closed.cancelled())
        .await
        .expect("live session was not closed after the event stream ended");
}

#[tokio::test]
async fn test_run_live_handler_forwards_text_messages() {
    let session = RecordingLiveSession::new();
    let (mut conn, _server) = dial_run_live_handler(blocking_live_run(session.clone())).await;

    let msg = json!({"content": {"role": "user", "parts": [{"text": "Hello over WebSocket"}]}});
    conn.send(Message::text(msg.to_string())).await.unwrap();

    let got = wait_for_live_request(&session).await;
    let content = got.content.expect("forwarded text content");
    assert_eq!(content.parts.len(), 1);
    assert_eq!(content.parts[0].text, "Hello over WebSocket");
    assert_eq!(
        got.realtime_input, None,
        "RealtimeInput must be nil for a text message"
    );

    close_live_client(&mut conn, &session).await;
}

#[tokio::test]
async fn test_run_live_handler_forwards_binary_audio() {
    let session = RecordingLiveSession::new();
    let (mut conn, _server) = dial_run_live_handler(blocking_live_run(session.clone())).await;

    let want_audio = vec![0x01, 0x02, 0x03, 0x04];
    conn.send(Message::binary(want_audio.clone()))
        .await
        .unwrap();

    let got = wait_for_live_request(&session).await;
    let b = blob(&got);
    assert_eq!(b.mime_type, "audio/pcm;rate=16000", "MIMEType");
    assert_eq!(b.data.as_ref(), want_audio.as_slice(), "Data");
    assert_eq!(
        got.content, None,
        "Content must be nil for a binary audio frame"
    );

    close_live_client(&mut conn, &session).await;
}

#[tokio::test]
async fn test_run_live_handler_forwards_realtime_input_variants() {
    type Check = fn(&LiveRequest);
    let cases: Vec<(&str, Value, Check)> = vec![
        ("activity start", json!({"activityStart": {}}), |req| {
            assert_eq!(req.realtime_input, Some(RealtimeInput::ActivityStart));
        }),
        ("activity end", json!({"activityEnd": {}}), |req| {
            assert_eq!(req.realtime_input, Some(RealtimeInput::ActivityEnd));
        }),
        (
            "blob",
            // Go marshals the []byte as base64.
            json!({"blob": {"mime_type": "text/plain", "data": "aGVsbG8="}}),
            |req| {
                let b = blob(req);
                assert_eq!(b.mime_type, "text/plain", "MIMEType");
                assert_eq!(b.data.as_ref(), b"hello", "Data");
            },
        ),
    ];

    for (name, message, check) in cases {
        let session = RecordingLiveSession::new();
        let (mut conn, _server) = dial_run_live_handler(blocking_live_run(session.clone())).await;
        conn.send(Message::text(message.to_string())).await.unwrap();
        let req = wait_for_live_request(&session).await;
        let res = std::panic::catch_unwind(|| check(&req));
        assert!(res.is_ok(), "case {name:?} failed");
        close_live_client(&mut conn, &session).await;
    }
}

#[tokio::test]
async fn test_run_live_handler_client_disconnect_stops_live_run() {
    for name in ["normal close frame", "abrupt connection close"] {
        let session = RecordingLiveSession::new();
        let iterator_started = CancellationToken::new();
        let iterator_done = CancellationToken::new();
        let (s, started, done) = (
            session.clone(),
            iterator_started.clone(),
            iterator_done.clone(),
        );
        let (mut conn, mut server) = dial_run_live_handler(run_live_fn(move || {
            let (s, started, done) = (s.clone(), started.clone(), done.clone());
            async move {
                let closed = s.closed.clone();
                let sess: Arc<dyn LiveSession> = s;
                Ok((
                    sess,
                    event_stream(move |_| async move {
                        started.cancel();
                        let _done = done.drop_guard();
                        closed.cancelled().await;
                    }),
                ))
            }
        }))
        .await;

        timeout(Duration::from_secs(1), iterator_started.cancelled())
            .await
            .unwrap_or_else(|_| panic!("{name}: live event iterator did not start"));

        if name == "normal close frame" {
            conn.send(close_frame(1000, "client finished"))
                .await
                .unwrap();
        } else {
            drop(conn);
        }

        timeout(Duration::from_secs(1), session.closed.cancelled())
            .await
            .unwrap_or_else(|_| {
                panic!("{name}: live session was not closed after the client disconnected")
            });
        timeout(Duration::from_secs(1), iterator_done.cancelled())
            .await
            .unwrap_or_else(|_| {
                panic!("{name}: live event iterator did not exit after the session was closed")
            });
        wait_for_handler_exit(&mut server.exits).await;
    }
}

// parity: adapted: a raw frame client stands in for gorilla with its close handler disabled
#[tokio::test]
async fn test_run_live_handler_run_live_error_sends_close_frame_and_drains_reply() {
    let mut server = start_run_live_server(
        |_| {},
        run_live_fn(|| async { Err(AgentError::other("live agent failed")) }),
    )
    .await;
    let mut conn = RawClient::dial(server.addr).await;

    let (opcode, payload) = timeout(TEST_WEB_SOCKET_READ_TIMEOUT, conn.read_frame())
        .await
        .expect("no close frame");
    assert_eq!(opcode, 0x8, "want a close frame");
    assert_eq!(
        u16::from_be_bytes([payload[0], payload[1]]),
        1011,
        "close code"
    );
    assert_eq!(&payload[2..], b"live agent failed", "close reason");

    assert!(
        timeout(TEST_CLOSE_DRAIN_GRACE_PERIOD, server.exits.recv())
            .await
            .is_err(),
        "RunLiveHandler returned before receiving the close reply"
    );
    conn.write_frame(0x8, &close_payload(1011, "client acknowledged"))
        .await;
    // Much shorter than the handler's one-second drain: a handler that sleeps instead of
    // draining the reply fails here.
    timeout(TEST_CLOSE_REPLY_EXIT_TIMEOUT, server.exits.recv())
        .await
        .expect("RunLiveHandler did not exit promptly after the close reply");
}

#[tokio::test]
async fn test_run_live_handler_pongs_do_not_extend_close_drain() {
    let (conn, mut server) = dial_run_live_handler(run_live_fn(|| async {
        Err(AgentError::other("live agent failed"))
    }))
    .await;

    // The client never reads, so the server's close frame goes unanswered, and it keeps
    // sending pongs. The drain has to end on its own one-second deadline all the same.
    let stop = CancellationToken::new();
    let _stop = stop.clone().drop_guard();
    send_pongs(
        Arc::new(tokio::sync::Mutex::new(conn)),
        Duration::from_millis(20),
        stop,
    );

    timeout(Duration::from_secs(3), server.exits.recv())
        .await
        .expect("RunLiveHandler was still draining 3s after its close frame while the client sent pongs");
}

// parity: adapted: Go's consumer-panic probe is dropped (a panicking Rust handler task sends no close frame, which the assertions catch)
#[tokio::test]
async fn test_run_live_handler_iterator_error_sends_close_frame() {
    let session = RecordingLiveSession::new();
    let continued_after_error = CancellationToken::new();
    let (s, continued) = (session.clone(), continued_after_error.clone());
    let (mut conn, mut server) = dial_run_live_handler(run_live_fn(move || {
        let (s, continued) = (s.clone(), continued.clone());
        async move {
            let sess: Arc<dyn LiveSession> = s;
            Ok((
                sess,
                event_stream(move |y| async move {
                    if !y
                        .yield_(Ok(make_event(
                            "invocation-1",
                            TEST_LIVE_APP_NAME,
                            "before failure",
                        )))
                        .await
                    {
                        return;
                    }
                    if !y.yield_(Err(AgentError::other("stream failed"))).await {
                        return;
                    }
                    // Real producers may continue after a per-item error; the handler must
                    // stop consuming before this event.
                    continued.cancel();
                    y.yield_(Ok(make_event(
                        "invocation-2",
                        TEST_LIVE_APP_NAME,
                        "after failure",
                    )))
                    .await;
                }),
            ))
        }
    }))
    .await;

    read_event(&mut conn).await;
    let (code, reason) = read_close_error(&mut conn).await;
    assert_eq!(code, 1011, "close code");
    assert_eq!(reason, "stream failed", "close reason");
    wait_for_handler_exit(&mut server.exits).await;
    assert!(
        !continued_after_error.is_cancelled(),
        "RunLiveHandler continued consuming events after the iterator error"
    );
}

#[tokio::test]
async fn test_run_live_handler_graceful_client_close_does_not_log_write_error() {
    let logs = LogBuffer::default();
    let _guard = logs.capture();

    let session = RecordingLiveSession::new();
    let s = session.clone();
    let (mut conn, mut server) = dial_run_live_handler(run_live_fn(move || {
        let s = s.clone();
        async move {
            let sess: Arc<dyn LiveSession> = s;
            Ok((
                sess,
                event_stream(|y| async move {
                    let event = make_event("invocation-1", TEST_LIVE_APP_NAME, "tick");
                    while y.yield_(Ok(event.clone())).await {
                        // Time for the reader to take the client's close frame before the
                        // write loop can fill the socket buffer.
                        sleep(Duration::from_millis(1)).await;
                    }
                }),
            ))
        }
    }))
    .await;

    conn.send(close_frame(1000, "client finished"))
        .await
        .unwrap();
    wait_for_handler_exit(&mut server.exits).await;
    let got = logs.text();
    assert!(
        !got.contains(&format!(
            "WebSocket write error for app {TEST_LIVE_APP_NAME}"
        )),
        "graceful close produced a write-error log line: {got:?}"
    );
}

// parity: adapted: a gadk models::Event always encodes, so the write fails on a connection the client dropped (Go logs that write error the same way)
#[tokio::test]
async fn test_run_live_handler_logs_write_json_failure() {
    let logs = LogBuffer::default();
    let _guard = logs.capture();

    let session = RecordingLiveSession::new();
    let s = session.clone();
    let (conn, mut server) = dial_run_live_handler(run_live_fn(move || {
        let s = s.clone();
        async move {
            let closed = s.closed.clone();
            let sess: Arc<dyn LiveSession> = s;
            Ok((
                sess,
                event_stream(move |y| async move {
                    // The reader closes the session once the client is gone.
                    closed.cancelled().await;
                    y.yield_(Ok(make_event(
                        "invocation-1",
                        TEST_LIVE_APP_NAME,
                        "invalid output",
                    )))
                    .await;
                }),
            ))
        }
    }))
    .await;
    drop(conn);

    wait_for_handler_exit(&mut server.exits).await;
    let got = logs.text();
    assert!(
        got.contains(&format!(
            "WebSocket write error for app {TEST_LIVE_APP_NAME}"
        )),
        "logs = {got:?}, want WebSocket write failure with app name"
    );
}

// parity: adapted: truncate_close_reason is crate-private; each reason travels as an iterator error and the client checks the close frame it receives
#[tokio::test]
async fn test_truncate_close_reason() {
    const MAX_CLOSE_REASON: usize = 123;
    let long_err = "live session: reconnect budget exhausted: 5 consecutive attempts delivered no content: \
                    failed to receive message: websocket: close 1006 (abnormal closure): unexpected EOF";
    let cases = [
        ("short reason is untouched", "agent not found".to_owned()),
        ("exactly at the limit", "a".repeat(MAX_CLOSE_REASON)),
        ("over the limit", long_err.to_owned()),
        (
            "multi-byte rune straddling the cut",
            "é".repeat(MAX_CLOSE_REASON),
        ),
    ];
    for (name, reason) in cases {
        let r = reason.clone();
        let (mut conn, _server) = dial_run_live_handler(run_live_fn(move || {
            let r = r.clone();
            async move {
                let sess: Arc<dyn LiveSession> = RecordingLiveSession::new();
                Ok((
                    sess,
                    event_stream(move |y| async move {
                        y.yield_(Err(AgentError::other(r))).await;
                    }),
                ))
            }
        }))
        .await;
        let (code, got) = read_close_error(&mut conn).await;
        assert_eq!(code, 1011, "{name}: close code");
        assert!(
            got.len() <= MAX_CLOSE_REASON,
            "{name}: len = {}, want at most {MAX_CLOSE_REASON}",
            got.len()
        );
        assert!(
            reason.starts_with(&got),
            "{name}: result {got:?} is not a prefix of the reason"
        );
        if reason.len() <= MAX_CLOSE_REASON {
            assert_eq!(
                got, reason,
                "{name}: a reason that already fits was altered"
            );
        }
        // The close frame payload (code + reason) fits a control frame.
        assert!(2 + got.len() <= 125, "{name}: close frame payload too long");
    }
}

#[tokio::test]
async fn test_run_live_handler_enforces_message_size_limit() {
    for (name, size, forward) in [
        (
            "at limit is forwarded",
            DEFAULT_MAX_LIVE_MESSAGE_BYTES,
            true,
        ),
        (
            "one byte over is rejected",
            DEFAULT_MAX_LIVE_MESSAGE_BYTES + 1,
            false,
        ),
    ] {
        let session = RecordingLiveSession::new();
        let (conn, mut server) = dial_run_live_handler(blocking_live_run(session.clone())).await;
        let (mut sink, mut stream) = conn.split();
        // The server stops reading at the over-limit frame's header and closes, so the rest of
        // that write may meet a closed socket; only the close frame matters.
        let write = tokio::spawn(async move { sink.send(Message::binary(vec![0u8; size])).await });

        if forward {
            write
                .await
                .unwrap()
                .unwrap_or_else(|e| panic!("{name}: WriteMessage() failed: {e}"));
            let got = wait_for_live_request(&session).await;
            assert_eq!(blob(&got).data.len(), size, "{name}");
            continue;
        }
        let close = timeout(TEST_WEB_SOCKET_READ_TIMEOUT, stream.next())
            .await
            .unwrap_or_else(|_| panic!("{name}: no close frame"));
        let Some(Ok(Message::Close(Some(CloseFrame { code, .. })))) = close else {
            panic!("{name}: ReadMessage() got {close:?}, want a close frame");
        };
        let code = u16::from(code);
        assert_eq!(code, 1009, "{name}: close code");
        assert!(
            session.rx.lock().await.try_recv().is_err(),
            "{name}: oversized message reached the live session"
        );
        wait_for_handler_exit(&mut server.exits).await;
    }
}

#[tokio::test]
async fn test_run_live_handler_closes_connection_on_oversized_message() {
    const READ_LIMIT: i64 = 64;

    let session = RecordingLiveSession::new();
    let mut server = start_run_live_server(
        |cfg| cfg.max_live_message_bytes = READ_LIMIT,
        blocking_live_run(session.clone()),
    )
    .await;
    let mut conn = dial_live(&server.ws_url).await;

    conn.send(Message::binary(vec![0u8; 2 * READ_LIMIT as usize]))
        .await
        .unwrap();

    let (code, _) = read_close_error(&mut conn).await;
    assert_eq!(code, 1009, "close code");
    assert!(
        session.rx.lock().await.try_recv().is_err(),
        "oversized message reached the live session, want it dropped"
    );
    wait_for_handler_exit(&mut server.exits).await;
}

#[tokio::test]
async fn test_run_live_handler_accepts_message_at_read_limit() {
    const READ_LIMIT: i64 = 1024;

    let session = RecordingLiveSession::new();
    let server = start_run_live_server(
        |cfg| cfg.max_live_message_bytes = READ_LIMIT,
        blocking_live_run(session.clone()),
    )
    .await;
    let mut conn = dial_live(&server.ws_url).await;

    conn.send(Message::binary(vec![0u8; READ_LIMIT as usize]))
        .await
        .unwrap();

    let req = wait_for_live_request(&session).await;
    assert_eq!(blob(&req).data.len(), READ_LIMIT as usize, "forwarded blob");
}

#[tokio::test]
async fn test_run_live_handler_closes_unresponsive_connection() {
    const KEEPALIVE: Duration = Duration::from_millis(100);

    let session = RecordingLiveSession::new();
    // The client never reads, so it never answers the server's pings and the keepalive is
    // the only thing that can end the connection.
    let mut server = start_run_live_server(
        |cfg| cfg.live_keepalive_timeout = Timeout::After(KEEPALIVE),
        blocking_live_run(session.clone()),
    )
    .await;
    let _conn = dial_live(&server.ws_url).await;

    timeout(Duration::from_secs(1), session.closed.cancelled())
        .await
        .expect("unresponsive connection was not dropped after the keepalive timeout");
    wait_for_handler_exit(&mut server.exits).await;
}

#[tokio::test]
async fn test_run_live_handler_pong_keeps_connection_open() {
    const KEEPALIVE: Duration = Duration::from_millis(100);

    let session = RecordingLiveSession::new();
    let server = start_run_live_server(
        |cfg| cfg.live_keepalive_timeout = Timeout::After(KEEPALIVE),
        blocking_live_run(session.clone()),
    )
    .await;
    answer_pings(dial_live(&server.ws_url).await);

    assert!(
        timeout(5 * KEEPALIVE, session.closed.cancelled())
            .await
            .is_err(),
        "connection was dropped while the client was answering pings"
    );
}

#[tokio::test]
async fn test_run_live_handler_keeps_responsive_client_through_slow_setup() {
    const KEEPALIVE: Duration = Duration::from_millis(100);

    let session = RecordingLiveSession::new();
    let blocking = blocking_live_run(session.clone());
    let server = start_run_live_server(
        |cfg| cfg.live_keepalive_timeout = Timeout::After(KEEPALIVE),
        run_live_fn(move || {
            let blocking = blocking.clone();
            async move {
                sleep(3 * KEEPALIVE).await;
                blocking().await
            }
        }),
    )
    .await;
    answer_pings(dial_live(&server.ws_url).await);

    assert!(
        timeout(5 * KEEPALIVE, session.closed.cancelled())
            .await
            .is_err(),
        "client answering pings was dropped after a session setup longer than the keepalive timeout"
    );
}

/// Go `slowSendLiveSession`: takes `delay` to accept each request.
struct SlowSendLiveSession {
    recorder: Arc<RecordingLiveSession>,
    delay: Duration,
}

#[async_trait]
impl LiveSession for SlowSendLiveSession {
    async fn send(&self, req: LiveRequest) -> Result<(), AgentError> {
        sleep(self.delay).await;
        self.recorder.send(req).await
    }
    async fn close(&self) -> Result<(), AgentError> {
        self.recorder.close().await
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[tokio::test]
async fn test_run_live_handler_keeps_responsive_client_through_slow_send() {
    const KEEPALIVE: Duration = Duration::from_millis(100);

    let recorder = RecordingLiveSession::new();
    let r = recorder.clone();
    let server = start_run_live_server(
        |cfg| cfg.live_keepalive_timeout = Timeout::After(KEEPALIVE),
        run_live_fn(move || {
            let r = r.clone();
            async move {
                let closed = r.closed.clone();
                let sess: Arc<dyn LiveSession> = Arc::new(SlowSendLiveSession {
                    recorder: r,
                    delay: 3 * KEEPALIVE,
                });
                Ok((
                    sess,
                    event_stream(move |_| async move { closed.cancelled().await }),
                ))
            }
        }),
    )
    .await;
    let mut conn = dial_live(&server.ws_url).await;
    conn.send(Message::binary(b"audio".to_vec())).await.unwrap();
    answer_pings(conn);

    assert!(
        timeout(8 * KEEPALIVE, recorder.closed.cancelled())
            .await
            .is_err(),
        "client answering pings was dropped while the agent took longer than the keepalive timeout to accept its message"
    );
}

#[tokio::test]
async fn test_run_live_handler_iterator_error_after_quiet_spell_sends_close_frame() {
    const KEEPALIVE: Duration = Duration::from_millis(100);

    let session = RecordingLiveSession::new();
    let s = session.clone();
    let mut server = start_run_live_server(
        |cfg| cfg.live_keepalive_timeout = Timeout::After(KEEPALIVE),
        run_live_fn(move || {
            let s = s.clone();
            async move {
                let sess: Arc<dyn LiveSession> = s;
                Ok((
                    sess,
                    event_stream(|y| async move {
                        if !y
                            .yield_(Ok(make_event(
                                "invocation-1",
                                TEST_LIVE_APP_NAME,
                                "before failure",
                            )))
                            .await
                        {
                            return;
                        }
                        // Quiet for longer than the keepalive timeout, so the deadline of the
                        // write above has passed by the time the error arrives.
                        sleep(3 * KEEPALIVE).await;
                        y.yield_(Err(AgentError::other("stream failed"))).await;
                    }),
                ))
            }
        }),
    )
    .await;
    let mut conn = dial_live(&server.ws_url).await;

    read_event(&mut conn).await;
    // Still reading, so the pings sent while the agent is quiet are answered.
    let (code, reason) = read_close_error(&mut conn).await;
    assert_eq!(code, 1011, "close code");
    assert_eq!(reason, "stream failed", "close reason");
    wait_for_handler_exit(&mut server.exits).await;
}

#[tokio::test]
async fn test_run_live_handler_drops_peer_that_stops_reading() {
    const KEEPALIVE: Duration = Duration::from_millis(400);

    let session = RecordingLiveSession::new();
    let stall_ready = CancellationToken::new();
    let (s, ready) = (session.clone(), stall_ready.clone());
    let mut server = start_run_live_server(
        |cfg| {
            cfg.live_keepalive_timeout = Timeout::After(KEEPALIVE);
            cfg.max_live_sessions = 1;
        },
        run_live_fn(move || {
            let (s, ready) = (s.clone(), ready.clone());
            async move {
                let sess: Arc<dyn LiveSession> = s;
                Ok((
                    sess,
                    event_stream(move |y| async move {
                        ready.cancelled().await;
                        y.yield_(Ok(make_event("inv", TEST_LIVE_APP_NAME, "tick")))
                            .await;
                    }),
                ))
            }
        }),
    )
    .await;
    let conn = dial_live(&server.ws_url).await;
    let server_conn = server.stalls.recv().await.unwrap();

    // The client keeps sending pongs so the server's read deadline never expires; only the
    // per-event write deadline can end the stalled write after one keepalive timeout.
    let (sink, _stream) = conn.split();
    let stop = CancellationToken::new();
    let _stop = stop.clone().drop_guard();
    send_pongs(Arc::new(tokio::sync::Mutex::new(sink)), KEEPALIVE / 4, stop);

    server_conn.stall();
    let start = Instant::now();
    stall_ready.cancel();

    let max_exit = KEEPALIVE * 13 / 10;
    tokio::time::timeout_at(start + max_exit, server.exits.recv())
        .await
        .unwrap_or_else(|_| {
            panic!(
                "RunLiveHandler still running {:?} after event write stalled, want exit before {max_exit:?}",
                start.elapsed()
            )
        });
    server_conn.unstall();
    // With a cap of one, this handshake succeeds only if the stalled peer's slot was released.
    dial_live(&server.ws_url).await;
    assert!(
        start.elapsed() < max_exit,
        "second session admitted after {:?}, want before {max_exit:?}",
        start.elapsed()
    );
}

#[tokio::test]
async fn test_run_live_handler_ping_write_deadline_is_keepalive_timeout() {
    const KEEPALIVE: Duration = Duration::from_millis(200);

    let session = RecordingLiveSession::new();
    let s = session.clone();
    // Echoes each binary message back as an event, so the test can tell whether the
    // connection still carries writes.
    let mut server = start_run_live_server(
        |cfg| cfg.live_keepalive_timeout = Timeout::After(KEEPALIVE),
        run_live_fn(move || {
            let s = s.clone();
            async move {
                let sess: Arc<dyn LiveSession> = s.clone();
                Ok((
                    sess,
                    event_stream(move |y| async move {
                        let mut rx = s.rx.lock().await;
                        loop {
                            tokio::select! {
                                req = rx.recv() => {
                                    let Some(req) = req else { return };
                                    if let Some(RealtimeInput::Blob(b)) = &req.realtime_input {
                                        let text = String::from_utf8_lossy(&b.data).into_owned();
                                        if !y.yield_(Ok(make_event("inv", TEST_LIVE_APP_NAME, &text))).await {
                                            return;
                                        }
                                    }
                                }
                                () = s.closed.cancelled() => return,
                            }
                        }
                    }),
                ))
            }
        }),
    )
    .await;
    let conn = dial_live(&server.ws_url).await;
    let server_conn = server.stalls.recv().await.unwrap();

    let (sink, mut stream) = conn.split();
    let sink = Arc::new(tokio::sync::Mutex::new(sink));
    let (echo_tx, mut echoes) = mpsc::unbounded_channel::<String>();
    let read_err = CancellationToken::new();
    let read_err_tx = read_err.clone();
    tokio::spawn(async move {
        loop {
            match stream.next().await {
                Some(Ok(Message::Text(t))) => {
                    let event: Value = serde_json::from_str(t.as_str()).unwrap();
                    if let Some(text) = first_text(&event) {
                        let _ = echo_tx.send(text.to_owned());
                    }
                }
                Some(Ok(_)) => {}
                _ => {
                    read_err_tx.cancel();
                    return;
                }
            }
        }
    });
    // The client's own pongs keep the server's read deadline from expiring while its pings
    // are stalled, so only a failed write can end the connection.
    let stop = CancellationToken::new();
    let _stop = stop.clone().drop_guard();
    send_pongs(sink.clone(), KEEPALIVE / 4, stop);

    // The agent is quiet, so the first write to meet the stall is a ping.
    let stall_ping = |d: Duration| {
        let server_conn = server_conn.clone();
        async move {
            server_conn.stall();
            if timeout(Duration::from_secs(1), server_conn.blocked.notified())
                .await
                .is_err()
            {
                server_conn.unstall();
                panic!("no ping reached the stalled connection");
            }
            sleep(d).await;
            server_conn.unstall();
        }
    };

    stall_ping(KEEPALIVE / 2).await;
    sink.lock()
        .await
        .send(Message::binary(b"after short stall".to_vec()))
        .await
        .unwrap();
    tokio::select! {
        got = echoes.recv() => assert_eq!(got.as_deref(), Some("after short stall"), "echo"),
        () = read_err.cancelled() => panic!("connection ended after a ping stall shorter than the keepalive timeout"),
        () = sleep(Duration::from_secs(1)) => panic!("echo never arrived after a ping stall shorter than the keepalive timeout"),
    }

    stall_ping(2 * KEEPALIVE).await;
    timeout(Duration::from_secs(1), read_err.cancelled())
        .await
        .expect("connection did not close after a ping stalled for twice the keepalive timeout");
}

#[tokio::test]
async fn test_run_live_handler_failed_ping_closes_connection() {
    const KEEPALIVE: Duration = Duration::from_millis(200);

    let session = RecordingLiveSession::new();
    let mut server = start_run_live_server(
        |cfg| cfg.live_keepalive_timeout = Timeout::After(KEEPALIVE),
        blocking_live_run(session.clone()),
    )
    .await;
    let conn = dial_live(&server.ws_url).await;
    let server_conn = server.stalls.recv().await.unwrap();

    let done = CancellationToken::new();
    let _done = done.clone().drop_guard();
    // The agent stays quiet and takes whatever the client sends.
    let (s, d) = (session.clone(), done.clone());
    tokio::spawn(async move {
        let mut rx = s.rx.lock().await;
        loop {
            tokio::select! {
                _ = rx.recv() => {}
                () = d.cancelled() => return,
            }
        }
    });
    // From here the peer accepts no writes, starting with the next ping, but keeps streaming
    // audio.
    server_conn.stall();
    let (mut sink, _stream) = conn.split();
    let d = done.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(KEEPALIVE / 4);
        loop {
            tokio::select! {
                () = d.cancelled() => return,
                _ = ticker.tick() => {}
            }
            if sink.send(Message::binary(b"pcm".to_vec())).await.is_err() {
                return;
            }
        }
    });

    let exited = timeout(15 * KEEPALIVE, server.exits.recv()).await;
    server_conn.unstall();
    assert!(
        exited.is_ok(),
        "handler still running {:?} after the peer stopped accepting writes",
        15 * KEEPALIVE
    );
}

/// Go `blockingSendLiveSession`: `send` blocks until the session closes.
#[derive(Default)]
struct BlockingSendLiveSession {
    entered: CancellationToken,
    closed: CancellationToken,
}

#[async_trait]
impl LiveSession for BlockingSendLiveSession {
    async fn send(&self, _req: LiveRequest) -> Result<(), AgentError> {
        self.entered.cancel();
        self.closed.cancelled().await;
        Err(AgentError::other("EOF"))
    }
    async fn close(&self) -> Result<(), AgentError> {
        self.closed.cancel();
        Ok(())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[tokio::test]
async fn test_run_live_handler_failed_ping_ends_handler_while_send_blocks() {
    const KEEPALIVE: Duration = Duration::from_millis(200);

    let sess = Arc::new(BlockingSendLiveSession::default());
    let s = sess.clone();
    let mut server = start_run_live_server(
        |cfg| cfg.live_keepalive_timeout = Timeout::After(KEEPALIVE),
        run_live_fn(move || {
            let s = s.clone();
            async move {
                // A quiet agent: no events until the session closes.
                let closed = s.closed.clone();
                let sess: Arc<dyn LiveSession> = s;
                Ok((
                    sess,
                    event_stream(move |_| async move { closed.cancelled().await }),
                ))
            }
        }),
    )
    .await;
    let mut conn = dial_live(&server.ws_url).await;
    let server_conn = server.stalls.recv().await.unwrap();
    conn.send(Message::binary(b"pcm".to_vec())).await.unwrap();
    // The reader is now inside send, where it neither reads nor has a read deadline armed,
    // and the agent emits nothing, so only the failed ping below can end the handler.
    timeout(Duration::from_secs(1), sess.entered.cancelled())
        .await
        .expect("client message never reached the live session's send");
    server_conn.stall();

    let exited = timeout(5 * KEEPALIVE, server.exits.recv()).await;
    server_conn.unstall();
    sess.closed.cancel();
    assert!(
        exited.is_ok(),
        "handler still running {:?} after the peer stopped accepting writes",
        5 * KEEPALIVE
    );
}

#[tokio::test]
async fn test_run_live_handler_read_deadline_closes_in_flight_write() {
    const KEEPALIVE: Duration = Duration::from_millis(400);
    const SOCK_BUF_BYTES: u32 = 4096;
    const EVENT_TEXT_BYTES: usize = 16 << 10;

    let session = RecordingLiveSession::new();
    let s = session.clone();
    let mut server = start_run_live_server_with(
        |cfg| {
            cfg.live_keepalive_timeout = Timeout::After(KEEPALIVE);
            cfg.max_live_sessions = 1;
        },
        run_live_fn(move || {
            let s = s.clone();
            async move {
                let closed = s.closed.clone();
                let sess: Arc<dyn LiveSession> = s;
                Ok((
                    sess,
                    event_stream(move |y| async move {
                        // Start writing just before the read deadline expires, so an in-flight
                        // write has a fresh write deadline near 1.9x while the read deadline
                        // expires at 1.0x.
                        sleep(KEEPALIVE * 9 / 10).await;
                        let event =
                            make_event("inv", TEST_LIVE_APP_NAME, &"a".repeat(EVENT_TEXT_BYTES));
                        while !closed.is_cancelled() {
                            if !y.yield_(Ok(event.clone())).await {
                                return;
                            }
                        }
                    }),
                ))
            }
        }),
        Some(SOCK_BUF_BYTES),
    )
    .await;

    let start = Instant::now();
    let socket = TcpSocket::new_v4().unwrap();
    socket.set_recv_buffer_size(SOCK_BUF_BYTES).unwrap();
    let tcp = socket.connect(server.addr).await.unwrap();
    let (_conn, resp) = tokio_tungstenite::client_async(server.ws_url.as_str(), tcp)
        .await
        .expect("dial");
    assert_eq!(resp.status(), 101, "handshake status");

    let max_exit = KEEPALIVE * 12 / 10;
    tokio::time::timeout_at(start + max_exit, server.exits.recv())
        .await
        .unwrap_or_else(|_| {
            panic!(
                "RunLiveHandler still running {:?} after connect, want exit before {max_exit:?}",
                start.elapsed()
            )
        });
    dial_live(&server.ws_url).await;
    assert!(
        start.elapsed() < max_exit,
        "second session admitted after {:?}, want before {max_exit:?}",
        start.elapsed()
    );
}

/// Go `failingFlowLiveSession`: a pending `send` is released with EOF when the session closes.
#[derive(Default)]
struct FailingFlowLiveSession {
    send_pending: CancellationToken,
    done: CancellationToken,
}

#[async_trait]
impl LiveSession for FailingFlowLiveSession {
    async fn send(&self, _req: LiveRequest) -> Result<(), AgentError> {
        self.send_pending.cancel();
        self.done.cancelled().await;
        Err(AgentError::other("EOF"))
    }
    async fn close(&self) -> Result<(), AgentError> {
        self.done.cancel();
        Ok(())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[tokio::test]
async fn test_run_live_handler_flow_error_while_client_sends_delivers_close_frame() {
    const WANT_ERR: &str = "failed to connect live session: websocket: bad handshake";
    let sess = Arc::new(FailingFlowLiveSession::default());
    let s = sess.clone();
    let (mut conn, mut server) = dial_run_live_handler(run_live_fn(move || {
        let s = s.clone();
        async move {
            let flow = s.clone();
            let sess: Arc<dyn LiveSession> = s;
            Ok((
                sess,
                event_stream(move |y| async move {
                    flow.send_pending.cancelled().await;
                    flow.done.cancel();
                    sleep(Duration::from_millis(5)).await;
                    y.yield_(Err(AgentError::other(WANT_ERR))).await;
                }),
            ))
        }
    }))
    .await;

    conn.send(Message::binary(b"pcm".to_vec())).await.unwrap();
    let (code, reason) = read_close_error(&mut conn).await;
    assert_eq!(code, 1011, "close code");
    assert_eq!(reason, WANT_ERR, "close reason");
    wait_for_handler_exit(&mut server.exits).await;
}

// parity: adapted: gadk sets no socket deadlines (timeouts wrap each read and write), so a disabled keepalive is observed as no pings and no dropped session instead of recorded zero deadlines
#[tokio::test]
async fn test_run_live_handler_negative_keepalive_sets_no_deadlines() {
    let session = RecordingLiveSession::new();
    let s = session.clone();
    let server = start_run_live_server(
        |cfg| cfg.live_keepalive_timeout = Timeout::Disabled,
        run_live_fn(move || {
            let s = s.clone();
            async move {
                let closed = s.closed.clone();
                let sess: Arc<dyn LiveSession> = s;
                Ok((
                    sess,
                    event_stream(move |y| async move {
                        if !y
                            .yield_(Ok(make_event("inv", TEST_LIVE_APP_NAME, "hello")))
                            .await
                        {
                            return;
                        }
                        closed.cancelled().await;
                    }),
                ))
            }
        }),
    )
    .await;
    let mut conn = dial_live(&server.ws_url).await;

    let got = read_event(&mut conn).await;
    assert_eq!(first_text(&got), Some("hello"));

    assert!(
        timeout(Duration::from_millis(50), session.closed.cancelled())
            .await
            .is_err(),
        "live session closed with the keepalive disabled"
    );

    conn.send(Message::binary(b"audio".to_vec())).await.unwrap();
    let req = wait_for_live_request(&session).await;
    assert_eq!(blob(&req).data.as_ref(), b"audio");

    // No keepalive: the server sends no pings.
    let next = timeout(Duration::from_millis(200), conn.next()).await;
    assert!(
        next.is_err(),
        "unexpected frame {next:?} with the keepalive disabled"
    );
}

#[tokio::test]
async fn test_run_live_handler_refuses_upgrade_past_session_limit() {
    let session = RecordingLiveSession::new();
    let mut server = start_run_live_server(
        |cfg| cfg.max_live_sessions = 1,
        blocking_live_run(session.clone()),
    )
    .await;
    let first = dial_live(&server.ws_url).await;

    let err = tokio_tungstenite::connect_async(server.ws_url.as_str())
        .await
        .expect_err("second dial must fail");
    let tokio_tungstenite::tungstenite::Error::Http(resp) = err else {
        panic!("second Dial() error = {err}, want a bad handshake");
    };
    assert_eq!(resp.status(), 503, "second handshake status");
    assert_eq!(
        resp.body().as_deref(),
        Some(&b"live session limit of 1 reached\n"[..]),
        "second handshake body"
    );
    // The refused request's connection ends too, so take its exit before waiting on the one
    // that releases the slot.
    wait_for_handler_exit(&mut server.exits).await;

    // The slot goes back when the first session ends, so the cap bounds concurrency rather
    // than the number of sessions the server ever serves.
    drop(first);
    wait_for_handler_exit(&mut server.exits).await;
    dial_live(&server.ws_url).await;
}

/// Opens `n` live sessions at once and keeps them open.
async fn dial_concurrently(ws_url: &str, n: usize) -> Vec<Client> {
    let mut clients = Vec::new();
    for _ in 0..n {
        clients.push(dial_live(ws_url).await);
    }
    clients
}

// parity: adapted: the resolved limits are crate-private (runtime.rs unit tests check them); each case checks their effect on live connections instead
#[tokio::test]
async fn test_new_runtime_api_controller_with_config_live_limits() {
    // "unset takes the defaults, with no session cap"
    {
        let session = RecordingLiveSession::new();
        let server = start_run_live_server(|_| {}, blocking_live_run(session)).await;
        assert_eq!(dial_concurrently(&server.ws_url, 3).await.len(), 3);
    }
    // "explicit values are kept"
    {
        let (run, mut requests) = per_connection_live_run();
        let server = start_run_live_server(
            |cfg| {
                cfg.max_live_message_bytes = 4096;
                cfg.live_keepalive_timeout = Timeout::After(Duration::from_secs(60));
                cfg.max_live_sessions = 3;
            },
            run,
        )
        .await;
        let mut clients = dial_concurrently(&server.ws_url, 3).await;
        clients[0]
            .send(Message::binary(vec![0u8; 4096]))
            .await
            .unwrap();
        assert_eq!(blob(&next_request(&mut requests).await).data.len(), 4096);
        clients[1]
            .send(Message::binary(vec![0u8; 4097]))
            .await
            .unwrap();
        assert_eq!(read_close_error(&mut clients[1]).await.0, 1009);
        // The over-limit client's session is gone; a fourth would exceed the cap while the
        // other two and a replacement hold it.
        let _replacement = timeout(Duration::from_secs(1), async {
            loop {
                if let Ok((c, _)) = tokio_tungstenite::connect_async(server.ws_url.as_str()).await {
                    return c;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("slot of the closed session was not released");
        let err = tokio_tungstenite::connect_async(server.ws_url.as_str())
            .await
            .expect_err("a fourth session must be refused");
        let tokio_tungstenite::tungstenite::Error::Http(resp) = err else {
            panic!("fourth dial error = {err}");
        };
        assert_eq!(
            resp.body().as_deref(),
            Some(&b"live session limit of 3 reached\n"[..])
        );
    }
    // "negative turns each limit off"
    {
        let (run, mut requests) = per_connection_live_run();
        let server = start_run_live_server(
            |cfg| {
                cfg.max_live_message_bytes = -1;
                cfg.live_keepalive_timeout = Timeout::Disabled;
                cfg.max_live_sessions = -1;
            },
            run,
        )
        .await;
        let mut clients = dial_concurrently(&server.ws_url, 3).await;
        clients[0]
            .send(Message::binary(vec![
                0u8;
                DEFAULT_MAX_LIVE_MESSAGE_BYTES + 1
            ]))
            .await
            .unwrap();
        assert_eq!(
            blob(&next_request(&mut requests).await).data.len(),
            DEFAULT_MAX_LIVE_MESSAGE_BYTES + 1
        );
    }
    // "a one-nanosecond keepalive keeps a non-zero ping interval"
    {
        let session = RecordingLiveSession::new();
        let mut server = start_run_live_server(
            |cfg| cfg.live_keepalive_timeout = Timeout::After(Duration::from_nanos(1)),
            blocking_live_run(session.clone()),
        )
        .await;
        let _conn = dial_live(&server.ws_url).await;
        timeout(Duration::from_secs(1), session.closed.cancelled())
            .await
            .expect("a one-nanosecond keepalive must drop the silent peer");
        wait_for_handler_exit(&mut server.exits).await;
    }
}

// parity: adapted: acquire_live_slot is crate-private; three concurrent sessions are admitted with the cap disabled
#[tokio::test]
async fn test_acquire_live_slot_uncapped_always_grants() {
    let session = RecordingLiveSession::new();
    let server = start_run_live_server(|_| {}, blocking_live_run(session)).await;
    assert_eq!(dial_concurrently(&server.ws_url, 3).await.len(), 3);
}
