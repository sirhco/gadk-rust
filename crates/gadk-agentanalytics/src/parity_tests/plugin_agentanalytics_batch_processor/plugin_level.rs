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

//! `batch_processor_test.go`: the three tests that 7b-2 deferred to 7b-3 because they drive
//! the plugin (`newTestPluginWithConfig`, `setupTestPlugin`) or build a row with
//! `buildLLMRequestContent` (`probeRowWithOptions`).

#![allow(clippy::field_reassign_with_default)] // Go's `cfg := DefaultConfig(); cfg.X = …` reads as written

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use gadk_genai::Content;
use pretty_assertions::assert_eq;

use super::support::{
    HookOffloader, ScriptedServer, collect_rows, dynamic_delay_server, event_row,
    exactly_once_test_config, image_content, pending_upload_bytes, processor, push_queue,
    queued_row, requests, reserve_with, status, upload_paused,
};
use crate::batch_processor::BatchProcessor;
use crate::bigquery::fake::RecordingLogger;
use crate::config::{Config, GcsContentOffloader, GcsUploadRequest, Logger, RetryConfig};
use crate::context::{Context, ContextError};
use crate::error::{Code, Error};
use crate::event_types::{
    EVENT_INVOCATION_COMPLETED, EVENT_LLM_REQUEST, EVENT_USER_MESSAGE_RECEIVED,
};
use crate::formatter::{PartsOptions, UPLOAD_FAILED_TEXT};
use crate::offload::MEDIA_OFFLOADED_TEXT;
use crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::{
    Mc, SpanPlugin, decode_arrow_rows, event_types, new_test_plugin_with_config, only_row_of_type,
    span_ids, until_event_type,
};
use crate::parity_tests::plugin_agentanalytics_gcs_offloader::add_content::probe_row_with_options;
use crate::parity_tests::plugin_agentanalytics_gcs_offloader::plugin_level::{
    content_parts_of, on_user_message,
};
use crate::row::{QueuedRow, RowExt, RowValue};
use crate::storagepb::{AppendRowsRequest, AppendRowsResponse};
use crate::testutil::fake_write_server::FakeWriteServer;
use crate::write::stream_writer::stream_writer_trace_id;

/// The `_default` stream Go's upload tests write to.
const P_DEFAULT_STREAM: &str = "projects/p/datasets/d/tables/t/_default";

/// Go `panickingLogger`: `Println` counts its calls and panics.
#[derive(Default)]
struct PanickingLogger {
    println_calls: AtomicI32,
}

