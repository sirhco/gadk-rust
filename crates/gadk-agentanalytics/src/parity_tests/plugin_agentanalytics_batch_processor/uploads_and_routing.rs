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

//! `batch_processor_test.go` ports, Go file order from `TestBatchProcessor_FlushRunEndStalledUploadPausesLaterBatches` through `TestStreamWriter_InBandErrorRetriesAreBounded`.

use super::support::{dynamic_delay_server, offload_opts};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use gadk_platform::BoxFuture;
use pretty_assertions::assert_eq;
use tokio::sync::oneshot;
use tokio::time::Instant;

use super::support::{
    HookOffloader, OffloadHook, ScriptedServer, as_logger, collect_rows, content_parts_of,
    event_row, exactly_once_test_config, hook_server, image_content, image_contents,
    new_error_classification_processor, pending_upload_bytes, processor, push_queue, queued_row,
    recording_server, requests, status, upload_paused,
};
use crate::batch_processor::BatchProcessor;
use crate::bigquery::fake::RecordingLogger;
use crate::config::{Config, GcsContentOffloader, GcsUploadRequest, RetryConfig};
use crate::context::{Context, ContextError};
use crate::error::{Code, Error, status_code};
use crate::formatter::{PartsOptions, UPLOAD_FAILED_TEXT};
use crate::row::{QueuedRow, Row, RowValue};
use crate::storagepb::AppendRowsResponse;
use crate::testutil::fake_write_server::FakeWriteServer;
use crate::write::stream_writer::tests::in_band;
use crate::write::stream_writer::{
    STREAM_WRITER_TRACE_ID_PREFIX, stream_writer_trace_id, stream_writer_trace_id_for_version,
};

/// The `_default` stream Go's upload tests write to.
const P_DEFAULT_STREAM: &str = "projects/p/datasets/d/tables/t/_default";

/// Go `ctx.Err()` of an ended context, as an error.
fn ctx_err(ctx: &Context) -> Error {
    Error::Context(ctx.err().unwrap_or(ContextError::Canceled))
}

/// A fakeOffloader hook: waits `d` (then `result`) unless `ctx` ends first.
async fn wait_or_done(ctx: &Context, d: Duration, result: Result<(), Error>) -> Result<(), Error> {
    tokio::select! {
        () = tokio::time::sleep(d) => result,
        () = ctx.done() => Err(ctx_err(ctx)),
    }
}

/// Go `GCSUploadTimeout`-style offloader config for `off`.
fn with_offloader(cfg: Config, off: &Arc<HookOffloader>, logger: &Arc<RecordingLogger>) -> Config {
    Config {
        gcs_offloader: Some(Arc::clone(off) as Arc<dyn GcsContentOffloader>),
        logger: as_logger(logger),
        ..cfg
    }
}

/// Sets Go's `row["event_type"]` and `row[eventIDField]`.
fn tag(mut row: QueuedRow, event_type: &str, event_id: &str) -> QueuedRow {
    row.row
        .insert("event_type".to_owned(), RowValue::from(event_type));
    row.row
        .insert("event_id".to_owned(), RowValue::from(event_id));
    row
}

/// The warm-up write of Go's upload tests: opens the connection and the stream so that setting
/// them up does not use up a run-end budget.
async fn warm_up(
    bp: &BatchProcessor,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<crate::storagepb::AppendRowsRequest>,
) {
    let warm = Context::background().with_timeout(Duration::from_secs(5));
    bp.write_batch(&warm, vec![event_row("USER_MESSAGE_RECEIVED")])
        .await
        .unwrap_or_else(|e| panic!("warm-up writeBatch: {e}"));
    warm.cancel();
    rx.recv().await.expect("warm-up request");
}

/// Whether the decoded part has `storage_mode` and `text`.
fn part_is(p: &Row, storage_mode: &str, text: &str) -> bool {
    p.get("storage_mode") == Some(&RowValue::from(storage_mode))
        && p.get("text") == Some(&RowValue::from(text))
}

