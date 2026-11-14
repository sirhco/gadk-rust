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

//! `gcs_offloader_test.go`: the batch upload timeout, the cooldown, and which deadlines pause
//! the uploads (unit tests on `resolve_uploads_for_batch` and run-end flushes of a processor).

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gadk_genai::{Content, Part};
use gadk_platform::BoxFuture;
use pretty_assertions::assert_eq;
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::super::plugin_agentanalytics_batch_processor::support::{
    Delayed, HookOffloader, collect_rows, decode_arrow_rows, delayed_server, dynamic_delay_server,
    event_row, image_content, processor, push_queue, queued_row, requests, rows_of_type,
    upload_paused,
};
use super::{
    as_log, as_off, bare, ctx_err, field, hook, hooked, kept, opts, parts, reserving, resolve,
    tagged, wait_done,
};
use crate::batch_processor::BatchProcessor;
use crate::bigquery::fake::RecordingLogger;
use crate::config::{Config, GcsUploadRequest};
use crate::context::Context;
use crate::error::Error;
use crate::formatter::UPLOAD_FAILED_TEXT;
use crate::row::{QueuedRow, Row, RowExt, RowValue};
use crate::storagepb::{AppendRowsRequest, AppendRowsResponse};
use crate::testutil::fake_write_server::FakeWriteServer;
use crate::write::AppendRowsClient;

/// Go `mockStream{recvRes: &storagepb.AppendRowsResponse{}, sendDelay, onSend}`: every send
/// runs `on_send`, waits `send_delay` and is recorded; every receive answers an empty response.
#[derive(Clone, Default)]
pub(super) struct DelayStream {
    pub(super) send_delay: Duration,
    pub(super) on_send: Option<Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>>,
    pub(super) sent: Arc<Mutex<Vec<AppendRowsRequest>>>,
}

#[async_trait::async_trait]
impl AppendRowsClient for DelayStream {
    async fn send(&mut self, req: AppendRowsRequest) -> Result<(), Error> {
        if let Some(on_send) = &self.on_send {
            on_send().await;
        }
        if !self.send_delay.is_zero() {
            tokio::time::sleep(self.send_delay).await;
        }
        self.sent.lock().unwrap().push(req);
        Ok(())
    }

    async fn recv(&mut self) -> Result<AppendRowsResponse, Error> {
        Ok(AppendRowsResponse::default())
    }

    fn close_send(&mut self) {}
}

impl DelayStream {
    /// Go `stream.requests()`.
    fn requests(&self) -> Vec<AppendRowsRequest> {
        self.sent.lock().unwrap().clone()
    }

    /// Go `bp.streamWriter.stream = stream`.
    async fn inject(&self, bp: &BatchProcessor) {
        bp.stream_writer.state.lock().await.stream = Some(Box::new(self.clone()));
    }
}

/// `p`'s `storage_mode` is `mode`.
fn mode_is(p: &Row, mode: &str) -> bool {
    p.get("storage_mode") == Some(&RowValue::from(mode))
}

/// Go's warm-up: `writeBatch` of one row under a 5s timeout, and its request read off `rx`.
pub(super) async fn warm_up(
    bp: &BatchProcessor,
    rx: &mut mpsc::UnboundedReceiver<AppendRowsRequest>,
) {
    let warm = Context::background().with_timeout(Duration::from_secs(5));
    bp.write_batch(&warm, vec![event_row("USER_MESSAGE_RECEIVED")])
        .await
        .unwrap_or_else(|e| panic!("warm-up writeBatch: {e}"));
    warm.cancel();
    rx.recv().await.expect("warm-up request");
}

#[tokio::test(start_paused = true)]
async fn test_resolve_uploads_batch_level_upload_timeout() {
    let contents: Vec<Content> = (0..6_u8).map(|i| image_content(&[i])).collect();
    let mut row = queued_row(&opts(50), &contents);
    let off = hooked(|ctx, _| Box::pin(wait_done(ctx)));
    let bp = bare(Config {
        gcs_offloader: as_off(&off),
        gcs_max_concurrent_uploads: 2,
        gcs_upload_timeout: Duration::from_millis(60),
        logger: as_log(&Arc::new(RecordingLogger::default())),
        ..Config::zero()
    });

    let start = Instant::now();
    resolve(
        &bp,
        &Context::background(),
        false,
        std::slice::from_mut(&mut row),
    )
    .await;
    let elapsed = start.elapsed();

    // With a per-batch timeout of 60ms across 6 uploads at concurrency 2, the entire batch
    // finishes around 60ms rather than 3 * 60ms = 180ms.
    assert!(
        elapsed < Duration::from_millis(150),
        "resolveUploads took {elapsed:?}, want < 150ms (single batch-level timeout)"
    );
    for (i, part) in parts(&row).iter().enumerate() {
        assert_eq!(field(part, "text"), UPLOAD_FAILED_TEXT, "part {i}");
    }
}

