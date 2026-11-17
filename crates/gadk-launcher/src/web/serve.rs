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

//! The web launcher's HTTP server: a TCP listener on `host:port`, HTTP/1 only or HTTP/1 plus
//! prior-knowledge cleartext HTTP/2 (`-h2c`), per-connection timeouts, and graceful shutdown.
//!
//! Timeouts, per connection:
//! - `-read-timeout`: an HTTP/1 request's headers must arrive within it, counted from the accept
//!   (first request) or from the first byte after an idle period; its body must be read within
//!   it from the same start. Under `-h2c` the first request's count starts once the HTTP/2 preface
//!   peek has its 14 bytes (the peek has no deadline), and an HTTP/2 connection waits for its
//!   first stream under `-idle-timeout`. An HTTP/2 stream's body must be read within it from
//!   the stream's start. A late read fails with `i/o timeout` (headers: the connection is
//!   closed).
//! - `-write-timeout`: from the end of an HTTP/1 request's headers, every write of the response
//!   must happen before it, or the connection is aborted; an HTTP/2 stream is reset instead. A
//!   handler may move or clear its response's deadline through the request's
//!   [`WriteDeadline`] (as the SSE handler does).
//!   An HTTP/1 response's deadline lasts until it is flushed; hyper's answer to a request it
//!   cannot read gets `-write-timeout` from its first write.
//! - `-idle-timeout` (`-read-timeout` when zero): a keep-alive connection waiting for its next
//!   request is closed after it.
//!
//! A zero or negative timeout is none. An expired HTTP/2 idle connection is dropped without
//! GOAWAY; an HTTP/2 handler still running at the write deadline is cancelled; a handler that
//! does not read the body is not cancelled when the read deadline passes.

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use axum::body::Body;
use axum::extract::ConnectInfo;
use bytes::Bytes;
use gadk_platform::{GoDuration, go_format_duration};
use gadk_server::mux::WriteDeadline;
use gadk_server::rest::LocalAddr;
use http::{StatusCode, Version};
use hyper::body::{Frame, Incoming, SizeHint};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::time::{Instant, Sleep};
use tokio_util::sync::CancellationToken;

use super::router::WebRouter;
use crate::golog::Logger;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// The server settings from the web launcher's flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Options {
    /// The read timeout.
    pub(crate) read_timeout: Option<Duration>,
    /// The write timeout.
    pub(crate) write_timeout: Option<Duration>,
    /// The idle timeout, or the read timeout when that is zero.
    pub(crate) idle_timeout: Option<Duration>,
    /// Whether unencrypted HTTP/2 is accepted (`-h2c`).
    pub(crate) h2c: bool,
}

impl Options {
    /// The options for the read, write and idle timeouts and `-h2c`.
    pub(crate) fn new(read: GoDuration, write: GoDuration, idle: GoDuration, h2c: bool) -> Options {
        let idle = if idle.0 != 0 { idle } else { read };
        Options {
            read_timeout: positive(read),
            write_timeout: positive(write),
            idle_timeout: positive(idle),
            h2c,
        }
    }
}

/// A deadline duration: `None` unless positive.
fn positive(d: GoDuration) -> Option<Duration> {
    if d.0 > 0 { d.to_std() } else { None }
}

/// Listens on `addr`: the error text for the common bind failures is
/// `listen tcp <addr>: bind: address already in use` (or `bind: permission denied`), as an ADK
/// server prints it; any other failure carries Rust's I/O error text after `listen tcp <addr>: `.
pub(crate) async fn listen(addr: &str) -> Result<TcpListener, String> {
    TcpListener::bind(addr).await.map_err(|e| {
        let cause = match e.kind() {
            io::ErrorKind::AddrInUse => "bind: address already in use".to_owned(),
            io::ErrorKind::PermissionDenied => "bind: permission denied".to_owned(),
            _ => e.to_string(),
        };
        format!("listen tcp {addr}: {cause}")
    })
}

/// The server after its listener closed; [`Draining::shutdown`] waits for its connections.
///
/// Every connection task holds a receiver of the channel; shutdown signals them all and waits
/// until every receiver is gone (hyper-util's `GracefulShutdown` scheme, which has no impl for
/// hyper's HTTP/1 connection with upgrades).
pub(crate) struct Draining {
    shutdown: watch::Sender<bool>,
    conns: Conns,
}

/// The server's connections, for [`Draining::shutdown`]'s idle check.
type Conns = Arc<Mutex<Vec<Weak<ConnState>>>>;

impl Draining {
    /// Graceful shutdown bounded by `timeout`: idle connections close at once, active ones after
    /// their current response; `context deadline exceeded` when they have not all closed within
    /// `timeout` (they keep running). The idle check runs before the timeout check, so the
    /// shutdown succeeds when every connection left is idle, even with a zero `timeout`.
    pub(crate) async fn shutdown(self, timeout: GoDuration) -> Result<(), String> {
        let timeout = timeout.to_std().unwrap_or_default();
        let _ = self.shutdown.send(true);
        match tokio::time::timeout(timeout, self.shutdown.closed()).await {
            Ok(()) => Ok(()),
            Err(_) if self.quiescent() => Ok(()),
            Err(_) => Err("context deadline exceeded".to_owned()),
        }
    }

    /// Every open connection is idle (and closing).
    fn quiescent(&self) -> bool {
        let conns = self.conns.lock().unwrap_or_else(|e| e.into_inner());
        conns
            .iter()
            .filter_map(Weak::upgrade)
            .all(|c| c.quiescent())
    }
}

