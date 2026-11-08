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

//! Shared helpers for the anthropicmodel parity tests (Task 6): fake HTTP server, request
//! recorder, tracing capture, SSE builders, sample PNG, model builders.
//!
//! Go equivalents:
//! - `httptest.NewServer(handler)` → [`FakeServer::start`] / [`FakeServer::start_async`]
//!   (a real HTTP/1 server on `127.0.0.1`, so the model's own `reqwest` client is exercised);
//! - `roundTripFunc` behind `http.Client` → [`ScriptedClient`] (an in-process
//!   [`HttpClient`], set as `ClientConfig::http_client`);
//! - `slogCaptureHandler` → [`LogCapture`] (a `tracing` layer; events render as
//!   `msg key=value …`, plan D10);
//! - `staticTokenSource` → [`static_token_credentials`];
//! - `t.TempDir()` + `adc.json` + `t.Setenv("GOOGLE_APPLICATION_CREDENTIALS", …)` →
//!   [`TempFile`], [`adc_authorized_user_file`], [`adc_env`] (ruling R-b: never the real ADC);
//! - `samplePNG1x1` → [`SAMPLE_PNG_1X1`];
//! - `scrubAnthropicRequest` / `scrubAnthropicResponse` / `openHTTPRR` →
//!   [`scrub_anthropic_request`] / [`scrub_anthropic_response`] / [`open_httprr`];
//! - `for resp, err := range m.GenerateContent(...)` → [`generate`], [`generate_last`],
//!   [`generate_err`].
//!
//! Every server and capture is per test: nothing global is installed. The fake server needs a
//! running tokio runtime (`#[tokio::test]`) and real time (do not combine it with
//! `start_paused`).

#![allow(dead_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::VecDeque;
use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use gadk_anthropic::{AnthropicError, AnthropicModel, Backend, ClientConfig, MapEnv};
use gadk_httprr::{HttprrError, RecordReplay};
use gadk_model::{BodyStream, HttpClient, Llm, LlmRequest, LlmResponse, ModelError, ReqwestClient};
use google_cloud_auth::credentials::{
    CacheableResource, Credentials, CredentialsProvider, EntityTag,
};
use google_cloud_auth::errors::CredentialsError;
use hyper::body::{Body, Frame, Incoming, SizeHint};
use tokio::sync::Notify;
use tracing::field::{Field, Visit};
use tracing::{Dispatch, Event, Level, Subscriber};
use tracing_subscriber::layer::{Context as LayerContext, Layer, SubscriberExt};

/// How long [`FakeServer::wait_for_hits`] and friends wait before failing the test.
pub const WAIT_TIMEOUT: Duration = Duration::from_secs(10);

/// The model name every Go test uses.
pub const MODEL: &str = "claude-sonnet-4-5-20250929";

// ------------------------------------------------------------------------------------ requests

/// One request as the fake server (or [`ScriptedClient`]) received it.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    /// HTTP method, upper case (`POST`).
    pub method: String,
    /// Absolute URL: `http://127.0.0.1:<port>/v1/messages?…` for [`FakeServer`], the request
    /// URI as sent for [`ScriptedClient`].
    pub url: String,
    /// URL path (Go `r.URL.Path`), e.g. `/v1/messages`.
    pub path: String,
    /// Raw query without `?` (Go `r.URL.RawQuery`); `None` when the URL has none.
    pub query: Option<String>,
    /// Request headers as received.
    pub headers: http::HeaderMap,
    /// The whole request body.
    pub body: Bytes,
}

impl RecordedRequest {
    /// `path` plus `?query` when there is one.
    pub fn path_and_query(&self) -> String {
        match &self.query {
            Some(q) => format!("{}?{q}", self.path),
            None => self.path.clone(),
        }
    }

    /// Go `r.Header.Get(name)`: the first value, or `""` when absent (case-insensitive name).
    pub fn header(&self, name: &str) -> String {
        self.headers
            .get(name)
            .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
            .unwrap_or_default()
    }

    /// Every value of header `name`, in order.
    pub fn header_values(&self, name: &str) -> Vec<String> {
        self.headers
            .get_all(name)
            .iter()
            .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
            .collect()
    }

    /// The body decoded as JSON; `Value::Null` when it is not JSON (Go's handlers ignore the
    /// `json.Unmarshal` error and keep a nil map).
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or(serde_json::Value::Null)
    }

    /// Go `lastBody["stream"] == true`: the body is a JSON object whose `stream` is `true`.
    pub fn is_stream(&self) -> bool {
        self.json().get("stream") == Some(&serde_json::Value::Bool(true))
    }

    /// The body as text (lossy UTF-8).
    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn from_parts(url: String, parts: &http::request::Parts, body: Bytes) -> Self {
        Self {
            method: parts.method.as_str().to_string(),
            url,
            path: parts.uri.path().to_string(),
            query: parts.uri.query().map(str::to_string),
            headers: parts.headers.clone(),
            body,
        }
    }
}

// ----------------------------------------------------------------------------------- responses

/// One step of a scripted response body.
#[derive(Debug, Clone)]
pub enum BodyStep {
    /// Send these bytes as one chunk (Go `io.WriteString` + `Flush`).
    Chunk(Bytes),
    /// Wait this long before the next step.
    Delay(Duration),
}

/// What happens after the last [`BodyStep`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyEnd {
    /// The body ends normally.
    Finish,
    /// The body never ends: the connection stays open until the client drops it (Go's handler
    /// blocking on `<-blockCh` after flushing).
    Hang,
}

/// What the server does with a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Answer with `status`, `headers` and the body steps.
    Respond,
    /// Never answer: the handler blocks until the client goes away (Go `<-release`). Counted
    /// as a drop when the client gives up.
    Never,
    /// No response: [`FakeServer`] closes the connection, [`ScriptedClient`] returns
    /// `ModelError::Transport(<message>)`.
    Reset(String),
}

/// A scripted response.
#[derive(Debug, Clone)]
pub struct FakeResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response headers, in order.
    pub headers: Vec<(String, String)>,
    /// Delay before the status line and headers are sent.
    pub header_delay: Option<Duration>,
    /// Body chunks and delays, in order.
    pub steps: Vec<BodyStep>,
    /// What happens after the last step.
    pub end: BodyEnd,
    /// Respond, never respond, or reset.
    pub outcome: Outcome,
}

impl FakeResponse {
    /// `status` with no headers and an empty body.
    pub fn new(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            header_delay: None,
            steps: Vec::new(),
            end: BodyEnd::Finish,
            outcome: Outcome::Respond,
        }
    }

    /// 200 with `Content-Type: application/json` and `body` (Go's most common handler).
    pub fn json(body: impl Into<String>) -> Self {
        Self::json_status(200, body)
    }

    /// `status` with `Content-Type: application/json` and `body`.
    pub fn json_status(status: u16, body: impl Into<String>) -> Self {
        Self::new(status)
            .header("Content-Type", "application/json")
            .chunk(body.into())
    }

    /// 200 with `Content-Type: text/event-stream` and the whole SSE `body` as one chunk.
    pub fn sse(body: impl Into<String>) -> Self {
        Self::new(200)
            .header("Content-Type", "text/event-stream")
            .chunk(body.into())
    }

    /// 200 `text/event-stream`, one chunk per item (Go's write-and-`Flush` loop).
    pub fn sse_chunks<I, S>(events: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut resp = Self::new(200).header("Content-Type", "text/event-stream");
        for ev in events {
            resp = resp.chunk(ev.into());
        }
        resp
    }

    /// A request that is never answered (see [`Outcome::Never`]).
    pub fn never() -> Self {
        Self {
            outcome: Outcome::Never,
            ..Self::new(200)
        }
    }

    /// A request answered by closing the connection (see [`Outcome::Reset`]).
    pub fn reset(message: impl Into<String>) -> Self {
        Self {
            outcome: Outcome::Reset(message.into()),
            ..Self::new(200)
        }
    }

    /// Replaces the status code.
    pub fn status(mut self, status: u16) -> Self {
        self.status = status;
        self
    }

    /// Appends a response header (Go `w.Header().Set`; call once per name).
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Appends a body chunk.
    pub fn chunk(mut self, data: impl Into<Bytes>) -> Self {
        self.steps.push(BodyStep::Chunk(data.into()));
        self
    }

    /// Appends a pause between body chunks.
    pub fn delay(mut self, d: Duration) -> Self {
        self.steps.push(BodyStep::Delay(d));
        self
    }

    /// Delays the status line and headers by `d`.
    pub fn delay_headers(mut self, d: Duration) -> Self {
        self.header_delay = Some(d);
        self
    }

    /// Keeps the body open after the last step (see [`BodyEnd::Hang`]).
    pub fn hang(mut self) -> Self {
        self.end = BodyEnd::Hang;
        self
    }

    /// The concatenated body chunks.
    pub fn body_bytes(&self) -> Bytes {
        let mut out = Vec::new();
        for step in &self.steps {
            if let BodyStep::Chunk(b) = step {
                out.extend_from_slice(b);
            }
        }
        Bytes::from(out)
    }
}

