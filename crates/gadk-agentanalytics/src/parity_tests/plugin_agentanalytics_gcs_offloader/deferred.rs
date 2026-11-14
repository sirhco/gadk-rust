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

//! `gcs_offloader_test.go`: deferred uploads (kept reservations, requeue / clear / drop and
//! their logs), Close's upload failures, and panicking loggers on every settling path.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, Ordering};
use std::time::Duration;

use pretty_assertions::assert_eq;

use super::super::plugin_agentanalytics_batch_processor::support::{
    Delayed, HookOffloader, Requests, ScriptedServer, collect_rows, delayed_server, event_row,
    exactly_once_test_config, image_content, pending_upload_bytes, processor, push_queue,
    queued_row, recording_server, requests, status, upload_paused,
};
use super::deadlines::warm_up;
use super::{
    AlwaysPanicLogger, ReproPanicLogger, SwitchLogger, as_log, as_off, bare, defer_uploads, field,
    hooked, kept, panic_value, panic_value_sync, parts, reserving, resolve, stalling_offloader,
    tagged, upload_failure_lines, upload_rows, wait_done, with_event_id,
};
use crate::batch_processor::BatchProcessor;
use crate::bigquery::fake::RecordingLogger;
use crate::config::{Config, Logger};
use crate::context::Context;
use crate::error::{Code, Error};
use crate::formatter::{INLINE_DATA_TEXT, UPLOAD_FAILED_TEXT};
use crate::row::{Row, RowExt};
use crate::storagepb::AppendRowsResponse;
use crate::testutil::fake_write_server::{AppendRowsServerStream, FakeWriteServer};
use crate::write::stream_writer::tests::in_band;

const PDT_DEFAULT_STREAM: &str = "projects/p/datasets/d/tables/t/_default";

/// `n` milliseconds.
const fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// Go `applyDefaults(Config{GCSOffloader, GCSUploadTimeout: time.Minute,
/// GCSMaxPendingUploadBytes: cap, Logger})` in a processor with `maxPendingUploadBytes: cap`.
fn capped(
    off: &Arc<HookOffloader>,
    cap: i64,
    logger: Option<Arc<dyn Logger>>,
    max_concurrent: i64,
) -> Arc<BatchProcessor> {
    let bp = bare(
        Config {
            gcs_offloader: as_off(off),
            gcs_upload_timeout: Duration::from_secs(60),
            gcs_max_pending_upload_bytes: cap,
            gcs_max_concurrent_uploads: max_concurrent,
            logger,
            ..Config::zero()
        }
        .apply_defaults(),
    );
    bp.set_max_pending_upload_bytes(cap);
    bp
}

/// Go `&BatchProcessor{config: Config{GCSOffloader, GCSUploadTimeout: time.Minute, Logger}}`.
fn minute(off: &Arc<HookOffloader>, logger: Option<Arc<dyn Logger>>) -> Arc<BatchProcessor> {
    bare(Config {
        gcs_offloader: as_off(off),
        gcs_upload_timeout: Duration::from_secs(60),
        logger,
        ..Config::zero()
    })
}

/// Go `scriptedWriteServer{script, respond, requests}`: each AppendRows request, numbered from
/// 1, waits as `script(n)` says (unless the stream ends first) or ends the stream with its
/// error, is recorded, and is answered by `respond(n)` (default an empty response).
#[allow(clippy::type_complexity)]
fn scripted_write_server(
    server: &FakeWriteServer,
    script: Arc<dyn Fn(i32) -> Result<Duration, tonic::Status> + Send + Sync>,
    respond: Option<Arc<dyn Fn(i32) -> Option<AppendRowsResponse> + Send + Sync>>,
    requests: Requests,
) {
    let count = Arc::new(AtomicI32::new(0));
    server.on_append_rows(Arc::new(move |mut stream: AppendRowsServerStream| {
        let (script, respond, requests, count) = (
            Arc::clone(&script),
            respond.clone(),
            requests.clone(),
            Arc::clone(&count),
        );
        Box::pin(async move {
            while let Some(req) = stream.recv().await? {
                let n = count.fetch_add(1, Ordering::SeqCst) + 1;
                let delay = script(n)?;
                tokio::select! {
                    () = tokio::time::sleep(delay) => {}
                    () = stream.cancelled() => return Err(tonic::Status::cancelled("context canceled")),
                }
                let _ = requests.send(req);
                let resp = respond
                    .as_ref()
                    .and_then(|r| r(n))
                    .unwrap_or_default();
                stream.send(resp).await?;
            }
            Ok(())
        })
    }));
}

/// The `content_parts` entry 0 of a written row.
fn part0(row: &Row) -> Row {
    row.parts("content_parts").unwrap()[0].clone()
}

/// Counts the lines of `logger` about `event` that report a failed upload: (final, retry).
fn failure_lines_of(logger: &RecordingLogger, event: &str) -> (usize, usize) {
    let (mut plain, mut retry) = (0, 0);
    for line in logger.lines() {
        if !line.contains(&format!("event {event}")) {
            continue;
        }
        if line.contains("pending retry") {
            retry += 1;
        } else if line.contains("GCS upload failed") {
            plain += 1;
        }
    }
    (plain, retry)
}

#[tokio::test(start_paused = true)]
async fn test_gcs_offloading_deferred_uploads_hold_pending_byte_reservation() {
    let stalled = Arc::new(AtomicBool::new(true));
    let off = stalling_offloader(&stalled);
    let logger = Arc::new(RecordingLogger::default());
    let bp = capped(&off, 64, as_log(&logger), 0);
    let o = reserving(&bp, 10);
    let mut row1 = with_event_id(queued_row(&o, &[image_content(&[0; 64])]), "ev-requeue");
    assert_eq!(
        pending_upload_bytes(&bp),
        64,
        "pendingUploadBytes after row1 reservation"
    );

    // A depleted caller deadline expires during resolveUploads, deferring row1's upload.
    let expired = Context::background().with_timeout(ms(25));
    resolve(&bp, &expired, false, std::slice::from_mut(&mut row1)).await;
    expired.cancel();

    // While row1's upload is deferred, its reservation must remain held so a concurrent caller
    // cannot reserve more than the cap.
    assert_eq!(
        pending_upload_bytes(&bp),
        64,
        "pendingUploadBytes while upload is deferred"
    );
    assert!(
        !bp.try_reserve_upload_bytes(1),
        "tryReserveUploadBytes(1) succeeded while 64/64 bytes were held by deferred upload"
    );
    // Nothing has been requeued or dropped yet, so the failure is not logged.
    assert!(
        !logger.contains("GCS upload failed for event ev-requeue"),
        "failure logged before requeueDeferredUploads:\n{logger}"
    );

    // Requeuing the deferred upload must keep the reservation without double-counting it.
    bp.requeue_deferred_uploads(std::slice::from_mut(&mut row1));
    assert_eq!(
        failure_lines_of(&logger, "ev-requeue"),
        (0, 1),
        "requeueDeferredUploads (plain, retry) lines:\n{logger}"
    );
    assert_eq!(
        pending_upload_bytes(&bp),
        64,
        "pendingUploadBytes after requeueDeferredUploads"
    );

    // When the requeued batch is resolved on the next flush, the reservation is released.
    stalled.store(false, Ordering::SeqCst);
    resolve(
        &bp,
        &Context::background(),
        false,
        std::slice::from_mut(&mut row1),
    )
    .await;
    assert_eq!(
        pending_upload_bytes(&bp),
        0,
        "pendingUploadBytes after retry resolveUploads"
    );

    // clearDeferredUploads also releases deferred bytes when a batch's write succeeds and the
    // deferred uploads are discarded.
    let mut row2 = with_event_id(queued_row(&o, &[image_content(&[0; 64])]), "ev-discard");
    assert_eq!(
        pending_upload_bytes(&bp),
        64,
        "pendingUploadBytes after row2 reservation"
    );
    stalled.store(true, Ordering::SeqCst);
    let expired2 = Context::background().with_timeout(ms(25));
    resolve(&bp, &expired2, false, std::slice::from_mut(&mut row2)).await;
    expired2.cancel();
    assert_eq!(
        pending_upload_bytes(&bp),
        64,
        "pendingUploadBytes while row2 upload is deferred"
    );
    assert!(
        !logger.contains("GCS upload failed for event ev-discard"),
        "discarded upload logged before clearDeferredUploads:\n{logger}"
    );
    bp.clear_deferred_uploads(std::slice::from_mut(&mut row2));
    assert_eq!(
        pending_upload_bytes(&bp),
        0,
        "pendingUploadBytes after clearDeferredUploads"
    );
    assert_eq!(
        failure_lines_of(&logger, "ev-discard"),
        (1, 0),
        "clearDeferredUploads (plain, retry) lines:\n{logger}"
    );
}

