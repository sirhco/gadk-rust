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

//! `batch_processor_test.go` ports, Go file order from `TestBatchProcessor_ClosePreservesInflightAndQueuedBatchesUnderStalledUploads` through `TestBatchProcessor_FlushRunEndSucceededWriteReleasesCutShortUploadBytes`.

use super::support::{dynamic_delay_server, offload_opts, row_count};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::time::Duration;

use gadk_genai::Content;
use gadk_platform::BoxFuture;
use pretty_assertions::assert_eq;
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::support::{
    Delayed, HookOffloader, OffloadHook, arrival_recording_server, collect_rows, content_parts_of,
    decode_arrow_rows, delayed_server, event_row, image_content, pending_upload_bytes, processor,
    push_queue, queue_len, queued_row, recording_server, requests, stalling_server,
    stalling_write_server, upload_paused,
};
use crate::batch_processor::BatchProcessor;
use crate::bigquery::fake::RecordingLogger;
use crate::config::{
    Config, DropStats, GcsContentOffloader, GcsUploadRequest, Logger, RetryConfig,
};
use crate::context::{Context, ContextError};
use crate::error::Error;
use crate::event_types::{EVENT_LLM_REQUEST, EVENT_USER_MESSAGE_RECEIVED};
use crate::formatter::{PartsOptions, UPLOAD_FAILED_TEXT};
use crate::offload::MEDIA_OFFLOADED_TEXT;
use crate::row::{Row, RowValue};
use crate::testutil::fake_write_server::FakeWriteServer;

/// The `_default` stream every test here writes to.
const DEFAULT_STREAM: &str = "projects/p/datasets/d/tables/t/_default";

/// `DefaultConfig()` with `BatchFlushIntv = time.Hour` and the given batch size and shutdown
/// timeout (the settings every test here starts from).
fn config(batch_size: i64, shutdown_timeout: Duration) -> Config {
    Config {
        batch_size,
        batch_flush_intv: Duration::from_secs(3600),
        shutdown_timeout,
        ..Config::default()
    }
}

/// Go `NewBatchProcessor(context.Background(), wc, "projects/p/…/_default", cfg)` over a new
/// client of `server`.
async fn on_default_stream(server: &FakeWriteServer, cfg: Config) -> Arc<BatchProcessor> {
    processor(
        &Context::background(),
        Some(server.client().unwrap()),
        DEFAULT_STREAM,
        cfg,
    )
    .await
}

/// Go `cfg.GCSOffloader = off` for a test-held `off`.
fn offloader(off: &Arc<HookOffloader>) -> Option<Arc<dyn GcsContentOffloader>> {
    Some(Arc::clone(off) as Arc<dyn GcsContentOffloader>)
}

/// `logger` as a config logger.
fn logger_of(logger: &Arc<RecordingLogger>) -> Option<Arc<dyn Logger>> {
    Some(Arc::clone(logger) as Arc<dyn Logger>)
}

/// A `fakeOffloader.hook`.
fn hook(
    f: impl Fn(Context, GcsUploadRequest) -> BoxFuture<'static, Result<(), Error>>
    + Send
    + Sync
    + 'static,
) -> Arc<HookOffloader> {
    let f: OffloadHook = Arc::new(f);
    Arc::new(HookOffloader::with_hook(f))
}

/// Go `<-ctx.Done(); return ctx.Err()`.
async fn until_done(ctx: &Context) -> Result<(), Error> {
    ctx.done().await;
    Err(Error::Context(ctx.err().unwrap_or(ContextError::Canceled)))
}

/// Go `select { case <-time.After(d): return nil; case <-ctx.Done(): return ctx.Err() }`.
async fn sleep_or_done(ctx: &Context, d: Duration) -> Result<(), Error> {
    tokio::select! {
        () = tokio::time::sleep(d) => Ok(()),
        r = until_done(ctx) => r,
    }
}

/// Go `row := queuedRow(opts, contents...); row["event_type"] = EventLLMRequest`.
fn llm_row(opts: &PartsOptions, contents: &[Content]) -> crate::row::QueuedRow {
    let mut row = queued_row(opts, contents);
    row.row
        .insert("event_type".to_owned(), RowValue::from(EVENT_LLM_REQUEST));
    row
}

/// Go `row[key] = value`.
fn set(row: &mut crate::row::QueuedRow, key: &str, value: &str) {
    row.row.insert(key.to_owned(), RowValue::from(value));
}

/// Go `len(bp.deferredBatch)`.
async fn deferred_len(bp: &BatchProcessor) -> usize {
    bp.flush_mu.lock().await.deferred_batch.len()
}

/// Go `part[key] == value` on a decoded `content_parts` entry.
fn part_is(part: &Row, key: &str, value: &str) -> bool {
    part.get(key) == Some(&RowValue::from(value))
}

/// Go `uploadFailureLines(l)` (gcs_offloader_test.go).
fn upload_failure_lines(logger: &RecordingLogger) -> Vec<String> {
    logger
        .lines()
        .into_iter()
        .filter(|l| l.contains("GCS upload failed"))
        .collect()
}

/// Go `time.Duration` nanoseconds of `d`.
fn nanos(d: Duration) -> i64 {
    i64::try_from(d.as_nanos()).unwrap()
}

// ---------------------------------------------------------------------------------------------
// TestBatchProcessor_ClosePreservesInflightAndQueuedBatchesUnderStalledUploads
// ---------------------------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
#[ignore = "needs Task 10"]
async fn test_batch_processor_close_preserves_inflight_and_queued_batches_under_stalled_uploads() {
    close_cancels_inflight_stalled_upload().await;
    close_caps_upload_deadline_to_half_shutdown_timeout().await;
    close_shares_one_budget_across_queued_batches().await;
    close_starts_drain_budget_after_inflight_write().await;
    close_logs_and_releases_unwritable_batches().await;
    background_flush_starts_append_timeout_after_uploads().await;
    close_writes_every_queued_batch_when_storage_stalls().await;
    close_shares_half_shutdown_timeout_between_uploads().await;
    close_uploads_queued_rows_after_canceling_inflight_upload().await;
    run_end_flush_stops_waiting_once_one_batch_times_out().await;
    run_end_flush_shares_one_shutdown_timeout().await;
}

