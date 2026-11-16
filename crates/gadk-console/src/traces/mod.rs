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

mod cloud;
pub use cloud::CloudTraceSource;

use std::collections::BTreeMap;
use std::fmt;

use chrono::{DateTime, Utc};

/// The trace-list time window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimeRange {
    #[default]
    Hour1,
    Hours6,
    Hours24,
    Days7,
}

impl TimeRange {
    /// Every range, shortest first.
    pub const ALL: [TimeRange; 4] = [
        TimeRange::Hour1,
        TimeRange::Hours6,
        TimeRange::Hours24,
        TimeRange::Days7,
    ];

    /// "1h" | "6h" | "24h" | "7d"; anything else is None.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.as_str() == s)
    }

    /// The query-string form: "1h", "6h", "24h" or "7d".
    pub fn as_str(self) -> &'static str {
        match self {
            TimeRange::Hour1 => "1h",
            TimeRange::Hours6 => "6h",
            TimeRange::Hours24 => "24h",
            TimeRange::Days7 => "7d",
        }
    }

    /// The window length.
    pub fn duration(self) -> chrono::TimeDelta {
        match self {
            TimeRange::Hour1 => chrono::TimeDelta::hours(1),
            TimeRange::Hours6 => chrono::TimeDelta::hours(6),
            TimeRange::Hours24 => chrono::TimeDelta::hours(24),
            TimeRange::Days7 => chrono::TimeDelta::days(7),
        }
    }
}

/// A trace id: exactly 32 lowercase hex characters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TraceId(String);

impl TraceId {
    /// Accepts 32 ASCII hex digits (either case) and lowercases them; anything else is None.
    pub fn parse(s: &str) -> Option<Self> {
        (s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| TraceId(s.to_ascii_lowercase()))
    }

    /// The 32 lowercase hex characters.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A trace-list request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TraceQuery {
    pub range: TimeRange,
    /// Root span name prefix.
    pub name: Option<String>,
    /// Exact `gen_ai.agent.name`.
    pub agent: Option<String>,
    /// Exact `gen_ai.conversation.id`.
    pub session: Option<String>,
    pub page_token: Option<String>,
}

/// One trace-list row.
#[derive(Debug, Clone, PartialEq)]
pub struct TraceSummary {
    pub trace_id: TraceId,
    pub root_name: String,
    pub start: DateTime<Utc>,
    pub duration_ms: f64,
    pub agent: Option<String>,
    pub session: Option<String>,
}

/// One page of the trace list.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TracePage {
    pub traces: Vec<TraceSummary>,
    pub next: Option<String>,
}

/// One span. Ids are 16 lowercase hex characters.
#[derive(Debug, Clone, PartialEq)]
pub struct RawSpan {
    pub span_id: String,
    /// None when absent or zero.
    pub parent_id: Option<String>,
    pub name: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub labels: BTreeMap<String, String>,
}

/// One Cloud Logging entry of the trace.
#[derive(Debug, Clone, PartialEq)]
pub struct LogEntry {
    pub timestamp: DateTime<Utc>,
    /// 16 lowercase hex, as written by gadk-telemetry.
    pub span_id: Option<String>,
    pub severity: Option<String>,
    /// The entry's `jsonPayload` (Null when absent).
    pub payload: serde_json::Value,
}

/// A trace's log entries; `truncated` when more than the collected 1,000 exist.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LogSet {
    pub entries: Vec<LogEntry>,
    pub truncated: bool,
}

/// A trace with its spans and (separately fallible) log entries.
#[derive(Debug, Clone, PartialEq)]
pub struct TraceDetail {
    pub trace_id: TraceId,
    pub spans: Vec<RawSpan>,
    pub logs: Result<LogSet, SourceError>,
}

/// The upstream API a [`SourceError`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    Trace,
    Logging,
}

impl fmt::Display for Service {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Service::Trace => "Cloud Trace",
            Service::Logging => "Cloud Logging",
        })
    }
}

/// A failed trace or log read.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum SourceError {
    /// Non-2xx reply. `excerpt` is at most 1,024 bytes of the body, cut on a char boundary.
    #[error("{service}: HTTP {status}: {excerpt}")]
    Upstream {
        service: Service,
        status: u16,
        excerpt: String,
        reason: Option<String>,
    },
    #[error("{service}: {detail}")]
    Transport { service: Service, detail: String },
    #[error("{service}: unexpected response: {detail}")]
    Decode { service: Service, detail: String },
    #[error("credentials: {0}")]
    Credentials(String),
    /// The trace id is unknown to Cloud Trace (HTTP 404 on get).
    #[error("trace not found")]
    NotFound,
}

