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
//! store that serve the debug trace endpoints (plan decision D6 is the contract with the engine's
//! spans and log events; plan 5a Task 12 fills the store).

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Serialize;
use serde_json::{Map, Value};

use super::DebugTelemetryConfig;

/// Go `defaultTraceCapacity`.
pub(crate) const DEFAULT_TRACE_CAPACITY: usize = 10_000;

/// The span store; plan 5a Task 12 defines it.
pub(crate) struct Store {}

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
        todo!("plan 5a Task 12")
    }

    /// Go `GetSpansByEventID`: the closed spans of the traces carrying `event_id`.
    pub fn get_spans_by_event_id(&self, event_id: &str) -> Vec<DebugSpan> {
        todo!("plan 5a Task 12")
    }

    /// Go `GetSpansBySessionID`: the closed spans of the traces of session `session_id`.
    pub fn get_spans_by_session_id(&self, session_id: &str) -> Vec<DebugSpan> {
        todo!("plan 5a Task 12")
    }
}

impl<S> tracing_subscriber::Layer<S> for DebugTelemetry where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>
{
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
}
