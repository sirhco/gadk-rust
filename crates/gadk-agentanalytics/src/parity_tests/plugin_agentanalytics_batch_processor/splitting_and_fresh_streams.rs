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

//! `batch_processor_test.go` ports, Go file order from `TestNextBatchAfterServerEndedStream` through `TestBatchProcessor_DeferredRetrySkipsConfirmedSplitPrefix`.
//! Go's `streamEndingServer`, `freshStreamPrematureCloseServer` and `scriptedWriteServer` are
//! hooks on `FakeWriteServer`; Go's client stream interceptors (`endedClientStream`,
//! `directSendErrClientStream`, `sendHookClientStream`) wrap the stream in
//! `WriteClient::with_stream_interceptor`.

use super::support::row_count;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::DateTime;
use gadk_platform::BoxFuture;
use pretty_assertions::assert_eq;
use prost::Message as _;
use tokio_util::sync::CancellationToken;

use super::support::{
    HookOffloader, ScriptedServer, decode_arrow_rows, exactly_once_test_config, image_content,
    inject_stream, new_error_classification_processor,
    new_error_classification_processor_for_client, processor, push_queue, qrow, queued_row,
    reserve_with, status,
};
use crate::batch_processor::BatchProcessor;
use crate::bigquery::fake::RecordingLogger;
use crate::config::{Config, GcsContentOffloader, GcsUploadRequest, Logger, RetryConfig};
use crate::context::Context;
use crate::error::{Code, Error, status_code};
use crate::formatter::PartsOptions;
use crate::row::{QueuedRow, Row, RowValue};
use crate::storagepb::{AppendRowsRequest, AppendRowsResponse};
use crate::testutil::fake_write_server::{AppendRowsServerStream, FakeWriteServer};
use crate::write::arrow_batch::{
    PER_CONTENT_PART_OVERHEAD_BYTES, PER_ROW_OVERHEAD_BYTES, estimate_row_bytes, serialize_record,
    split_batch_by_estimated_bytes,
};
use crate::write::gapic::{OpenAppendRows, StreamInterceptor, WriteClient};
use crate::write::stream_writer::tests::{Fail, MockCounts, MockStream, in_band};
use crate::write::{AppendRowsClient, eof, is_eof};

/// The `_default` stream Go's split tests write to.
const PDT_DEFAULT: &str = "projects/p/datasets/d/tables/t/_default";

/// The `_default` stream of the scripted table.
const TEST_DEFAULT_STREAM: &str =
    "projects/test-proj/datasets/test-ds/tables/agent_events/_default";

/// Go `map[string]any{"event_id": id, "event_type": "X"}`.
fn id_row(id: &str) -> QueuedRow {
    qrow(&[("event_id", id.into()), ("event_type", "X".into())])
}

/// Go `map[string]any{"event_id": id, "content": strings.Repeat("x", 1000)}`.
fn content_row(id: &str) -> QueuedRow {
    qrow(&[
        ("event_id", id.into()),
        ("content", "x".repeat(1000).into()),
    ])
}

/// The `event_id` of a decoded row (Go `fmt.Sprint(row["event_id"])`).
fn event_id(row: &Row) -> String {
    match row.get("event_id") {
        Some(RowValue::Str(s)) => s.clone(),
        other => format!("{other:?}"),
    }
}

/// Go `deliveredRequest`.
#[derive(Clone)]
struct DeliveredRequest {
    stream: usize,
    offset: i64,
    req: AppendRowsRequest,
}

/// The `stream_no:event_id` labels and offsets of delivered requests (each one row).
fn labels(delivered: &[DeliveredRequest]) -> (Vec<String>, Vec<i64>) {
    let mut got = Vec::new();
    let mut offsets = Vec::new();
    for d in delivered {
        let rows = decode_arrow_rows(&d.req);
        assert_eq!(rows.len(), 1, "a request holds {} rows, want 1", rows.len());
        got.push(format!("{}:{}", d.stream, event_id(&rows[0])));
        offsets.push(d.offset);
    }
    (got, offsets)
}

/// What Go's `streamEndingServer` / `freshStreamPrematureCloseServer` record.
#[derive(Default)]
struct Received {
    streams: usize,
    delivered: Vec<DeliveredRequest>,
}

type Recorded = Arc<Mutex<Received>>;

/// Records a request received on `stream_no`.
fn record(rec: &Recorded, stream_no: usize, req: &AppendRowsRequest) {
    rec.lock().unwrap().delivered.push(DeliveredRequest {
        stream: stream_no,
        offset: req.offset.map_or(-1, |o| o.value),
        req: req.clone(),
    });
}

/// A scripted server (Go `scriptedCommittedStreamServer`'s `CreateWriteStream`) whose
/// AppendRows is replaced by `hook`.
fn scripted_with(
    hook: impl Fn(AppendRowsServerStream) -> BoxFuture<'static, Result<(), tonic::Status>>
    + Send
    + Sync
    + 'static,
) -> ScriptedServer {
    let srv = ScriptedServer::new(Vec::new(), |_, _| Ok(AppendRowsResponse::default()), None);
    srv.server.on_append_rows(Arc::new(hook));
    srv
}

/// Go `streamEndingServer{code, endOnNextRequest}`.
fn stream_ending_server(code: Code, end_on_next_request: bool) -> (ScriptedServer, Recorded) {
    let rec: Recorded = Arc::default();
    let r = Arc::clone(&rec);
    let srv = scripted_with(move |mut stream| {
        let rec = Arc::clone(&r);
        Box::pin(async move {
            let stream_no = {
                let mut s = rec.lock().unwrap();
                s.streams += 1;
                s.streams
            };
            let first = stream_no == 1;
            let mut answered = 0;
            while let Some(req) = stream.recv().await? {
                record(&rec, stream_no, &req);
                if first && end_on_next_request && answered == 1 {
                    return Err(status(code, "stream ended by server"));
                }
                stream.send(AppendRowsResponse::default()).await?;
                answered += 1;
                if first && !end_on_next_request {
                    return Err(status(code, "stream ended by server"));
                }
            }
            Ok(())
        })
    });
    (srv, rec)
}

/// A gate a server stream waits on (Go's `chan struct{}` closed once).
type Gates = Arc<Mutex<HashMap<usize, CancellationToken>>>;

fn gate_for(gates: &Gates, stream_no: usize) -> CancellationToken {
    gates.lock().unwrap().entry(stream_no).or_default().clone()
}

/// Go `freshStreamPrematureCloseServer{closeStreams, codeFor, waitFor}`: the first
/// `close_streams` streams end with `code_for(stream_no)` before reading a request, after
/// `wait_for(stream_no)` opens (if any); later streams accept every request.
fn premature_close_server(
    close_streams: usize,
    code_for: impl Fn(usize) -> Code + Send + Sync + 'static,
    wait_for: impl Fn(usize) -> Option<CancellationToken> + Send + Sync + 'static,
) -> (ScriptedServer, Recorded) {
    let rec: Recorded = Arc::default();
    let r = Arc::clone(&rec);
    let code_for = Arc::new(code_for);
    let wait_for = Arc::new(wait_for);
    let srv = scripted_with(move |mut stream| {
        let rec = Arc::clone(&r);
        let code_for = Arc::clone(&code_for);
        let wait_for = Arc::clone(&wait_for);
        Box::pin(async move {
            let stream_no = {
                let mut s = rec.lock().unwrap();
                s.streams += 1;
                s.streams
            };
            if stream_no <= close_streams {
                if let Some(gate) = wait_for(stream_no) {
                    tokio::select! {
                        () = gate.cancelled() => {}
                        () = stream.cancelled() => return Err(tonic::Status::cancelled("context canceled")),
                    }
                }
                return Err(status(code_for(stream_no), "stream closed before recv"));
            }
            while let Some(req) = stream.recv().await? {
                record(&rec, stream_no, &req);
                stream.send(AppendRowsResponse::default()).await?;
            }
            Ok(())
        })
    });
    (srv, rec)
}

/// Go `sendHookClientStream.afterSend`.
type AfterSend = Box<dyn Fn(&Result<(), Error>) + Send>;

/// Go `sendHookClientStream`: `wait_for_end` before the first send is Go's `raw.Header()`
/// (wait for the server's close to arrive): with the in-memory transport and paused time, a
/// sleep only ends once every task is idle, i.e. after the server's trailers reached the
/// stream; `after_send` sees each send's result.
struct SendHookStream {
    inner: Box<dyn AppendRowsClient>,
    wait_for_end: bool,
    after_send: Option<AfterSend>,
}

