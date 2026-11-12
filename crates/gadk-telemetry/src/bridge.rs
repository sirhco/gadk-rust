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

//! [`OtelBridge`]: the `tracing` layer that exports gadk's ADK spans (the 5a D6 layout) and log
//! records through the OpenTelemetry SDK providers.
//!
//! Spans and records are what Go's `internal/telemetry` hands its tracer and logger (scope
//! `gcp.vertex.agent`, Go `systemName`), decoded by [`gadk_engine::telemetry::d6`]:
//! - an ADK span starts an SDK span (`Tracer::build_with_context`) under the context of its
//!   nearest ADK ancestor (non-ADK spans are transparent; none → a new trace); the span's
//!   registry extensions keep the `opentelemetry::Context` carrying it (an SDK `Span` is not
//!   `Clone`) and its IDs as a [`SpanIds`], which later layers (the debug store) reuse;
//! - its start attributes go to the span builder (the sampler sees them, as Go's
//!   `trace.WithAttributes`); attributes recorded later are merged (a key recorded again keeps
//!   its last value) and set when the span closes, with the status, before it ends;
//! - a log event on `LOG_TARGET` is an SDK log record carrying the trace context of its nearest
//!   ADK span.

use std::collections::HashSet;
use std::sync::LazyLock;

use gadk_engine::telemetry::d6::{self, D6Fields, D6StatusCode, D6Value, SpanIds};
use opentelemetry::logs::{AnyValue, LogRecord as _, Logger as _, LoggerProvider as _, Severity};
use opentelemetry::trace::{Span as _, Status, TraceContextExt, Tracer as _, TracerProvider as _};
use opentelemetry::{Array, Context, InstrumentationScope, Key, KeyValue, StringValue, Value};
use opentelemetry_sdk::logs::{SdkLogger, SdkLoggerProvider};
use opentelemetry_sdk::trace::{SdkTracer, SdkTracerProvider};
use parking_lot::Mutex;
use serde_json::Value as Json;
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context as LayerContext;
use tracing_subscriber::registry::LookupSpan;

/// Go `internal/telemetry` `systemName`: the instrumentation scope of ADK's tracer and logger.
const SYSTEM_NAME: &str = "gcp.vertex.agent";

/// Exports every ADK span (a `tracing` span declaring `otel.name`) as an SDK span on the tracer
/// provider, parented under its nearest ADK ancestor, and every log event on
/// `gadk_engine::telemetry::LOG_TARGET` as an SDK log record on the logger provider. With
/// neither provider it records nothing. Install it before any other layer that reads the
/// `SpanIds` extension (`Registry::default().with(bridge).with(…)`).
#[derive(Debug)]
pub struct OtelBridge {
    tracer: Option<SdkTracer>,
    logger: Option<SdkLogger>,
}

impl OtelBridge {
    /// A bridge exporting through `tracer_provider` and `logger_provider`.
    pub fn new(
        tracer_provider: Option<&SdkTracerProvider>,
        logger_provider: Option<&SdkLoggerProvider>,
    ) -> Self {
        Self {
            tracer: tracer_provider.map(|tp| tp.tracer_with_scope(scope())),
            logger: logger_provider.map(|lp| lp.logger_with_scope(scope())),
        }
    }
}

/// Go `tracer()` / `otelLogger()`: scope `gcp.vertex.agent` at the gadk version.
// Divergence (ledger 7a): Go also sets the scope's schema URL from
// ADK_TELEMETRY_SCHEMA_VERSION_OPT_IN (1.36.0 legacy, 1.44.0 otherwise); the engine's schema
// choice is crate-private, so gadk's scope carries no schema URL.
fn scope() -> InstrumentationScope {
    InstrumentationScope::builder(SYSTEM_NAME)
        .with_version(env!("CARGO_PKG_VERSION"))
        .build()
}