/// Serves one connection until it ends, shutting it down gracefully (finish the response in
/// flight, then close) when `shutdown` changes.
macro_rules! spawn_connection {
    ($conn:expr, $shutdown:expr) => {{
        let conn = $conn;
        let mut shutdown = $shutdown;
        tokio::spawn(async move {
            let mut conn = std::pin::pin!(conn);
            tokio::select! {
                _ = conn.as_mut() => return,
                _ = shutdown.changed() => {}
            }
            conn.as_mut().graceful_shutdown();
            let _ = conn.await;
            drop(shutdown);
        });
    }};
}

/// Serves `listener` until `cancel` fires: every accepted connection served by `router` with
/// `opts`. Accept errors are retried after a backoff (5ms doubling to 1s) and logged as
/// `http: Accept error: <err>; retrying in <delay>`. Every accept error is retried. Returns once
/// cancelled, with the listener closed.
pub(crate) async fn serve(
    listener: TcpListener,
    router: WebRouter,
    opts: Options,
    cancel: CancellationToken,
    log: Logger,
) -> Draining {
    let (shutdown, watching) = watch::channel(false);
    let conns = Conns::default();
    let builder = builder(opts);
    let mut delay = Duration::ZERO;
    loop {
        let accepted = tokio::select! {
            biased;
            () = cancel.cancelled() => break,
            accepted = listener.accept() => accepted,
        };
        let stream = match accepted {
            Ok((stream, _)) => {
                delay = Duration::ZERO;
                stream
            }
            Err(e) => {
                delay = if delay.is_zero() {
                    Duration::from_millis(5)
                } else {
                    (delay * 2).min(Duration::from_secs(1))
                };
                log.print(&format!(
                    "http: Accept error: {e}; retrying in {}",
                    go_format_duration(GoDuration::from_std(delay))
                ));
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => break,
                    () = tokio::time::sleep(delay) => continue,
                }
            }
        };
        // An unreadable socket address reads as the unspecified address, as for
        // `gadk_server::rest::LocalAddr` under axum.
        let local = stream
            .local_addr()
            .unwrap_or_else(|_| SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)));
        let state = ConnState::new(opts);
        {
            let mut conns = conns.lock().unwrap_or_else(|e| e.into_inner());
            conns.retain(|c| c.strong_count() > 0);
            conns.push(Arc::downgrade(&state));
        }
        let io = TokioIo::new(DeadlineIo::new(stream, Arc::clone(&state)));
        let service = connection_service(router.clone(), state, local);
        // A connection error (timeouts, resets, a malformed request answered by hyper) ends that
        // connection only and is not logged.
        match &builder {
            Builder::Http1(b) => spawn_connection!(
                b.serve_connection(io, service).with_upgrades(),
                watching.clone()
            ),
            Builder::Auto(b) => spawn_connection!(
                b.serve_connection_with_upgrades(io, service).into_owned(),
                watching.clone()
            ),
        }
    }
    drop(listener);
    drop(watching);
    Draining { shutdown, conns }
}

/// The connection builder: HTTP/1 only, or HTTP/1 and prior-knowledge HTTP/2 with `-h2c` (no
/// `Upgrade: h2c`). HTTP/1 header names are written in title case; the header-read timeout is the
/// connection's own read deadline.
enum Builder {
    Http1(hyper::server::conn::http1::Builder),
    Auto(auto::Builder<TokioExecutor>),
}

fn builder(opts: Options) -> Builder {
    if opts.h2c {
        let mut builder = auto::Builder::new(TokioExecutor::new());
        builder
            .http1()
            .title_case_headers(true)
            .header_read_timeout(None::<Duration>);
        Builder::Auto(builder)
    } else {
        let mut builder = hyper::server::conn::http1::Builder::new();
        builder
            .title_case_headers(true)
            .header_read_timeout(None::<Duration>);
        Builder::Http1(builder)
    }
}

/// The per-connection service: the router with the per-request deadlines and the accepted
/// address as `ConnectInfo<LocalAddr>` (read by the origin guard).
fn connection_service(
    router: WebRouter,
    state: Arc<ConnState>,
    local: SocketAddr,
) -> impl hyper::service::Service<
    http::Request<Incoming>,
    Response = http::Response<TrackedBody>,
    Error = BoxError,
    Future = impl Future<Output = Result<http::Response<TrackedBody>, BoxError>> + Send + 'static,
> + Send
+ 'static {
    hyper::service::service_fn(move |req: http::Request<Incoming>| {
        let router = router.clone();
        let state = Arc::clone(&state);
        async move {
            let close_after_reply = close_after_reply(&req);
            let started = state.begin_request(req.version());
            let (mut parts, body) = req.into_parts();
            parts.extensions.insert(ConnectInfo(LocalAddr(local)));
            parts.extensions.insert(started.write_deadline_hook());
            let body = Body::new(ReadDeadlineBody::new(body, started.body_deadline));
            let served = router.serve(http::Request::from_parts(parts, body));
            let mut stream_deadline = started.stream_deadline.map(StreamDeadline::new);
            let resp = match &mut stream_deadline {
                Some(deadline) => {
                    let mut served = std::pin::pin!(served);
                    std::future::poll_fn(|cx| match served.as_mut().poll(cx) {
                        Poll::Ready(resp) => Poll::Ready(Ok(resp)),
                        Poll::Pending if deadline.poll_expired(cx) => {
                            Poll::Ready(Err(BoxError::from(timed_out())))
                        }
                        Poll::Pending => Poll::Pending,
                    })
                    .await?
                }
                None => served.await,
            };
            if resp.status() == StatusCode::SWITCHING_PROTOCOLS {
                state.hijack();
            }
            let mut resp = resp;
            if let Some(connection) = close_after_reply {
                set_close_connection(resp.headers_mut(), connection);
            }
            Ok(resp.map(|body| TrackedBody::new(body, started.guard, stream_deadline)))
        }
    })
}