#[async_trait::async_trait]
impl AppendRowsClient for SendHookStream {
    async fn send(&mut self, req: AppendRowsRequest) -> Result<(), Error> {
        if self.wait_for_end {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        let result = self.inner.send(req).await;
        if let Some(after_send) = &self.after_send {
            after_send(&result);
        }
        result
    }

    async fn recv(&mut self) -> Result<AppendRowsResponse, Error> {
        self.inner.recv().await
    }

    fn close_send(&mut self) {
        self.inner.close_send();
    }
}

/// Go `endedClientStream` (`send_eof`) / `directSendErrClientStream`.
struct ScriptedErrStream {
    inner: Box<dyn AppendRowsClient>,
    code: Code,
    send_eof: bool,
}

impl ScriptedErrStream {
    fn err(&self) -> Error {
        Error::Status {
            code: self.code,
            message: "scripted".to_owned(),
        }
    }
}

#[async_trait::async_trait]
impl AppendRowsClient for ScriptedErrStream {
    async fn send(&mut self, _req: AppendRowsRequest) -> Result<(), Error> {
        if self.send_eof {
            Err(eof())
        } else {
            Err(self.err())
        }
    }

    async fn recv(&mut self) -> Result<AppendRowsResponse, Error> {
        if self.send_eof {
            Err(self.err())
        } else {
            self.inner.recv().await
        }
    }

    fn close_send(&mut self) {
        self.inner.close_send();
    }
}

/// A stream that records each request and answers it itself, never forwarding it: the fake
/// server decodes at most 4 MiB (tonic's default; Go's tests raise the server's limit with
/// `grpc.MaxRecvMsgSize(64 MiB)`).
struct CapturingStream {
    _inner: Box<dyn AppendRowsClient>,
    captured: Arc<Mutex<Vec<AppendRowsRequest>>>,
    pending: usize,
}

#[async_trait::async_trait]
impl AppendRowsClient for CapturingStream {
    async fn send(&mut self, req: AppendRowsRequest) -> Result<(), Error> {
        self.captured.lock().unwrap().push(req);
        self.pending += 1;
        Ok(())
    }

    async fn recv(&mut self) -> Result<AppendRowsResponse, Error> {
        if self.pending == 0 {
            return Err(eof());
        }
        self.pending -= 1;
        Ok(AppendRowsResponse::default())
    }

