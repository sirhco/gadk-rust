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

//! Test doubles for code that sends through an [`HttpClient`] (feature `test-util`), shared
//! by the provider crates' tests:
//!
//! - [`ScriptedClient`]: an in-process [`HttpClient`] (Go `roundTripFunc` behind an
//!   `http.Client`) that records every request ([`RecordedRequest`], with the tokio time it
//!   arrived) and answers from a handler or a script of [`FakeResponse`]s;
//! - [`FakeServer`]: the same handlers behind a real HTTP/1 server on `127.0.0.1` (Go
//!   `httptest.NewServer`), for code that builds its own `reqwest` client;
//! - [`FakeResponse`]: status, headers, body chunks, pauses, a body error
//!   ([`FakeResponse::fail`], a dropped connection), a body that never ends
//!   ([`FakeResponse::hang`]), a request never answered ([`FakeResponse::never`]) and a
//!   connection error ([`FakeResponse::reset`]);
//! - [`sse_event`] / [`sse_lines`]: SSE transcripts as Go's tests write them.
//!
//! [`ScriptedClient`] works under paused tokio time (`start_paused`); [`FakeServer`] needs a
//! running tokio runtime and real time.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::VecDeque;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use futures::future::BoxFuture;
use hyper::body::{Body, Frame, Incoming, SizeHint};
use parking_lot::Mutex;
use tokio::sync::Notify;
use tokio::time::Instant;

use crate::error::ModelError;
use crate::http::{BodyStream, HttpClient};

/// How long the `wait_for_*` helpers wait before failing the test.
pub const WAIT_TIMEOUT: Duration = Duration::from_secs(10);

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
    /// When the request arrived (tokio time, so paused-time tests can measure backoff).
    pub at: Instant,
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
            at: Instant::now(),
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
    /// The body fails here, as a dropped connection does: [`ScriptedClient`] yields
    /// `Err(ModelError::Transport(<message>))` and ends the body; [`FakeServer`] aborts the
    /// connection. Later steps are skipped.
    Fail(String),
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

    /// Appends a body failure (see [`BodyStep::Fail`]), e.g. `fail("unexpected EOF")`.
    pub fn fail(mut self, message: impl Into<String>) -> Self {
        self.steps.push(BodyStep::Fail(message.into()));
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
        let mut q = queue.lock();
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
        self.requests.lock().push(req);
        self.hits.fetch_add(1, Ordering::SeqCst);
        self.changed.notify_waiters();
    }

    fn dropped(&self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
        self.changed.notify_waiters();
    }

    fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().clone()
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }

    fn drops(&self) -> usize {
        self.drops.load(Ordering::SeqCst)
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
                self.hits(),
                self.drops()
            );
        }
    }

    /// Runs the handler, applying `Never` and the header delay. `Err` = reset with message.
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
/// has no steps) with [`BodyEnd::Finish`], or when a [`BodyStep::Fail`] is played: hyper drops
/// a fixed-length body after writing the declared bytes without polling it to `None`, so
/// waiting for `None` would count every fully sent `Content-Length` response as a drop.
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

    fn poll_chunk(&mut self, cx: &mut Context<'_>) -> Poll<Option<Result<Bytes, String>>> {
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
                    return Poll::Ready(Some(Ok(b)));
                }
                Some(BodyStep::Delay(d)) => self.sleep = Some(Box::pin(tokio::time::sleep(d))),
                Some(BodyStep::Fail(msg)) => {
                    self.steps.clear();
                    self.end = BodyEnd::Finish;
                    self.finished = true;
                    return Poll::Ready(Some(Err(msg)));
                }
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
    type Error = std::io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, std::io::Error>>> {
        self.get_mut()
            .poll_chunk(cx)
            .map(|chunk| chunk.map(|r| r.map(Frame::data).map_err(std::io::Error::other)))
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

fn build_response<B>(resp: &FakeResponse, body: B) -> http::Response<B> {
    let mut builder = http::Response::builder().status(resp.status);
    for (k, v) in &resp.headers {
        builder = builder.header(k.as_str(), v.as_str());
    }
    builder.body(body).expect("valid scripted response head")
}

fn transport_error(message: String) -> ModelError {
    ModelError::Transport(Box::new(std::io::Error::other(message)))
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
                acc_tasks.lock().push(conn.abort_handle());
            }
        });
        tasks.lock().push(accept.abort_handle());
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
        self.shared.requests()
    }

    /// The most recent request; panics when there is none.
    pub fn last_request(&self) -> RecordedRequest {
        self.requests()
            .pop()
            .expect("the fake server received no request")
    }

    /// Number of requests received (Go's `calls++` / `requestCount++`).
    pub fn hits(&self) -> usize {
        self.shared.hits()
    }

    /// Number of responses the client abandoned: a [`FakeResponse::never`] request whose
    /// connection closed, or a body dropped before its last chunk was handed out
    /// (drop-cancellation evidence). A fully sent response, including a fixed-length or empty
    /// one, is never a drop.
    pub fn drops(&self) -> usize {
        self.shared.drops()
    }

    /// Waits until at least `n` requests arrived (Go `<-started`); panics after
    /// [`WAIT_TIMEOUT`].
    pub async fn wait_for_hits(&self, n: usize) {
        self.shared
            .wait_until(&format!("{n} request(s)"), |s| s.hits() >= n)
            .await;
    }

    /// Waits until at least `n` responses were abandoned by the client; panics after
    /// [`WAIT_TIMEOUT`].
    pub async fn wait_for_drops(&self, n: usize) {
        self.shared
            .wait_until(&format!("{n} dropped response(s)"), |s| s.drops() >= n)
            .await;
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        for task in self.tasks.lock().drain(..) {
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
/// `ModelError::Transport(<message>)`; a [`BodyStep::Fail`] becomes a body item
/// `Err(ModelError::Transport(<message>))`. Clone the `Arc` into a provider's `http_client`.
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
        self.shared.requests()
    }

    /// The most recent request; panics when there is none.
    pub fn last_request(&self) -> RecordedRequest {
        self.requests()
            .pop()
            .expect("the scripted client got no request")
    }

    /// Number of requests sent.
    pub fn hits(&self) -> usize {
        self.shared.hits()
    }

    /// Number of responses abandoned before completion (see [`FakeServer::drops`]).
    pub fn drops(&self) -> usize {
        self.shared.drops()
    }

    /// The tokio time between request `i` and request `i + 1`; panics when either is missing.
    pub fn gap(&self, i: usize) -> Duration {
        let requests = self.shared.requests.lock();
        requests[i + 1].at - requests[i].at
    }

    /// Waits until at least `n` requests were sent.
    pub async fn wait_for_hits(&self, n: usize) {
        self.shared
            .wait_until(&format!("{n} request(s)"), |s| s.hits() >= n)
            .await;
    }

    /// Waits until at least `n` responses were abandoned.
    pub async fn wait_for_drops(&self, n: usize) {
        self.shared
            .wait_until(&format!("{n} dropped response(s)"), |s| s.drops() >= n)
            .await;
    }
}

