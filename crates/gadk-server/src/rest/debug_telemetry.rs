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

//! Port of Go `server/adkrest/internal/services/debugtelemetry.go`: a `tracing` layer and span
//! store that serve the debug trace endpoints.
//!
//! Go registers the store as an OTel span processor and log processor; gadk's telemetry is
//! `tracing`, so [`DebugTelemetry`] is a [`tracing_subscriber::Layer`] reading the engine's spans
//! and log events as plan decision D6 lays them out:
//! - a span's Go name is its `otel.name` field, else its static name; every other field is an
//!   OTel attribute (str → STRING, i64/u64 → INT64, f64 → FLOAT64, bool → BOOL, bytes → the JSON
//!   text of a structured value, an all-string array being Go's STRINGSLICE), except the other
//!   `otel.*` fields and `error.message` (the span status and its description);
//! - an OTel log record is an event with target `gadk_engine::telemetry::log` and fields
//!   `event.name`, `otel.log.body` (JSON text) and `otel.log.attributes` (JSON object text),
//!   attached to the span it is emitted under.
//!
//! Trace and span IDs are random (16 and 8 bytes, lowercase hex) as the OTel SDK's; a span keeps
//! its parent's trace ID, a root span (`parent_span_id` sixteen zeros) starts a trace. Times are
//! wall-clock Unix nanoseconds. A span is stored when it closes (Go: when it ends), with the log
//! records emitted under it; the store keeps the most recently used `trace_capacity` traces.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use chrono::{DateTime, FixedOffset, Local, Timelike};
use gadk_platform::go_sprint_value;
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{Map, Number, Value};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing_subscriber::layer::Context as LayerContext;
use tracing_subscriber::registry::LookupSpan;

use super::DebugTelemetryConfig;

/// Go `defaultTraceCapacity`.
pub(crate) const DEFAULT_TRACE_CAPACITY: usize = 10_000;

/// The `tracing` target of the engine's OTel log records (plan decision D6; gadk-engine's
/// `telemetry::LOG_TARGET`).
const LOG_TARGET: &str = "gadk_engine::telemetry::log";

/// Go `eventIDKey` / `eventIDAttribute`.
pub(crate) const EVENT_ID_ATTRIBUTE: &str = "gcp.vertex.agent.event_id";
/// Go `semconv.GenAIConversationIDKey`: the attribute the session index reads.
pub(crate) const CONVERSATION_ID_ATTRIBUTE: &str = "gen_ai.conversation.id";
/// Go `generateContentSpanPrefix`.
const GENERATE_CONTENT_SPAN_PREFIX: &str = "generate_content";

/// Go `userMessageEventName`.
const USER_MESSAGE_EVENT_NAME: &str = "gen_ai.user.message";
/// Go `choiceEventName`.
const CHOICE_EVENT_NAME: &str = "gen_ai.choice";
/// Go `inferenceDetailsEventName`.
const INFERENCE_DETAILS_EVENT_NAME: &str = "gen_ai.client.inference.operation.details";
/// Go `inputMessagesKey`.
const INPUT_MESSAGES_KEY: &str = "gen_ai.input.messages";
/// Go `outputMessagesKey`.
const OUTPUT_MESSAGES_KEY: &str = "gen_ai.output.messages";
/// Go `systemInstructionsKey`.
const SYSTEM_INSTRUCTIONS_KEY: &str = "gen_ai.system_instructions";

/// Go `contentKey`.
const CONTENT_KEY: &str = "content";
/// Go `partsKey`.
const PARTS_KEY: &str = "parts";
/// Go `textKey`.
const TEXT_KEY: &str = "text";
/// Go `roleKey`.
const ROLE_KEY: &str = "role";
/// Go `userRole`.
const USER_ROLE: &str = "user";
/// Go `modelRole`.
const MODEL_ROLE: &str = "model";
/// Go `unrepresentableContent`.
const UNREPRESENTABLE_CONTENT: &str = "<unrepresentable>";

/// Go's zero `time.Time` as `UnixNano()` (an overflowed value Go returns as is): the start and
/// end of a [`SpanRecord`] that never had times.
pub(crate) const GO_ZERO_TIME_UNIX_NANO: i64 = -6_795_364_578_871_345_152;

/// Go `spanRecord`: a closed span and its logs.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SpanRecord {
    pub(crate) name: String,
    pub(crate) start_time: i64,
    pub(crate) end_time: i64,
    pub(crate) trace_id: [u8; 16],
    pub(crate) span_id: [u8; 8],
    pub(crate) parent_span_id: [u8; 8],
    pub(crate) attributes: BTreeMap<String, String>,
    pub(crate) logs: Vec<DebugLog>,
}

impl Default for SpanRecord {
    /// Go's zero `spanRecord`: zero times, invalid (all-zero) IDs, no attributes or logs.
    fn default() -> Self {
        Self {
            name: String::new(),
            start_time: GO_ZERO_TIME_UNIX_NANO,
            end_time: GO_ZERO_TIME_UNIX_NANO,
            trace_id: [0; 16],
            span_id: [0; 8],
            parent_span_id: [0; 8],
            attributes: BTreeMap::new(),
            logs: Vec::new(),
        }
    }
}

