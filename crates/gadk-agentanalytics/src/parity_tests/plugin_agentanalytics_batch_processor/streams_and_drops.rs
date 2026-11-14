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

//! `batch_processor_test.go` ports, Go file order from `TestStreamWriter_SendError` through
//! `TestBatchProcessor_ExactlyOnceDelivery_RetiredStreamFinalizeBoundedByRotateTimeout`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::time::Duration;

use pretty_assertions::assert_eq;
use tokio::sync::Notify;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::support::{
    CommittedState, HookRecordingLogger, OnFinalize, ScriptedServer, append_failure_stats,
    as_logger, committed, committed_stream_server, event_row, exactly_once_test_config,
    inject_stream, no_retry_config, processor, push_queue, queue_len, status,
};
use crate::batch_processor::BatchProcessor;
use crate::bigquery::fake::RecordingLogger;
use crate::config::{Config, DropStats, Logger};
use crate::context::Context;
use crate::error::{Code, Error};
use crate::storagepb::{
    AppendRowsResponse, FinalizeWriteStreamRequest, FinalizeWriteStreamResponse, RowError,
    write_stream,
};
use crate::testutil::fake_write_server::{AppendRowsServerStream, FakeWriteServer, UnaryCall};
use crate::write::stream_writer::tests::{Fail, MockCounts, MockStream, in_band, tonic_status};

/// Go `status.FromError(err)`: the code of the first gRPC status in `err`'s tree.
fn status_of(err: &Error) -> Option<Code> {
    match err.find(&mut |e| matches!(e, Error::Status { .. })) {
        Some(Error::Status { code, .. }) => Some(*code),
        _ => None,
    }
}

/// Go `&mockStream{recvRes: &storagepb.AppendRowsResponse{}}`.
fn healthy_stream() -> MockStream {
    MockStream {
        recv_res: Some(AppendRowsResponse::default()),
        ..MockStream::default()
    }
}

/// A Go `onAppend` that may block: Go's `time.Sleep` in a handler is an async sleep here.
type AsyncOnAppend = Arc<
    dyn Fn(usize) -> gadk_platform::BoxFuture<'static, Result<AppendRowsResponse, tonic::Status>>
        + Send
        + Sync,
>;

/// Go `scriptedCommittedStreamServer.AppendRows` with an `onAppend` that may sleep: every request
/// is counted (across streams, as Go's `appendAttempts`), then `on_append(attempt)` answers it
/// or ends the stream with its status.
fn async_scripted(server: &FakeWriteServer, on_append: AsyncOnAppend) -> Arc<AtomicUsize> {
    let attempts = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&attempts);
    server.on_append_rows(Arc::new(move |mut stream: AppendRowsServerStream| {
        let on_append = Arc::clone(&on_append);
        let counter = Arc::clone(&counter);
        Box::pin(async move {
            while stream.recv().await?.is_some() {
                let attempt = counter.fetch_add(1, Ordering::SeqCst) + 1;
                let res = on_append(attempt).await?;
                stream.send(res).await?;
            }
            Ok(())
        })
    }));
    attempts
}

/// A Go `onFinalize` that, for `retired`, first runs `on_retired` (its future is the call's
/// outcome), and answers every other stream at once.
fn finalize_retired(
    retired: &'static str,
    on_retired: impl Fn() -> gadk_platform::BoxFuture<
        'static,
        Result<FinalizeWriteStreamResponse, tonic::Status>,
    > + Send
    + Sync
    + 'static,
) -> OnFinalize {
    let on_retired = Arc::new(on_retired);
    Arc::new(move |call: UnaryCall<FinalizeWriteStreamRequest>| {
        if call.request.name == retired {
            on_retired()
        } else {
            Box::pin(async { Ok(FinalizeWriteStreamResponse::default()) })
        }
    })
}

/// Go `bp.streamWriter.rotateTimeout = d`.
async fn set_rotate_timeout(bp: &BatchProcessor, d: Duration) {
    bp.stream_writer.state.lock().await.rotate_timeout = d;
}

/// Go's `t.Cleanup(func() { bp.streamWriter.cancelFinalizations(); bp.streamWriter.waitFinalizations() })`.
async fn cancel_and_wait_finalizations(bp: &BatchProcessor) {
    bp.stream_writer.cancel_finalizations();
    bp.stream_writer.wait_finalizations().await;
}