#[tokio::test(start_paused = true)]
async fn test_gcs_offloading_run_end_pauses_stalled_uploads() {
    let cases = [
        (
            "partial outage in first batch",
            0,
            true,
            Duration::from_millis(100),
        ),
        (
            "outage in later batch",
            1,
            false,
            Duration::from_millis(100),
        ),
        (
            "partial outage in later batch",
            1,
            true,
            Duration::from_millis(100),
        ),
        (
            "outage after slow first batch",
            1,
            false,
            Duration::from_millis(2800),
        ),
        (
            "partial outage after slow first batch",
            1,
            true,
            Duration::from_millis(2800),
        ),
    ];
    for (name, stalled_batch, partial_success, upload_delay) in cases {
        let off = hooked(move |ctx, req| {
            let b = usize::from(req.data[0]);
            Box::pin(async move {
                if b / 2 < stalled_batch || (partial_success && b % 2 == 0) {
                    tokio::time::sleep(upload_delay).await;
                    return Ok(());
                }
                wait_done(ctx).await
            })
        });
        let mut cfg = Config::default();
        cfg.batch_size = 2;
        cfg.logger = as_log(&Arc::new(RecordingLogger::default()));
        cfg.gcs_offloader = as_off(&off);
        let shutdown = cfg.shutdown_timeout;
        let bp = processor(&Context::background(), None, "test_stream", cfg).await;
        let stream = DelayStream {
            send_delay: Duration::from_millis(100),
            ..DelayStream::default()
        };
        stream.inject(&bp).await;
        let o = reserving(&bp, 50);
        const ROW_COUNT: usize = 16;
        for i in 0..ROW_COUNT {
            let i = u8::try_from(i).unwrap();
            push_queue(
                &bp,
                tagged(queued_row(&o, &[image_content(&[i])]), "LLM_REQUEST", ""),
            );
        }

        let start = Instant::now();
        bp.flush_run_end().await;
        let elapsed = start.elapsed();
        assert!(
            elapsed <= shutdown,
            "{name}: run-end flush took {elapsed:?}, exceeds {shutdown:?}"
        );
        assert_eq!(
            off.uploads().len(),
            2 * (stalled_batch + 1),
            "{name}: upload attempts; later batches must skip stalled storage"
        );
        let rows: Vec<Row> = stream
            .requests()
            .iter()
            .flat_map(decode_arrow_rows)
            .collect();
        assert_eq!(rows.len(), ROW_COUNT, "{name}: persisted rows");
        for (i, row) in rows.iter().enumerate() {
            let want = if i / 2 < stalled_batch
                || (i / 2 == stalled_batch && partial_success && i % 2 == 0)
            {
                "GCS_REFERENCE"
            } else {
                "INLINE"
            };
            assert!(
                mode_is(&row.parts("content_parts").unwrap()[0], want),
                "{name}: row {i} has incorrect storage mode, want {want}"
            );
        }
        assert_eq!(
            bp.pending_upload_bytes.load(Ordering::SeqCst),
            0,
            "{name}: pending upload bytes"
        );
        bp.close().await;
    }
}

// TestGCSOffloading_RunEndOwnDeadlineErrorBeforeTheDeadlineDoesNotPause runs a run-end flush
// under the default 10s ShutdownTimeout whose first batch's BigQuery write takes 7s, so that the
// next batch starts with 3s left, enough for its deadline to pause the uploads, and has 1.5s
// for its two uploads. One upload fails after 1.2s, more than three quarters of that time, with
// a deadline error of its own while that deadline has not passed yet; the other succeeds once
// the deadline has ended the uploads. The uploads are not paused, and the next batch uploads.
// parity: adapted: Go reads the next row's content_parts on its queued map; gadk moves the row, so the written row is read
#[tokio::test(start_paused = true)]
async fn test_gcs_offloading_run_end_own_deadline_error_before_the_deadline_does_not_pause() {
    let off = hooked(|ctx, req| {
        let b = req.data[0];
        Box::pin(async move {
            match b {
                1 => {
                    tokio::time::sleep(Duration::from_millis(1200)).await;
                    return Err(Error::Context(
                        crate::context::ContextError::DeadlineExceeded,
                    ));
                }
                2 => ctx.done().await,
                _ => {}
            }
            Ok(())
        })
    });
    let mut cfg = Config::default();
    cfg.batch_size = 2;
    cfg.logger = as_log(&Arc::new(RecordingLogger::default()));
    cfg.gcs_offloader = as_off(&off);
    let bp = processor(&Context::background(), None, "test_stream", cfg).await;
    let writes = Arc::new(AtomicUsize::new(0));
    let stream = DelayStream {
        on_send: Some({
            let writes = Arc::clone(&writes);
            Arc::new(move || {
                let first = writes.fetch_add(1, Ordering::SeqCst) == 0;
                Box::pin(async move {
                    if first {
                        tokio::time::sleep(Duration::from_secs(7)).await;
                    }
                })
            })
        }),
        ..DelayStream::default()
    };
    stream.inject(&bp).await;
    let o = reserving(&bp, 50);
    push_queue(&bp, event_row("USER_MESSAGE_RECEIVED"));
    push_queue(&bp, event_row("USER_MESSAGE_RECEIVED"));
    for i in 1..=2_u8 {
        push_queue(
            &bp,
            tagged(queued_row(&o, &[image_content(&[i])]), "LLM_REQUEST", ""),
        );
    }
    bp.flush_run_end().await;
    assert_eq!(off.uploads().len(), 2, "upload attempts");
    assert!(
        !upload_paused(&bp),
        "an upload that failed with a deadline error of its own before the deadline paused the uploads"
    );

    push_queue(
        &bp,
        tagged(
            queued_row(&o, &[image_content(&[3])]),
            "LLM_REQUEST",
            "next",
        ),
    );
    bp.flush_run_end().await;
    assert_eq!(
        off.uploads().len(),
        3,
        "upload attempts after the next flush"
    );
    let rows: Vec<Row> = stream
        .requests()
        .iter()
        .flat_map(decode_arrow_rows)
        .collect();
    let next = rows
        .iter()
        .find(|r| r.str("event_id") == Some("next"))
        .expect("the next row was written");
    assert!(
        mode_is(&next.parts("content_parts").unwrap()[0], "GCS_REFERENCE"),
        "next batch storage_mode, want GCS_REFERENCE"
    );
    assert_eq!(
        bp.pending_upload_bytes.load(Ordering::SeqCst),
        0,
        "pending upload bytes"
    );
    bp.close().await;
}