/// Go `spanStore`, behind one lock. Go's `recordsBySpanID` (logs arriving before their span is
/// exported) is the open span's state in the `tracing` registry here.
pub(crate) struct Store {
    inner: Mutex<StoreInner>,
}

struct StoreInner {
    /// Go `recordsByTraceID`: the trace LRU.
    traces: TraceLru,
    /// Go `traceIDsBySessionID`.
    trace_ids_by_session_id: HashMap<String, BTreeSet<String>>,
    /// Go `recordsByEventID`.
    records_by_event_id: HashMap<String, Vec<Arc<SpanRecord>>>,
}

/// Go's `lru.Cache[string, []*spanRecord]` (hashicorp/golang-lru): `get` marks a trace most
/// recently used; adding a trace beyond the capacity evicts the least recently used one.
struct TraceLru {
    capacity: usize,
    tick: u64,
    entries: HashMap<String, (u64, Vec<Arc<SpanRecord>>)>,
    order: BTreeMap<u64, String>,
}

impl TraceLru {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            tick: 0,
            entries: HashMap::new(),
            order: BTreeMap::new(),
        }
    }

    fn next_tick(&mut self) -> u64 {
        self.tick += 1;
        self.tick
    }

    /// Go `Get`: the trace's records, marking it most recently used.
    fn get(&mut self, trace_id: &str) -> Option<Vec<Arc<SpanRecord>>> {
        let tick = self.next_tick();
        let (used, records) = self.entries.get_mut(trace_id)?;
        self.order.remove(used);
        *used = tick;
        self.order.insert(tick, trace_id.to_owned());
        Some(records.clone())
    }

    /// Go `Get` then `Add(traceID, append(records, record))`: appends `record` to its trace and
    /// marks the trace most recently used; a new trace beyond the capacity evicts the least
    /// recently used one, which is returned.
    fn push(
        &mut self,
        trace_id: &str,
        record: Arc<SpanRecord>,
    ) -> Option<(String, Vec<Arc<SpanRecord>>)> {
        let tick = self.next_tick();
        if let Some((used, records)) = self.entries.get_mut(trace_id) {
            self.order.remove(used);
            *used = tick;
            records.push(record);
            self.order.insert(tick, trace_id.to_owned());
            return None;
        }
        self.entries
            .insert(trace_id.to_owned(), (tick, vec![record]));
        self.order.insert(tick, trace_id.to_owned());
        if self.entries.len() <= self.capacity {
            return None;
        }
        let (_, oldest) = self.order.pop_first()?;
        let (_, records) = self.entries.remove(&oldest)?;
        Some((oldest, records))
    }
}

impl Store {
    fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(StoreInner {
                traces: TraceLru::new(capacity),
                trace_ids_by_session_id: HashMap::new(),
                records_by_event_id: HashMap::new(),
            }),
        }
    }

    /// Go `ExportSpans` for one span: stores `record` and indexes it (Go `updateSpanIndexes`).
    pub(crate) fn export(&self, record: SpanRecord) {
        let record = Arc::new(record);
        let trace_id = hex(&record.trace_id);
        let mut inner = self.inner.lock();
        if let Some(session_id) = record.attributes.get(CONVERSATION_ID_ATTRIBUTE) {
            inner
                .trace_ids_by_session_id
                .entry(session_id.clone())
                .or_default()
                .insert(trace_id.clone());
        }
        if let Some(event_id) = record.attributes.get(EVENT_ID_ATTRIBUTE) {
            inner
                .records_by_event_id
                .entry(event_id.clone())
                .or_default()
                .push(record.clone());
        }
        if let Some((evicted_trace, records)) = inner.traces.push(&trace_id, record) {
            inner.evict(&evicted_trace, &records);
        }
    }

    /// Go `getSpansByEventID`.
    fn spans_by_event_id(&self, event_id: &str) -> Vec<DebugSpan> {
        let mut inner = self.inner.lock();
        let records = inner
            .records_by_event_id
            .get(event_id)
            .cloned()
            .unwrap_or_default();
        inner.touch_traces(&records);
        drop(inner);
        convert_records(records)
    }

    /// Go `getSpansBySessionID`.
    fn spans_by_session_id(&self, session_id: &str) -> Vec<DebugSpan> {
        let mut inner = self.inner.lock();
        let traces: Vec<String> = inner
            .trace_ids_by_session_id
            .get(session_id)
            .map(|t| t.iter().cloned().collect())
            .unwrap_or_default();
        let mut records = Vec::new();
        for trace_id in traces {
            if let Some(trace_records) = inner.traces.get(&trace_id) {
                records.extend(trace_records);
            }
        }
        drop(inner);
        convert_records(records)
    }
}

impl StoreInner {
    /// Go `touchTraces`: fetching by event ID bypasses the trace LRU, so each trace the records
    /// belong to is marked recently used once.
    fn touch_traces(&mut self, records: &[Arc<SpanRecord>]) {
        let mut touched = BTreeSet::new();
        for r in records {
            let trace_id = hex(&r.trace_id);
            if touched.insert(trace_id.clone()) {
                let _ = self.traces.get(&trace_id);
            }
        }
    }