    fn close_send(&mut self) {}
}

/// A client of a fake server whose AppendRows requests are captured client-side (Go's
/// `scriptedWriteServer{script: 0, requests}` behind `grpc.MaxRecvMsgSize(64 MiB)`).
fn capturing_client(server: &FakeWriteServer) -> (WriteClient, Arc<Mutex<Vec<AppendRowsRequest>>>) {
    let captured: Arc<Mutex<Vec<AppendRowsRequest>>> = Arc::default();
    let c = Arc::clone(&captured);
    let interceptor: StreamInterceptor = Arc::new(move |_ctx: Context, open: OpenAppendRows| {
        let captured = Arc::clone(&c);
        Box::pin(async move {
            let inner = open.await?;
            Ok(Box::new(CapturingStream {
                _inner: inner,
                captured,
                pending: 0,
            }) as Box<dyn AppendRowsClient>)
        })
    });
    let client = server
        .client()
        .unwrap()
        .with_stream_interceptor(interceptor);
    (client, captured)
}

/// An interceptor wrapping stream `n` (1-based) in a [`SendHookStream`] made by `wrap`.
fn send_hook_interceptor(
    opened: Arc<AtomicUsize>,
    wrap: impl Fn(usize) -> Option<(bool, Option<AfterSend>)> + Send + Sync + 'static,
) -> StreamInterceptor {
    let wrap = Arc::new(wrap);
    Arc::new(move |_ctx: Context, open: OpenAppendRows| {
        let opened = Arc::clone(&opened);
        let wrap = Arc::clone(&wrap);
        Box::pin(async move {
            let inner = open.await?;
            let n = opened.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(match wrap(n) {
                Some((wait_for_end, after_send)) => Box::new(SendHookStream {
                    inner,
                    wait_for_end,
                    after_send,
                }) as Box<dyn AppendRowsClient>,
                None => inner,
            })
        })
    })
}

/// A Go `sendHookClientStream` for the `site` of stream `n`: `send` waits for the server's
/// close; `recv` opens the server's gate after the send.
fn site_hook(site: &str, gates: &Gates, n: usize) -> (bool, Option<AfterSend>) {
    let gate = gate_for(gates, n);
    let recv = site == "recv";
    (
        site == "send",
        Some(Box::new(move |_r: &Result<(), Error>| {
            if recv {
                gate.cancel();
            }
        })),
    )
}

#[tokio::test(start_paused = true)]
async fn test_next_batch_after_server_ended_stream() {
    let end_codes = [
        // OK ends the stream cleanly, which Send and Recv report as EOF.
        Code::Ok,
        Code::Unavailable,
        Code::FailedPrecondition,
        Code::Unauthenticated,
        Code::PermissionDenied,
        Code::InvalidArgument,
        Code::Unimplemented,
        Code::NotFound,
    ];
    let cases: [(&str, bool, Duration); 3] = [
        ("ended after the answer", false, Duration::ZERO),
        (
            "ended after the answer and a pause",
            false,
            Duration::from_millis(50),
        ),
        ("ended on the next request", true, Duration::ZERO),
    ];
    for exactly_once in [false, true] {
        for code in end_codes {
            if exactly_once && code == Code::NotFound {
                // With exactly-once delivery, NotFound replaces the committed stream, which
                // other tests cover.
                continue;
            }
            for (name, end_on_next_request, pause) in cases {
                let label = format!("{code:?}/exactlyOnce={exactly_once}/{name}");
                let (srv, rec) = stream_ending_server(code, end_on_next_request);
                let bp = new_error_classification_processor(&srv.server, exactly_once).await;
                let ctx = Context::background();

                if let Err(e) = bp.write_batch(&ctx, vec![id_row("A")]).await {
                    panic!("{label}: first batch error = {e}");
                }
                tokio::time::sleep(pause).await;
                if let Err(e) = bp.write_batch(&ctx, vec![id_row("B")]).await {
                    let streams = rec.lock().unwrap().streams;
                    panic!("{label}: second batch dropped after {streams} stream(s): {e}");
                }

                let (streams, delivered) = {
                    let r = rec.lock().unwrap();
                    (r.streams, r.delivered.clone())
                };
                assert_eq!(
                    streams, 2,
                    "{label}: streams served, want 2: the second batch needs a fresh one"
                );
                let (got, got_offsets) = labels(&delivered);
                // In the first case the second request is not received by the first stream,
                // which has ended; in the second it is, and is sent again.
                let (want, mut want_offsets) = if end_on_next_request {
                    (vec!["1:A", "1:B", "2:B"], vec![0, 1, 1])
                } else {
                    (vec!["1:A", "2:B"], vec![0, 1])
                };
                if !exactly_once {
                    want_offsets = vec![-1; want.len()];
                }
                assert_eq!(got, want, "{label}: requests received (stream:event_id)");
                assert_eq!(got_offsets, want_offsets, "{label}: request offsets");
                if exactly_once {
                    assert_eq!(
                        srv.create_count(),
                        1,
                        "{label}: CreateWriteStream calls: the committed stream stays"
                    );
                }
                assert_eq!(
                    bp.drop_stats().append_failure_drops,
                    0,
                    "{label}: AppendFailureDrops"
                );
                bp.close().await;
            }
        }
    }
}

#[tokio::test(start_paused = true)]
async fn test_stream_writer_status_of_ended_stream_is_retried_on_fresh_stream() {
    let permanent = [
        Code::PermissionDenied,
        Code::InvalidArgument,
        Code::Unauthenticated,
        Code::FailedPrecondition,
        Code::Unimplemented,
        Code::NotFound,
    ];
    for exactly_once in [false, true] {
        for code in permanent {
            if exactly_once && code == Code::NotFound {
                // With exactly-once delivery, NotFound replaces the committed stream, which
                // other tests cover.
                continue;
            }
            for site in ["send", "recv"] {
                for fresh_stream_accepts in [true, false] {
                    let label = format!(
                        "{code:?}/exactlyOnce={exactly_once}/{site}/freshStreamAccepts={fresh_stream_accepts}"
                    );
                    let srv = ScriptedServer::new(
                        Vec::new(),
                        move |_, _| {
                            if fresh_stream_accepts {
                                Ok(AppendRowsResponse::default())
                            } else {
                                Err(status(code, "scripted"))
                            }
                        },
                        None,
                    );
                    let bp = new_error_classification_processor(&srv.server, exactly_once).await;
                    let counts = Arc::new(MockCounts::default());
                    inject_stream(
                        &bp,
                        MockStream {
                            recv_err: Some(Fail::Status(code, "stream ended by server")),
                            send_err: (site == "send").then_some(Fail::Eof),
                            counts: Arc::clone(&counts),
                            ..MockStream::default()
                        },
                    )
                    .await;

                    let result = bp
                        .write_batch(&Context::background(), vec![id_row("A")])
                        .await;

                    assert_eq!(
                        counts.sends.load(Ordering::SeqCst),
                        1,
                        "{label}: sends on the ended stream"
                    );
                    // The ended stream takes the request first; when the first fresh stream
                    // rejects with a transport status, one bounded fresh-stream retry runs
                    // before failing fast without exhausting the final retry.
                    let want_attempts = if fresh_stream_accepts { 1 } else { 2 };
                    assert_eq!(
                        srv.attempts(),
                        want_attempts,
                        "{label}: requests that reached the server (error: {result:?})"
                    );
                    if fresh_stream_accepts {
                        if let Err(e) = &result {
                            panic!("{label}: writeBatch() error = {e}, want nil");
                        }
                        assert_eq!(bp.drop_stats().append_failure_drops, 0, "{label}");
                    } else {
                        let err = result.expect_err("want an error");
                        assert_eq!(status_code(&err), code, "{label}: status code ({err})");
                        assert_eq!(bp.drop_stats().append_failure_drops, 1, "{label}");
                    }
                    bp.close().await;
                }
            }
        }
    }
}

#[tokio::test(start_paused = true)]
async fn test_stream_writer_permanent_status_fails_fast_on_fresh_stream() {
    let want_err = |site: &str| match site {
        "open" => "failed to open append rows stream",
        _ => "failed to send row batch",
    };
    let mut cases = Vec::new();
    for site in ["open", "send", "send-direct"] {
        let want_perm_opened = if site == "send" { 2 } else { 1 };
        for code in [
            Code::PermissionDenied,
            Code::InvalidArgument,
            Code::Unauthenticated,
            Code::FailedPrecondition,
            Code::Unimplemented,
            Code::NotFound,
        ] {
            cases.push((site, code, want_perm_opened));
        }
    }
    // A transient status still uses up the retries: four streams in all. (The client retries
    // Unavailable when it opens a stream, so the open site has no such case.)
    cases.push(("send", Code::Unavailable, 4));
    cases.push(("send-direct", Code::Unavailable, 4));

    for (site, code, want_opened) in cases {
        let label = format!("{site}/{code:?}");
        let srv = ScriptedServer::new(
            Vec::new(),
            |_, _| panic!("a request reached the server, want none"),
            None,
        );
        let opened = Arc::new(AtomicUsize::new(0));
        let o = Arc::clone(&opened);
        let site_owned = site.to_owned();
        let interceptor: StreamInterceptor =
            Arc::new(move |_ctx: Context, open: OpenAppendRows| {
                let opened = Arc::clone(&o);
                let site = site_owned.clone();
                Box::pin(async move {
                    opened.fetch_add(1, Ordering::SeqCst);
                    if site == "open" {
                        return Err(Error::Status {
                            code,
                            message: "scripted".to_owned(),
                        });
                    }
                    let inner = open.await?;
                    Ok(Box::new(ScriptedErrStream {
                        inner,
                        code,
                        send_eof: site == "send",
                    }) as Box<dyn AppendRowsClient>)
                })
            });
        let client = srv
            .server
            .client()
            .unwrap()
            .with_stream_interceptor(interceptor);
        let bp = new_error_classification_processor_for_client(client, false).await;

        let err = bp
            .write_batch(&Context::background(), vec![id_row("A")])
            .await
            .expect_err("want an error");

        assert!(
            err.to_string().contains(want_err(site)),
            "{label}: writeBatch() error = {err}, want it to contain {:?}",
            want_err(site)
        );
        assert_eq!(status_code(&err), code, "{label}: status code ({err})");
        assert_eq!(
            opened.load(Ordering::SeqCst),
            want_opened,
            "{label}: streams opened"
        );
        assert_eq!(bp.drop_stats().append_failure_drops, 1, "{label}");
        bp.close().await;
    }
}

// parity: adapted: the "send" site waits for the server's close with a paused-time sleep (tonic has no Go raw.Header() to wait on)
#[tokio::test(start_paused = true)]
async fn test_fresh_stream_closed_before_recv_is_retried() {
    let end_codes = [
        Code::Ok,
        Code::Unavailable,
        Code::FailedPrecondition,
        Code::Unauthenticated,
        Code::PermissionDenied,
        Code::InvalidArgument,
        Code::Unimplemented,
        Code::NotFound,
    ];
    for exactly_once in [false, true] {
        for code in end_codes {
            for site in ["send", "recv"] {
                let label = format!("{code:?}/exactlyOnce={exactly_once}/{site}");
                let after_first_send = CancellationToken::new();
                let gate = after_first_send.clone();
                let (srv, rec) = premature_close_server(
                    1,
                    move |_| code,
                    move |n| (n == 1 && site == "recv").then(|| gate.clone()),
                );
                let first_send: Arc<Mutex<Option<Result<(), String>>>> = Arc::default();
                let opened = Arc::new(AtomicUsize::new(0));
                let fs = Arc::clone(&first_send);
                let open_gate = after_first_send.clone();
                let interceptor = send_hook_interceptor(Arc::clone(&opened), move |n| {
                    if n != 1 {
                        return None;
                    }
                    let fs = Arc::clone(&fs);
                    let open_gate = open_gate.clone();
                    Some((
                        site == "send",
                        Some(Box::new(move |r: &Result<(), Error>| {
                            let mut first = fs.lock().unwrap();
                            if first.is_none() {
                                *first = Some(match r {
                                    Ok(()) => Ok(()),
                                    Err(e) if is_eof(e) => Err("EOF".to_owned()),
                                    Err(e) => Err(e.to_string()),
                                });
                            }
                            if site == "recv" {
                                open_gate.cancel();
                            }
                        }) as AfterSend),
                    ))
                });
                let client = srv
                    .server
                    .client()
                    .unwrap()
                    .with_stream_interceptor(interceptor);
                let bp = new_error_classification_processor_for_client(client, exactly_once).await;

                if let Err(e) = bp
                    .write_batch(&Context::background(), vec![id_row("A")])
                    .await
                {
                    let streams = rec.lock().unwrap().streams;
                    panic!("{label}: writeBatch() failed after {streams} stream(s): {e}");
                }

                let first = first_send
                    .lock()
                    .unwrap()
                    .clone()
                    .unwrap_or_else(|| panic!("{label}: first stream SendMsg did not run"));
                if site == "send" {
                    assert_eq!(
                        first,
                        Err("EOF".to_owned()),
                        "{label}: first stream Send() error, want EOF so the Send branch is exercised"
                    );
                } else {
                    assert_eq!(
                        first,
                        Ok(()),
                        "{label}: first stream Send() error, want nil so the Recv branch is exercised"
                    );
                }

                let (streams, delivered) = {
                    let r = rec.lock().unwrap();
                    (r.streams, r.delivered.clone())
                };
                assert_eq!(streams, 2, "{label}: streams served");
                let (got, got_offsets) = labels(&delivered);
                // Stream 1 closed before reading any request, so only stream 2 receives the row.
                assert_eq!(got, vec!["2:A"], "{label}: requests received");
                let want_offsets = if exactly_once { vec![0] } else { vec![-1] };
                assert_eq!(got_offsets, want_offsets, "{label}: request offsets");
                if exactly_once {
                    let want_creates = if code == Code::NotFound { 2 } else { 1 };
                    assert_eq!(
                        srv.create_count(),
                        want_creates,
                        "{label}: CreateWriteStream calls"
                    );
                }
                assert_eq!(bp.drop_stats().append_failure_drops, 0, "{label}");
                bp.close().await;
            }
        }
    }
}

/// Go's `siteForStream(mode, streamNo)`.
fn site_for_stream(mode: &str, stream_no: usize) -> &'static str {
    match mode {
        "send" => "send",
        "recv" => "recv",
        "send-then-recv" => {
            if stream_no == 1 {
                "send"
            } else {
                "recv"
            }
        }
        "recv-then-send" => {
            if stream_no == 1 {
                "recv"
            } else {
                "send"
            }
        }
        _ => panic!("unknown mode: {mode}"),
    }
}

/// A premature-close server whose streams each close with `code_for(n)` once their site's
/// condition holds, with the matching client hooks; returns the client too.
fn gated_premature_close(
    code_for: impl Fn(usize) -> Code + Send + Sync + 'static,
    site_of: impl Fn(usize) -> &'static str + Send + Sync + Clone + 'static,
) -> (ScriptedServer, Recorded, WriteClient) {
    let gates: Gates = Arc::default();
    let g = Arc::clone(&gates);
    let s = site_of.clone();
    let (srv, rec) = premature_close_server(10, code_for, move |n| {
        (s(n) == "recv").then(|| gate_for(&g, n))
    });
    let interceptor = send_hook_interceptor(Arc::new(AtomicUsize::new(0)), move |n| {
        Some(site_hook(site_of(n), &gates, n))
    });
    let client = srv
        .server
        .client()
        .unwrap()
        .with_stream_interceptor(interceptor);
    (srv, rec, client)
}

