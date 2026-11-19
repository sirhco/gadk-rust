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

//! Requests, traces and spans, and the v1 wire types they are decoded from.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::error::TraceError;

/// A zero parent span id means "no parent".
const ZERO_SPAN_ID: &str = "0000000000000000";

/// How much of each trace a list returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum View {
    /// Only the root span (`ROOTSPAN`). A span id that is not a decimal u64 is kept as sent.
    RootSpan,
    /// Every span (`COMPLETE`). A span id that is not a decimal u64 is a
    /// [`TraceError::Decode`].
    Complete,
}

impl View {
    /// The `view` query parameter value.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            View::RootSpan => "ROOTSPAN",
            View::Complete => "COMPLETE",
        }
    }
}

/// One filter term. Terms are joined with a space (all must match); a term with an empty prefix
/// or value is not sent.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Filter {
    /// The root span's name starts with this prefix: `root:"<prefix>"`.
    Root(String),
    /// Some span's name starts with this prefix: `span:"<prefix>"`.
    Span(String),
    /// Some span has label `key` set to exactly `value`: `+<key>:"<value>"`. The key is sent
    /// verbatim, so callers must pass a valid label key; only the value is quoted.
    LabelExact(String, String),
}

impl Filter {
    /// The filter term, its value double-quoted with `\` and `"` backslash-escaped; `None` for
    /// an empty prefix or value.
    pub(crate) fn term(&self) -> Option<String> {
        let (key, value) = match self {
            Filter::Root(prefix) => ("root:".to_owned(), prefix),
            Filter::Span(prefix) => ("span:".to_owned(), prefix),
            Filter::LabelExact(k, v) => (format!("+{k}:"), v),
        };
        (!value.is_empty()).then(|| format!("{key}{}", quote(value)))
    }
}

/// A v1 filter value: double-quoted, with `\` and `"` backslash-escaped.
fn quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// One `traces.list` request. Build it with [`ListRequest::new`] and set the other fields by
/// assignment.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ListRequest {
    /// The window start (`startTime`).
    pub start: DateTime<Utc>,
    /// The window end (`endTime`).
    pub end: DateTime<Utc>,
    /// How much of each trace to return.
    pub view: View,
    /// The filter terms; all must match.
    pub filters: Vec<Filter>,
    /// Newest first (`orderBy=start desc`); `false` requests no order.
    pub start_descending: bool,
    /// The most traces per page (`pageSize`).
    pub page_size: u32,
    /// The previous page's [`TracePage::next_page_token`]; `None` or empty for the first page.
    pub page_token: Option<String>,
}

/// One page of traces.
#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct TracePage {
    /// The traces, in the order Cloud Trace returned them.
    pub traces: Vec<Trace>,
    /// The token of the next page; `None` on the last page.
    pub next_page_token: Option<String>,
}

/// A trace.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Trace {
    /// The trace id as Cloud Trace returned it (normally 32 hex characters; not validated).
    pub trace_id: String,
    /// The spans, in the order Cloud Trace returned them.
    pub spans: Vec<Span>,
}

/// A span. Build it with [`Span::new`] and set the other fields by assignment.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Span {
    /// 16 lowercase hex characters (a [`View::RootSpan`] list keeps an id that is not a decimal
    /// u64 as sent).
    pub span_id: String,
    /// The parent's id (16 lowercase hex); `None` when absent, zero or not a decimal u64.
    pub parent_id: Option<String>,
    /// The span name.
    pub name: String,
    /// When the span started.
    pub start: DateTime<Utc>,
    /// When the span ended.
    pub end: DateTime<Utc>,
    /// The span's labels (its attributes, as text).
    pub labels: BTreeMap<String, String>,
}

impl ListRequest {
    /// A request for the traces in `start..end`: [`View::RootSpan`], no filters, no order
    /// (`start_descending` false), `page_size` 0 (sent as `pageSize=0`, so Cloud Trace picks the
    /// page size) and no page token.
    pub fn new(start: DateTime<Utc>, end: DateTime<Utc>) -> Self {
        Self {
            start,
            end,
            view: View::RootSpan,
            filters: Vec::new(),
            start_descending: false,
            page_size: 0,
            page_token: None,
        }
    }
}

impl TracePage {
    /// A page of `traces`, followed by the page `next_page_token` names (`None` on the last).
    pub fn new(traces: Vec<Trace>, next_page_token: Option<String>) -> Self {
        Self {
            traces,
            next_page_token,
        }
    }
}

impl Trace {
    /// The trace `trace_id` with `spans`.
    pub fn new(trace_id: impl Into<String>, spans: Vec<Span>) -> Self {
        Self {
            trace_id: trace_id.into(),
            spans,
        }
    }
}

impl Span {
    /// The span `span_id` named `name`, from `start` to `end`, with no parent and no labels.
    pub fn new(
        span_id: impl Into<String>,
        name: impl Into<String>,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Self {
        Self {
            span_id: span_id.into(),
            parent_id: None,
            name: name.into(),
            start,
            end,
            labels: BTreeMap::new(),
        }
    }
}

/// Cloud Trace v1 `ListTracesResponse`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TracesPage {
    #[serde(default)]
    pub(crate) traces: Vec<V1Trace>,
    pub(crate) next_page_token: Option<String>,
}

