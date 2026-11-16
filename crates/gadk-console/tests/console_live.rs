// Copyright 2026 Chris Olson
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

//! Opt-in round trip against a real GCP project: an agent-style `render_prompt` span and one
//! log record go out through gadk-telemetry's `with_otel_to_cloud(true)` (traces to Cloud
//! Trace, logs to Cloud Logging under the default log id `gadk`), and the console's
//! `CloudTraceSource` finds the trace by agent, reads its labels and attaches the log entry
//! to the span.
//!
//! Run: `GADK_CONSOLE_PROJECT=<project> cargo test -p gadk-console --test console_live --
//! --ignored` with Application Default Credentials that may write traces and logs
//! (`roles/cloudtrace.agent`, `roles/logging.logWriter`) and read them
//! (`roles/cloudtrace.user`, `roles/logging.viewer`) in that project.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gadk_console::traces::{CloudTraceSource, TimeRange, TraceQuery, TraceSource as _};
use gadk_telemetry::{new, with_gcp_quota_project, with_gcp_resource_project, with_otel_to_cloud};
use opentelemetry::KeyValue;
use opentelemetry::logs::{AnyValue, LogRecord as _, Logger as _, LoggerProvider as _, Severity};
use opentelemetry::trace::{Span as _, SpanContext, Tracer as _, TracerProvider as _};

/// How long to wait for the trace (and its log entry) to become readable. Both APIs are
/// eventually consistent; Cloud Logging reads have been seen to take over a minute.
const READ_BACK_DEADLINE: Duration = Duration::from_secs(180);
/// Pause between polls.
const POLL_INTERVAL: Duration = Duration::from_secs(5);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "writes to and reads from a real GCP project; set GADK_CONSOLE_PROJECT"]
async fn live_trace_and_log_round_trip() {
    let project =
        std::env::var("GADK_CONSOLE_PROJECT").expect("set GADK_CONSOLE_PROJECT to run this test");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let marker = format!("gadk-console-live-{}-{nanos}", std::process::id());

    // Nothing else supplied: gadk-telemetry builds the Cloud Trace and Cloud Logging exporters.
    let providers = new(&[
        with_otel_to_cloud(true),
        with_gcp_resource_project(project.clone()),
        with_gcp_quota_project(project.clone()),
    ])
    .await
    .unwrap();
    let tp = providers.tracer_provider.clone().unwrap();
    let lp = providers.logger_provider.clone().unwrap();

    let tracer = tp.tracer("gadk-console-live");
    let mut span = tracer
        .span_builder("render_prompt live")
        .with_attributes([
            KeyValue::new("gen_ai.agent.name", marker.clone()),
            KeyValue::new("prompt.name", "live/greet"),
            KeyValue::new("prompt.version", 3_i64),
        ])
        .start(&tracer);
    let sc: SpanContext = span.span_context().clone();
    let span_hex = sc.span_id().to_string();
    let trace_hex = sc.trace_id().to_string();

    let logger = lp.logger("gadk-console-live");
    let mut record = logger.create_log_record();
    record.set_event_name("gadk.console_live_test");
    record.set_severity_number(Severity::Info);
    record.set_body(AnyValue::from(marker.clone()));
    record.set_trace_context(sc.trace_id(), sc.span_id(), Some(sc.trace_flags()));
    logger.emit(record);
    span.end();

    // The batch processors export on their own threads; their HTTP requests run on this
    // runtime, so the blocking flushes go to `spawn_blocking`.
    let (tp_flush, lp_flush) = (tp.clone(), lp.clone());
    tokio::task::spawn_blocking(move || (tp_flush.force_flush(), lp_flush.force_flush()))
        .await
        .map(|(t, l)| {
            t.expect("flush the tracer provider");
            l.expect("flush the logger provider");
        })
        .unwrap();
    providers.shutdown().await.unwrap();

    let source = CloudTraceSource::new(project.clone(), "gadk");
    let query = TraceQuery {
        agent: Some(marker.clone()),
        range: TimeRange::Hour1,
        ..TraceQuery::default()
    };
    let deadline = Instant::now() + READ_BACK_DEADLINE;
    let summary = loop {
        let page = source.list(&query).await.expect("list traces");
        if let Some(t) = page.traces.into_iter().next() {
            break t;
        }
        assert!(
            Instant::now() < deadline,
            "trace for agent {marker} not listed within {READ_BACK_DEADLINE:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    };
    assert_eq!(summary.trace_id.as_str(), trace_hex);
    assert_eq!(summary.root_name, "render_prompt live");
    assert_eq!(summary.agent.as_deref(), Some(marker.as_str()));

    // Spans and logs index separately: poll `get` until the log entry is attached too.
    let detail = loop {
        let detail = source.get(&summary.trace_id).await.expect("get trace");
        let logs = match &detail.logs {
            Ok(logs) => logs,
            Err(e) => panic!("logs for trace {trace_hex}: {e}"),
        };
        let attached = logs
            .entries
            .iter()
            .any(|e| e.span_id.as_deref() == Some(span_hex.as_str()));
        if attached {
            break detail;
        }
        assert!(
            Instant::now() < deadline,
            "log entry for span {span_hex} not attached within {READ_BACK_DEADLINE:?}: {:?}",
            logs.entries
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    };

    let span = detail
        .spans
        .iter()
        .find(|s| s.span_id == span_hex)
        .unwrap_or_else(|| panic!("span {span_hex} in {:?}", detail.spans));
    assert_eq!(span.name, "render_prompt live");
    assert_eq!(span.parent_id, None);
    assert_eq!(
        span.labels.get("gen_ai.agent.name").map(String::as_str),
        Some(marker.as_str()),
        "{:?}",
        span.labels
    );
    assert_eq!(
        span.labels.get("prompt.name").map(String::as_str),
        Some("live/greet"),
        "{:?}",
        span.labels
    );
    assert_eq!(
        span.labels.get("prompt.version").map(String::as_str),
        Some("3"),
        "{:?}",
        span.labels
    );
    // The trace page's prompt link (`model::prompt_link`, crate-private) derives from exactly
    // these: a `render_prompt…` span name, `prompt.name`, and a u32 `prompt.version`.
    assert!(span.name.starts_with("render_prompt"));
    assert_eq!(span.labels["prompt.version"].parse::<u32>(), Ok(3));

    let entry = detail
        .logs
        .as_ref()
        .unwrap()
        .entries
        .iter()
        .find(|e| e.span_id.as_deref() == Some(span_hex.as_str()))
        .unwrap();
    assert_eq!(entry.severity.as_deref(), Some("INFO"));
    assert_eq!(
        entry.payload["event.name"],
        serde_json::json!("gadk.console_live_test")
    );
}