/// An async request handler: gets each recorded request, returns the response to send.
pub type Handler = Arc<dyn Fn(RecordedRequest) -> BoxFuture<'static, FakeResponse> + Send + Sync>;

fn sync_handler<F>(f: F) -> Handler
where
    F: Fn(&RecordedRequest) -> FakeResponse + Send + Sync + 'static,
{
    Arc::new(move |req: RecordedRequest| {
        let resp = f(&req);
        Box::pin(async move { resp }) as BoxFuture<'static, FakeResponse>
    })
}

fn async_handler<F, Fut>(f: F) -> Handler
where
    F: Fn(RecordedRequest) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = FakeResponse> + Send + 'static,
{
    Arc::new(move |req: RecordedRequest| Box::pin(f(req)) as BoxFuture<'static, FakeResponse>)
}

fn sequence_handler(responses: Vec<FakeResponse>) -> Handler {
    assert!(
        !responses.is_empty(),
        "sequence needs at least one response"
    );
    let queue = Mutex::new(VecDeque::from(responses));
    sync_handler(move |_| {
        let mut q = queue.lock().unwrap();
        if q.len() > 1 {
            q.pop_front().unwrap()
        } else {
            q.front().cloned().unwrap()
        }
    })
}

/// Shared by [`FakeServer`] and [`ScriptedClient`]: the handler, the request log and counters.
struct Shared {
    handler: Handler,
    requests: Mutex<Vec<RecordedRequest>>,
    hits: AtomicUsize,
    drops: AtomicUsize,
    changed: Notify,
}

impl Shared {
    fn new(handler: Handler) -> Arc<Self> {
        Arc::new(Self {
            handler,
            requests: Mutex::new(Vec::new()),
            hits: AtomicUsize::new(0),
            drops: AtomicUsize::new(0),
            changed: Notify::new(),
        })
    }

    fn record(&self, req: RecordedRequest) {
        self.requests.lock().unwrap().push(req);
        self.hits.fetch_add(1, Ordering::SeqCst);
        self.changed.notify_waiters();
    }

    fn dropped(&self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
        self.changed.notify_waiters();
    }

    async fn wait_until(&self, what: &str, cond: impl Fn(&Self) -> bool) {
        let wait = async {
            loop {
                let notified = self.changed.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if cond(self) {
                    return;
                }
                notified.await;
            }
        };
        if tokio::time::timeout(WAIT_TIMEOUT, wait).await.is_err() {
            panic!(
                "timed out after {WAIT_TIMEOUT:?} waiting for {what} (hits = {}, drops = {})",
                self.hits.load(Ordering::SeqCst),
                self.drops.load(Ordering::SeqCst)
            );
        }
    }

    /// Runs the handler, applying `Never` and the header delay. `None` = reset with message.
    async fn respond(self: &Arc<Self>, req: RecordedRequest) -> Result<FakeResponse, String> {
        let guard = DropGuard::new(self.clone());
        let resp = (self.handler)(req).await;
        match &resp.outcome {
            Outcome::Respond => {}
            Outcome::Never => std::future::pending::<()>().await,
            Outcome::Reset(msg) => {
                guard.disarm();
                return Err(msg.clone());
            }
        }
        if let Some(d) = resp.header_delay {
            tokio::time::sleep(d).await;
        }
        guard.disarm();
        Ok(resp)
    }
}

/// Counts a drop when the request future is dropped before the response head is ready.
struct DropGuard {
    shared: Option<Arc<Shared>>,
}

impl DropGuard {
    fn new(shared: Arc<Shared>) -> Self {
        Self {
            shared: Some(shared),
        }
    }

    fn disarm(mut self) {
        self.shared = None;
    }
}

impl Drop for DropGuard {
    fn drop(&mut self) {
        if let Some(shared) = self.shared.take() {
            shared.dropped();
        }
    }
}

/// A response body that plays [`BodyStep`]s; counts a drop if dropped before it finished.
///
/// The body counts as finished as soon as its last chunk is handed out (or at once when it
/// has no steps) with [`BodyEnd::Finish`]: hyper drops a fixed-length body after writing the
/// declared bytes without polling it to `None`, so waiting for `None` would count every fully
/// sent `Content-Length` response as a drop.
struct ScriptedBody {
    steps: VecDeque<BodyStep>,
    end: BodyEnd,
    sleep: Option<Pin<Box<tokio::time::Sleep>>>,
    shared: Arc<Shared>,
    finished: bool,
    exact_len: Option<u64>,
}

impl ScriptedBody {
    fn new(resp: &FakeResponse, shared: Arc<Shared>) -> Self {
        let only_chunks = resp.end == BodyEnd::Finish
            && resp.steps.iter().all(|s| matches!(s, BodyStep::Chunk(_)));
        Self {
            steps: resp.steps.iter().cloned().collect(),
            end: resp.end,
            sleep: None,
            shared,
            finished: resp.end == BodyEnd::Finish && resp.steps.is_empty(),
            exact_len: only_chunks.then(|| resp.body_bytes().len() as u64),
        }
    }

    fn poll_chunk(&mut self, cx: &mut Context<'_>) -> Poll<Option<Bytes>> {
        loop {
            if let Some(sleep) = self.sleep.as_mut() {
                if sleep.as_mut().poll(cx).is_pending() {
                    return Poll::Pending;
                }
                self.sleep = None;
            }
            match self.steps.pop_front() {
                Some(BodyStep::Chunk(b)) => {
                    if self.steps.is_empty() && self.end == BodyEnd::Finish {
                        self.finished = true;
                    }
                    return Poll::Ready(Some(b));
                }
                Some(BodyStep::Delay(d)) => self.sleep = Some(Box::pin(tokio::time::sleep(d))),
                None => {
                    return match self.end {
                        BodyEnd::Finish => {
                            self.finished = true;
                            Poll::Ready(None)
                        }
                        BodyEnd::Hang => Poll::Pending,
                    };
                }
            }
        }
    }
}

impl Drop for ScriptedBody {
    fn drop(&mut self) {
        if !self.finished {
            self.shared.dropped();
        }
    }
}

impl Body for ScriptedBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        self.get_mut()
            .poll_chunk(cx)
            .map(|chunk| chunk.map(|b| Ok(Frame::data(b))))
    }

    fn is_end_stream(&self) -> bool {
        self.finished
    }

    fn size_hint(&self) -> SizeHint {
        match self.exact_len {
            Some(n) => SizeHint::with_exact(n),
            None => SizeHint::default(),
        }
    }
}