    /// Go `evict`: drops an evicted trace's spans from the event and session indexes.
    fn evict(&mut self, trace_id: &str, records: &[Arc<SpanRecord>]) {
        for span in records {
            if !is_valid_trace_id(&span.trace_id) {
                continue;
            }
            if let Some(event_id) = span.attributes.get(EVENT_ID_ATTRIBUTE) {
                // Go `evictRecordsByEventID`.
                if let Some(list) = self.records_by_event_id.get_mut(event_id) {
                    list.retain(|r| r.span_id != span.span_id);
                    if list.is_empty() {
                        self.records_by_event_id.remove(event_id);
                    }
                }
            }
            if let Some(session_id) = span.attributes.get(CONVERSATION_ID_ATTRIBUTE) {
                // Go `evictTraceIDsBySessionID`.
                if let Some(traces) = self.trace_ids_by_session_id.get_mut(session_id) {
                    traces.remove(trace_id);
                    if traces.is_empty() {
                        self.trace_ids_by_session_id.remove(session_id);
                    }
                }
            }
        }
    }
}

/// Go `TraceID.IsValid`.
fn is_valid_trace_id(id: &[u8; 16]) -> bool {
    id.iter().any(|b| *b != 0)
}

/// Go `TraceID.String` / `SpanID.String`: lowercase hex.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Go `convertRecords`: drops records without a valid trace ID, sorts the rest by start time
/// (stable) and converts them, with empty containers rather than absent ones.
pub(crate) fn convert_records(records: Vec<Arc<SpanRecord>>) -> Vec<DebugSpan> {
    let mut records: Vec<Arc<SpanRecord>> = records
        .into_iter()
        .filter(|r| is_valid_trace_id(&r.trace_id))
        .collect();
    records.sort_by_key(|r| r.start_time);
    records
        .iter()
        .map(|r| DebugSpan {
            name: r.name.clone(),
            start_time: r.start_time,
            end_time: r.end_time,
            span_id: hex(&r.span_id),
            trace_id: hex(&r.trace_id),
            parent_span_id: hex(&r.parent_span_id),
            attributes: event_id_attributes(r),
            logs: r.logs.clone(),
        })
        .collect()
}

/// Go `eventIDAttributes`: a fresh copy of the span's attributes in which a `generate_content`
/// span without an event ID gets its own span ID as one (the web UI's schema requires the
/// attribute there, and a failed model call never records it). The stored record is untouched.
fn event_id_attributes(r: &SpanRecord) -> BTreeMap<String, String> {
    let mut attributes = r.attributes.clone();
    if !attributes.contains_key(EVENT_ID_ATTRIBUTE)
        && r.name.starts_with(GENERATE_CONTENT_SPAN_PREFIX)
    {
        attributes.insert(EVENT_ID_ATTRIBUTE.to_owned(), hex(&r.span_id));
    }
    attributes
}

/// Port of Go `services.DebugTelemetry`: a [`tracing_subscriber::Layer`] recording spans and the
/// engine's log events, and the store the debug controller reads. Clones share one store.
#[derive(Clone)]
pub struct DebugTelemetry {
    store: Arc<Store>,
}

impl DebugTelemetry {
    /// Go `NewDebugTelemetryWithConfig`: a store keeping `cfg.trace_capacity` traces (zero or
    /// negative: 10 000).
    pub fn new(cfg: DebugTelemetryConfig) -> Self {
        let capacity = usize::try_from(cfg.trace_capacity)
            .ok()
            .filter(|c| *c > 0)
            .unwrap_or(DEFAULT_TRACE_CAPACITY);
        Self {
            store: Arc::new(Store::new(capacity)),
        }
    }

    /// Go `GetSpansByEventID`: the closed spans carrying `event_id`, by start time.
    pub fn get_spans_by_event_id(&self, event_id: &str) -> Vec<DebugSpan> {
        self.store.spans_by_event_id(event_id)
    }

    /// Go `GetSpansBySessionID`: the closed spans of the traces of session `session_id` (every
    /// trace with a span whose `gen_ai.conversation.id` is `session_id`), by start time.
    pub fn get_spans_by_session_id(&self, session_id: &str) -> Vec<DebugSpan> {
        self.store.spans_by_session_id(session_id)
    }
}

/// An open span's record, kept in the span's registry extensions until it closes.
struct OpenSpan(SpanRecord);

/// Unix nanoseconds now (Go `time.Now().UnixNano()`).
fn now_unix_nano() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_nanos()).ok())
        .unwrap_or_default()
}

/// A random non-zero trace ID (the OTel SDK's random ID generator).
fn random_trace_id() -> [u8; 16] {
    loop {
        let id: u128 = rand::random();
        if id != 0 {
            return id.to_be_bytes();
        }
    }
}

/// A random non-zero span ID (the OTel SDK's random ID generator).
fn random_span_id() -> [u8; 8] {
    loop {
        let id: u64 = rand::random();
        if id != 0 {
            return id.to_be_bytes();
        }
    }
}