/// Cloud Trace v1 `Trace` (`projectId` is ignored).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct V1Trace {
    trace_id: String,
    #[serde(default)]
    spans: Vec<V1Span>,
}

/// Cloud Trace v1 `TraceSpan` (`kind` is ignored).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct V1Span {
    span_id: String,
    #[serde(default)]
    name: String,
    start_time: DateTime<Utc>,
    end_time: DateTime<Utc>,
    parent_span_id: Option<String>,
    #[serde(default)]
    labels: BTreeMap<String, String>,
}

impl V1Trace {
    /// The trace with hex span ids; `strict` makes an unparsable span id a decode error.
    pub(crate) fn into_trace(self, strict: bool) -> Result<Trace, TraceError> {
        let spans = self
            .spans
            .into_iter()
            .map(|s| s.into_span(strict))
            .collect::<Result<_, _>>()?;
        Ok(Trace {
            trace_id: self.trace_id,
            spans,
        })
    }
}

impl V1Span {
    fn into_span(self, strict: bool) -> Result<Span, TraceError> {
        let span_id = match hex_span_id(&self.span_id) {
            Some(hex) => hex,
            None if strict => {
                return Err(TraceError::Decode(format!(
                    "span id {:?} is not a decimal u64",
                    self.span_id
                )));
            }
            None => self.span_id,
        };
        let parent_id = self
            .parent_span_id
            .as_deref()
            .and_then(hex_span_id)
            .filter(|p| p != ZERO_SPAN_ID);
        Ok(Span {
            span_id,
            parent_id,
            name: self.name,
            start: self.start_time,
            end: self.end_time,
            labels: self.labels,
        })
    }
}

/// A v1 decimal span id ("12345") as 16 lowercase hex digits ("0000000000003039"); `None` if it
/// is not a u64. "0" maps to `Some("0000000000000000")`.
pub(crate) fn hex_span_id(decimal: &str) -> Option<String> {
    // `u64::from_str` also takes a leading '+'; span ids are plain digits.
    if decimal.is_empty() || !decimal.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    decimal.parse::<u64>().ok().map(|n| format!("{n:016x}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_span_id_converts_decimal_u64() {
        assert_eq!(hex_span_id("12345").as_deref(), Some("0000000000003039"));
        assert_eq!(hex_span_id("0").as_deref(), Some("0000000000000000"));
        assert_eq!(
            hex_span_id("18446744073709551615").as_deref(),
            Some("ffffffffffffffff")
        );
        assert_eq!(hex_span_id("00255").as_deref(), Some("00000000000000ff"));
    }

    #[test]
    fn hex_span_id_rejects_non_u64() {
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
            assert_eq!(hex_span_id(s), None, "{s:?}");
        }
    }

    #[test]
    fn filter_terms_are_quoted_and_escaped() {
        assert_eq!(
            Filter::Root("chat turn".into()).term().as_deref(),
            Some(r#"root:"chat turn""#)
        );
        assert_eq!(
            Filter::Span("invoke_agent".into()).term().as_deref(),
            Some(r#"span:"invoke_agent""#)
        );
        assert_eq!(
            Filter::LabelExact("k".into(), r#"a "b" \c"#.into())
                .term()
                .as_deref(),
            Some(r#"+k:"a \"b\" \\c""#)
        );
        assert_eq!(Filter::Root(String::new()).term(), None);
        assert_eq!(Filter::Span(String::new()).term(), None);
        assert_eq!(Filter::LabelExact("k".into(), String::new()).term(), None);
    }

    #[test]
    fn constructors_default_the_other_fields() {
        let start = DateTime::from_timestamp(1_000, 0).unwrap();
        let end = DateTime::from_timestamp(2_000, 0).unwrap();

        let req = ListRequest::new(start, end);
        assert_eq!((req.start, req.end), (start, end));
        assert_eq!(req.view, View::RootSpan);
        assert!(req.filters.is_empty());
        assert!(!req.start_descending);
        assert_eq!(req.page_size, 0);
        assert_eq!(req.page_token, None);

        let span = Span::new("00000000000000ff", "invoke_agent a", start, end);
        assert_eq!(span.span_id, "00000000000000ff");
        assert_eq!(span.name, "invoke_agent a");
        assert_eq!((span.start, span.end), (start, end));
        assert_eq!(span.parent_id, None);
        assert!(span.labels.is_empty());

        let trace = Trace::new("t1", vec![span.clone()]);
        assert_eq!(trace.trace_id, "t1");
        assert_eq!(trace.spans, vec![span]);

        let page = TracePage::new(vec![trace.clone()], Some("p2".into()));
        assert_eq!(page.traces, vec![trace]);
        assert_eq!(page.next_page_token.as_deref(), Some("p2"));
        assert_eq!(TracePage::new(Vec::new(), None), TracePage::default());
    }

    #[test]
    fn view_parameter_values() {
        assert_eq!(View::RootSpan.as_str(), "ROOTSPAN");
        assert_eq!(View::Complete.as_str(), "COMPLETE");
    }
}