#[tokio::test(start_paused = true)]
async fn test_stream_writer_send_error() {
    let ctx = Context::background();
    struct Case {
        name: &'static str,
        send_err: Fail,
        recv_err: Option<Fail>,
        recv_seq: Vec<Result<AppendRowsResponse, Fail>>,
        want_has_status: bool,
        want_code: Code,
        want_err_msg: &'static str,
        want_recv_count: usize,
    }
    let ok = || Ok(AppendRowsResponse::default());
    let cases = vec![
        Case {
            name: "server-side termination reports the status from Recv",
            send_err: Fail::Eof,
            recv_err: Some(Fail::Status(
                Code::Unavailable,
                "the connection is draining",
            )),
            recv_seq: Vec::new(),
            want_has_status: true,
            want_code: Code::Unavailable,
            want_err_msg: "the connection is draining",
            want_recv_count: 1,
        },
        Case {
            name: "buffered responses are drained until the status surfaces",
            send_err: Fail::Eof,
            recv_seq: vec![ok(), ok(), ok()],
            recv_err: Some(Fail::Status(
                Code::ResourceExhausted,
                "Exceeds 'AppendRows throughput' quota",
            )),
            want_has_status: true,
            want_code: Code::ResourceExhausted,
            want_err_msg: "AppendRows throughput",
            want_recv_count: 4,
        },
        Case {
            name: "a termination without a status is drained exactly once",
            send_err: Fail::Eof,
            recv_err: Some(Fail::Eof),
            recv_seq: Vec::new(),
            want_has_status: false,
            want_code: Code::Ok,
            want_err_msg: "EOF",
            want_recv_count: 1,
        },
        Case {
            name: "a wrapped io.EOF still drains and keeps its context",
            send_err: Fail::WrappedEof("send aborted"),
            recv_err: Some(Fail::Eof),
            recv_seq: Vec::new(),
            want_has_status: false,
            want_code: Code::Ok,
            want_err_msg: "send aborted",
            want_recv_count: 1,
        },
        Case {
            name: "a wrapped io.EOF still yields to the recovered status",
            send_err: Fail::WrappedEof("send aborted"),
            recv_err: Some(Fail::Status(
                Code::ResourceExhausted,
                "Exceeds 'AppendRows throughput' quota",
            )),
            recv_seq: Vec::new(),
            want_has_status: true,
            want_code: Code::ResourceExhausted,
            want_err_msg: "AppendRows throughput",
            want_recv_count: 1,
        },
        Case {
            name: "client-side error is reported as is",
            send_err: Fail::Msg("send failed"),
            recv_err: None,
            recv_seq: Vec::new(),
            want_has_status: false,
            want_code: Code::Ok,
            want_err_msg: "send failed",
            want_recv_count: 0,
        },
    ];
    for tt in cases {
        let name = tt.name;
        // MaxRetries 0 prevents a reconnect on the nil client.
        let bp = processor(&ctx, None, "test_stream", no_retry_config()).await;
        let counts = Arc::new(MockCounts::default());
        inject_stream(
            &bp,
            MockStream {
                send_err: Some(tt.send_err),
                recv_err: tt.recv_err,
                recv_seq: tt.recv_seq,
                counts: Arc::clone(&counts),
                ..MockStream::default()
            },
        )
        .await;

        let err = bp
            .write_batch(&ctx, vec![event_row("send_error_test_event")])
            .await
            .expect_err("writeBatch: expected an error, got nil");
        let text = err.to_string();
        assert!(
            text.contains("max retries exceeded"),
            "{name}: error {text:?} lost the retry-exhaustion wrapper"
        );
        assert!(
            text.contains("failed to send row batch"),
            "{name}: error {text:?} lost the send-error wrapper"
        );
        assert_eq!(
            counts.recvs.load(Ordering::SeqCst),
            tt.want_recv_count,
            "{name}: Recv calls"
        );
        assert_eq!(counts.sends.load(Ordering::SeqCst), 1, "{name}: Send calls");
        assert!(
            bp.stream_writer.state.lock().await.stream.is_none(),
            "{name}: streamWriter.stream not reset"
        );
        let st = status_of(&err);
        assert_eq!(
            st.is_some(),
            tt.want_has_status,
            "{name}: gRPC status present (err: {text})"
        );
        if tt.want_has_status {
            assert_eq!(st, Some(tt.want_code), "{name}: status code (err: {text})");
        }
        assert!(
            text.contains(tt.want_err_msg),
            "{name}: error {text:?} does not contain {:?}",
            tt.want_err_msg
        );
    }
}

#[tokio::test(start_paused = true)]
async fn test_stream_writer_retry_opens_a_fresh_stream() {
    let ctx = Context::background();
    let server = FakeWriteServer::new();
    let mut config = Config::default();
    config.retry_config.max_retries = 1;
    let bp = processor(
        &ctx,
        Some(server.client().unwrap()),
        "projects/p/datasets/d/tables/t/_default",
        config,
    )
    .await;

    let dead = Arc::new(MockCounts::default());
    inject_stream(
        &bp,
        MockStream {
            send_err: Some(Fail::Eof),
            recv_err: Some(Fail::Eof),
            counts: Arc::clone(&dead),
            ..MockStream::default()
        },
    )
    .await;

    if let Err(e) = bp.write_batch(&ctx, vec![event_row("retry_probe")]).await {
        panic!("attempt 1 did not open a fresh stream: {e}");
    }
    assert_eq!(
        dead.sends.load(Ordering::SeqCst),
        1,
        "the aborted stream was reused"
    );
    bp.close().await;
}

