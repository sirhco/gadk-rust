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

//! `batch_processor_test.go` ports, Go file order from `TestBatchProcessor_ExactlyOnceDelivery_B1AmbiguousExhaustedThenB2AmbiguousDoesNotLoseBatch` through `TestBatchProcessor_ConcurrentStartAndClose`.

use super::support::row_count;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gadk_platform::BoxFuture;
use pretty_assertions::assert_eq;
use tokio::sync::{Notify, oneshot};
use tokio::time::Instant;

use super::support::{
    HookOffloader, ScriptedServer, append_failure_stats, as_logger, committed, decode_arrow_rows,
    event_row, exactly_once_test_config, image_content, inject_stream, no_retry_config,
    pending_upload_bytes, processor, push_queue, queue_len, queued_row, recording_server, requests,
    reserve_with, status,
};
use crate::batch_processor::BatchProcessor;
use crate::bigquery::fake::RecordingLogger;
use crate::config::{Config, GcsContentOffloader, RetryConfig};
use crate::context::Context;
use crate::error::{Code, Error};
use crate::formatter::PartsOptions;
use crate::row::RowValue;
use crate::storagepb::{AppendRowsRequest, AppendRowsResponse, WriteStream};
use crate::testutil::fake_write_server::{AppendRowsServerStream, FakeWriteServer, UnaryCall};
use crate::write::stream_writer::StreamWriter;
use crate::write::stream_writer::tests::{Fail, MockCounts, MockStream, in_band};
use crate::write::{AppendRowsClient, eof};

/// Go `bp.streamWriter.now = func() time.Time { return now }` with a settable `now`.
async fn pin_clock(bp: &BatchProcessor) -> Arc<Mutex<Instant>> {
    let now = Arc::new(Mutex::new(Instant::now()));
    let clock = Arc::clone(&now);
    bp.stream_writer.state.lock().await.now = Some(Arc::new(move || *clock.lock().unwrap()));
    now
}

/// Go `now = now.Add(d)`.
fn advance(now: &Mutex<Instant>, d: Duration) {
    let mut now = now.lock().unwrap();
    *now += d;
}

/// The exactly-once test config with `max_retries` and millisecond backoff up to `max_delay`.
fn exactly_once_with_retries(max_retries: i64, max_delay: Duration) -> Config {
    Config {
        retry_config: RetryConfig {
            max_retries,
            initial_delay: Duration::from_millis(1),
            max_delay,
            multiplier: 1.0,
        },
        ..exactly_once_test_config()
    }
}

/// An async `onAppend` (Go's `scriptedCommittedStreamServer` whose handler may block): every
/// request is answered by `on_append(req)` or ends the stream with its status.
fn async_scripted(
    server: &FakeWriteServer,
    on_append: Arc<
        dyn Fn(AppendRowsRequest) -> BoxFuture<'static, Result<AppendRowsResponse, tonic::Status>>
            + Send
            + Sync,
    >,
) {
    server.on_append_rows(Arc::new(move |mut stream: AppendRowsServerStream| {
        let on_append = Arc::clone(&on_append);
        Box::pin(async move {
            while let Some(req) = stream.recv().await? {
                let res = on_append(req).await?;
                stream.send(res).await?;
            }
            Ok(())
        })
    }));
}

/// Go `replacementCreateServer`: the first CreateWriteStream is answered at once
/// (`committed-<n>`), each later one runs `replace` first and fails with its status. Returns
/// Go's `creates` counter.
fn replacement_create_server(
    server: &FakeWriteServer,
    replace: Arc<dyn Fn() -> BoxFuture<'static, Result<(), tonic::Status>> + Send + Sync>,
) -> Arc<AtomicUsize> {
    let creates = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&creates);
    server.on_create_write_stream(Arc::new(move |call: UnaryCall<_>| {
        let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
        let replace = Arc::clone(&replace);
        Box::pin(async move {
            if n > 1 {
                replace().await?;
            }
            Ok(WriteStream {
                name: committed(n),
                r#type: call.request.write_stream.as_ref().map_or(0, |w| w.r#type),
                ..WriteStream::default()
            })
        })
    }));
    creates
}

/// Go's `mockStream` with `sendDelay` and `onSend` (awaited before each send), over T8's
/// [`MockStream`].
struct HookedStream {
    inner: MockStream,
    send_delay: Duration,
    on_send: Option<Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>>,
}

#[async_trait::async_trait]
impl AppendRowsClient for HookedStream {
    async fn send(&mut self, req: AppendRowsRequest) -> Result<(), Error> {
        if let Some(on_send) = &self.on_send {
            on_send().await;
        }
        if !self.send_delay.is_zero() {
            tokio::time::sleep(self.send_delay).await;
        }
        self.inner.send(req).await
    }

    async fn recv(&mut self) -> Result<AppendRowsResponse, Error> {
        self.inner.recv().await
    }

    fn close_send(&mut self) {
        self.inner.close_send();
    }
}

/// Go `scriptedClientStream`: on the first intercepted open, Send succeeds (without reaching
/// the server) and Recv fails Unavailable; on later ones Send fails with `io.EOF` and Recv
/// with AlreadyExists.
struct ScriptedClientStream {
    inner: Box<dyn AppendRowsClient>,
    open_num: i32,
}

#[async_trait::async_trait]
impl AppendRowsClient for ScriptedClientStream {
    async fn send(&mut self, _req: AppendRowsRequest) -> Result<(), Error> {
        if self.open_num == 1 {
            return Ok(());
        }
        Err(eof())
    }

    async fn recv(&mut self) -> Result<AppendRowsResponse, Error> {
        if self.open_num == 1 {
            return Err(Fail::Status(Code::Unavailable, "ambiguous recv loss").error());
        }
        Err(Fail::Status(Code::AlreadyExists, "offset already committed").error())
    }

    fn close_send(&mut self) {
        self.inner.close_send();
    }
}