// parity: adapted: the "send" site waits for the server's close with a paused-time sleep (tonic has no Go raw.Header() to wait on)
#[tokio::test(start_paused = true)]
async fn test_fresh_stream_closed_before_recv_retry_is_bounded() {
    let permanent = [
        Code::PermissionDenied,
        Code::InvalidArgument,
        Code::Unauthenticated,
        Code::FailedPrecondition,
        Code::Unimplemented,
        Code::NotFound,
    ];
    let ctx = Context::background();
    for exactly_once in [false, true] {
        for code in permanent {
            if exactly_once && code == Code::NotFound {
                continue;
            }
            for mode in ["send", "recv", "send-then-recv", "recv-then-send"] {
                let label = format!("{code:?}/exactlyOnce={exactly_once}/{mode}");
                let (_srv, rec, client) =
                    gated_premature_close(move |_| code, move |n| site_for_stream(mode, n));
                let bp = new_error_classification_processor_for_client(client, exactly_once).await;

                let err = bp
                    .write_batch(&ctx, vec![id_row("A")])
                    .await
                    .expect_err("writeBatch() error = nil, want permanent error");
                assert_eq!(status_code(&err), code, "{label}: status code ({err})");
                let (streams, delivered) = {
                    let r = rec.lock().unwrap();
                    (r.streams, r.delivered.len())
                };
                assert_eq!(streams, 2, "{label}: streams served (1 fresh-stream retry)");
                assert_eq!(delivered, 0, "{label}: delivered requests");
                assert_eq!(bp.drop_stats().append_failure_drops, 1, "{label}");
                bp.close().await;
            }
        }
    }

    // maxRetries=0 does not reconnect.
    {
        let (_srv, rec, client) = gated_premature_close(|_| Code::FailedPrecondition, |_| "send");
        let config = Config {
            retry_config: RetryConfig {
                max_retries: 0,
                initial_delay: Duration::from_millis(1),
                max_delay: Duration::from_millis(2),
                multiplier: 1.0,
            },
            ..Config::default()
        };
        let bp = processor(&ctx, Some(client), TEST_DEFAULT_STREAM, config).await;
        let err = bp
            .write_batch(&ctx, vec![id_row("A")])
            .await
            .expect_err("want FailedPrecondition");
        assert_eq!(status_code(&err), Code::FailedPrecondition, "({err})");
        assert_eq!(
            rec.lock().unwrap().streams,
            1,
            "streams served, want 1 when MaxRetries == 0"
        );
        assert_eq!(bp.drop_stats().append_failure_drops, 1);
        bp.close().await;
    }

    for site in ["send", "recv"] {
        // rotation_budget_preserved_across_PermissionDenied: stream 1 closes with NotFound
        // and rotates the committed stream once; stream 2 closes before Recv with
        // PermissionDenied and uses the bounded fresh-stream retry without replenishing the
        // rotation budget; stream 3 closes with NotFound and must fail with "already rotated"
        // rather than calling CreateWriteStream a third time.
        {
            let label = format!("rotation_budget_preserved_across_PermissionDenied/{site}");
            let site_of: &'static str = if site == "send" { "send" } else { "recv" };
            let (srv, rec, client) = gated_premature_close(
                |n| {
                    if n == 2 {
                        Code::PermissionDenied
                    } else {
                        Code::NotFound
                    }
                },
                move |_| site_of,
            );
            let bp = new_error_classification_processor_for_client(client, true).await;
            let err = bp
                .write_batch(&ctx, vec![id_row("A")])
                .await
                .expect_err("want an error");
            assert!(
                err.to_string().contains("already rotated"),
                "{label}: writeBatch() error = {err}, want committed write stream already rotated"
            );
            assert_eq!(
                srv.create_count(),
                2,
                "{label}: CreateWriteStream calls (PermissionDenied must not replenish rotation budget)"
            );
            assert_eq!(rec.lock().unwrap().streams, 3, "{label}: streams served");
            assert!(
                bp.stream_writer.is_rotation_in_cooldown().await,
                "{label}: isRotationInCooldown() = false, want true because freshlyRotated remained armed"
            );
            bp.close().await;
        }

        // rotatedInBatch_preserved_when_freshlyRotated_is_false: start on the initial
        // committed stream with rotated = true; stream 1 closes before Recv with
        // PermissionDenied (the bounded fresh-stream retry), stream 2 with NotFound. Only
        // rotated_in_batch prevents a second rotation.
        {
            let label = format!("rotatedInBatch_preserved_when_freshlyRotated_is_false/{site}");
            let site_of: &'static str = if site == "send" { "send" } else { "recv" };
            let (srv, rec, client) = gated_premature_close(
                |n| {
                    if n == 1 {
                        Code::PermissionDenied
                    } else {
                        Code::NotFound
                    }
                },
                move |_| site_of,
            );
            let bp = new_error_classification_processor_for_client(client, true).await;
            let mut rotated = true;
            let (_, result) = bp
                .append_chunk_with_rotation_budget(&ctx, &[id_row("A")], &mut rotated)
                .await;
            let err = result.expect_err("want an error");
            assert!(
                err.to_string().contains("already rotated"),
                "{label}: appendChunkWithRotationBudget() error = {err}, want committed write stream already rotated"
            );
            assert!(
                rotated,
                "{label}: rotated = false, want true (PermissionDenied retry must not clear rotated_in_batch)"
            );
            assert_eq!(srv.create_count(), 1, "{label}: CreateWriteStream calls");
            assert_eq!(rec.lock().unwrap().streams, 2, "{label}: streams served");
            assert!(
                !bp.stream_writer.is_rotation_in_cooldown().await,
                "{label}: isRotationInCooldown() = true, want false because freshlyRotated was false"
            );
            bp.close().await;
        }
    }
}

#[test]
fn test_split_batch_by_estimated_bytes() {
    let row = |n: usize| -> Row {
        [("content".to_owned(), RowValue::from("x".repeat(n)))]
            .into_iter()
            .collect()
    };
    let row_cost = estimate_row_bytes(&row(1000));
    let rows = |sizes: &[usize]| -> Vec<Row> { sizes.iter().map(|n| row(*n)).collect() };
    let cases: Vec<(&str, Vec<Row>, usize, Vec<usize>)> = vec![
        ("empty", Vec::new(), 10, vec![0]),
        ("fits", rows(&[1000, 1000]), 2 * row_cost, vec![2]),
        (
            "one byte over",
            rows(&[1000, 1000]),
            2 * row_cost - 1,
            vec![1, 1],
        ),
        (
            "packs greedily",
            rows(&[1000, 1000, 1000, 1000, 1000]),
            2 * row_cost,
            vec![2, 2, 1],
        ),
        (
            "oversized row stands alone",
            rows(&[1000, 50000, 1000]),
            2 * row_cost,
            vec![1, 1, 1],
        ),
        (
            "leading oversized row",
            rows(&[50000, 1000, 1000]),
            2 * row_cost,
            vec![1, 2],
        ),
        ("no limit", rows(&[1000, 1000]), 0, vec![2]),
    ];
    for (name, batch, limit, want) in cases {
        let chunks = split_batch_by_estimated_bytes(&batch, limit);
        let got: Vec<usize> = chunks.iter().map(|c| c.len()).collect();
        assert_eq!(got, want, "{name}: chunk sizes");
        let flat: Vec<&Row> = chunks.iter().flat_map(|c| c.iter()).collect();
        assert_eq!(flat.len(), batch.len(), "{name}: rows in chunks");
        for (i, r) in flat.iter().enumerate() {
            assert!(
                std::ptr::eq(*r, &batch[i]),
                "{name}: row {i} moved: chunks must keep the order of the batch"
            );
        }
    }
}

#[test]
fn test_estimate_row_bytes_counts_nested_content_parts() {
    let object_ref: Row = [("uri".to_owned(), RowValue::from("c".repeat(300)))]
        .into_iter()
        .collect();
    let part: Row = [
        ("text".to_owned(), RowValue::from("b".repeat(200))),
        ("object_ref".to_owned(), RowValue::Map(object_ref)),
    ]
    .into_iter()
    .collect();
    let row: Row = [
        ("content".to_owned(), RowValue::from("a".repeat(100))),
        ("content_parts".to_owned(), RowValue::Parts(vec![part])),
        ("is_truncated".to_owned(), RowValue::Bool(true)),
        (
            "timestamp".to_owned(),
            RowValue::Timestamp(DateTime::from_timestamp(0, 0).unwrap()),
        ),
    ]
    .into_iter()
    .collect();
    let got = estimate_row_bytes(&row);
    assert!(
        got >= PER_ROW_OVERHEAD_BYTES + 600,
        "estimateRowBytes() = {got}, want at least {} to cover the strings",
        PER_ROW_OVERHEAD_BYTES + 600
    );

    // Go's empty maps and nil maps are both empty rows here.
    let empty_parts_row: Row = [(
        "content_parts".to_owned(),
        RowValue::Parts(vec![Row::new(); 1000]),
    )]
    .into_iter()
    .collect();
    let want = PER_ROW_OVERHEAD_BYTES + 1000 * PER_CONTENT_PART_OVERHEAD_BYTES;
    let got = estimate_row_bytes(&empty_parts_row);
    assert!(
        got >= want,
        "estimateRowBytes(emptyPartsRow) = {got}, want at least {want}"
    );
    let nil_parts_row = empty_parts_row.clone();
    let got = estimate_row_bytes(&nil_parts_row);
    assert!(
        got >= want,
        "estimateRowBytes(nilPartsRow) = {got}, want at least {want}"
    );
}