#[tokio::test(start_paused = true)]
async fn test_resolve_uploads_batch_timeout_cooldown() {
    let should_block = Arc::new(AtomicBool::new(true));
    let off = {
        let should_block = Arc::clone(&should_block);
        hooked(move |ctx, _| {
            let block = should_block.load(Ordering::SeqCst);
            Box::pin(async move {
                if block {
                    return wait_done(ctx).await;
                }
                Ok(())
            })
        })
    };
    let logger = Arc::new(RecordingLogger::default());
    let bp = bare(Config {
        gcs_offloader: as_off(&off),
        gcs_max_concurrent_uploads: 2,
        gcs_upload_timeout: Duration::from_millis(40),
        logger: as_log(&logger),
        ..Config::zero()
    });

    // Batch 1 times out on GCSUploadTimeout and arms the cooldown.
    let mut row1 = queued_row(&opts(50), &[image_content(&[1])]);
    resolve(
        &bp,
        &Context::background(),
        false,
        std::slice::from_mut(&mut row1),
    )
    .await;
    assert_eq!(off.uploads().len(), 1, "after batch 1 upload attempts");

    // Batch 2 runs during the active cooldown: skips calling UploadContent and returns at once.
    let mut row2 = queued_row(&opts(50), &[image_content(&[2])]);
    let start = Instant::now();
    resolve(
        &bp,
        &Context::background(),
        false,
        std::slice::from_mut(&mut row2),
    )
    .await;
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_millis(20),
        "cooldown batch took {elapsed:?}, want < 20ms without waiting on offloader"
    );
    assert_eq!(
        off.uploads().len(),
        1,
        "after cooldown batch (upload skipped during cooldown)"
    );
    assert_eq!(
        field(&parts(&row2)[0], "text"),
        UPLOAD_FAILED_TEXT,
        "cooldown batch part text"
    );
    assert!(
        logger.contains("upload skipped during timeout cooldown"),
        "expected cooldown log entry, got:\n{logger}"
    );

    // Once cooldown expires and storage recovers, Batch 3 uploads normally.
    tokio::time::sleep(Duration::from_millis(50)).await;
    should_block.store(false, Ordering::SeqCst);
    let mut row3 = queued_row(&opts(50), &[image_content(&[3])]);
    resolve(
        &bp,
        &Context::background(),
        false,
        std::slice::from_mut(&mut row3),
    )
    .await;
    assert_eq!(
        off.uploads().len(),
        2,
        "after cooldown expiry upload attempts"
    );
    assert_eq!(
        field(&parts(&row3)[0], "storage_mode"),
        "GCS_REFERENCE",
        "after cooldown expiry storage_mode"
    );
}

/// How the first batch of a `TestResolveUploads_DeadlinesPauseTheUploads` case is resolved
/// (Go's `newCtx`).
#[derive(Clone, Copy)]
enum NewCtx {
    Background,
    Timeout(u64),
    CancelAfter(u64),
    CanceledOnArrival,
    DeadlinePassed,
}

impl NewCtx {
    fn make(self) -> Context {
        match self {
            NewCtx::Background => Context::background(),
            NewCtx::Timeout(ms) => Context::background().with_timeout(Duration::from_millis(ms)),
            NewCtx::CancelAfter(ms) => {
                let ctx = Context::background().with_cancel();
                let cancel = ctx.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(ms)).await;
                    cancel.cancel();
                });
                ctx
            }
            NewCtx::CanceledOnArrival => {
                let ctx = Context::background().with_cancel();
                ctx.cancel();
                ctx
            }
            NewCtx::DeadlinePassed => {
                let now = Instant::now();
                Context::background()
                    .with_deadline(now.checked_sub(Duration::from_secs(1)).unwrap_or(now))
            }
        }
    }
}