#[tokio::test(start_paused = true)]
async fn test_batch_processor_exactly_once_delivery_b1_ambiguous_exhausted_then_b2_ambiguous_does_not_lose_batch()
 {
    let ctx = Context::background();
    // Per-stream committed row counts model BigQuery's COMMITTED stream offset semantics (see
    // the Go test's comment for the five attempts).
    let committed_rows: Arc<Mutex<HashMap<String, i64>>> = Arc::default();
    let rows_log = Arc::clone(&committed_rows);
    let srv = ScriptedServer::new(
        Vec::new(),
        move |attempt, req| {
            let mut rows = rows_log.lock().unwrap();
            let name = req.write_stream.clone();
            let offset = req.offset.map_or(0, |o| o.value);
            let n = row_count(req);
            let already = offset < rows.get(&name).copied().unwrap_or(0);
            match attempt {
                1 => {
                    rows.insert(name, offset + n);
                    Err(status(
                        Code::Unavailable,
                        "connection lost after server commit of B1",
                    ))
                }
                2 => Err(status(
                    Code::Unavailable,
                    "transient network failure on B1 retry",
                )),
                3 => {
                    if already {
                        return Ok(in_band(Code::AlreadyExists, "offset already committed"));
                    }
                    rows.insert(name, offset + n);
                    Err(status(
                        Code::Unavailable,
                        "connection lost after server commit of B2",
                    ))
                }
                _ => {
                    if already {
                        return Ok(in_band(Code::AlreadyExists, "offset already committed"));
                    }
                    rows.insert(name, offset + n);
                    Ok(AppendRowsResponse::default())
                }
            }
        },
        None,
    );
    let config = exactly_once_with_retries(1, Duration::from_millis(5));
    let bp = processor(&ctx, Some(srv.server.client().unwrap()), "", config).await;

    // B1 (2 rows): exhausts retries after an ambiguous send.
    assert!(
        bp.write_batch(&ctx, vec![event_row("B1_ROW_1"), event_row("B1_ROW_2")])
            .await
            .is_err(),
        "writeBatch(B1) error = nil, want retry exhaustion"
    );
    // B2 (3 rows): rotates to committed-2 before sending so B2 is committed on committed-2.
    bp.write_batch(
        &ctx,
        vec![
            event_row("B2_ROW_1"),
            event_row("B2_ROW_2"),
            event_row("B2_ROW_3"),
        ],
    )
    .await
    .unwrap_or_else(|e| panic!("writeBatch(B2) error = {e}, want nil"));
    // B3 (1 row): must append at offset 3 on committed-2.
    bp.write_batch(&ctx, vec![event_row("B3_ROW_1")])
        .await
        .unwrap_or_else(|e| panic!("writeBatch(B3) error = {e}, want nil"));

    assert_eq!(
        committed_rows
            .lock()
            .unwrap()
            .get(&committed(2))
            .copied()
            .unwrap_or(0),
        4,
        "committed-2 total rows, want 4 (3 from B2 + 1 from B3)"
    );
    assert_eq!(
        srv.append_offsets(),
        vec![0, 0, 0, 0, 3],
        "AppendRows offsets"
    );
    bp.close().await;
}