// --------------------------------------------------------------------------------- fake server

/// A local HTTP/1 server on `127.0.0.1` (Go `httptest.NewServer`). Every request is recorded,
/// counted and answered by the handler. Dropping the server stops it and closes its
/// connections (Go `srv.Close()`).
pub struct FakeServer {
    addr: SocketAddr,
    shared: Arc<Shared>,
    tasks: Arc<Mutex<Vec<tokio::task::AbortHandle>>>,
}

impl FakeServer {
    /// Starts a server answering each request with `handler(&request)`. Needs a tokio runtime.
    pub fn start<F>(handler: F) -> Self
    where
        F: Fn(&RecordedRequest) -> FakeResponse + Send + Sync + 'static,
    {
        Self::with_handler(sync_handler(handler))
    }

    /// Starts a server with an async handler (for handlers that wait on a signal first).
    pub fn start_async<F, Fut>(handler: F) -> Self
    where
        F: Fn(RecordedRequest) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = FakeResponse> + Send + 'static,
    {
        Self::with_handler(async_handler(handler))
    }

    /// Starts a server answering every request with a clone of `resp`.
    pub fn always(resp: FakeResponse) -> Self {
        Self::start(move |_| resp.clone())
    }

    /// Starts a server answering every request with `200 application/json` `body`.
    pub fn json(body: impl Into<String>) -> Self {
        Self::always(FakeResponse::json(body))
    }

    /// Starts a server answering request `i` with `responses[i]`; the last one repeats.
    pub fn sequence(responses: Vec<FakeResponse>) -> Self {
        Self::with_handler(sequence_handler(responses))
    }

    /// Starts a server with a prepared [`Handler`].
    pub fn with_handler(handler: Handler) -> Self {
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind 127.0.0.1:0");
        std_listener.set_nonblocking(true).expect("set_nonblocking");
        let addr = std_listener.local_addr().expect("local_addr");
        let listener = tokio::net::TcpListener::from_std(std_listener).expect("tokio listener");
        let shared = Shared::new(handler);
        let tasks: Arc<Mutex<Vec<tokio::task::AbortHandle>>> = Arc::default();
        let base = format!("http://{addr}");

        let (acc_shared, acc_tasks) = (shared.clone(), tasks.clone());
        let accept = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    continue;
                };
                let (shared, base) = (acc_shared.clone(), base.clone());
                let service = hyper::service::service_fn(move |req| {
                    let (shared, base) = (shared.clone(), base.clone());
                    async move { serve(shared, base, req).await }
                });
                let conn = tokio::spawn(async move {
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                        .await;
                });
                acc_tasks.lock().unwrap().push(conn.abort_handle());
            }
        });
        tasks.lock().unwrap().push(accept.abort_handle());
        Self {
            addr,
            shared,
            tasks,
        }
    }

    /// Go `srv.URL`: `http://127.0.0.1:<port>`, no trailing slash.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// The listening address.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Every request received so far, in arrival order.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.shared.requests.lock().unwrap().clone()
    }

    /// The most recent request; panics when there is none.
    pub fn last_request(&self) -> RecordedRequest {
        self.requests()
            .pop()
            .expect("the fake server received no request")
    }

    /// Number of requests received (Go's `calls++` / `requestCount++`).
    pub fn hits(&self) -> usize {
        self.shared.hits.load(Ordering::SeqCst)
    }

    /// Number of responses the client abandoned: a [`FakeResponse::never`] request whose
    /// connection closed, or a body dropped before its last chunk was handed out
    /// (drop-cancellation evidence). A fully sent response, including a fixed-length or empty
    /// one, is never a drop.
    pub fn drops(&self) -> usize {
        self.shared.drops.load(Ordering::SeqCst)
    }

    /// Waits until at least `n` requests arrived (Go `<-started`); panics after
    /// [`WAIT_TIMEOUT`].
    pub async fn wait_for_hits(&self, n: usize) {
        self.shared
            .wait_until(&format!("{n} request(s)"), |s| {
                s.hits.load(Ordering::SeqCst) >= n
            })
            .await;
    }

    /// Waits until at least `n` responses were abandoned by the client; panics after
    /// [`WAIT_TIMEOUT`].
    pub async fn wait_for_drops(&self, n: usize) {
        self.shared
            .wait_until(&format!("{n} dropped response(s)"), |s| {
                s.drops.load(Ordering::SeqCst) >= n
            })
            .await;
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        for task in self.tasks.lock().unwrap().drain(..) {
            task.abort();
        }
    }
}

async fn read_incoming(mut body: Incoming) -> Result<Bytes, hyper::Error> {
    let mut out = Vec::new();
    while let Some(frame) = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await {
        if let Ok(data) = frame?.into_data() {
            out.extend_from_slice(&data);
        }
    }
    Ok(Bytes::from(out))
}

fn build_response<B>(resp: &FakeResponse, body: B) -> http::Response<B> {
    let mut builder = http::Response::builder().status(resp.status);
    for (k, v) in &resp.headers {
        builder = builder.header(k.as_str(), v.as_str());
    }
    builder.body(body).expect("valid scripted response head")
}

async fn serve(
    shared: Arc<Shared>,
    base: String,
    req: http::Request<Incoming>,
) -> Result<http::Response<ScriptedBody>, std::io::Error> {
    let (parts, body) = req.into_parts();
    let body = read_incoming(body).await.map_err(std::io::Error::other)?;
    let path_and_query = parts
        .uri
        .path_and_query()
        .map(|p| p.as_str().to_string())
        .unwrap_or_default();
    let recorded = RecordedRequest::from_parts(format!("{base}{path_and_query}"), &parts, body);
    shared.record(recorded.clone());
    let resp = shared
        .respond(recorded)
        .await
        .map_err(std::io::Error::other)?;
    let body = ScriptedBody::new(&resp, shared.clone());
    Ok(build_response(&resp, body))
}

// ------------------------------------------------------------------------------ scripted client

/// An in-process [`HttpClient`] (Go `roundTripFunc`): records each request and answers it
/// from the handler without a socket. [`Outcome::Reset`] becomes
/// `ModelError::Transport(<message>)`. Clone the `Arc` into `ClientConfig::http_client`.
pub struct ScriptedClient {
    shared: Arc<Shared>,
}

impl ScriptedClient {
    /// A client answering each request with `handler(&request)`.
    pub fn new<F>(handler: F) -> Arc<Self>
    where
        F: Fn(&RecordedRequest) -> FakeResponse + Send + Sync + 'static,
    {
        Self::with_handler(sync_handler(handler))
    }

    /// A client with an async handler.
    pub fn new_async<F, Fut>(handler: F) -> Arc<Self>
    where
        F: Fn(RecordedRequest) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = FakeResponse> + Send + 'static,
    {
        Self::with_handler(async_handler(handler))
    }

    /// A client answering every request with a clone of `resp`.
    pub fn always(resp: FakeResponse) -> Arc<Self> {
        Self::new(move |_| resp.clone())
    }

    /// A client answering every request with `200 application/json` `body`.
    pub fn json(body: impl Into<String>) -> Arc<Self> {
        Self::always(FakeResponse::json(body))
    }

    /// A client answering request `i` with `responses[i]`; the last one repeats.
    pub fn sequence(responses: Vec<FakeResponse>) -> Arc<Self> {
        Self::with_handler(sequence_handler(responses))
    }

    /// A client with a prepared [`Handler`].
    pub fn with_handler(handler: Handler) -> Arc<Self> {
        Arc::new(Self {
            shared: Shared::new(handler),
        })
    }