/// The error a first upload fails with (Go `firstUploadErr`).
#[derive(Clone, Copy)]
enum FirstErr {
    Msg(&'static str),
    Deadline,
}

/// One case of `TestResolveUploads_DeadlinesPauseTheUploads` (Go's fields; zero = unset).
#[derive(Clone, Copy)]
struct PauseCase {
    name: &'static str,
    upload_timeout: Duration,
    pause_on_caller_deadline: bool,
    partial_success: bool,
    first_upload_takes: Duration,
    max_concurrent: i64,
    uploads: usize,
    fail_after: Duration,
    return_lag: Duration,
    new_ctx: NewCtx,
    want_calls: usize,
    min_calls: usize,
    max_calls: usize,
    want_paused: bool,
    want_kept: bool,
    want_kept_count: usize,
    first_upload_uri: &'static str,
    first_upload_err: Option<FirstErr>,
    after_done_err: Option<&'static str>,
    return_cause: bool,
    want_log_substring: &'static str,
    first_upload_err_after: Duration,
    succeed_once_done: bool,
}

const BASE: PauseCase = PauseCase {
    name: "",
    upload_timeout: Duration::from_secs(60),
    pause_on_caller_deadline: false,
    partial_success: false,
    first_upload_takes: Duration::ZERO,
    max_concurrent: 0,
    uploads: 0,
    fail_after: Duration::ZERO,
    return_lag: Duration::ZERO,
    new_ctx: NewCtx::Background,
    want_calls: 0,
    min_calls: 0,
    max_calls: 0,
    want_paused: false,
    want_kept: false,
    want_kept_count: 0,
    first_upload_uri: "",
    first_upload_err: None,
    after_done_err: None,
    return_cause: false,
    want_log_substring: "",
    first_upload_err_after: Duration::ZERO,
    succeed_once_done: false,
};

const fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

// TestResolveUploads_DeadlinesPauseTheUploads checks which ways of ending the uploads of a batch
// show that Cloud Storage is stalled, and so make the batches after it fall back without calling
// the offloader, and which show nothing about Cloud Storage. It also checks which of them keep
// the part that they cut short on its row, for a retry if the batch is deferred.
#[tokio::test(start_paused = true)]
async fn test_resolve_uploads_deadlines_pause_the_uploads() {
    let tests = [
        PauseCase {
            name: "the upload timeout runs out",
            upload_timeout: ms(40),
            want_calls: 1,
            want_paused: true,
            ..BASE
        },
        PauseCase {
            name: "a fresh deadline of the caller runs out before the upload timeout",
            pause_on_caller_deadline: true,
            new_ctx: NewCtx::Timeout(250),
            want_calls: 1,
            want_paused: true,
            ..BASE
        },
        PauseCase {
            name: "a depleted deadline of the caller runs out before the upload timeout",
            new_ctx: NewCtx::Timeout(40),
            want_calls: 1,
            want_kept: true,
            ..BASE
        },
        PauseCase {
            name: "an upload succeeds before the deadline runs out on another",
            upload_timeout: ms(40),
            pause_on_caller_deadline: true,
            partial_success: true,
            want_calls: 2,
            want_paused: true,
            ..BASE
        },
        PauseCase {
            name: "an upload succeeds before a fresh deadline of the caller runs out on another",
            pause_on_caller_deadline: true,
            partial_success: true,
            new_ctx: NewCtx::Timeout(250),
            want_calls: 2,
            want_paused: true,
            ..BASE
        },
        PauseCase {
            name: "a fresh deadline of the caller cuts short an upload that ran for 60% of it",
            pause_on_caller_deadline: true,
            partial_success: true,
            first_upload_takes: ms(200),
            max_concurrent: 1,
            new_ctx: NewCtx::Timeout(500),
            want_calls: 2,
            want_kept: true,
            ..BASE
        },
        PauseCase {
            name: "a fresh deadline of the caller cuts short an upload that ran for 85% of it",
            pause_on_caller_deadline: true,
            partial_success: true,
            first_upload_takes: ms(180),
            max_concurrent: 1,
            new_ctx: NewCtx::Timeout(1200),
            want_calls: 2,
            want_paused: true,
            ..BASE
        },
        PauseCase {
            name: "a fresh deadline of the caller cuts short an upload that ran for 60% of it, and the offloader returns late",
            pause_on_caller_deadline: true,
            partial_success: true,
            first_upload_takes: ms(200),
            max_concurrent: 1,
            return_lag: ms(150),
            new_ctx: NewCtx::Timeout(500),
            want_calls: 2,
            want_kept: true,
            ..BASE
        },
        PauseCase {
            name: "uploads fail with errors until a fresh deadline of the caller runs out",
            pause_on_caller_deadline: true,
            uploads: 7,
            fail_after: ms(200),
            max_concurrent: 1,
            new_ctx: NewCtx::Timeout(1000),
            min_calls: 2,
            max_calls: 6,
            want_paused: true,
            ..BASE
        },
        PauseCase {
            name: "the upload timeout runs out before a deadline of the caller, and the offloader returns after both",
            upload_timeout: ms(300),
            return_lag: ms(300),
            new_ctx: NewCtx::Timeout(500),
            want_calls: 1,
            want_paused: true,
            ..BASE
        },
        PauseCase {
            name: "the upload timeout runs out before a deadline of the caller, and an upload that succeeds returns after both",
            upload_timeout: ms(100),
            pause_on_caller_deadline: true,
            partial_success: true,
            first_upload_takes: ms(800),
            max_concurrent: 1,
            new_ctx: NewCtx::Timeout(600),
            want_calls: 1,
            ..BASE
        },
        PauseCase {
            name: "the caller cancels the context",
            pause_on_caller_deadline: true,
            new_ctx: NewCtx::CancelAfter(40),
            want_calls: 1,
            want_kept: true,
            ..BASE
        },
        PauseCase {
            name: "the context is canceled on arrival",
            pause_on_caller_deadline: true,
            new_ctx: NewCtx::CanceledOnArrival,
            want_calls: 0,
            want_kept: true,
            ..BASE
        },
        PauseCase {
            name: "the deadline of the context has passed on arrival",
            pause_on_caller_deadline: true,
            new_ctx: NewCtx::DeadlinePassed,
            want_calls: 0,
            want_kept: true,
            ..BASE
        },
        PauseCase {
            name: "an offloader returns a non-context error once a depleted deadline of the caller runs out",
            after_done_err: Some("opaque transport error"),
            new_ctx: NewCtx::Timeout(250),
            want_calls: 1,
            want_kept: true,
            ..BASE
        },
        PauseCase {
            name: "an upload fails early with a non-context error and another is cut short by a depleted deadline",
            uploads: 2,
            first_upload_err: Some(FirstErr::Msg("early failure")),
            new_ctx: NewCtx::Timeout(250),
            want_calls: 2,
            want_kept_count: 2,
            ..BASE
        },
        PauseCase {
            name: "the upload timeout runs out and the offloader returns the context cause",
            upload_timeout: ms(100),
            return_cause: true,
            want_calls: 1,
            want_paused: true,
            want_log_substring: "context deadline exceeded; keeping the part inline",
            ..BASE
        },
        PauseCase {
            name: "depleted caller deadline drops succeeded non-gs URI upload and keeps cut-short upload",
            partial_success: true,
            uploads: 2,
            first_upload_uri: "https://example.com/not-gs",
            new_ctx: NewCtx::Timeout(250),
            want_calls: 2,
            want_kept: true,
            want_kept_count: 1,
            ..BASE
        },
        PauseCase {
            name: "live caller context does not keep upload when offloader returns immediate DeadlineExceeded",
            uploads: 1,
            first_upload_err: Some(FirstErr::Deadline),
            want_calls: 1,
            ..BASE
        },
        PauseCase {
            name: "an upload fails with a deadline error of its own before a fresh deadline of the caller, and another succeeds once that deadline ends the uploads",
            pause_on_caller_deadline: true,
            uploads: 2,
            first_upload_err: Some(FirstErr::Deadline),
            first_upload_err_after: ms(800),
            succeed_once_done: true,
            new_ctx: NewCtx::Timeout(1000),
            want_calls: 2,
            want_kept: true,
            ..BASE
        },
    ];
    for tt in tests {
        let name = tt.name;
        let stalled = Arc::new(AtomicBool::new(true));
        let call_count = Arc::new(AtomicI32::new(0));
        let first_call_path = Arc::new(Mutex::new(None::<String>));
        let hook_fn = {
            let (stalled, call_count, first_call_path) = (
                Arc::clone(&stalled),
                Arc::clone(&call_count),
                Arc::clone(&first_call_path),
            );
            hook(move |ctx: Context, req: GcsUploadRequest| {
                let is_stalled = stalled.load(Ordering::SeqCst);
                let call = if is_stalled {
                    call_count.fetch_add(1, Ordering::SeqCst) + 1
                } else {
                    0
                };
                if call == 1 {
                    *first_call_path.lock().unwrap() = Some(req.path.clone());
                }
                Box::pin(async move {
                    if !is_stalled {
                        return Ok(());
                    }
                    if tt.partial_success && call == 1 {
                        tokio::time::sleep(tt.first_upload_takes).await;
                        return Ok(());
                    }
                    if let Some(first_err) = tt.first_upload_err
                        && call == 1
                    {
                        tokio::time::sleep(tt.first_upload_err_after).await;
                        return Err(match first_err {
                            FirstErr::Msg(m) => Error::msg(m),
                            FirstErr::Deadline => {
                                Error::Context(crate::context::ContextError::DeadlineExceeded)
                            }
                        });
                    }
                    if !tt.fail_after.is_zero() {
                        return tokio::select! {
                            () = tokio::time::sleep(tt.fail_after) => Err(Error::msg("storage unavailable")),
                            () = ctx.done() => Err(ctx_err(&ctx)),
                        };
                    }
                    ctx.done().await;
                    tokio::time::sleep(tt.return_lag).await;
                    if tt.return_cause {
                        let cause = ctx.cause().expect("a done context has a cause");
                        return Err(Error::Other(Box::new(SharedError(cause))));
                    }
                    if let Some(m) = tt.after_done_err {
                        return Err(Error::msg(m));
                    }
                    if tt.succeed_once_done {
                        return Ok(());
                    }
                    Err(ctx_err(&ctx))
                })
            })
        };
        let uri_fn = {
            let (stalled, first_call_path) = (Arc::clone(&stalled), Arc::clone(&first_call_path));
            Arc::new(move |req: &GcsUploadRequest| {
                let first = first_call_path.lock().unwrap().clone();
                if stalled.load(Ordering::SeqCst)
                    && !tt.first_upload_uri.is_empty()
                    && first.as_deref() == Some(req.path.as_str())
                {
                    return tt.first_upload_uri.to_owned();
                }
                format!("gs://fake-bucket/{}", req.path)
            })
        };
        let off = Arc::new(HookOffloader::new(Some(hook_fn), Some(uri_fn)));
        let logger = Arc::new(RecordingLogger::default());
        let bp = bare(Config {
            gcs_offloader: as_off(&off),
            gcs_upload_timeout: tt.upload_timeout,
            gcs_max_concurrent_uploads: tt.max_concurrent,
            logger: as_log(&logger),
            ..Config::zero()
        });

        let mut first_contents = vec![image_content(&[1])];
        if tt.partial_success {
            first_contents = vec![image_content(&[1]), image_content(&[2])];
        }
        if tt.uploads > 0 {
            first_contents = (0..tt.uploads).map(|_| image_content(&[1])).collect();
        }
        let mut first = queued_row(&opts(50), &first_contents);
        let ctx = tt.new_ctx.make();
        resolve(
            &bp,
            &ctx,
            tt.pause_on_caller_deadline,
            std::slice::from_mut(&mut first),
        )
        .await;
        let first_calls = off.uploads().len();
        let (min_calls, max_calls) = if tt.min_calls == 0 && tt.max_calls == 0 {
            (tt.want_calls, tt.want_calls)
        } else {
            (tt.min_calls, tt.max_calls)
        };
        assert!(
            (min_calls..=max_calls).contains(&first_calls),
            "{name}: the first batch started {first_calls} uploads, want [{min_calls}, {max_calls}]"
        );
        // A batch that keeps cut-short parts keeps want_kept_count when set, or one part when
        // want_kept is true.
        let want_kept = if tt.want_kept_count == 0 && tt.want_kept {
            1
        } else {
            tt.want_kept_count
        };
        assert_eq!(kept(&first), want_kept, "{name}: parts kept for a retry");
        if !tt.want_kept && tt.want_kept_count == 0 {
            assert!(
                first.deferred.is_none(),
                "{name}: expected no deferred state on the row"
            );
        }
        if !tt.want_log_substring.is_empty() {
            let got = logger.lines().join("\n");
            assert!(
                got.contains(tt.want_log_substring),
                "{name}: log = {got:?}, want substring {:?}",
                tt.want_log_substring
            );
            assert!(
                !got.contains("error type"),
                "{name}: log = {got:?}, must not fall back to generic error type summary"
            );
        }

        // Cloud Storage is back by the time that the next batch is resolved: whether that batch
        // calls it depends on the pause alone.
        stalled.store(false, Ordering::SeqCst);
        let mut next = queued_row(&opts(50), &[image_content(&[3])]);
        resolve(
            &bp,
            &Context::background(),
            false,
            std::slice::from_mut(&mut next),
        )
        .await;

        let part = &parts(&next)[0];
        let (want_calls, want_mode, want_text) = if tt.want_paused {
            (first_calls, "INLINE", UPLOAD_FAILED_TEXT)
        } else {
            (first_calls + 1, "GCS_REFERENCE", "[MEDIA OFFLOADED]")
        };
        assert_eq!(
            off.uploads().len(),
            want_calls,
            "{name}: offloader calls in all"
        );
        assert_eq!(
            (field(part, "storage_mode"), field(part, "text")),
            (want_mode, want_text),
            "{name}: the next batch's storage_mode and text"
        );
        ctx.cancel();
    }
}

/// Go `context.Cause(ctx)` returned by an offloader: the cause (an `Arc<Error>`) as an error
/// whose `source` is the cause, so that `errors.Is` sees through it.
#[derive(Debug)]
struct SharedError(Arc<Error>);

impl std::fmt::Display for SharedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for SharedError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}

