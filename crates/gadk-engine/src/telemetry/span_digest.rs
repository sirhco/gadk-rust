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

//! Port of Go `internal/telemetry/telemetrytest/span_digest.go`: the span tree a telemetry test
//! compares, with each log record attached to the span it was emitted under. Spans and log
//! records are captured from `tracing` (plan decision D6) instead of an OTel SDK: [`SpanStub`] is
//! Go's `tracetest.SpanStub` and [`LogRecord`] Go's `sdklog.Record`, reduced to what a digest
//! reads.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing_subscriber::layer::{Context as LayerContext, Layer, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;

use super::LOG_TARGET;
use super::log_digest::{LogDigest, LogRecord, build_log_digest};

/// Go `present`: stands in for a value that cannot be pinned (a generated ID).
pub const PRESENT: &str = "PRESENT";

/// Go `nonDeterministicAttributes`: attributes whose value changes run to run.
pub(crate) const NON_DETERMINISTIC_ATTRIBUTES: &[&str] = &[
    "gcp.vertex.agent.event_id",
    "gen_ai.tool.call.id",
    "gen_ai.conversation.id",
    "gcp.vertex.agent.invocation_id",
];

/// Go `jsonStringAttributes`: span attributes holding a JSON string; a digest shows one parsed,
/// wrapped as `{"JSON_STRING": value}`.
pub(crate) const JSON_STRING_ATTRIBUTES: &[&str] = &[
    "gen_ai.input.messages",
    "gen_ai.output.messages",
    "gen_ai.system_instructions",
    "gcp.vertex.agent.tool_call_args",
    "gcp.vertex.agent.tool_response",
];

/// Go `jsonStringKey`.
pub(crate) const JSON_STRING_KEY: &str = "JSON_STRING";

/// One captured span (Go `tracetest.SpanStub`, reduced to what a digest reads).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SpanStub {
    /// Go `SpanStub.Name`: the span's `otel.name` (Go's full span name).
    pub name: String,
    /// Go `SpanContext.SpanID()`: an ID unique within one capture.
    pub span_id: u64,
    /// Go `Parent.SpanID()`; `None` for a span without a parent.
    pub parent_span_id: Option<u64>,
    /// Go `StartTime.UnixNano()`: orders siblings; a capture records a strictly increasing
    /// sequence number.
    pub start_time: i64,
    /// The span's OTel attributes in recording order (D6: every recorded field except `otel.*`
    /// and `error.message`; str → string, i64/u64 → integer, f64 → float, bool → bool).
    pub attributes: Vec<(String, Value)>,
    /// Go `strings.ToUpper(Status.Code.String())`: `UNSET`, `ERROR` (`otel.status_code` recorded
    /// as `ERROR`) or `OK`.
    pub status: String,
}

/// Go `telemetrytest.Digest`: what a golden compares of one scenario run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Digest {
    /// Go `RootSpan` (`root_span`).
    pub root_span: SpanDigest,
}

/// Go `telemetrytest.SpanDigest`: a deterministic snapshot of one span.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpanDigest {
    /// The span name.
    pub name: String,
    /// Normalized attributes (Go `map[string]any`, so sorted by key on the wire).
    pub attributes: BTreeMap<String, Value>,
    /// `UNSET`, `ERROR` or `OK`.
    pub status: String,
    /// Child spans, by start time (name as tiebreaker).
    pub children: Vec<SpanDigest>,
    /// The log records emitted under this span, in emit order.
    pub logs: Vec<LogDigest>,
}

/// Go `BuildDigest`: collects `spans` and `logs` into one tree. Fails (Go's error text) when a log
/// record's span was not collected (`log record "<name>" was emitted outside any collected span`)
/// or when there is not exactly one root span (`expected exactly 1 root span, got <n>`).
pub fn build_digest(spans: &[SpanStub], logs: &[LogRecord]) -> Result<Digest, String> {
    let mut digests: Vec<SpanDigest> = spans
        .iter()
        .map(|s| SpanDigest {
            name: s.name.clone(),
            attributes: normalize_span_attributes(&s.attributes),
            status: s.status.clone(),
            children: Vec::new(),
            logs: Vec::new(),
        })
        .collect();
    let by_span_id: HashMap<u64, usize> = spans
        .iter()
        .enumerate()
        .map(|(i, s)| (s.span_id, i))
        .collect();
    for r in logs {
        let Some(&i) = r.span_id.and_then(|id| by_span_id.get(&id)) else {
            return Err(format!(
                "log record {} was emitted outside any collected span",
                gadk_platform::go_quote(&r.event_name)
            ));
        };
        digests[i].logs.push(build_log_digest(r));
    }
    let mut roots = link_and_sort(spans, digests, &by_span_id);
    if roots.len() != 1 {
        return Err(format!("expected exactly 1 root span, got {}", roots.len()));
    }
    Ok(Digest {
        root_span: roots.remove(0),
    })
}