/// For an HTTP/1 request that asks to close the connection (HTTP/1.1 `Connection: close`,
/// HTTP/1.0 without `keep-alive`): the `Connection` value written in place of the handler's
/// (`close` for HTTP/1.1, none for HTTP/1.0). `None` when the connection stays open.
fn close_after_reply<B>(req: &http::Request<B>) -> Option<Option<http::HeaderValue>> {
    let has_token = |token: &str| {
        req.headers()
            .get_all(http::header::CONNECTION)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .any(|t| t.trim().eq_ignore_ascii_case(token))
    };
    match req.version() {
        Version::HTTP_11 if has_token("close") => {
            Some(Some(http::HeaderValue::from_static("close")))
        }
        Version::HTTP_10 if !has_token("keep-alive") => Some(None),
        _ => None,
    }
}

/// When closing after the reply: a handler's `Connection` header without a `close` token is
/// dropped and `connection` (if any) written instead.
fn set_close_connection(headers: &mut http::HeaderMap, connection: Option<http::HeaderValue>) {
    let has_close = headers
        .get_all(http::header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|t| t.trim().eq_ignore_ascii_case("close"));
    if has_close {
        return;
    }
    headers.remove(http::header::CONNECTION);
    if let Some(value) = connection {
        headers.insert(http::header::CONNECTION, value);
    }
}

/// The deadline-exceeded error (`i/o timeout`).
fn timed_out() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "i/o timeout")
}

/// One connection's deadlines, shared by its IO and its requests.
struct ConnState {
    opts: Options,
    /// When the connection was accepted.
    accepted: Instant,
    times: Mutex<Times>,
}

/// The prefix peeked, under the accept-time read deadline, to tell an `-h2c` connection's HTTP/2
/// preface from an HTTP/1 request.
const H2_PREFACE_PREFIX: &[u8] = b"PRI * HTTP/2.0";

struct Times {
    /// When the current HTTP/1 request started : the accept, or the first byte after
    /// an idle period.
    request_start: Instant,
    /// The deadline [`DeadlineIo`] applies to reads: headers (`ReadTimeout`) or idle
    /// (`IdleTimeout`); `None` while a request is being served (its body has its own).
    read_deadline: Option<Instant>,
    /// The HTTP/1 response's write deadline.
    write_deadline: Option<Instant>,
    /// Requests whose response is not finished.
    in_flight: usize,
    /// Waiting for the next request (keep-alive).
    idle: bool,
    /// A request arrived over HTTP/2.
    h2: bool,
    /// The connection was upgraded (101): no deadlines.
    hijacked: bool,
    /// No byte has been read yet.
    fresh: bool,
    /// `-h2c`: the first bytes, until [`H2_PREFACE_PREFIX`]'s length has arrived.
    preface: Option<Vec<u8>>,
    /// The task waiting on a read, woken when the read deadline changes.
    read_waker: Option<Waker>,
}

/// What a request was given by [`ConnState::begin_request`].
struct Started {
    guard: RequestGuard,
    /// The request body's read deadline.
    body_deadline: Option<Instant>,
    /// HTTP/2: the stream's write deadline, which the handler may move.
    stream_deadline: Option<Arc<Mutex<Option<Instant>>>>,
    conn: Arc<ConnState>,
}

impl Started {
    /// The request's [`WriteDeadline`]: the HTTP/1
    /// connection's write deadline, or the HTTP/2 stream's.
    fn write_deadline_hook(&self) -> WriteDeadline {
        match &self.stream_deadline {
            Some(cell) => {
                let cell = Arc::clone(cell);
                WriteDeadline::new(move |d| {
                    *cell.lock().unwrap_or_else(|e| e.into_inner()) = d;
                })
            }
            None => {
                let conn = Arc::clone(&self.conn);
                WriteDeadline::new(move |d| conn.times().write_deadline = d)
            }
        }
    }
}

impl ConnState {
    fn new(opts: Options) -> Arc<ConnState> {
        let now = Instant::now();
        Arc::new(ConnState {
            opts,
            accepted: now,
            times: Mutex::new(Times {
                request_start: now,
                // The header read deadline starts at accept, before an `-h2c` preface peek too.
                read_deadline: opts.read_timeout.map(|d| now + d),
                write_deadline: None,
                in_flight: 0,
                idle: false,
                h2: false,
                hijacked: false,
                fresh: true,
                preface: opts.h2c.then(Vec::new),
                read_waker: None,
            }),
        })
    }

    /// The test for one connection: idle, upgraded (no longer
    /// tracked), or new for more than 5 seconds. An HTTP/2 connection is never idle to it.
    fn quiescent(&self) -> bool {
        let t = self.times();
        t.hijacked
            || (!t.h2 && t.idle && t.in_flight == 0)
            || (t.fresh && self.accepted.elapsed() > Duration::from_secs(5))
    }

    fn times(&self) -> std::sync::MutexGuard<'_, Times> {
        self.times.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn read_deadline(&self) -> Option<Instant> {
        let t = self.times();
        if t.hijacked { None } else { t.read_deadline }
    }