#[tokio::test(start_paused = true)]
async fn test_gcs_offloading_close_logs_upload_failures_under_caller_deadline() {
    let server = FakeWriteServer::new();
    let (tx, mut rx) = requests();
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
    cfg.shutdown_timeout = ms(400);
    cfg.retry_config.max_retries = 0;
    cfg.gcs_offloader = as_off(&off);
    cfg.logger = as_log(&logger);
    let ctx = Context::background().with_cancel();
    let bp = processor(
        &ctx,
        Some(server.client().unwrap()),
        PDT_DEFAULT_STREAM,
        cfg,
    )
    .await;
    bp.start();

    let row = tagged(
        queued_row(&reserving(&bp, 10), &[image_content(&[1, 2, 3])]),
        "LLM_REQUEST",
        "ev-close-fail",
    );
    push_queue(&bp, row);

    bp.close().await;

    let rows = collect_rows(&mut rx, |rows| !rows.is_empty()).await;
    assert_eq!(rows.len(), 1, "written rows");
    assert_eq!(
        field(&part0(&rows[0]), "text"),
        UPLOAD_FAILED_TEXT,
        "part text"
    );
    assert_eq!(
        failure_lines_of(&logger, "ev-close-fail"),
        (1, 0),
        "Close (plain, retry) lines:\n{logger}"
    );
    ctx.cancel();
}

// TestGCSOffloading_CloseEarlyDropLoggerPanicReleasesUploadBytes checks that Close's early-drop
// branch releases the pending upload reservations before logging the warning, so that a Logger
// that panics on the warning cannot leak them.
// parity: adapted: gadk's config is fixed at construction, so the logger that starts to panic before Close is a switched logger
#[tokio::test(start_paused = true)]
async fn test_gcs_offloading_close_early_drop_logger_panic_releases_upload_bytes() {
    let off = Arc::new(HookOffloader::default());
    let logger = SwitchLogger::<AlwaysPanicLogger>::new(false);
    let mut cfg = exactly_once_test_config();
    cfg.batch_flush_intv = Duration::from_secs(3600);
    cfg.batch_size = 100;
    cfg.gcs_offloader = as_off(&off);
    cfg.logger = as_log(&logger);
    cfg.shutdown_timeout = Duration::from_secs(5);
    let srv = ScriptedServer::new(
        Vec::new(),
        |_, _| Err(status(Code::OutOfRange, "offset beyond end")),
        None,
    );
    let bp = processor(
        &Context::background(),
        Some(srv.server.client().unwrap()),
        "",
        cfg,
    )
    .await;

    // Establish rotation cooldown before Close so the 3 upload rows form a batch that hits the
    // cooldown early-drop branch immediately.
    assert!(
        bp.write_batch(&Context::background(), vec![event_row("B1")])
            .await
            .is_err(),
        "writeBatch(B1) error = nil, want an offset error"
    );
    assert!(
        bp.stream_writer.is_rotation_in_cooldown().await,
        "rotation is not paused after the freshly rotated stream rejected the batch"
    );

    // Switch to the panicking Logger for Close.
    logger.panicking.store(true, Ordering::SeqCst);
    for row in upload_rows(&bp, 3) {
        push_queue(&bp, row);
    }
    assert_eq!(
        pending_upload_bytes(&bp),
        48,
        "pendingUploadBytes before Close"
    );

    // Close's early-drop branch logs a warning that the panicking Logger turns into a panic;
    // the reservations must already have been released.
    let r = panic_value(bp.close())
        .await
        .expect("Close did not panic on the logger's warning");
    assert!(
        r.contains("committed write stream rotation in cooldown"),
        "panic = {r:?}, want it to contain the cooldown warning"
    );
    assert_eq!(
        pending_upload_bytes(&bp),
        0,
        "pendingUploadBytes after Close panicked on the early-drop warning"
    );
}

#[tokio::test(start_paused = true)]
async fn test_gcs_offloading_close_logs_final_failure_of_background_flush_upload_it_cancels() {
    let server = FakeWriteServer::new();
    let (tx, mut rx) = requests();
    recording_server(&server, tx);
    let started = Arc::new(tokio::sync::Notify::new());
    let off = {
        let started = Arc::clone(&started);
        hooked(move |ctx, _| {
            started.notify_one();
            Box::pin(wait_done(ctx))
        })
    };
    let logger = Arc::new(RecordingLogger::default());
    let mut cfg = Config::default();
    cfg.batch_size = 1;
    cfg.batch_flush_intv = Duration::from_secs(3600);
    cfg.shutdown_timeout = Duration::from_secs(2);
    cfg.gcs_upload_timeout = Duration::from_secs(5);
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

    let row = tagged(
        queued_row(&reserving(&bp, 10), &[image_content(&[1, 2, 3])]),
        "LLM_REQUEST",
        "ev-bg",
    );
    bp.append(row); // reaches BatchSize, so a background flush starts uploading
    tokio::time::timeout(Duration::from_secs(2), started.notified())
        .await
        .expect("upload did not start");
    bp.close().await; // cancels the in-flight upload of the background flush

    let rows = collect_rows(&mut rx, |rows| !rows.is_empty()).await;
    assert_eq!(
        field(&part0(&rows[0]), "text"),
        UPLOAD_FAILED_TEXT,
        "part text"
    );
    let lines: Vec<String> = upload_failure_lines(&logger)
        .into_iter()
        .filter(|l| l.contains("ev-bg"))
        .collect();
    assert_eq!(
        lines,
        [
            "agentanalytics: GCS upload failed for event ev-bg part 0 (3 bytes): context canceled; keeping the part inline"
        ]
    );
    assert_eq!(pending_upload_bytes(&bp), 0, "pendingUploadBytes");
}