impl Logger for PanickingLogger {
    fn printf(&self, _: fmt::Arguments<'_>) {}
    fn println(&self, _: fmt::Arguments<'_>) {
        self.println_calls.fetch_add(1, Ordering::SeqCst);
        panic!("logger failed");
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

#[tokio::test(start_paused = true)]
async fn test_batch_processor_queue_full_panicking_logger_does_not_deadlock_close() {
    let logger = Arc::new(PanickingLogger::default());
    let tp = new_test_plugin_with_config(|c| {
        c.queue_max_size = 1;
        c.batch_size = 100;
        c.batch_flush_intv = Duration::from_secs(3600);
        c.logger = Some(Arc::clone(&logger) as Arc<dyn Logger>);
    })
    .await;
    let mc = Mc::of(&tp.ctx);

    // The first message fills the queue. The second is dropped, and the logger panics while
    // reporting it.
    for _ in 0..2 {
        on_user_message(&tp, &mc, Content::from_text("hello", "user")).await;
    }
    assert_eq!(
        logger.println_calls.load(Ordering::SeqCst),
        1,
        "Logger.Println calls, want 1 for the dropped event"
    );

    match tokio::time::timeout(Duration::from_secs(5), tp.close()).await {
        Ok(result) => result.unwrap_or_else(|e| panic!("Close() error = {e}")),
        Err(_) => panic!("Close() did not return after the logger panicked on a dropped event"),
    }
}

#[tokio::test(start_paused = true)]
async fn test_batch_processor_flush_run_end_deferred_retry_does_not_pause_healthy_uploads() {
    const LONG_PREFIX: &str = "LONG-TEXT-PREFIX-";
    const RETRY_LOG: &str = "pending retry on next flush";
    let server = FakeWriteServer::new();
    let (tx, mut rx) = requests();
    let server_delay = Arc::new(AtomicI64::new(0));
    let in_retry = Arc::new(AtomicBool::new(false));
    let post_retry = Arc::new(AtomicBool::new(false));
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

    let off = {
        let (in_retry, post_retry) = (Arc::clone(&in_retry), Arc::clone(&post_retry));
        Arc::new(HookOffloader::with_hook(Arc::new(
            move |ctx: Context, _req: GcsUploadRequest| {
                let in_retry = in_retry.load(Ordering::SeqCst);
                let post_retry = post_retry.load(Ordering::SeqCst);
                Box::pin(async move {
                    let ctx_err =
                        |ctx: &Context| Error::Context(ctx.err().unwrap_or(ContextError::Canceled));
                    if in_retry {
                        ctx.done().await;
                        return Err(ctx_err(&ctx));
                    }
                    let d = if post_retry {
                        Duration::from_millis(20)
                    } else {
                        Duration::from_millis(1200)
                    };
                    tokio::select! {
                        () = tokio::time::sleep(d) => Ok(()),
                        () = ctx.done() => Err(ctx_err(&ctx)),
                    }
                })
            },
        )))
    };
    let logger = Arc::new(RecordingLogger::default());
    let mut cfg = Config::default();
    cfg.batch_flush_intv = Duration::from_secs(3600);
    cfg.shutdown_timeout = Duration::from_secs(4);
    cfg.retry_config.max_retries = 0;
    cfg.gcs_offloader = Some(Arc::clone(&off) as Arc<dyn GcsContentOffloader>);
    cfg.gcs_max_concurrent_uploads = 1;
    cfg.logger = Some(Arc::clone(&logger) as Arc<dyn Logger>);
    let bp = processor(
        &Context::background(),
        Some(server.client().unwrap()),
        P_DEFAULT_STREAM,
        cfg,
    )
    .await;
    let _ = cell.set(Arc::clone(&bp));

    let warm = Context::background().with_timeout(Duration::from_secs(5));
    bp.write_batch(&warm, vec![event_row(EVENT_USER_MESSAGE_RECEIVED)])
        .await
        .unwrap_or_else(|e| panic!("warm-up writeBatch: {e}"));
    warm.cancel();
    rx.recv().await.expect("warm-up request");
    server_delay.store(2_800_000_000, Ordering::SeqCst);

    let opts = PartsOptions {
        max_len: 1000,
        offload: true,
        inline_limit: 10,
        reserve_bytes: reserve_with(&bp),
        ..PartsOptions::default()
    };
    for i in 0..2_u8 {
        let row = queued_row(&opts, &[image_content(&[i + 1, 2, 3])]);
        push_queue(&bp, tag(row, EVENT_LLM_REQUEST, &format!("ev-{i}")));
    }
    let text_row = probe_row_with_options(
        PartsOptions {
            max_len: 50,
            offload: true,
            inline_limit: 10,
            reserve_bytes: reserve_with(&bp),
            ..PartsOptions::default()
        },
        vec![Content::from_text(
            format!("{LONG_PREFIX}{}", "x".repeat(200)),
            "user",
        )],
    );
    push_queue(&bp, tag(text_row, EVENT_LLM_REQUEST, "ev-2"));

    bp.flush_run_end().await;
    assert_eq!(
        bp.flush_mu.lock().await.deferred_batch.len(),
        3,
        "deferredBatch length after first flushRunEnd"
    );
    assert!(
        !upload_paused(&bp),
        "uploadCooldownUntil after first flushRunEnd is set, want 0"
    );
    assert_ne!(
        pending_upload_bytes(&bp),
        0,
        "pendingUploadBytes after first flushRunEnd = 0, want kept upload bytes"
    );
    let retry_logs_before = logger.to_string().matches(RETRY_LOG).count();
    assert_eq!(
        retry_logs_before, 2,
        "first flushRunEnd {RETRY_LOG:?} log count:\n{logger}"
    );

    in_retry.store(true, Ordering::SeqCst);
    server_delay.store(20_000_000, Ordering::SeqCst);
    let calls_before_retry = off.uploads().len();
    bp.flush_run_end().await;
    in_retry.store(false, Ordering::SeqCst);

    let ds = bp.drop_stats();
    assert_eq!(
        ds.total_drops, 0,
        "DropStats after retry flushRunEnd = {ds:?}, want no drops"
    );
    assert_eq!(
        pending_during_retry_write.load(Ordering::SeqCst),
        0,
        "pendingUploadBytes during retry write (released before tryAppendBatch)"
    );
    assert_eq!(
        bp.flush_mu.lock().await.deferred_batch.len(),
        0,
        "deferredBatch length after retry flushRunEnd"
    );
    assert_eq!(
        pending_upload_bytes(&bp),
        0,
        "pendingUploadBytes after retry flushRunEnd"
    );
    assert_eq!(
        off.uploads().len() - calls_before_retry,
        1,
        "retry flushRunEnd offloader calls (the first kept upload ran until the ~1s retry deadline)"
    );
    assert!(
        !upload_paused(&bp),
        "uploadCooldownUntil after retry flushRunEnd is set, want 0 (run-end retry deadline must not pause healthy uploads)"
    );
    assert_eq!(
        logger.to_string().matches(RETRY_LOG).count(),
        retry_logs_before,
        "{RETRY_LOG:?} log count after retry flushRunEnd (run-end retry cannot defer again):\n{logger}"
    );

    let rows = collect_rows(&mut rx, |rows| rows.len() >= 3).await;
    assert_eq!(rows.len(), 3, "persisted rows");
    let p0 = &content_parts_of(&rows[0])[0];
    assert!(
        p0.str("storage_mode") == Some("GCS_REFERENCE")
            && p0.str("text") == Some(MEDIA_OFFLOADED_TEXT),
        "row 0 storage_mode = {:?}, textMatches = {}, want GCS_REFERENCE and mediaOffloadedText",
        p0.get("storage_mode"),
        p0.str("text") == Some(MEDIA_OFFLOADED_TEXT)
    );
    let p1 = &content_parts_of(&rows[1])[0];
    assert!(
        p1.str("storage_mode") == Some("INLINE") && p1.str("text") == Some(UPLOAD_FAILED_TEXT),
        "row 1 storage_mode = {:?}, textMatches = {}, want INLINE and uploadFailedText",
        p1.get("storage_mode"),
        p1.str("text") == Some(UPLOAD_FAILED_TEXT)
    );
    let p2 = &content_parts_of(&rows[2])[0];
    assert_eq!(p2.str("storage_mode"), Some("INLINE"), "row 2 storage_mode");
    assert_eq!(
        rows[2].bool("is_truncated"),
        Some(true),
        "row 2 is_truncated"
    );
    let content = rows[2].str("content").unwrap_or_default();
    assert!(
        content.contains(LONG_PREFIX),
        "row 2 content len = {}, hasPrefix = false, want inline summary prefix restored",
        content.len()
    );

    // A subsequent background flush must upload to Cloud Storage rather than skipping under
    // a pause.
    post_retry.store(true, Ordering::SeqCst);
    server_delay.store(0, Ordering::SeqCst);
    let calls_before_bg = off.uploads().len();
    let healthy_row = queued_row(&opts, &[image_content(&[9, 8, 7])]);
    push_queue(&bp, tag(healthy_row, EVENT_LLM_REQUEST, "ev-after-retry"));
    bp.flush().await;

    assert_eq!(
        off.uploads().len() - calls_before_bg,
        1,
        "background flush offloader calls"
    );
    let after_rows = collect_rows(&mut rx, |rows| !rows.is_empty()).await;
    assert_eq!(
        content_parts_of(&after_rows[0])[0].str("storage_mode"),
        Some("GCS_REFERENCE"),
        "subsequent healthy flush row storage_mode"
    );
    bp.close().await;
}

// parity: adapted: Go's span context in mCtx.ctx → the callback bodies called with CallbackCtx::with_span (SpanPlugin); gadk's stream trace ID is stream_writer_trace_id() (ledgered STREAM_WRITER_TRACE_ID_PREFIX divergence)
#[tokio::test(start_paused = true)]
async fn test_batch_processor_append_rows_request_trace_id() {
    {
        let name = "attaches TraceId across initial writes, reconnect retries, and committed stream rotations";
        let ctx = Context::background();
        let attempts = Arc::new(AtomicI32::new(0));
        let got: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let srv = {
            let (attempts, got) = (Arc::clone(&attempts), Arc::clone(&got));
            ScriptedServer::new(
                Vec::new(),
                move |_, req: &AppendRowsRequest| {
                    got.lock()
                        .unwrap()
                        .push((req.trace_id.clone(), req.write_stream.clone()));
                    match attempts.fetch_add(1, Ordering::SeqCst) + 1 {
                        // Force stream rotation on first committed stream attempt.
                        1 => Err(status(Code::NotFound, "stream expired")),
                        // Succeed on rotated committed stream.
                        2 => Ok(AppendRowsResponse::default()),
                        // Force transient retry on second batch (not ambiguous:
                        // ResourceExhausted).
                        3 => Err(status(Code::ResourceExhausted, "rate limited")),
                        _ => Ok(AppendRowsResponse::default()),
                    }
                },
                None,
            )
        };
        let config = Config {
            project_id: "test-proj".into(),
            dataset_id: "test-ds".into(),
            table_name: "agent_events".into(),
            exactly_once_delivery: true,
            retry_config: RetryConfig {
                max_retries: 2,
                initial_delay: Duration::from_millis(1),
                max_delay: Duration::from_millis(5),
                multiplier: 1.0,
            },
            ..exactly_once_test_config()
        };
        let bp = processor(&ctx, Some(srv.server.client().unwrap()), "", config).await;

        bp.write_batch(&ctx, vec![event_row("BATCH_1")])
            .await
            .unwrap_or_else(|e| panic!("{name}: writeBatch(BATCH_1) error = {e}"));
        bp.write_batch(&ctx, vec![event_row("BATCH_2")])
            .await
            .unwrap_or_else(|e| panic!("{name}: writeBatch(BATCH_2) error = {e}"));

        let want_trace_id = stream_writer_trace_id();
        let got = got.lock().unwrap().clone();
        assert_eq!(got.len(), 4, "{name}: AppendRows requests");
        for (i, (id, stream)) in got.iter().enumerate() {
            assert_eq!(
                id, &want_trace_id,
                "{name}: request {i} (stream {stream}) TraceId"
            );
        }
        bp.close().await;
    }
    {
        let name = "preserves user trace_id and session_id in Arrow rows while setting AppendRowsRequest.TraceId";
        let sp = SpanPlugin::new(|_| {}).await;
        const USER_TRACE_ID: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
        const USER_SPAN_ID: &str = "00f067aa0ba902b7";
        const USER_SESSION_ID: &str = "user-custom-session-42";
        let mut m = sp.tp.ctx.clone();
        m.session_id = USER_SESSION_ID.into();
        let ctx = SpanPlugin::ctx(&m, span_ids(USER_TRACE_ID, USER_SPAN_ID));

        sp.ap
            .on_user_message(&ctx, &Content::from_text("hello", "user"));
        sp.after_run(&ctx).await;

        let mut got_reqs: Vec<AppendRowsRequest> = Vec::new();
        let mut all_rows = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !until_event_type(EVENT_INVOCATION_COMPLETED)(&all_rows) {
            let n = got_reqs.len();
            if tokio::time::timeout_at(deadline, sp.tp.server.wait_for_append_requests(n + 1))
                .await
                .is_err()
            {
                panic!(
                    "{name}: timed out waiting for {EVENT_INVOCATION_COMPLETED}; got {:?}",
                    event_types(&all_rows)
                );
            }
            for r in sp.tp.server.append_requests().into_iter().skip(n) {
                all_rows.extend(decode_arrow_rows(&r.request));
                got_reqs.push(r.request);
            }
        }

        let want_stream_trace_id = stream_writer_trace_id();
        for (i, req) in got_reqs.iter().enumerate() {
            assert_eq!(
                req.trace_id, want_stream_trace_id,
                "{name}: AppendRowsRequest[{i}].GetTraceId()"
            );
            assert!(
                req.trace_id != USER_TRACE_ID && req.trace_id != USER_SESSION_ID,
                "{name}: AppendRowsRequest[{i}].GetTraceId() = {:?}, must be distinct from user trace_id/session_id",
                req.trace_id
            );
        }
        let user_msg_row = only_row_of_type(&all_rows, EVENT_USER_MESSAGE_RECEIVED);
        assert_eq!(
            user_msg_row.str("trace_id"),
            Some(USER_TRACE_ID),
            "{name}: row trace_id"
        );
        assert_eq!(
            user_msg_row.str("session_id"),
            Some(USER_SESSION_ID),
            "{name}: row session_id"
        );
    }
}
