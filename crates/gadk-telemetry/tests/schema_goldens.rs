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

//! Spec R14: every telemetry schema golden the engine's schema test checks
//! (`gadk-engine/tests/internal_telemetry_functionaltest_telemetry_schema`, a port of ADK-Go
//! `telemetry_schema_test.go`) holds for the spans and log records exported through the
//! OTel bridge into in-memory exporters. The scenarios and goldens are the engine test's own
//! (`scenarios.rs` included by `#[path]`, `testdata/` read in place). Exported span attributes
//! the bridge stringified (structured values: the ones the D6 decoder reads as
//! [`D6Value::Json`], noted by a probe layer after the bridge) are compared after parsing their
//! JSON text.
//!
//! The cases run one after another on this test's thread, each under its own thread-default
//! subscriber, so no global `tracing` dispatcher is needed to keep callsite interest stable.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "../../gadk-engine/tests/internal_telemetry_functionaltest_telemetry_schema/scenarios.rs"]
mod scenarios;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use gadk_engine::telemetry::d6::{D6Fields, D6Value, SpanIds};
use gadk_engine::testutil::telemetry::{ENV_LOCK, LogRecord, SpanStub, build_digest};
use gadk_platform::env::MapEnv;
use gadk_telemetry::OtelBridge;
use opentelemetry::logs::AnyValue;
use opentelemetry::trace::{SpanId, Status, TraceId};
use opentelemetry::{Array, Value as OtelValue};
use opentelemetry_sdk::logs::{InMemoryLogExporter, SdkLoggerProvider};
use opentelemetry_sdk::trace::{IdGenerator, InMemorySpanExporter, SdkTracerProvider, SpanData};
use pretty_assertions::assert_eq;
use serde_json::{Map, Number, Value};
use tracing::span::{Attributes, Id, Record};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;

use scenarios::{Case, cases};

/// Go `captureContentEnvVar`.
const CAPTURE_CONTENT_ENV_VAR: &str = "OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT";
/// Go `adkTelemetrySchemaVersionOptIn`.
const ADK_TELEMETRY_SCHEMA_VERSION_OPT_IN: &str = "ADK_TELEMETRY_SCHEMA_VERSION_OPT_IN";

fn testdata() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../gadk-engine/tests/internal_telemetry_functionaltest_telemetry_schema/testdata")
}

/// Sequential IDs: a span's ID orders it by start, as the engine's capture numbers its spans
/// (the digest orders siblings by start time).
#[derive(Debug, Default)]
struct SeqIds(AtomicU64);

impl SeqIds {
    fn next(&self) -> u64 {
        self.0.fetch_add(1, Ordering::Relaxed) + 1
    }
}

impl IdGenerator for SeqIds {
    fn new_trace_id(&self) -> TraceId {
        TraceId::from(u128::from(self.next()))
    }
    fn new_span_id(&self) -> SpanId {
        SpanId::from(self.next())
    }
}

/// The (span ID, key) pairs of the span attributes last recorded as structured values
/// ([`D6Value::Json`]), which the bridge exports as JSON text. Installed after the bridge, it
/// reads the span ID from the bridge's [`SpanIds`].
#[derive(Clone, Default)]
struct StructuredProbe(Arc<Mutex<HashSet<(u64, String)>>>);

impl StructuredProbe {
    fn note<S>(&self, id: &Id, ctx: &Context<'_, S>, fields: D6Fields)
    where
        S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    {
        let Some(ids) = ctx
            .span(id)
            .and_then(|s| s.extensions().get::<SpanIds>().copied())
        else {
            return;
        };
        let span_id = u64::from_be_bytes(ids.span_id);
        let mut set = self.0.lock().unwrap();
        for (key, value) in fields.attributes {
            if matches!(value, D6Value::Json(_)) {
                set.insert((span_id, key));
            } else {
                set.remove(&(span_id, key));
            }
        }
    }

    fn contains(&self, span_id: u64, key: &str) -> bool {
        self.0.lock().unwrap().contains(&(span_id, key.to_owned()))
    }
}

impl<S> Layer<S> for StructuredProbe
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let mut fields = D6Fields::default();
        attrs.record(&mut fields);
        self.note(id, &ctx, fields);
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let mut fields = D6Fields::default();
        values.record(&mut fields);
        self.note(id, &ctx, fields);
    }
}

/// Runs `c` with the bridge exporting into in-memory exporters and returns the digest (as JSON)
/// of what they received.
fn record(c: &Case) -> Value {
    let span_exporter = InMemorySpanExporter::default();
    let log_exporter = InMemoryLogExporter::default();
    let tp = SdkTracerProvider::builder()
        .with_simple_exporter(span_exporter.clone())
        .with_id_generator(SeqIds::default())
        .build();
    let lp = SdkLoggerProvider::builder()
        .with_simple_exporter(log_exporter.clone())
        .build();

    let _lock = ENV_LOCK.lock();
    gadk_engine::telemetry::set_env_provider(Arc::new(MapEnv(
        [
            (CAPTURE_CONTENT_ENV_VAR.to_owned(), c.capture.to_owned()),
            (
                ADK_TELEMETRY_SCHEMA_VERSION_OPT_IN.to_owned(),
                c.schema_version.to_owned(),
            ),
        ]
        .into(),
    )));
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let structured = StructuredProbe::default();
    let sub = tracing_subscriber::registry()
        .with(OtelBridge::new(Some(&tp), Some(&lp)))
        .with(structured.clone());
    let res = {
        let _g = tracing::subscriber::set_default(sub);
        let res = rt.block_on(scenarios::run(c.scenario));
        // Tasks still holding spans (a run that failed early) drop with the runtime, which
        // ends those spans before the exporters are read.
        drop(rt);
        res
    };
    gadk_engine::telemetry::set_env_provider(Arc::new(MapEnv::default()));
    assert_eq!(
        res.is_err(),
        c.scenario.want_err(),
        "{}: run error = {res:?}, want error: {}",
        c.id(),
        c.scenario.want_err()
    );

    let spans: Vec<SpanStub> = span_exporter
        .get_finished_spans()
        .unwrap()
        .iter()
        .map(|s| span_stub(c, s, &structured))
        .collect();
    let logs: Vec<LogRecord> = log_exporter
        .get_emitted_logs()
        .unwrap()
        .iter()
        .map(|l| log_record(&l.record))
        .collect();
    let d = build_digest(&spans, &logs).unwrap_or_else(|e| panic!("{}: build digest: {e}", c.id()));
    serde_json::to_value(&d).expect("marshal digest")
}