#[tokio::test(start_paused = true)]
async fn test_gcs_offloading_later_run_end_batch_logs_upload_failures() {
    let logger = Arc::new(RecordingLogger::default());
    // Whether the failure of row 2's upload was already logged when row 2's write reached
    // BigQuery, which is after resolve_uploads_for_batch returned and before the write returns
    // and clear_deferred_uploads settles the deferred upload.
    let logged_during_resolve = Arc::new(AtomicBool::new(false));
    let server = FakeWriteServer::new();
    let (tx, mut rx) = requests();
    {
        let (logger, logged) = (Arc::clone(&logger), Arc::clone(&logged_during_resolve));
        scripted_write_server(
            &server,
            Arc::new(move |n| {
                Ok(match n {
                    1 => Duration::ZERO,
                    // Row 1's 1.6s write leaves at most 400ms, less than a quarter of the 2s
                    // ShutdownTimeout, so row 2's run-end deadline does not pause the uploads
                    // and its stalled upload is deferred, then logged as a final failure once
                    // row 2's write succeeds.
                    2 => ms(1600),
                    3 => {
                        logged.store(logger.contains("ev-hang"), Ordering::SeqCst);
                        ms(50)
                    }
                    _ => ms(50),
                })
            }),
            None,
            tx,
        );
    }
    let upload_count = Arc::new(AtomicI32::new(0));
    let off = {
        let upload_count = Arc::clone(&upload_count);
        hooked(move |ctx, _| {
            let first = upload_count.fetch_add(1, Ordering::SeqCst) == 0;
            Box::pin(async move {
                if first {
                    return Ok(());
                }
                wait_done(ctx).await
            })
        })
    };
    let mut cfg = Config::default();
    cfg.batch_size = 1;
    cfg.batch_flush_intv = Duration::from_secs(3600);
    cfg.shutdown_timeout = Duration::from_secs(2);
    cfg.retry_config.max_retries = 0;
    cfg.gcs_offloader = as_off(&off);
    cfg.logger = as_log(&logger);
    let ctx = Context::background().with_cancel();
    let bp = processor(
        &ctx,
        Some(server.client().unwrap()),
        PDT_DEFAULT_STREAM,
        cfg,
    )
    .await;

    // Warm up the write stream before starting the run-end budget.
    warm_up(&bp, &mut rx).await;
    bp.start();

    let o = reserving(&bp, 10);
    push_queue(
        &bp,
        tagged(
            queued_row(&o, &[image_content(&[1, 2, 3])]),
            "LLM_REQUEST",
            "ev-success",
        ),
    );
    push_queue(
        &bp,
        tagged(
            queued_row(&o, &[image_content(&[4, 5, 6])]),
            "LLM_REQUEST",
            "ev-hang",
        ),
    );

    bp.flush_run_end().await;

    let rows = collect_rows(&mut rx, |rows| rows.len() >= 2).await;
    assert_eq!(rows.len(), 2, "written rows");
    assert_eq!(
        field(&part0(&rows[0]), "storage_mode"),
        "GCS_REFERENCE",
        "row 1 storage_mode"
    );
    assert_eq!(
        field(&part0(&rows[1]), "text"),
        UPLOAD_FAILED_TEXT,
        "row 2 text"
    );
    assert!(
        logger.contains("GCS upload failed for event ev-hang"),
        "expected GCS upload failure logged for second batch, got:\n{logger}"
    );
    assert!(
        !logged_during_resolve.load(Ordering::SeqCst),
        "the failure of row 2's upload was logged before row 2's write, want it deferred"
    );
    assert!(
        !upload_paused(&bp),
        "row 2's run-end deadline must not pause the uploads"
    );
    assert_eq!(
        bp.flush_mu.lock().await.deferred_batch.len(),
        0,
        "deferredBatch length: row 2 was written"
    );
    // The second batch was written, so its upload is not retried.
    assert!(
        !logger.contains("pending retry on next flush"),
        "flushRunEnd logged a retry of an upload of a written batch, got:\n{logger}"
    );
    bp.close().await;
    ctx.cancel();
}

#[tokio::test(start_paused = true)]
async fn test_resolve_uploads_panicking_logger_releases_deferred_reservation() {
    let new_processor =
        |off: &Arc<HookOffloader>, cap: i64| capped(off, cap, Some(Arc::new(ReproPanicLogger)), 2);

    // an in-flight log panic after deferring an earlier part: the first part waits out the
    // caller's context and is deferred; the second returns a non-gs URI, which apply_upload
    // logs, and that log panics after the deferred reservation is recorded.
    {
        let second_started = Arc::new(tokio::sync::Notify::new());
        let off = Arc::new(HookOffloader::new(
            Some({
                let second_started = Arc::clone(&second_started);
                super::hook(move |ctx, req| {
                    let second = req.path.contains("_c1_p0");
                    if second {
                        second_started.notify_one();
                    }
                    Box::pin(async move {
                        if second {
                            return Ok(());
                        }
                        wait_done(ctx).await
                    })
                })
            }),
            Some(Arc::new(|req: &crate::config::GcsUploadRequest| {
                if req.path.contains("_c1_p0") {
                    "https://example.com/not-gs".to_owned()
                } else {
                    format!("gs://fake-bucket/{}", req.path)
                }
            })),
        ));
        let bp = new_processor(&off, 128);
        let o = reserving(&bp, 10);
        let mut row = queued_row(&o, &[image_content(&[1; 64]), image_content(&[2; 64])]);
        assert_eq!(
            pending_upload_bytes(&bp),
            128,
            "pendingUploadBytes after reservation"
        );
        let ctx = Context::background().with_cancel();
        let canceler = {
            let (ctx, second_started) = (ctx.clone(), Arc::clone(&second_started));
            tokio::spawn(async move {
                let _ =
                    tokio::time::timeout(Duration::from_secs(2), second_started.notified()).await;
                ctx.cancel();
            })
        };
        let r = panic_value(resolve(&bp, &ctx, false, std::slice::from_mut(&mut row))).await;
        assert!(r.is_some(), "resolveUploads did not panic");
        canceler.await.unwrap();
        // The panic leaves flushRunEnd before the batch is requeued, so the deferred row is
        // gone too. Unwinding must release those bytes as well.
        assert_eq!(pending_upload_bytes(&bp), 0, "pendingUploadBytes");
        assert!(
            row.deferred.is_none(),
            "row still holds deferred uploads after resolveUploads panicked"
        );
    }

    // a deferred upload that is dropped
    {
        let stalled = Arc::new(AtomicBool::new(true));
        let bp = new_processor(&stalling_offloader(&stalled), 64);
        let o = reserving(&bp, 10);
        let mut row = vec![queued_row(&o, &[image_content(&[0; 64])])];
        // The deferred upload keeps its reservation, and its failure is not logged, until it is
        // dropped or requeued.
        defer_uploads(&bp, &mut row).await;
        assert_eq!(
            pending_upload_bytes(&bp),
            64,
            "pendingUploadBytes while the upload is deferred"
        );
        assert!(
            panic_value_sync(|| bp.clear_deferred_uploads(&mut row)).is_some(),
            "clearDeferredUploads did not log the failure of the upload that it dropped"
        );
        assert_eq!(pending_upload_bytes(&bp), 0, "pendingUploadBytes");
    }

    // a failed upload that is logged at once
    {
        let off = hooked(|_, _| Box::pin(async { Err(Error::msg("upload failed")) }));
        let bp = new_processor(&off, 64);
        let o = reserving(&bp, 10);
        let mut row = queued_row(&o, &[image_content(&[0; 64])]);
        assert!(
            panic_value(resolve(
                &bp,
                &Context::background(),
                false,
                std::slice::from_mut(&mut row)
            ))
            .await
            .is_some(),
            "resolveUploads did not log the failed upload"
        );
        assert_eq!(pending_upload_bytes(&bp), 0, "pendingUploadBytes");
    }
}