    /// `bytes` arrived: on an idle HTTP/1 connection a new request starts (the read
    /// deadline is reset to the read timeout from now). The previous response's write deadline is
    /// left to [`ConnState::on_flushed`]: its tail may still be buffered.
    ///
    /// Under `-h2c` the first bytes are the preface peek, under the accept-time read timeout:
    /// an HTTP/2 preface starts HTTP/2's idle timer (the idle timeout), anything else an HTTP/1
    /// request whose read timeout counts again from the end of the peek.
    fn on_bytes_read(&self, bytes: &[u8]) {
        let mut t = self.times();
        t.fresh = false;
        if let Some(mut seen) = t.preface.take() {
            let want = H2_PREFACE_PREFIX.len() - seen.len();
            seen.extend_from_slice(&bytes[..want.min(bytes.len())]);
            if seen.len() < H2_PREFACE_PREFIX.len() {
                t.preface = Some(seen);
                return;
            }
            let now = Instant::now();
            t.request_start = now;
            if seen == H2_PREFACE_PREFIX {
                t.h2 = true;
                t.read_deadline = self.opts.idle_timeout.map(|d| now + d);
            } else {
                t.read_deadline = self.opts.read_timeout.map(|d| now + d);
            }
            drop(t);
            // The read deadline moved: a pending read re-arms its timer.
            self.wake_reader();
            return;
        }
        if t.idle && !t.h2 {
            let now = Instant::now();
            t.idle = false;
            t.request_start = now;
            t.read_deadline = self.opts.read_timeout.map(|d| now + d);
        }
    }

    /// Everything written so far was flushed: on an idle HTTP/1 connection the last response
    /// is complete and its write deadline no longer applies (the next one is set when the next
    /// request's headers have been read).
    fn on_flushed(&self) {
        let mut t = self.times();
        if t.idle && t.in_flight == 0 && !t.h2 {
            t.write_deadline = None;
        }
    }

    /// The HTTP/1 write deadline, set first when hyper writes with no request in flight: its
    /// answer to a request it could not read.
    fn write_deadline_for_write(&self) -> Option<Instant> {
        let mut t = self.times();
        if t.hijacked {
            return None;
        }
        if t.in_flight == 0 && !t.h2 && t.write_deadline.is_none() {
            t.write_deadline = self.opts.write_timeout.map(|d| Instant::now() + d);
        }
        t.write_deadline
    }