    /// Every request sent so far.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.shared.requests.lock().unwrap().clone()
    }

    /// The most recent request; panics when there is none.
    pub fn last_request(&self) -> RecordedRequest {
        self.requests()
            .pop()
            .expect("the scripted client got no request")
    }

    /// Number of requests sent.
    pub fn hits(&self) -> usize {
        self.shared.hits.load(Ordering::SeqCst)
    }

    /// Number of responses abandoned before completion (see [`FakeServer::drops`]).
    pub fn drops(&self) -> usize {
        self.shared.drops.load(Ordering::SeqCst)
    }

    /// Waits until at least `n` requests were sent.
    pub async fn wait_for_hits(&self, n: usize) {
        self.shared
            .wait_until(&format!("{n} request(s)"), |s| {
                s.hits.load(Ordering::SeqCst) >= n
            })
            .await;
    }

    /// Waits until at least `n` responses were abandoned.
    pub async fn wait_for_drops(&self, n: usize) {
        self.shared
            .wait_until(&format!("{n} dropped response(s)"), |s| {
                s.drops.load(Ordering::SeqCst) >= n
            })
            .await;
    }

    async fn send_scripted(
        &self,
        req: http::Request<Bytes>,
    ) -> Result<http::Response<BodyStream>, ModelError> {
        let (parts, body) = req.into_parts();
        let recorded = RecordedRequest::from_parts(parts.uri.to_string(), &parts, body);
        self.shared.record(recorded.clone());
        let resp = self
            .shared
            .respond(recorded)
            .await
            .map_err(|msg| ModelError::Transport(msg.into()))?;
        let mut body = ScriptedBody::new(&resp, self.shared.clone());
        let stream: BodyStream =
            futures::stream::poll_fn(move |cx| body.poll_chunk(cx).map(|c| c.map(Ok))).boxed();
        Ok(build_response(&resp, stream))
    }
}

// `HttpClient` is an `async_trait` trait; gadk-anthropic has no `async-trait` dev-dependency,
// so this is the macro's expansion written out.
impl HttpClient for ScriptedClient {
    fn send<'life0, 'async_trait>(
        &'life0 self,
        req: http::Request<Bytes>,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<http::Response<BodyStream>, ModelError>>
                + Send
                + 'async_trait,
        >,
    >
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(self.send_scripted(req))
    }
}

// --------------------------------------------------------------------------------- log capture

/// One captured `tracing` event (Go `capturedLogRecord`).
#[derive(Debug, Clone, PartialEq)]
pub struct CapturedEvent {
    /// Event level (Go `slog.LevelWarn` → `Level::WARN`).
    pub level: Level,
    /// Event target (module path of the emitting code).
    pub target: String,
    /// The message (Go `Record.Message`).
    pub message: String,
    /// Fields in emission order (Go `Attrs`), values rendered like Go `Value.String()`:
    /// strings raw, numbers and bools in decimal/`true`, anything else via `Debug`.
    pub fields: Vec<(String, String)>,
}

impl CapturedEvent {
    /// Go `records[i].Attrs[key]`: the field's value, if present.
    pub fn field(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// `msg key=value key=value …` (plan D10), fields in emission order.
    pub fn render(&self) -> String {
        let mut out = self.message.clone();
        for (k, v) in &self.fields {
            out.push_str(&format!(" {k}={v}"));
        }
        out
    }
}

#[derive(Default)]
struct FieldVisitor {
    message: String,
    fields: Vec<(String, String)>,
}

impl FieldVisitor {
    fn put(&mut self, field: &Field, value: String) {
        if field.name() == "message" {
            self.message = value;
        } else {
            self.fields.push((field.name().to_string(), value));
        }
    }
}

impl Visit for FieldVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.put(field, value.to_string());
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.put(field, value.to_string());
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.put(field, value.to_string());
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.put(field, value.to_string());
    }
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.put(field, value.to_string());
    }
    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.put(field, value.to_string());
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.put(field, format!("{value:?}"));
    }
}

struct CaptureLayer {
    events: Arc<Mutex<Vec<CapturedEvent>>>,
    target_prefix: Option<String>,
}

impl<S: Subscriber> Layer<S> for CaptureLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: LayerContext<'_, S>) {
        let meta = event.metadata();
        if let Some(prefix) = &self.target_prefix
            && !meta.target().starts_with(prefix.as_str())
        {
            return;
        }
        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);
        self.events.lock().unwrap().push(CapturedEvent {
            level: *meta.level(),
            target: meta.target().to_string(),
            message: visitor.message,
            fields: visitor.fields,
        });
    }
}

/// Captures `tracing` events (Go `slogCaptureHandler` passed as `ClientConfig.Logger`).
/// Nothing global is installed: activate it per test with [`set_default`](Self::set_default)
/// (current thread; `#[tokio::test]`'s default runtime polls the model there) or per future
/// with [`capture`](Self::capture).
#[derive(Clone)]
pub struct LogCapture {
    events: Arc<Mutex<Vec<CapturedEvent>>>,
    dispatch: Dispatch,
}

impl Default for LogCapture {
    fn default() -> Self {
        Self::new()
    }
}

impl LogCapture {
    /// Captures events whose target starts with `gadk_anthropic` (the model's own warnings,
    /// as Go's per-model logger sees only the model's records).
    pub fn new() -> Self {
        Self::with_target_prefix(Some("gadk_anthropic"))
    }

    /// Captures events whose target starts with `prefix`, or every event for `None`.
    pub fn with_target_prefix(prefix: Option<&str>) -> Self {
        let events: Arc<Mutex<Vec<CapturedEvent>>> = Arc::default();
        let layer = CaptureLayer {
            events: events.clone(),
            target_prefix: prefix.map(str::to_string),
        };
        let dispatch = Dispatch::new(tracing_subscriber::registry().with(layer));
        Self { events, dispatch }
    }

    /// The subscriber, for `tracing::dispatcher::with_default` or
    /// `tracing::instrument::WithSubscriber::with_subscriber`.
    pub fn dispatch(&self) -> Dispatch {
        self.dispatch.clone()
    }

    /// Makes this capture the current thread's subscriber until the guard drops.
    pub fn set_default(&self) -> tracing::dispatcher::DefaultGuard {
        tracing::dispatcher::set_default(&self.dispatch)
    }

    /// Runs `fut` with this capture as its subscriber (wherever it is polled).
    pub async fn capture<F: Future>(&self, fut: F) -> F::Output {
        use tracing::instrument::WithSubscriber;
        fut.with_subscriber(self.dispatch.clone()).await
    }

    /// Go `Snapshot()`: every event captured so far.
    pub fn events(&self) -> Vec<CapturedEvent> {
        self.events.lock().unwrap().clone()
    }

    /// Only the `WARN` events.
    pub fn warnings(&self) -> Vec<CapturedEvent> {
        self.events()
            .into_iter()
            .filter(|e| e.level == Level::WARN)
            .collect()
    }

    /// Every event rendered as `msg key=value …`.
    pub fn rendered(&self) -> Vec<String> {
        self.events().iter().map(CapturedEvent::render).collect()
    }

    /// Forgets the events captured so far.
    pub fn clear(&self) {
        self.events.lock().unwrap().clear();
    }
}

// ------------------------------------------------------------------------------- SSE and JSON

/// One SSE event as Go's tests write it: `event: <name>\ndata: <data>\n\n`.
pub fn sse_event(name: &str, data: &str) -> String {
    format!("event: {name}\ndata: {data}\n\n")
}

/// Go `strings.Join(lines, "\n")` over an SSE transcript written line by line.
pub fn sse_lines(lines: &[&str]) -> String {
    lines.join("\n")
}