/// A fakeOffloader hook: succeeds after `d` unless `ctx` ends first.
async fn after_or_done(ctx: Context, d: Duration) -> Result<(), Error> {
    tokio::select! {
        () = tokio::time::sleep(d) => Ok(()),
        () = ctx.done() => Err(ctx_err(&ctx)),
    }
}

/// Go `cfg.ProjectID, cfg.DatasetID, cfg.TableName = "p", "d", "t"` on `DefaultConfig()`.
fn pdt_config() -> Config {
    Config {
        project_id: "p".to_owned(),
        dataset_id: "d".to_owned(),
        table_name: "t".to_owned(),
        ..Config::default()
    }
}

const PDT_DEFAULT_STREAM: &str = "projects/p/datasets/d/tables/t/_default";

#[tokio::test(start_paused = true)]
async fn test_gcs_offloading_run_end_deadline_does_not_pause_healthy_storage() {
    let server = FakeWriteServer::new();
    let (tx, mut rx) = requests();
    // BigQuery AppendRows takes 1.5s.
    delayed_server(
        &server,
        Delayed {
            delay: ms(1500),
            requests: Some(tx),
            ..Delayed::default()
        },
    );
    let logger = Arc::new(RecordingLogger::default());
    // Cloud Storage is healthy: every upload succeeds in 300ms (well below the 10s timeout).
    let off = hooked(|ctx, _| Box::pin(after_or_done(ctx, ms(300))));
    let mut cfg = pdt_config();
    cfg.batch_size = 2;
    cfg.batch_flush_intv = Duration::from_secs(3600);
    cfg.shutdown_timeout = Duration::from_secs(2);
    cfg.logger = as_log(&logger);
    cfg.gcs_offloader = as_off(&off);
    let bp = processor(
        &Context::background(),
        Some(server.client().unwrap()),
        PDT_DEFAULT_STREAM,
        cfg,
    )
    .await;
    bp.start();
    let enqueue_image_row = |id: &str| {
        let mut q = tagged(
            queued_row(&reserving(&bp, 10), &[image_content(&[1, 2, 3])]),
            "LLM_REQUEST",
            "",
        );
        q.row.insert("session_id".to_owned(), RowValue::from(id));
        push_queue(&bp, q);
    };

    // First run: 3 image rows + 1 INVOCATION_COMPLETED = 2 batches of 2. Batch 1 uses ~1.8s
    // (300ms upload + 1.5s write), leaving ~200ms of the 2s budget for Batch 2's upload.
    for i in 0..3 {
        enqueue_image_row(&format!("run1-{i}"));
    }
    push_queue(&bp, event_row("INVOCATION_COMPLETED"));
    bp.flush_run_end().await;
    assert!(
        !upload_paused(&bp),
        "uploadCooldownUntil set after depleted second-batch run-end deadline, want 0"
    );

    // Second run: storage is still healthy. Both the deferred Batch 2 from the first run and
    // the new image row must be uploaded, not skipped during a cooldown.
    enqueue_image_row("run2-0");
    push_queue(&bp, event_row("INVOCATION_COMPLETED"));
    bp.flush_run_end().await;
    bp.close().await;

    assert!(
        !logger.contains("upload skipped during timeout cooldown"),
        "healthy storage was paused after run-end flush deadline expired:\n{logger}"
    );
    let rows = collect_rows(&mut rx, |rows| rows_of_type(rows, "LLM_REQUEST").len() >= 4).await;
    let llm_rows = rows_of_type(&rows, "LLM_REQUEST");
    assert!(
        llm_rows.len() >= 4,
        "got {} LLM_REQUEST rows, want >= 4",
        llm_rows.len()
    );
    for (i, r) in llm_rows.iter().enumerate() {
        let part = &r.parts("content_parts").unwrap()[0];
        assert!(
            mode_is(part, "GCS_REFERENCE"),
            "LLM_REQUEST row {i} (session_id={:?}) storage_mode = {:?} (text={:?}), want GCS_REFERENCE",
            r.get("session_id"),
            part.get("storage_mode"),
            part.get("text")
        );
    }
}