/// Reads span fields as Go span attributes (see the module docs).
struct SpanFields<'a>(&'a mut SpanRecord);

impl SpanFields<'_> {
    fn set(&mut self, f: &Field, v: String) {
        match f.name() {
            "otel.name" => self.0.name = v,
            "error.message" => {}
            n if n.starts_with("otel.") => {}
            n => {
                self.0.attributes.insert(n.to_owned(), v);
            }
        }
    }
}

impl Visit for SpanFields<'_> {
    fn record_str(&mut self, f: &Field, v: &str) {
        self.set(f, v.to_owned());
    }
    fn record_bool(&mut self, f: &Field, v: bool) {
        self.set(f, v.to_string());
    }
    fn record_i64(&mut self, f: &Field, v: i64) {
        self.set(f, v.to_string());
    }
    fn record_u64(&mut self, f: &Field, v: u64) {
        self.set(f, v.to_string());
    }
    fn record_i128(&mut self, f: &Field, v: i128) {
        self.set(f, v.to_string());
    }
    fn record_u128(&mut self, f: &Field, v: u128) {
        self.set(f, v.to_string());
    }
    fn record_f64(&mut self, f: &Field, v: f64) {
        self.set(f, emit_float(v));
    }
    fn record_bytes(&mut self, f: &Field, v: &[u8]) {
        self.set(f, emit_structured(v));
    }
    fn record_debug(&mut self, f: &Field, v: &dyn std::fmt::Debug) {
        self.set(f, format!("{v:?}"));
    }
}

/// Go `attribute.Value.Emit` of a FLOAT64: `fmt.Sprint(f)`.
fn emit_float(f: f64) -> String {
    if f.is_nan() {
        "NaN".to_owned()
    } else if f.is_infinite() {
        if f > 0.0 { "+Inf" } else { "-Inf" }.to_owned()
    } else {
        Number::from_f64(f).map_or_else(String::new, |n| go_sprint_value(&Value::Number(n)))
    }
}

/// Go `attribute.Value.Emit` of a structured attribute recorded as the bytes of its JSON text: a
/// JSON array of strings is a STRINGSLICE (`json.Marshal`, HTML-escaped), another array a SLICE
/// and an object a MAP (OTel's own JSON rendering: no HTML escaping, floats as `%g`). Bytes that
/// are not JSON are a BYTESLICE (base64).
fn emit_structured(v: &[u8]) -> String {
    let Ok(value) = serde_json::from_slice::<Value>(v) else {
        return base64::engine::general_purpose::STANDARD.encode(v);
    };
    if let Value::Array(items) = &value
        && items.iter().all(Value::is_string)
        && let Ok(bytes) = gadk_model::to_go_json(&value)
    {
        return String::from_utf8_lossy(&bytes).into_owned();
    }
    let mut out = String::new();
    append_otel_json(&mut out, &value);
    out
}

/// OTel `appendJSONValue` for a JSON-decoded value.
fn append_otel_json(out: &mut String, v: &Value) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) if n.is_f64() => out.push_str(&go_sprint_value(v)),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => append_otel_json_string(out, s),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                append_otel_json(out, item);
            }
            out.push(']');
        }
        Value::Object(m) => {
            out.push('{');
            for (i, (k, item)) in m.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                append_otel_json_string(out, k);
                out.push(':');
                append_otel_json(out, item);
            }
            out.push('}');
        }
    }
}

/// OTel `appendJSONString`: `encoding/json` string escaping without the HTML escapes.
fn append_otel_json_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' | '"' => {
                out.push('\\');
                out.push(c);
            }
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Reads the fields of a D6 log event. `None` body/attributes: the field was absent;
/// `Some(Err(()))`: its text is not JSON (Go: a value `encoding/json` cannot marshal).
#[derive(Default)]
struct LogFields {
    event_name: String,
    body: Option<Result<Value, ()>>,
    attributes: Option<Result<Value, ()>>,
}

impl Visit for LogFields {
    fn record_str(&mut self, f: &Field, v: &str) {
        match f.name() {
            "event.name" => self.event_name = v.to_owned(),
            "otel.log.body" => self.body = Some(serde_json::from_str(v).map_err(|_| ())),
            "otel.log.attributes" => {
                self.attributes = Some(serde_json::from_str(v).map_err(|_| ()));
            }
            _ => {}
        }
    }
    fn record_debug(&mut self, f: &Field, v: &dyn std::fmt::Debug) {
        self.record_str(f, &format!("{v:?}"));
    }
}

/// Go `log.ObservedTimestamp().Format(time.RFC3339Nano)` of the local wall clock: trailing zeros
/// of the fraction trimmed (the fraction dropped when zero), `Z` for a zero offset.
fn go_rfc3339_nano_local(t: DateTime<FixedOffset>) -> String {
    let mut s = t.format("%Y-%m-%dT%H:%M:%S").to_string();
    let nanos = t.nanosecond() % 1_000_000_000;
    if nanos != 0 {
        let frac = format!("{nanos:09}");
        s.push('.');
        s.push_str(frac.trim_end_matches('0'));
    }
    if t.offset().local_minus_utc() == 0 {
        s.push('Z');
    } else {
        s.push_str(&t.format("%:z").to_string());
    }
    s
}

