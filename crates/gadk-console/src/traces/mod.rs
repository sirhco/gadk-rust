// Copyright 2026 Chris Olson
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

//! The trace source seam and the trace types the pages render.

mod bigquery;
mod cloud;
pub use bigquery::BigQueryTraceSource;
pub use cloud::CloudTraceSource;
pub use gadk_analytics::{BigQueryTable, Service, SourceError, TimeRange, TraceId};

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};

/// A trace-list request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TraceQuery {
    /// The time window to list.
    pub range: TimeRange,
    /// Root span name prefix.
    pub name: Option<String>,
    /// Exact `gen_ai.agent.name`.
    pub agent: Option<String>,
    /// Exact `gen_ai.conversation.id`.
    pub session: Option<String>,
    /// The opaque token of the page to fetch; `None` fetches the first page.
    pub page_token: Option<String>,
}

/// One trace-list row.
#[derive(Debug, Clone, PartialEq)]
pub struct TraceSummary {
    /// The trace id.
    pub trace_id: TraceId,
    /// The root span's name (empty when the trace has no spans).
    pub root_name: String,
    /// The root span's start (the Unix epoch when the trace has no spans).
    pub start: DateTime<Utc>,
    /// The root span's duration in milliseconds.
    pub duration_ms: f64,
    /// The root span's `gen_ai.agent.name` label.
    pub agent: Option<String>,
    /// The root span's `gen_ai.conversation.id` label.
    pub session: Option<String>,
}

/// One page of the trace list.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TracePage {
    /// The rows of this page.
    pub traces: Vec<TraceSummary>,
    /// The token of the next page, if any.
    pub next: Option<String>,
}

/// One span. Ids are 16 lowercase hex characters.
#[derive(Debug, Clone, PartialEq)]
pub struct RawSpan {
    /// The span id.
    pub span_id: String,
    /// None when absent or zero.
    pub parent_id: Option<String>,
    /// The span name.
    pub name: String,
    /// When the span started.
    pub start: DateTime<Utc>,
    /// When the span ended.
    pub end: DateTime<Utc>,
    /// The span's labels.
    pub labels: BTreeMap<String, String>,
}

/// One Cloud Logging entry of the trace.
#[derive(Debug, Clone, PartialEq)]
pub struct LogEntry {
    /// When the entry was written.
    pub timestamp: DateTime<Utc>,
    /// 16 lowercase hex, as written by gadk-telemetry.
    pub span_id: Option<String>,
    /// The entry's severity, as Cloud Logging reports it.
    pub severity: Option<String>,
    /// The entry's `jsonPayload` (Null when absent).
    pub payload: serde_json::Value,
}

/// A trace's log entries; `truncated` when more than the collected 1,000 exist.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LogSet {
    /// The entries read.
    pub entries: Vec<LogEntry>,
    /// Whether entries were left unread.
    pub truncated: bool,
}

/// A trace with its spans and (separately fallible) log entries.
#[derive(Debug, Clone, PartialEq)]
pub struct TraceDetail {
    /// The trace id.
    pub trace_id: TraceId,
    /// The trace's spans.
    pub spans: Vec<RawSpan>,
    /// The trace's log entries, or the error reading them.
    pub logs: Result<LogSet, SourceError>,
}

/// Converts a Cloud Trace v1 decimal span id ("12345") to 16 lowercase hex digits
/// ("0000000000003039"); None if it is not a u64. "0" maps to Some("0000000000000000").
pub fn span_id_hex(decimal: &str) -> Option<String> {
    // `u64::from_str` also takes a leading '+'; span ids are plain digits.
    if decimal.is_empty() || !decimal.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    decimal.parse::<u64>().ok().map(|n| format!("{n:016x}"))
}

/// The most bytes of an upstream body an error carries.
pub(crate) const EXCERPT_LIMIT: usize = 1024;

/// At most 1,024 bytes of `body` (lossy UTF-8), cut on a char boundary.
pub(crate) fn excerpt(body: &[u8]) -> String {
    // Lossy decoding never shrinks the input, so the first `EXCERPT_LIMIT + 4` bytes cover
    // every character that can end within `EXCERPT_LIMIT` decoded bytes.
    let mut text = String::from_utf8_lossy(&body[..body.len().min(EXCERPT_LIMIT + 4)]).into_owned();
    text.truncate(text.floor_char_boundary(EXCERPT_LIMIT));
    text
}

