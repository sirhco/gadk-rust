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
//! reads. Task 3 fills [`build_digest`] and the capture.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::collections::BTreeMap;
use std::future::Future;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::log_digest::{LogDigest, LogRecord};

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
    todo!("plan 5a Task 3")
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
    todo!("plan 5a Task 3")
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
