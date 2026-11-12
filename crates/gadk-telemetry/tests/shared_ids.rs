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

//! Spec R11: with the OTel bridge installed before gadk-server's debug store
//! (`Registry::default().with(bridge).with(debug)`, plan decision D3), the debug store reports
//! the IDs of the exported SDK spans and log records; without the bridge, or with a shut-down
//! provider, it generates its own.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use gadk_server::rest::{DebugSpan, DebugTelemetry, DebugTelemetryConfig};
use gadk_telemetry::OtelBridge;
use opentelemetry::trace::SpanId;
use opentelemetry_sdk::logs::{InMemoryLogExporter, SdkLoggerProvider};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};
use tracing_subscriber::layer::SubscriberExt;

/// The log target of gadk's OTel log records (`gadk_engine::telemetry::LOG_TARGET`).
const LOG: &str = "gadk_engine::telemetry::log";

/// An ADK root span of session `s1`, a non-ADK span, an ADK child and a log record under it.
fn emit() {
    let root = tracing::info_span!(
        "invoke_agent",
        otel.name = "invoke_agent a",
        gen_ai.conversation.id = "s1"
    );
    let _r = root.enter();
    let foreign = tracing::info_span!("Connection");
    let _f = foreign.enter();
    let child = tracing::info_span!("generate_content", otel.name = "generate_content m");
    let _c = child.enter();
    tracing::event!(
        target: LOG,
        tracing::Level::INFO,
        event.name = "gen_ai.user.message",
        otel.log.body = r#"{"content":"hi"}"#
    );
}

/// The debug store's spans of session `s1`: (root, child).
fn debug_spans(dt: &DebugTelemetry) -> (DebugSpan, DebugSpan) {
    let spans = dt.get_spans_by_session_id("s1");
    assert_eq!(spans.len(), 2, "{spans:?}");
    let find = |name: &str| {
        spans
            .iter()
            .find(|s| s.name == name)
            .cloned()
            .unwrap_or_else(|| panic!("no span {name}"))
    };
    (find("invoke_agent a"), find("generate_content m"))
}

fn exported(spans: &[SpanData], name: &str) -> SpanData {
    spans
        .iter()
        .find(|s| s.name == name)
        .cloned()
        .unwrap_or_else(|| panic!("no exported span {name}"))
}

/// Debug store IDs equal the exported span and log record IDs, the child's parent ID included.
#[test]
fn debug_store_reports_exported_ids() {
    let spans = InMemorySpanExporter::default();
    let logs = InMemoryLogExporter::default();
    let tp = SdkTracerProvider::builder()
        .with_simple_exporter(spans.clone())
        .build();
    let lp = SdkLoggerProvider::builder()
        .with_simple_exporter(logs.clone())
        .build();
    let dt = DebugTelemetry::new(DebugTelemetryConfig::default());
    let sub = tracing_subscriber::registry()
        .with(OtelBridge::new(Some(&tp), Some(&lp)))
        .with(dt.clone());
    tracing::subscriber::with_default(sub, emit);

    let (root, child) = debug_spans(&dt);
    let finished = spans.get_finished_spans().unwrap();
    assert_eq!(finished.len(), 2);
    let (x_root, x_child) = (
        exported(&finished, "invoke_agent a"),
        exported(&finished, "generate_content m"),
    );
    assert_eq!(root.trace_id, x_root.span_context.trace_id().to_string());
    assert_eq!(root.span_id, x_root.span_context.span_id().to_string());
    assert_eq!(root.parent_span_id, SpanId::INVALID.to_string());
    assert_eq!(child.trace_id, x_child.span_context.trace_id().to_string());
    assert_eq!(child.span_id, x_child.span_context.span_id().to_string());
    assert_eq!(child.parent_span_id, x_child.parent_span_id.to_string());
    assert_eq!(child.parent_span_id, root.span_id);

    let records = logs.get_emitted_logs().unwrap();
    assert_eq!(records.len(), 1);
    let tc = records[0].record.trace_context().expect("trace context");
    assert_eq!(child.logs.len(), 1);
    assert_eq!(child.logs[0].trace_id, tc.trace_id.to_string());
    assert_eq!(child.logs[0].span_id, tc.span_id.to_string());
    assert_eq!(child.logs[0].span_id, child.span_id);
}

/// A subscriber with the debug store only still gets IDs (generated, a trace per root).
#[test]
fn debug_only_subscriber_gets_ids() {
    let dt = DebugTelemetry::new(DebugTelemetryConfig::default());
    let sub = tracing_subscriber::registry().with(dt.clone());
    tracing::subscriber::with_default(sub, emit);

    let (root, child) = debug_spans(&dt);
    assert_ne!(root.trace_id, "0".repeat(32));
    assert_ne!(root.span_id, "0".repeat(16));
    assert_eq!(child.trace_id, root.trace_id);
    assert_eq!(child.parent_span_id, root.span_id);
    assert_eq!(child.logs[0].span_id, child.span_id);
}

/// With a shut-down tracer provider the bridge stores all-zero IDs; the debug store replaces
/// them with generated ones and nothing exports.
#[test]
fn shut_down_provider_ids_are_generated() {
    let spans = InMemorySpanExporter::default();
    let tp = SdkTracerProvider::builder()
        .with_simple_exporter(spans.clone())
        .build();
    tp.shutdown().unwrap();
    let dt = DebugTelemetry::new(DebugTelemetryConfig::default());
    let sub = tracing_subscriber::registry()
        .with(OtelBridge::new(Some(&tp), None))
        .with(dt.clone());
    tracing::subscriber::with_default(sub, emit);

    let (root, child) = debug_spans(&dt);
    assert_ne!(root.trace_id, "0".repeat(32));
    assert_ne!(root.span_id, "0".repeat(16));
    assert_ne!(child.span_id, "0".repeat(16));
    assert_eq!(child.trace_id, root.trace_id);
    assert_eq!(child.parent_span_id, root.span_id);
    assert!(spans.get_finished_spans().unwrap().is_empty());
}