#[tokio::test(start_paused = true)]
async fn test_batch_processor_flush_run_end_stalled_upload_pauses_later_batches() {
    type Rows = fn(&PartsOptions) -> Vec<QueuedRow>;
    let cases: [(&str, Rows); 2] = [
        (
            "an upload stalls while another in the batch succeeds",
            |opts| {
                vec![
                    queued_row(opts, &[image_content(&[1]), image_content(&[2])]),
                    queued_row(opts, &[image_content(&[3])]),
                ]
            },
        ),
        ("the stall starts on a later batch", |opts| {
            vec![
                queued_row(opts, &[image_content(&[1])]),
                queued_row(opts, &[image_content(&[2])]),
                queued_row(opts, &[image_content(&[3])]),
            ]
        }),
    ];
    for (name, rows) in cases {
        let server = FakeWriteServer::new();
        let (tx, mut rx) = requests();
        super::support::delayed_server(
            &server,
            super::support::Delayed {
                delay: Duration::from_millis(10),
                requests: Some(tx),
                ..super::support::Delayed::default()
            },
        );
        let calls = Arc::new(AtomicI64::new(0));
        let healthy = Arc::new(AtomicBool::new(false));
        let hook: OffloadHook = {
            let (calls, healthy) = (Arc::clone(&calls), Arc::clone(&healthy));
            Arc::new(move |ctx: Context, _req: GcsUploadRequest| {
                let first = calls.fetch_add(1, Ordering::SeqCst) + 1 == 1;
                let healthy = healthy.load(Ordering::SeqCst);
                Box::pin(async move {
                    if first || healthy {
                        return Ok(());
                    }
                    ctx.done().await;
                    Err(ctx_err(&ctx))
                })
            })
        };
        let off = Arc::new(HookOffloader::with_hook(hook));
        let logger = Arc::new(RecordingLogger::default());
        let mut cfg = with_offloader(Config::default(), &off, &logger);
        cfg.batch_size = 1;
        cfg.batch_flush_intv = Duration::from_secs(3600);
        cfg.shutdown_timeout = Duration::from_secs(1);
        cfg.retry_config.max_retries = 0;
        let shutdown = cfg.shutdown_timeout;
        let bp = processor(
            &Context::background(),
            Some(server.client().unwrap()),
            P_DEFAULT_STREAM,
            cfg,
        )
        .await;
        warm_up(&bp, &mut rx).await;

        // The rows are queued directly, so that no background flush takes flush_mu before
        // flush_run_end does. Each batch starts with more than a quarter of the 1s
        // ShutdownTimeout left.
        let opts = offload_opts(Some(&bp));
        for (i, row) in rows(&opts).into_iter().enumerate() {
            push_queue(&bp, tag(row, "LLM_REQUEST", &format!("ev-{i}")));
        }
        let start = Instant::now();
        bp.flush_run_end().await;

        assert_eq!(
            off.uploads().len(),
            2,
            "{name}: offloader calls (the batch after the stall falls back without calling it)"
        );
        assert!(
            upload_paused(&bp),
            "{name}: uploadCooldownUntil = 0 after the run-end flush, want the uploads paused"
        );
        assert!(
            logger.contains("upload skipped during timeout cooldown"),
            "{name}: no batch fell back during the pause, log:\n{logger}"
        );

        // Once the budget of the run-end flush is over, a background flush still falls back
        // without calling the offloader, even though Cloud Storage is healthy again.
        tokio::time::sleep_until(start + shutdown + Duration::from_millis(50)).await;
        healthy.store(true, Ordering::SeqCst);
        let late = tag(
            queued_row(&opts, &[image_content(&[4])]),
            "LLM_REQUEST",
            "ev-late",
        );
        push_queue(&bp, late);
        bp.flush().await;
        assert_eq!(
            off.uploads().len(),
            2,
            "{name}: offloader calls after a later background flush (the pause outlasts the run-end flush)"
        );
        bp.close().await;
    }
}