/// "inflight size-triggered flush with stalled upload does not lose row on Close"
async fn close_cancels_inflight_stalled_upload() {
    let server = FakeWriteServer::new();
    let (tx, mut rx) = requests();
    recording_server(&server, tx);

    let started = Arc::new(tokio::sync::Notify::new());
    let upload_started = Arc::clone(&started);
    let off = hook(move |ctx, _| {
        let started = Arc::clone(&upload_started);
        Box::pin(async move {
            started.notify_one();
            until_done(&ctx).await
        })
    });
    let cfg = Config {
        gcs_upload_timeout: Duration::from_secs(5),
        gcs_offloader: offloader(&off),
        logger: logger_of(&Arc::new(RecordingLogger::default())),
        ..config(1, Duration::from_millis(500))
    };
    let bp = on_default_stream(&server, cfg).await;
    bp.start();

    // Open the stream on a short-lived context that is canceled before Close's final write,
    // verifying that the stream writer reopens a canceled stream.
    let stale_ctx = Context::background().with_cancel();
    bp.write_batch(&stale_ctx, vec![event_row("WARMUP")])
        .await
        .unwrap_or_else(|e| panic!("warmup writeBatch: {e}"));
    rx.recv().await.unwrap();
    stale_ctx.cancel();

    bp.append(llm_row(&offload_opts(None), &[image_content(&[1, 2, 3])]));

    tokio::time::timeout(Duration::from_secs(2), started.notified())
        .await
        .expect("timed out waiting for in-flight upload to start");

    let start = Instant::now();
    bp.close().await;
    // Close cancels the upload that is in flight. It does not wait for the 5s that the upload
    // could still take.
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(2),
        "Close() took {elapsed:?}, want < 2s: it has to cancel the in-flight upload instead of waiting for GCSUploadTimeout"
    );

    match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
        Ok(Some(req)) => assert_eq!(row_count(&req), 1, "persisted row count"),
        _ => panic!("in-flight batch was dropped when Close canceled the stalled upload"),
    }
}

/// "shutdown flush caps upload deadline to half of ShutdownTimeout so write succeeds"
async fn close_caps_upload_deadline_to_half_shutdown_timeout() {
    let server = FakeWriteServer::new();
    let (tx, mut rx) = requests();
    recording_server(&server, tx);
    let off = hook(|ctx, _| Box::pin(async move { until_done(&ctx).await }));
    let shutdown_timeout = Duration::from_millis(300);
    let cfg = Config {
        gcs_upload_timeout: Duration::from_secs(10),
        gcs_offloader: offloader(&off),
        logger: logger_of(&Arc::new(RecordingLogger::default())),
        ..config(100, shutdown_timeout)
    };
    let bp = on_default_stream(&server, cfg).await;
    bp.start();

    bp.append(llm_row(&offload_opts(None), &[image_content(&[9])]));
    bp.append(event_row(EVENT_USER_MESSAGE_RECEIVED));

    let start = Instant::now();
    bp.close().await;
    let elapsed = start.elapsed();
    assert!(
        elapsed < shutdown_timeout,
        "Close() took {elapsed:?}, want < {shutdown_timeout:?} (uploads should use at most half of ShutdownTimeout)"
    );

    match rx.try_recv() {
        Ok(req) => assert_eq!(row_count(&req), 2, "persisted row count"),
        Err(_) => panic!("queued rows were lost on Close when upload stalled"),
    }
}

/// "Close shares a single ShutdownTimeout budget across multiple queued batches"
async fn close_shares_one_budget_across_queued_batches() {
    let server = FakeWriteServer::new();
    stalling_server(&server);
    let mut cfg = Config {
        gcs_offloader: offloader(&Arc::new(HookOffloader::default())),
        logger: logger_of(&Arc::new(RecordingLogger::default())),
        ..config(1, Duration::from_millis(80))
    };
    cfg.retry_config.max_retries = 0;
    let bp = on_default_stream(&server, cfg).await;
    bp.start();

    // Send rows straight into the queue so no background flush is spawned and Close drains
    // all five batches deterministically.
    for _ in 0..5 {
        push_queue(
            &bp,
            llm_row(&offload_opts(Some(&bp)), &[image_content(&[1, 2, 3, 4])]),
        );
    }

    let start = Instant::now();
    bp.close().await;
    let elapsed = start.elapsed();
    // Five 1-row batches renewing an 80ms budget per batch would take >= 400ms.
    assert!(
        elapsed < Duration::from_millis(250),
        "Close() took {elapsed:?} across 5 stalled batches, want < 250ms (single shared ShutdownTimeout)"
    );
    assert_eq!(
        pending_upload_bytes(&bp),
        0,
        "pendingUploadBytes after Close"
    );
    assert_eq!(
        bp.drop_stats(),
        DropStats {
            append_failure_drops: 1,
            shutdown_drops: 4,
            total_drops: 5,
            ..DropStats::default()
        },
        "DropStats() after stalled Close"
    );
}

/// "Close starts its drain budget after the in-flight write finishes"
async fn close_starts_drain_budget_after_inflight_write() {
    let server = FakeWriteServer::new();
    let (tx, mut rx) = requests();
    let (received_tx, mut received) = mpsc::channel(8);
    // A write takes 300ms. That fits in the 450ms ShutdownTimeout once but not twice, as it
    // would have to if waiting for the in-flight write used up the budget that the queued row
    // is written under.
    delayed_server(
        &server,
        Delayed {
            delay: Duration::from_millis(300),
            requests: Some(tx),
            received: Some(received_tx),
            ..Delayed::default()
        },
    );
    let logger = Arc::new(RecordingLogger::default());
    let mut cfg = Config {
        logger: logger_of(&logger),
        ..config(1, Duration::from_millis(450))
    };
    cfg.retry_config.max_retries = 0;
    let bp = on_default_stream(&server, cfg).await;
    bp.start();

    bp.append(event_row(EVENT_USER_MESSAGE_RECEIVED)); // starts a background flush
    tokio::time::timeout(Duration::from_secs(2), received.recv())
        .await
        .expect("timed out waiting for the background flush to start its write");
    bp.append(event_row(EVENT_LLM_REQUEST)); // waits in the queue behind that write

    bp.close().await;

    for i in 0..2 {
        match rx.try_recv() {
            Ok(req) => assert_eq!(row_count(&req), 1, "request {i} rows"),
            Err(_) => panic!(
                "Close persisted {i} of 2 batches: the queued row was dropped after waiting for the in-flight write"
            ),
        }
    }
    assert!(
        !logger.contains("Failed to write batch"),
        "unexpected write failure in the log:\n{logger}"
    );
}