/// The rows of captured requests (each checked against BigQuery's request size limit).
fn captured_ids(
    captured: &Arc<Mutex<Vec<AppendRowsRequest>>>,
    limit: usize,
) -> (usize, Vec<String>) {
    let reqs = captured.lock().unwrap().clone();
    let mut ids = Vec::new();
    for (i, req) in reqs.iter().enumerate() {
        let size = req.encoded_len();
        assert!(
            size <= limit,
            "request {} is {size} bytes, want at most {limit}",
            i + 1
        );
        ids.extend(decode_arrow_rows(req).iter().map(event_id));
    }
    (reqs.len(), ids)
}

// parity: adapted: requests are captured and answered client-side (stream interceptor), since the fake server cannot raise its 4 MiB receive limit as Go's grpc.MaxRecvMsgSize(64 MiB) does
#[tokio::test(start_paused = true)]
async fn test_batch_processor_splits_oversized_batch_across_requests() {
    const NUM_ROWS: usize = 100;
    const CONTENT_LEN: usize = 500 * 1024;
    // The limit that BigQuery documents for the size of one request.
    const BIG_QUERY_REQUEST_LIMIT: usize = 10 * 1024 * 1024;
    let server = FakeWriteServer::new();
    let (client, captured) = capturing_client(&server);
    let mut cfg = Config::default();
    cfg.retry_config.max_retries = 0;
    let bp = processor(&Context::background(), Some(client), PDT_DEFAULT, cfg).await;

    let content = "x".repeat(CONTENT_LEN);
    let batch: Vec<QueuedRow> = (0..NUM_ROWS)
        .map(|i| {
            qrow(&[
                ("event_id", format!("row-{i:03}").into()),
                ("event_type", "X".into()),
                ("content", content.clone().into()),
            ])
        })
        .collect();
    bp.write_batch(&Context::background(), batch)
        .await
        .unwrap_or_else(|e| panic!("writeBatch() error = {e}"));

    let (num_requests, got_ids) = captured_ids(&captured, BIG_QUERY_REQUEST_LIMIT);
    assert!(
        num_requests >= 2,
        "requests = {num_requests}, want the batch split across several"
    );
    assert_eq!(got_ids.len(), NUM_ROWS, "rows received");
    for (i, id) in got_ids.iter().enumerate() {
        assert_eq!(
            *id,
            format!("row-{i:03}"),
            "row {i}: rows must arrive in order"
        );
    }
    assert_eq!(bp.drop_stats().append_failure_drops, 0);
    bp.close().await;
}

#[tokio::test(start_paused = true)]
async fn test_batch_processor_split_stops_at_failed_request() {
    let received: Arc<Mutex<Vec<AppendRowsRequest>>> = Arc::default();
    let r = Arc::clone(&received);
    let srv = ScriptedServer::new(
        Vec::new(),
        move |attempt, req| {
            r.lock().unwrap().push(req.clone());
            // Confirm the first request; reject every later one.
            if attempt >= 2 {
                return Err(status(Code::PermissionDenied, "scripted"));
            }
            Ok(AppendRowsResponse::default())
        },
        None,
    );
    let bp = new_error_classification_processor(&srv.server, false).await;
    // Each row is a request of its own under this limit.
    bp.set_max_append_bytes(estimate_row_bytes(&content_row("a").row));

    let err = bp
        .write_batch(
            &Context::background(),
            vec![content_row("a"), content_row("b"), content_row("c")],
        )
        .await
        .expect_err("writeBatch() error = nil, want the failure of the second request");
    assert_eq!(status_code(&err), Code::PermissionDenied, "({err})");
    let reqs = received.lock().unwrap().clone();
    let got: Vec<String> = reqs
        .iter()
        .enumerate()
        .map(|(i, req)| {
            let rows = decode_arrow_rows(req);
            assert_eq!(
                rows.len(),
                1,
                "request {} carries {} rows, want 1",
                i + 1,
                rows.len()
            );
            event_id(&rows[0])
        })
        .collect();
    // a is confirmed; b is rejected on the reused stream, on the first fresh stream, and on
    // the bounded fresh-stream retry (stopping before the final retry); c is never sent.
    assert_eq!(
        got,
        vec!["a", "b", "b", "b"],
        "requests received: the third request must not be sent"
    );
    assert_eq!(
        bp.drop_stats().append_failure_drops,
        2,
        "AppendFailureDrops (the rows after the confirmed first request)"
    );
    bp.close().await;
}

#[tokio::test(start_paused = true)]
async fn test_in_band_resend_then_permanent_status() {
    let ctx = Context::background();
    for exactly_once in [false, true] {
        // stream_opened_by_this_request
        {
            let label = format!("exactlyOnce={exactly_once}/stream_opened_by_this_request");
            let srv = ScriptedServer::new(
                Vec::new(),
                |attempt, _| {
                    if attempt == 1 {
                        return Ok(in_band(Code::Unavailable, "transient in-band"));
                    }
                    Err(status(Code::PermissionDenied, "permission denied"))
                },
                None,
            );
            let bp = new_error_classification_processor(&srv.server, exactly_once).await;
            let err = bp
                .write_batch(&ctx, vec![super::support::event_row("X")])
                .await
                .expect_err("want an error");
            assert_eq!(status_code(&err), Code::PermissionDenied, "{label} ({err})");
            assert_eq!(
                srv.attempts(),
                3,
                "{label}: attempts (in-band transient on attempt 1 + stream-level PermissionDenied on attempt 2 + one fresh-stream retry on attempt 3)"
            );
            assert_eq!(bp.drop_stats().append_failure_drops, 1, "{label}");
            bp.close().await;
        }

        // stream_reused_from_earlier_request
        {
            let label = format!("exactlyOnce={exactly_once}/stream_reused_from_earlier_request");
            let srv = ScriptedServer::new(
                Vec::new(),
                |attempt, _| match attempt {
                    // The first request opens the stream and succeeds.
                    1 => Ok(AppendRowsResponse::default()),
                    // The second reuses that stream and gets an in-band transient error.
                    2 => Ok(in_band(
                        Code::Unavailable,
                        "transient in-band on reused stream",
                    )),
                    _ => Err(status(Code::PermissionDenied, "permission denied")),
                },
                None,
            );
            let bp = new_error_classification_processor(&srv.server, exactly_once).await;
            if let Err(e) = bp
                .write_batch(&ctx, vec![super::support::event_row("SEED")])
                .await
            {
                panic!("{label}: seed writeBatch() error = {e}, want nil");
            }
            let err = bp
                .write_batch(&ctx, vec![super::support::event_row("X")])
                .await
                .expect_err("want an error");
            assert_eq!(status_code(&err), Code::PermissionDenied, "{label} ({err})");
            assert_eq!(
                srv.attempts() - 1,
                4,
                "{label}: second request attempts (in-band transient on reused stream + PermissionDenied on reused stream + fresh-stream attempt + one bounded fresh-stream retry)"
            );
            assert_eq!(bp.drop_stats().append_failure_drops, 1, "{label}");
            bp.close().await;
        }
    }
}

#[tokio::test(start_paused = true)]
async fn test_stream_writer_append_uses_pre_serialized_batch_bytes() {
    let captured: Arc<Mutex<Vec<AppendRowsRequest>>> = Arc::default();
    let c = Arc::clone(&captured);
    let srv = ScriptedServer::new(
        Vec::new(),
        move |_, req| {
            c.lock().unwrap().push(req.clone());
            Ok(AppendRowsResponse::default())
        },
        None,
    );
    let bp = new_error_classification_processor(&srv.server, false).await;

    let pre_record = bp
        .build_record_batch(&[qrow(&[("event_id", "pre-serialized".into())])])
        .unwrap();
    let pre_bytes = serialize_record(&pre_record, None).unwrap_or_default();
    assert!(
        !pre_bytes.is_empty(),
        "serializeRecord(preRecord) returned empty bytes"
    );
    let fallback_record = bp
        .build_record_batch(&[qrow(&[("event_id", "fallback-record".into())])])
        .unwrap();

    let ctx = Context::background();
    if let Err(e) = bp
        .stream_writer
        .append(&ctx, &fallback_record, Some(pre_bytes), &mut false)
        .await
    {
        panic!("append with preBytes error = {e}, want nil");
    }
    if let Err(e) = bp
        .stream_writer
        .append(&ctx, &fallback_record, None, &mut false)
        .await
    {
        panic!("append with nil batchBytes error = {e}, want nil");
    }

    let reqs = captured.lock().unwrap().clone();
    assert_eq!(reqs.len(), 2, "captured requests");
    assert_eq!(
        event_id(&decode_arrow_rows(&reqs[0])[0]),
        "pre-serialized",
        "first request event_id (pre-serialized batchBytes must not be re-serialized)"
    );
    assert_eq!(
        event_id(&decode_arrow_rows(&reqs[1])[0]),
        "fallback-record",
        "second request event_id"
    );
    bp.close().await;
}