/// Go `normalizeLogBody`: the body reshaped for the web UI ([`normalize_message_shape`]); a body
/// that is not representable (`Err`) becomes `{"content": "<unrepresentable>"}`, reshaped too.
pub(crate) fn normalize_log_body(event_name: &str, body: Result<Value, ()>) -> Value {
    match body {
        Ok(body) => normalize_message_shape(event_name, body),
        Err(()) => normalize_message_shape(
            event_name,
            Value::Object(Map::from_iter([(
                CONTENT_KEY.to_owned(),
                Value::String(UNREPRESENTABLE_CONTENT.to_owned()),
            )])),
        ),
    }
}

/// Go `normalizeMessageShape`: for `gen_ai.user.message` and `gen_ai.choice`, `body.content`
/// becomes a message object (see [`message_content`]); other event names are untouched.
fn normalize_message_shape(event_name: &str, body: Value) -> Value {
    if event_name != USER_MESSAGE_EVENT_NAME && event_name != CHOICE_EVENT_NAME {
        return body;
    }
    let role = default_role(event_name);
    match body {
        Value::Object(mut fields) => {
            let content = fields.get(CONTENT_KEY).cloned().unwrap_or(Value::Null);
            let outer_role = fields.get(ROLE_KEY).cloned().unwrap_or(Value::Null);
            fields.insert(
                CONTENT_KEY.to_owned(),
                Value::Object(message_content(content, role, &outer_role)),
            );
            Value::Object(fields)
        }
        other => Value::Object(Map::from_iter([(
            CONTENT_KEY.to_owned(),
            Value::Object(message_content(other, role, &Value::Null)),
        )])),
    }
}

/// Go `defaultRole`.
fn default_role(event_name: &str) -> &'static str {
    if event_name == CHOICE_EVENT_NAME {
        MODEL_ROLE
    } else {
        USER_ROLE
    }
}

/// Go `messageContent`: `v` as a `{"parts": [...], "role": …}` object keeping its other fields;
/// a missing or empty role is `outer_role` when that is a non-empty string, else
/// `fallback_role`.
fn message_content(v: Value, fallback_role: &str, outer_role: &Value) -> Map<String, Value> {
    let mut content = match v {
        Value::Object(mut content) => {
            if !content.get(PARTS_KEY).is_some_and(Value::is_array) {
                let parts = text_parts(content.get(PARTS_KEY).unwrap_or(&Value::Null));
                content.insert(PARTS_KEY.to_owned(), parts);
            }
            content
        }
        other => Map::from_iter([(PARTS_KEY.to_owned(), text_parts(&other))]),
    };
    let has_role = content
        .get(ROLE_KEY)
        .and_then(Value::as_str)
        .is_some_and(|r| !r.is_empty());
    if !has_role {
        let role = match outer_role.as_str() {
            Some(r) if !r.is_empty() => r,
            _ => fallback_role,
        };
        content.insert(ROLE_KEY.to_owned(), Value::String(role.to_owned()));
    }
    content
}

/// Go `textParts`: no parts for a missing value, else one text part (`fmt.Sprint` of a
/// non-string).
fn text_parts(v: &Value) -> Value {
    let text = match v {
        Value::Null => return Value::Array(Vec::new()),
        Value::String(s) => s.clone(),
        other => go_sprint_log_value(other),
    };
    Value::Array(vec![Value::Object(Map::from_iter([(
        TEXT_KEY.to_owned(),
        Value::String(text),
    )]))])
}

/// Go `fmt.Sprint` of a `telemetry.FromLogValue` result: like
/// [`go_sprint_value`] (`[a b]`, `map[k:v]` with sorted keys), but an integer is Go's `int64`
/// (printed in full) rather than a JSON-decoded `float64`.
fn go_sprint_log_value(v: &Value) -> String {
    match v {
        Value::Number(n) if !n.is_f64() => n.to_string(),
        Value::Array(items) => format!(
            "[{}]",
            items
                .iter()
                .map(go_sprint_log_value)
                .collect::<Vec<_>>()
                .join(" ")
        ),
        Value::Object(m) => format!(
            "map[{}]",
            m.iter()
                .map(|(k, v)| format!("{k}:{}", go_sprint_log_value(v)))
                .collect::<Vec<_>>()
                .join(" ")
        ),
        other => go_sprint_value(other),
    }
}

/// Go `normalizeLogAttributes`: in an inference-details record, rewrites the message parts the
/// web UI cannot render ([`ui_parts`]); attributes that are not representable (`Err`, or not a
/// JSON object) are dropped.
pub(crate) fn normalize_log_attributes(
    event_name: &str,
    attrs: Option<Result<Value, ()>>,
) -> Option<Map<String, Value>> {
    let Some(Ok(Value::Object(mut attrs))) = attrs else {
        return None;
    };
    if event_name == INFERENCE_DETAILS_EVENT_NAME {
        for key in [INPUT_MESSAGES_KEY, OUTPUT_MESSAGES_KEY] {
            if let Some(Value::Array(msgs)) = attrs.get_mut(key) {
                for m in msgs {
                    if let Value::Object(msg) = m {
                        let parts = msg.remove(PARTS_KEY).unwrap_or(Value::Null);
                        msg.insert(PARTS_KEY.to_owned(), ui_parts(parts));
                    }
                }
            }
        }
        if let Some(parts) = attrs.remove(SYSTEM_INSTRUCTIONS_KEY) {
            attrs.insert(SYSTEM_INSTRUCTIONS_KEY.to_owned(), ui_parts(parts));
        }
    }
    Some(attrs)
}