// parity: adapted: gadk's config is fixed at construction, so the logger that stops panicking before the retry is a switched logger
#[tokio::test(start_paused = true)]
async fn test_requeue_deferred_uploads_panicking_logger_keeps_batch() {
    let stalled = Arc::new(AtomicBool::new(true));
    let logger = SwitchLogger::<ReproPanicLogger>::new(true);
    let bp = capped(&stalling_offloader(&stalled), 64, as_log(&logger), 0);
    let o = reserving(&bp, 10);
    let mut row = with_event_id(
        queued_row(&o, &[image_content(&[0; 64])]),
        "ev-requeue-panic",
    );
    let expired = Context::background().with_timeout(ms(25));
    resolve(&bp, &expired, false, std::slice::from_mut(&mut row)).await;
    assert_eq!(
        pending_upload_bytes(&bp),
        64,
        "pendingUploadBytes while upload is deferred"
    );
    {
        let mut state = bp.flush_mu.lock().await;
        assert!(
            panic_value(bp.write_run_end_batch_locked(&mut state, &expired, false, vec![row]))
                .await
                .is_some(),
            "writeRunEndBatchLocked did not panic on requeue log"
        );
        assert_eq!(state.deferred_batch.len(), 1, "deferredBatch len");
    }
    expired.cancel();
    assert_eq!(
        pending_upload_bytes(&bp),
        64,
        "pendingUploadBytes after logger panic"
    );
    let mut saved = std::mem::take(&mut bp.flush_mu.lock().await.deferred_batch);
    stalled.store(false, Ordering::SeqCst);
    logger.panicking.store(false, Ordering::SeqCst);
    resolve(&bp, &Context::background(), false, &mut saved).await;
    assert_eq!(
        pending_upload_bytes(&bp),
        0,
        "pendingUploadBytes after resolving the saved batch"
    );
}

// resolveUploads consumed the queued payloads (releasing their reservations) but each row's
// deferred upload still holds its 64-byte reservation. releaseRowUploads drops a row via
// releaseBatchUploads, which releases the deferred reservation before logging. A panic on the
// first row's line must not keep the second row's bytes when it is released separately.
#[tokio::test(start_paused = true)]
async fn test_release_row_uploads_panicking_logger_releases_later_rows() {
    let stalled = Arc::new(AtomicBool::new(true));
    let bp = capped(
        &stalling_offloader(&stalled),
        128,
        Some(Arc::new(ReproPanicLogger)),
        0,
    );
    let o = reserving(&bp, 10);
    let mut batch = vec![
        with_event_id(
            queued_row(&o, &[image_content(&[0; 64])]),
            "ev-write-fail-1",
        ),
        with_event_id(
            queued_row(&o, &[image_content(&[0; 64])]),
            "ev-write-fail-2",
        ),
    ];
    let expired = Context::background().with_timeout(ms(25));
    resolve(&bp, &expired, false, &mut batch).await;
    expired.cancel();
    assert_eq!(
        pending_upload_bytes(&bp),
        128,
        "pendingUploadBytes while uploads are deferred"
    );
    let (first, second) = batch.split_at_mut(1);
    assert!(
        panic_value_sync(|| {
            bp.release_row_uploads(&mut first[0]);
            bp.config.logf(format_args!(
                "ERROR: Failed to write batch to BigQuery: {}",
                Error::msg("write failed")
            ));
        })
        .is_some(),
        "logger did not panic"
    );
    assert_eq!(
        pending_upload_bytes(&bp),
        64,
        "pendingUploadBytes after releasing row1"
    );
    assert!(
        panic_value_sync(|| bp.release_row_uploads(&mut second[0])).is_some(),
        "logger did not panic on row2"
    );
    assert_eq!(
        pending_upload_bytes(&bp),
        0,
        "pendingUploadBytes after releasing row2"
    );
}

/// Go `callbackRaceLogger`: counts the lines.
#[derive(Default)]
struct CallbackRaceLogger(std::sync::atomic::AtomicI32);

impl Logger for CallbackRaceLogger {
    fn printf(&self, _args: std::fmt::Arguments<'_>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    fn println(&self, _args: std::fmt::Arguments<'_>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

// parity: adapted: Go's -race check of concurrent callback logs becomes a thread appending and reserving while the row's uploads are released (Rust's type system rules out the data race)
#[tokio::test(start_paused = true)]
async fn test_release_row_uploads_concurrent_callback_logs_do_not_race() {
    let off = hooked(|ctx, _| Box::pin(wait_done(ctx)));
    let logger = Arc::new(CallbackRaceLogger::default());
    let bp = Arc::new(BatchProcessor::bare(
        Config {
            gcs_offloader: as_off(&off),
            gcs_upload_timeout: Duration::from_secs(60),
            gcs_max_pending_upload_bytes: 64,
            batch_size: 1000,
            logger: as_log(&logger),
            ..Config::zero()
        }
        .apply_defaults(),
        0,
    ));
    bp.set_max_pending_upload_bytes(64);
    let o = reserving(&bp, 10);
    let mut row = queued_row(&o, &[image_content(&[0; 64])]);
    let expired = Context::background().with_timeout(ms(25));
    resolve(&bp, &expired, false, std::slice::from_mut(&mut row)).await;
    expired.cancel();
    assert_eq!(
        pending_upload_bytes(&bp),
        64,
        "pendingUploadBytes while upload is deferred"
    );

    let stop = Arc::new(AtomicBool::new(false));
    let worker = {
        let (bp, stop) = (Arc::clone(&bp), Arc::clone(&stop));
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                bp.upload_budget_warned.store(false, Ordering::SeqCst);
                bp.try_reserve_upload_bytes(1);
                bp.append(with_event_id(
                    crate::row::QueuedRow::new(Row::new()),
                    "drop",
                ));
            }
        })
    };
    for _ in 0..8 {
        std::thread::yield_now();
    }
    bp.release_row_uploads(&mut row);
    stop.store(true, Ordering::SeqCst);
    worker.join().unwrap();
    bp.clear_deferred_uploads(std::slice::from_mut(&mut row));
    assert!(logger.0.load(Ordering::SeqCst) > 0, "no line was logged");
}