#[tokio::test(start_paused = true)]
async fn test_gcs_offloading_run_end_continuous_arrivals_do_not_pause_healthy_storage() {
    let server = FakeWriteServer::new();
    let (tx, _rx) = requests();
    delayed_server(
        &server,
        Delayed {
            delay: ms(100),
            requests: Some(tx),
            ..Delayed::default()
        },
    );
    let logger = Arc::new(RecordingLogger::default());
    let first_upload_started = Arc::new(tokio::sync::Notify::new());
    let off = {
        let started = Arc::clone(&first_upload_started);
        hooked(move |ctx, _| {
            started.notify_one();
            Box::pin(after_or_done(ctx, ms(60)))
        })
    };
    let mut cfg = pdt_config();
    cfg.batch_size = 2;
    cfg.batch_flush_intv = Duration::from_secs(3600);
    cfg.shutdown_timeout = Duration::from_secs(1);
    cfg.logger = as_log(&logger);
    cfg.gcs_offloader = as_off(&off);
    let bp = processor(
        &Context::background(),
        Some(server.client().unwrap()),
        PDT_DEFAULT_STREAM,
        cfg,
    )
    .await;
    bp.start();
    let new_image_row = {
        let bp = Arc::clone(&bp);
        move || {
            tagged(
                queued_row(&reserving(&bp, 10), &[image_content(&[1, 2, 3])]),
                "LLM_REQUEST",
                "",
            )
        }
    };

    // The first batch is queued before flush_run_end starts, so that it takes flush_mu first;
    // the feeder starts once the first upload is in flight under flush_run_end.
    push_queue(&bp, new_image_row());
    push_queue(&bp, new_image_row());
    let stop_feeder = tokio_util::sync::CancellationToken::new();
    let feeder = {
        let (bp, stop, started, new_image_row) = (
            Arc::clone(&bp),
            stop_feeder.clone(),
            Arc::clone(&first_upload_started),
            new_image_row.clone(),
        );
        tokio::spawn(async move {
            tokio::select! {
                () = started.notified() => {}
                () = stop.cancelled() => return,
            }
            let mut ticker = tokio::time::interval_at(Instant::now() + ms(20), ms(20));
            loop {
                tokio::select! {
                    () = stop.cancelled() => return,
                    _ = ticker.tick() => bp.append(new_image_row()),
                }
            }
        })
    };

    bp.flush_run_end().await;
    stop_feeder.cancel();
    feeder.await.unwrap();

    let calls_before_run2 = off.uploads().len();
    for _ in 0..10 {
        push_queue(&bp, new_image_row());
    }
    bp.flush_run_end().await;
    bp.close().await;

    assert!(
        !logger.contains("upload skipped during timeout cooldown"),
        "healthy storage was paused after continuous arrivals during run-end flush:\n{logger}"
    );
    let calls_in_run2 = off.uploads().len() - calls_before_run2;
    assert!(
        calls_in_run2 >= 10,
        "offloader calls after first run-end flush = {calls_in_run2}, want >= 10 (storage should not be paused)"
    );
}