/// Go `uiParts`.
fn ui_parts(v: Value) -> Value {
    match v {
        Value::Array(parts) => Value::Array(
            parts
                .into_iter()
                .map(|p| match p {
                    Value::Object(part) => Value::Object(ui_part(part)),
                    other => other,
                })
                .collect(),
        ),
        other => other,
    }
}

/// Go `uiPart`: the web UI's part shapes for reasoning, blob and URI parts, and object-valued
/// tool-call arguments and responses.
fn ui_part(mut p: Map<String, Value>) -> Map<String, Value> {
    let field = |p: &Map<String, Value>, k: &str| p.get(k).cloned().unwrap_or(Value::Null);
    let mime_type = Value::String(
        p.get("mime_type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    );
    let s = |v: &str| Value::String(v.to_owned());
    match p.get("type").and_then(Value::as_str) {
        Some("reasoning") => Map::from_iter([
            ("type".to_owned(), s("text")),
            ("content".to_owned(), field(&p, "content")),
        ]),
        Some("blob") => Map::from_iter([
            ("type".to_owned(), s("blob")),
            ("mime_type".to_owned(), mime_type),
            ("data".to_owned(), field(&p, "content")),
        ]),
        Some("uri") => Map::from_iter([
            ("type".to_owned(), s("file_data")),
            ("mime_type".to_owned(), mime_type),
            ("uri".to_owned(), field(&p, "uri")),
        ]),
        Some("tool_call") => {
            let args = ui_object(field(&p, "arguments"));
            p.insert("arguments".to_owned(), args);
            p
        }
        Some("tool_call_response") => {
            let resp = ui_object(field(&p, "response"));
            p.insert("response".to_owned(), resp);
            p
        }
        _ => p,
    }
}

/// Go `uiObject`: an object or `null` as is, anything else wrapped as `{"value": v}`.
fn ui_object(v: Value) -> Value {
    match v {
        Value::Object(_) | Value::Null => v,
        other => Value::Object(Map::from_iter([("value".to_owned(), other)])),
    }
}

impl<S> tracing_subscriber::Layer<S> for DebugTelemetry
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: LayerContext<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let parent = span.parent().and_then(|p| {
            p.extensions()
                .get::<OpenSpan>()
                .map(|o| (o.0.trace_id, o.0.span_id))
        });
        let (trace_id, parent_span_id) = parent.unwrap_or_else(|| (random_trace_id(), [0; 8]));
        let mut record = SpanRecord {
            name: attrs.metadata().name().to_owned(),
            start_time: now_unix_nano(),
            end_time: 0,
            trace_id,
            span_id: random_span_id(),
            parent_span_id,
            attributes: BTreeMap::new(),
            logs: Vec::new(),
        };
        attrs.record(&mut SpanFields(&mut record));
        span.extensions_mut().insert(OpenSpan(record));
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: LayerContext<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        if let Some(open) = span.extensions_mut().get_mut::<OpenSpan>() {
            values.record(&mut SpanFields(&mut open.0));
        }
    }

    fn on_event(&self, event: &tracing::Event<'_>, ctx: LayerContext<'_, S>) {
        if event.metadata().target() != LOG_TARGET {
            return;
        }
        // Go drops a record without a span: it would never be returned.
        let Some(span) = ctx.event_span(event) else {
            return;
        };
        let mut fields = LogFields::default();
        event.record(&mut fields);
        let observed = go_rfc3339_nano_local(Local::now().fixed_offset());
        let mut ext = span.extensions_mut();
        let Some(open) = ext.get_mut::<OpenSpan>() else {
            return;
        };
        let event_name = fields.event_name;
        open.0.logs.push(DebugLog {
            body: normalize_log_body(&event_name, fields.body.unwrap_or(Ok(Value::Null))),
            attributes: normalize_log_attributes(&event_name, fields.attributes),
            observed_timestamp: observed,
            trace_id: hex(&open.0.trace_id),
            span_id: hex(&open.0.span_id),
            event_name,
        });
    }

    fn on_close(&self, id: Id, ctx: LayerContext<'_, S>) {
        let Some(span) = ctx.span(&id) else { return };
        let Some(OpenSpan(mut record)) = span.extensions_mut().remove::<OpenSpan>() else {
            return;
        };
        record.end_time = now_unix_nano();
        self.store.export(record);
    }
}