/// "Close logs and releases the batches it cannot write within its budget"
async fn close_logs_and_releases_unwritable_batches() {
    let server = FakeWriteServer::new();
    stalling_server(&server);
    let logger = Arc::new(RecordingLogger::default());
    let mut cfg = Config {
        gcs_offloader: offloader(&Arc::new(HookOffloader::default())),
        logger: logger_of(&logger),
        ..config(1, Duration::from_millis(80))
    };
    cfg.retry_config.max_retries = 0;
    let bp = on_default_stream(&server, cfg).await;
    bp.start();

    // The rows go straight into the queue, so that no background flush is running and Close
    // drains all three batches.
    for _ in 0..3 {
        push_queue(
            &bp,
            llm_row(&offload_opts(Some(&bp)), &[image_content(&[1, 2, 3, 4])]),
        );
    }
    assert_eq!(
        pending_upload_bytes(&bp),
        12,
        "pendingUploadBytes before Close"
    );

    bp.close().await;

    // The first batch uses up the budget on the stalled write, so at least the other two are
    // dropped.
    let dropped = "Failed to write batch of 1 events: context deadline exceeded";
    let got = logger.to_string().matches(dropped).count();
    assert!(
        got >= 2,
        "log has {got} lines with {dropped:?}, want at least 2:\n{logger}"
    );
    assert_eq!(
        pending_upload_bytes(&bp),
        0,
        "pendingUploadBytes after Close"
    );
}

/// "background flush starts append timeout after uploads finish"
async fn background_flush_starts_append_timeout_after_uploads() {
    let server = FakeWriteServer::new();
    let (tx, mut rx) = requests();
    delayed_server(
        &server,
        Delayed {
            delay: Duration::from_millis(150),
            requests: Some(tx),
            ..Delayed::default()
        },
    );
    let off = hook(|ctx, _| {
        Box::pin(async move { sleep_or_done(&ctx, Duration::from_millis(300)).await })
    });
    let mut cfg = Config {
        gcs_upload_timeout: Duration::from_millis(400),
        gcs_offloader: offloader(&off),
        logger: logger_of(&Arc::new(RecordingLogger::default())),
        ..config(2, Duration::from_millis(400))
    };
    cfg.retry_config.max_retries = 0;
    let bp = on_default_stream(&server, cfg).await;
    bp.start();

    bp.append(llm_row(
        &offload_opts(Some(&bp)),
        &[image_content(&[1, 2, 3])],
    ));
    // Triggers the background flush: a 300ms upload + a 150ms write > 400ms if shared, but it
    // succeeds when the write timeout starts after the upload.
    bp.append(event_row(EVENT_USER_MESSAGE_RECEIVED));

    match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
        Ok(Some(req)) => assert_eq!(row_count(&req), 2, "persisted row count"),
        _ => panic!(
            "background flush lost batch when upload used 300ms of 400ms GCSUploadTimeout before 150ms BigQuery append"
        ),
    }
    bp.close().await;
}

/// "Close writes every queued batch when Cloud Storage stalls"
async fn close_writes_every_queued_batch_when_storage_stalls() {
    const BATCHES: usize = 5;
    let server = FakeWriteServer::new();
    let (tx, mut rx) = requests();
    // The writes take 5 x 80ms = 400ms. They fit in the 600ms of ShutdownTimeout, but not in
    // the 300ms that would be left of it once the uploads of the first batch had waited for
    // their 300ms, which is half of it.
    delayed_server(
        &server,
        Delayed {
            delay: Duration::from_millis(80),
            requests: Some(tx),
            ..Delayed::default()
        },
    );
    let off = hook(|ctx, _| Box::pin(async move { until_done(&ctx).await }));
    let logger = Arc::new(RecordingLogger::default());
    let mut cfg = Config {
        gcs_offloader: offloader(&off),
        logger: logger_of(&logger),
        ..config(1, Duration::from_millis(600))
    };
    cfg.retry_config.max_retries = 0;
    let bp = on_default_stream(&server, cfg).await;
    bp.start();

    // The rows go straight into the queue, so that no background flush is running and Close
    // drains all of the batches.
    for _ in 0..BATCHES {
        push_queue(
            &bp,
            llm_row(&offload_opts(Some(&bp)), &[image_content(&[1, 2, 3, 4])]),
        );
    }

    bp.close().await;

    let rows = collect_rows(&mut rx, |rows| rows.len() >= BATCHES).await;
    assert_eq!(rows.len(), BATCHES, "Close wrote {} rows", rows.len());
    for (i, row) in rows.iter().enumerate() {
        assert!(
            part_is(&content_parts_of(row)[0], "text", UPLOAD_FAILED_TEXT),
            "row {i} part text = {:?}, want {UPLOAD_FAILED_TEXT:?}",
            content_parts_of(row)[0].get("text")
        );
    }
    assert_eq!(
        off.uploads().len(),
        1,
        "the offloader was called {} times, want once: the batches after the first skip Cloud Storage",
        off.uploads().len()
    );
    assert!(
        !logger.contains("Failed to write batch"),
        "unexpected write failure in the log:\n{logger}"
    );
    assert_eq!(
        pending_upload_bytes(&bp),
        0,
        "pendingUploadBytes after Close"
    );
}

/// "Close shares half a ShutdownTimeout between the uploads of all the queued batches"
async fn close_shares_half_shutdown_timeout_between_uploads() {
    const BATCHES: usize = 16;
    let server = FakeWriteServer::new();
    let (tx, mut rx) = requests();
    recording_server(&server, tx);
    // Cloud Storage is slow, not stalled: every upload takes 50ms and succeeds unless its
    // context ends first. The uploads of the drain have 400ms in all, which is half of
    // ShutdownTimeout, so at most 8 of them can finish. A drain that gave each batch that
    // long, or all of ShutdownTimeout, would upload every one of the 16.
    let off = hook(|ctx, _| {
        Box::pin(async move { sleep_or_done(&ctx, Duration::from_millis(50)).await })
    });
    let logger = Arc::new(RecordingLogger::default());
    let mut cfg = Config {
        gcs_offloader: offloader(&off),
        logger: logger_of(&logger),
        ..config(1, Duration::from_millis(800))
    };
    cfg.retry_config.max_retries = 0;
    let bp = on_default_stream(&server, cfg).await;
    bp.start();

    for _ in 0..BATCHES {
        push_queue(
            &bp,
            llm_row(&offload_opts(Some(&bp)), &[image_content(&[1, 2, 3, 4])]),
        );
    }

    bp.close().await;

    let rows = collect_rows(&mut rx, |rows| rows.len() >= BATCHES).await;
    assert_eq!(rows.len(), BATCHES, "Close wrote {} rows", rows.len());
    let (mut uploaded, mut failed) = (0, 0);
    for row in &rows {
        let part = &content_parts_of(row)[0];
        if part_is(part, "storage_mode", "GCS_REFERENCE") {
            uploaded += 1;
        } else if part_is(part, "text", UPLOAD_FAILED_TEXT) {
            failed += 1;
        }
    }
    assert_eq!(
        uploaded + failed,
        BATCHES,
        "got {uploaded} uploaded and {failed} failed rows, want {BATCHES} rows in all"
    );
    assert!(
        (2..=8).contains(&uploaded),
        "{uploaded} of {BATCHES} rows were uploaded, want between 2 and 8: the uploads of all the batches share half of ShutdownTimeout"
    );
    assert!(
        !logger.contains("Failed to write batch"),
        "unexpected write failure in the log:\n{logger}"
    );
    assert_eq!(
        pending_upload_bytes(&bp),
        0,
        "pendingUploadBytes after Close"
    );
}