/// Go `linkAndSort`: links each span to its parent, orders siblings by start time (name as
/// tiebreaker), and returns the spans whose parent was not collected.
fn link_and_sort(
    spans: &[SpanStub],
    digests: Vec<SpanDigest>,
    by_span_id: &HashMap<u64, usize>,
) -> Vec<SpanDigest> {
    let mut order: Vec<usize> = (0..spans.len()).collect();
    order.sort_by(|&a, &b| {
        spans[a]
            .start_time
            .cmp(&spans[b].start_time)
            .then_with(|| spans[a].name.cmp(&spans[b].name))
    });
    let parent_of = |i: usize| -> Option<usize> {
        spans[i]
            .parent_span_id
            .and_then(|p| by_span_id.get(&p).copied())
    };
    // Children in sibling order, per parent index.
    let mut children: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut roots = Vec::new();
    for &i in &order {
        match parent_of(i) {
            Some(p) => children.entry(p).or_default().push(i),
            None => roots.push(i),
        }
    }
    let mut slots: Vec<Option<SpanDigest>> = digests.into_iter().map(Some).collect();
    // Each slot is taken once, so a malformed parent cycle cannot recurse forever.
    fn build(
        i: usize,
        slots: &mut [Option<SpanDigest>],
        children: &HashMap<usize, Vec<usize>>,
    ) -> Option<SpanDigest> {
        let mut d = slots.get_mut(i)?.take()?;
        for &c in children.get(&i).map(Vec::as_slice).unwrap_or_default() {
            if let Some(child) = build(c, slots, children) {
                d.children.push(child);
            }
        }
        Some(d)
    }
    roots
        .into_iter()
        .filter_map(|r| build(r, &mut slots, &children))
        .collect()
}

/// Runs `fut` with a recording subscriber as the thread default (use a current-thread runtime)
/// and returns its output and the spans it opened.
pub async fn capture_spans<F: Future>(fut: F) -> (F::Output, Vec<SpanStub>) {
    let (out, spans, _logs) = capture(fut).await;
    (out, spans)
}

/// The one recording subscriber behind [`capture_spans`] and
/// [`capture_logs`](super::log_digest::capture_logs): spans, and the D6 log events (target
/// `gadk_engine::telemetry::log`, fields `event.name`, `otel.log.body`, `otel.log.attributes`)
/// as [`LogRecord`]s attached to their span.
pub(crate) async fn capture<F: Future>(fut: F) -> (F::Output, Vec<SpanStub>, Vec<LogRecord>) {
    pin_tracing_interest();
    let store: Arc<Mutex<Store>> = Arc::default();
    let sub = tracing_subscriber::registry().with(CaptureLayer(store.clone()));
    let out = {
        let _g = tracing::subscriber::set_default(sub);
        fut.await
    };
    let store = std::mem::take(&mut *store.lock());
    (out, store.spans, store.logs)
}

/// Installs (once per process) a no-op global `Registry`, so the global max-level hint and
/// callsite interest never collapse to "off" while thread-default capture subscribers come and
/// go on parallel test threads (a concurrent interest rebuild on another thread, which does not
/// see this thread's default, could otherwise disable every span macro).
pub(crate) fn pin_tracing_interest() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = tracing::subscriber::set_global_default(tracing_subscriber::registry());
    });
}

#[derive(Default)]
struct Store {
    spans: Vec<SpanStub>,
    logs: Vec<LogRecord>,
}

struct CaptureLayer(Arc<Mutex<Store>>);

/// The index of a span in the captured list (in the span's extensions).
struct Idx(usize);

/// Reads span fields as D6 attributes: `otel.name` and `otel.status_code` set the name and
/// status, `error.message` (the status description) and other `otel.*` fields are not
/// attributes, a byte field is a structured value's JSON text.
struct SpanFields<'a> {
    stub: &'a mut SpanStub,
}

impl SpanFields<'_> {
    fn set(&mut self, f: &Field, v: Value) {
        let name = f.name();
        match name {
            "otel.name" => {
                if let Value::String(s) = v {
                    self.stub.name = s;
                }
            }
            "otel.status_code" => {
                self.stub.status = match v.as_str() {
                    Some("ERROR") => "ERROR",
                    Some("OK") => "OK",
                    _ => "UNSET",
                }
                .to_owned();
            }
            "error.message" => {}
            n if n.starts_with("otel.") => {}
            _ => match self.stub.attributes.iter_mut().find(|(k, _)| k == name) {
                Some(slot) => slot.1 = v,
                None => self.stub.attributes.push((name.to_owned(), v)),
            },
        }
    }
}