#[tokio::test(start_paused = true)]
async fn test_gcs_offloading_flush_run_end_caps_second_batch_upload_at_half_remaining_budget() {
    let server = FakeWriteServer::new();
    let (tx, mut rx) = requests();
    // The delay stays 0 for the warm-up write below.
    let server_delay = Arc::new(AtomicI64::new(0));
    dynamic_delay_server(&server, Arc::clone(&server_delay), None, Some(tx));

    // Only the second batch has an upload, and that upload stalls until its context expires.
    let off = hooked(|ctx, _| Box::pin(wait_done(ctx)));
    let logger = Arc::new(RecordingLogger::default());
    let mut cfg = Config::default();
    cfg.batch_size = 1;
    cfg.batch_flush_intv = Duration::from_secs(3600);
    cfg.shutdown_timeout = Duration::from_secs(2);
    cfg.retry_config.max_retries = 0;
    cfg.gcs_offloader = as_off(&off);
    cfg.logger = as_log(&logger);
    let bp = processor(
        &Context::background(),
        Some(server.client().unwrap()),
        PDT_DEFAULT_STREAM,
        cfg,
    )
    .await;

    // Open the connection and the write stream first, so that setting them up does not use up
    // the run-end budget below.
    warm_up(&bp, &mut rx).await;
    server_delay.store(1_000_000_000, Ordering::SeqCst);

    // Batch 0: no upload, takes 1s in AppendRows, leaving ~1s of the 2s ShutdownTimeout.
    push_queue(&bp, event_row("USER_MESSAGE_RECEIVED"));
    // Batch 1: an image upload whose upload context is capped at half the remaining ~1s.
    push_queue(
        &bp,
        tagged(
            queued_row(&reserving(&bp, 10), &[image_content(&[1, 2, 3])]),
            "LLM_REQUEST",
            "",
        ),
    );

    bp.flush_run_end().await;

    let uploads = off.uploads();
    assert_eq!(uploads.len(), 1, "upload attempts");
    assert!(
        uploads[0].has_deadline
            && uploads[0].remaining >= ms(200)
            && uploads[0].remaining <= ms(700),
        "second batch upload context remaining = {:?} (hasDeadline={}), want in [200ms, 700ms]",
        uploads[0].remaining,
        uploads[0].has_deadline
    );
    assert!(
        upload_paused(&bp),
        "uploadCooldownUntil = 0, want non-zero (the second batch started with more than a quarter of ShutdownTimeout left, and its upload stalled)"
    );
    bp.close().await;
}

#[tokio::test(start_paused = true)]
async fn test_gcs_offloading_flush_run_end_pauses_batch_starting_with_thirty_percent_budget() {
    let server = FakeWriteServer::new();
    let (tx, mut rx) = requests();
    // The delay stays 0 for the warm-up write below (request 1).
    let server_delay = Arc::new(AtomicI64::new(0));
    dynamic_delay_server(&server, Arc::clone(&server_delay), None, Some(tx));

    let stalled = Arc::new(AtomicBool::new(true));
    let off = {
        let (server_delay, stalled) = (Arc::clone(&server_delay), Arc::clone(&stalled));
        hooked(move |ctx, _| {
            // Batch 0's 1.4s write (request 2) has completed; clear the delay so Batch 1's
            // inline fallback row (request 3) writes within what remains.
            server_delay.store(0, Ordering::SeqCst);
            let stalled = stalled.load(Ordering::SeqCst);
            Box::pin(async move {
                if stalled {
                    return wait_done(ctx).await;
                }
                Ok(())
            })
        })
    };
    let logger = Arc::new(RecordingLogger::default());
    let mut cfg = Config::default();
    cfg.batch_size = 1;
    cfg.batch_flush_intv = Duration::from_secs(3600);
    cfg.shutdown_timeout = Duration::from_secs(2);
    cfg.retry_config.max_retries = 0;
    cfg.gcs_offloader = as_off(&off);
    cfg.logger = as_log(&logger);
    let bp = processor(
        &Context::background(),
        Some(server.client().unwrap()),
        PDT_DEFAULT_STREAM,
        cfg,
    )
    .await;

    warm_up(&bp, &mut rx).await;
    server_delay.store(1_400_000_000, Ordering::SeqCst);

    let o = reserving(&bp, 32 * 1024);
    let text = |t: String| Content {
        role: "user".to_owned(),
        parts: vec![Part {
            text: t,
            ..Part::default()
        }],
    };
    // Batch 0: inline text below the 32 KiB offload threshold (no uploads); request 2 takes
    // 1.4s, leaving ~600ms (~30% of the 2s ShutdownTimeout) when Batch 1 starts.
    push_queue(
        &bp,
        tagged(
            queued_row(&o, &[text("batch-0-inline".to_owned())]),
            "USER_MESSAGE_RECEIVED",
            "batch-0-inline",
        ),
    );
    // Batch 1: a 40 KiB upload stalling for its ~300ms half-remaining window.
    push_queue(
        &bp,
        tagged(
            queued_row(&o, &[text("b".repeat(40 * 1024))]),
            "LLM_REQUEST",
            "batch-1-upload",
        ),
    );

    bp.flush_run_end().await;

    assert_eq!(off.uploads().len(), 1, "upload attempts");
    assert!(
        upload_paused(&bp),
        "uploadCooldownUntil = 0, want non-zero (Batch 1 started with ~30% of ShutdownTimeout left, and its upload stalled)"
    );
    assert_eq!(
        bp.flush_mu.lock().await.deferred_batch.len(),
        0,
        "deferredBatch length"
    );
    for (i, want_id) in ["batch-0-inline", "batch-1-upload"].into_iter().enumerate() {
        let req = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for request {} ({want_id})", i + 2))
            .expect("request");
        let rows = decode_arrow_rows(&req);
        assert!(
            rows.len() == 1 && rows[0].str("event_id") == Some(want_id),
            "request {} got {} rows, want 1 row with event_id {want_id:?}",
            i + 2,
            rows.len()
        );
        if i == 1 {
            assert!(
                mode_is(&rows[0].parts("content_parts").unwrap()[0], "INLINE"),
                "request 3 part storage_mode, want INLINE"
            );
        }
    }
    bp.close().await;
}