#[tokio::test(start_paused = true)]
async fn test_batch_processor_exactly_once_delivery_rotation_cooldown_and_guards() {
    let ctx = Context::background();

    // rejects non-empty or _default streamName when ExactlyOnceDelivery is enabled
    {
        let cfg = Config {
            exactly_once_delivery: true,
            ..Config::default()
        };
        assert!(
            BatchProcessor::new(
                &ctx,
                None,
                "projects/p/datasets/d/tables/t/streams/_default",
                cfg
            )
            .await
            .is_err(),
            "NewBatchProcessor() with _default streamName and ExactlyOnceDelivery=true succeeded, want error"
        );
    }

    // Send failure followed by AlreadyExists rotates stream instead of advancing offset
    {
        let srv = ScriptedServer::new(Vec::new(), |_, _| Ok(AppendRowsResponse::default()), None);
        let cfg = exactly_once_with_retries(1, Duration::from_millis(5));
        let bp = processor(&ctx, Some(srv.server.client().unwrap()), "", cfg).await;
        // A stream whose Send fails with io.EOF and whose Recv surfaces AlreadyExists
        // (send_succeeded = false).
        inject_stream(
            &bp,
            MockStream {
                send_err: Some(Fail::Eof),
                recv_err: Some(Fail::Status(
                    Code::AlreadyExists,
                    "offset 0 already exists on send failure",
                )),
                ..MockStream::default()
            },
        )
        .await;
        bp.write_batch(&ctx, vec![event_row("ROW_1"), event_row("ROW_2")])
            .await
            .unwrap_or_else(|e| {
                panic!("writeBatch() error = {e}, want nil after rotating to committed-2")
            });
        assert_eq!(
            srv.create_count(),
            2,
            "CreateWriteStream calls, want 2 (initial + rotation after send-path AlreadyExists)"
        );
        assert_eq!(
            srv.append_stream_names(),
            vec![committed(2)],
            "AppendRows streams"
        );
        bp.close().await;
    }

    // Send failure with AlreadyExists after ambiguous send confirms batch and advances offset,
    // and rotation errors on Send or in-band response return immediately
    {
        let fail_create = Arc::new(AtomicBool::new(false));
        let intercept_send_mode = Arc::new(AtomicI32::new(0));
        let stream_open_count = Arc::new(AtomicI32::new(0));
        let srv = ScriptedServer::new(
            Vec::new(),
            |_, _| Ok(in_band(Code::OutOfRange, "in-band offset out of range")),
            None,
        );
        // Go's unary server interceptor failing CreateWriteStream on demand.
        let fail = Arc::clone(&fail_create);
        let names = Arc::new(AtomicUsize::new(0));
        srv.server
            .on_create_write_stream(Arc::new(move |call: UnaryCall<_>| {
                // Go's interceptor rejects before the handler, which names streams by the
                // calls it handled.
                let fail = fail.load(Ordering::SeqCst);
                let names = Arc::clone(&names);
                Box::pin(async move {
                    if fail {
                        return Err(status(Code::PermissionDenied, "rotation failed"));
                    }
                    let n = names.fetch_add(1, Ordering::SeqCst) + 1;
                    Ok(WriteStream {
                        name: committed(n),
                        r#type: call.request.write_stream.as_ref().map_or(0, |w| w.r#type),
                        ..WriteStream::default()
                    })
                })
            }));
        // Go's client stream interceptor wrapping streams in scriptedClientStream.
        let mode = Arc::clone(&intercept_send_mode);
        let opens = Arc::clone(&stream_open_count);
        let client = srv
            .server
            .client()
            .unwrap()
            .with_stream_interceptor(Arc::new(move |_ctx, open| {
                let mode = Arc::clone(&mode);
                let opens = Arc::clone(&opens);
                Box::pin(async move {
                    let cs = open.await?;
                    if mode.load(Ordering::SeqCst) == 0 {
                        return Ok(cs);
                    }
                    let open_num = opens.fetch_add(1, Ordering::SeqCst) + 1;
                    Ok(Box::new(ScriptedClientStream {
                        inner: cs,
                        open_num,
                    }) as Box<dyn AppendRowsClient>)
                })
            }));
        let cfg = exactly_once_with_retries(2, Duration::from_millis(2));
        let bp = processor(&ctx, Some(client), "", cfg).await;

        // 1. Send-path AlreadyExists confirmation after a prior ambiguous send: stream #1's
        // Send succeeds and Recv returns Unavailable (an ambiguous send); stream #2's Send
        // returns io.EOF and the drain surfaces AlreadyExists, which confirms the batch.
        intercept_send_mode.store(1, Ordering::SeqCst);
        bp.write_batch(&ctx, vec![event_row("R1"), event_row("R2")])
            .await
            .unwrap_or_else(|e| {
                panic!(
                    "writeBatch() after Send-path AlreadyExists confirmation error = {e}, want nil"
                )
            });
        assert_eq!(
            bp.stream_writer.stream_offset.load(Ordering::SeqCst),
            2,
            "streamOffset after Send-path AlreadyExists confirmation"
        );
        intercept_send_mode.store(0, Ordering::SeqCst);
        StreamWriter::reset_stream(&mut *bp.stream_writer.state.lock().await);

        // 2. Send-path rotation error return: Send fails with io.EOF + OutOfRange while
        // CreateWriteStream fails.
        fail_create.store(true, Ordering::SeqCst);
        {
            let mut state = bp.stream_writer.state.lock().await;
            state.rotate_retry_at = None;
            state.freshly_rotated = false;
            state.stream = Some(Box::new(MockStream {
                send_err: Some(Fail::Eof),
                recv_err: Some(Fail::Status(Code::OutOfRange, "send-path out of range")),
                ..MockStream::default()
            }));
        }
        match bp.write_batch(&ctx, vec![event_row("R3")]).await {
            Err(e)
                if e.to_string()
                    .contains("failed to rotate committed write stream") => {}
            other => panic!(
                "writeBatch() on Send-path rotation failure = {other:?}, want rotation error"
            ),
        }

        // 3. In-band response error path rotation error return: Recv returns in-band
        // OutOfRange while CreateWriteStream fails.
        {
            let mut state = bp.stream_writer.state.lock().await;
            state.offset_desynced = false;
            state.rotate_retry_at = None;
            state.freshly_rotated = false;
        }
        match bp.write_batch(&ctx, vec![event_row("R4")]).await {
            Err(e)
                if e.to_string()
                    .contains("failed to rotate committed write stream") => {}
            other => {
                panic!("writeBatch() on in-band rotation failure = {other:?}, want rotation error")
            }
        }
        bp.close().await;
    }

    // rotates at most once per batch, attaches x-goog-request-params header, and enforces
    // cooldown when rotation or freshly rotated stream fails
    {
        let create_calls = Arc::new(AtomicUsize::new(0));
        let fail_create = Arc::new(AtomicBool::new(false));
        let initial_create_has_deadline = Arc::new(AtomicBool::new(false));
        let srv = ScriptedServer::new(
            Vec::new(),
            |_, req| {
                // committed-1 and the freshly rotated committed-2 both fail with OutOfRange.
                if req.write_stream.ends_with("/committed-1")
                    || req.write_stream.ends_with("/committed-2")
                {
                    return Err(status(Code::OutOfRange, "offset out of range"));
                }
                Ok(AppendRowsResponse::default())
            },
            None,
        );
        let (calls, fail, deadline) = (
            Arc::clone(&create_calls),
            Arc::clone(&fail_create),
            Arc::clone(&initial_create_has_deadline),
        );
        // Go's handler names streams by the calls it handled (the interceptor rejects first).
        let handled = Arc::new(AtomicUsize::new(0));
        srv.server
            .on_create_write_stream(Arc::new(move |call: UnaryCall<_>| {
                let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
                if n == 1 {
                    deadline.store(call.timeout.is_some(), Ordering::SeqCst);
                }
                let fail = fail.load(Ordering::SeqCst);
                let handled = Arc::clone(&handled);
                Box::pin(async move {
                    if fail {
                        return Err(status(
                            Code::PermissionDenied,
                            "quota or permission failure",
                        ));
                    }
                    let n = handled.fetch_add(1, Ordering::SeqCst) + 1;
                    Ok(WriteStream {
                        name: committed(n),
                        r#type: call.request.write_stream.as_ref().map_or(0, |w| w.r#type),
                        ..WriteStream::default()
                    })
                })
            }));
        let cfg = exactly_once_with_retries(3, Duration::from_millis(5));
        let bp = processor(&ctx, Some(srv.server.client().unwrap()), "", cfg).await;

        assert!(
            initial_create_has_deadline.load(Ordering::SeqCst),
            "initial CreateWriteStream context had no deadline, want bounded by defaultRotateTimeout"
        );

        let now = pin_clock(&bp).await;
        bp.stream_writer.state.lock().await.rotate_timeout = Duration::from_secs(30);

        // Batch 1: attempt 1 on committed-1 gets OutOfRange and rotates once to committed-2;
        // attempt 2 on the freshly rotated committed-2 gets OutOfRange, aborts without a second
        // rotation, and arms the 30s cooldown.
        assert!(
            bp.write_batch(&ctx, vec![event_row("B1")]).await.is_err(),
            "writeBatch(B1) error = nil, want second-rotation abort error"
        );
        assert_eq!(
            create_calls.load(Ordering::SeqCst),
            2,
            "CreateWriteStream calls after B1, want 2 (1 initial + at most 1 rotation per batch)"
        );

        // Batch 1b within the 30s cooldown: fails fast without CreateWriteStream.
        advance(&now, Duration::from_secs(10));
        assert!(
            bp.write_batch(&ctx, vec![event_row("B1b")]).await.is_err(),
            "writeBatch(B1b) error = nil, want cooldown error after freshly rotated stream failed"
        );
        assert_eq!(
            create_calls.load(Ordering::SeqCst),
            2,
            "CreateWriteStream calls during freshlyRotated cooldown"
        );

        // Past the first cooldown with CreateWriteStream failing: Batch 2 attempts rotation,
        // fails CreateWriteStream, and arms a new 30s cooldown.
        advance(&now, Duration::from_secs(25));
        fail_create.store(true, Ordering::SeqCst);
        assert!(
            bp.write_batch(&ctx, vec![event_row("B2")]).await.is_err(),
            "writeBatch(B2) error = nil, want CreateWriteStream error"
        );
        assert_eq!(
            create_calls.load(Ordering::SeqCst),
            3,
            "CreateWriteStream calls after B2"
        );

        // Batch 3 within the cooldown: fails fast without CreateWriteStream.
        advance(&now, Duration::from_secs(10));
        assert!(
            bp.write_batch(&ctx, vec![event_row("B3")]).await.is_err(),
            "writeBatch(B3) error = nil, want cooldown error"
        );
        assert_eq!(
            create_calls.load(Ordering::SeqCst),
            3,
            "CreateWriteStream calls during cooldown, want 3 (no extra RPC during cooldown)"
        );

        // Past the cooldown with CreateWriteStream restored: Batch 4 rotates to committed-3
        // and succeeds.
        advance(&now, Duration::from_secs(25));
        fail_create.store(false, Ordering::SeqCst);
        bp.write_batch(&ctx, vec![event_row("B4")])
            .await
            .unwrap_or_else(|e| panic!("writeBatch(B4) after cooldown error = {e}, want nil"));
        assert_eq!(
            create_calls.load(Ordering::SeqCst),
            4,
            "CreateWriteStream calls after cooldown expiry"
        );

        let params: Vec<String> = srv
            .server
            .append_stream_metadata()
            .into_iter()
            .flat_map(|md| md.get("x-goog-request-params").cloned().unwrap_or_default())
            .collect();
        assert_eq!(
            params,
            vec![
                "write_stream=projects%2Ftest-proj%2Fdatasets%2Ftest-ds%2Ftables%2Fagent_events%2Fstreams%2Fcommitted-1".to_owned(),
                "write_stream=projects%2Ftest-proj%2Fdatasets%2Ftest-ds%2Ftables%2Fagent_events%2Fstreams%2Fcommitted-2".to_owned(),
                "write_stream=projects%2Ftest-proj%2Fdatasets%2Ftest-ds%2Ftables%2Fagent_events%2Fstreams%2Fcommitted-3".to_owned(),
            ],
            "AppendRows x-goog-request-params"
        );
        bp.close().await;
    }

    // hanging CreateWriteStream during rotation times out at rotateTimeout
    {
        let srv = ScriptedServer::new(
            Vec::new(),
            |_, _| Err(status(Code::NotFound, "stream expired")),
            None,
        );
        let calls = Arc::new(AtomicUsize::new(0));
        srv.server
            .on_create_write_stream(Arc::new(move |call: UnaryCall<_>| {
                let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
                Box::pin(async move {
                    if n >= 2 {
                        // Go: <-ctx.Done(); the client's deadline drops this call.
                        std::future::pending::<()>().await;
                    }
                    Ok(WriteStream {
                        name: committed(n),
                        r#type: call.request.write_stream.as_ref().map_or(0, |w| w.r#type),
                        ..WriteStream::default()
                    })
                })
            }));
        let mut cfg = exactly_once_test_config();
        cfg.retry_config.max_retries = 0;
        let bp = processor(&ctx, Some(srv.server.client().unwrap()), "", cfg).await;

        bp.stream_writer.state.lock().await.rotate_timeout = Duration::from_millis(15);
        let start = Instant::now();
        let result = bp
            .write_batch(&ctx, vec![event_row("ROTATE_TIMEOUT")])
            .await;
        let elapsed = start.elapsed();
        assert!(
            result.is_err(),
            "writeBatch() with hanging rotation CreateWriteStream succeeded, want timeout error"
        );
        assert!(
            elapsed < Duration::from_millis(150),
            "writeBatch() with rotateTimeout=15ms took {elapsed:?}, want < 150ms"
        );
        bp.close().await;
    }
}

