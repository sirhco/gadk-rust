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

//! Port of ADK-Go `plugin/agentanalytics/batch_processor_test.go` (ledger 7b-2),
//! with the `arrow_test_helper_test.go` helpers it uses ([`support`]). Go's `mockStream` is
//! `write::stream_writer::tests::MockStream` (injected into `stream_writer.state.stream`), Go's
//! gRPC servers are hooks on the in-process `FakeWriteServer`, and time is tokio's paused clock
//! (Go's sleeps and timeouts advance it). Go's `t.Cleanup(bp.Close)` is an explicit
//! `bp.close().await` at the end of a test. The tests are split by the Go file's order:
//! this file (the first tests), [`streams_and_drops`], [`rotation_and_close`],
//! [`run_end_flushes`], [`uploads_and_routing`], [`splitting_and_fresh_streams`].

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod plugin_level;
mod rotation_and_close;
mod run_end_flushes;
mod splitting_and_fresh_streams;
mod streams_and_drops;
pub(crate) mod support;
mod uploads_and_routing;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use chrono::Utc;
use pretty_assertions::assert_eq;

use self::support::{
    decode_arrow_rows, event_row, inject_stream, no_retry_config, processor, qrow, queue_len, utc,
};
use crate::config::Config;
use crate::context::Context;
use crate::error::Code;
use crate::row::{Row, RowValue};
use crate::storagepb::AppendRowsResponse;
use crate::write::stream_writer::tests::{Fail, MockCounts, MockStream, in_band};

/// Go `&mockStream{recvRes: &storagepb.AppendRowsResponse{}}` sharing `counts`.
fn ok_stream(counts: &Arc<MockCounts>) -> MockStream {
    MockStream {
        recv_res: Some(AppendRowsResponse::default()),
        counts: Arc::clone(counts),
        ..MockStream::default()
    }
}

#[tokio::test(start_paused = true)]
async fn test_batch_processor_append_and_flush() {
    let ctx = Context::background().with_cancel();
    let config = Config {
        batch_size: 2,
        batch_flush_intv: Duration::from_millis(100),
        ..no_retry_config()
    };
    let bp = processor(&ctx, None, "test_stream", config).await;
    let counts = Arc::new(MockCounts::default());
    inject_stream(&bp, ok_stream(&counts)).await;

    bp.start();

    // Append 3 rows. The first 2 should trigger an immediate flush.
    bp.append(event_row("test_event_1"));
    bp.append(event_row("test_event_2"));
    bp.append(event_row("test_event_3"));

    // Wait for the background task to process the remaining row via periodic flush.
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(
        counts.sends.load(Ordering::SeqCst),
        2,
        "Expected 2 batch sends (one for size, one for timer)"
    );
    bp.close().await;
    ctx.cancel();
}

