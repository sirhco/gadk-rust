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

//! The web launcher's HTTP server (Go `http.Server` as `buildHTTPServer` configures it): a TCP
//! listener on `host:port`, HTTP/1 only or HTTP/1 plus prior-knowledge cleartext HTTP/2 (`-h2c`),
//! Go's timeouts, and graceful shutdown.
//!
//! Timeouts (spec R8), per connection as Go's `net/http` sets its deadlines:
//! - `-read-timeout`: an HTTP/1 request's headers must arrive within it, counted from the accept
//!   (first request) or from the first byte after an idle period; its body must be read within
//!   it from the same start. An HTTP/2 stream's body must be read within it from the stream's
//!   start. A late read fails with `i/o timeout` (headers: the connection is closed).
//! - `-write-timeout`: from the end of an HTTP/1 request's headers, every write of the response
//!   must happen before it, or the connection is aborted; an HTTP/2 stream is reset instead.
//! - `-idle-timeout` (`-read-timeout` when zero): a keep-alive connection waiting for its next
//!   request is closed after it.
//!
//! A zero or negative timeout is none. Divergences from `net/http` (ledger 6): an expired
//! HTTP/2 idle connection is dropped without GOAWAY; an HTTP/2 handler still running at the
//! write deadline is cancelled (Go lets it finish and resets the stream); a handler that does
//! not read the body is not cancelled when the read deadline passes (Go cancels its context).

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use axum::body::Body;
use axum::extract::ConnectInfo;
use bytes::Bytes;
use gadk_platform::{GoDuration, go_format_duration};
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

/// The server settings from the web launcher's flags (Go `http.Server` fields).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Options {
    /// Go `ReadTimeout`.
    pub(crate) read_timeout: Option<Duration>,
    /// Go `WriteTimeout`.
    pub(crate) write_timeout: Option<Duration>,
    /// Go `srv.idleTimeout()`: `IdleTimeout`, or `ReadTimeout` when that is zero.
    pub(crate) idle_timeout: Option<Duration>,
    /// Go `Protocols` with unencrypted HTTP/2 (`-h2c`).
    pub(crate) h2c: bool,
}

impl Options {
    /// The options for Go's `ReadTimeout`, `WriteTimeout`, `IdleTimeout` and `-h2c`.
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

/// A Go deadline duration: `None` unless positive.
fn positive(d: GoDuration) -> Option<Duration> {
    if d.0 > 0 { d.to_std() } else { None }
}

/// Go `net.Listen("tcp", addr)` (`ListenAndServe`): the error is Go's text for the common bind
/// failures (`listen tcp <addr>: bind: address already in use`).
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
pub(crate) struct Draining(watch::Sender<bool>);

impl Draining {
    /// Go `srv.Shutdown(ctx)` with a `timeout` context: idle connections close at once, active
    /// ones after their current response; `context deadline exceeded` when they have not all
    /// closed within `timeout` (they keep running, as in Go).
    pub(crate) async fn shutdown(self, timeout: GoDuration) -> Result<(), String> {
        let timeout = timeout.to_std().unwrap_or_default();
        let _ = self.0.send(true);
        tokio::time::timeout(timeout, self.0.closed())
            .await
            .map_err(|_| "context deadline exceeded".to_owned())
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

/// Go `srv.Serve(listener)` until `cancel` fires: every accepted connection served by `router`
/// with `opts`. Accept errors are retried after Go's backoff (5ms doubling to 1s), logged as Go
/// logs them. Returns once cancelled, with the listener closed.
pub(crate) async fn serve(
    listener: TcpListener,
    router: WebRouter,
    opts: Options,
    cancel: CancellationToken,
    log: Logger,
) -> Draining {
    let (shutdown, watching) = watch::channel(false);
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
        let io = TokioIo::new(DeadlineIo::new(stream, Arc::clone(&state)));
        let service = connection_service(router.clone(), state, local);
        // A connection error (timeouts, resets, a malformed request answered by hyper) ends that
        // connection only; Go's server does not log these either.
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
    Draining(shutdown)
}

/// The connection builder: HTTP/1 only, or HTTP/1 and prior-knowledge HTTP/2 with `-h2c` (Go
/// `Protocols{HTTP1, UnencryptedHTTP2}`; no `Upgrade: h2c`). HTTP/1 header names are written in
/// Go's canonical case; the header-read timeout is the connection's own read deadline.
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

/// The per-connection service: the router with Go's per-request deadlines and the accepted
/// address as `ConnectInfo<LocalAddr>` (Go `http.LocalAddrContextKey`, read by the origin guard).
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
            let started = state.begin_request(req.version());
            let (mut parts, body) = req.into_parts();
            parts.extensions.insert(ConnectInfo(LocalAddr(local)));
            let body = Body::new(ReadDeadlineBody::new(body, started.body_deadline));
            let served = router.serve(http::Request::from_parts(parts, body));
            let resp = match started.stream_deadline {
                Some(deadline) => tokio::time::timeout_at(deadline, served)
                    .await
                    .map_err(|_| BoxError::from(timed_out()))?,
                None => served.await,
            };
            if resp.status() == StatusCode::SWITCHING_PROTOCOLS {
                state.hijack();
            }
            Ok(resp.map(|body| TrackedBody::new(body, started.guard, started.stream_deadline)))
        }
    })
}

