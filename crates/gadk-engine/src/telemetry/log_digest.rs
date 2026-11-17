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

//! One OTel log record as a test digest sees it. Log records are `tracing` events; [`LogRecord`] is
//! a captured record reduced to what a digest reads.

use std::collections::BTreeMap;
use std::future::Future;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::span_digest::{SpanStub, capture, normalize_attribute};

/// One captured log record, reduced to what a digest reads.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LogRecord {
    /// The event's `event.name` field.
    pub event_name: String,
    /// The span the record was emitted under; `None` outside any span.
    pub span_id: Option<u64>,
    /// The event's `otel.log.body` JSON text, parsed.
    pub body: Value,
    /// The event's `otel.log.attributes` JSON object, in key order.
    pub attributes: Vec<(String, Value)>,
}

/// A deterministic snapshot of one log record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogDigest {
    /// The record's event name.
    pub event_name: String,
    /// The record's body.
    pub body: Value,
    /// Normalized attributes (non-deterministic ones read [`PRESENT`]).
    ///
    /// [`PRESENT`]: super::span_digest::PRESENT
    pub attributes: BTreeMap<String, Value>,
}

/// Builds the deterministic digest of one record.
pub fn build_log_digest(r: &LogRecord) -> LogDigest {
    LogDigest {
        event_name: r.event_name.clone(),
        body: r.body.clone(),
        attributes: r
            .attributes
            .iter()
            .map(|(k, v)| (k.clone(), normalize_attribute(k, v.clone())))
            .collect(),
    }
}

/// Runs `fut` with a recording subscriber as the thread default (use a current-thread runtime) and
/// returns its output, the spans it opened and the log records it emitted.
pub async fn capture_logs<F: Future>(fut: F) -> (F::Output, Vec<SpanStub>, Vec<LogRecord>) {
    capture(fut).await
}