#[tokio::test(start_paused = true)]
async fn test_batch_processor_write_batch_errors() {
    let ctx = Context::background();
    let cases: [(&str, MockStream); 3] = [
        (
            "Send error",
            MockStream {
                send_err: Some(Fail::Msg("send failed")),
                ..MockStream::default()
            },
        ),
        (
            "Recv error",
            MockStream {
                recv_err: Some(Fail::Msg("recv failed")),
                ..MockStream::default()
            },
        ),
        (
            "BigQuery append error",
            MockStream {
                recv_res: Some(in_band(Code::Ok, "some append error")),
                ..MockStream::default()
            },
        ),
    ];
    for (name, stream) in cases {
        // MaxRetries 0 prevents a reconnect on the nil client.
        let bp = processor(&ctx, None, "test_stream", no_retry_config()).await;
        inject_stream(&bp, stream).await;
        let err = bp
            .write_batch(&ctx, vec![event_row("error_test_event")])
            .await;
        assert!(
            err.is_err(),
            "{name}: Expected error in writeBatch, got nil"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn test_batch_processor_data_types() {
    let ctx = Context::background();
    let bp = processor(&ctx, None, "test_stream", no_retry_config()).await;
    let counts = Arc::new(MockCounts::default());
    inject_stream(&bp, ok_stream(&counts)).await;

    let row = qrow(&[
        ("timestamp", RowValue::Timestamp(Utc::now())),
        ("event_type", "data_types_test".into()),
        ("agent", "test_agent".into()),
        ("session_id", "sess-123".into()),
        ("invocation_id", "inv-456".into()),
        ("user_id", "u-789".into()),
        ("trace_id", "trace-001".into()),
        ("span_id", "span-001".into()),
        ("parent_span_id", "pspan-001".into()),
        ("is_truncated", true.into()),
        ("content", "{\"hello\":\"world\"}".into()),
        ("attributes", "{\"key\":\"value\"}".into()),
        ("latency_ms", "{\"llm\":100}".into()),
        ("status", "OK".into()),
        ("error_message", "".into()),
    ]);
    bp.write_batch(&ctx, vec![row])
        .await
        .unwrap_or_else(|e| panic!("writeBatch failed with data types: {e}"));
    assert_eq!(counts.sends.load(Ordering::SeqCst), 1, "Expected 1 send");
}

#[tokio::test(start_paused = true)]
async fn test_batch_processor_write_batch_encodes_rows() {
    let ctx = Context::background();
    let bp = processor(&ctx, None, "test_stream", no_retry_config()).await;
    let counts = Arc::new(MockCounts::default());
    inject_stream(&bp, ok_stream(&counts)).await;

    let ts = utc(2026, 1, 2, 3, 4, 5, 123_456_789);
    let part = |index: RowValue| -> Row {
        [
            ("mime_type", RowValue::from("text/plain")),
            ("text", "hi".into()),
            ("part_index", index),
            ("storage_mode", "INLINE".into()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect()
    };
    let row = qrow(&[
        ("timestamp", RowValue::Timestamp(ts)),
        ("event_id", "0123456789abcdef0123456789abcdef".into()),
        ("event_type", "LLM_REQUEST".into()),
        ("content", r#"{"prompt":"hi"}"#.into()),
        (
            "content_parts",
            RowValue::Parts(vec![part(RowValue::Int(0))]),
        ),
        ("status", "OK".into()),
        ("is_truncated", false.into()),
    ]);
    bp.write_batch(&ctx, vec![row])
        .await
        .unwrap_or_else(|e| panic!("writeBatch() error: {e}"));

    let sent = counts.sent.lock().unwrap().clone();
    assert_eq!(
        sent.len(),
        1,
        "Send received {} requests, want 1",
        sent.len()
    );
    let want = vec![
        qrow(&[
            (
                "timestamp",
                RowValue::Timestamp(utc(2026, 1, 2, 3, 4, 5, 123_456_000)),
            ),
            ("event_id", "0123456789abcdef0123456789abcdef".into()),
            ("event_type", "LLM_REQUEST".into()),
            ("content", r#"{"prompt":"hi"}"#.into()),
            (
                "content_parts",
                RowValue::Parts(vec![part(RowValue::Int(0))]),
            ),
            ("status", "OK".into()),
            ("is_truncated", false.into()),
        ])
        .row,
    ];
    assert_eq!(decode_arrow_rows(&sent[0]), want, "decoded rows mismatch");
}

#[tokio::test(start_paused = true)]
async fn test_batch_processor_queue_full() {
    let ctx = Context::background();
    let config = Config {
        batch_size: 10,
        queue_max_size: 1,
        ..Config::default()
    };
    let bp = processor(&ctx, None, "test_stream", config).await;

    // This should succeed and sit in the queue.
    bp.append(event_row("1"));
    // This should drop since the queue is full (size 1) and we haven't flushed.
    bp.append(event_row("2"));

    assert_eq!(queue_len(&bp), 1, "Expected queue size 1");
}

#[tokio::test(start_paused = true)]
async fn test_batch_processor_content_parts_edge_cases() {
    let ctx = Context::background();
    let bp = processor(&ctx, None, "test_stream", no_retry_config()).await;
    let counts = Arc::new(MockCounts::default());
    inject_stream(&bp, ok_stream(&counts)).await;

    let cases = [
        (
            "Missing content_parts",
            qrow(&[
                ("timestamp", RowValue::Timestamp(Utc::now())),
                ("event_type", "missing_parts".into()),
            ]),
        ),
        (
            "Nil content_parts",
            qrow(&[
                ("timestamp", RowValue::Timestamp(Utc::now())),
                ("event_type", "nil_parts".into()),
                ("content_parts", RowValue::Null),
            ]),
        ),
        (
            "Empty content_parts",
            qrow(&[
                ("timestamp", RowValue::Timestamp(Utc::now())),
                ("event_type", "empty_parts".into()),
                ("content_parts", RowValue::Parts(Vec::new())),
            ]),
        ),
        (
            "Invalid type content_parts",
            qrow(&[
                ("timestamp", RowValue::Timestamp(Utc::now())),
                ("event_type", "invalid_parts".into()),
                ("content_parts", "not a slice".into()),
            ]),
        ),
    ];
    for (name, row) in cases {
        if let Err(e) = bp.write_batch(&ctx, vec![row]).await {
            panic!("{name}: writeBatch failed: {e}");
        }
    }
}