/// "Close uploads the queued rows after canceling the upload that was in flight"
async fn close_uploads_queued_rows_after_canceling_inflight_upload() {
    let server = FakeWriteServer::new();
    let (tx, mut rx) = requests();
    recording_server(&server, tx);
    // Cloud Storage is healthy: only the first upload is slow enough to be still running
    // when Close cancels it. That says nothing about Cloud Storage, so the upload of the
    // queued row must not be skipped.
    let calls = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(tokio::sync::Notify::new());
    let (hook_calls, upload_started) = (Arc::clone(&calls), Arc::clone(&started));
    let off = hook(move |ctx, _| {
        let first = hook_calls.fetch_add(1, Ordering::SeqCst) == 0;
        let started = Arc::clone(&upload_started);
        Box::pin(async move {
            if !first {
                return Ok(());
            }
            started.notify_one();
            until_done(&ctx).await
        })
    });
    let cfg = Config {
        gcs_offloader: offloader(&off),
        logger: logger_of(&Arc::new(RecordingLogger::default())),
        ..config(1, Duration::from_secs(10))
    };
    let bp = on_default_stream(&server, cfg).await;
    bp.start();

    // Starts a background flush that waits for its upload.
    bp.append(llm_row(&offload_opts(Some(&bp)), &[image_content(&[1])]));
    tokio::time::timeout(Duration::from_secs(2), started.notified())
        .await
        .expect("timed out waiting for the upload of the background flush to start");
    push_queue(
        &bp,
        llm_row(&offload_opts(Some(&bp)), &[image_content(&[2])]),
    );

    bp.close().await;

    let rows = collect_rows(&mut rx, |rows| rows.len() >= 2).await;
    let (mut canceled, mut uploaded) = (0, 0);
    for row in &rows {
        let part = &content_parts_of(row)[0];
        if part_is(part, "storage_mode", "GCS_REFERENCE") {
            uploaded += 1;
        } else if part_is(part, "text", UPLOAD_FAILED_TEXT) {
            canceled += 1;
        }
    }
    assert!(
        canceled == 1 && uploaded == 1,
        "got {canceled} rows whose upload was canceled and {uploaded} uploaded, want 1 and 1"
    );
    assert_eq!(
        pending_upload_bytes(&bp),
        0,
        "pendingUploadBytes after Close"
    );
}

/// "run-end flush stops waiting for Cloud Storage once the uploads of one batch time out"
async fn run_end_flush_stops_waiting_once_one_batch_times_out() {
    const BATCHES: usize = 4;
    let server = FakeWriteServer::new();
    let (tx, mut rx) = requests();
    recording_server(&server, tx);
    let off = hook(|ctx, _| Box::pin(async move { until_done(&ctx).await }));
    let logger = Arc::new(RecordingLogger::default());
    let cfg = Config {
        gcs_offloader: offloader(&off),
        logger: logger_of(&logger),
        ..config(1, Duration::from_millis(800))
    };
    let bp = on_default_stream(&server, cfg).await;
    bp.start();

    for _ in 0..BATCHES {
        push_queue(
            &bp,
            llm_row(&offload_opts(Some(&bp)), &[image_content(&[1, 2, 3, 4])]),
        );
    }

    let start = Instant::now();
    bp.flush_run_end().await;
    let elapsed = start.elapsed();

    // The uploads of the first batch wait for half of the ShutdownTimeout, 400ms. If each
    // batch waited for half of what was left, the flush would take 400 + 200 + 100 + 50ms.
    assert!(
        elapsed < Duration::from_millis(600),
        "flushRunEnd() took {elapsed:?}, want < 600ms: only the first batch waits for Cloud Storage"
    );
    assert_eq!(off.uploads().len(), 1, "the offloader calls, want once");
    let rows = collect_rows(&mut rx, |rows| rows.len() >= BATCHES).await;
    assert_eq!(
        rows.len(),
        BATCHES,
        "flushRunEnd() wrote {} rows",
        rows.len()
    );
    assert!(
        !logger.contains("Failed to write batch"),
        "unexpected write failure in the log:\n{logger}"
    );
    bp.close().await;
}

/// "run-end flush shares one ShutdownTimeout across the batches it writes"
async fn run_end_flush_shares_one_shutdown_timeout() {
    const BATCHES: usize = 5;
    let server = FakeWriteServer::new();
    stalling_server(&server);
    let mut cfg = Config {
        logger: logger_of(&Arc::new(RecordingLogger::default())),
        ..config(1, Duration::from_millis(80))
    };
    cfg.retry_config.max_retries = 0;
    let bp = on_default_stream(&server, cfg).await;
    bp.start();

    for _ in 0..BATCHES {
        push_queue(&bp, event_row(EVENT_USER_MESSAGE_RECEIVED));
    }

    let start = Instant::now();
    bp.flush_run_end().await;
    let elapsed = start.elapsed();

    // Five batches that each had an 80ms budget would take 400ms. The stalled write of the
    // first batch uses it up, and the others stay queued for the next flush or for Close.
    assert!(
        elapsed < Duration::from_millis(250),
        "flushRunEnd() took {elapsed:?} across {BATCHES} batches, want < 250ms (one shared ShutdownTimeout)"
    );
    assert_eq!(
        queue_len(&bp),
        BATCHES - 1,
        "rows still queued: a flush that runs out of budget leaves the batches it did not write queued"
    );
    bp.close().await;
}