#[tokio::test(start_paused = true)]
async fn test_batch_processor_close_drains_and_rejects_new_appends() {
    let ctx = Context::background();
    let config = Config {
        batch_size: 100,
        batch_flush_intv: Duration::from_secs(3600),
        ..no_retry_config()
    };
    let bp = processor(&ctx, None, "test_stream", config).await;
    let counts = Arc::new(MockCounts::default());
    inject_stream(
        &bp,
        MockStream {
            counts: Arc::clone(&counts),
            ..healthy_stream()
        },
    )
    .await;

    bp.start();
    bp.append(event_row("before_close"));
    bp.close().await;

    // Appends after Close must be ignored and counted in ShutdownDrops.
    bp.append(event_row("after_close"));
    assert_eq!(queue_len(&bp), 0, "queue len after Close");
    assert_eq!(counts.sends.load(Ordering::SeqCst), 1, "Send calls");
    assert_eq!(
        bp.drop_stats(),
        DropStats {
            shutdown_drops: 1,
            total_drops: 1,
            ..DropStats::default()
        },
        "DropStats() after Close"
    );
}

// parity: adapted: Go's nil *BatchProcessor DropStats() check is the zero-counter DropStats here
#[tokio::test(start_paused = true)]
async fn test_batch_processor_drop_stats() {
    let ctx = Context::background();

    assert_eq!(
        crate::batch_processor::DropCounters::default().drop_stats(),
        DropStats::default(),
        "zero counters DropStats()"
    );

    let config = Config {
        batch_size: 10,
        queue_max_size: 1,
        ..no_retry_config()
    };
    let bp = processor(&ctx, None, "test_stream", config).await;

    // 1. Trigger 2 queue-full drops (queue capacity is 1).
    bp.append(event_row("ok_1"));
    bp.append(event_row("drop_1"));
    bp.append(event_row("drop_2"));

    // 2. Trigger append failure drops (batch of 3 rows failing on Send).
    inject_stream(
        &bp,
        MockStream {
            send_err: Some(Fail::Msg("send failure")),
            ..MockStream::default()
        },
    )
    .await;
    let _ = bp
        .write_batch(
            &ctx,
            vec![
                event_row("fail_1"),
                event_row("fail_2"),
                event_row("fail_3"),
            ],
        )
        .await;

    // 3. Trigger append failure drops from BigQuery RowErrors (batch of 2 rows).
    inject_stream(
        &bp,
        MockStream {
            recv_res: Some(AppendRowsResponse {
                row_errors: vec![RowError {
                    index: 0,
                    message: "bad row".to_owned(),
                    ..RowError::default()
                }],
                ..AppendRowsResponse::default()
            }),
            ..MockStream::default()
        },
    )
    .await;
    let _ = bp
        .write_batch(&ctx, vec![event_row("row_err_1"), event_row("row_err_2")])
        .await;

    // 4. Trigger append failure drops from an in-band AppendRowsResponse.Error (4 rows).
    inject_stream(
        &bp,
        MockStream {
            recv_res: Some(in_band(Code::InvalidArgument, "in-band batch error")),
            ..MockStream::default()
        },
    )
    .await;
    let _ = bp
        .write_batch(
            &ctx,
            vec![
                event_row("inband_err_1"),
                event_row("inband_err_2"),
                event_row("inband_err_3"),
                event_row("inband_err_4"),
            ],
        )
        .await;

    // 5. A formatter failure drop (not in TotalDrops: the row is still written).
    bp.record_formatter_failure_drop();

    // 6. A shutdown drop after Close (a healthy stream lets ok_1 flush cleanly on Close).
    inject_stream(&bp, healthy_stream()).await;
    bp.close().await;
    bp.append(event_row("after_close"));

    assert_eq!(
        bp.drop_stats(),
        DropStats {
            queue_full_drops: 2,
            append_failure_drops: 9,
            formatter_failure_drops: 1,
            shutdown_drops: 1,
            total_drops: 12,
        },
        "DropStats()"
    );
}