/// Go's non-streaming text reply literal:
/// `{"id":<id>,"type":"message","role":"assistant","model":"claude-sonnet-4-5-20250929",
/// "stop_reason":"end_turn","content":[{"type":"text","text":<text>}],
/// "usage":{"input_tokens":<in>,"output_tokens":<out>}}` (one line).
pub fn text_message_json(id: &str, text: &str, input_tokens: u64, output_tokens: u64) -> String {
    let id = serde_json::to_string(id).unwrap();
    let text = serde_json::to_string(text).unwrap();
    format!(
        r#"{{"id":{id},"type":"message","role":"assistant","model":"{MODEL}","stop_reason":"end_turn","content":[{{"type":"text","text":{text}}}],"usage":{{"input_tokens":{input_tokens},"output_tokens":{output_tokens}}}}}"#
    )
}

/// Go `json.Compact`: drops insignificant whitespace outside strings; `None` when `data` is
/// not valid JSON (Go returns an error and the caller keeps the original bytes).
pub fn json_compact(data: &[u8]) -> Option<Vec<u8>> {
    serde_json::from_slice::<serde_json::Value>(data).ok()?;
    let mut out = Vec::with_capacity(data.len());
    let (mut in_string, mut escaped) = (false, false);
    for &b in data {
        if in_string {
            out.push(b);
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b' ' | b'\t' | b'\n' | b'\r' => {}
            b'"' => {
                in_string = true;
                out.push(b);
            }
            _ => out.push(b),
        }
    }
    Some(out)
}

// ------------------------------------------------------------------------------------ fixtures

/// Go `samplePNG1x1`: a valid 1x1 blue PNG (73 bytes).
pub const SAMPLE_PNG_1X1: [u8; 73] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, //
    0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, //
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, //
    0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53, //
    0xde, 0x00, 0x00, 0x00, 0x10, 0x49, 0x44, 0x41, //
    0x54, 0x78, 0x9c, 0x62, 0x62, 0x60, 0xf8, 0x0f, //
    0x08, 0x00, 0x00, 0xff, 0xff, 0x01, 0x0b, 0x01, //
    0x02, 0xd6, 0x95, 0x43, 0xa8, 0x00, 0x00, 0x00, //
    0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, //
    0x82,
];

/// [`SAMPLE_PNG_1X1`] as `Bytes` (for `Part::from_bytes`).
pub fn sample_png() -> Bytes {
    Bytes::from_static(&SAMPLE_PNG_1X1)
}

/// Go's ADC file contents in `TestNewModel_VertexAndModelNames` (authorized user, no
/// `token_uri`).
pub const ADC_AUTHORIZED_USER_JSON: &str = r#"{"type":"authorized_user","client_id":"test-client","client_secret":"test-secret","refresh_token":"test-refresh"}"#;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_temp_path(suffix: &str) -> PathBuf {
    let n = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!(
        "gadk-anthropic-test-{}-{nanos}-{n}-{suffix}",
        std::process::id()
    ))
}

/// A file in the system temp directory, deleted on drop (Go `t.TempDir()` + `os.WriteFile`).
#[derive(Debug)]
pub struct TempFile {
    path: PathBuf,
}

impl TempFile {
    /// Writes `contents` to a fresh file whose name ends in `suffix` (mode 0600 on Unix).
    pub fn with_contents(suffix: &str, contents: &str) -> Self {
        let path = unique_temp_path(suffix);
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&path).expect("create temp file");
        std::io::Write::write_all(&mut file, contents.as_bytes()).expect("write temp file");
        Self { path }
    }

    /// The file's path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The file's path as a `String` (for env values).
    pub fn path_string(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// An `adc.json` with [`ADC_AUTHORIZED_USER_JSON`].
pub fn adc_authorized_user_file() -> TempFile {
    TempFile::with_contents("adc.json", ADC_AUTHORIZED_USER_JSON)
}

/// Go's `fmt.Sprintf(`{…,"token_uri":%q}`, tokenSrv.URL)` ADC file (authorized user whose
/// token endpoint is a fake server).
pub fn adc_authorized_user_file_with_token_uri(token_uri: &str) -> TempFile {
    let uri = serde_json::to_string(token_uri).unwrap();
    TempFile::with_contents(
        "adc.json",
        &format!(
            r#"{{"type":"authorized_user","client_id":"test-client","client_secret":"test-secret","refresh_token":"test-refresh","token_uri":{uri}}}"#
        ),
    )
}

/// A path in the temp directory that does not exist (ruling R-b's "no ADC anywhere" form).
pub fn missing_adc_path() -> String {
    unique_temp_path("missing-adc.json")
        .to_string_lossy()
        .into_owned()
}

/// An empty environment: no `ANTHROPIC_*`, `GOOGLE_*` or `CLOUD_ML_*` variables (Go's
/// `t.Setenv(name, "")` resets).
pub fn empty_env() -> Arc<MapEnv> {
    Arc::new(MapEnv(Default::default()))
}

/// An environment with exactly these variables.
pub fn env(vars: &[(&str, &str)]) -> Arc<MapEnv> {
    Arc::new(MapEnv(
        vars.iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect(),
    ))
}

/// An environment whose only variable is `GOOGLE_APPLICATION_CREDENTIALS=<path>`.
pub fn adc_env(path: &str) -> Arc<MapEnv> {
    env(&[("GOOGLE_APPLICATION_CREDENTIALS", path)])
}

// ------------------------------------------------------------------------------ model builders

/// Go `staticTokenSource{token}`: credentials whose headers are
/// `authorization: Bearer <token>`.
pub fn static_token_credentials(token: &str) -> Credentials {
    Credentials::from(StaticToken(format!("Bearer {token}")))
}

#[derive(Debug)]
struct StaticToken(String);

impl CredentialsProvider for StaticToken {
    async fn headers(
        &self,
        _extensions: http::Extensions,
    ) -> Result<CacheableResource<http::HeaderMap>, CredentialsError> {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            http::HeaderValue::from_str(&self.0).expect("ASCII token"),
        );
        Ok(CacheableResource::New {
            entity_tag: EntityTag::new(),
            data: headers,
        })
    }

    async fn universe_domain(&self) -> Option<String> {
        None
    }
}

/// Go `ClientConfig{APIKey: "test-key", BaseURL: srv.URL}` with an empty environment.
pub fn anthropic_config(base_url: &str) -> ClientConfig {
    ClientConfig {
        backend: Backend::Anthropic,
        api_key: Some("test-key".to_string()),
        base_url: Some(base_url.to_string()),
        env: empty_env(),
        ..Default::default()
    }
}

/// Go `ClientConfig{Backend: BackendVertex, Project: "test-project", Location: "us-east5",
/// TokenSource: staticTokenSource{token: "test-token"}, BaseURL: srv.URL}` with an empty
/// environment.
pub fn vertex_config(base_url: &str) -> ClientConfig {
    ClientConfig {
        backend: Backend::Vertex,
        project: Some("test-project".to_string()),
        location: Some("us-east5".to_string()),
        credentials: Some(static_token_credentials("test-token")),
        base_url: Some(base_url.to_string()),
        env: empty_env(),
        ..Default::default()
    }
}

/// `AnthropicModel::new(name, cfg)`, panicking like Go's `t.Fatalf("NewModel() error = %v")`.
pub async fn new_model(name: &str, cfg: ClientConfig) -> Arc<dyn Llm> {
    match AnthropicModel::new(name, cfg).await {
        Ok(m) => m,
        Err(e) => panic!("NewModel() error = {e}"),
    }
}

/// Go `&model.LLMRequest{Contents: genai.Text(s)}`.
pub fn text_request(s: &str) -> LlmRequest {
    LlmRequest {
        contents: gadk_genai::text(s),
        ..Default::default()
    }
}

// --------------------------------------------------------------------------- stream collectors