#[tokio::test(start_paused = true)]
async fn test_write_run_end_batch_drops_only_unconfirmed_rows() {
    for retry in [false, true] {
        let label = format!("retry={retry}");
        let srv = ScriptedServer::new(
            Vec::new(),
            |attempt, _| {
                // Confirm the first request; reject every later one.
                if attempt >= 2 {
                    return Err(status(Code::PermissionDenied, "scripted"));
                }
                Ok(AppendRowsResponse::default())
            },
            None,
        );
        let bp = new_error_classification_processor(&srv.server, false).await;
        let rows = vec![content_row("a"), content_row("b"), content_row("c")];
        // Each row is a request of its own under this limit.
        bp.set_max_append_bytes(estimate_row_bytes(&rows[0].row));
        if retry {
            bp.flush_mu.lock().await.deferred_batch = rows;
        } else {
            for r in rows {
                push_queue(&bp, r);
            }
        }

        bp.flush_run_end().await;

        // a is confirmed; b is rejected on the reused stream, on the first fresh stream, and on
        // the bounded fresh-stream retry (stopping before the final retry); c is never sent.
        assert_eq!(srv.attempts(), 4, "{label}: AppendRows attempts");
        // Counting the whole batch as dropped would report 3 here.
        assert_eq!(
            bp.drop_stats().append_failure_drops,
            2,
            "{label}: AppendFailureDrops (b and c; a was confirmed)"
        );
        assert_eq!(
            bp.flush_mu.lock().await.deferred_batch.len(),
            0,
            "{label}: len(deferredBatch): a failed write is dropped, not kept"
        );
        bp.close().await;
    }
}

/// A [`RowValue::Map`] nested `depth` levels deep under `key`.
fn nested_map(key: &str, depth: usize) -> RowValue {
    let mut v = RowValue::from("leaf");
    for _ in 0..depth {
        v = RowValue::Map([(key.to_owned(), v)].into_iter().collect());
    }
    v
}

// parity: adapted: a RowValue tree cannot be cyclic, so the ignored keys hold deeply nested maps instead of Go's self-references (no subprocess with a small stack is needed)
#[tokio::test(start_paused = true)]
async fn test_review_ignored_cyclic_metadata_does_not_crash() {
    let server = FakeWriteServer::new();
    let (tx, mut rx) = super::support::requests();
    super::support::recording_server(&server, tx);
    let cfg = Config {
        batch_size: 10,
        batch_flush_intv: Duration::from_secs(3600),
        ..Config::default()
    };
    let bp = processor(
        &Context::background(),
        Some(server.client().unwrap()),
        PDT_DEFAULT,
        cfg,
    )
    .await;
    bp.start();

    let mut object_ref = Row::new();
    object_ref.insert("uri".to_owned(), "gs://bucket/obj".into());
    object_ref.insert(
        "ignored_ref_cycle".to_owned(),
        nested_map("ignored_ref_cycle", 1000),
    );
    let mut part = Row::new();
    part.insert("text".to_owned(), "part-text".into());
    part.insert("object_ref".to_owned(), RowValue::Map(object_ref));
    part.insert(
        "ignored_part_cycle".to_owned(),
        nested_map("ignored_part_cycle", 1000),
    );
    let mut row = qrow(&[("event_type", "X".into())]);
    row.row.insert(
        "ignored_metadata".to_owned(),
        nested_map("ignored_metadata", 1000),
    );
    row.row
        .insert("content_parts".to_owned(), RowValue::Parts(vec![part]));

    bp.append(row);
    bp.append(qrow(&[("event_type", "Y".into())]));
    bp.close().await;

    let mut rows = Vec::new();
    while let Ok(req) = rx.try_recv() {
        rows.extend(decode_arrow_rows(&req));
    }
    assert_eq!(rows.len(), 2, "received rows");
}

#[tokio::test(start_paused = true)]
async fn test_stream_writer_in_band_rejection_does_not_introduce_ambiguity() {
    let ctx = Context::background();
    // in_band_rejection_then_already_exists_recognizes_unconfirmed_offset_conflict
    {
        let srv = ScriptedServer::new(
            Vec::new(),
            |attempt, _| {
                if attempt == 1 {
                    return Ok(in_band(Code::Internal, "definite in-band rejection"));
                }
                Ok(in_band(Code::AlreadyExists, "offset 0 already occupied"))
            },
            None,
        );
        let bp = new_error_classification_processor(&srv.server, true).await;
        let result = bp
            .write_batch(
                &ctx,
                vec![
                    super::support::event_row("X"),
                    super::support::event_row("Y"),
                ],
            )
            .await;
        assert!(
            result.is_err(),
            "writeBatch() error = nil, want unconfirmed offset conflict failure"
        );
        assert_eq!(
            bp.stream_writer.stream_offset.load(Ordering::SeqCst),
            0,
            "streamOffset (neither request accepted the batch)"
        );
        assert_eq!(bp.drop_stats().append_failure_drops, 2);
        assert_eq!(
            srv.create_count(),
            2,
            "CreateWriteStream calls (initial stream + rotation after unconfirmed AlreadyExists)"
        );
        bp.close().await;
    }

    // not_found_rotation_followed_by_in_band_rejections_does_not_desync_or_arm_cooldown
    {
        let srv = ScriptedServer::new(
            Vec::new(),
            |attempt, _| match attempt {
                1 => Err(status(Code::NotFound, "stream expired")),
                2..=5 => Ok(in_band(
                    Code::Internal,
                    "definite in-band rejection on fresh stream",
                )),
                _ => Ok(AppendRowsResponse::default()),
            },
            None,
        );
        let bp = new_error_classification_processor(&srv.server, true).await;
        let result = bp
            .write_batch(&ctx, vec![super::support::event_row("BATCH_1")])
            .await;
        assert!(
            result.is_err(),
            "first writeBatch() error = nil, want failure after retries exhausted"
        );
        assert_eq!(
            bp.drop_stats().append_failure_drops,
            1,
            "AppendFailureDrops after first batch"
        );
        {
            let state = bp.stream_writer.state.lock().await;
            assert!(
                !state.offset_desynced,
                "offsetDesynced = true after definite in-band rejections, want false"
            );
            assert!(
                state.rotate_retry_at.is_none(),
                "rotateRetryAt = {:?}, want zero (no cooldown after definite in-band rejections)",
                state.rotate_retry_at
            );
        }
        if let Err(e) = bp
            .write_batch(&ctx, vec![super::support::event_row("BATCH_2")])
            .await
        {
            panic!("second writeBatch() error = {e}, want nil on healthy rotated stream");
        }
        assert_eq!(
            bp.stream_writer.stream_offset.load(Ordering::SeqCst),
            1,
            "streamOffset after second batch"
        );
        assert_eq!(
            srv.create_count(),
            2,
            "CreateWriteStream calls (no extra rotation for second batch)"
        );
        bp.close().await;
    }
}

// parity: adapted: requests are captured and answered client-side (stream interceptor), since the fake server cannot raise its 4 MiB receive limit as Go's grpc.MaxRecvMsgSize(64 MiB) does
#[tokio::test(start_paused = true)]
async fn test_batch_processor_splits_sparse_content_parts_and_enforces_serialized_cap() {
    const NUM_ROWS: usize = 60;
    const PARTS_PER_ROW: usize = 10000;
    const BIG_QUERY_REQUEST_LIMIT: usize = 10 * 1024 * 1024;
    let sparse_parts = RowValue::Parts(vec![Row::new(); PARTS_PER_ROW]);
    let batch = |prefix: &str| -> Vec<QueuedRow> {
        (0..NUM_ROWS)
            .map(|i| {
                qrow(&[
                    ("event_id", format!("{prefix}-{i:03}").into()),
                    ("event_type", "X".into()),
                    ("content_parts", sparse_parts.clone()),
                ])
            })
            .collect()
    };
    let ctx = Context::background();

    // writeBatch_splits_sparse_content_parts_under_10MiB
    {
        let server = FakeWriteServer::new();
        let (client, captured) = capturing_client(&server);
        let mut cfg = Config::default();
        cfg.retry_config.max_retries = 0;
        let bp = processor(&ctx, Some(client), PDT_DEFAULT, cfg).await;
        bp.write_batch(&ctx, batch("sparse"))
            .await
            .unwrap_or_else(|e| panic!("writeBatch() error = {e}"));
        let (num_requests, got_ids) = captured_ids(&captured, BIG_QUERY_REQUEST_LIMIT);
        assert!(
            num_requests >= 3,
            "requests = {num_requests}, want sparse batch split across at least 3 requests"
        );
        assert_eq!(got_ids.len(), NUM_ROWS, "rows received");
        for (i, id) in got_ids.iter().enumerate() {
            assert_eq!(*id, format!("sparse-{i:03}"), "row {i}");
        }
        bp.close().await;
    }

    // appendChunk_enforces_serialized_request_cap_directly
    {
        let server = FakeWriteServer::new();
        let (client, captured) = capturing_client(&server);
        let mut cfg = Config::default();
        cfg.retry_config.max_retries = 0;
        let bp = processor(&ctx, Some(client), PDT_DEFAULT, cfg).await;
        // Calling append_chunk directly bypasses split_batch_by_estimated_bytes and exercises
        // the post-serialization halving fallback.
        bp.append_chunk(&ctx, &batch("direct"))
            .await
            .unwrap_or_else(|e| panic!("appendChunk() error = {e}"));
        let (num_requests, got_ids) = captured_ids(&captured, BIG_QUERY_REQUEST_LIMIT);
        assert!(
            num_requests >= 3,
            "requests = {num_requests}, want appendChunk fallback to split across at least 3 requests"
        );
        assert_eq!(got_ids.len(), NUM_ROWS, "rows received");
        bp.close().await;
    }
}

