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

//! The one decoder of gadk's ADK telemetry as `tracing` carries it, shared by every consumer:
//! gadk-server's debug store, the engine's test digests and gadk-telemetry's OpenTelemetry bridge.
//! No OpenTelemetry dependency; consumers render the decoded values.
//!
//! - An ADK span is a span whose metadata declares [`OTEL_NAME`] ([`is_adk_span`]); any other
//!   span is foreign and transparent to ADK parenting.
//! - [`D6Fields`] reads an ADK span's fields: [`OTEL_NAME`] is the span name,
//!   [`OTEL_STATUS_CODE`] and [`ERROR_MESSAGE`] the span status and its description, any other
//!   `otel.*` field is ignored, and every remaining field is an OTel attribute ([`D6Value`]).
//! - [`decode_log`] reads an OTel log record: an event on [`LOG_TARGET`] with fields
//!   [`EVENT_NAME`], [`LOG_BODY`] (JSON text) and [`LOG_ATTRIBUTES`] (JSON object text).
//! - [`SpanIds`] is a span extension carrying the span's OTel trace and span IDs, written by the
//!   layer that owns them (the bridge) and read by the others (the debug store).

use serde_json::{Map, Value};
use tracing::field::{Field, Visit};

use super::LOG_TARGET;

/// The span field carrying the full span name; declaring it makes a span an ADK span.
pub const OTEL_NAME: &str = "otel.name";
/// The span field carrying the span status code (`OK` or `ERROR`).
pub const OTEL_STATUS_CODE: &str = "otel.status_code";
/// The span field carrying the span status description.
pub const ERROR_MESSAGE: &str = "error.message";
/// The log event field carrying the record's event name.
pub const EVENT_NAME: &str = "event.name";
/// The log event field carrying the record's body as JSON text.
pub const LOG_BODY: &str = "otel.log.body";
/// The log event field carrying the record's attributes as JSON object text.
pub const LOG_ATTRIBUTES: &str = "otel.log.attributes";

/// Whether a span is an ADK span (one an OTel SDK would see): its metadata declares [`OTEL_NAME`].
pub fn is_adk_span(meta: &tracing::Metadata<'_>) -> bool {
    meta.fields().field(OTEL_NAME).is_some()
}

/// One decoded span attribute value, by the `tracing` field kind that recorded it.
#[derive(Debug, Clone, PartialEq)]
pub enum D6Value {
    /// A `str` field, or a `Debug`/`Display` field's text; also an `i128`/`u128` field's decimal
    /// text.
    Str(String),
    /// An `i64` field, or a `u64` field that fits in `i64`.
    I64(i64),
    /// A `u64` field above `i64::MAX` (OTel has no unsigned integer type).
    U64(u64),
    /// An `f64` field (possibly NaN or infinite).
    F64(f64),
    /// A `bool` field.
    Bool(bool),
    /// A bytes field whose JSON text is an array of strings only.
    StrSlice(Vec<String>),
    /// A bytes field whose JSON text is any other JSON value (a structured attribute).
    Json(Value),
    /// A bytes field that is not JSON text.
    Bytes(Vec<u8>),
}

impl D6Value {
    /// The value as JSON: a string, a number (a non-finite float is `null`), a bool, an array of
    /// strings, the structured value itself, or `null` for bytes that are not JSON.
    pub fn to_json(&self) -> Value {
        match self {
            Self::Str(s) => Value::String(s.clone()),
            Self::I64(i) => Value::from(*i),
            Self::U64(u) => Value::from(*u),
            Self::F64(f) => serde_json::Number::from_f64(*f).map_or(Value::Null, Value::Number),
            Self::Bool(b) => Value::Bool(*b),
            Self::StrSlice(items) => {
                Value::Array(items.iter().cloned().map(Value::String).collect())
            }
            Self::Json(v) => v.clone(),
            Self::Bytes(_) => Value::Null,
        }
    }

    /// Decodes a bytes field: the JSON text of a structured value.
    fn from_bytes(v: &[u8]) -> Self {
        match serde_json::from_slice::<Value>(v) {
            Ok(Value::Array(items)) if items.iter().all(Value::is_string) => Self::StrSlice(
                items
                    .into_iter()
                    .filter_map(|item| match item {
                        Value::String(s) => Some(s),
                        _ => None,
                    })
                    .collect(),
            ),
            Ok(value) => Self::Json(value),
            Err(_) => Self::Bytes(v.to_vec()),
        }
    }
}