/// One exported span as the digest reads it; an attribute key appears at most once.
fn span_stub(c: &Case, s: &SpanData, structured: &StructuredProbe) -> SpanStub {
    let span_id = u64::from_be_bytes(s.span_context.span_id().to_bytes());
    let mut seen = HashSet::new();
    let attributes = s
        .attributes
        .iter()
        .map(|kv| {
            let key = kv.key.as_str().to_owned();
            assert!(
                seen.insert(key.clone()),
                "{}: span {} exports attribute {key} twice",
                c.id(),
                s.name
            );
            let value = attribute_json(&kv.value, structured.contains(span_id, &key));
            (key, value)
        })
        .collect();
    SpanStub {
        name: s.name.to_string(),
        span_id,
        parent_span_id: (s.parent_span_id != SpanId::INVALID)
            .then(|| u64::from_be_bytes(s.parent_span_id.to_bytes())),
        start_time: i64::try_from(span_id).expect("span id"),
        attributes,
        status: match s.status {
            Status::Unset => "UNSET",
            Status::Error { .. } => "ERROR",
            Status::Ok => "OK",
        }
        .to_owned(),
    }
}

/// An exported span attribute as JSON; a `structured` one (JSON text) parsed back.
fn attribute_json(value: &OtelValue, structured: bool) -> Value {
    match value {
        OtelValue::Bool(b) => Value::Bool(*b),
        OtelValue::I64(i) => Value::from(*i),
        OtelValue::F64(f) => float(*f),
        OtelValue::String(s) if structured => serde_json::from_str(s.as_str())
            .unwrap_or_else(|e| panic!("structured attribute {s:?} is not JSON: {e}")),
        OtelValue::String(s) => Value::String(s.as_str().to_owned()),
        OtelValue::Array(Array::Bool(v)) => v.iter().copied().map(Value::Bool).collect(),
        OtelValue::Array(Array::I64(v)) => v.iter().copied().map(Value::from).collect(),
        OtelValue::Array(Array::F64(v)) => v.iter().copied().map(float).collect(),
        OtelValue::Array(Array::String(v)) => v
            .iter()
            .map(|s| Value::String(s.as_str().to_owned()))
            .collect(),
        other => panic!("unexpected attribute value {other:?}"),
    }
}

fn float(f: f64) -> Value {
    Number::from_f64(f).map_or(Value::Null, Value::Number)
}

/// One exported log record as the digest reads it (Go `FromLogValue` of body and attributes).
fn log_record(r: &opentelemetry_sdk::logs::SdkLogRecord) -> LogRecord {
    LogRecord {
        event_name: r.event_name().unwrap_or_default().to_owned(),
        span_id: r
            .trace_context()
            .map(|tc| u64::from_be_bytes(tc.span_id.to_bytes())),
        body: r.body().map_or(Value::Null, any_json),
        attributes: r
            .attributes_iter()
            .map(|(k, v)| (k.to_string(), any_json(v)))
            .collect(),
    }
}

/// Go `FromLogValue` over an exported `AnyValue`.
fn any_json(v: &AnyValue) -> Value {
    match v {
        AnyValue::Int(i) => Value::from(*i),
        AnyValue::Double(f) => float(*f),
        AnyValue::String(s) => Value::String(s.as_str().to_owned()),
        AnyValue::Boolean(b) => Value::Bool(*b),
        AnyValue::Bytes(b) => b.iter().copied().map(Value::from).collect(),
        AnyValue::ListAny(items) => items.iter().map(any_json).collect(),
        AnyValue::Map(m) => Value::Object(
            m.iter()
                .map(|(k, v)| (k.to_string(), any_json(v)))
                .collect::<Map<_, _>>(),
        ),
        other => panic!("unexpected log value {other:?}"),
    }
}

fn read_golden(path: &Path) -> Value {
    let b =
        std::fs::read(path).unwrap_or_else(|e| panic!("missing golden {}: {e}", path.display()));
    serde_json::from_slice(&b).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

/// Every case of the engine's schema test, through the exporter.
#[test]
fn schema_goldens_hold_through_the_exporter() {
    let all = cases();
    assert!(!all.is_empty());
    for c in &all {
        let golden = testdata().join(format!("{}.json", c.id()));
        let want = read_golden(&golden);
        let got = record(c);
        assert_eq!(want, got, "telemetry differs from {}", golden.display());
    }
}