/// How a split path writes a batch: `(setup, uses_write_batch)`.
#[derive(Clone, Copy)]
enum SplitPath {
    /// Each row is a request of its own under the estimated limit (through `write_batch`).
    EstimatedViaWriteBatch,
    /// Every multi-row request exceeds the serialized limit (through `append_chunk`).
    HalvingViaAppendChunk,
}

impl SplitPath {
    fn name(self) -> &'static str {
        match self {
            SplitPath::EstimatedViaWriteBatch => "estimated_split_via_writeBatch",
            SplitPath::HalvingViaAppendChunk => "recursive_halving_via_appendChunk",
        }
    }

    fn setup(self, bp: &BatchProcessor) {
        match self {
            SplitPath::EstimatedViaWriteBatch => {
                bp.set_max_append_bytes(estimate_row_bytes(&content_row("a").row));
            }
            SplitPath::HalvingViaAppendChunk => bp.set_max_serialized_append_bytes(1),
        }
    }

    async fn write(self, bp: &BatchProcessor, batch: Vec<QueuedRow>) -> Result<(), Error> {
        match self {
            SplitPath::EstimatedViaWriteBatch => {
                bp.write_batch(&Context::background(), batch).await
            }
            SplitPath::HalvingViaAppendChunk => {
                bp.append_chunk(&Context::background(), &batch).await
            }
        }
    }

    /// Whether the entry point records `DropStats.append_failure_drops`.
    fn counts_drops(self) -> bool {
        matches!(self, SplitPath::EstimatedViaWriteBatch)
    }
}

/// A scripted server recording the row count of every request.
fn row_counting_server(
    on_append: impl Fn(usize) -> Result<AppendRowsResponse, tonic::Status> + Send + Sync + 'static,
) -> (ScriptedServer, Arc<Mutex<Vec<i64>>>) {
    let rows: Arc<Mutex<Vec<i64>>> = Arc::default();
    let r = Arc::clone(&rows);
    let srv = ScriptedServer::new(
        Vec::new(),
        move |attempt, req| {
            r.lock().unwrap().push(row_count(req));
            on_append(attempt)
        },
        None,
    );
    (srv, rows)
}

// parity: adapted: Go lowers MaxRetries after construction (bp.config / bp.streamWriter.config); here the entry-rotation processor is built with MaxRetries 0
#[tokio::test(start_paused = true)]
async fn test_batch_processor_split_batch_shares_exactly_once_rotation_budget() {
    let batch = || vec![content_row("a"), content_row("b"), content_row("c")];
    for path in [
        SplitPath::EstimatedViaWriteBatch,
        SplitPath::HalvingViaAppendChunk,
    ] {
        // second_chunk_offset_error_after_first_chunk_rotated_does_not_rotate_again
        {
            let label = format!(
                "{}/second_chunk_offset_error_after_first_chunk_rotated_does_not_rotate_again",
                path.name()
            );
            let (srv, rows) = row_counting_server(|attempt| match attempt {
                // Chunk 1, attempt 1 on committed-1: the batch's one allowed rotation.
                1 => Err(status(Code::NotFound, "stream 1 expired")),
                // Chunk 1, attempt 2 on committed-2: succeeds at offset 0.
                2 => Ok(AppendRowsResponse::default()),
                // Chunk 2, attempt 1 on committed-2: NotFound after chunk 1 already rotated.
                3 => Err(status(Code::NotFound, "stream 2 expired")),
                _ => Ok(AppendRowsResponse::default()),
            });
            let bp = new_error_classification_processor(&srv.server, true).await;
            path.setup(&bp);

            let err = path.write(&bp, batch()).await.expect_err("want an error");
            assert!(
                err.to_string()
                    .contains("committed write stream already rotated"),
                "{label}: write error = {err}, want already-rotated error on second chunk"
            );
            assert_eq!(
                srv.attempts(),
                3,
                "{label}: AppendRows attempts (third chunk must not be sent)"
            );
            assert_eq!(
                srv.create_count(),
                2,
                "{label}: CreateWriteStream calls (initial stream + at most 1 rotation per batch)"
            );
            assert_eq!(
                srv.append_offsets(),
                vec![0, 0, 1],
                "{label}: append offsets"
            );
            assert_eq!(
                *rows.lock().unwrap(),
                vec![1, 1, 1],
                "{label}: rows per AppendRows request"
            );
            // The first chunk was confirmed, so only the two rows after it are dropped.
            if path.counts_drops() {
                assert_eq!(bp.drop_stats().append_failure_drops, 2, "{label}");
            }
            bp.close().await;
        }

        // single_rotation_on_first_chunk_allows_remaining_chunks_on_replacement_stream
        {
            let label = format!(
                "{}/single_rotation_on_first_chunk_allows_remaining_chunks_on_replacement_stream",
                path.name()
            );
            let (srv, rows) = row_counting_server(|attempt| {
                if attempt == 1 {
                    return Err(status(Code::NotFound, "stream 1 expired"));
                }
                Ok(AppendRowsResponse::default())
            });
            let bp = new_error_classification_processor(&srv.server, true).await;
            path.setup(&bp);

            if let Err(e) = path.write(&bp, batch()).await {
                panic!("{label}: write error = {e}, want nil");
            }
            assert_eq!(
                srv.attempts(),
                4,
                "{label}: AppendRows attempts (1 failed on stream 1 + 3 chunks on stream 2)"
            );
            assert_eq!(srv.create_count(), 2, "{label}: CreateWriteStream calls");
            assert_eq!(
                srv.append_offsets(),
                vec![0, 0, 1, 2],
                "{label}: append offsets"
            );
            assert_eq!(
                *rows.lock().unwrap(),
                vec![1, 1, 1, 1],
                "{label}: rows per AppendRows request"
            );
            bp.close().await;
        }

        // entry_rotation_on_first_chunk_consumes_batch_rotation_budget
        {
            let label = format!(
                "{}/entry_rotation_on_first_chunk_consumes_batch_rotation_budget",
                path.name()
            );
            let srv = ScriptedServer::new(
                Vec::new(),
                |attempt, _| match attempt {
                    // The first batch fails on Recv with an ambiguous transport error, leaving
                    // the offset desynced.
                    1 => Err(status(Code::Unavailable, "transport lost after send")),
                    // The second batch, chunk 1: rotated on entry (consuming the batch's
                    // rotation budget) and succeeds on committed-2.
                    2 => Ok(AppendRowsResponse::default()),
                    // The second batch, chunk 2: NotFound on committed-2 must not rotate a
                    // second time in the same batch.
                    3 => Err(status(Code::NotFound, "stream 2 expired")),
                    _ => Ok(AppendRowsResponse::default()),
                },
                None,
            );
            let mut config = exactly_once_test_config();
            config.retry_config = RetryConfig {
                max_retries: 0,
                initial_delay: Duration::from_millis(1),
                max_delay: Duration::from_millis(2),
                multiplier: 1.0,
            };
            let bp = processor(
                &Context::background(),
                Some(srv.server.client().unwrap()),
                "",
                config,
            )
            .await;
            path.setup(&bp);

            let result = bp
                .write_batch(&Context::background(), vec![content_row("seed")])
                .await;
            assert!(
                result.is_err(),
                "{label}: first writeBatch() error = nil, want transport error"
            );
            assert!(
                bp.stream_writer.state.lock().await.offset_desynced,
                "{label}: offsetDesynced = false after ambiguous first batch failure, want true"
            );

            let err = path
                .write(&bp, vec![content_row("a"), content_row("b")])
                .await
                .expect_err("want an error");
            assert!(
                err.to_string()
                    .contains("committed write stream already rotated"),
                "{label}: second batch write error = {err}, want already-rotated error on second chunk"
            );
            assert_eq!(srv.attempts(), 3, "{label}: AppendRows attempts");
            assert_eq!(
                srv.create_count(),
                2,
                "{label}: CreateWriteStream calls (initial stream + 1 entry rotation on chunk 1)"
            );
            if path.counts_drops() {
                assert_eq!(
                    bp.drop_stats().append_failure_drops,
                    2,
                    "{label}: AppendFailureDrops (1 from first batch + 1 unconfirmed chunk from second batch)"
                );
            }
            bp.close().await;
        }
    }
}