/// Port of Go `services.DebugSpan`: one span as the debug endpoints return it.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct DebugSpan {
    /// The span name (Go's full span name, the engine's `otel.name`).
    pub name: String,
    /// Start, Unix nanoseconds.
    pub start_time: i64,
    /// End, Unix nanoseconds.
    pub end_time: i64,
    /// 8 random bytes, lowercase hex.
    pub span_id: String,
    /// 16 random bytes, lowercase hex.
    pub trace_id: String,
    /// The parent's span ID; 16 zeros for a root span (Go `SpanID{}.String()`).
    pub parent_span_id: String,
    /// The span's attributes as strings (Go `attribute.Value.Emit`), sorted by key.
    pub attributes: BTreeMap<String, String>,
    /// The log records emitted under the span.
    pub logs: Vec<DebugLog>,
}

/// Port of Go `services.DebugLog`: one log record attached to a [`DebugSpan`].
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct DebugLog {
    /// The record's body.
    pub body: Value,
    /// The record's attributes; omitted when `None` or empty (Go `omitempty` on a map).
    #[serde(skip_serializing_if = "is_none_or_empty")]
    pub attributes: Option<Map<String, Value>>,
    /// When the record was observed (Go's text).
    pub observed_timestamp: String,
    /// The trace ID, lowercase hex.
    pub trace_id: String,
    /// The span ID, lowercase hex.
    pub span_id: String,
    /// The record's event name.
    pub event_name: String,
}