impl Visit for SpanFields<'_> {
    fn record_str(&mut self, f: &Field, v: &str) {
        self.set(f, Value::String(v.to_owned()));
    }
    fn record_bool(&mut self, f: &Field, v: bool) {
        self.set(f, Value::Bool(v));
    }
    fn record_i64(&mut self, f: &Field, v: i64) {
        self.set(f, Value::from(v));
    }
    fn record_u64(&mut self, f: &Field, v: u64) {
        self.set(f, Value::from(v));
    }
    fn record_f64(&mut self, f: &Field, v: f64) {
        self.set(
            f,
            serde_json::Number::from_f64(v).map_or(Value::Null, Value::Number),
        );
    }
    fn record_bytes(&mut self, f: &Field, v: &[u8]) {
        self.set(f, serde_json::from_slice(v).unwrap_or(Value::Null));
    }
    fn record_debug(&mut self, f: &Field, v: &dyn std::fmt::Debug) {
        self.set(f, Value::String(format!("{v:?}")));
    }
}

/// Reads the fields of a D6 log event.
#[derive(Default)]
struct LogFields {
    event_name: String,
    body: Value,
    attributes: Vec<(String, Value)>,
}

impl Visit for LogFields {
    fn record_str(&mut self, f: &Field, v: &str) {
        match f.name() {
            "event.name" => self.event_name = v.to_owned(),
            "otel.log.body" => self.body = serde_json::from_str(v).unwrap_or(Value::Null),
            "otel.log.attributes" => {
                if let Ok(Value::Object(m)) = serde_json::from_str::<Value>(v) {
                    self.attributes = m.into_iter().collect();
                }
            }
            _ => {}
        }
    }
    fn record_debug(&mut self, f: &Field, v: &dyn std::fmt::Debug) {
        self.record_str(f, &format!("{v:?}"));
    }
}

impl CaptureLayer {
    fn index_of<S>(id: &Id, ctx: &LayerContext<'_, S>) -> Option<usize>
    where
        S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    {
        ctx.span(id)?.extensions().get::<Idx>().map(|i| i.0)
    }
}

impl<S> Layer<S> for CaptureLayer
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: LayerContext<'_, S>) {
        let parent = ctx
            .span(id)
            .and_then(|s| s.parent())
            .and_then(|p| p.extensions().get::<Idx>().map(|i| i.0 as u64 + 1));
        let mut store = self.0.lock();
        let i = store.spans.len();
        let mut stub = SpanStub {
            name: attrs.metadata().name().to_owned(),
            span_id: i as u64 + 1,
            parent_span_id: parent,
            start_time: i as i64,
            attributes: Vec::new(),
            status: "UNSET".to_owned(),
        };
        attrs.record(&mut SpanFields { stub: &mut stub });
        store.spans.push(stub);
        drop(store);
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(Idx(i));
        }
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: LayerContext<'_, S>) {
        let Some(i) = Self::index_of(id, &ctx) else {
            return;
        };
        if let Some(stub) = self.0.lock().spans.get_mut(i) {
            values.record(&mut SpanFields { stub });
        }
    }

    fn on_event(&self, event: &tracing::Event<'_>, ctx: LayerContext<'_, S>) {
        if event.metadata().target() != LOG_TARGET {
            return;
        }
        let mut fields = LogFields::default();
        event.record(&mut fields);
        let span_id = ctx
            .event_span(event)
            .and_then(|s| s.extensions().get::<Idx>().map(|i| i.0 as u64 + 1));
        self.0.lock().logs.push(LogRecord {
            event_name: fields.event_name,
            span_id,
            body: fields.body,
            attributes: fields.attributes,
        });
    }
}

/// Go `normalizeSpanAttributes`: JSON-string attributes parsed and wrapped as
/// `{"JSON_STRING": value}` (a string that is not JSON is kept), then [`normalize_attribute`].
pub(crate) fn normalize_span_attributes(attrs: &[(String, Value)]) -> BTreeMap<String, Value> {
    attrs
        .iter()
        .map(|(key, value)| {
            let mut value = value.clone();
            if let Value::String(s) = &value
                && JSON_STRING_ATTRIBUTES.contains(&key.as_str())
                && let Ok(parsed) = serde_json::from_str::<Value>(s)
            {
                value = Value::Object(Map::from_iter([(JSON_STRING_KEY.to_owned(), parsed)]));
            }
            (key.clone(), normalize_attribute(key, value))
        })
        .collect()
}

/// Go `normalizeAttribute`: [`PRESENT`] for a non-deterministic attribute, else `value`.
pub(crate) fn normalize_attribute(key: &str, value: Value) -> Value {
    if NON_DETERMINISTIC_ATTRIBUTES.contains(&key) {
        return Value::String(PRESENT.to_owned());
    }
    value
}