#[tokio::test(start_paused = true)]
async fn test_batch_processor_exactly_once_delivery_committed_stream_and_offsets() {
    let ctx = Context::background();
    let created = "projects/test-proj/datasets/test-ds/tables/agent_events/streams/committed-1";
    let server = FakeWriteServer::new();
    let srv = committed_stream_server(
        &server,
        CommittedState {
            created_stream_name: created.to_owned(),
            // Fail the first attempt of batch 2 so the retry reuses the same offset.
            fail_append_attempt_n: 2,
            ..CommittedState::default()
        },
    );
    let client = server.client().unwrap();
    let bp = processor(&ctx, Some(client), "", exactly_once_test_config()).await;

    // CreateWriteStream was called with the COMMITTED type.
    let creates = server.create_calls();
    assert_eq!(creates.len(), 1, "CreateWriteStream calls");
    assert_eq!(
        creates[0].request.parent,
        "projects/test-proj/datasets/test-ds/tables/agent_events"
    );
    assert_eq!(
        creates[0].request.write_stream.as_ref().map(|w| w.r#type),
        Some(write_stream::Type::Committed as i32)
    );

    // Batch 1 (2 rows) -> offset 0, advances the offset to 2.
    bp.write_batch(
        &ctx,
        vec![event_row("LLM_REQUEST"), event_row("LLM_RESPONSE")],
    )
    .await
    .unwrap_or_else(|e| panic!("writeBatch(batch 1) error = {e}"));
    // Batch 2 (3 rows) -> attempt 0 fails at offset 2, the retry succeeds at offset 2,
    // advances the offset to 5.
    bp.write_batch(
        &ctx,
        vec![
            event_row("TOOL_STARTING"),
            event_row("TOOL_COMPLETED"),
            event_row("AGENT_COMPLETED"),
        ],
    )
    .await
    .unwrap_or_else(|e| panic!("writeBatch(batch 2) error = {e}"));
    // Batch 3 (1 row) -> offset 5.
    bp.write_batch(&ctx, vec![event_row("INVOCATION_COMPLETED")])
        .await
        .unwrap_or_else(|e| panic!("writeBatch(batch 3) error = {e}"));

    bp.close().await;

    let s = srv.lock().unwrap();
    assert_eq!(s.append_offsets, vec![0, 2, 2, 5], "AppendRows offsets");
    for (i, has_offset) in s.append_has_offsets.iter().enumerate() {
        assert!(has_offset, "AppendRows request {i} had nil Offset");
        assert_eq!(
            s.append_stream_names[i], created,
            "AppendRows request {i} stream"
        );
    }
    let finalizes = server.finalize_calls();
    assert_eq!(finalizes.len(), 1, "FinalizeWriteStream calls");
    assert_eq!(
        finalizes[0].request.name, created,
        "FinalizeWriteStream name"
    );
}

#[tokio::test(start_paused = true)]
async fn test_batch_processor_exactly_once_delivery_create_write_stream_failure() {
    let ctx = Context::background();
    let server = FakeWriteServer::new();
    committed_stream_server(
        &server,
        CommittedState {
            create_err: Some((Code::PermissionDenied, "cannot create stream".to_owned())),
            ..CommittedState::default()
        },
    );
    let client = server.client().unwrap();
    let config = Config {
        project_id: "test-proj".to_owned(),
        dataset_id: "test-ds".to_owned(),
        exactly_once_delivery: true,
        ..Config::default()
    };
    assert!(
        BatchProcessor::new(&ctx, Some(client.clone()), "", config.clone())
            .await
            .is_err(),
        "NewBatchProcessor() error = nil, want CreateWriteStream error"
    );

    // Honors caller ctx cancellation during the initial CreateWriteStream.
    let canceled = ctx.with_cancel();
    canceled.cancel();
    assert!(
        BatchProcessor::new(&canceled, Some(client), "", config)
            .await
            .is_err(),
        "NewBatchProcessor() with canceled ctx error = nil, want context canceled error"
    );
}

#[tokio::test(start_paused = true)]
async fn test_batch_processor_exactly_once_delivery_ambiguous_send_then_already_exists_advances_offset()
 {
    for via in ["response", "grpc"] {
        let ctx = Context::background();
        let srv = ScriptedServer::new(
            Vec::new(),
            move |attempt, _| match attempt {
                // The server commits batch 1 (2 rows) at offset 0, then the connection drops
                // before the client's Recv completes.
                1 => Err(tonic_status(
                    Code::Unavailable,
                    "connection reset after commit",
                )),
                // The retry of batch 1 at offset 0 gets AlreadyExists, confirming the commit.
                2 if via == "grpc" => {
                    Err(tonic_status(Code::AlreadyExists, "offset 0 already exists"))
                }
                2 => Ok(in_band(Code::AlreadyExists, "offset 0 already exists")),
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

        // Batch 1: attempt 1 fails after the send, attempt 2 gets AlreadyExists -> success,
        // the offset advances to 2.
        if let Err(e) = bp
            .write_batch(
                &ctx,
                vec![event_row("LLM_REQUEST"), event_row("LLM_RESPONSE")],
            )
            .await
        {
            panic!("{via}: writeBatch(batch 1) error = {e}, want nil");
        }
        // Batch 2 (1 row) must use offset 2 on the same stream.
        if let Err(e) = bp
            .write_batch(&ctx, vec![event_row("INVOCATION_COMPLETED")])
            .await
        {
            panic!("{via}: writeBatch(batch 2) error = {e}, want nil");
        }
        assert_eq!(bp.drop_stats(), DropStats::default(), "{via}: DropStats()");
        assert_eq!(
            srv.create_count(),
            1,
            "{via}: CreateWriteStream calls (no stream rotation expected)"
        );
        assert_eq!(
            srv.append_offsets(),
            vec![0, 0, 2],
            "{via}: AppendRows offsets"
        );
        bp.close().await;
    }
}

#[tokio::test(start_paused = true)]
async fn test_batch_processor_exactly_once_delivery_unrecoverable_offset_conflict_rotates_stream() {
    let ctx = Context::background();
    let srv = ScriptedServer::new(
        Vec::new(),
        |attempt, _| match attempt {
            // Batch 2 (3 rows) at offset 2 on committed-1 hits an unexpected AlreadyExists
            // without a prior ambiguous send in this call.
            2 => Ok(in_band(
                Code::AlreadyExists,
                "offset 2 already exists from prior timed-out batch",
            )),
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

    // Batch 1 (2 rows) -> committed-1 @ offset 0.
    bp.write_batch(
        &ctx,
        vec![event_row("LLM_REQUEST"), event_row("LLM_RESPONSE")],
    )
    .await
    .unwrap_or_else(|e| panic!("writeBatch(batch 1) error = {e}"));
    // Batch 2 (3 rows) -> committed-1 @ offset 2 gets the unrecoverable AlreadyExists, rotates
    // to committed-2 @ offset 0, succeeds and advances the offset to 3.
    bp.write_batch(
        &ctx,
        vec![
            event_row("TOOL_STARTING"),
            event_row("TOOL_COMPLETED"),
            event_row("AGENT_COMPLETED"),
        ],
    )
    .await
    .unwrap_or_else(|e| panic!("writeBatch(batch 2) error = {e}"));
    // Batch 3 (1 row) -> committed-2 @ offset 3.
    bp.write_batch(&ctx, vec![event_row("INVOCATION_COMPLETED")])
        .await
        .unwrap_or_else(|e| panic!("writeBatch(batch 3) error = {e}"));

    bp.close().await;

    assert_eq!(
        srv.create_count(),
        2,
        "CreateWriteStream calls (initial + rotated)"
    );
    assert_eq!(srv.append_offsets(), vec![0, 2, 0, 3], "AppendRows offsets");
    assert_eq!(
        srv.append_stream_names(),
        vec![committed(1), committed(1), committed(2), committed(2)],
        "AppendRows stream names"
    );
    assert_eq!(
        srv.finalized(),
        vec![committed(1), committed(2)],
        "FinalizeWriteStream names"
    );
}

/// Go `exactlyOnceTestConfig()` with `MaxRetries: 1`.
fn one_retry_exactly_once_config() -> Config {
    let mut config = exactly_once_test_config();
    config.retry_config.max_retries = 1;
    config
}

#[tokio::test(start_paused = true)]
async fn test_batch_processor_exactly_once_delivery_terminal_ambiguous_send_rotates_stream_on_next_batch()
 {
    let ctx = Context::background();

    // Exhausted ambiguous retries rotate the stream before the next batch.
    {
        let srv = ScriptedServer::new(
            Vec::new(),
            |attempt, _| match attempt {
                // Batch A (2 rows) at offset 0 on committed-1 fails with Unavailable on both
                // attempts (MaxRetries=1).
                1 | 2 => Err(tonic_status(Code::Unavailable, "ambiguous connection drop")),
                // Batch B (1 row) must rotate to committed-2 at offset 0.
                _ => Ok(AppendRowsResponse::default()),
            },
            None,
        );
        let bp = processor(
            &ctx,
            Some(srv.server.client().unwrap()),
            "",
            one_retry_exactly_once_config(),
        )
        .await;
        assert!(
            bp.write_batch(
                &ctx,
                vec![event_row("LLM_REQUEST"), event_row("LLM_RESPONSE")]
            )
            .await
            .is_err(),
            "writeBatch(Batch A) error = nil, want retry exhaustion error"
        );
        bp.write_batch(&ctx, vec![event_row("INVOCATION_COMPLETED")])
            .await
            .unwrap_or_else(|e| panic!("writeBatch(Batch B) error = {e}, want nil"));
        bp.close().await;

        assert_eq!(bp.drop_stats(), append_failure_stats(2), "DropStats()");
        assert_eq!(
            srv.append_stream_names(),
            vec![committed(1), committed(1), committed(2)],
            "AppendRows stream names"
        );
        assert_eq!(srv.append_offsets(), vec![0, 0, 0], "AppendRows offsets");
        assert_eq!(
            srv.finalized(),
            vec![committed(1), committed(2)],
            "FinalizeWriteStream names"
        );
    }

    // An ambiguous send followed by a rejected response rotates the stream before the next
    // batch.
    {
        let srv = ScriptedServer::new(
            Vec::new(),
            |attempt, _| match attempt {
                1 => Err(tonic_status(Code::Unavailable, "ambiguous drop")),
                2 => Ok(in_band(Code::InvalidArgument, "bad batch")),
                _ => Ok(AppendRowsResponse::default()),
            },
            None,
        );
        let bp = processor(
            &ctx,
            Some(srv.server.client().unwrap()),
            "",
            one_retry_exactly_once_config(),
        )
        .await;
        assert!(
            bp.write_batch(&ctx, vec![event_row("LLM_REQUEST")])
                .await
                .is_err(),
            "writeBatch(Batch A) error = nil, want InvalidArgument error"
        );
        bp.write_batch(&ctx, vec![event_row("LLM_RESPONSE")])
            .await
            .unwrap_or_else(|e| panic!("writeBatch(Batch B) error = {e}, want nil"));
        assert_eq!(
            srv.append_stream_names(),
            vec![committed(1), committed(1), committed(2)],
            "AppendRows stream names"
        );
        bp.close().await;
    }

    // An ambiguous send followed by row errors rotates the stream before the next batch.
    {
        let srv = ScriptedServer::new(
            Vec::new(),
            |attempt, _| match attempt {
                1 => Err(tonic_status(Code::Unavailable, "unknown outcome")),
                2 => Ok(AppendRowsResponse {
                    row_errors: vec![RowError {
                        index: 0,
                        ..RowError::default()
                    }],
                    ..AppendRowsResponse::default()
                }),
                _ => Ok(AppendRowsResponse::default()),
            },
            None,
        );
        let bp = processor(
            &ctx,
            Some(srv.server.client().unwrap()),
            "",
            one_retry_exactly_once_config(),
        )
        .await;
        assert!(
            bp.write_batch(&ctx, vec![event_row("LLM_REQUEST")])
                .await
                .is_err(),
            "writeBatch(Batch A) error = nil, want row error"
        );
        bp.write_batch(&ctx, vec![event_row("LLM_RESPONSE")])
            .await
            .unwrap_or_else(|e| panic!("writeBatch(Batch B) error = {e}, want nil"));
        assert_eq!(
            srv.append_stream_names(),
            vec![committed(1), committed(1), committed(2)],
            "AppendRows stream names"
        );
        assert_eq!(srv.append_offsets(), vec![0, 0, 0], "AppendRows offsets");
        bp.close().await;
    }
}

/// The exactly-once test config of the table the scripted server names, with Go
/// `DefaultConfig()`'s retries.
fn exactly_once_default_retries() -> Config {
    Config {
        project_id: "test-proj".to_owned(),
        dataset_id: "test-ds".to_owned(),
        table_name: "agent_events".to_owned(),
        exactly_once_delivery: true,
        ..Config::default()
    }
}

// parity: adapted: Go swaps the server's onAppend/onFinalize between sub-steps; here a mode switch read by one async AppendRows hook (Go's handler sleeps are async sleeps) and a replaced finalize hook that never answers (Go's <-ctx.Done())
#[tokio::test(start_paused = true)]
async fn test_batch_processor_exactly_once_delivery_finalize_preserves_caller_context_metadata() {
    let base_ctx =
        Context::background().append_outgoing_metadata(&[("x-test-caller", "preserve-me")]);
    let caller_ctx = base_ctx.with_cancel();

    let srv = ScriptedServer::new(Vec::new(), |_, _| Ok(AppendRowsResponse::default()), None);
    // 0: answer at once; 1: answer after 40ms; 2: fail with DeadlineExceeded after 80ms.
    let mode = Arc::new(AtomicU8::new(0));
    let append_mode = Arc::clone(&mode);
    async_scripted(
        &srv.server,
        Arc::new(move |_| {
            let mode = append_mode.load(Ordering::SeqCst);
            Box::pin(async move {
                match mode {
                    0 => Ok(AppendRowsResponse::default()),
                    1 => {
                        tokio::time::sleep(Duration::from_millis(40)).await;
                        Ok(AppendRowsResponse::default())
                    }
                    _ => {
                        tokio::time::sleep(Duration::from_millis(80)).await;
                        Err(status(Code::DeadlineExceeded, "drain timed out"))
                    }
                }
            })
        }),
    );
    let client = srv.server.client().unwrap();

    let bp = processor(
        &caller_ctx,
        Some(client.clone()),
        "",
        exactly_once_default_retries(),
    )
    .await;
    caller_ctx.cancel();
    bp.close().await;

    let md = srv.finalize_metadata();
    assert_eq!(md.len(), 1, "FinalizeWriteStream calls");
    assert_eq!(
        md[0].get("x-test-caller"),
        Some(&vec!["preserve-me".to_owned()]),
        "FinalizeWriteStream incoming metadata x-test-caller"
    );

    // A stalled finalizer honors a small ShutdownTimeout and the remaining drain budget.
    srv.server.on_finalize_write_stream(Arc::new(|_| {
        Box::pin(std::future::pending::<
            Result<FinalizeWriteStreamResponse, tonic::Status>,
        >())
    }));

    let short_cfg = Config {
        shutdown_timeout: Duration::from_millis(5),
        ..exactly_once_default_retries()
    };
    let bp_short = processor(&Context::background(), Some(client.clone()), "", short_cfg).await;
    let start = Instant::now();
    bp_short.close().await;
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_millis(50),
        "Close() with stalled finalizer and ShutdownTimeout=5ms took {elapsed:?}, want < 50ms"
    );

    mode.store(1, Ordering::SeqCst);
    let rem_cfg = Config {
        batch_flush_intv: Duration::from_secs(3600),
        shutdown_timeout: Duration::from_millis(60),
        ..exactly_once_default_retries()
    };
    let bp_rem = processor(&Context::background(), Some(client.clone()), "", rem_cfg).await;
    bp_rem.start();
    bp_rem.append(event_row("DRAIN_BEFORE_FINALIZE"));
    let rem_start = Instant::now();
    bp_rem.close().await;
    let rem_elapsed = rem_start.elapsed();
    assert!(
        rem_elapsed < Duration::from_millis(95),
        "Close() with 40ms drain + stalled finalizer and ShutdownTimeout=60ms took {rem_elapsed:?}, want < 95ms (remaining budget only)"
    );

    let finalize_count_before = srv.server.finalize_calls().len();
    mode.store(2, Ordering::SeqCst);
    let exhaust_log = Arc::new(RecordingLogger::default());
    let mut exhaust_cfg = Config {
        batch_flush_intv: Duration::from_secs(3600),
        shutdown_timeout: Duration::from_millis(20),
        logger: as_logger(&exhaust_log),
        ..exactly_once_default_retries()
    };
    exhaust_cfg.retry_config.max_retries = 0;
    let bp_exhaust = processor(&Context::background(), Some(client), "", exhaust_cfg).await;
    bp_exhaust.start();
    bp_exhaust.append(event_row("EXHAUST_DRAIN_BUDGET"));
    bp_exhaust.close().await;

    assert_eq!(
        srv.server.finalize_calls().len(),
        finalize_count_before,
        "FinalizeWriteStream calls after exhausted drain (must skip FinalizeWriteStream when writeLeft <= 0)"
    );
    assert!(
        exhaust_log.contains("skipping FinalizeWriteStream"),
        "expected skipping FinalizeWriteStream log, got {:?}",
        exhaust_log.to_string()
    );
}

// parity: adapted: Go's finalizer hangs on <-ctx.Done(); here the fake's handler never answers and the client's context ends the call
#[tokio::test(start_paused = true)]
async fn test_batch_processor_exactly_once_delivery_close_bounded_when_retired_stream_finalize_hangs()
 {
    const RETIRED: &str =
        "projects/test-proj/datasets/test-ds/tables/agent_events/streams/committed-1";
    const ACTIVE: &str =
        "projects/test-proj/datasets/test-ds/tables/agent_events/streams/committed-2";

    for (name, shutdown_timeout, queue_drain_row, want_skipped_active) in [
        (
            "remaining write budget",
            Duration::from_millis(100),
            false,
            false,
        ),
        (
            "drain exhausts write budget",
            Duration::from_millis(80),
            true,
            true,
        ),
    ] {
        let ctx = Context::background();
        let finalize_started = Arc::new(Notify::new());
        let started = Arc::clone(&finalize_started);
        let srv = ScriptedServer::new(
            Vec::new(),
            |_, _| Ok(AppendRowsResponse::default()),
            Some(finalize_retired(RETIRED, move || {
                started.notify_one();
                // FinalizeWriteStream hangs until Close cancels it.
                Box::pin(std::future::pending())
            })),
        );
        async_scripted(
            &srv.server,
            Arc::new(|attempt| {
                Box::pin(async move {
                    match attempt {
                        // An offset error replaces the stream; the first stream is retired.
                        1 => Err(status(Code::OutOfRange, "offset beyond end")),
                        2 => Ok(AppendRowsResponse::default()),
                        _ => {
                            // Exceed ShutdownTimeout while draining queued rows in Close.
                            tokio::time::sleep(Duration::from_millis(120)).await;
                            Ok(AppendRowsResponse::default())
                        }
                    }
                })
            }),
        );

        let base_log = Arc::new(RecordingLogger::default());
        let logger = HookRecordingLogger {
            inner: Arc::clone(&base_log),
            before_record: Box::new(|msg: &str| {
                if msg.contains(&format!("failed to finalize write stream {RETIRED}")) {
                    std::thread::sleep(Duration::from_millis(35));
                }
            }),
        };
        let mut config = exactly_once_test_config();
        config.batch_flush_intv = Duration::from_secs(3600);
        config.shutdown_timeout = shutdown_timeout;
        config.retry_config.max_retries = 0;
        config.logger = Some(Arc::new(logger) as Arc<dyn Logger>);
        let bp = processor(&ctx, Some(srv.server.client().unwrap()), "", config).await;
        set_rotate_timeout(&bp, Duration::from_millis(2500)).await;

        bp.write_batch(&ctx, vec![event_row("LLM_REQUEST")])
            .await
            .unwrap_or_else(|e| panic!("{name}: writeBatch() error = {e}"));
        finalize_started.notified().await;
        if queue_drain_row {
            push_queue(&bp, event_row("DRAIN_EXHAUSTS_BUDGET"));
        }

        let start = Instant::now();
        bp.close().await;
        let d = start.elapsed();
        assert!(
            base_log.contains(&format!("failed to finalize write stream {RETIRED}")),
            "{name}: Close() returned before the retired stream's FinalizeWriteStream task finished; log:\n{base_log}"
        );
        // Close documents a bound of about two and a half times ShutdownTimeout.
        let limit = shutdown_timeout * 5 / 2;
        assert!(
            d <= limit + Duration::from_millis(500),
            "{name}: Close() took {d:?} with ShutdownTimeout {shutdown_timeout:?}, want at most about {limit:?}"
        );
        assert_eq!(
            base_log.contains(&format!("skipping FinalizeWriteStream for {ACTIVE}")),
            want_skipped_active,
            "{name}: log contains skipping FinalizeWriteStream for the active stream; log:\n{base_log}"
        );
        cancel_and_wait_finalizations(&bp).await;
    }
}

#[tokio::test(start_paused = true)]
async fn test_batch_processor_exactly_once_delivery_retired_stream_finalize_keeps_metadata_and_logs_errors()
 {
    const RETIRED: &str =
        "projects/test-proj/datasets/test-ds/tables/agent_events/streams/committed-1";
    let finalize_started = Arc::new(Notify::new());
    let close_entered = CancellationToken::new();
    let started = Arc::clone(&finalize_started);
    let entered = close_entered.clone();
    let srv = ScriptedServer::new(
        Vec::new(),
        |attempt, _| {
            if attempt == 1 {
                // An offset error replaces the stream; the first stream is retired.
                return Err(tonic_status(Code::OutOfRange, "offset beyond end"));
            }
            Ok(AppendRowsResponse::default())
        },
        Some(finalize_retired(RETIRED, move || {
            started.notify_one();
            let entered = entered.clone();
            Box::pin(async move {
                entered.cancelled().await;
                tokio::time::sleep(Duration::from_millis(35)).await;
                Err(status(Code::PermissionDenied, "finalize denied"))
            })
        })),
    );

    let caller_ctx = Context::background()
        .append_outgoing_metadata(&[("x-test-caller", "preserve-me")])
        .with_cancel();
    let logger = Arc::new(RecordingLogger::default());
    let config = Config {
        logger: as_logger(&logger),
        ..exactly_once_test_config()
    };
    let bp = processor(&caller_ctx, Some(srv.server.client().unwrap()), "", config).await;
    let stop = bp.stop.clone();
    tokio::spawn(async move {
        stop.cancelled().await;
        close_entered.cancel();
    });
    // The processor ignores the cancellation of its constructor's context, but keeps the
    // context's values.
    caller_ctx.cancel();
    bp.write_batch(&Context::background(), vec![event_row("LLM_REQUEST")])
        .await
        .unwrap_or_else(|e| panic!("writeBatch() error = {e}"));
    finalize_started.notified().await;
    bp.close().await;
    let want = format!("failed to finalize write stream {RETIRED}");
    assert!(
        logger.contains(&want),
        "Close() returned before the retired stream's FinalizeWriteStream logged {want:?}; got:\n{logger}"
    );

    let calls = srv.server.finalize_calls();
    let names: Vec<String> = calls.iter().map(|c| c.request.name.clone()).collect();
    let retired_md = calls
        .iter()
        .find(|c| c.request.name == RETIRED)
        .map(|c| c.metadata.clone())
        .unwrap_or_else(|| {
            panic!("FinalizeWriteStream names = {names:?}, want the retired stream {RETIRED} among them")
        });
    assert_eq!(
        retired_md.get("x-test-caller"),
        Some(&vec!["preserve-me".to_owned()]),
        "retired stream's FinalizeWriteStream incoming metadata x-test-caller"
    );
    cancel_and_wait_finalizations(&bp).await;
}

// parity: adapted: Go's finalizer hangs on <-ctx.Done(); here the fake's handler never answers and the client's context ends the call
#[tokio::test(start_paused = true)]
async fn test_batch_processor_exactly_once_delivery_retired_stream_finalize_bounded_by_rotate_timeout()
 {
    const RETIRED: &str =
        "projects/test-proj/datasets/test-ds/tables/agent_events/streams/committed-1";
    let srv = ScriptedServer::new(
        Vec::new(),
        |attempt, _| {
            if attempt == 1 {
                return Err(tonic_status(Code::OutOfRange, "offset beyond end"));
            }
            Ok(AppendRowsResponse::default())
        },
        Some(finalize_retired(RETIRED, || {
            Box::pin(std::future::pending())
        })),
    );
    let logger = Arc::new(RecordingLogger::default());
    let config = Config {
        logger: as_logger(&logger),
        ..exactly_once_test_config()
    };
    let bp = processor(
        &Context::background(),
        Some(srv.server.client().unwrap()),
        "",
        config,
    )
    .await;
    set_rotate_timeout(&bp, Duration::from_millis(50)).await;

    bp.write_batch(&Context::background(), vec![event_row("LLM_REQUEST")])
        .await
        .unwrap_or_else(|e| panic!("writeBatch() error = {e}"));

    if tokio::time::timeout(
        Duration::from_millis(1500),
        bp.stream_writer.wait_finalizations(),
    )
    .await
    .is_err()
    {
        panic!(
            "retired-stream finalization did not finish within 1.5s under rotateTimeout=50ms without Close"
        );
    }
    let want = format!("failed to finalize write stream {RETIRED}");
    assert!(
        logger.contains(&want),
        "log does not contain {want:?}; got:\n{logger}"
    );
    assert!(
        logger.contains("context deadline exceeded"),
        "retired-stream finalization log does not mention \"context deadline exceeded\"; got:\n{logger}"
    );

    bp.stream_writer.cancel_finalizations();
    bp.close().await;
}