/// An ADK span's bridge state, in its registry extensions until it closes.
struct BridgeSpan {
    /// The context carrying the started SDK span (the parent context of its ADK children).
    cx: Context,
    /// The attributes the span started with (already on the SDK span).
    start: Vec<(String, D6Value)>,
    /// Attributes recorded after the start, merged (a key keeps its first position and its
    /// last value).
    recorded: Vec<(String, D6Value)>,
    /// The last recorded status code.
    status: D6StatusCode,
    /// The last recorded status description.
    description: Option<String>,
}

impl BridgeSpan {
    /// Merges the status of one visit.
    fn merge_status(&mut self, fields: &mut D6Fields) {
        if let Some(code) = fields.status_code {
            self.status = code;
        }
        if let Some(description) = fields.status_description.take() {
            self.description = Some(description);
        }
    }
}

impl<S> Layer<S> for OtelBridge
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: LayerContext<'_, S>) {
        let Some(tracer) = &self.tracer else { return };
        if !d6::is_adk_span(attrs.metadata()) {
            return;
        }
        let Some(span) = ctx.span(id) else { return };
        // The nearest ADK ancestor: only ADK spans hold a `BridgeSpan`.
        let parent_cx = span
            .scope()
            .skip(1)
            .find_map(|p| p.extensions().get::<BridgeSpan>().map(|b| b.cx.clone()))
            .unwrap_or_default();
        let mut fields = D6Fields::default();
        attrs.record(&mut fields);
        let name = fields
            .name
            .take()
            .unwrap_or_else(|| attrs.metadata().name().to_owned());
        let start = std::mem::take(&mut fields.attributes);
        let attributes: Vec<KeyValue> = start
            .iter()
            .map(|(k, v)| key_value(k.clone(), v.clone()))
            .collect();
        let builder = tracer.span_builder(name).with_attributes(attributes);
        let otel_span = tracer.build_with_context(builder, &parent_cx);
        let sc = otel_span.span_context().clone();
        let mut state = BridgeSpan {
            cx: parent_cx.with_span(otel_span),
            start,
            recorded: Vec::new(),
            status: D6StatusCode::Unset,
            description: None,
        };
        state.merge_status(&mut fields);
        let mut ext = span.extensions_mut();
        ext.insert(SpanIds {
            trace_id: sc.trace_id().to_bytes(),
            span_id: sc.span_id().to_bytes(),
        });
        ext.insert(state);
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: LayerContext<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut ext = span.extensions_mut();
        let Some(state) = ext.get_mut::<BridgeSpan>() else {
            return;
        };
        let mut fields = D6Fields::default();
        values.record(&mut fields);
        if let Some(name) = fields.name.take() {
            state.cx.span().update_name(name);
        }
        state.merge_status(&mut fields);
        for (key, value) in fields.attributes {
            match state.recorded.iter_mut().find(|(k, _)| *k == key) {
                Some(slot) => slot.1 = value,
                None => state.recorded.push((key, value)),
            }
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: LayerContext<'_, S>) {
        let Some(logger) = &self.logger else { return };
        let Some(log) = d6::decode_log(event) else {
            return;
        };
        let mut record = logger.create_log_record();
        record.set_event_name(intern(&log.event_name));
        // Go sets a severity only on its DEBUG records (`log.SeverityDebug`); its other records
        // leave it unset.
        if log.level == tracing::Level::DEBUG {
            record.set_severity_number(Severity::Debug);
        }
        if let Some(body) = log.body.ok().and_then(any_value) {
            record.set_body(body);
        }
        for (key, value) in log.attributes.unwrap_or_default() {
            if let Some(value) = any_value(value) {
                record.add_attribute(key, value);
            }
        }
        // The trace context of the nearest ADK span the event is emitted under (Go: the span in
        // the record's context).
        let sc = ctx.event_scope(event).and_then(|mut scope| {
            scope.find_map(|s| {
                s.extensions()
                    .get::<BridgeSpan>()
                    .map(|b| b.cx.span().span_context().clone())
            })
        });
        if let Some(sc) = sc.filter(|sc| sc.is_valid()) {
            record.set_trace_context(sc.trace_id(), sc.span_id(), Some(sc.trace_flags()));
        }
        logger.emit(record);
    }

    fn on_close(&self, id: Id, ctx: LayerContext<'_, S>) {
        let Some(span) = ctx.span(&id) else { return };
        let Some(state) = span.extensions_mut().remove::<BridgeSpan>() else {
            return;
        };
        let otel_span = state.cx.span();
        // A start attribute recorded again with the same value is already on the span (the SDK
        // appends, it does not replace).
        let start = &state.start;
        otel_span.set_attributes(
            state
                .recorded
                .into_iter()
                .filter(|kv| !start.contains(kv))
                .map(|(k, v)| key_value(k, v)),
        );
        match state.status {
            D6StatusCode::Unset => {}
            D6StatusCode::Ok => otel_span.set_status(Status::Ok),
            D6StatusCode::Error => {
                otel_span.set_status(Status::error(state.description.unwrap_or_default()));
            }
        }
        otel_span.end();
    }
}