fn is_none_or_empty(m: &Option<Map<String, Value>>) -> bool {
    m.as_ref().is_none_or(Map::is_empty)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use chrono::TimeZone;
    use tracing_subscriber::layer::SubscriberExt;

    use super::*;

    #[test]
    fn debug_span_bytes_follow_go_tags() {
        let span = DebugSpan {
            name: "invoke_agent a".into(),
            start_time: 1,
            end_time: 2,
            span_id: "00000000000000aa".into(),
            trace_id: "0000000000000000000000000000000b".into(),
            parent_span_id: "0000000000000000".into(),
            attributes: BTreeMap::from([("z".into(), "1".into()), ("a".into(), "<x>".into())]),
            logs: vec![DebugLog {
                body: serde_json::json!({"content": "hi"}),
                attributes: Some(Map::new()),
                observed_timestamp: "t".into(),
                trace_id: "tr".into(),
                span_id: "sp".into(),
                event_name: "gen_ai.user.message".into(),
            }],
        };
        assert_eq!(
            String::from_utf8(gadk_model::to_go_json(&span).unwrap()).unwrap(),
            concat!(
                r#"{"name":"invoke_agent a","start_time":1,"end_time":2,"span_id":"00000000000000aa","#,
                r#""trace_id":"0000000000000000000000000000000b","parent_span_id":"0000000000000000","#,
                r#""attributes":{"a":"\u003cx\u003e","z":"1"},"logs":[{"body":{"content":"hi"},"#,
                r#""observed_timestamp":"t","trace_id":"tr","span_id":"sp","event_name":"gen_ai.user.message"}]}"#
            )
        );
    }

    /// Go `attribute.Value.Emit` per type, as the engine records each (plan decision D6).
    #[test]
    fn span_attributes_emit_like_go() {
        let dt = DebugTelemetry::new(DebugTelemetryConfig::default());
        let sub = tracing_subscriber::registry().with(dt.clone());
        tracing::subscriber::with_default(sub, || {
            let span = tracing::info_span!(
                "generate_content",
                otel.name = %"generate_content m",
                gen_ai.conversation.id = "s",
                s = "<a>",
                b = true,
                i = -3_i64,
                u = 7_u64,
                f_int = 1_000_000.0_f64,
                f_frac = 0.5_f64,
                f_nan = f64::NAN,
                f_inf = f64::NEG_INFINITY,
                otel.status_code = "ERROR",
                error.message = "boom",
                strs = br#"["tool_call","<x>"]"#.as_slice(),
                slice = br#"[1,"<y>",2.5,1e21,true,null]"#.as_slice(),
                map = br#"{"k":"v\u2028"}"#.as_slice(),
                raw = b"\xff".as_slice(),
            );
            drop(span);
        });
        let spans = dt.get_spans_by_session_id("s");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].name, "generate_content m");
        let attrs = &spans[0].attributes;
        let want: BTreeMap<String, String> = [
            ("gen_ai.conversation.id", "s"),
            ("s", "<a>"),
            ("b", "true"),
            ("i", "-3"),
            ("u", "7"),
            ("f_int", "1e+06"),
            ("f_frac", "0.5"),
            ("f_nan", "NaN"),
            ("f_inf", "-Inf"),
            ("strs", r#"["tool_call","\u003cx\u003e"]"#),
            ("slice", r#"[1,"<y>",2.5,1e+21,true,null]"#),
            ("map", r#"{"k":"v\u2028"}"#),
            ("raw", "/w=="),
            // A failed generate_content span gains its own span ID as the event ID.
            (EVENT_ID_ATTRIBUTE, spans[0].span_id.as_str()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
        assert_eq!(attrs, &want);
    }

    /// Children keep the root's trace ID; a root's parent is sixteen zeros; IDs are 32 and 16
    /// lowercase hex digits; times are ordered.
    #[test]
    fn ids_and_times_follow_the_otel_sdk() {
        let dt = DebugTelemetry::new(DebugTelemetryConfig::default());
        let sub = tracing_subscriber::registry().with(dt.clone());
        tracing::subscriber::with_default(sub, || {
            let root = tracing::info_span!("root", gen_ai.conversation.id = "s");
            let _g = root.enter();
            let child = tracing::info_span!("child");
            drop(child);
        });
        let mut spans = dt.get_spans_by_session_id("s");
        assert_eq!(spans.len(), 2);
        spans.sort_by(|a, b| b.name.cmp(&a.name));
        let (root, child) = (&spans[0], &spans[1]);
        assert_eq!(root.name, "root");
        assert_eq!(root.parent_span_id, "0000000000000000");
        assert_eq!(child.parent_span_id, root.span_id);
        assert_eq!(child.trace_id, root.trace_id);
        for s in &spans {
            assert_eq!(s.trace_id.len(), 32);
            assert_eq!(s.span_id.len(), 16);
            assert!(
                s.trace_id
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            );
            assert!(s.start_time > 0 && s.start_time <= s.end_time);
        }
        assert!(root.end_time >= child.end_time);
    }

    /// Log events attach to their span with its IDs; other targets are not logs.
    #[test]
    fn log_events_attach_to_their_span() {
        let dt = DebugTelemetry::new(DebugTelemetryConfig::default());
        let sub = tracing_subscriber::registry().with(dt.clone());
        tracing::subscriber::with_default(sub, || {
            let span = tracing::info_span!("s", gen_ai.conversation.id = "s");
            tracing::event!(
                target: "gadk_engine::telemetry::log",
                parent: &span,
                tracing::Level::DEBUG,
                event.name = "e",
                otel.log.body = "null",
                otel.log.attributes = r#"{"a":[1]}"#
            );
            tracing::info!(parent: &span, "not a log record");
        });
        let spans = dt.get_spans_by_session_id("s");
        let logs = &spans[0].logs;
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].body, Value::Null);
        assert_eq!(
            logs[0].attributes,
            Some(serde_json::from_str(r#"{"a":[1]}"#).unwrap())
        );
        assert_eq!(logs[0].event_name, "e");
        assert_eq!(logs[0].trace_id, spans[0].trace_id);
        assert_eq!(logs[0].span_id, spans[0].span_id);
        assert!(DateTime::parse_from_rfc3339(&logs[0].observed_timestamp).is_ok());
    }

    /// Go `time.RFC3339Nano`: trimmed fraction, `Z` for UTC, `±hh:mm` otherwise.
    #[test]
    fn observed_timestamp_formats_like_go() {
        let utc = FixedOffset::east_opt(0).unwrap();
        let cst = FixedOffset::west_opt(6 * 3600).unwrap();
        let at = |o: FixedOffset, ns: u32| {
            o.with_ymd_and_hms(2026, 1, 2, 3, 4, 5)
                .unwrap()
                .with_nanosecond(ns)
                .unwrap()
        };
        assert_eq!(go_rfc3339_nano_local(at(utc, 0)), "2026-01-02T03:04:05Z");
        assert_eq!(
            go_rfc3339_nano_local(at(cst, 256_916_000)),
            "2026-01-02T03:04:05.256916-06:00"
        );
        assert_eq!(
            go_rfc3339_nano_local(at(utc, 1)),
            "2026-01-02T03:04:05.000000001Z"
        );
    }

    /// Go `textParts` prints a non-string with `fmt.Sprint` (an int64 in full).
    #[test]
    fn text_parts_print_non_strings_like_go() {
        let parts = |v: Value| text_parts(&v);
        assert_eq!(parts(Value::Null), serde_json::json!([]));
        assert_eq!(
            parts(serde_json::json!(1_000_000)),
            serde_json::json!([{"text": "1000000"}])
        );
        assert_eq!(
            parts(serde_json::json!(2.5)),
            serde_json::json!([{"text": "2.5"}])
        );
        assert_eq!(
            parts(serde_json::json!({"b": [1, true], "a": "x"})),
            serde_json::json!([{"text": "map[a:x b:[1 true]]"}])
        );
    }

    /// The trace LRU evicts the least recently used trace and its index entries.
    #[test]
    fn eviction_clears_the_indexes() {
        let store = Store::new(1);
        let record = |t: u8, s: &str, e: &str| SpanRecord {
            name: "n".into(),
            start_time: 1,
            end_time: 2,
            trace_id: [t; 16],
            span_id: [t; 8],
            attributes: BTreeMap::from([
                (CONVERSATION_ID_ATTRIBUTE.to_owned(), s.to_owned()),
                (EVENT_ID_ATTRIBUTE.to_owned(), e.to_owned()),
            ]),
            ..Default::default()
        };
        store.export(record(1, "s1", "e1"));
        store.export(record(2, "s2", "e2"));
        let inner = store.inner.lock();
        assert!(!inner.trace_ids_by_session_id.contains_key("s1"));
        assert!(!inner.records_by_event_id.contains_key("e1"));
        assert!(inner.trace_ids_by_session_id.contains_key("s2"));
        assert!(inner.records_by_event_id.contains_key("e2"));
    }
}