#[async_trait::async_trait]
impl HttpClient for ScriptedClient {
    async fn send(
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
            .map_err(transport_error)?;
        let mut body = ScriptedBody::new(&resp, self.shared.clone());
        let stream: BodyStream = futures::stream::poll_fn(move |cx| {
            body.poll_chunk(cx)
                .map(|c| c.map(|r| r.map_err(transport_error)))
        })
        .boxed();
        Ok(build_response(&resp, stream))
    }
}

// ------------------------------------------------------------------------------------------ SSE

/// One SSE event as Go's tests write it: `event: <name>\ndata: <data>\n\n`.
pub fn sse_event(name: &str, data: &str) -> String {
    format!("event: {name}\ndata: {data}\n\n")
}

/// Go `strings.Join(lines, "\n")` over an SSE transcript written line by line.
pub fn sse_lines(lines: &[&str]) -> String {
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::http::collect_body;

    const MSG: &str = r#"{"id":"msg_1","text":"ok"}"#;

    #[tokio::test]
    async fn server_records_request_and_answers_json() {
        let srv = FakeServer::json(MSG);
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
        assert_eq!(resp.text().await.unwrap(), MSG);

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
    async fn server_streams_chunks_with_delays() {
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
    async fn server_fully_read_fixed_length_bodies_are_not_drops() {
        let srv = FakeServer::json(MSG);
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
    async fn server_fully_read_empty_body_is_not_a_drop() {
        let srv = FakeServer::always(FakeResponse::new(204));
        let resp = reqwest::get(srv.url()).await.unwrap();
        assert_eq!(resp.status(), 204);
        assert!(resp.bytes().await.unwrap().is_empty());
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!((srv.hits(), srv.drops()), (1, 0));
    }

    #[tokio::test]
    async fn server_observes_client_drop_mid_body() {
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
    async fn server_observes_drop_of_unanswered_request() {
        let srv = FakeServer::always(FakeResponse::never());
        let url = srv.url();
        let client = tokio::spawn(async move { reqwest::get(url).await });
        srv.wait_for_hits(1).await;
        assert_eq!(srv.drops(), 0);
        client.abort();
        srv.wait_for_drops(1).await;
    }

    #[tokio::test]
    async fn server_async_handler_waits_for_release() {
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
    async fn server_sequence_status_headers_and_reset() {
        let srv = FakeServer::sequence(vec![
            FakeResponse::json_status(529, r#"{"error":"overloaded"}"#)
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
    async fn server_body_failure_aborts_the_body() {
        // The pause lets hyper send the head and the first chunk before the body fails.
        let srv = FakeServer::always(
            FakeResponse::sse("data: a\n\n")
                .delay(Duration::from_millis(20))
                .fail("unexpected EOF"),
        );
        let resp = reqwest::get(srv.url()).await.unwrap();
        let mut body = resp.bytes_stream();
        let mut got = Vec::new();
        let mut failed = false;
        while let Some(chunk) = body.next().await {
            match chunk {
                Ok(c) => got.extend_from_slice(&c),
                Err(_) => failed = true,
            }
        }
        assert!(failed, "the body must fail");
        assert_eq!(&got[..], b"data: a\n\n");
        assert_eq!(srv.drops(), 0);
    }

    fn request(uri: &str, body: &'static [u8]) -> http::Request<Bytes> {
        http::Request::builder()
            .method("POST")
            .uri(uri)
            .header("x-api-key", "k")
            .body(Bytes::from_static(body))
            .unwrap()
    }

    #[tokio::test]
    async fn scripted_client_records_and_streams() {
        let client = ScriptedClient::always(
            FakeResponse::sse_chunks(["a", "b"])
                .delay(Duration::from_millis(5))
                .chunk("c"),
        );
        let resp = client
            .send(request(
                "https://api.anthropic.com/v1/messages",
                br#"{"stream":true}"#,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.headers()["content-type"], "text/event-stream");
        let body = collect_body(resp.into_body()).await.unwrap();
        assert_eq!(&body[..], b"abc");
        let rec = client.last_request();
        assert_eq!(rec.url, "https://api.anthropic.com/v1/messages");
        assert_eq!(rec.path, "/v1/messages");
        assert_eq!(rec.header("X-Api-Key"), "k");
        assert!(rec.is_stream());
        assert_eq!((client.hits(), client.drops()), (1, 0));
    }

    #[tokio::test]
    async fn scripted_client_reset_and_drop() {
        let client = ScriptedClient::sequence(vec![
            FakeResponse::reset("boom"),
            FakeResponse::sse("x").hang(),
        ]);
        match client.send(request("http://h/", b"")).await {
            Err(ModelError::Transport(e)) => assert_eq!(e.to_string(), "boom"),
            other => panic!("want transport error, got {:?}", other.map(|r| r.status())),
        }
        let resp = client.send(request("http://h/", b"")).await.unwrap();
        let mut body = resp.into_body();
        assert_eq!(&body.next().await.unwrap().unwrap()[..], b"x");
        drop(body);
        assert_eq!(client.drops(), 1);
        client.wait_for_drops(1).await;
    }

    #[tokio::test]
    async fn scripted_client_body_failure_ends_the_body() {
        let client = ScriptedClient::always(
            FakeResponse::json("{")
                .fail("unexpected EOF")
                .chunk("never sent"),
        );
        let resp = client.send(request("http://h/", b"")).await.unwrap();
        let mut body = resp.into_body();
        assert_eq!(&body.next().await.unwrap().unwrap()[..], b"{");
        match body.next().await {
            Some(Err(ModelError::Transport(e))) => assert_eq!(e.to_string(), "unexpected EOF"),
            other => panic!("want a transport body error, got {other:?}"),
        }
        assert!(body.next().await.is_none());
        drop(body);
        assert_eq!(client.drops(), 0, "a scripted failure is not a client drop");
    }

    #[tokio::test(start_paused = true)]
    async fn scripted_client_never_counts_a_drop_and_gap_measures_tokio_time() {
        let client =
            ScriptedClient::sequence(vec![FakeResponse::never(), FakeResponse::json("{}")]);
        let first = tokio::time::timeout(
            Duration::from_secs(3),
            client.send(request("http://h/", b"")),
        )
        .await;
        assert!(first.is_err(), "never answered");
        assert_eq!(client.drops(), 1);
        tokio::time::sleep(Duration::from_secs(2)).await;
        client.send(request("http://h/", b"")).await.unwrap();
        assert_eq!(client.gap(0), Duration::from_secs(5));
        assert_eq!(client.requests().len(), 2);
    }

    #[test]
    fn sse_builders_match_go_layout() {
        assert_eq!(
            sse_event("message_stop", r#"{"type":"message_stop"}"#),
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
        );
        assert_eq!(
            sse_lines(&["event: a", "data: {}", "", "event: b", "data: {}", ""]),
            "event: a\ndata: {}\n\nevent: b\ndata: {}\n"
        );
    }

    #[test]
    fn fake_response_builders() {
        let r = FakeResponse::json_status(429, "{}")
            .header("Retry-After", "1")
            .delay(Duration::from_secs(1))
            .chunk("x")
            .fail("eof")
            .status(503)
            .delay_headers(Duration::from_secs(2))
            .hang();
        assert_eq!(r.status, 503);
        assert_eq!(
            r.headers,
            vec![
                ("Content-Type".to_string(), "application/json".to_string()),
                ("Retry-After".to_string(), "1".to_string()),
            ]
        );
        assert_eq!(r.header_delay, Some(Duration::from_secs(2)));
        assert_eq!(r.end, BodyEnd::Hang);
        assert_eq!(r.outcome, Outcome::Respond);
        assert_eq!(&r.body_bytes()[..], b"{}x");
        assert!(matches!(r.steps[3], BodyStep::Fail(ref m) if m == "eof"));
        assert_eq!(FakeResponse::never().outcome, Outcome::Never);
        assert_eq!(
            FakeResponse::reset("x").outcome,
            Outcome::Reset("x".to_string())
        );
    }
}