#[tokio::test(start_paused = true)]
async fn test_batch_processor_exactly_once_delivery_hanging_create_write_stream_arms_cooldown() {
    let ctx = Context::background();
    let srv = ScriptedServer::new(
        Vec::new(),
        |_, _| Err(status(Code::OutOfRange, "offset beyond end")),
        None,
    );
    // Hang until the caller gives up.
    let creates = replacement_create_server(
        &srv.server,
        Arc::new(|| Box::pin(std::future::pending::<Result<(), tonic::Status>>())),
    );
    // As with the defaults (10s against 30s), the write deadline is shorter than the rotation
    // timeout, so a hanging CreateWriteStream ends at the write deadline.
    let config = Config {
        shutdown_timeout: Duration::from_millis(300),
        ..exactly_once_test_config()
    };
    let bp = processor(&ctx, Some(srv.server.client().unwrap()), "", config).await;

    for _ in 0..3 {
        let _ = bp.write_batch(&ctx, vec![event_row("LLM_REQUEST")]).await;
    }
    assert_eq!(
        creates.load(Ordering::SeqCst),
        2,
        "CreateWriteStream calls after 3 batches while it hangs, want 2 (initial stream + first replacement that starts the cooldown)"
    );
    bp.close().await;
}

#[tokio::test(start_paused = true)]
async fn test_batch_processor_exactly_once_delivery_rotation_on_depleted_budget() {
    type Replace = Arc<dyn Fn() -> BoxFuture<'static, Result<(), tonic::Status>> + Send + Sync>;
    let stream1 = committed(1);
    let cases: [(&str, Replace, usize, bool, i64, i64, usize); 2] = [
        (
            "healthy server cut short by the write deadline",
            // Slower than the budget left after the rejection, faster than a full one (the
            // call is dropped when the caller's deadline passes, Go's ctx.Done()).
            Arc::new(|| {
                Box::pin(async {
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    Ok(())
                })
            }),
            4,
            false,
            7,
            0,
            3,
        ),
        (
            "server rejects the call in time",
            Arc::new(|| {
                Box::pin(async { Err(status(Code::PermissionDenied, "permission denied")) })
            }),
            0,
            true,
            0,
            7,
            2,
        ),
    ];
    for (name, replace, want_deferred, want_cooldown, want_delivered, want_drops, want_creates) in
        cases
    {
        let delivered = Arc::new(std::sync::atomic::AtomicI64::new(0));
        let server = FakeWriteServer::new();
        let (count, first) = (Arc::clone(&delivered), stream1.clone());
        async_scripted(
            &server,
            Arc::new(move |req: AppendRowsRequest| {
                let count = Arc::clone(&count);
                let first = first.clone();
                Box::pin(async move {
                    if req.write_stream == first {
                        // Rejected after 320ms of the 450ms write budget.
                        tokio::time::sleep(Duration::from_millis(320)).await;
                        return Err(status(Code::OutOfRange, "offset beyond end"));
                    }
                    count.fetch_add(row_count(&req), Ordering::SeqCst);
                    Ok(AppendRowsResponse::default())
                })
            }),
        );
        let creates = replacement_create_server(&server, replace);
        let config = Config {
            batch_size: 4,
            batch_flush_intv: Duration::from_secs(3600),
            shutdown_timeout: Duration::from_millis(450),
            ..exactly_once_test_config()
        };
        let bp = processor(
            &Context::background(),
            Some(server.client().unwrap()),
            "",
            config,
        )
        .await;

        for i in 0..4 {
            push_queue(&bp, event_row(&format!("RUN_END_{i}")));
        }
        // The rejection leaves about 130ms of the run-end budget for the replacement
        // CreateWriteStream.
        bp.flush_run_end().await;
        assert_eq!(
            bp.flush_mu.lock().await.deferred_batch.len(),
            want_deferred,
            "{name}: deferredBatch length after the run-end flush"
        );
        assert_eq!(
            bp.stream_writer.is_rotation_in_cooldown().await,
            want_cooldown,
            "{name}: rotation paused after the run-end flush"
        );

        for i in 0..3 {
            push_queue(&bp, event_row(&format!("LATER_{i}")));
        }
        bp.flush().await;
        bp.flush_run_end().await;

        assert_eq!(
            delivered.load(Ordering::SeqCst),
            want_delivered,
            "{name}: rows written"
        );
        assert_eq!(
            bp.drop_stats().append_failure_drops,
            want_drops,
            "{name}: AppendFailureDrops"
        );
        assert_eq!(
            creates.load(Ordering::SeqCst),
            want_creates,
            "{name}: CreateWriteStream calls"
        );
        bp.close().await;
    }
}

