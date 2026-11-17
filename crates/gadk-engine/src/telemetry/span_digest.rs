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

//! The span tree a telemetry test compares, with each log record attached to the span it was
//! emitted under. Spans and log records are captured from `tracing` instead of an OTel SDK:
//! [`SpanStub`] and [`LogRecord`] are captured spans and records reduced to what a digest reads.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tracing::span::{Attributes, Id, Record};
use tracing_subscriber::layer::{Context as LayerContext, Layer, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;

use super::d6::{self, D6Fields};
use super::log_digest::{LogDigest, LogRecord, build_log_digest};

/// Stands in for a value that cannot be pinned (a generated ID).
pub const PRESENT: &str = "PRESENT";

/// Attributes whose value changes run to run.
pub(crate) const NON_DETERMINISTIC_ATTRIBUTES: &[&str] = &[
    "gcp.vertex.agent.event_id",
    "gen_ai.tool.call.id",
    "gen_ai.conversation.id",
    "gcp.vertex.agent.invocation_id",
];

/// Span attributes holding a JSON string; a digest shows one parsed, wrapped as
/// `{"JSON_STRING": value}`.
pub(crate) const JSON_STRING_ATTRIBUTES: &[&str] = &[
    "gen_ai.input.messages",
    "gen_ai.output.messages",
    "gen_ai.system_instructions",
    "gcp.vertex.agent.tool_call_args",
    "gcp.vertex.agent.tool_response",
];

/// The key a parsed JSON-string attribute is wrapped under.
pub(crate) const JSON_STRING_KEY: &str = "JSON_STRING";

/// One captured span, reduced to what a digest reads.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SpanStub {
    /// The span's `otel.name` (its full span name).
    pub name: String,
    /// An ID unique within one capture.
    pub span_id: u64,
    /// The parent's span ID; `None` for a span without a parent.
    pub parent_span_id: Option<u64>,
    /// Orders siblings; a capture records a strictly increasing sequence number.
    pub start_time: i64,
    /// The span's OTel attributes in recording order (every recorded field except `otel.*` and
    /// `error.message`; str → string, i64/u64 → integer, f64 → float, bool → bool).
    pub attributes: Vec<(String, Value)>,
    /// The status: `UNSET`, `ERROR` (`otel.status_code` recorded as `ERROR`) or `OK`.
    pub status: String,
}

/// What a golden compares of one scenario run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Digest {
    /// The root span.
    pub root_span: SpanDigest,
}

/// A deterministic snapshot of one span.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpanDigest {
    /// The span name.
    pub name: String,
    /// Normalized attributes (sorted by key).
    pub attributes: BTreeMap<String, Value>,
    /// `UNSET`, `ERROR` or `OK`.
    pub status: String,
    /// Child spans, by start time (name as tiebreaker).
    pub children: Vec<SpanDigest>,
    /// The log records emitted under this span, in emit order.
    pub logs: Vec<LogDigest>,
}

/// Collects `spans` and `logs` into one tree. Fails when a log record's span was not collected
/// (`log record "<name>" was emitted outside any collected span`) or when there is not exactly one
/// root span (`expected exactly 1 root span, got <n>`).
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

/// Links each span to its parent, orders siblings by start time (name as tiebreaker), and returns
/// the spans whose parent was not collected.
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
/// [`capture_logs`](super::log_digest::capture_logs): spans (fields decoded by [`D6Fields`]), and
/// the log events ([`d6::decode_log`]) as [`LogRecord`]s attached to their span.
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

/// Merges one visit's decoded fields into `stub`: a recorded name or status replaces the current
/// one, an attribute recorded again keeps its position and takes the new value (as JSON,
/// [`d6::D6Value::to_json`]).
fn apply(stub: &mut SpanStub, fields: D6Fields) {
    if let Some(name) = fields.name {
        stub.name = name;
    }
    if let Some(code) = fields.status_code {
        stub.status = code.as_str().to_owned();
    }
    for (key, value) in fields.attributes {
        let value = value.to_json();
        match stub.attributes.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = value,
            None => stub.attributes.push((key, value)),
        }
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
        let mut fields = D6Fields::default();
        attrs.record(&mut fields);
        apply(&mut stub, fields);
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
        let mut fields = D6Fields::default();
        values.record(&mut fields);
        if let Some(stub) = self.0.lock().spans.get_mut(i) {
            apply(stub, fields);
        }
    }

    fn on_event(&self, event: &tracing::Event<'_>, ctx: LayerContext<'_, S>) {
        let Some(log) = d6::decode_log(event) else {
            return;
        };
        let span_id = ctx
            .event_span(event)
            .and_then(|s| s.extensions().get::<Idx>().map(|i| i.0 as u64 + 1));
        self.0.lock().logs.push(LogRecord {
            event_name: log.event_name,
            span_id,
            body: log.body.unwrap_or(Value::Null),
            attributes: log
                .attributes
                .map(|m| m.into_iter().collect())
                .unwrap_or_default(),
        });
    }
}

/// JSON-string attributes parsed and wrapped as `{"JSON_STRING": value}` (a string that is not JSON
/// is kept), then [`normalize_attribute`].
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

/// [`PRESENT`] for a non-deterministic attribute, else `value`.
pub(crate) fn normalize_attribute(key: &str, value: Value) -> Value {
    if NON_DETERMINISTIC_ATTRIBUTES.contains(&key) {
        return Value::String(PRESENT.to_owned());
    }
    value
}