#[tokio::test(start_paused = true)]
async fn test_batch_processor_flush_run_end_deferred_retry_prioritizes_big_query_write() {
    const ROW_COUNT: usize = 12;
    const RETRY_LOG: &str = "pending retry on next flush";
    let server = FakeWriteServer::new();
    let (tx, mut rx) = requests();
    // server_delay stays 0 for the warm-up write below.
    let server_delay = Arc::new(AtomicI64::new(0));
    let in_retry = Arc::new(AtomicBool::new(false));
    let pending_during_retry_write = Arc::new(AtomicI64::new(-1));
    let cell: Arc<OnceLock<Arc<BatchProcessor>>> = Arc::new(OnceLock::new());
    let on_append: Arc<dyn Fn() + Send + Sync> = {
        let (in_retry, pending, cell) = (
            Arc::clone(&in_retry),
            Arc::clone(&pending_during_retry_write),
            Arc::clone(&cell),
        );
        Arc::new(move || {
            if in_retry.load(Ordering::SeqCst)
                && let Some(bp) = cell.get()
            {
                pending.store(pending_upload_bytes(bp), Ordering::SeqCst);
            }
        })
    };
    dynamic_delay_server(
        &server,
        Arc::clone(&server_delay),
        Some(on_append),
        Some(tx),
    );
    let off = Arc::new(HookOffloader::with_hook(Arc::new(
        |ctx: Context, _req: GcsUploadRequest| {
            Box::pin(async move { wait_or_done(&ctx, Duration::from_millis(400), Ok(())).await })
        },
    )));
    let logger = Arc::new(RecordingLogger::default());
    let mut cfg = with_offloader(Config::default(), &off, &logger);
    cfg.batch_flush_intv = Duration::from_secs(3600);
    cfg.shutdown_timeout = Duration::from_secs(4);
    cfg.retry_config.max_retries = 0;
    cfg.gcs_max_concurrent_uploads = 1;
    let bp = processor(
        &Context::background(),
        Some(server.client().unwrap()),
        P_DEFAULT_STREAM,
        cfg,
    )
    .await;
    let _ = cell.set(Arc::clone(&bp));

    warm_up(&bp, &mut rx).await;
    server_delay.store(2_300_000_000, Ordering::SeqCst);

    // The rows are queued directly, so that no background flush takes flush_mu before
    // flush_run_end does. They make a single batch with an upload each.
    let opts = offload_opts(Some(&bp));
    for i in 0..ROW_COUNT {
        let row = queued_row(&opts, &[image_content(&[1, 2, 3])]);
        push_queue(&bp, tag(row, "LLM_REQUEST", &format!("ev-{i}")));
    }

    // The uploads get half of the 4s budget (~2s) and leave the write less than the 2.3s that
    // it takes, so the batch is deferred with the uploads that were not done in time.
    bp.flush_run_end().await;
    let deferred = bp.flush_mu.lock().await.deferred_batch.len();
    assert_eq!(
        deferred, ROW_COUNT,
        "deferredBatch length after the first flushRunEnd"
    );
    assert_ne!(
        pending_upload_bytes(&bp),
        0,
        "pendingUploadBytes after the first flushRunEnd = 0, want the bytes of the uploads kept for the retry"
    );
    let retry_logs_before = logger.to_string().matches(RETRY_LOG).count();
    assert_ne!(
        retry_logs_before, 0,
        "first flushRunEnd logged no {RETRY_LOG:?} lines:\n{logger}"
    );

    let calls_before = off.uploads().len();
    in_retry.store(true, Ordering::SeqCst);
    bp.flush_run_end().await;
    in_retry.store(false, Ordering::SeqCst);

    let ds = bp.drop_stats();
    assert_eq!(
        ds.total_drops, 0,
        "DropStats after the second flushRunEnd = {ds:?}, want no drops: the uploads of the retry left the write too little time"
    );
    assert_eq!(
        pending_during_retry_write.load(Ordering::SeqCst),
        0,
        "pendingUploadBytes during retry write (released before tryAppendBatch)"
    );
    assert_eq!(
        bp.flush_mu.lock().await.deferred_batch.len(),
        0,
        "deferredBatch length after the second flushRunEnd"
    );
    assert_eq!(
        pending_upload_bytes(&bp),
        0,
        "pendingUploadBytes after the second flushRunEnd"
    );
    // The retry still uploads the parts that the first flush kept, for the quarter of the
    // budget (~1s) that it leaves them.
    let got = off.uploads().len() - calls_before;
    assert!(
        got >= 2,
        "the second flushRunEnd called the offloader {got} times, want at least 2"
    );
    assert_eq!(
        logger.to_string().matches(RETRY_LOG).count(),
        retry_logs_before,
        "{RETRY_LOG:?} log count after the second flushRunEnd (run-end retry cannot defer again):\n{logger}"
    );
    let rows = collect_rows(&mut rx, |rows| rows.len() >= ROW_COUNT).await;
    assert_eq!(rows.len(), ROW_COUNT, "persisted rows");
    bp.close().await;
}

// parity: skipped TestBatchProcessor_FlushRunEndDeferredRetryDoesNotPauseHealthyUploads: deferred to 7b-3 (its long-text row comes from probeRowWithOptions, which builds the row's content summary with buildLLMRequestContent)