#[tokio::test(start_paused = true)]
async fn test_release_row_uploads_shared_logger_keeps_drop_lines_with_their_processor() {
    let off = hooked(|ctx, _| Box::pin(wait_done(ctx)));
    let logger = Arc::new(RecordingLogger::default());
    let other = capped(&off, 64, as_log(&logger), 0);
    let bp = capped(&off, 64, as_log(&logger), 0);
    let o = reserving(&bp, 10);
    let mut row = with_event_id(
        queued_row(&o, &[image_content(&[0; 64])]),
        "ev-shared-logger",
    );
    let expired = Context::background().with_timeout(ms(25));
    resolve(&bp, &expired, false, std::slice::from_mut(&mut row)).await;
    expired.cancel();

    other.config.logf(format_args!(
        "ERROR: Failed to write batch to BigQuery: {}",
        Error::msg("other processor")
    ));
    bp.release_row_uploads(&mut row);
    assert_eq!(
        pending_upload_bytes(&bp),
        0,
        "pendingUploadBytes after releaseRowUploads"
    );
    bp.clear_deferred_uploads(std::slice::from_mut(&mut row));
    assert_eq!(
        upload_failure_lines(&logger),
        [
            "agentanalytics: GCS upload failed for event ev-shared-logger part 0 (64 bytes): context deadline exceeded; keeping the part inline"
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn test_resolve_uploads_custom_error_under_caller_deadline_stays_deferred() {
    let off = hooked(|ctx, _| {
        Box::pin(async move {
            ctx.done().await;
            Err(Error::msg("custom offloader failure"))
        })
    });
    let bp = capped(&off, 64, as_log(&Arc::new(RecordingLogger::default())), 0);
    let o = reserving(&bp, 10);
    let mut row = queued_row(&o, &[image_content(&[0; 64])]);
    let ctx = Context::background().with_timeout(ms(100));
    resolve(&bp, &ctx, false, std::slice::from_mut(&mut row)).await;
    assert_eq!(pending_upload_bytes(&bp), 64, "pendingUploadBytes");
    assert!(
        row.raw_parts.is_empty(),
        "payloads queued before requeueDeferredUploads"
    );
    bp.requeue_deferred_uploads(std::slice::from_mut(&mut row));
    assert_eq!(
        row.raw_parts.len(),
        1,
        "requeued uploads, want the deferred part"
    );
    ctx.cancel();
}

// TestResolveUploads_DeferredUploadLogsRetryOnlyWhenRequeued checks that the failure of an upload
// that a depleted caller deadline cut short is logged once it is known whether the upload is
// retried, and only then: as pending a retry if requeued, and as final if dropped.
#[tokio::test(start_paused = true)]
async fn test_resolve_uploads_deferred_upload_logs_retry_only_when_requeued() {
    const FAILURE: &str = "agentanalytics: GCS upload failed for event ev-deferred part 0 (3 bytes): context deadline exceeded; keeping the part inline";
    for (name, requeue, want_log) in [
        (
            "the upload is requeued",
            true,
            format!("{FAILURE} pending retry on next flush"),
        ),
        ("the upload is dropped", false, FAILURE.to_owned()),
    ] {
        let stalled = Arc::new(AtomicBool::new(true));
        let logger = Arc::new(RecordingLogger::default());
        let bp = minute(&stalling_offloader(&stalled), as_log(&logger));
        let mut batch = vec![with_event_id(
            queued_row(&super::opts(10), &[image_content(&[1, 2, 3])]),
            "ev-deferred",
        )];

        defer_uploads(&bp, &mut batch).await;
        assert!(
            upload_failure_lines(&logger).is_empty(),
            "{name}: resolveUploads logged {:?} before it was known whether the deferred upload is retried",
            upload_failure_lines(&logger)
        );
        if requeue {
            bp.requeue_deferred_uploads(&mut batch);
        } else {
            bp.clear_deferred_uploads(&mut batch);
        }
        assert_eq!(upload_failure_lines(&logger), [want_log], "{name}");
    }
}

// TestResolveUploads_CustomOffloaderErrorAfterCallerDeadlineIsDeferred checks that an offloader
// error that does not wrap a deadline error, returned once a depleted caller deadline has ended
// the uploads, defers the upload as a deadline error does.
#[tokio::test(start_paused = true)]
async fn test_resolve_uploads_custom_offloader_error_after_caller_deadline_is_deferred() {
    let stalled = Arc::new(AtomicBool::new(true));
    let off = {
        let stalled = Arc::clone(&stalled);
        hooked(move |ctx, _| {
            let stalled = stalled.load(Ordering::SeqCst);
            Box::pin(async move {
                if stalled {
                    ctx.done().await;
                    return Err(Error::msg("custom timeout error"));
                }
                Ok(())
            })
        })
    };
    let logger = Arc::new(RecordingLogger::default());
    let bp = minute(&off, as_log(&logger));
    let mut batch = vec![with_event_id(
        queued_row(&super::opts(10), &[image_content(&[1, 2, 3])]),
        "ev-custom",
    )];

    let ctx = Context::background().with_timeout(ms(25));
    resolve(&bp, &ctx, false, &mut batch).await;
    ctx.cancel();
    assert_eq!(
        kept(&batch[0]),
        1,
        "the upload that failed with a custom error after the caller deadline ran out was not deferred; log:\n{logger}"
    );

    bp.requeue_deferred_uploads(&mut batch);
    assert_eq!(
        batch[0].raw_parts.len(),
        1,
        "uploads queued after requeueDeferredUploads"
    );
    assert_eq!(
        upload_failure_lines(&logger),
        [
            "agentanalytics: GCS upload failed for event ev-custom part 0 (3 bytes): error type *errors.errorString; keeping the part inline pending retry on next flush"
        ]
    );

    stalled.store(false, Ordering::SeqCst);
    resolve(&bp, &Context::background(), false, &mut batch).await;
    assert_eq!(
        field(&parts(&batch[0])[0], "storage_mode"),
        "GCS_REFERENCE",
        "storage_mode after the retry"
    );
    assert_eq!(
        off.uploads().len(),
        2,
        "offloader calls: the cut-short upload and its retry"
    );
}

// TestClearDeferredUploads_PanickingLoggerReleasesEveryRow checks that the reservations of the
// deferred uploads of every row are released before any of their failures is logged.
#[tokio::test(start_paused = true)]
async fn test_clear_deferred_uploads_panicking_logger_releases_every_row() {
    let stalled = Arc::new(AtomicBool::new(true));
    let bp = minute(
        &stalling_offloader(&stalled),
        Some(Arc::new(ReproPanicLogger)),
    );
    let mut batch = upload_rows(&bp, 3);
    defer_uploads(&bp, &mut batch).await;
    assert_eq!(
        pending_upload_bytes(&bp),
        48,
        "pendingUploadBytes while the uploads are deferred"
    );

    assert!(
        panic_value_sync(|| bp.clear_deferred_uploads(&mut batch)).is_some(),
        "clearDeferredUploads did not log the failures of the uploads that it dropped"
    );
    assert_eq!(
        pending_upload_bytes(&bp),
        0,
        "pendingUploadBytes after the logger panicked"
    );
    for (i, q) in batch.iter().enumerate() {
        assert!(
            q.deferred.is_none(),
            "row {i} still holds its deferred uploads"
        );
    }
}

// TestRequeueDeferredUploads_PanickingLoggerRequeuesEveryRow checks that every row is requeued,
// keeping its reservations, before the failures of the deferred uploads are logged.
#[tokio::test(start_paused = true)]
async fn test_requeue_deferred_uploads_panicking_logger_requeues_every_row() {
    let stalled = Arc::new(AtomicBool::new(true));
    let bp = minute(
        &stalling_offloader(&stalled),
        Some(Arc::new(ReproPanicLogger)),
    );
    let mut batch = upload_rows(&bp, 3);
    defer_uploads(&bp, &mut batch).await;

    assert!(
        panic_value_sync(|| bp.requeue_deferred_uploads(&mut batch)).is_some(),
        "requeueDeferredUploads did not log the failures of the uploads that it requeued"
    );
    for (i, q) in batch.iter().enumerate() {
        assert_eq!(
            q.raw_parts.len(),
            1,
            "row {i} uploads queued after the logger panicked"
        );
        assert_eq!(
            field(&parts(q)[0], "text"),
            INLINE_DATA_TEXT,
            "row {i} part text until the upload is retried"
        );
    }
    assert_eq!(
        pending_upload_bytes(&bp),
        48,
        "pendingUploadBytes after the logger panicked, kept for the retry"
    );

    // The next flush retries every upload and releases the reservations.
    stalled.store(false, Ordering::SeqCst);
    resolve(&bp, &Context::background(), false, &mut batch).await;
    for (i, q) in batch.iter().enumerate() {
        assert_eq!(
            field(&parts(q)[0], "storage_mode"),
            "GCS_REFERENCE",
            "row {i} storage_mode after the retry"
        );
    }
    assert_eq!(
        pending_upload_bytes(&bp),
        0,
        "pendingUploadBytes after the retry"
    );
}

// TestReleaseBatchUploads_PanickingLoggerReleasesEveryRow checks that dropping a batch releases
// the reservations of the payloads still queued on its rows, and of its deferred uploads,
// before the failures of the deferred uploads are logged.
#[tokio::test(start_paused = true)]
async fn test_release_batch_uploads_panicking_logger_releases_every_row() {
    let stalled = Arc::new(AtomicBool::new(true));
    let bp = minute(
        &stalling_offloader(&stalled),
        Some(Arc::new(ReproPanicLogger)),
    );
    let mut batch = upload_rows(&bp, 3);
    // The first two rows have deferred uploads; the last still has its payload queued.
    defer_uploads(&bp, &mut batch[..2]).await;
    assert_eq!(
        pending_upload_bytes(&bp),
        48,
        "pendingUploadBytes before the batch is dropped"
    );

    assert!(
        panic_value_sync(|| bp.release_batch_uploads(&mut batch)).is_some(),
        "releaseBatchUploads did not log the failures of the deferred uploads that it dropped"
    );
    assert_eq!(
        pending_upload_bytes(&bp),
        0,
        "pendingUploadBytes after the logger panicked"
    );
    for (i, q) in batch.iter().enumerate() {
        assert!(q.raw_parts.is_empty(), "row {i} still has queued payloads");
        assert!(q.deferred.is_none(), "row {i} still has deferred uploads");
    }
}

// TestGCSOffloading_FlushRunEndKeepsDeferredBatchWhenLoggerPanics checks that a run-end flush
// whose BigQuery write runs out of its budget keeps the batch in deferredBatch before it logs
// that the batch's deferred uploads are pending a retry, so that a Logger that panics on that
// line loses neither the batch nor the reservations of its uploads.
// parity: adapted: Go reads the kept row's queued payloads on its map; gadk reads them on the kept QueuedRow in deferred_batch
#[tokio::test(start_paused = true)]
async fn test_gcs_offloading_flush_run_end_keeps_deferred_batch_when_logger_panics() {
    let server = FakeWriteServer::new();
    let (tx, mut rx) = requests();
    let server_delay = Arc::new(AtomicI64::new(0));
    {
        let server_delay = Arc::clone(&server_delay);
        scripted_write_server(
            &server,
            Arc::new(move |_| {
                Ok(Duration::from_nanos(
                    server_delay.load(Ordering::SeqCst).unsigned_abs(),
                ))
            }),
            None,
            tx,
        );
    }
    let stalled = Arc::new(AtomicBool::new(true));
    let mut cfg = Config::default();
    cfg.batch_size = 1;
    cfg.batch_flush_intv = Duration::from_secs(3600);
    cfg.shutdown_timeout = Duration::from_secs(1);
    cfg.retry_config.max_retries = 0;
    cfg.gcs_offloader = as_off(&stalling_offloader(&stalled));
    cfg.logger = Some(Arc::new(ReproPanicLogger));
    let bp = processor(
        &Context::background(),
        Some(server.client().unwrap()),
        PDT_DEFAULT_STREAM,
        cfg,
    )
    .await;

    // Warm up the write stream before starting the run-end budget.
    warm_up(&bp, &mut rx).await;
    server_delay.store(800_000_000, Ordering::SeqCst);

    // Batch 0 has no uploads, and its 800ms write leaves ~200ms, less than a quarter (250ms) of
    // the 1s ShutdownTimeout: batch 1 starts with pause_on_caller_deadline false, so its stalled
    // upload is deferred without pausing uploads, and its 800ms write runs out of the budget,
    // keeping batch 1 for the next flush.
    push_queue(&bp, event_row("USER_MESSAGE_RECEIVED"));
    push_queue(
        &bp,
        tagged(
            queued_row(&reserving(&bp, 10), &[image_content(&[0; 64])]),
            "LLM_REQUEST",
            "",
        ),
    );

    assert!(
        panic_value(bp.flush_run_end()).await.is_some(),
        "flushRunEnd did not log the failure of the requeued upload"
    );
    assert!(
        !upload_paused(&bp),
        "the deferred upload must not pause the uploads"
    );
    {
        let state = bp.flush_mu.lock().await;
        assert_eq!(
            state.deferred_batch.len(),
            1,
            "deferredBatch length after the logger panicked"
        );
        assert_eq!(
            state.deferred_batch[0].row.str("event_type"),
            Some("LLM_REQUEST"),
            "deferredBatch[0] event_type"
        );
        assert_eq!(
            state.deferred_batch[0].raw_parts.len(),
            1,
            "the kept row's uploads queued, want its deferred upload"
        );
    }
    assert_eq!(
        pending_upload_bytes(&bp),
        64,
        "pendingUploadBytes after the logger panicked, kept for the retry"
    );

    // Once Cloud Storage and BigQuery recover, the next flush writes the kept batch, retrying
    // its upload.
    stalled.store(false, Ordering::SeqCst);
    server_delay.store(0, Ordering::SeqCst);
    bp.flush().await;
    assert_eq!(
        bp.flush_mu.lock().await.deferred_batch.len(),
        0,
        "deferredBatch length after flush"
    );
    assert_eq!(
        pending_upload_bytes(&bp),
        0,
        "pendingUploadBytes after flush"
    );

    // Under at-least-once delivery, the timed-out INLINE attempt for batch 1 may also be
    // recorded before the retried GCS_REFERENCE write.
    let mut rows = collect_rows(&mut rx, |rows| rows.len() >= 2).await;
    if field(&part0(rows.last().unwrap()), "storage_mode") == "INLINE"
        && let Ok(Some(req)) = tokio::time::timeout(ms(250), rx.recv()).await
    {
        rows.extend(
            super::super::plugin_agentanalytics_batch_processor::support::decode_arrow_rows(&req),
        );
    }
    assert!(
        rows.len() == 2 || rows.len() == 3,
        "persisted rows = {}, want 2 or 3",
        rows.len()
    );
    assert_eq!(
        rows[0].str("event_type"),
        Some("USER_MESSAGE_RECEIVED"),
        "rows[0] event_type"
    );
    if rows.len() == 3 {
        assert_eq!(
            rows[1].str("event_type"),
            Some("LLM_REQUEST"),
            "rows[1] event_type"
        );
    }
    let last = rows.last().unwrap();
    assert_eq!(
        last.str("event_type"),
        Some("LLM_REQUEST"),
        "last row event_type"
    );
    assert_eq!(
        field(&part0(last), "storage_mode"),
        "GCS_REFERENCE",
        "the retried part's storage_mode"
    );

    stalled.store(false, Ordering::SeqCst);
    server_delay.store(0, Ordering::SeqCst);
    let _ = panic_value(bp.close()).await;
}

// TestResolveUploads_CanceledCallerContextDefersUploads checks that an upload that a canceled
// caller context cuts short without pausing uploads is deferred, as one that a depleted deadline
// cuts short is, so that a requeued batch retries it.
#[tokio::test(start_paused = true)]
async fn test_resolve_uploads_canceled_caller_context_defers_uploads() {
    let started = Arc::new(tokio::sync::Notify::new());
    let calls = Arc::new(AtomicI32::new(0));
    let off = {
        let (started, calls) = (Arc::clone(&started), Arc::clone(&calls));
        hooked(move |ctx, _| {
            let first = calls.fetch_add(1, Ordering::SeqCst) == 0;
            if first {
                started.notify_one();
            }
            Box::pin(async move {
                if first {
                    return wait_done(ctx).await;
                }
                Ok(())
            })
        })
    };
    let logger = Arc::new(RecordingLogger::default());
    let bp = minute(&off, as_log(&logger));
    let mut batch = vec![with_event_id(
        queued_row(&super::opts(10), &[image_content(&[1, 2, 3])]),
        "ev-canceled",
    )];

    let ctx = Context::background().with_cancel();
    let canceler = {
        let (ctx, started) = (ctx.clone(), Arc::clone(&started));
        tokio::spawn(async move {
            started.notified().await;
            ctx.cancel();
        })
    };
    resolve(&bp, &ctx, false, &mut batch).await;
    canceler.await.unwrap();
    assert_eq!(
        kept(&batch[0]),
        1,
        "the upload that a canceled caller context cut short was not deferred; log:\n{logger}"
    );

    bp.requeue_deferred_uploads(&mut batch);
    assert_eq!(
        upload_failure_lines(&logger),
        [
            "agentanalytics: GCS upload failed for event ev-canceled part 0 (3 bytes): context canceled; keeping the part inline pending retry on next flush"
        ]
    );
    resolve(&bp, &Context::background(), false, &mut batch).await;
    assert_eq!(
        field(&parts(&batch[0])[0], "storage_mode"),
        "GCS_REFERENCE",
        "storage_mode after the retry"
    );
}

// TestGCSOffloading_CloseRetriesUploadThatItCancelsInRunEndFlush checks that an upload that
// Close cancels while a run-end flush is uploading is deferred: when the BigQuery write of its
// batch then runs out of the budget of the flush, the flush requeues the upload, logs it once as
// pending a retry, and keeps the batch, and Close retries the upload when it writes the batch.
#[tokio::test(start_paused = true)]
async fn test_gcs_offloading_close_retries_upload_that_it_cancels_in_run_end_flush() {
    let server = FakeWriteServer::new();
    let (tx, mut rx) = requests();
    scripted_write_server(
        &server,
        Arc::new(|n| {
            Ok(match n {
                // The write of row A leaves ~1.2s of the 3s budget (~600ms for row B's upload
                // window), leaving margin for Close to cancel row B's in-flight upload.
                1 => ms(1800),
                // The write of the batch of row B outlasts the budget.
                2 => Duration::from_secs(60),
                _ => Duration::ZERO,
            })
        }),
        None,
        tx,
    );
    let b_started = Arc::new(tokio::sync::Notify::new());
    let b_calls = Arc::new(AtomicI32::new(0));
    let off = {
        let (b_started, b_calls) = (Arc::clone(&b_started), Arc::clone(&b_calls));
        hooked(move |ctx, req| {
            let first_b =
                req.path.contains("_ev-b_") && b_calls.fetch_add(1, Ordering::SeqCst) == 0;
            if first_b {
                b_started.notify_one();
            }
            Box::pin(async move {
                if first_b {
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
    cfg.shutdown_timeout = Duration::from_secs(3);
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

    let o = reserving(&bp, 10);
    for id in ["ev-a", "ev-b"] {
        push_queue(
            &bp,
            tagged(
                queued_row(&o, &[image_content(&[1, 2, 3])]),
                "LLM_REQUEST",
                id,
            ),
        );
    }

    // Close cancels the uploads of the run-end flush while the upload of row B is in flight,
    // and then waits for the flush to finish.
    let closed = {
        let (bp, b_started) = (Arc::clone(&bp), Arc::clone(&b_started));
        tokio::spawn(async move {
            b_started.notified().await;
            bp.close().await;
        })
    };
    bp.flush_run_end().await;
    tokio::time::timeout(Duration::from_secs(10), closed)
        .await
        .expect("Close did not return")
        .unwrap();

    let rows = collect_rows(&mut rx, |rows| rows.len() >= 2).await;
    let row_b = rows
        .iter()
        .find(|r| r.str("event_id") == Some("ev-b"))
        .unwrap_or_else(|| panic!("row B was not written; rows: {rows:?}"));
    assert_eq!(
        field(&part0(row_b), "storage_mode"),
        "GCS_REFERENCE",
        "row B storage_mode: Close retries the upload that it canceled"
    );
    assert_eq!(
        off.uploads().len(),
        3,
        "offloader calls: row A, row B, and the retry of row B"
    );
    assert_eq!(
        upload_failure_lines(&logger),
        [
            "agentanalytics: GCS upload failed for event ev-b part 0 (3 bytes): context canceled; keeping the part inline pending retry on next flush"
        ]
    );
    assert_eq!(
        pending_upload_bytes(&bp),
        0,
        "pendingUploadBytes after Close"
    );
    assert!(
        !upload_paused(&bp),
        "canceling an upload must not pause the uploads"
    );
}

/// The offloader of the two drop-path tests: row B's uploads stall until `closing`.
fn b_stalling_offloader(closing: &Arc<AtomicBool>) -> Arc<HookOffloader> {
    let closing = Arc::clone(closing);
    hooked(move |ctx, req| {
        let stall = !closing.load(Ordering::SeqCst) && req.path.contains("_ev-b");
        Box::pin(async move {
            if stall {
                return wait_done(ctx).await;
            }
            Ok(())
        })
    })
}

/// Queues batch A (`ev-a0..2`) and batch B (`ev-b0..2`) of 16-byte images.
fn queue_a_and_b(bp: &Arc<BatchProcessor>) {
    let o = reserving(bp, 10);
    for id in ["ev-a0", "ev-a1", "ev-a2", "ev-b0", "ev-b1", "ev-b2"] {
        push_queue(
            bp,
            tagged(
                queued_row(&o, &[image_content(&[0; 16])]),
                "LLM_REQUEST",
                id,
            ),
        );
    }
}

// TestGCSOffloading_FlushRunEndDropPathReleasesAllUploadsWhenLoggerPanics checks that when
// BigQuery rejects the write of a run-end batch whose uploads were deferred, flushRunEnd releases
// the reservations of the deferred uploads of every row before it logs their failures.
#[tokio::test(start_paused = true)]
async fn test_gcs_offloading_flush_run_end_drop_path_releases_all_uploads_when_logger_panics() {
    let server = FakeWriteServer::new();
    let (tx, mut rx) = requests();
    scripted_write_server(
        &server,
        Arc::new(|n| match n {
            1 => Ok(Duration::ZERO),
            // Batch A's 1.6s write leaves less than a quarter (500ms) of the 2s budget, so
            // batch B's run-end deadline cannot pause the uploads and its stalled uploads are
            // deferred, while leaving time for batch B's rejected write.
            2 => Ok(ms(1600)),
            _ => Err(status(Code::InvalidArgument, "rejected")),
        }),
        None,
        tx,
    );
    let closing = Arc::new(AtomicBool::new(false));
    let mut cfg = Config::default();
    cfg.batch_size = 3;
    cfg.batch_flush_intv = Duration::from_secs(3600);
    cfg.shutdown_timeout = Duration::from_secs(2);
    cfg.retry_config.max_retries = 0;
    cfg.gcs_offloader = as_off(&b_stalling_offloader(&closing));
    cfg.logger = Some(Arc::new(ReproPanicLogger));
    let bp = processor(
        &Context::background(),
        Some(server.client().unwrap()),
        PDT_DEFAULT_STREAM,
        cfg,
    )
    .await;

    warm_up(&bp, &mut rx).await;

    // Batch A uploads its rows; batch B has its uploads deferred and its write rejected, which
    // drops it.
    queue_a_and_b(&bp);

    assert!(
        panic_value(bp.flush_run_end()).await.is_some(),
        "flushRunEnd did not log the failures of the deferred uploads of the rejected batch"
    );
    assert_eq!(
        pending_upload_bytes(&bp),
        0,
        "pendingUploadBytes after the logger panicked on the drop path"
    );
    assert_eq!(
        bp.flush_mu.lock().await.deferred_batch.len(),
        0,
        "deferredBatch length: a rejected batch is dropped"
    );
    assert_eq!(
        bp.drop_stats().append_failure_drops,
        3,
        "AppendFailureDrops, the rows of the rejected batch"
    );

    closing.store(true, Ordering::SeqCst);
    let _ = panic_value(bp.close()).await;
}

// TestResolveUploads_UnsettledDeferredUploadsLoseNoReservationWhenLoggerPanics checks that
// resolveUploads drops the deferred uploads that a caller left unsettled before it takes over the
// reservations of the payloads queued on the batch.
#[tokio::test(start_paused = true)]
async fn test_resolve_uploads_unsettled_deferred_uploads_lose_no_reservation_when_logger_panics() {
    let stalled = Arc::new(AtomicBool::new(true));
    let bp = minute(
        &stalling_offloader(&stalled),
        Some(Arc::new(ReproPanicLogger)),
    );
    let mut batch = upload_rows(&bp, 2);
    // The upload of the second row is deferred and left unsettled, while the first row still
    // has its payload queued.
    defer_uploads(&bp, &mut batch[1..]).await;
    assert_eq!(
        pending_upload_bytes(&bp),
        32,
        "pendingUploadBytes before the batch is resolved"
    );

    stalled.store(false, Ordering::SeqCst);
    assert!(
        panic_value(resolve(&bp, &Context::background(), false, &mut batch))
            .await
            .is_some(),
        "resolveUploads did not log the failure of the unsettled deferred upload that it dropped"
    );
    // Right after the recovered panic, the first row's still-queued payload holds 16 bytes; the
    // second row's stale deferred reservation was released, so 16 bytes remain reserved.
    assert_eq!(
        pending_upload_bytes(&bp),
        16,
        "pendingUploadBytes after the logger panicked, before releaseBatchUploads"
    );
    // Dropping the batch releases what its rows still hold, which must be every reservation.
    bp.release_batch_uploads(&mut batch);
    assert_eq!(
        pending_upload_bytes(&bp),
        0,
        "pendingUploadBytes after the logger panicked and the batch was dropped"
    );
}

// TestGCSOffloading_FlushRunEndReleasesDeferredUploadsWhenAppendLogPanics checks that when the
// BigQuery write of a run-end batch whose uploads were deferred panics, as it does when BigQuery
// answers with an in-band error and the Logger panics on the "BigQuery response" line, the
// reservations of the deferred uploads are released although they were never settled.
#[tokio::test(start_paused = true)]
async fn test_gcs_offloading_flush_run_end_releases_deferred_uploads_when_append_log_panics() {
    let server = FakeWriteServer::new();
    let (tx, mut rx) = requests();
    scripted_write_server(
        &server,
        Arc::new(|n| {
            Ok(if n == 2 {
                // Batch A's 1.6s write leaves less than a quarter (500ms) of the 2s budget, so
                // batch B's stalled uploads are deferred without pausing the uploads.
                ms(1600)
            } else {
                Duration::ZERO
            })
        }),
        Some(Arc::new(|n| {
            (n > 2).then(|| in_band(Code::InvalidArgument, "rejected"))
        })),
        tx,
    );
    let closing = Arc::new(AtomicBool::new(false));
    let mut cfg = Config::default();
    cfg.batch_size = 3;
    cfg.batch_flush_intv = Duration::from_secs(3600);
    cfg.shutdown_timeout = Duration::from_secs(2);
    cfg.retry_config.max_retries = 0;
    cfg.gcs_offloader = as_off(&b_stalling_offloader(&closing));
    cfg.logger = Some(Arc::new(AlwaysPanicLogger));
    let bp = processor(
        &Context::background(),
        Some(server.client().unwrap()),
        PDT_DEFAULT_STREAM,
        cfg,
    )
    .await;

    warm_up(&bp, &mut rx).await;

    // Batch A uploads its rows; batch B has its three 16-byte uploads deferred, and its write is
    // answered with an in-band error, whose log line panics before the deferred uploads are
    // settled.
    queue_a_and_b(&bp);

    let r = panic_value(bp.flush_run_end()).await;
    assert!(
        r.as_deref()
            .is_some_and(|s| s.contains("BigQuery response")),
        "flushRunEnd panicked with {r:?}, want the panic of the BigQuery response log line"
    );
    assert_eq!(
        pending_upload_bytes(&bp),
        0,
        "pendingUploadBytes after the append panicked"
    );

    closing.store(true, Ordering::SeqCst);
    let _ = panic_value(bp.close()).await;
}