#[tokio::test(start_paused = true)]
async fn test_batch_processor_deferred_retry_skips_confirmed_split_prefix() {
    #[derive(Clone, Copy)]
    enum Setup {
        MaxAppendBytes,
        MaxSerializedAppendBytes,
    }
    for (name, n_rows, setup) in [
        // Each row is a request of its own under this estimated limit.
        ("estimated_split", 2, Setup::MaxAppendBytes),
        // The batch is a single estimated chunk, which serializes to more than this limit,
        // so it is halved into [ev-0] and [ev-1].
        (
            "recursive_serialized_halving",
            2,
            Setup::MaxSerializedAppendBytes,
        ),
        // The batch is halved into [ev-0 ev-1] and [ev-2 ev-3 ev-4], and the first half into
        // [ev-0] and [ev-1], whose request is the one that fails.
        (
            "recursive_serialized_halving_progress_in_first_half",
            5,
            Setup::MaxSerializedAppendBytes,
        ),
    ] {
        let ids: Vec<String> = (0..n_rows).map(|i| format!("ev-{i}")).collect();
        // Only the first row is confirmed in the first flush.
        let kept: Vec<String> = ids[1..].to_vec();

        let release = CancellationToken::new();
        let acked: Arc<Mutex<Vec<AppendRowsRequest>>> = Arc::default();
        let rows_per_request: Arc<Mutex<Vec<i64>>> = Arc::default();
        let srv = ScriptedServer::new(Vec::new(), |_, _| Ok(AppendRowsResponse::default()), None);
        {
            // Go's onAppend blocks on the release, so it is an async AppendRows hook here,
            // recording into the scripted server's log as `scripted` does.
            let delivered = Arc::clone(&srv.delivered);
            let release = release.clone();
            let acked = Arc::clone(&acked);
            let rows_per_request = Arc::clone(&rows_per_request);
            srv.server
                .on_append_rows(Arc::new(move |mut stream: AppendRowsServerStream| {
                    let delivered = Arc::clone(&delivered);
                    let release = release.clone();
                    let acked = Arc::clone(&acked);
                    let rows_per_request = Arc::clone(&rows_per_request);
                    Box::pin(async move {
                        while let Some(req) = stream.recv().await? {
                            let attempt = {
                                let mut log = delivered.lock().unwrap();
                                log.push((
                                    req.write_stream.clone(),
                                    req.offset.map_or(-1, |o| o.value),
                                ));
                                log.len()
                            };
                            rows_per_request.lock().unwrap().push(row_count(&req));
                            if attempt == 2 {
                                // The request of the second row outlasts the run-end budget
                                // and is not committed.
                                tokio::select! {
                                    () = release.cancelled() => {}
                                    () = tokio::time::sleep(Duration::from_secs(30)) => {}
                                }
                                return Err(status(Code::Unavailable, "scripted: not committed"));
                            }
                            acked.lock().unwrap().push(req);
                            stream.send(AppendRowsResponse::default()).await?;
                        }
                        Ok(())
                    })
                }));
        }

        // In the first flush, the first upload of ev-0 holds the only slot for 400ms of the
        // ~1s that the uploads get and succeeds, so that the uploads are not paused; the second
        // upload of ev-0 and the uploads of the other rows are cut short by the deadline and
        // kept.
        let retrying = Arc::new(AtomicBool::new(false));
        let r = Arc::clone(&retrying);
        let off = Arc::new(HookOffloader::with_hook(Arc::new(
            move |ctx: Context, req: GcsUploadRequest| {
                let retrying = r.load(Ordering::SeqCst);
                Box::pin(async move {
                    if retrying {
                        return Ok(());
                    }
                    if req.path.contains("_ev-0_") && req.path.contains("_c0_") {
                        tokio::select! {
                            () = tokio::time::sleep(Duration::from_millis(400)) => return Ok(()),
                            () = ctx.done() => {
                                return Err(Error::Context(ctx.err().unwrap_or(crate::context::ContextError::Canceled)));
                            }
                        }
                    }
                    ctx.done().await;
                    Err(Error::Context(
                        ctx.err().unwrap_or(crate::context::ContextError::Canceled),
                    ))
                })
            },
        )));
        let mut cfg = exactly_once_test_config();
        cfg.batch_flush_intv = Duration::from_secs(3600);
        cfg.shutdown_timeout = Duration::from_secs(2);
        cfg.retry_config.max_retries = 0;
        cfg.gcs_offloader = Some(Arc::clone(&off) as Arc<dyn GcsContentOffloader>);
        cfg.gcs_max_concurrent_uploads = 1;
        cfg.logger = Some(Arc::new(RecordingLogger::default()) as Arc<dyn Logger>);
        let bp = processor(
            &Context::background(),
            Some(srv.server.client().unwrap()),
            "",
            cfg,
        )
        .await;
        match setup {
            Setup::MaxAppendBytes => bp.set_max_append_bytes(1),
            Setup::MaxSerializedAppendBytes => bp.set_max_serialized_append_bytes(1),
        }

        // The rows are queued directly, so that no background flush takes flush_mu before
        // flush_run_end does. ev-0 has a 3-byte and a 5-byte upload, and each other row a
        // 7-byte upload.
        let opts = PartsOptions {
            max_len: 1000,
            offload: true,
            inline_limit: 10,
            reserve_bytes: reserve_with(&bp),
            ..PartsOptions::default()
        };
        for (i, id) in ids.iter().enumerate() {
            let mut row = if i == 0 {
                queued_row(
                    &opts,
                    &[image_content(&[1, 2, 3]), image_content(&[4, 5, 6, 7, 8])],
                )
            } else {
                queued_row(&opts, &[image_content(&[1, 2, 3, 4, 5, 6, 7])])
            };
            row.row
                .insert("event_type".to_owned(), "LLM_REQUEST".into());
            row.row.insert("event_id".to_owned(), id.as_str().into());
            push_queue(&bp, row);
        }

        bp.flush_run_end().await;

        let got_kept: Vec<String> = bp
            .flush_mu
            .lock()
            .await
            .deferred_batch
            .iter()
            .map(|r| event_id(&r.row))
            .collect();
        assert_eq!(
            got_kept, kept,
            "{name}: deferredBatch event_ids after the first flushRunEnd (only the rows that BigQuery did not confirm are kept)"
        );
        assert!(
            bp.upload_cooldown_until.lock().unwrap().is_none(),
            "{name}: uploadCooldownUntil after the first flushRunEnd, want unset (uploads not paused)"
        );
        assert_eq!(
            bp.pending_upload_bytes.load(Ordering::SeqCst),
            7 * i64::try_from(kept.len()).unwrap(),
            "{name}: pendingUploadBytes after the first flushRunEnd (ev-0's 5-byte upload released, the 7-byte upload of each other row kept)"
        );
        assert_eq!(
            bp.drop_stats().total_drops,
            0,
            "{name}: DropStats after the first flushRunEnd = {:?}, want no drops",
            bp.drop_stats()
        );

        release.cancel();
        retrying.store(true, Ordering::SeqCst);
        let calls_before = off.uploads().len();
        bp.flush_run_end().await;

        assert_eq!(
            bp.flush_mu.lock().await.deferred_batch.len(),
            0,
            "{name}: deferredBatch length after the retry flushRunEnd"
        );
        assert_eq!(
            bp.pending_upload_bytes.load(Ordering::SeqCst),
            0,
            "{name}: pendingUploadBytes after the retry flushRunEnd"
        );
        assert_eq!(
            off.uploads().len() - calls_before,
            kept.len(),
            "{name}: retry flushRunEnd offloader calls (only the kept uploads of the rows that BigQuery had not confirmed)"
        );
        assert_eq!(
            bp.drop_stats().total_drops,
            0,
            "{name}: DropStats after the retry flushRunEnd = {:?}, want no drops",
            bp.drop_stats()
        );

        let committed: Vec<String> = acked
            .lock()
            .unwrap()
            .iter()
            .flat_map(|req| {
                decode_arrow_rows(req)
                    .iter()
                    .map(event_id)
                    .collect::<Vec<_>>()
            })
            .collect();
        assert_eq!(committed, ids, "{name}: committed rows");
        // The two requests of the first flush, the second of which was not committed, and one
        // request for each kept row.
        assert_eq!(
            *rows_per_request.lock().unwrap(),
            vec![1; 2 + kept.len()],
            "{name}: rows per AppendRows request"
        );
        // The retry replaces the stream, whose offset the request that the budget cut short left
        // unknown, and resends the kept rows from offset 0 of the new stream.
        assert_eq!(srv.create_count(), 2, "{name}: CreateWriteStream calls");
        let mut want_offsets = vec![0, 1];
        want_offsets.extend((0..kept.len()).map(|i| i64::try_from(i).unwrap()));
        assert_eq!(srv.append_offsets(), want_offsets, "{name}: append offsets");

        // Go's cleanups run last-in first-out: the blocked request is released before Close.
        release.cancel();
        bp.close().await;
    }
}