#[tokio::test(start_paused = true)]
async fn test_batch_processor_non_deferrable_flushes_do_not_defer_cut_short_uploads() {
    const INLINE_LOG: &str = "keeping the part inline";
    const RETRY_LOG: &str = "pending retry";

    /// The processor of both sub-tests: a hook server measuring `pendingUploadBytes` while
    /// `measure` is set, and `off` as the offloader.
    async fn setup(
        off: &Arc<HookOffloader>,
        logger: &Arc<RecordingLogger>,
        shutdown: Duration,
    ) -> (
        Arc<BatchProcessor>,
        tokio::sync::mpsc::UnboundedReceiver<crate::storagepb::AppendRowsRequest>,
        Arc<AtomicBool>,
        Arc<AtomicI64>,
    ) {
        let server = FakeWriteServer::new();
        let (tx, rx) = requests();
        let measure = Arc::new(AtomicBool::new(false));
        let pending_during_write = Arc::new(AtomicI64::new(-1));
        let cell: Arc<OnceLock<Arc<BatchProcessor>>> = Arc::new(OnceLock::new());
        let on_append: Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync> = {
            let (measure, pending, cell) = (
                Arc::clone(&measure),
                Arc::clone(&pending_during_write),
                Arc::clone(&cell),
            );
            Arc::new(move || {
                if measure.load(Ordering::SeqCst)
                    && let Some(bp) = cell.get()
                {
                    pending.store(pending_upload_bytes(bp), Ordering::SeqCst);
                }
                Box::pin(async {})
            })
        };
        hook_server(&server, Some(on_append), Some(tx));
        let mut cfg = with_offloader(Config::default(), off, logger);
        cfg.batch_size = 1;
        cfg.batch_flush_intv = Duration::from_secs(3600);
        cfg.shutdown_timeout = shutdown;
        cfg.gcs_upload_timeout = Duration::from_secs(60);
        cfg.retry_config.max_retries = 0;
        cfg.gcs_max_concurrent_uploads = 1;
        let bp = processor(
            &Context::background(),
            Some(server.client().unwrap()),
            P_DEFAULT_STREAM,
            cfg,
        )
        .await;
        let _ = cell.set(Arc::clone(&bp));
        (bp, rx, measure, pending_during_write)
    }

    // Close drain.
    {
        let off = Arc::new(HookOffloader::with_hook(Arc::new(
            |ctx: Context, _req: GcsUploadRequest| {
                Box::pin(async move {
                    ctx.done().await;
                    Err(ctx_err(&ctx))
                })
            },
        )));
        let logger = Arc::new(RecordingLogger::default());
        let (bp, mut rx, measure, pending_during_write) =
            setup(&off, &logger, Duration::from_millis(800)).await;
        warm_up(&bp, &mut rx).await;

        let row = tag(
            queued_row(&offload_opts(Some(&bp)), &[image_content(&[4, 5, 6, 7, 8])]),
            "LLM_REQUEST",
            "ev-close-drain",
        );
        assert_eq!(
            pending_upload_bytes(&bp),
            5,
            "pendingUploadBytes before Close"
        );
        measure.store(true, Ordering::SeqCst);
        push_queue(&bp, row);

        bp.close().await;

        assert_eq!(
            pending_during_write.load(Ordering::SeqCst),
            0,
            "Close drain: pendingUploadBytes during the write (released before appendBatch)"
        );
        assert_eq!(
            pending_upload_bytes(&bp),
            0,
            "pendingUploadBytes after Close"
        );
        assert!(
            logger.contains(INLINE_LOG),
            "Close drain did not log {INLINE_LOG:?}"
        );
        assert!(
            !logger.contains(RETRY_LOG),
            "Close drain logged {RETRY_LOG:?} for a non-deferrable flush"
        );
        let rows = collect_rows(&mut rx, |rows| !rows.is_empty()).await;
        let p = &content_parts_of(&rows[0])[0];
        assert!(
            part_is(p, "INLINE", UPLOAD_FAILED_TEXT),
            "Close drain part = {p:?}, want INLINE and uploadFailedText"
        );
    }

    // Background flush canceled by Close.
    {
        let (started_tx, started_rx) = oneshot::channel::<()>();
        let started_tx = Arc::new(Mutex::new(Some(started_tx)));
        let off = Arc::new(HookOffloader::with_hook(Arc::new(
            move |ctx: Context, _req: GcsUploadRequest| {
                if let Some(tx) = started_tx.lock().unwrap().take() {
                    let _ = tx.send(());
                }
                Box::pin(async move {
                    ctx.done().await;
                    Err(ctx_err(&ctx))
                })
            },
        )));
        let logger = Arc::new(RecordingLogger::default());
        let (bp, mut rx, measure, pending_during_write) =
            setup(&off, &logger, Duration::from_secs(2)).await;
        warm_up(&bp, &mut rx).await;

        let row = tag(
            queued_row(&offload_opts(Some(&bp)), &[image_content(&[4, 5, 6, 7, 8])]),
            "LLM_REQUEST",
            "ev-bg-flush",
        );
        assert_eq!(
            pending_upload_bytes(&bp),
            5,
            "pendingUploadBytes before flush"
        );
        measure.store(true, Ordering::SeqCst);
        push_queue(&bp, row);

        let flusher = Arc::clone(&bp);
        let flush_done = tokio::spawn(async move { flusher.flush().await });
        started_rx.await.expect("upload started");
        bp.close().await;
        flush_done.await.unwrap();

        assert_eq!(
            pending_during_write.load(Ordering::SeqCst),
            0,
            "background flush: pendingUploadBytes during the write (released before appendBatch)"
        );
        assert_eq!(
            pending_upload_bytes(&bp),
            0,
            "pendingUploadBytes after Close"
        );
        assert!(
            logger.contains(INLINE_LOG),
            "background flush did not log {INLINE_LOG:?}"
        );
        assert!(
            !logger.contains(RETRY_LOG),
            "background flush logged {RETRY_LOG:?} for a non-deferrable flush"
        );
        let rows = collect_rows(&mut rx, |rows| !rows.is_empty()).await;
        let p = &content_parts_of(&rows[0])[0];
        assert!(
            part_is(p, "INLINE", UPLOAD_FAILED_TEXT),
            "background flush part = {p:?}, want INLINE and uploadFailedText"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn test_batch_processor_flush_run_end_failing_uploads_pause_later_batches() {
    const BATCHES: usize = 3;
    const UPLOADS: usize = 12;
    let server = FakeWriteServer::new();
    let (tx, mut rx) = requests();
    recording_server(&server, tx);
    let off = Arc::new(HookOffloader::with_hook(Arc::new(
        |ctx: Context, _req: GcsUploadRequest| {
            Box::pin(async move {
                wait_or_done(
                    &ctx,
                    Duration::from_millis(150),
                    Err(Error::msg("storage unavailable")),
                )
                .await
            })
        },
    )));
    let logger = Arc::new(RecordingLogger::default());
    let mut cfg = with_offloader(Config::default(), &off, &logger);
    cfg.batch_size = 1;
    cfg.batch_flush_intv = Duration::from_secs(3600);
    cfg.shutdown_timeout = Duration::from_secs(2);
    cfg.retry_config.max_retries = 0;
    cfg.gcs_max_concurrent_uploads = 1;
    let bp = processor(
        &Context::background(),
        Some(server.client().unwrap()),
        P_DEFAULT_STREAM,
        cfg,
    )
    .await;
    warm_up(&bp, &mut rx).await;

    // The rows are queued directly, so that no background flush takes flush_mu before
    // flush_run_end does.
    let opts = offload_opts(Some(&bp));
    for i in 0..BATCHES {
        let row = queued_row(&opts, &image_contents(UPLOADS));
        push_queue(&bp, tag(row, "LLM_REQUEST", &format!("ev-{i}")));
    }

    bp.flush_run_end().await;

    let later = off
        .uploads()
        .iter()
        .filter(|c| !c.path.contains("_ev-0_"))
        .count();
    assert_eq!(
        later, 0,
        "the offloader was called {later} times for the batches after the first, want 0: the failed uploads of the first batch pause the uploads"
    );
    assert!(
        upload_paused(&bp),
        "uploadCooldownUntil = 0 after the run-end flush, want the uploads paused"
    );
    assert!(
        logger.contains("upload skipped during timeout cooldown"),
        "no log line says that an upload was skipped during the pause: no batch fell back without calling the offloader"
    );
    let ds = bp.drop_stats();
    assert_eq!(
        ds.total_drops, 0,
        "DropStats after the run-end flush = {ds:?}, want no drops"
    );
    // Every batch is written, with all of its parts inline.
    let rows = collect_rows(&mut rx, |rows| rows.len() >= BATCHES).await;
    assert_eq!(rows.len(), BATCHES, "persisted rows");
    for row in &rows {
        let parts = content_parts_of(row);
        assert_eq!(parts.len(), UPLOADS, "content parts of a written row");
        for (i, part) in parts.iter().enumerate() {
            assert!(
                part_is(part, "INLINE", UPLOAD_FAILED_TEXT),
                "part {i} of a written row is {part:?}, want INLINE and {UPLOAD_FAILED_TEXT:?}"
            );
        }
    }
    bp.close().await;
}

#[tokio::test(start_paused = true)]
async fn test_batch_processor_exactly_once_delivery_not_found_after_lost_response_is_not_resent() {
    let ctx = Context::background();
    let srv = ScriptedServer::new(
        Vec::new(),
        |attempt, _req| match attempt {
            // Send succeeds; Recv fails ambiguously.
            1 => Err(status(Code::Unavailable, "lost connection")),
            // The server reports NotFound on the same stream.
            2 => Err(status(Code::NotFound, "stream expired or missing")),
            _ => Ok(AppendRowsResponse::default()),
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

    let _ = bp
        .write_batch(
            &ctx,
            vec![event_row("LLM_REQUEST"), event_row("LLM_RESPONSE")],
        )
        .await;

    let names = srv.append_stream_names();
    let first = names[0].clone();
    for (i, name) in names.iter().enumerate() {
        assert_eq!(
            *name,
            first,
            "attempt {} sent the batch to {name} after an attempt on {first} whose outcome was unknown; it may now be stored on both streams",
            i + 1
        );
    }
    assert_eq!(
        bp.drop_stats().append_failure_drops,
        2,
        "AppendFailureDrops (the batch's outcome is unknown, so it is dropped and counted)"
    );
    bp.close().await;
}

/// The `x-goog-request-params` values of every AppendRows stream the fake received (Go's
/// server stream interceptor).
fn append_request_params(server: &FakeWriteServer) -> Vec<String> {
    server
        .append_stream_metadata()
        .into_iter()
        .flat_map(|md| md.get("x-goog-request-params").cloned().unwrap_or_default())
        .collect()
}

// parity: adapted: Go's grpc.StreamInterceptor reading the incoming metadata is the fake's recorded AppendRows stream metadata
#[tokio::test(start_paused = true)]
async fn test_batch_processor_default_stream_sends_routing_header_without_offset() {
    let ctx = Context::background();
    let srv = ScriptedServer::new(Vec::new(), |_, _| Ok(AppendRowsResponse::default()), None);
    let config = Config {
        project_id: "test-proj".to_owned(),
        dataset_id: "test-ds".to_owned(),
        table_name: "agent_events".to_owned(),
        exactly_once_delivery: false,
        ..Config::default()
    };
    let default_stream = "projects/test-proj/datasets/test-ds/tables/agent_events/streams/_default";
    let bp = processor(
        &ctx,
        Some(srv.server.client().unwrap()),
        default_stream,
        config,
    )
    .await;

    bp.write_batch(&ctx, vec![event_row("DEFAULT_STREAM")])
        .await
        .unwrap_or_else(|e| panic!("writeBatch() error = {e}"));

    assert_eq!(
        append_request_params(&srv.server),
        vec![
            "write_stream=projects%2Ftest-proj%2Fdatasets%2Ftest-ds%2Ftables%2Fagent_events%2Fstreams%2F_default"
                .to_owned()
        ],
        "default stream x-goog-request-params mismatch"
    );
    assert_eq!(
        srv.append_offsets(),
        vec![-1],
        "default stream Offset, want [nil]"
    );
    bp.close().await;
}

// parity: adapted: Go's grpc.StreamInterceptor reading the incoming metadata is the fake's recorded AppendRows stream metadata
#[tokio::test(start_paused = true)]
async fn test_batch_processor_routing_header_unicode_table_name() {
    const DEFAULT_STREAM: &str =
        "projects/test-proj/datasets/test-ds/tables/événements/streams/_default";
    const COMMITTED_STREAM: &str =
        "projects/test-proj/datasets/test-ds/tables/événements/streams/committed-1";
    let cases = [
        (
            "DefaultStream",
            false,
            DEFAULT_STREAM,
            DEFAULT_STREAM,
            "write_stream=projects%2Ftest-proj%2Fdatasets%2Ftest-ds%2Ftables%2F%C3%A9v%C3%A9nements%2Fstreams%2F_default",
        ),
        (
            "ExactlyOnceDelivery",
            true,
            "",
            COMMITTED_STREAM,
            "write_stream=projects%2Ftest-proj%2Fdatasets%2Ftest-ds%2Ftables%2F%C3%A9v%C3%A9nements%2Fstreams%2Fcommitted-1",
        ),
    ];
    for (name, exactly_once, stream_name, want_stream, want_header) in cases {
        let ctx = Context::background();
        // CreateWriteStream returns a stream of the Unicode table, as BigQuery would.
        let srv = ScriptedServer::new(
            vec![COMMITTED_STREAM.to_owned()],
            |_, _| Ok(AppendRowsResponse::default()),
            None,
        );
        let config = Config {
            project_id: "test-proj".to_owned(),
            dataset_id: "test-ds".to_owned(),
            table_name: "événements".to_owned(),
            exactly_once_delivery: exactly_once,
            ..Config::default()
        };
        let bp = processor(
            &ctx,
            Some(srv.server.client().unwrap()),
            stream_name,
            config,
        )
        .await;

        bp.write_batch(&ctx, vec![event_row("UNICODE_TABLE")])
            .await
            .unwrap_or_else(|e| panic!("{name}: writeBatch() error = {e}"));
        assert_eq!(
            bp.drop_stats().append_failure_drops,
            0,
            "{name}: AppendFailureDrops"
        );
        // Only the header is encoded; the request names the stream as is.
        assert_eq!(
            srv.append_stream_names(),
            vec![want_stream.to_owned()],
            "{name}: AppendRows WriteStream mismatch"
        );
        assert_eq!(
            append_request_params(&srv.server),
            vec![want_header.to_owned()],
            "{name}: AppendRows x-goog-request-params mismatch"
        );
        bp.close().await;
    }
}

// parity: adapted: Go's grpc.UnaryInterceptor counting CreateWriteStream calls is the fake's recorded create calls
#[tokio::test(start_paused = true)]
async fn test_batch_processor_exactly_once_delivery_bounded_rotation_on_persistent_rejection() {
    let ctx = Context::background();
    let srv = ScriptedServer::new(
        Vec::new(),
        |_, _| Err(status(Code::InvalidArgument, "invalid argument rejection")),
        None,
    );
    let config = Config {
        retry_config: RetryConfig {
            max_retries: 1,
            initial_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(2),
            multiplier: 1.0,
        },
        ..exactly_once_test_config()
    };
    let bp = processor(&ctx, Some(srv.server.client().unwrap()), "", config).await;

    // Write 10 batches. Because InvalidArgument is not an ambiguous send error, none of the
    // batches should trigger stream rotation across batches.
    for i in 0..10 {
        let _ = bp
            .write_batch(&ctx, vec![event_row(&format!("BATCH_{i}"))])
            .await;
    }

    assert_eq!(
        srv.create_count(),
        1,
        "CreateWriteStream calls (only initial stream creation; persistent rejections must not rotate)"
    );
    bp.close().await;
}

#[tokio::test(start_paused = true)]
async fn test_batch_processor_exactly_once_delivery_retry_attempt_refund_on_rotation() {
    let ctx = Context::background();
    let srv = ScriptedServer::new(
        Vec::new(),
        |attempt, _| match attempt {
            // Stream 1 gets NotFound without a prior ambiguous send.
            1 => Err(status(Code::NotFound, "stream expired")),
            // Stream 2 (after rotation) succeeds.
            _ => Ok(AppendRowsResponse::default()),
        },
        None,
    );
    // MaxRetries 0 means no regular retries are allowed; the non-zero InitialDelay keeps
    // apply_defaults from replacing an all-zero RetryConfig with the default MaxRetries 3. Only
    // the rotation attempt refund allows sending on the newly created stream.
    let config = Config {
        retry_config: RetryConfig {
            max_retries: 0,
            initial_delay: Duration::from_millis(1),
            max_delay: Duration::ZERO,
            multiplier: 0.0,
        },
        ..exactly_once_test_config()
    };
    let bp = processor(&ctx, Some(srv.server.client().unwrap()), "", config).await;
    assert_eq!(
        bp.config.retry_config.max_retries, 0,
        "MaxRetries after applyDefaults"
    );

    bp.write_batch(&ctx, vec![event_row("EVENT")])
        .await
        .unwrap_or_else(|e| {
            panic!("writeBatch() with MaxRetries: 0 failed unexpectedly after rotation: {e}")
        });
    assert_eq!(
        srv.attempts(),
        2,
        "AppendRows attempts (one on each stream)"
    );
    bp.close().await;
}

// parity: adapted: gadk's trace id prefix `gadk-bq-logger-rust` and the crate version (ledgered divergence on STREAM_WRITER_TRACE_ID_PREFIX); Go's adkVersion() is CARGO_PKG_VERSION
#[test]
fn test_stream_writer_trace_id() {
    assert_eq!(
        stream_writer_trace_id_for_version(""),
        "gadk-bq-logger-rust",
        "streamWriterTraceIDForVersion(\"\")"
    );
    assert_eq!(
        stream_writer_trace_id_for_version("2.6.0"),
        "gadk-bq-logger-rust/2.6.0",
        "streamWriterTraceIDForVersion(\"2.6.0\")"
    );
    let got = stream_writer_trace_id();
    assert!(
        got.starts_with(STREAM_WRITER_TRACE_ID_PREFIX),
        "streamWriterTraceID() = {got:?}, want prefix {STREAM_WRITER_TRACE_ID_PREFIX:?}"
    );
    assert_eq!(
        got,
        stream_writer_trace_id_for_version(env!("CARGO_PKG_VERSION")),
        "streamWriterTraceID()"
    );
}

// parity: skipped TestBatchProcessor_AppendRowsRequestTraceID: deferred to 7b-3 (its second sub-test drives the plugin through setupTestPlugin, OnUserMessageCallback and AfterRunCallback)

#[tokio::test(start_paused = true)]
async fn test_stream_writer_permanent_status_fails_fast() {
    // A transport status on the first fresh stream may reflect a premature stream close
    // rather than a request verdict, so append allows one bounded fresh-stream retry (2
    // attempts) before failing fast. NotFound is permanent without exactly-once; with it the
    // stream is replaced once and the second NotFound stops rotation (also 2 attempts).
    // Transient statuses still use up the retries.
    let cases = [
        (Code::PermissionDenied, 2),
        (Code::InvalidArgument, 2),
        (Code::Unauthenticated, 2),
        (Code::FailedPrecondition, 2),
        (Code::Unimplemented, 2),
        (Code::NotFound, 2),
        (Code::Unavailable, 4),
    ];
    for exactly_once in [false, true] {
        for (code, want_attempts) in cases {
            let name = format!("{code:?}/exactlyOnce={exactly_once}");
            let srv =
                ScriptedServer::new(Vec::new(), move |_, _| Err(status(code, "scripted")), None);
            let bp = new_error_classification_processor(&srv.server, exactly_once).await;

            let err = bp
                .write_batch(&Context::background(), vec![event_row("X")])
                .await
                .expect_err("writeBatch() error = nil, want error");
            let got = status_code(&err);
            assert!(
                got == code || (exactly_once && code == Code::NotFound),
                "{name}: status.Code(writeBatch() error) = {got:?}, want {code:?} (error: {err})"
            );
            assert_eq!(
                srv.attempts(),
                want_attempts,
                "{name}: AppendRows attempts (error: {err})"
            );
            assert_eq!(
                bp.drop_stats().append_failure_drops,
                1,
                "{name}: AppendFailureDrops"
            );
            bp.close().await;
        }
    }
}

#[tokio::test(start_paused = true)]
async fn test_stream_writer_transient_in_band_error_is_retried() {
    for exactly_once in [false, true] {
        for code in [
            Code::Unavailable,
            Code::ResourceExhausted,
            Code::Internal,
            Code::DeadlineExceeded,
            Code::Aborted,
        ] {
            let name = format!("{code:?}/exactlyOnce={exactly_once}");
            let srv = ScriptedServer::new(
                Vec::new(),
                move |attempt, _| {
                    if attempt <= 2 {
                        Ok(in_band(code, "scripted"))
                    } else {
                        Ok(AppendRowsResponse::default())
                    }
                },
                None,
            );
            let bp = new_error_classification_processor(&srv.server, exactly_once).await;

            if let Err(e) = bp
                .write_batch(&Context::background(), vec![event_row("X"), event_row("Y")])
                .await
            {
                panic!("{name}: writeBatch() error = {e}, want nil after the retries");
            }
            assert_eq!(srv.attempts(), 3, "{name}: AppendRows attempts");
            assert_eq!(
                bp.drop_stats().append_failure_drops,
                0,
                "{name}: AppendFailureDrops"
            );
            if exactly_once {
                // Every resend of the batch carries the offset of the first send.
                assert_eq!(
                    srv.append_offsets(),
                    vec![0, 0, 0],
                    "{name}: append offsets"
                );
            }
            bp.close().await;
        }
    }
}

#[tokio::test(start_paused = true)]
async fn test_stream_writer_in_band_error_retries_are_bounded() {
    for (code, want_attempts) in [
        (Code::Unavailable, 4),
        (Code::ResourceExhausted, 4),
        (Code::PermissionDenied, 1),
        (Code::InvalidArgument, 1),
    ] {
        let srv = ScriptedServer::new(
            Vec::new(),
            move |_, _| Ok(in_band(code, "scripted message")),
            None,
        );
        let bp = new_error_classification_processor(&srv.server, false).await;

        let err = bp
            .write_batch(&Context::background(), vec![event_row("X")])
            .await;
        match &err {
            Err(e) if e.to_string().contains("scripted message") => {}
            _ => panic!("{code:?}: writeBatch() error = {err:?}, want BigQuery's message"),
        }
        assert_eq!(
            srv.attempts(),
            want_attempts,
            "{code:?}: AppendRows attempts"
        );
        assert_eq!(
            bp.drop_stats().append_failure_drops,
            1,
            "{code:?}: AppendFailureDrops"
        );
        bp.close().await;
    }
}