/// Drains a `generate_content` stream.
pub async fn collect(
    stream: BoxStream<'static, Result<LlmResponse, ModelError>>,
) -> Vec<Result<LlmResponse, ModelError>> {
    stream.collect().await
}

/// Go's `for resp, err := range m.GenerateContent(ctx, req, stream)`: every item, in order.
pub async fn generate(
    m: &dyn Llm,
    req: LlmRequest,
    stream: bool,
) -> Vec<Result<LlmResponse, ModelError>> {
    collect(m.generate_content(req, stream)).await
}

/// Every response; panics on the first error (Go `t.Fatalf("GenerateContent() error = %v")`).
pub async fn generate_ok(m: &dyn Llm, req: LlmRequest, stream: bool) -> Vec<LlmResponse> {
    generate(m, req, stream)
        .await
        .into_iter()
        .map(|r| r.unwrap_or_else(|e| panic!("GenerateContent() error = {e}")))
        .collect()
}

/// The last response (Go `got = resp`); panics on any error or an empty stream.
pub async fn generate_last(m: &dyn Llm, req: LlmRequest, stream: bool) -> LlmResponse {
    generate_ok(m, req, stream)
        .await
        .pop()
        .expect("GenerateContent() yielded no response")
}

/// The last error (Go `gotErr = err`); panics when the stream yielded none.
pub async fn generate_err(m: &dyn Llm, req: LlmRequest, stream: bool) -> ModelError {
    generate(m, req, stream)
        .await
        .into_iter()
        .filter_map(Result::err)
        .next_back()
        .expect("GenerateContent() yielded no error")
}

/// The [`AnthropicError`] inside a `ModelError::Other` (how the model reports every error),
/// or `None`.
pub fn as_anthropic(err: &ModelError) -> Option<&AnthropicError> {
    match err {
        ModelError::Other(inner) => inner.downcast_ref::<AnthropicError>(),
        _ => None,
    }
}

/// Like [`as_anthropic`], panicking when `err` is not an `AnthropicError`.
pub fn anthropic_error(err: &ModelError) -> &AnthropicError {
    as_anthropic(err).unwrap_or_else(|| panic!("error {err:?} is not an AnthropicError"))
}

// ------------------------------------------------------------------------------------- httprr

/// `crates/gadk-anthropic/tests/testdata/model/anthropicmodel/testdata/<file>`.
pub fn testdata_path(file: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/testdata/model/anthropicmodel/testdata")
        .join(file)
}

/// Go `openHTTPRR(t, rrfile)`: opens `testdata_path(file)` over a real `reqwest` client
/// (used only when `GADK_HTTPRR_RECORD` selects the file), with Go's scrubbers. Use
/// `is_recording()` for Go's `rr.Recording()`.
pub fn open_httprr(file: &str) -> Arc<RecordReplay> {
    let path = testdata_path(file);
    let mut rr = RecordReplay::open(&path, Arc::new(ReqwestClient::default()))
        .unwrap_or_else(|e| panic!("httprr.Open({:?}) failed: {e}", path.display()));
    rr.scrub_req(Box::new(scrub_anthropic_request));
    rr.scrub_resp(Box::new(scrub_anthropic_response));
    Arc::new(rr)
}

/// Go `scrubAnthropicRequest`: drops `authorization`, `x-api-key`, `user-agent`,
/// `x-goog-api-client` and `x-stainless-*`; rewrites `/projects/<p>/` to
/// `/projects/test-project/`; compacts a JSON body.
pub fn scrub_anthropic_request(req: &mut http::Request<Bytes>) -> Result<(), HttprrError> {
    let drop: Vec<http::HeaderName> = req
        .headers()
        .keys()
        .filter(|k| {
            let lower = k.as_str().to_ascii_lowercase();
            matches!(
                lower.as_str(),
                "authorization" | "x-api-key" | "user-agent" | "x-goog-api-client"
            ) || lower.starts_with("x-stainless-")
        })
        .cloned()
        .collect();
    for k in drop {
        req.headers_mut().remove(k);
    }

    let path = req.uri().path().to_string();
    if let Some(idx) = path.find("/projects/") {
        let rest = &path[idx + "/projects/".len()..];
        if let Some(slash) = rest.find('/').filter(|&s| s > 0) {
            let new_path = format!("{}/projects/test-project{}", &path[..idx], &rest[slash..]);
            let path_and_query = match req.uri().query() {
                Some(q) => format!("{new_path}?{q}"),
                None => new_path,
            };
            let mut parts = req.uri().clone().into_parts();
            parts.path_and_query = Some(
                path_and_query
                    .parse()
                    .map_err(|e: http::uri::InvalidUri| HttprrError::Wire(e.to_string()))?,
            );
            *req.uri_mut() =
                http::Uri::from_parts(parts).map_err(|e| HttprrError::Wire(e.to_string()))?;
        }
    }

    if !req.body().is_empty()
        && let Some(compact) = json_compact(req.body())
    {
        *req.body_mut() = Bytes::from(compact);
    }
    Ok(())
}

/// Go `scrubAnthropicResponse`: removes the `anthropic-organization-id`,
/// `anthropic-workspace-id`, `set-cookie`, `request-id` and `x-request-id` header lines
/// (case-insensitive) from the logged response, up to the first empty line.
pub fn scrub_anthropic_response(buf: &mut Vec<u8>) -> Result<(), HttprrError> {
    const DROPPED: [&str; 5] = [
        "anthropic-organization-id:",
        "anthropic-workspace-id:",
        "set-cookie:",
        "request-id:",
        "x-request-id:",
    ];
    let mut out: Vec<&[u8]> = Vec::new();
    let mut in_headers = true;
    for line in buf.split(|&b| b == b'\n') {
        if in_headers {
            let mut trimmed = line;
            while let Some(rest) = trimmed.strip_suffix(b"\r") {
                trimmed = rest;
            }
            if trimmed.is_empty() {
                in_headers = false;
                out.push(line);
                continue;
            }
            let lower = trimmed.to_ascii_lowercase();
            if DROPPED.iter().any(|p| lower.starts_with(p.as_bytes())) {
                continue;
            }
        }
        out.push(line);
    }
    let joined = out.join(&b'\n');
    *buf = joined;
    Ok(())
}

// ---------------------------------------------------------------------------- harness self-test