/// Where traces come from (Cloud Trace today; room for others).
#[async_trait::async_trait]
pub trait TraceSource: Send + Sync {
    /// One page of traces matching `query`, newest first.
    async fn list(&self, query: &TraceQuery) -> Result<TracePage, SourceError>;
    /// The trace's spans and log entries; [`SourceError::NotFound`] for an unknown id.
    async fn get(&self, id: &TraceId) -> Result<TraceDetail, SourceError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_range_durations_and_default() {
        assert_eq!(TimeRange::Hour1.duration(), chrono::TimeDelta::hours(1));
        assert_eq!(TimeRange::Hours6.duration(), chrono::TimeDelta::hours(6));
        assert_eq!(TimeRange::Hours24.duration(), chrono::TimeDelta::hours(24));
        assert_eq!(TimeRange::Days7.duration(), chrono::TimeDelta::days(7));
        assert_eq!(TimeRange::default(), TimeRange::Hour1);
        assert_eq!(TraceQuery::default().range, TimeRange::Hour1);
    }

    #[test]
    fn span_id_hex_converts_decimal_u64() {
        assert_eq!(span_id_hex("12345").as_deref(), Some("0000000000003039"));
        assert_eq!(span_id_hex("0").as_deref(), Some("0000000000000000"));
        assert_eq!(
            span_id_hex("18446744073709551615").as_deref(),
            Some("ffffffffffffffff")
        );
        assert_eq!(span_id_hex("00255").as_deref(), Some("00000000000000ff"));
    }

    #[test]
    fn span_id_hex_rejects_non_u64() {
        for s in [
            "",
            "-1",
            "+1",
            "abc",
            "12a",
            "0x10",
            " 1",
            "1.0",
            "18446744073709551616", // u64::MAX + 1
        ] {
            assert_eq!(span_id_hex(s), None, "{s:?}");
        }
    }

    #[test]
    fn excerpt_keeps_short_ascii() {
        assert_eq!(excerpt(b""), "");
        assert_eq!(excerpt(b"permission denied"), "permission denied");
    }

    #[test]
    fn excerpt_cuts_long_ascii_at_the_limit() {
        let body = vec![b'a'; 3000];
        let out = excerpt(&body);
        assert_eq!(out.len(), EXCERPT_LIMIT);
        assert!(out.bytes().all(|b| b == b'a'));
        assert_eq!(excerpt(&vec![b'b'; EXCERPT_LIMIT]).len(), EXCERPT_LIMIT);
    }

    #[test]
    fn excerpt_never_splits_a_multibyte_char() {
        // "é" (2 bytes) straddles byte 1024: it is dropped whole.
        let mut body = vec![b'a'; EXCERPT_LIMIT - 1];
        body.extend_from_slice("é tail".as_bytes());
        let out = excerpt(&body);
        assert_eq!(out.len(), EXCERPT_LIMIT - 1);
        assert!(out.bytes().all(|b| b == b'a'));

        // "€" (3 bytes) at 1022..1025: dropped whole.
        let mut body = vec![b'a'; EXCERPT_LIMIT - 2];
        body.extend_from_slice("€€".as_bytes());
        assert_eq!(excerpt(&body).len(), EXCERPT_LIMIT - 2);

        // "é" ending exactly at byte 1024: kept.
        let mut body = vec![b'a'; EXCERPT_LIMIT - 2];
        body.extend_from_slice("éé".as_bytes());
        let out = excerpt(&body);
        assert_eq!(out.len(), EXCERPT_LIMIT);
        assert!(out.ends_with('é'));
    }

    #[test]
    fn excerpt_decodes_invalid_utf8_lossily() {
        assert_eq!(excerpt(b"ok\xffok"), "ok\u{FFFD}ok");
        // A replacement char (3 bytes) at byte 1023 would cross the limit: dropped.
        let mut body = vec![b'a'; EXCERPT_LIMIT - 1];
        body.extend_from_slice(b"\xff\xfe");
        let out = excerpt(&body);
        assert_eq!(out.len(), EXCERPT_LIMIT - 1);
        assert!(!out.contains('\u{FFFD}'));
    }
}