/// The span status code as recorded on [`OTEL_STATUS_CODE`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum D6StatusCode {
    /// No status, or a recorded value other than `OK`/`ERROR`.
    #[default]
    Unset,
    /// Success (recorded as `OK`).
    Ok,
    /// Failure (recorded as `ERROR`).
    Error,
}

impl D6StatusCode {
    /// The upper-case name: `UNSET`, `OK` or `ERROR`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unset => "UNSET",
            Self::Ok => "OK",
            Self::Error => "ERROR",
        }
    }
}

/// The ADK fields of one `tracing` visit (a span's `Attributes` or one `Record`), as a
/// [`Visit`] implementation. Each member is what this visit recorded; a consumer merges it into
/// the span's state (a later value of a name or attribute replaces the earlier one).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct D6Fields {
    /// The span name: the [`OTEL_NAME`] field when recorded as a string (`str` or `Debug`/`Display`
    /// text).
    pub name: Option<String>,
    /// The OTel attributes, in record order (a field recorded twice in one visit keeps its first
    /// position and its last value).
    pub attributes: Vec<(String, D6Value)>,
    /// The status code, when [`OTEL_STATUS_CODE`] was recorded: `ERROR` and `OK` (as strings)
    /// are [`D6StatusCode::Error`] and [`D6StatusCode::Ok`], any other value is unset.
    pub status_code: Option<D6StatusCode>,
    /// The status description: the [`ERROR_MESSAGE`] field when recorded as a string.
    pub status_description: Option<String>,
}

impl D6Fields {
    fn set(&mut self, f: &Field, v: D6Value) {
        match f.name() {
            OTEL_NAME => {
                if let D6Value::Str(s) = v {
                    self.name = Some(s);
                }
            }
            OTEL_STATUS_CODE => {
                self.status_code = Some(match &v {
                    D6Value::Str(s) if s == "ERROR" => D6StatusCode::Error,
                    D6Value::Str(s) if s == "OK" => D6StatusCode::Ok,
                    _ => D6StatusCode::Unset,
                });
            }
            ERROR_MESSAGE => {
                if let D6Value::Str(s) = v {
                    self.status_description = Some(s);
                }
            }
            n if n.starts_with("otel.") => {}
            n => match self.attributes.iter_mut().find(|(k, _)| k == n) {
                Some(slot) => slot.1 = v,
                None => self.attributes.push((n.to_owned(), v)),
            },
        }
    }
}

impl Visit for D6Fields {
    fn record_str(&mut self, f: &Field, v: &str) {
        self.set(f, D6Value::Str(v.to_owned()));
    }
    fn record_bool(&mut self, f: &Field, v: bool) {
        self.set(f, D6Value::Bool(v));
    }
    fn record_i64(&mut self, f: &Field, v: i64) {
        self.set(f, D6Value::I64(v));
    }
    fn record_u64(&mut self, f: &Field, v: u64) {
        self.set(f, i64::try_from(v).map_or(D6Value::U64(v), D6Value::I64));
    }
    fn record_i128(&mut self, f: &Field, v: i128) {
        self.set(f, D6Value::Str(v.to_string()));
    }
    fn record_u128(&mut self, f: &Field, v: u128) {
        self.set(f, D6Value::Str(v.to_string()));
    }
    fn record_f64(&mut self, f: &Field, v: f64) {
        self.set(f, D6Value::F64(v));
    }
    fn record_bytes(&mut self, f: &Field, v: &[u8]) {
        self.set(f, D6Value::from_bytes(v));
    }
    fn record_debug(&mut self, f: &Field, v: &dyn std::fmt::Debug) {
        self.set(f, D6Value::Str(format!("{v:?}")));
    }
}