/// One decoded D6 attribute as an SDK span attribute.
fn key_value(key: String, value: D6Value) -> KeyValue {
    KeyValue::new(key, span_value(value))
}

/// The SDK span value of a decoded D6 attribute. OTel Rust span values are scalars and
/// homogeneous arrays only, so the kinds it cannot hold become strings.
fn span_value(value: D6Value) -> Value {
    match value {
        D6Value::Str(s) => Value::String(s.into()),
        D6Value::I64(i) => Value::I64(i),
        // Go's attribute package has no unsigned integer; ADK records none.
        // Divergence (ledger 7a): a u64 is exported as its decimal text (above `i64::MAX` an
        // OTel Rust `I64` cannot hold it).
        D6Value::U64(u) => Value::String(u.to_string().into()),
        D6Value::F64(f) => Value::F64(f),
        D6Value::Bool(b) => Value::Bool(b),
        D6Value::StrSlice(items) => Value::Array(Array::String(
            items.into_iter().map(StringValue::from).collect(),
        )),
        // Divergence (ledger 7a): Go exports SLICE and MAP attributes as OTLP array and kvlist
        // values; OTel Rust span attributes cannot hold them, so a structured attribute is its
        // JSON text.
        json @ D6Value::Json(_) => Value::String(json.to_json().to_string().into()),
        // Divergence (ledger 7a): Go exports BYTESLICE as OTLP bytes; OTel Rust span attributes
        // have no bytes, so the value is its base64 text (Go `attribute.Value.Emit`).
        D6Value::Bytes(b) => Value::String(base64(&b).into()),
    }
}

/// A JSON value as a log `AnyValue`, as Go's `toLogValue` builds it: a number is an integer when
/// it fits `i64`, else a float, else its text. OTel Rust's `AnyValue` has no empty value: a
/// `null` (Go's EMPTY) is `None`, and dropped from maps and lists.
///
/// Divergence (ledger 7a): a JSON `null` is dropped (no empty `AnyValue`), and a `null` body
/// leaves the record without a body.
fn any_value(value: Json) -> Option<AnyValue> {
    Some(match value {
        Json::Null => return None,
        Json::Bool(b) => AnyValue::Boolean(b),
        Json::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => AnyValue::Int(i),
            (None, Some(f)) if f.is_finite() => AnyValue::Double(f),
            _ => AnyValue::String(n.to_string().into()),
        },
        Json::String(s) => AnyValue::String(s.into()),
        Json::Array(items) => {
            AnyValue::ListAny(Box::new(items.into_iter().filter_map(any_value).collect()))
        }
        Json::Object(map) => AnyValue::Map(Box::new(
            map.into_iter()
                .filter_map(|(k, v)| any_value(v).map(|v| (Key::from(k), v)))
                .collect(),
        )),
    })
}

/// The process-wide table of log event names: `LogRecord::set_event_name` takes a
/// `&'static str`, and gadk emits a fixed set of Go event names, each leaked once.
static EVENT_NAMES: LazyLock<Mutex<HashSet<&'static str>>> = LazyLock::new(Mutex::default);