#[tokio::test(start_paused = true)]
async fn test_batch_processor_exactly_once_delivery_fresh_stream_unknown_outcome_arms_cooldown() {
    type OnStream2 = fn(usize) -> tonic::Status;
    let cases: [(&str, OnStream2, &str); 3] = [
        (
            "Unavailable after each Send until retries are exhausted",
            |_| status(Code::Unavailable, "connection lost after send"),
            "max retries exceeded",
        ),
        (
            "NotFound after an Unavailable Send",
            |n| {
                if n == 1 {
                    return status(Code::Unavailable, "connection lost after send");
                }
                status(Code::NotFound, "stream not found")
            },
            "batch outcome unknown",
        ),
        (
            "OutOfRange after an Unavailable Send",
            |n| {
                if n == 1 {
                    return status(Code::Unavailable, "connection lost after send");
                }
                status(Code::OutOfRange, "offset beyond end")
            },
            "batch outcome unknown",
        ),
    ];
    for (name, on_stream2, want_err) in cases {
        let ctx = Context::background();
        let stream2_appends = Arc::new(AtomicUsize::new(0));
        let appends = Arc::clone(&stream2_appends);
        let srv = ScriptedServer::new(
            Vec::new(),
            move |_, req| {
                if req.write_stream == committed(1) {
                    // An offset error replaces the first stream.
                    return Err(status(Code::OutOfRange, "offset beyond end"));
                }
                if req.write_stream == committed(2) {
                    return Err(on_stream2(appends.fetch_add(1, Ordering::SeqCst) + 1));
                }
                Ok(AppendRowsResponse::default())
            },
            None,
        );
        let bp = processor(
            &ctx,
            Some(srv.server.client().unwrap()),
            "",
            exactly_once_test_config(),
        )
        .await;
        let now = pin_clock(&bp).await;

        // Batch 1 replaces the first stream within its append, and its outcome on the fresh
        // stream stays unknown.
        match bp.write_batch(&ctx, vec![event_row("B1")]).await {
            Err(e) if e.to_string().contains(want_err) => {}
            other => panic!(
                "{name}: writeBatch(B1) error = {other:?}, want an error containing {want_err:?}"
            ),
        }
        assert_eq!(
            srv.create_count(),
            2,
            "{name}: CreateWriteStream calls after B1, want 2 (initial + one replacement)"
        );

        // Batch 2, within the 30s cooldown, is dropped without CreateWriteStream.
        advance(&now, Duration::from_secs(29));
        match bp.write_batch(&ctx, vec![event_row("B2")]).await {
            Err(e) if e.to_string().contains("rotation in cooldown") => {}
            other => {
                panic!("{name}: writeBatch(B2) error = {other:?}, want a rotation cooldown error")
            }
        }
        assert_eq!(
            srv.create_count(),
            2,
            "{name}: CreateWriteStream calls during the cooldown"
        );
        assert_eq!(
            bp.drop_stats(),
            append_failure_stats(2),
            "{name}: DropStats() during the cooldown"
        );

        // Batch 3, after the cooldown, replaces the stream and is written.
        advance(&now, Duration::from_secs(2));
        bp.write_batch(&ctx, vec![event_row("B3")])
            .await
            .unwrap_or_else(|e| {
                panic!("{name}: writeBatch(B3) after the cooldown error = {e}, want nil")
            });
        assert_eq!(
            srv.create_count(),
            3,
            "{name}: CreateWriteStream calls after the cooldown"
        );
        bp.close().await;
    }
}