    fn wake_reader(&self) {
        let waker = self.times().read_waker.take();
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    /// A request's headers were read.
    fn begin_request(self: &Arc<Self>, version: Version) -> Started {
        let now = Instant::now();
        let mut t = self.times();
        t.in_flight += 1;
        t.idle = false;
        t.read_deadline = None;
        let h2 = version == Version::HTTP_2;
        t.h2 |= h2;
        let start = if h2 { now } else { t.request_start };
        let body_deadline = self.opts.read_timeout.map(|d| start + d);
        let write_deadline = self.opts.write_timeout.map(|d| now + d);
        if !h2 {
            t.write_deadline = write_deadline;
        }
        Started {
            guard: RequestGuard(Arc::clone(self)),
            body_deadline,
            stream_deadline: h2.then(|| Arc::new(Mutex::new(write_deadline))),
            conn: Arc::clone(self),
        }
    }

    /// A response finished: with nothing else in flight the connection is idle (the read
    /// deadline is the idle timeout from now).
    fn end_request(&self) {
        let waker = {
            let mut t = self.times();
            t.in_flight = t.in_flight.saturating_sub(1);
            if t.in_flight > 0 {
                return;
            }
            let now = Instant::now();
            t.idle = true;
            t.request_start = now;
            t.read_deadline = self.opts.idle_timeout.map(|d| now + d);
            t.read_waker.take()
        };
        // A pending read registered no timer while the request was in flight.
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    /// Remembers the task waiting on a read.
    fn register_read_waker(&self, waker: &Waker) {
        let mut t = self.times();
        match &t.read_waker {
            Some(w) if w.will_wake(waker) => {}
            _ => t.read_waker = Some(waker.clone()),
        }
    }

    fn hijack(&self) {
        self.times().hijacked = true;
    }
}

/// Ends its request when dropped (the response body finished or was dropped).
struct RequestGuard(Arc<ConnState>);

impl Drop for RequestGuard {
    fn drop(&mut self) {
        self.0.end_request();
    }
}

/// Polls `timer` towards `deadline`; whether it has passed.
fn deadline_passed(timer: &mut Pin<Box<Sleep>>, deadline: Instant, cx: &mut Context<'_>) -> bool {
    if timer.deadline() != deadline {
        timer.as_mut().reset(deadline);
    }
    timer.as_mut().poll(cx).is_ready()
}

/// The TCP stream with the connection's read and write deadlines.
struct DeadlineIo {
    inner: TcpStream,
    state: Arc<ConnState>,
    read_timer: Pin<Box<Sleep>>,
    write_timer: Pin<Box<Sleep>>,
}

impl DeadlineIo {
    fn new(inner: TcpStream, state: Arc<ConnState>) -> DeadlineIo {
        let now = Instant::now();
        DeadlineIo {
            inner,
            state,
            read_timer: Box::pin(tokio::time::sleep_until(now)),
            write_timer: Box::pin(tokio::time::sleep_until(now)),
        }
    }

    fn poll_write_with<T>(
        &mut self,
        cx: &mut Context<'_>,
        op: impl FnOnce(Pin<&mut TcpStream>, &mut Context<'_>) -> Poll<io::Result<T>>,
    ) -> Poll<io::Result<T>> {
        let deadline = self.state.write_deadline_for_write();
        if let Some(deadline) = deadline
            && Instant::now() >= deadline
        {
            return Poll::Ready(Err(timed_out()));
        }
        match op(Pin::new(&mut self.inner), cx) {
            // The deadline is read again: the handler may have moved it meanwhile.
            Poll::Pending => match self.state.write_deadline_for_write() {
                Some(deadline) if deadline_passed(&mut self.write_timer, deadline, cx) => {
                    Poll::Ready(Err(timed_out()))
                }
                _ => Poll::Pending,
            },
            ready => ready,
        }
    }
}

impl AsyncRead for DeadlineIo {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if let Some(deadline) = this.state.read_deadline()
            && Instant::now() >= deadline
        {
            return Poll::Ready(Err(timed_out()));
        }
        let before = buf.filled().len();
        match Pin::new(&mut this.inner).poll_read(cx, buf) {
            Poll::Ready(Ok(())) => {
                if buf.filled().len() > before {
                    this.state.on_bytes_read(&buf.filled()[before..]);
                }
                Poll::Ready(Ok(()))
            }
            Poll::Pending => {
                this.state.register_read_waker(cx.waker());
                match this.state.read_deadline() {
                    Some(deadline) if deadline_passed(&mut this.read_timer, deadline, cx) => {
                        Poll::Ready(Err(timed_out()))
                    }
                    _ => Poll::Pending,
                }
            }
            err => err,
        }
    }
}

impl AsyncWrite for DeadlineIo {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.get_mut()
            .poll_write_with(cx, |io, cx| io.poll_write(cx, buf))
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        self.get_mut()
            .poll_write_with(cx, |io, cx| io.poll_write_vectored(cx, bufs))
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    /// No deadline: a `TcpStream` flush writes nothing (hyper's buffered bytes go through
    /// `poll_write*`), and hyper flushes on every dispatch loop, idle connections included.
    /// hyper flushes its socket only once its own buffer is written, so a completed flush on
    /// an idle connection ends the last response's write deadline.
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let flushed = Pin::new(&mut this.inner).poll_flush(cx);
        if let Poll::Ready(Ok(())) = flushed {
            this.state.on_flushed();
        }
        flushed
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

/// A request body that fails with `i/o timeout` when read after its deadline.
struct ReadDeadlineBody {
    inner: Incoming,
    deadline: Option<(Instant, Pin<Box<Sleep>>)>,
}

impl ReadDeadlineBody {
    fn new(inner: Incoming, deadline: Option<Instant>) -> ReadDeadlineBody {
        ReadDeadlineBody {
            inner,
            deadline: deadline.map(|d| (d, Box::pin(tokio::time::sleep_until(d)))),
        }
    }
}

impl hyper::body::Body for ReadDeadlineBody {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        let this = self.get_mut();
        if let Some((deadline, _)) = &this.deadline
            && Instant::now() >= *deadline
            && !this.inner.is_end_stream()
        {
            return Poll::Ready(Some(Err(timed_out().into())));
        }
        match Pin::new(&mut this.inner).poll_frame(cx) {
            Poll::Pending => {
                let expired = this
                    .deadline
                    .as_mut()
                    .is_some_and(|(_, timer)| timer.as_mut().poll(cx).is_ready());
                if expired {
                    Poll::Ready(Some(Err(timed_out().into())))
                } else {
                    Poll::Pending
                }
            }
            Poll::Ready(frame) => Poll::Ready(frame.map(|f| f.map_err(BoxError::from))),
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

/// An HTTP/2 stream's write deadline, which the handler may move through its
/// [`WriteDeadline`].
struct StreamDeadline {
    cell: Arc<Mutex<Option<Instant>>>,
    timer: Pin<Box<Sleep>>,
}

impl StreamDeadline {
    fn new(cell: Arc<Mutex<Option<Instant>>>) -> StreamDeadline {
        StreamDeadline {
            cell,
            timer: Box::pin(tokio::time::sleep_until(Instant::now())),
        }
    }

    /// Whether the deadline has passed, registering for it otherwise.
    fn poll_expired(&mut self, cx: &mut Context<'_>) -> bool {
        let deadline = *self.cell.lock().unwrap_or_else(|e| e.into_inner());
        deadline.is_some_and(|d| Instant::now() >= d || deadline_passed(&mut self.timer, d, cx))
    }
}

/// A response body that ends its request when finished or dropped and, for an HTTP/2 stream,
/// fails at the stream's write deadline (resetting the stream).
pub(crate) struct TrackedBody {
    inner: Body,
    guard: Option<RequestGuard>,
    deadline: Option<StreamDeadline>,
}

impl TrackedBody {
    fn new(inner: Body, guard: RequestGuard, deadline: Option<StreamDeadline>) -> TrackedBody {
        TrackedBody {
            inner,
            guard: Some(guard),
            deadline,
        }
    }
}

impl hyper::body::Body for TrackedBody {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        let this = self.get_mut();
        if let Some(deadline) = &mut this.deadline
            && deadline.poll_expired(cx)
        {
            return Poll::Ready(Some(Err(timed_out().into())));
        }
        match Pin::new(&mut this.inner).poll_frame(cx) {
            Poll::Ready(None) => {
                this.guard.take();
                Poll::Ready(None)
            }
            Poll::Ready(Some(frame)) => Poll::Ready(Some(frame.map_err(BoxError::from))),
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use gadk_server::mux::{Request, Response, handler};
    use http::Method;
    use pretty_assertions::assert_eq;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    fn ms(n: i64) -> GoDuration {
        GoDuration(n * 1_000_000)
    }

    /// Go's `closeAfterReply` header rule: a handler's `Connection: keep-alive` gives way to
    /// `close` (HTTP/1.1) or to nothing (HTTP/1.0); a `close` token the handler set stays.
    #[test]
    fn closing_after_reply_replaces_the_handler_connection_header() {
        let req = |version: Version, connection: Option<&str>| {
            let mut b = http::Request::builder().version(version);
            if let Some(c) = connection {
                b = b.header(http::header::CONNECTION, c);
            }
            b.body(()).unwrap()
        };
        let apply = |req: &http::Request<()>, handler: Option<&str>| {
            let mut headers = http::HeaderMap::new();
            if let Some(h) = handler {
                headers.insert(http::header::CONNECTION, h.parse().unwrap());
            }
            if let Some(connection) = close_after_reply(req) {
                set_close_connection(&mut headers, connection);
            }
            headers
                .get(http::header::CONNECTION)
                .map(|v| v.to_str().unwrap().to_owned())
        };
        let close11 = req(Version::HTTP_11, Some("Close"));
        assert_eq!(
            apply(&close11, Some("keep-alive")).as_deref(),
            Some("close")
        );
        assert_eq!(apply(&close11, None).as_deref(), Some("close"));
        assert_eq!(
            apply(&close11, Some("upgrade, close")).as_deref(),
            Some("upgrade, close")
        );
        let open11 = req(Version::HTTP_11, None);
        assert_eq!(
            apply(&open11, Some("keep-alive")).as_deref(),
            Some("keep-alive")
        );
        let plain10 = req(Version::HTTP_10, None);
        assert_eq!(apply(&plain10, Some("keep-alive")), None);
        let keep10 = req(Version::HTTP_10, Some("keep-alive"));
        assert_eq!(
            apply(&keep10, Some("keep-alive")).as_deref(),
            Some("keep-alive")
        );
    }

    fn opts(read: i64, write: i64, idle: i64) -> Options {
        Options::new(ms(read), ms(write), ms(idle), false)
    }

    /// Serves `router` on an ephemeral port; the address and the token that stops it.
    async fn start(router: WebRouter, opts: Options) -> (SocketAddr, CancellationToken) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let cancel = CancellationToken::new();
        let (log, _) = crate::golog::capture();
        tokio::spawn(serve(listener, router, opts, cancel.clone(), log));
        (addr, cancel)
    }

    fn router() -> WebRouter {
        let mut r = WebRouter::new(true);
        r.handle(
            &[Method::GET],
            "/ok",
            handler(|_req: Request| async { Response::new(Body::from("ok")) }),
        );
        r.handle(
            &[Method::POST],
            "/read",
            handler(|req: Request| async move {
                match axum::body::to_bytes(req.into_body(), usize::MAX).await {
                    Ok(b) => Response::new(Body::from(b)),
                    Err(e) => Response::new(Body::from(format!("read failed: {e}"))),
                }
            }),
        );
        r.handle(
            &[Method::GET],
            "/slow",
            handler(|_req: Request| async {
                tokio::time::sleep(Duration::from_millis(300)).await;
                Response::new(Body::from("late"))
            }),
        );
        // An SSE-like handler: moves the write deadline as `?deadline=` says (`none` clears
        // it), then streams one chunk at once and one 300ms later.
        r.handle(
            &[Method::GET],
            "/stream",
            handler(|req: Request| async move {
                let deadline = match req.uri().query() {
                    Some("deadline=none") => None,
                    _ => Some(Instant::now() + Duration::from_secs(1)),
                };
                if let Some(hook) = req.extensions().get::<WriteDeadline>() {
                    hook.set(deadline);
                }
                let chunks = futures::stream::unfold(0, |n| async move {
                    match n {
                        0 => Some((Ok::<_, io::Error>(Bytes::from("a")), 1)),
                        1 => {
                            tokio::time::sleep(Duration::from_millis(300)).await;
                            Some((Ok(Bytes::from("b")), 2))
                        }
                        _ => None,
                    }
                });
                Response::new(Body::from_stream(chunks))
            }),
        );
        r.handle(
            &[Method::GET],
            "/addr",
            handler(|req: Request| async move {
                let addr = req
                    .extensions()
                    .get::<ConnectInfo<LocalAddr>>()
                    .map(|ConnectInfo(LocalAddr(a))| a.to_string())
                    .unwrap_or_default();
                Response::new(Body::from(addr))
            }),
        );
        r
    }

    /// Reads one response's head and `Content-Length` body.
    async fn read_response(conn: &mut TcpStream) -> io::Result<String> {
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        while !buf.ends_with(b"\r\n\r\n") {
            if conn.read(&mut byte).await? == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            buf.push(byte[0]);
        }
        let head = String::from_utf8(buf).unwrap();
        let len: usize = head
            .lines()
            .find_map(|l| l.strip_prefix("Content-Length: "))
            .map_or(0, |v| v.trim().parse().unwrap());
        let mut body = vec![0u8; len];
        conn.read_exact(&mut body).await?;
        Ok(format!("{head}{}", String::from_utf8(body).unwrap()))
    }

    #[tokio::test]
    async fn serves_http1_with_go_header_case_and_local_addr() {
        let (addr, cancel) = start(router(), opts(1000, 1000, 1000)).await;
        let mut conn = TcpStream::connect(addr).await.unwrap();
        conn.write_all(b"GET /addr HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        let resp = read_response(&mut conn).await.unwrap();
        assert!(resp.starts_with("HTTP/1.1 200 OK\r\n"), "{resp}");
        assert!(resp.contains("\r\nContent-Length: 15\r\n"), "{resp}");
        assert!(resp.ends_with(&addr.to_string()), "{resp}");
        cancel.cancel();
    }

    #[tokio::test]
    async fn idle_connection_closes_after_the_idle_timeout() {
        let (addr, cancel) = start(router(), opts(2000, 2000, 100)).await;
        let mut conn = TcpStream::connect(addr).await.unwrap();
        conn.write_all(b"GET /ok HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        read_response(&mut conn).await.unwrap();
        // Keep-alive: a second request within the idle timeout is served.
        tokio::time::sleep(Duration::from_millis(30)).await;
        conn.write_all(b"GET /ok HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        read_response(&mut conn).await.unwrap();
        // Then the connection is closed once idle for longer than 100ms.
        let started = Instant::now();
        let mut rest = Vec::new();
        conn.read_to_end(&mut rest).await.unwrap();
        assert!(rest.is_empty());
        assert!(started.elapsed() < Duration::from_millis(1500));
        cancel.cancel();
    }

    /// Go sets the write deadline only after `readRequest`: an earlier request's deadline, past
    /// while the connection was idle, must not abort the next request's split head.
    #[tokio::test]
    async fn a_stale_write_deadline_does_not_abort_the_next_request() {
        let (addr, cancel) = start(router(), opts(5000, 100, 5000)).await;
        let mut conn = TcpStream::connect(addr).await.unwrap();
        conn.write_all(b"GET /ok HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        read_response(&mut conn).await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        conn.write_all(b"GET /ok HTTP/1.1\r\n").await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        conn.write_all(b"Host: x\r\n\r\n").await.unwrap();
        let resp = read_response(&mut conn).await.unwrap();
        assert!(resp.starts_with("HTTP/1.1 200 OK\r\n"), "{resp}");
        cancel.cancel();
    }

    /// Go flushes a response under its write deadline before it reads the next request: a
    /// client that pipelines an incomplete head and stops reading does not hold the response's
    /// tail unbounded.
    #[tokio::test]
    async fn a_pipelined_head_does_not_lift_the_write_deadline_of_the_unflushed_response() {
        const BIG: usize = 16 << 20;
        let mut r = router();
        r.handle(
            &[Method::GET],
            "/big",
            handler(|_req: Request| async { Response::new(Body::from(vec![b'x'; BIG])) }),
        );
        let (addr, cancel) = start(r, opts(0, 200, 0)).await;
        let mut conn = TcpStream::connect(addr).await.unwrap();
        conn.write_all(b"GET /big HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        conn.write_all(b"GET /ok HTTP/1.1\r\n").await.unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        // The write deadline passed with the response unwritten: the connection was aborted.
        let mut got = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(5), conn.read_to_end(&mut got)).await;
        assert!(
            got.len() < BIG,
            "the whole response was written ({} bytes)",
            got.len()
        );
        cancel.cancel();
    }

    #[tokio::test]
    async fn headers_must_arrive_within_the_read_timeout() {
        let (addr, cancel) = start(router(), opts(100, 2000, 2000)).await;
        let mut conn = TcpStream::connect(addr).await.unwrap();
        conn.write_all(b"GET /ok HTTP/1.1\r\n").await.unwrap();
        let mut rest = Vec::new();
        let started = Instant::now();
        // The connection is closed without a response.
        let _ = conn.read_to_end(&mut rest).await;
        assert!(rest.is_empty(), "{}", String::from_utf8_lossy(&rest));
        assert!(started.elapsed() < Duration::from_millis(1500));
        cancel.cancel();
    }

    #[tokio::test]
    async fn a_body_read_after_the_read_timeout_fails() {
        let (addr, cancel) = start(router(), opts(150, 2000, 2000)).await;
        let mut conn = TcpStream::connect(addr).await.unwrap();
        conn.write_all(b"POST /read HTTP/1.1\r\nHost: x\r\nContent-Length: 4\r\n\r\nab")
            .await
            .unwrap();
        let resp = read_response(&mut conn).await.unwrap();
        assert!(resp.ends_with("read failed: i/o timeout"), "{resp}");
        cancel.cancel();
    }

    #[tokio::test]
    async fn a_response_after_the_write_timeout_aborts_the_connection() {
        let (addr, cancel) = start(router(), opts(2000, 100, 2000)).await;
        let mut conn = TcpStream::connect(addr).await.unwrap();
        conn.write_all(b"GET /slow HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        let mut rest = Vec::new();
        let _ = conn.read_to_end(&mut rest).await;
        assert!(rest.is_empty(), "{}", String::from_utf8_lossy(&rest));
        cancel.cancel();
    }

    #[tokio::test]
    async fn shutdown_closes_idle_connections_and_waits_for_active_ones() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let cancel = CancellationToken::new();
        let (log, _) = crate::golog::capture();
        let server = tokio::spawn(serve(
            listener,
            router(),
            opts(0, 0, 0),
            cancel.clone(),
            log,
        ));
        let mut active = TcpStream::connect(addr).await.unwrap();
        active
            .write_all(b"GET /slow HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();
        let draining = server.await.unwrap();
        // The listener is closed.
        assert!(TcpStream::connect(addr).await.is_err());
        let drained = tokio::spawn(draining.shutdown(GoDuration::from_secs(5)));
        let resp = read_response(&mut active).await.unwrap();
        assert!(resp.ends_with("late"), "{resp}");
        assert_eq!(drained.await.unwrap(), Ok(()));
    }

    #[tokio::test]
    async fn shutdown_past_its_timeout_is_context_deadline_exceeded() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let cancel = CancellationToken::new();
        let (log, _) = crate::golog::capture();
        let server = tokio::spawn(serve(
            listener,
            router(),
            opts(0, 0, 0),
            cancel.clone(),
            log,
        ));
        let mut active = TcpStream::connect(addr).await.unwrap();
        active
            .write_all(b"GET /slow HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();
        let draining = server.await.unwrap();
        assert_eq!(
            draining.shutdown(ms(10)).await,
            Err("context deadline exceeded".to_owned())
        );
    }

    /// Go `RunSSEHandler`'s `rc.SetWriteDeadline`: a handler's deadline overrides the
    /// server's write timeout, over HTTP/1 and HTTP/2, and `None` clears it.
    #[tokio::test]
    async fn a_handler_write_deadline_overrides_the_write_timeout() {
        for h2c in [false, true] {
            let opts = Options::new(ms(2000), ms(100), ms(2000), h2c);
            let (addr, cancel) = start(router(), opts).await;
            let mut client = reqwest::Client::builder();
            if h2c {
                client = client.http2_prior_knowledge();
            }
            let client = client.build().unwrap();
            for query in ["deadline=1s", "deadline=none"] {
                let resp = client
                    .get(format!("http://{addr}/stream?{query}"))
                    .send()
                    .await
                    .unwrap();
                let body = resp.text().await;
                assert_eq!(body.ok().as_deref(), Some("ab"), "h2c={h2c} {query}");
            }
            cancel.cancel();
        }
    }

    /// The control for the test above: a stream past `-write-timeout` that does not move its
    /// deadline is cut off.
    #[tokio::test]
    async fn a_stream_past_the_write_timeout_is_cut_off() {
        let mut r = WebRouter::new(true);
        r.handle(
            &[Method::GET],
            "/stream",
            handler(|_req: Request| async move {
                let chunks = futures::stream::unfold(0, |n| async move {
                    match n {
                        0 => Some((Ok::<_, io::Error>(Bytes::from("a")), 1)),
                        1 => {
                            tokio::time::sleep(Duration::from_millis(300)).await;
                            Some((Ok(Bytes::from("b")), 2))
                        }
                        _ => None,
                    }
                });
                Response::new(Body::from_stream(chunks))
            }),
        );
        for h2c in [false, true] {
            let (addr, cancel) =
                start(r.clone(), Options::new(ms(2000), ms(100), ms(2000), h2c)).await;
            let mut client = reqwest::Client::builder();
            if h2c {
                client = client.http2_prior_knowledge();
            }
            let resp = client
                .build()
                .unwrap()
                .get(format!("http://{addr}/stream"))
                .send()
                .await;
            let body = match resp {
                Ok(resp) => resp.text().await.ok(),
                Err(_) => None,
            };
            assert_ne!(body.as_deref(), Some("ab"), "h2c={h2c}");
            cancel.cancel();
        }
    }

    /// Go sets `readHeaderTimeout()` (here `-read-timeout`) at accept, before it peeks an
    /// `-h2c` connection's preface (server.go `conn.serve`): a client silent past it is closed.
    #[tokio::test]
    async fn h2c_preface_peek_has_the_read_timeout_from_accept() {
        let (addr, cancel) = start(router(), Options::new(ms(100), ms(2000), ms(2000), true)).await;
        let mut conn = TcpStream::connect(addr).await.unwrap();
        let mut rest = Vec::new();
        let closed =
            tokio::time::timeout(Duration::from_millis(1500), conn.read_to_end(&mut rest)).await;
        assert!(
            closed.is_ok(),
            "a silent h2c client still holds the connection"
        );
        assert!(rest.is_empty(), "{}", String::from_utf8_lossy(&rest));
        // Once the peek is over, an HTTP/1 request's headers have `-read-timeout` again.
        let mut conn = TcpStream::connect(addr).await.unwrap();
        conn.write_all(b"GET /ok HTTP/1.1\r\nHo").await.unwrap();
        let mut rest = Vec::new();
        let started = Instant::now();
        let _ = conn.read_to_end(&mut rest).await;
        assert!(rest.is_empty(), "{}", String::from_utf8_lossy(&rest));
        assert!(started.elapsed() < Duration::from_millis(1500));
        cancel.cancel();
    }

    /// Go's `Shutdown` closes idle connections before it checks its context: with a zero
    /// timeout and only an idle keep-alive connection it succeeds.
    #[tokio::test]
    async fn shutdown_with_a_zero_timeout_closes_idle_connections() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let cancel = CancellationToken::new();
        let (log, _) = crate::golog::capture();
        let server = tokio::spawn(serve(
            listener,
            router(),
            opts(0, 0, 0),
            cancel.clone(),
            log,
        ));
        // Enough idle connections that they cannot all have closed before a zero timeout.
        let mut idle = Vec::new();
        for _ in 0..64 {
            let mut conn = TcpStream::connect(addr).await.unwrap();
            conn.write_all(b"GET /ok HTTP/1.1\r\nHost: x\r\n\r\n")
                .await
                .unwrap();
            read_response(&mut conn).await.unwrap();
            idle.push(conn);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        cancel.cancel();
        let draining = server.await.unwrap();
        assert_eq!(draining.shutdown(GoDuration(0)).await, Ok(()));
        // And the idle connections are closed.
        for mut conn in idle {
            let mut rest = Vec::new();
            let closed =
                tokio::time::timeout(Duration::from_secs(2), conn.read_to_end(&mut rest)).await;
            assert!(closed.is_ok() && rest.is_empty());
        }
    }

    #[tokio::test]
    async fn listen_errors_are_go_text() {
        let taken = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = taken.local_addr().unwrap().to_string();
        let err = listen(&addr).await.unwrap_err();
        assert_eq!(
            err,
            format!("listen tcp {addr}: bind: address already in use")
        );
    }

    #[test]
    fn options_follow_go_idle_fallback_and_zero_rules() {
        let s = GoDuration::from_secs;
        assert_eq!(
            Options::new(s(15), s(15), s(60), true),
            Options {
                read_timeout: Some(Duration::from_secs(15)),
                write_timeout: Some(Duration::from_secs(15)),
                idle_timeout: Some(Duration::from_secs(60)),
                h2c: true,
            }
        );
        let o = Options::new(s(15), GoDuration(0), GoDuration(0), false);
        assert_eq!(o.idle_timeout, Some(Duration::from_secs(15)));
        assert_eq!(o.write_timeout, None);
        let o = Options::new(s(15), s(1), GoDuration(-1), false);
        assert_eq!(o.idle_timeout, None);
    }
}