/// `name` as a `&'static str`, from [`EVENT_NAMES`].
fn intern(name: &str) -> &'static str {
    let mut names = EVENT_NAMES.lock();
    if let Some(interned) = names.get(name) {
        return interned;
    }
    let interned: &'static str = Box::leak(name.to_owned().into_boxed_str());
    names.insert(interned);
    interned
}

/// Standard base64 with padding (Go `base64.StdEncoding`, as `attribute.Value.Emit` prints a
/// BYTESLICE).
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> shift) & 0x3f) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Arc;

    use opentelemetry::trace::{SpanId, TraceId};
    use opentelemetry_sdk::logs::{InMemoryLogExporter, SdkLogRecord};
    use opentelemetry_sdk::trace::{InMemorySpanExporter, SpanData};
    use serde_json::json;
    use tracing_subscriber::layer::SubscriberExt;

    use super::*;

    /// The log target of gadk's OTel log records (`gadk_engine::telemetry::LOG_TARGET`).
    const LOG: &str = "gadk_engine::telemetry::log";

    /// In-memory span and log exporters behind simple processors.
    struct Exporters {
        spans: InMemorySpanExporter,
        logs: InMemoryLogExporter,
        tp: SdkTracerProvider,
        lp: SdkLoggerProvider,
    }

    impl Exporters {
        fn new() -> Self {
            let spans = InMemorySpanExporter::default();
            let logs = InMemoryLogExporter::default();
            let tp = SdkTracerProvider::builder()
                .with_simple_exporter(spans.clone())
                .build();
            let lp = SdkLoggerProvider::builder()
                .with_simple_exporter(logs.clone())
                .build();
            Self {
                spans,
                logs,
                tp,
                lp,
            }
        }

        fn spans(&self) -> Vec<SpanData> {
            self.spans.get_finished_spans().unwrap()
        }

        fn span(&self, name: &str) -> SpanData {
            self.spans()
                .into_iter()
                .find(|s| s.name == name)
                .unwrap_or_else(|| panic!("no span {name}"))
        }

        fn logs(&self) -> Vec<SdkLogRecord> {
            self.logs
                .get_emitted_logs()
                .unwrap()
                .into_iter()
                .map(|l| l.record)
                .collect()
        }
    }

    /// Records the [`SpanIds`] the bridge left on each span, per span name (a layer after the
    /// bridge, as the debug store is).
    #[derive(Clone, Default)]
    struct IdsProbe(Arc<Mutex<Vec<ProbedIds>>>);

    /// A span's name and the [`SpanIds`] in its extensions when the probe saw it.
    type ProbedIds = (String, Option<SpanIds>);

    impl<S> Layer<S> for IdsProbe
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: LayerContext<'_, S>) {
            let ids = ctx
                .span(id)
                .and_then(|s| s.extensions().get::<SpanIds>().copied());
            self.0
                .lock()
                .push((attrs.metadata().name().to_owned(), ids));
        }
    }

    impl IdsProbe {
        fn get(&self, name: &str) -> Option<SpanIds> {
            self.0
                .lock()
                .iter()
                .find(|(n, _)| n == name)
                .unwrap_or_else(|| panic!("no span {name}"))
                .1
        }
    }

    /// Runs `f` under `Registry + bridge + probe` and returns the probe.
    fn run(bridge: OtelBridge, f: impl FnOnce()) -> IdsProbe {
        let probe = IdsProbe::default();
        let sub = tracing_subscriber::registry()
            .with(bridge)
            .with(probe.clone());
        tracing::subscriber::with_default(sub, f);
        probe
    }

    fn ids_of(s: &SpanData) -> SpanIds {
        SpanIds {
            trace_id: s.span_context.trace_id().to_bytes(),
            span_id: s.span_context.span_id().to_bytes(),
        }
    }

    fn attr(s: &SpanData, key: &str) -> Option<Value> {
        s.attributes
            .iter()
            .find(|kv| kv.key.as_str() == key)
            .map(|kv| kv.value.clone())
    }

    /// An ADK span exports under Go's span name, parented under its nearest ADK ancestor across a
    /// non-ADK span, in the ADK scope, with its start and end times; a non-ADK span exports
    /// nothing; the started span's IDs are the `SpanIds` extension (spec R11, R12).
    #[test]
    fn spans_export_name_parent_scope_and_ids() {
        let e = Exporters::new();
        let probe = run(OtelBridge::new(Some(&e.tp), None), || {
            let root = tracing::info_span!("invoke_agent", otel.name = "invoke_agent a");
            let _r = root.enter();
            let foreign = tracing::info_span!("Connection", gen_ai.conversation.id = "x");
            let _f = foreign.enter();
            let _child = tracing::info_span!("generate_content", otel.name = "generate_content m");
        });
        let spans = e.spans();
        assert_eq!(spans.len(), 2, "{spans:?}");
        let root = e.span("invoke_agent a");
        let child = e.span("generate_content m");
        assert_eq!(root.parent_span_id, SpanId::INVALID);
        assert_eq!(child.parent_span_id, root.span_context.span_id());
        assert_eq!(child.span_context.trace_id(), root.span_context.trace_id());
        assert_ne!(root.span_context.trace_id(), TraceId::INVALID);
        assert!(root.start_time <= child.start_time);
        assert!(child.end_time <= root.end_time);
        assert_eq!(root.instrumentation_scope.name(), "gcp.vertex.agent");
        assert_eq!(
            root.instrumentation_scope.version(),
            Some(env!("CARGO_PKG_VERSION"))
        );
        assert_eq!(probe.get("invoke_agent"), Some(ids_of(&root)));
        assert_eq!(probe.get("generate_content"), Some(ids_of(&child)));
        assert_eq!(probe.get("Connection"), None);
    }

    /// An ADK span with no ADK ancestor starts a new trace.
    #[test]
    fn sibling_roots_start_separate_traces() {
        let e = Exporters::new();
        run(OtelBridge::new(Some(&e.tp), None), || {
            drop(tracing::info_span!("a", otel.name = "a"));
            drop(tracing::info_span!("b", otel.name = "b"));
        });
        let (a, b) = (e.span("a"), e.span("b"));
        assert_eq!(b.parent_span_id, SpanId::INVALID);
        assert_ne!(a.span_context.trace_id(), b.span_context.trace_id());
    }

    /// Each decoded D6 kind exports as its SDK value: structured values as JSON text, bytes
    /// that are not JSON as base64, a u64 above i64 as decimal text; attributes recorded later
    /// are set once with their last value; a recorded `otel.name` renames the span.
    #[test]
    fn attributes_are_typed() {
        let e = Exporters::new();
        run(OtelBridge::new(Some(&e.tp), None), || {
            let span = tracing::info_span!(
                "execute_tool",
                otel.name = "execute_tool f",
                s = "x",
                i = -3_i64,
                u = 7_u64,
                u_big = u64::MAX,
                f = 0.5_f64,
                b = true,
                strs = br#"["a","b"]"#.as_slice(),
                map = br#"{"k":[1,null]}"#.as_slice(),
                raw = b"\xff\x00".as_slice(),
                late = tracing::field::Empty,
            );
            span.record("late", "first");
            span.record("late", "second");
            span.record("otel.name", "execute_tool g");
        });
        let s = e.span("execute_tool g");
        assert_eq!(attr(&s, "s"), Some(Value::from("x")));
        assert_eq!(attr(&s, "i"), Some(Value::I64(-3)));
        assert_eq!(attr(&s, "u"), Some(Value::I64(7)));
        assert_eq!(attr(&s, "u_big"), Some(Value::from(u64::MAX.to_string())));
        assert_eq!(attr(&s, "f"), Some(Value::F64(0.5)));
        assert_eq!(attr(&s, "b"), Some(Value::Bool(true)));
        assert_eq!(
            attr(&s, "strs"),
            Some(Value::Array(Array::String(vec!["a".into(), "b".into()])))
        );
        assert_eq!(attr(&s, "map"), Some(Value::from(r#"{"k":[1,null]}"#)));
        assert_eq!(attr(&s, "raw"), Some(Value::from("/wA=")));
        assert_eq!(attr(&s, "late"), Some(Value::from("second")));
        let keys: Vec<&str> = s.attributes.iter().map(|kv| kv.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "s", "i", "u", "u_big", "f", "b", "strs", "map", "raw", "late"
            ]
        );
    }

    /// The status is the last recorded `otel.status_code` with `error.message` as an error's
    /// description; none recorded is unset.
    #[test]
    fn status_from_d6_fields() {
        let e = Exporters::new();
        run(OtelBridge::new(Some(&e.tp), None), || {
            let failed = tracing::info_span!(
                "a",
                otel.name = "failed",
                otel.status_code = tracing::field::Empty,
                error.message = tracing::field::Empty,
            );
            failed.record("otel.status_code", "ERROR");
            failed.record("error.message", "boom");
            drop(tracing::info_span!(
                "b",
                otel.name = "ok",
                otel.status_code = "OK"
            ));
            drop(tracing::info_span!("c", otel.name = "unset"));
        });
        assert_eq!(e.span("failed").status, Status::error("boom"));
        assert_eq!(e.span("ok").status, Status::Ok);
        assert_eq!(e.span("unset").status, Status::Unset);
        assert!(e.span("failed").attributes.is_empty());
    }

    /// A log event on `LOG_TARGET` exports as a log record: interned event name, Go's severity
    /// (DEBUG set, INFO unset), body and attributes as `AnyValue` trees (maps and lists kept,
    /// nulls dropped), and the trace context of its nearest ADK span; other events export
    /// nothing (spec R13).
    #[test]
    fn log_records_export() {
        let e = Exporters::new();
        run(OtelBridge::new(Some(&e.tp), Some(&e.lp)), || {
            let root = tracing::info_span!("invoke_agent", otel.name = "invoke_agent a");
            let _r = root.enter();
            let foreign = tracing::info_span!("Connection");
            let _f = foreign.enter();
            tracing::event!(
                target: LOG,
                tracing::Level::DEBUG,
                event.name = "gen_ai.client.inference.operation.details",
                otel.log.body = "null",
                otel.log.attributes = r#"{"n":1,"big":18446744073709551615,"l":["x",null],"z":null}"#
            );
            tracing::event!(
                target: LOG,
                tracing::Level::INFO,
                event.name = "gen_ai.choice",
                otel.log.body = r#"{"index":0,"content":{"parts":[{"text":"hi"}],"role":"model"},"f":0.5}"#
            );
            tracing::info!(event.name = "other", "not a log record");
        });
        let root = e.span("invoke_agent a");
        let logs = e.logs();
        assert_eq!(logs.len(), 2);
        let (debug, info) = (&logs[0], &logs[1]);
        assert_eq!(
            debug.event_name(),
            Some("gen_ai.client.inference.operation.details")
        );
        assert_eq!(debug.severity_number(), Some(Severity::Debug));
        assert_eq!(debug.severity_text(), None);
        assert_eq!(debug.body(), None);
        let attrs: std::collections::HashMap<String, AnyValue> = debug
            .attributes_iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        let want: std::collections::HashMap<String, AnyValue> = [
            ("n", AnyValue::Int(1)),
            ("big", AnyValue::Double(1.844_674_407_370_955_2e19)),
            (
                "l",
                AnyValue::ListAny(Box::new(vec![AnyValue::String("x".into())])),
            ),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect();
        assert_eq!(attrs, want);
        assert_eq!(info.event_name(), Some("gen_ai.choice"));
        assert_eq!(info.severity_number(), None);
        assert_eq!(info.attributes_iter().count(), 0);
        let content = AnyValue::Map(Box::new(
            [
                (
                    Key::from("parts"),
                    AnyValue::ListAny(Box::new(vec![AnyValue::Map(Box::new(
                        [(Key::from("text"), AnyValue::String("hi".into()))].into(),
                    ))])),
                ),
                (Key::from("role"), AnyValue::String("model".into())),
            ]
            .into(),
        ));
        let body = AnyValue::Map(Box::new(
            [
                (Key::from("index"), AnyValue::Int(0)),
                (Key::from("content"), content),
                (Key::from("f"), AnyValue::Double(0.5)),
            ]
            .into(),
        ));
        assert_eq!(info.body(), Some(&body));
        for r in [debug, info] {
            let tc = r.trace_context().expect("trace context");
            assert_eq!(tc.trace_id, root.span_context.trace_id());
            assert_eq!(tc.span_id, root.span_context.span_id());
            assert_eq!(tc.trace_flags, Some(root.span_context.trace_flags()));
        }
    }

    /// A log record outside any ADK span carries no trace context; without a tracer provider
    /// records still export (no trace context) and spans get no IDs.
    #[test]
    fn logs_without_tracer_or_span() {
        let e = Exporters::new();
        let probe = run(OtelBridge::new(None, Some(&e.lp)), || {
            tracing::event!(target: LOG, tracing::Level::INFO, event.name = "outside", otel.log.body = "1");
            let _s = tracing::info_span!("a", otel.name = "a").entered();
            tracing::event!(target: LOG, tracing::Level::INFO, event.name = "inside", otel.log.body = "1");
        });
        let logs = e.logs();
        assert_eq!(logs.len(), 2);
        assert!(logs.iter().all(|r| r.trace_context().is_none()));
        assert_eq!(logs[0].body(), Some(&AnyValue::Int(1)));
        assert_eq!(probe.get("a"), None);
        assert!(e.spans().is_empty());
    }

    /// With neither provider the layer records nothing.
    #[test]
    fn no_providers_record_nothing() {
        let e = Exporters::new();
        let probe = run(OtelBridge::new(None, None), || {
            let _s = tracing::info_span!("a", otel.name = "a").entered();
            tracing::event!(target: LOG, tracing::Level::INFO, event.name = "e", otel.log.body = "1");
        });
        assert_eq!(probe.get("a"), None);
        assert!(e.spans().is_empty());
        assert!(e.logs().is_empty());
    }

    /// After provider shutdown the SDK hands out the all-zero span context: the stored IDs are
    /// invalid (the debug store then generates its own) and nothing exports.
    #[test]
    fn shut_down_provider_gives_invalid_ids() {
        let e = Exporters::new();
        e.tp.shutdown().unwrap();
        let probe = run(OtelBridge::new(Some(&e.tp), None), || {
            drop(tracing::info_span!("a", otel.name = "a"));
        });
        let ids = probe.get("a").expect("ids");
        assert!(!ids.is_valid());
        assert!(e.spans().is_empty());
    }

    /// Event names intern once.
    #[test]
    fn event_names_are_interned() {
        let a = intern("gen_ai.user.message");
        let b = intern(&String::from("gen_ai.user.message"));
        assert_eq!(a, "gen_ai.user.message");
        assert!(std::ptr::eq(a, b));
    }

    /// JSON numbers follow Go's `toLogValue`; a top-level null is no value.
    #[test]
    fn json_any_values() {
        assert_eq!(any_value(Json::Null), None);
        assert_eq!(any_value(json!(-2)), Some(AnyValue::Int(-2)));
        assert_eq!(any_value(json!(1.0)), Some(AnyValue::Double(1.0)));
        assert_eq!(any_value(json!(true)), Some(AnyValue::Boolean(true)));
        assert_eq!(
            any_value(json!([null])),
            Some(AnyValue::ListAny(Box::default()))
        );
    }

    /// Go `base64.StdEncoding` vectors.
    #[test]
    fn base64_matches_go() {
        let cases: [(&[u8], &str); 7] = [
            (b"", ""),
            (b"f", "Zg=="),
            (b"fo", "Zm8="),
            (b"foo", "Zm9v"),
            (b"foob", "Zm9vYg=="),
            (b"fooba", "Zm9vYmE="),
            (b"\xff\xfe\xfd", "//79"),
        ];
        for (input, want) in cases {
            assert_eq!(base64(input), want, "{input:?}");
        }
    }
}