#[tokio::test(start_paused = true)]
async fn test_batch_processor_exactly_once_delivery_close_drain_drops_batches_during_cooldown() {
    let ctx = Context::background();
    // Every stream, the replacement included, rejects its offset.
    let srv = ScriptedServer::new(
        Vec::new(),
        |_, _| Err(status(Code::OutOfRange, "offset beyond end")),
        None,
    );
    let off = Arc::new(HookOffloader::default());
    let logger = Arc::new(RecordingLogger::default());
    let config = Config {
        batch_flush_intv: Duration::from_secs(3600),
        gcs_offloader: Some(Arc::clone(&off) as Arc<dyn GcsContentOffloader>),
        logger: as_logger(&logger),
        ..exactly_once_test_config()
    };
    let bp = processor(&ctx, Some(srv.server.client().unwrap()), "", config).await;

    // The first stream's offset error replaces it, and the freshly rotated stream rejects the
    // batch too, which pauses rotation.
    assert!(
        bp.write_batch(&ctx, vec![event_row("B1")]).await.is_err(),
        "writeBatch(B1) error = nil, want an offset error"
    );
    assert!(
        bp.stream_writer.is_rotation_in_cooldown().await,
        "rotation is not paused after the freshly rotated stream rejected the batch"
    );

    // Queue a row whose payload is still to be uploaded.
    let opts = PartsOptions {
        max_len: 1000,
        offload: true,
        inline_limit: 10,
        reserve_bytes: reserve_with(&bp),
        ..PartsOptions::default()
    };
    let mut row = queued_row(&opts, &[image_content(&[0; 64])]);
    row.row
        .insert("event_type".to_owned(), RowValue::from("LLM_REQUEST"));
    push_queue(&bp, row);
    assert_eq!(
        pending_upload_bytes(&bp),
        64,
        "pendingUploadBytes before Close"
    );

    bp.close().await;

    assert_eq!(
        bp.drop_stats(),
        append_failure_stats(2),
        "DropStats() after Close"
    );
    assert_eq!(
        pending_upload_bytes(&bp),
        0,
        "pendingUploadBytes after Close"
    );
    assert!(
        off.uploads().is_empty(),
        "uploads during Close = {:?}, want none: the batch is dropped before its uploads",
        off.uploads()
    );
    let want =
        "WARNING: Failed to write batch of 1 events: committed write stream rotation in cooldown";
    assert!(
        logger.contains(want),
        "log does not contain {want:?}; got:\n{logger}"
    );
    assert!(
        srv.attempts() == 2 && srv.create_count() == 2,
        "AppendRows calls = {}, CreateWriteStream calls = {}, want 2 and 2: Close must neither write nor replace the stream during the cooldown",
        srv.attempts(),
        srv.create_count()
    );
}