/// Go's `os.ErrDeadlineExceeded` (`i/o timeout`).
fn timed_out() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "i/o timeout")
}

/// One connection's deadlines, shared by its IO and its requests.
struct ConnState {
    opts: Options,
    times: Mutex<Times>,
}

struct Times {
    /// When the current HTTP/1 request started (Go's `t0`): the accept, or the first byte after
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
    /// The connection was upgraded (101): no deadlines (Go clears them on hijack).
    hijacked: bool,
    /// The task waiting on a read, woken when the read deadline changes.
    read_waker: Option<Waker>,
}

/// What a request was given by [`ConnState::begin_request`].
struct Started {
    guard: RequestGuard,
    /// The request body's read deadline.
    body_deadline: Option<Instant>,
    /// HTTP/2: the stream's write deadline.
    stream_deadline: Option<Instant>,
}

impl ConnState {
    fn new(opts: Options) -> Arc<ConnState> {
        let now = Instant::now();
        Arc::new(ConnState {
            opts,
            times: Mutex::new(Times {
                request_start: now,
                read_deadline: opts.read_timeout.map(|d| now + d),
                write_deadline: None,
                in_flight: 0,
                idle: false,
                h2: false,
                hijacked: false,
                read_waker: None,
            }),
        })
    }

    fn times(&self) -> std::sync::MutexGuard<'_, Times> {
        self.times.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn read_deadline(&self) -> Option<Instant> {
        let t = self.times();
        if t.hijacked { None } else { t.read_deadline }
    }

    fn write_deadline(&self) -> Option<Instant> {
        let t = self.times();
        if t.hijacked { None } else { t.write_deadline }
    }

    /// Bytes arrived: on an idle HTTP/1 connection a new request starts (Go: the read deadline
    /// is reset to `ReadTimeout` from now).
    fn on_bytes_read(&self) {
        let mut t = self.times();
        if t.idle && !t.h2 {
            let now = Instant::now();
            t.idle = false;
            t.request_start = now;
            t.read_deadline = self.opts.read_timeout.map(|d| now + d);
        }
    }

    /// A request's headers were read (Go `readRequest` returned).
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
            stream_deadline: if h2 { write_deadline } else { None },
        }
    }

    /// A response finished: with nothing else in flight the connection is idle (Go: read
    /// deadline `idleTimeout()` from now).
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

/// The TCP stream with the connection's read and write deadlines (Go `SetReadDeadline` /
/// `SetWriteDeadline`).
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

    /// `Err(i/o timeout)` once the write deadline has passed, registering for it otherwise.
    fn check_write(&mut self, cx: &mut Context<'_>) -> Option<io::Error> {
        let deadline = self.state.write_deadline()?;
        (Instant::now() >= deadline || deadline_passed(&mut self.write_timer, deadline, cx))
            .then(timed_out)
    }

    fn poll_write_with<T>(
        &mut self,
        cx: &mut Context<'_>,
        op: impl FnOnce(Pin<&mut TcpStream>, &mut Context<'_>) -> Poll<io::Result<T>>,
    ) -> Poll<io::Result<T>> {
        if let Some(deadline) = self.state.write_deadline()
            && Instant::now() >= deadline
        {
            return Poll::Ready(Err(timed_out()));
        }
        match op(Pin::new(&mut self.inner), cx) {
            Poll::Pending => match self.check_write(cx) {
                Some(e) => Poll::Ready(Err(e)),
                None => Poll::Pending,
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
                    this.state.on_bytes_read();
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

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.get_mut()
            .poll_write_with(cx, |io, cx| io.poll_flush(cx))
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

/// A response body that ends its request when finished or dropped and, for an HTTP/2 stream,
/// fails at the stream's write deadline (resetting the stream).
pub(crate) struct TrackedBody {
    inner: Body,
    guard: Option<RequestGuard>,
    deadline: Option<Pin<Box<Sleep>>>,
}

impl TrackedBody {
    fn new(inner: Body, guard: RequestGuard, deadline: Option<Instant>) -> TrackedBody {
        TrackedBody {
            inner,
            guard: Some(guard),
            deadline: deadline.map(|d| Box::pin(tokio::time::sleep_until(d))),
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
        if let Some(timer) = &mut this.deadline
            && timer.as_mut().poll(cx).is_ready()
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