/// One decoded OTel log record.
#[derive(Debug, Clone, PartialEq)]
pub struct D6Log {
    /// The [`EVENT_NAME`] field (empty when absent).
    pub event_name: String,
    /// The event's level (`INFO` or `DEBUG` as gadk emits them).
    pub level: tracing::Level,
    /// The [`LOG_BODY`] JSON text, parsed: `Ok(null)` when absent, `Err(())` when not JSON (the
    /// body could not be serialized).
    pub body: Result<Value, ()>,
    /// The [`LOG_ATTRIBUTES`] JSON object, parsed; `None` when absent, not JSON or not an object.
    pub attributes: Option<Map<String, Value>>,
}

/// Decodes an OTel log record: `None` for an event whose target is not [`LOG_TARGET`].
pub fn decode_log(event: &tracing::Event<'_>) -> Option<D6Log> {
    let meta = event.metadata();
    if meta.target() != LOG_TARGET {
        return None;
    }
    let mut fields = LogFields::default();
    event.record(&mut fields);
    Some(D6Log {
        event_name: fields.event_name,
        level: *meta.level(),
        body: fields.body.unwrap_or(Ok(Value::Null)),
        attributes: match fields.attributes {
            Some(Ok(Value::Object(m))) => Some(m),
            _ => None,
        },
    })
}

/// The visitor behind [`decode_log`].
#[derive(Default)]
struct LogFields {
    event_name: String,
    body: Option<Result<Value, ()>>,
    attributes: Option<Result<Value, ()>>,
}

impl Visit for LogFields {
    fn record_str(&mut self, f: &Field, v: &str) {
        match f.name() {
            EVENT_NAME => self.event_name = v.to_owned(),
            LOG_BODY => self.body = Some(serde_json::from_str(v).map_err(|_| ())),
            LOG_ATTRIBUTES => self.attributes = Some(serde_json::from_str(v).map_err(|_| ())),
            _ => {}
        }
    }
    fn record_debug(&mut self, f: &Field, v: &dyn std::fmt::Debug) {
        self.record_str(f, &format!("{v:?}"));
    }
}

/// An ADK span's OTel IDs, kept in the span's `tracing` registry extensions by the layer that
/// owns them (gadk-telemetry's bridge, from the SDK span it started) and reused by the other
/// layers (gadk-server's debug store), so every consumer reports the same IDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SpanIds {
    /// The trace ID (OTel `TraceId` bytes, big-endian).
    pub trace_id: [u8; 16],
    /// The span ID (OTel `SpanId` bytes, big-endian).
    pub span_id: [u8; 8],
}

impl SpanIds {
    /// Whether both IDs are non-zero; a shut-down provider hands out all-zero IDs, which consumers
    /// replace with their own.
    pub fn is_valid(&self) -> bool {
        self.trace_id.iter().any(|b| *b != 0) && self.span_id.iter().any(|b| *b != 0)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Arc;

    use parking_lot::Mutex;
    use tracing::span::{Attributes, Id, Record};
    use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
    use tracing_subscriber::registry::LookupSpan;

    use super::*;

    /// One probed span: whether it is an ADK span, and the [`D6Fields`] of each visit.
    type ProbedSpan = (bool, Vec<D6Fields>);

    /// Records, per span, whether it is an ADK span and the [`D6Fields`] of its visits; and every
    /// decoded log record.
    #[derive(Clone, Default)]
    struct Probe {
        spans: Arc<Mutex<Vec<ProbedSpan>>>,
        logs: Arc<Mutex<Vec<D6Log>>>,
    }

    struct Idx(usize);

    impl<S> Layer<S> for Probe
    where
        S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    {
        fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
            let mut fields = D6Fields::default();
            attrs.record(&mut fields);
            let mut spans = self.spans.lock();
            spans.push((is_adk_span(attrs.metadata()), vec![fields]));
            let i = spans.len() - 1;
            drop(spans);
            if let Some(span) = ctx.span(id) {
                span.extensions_mut().insert(Idx(i));
            }
        }

        fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
            let Some(i) = ctx
                .span(id)
                .and_then(|s| s.extensions().get::<Idx>().map(|i| i.0))
            else {
                return;
            };
            let mut fields = D6Fields::default();
            values.record(&mut fields);
            if let Some(span) = self.spans.lock().get_mut(i) {
                span.1.push(fields);
            }
        }

        fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
            if let Some(log) = decode_log(event) {
                self.logs.lock().push(log);
            }
        }
    }

    fn probe(f: impl FnOnce()) -> Probe {
        let p = Probe::default();
        let sub = tracing_subscriber::registry().with(p.clone());
        tracing::subscriber::with_default(sub, f);
        p
    }

    /// Every field kind decodes to its value; `otel.*` and `error.message` are not attributes.
    #[test]
    fn span_fields_decode_each_value_kind() {
        let p = probe(|| {
            let _span = tracing::info_span!(
                "generate_content",
                otel.name = %"generate_content m",
                s = "<a>",
                d = ?"dbg",
                b = true,
                i = -3_i64,
                u = 7_u64,
                u_big = u64::MAX,
                i128v = -5_i128,
                u128v = 6_u128,
                f = 0.5_f64,
                f_nan = f64::NAN,
                strs = br#"["a","<b>"]"#.as_slice(),
                empty = br#"[]"#.as_slice(),
                mixed = br#"[1,"x"]"#.as_slice(),
                map = br#"{"k":"v"}"#.as_slice(),
                scalar = br#"1.5"#.as_slice(),
                raw = b"\xff".as_slice(),
                otel.kind = "client",
            );
        });
        let spans = p.spans.lock();
        assert_eq!(spans.len(), 1);
        let (adk, visits) = &spans[0];
        assert!(*adk);
        let f = &visits[0];
        assert_eq!(f.name.as_deref(), Some("generate_content m"));
        assert_eq!(f.status_code, None);
        assert_eq!(f.status_description, None);
        let attrs: Vec<(&str, &D6Value)> =
            f.attributes.iter().map(|(k, v)| (k.as_str(), v)).collect();
        assert_eq!(attrs.len(), 16);
        let want = [
            ("s", D6Value::Str("<a>".into())),
            ("d", D6Value::Str("\"dbg\"".into())),
            ("b", D6Value::Bool(true)),
            ("i", D6Value::I64(-3)),
            ("u", D6Value::I64(7)),
            ("u_big", D6Value::U64(u64::MAX)),
            ("i128v", D6Value::Str("-5".into())),
            ("u128v", D6Value::Str("6".into())),
            ("f", D6Value::F64(0.5)),
        ];
        for (i, (k, v)) in want.iter().enumerate() {
            assert_eq!(attrs[i], (*k, v), "attribute {i}");
        }
        assert_eq!(attrs[9].0, "f_nan");
        assert!(matches!(attrs[9].1, D6Value::F64(x) if x.is_nan()));
        let rest = [
            ("strs", D6Value::StrSlice(vec!["a".into(), "<b>".into()])),
            ("empty", D6Value::StrSlice(Vec::new())),
            ("mixed", D6Value::Json(serde_json::json!([1, "x"]))),
            ("map", D6Value::Json(serde_json::json!({"k": "v"}))),
            ("scalar", D6Value::Json(serde_json::json!(1.5))),
            ("raw", D6Value::Bytes(vec![0xff])),
        ];
        for (i, (k, v)) in rest.iter().enumerate() {
            assert_eq!(attrs[10 + i], (*k, v), "attribute {}", 10 + i);
        }
    }

    /// `to_json` is the digest's view of each value kind.
    #[test]
    fn values_as_json() {
        use serde_json::json;
        assert_eq!(D6Value::Str("x".into()).to_json(), json!("x"));
        assert_eq!(D6Value::I64(-1).to_json(), json!(-1));
        assert_eq!(D6Value::U64(u64::MAX).to_json(), json!(u64::MAX));
        assert_eq!(D6Value::F64(0.5).to_json(), json!(0.5));
        assert_eq!(D6Value::F64(f64::INFINITY).to_json(), Value::Null);
        assert_eq!(D6Value::Bool(false).to_json(), json!(false));
        assert_eq!(D6Value::StrSlice(vec!["a".into()]).to_json(), json!(["a"]));
        assert_eq!(D6Value::Json(json!({"k": 1})).to_json(), json!({"k": 1}));
        assert_eq!(D6Value::Bytes(vec![0xff]).to_json(), Value::Null);
    }

    /// The status code and description come from `otel.status_code` and `error.message`, also
    /// when recorded later; a value other than `OK`/`ERROR` is unset.
    #[test]
    fn status_fields_decode() {
        let p = probe(|| {
            let span = tracing::info_span!(
                "execute_tool",
                otel.name = "execute_tool f",
                otel.status_code = tracing::field::Empty,
                error.message = tracing::field::Empty,
            );
            span.record("otel.status_code", "ERROR");
            span.record("error.message", "boom");
            span.record("otel.status_code", "OK");
            span.record("otel.status_code", "nope");
            span.record("otel.status_code", 1_i64);
        });
        let spans = p.spans.lock();
        let visits = &spans[0].1;
        assert_eq!(visits.len(), 6);
        assert_eq!(visits[0].status_code, None);
        assert_eq!(visits[1].status_code, Some(D6StatusCode::Error));
        assert_eq!(visits[2].status_description.as_deref(), Some("boom"));
        assert_eq!(visits[3].status_code, Some(D6StatusCode::Ok));
        assert_eq!(visits[4].status_code, Some(D6StatusCode::Unset));
        assert_eq!(visits[5].status_code, Some(D6StatusCode::Unset));
        assert!(visits.iter().all(|v| v.attributes.is_empty()));
        assert_eq!(D6StatusCode::Error.as_str(), "ERROR");
        assert_eq!(D6StatusCode::Ok.as_str(), "OK");
        assert_eq!(D6StatusCode::default().as_str(), "UNSET");
    }

    /// A span without `otel.name` is not an ADK span; a non-string `otel.name` is no name.
    #[test]
    fn non_adk_spans_are_classified() {
        let p = probe(|| {
            let _a = tracing::debug_span!("Connection", gen_ai.conversation.id = "s");
            let _b = tracing::info_span!("n", otel.name = 5_i64);
        });
        let spans = p.spans.lock();
        assert!(!spans[0].0);
        assert_eq!(spans[0].1[0].name, None);
        assert_eq!(
            spans[0].1[0].attributes,
            vec![(
                "gen_ai.conversation.id".to_owned(),
                D6Value::Str("s".into())
            )]
        );
        assert!(spans[1].0);
        assert_eq!(spans[1].1[0].name, None);
        assert!(spans[1].1[0].attributes.is_empty());
    }

    /// Log events on `LOG_TARGET` decode; other events do not.
    #[test]
    fn log_events_decode() {
        let p = probe(|| {
            tracing::event!(
                target: "gadk_engine::telemetry::log",
                tracing::Level::DEBUG,
                event.name = "e",
                otel.log.body = r#"{"content":"hi"}"#,
                otel.log.attributes = r#"{"a":[1]}"#
            );
            tracing::event!(
                target: "gadk_engine::telemetry::log",
                tracing::Level::INFO,
                event.name = "bad",
                otel.log.body = "not json",
                otel.log.attributes = "[1]"
            );
            tracing::event!(
                target: "gadk_engine::telemetry::log",
                tracing::Level::INFO,
                event.name = "absent"
            );
            tracing::info!(event.name = "other", "not a log record");
        });
        let logs = p.logs.lock();
        assert_eq!(logs.len(), 3);
        assert_eq!(
            logs[0],
            D6Log {
                event_name: "e".into(),
                level: tracing::Level::DEBUG,
                body: Ok(serde_json::json!({"content": "hi"})),
                attributes: serde_json::from_str(r#"{"a":[1]}"#).unwrap(),
            }
        );
        assert_eq!(logs[1].event_name, "bad");
        assert_eq!(logs[1].level, tracing::Level::INFO);
        assert_eq!(logs[1].body, Err(()));
        assert_eq!(logs[1].attributes, None);
        assert_eq!(logs[2].body, Ok(Value::Null));
        assert_eq!(logs[2].attributes, None);
    }

    /// IDs are valid only when both are non-zero.
    #[test]
    fn span_ids_validity() {
        assert!(!SpanIds::default().is_valid());
        let ids = |t: u8, s: u8| SpanIds {
            trace_id: [t; 16],
            span_id: [s; 8],
        };
        assert!(ids(1, 2).is_valid());
        assert!(!ids(0, 2).is_valid());
        assert!(!ids(1, 0).is_valid());
    }
}