// parity: adapted: Go's goroutines are tasks on a multi-threaded runtime (no paused time)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_batch_processor_concurrent_append_and_close() {
    for iter in 0..20 {
        let config = Config {
            batch_size: 50,
            queue_max_size: 10000,
            batch_flush_intv: Duration::from_secs(3600),
            ..no_retry_config()
        };
        let bp = processor(&Context::background(), None, "test_stream", config).await;
        let counts = Arc::new(MockCounts::default());
        inject_stream(
            &bp,
            MockStream {
                recv_res: Some(AppendRowsResponse::default()),
                counts: Arc::clone(&counts),
                ..MockStream::default()
            },
        )
        .await;
        bp.start();

        let (start_tx, start_rx) = tokio::sync::watch::channel(false);
        let mut tasks = Vec::new();
        for id in 0..8 {
            let bp = Arc::clone(&bp);
            let mut start = start_rx.clone();
            tasks.push(tokio::spawn(async move {
                let _ = start.wait_for(|s| *s).await;
                for i in 0..50 {
                    bp.append(event_row(&format!("evt_{id}_{i}")));
                }
            }));
        }
        let _ = start_tx.send(true);
        bp.close().await;
        for t in tasks {
            t.await.unwrap();
        }

        assert_eq!(
            queue_len(&bp),
            0,
            "iter {iter}: queue len after Close and concurrent Appends, want 0 (unflushed items stranded in queue)"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn test_batch_processor_close_drains_in_flight_and_queued_batches() {
    let config = Config {
        batch_size: 5,
        queue_max_size: 1000,
        batch_flush_intv: Duration::from_secs(3600),
        ..no_retry_config()
    };
    let bp = processor(&Context::background(), None, "test_stream", config).await;

    let first_send_started = Arc::new(Notify::new());
    let once = Arc::new(AtomicBool::new(false));
    let started = Arc::clone(&first_send_started);
    let counts = Arc::new(MockCounts::default());
    let stream = HookedStream {
        inner: MockStream {
            recv_res: Some(AppendRowsResponse::default()),
            counts: Arc::clone(&counts),
            ..MockStream::default()
        },
        send_delay: Duration::from_millis(15),
        on_send: Some(Arc::new(move || {
            if !once.swap(true, Ordering::SeqCst) {
                started.notify_one();
            }
            Box::pin(async {})
        })),
    };
    bp.stream_writer.state.lock().await.stream = Some(Box::new(stream));
    bp.start();

    const TOTAL_ROWS: usize = 35;
    for i in 0..TOTAL_ROWS {
        bp.append(event_row(&format!("evt_{i}")));
    }

    // Wait until the first append-triggered background flush is in flight, then close while
    // the remaining batches are still queued.
    first_send_started.notified().await;
    bp.close().await;

    assert_eq!(queue_len(&bp), 0, "queue len after Close");
    let got_rows: usize = counts
        .sent
        .lock()
        .unwrap()
        .iter()
        .map(|r| decode_arrow_rows(r).len())
        .sum();
    assert_eq!(got_rows, TOTAL_ROWS, "total rows sent");
}

#[tokio::test(start_paused = true)]
async fn test_batch_processor_reuses_cached_stream_across_flushes() {
    let server = FakeWriteServer::new();
    let (tx, mut rx) = requests();
    recording_server(&server, tx);
    let config = Config {
        batch_size: 10,
        batch_flush_intv: Duration::from_secs(3600),
        ..no_retry_config()
    };
    let bp = processor(
        &Context::background(),
        Some(server.client().unwrap()),
        "projects/p/datasets/d/tables/t/_default",
        config,
    )
    .await;
    bp.start();

    const NUM_FLUSHES: usize = 20;
    for i in 0..NUM_FLUSHES {
        bp.append(event_row(&format!("flush_{i}")));
        bp.flush().await;
    }
    bp.close().await;

    assert_eq!(
        server.streams_opened(),
        1,
        "AppendRows opened streams across {NUM_FLUSHES} flushes"
    );
    let mut got_rows = 0;
    while let Ok(req) = rx.try_recv() {
        got_rows += decode_arrow_rows(&req).len();
    }
    assert_eq!(
        got_rows, NUM_FLUSHES,
        "received rows across {NUM_FLUSHES} flushes with MaxRetries=0"
    );
}

#[tokio::test(start_paused = true)]
async fn test_batch_processor_concurrent_close_waits_for_drain() {
    let config = Config {
        batch_size: 5,
        queue_max_size: 100,
        batch_flush_intv: Duration::from_secs(3600),
        ..no_retry_config()
    };
    let bp = processor(&Context::background(), None, "test_stream", config).await;

    let (entered_tx, entered_rx) = oneshot::channel::<()>();
    let (release_tx, release_rx) = oneshot::channel::<()>();
    let first = Arc::new(Mutex::new(Some((entered_tx, release_rx))));
    let counts = Arc::new(MockCounts::default());
    let stream = HookedStream {
        inner: MockStream {
            recv_res: Some(AppendRowsResponse::default()),
            counts: Arc::clone(&counts),
            ..MockStream::default()
        },
        send_delay: Duration::ZERO,
        on_send: Some(Arc::new(move || {
            let first = first.lock().unwrap().take();
            Box::pin(async move {
                if let Some((entered, release)) = first {
                    let _ = entered.send(());
                    let _ = release.await;
                }
            })
        })),
    };
    bp.stream_writer.state.lock().await.stream = Some(Box::new(stream));
    bp.start();

    const TOTAL_ROWS: usize = 15;
    for i in 0..TOTAL_ROWS {
        bp.append(event_row(&format!("evt_{i}")));
    }

    entered_rx.await.unwrap();

    let close1 = tokio::spawn({
        let bp = Arc::clone(&bp);
        async move { bp.close().await }
    });
    let close2 = tokio::spawn({
        let bp = Arc::clone(&bp);
        async move { bp.close().await }
    });

    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        !close2.is_finished(),
        "second Close() returned while first Close() was still draining"
    );

    let _ = release_tx.send(());
    close1.await.unwrap();
    close2.await.unwrap();

    let got_rows: usize = counts
        .sent
        .lock()
        .unwrap()
        .iter()
        .map(|r| decode_arrow_rows(r).len())
        .sum();
    assert_eq!(
        got_rows, TOTAL_ROWS,
        "total rows sent after concurrent Close"
    );
}

/// Go's DefaultConfig with BatchSize 10, an hour's flush interval, a 1s ShutdownTimeout and
/// one retry (the drain tests' config).
fn drain_config() -> Config {
    let mut config = Config {
        batch_size: 10,
        batch_flush_intv: Duration::from_secs(3600),
        shutdown_timeout: Duration::from_secs(1),
        ..Config::default()
    };
    config.retry_config.max_retries = 1;
    config
}

/// Go's `select { case req := <-srv.requests: …; case <-time.After(time.Second): t.Fatal }`
/// expecting one row of `event_type`.
async fn expect_single_row(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppendRowsRequest>,
    event_type: &str,
    timeout_msg: &str,
) {
    match tokio::time::timeout(Duration::from_secs(1), rx.recv()).await {
        Ok(Some(req)) => {
            let rows = decode_arrow_rows(&req);
            assert_eq!(rows.len(), 1, "got {} rows, want 1", rows.len());
            assert_eq!(
                rows[0].get("event_type"),
                Some(&RowValue::from(event_type)),
                "event_type"
            );
        }
        _ => panic!("{timeout_msg}"),
    }
}

#[tokio::test(start_paused = true)]
async fn test_batch_processor_drain_delivers_queued_rows_after_constructor_context_canceled_fresh_stream()
 {
    let server = FakeWriteServer::new();
    let (tx, mut rx) = requests();
    recording_server(&server, tx);
    let ctx = Context::background().with_cancel();
    let bp = processor(
        &ctx,
        Some(server.client().unwrap()),
        "projects/p/datasets/d/tables/t/_default",
        drain_config(),
    )
    .await;

    bp.start();
    bp.append(event_row("queued_fresh_row"));

    // Cancel the constructor context before Close().
    ctx.cancel();
    bp.close().await;

    expect_single_row(
        &mut rx,
        "queued_fresh_row",
        "queued row was not delivered during shutdown drain with canceled constructor context",
    )
    .await;
    assert!(
        bp.stream_writer.state.lock().await.stream.is_none(),
        "stream was not explicitly closed after draining"
    );
}

#[tokio::test(start_paused = true)]
async fn test_batch_processor_drain_delivers_queued_rows_after_constructor_context_canceled_cached_stream()
 {
    let server = FakeWriteServer::new();
    let (tx, mut rx) = requests();
    recording_server(&server, tx);
    let ctx = Context::background().with_cancel();
    let bp = processor(
        &ctx,
        Some(server.client().unwrap()),
        "projects/p/datasets/d/tables/t/_default",
        drain_config(),
    )
    .await;

    bp.start();

    // Send an initial batch to establish the cached stream.
    bp.write_batch(&ctx, vec![event_row("initial_cached")])
        .await
        .unwrap_or_else(|e| panic!("initial writeBatch: {e}"));
    if tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .is_err()
    {
        panic!("initial batch not received");
    }
    assert!(
        bp.stream_writer.state.lock().await.stream.is_some(),
        "expected cached stream to be present"
    );

    bp.append(event_row("queued_cached_row"));

    // Cancel the constructor context, rendering the cached stream canceled.
    ctx.cancel();
    bp.close().await;

    expect_single_row(
        &mut rx,
        "queued_cached_row",
        "queued row was not delivered during shutdown drain with canceled cached stream",
    )
    .await;
    assert!(
        bp.stream_writer.state.lock().await.stream.is_none(),
        "stream was not explicitly closed after draining"
    );
}

#[tokio::test(start_paused = true)]
async fn test_batch_processor_root_context_cancel_before_first_stream_open_drains_queued_row_on_close()
 {
    let server = FakeWriteServer::new();
    let (tx, mut rx) = requests();
    recording_server(&server, tx);
    let ctx = Context::background().with_cancel();
    let bp = processor(
        &ctx,
        Some(server.client().unwrap()),
        "projects/p/datasets/d/tables/t/_default",
        drain_config(),
    )
    .await;

    bp.start();
    // Append a row without a prior write, so the stream is not open.
    bp.append(event_row("queued_unopened_stream_row"));
    // Cancel the constructor context before the stream is ever opened.
    ctx.cancel();
    bp.close().await;

    expect_single_row(
        &mut rx,
        "queued_unopened_stream_row",
        "queued row was not delivered during shutdown drain when root context was cancelled before opening stream",
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn test_batch_processor_init_context_canceled_does_not_stop_processing() {
    let server = FakeWriteServer::new();
    let (tx, mut rx) = requests();
    recording_server(&server, tx);
    let init_ctx = Context::background().with_cancel();
    let config = Config {
        batch_size: 1,
        batch_flush_intv: Duration::from_millis(10),
        shutdown_timeout: Duration::from_secs(1),
        ..Config::default()
    };
    let bp = processor(
        &init_ctx,
        Some(server.client().unwrap()),
        "projects/p/datasets/d/tables/t/_default",
        config,
    )
    .await;

    // Cancel the init context immediately after construction.
    init_ctx.cancel();
    bp.start();

    // A row appended after the init context is canceled is still flushed and delivered.
    bp.append(event_row("post_cancel_row"));
    match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
        Ok(Some(req)) => {
            let rows = decode_arrow_rows(&req);
            assert!(
                rows.len() == 1
                    && rows[0].get("event_type") == Some(&RowValue::from("post_cancel_row")),
                "got rows {rows:?}, want 1 row with post_cancel_row"
            );
        }
        _ => panic!(
            "row was not flushed because processor stopped when constructor context was canceled"
        ),
    }
    bp.close().await;
}

// parity: adapted: Go's goroutines are tasks on a multi-threaded runtime (no paused time)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_batch_processor_concurrent_start_and_close() {
    let server = FakeWriteServer::new();
    let (tx, _rx) = requests();
    recording_server(&server, tx);
    let config = Config {
        batch_size: 10,
        batch_flush_intv: Duration::from_millis(10),
        ..Config::default()
    };
    let bp = processor(
        &Context::background(),
        Some(server.client().unwrap()),
        "projects/p/datasets/d/tables/t/_default",
        config,
    )
    .await;

    let mut tasks = Vec::new();
    for _ in 0..8 {
        let starter = Arc::clone(&bp);
        tasks.push(tokio::spawn(async move { starter.start() }));
        let closer = Arc::clone(&bp);
        tasks.push(tokio::spawn(async move { closer.close().await }));
    }
    for t in tasks {
        t.await.unwrap();
    }
}