impl SourceError {
    /// The R13 hint shown under the error banner, if any.
    pub fn hint(&self) -> Option<&'static str> {
        match self {
            SourceError::Upstream {
                service,
                reason: Some(reason),
                ..
            } if reason == "SERVICE_DISABLED" => Some(match service {
                Service::Trace => "enable cloudtrace.googleapis.com",
                Service::Logging => "enable logging.googleapis.com",
            }),
            SourceError::Upstream {
                service: Service::Trace,
                status: 403,
                ..
            } => Some("needs roles/cloudtrace.user"),
            SourceError::Upstream {
                service: Service::Logging,
                status: 403,
                ..
            } => Some("needs roles/logging.viewer"),
            SourceError::Credentials(_) => Some("run `gcloud auth application-default login`"),
            _ => None,
        }
    }
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
    fn time_range_parses_its_own_strings() {
        for r in TimeRange::ALL {
            assert_eq!(TimeRange::parse(r.as_str()), Some(r));
        }
        let strs: Vec<&str> = TimeRange::ALL.iter().map(|r| r.as_str()).collect();
        assert_eq!(strs, ["1h", "6h", "24h", "7d"]);
    }

    #[test]
    fn time_range_rejects_other_strings() {
        for s in ["", "1H", "2h", "1d", "7D", " 1h", "1h ", "hour"] {
            assert_eq!(TimeRange::parse(s), None, "{s:?}");
        }
    }

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
    fn trace_id_accepts_32_hex_and_lowercases() {
        let lower = "0123456789abcdef0123456789abcdef";
        assert_eq!(
            TraceId::parse(lower).map(|t| t.as_str().to_owned()),
            Some(lower.to_owned())
        );
        let upper = "0123456789ABCDEF0123456789ABCDEF";
        assert_eq!(
            TraceId::parse(upper).map(|t| t.as_str().to_owned()),
            Some(lower.to_owned())
        );
        assert_eq!(TraceId::parse(upper), TraceId::parse(lower));
    }

    #[test]
    fn trace_id_rejects_bad_ids() {
        for s in [
            "",
            "0123456789abcdef0123456789abcde",   // 31
            "0123456789abcdef0123456789abcdef0", // 33
            "0123456789abcdef0123456789abcdeg",  // not hex
            "0123456789abcdef-123456789abcdef",  // dash
            "0123456789abcdef0123456789abcdé",   // 32 bytes, not ASCII
            " 123456789abcdef0123456789abcdef",  // space
        ] {
            assert_eq!(TraceId::parse(s), None, "{s:?}");
        }
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

    fn upstream(service: Service, status: u16, reason: Option<&str>) -> SourceError {
        SourceError::Upstream {
            service,
            status,
            excerpt: "{}".into(),
            reason: reason.map(str::to_owned),
        }
    }

    #[test]
    fn hint_for_every_row() {
        let disabled = Some("SERVICE_DISABLED");
        assert_eq!(
            upstream(Service::Trace, 403, disabled).hint(),
            Some("enable cloudtrace.googleapis.com")
        );
        assert_eq!(
            upstream(Service::Logging, 403, disabled).hint(),
            Some("enable logging.googleapis.com")
        );
        // SERVICE_DISABLED wins whatever the status.
        assert_eq!(
            upstream(Service::Logging, 400, disabled).hint(),
            Some("enable logging.googleapis.com")
        );
        assert_eq!(
            upstream(Service::Trace, 403, None).hint(),
            Some("needs roles/cloudtrace.user")
        );
        assert_eq!(
            upstream(Service::Logging, 403, None).hint(),
            Some("needs roles/logging.viewer")
        );
        assert_eq!(
            upstream(Service::Trace, 403, Some("IAM_PERMISSION_DENIED")).hint(),
            Some("needs roles/cloudtrace.user")
        );
        assert_eq!(
            SourceError::Credentials("no ADC".into()).hint(),
            Some("run `gcloud auth application-default login`")
        );
    }

    #[test]
    fn hint_is_none_otherwise() {
        assert_eq!(upstream(Service::Trace, 500, None).hint(), None);
        assert_eq!(upstream(Service::Logging, 404, Some("OTHER")).hint(), None);
        let transport = SourceError::Transport {
            service: Service::Trace,
            detail: "timed out".into(),
        };
        assert_eq!(transport.hint(), None);
        let decode = SourceError::Decode {
            service: Service::Logging,
            detail: "bad json".into(),
        };
        assert_eq!(decode.hint(), None);
        assert_eq!(SourceError::NotFound.hint(), None);
    }

    #[test]
    fn service_display() {
        assert_eq!(Service::Trace.to_string(), "Cloud Trace");
        assert_eq!(Service::Logging.to_string(), "Cloud Logging");
    }

    #[test]
    fn source_error_display() {
        assert_eq!(
            upstream(Service::Trace, 403, None).to_string(),
            "Cloud Trace: HTTP 403: {}"
        );
        let transport = SourceError::Transport {
            service: Service::Logging,
            detail: "timed out".into(),
        };
        assert_eq!(transport.to_string(), "Cloud Logging: timed out");
        let decode = SourceError::Decode {
            service: Service::Trace,
            detail: "missing traces".into(),
        };
        assert_eq!(
            decode.to_string(),
            "Cloud Trace: unexpected response: missing traces"
        );
        assert_eq!(
            SourceError::Credentials("no ADC".into()).to_string(),
            "credentials: no ADC"
        );
        assert_eq!(SourceError::NotFound.to_string(), "trace not found");
    }
}