#[tokio::test]
async fn harness_server_records_request_and_answers_json() {
    let srv = FakeServer::json(text_message_json("msg_1", "ok", 5, 1));
    let resp = reqwest::Client::new()
        .post(format!("{}/v1/messages?beta=true", srv.url()))
        .header("X-Api-Key", "k")
        .header("anthropic-version", "2023-06-01")
        .body(r#"{"model":"m","stream":true}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["content-type"], "application/json");
    assert_eq!(
        resp.text().await.unwrap(),
        r#"{"id":"msg_1","type":"message","role":"assistant","model":"claude-sonnet-4-5-20250929","stop_reason":"end_turn","content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":5,"output_tokens":1}}"#
    );

    assert_eq!(srv.hits(), 1);
    let req = srv.last_request();
    assert_eq!(req.method, "POST");
    assert_eq!(req.path, "/v1/messages");
    assert_eq!(req.query.as_deref(), Some("beta=true"));
    assert_eq!(req.path_and_query(), "/v1/messages?beta=true");
    assert_eq!(req.url, format!("{}/v1/messages?beta=true", srv.url()));
    assert_eq!(req.header("x-api-key"), "k");
    assert_eq!(req.header("Anthropic-Version"), "2023-06-01");
    assert_eq!(req.header("authorization"), "");
    assert_eq!(req.json()["model"], "m");
    assert!(req.is_stream());
    assert_eq!(req.body_text(), r#"{"model":"m","stream":true}"#);
}

#[tokio::test]
async fn harness_server_streams_chunks_with_delays() {
    let events = [
        sse_event("message_start", r#"{"type":"message_start"}"#),
        sse_event("message_stop", r#"{"type":"message_stop"}"#),
    ];
    let resp = FakeResponse::sse_chunks(events.clone())
        .delay(Duration::from_millis(20))
        .chunk("event: ping\ndata: {}\n\n");
    let srv = FakeServer::always(resp);
    let resp = reqwest::get(srv.url()).await.unwrap();
    assert_eq!(resp.headers()["content-type"], "text/event-stream");
    let mut body = resp.bytes_stream();
    let mut got = Vec::new();
    while let Some(chunk) = body.next().await {
        got.extend_from_slice(&chunk.unwrap());
    }
    assert_eq!(
        String::from_utf8(got).unwrap(),
        format!("{}{}event: ping\ndata: {{}}\n\n", events[0], events[1])
    );
    assert_eq!(srv.drops(), 0);
}

#[tokio::test]
async fn harness_server_fully_read_fixed_length_bodies_are_not_drops() {
    let srv = FakeServer::json(text_message_json("msg_1", "ok", 5, 1));
    let client = reqwest::Client::new();
    for _ in 0..2 {
        let resp = client.post(srv.url()).body("{}").send().await.unwrap();
        assert!(resp.headers().contains_key("content-length"));
        assert!(resp.text().await.unwrap().contains("msg_1"));
    }
    // hyper releases the body after the response is written; give it time to do so.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!((srv.hits(), srv.drops()), (2, 0));
}

#[tokio::test]
async fn harness_server_fully_read_empty_body_is_not_a_drop() {
    let srv = FakeServer::always(FakeResponse::new(204));
    let resp = reqwest::get(srv.url()).await.unwrap();
    assert_eq!(resp.status(), 204);
    assert!(resp.bytes().await.unwrap().is_empty());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!((srv.hits(), srv.drops()), (1, 0));
}

#[tokio::test]
async fn harness_server_observes_client_drop_mid_body() {
    let srv = FakeServer::always(
        FakeResponse::sse(sse_event("message_start", "{}"))
            .delay(Duration::from_millis(10))
            .hang(),
    );
    let resp = reqwest::get(srv.url()).await.unwrap();
    let mut body = resp.bytes_stream();
    let first = body.next().await.unwrap().unwrap();
    assert_eq!(&first[..], b"event: message_start\ndata: {}\n\n");
    drop(body);
    srv.wait_for_drops(1).await;
    assert_eq!(srv.drops(), 1);
}

#[tokio::test]
async fn harness_server_observes_drop_of_unanswered_request() {
    let srv = FakeServer::always(FakeResponse::never());
    let url = srv.url();
    let client = tokio::spawn(async move { reqwest::get(url).await });
    srv.wait_for_hits(1).await;
    assert_eq!(srv.drops(), 0);
    client.abort();
    srv.wait_for_drops(1).await;
}

#[tokio::test]
async fn harness_server_async_handler_waits_for_release() {
    let release = Arc::new(Notify::new());
    let gate = release.clone();
    let srv = FakeServer::start_async(move |req| {
        let gate = gate.clone();
        async move {
            gate.notified().await;
            FakeResponse::json(format!(r#"{{"path":"{}"}}"#, req.path))
        }
    });
    let url = format!("{}/v1/messages", srv.url());
    let client = tokio::spawn(async move { reqwest::get(url).await.unwrap().text().await });
    srv.wait_for_hits(1).await;
    release.notify_one();
    assert_eq!(client.await.unwrap().unwrap(), r#"{"path":"/v1/messages"}"#);
}

#[tokio::test]
async fn harness_server_sequence_status_headers_and_reset() {
    let srv = FakeServer::sequence(vec![
        FakeResponse::json_status(
            529,
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
        )
        .header("Retry-After-Ms", "1"),
        FakeResponse::reset("connection reset by script"),
        FakeResponse::json("{}").delay_headers(Duration::from_millis(5)),
    ]);
    let client = reqwest::Client::new();
    let first = client.get(srv.url()).send().await.unwrap();
    assert_eq!(first.status(), 529);
    assert_eq!(first.headers()["retry-after-ms"], "1");
    assert!(client.get(srv.url()).send().await.is_err());
    for _ in 0..2 {
        let ok = client.get(srv.url()).send().await.unwrap();
        assert_eq!(ok.status(), 200);
        assert_eq!(ok.text().await.unwrap(), "{}");
    }
    assert_eq!(srv.hits(), 4);
}

#[tokio::test]
async fn harness_scripted_client_records_and_streams() {
    let client = ScriptedClient::always(
        FakeResponse::sse_chunks(["a", "b"])
            .delay(Duration::from_millis(5))
            .chunk("c"),
    );
    let req = http::Request::builder()
        .method("POST")
        .uri("https://api.anthropic.com/v1/messages")
        .header("x-api-key", "k")
        .body(Bytes::from_static(br#"{"stream":true}"#))
        .unwrap();
    let resp = client.send(req).await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["content-type"], "text/event-stream");
    let body = gadk_model::collect_body(resp.into_body()).await.unwrap();
    assert_eq!(&body[..], b"abc");
    let rec = client.last_request();
    assert_eq!(rec.url, "https://api.anthropic.com/v1/messages");
    assert_eq!(rec.path, "/v1/messages");
    assert_eq!(rec.header("X-Api-Key"), "k");
    assert!(rec.is_stream());
    assert_eq!((client.hits(), client.drops()), (1, 0));
}

#[tokio::test]
async fn harness_scripted_client_reset_and_drop() {
    let client = ScriptedClient::sequence(vec![
        FakeResponse::reset("boom"),
        FakeResponse::sse("x").hang(),
    ]);
    let req = || {
        http::Request::builder()
            .uri("http://h/")
            .body(Bytes::new())
            .unwrap()
    };
    match client.send(req()).await {
        Err(ModelError::Transport(e)) => assert_eq!(e.to_string(), "boom"),
        other => panic!("want transport error, got {:?}", other.map(|r| r.status())),
    }
    let resp = client.send(req()).await.unwrap();
    let mut body = resp.into_body();
    assert_eq!(&body.next().await.unwrap().unwrap()[..], b"x");
    drop(body);
    assert_eq!(client.drops(), 1);
    client.wait_for_drops(1).await;
}

#[tokio::test]
async fn harness_log_capture_renders_msg_key_value() {
    let logs = LogCapture::new();
    {
        let _guard = logs.set_default();
        tracing::warn!(
            target: "gadk_anthropic::request",
            backend = "vertex",
            model = MODEL,
            role = "user",
            part_kind = "file_data",
            index = 3_i64,
            "skipping unsupported content part for Claude request"
        );
        tracing::info!(target: "hyper::proto", "ignored: outside gadk_anthropic");
    }
    tracing::warn!(target: "gadk_anthropic::request", "after the guard: not captured");

    let events = logs.events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].level, Level::WARN);
    assert_eq!(events[0].target, "gadk_anthropic::request");
    assert_eq!(
        events[0].message,
        "skipping unsupported content part for Claude request"
    );
    assert_eq!(events[0].field("backend"), Some("vertex"));
    assert_eq!(events[0].field("index"), Some("3"));
    assert_eq!(events[0].field("missing"), None);
    assert_eq!(
        logs.rendered(),
        vec![format!(
            "skipping unsupported content part for Claude request backend=vertex model={MODEL} role=user part_kind=file_data index=3"
        )]
    );
    assert_eq!(logs.warnings().len(), 1);

    let all = LogCapture::with_target_prefix(None);
    all.capture(async {
        tracing::info!(target: "elsewhere", answer = 42_u64, "inside capture");
    })
    .await;
    assert_eq!(all.rendered(), vec!["inside capture answer=42".to_string()]);
    assert!(all.warnings().is_empty());
    all.clear();
    assert!(all.events().is_empty());
}

#[test]
fn harness_sse_and_json_builders_match_go_layout() {
    assert_eq!(
        sse_event("message_stop", r#"{"type":"message_stop"}"#),
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
    );
    assert_eq!(
        sse_lines(&["event: a", "data: {}", "", "event: b", "data: {}", ""]),
        "event: a\ndata: {}\n\nevent: b\ndata: {}\n"
    );
    assert_eq!(
        text_message_json("msg_v", "say \"hi\"", 4, 1),
        r#"{"id":"msg_v","type":"message","role":"assistant","model":"claude-sonnet-4-5-20250929","stop_reason":"end_turn","content":[{"type":"text","text":"say \"hi\""}],"usage":{"input_tokens":4,"output_tokens":1}}"#
    );
    assert_eq!(
        json_compact(b"{ \"a\" : [1, 2],\n\t\"b\": \"x y\\\" z\" }").unwrap(),
        br#"{"a":[1,2],"b":"x y\" z"}"#.to_vec()
    );
    assert_eq!(json_compact(b"not json"), None);
}

#[test]
fn harness_sample_png_is_go_bytes() {
    assert_eq!(SAMPLE_PNG_1X1.len(), 73);
    assert_eq!(&SAMPLE_PNG_1X1[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(&SAMPLE_PNG_1X1[12..16], b"IHDR");
    assert_eq!(&SAMPLE_PNG_1X1[65..73], b"IEND\xaeB`\x82");
    assert_eq!(sample_png().len(), 73);
}

#[test]
fn harness_adc_files_and_env() {
    let path;
    {
        let file = adc_authorized_user_file();
        path = file.path().to_path_buf();
        assert_eq!(
            std::fs::read_to_string(file.path()).unwrap(),
            ADC_AUTHORIZED_USER_JSON
        );
        let env = adc_env(&file.path_string());
        assert_eq!(
            gadk_anthropic::EnvProvider::var(&*env, "GOOGLE_APPLICATION_CREDENTIALS"),
            Some(file.path_string())
        );
        let with_uri = adc_authorized_user_file_with_token_uri("http://127.0.0.1:9/token");
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(with_uri.path()).unwrap()).unwrap();
        assert_eq!(v["token_uri"], "http://127.0.0.1:9/token");
        assert_eq!(v["type"], "authorized_user");
    }
    assert!(!path.exists(), "TempFile must be removed on drop");
    assert!(!Path::new(&missing_adc_path()).exists());
    assert_eq!(
        gadk_anthropic::EnvProvider::var(&*empty_env(), "ANTHROPIC_API_KEY"),
        None
    );
    let e = env(&[("ANTHROPIC_BASE_URL", "")]);
    assert_eq!(
        gadk_anthropic::EnvProvider::var(&*e, "ANTHROPIC_BASE_URL"),
        Some(String::new())
    );
}

#[tokio::test]
async fn harness_static_token_credentials_and_configs() {
    let creds = static_token_credentials("test-token");
    let CacheableResource::New { data, .. } = creds.headers(http::Extensions::new()).await.unwrap()
    else {
        panic!("want new headers");
    };
    assert_eq!(data["authorization"], "Bearer test-token");

    let a = anthropic_config("http://127.0.0.1:1");
    assert_eq!(a.backend, Backend::Anthropic);
    assert_eq!(a.api_key.as_deref(), Some("test-key"));
    assert_eq!(a.base_url.as_deref(), Some("http://127.0.0.1:1"));
    assert_eq!(a.env.var("ANTHROPIC_API_KEY"), None);

    let v = vertex_config("http://127.0.0.1:1");
    assert_eq!(v.backend, Backend::Vertex);
    assert_eq!(v.project.as_deref(), Some("test-project"));
    assert_eq!(v.location.as_deref(), Some("us-east5"));
    assert!(v.credentials.is_some());
    assert!(v.api_key.is_none());

    let req = text_request("hi");
    assert_eq!(req.contents, gadk_genai::text("hi"));
}

#[tokio::test]
async fn harness_httprr_replays_with_go_scrubbers() {
    let rr = open_httprr("TestGenerate_Vertex.httprr");
    assert!(!rr.is_recording());
    // A pretty-printed body, a real project ID and secret headers: the scrubbers make it
    // match the recorded request.
    let body = "{\n  \"max_tokens\": 8192,\n  \"messages\": [{\"content\": [{\"text\": \"What is 2 + 2? Reply with the digit only.\", \"type\": \"text\"}], \"role\": \"user\"}],\n  \"temperature\": 0,\n  \"anthropic_version\": \"vertex-2023-10-16\"\n}";
    let req = http::Request::builder()
        .method("POST")
        .uri("https://us-east5-aiplatform.googleapis.com/v1/projects/my-real-proj/locations/us-east5/publishers/anthropic/models/claude-sonnet-4-5-20250929:rawPredict")
        .header("authorization", "Bearer secret")
        .header("x-goog-api-client", "gadk")
        .header("content-type", "application/json")
        .body(Bytes::from(body))
        .unwrap();
    let resp = rr.send(req).await.unwrap();
    assert_eq!(resp.status(), 200);
    let body = gadk_model::collect_body(resp.into_body()).await.unwrap();
    assert!(String::from_utf8_lossy(&body).contains(r#""text":"4""#));
}

#[test]
fn harness_scrub_request_matches_go() {
    let mut req = http::Request::builder()
        .uri("https://h/v1/projects/p-1/locations/l/x:rawPredict?alt=sse")
        .header("Authorization", "Bearer s")
        .header("X-Api-Key", "k")
        .header("User-Agent", "ua")
        .header("X-Goog-Api-Client", "c")
        .header("X-Stainless-Os", "linux")
        .header("Anthropic-Version", "2023-06-01")
        .body(Bytes::from_static(b"{ \"a\": 1 }"))
        .unwrap();
    scrub_anthropic_request(&mut req).unwrap();
    assert_eq!(
        req.uri().to_string(),
        "https://h/v1/projects/test-project/locations/l/x:rawPredict?alt=sse"
    );
    let names: Vec<&str> = req.headers().keys().map(|k| k.as_str()).collect();
    assert_eq!(names, vec!["anthropic-version"]);
    assert_eq!(&req.body()[..], br#"{"a":1}"#);

    // `/projects//` (empty project) and non-JSON bodies are left alone, as in Go.
    let mut req = http::Request::builder()
        .uri("https://h/v1/projects//x")
        .body(Bytes::from_static(b"not json "))
        .unwrap();
    scrub_anthropic_request(&mut req).unwrap();
    assert_eq!(req.uri().to_string(), "https://h/v1/projects//x");
    assert_eq!(&req.body()[..], b"not json ");
}

#[test]
fn harness_scrub_response_matches_go() {
    let mut buf = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nRequest-Id: req_1\r\nanthropic-organization-id: org\r\nSet-Cookie: a=b\r\nX-Request-Id: x\r\nAnthropic-Workspace-Id: w\r\n\r\nrequest-id: body line kept\n".to_vec();
    scrub_anthropic_response(&mut buf).unwrap();
    assert_eq!(
        String::from_utf8(buf).unwrap(),
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\nrequest-id: body line kept\n"
    );

    // Go `strings.TrimRight(line, "\r")` strips every trailing CR: `\r\r` still ends the headers.
    let mut buf = b"HTTP/1.1 200 OK\r\n\r\r\nrequest-id: body\n".to_vec();
    scrub_anthropic_response(&mut buf).unwrap();
    assert_eq!(
        String::from_utf8(buf).unwrap(),
        "HTTP/1.1 200 OK\r\n\r\r\nrequest-id: body\n"
    );
}