#[tokio::test(start_paused = true)]
async fn test_gcs_offloading_flush_run_end_batch_after_empty_batch_pauses_stalled_storage() {
    let server = FakeWriteServer::new();
    let (tx, _rx) = requests();
    delayed_server(
        &server,
        Delayed {
            delay: ms(10),
            requests: Some(tx),
            ..Delayed::default()
        },
    );
    let off = hooked(|ctx, _| Box::pin(wait_done(ctx)));
    let logger = Arc::new(RecordingLogger::default());
    let mut cfg = Config::default();
    cfg.batch_size = 1;
    cfg.batch_flush_intv = Duration::from_secs(3600);
    cfg.shutdown_timeout = Duration::from_secs(1);
    cfg.retry_config.max_retries = 0;
    cfg.gcs_offloader = as_off(&off);
    cfg.logger = as_log(&logger);
    let bp = processor(
        &Context::background(),
        Some(server.client().unwrap()),
        PDT_DEFAULT_STREAM,
        cfg,
    )
    .await;
    bp.start();

    // Batch 0 has no uploads and completes AppendRows in 10ms (~990ms of 1s remaining).
    push_queue(&bp, event_row("USER_MESSAGE_RECEIVED"));
    // Batch 1 starts with more than a quarter of the run-end budget left; when storage stalls
    // until Batch 1's ~495ms upload deadline, the cooldown must be armed.
    push_queue(
        &bp,
        tagged(
            queued_row(&reserving(&bp, 10), &[image_content(&[1, 2, 3])]),
            "LLM_REQUEST",
            "",
        ),
    );

    bp.flush_run_end().await;

    assert!(
        upload_paused(&bp),
        "uploadCooldownUntil = 0, want non-zero when storage stalls for all of the upload time of a batch that starts with more than a quarter of the run-end budget left"
    );
    bp.close().await;
}

#[tokio::test(start_paused = true)]
async fn test_resolve_uploads_partial_outage_brownout_pauses_uploads() {
    let call_num = Arc::new(AtomicI32::new(0));
    let off = {
        let call_num = Arc::clone(&call_num);
        hooked(move |ctx, _| {
            // Every odd upload succeeds at once; every even one hangs until the batch's
            // GCSUploadTimeout expires (partial Cloud Storage brownout).
            let odd = (call_num.fetch_add(1, Ordering::SeqCst) + 1) % 2 == 1;
            Box::pin(async move {
                if odd {
                    return Ok(());
                }
                wait_done(ctx).await
            })
        })
    };
    let bp = bare(Config {
        gcs_offloader: as_off(&off),
        gcs_max_concurrent_uploads: 4,
        gcs_upload_timeout: ms(60),
        logger: as_log(&Arc::new(RecordingLogger::default())),
        ..Config::zero()
    });

    // Batch 0 experiences a partial outage and must arm the cooldown.
    let mut batch0 = vec![queued_row(
        &opts(50),
        &[image_content(&[1]), image_content(&[2])],
    )];
    resolve(&bp, &Context::background(), false, &mut batch0).await;
    assert!(
        upload_paused(&bp),
        "uploadCooldownUntil = 0 after partial-outage batch timeout, want cooldown armed"
    );
    assert_eq!(off.uploads().len(), 2, "offloader calls after batch 0");

    // Batches 1..3 run while cooldown is active and must return at once.
    let start = Instant::now();
    for i in 1..4_u8 {
        let mut batch = vec![queued_row(
            &opts(50),
            &[image_content(&[2 * i + 1]), image_content(&[2 * i + 2])],
        )];
        resolve(&bp, &Context::background(), false, &mut batch).await;
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed < ms(30),
        "batches 1..3 during brownout cooldown took {elapsed:?}, want < 30ms"
    );
    assert_eq!(
        off.uploads().len(),
        2,
        "total offloader calls across 4 batches (batches 1..3 skipped during cooldown)"
    );
}

#[tokio::test(start_paused = true)]
async fn test_resolve_uploads_concurrency_limited_brownout_pauses_uploads() {
    let call_num = Arc::new(AtomicI32::new(0));
    let off = {
        let call_num = Arc::clone(&call_num);
        hooked(move |ctx, _| {
            let n = call_num.fetch_add(1, Ordering::SeqCst) + 1;
            Box::pin(async move {
                if n == 1 {
                    return after_or_done(ctx, ms(35)).await;
                }
                wait_done(ctx).await
            })
        })
    };
    let bp = bare(Config {
        gcs_offloader: as_off(&off),
        gcs_max_concurrent_uploads: 1,
        gcs_upload_timeout: ms(60),
        logger: as_log(&Arc::new(RecordingLogger::default())),
        ..Config::zero()
    });
    let mut batch: Vec<QueuedRow> = vec![queued_row(
        &opts(50),
        &[image_content(&[1]), image_content(&[2])],
    )];
    resolve(&bp, &Context::background(), false, &mut batch).await;
    assert!(
        upload_paused(&bp),
        "uploadCooldownUntil = 0 after concurrency-limited stall, want cooldown armed"
    );
}