// ---------------------------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn test_batch_processor_close_timeout_counts_all_lost_rows_in_drop_stats() {
    // "Close timeout with BatchSize=5 and ShutdownTimeout=300ms over 66 events counts all 56
    // lost rows"
    {
        let server = FakeWriteServer::new();
        let (tx, mut rx) = requests();
        delayed_server(
            &server,
            Delayed {
                delay: Duration::from_millis(40),
                stall_after_req: 2,
                requests: Some(tx),
                ..Delayed::default()
            },
        );
        let shutdown_timeout = Duration::from_millis(300);
        let cfg = Config {
            retry_config: RetryConfig {
                max_retries: 0,
                initial_delay: Duration::from_millis(1),
                max_delay: Duration::from_millis(1),
                multiplier: 1.0,
            },
            logger: logger_of(&Arc::new(RecordingLogger::default())),
            ..config(5, shutdown_timeout)
        };
        let bp = on_default_stream(&server, cfg).await;
        bp.start();

        // Send rows straight into the queue so no background flush is spawned and Close
        // drains all batches deterministically.
        const TOTAL_EVENTS: i64 = 66;
        for i in 0..TOTAL_EVENTS {
            push_queue(&bp, event_row(&format!("evt_{i}")));
        }

        let start = Instant::now();
        bp.close().await;
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(150),
            "Close() returned in {elapsed:?}, expected it to wait until ShutdownTimeout ({shutdown_timeout:?}) expired on stalled server"
        );

        let mut persisted_rows = 0;
        while let Ok(req) = rx.try_recv() {
            persisted_rows += row_count(&req);
        }
        assert_eq!(persisted_rows, 10, "persisted rows (2 batches of 5)");

        let stats = bp.drop_stats();
        assert_eq!(
            stats,
            DropStats {
                append_failure_drops: 5,
                shutdown_drops: 51,
                total_drops: 56,
                ..DropStats::default()
            },
            "DropStats()"
        );
        assert_eq!(
            persisted_rows + stats.total_drops,
            TOTAL_EVENTS,
            "persistedRows + TotalDrops"
        );
    }

    // "writeBatchWithContexts counts batch in AppendFailureDrops when writeCtx is already
    // canceled"
    {
        let mut cfg = Config::default();
        cfg.retry_config.max_retries = 0;
        let bp = processor(&Context::background(), None, "test_stream", cfg).await;
        let canceled_ctx = Context::background().with_cancel();
        canceled_ctx.cancel();
        let mut batch = vec![event_row("row_1"), event_row("row_2"), event_row("row_3")];
        let err = bp
            .write_batch_with_contexts(&Context::background(), &canceled_ctx, false, &mut batch)
            .await
            .expect_err("writeBatchWithContexts() error = nil, want context canceled");
        assert!(
            err.is_context(ContextError::Canceled),
            "writeBatchWithContexts() error = {err}, want context canceled"
        );
        assert_eq!(
            bp.drop_stats(),
            DropStats {
                append_failure_drops: 3,
                total_drops: 3,
                ..DropStats::default()
            },
            "DropStats()"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn test_batch_processor_stalled_append_rows_ends_at_shutdown_timeout() {
    let server = FakeWriteServer::new();
    let (received_tx, mut received) = mpsc::channel(2);
    stalling_write_server(&server, received_tx);
    let shutdown_timeout = Duration::from_millis(200);
    let mut cfg = config(10, shutdown_timeout);
    cfg.retry_config.max_retries = 0;
    let bp = on_default_stream(&server, cfg).await;
    bp.start();

    // Go runWithin: f must return within 5 seconds; how long it took.
    async fn run_within(name: &str, f: impl std::future::Future<Output = ()>) -> Duration {
        let start = Instant::now();
        tokio::time::timeout(Duration::from_secs(5), f)
            .await
            .unwrap_or_else(|_| {
                panic!("{name} did not return while the AppendRows response was stalled")
            });
        start.elapsed()
    }
    async fn await_request(received: &mut mpsc::Receiver<()>, name: &str) {
        if !matches!(
            tokio::time::timeout(Duration::from_secs(5), received.recv()).await,
            Ok(Some(()))
        ) {
            panic!("server did not receive the batch written by {name}");
        }
    }

    bp.append(event_row("stalled_flush_row"));
    let elapsed = run_within("flush", bp.flush()).await;
    assert!(
        elapsed >= shutdown_timeout,
        "flush returned after {elapsed:?}, before ShutdownTimeout ({shutdown_timeout:?}) expired"
    );
    await_request(&mut received, "flush").await;

    bp.append(event_row("stalled_drain_row"));
    run_within("Close", bp.close()).await;
    await_request(&mut received, "Close").await;
}

#[tokio::test(start_paused = true)]
async fn test_batch_processor_flush_run_end_does_not_drop_batch_when_budget_expires() {
    let server = FakeWriteServer::new();
    let (tx, mut rx) = requests();
    // Each batch write takes 250ms: Batch 0 succeeds in 250ms, leaving 150ms of the 400ms
    // ShutdownTimeout, so Batch 1's 250ms write times out during the first flushRunEnd and
    // must be deferred to the next flush rather than lost.
    delayed_server(
        &server,
        Delayed {
            delay: Duration::from_millis(250),
            requests: Some(tx),
            ..Delayed::default()
        },
    );
    let logger = Arc::new(RecordingLogger::default());
    let mut cfg = Config {
        logger: logger_of(&logger),
        ..config(2, Duration::from_millis(400))
    };
    cfg.retry_config.max_retries = 0;
    let bp = on_default_stream(&server, cfg).await;
    bp.start();

    let want_order = ["row-0", "row-1", "row-2", "row-3"];
    for ev in want_order {
        push_queue(&bp, event_row(ev));
    }

    bp.flush_run_end().await;
    assert!(
        !logger.contains("Failed to write batch"),
        "flushRunEnd logged a batch drop error when its budget expired:\n{logger}"
    );

    // A subsequent flush writes the deferred batch so all 4 rows arrive in order.
    bp.close().await;

    let rows = collect_rows(&mut rx, |rows| rows.len() >= want_order.len()).await;
    assert_eq!(
        super::support::event_types(&rows),
        want_order,
        "written event_type order"
    );
}

#[tokio::test(start_paused = true)]
async fn test_batch_processor_flush_run_end_bounded_re_deferral() {
    let server = FakeWriteServer::new();
    let (tx, mut rx) = requests();
    // Each batch write takes 250ms with a ShutdownTimeout of 200ms, so every write attempt
    // during flushRunEnd times out with flushCtx ended.
    arrival_recording_server(&server, Duration::from_millis(250), tx);
    let logger = Arc::new(RecordingLogger::default());
    let mut cfg = Config {
        logger: logger_of(&logger),
        ..config(2, Duration::from_millis(200))
    };
    cfg.retry_config.max_retries = 0;
    let bp = on_default_stream(&server, cfg).await;
    bp.start();

    // Queue 4 rows: batch 1 = {row-0, row-1}, batch 2 = {row-2, row-3}.
    for ev in ["row-0", "row-1", "row-2", "row-3"] {
        push_queue(&bp, event_row(ev));
    }

    // First flushRunEnd: batch 1 starts writing, times out at 200ms. Because !retry
    // (deferredBatch was empty), it is deferred to deferredBatch.
    bp.flush_run_end().await;
    assert_eq!(
        deferred_len(&bp).await,
        2,
        "deferredBatch length after first flushRunEnd"
    );
    assert_eq!(
        bp.drop_stats().append_failure_drops,
        0,
        "AppendFailureDrops after batch 1 was deferred (a deferred batch is not dropped)"
    );

    // Second flushRunEnd: batch 1 is popped from deferredBatch (retry == true). The write
    // times out again at 200ms. Since retry is true, batch 1 is dropped rather than
    // re-deferred indefinitely, leaving deferredBatch empty.
    bp.flush_run_end().await;
    assert_eq!(
        deferred_len(&bp).await,
        0,
        "deferredBatch length after second flushRunEnd (bounded single retry)"
    );
    assert!(
        logger.contains("Failed to write batch to BigQuery"),
        "expected drop error logged after retry failure, got:\n{logger}"
    );
    assert_eq!(
        bp.drop_stats().append_failure_drops,
        2,
        "AppendFailureDrops after batch 1 was dropped (its 2 rows, counted once)"
    );

    // Third and fourth flushRunEnd: batch 2 is now reached and attempted (not starved by
    // indefinite re-deferral of batch 1). It is attempted once and retried once before being
    // dropped.
    bp.flush_run_end().await;
    bp.flush_run_end().await;
    assert_eq!(
        bp.drop_stats().append_failure_drops,
        4,
        "AppendFailureDrops after both batches were dropped (2 rows per batch, each batch counted once)"
    );

    // Drain all requests received by the arrival-recording server.
    let mut received_batches = Vec::new();
    while let Ok(req) = rx.try_recv() {
        received_batches.push(super::support::event_types(&decode_arrow_rows(&req)));
    }
    // At most two sends per batch, and batch 2 was written.
    assert_eq!(
        received_batches,
        vec![
            vec!["row-0", "row-1"],
            vec!["row-0", "row-1"],
            vec!["row-2", "row-3"],
            vec!["row-2", "row-3"],
        ],
        "received batches"
    );
    bp.close().await;
}

#[tokio::test(start_paused = true)]
#[ignore = "needs Task 10"]
async fn test_batch_processor_background_flush_retries_deferred_batch() {
    let server = FakeWriteServer::new();
    let (tx, mut rx) = requests();
    // serverDelay stays 0 for the warm-up write below.
    let server_delay = Arc::new(AtomicI64::new(0));
    dynamic_delay_server(&server, Arc::clone(&server_delay), None, Some(tx));

    let stalled = Arc::new(AtomicBool::new(true));
    let hook_stalled = Arc::clone(&stalled);
    let off = hook(move |ctx, _| {
        let stalled = hook_stalled.load(Ordering::SeqCst);
        Box::pin(async move {
            if stalled {
                return until_done(&ctx).await;
            }
            Ok(())
        })
    });
    let logger = Arc::new(RecordingLogger::default());
    let mut cfg = Config {
        gcs_offloader: offloader(&off),
        gcs_max_pending_upload_bytes: 64,
        logger: logger_of(&logger),
        ..config(1, Duration::from_millis(2400))
    };
    cfg.retry_config.max_retries = 0;
    let bp = on_default_stream(&server, cfg).await;
    bp.start();

    // Open the connection and the write stream first, so that setting them up does not use
    // up the run-end budget below.
    let warm_ctx = Context::background().with_timeout(Duration::from_secs(5));
    bp.write_batch(&warm_ctx, vec![event_row(EVENT_USER_MESSAGE_RECEIVED)])
        .await
        .unwrap_or_else(|e| panic!("warm-up writeBatch: {e}"));
    rx.recv().await.unwrap();
    server_delay.store(nanos(Duration::from_millis(1900)), Ordering::SeqCst);

    // Batch 0: no upload; takes 1.9s in AppendRows, leaving less than a quarter (600ms) of
    // the 2.4s ShutdownTimeout.
    push_queue(&bp, event_row(EVENT_USER_MESSAGE_RECEIVED));

    // Batch 1: a 64-byte upload; because the budget is already depleted,
    // pauseOnCallerDeadline is false. The upload stalls for half the remaining budget and is
    // deferred with 64 bytes retained in pendingUploadBytes. AppendRows takes 1.9s, exceeding
    // what is left, so flushRunEnd saves batch 1 in deferredBatch.
    push_queue(
        &bp,
        llm_row(&offload_opts(Some(&bp)), &[image_content(&[0; 64])]),
    );

    bp.flush_run_end().await;

    assert!(
        !upload_paused(&bp),
        "uploadCooldownUntil after flushRunEnd != 0, want 0 (the depleted deadline of batch 1 must not pause the uploads)"
    );
    {
        let state = bp.flush_mu.lock().await;
        assert_eq!(
            state.deferred_batch.len(),
            1,
            "deferredBatch length after flushRunEnd"
        );
        assert_eq!(
            state.deferred_batch[0].row.get("event_type"),
            Some(&RowValue::from(EVENT_LLM_REQUEST)),
            "deferredBatch[0] event_type"
        );
    }
    assert_eq!(
        pending_upload_bytes(&bp),
        64,
        "pendingUploadBytes after flushRunEnd"
    );
    // The failure of the deferred upload is logged once, as pending a retry, and not as final.
    let (mut retry_lines, mut plain_lines) = (0, 0);
    for line in upload_failure_lines(&logger) {
        if line.contains("pending retry on next flush") {
            retry_lines += 1;
        } else {
            plain_lines += 1;
        }
    }
    assert!(
        retry_lines == 1 && plain_lines == 0,
        "upload failure retry lines = {retry_lines}, plain lines = {plain_lines}, want 1 and 0:\n{logger}"
    );

    // Restore healthy storage and fast BigQuery writes.
    stalled.store(false, Ordering::SeqCst);
    server_delay.store(nanos(Duration::from_millis(30)), Ordering::SeqCst);

    // The background flush must write deferredBatch before draining the queue.
    bp.flush().await;

    assert_eq!(
        deferred_len(&bp).await,
        0,
        "deferredBatch length after flush"
    );
    assert_eq!(
        pending_upload_bytes(&bp),
        0,
        "pendingUploadBytes after flush"
    );

    let mut rows = collect_rows(&mut rx, |rows| rows.len() >= 2).await;
    // The fake server can still record batch 1's abandoned run-end write (at-least-once
    // delivery), producing an INLINE duplicate before the retried GCS_REFERENCE row. Drain
    // boundedly for a third row so a real regression fails on storage_mode below.
    if rows.len() == 2
        && part_is(&content_parts_of(&rows[1])[0], "storage_mode", "INLINE")
        && let Ok(Some(req)) = tokio::time::timeout(Duration::from_millis(300), rx.recv()).await
    {
        rows.extend(decode_arrow_rows(&req));
    }
    assert!(
        (2..=3).contains(&rows.len()),
        "persisted rows = {}, want 2 (or 3 with an at-least-once duplicate)",
        rows.len()
    );
    assert_eq!(
        rows[0].get("event_type"),
        Some(&RowValue::from(EVENT_USER_MESSAGE_RECEIVED)),
        "rows[0] event_type"
    );
    let last = &rows[rows.len() - 1];
    assert_eq!(
        last.get("event_type"),
        Some(&RowValue::from(EVENT_LLM_REQUEST)),
        "last row event_type"
    );
    assert!(
        part_is(&content_parts_of(last)[0], "storage_mode", "GCS_REFERENCE"),
        "last row part storage_mode = {:?}, want GCS_REFERENCE",
        content_parts_of(last)[0].get("storage_mode")
    );
    let ds = bp.drop_stats();
    assert_eq!(
        ds.total_drops, 0,
        "DropStats after both rows were written = {ds:?}, want no drops"
    );
    bp.close().await;
}

/// One case of Go's `TestBatchProcessor_FlushRunEndDeferredBatchRetriesCutShortUploads`.
struct CutShortCase {
    name: &'static str,
    /// Whether the first batch has an upload (which succeeds at once).
    first_batch_uploads: bool,
    /// How long the BigQuery write of the first batch takes.
    first_write_takes: Duration,
    /// Whether there is no first batch, so the second starts with all of the budget left.
    only_batch: bool,
    /// How long the first upload of the second batch holds the only slot before it succeeds;
    /// zero: it stalls until the deadline. The second upload stalls once it starts.
    slot_held_for: Duration,
    /// Whether the second batch pauses the uploads.
    want_paused: bool,
    /// Whether it keeps its second upload for the retry.
    want_kept: bool,
}

#[tokio::test(start_paused = true)]
#[ignore = "needs Task 10"]
async fn test_batch_processor_flush_run_end_deferred_batch_retries_cut_short_uploads() {
    let cases = [
        // The second batch starts with about 1990ms of the 2s budget left, so its uploads get
        // about 995ms. Its second upload starts after 400ms of them, too late to run for three
        // quarters of them.
        CutShortCase {
            name: "a later batch with uploads",
            first_batch_uploads: true,
            first_write_takes: Duration::ZERO,
            only_batch: false,
            slot_held_for: Duration::from_millis(400),
            want_paused: false,
            want_kept: true,
        },
        // The second batch starts with about 1400ms of the 2s budget left, so its uploads get
        // about 700ms. Its second upload starts after 280ms of them, leaving ~420ms against
        // the 525ms three-quarters stall threshold.
        CutShortCase {
            name: "the first batch with uploads",
            first_batch_uploads: false,
            first_write_takes: Duration::from_millis(600),
            only_batch: false,
            slot_held_for: Duration::from_millis(280),
            want_paused: false,
            want_kept: true,
        },
        // The batch is the only one, so it starts with all of the 2s budget left and its
        // uploads get about 1s. Its second upload starts after 400ms of them, too late to run
        // for three quarters of them.
        CutShortCase {
            name: "the only batch",
            first_batch_uploads: false,
            first_write_takes: Duration::ZERO,
            only_batch: true,
            slot_held_for: Duration::from_millis(400),
            want_paused: false,
            want_kept: true,
        },
        // The first upload of the second batch stalls for all of the batch's upload time,
        // holding the only slot, so that the second upload never starts.
        CutShortCase {
            name: "a batch that pauses the uploads",
            first_batch_uploads: true,
            first_write_takes: Duration::ZERO,
            only_batch: false,
            slot_held_for: Duration::ZERO,
            want_paused: true,
            want_kept: false,
        },
    ];
    for tc in cases {
        cut_short_case(&tc).await;
    }
}

async fn cut_short_case(tc: &CutShortCase) {
    let name = tc.name;
    let server = FakeWriteServer::new();
    let (tx, mut rx) = requests();
    // serverDelay stays 0 for the warm-up write below.
    let server_delay = Arc::new(AtomicI64::new(0));
    dynamic_delay_server(&server, Arc::clone(&server_delay), None, Some(tx));

    let stalled = Arc::new(AtomicBool::new(true));
    let (hook_stalled, hook_delay) = (Arc::clone(&stalled), Arc::clone(&server_delay));
    let slot_held_for = tc.slot_held_for;
    let off = hook(move |ctx, req| {
        let stalled = hook_stalled.load(Ordering::SeqCst);
        let delay = Arc::clone(&hook_delay);
        Box::pin(async move {
            if !stalled || req.path.contains("_ev-0_") {
                return Ok(());
            }
            // The uploads of the second batch have started: the BigQuery write that follows
            // them is to run out of the budget.
            delay.store(nanos(Duration::from_secs(60)), Ordering::SeqCst);
            if req.path.contains("_c0_") && slot_held_for > Duration::ZERO {
                return sleep_or_done(&ctx, slot_held_for).await;
            }
            until_done(&ctx).await
        })
    });
    let mut cfg = Config {
        gcs_offloader: offloader(&off),
        gcs_max_concurrent_uploads: 1,
        logger: logger_of(&Arc::new(RecordingLogger::default())),
        ..config(1, Duration::from_secs(2))
    };
    cfg.retry_config.max_retries = 0;
    let bp = on_default_stream(&server, cfg).await;

    // Open the connection and the write stream first, so that setting them up does not use
    // up the run-end budget below.
    let warm_ctx = Context::background().with_timeout(Duration::from_secs(5));
    bp.write_batch(&warm_ctx, vec![event_row(EVENT_USER_MESSAGE_RECEIVED)])
        .await
        .unwrap_or_else(|e| panic!("{name}: warm-up writeBatch: {e}"));
    rx.recv().await.unwrap();
    server_delay.store(nanos(tc.first_write_takes), Ordering::SeqCst);

    // The rows are queued directly, so that no background flush takes flushMu before
    // flushRunEnd does.
    let opts = offload_opts(Some(&bp));
    if !tc.only_batch {
        let mut first = event_row(EVENT_USER_MESSAGE_RECEIVED);
        if tc.first_batch_uploads {
            first = llm_row(&opts, &[image_content(&[1])]);
        }
        set(&mut first, "event_id", "ev-0");
        push_queue(&bp, first);
    }
    // The second batch has a 3-byte and a 5-byte upload.
    let mut second = llm_row(
        &opts,
        &[image_content(&[1, 2, 3]), image_content(&[4, 5, 6, 7, 8])],
    );
    set(&mut second, "event_id", "ev-1");
    push_queue(&bp, second);

    bp.flush_run_end().await;

    {
        let state = bp.flush_mu.lock().await;
        assert_eq!(
            state.deferred_batch.len(),
            1,
            "{name}: deferredBatch length after flushRunEnd (the batch with the two uploads)"
        );
        assert_eq!(
            state.deferred_batch[0].row.get("event_id"),
            Some(&RowValue::from("ev-1")),
            "{name}: deferredBatch[0] event_id"
        );
    }
    assert_eq!(
        upload_paused(&bp),
        tc.want_paused,
        "{name}: uploads paused after flushRunEnd"
    );
    let want_reserved = if tc.want_kept { 5 } else { 0 };
    assert_eq!(
        pending_upload_bytes(&bp),
        want_reserved,
        "{name}: pendingUploadBytes held for the deferred batch"
    );

    // Cloud Storage and BigQuery are back, and any pause is over, so that only what the
    // second batch kept decides whether the next flush uploads its parts.
    stalled.store(false, Ordering::SeqCst);
    server_delay.store(nanos(Duration::from_millis(30)), Ordering::SeqCst);
    *bp.upload_cooldown_until.lock().unwrap() = None;
    let calls_before = off.uploads().len();
    bp.flush().await;

    assert_eq!(
        deferred_len(&bp).await,
        0,
        "{name}: deferredBatch length after flush"
    );
    assert_eq!(
        pending_upload_bytes(&bp),
        0,
        "{name}: pendingUploadBytes after flush"
    );
    let want_calls = usize::from(tc.want_kept);
    assert_eq!(
        off.uploads().len() - calls_before,
        want_calls,
        "{name}: the flush's offloader calls"
    );
    let last_second_idx = |rows: &[Row]| {
        rows.iter()
            .rposition(|r| r.get("event_id") == Some(&RowValue::from("ev-1")))
    };
    let mut rows = collect_rows(&mut rx, |rows| last_second_idx(rows).is_some()).await;
    // The fake server can record the abandoned run-end write of ev-1 (with part 1 INLINE)
    // before the retried flush write. Drain boundedly and inspect the last ev-1 row.
    if tc.want_kept {
        let idx = last_second_idx(&rows).unwrap();
        if part_is(&content_parts_of(&rows[idx])[1], "storage_mode", "INLINE")
            && let Ok(Some(req)) = tokio::time::timeout(Duration::from_millis(300), rx.recv()).await
        {
            rows.extend(decode_arrow_rows(&req));
        }
    }
    let parts = content_parts_of(&rows[last_second_idx(&rows).unwrap()]);
    assert_eq!(parts.len(), 2, "{name}: the second batch's content parts");
    for (i, uploaded) in [!tc.want_paused, tc.want_kept].into_iter().enumerate() {
        let (want_mode, want_text) = if uploaded {
            ("GCS_REFERENCE", MEDIA_OFFLOADED_TEXT)
        } else {
            ("INLINE", UPLOAD_FAILED_TEXT)
        };
        assert!(
            part_is(&parts[i], "storage_mode", want_mode) && part_is(&parts[i], "text", want_text),
            "{name}: part {i} of the second batch has storage_mode {:?} and text {:?}, want {want_mode} and {want_text:?}",
            parts[i].get("storage_mode"),
            parts[i].get("text")
        );
    }
    let ds = bp.drop_stats();
    assert_eq!(
        ds.total_drops, 0,
        "{name}: DropStats after both batches were written = {ds:?}, want no drops"
    );
    bp.close().await;
}

// parity: adapted: the queued row is moved into the queue, so Go's check that the written row no longer holds deferred uploads is left out (pendingUploadBytes and the log cover their release)
#[tokio::test(start_paused = true)]
#[ignore = "needs Task 10"]
async fn test_batch_processor_flush_run_end_succeeded_write_releases_cut_short_upload_bytes() {
    let server = FakeWriteServer::new();
    let (tx, mut rx) = requests();
    recording_server(&server, tx);
    let off = hook(|ctx, req| {
        Box::pin(async move {
            if req.path.contains("_c0_") {
                return sleep_or_done(&ctx, Duration::from_millis(300)).await;
            }
            until_done(&ctx).await
        })
    });
    let logger = Arc::new(RecordingLogger::default());
    let mut cfg = Config {
        gcs_offloader: offloader(&off),
        gcs_max_concurrent_uploads: 1,
        logger: logger_of(&logger),
        ..config(1, Duration::from_millis(800))
    };
    cfg.retry_config.max_retries = 0;
    let bp = on_default_stream(&server, cfg).await;

    // Open the connection and the write stream first, so that setting them up does not use
    // up the run-end budget below.
    let warm_ctx = Context::background().with_timeout(Duration::from_secs(5));
    bp.write_batch(&warm_ctx, vec![event_row(EVENT_USER_MESSAGE_RECEIVED)])
        .await
        .unwrap_or_else(|e| panic!("warm-up writeBatch: {e}"));
    rx.recv().await.unwrap();

    // The batch has a 3-byte and a 5-byte upload.
    let mut row = llm_row(
        &offload_opts(Some(&bp)),
        &[image_content(&[1, 2, 3]), image_content(&[4, 5, 6, 7, 8])],
    );
    set(&mut row, "event_id", "ev-0");
    push_queue(&bp, row);

    bp.flush_run_end().await;

    assert!(
        !upload_paused(&bp),
        "uploadCooldownUntil after flushRunEnd != 0, want 0: an upload cut short after waiting for the slot does not pause the uploads"
    );
    assert_eq!(
        deferred_len(&bp).await,
        0,
        "deferredBatch length after flushRunEnd: the batch was written"
    );
    assert_eq!(
        pending_upload_bytes(&bp),
        0,
        "pendingUploadBytes after flushRunEnd"
    );
    // The batch was written, so clearDeferredUploads logs the plain fallback line and must
    // not claim that a retry is pending on the next flush.
    let want_log = "GCS upload failed for event ev-0 part 0 (5 bytes): context deadline exceeded; keeping the part inline";
    assert!(
        logger.contains(want_log),
        "no log line says {want_log:?}:\n{logger}"
    );
    assert!(
        !logger.contains("pending retry"),
        "a log line says that a retry is pending, though the batch was written:\n{logger}"
    );
    let rows = collect_rows(&mut rx, |rows| !rows.is_empty()).await;
    let parts = content_parts_of(&rows[0]);
    assert_eq!(parts.len(), 2, "the written row's content parts");
    for (i, (mode, text)) in [
        ("GCS_REFERENCE", MEDIA_OFFLOADED_TEXT),
        ("INLINE", UPLOAD_FAILED_TEXT),
    ]
    .into_iter()
    .enumerate()
    {
        assert!(
            part_is(&parts[i], "storage_mode", mode) && part_is(&parts[i], "text", text),
            "part {i} of the written row has storage_mode {:?} and text {:?}, want {mode} and {text:?}",
            parts[i].get("storage_mode"),
            parts[i].get("text")
        );
    }
    bp.close().await;
}
