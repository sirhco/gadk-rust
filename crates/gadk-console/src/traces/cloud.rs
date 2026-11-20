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

//! The Cloud Trace v1 and Cloud Logging trace source.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use chrono::{DateTime, Utc};
use gadk_cloudtrace::{Adc, AuthHeaders, CloudTraceClient, Filter, ListRequest, TraceError, View};
use gadk_model::{HttpClient, ModelError, ReqwestClient, collect_body, transport_error_detail};
use http::header::CONTENT_TYPE;
use http::{HeaderMap, HeaderValue, Method};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::{
    LogEntry, LogSet, RawSpan, Service, SourceError, TraceDetail, TraceId, TracePage, TraceQuery,
    TraceSource, TraceSummary, excerpt,
};
use crate::url::percent_encode;

const LOGGING_BASE: &str = "https://logging.googleapis.com";
/// Traces per list page.
const LIST_PAGE_SIZE: u32 = 50;
/// Log entries kept per trace (and the Logging page size).
const MAX_LOG_ENTRIES: usize = 1000;
/// Logging pages fetched per trace before the entries are marked truncated.
const MAX_LOG_PAGES: usize = 20;
/// Whole-request timeout of the production HTTP client.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const ERROR_INFO: &str = "type.googleapis.com/google.rpc.ErrorInfo";
const QUOTA_PROJECT: &str = "x-goog-user-project";
const AGENT_LABEL: &str = "gen_ai.agent.name";
const SESSION_LABEL: &str = "gen_ai.conversation.id";

/// Reads traces from Cloud Trace v1 (through [`CloudTraceClient`]) and their log entries from
/// Cloud Logging.
pub struct CloudTraceSource {
    client: CloudTraceClient,
    http: Arc<dyn HttpClient>,
    auth: Arc<dyn AuthHeaders>,
    project: String,
    log_id: String,
    logging_base: String,
    now: fn() -> DateTime<Utc>,
}

impl CloudTraceSource {
    /// Real endpoints, a reqwest client, lazily built ADC (cloud-platform scope).
    pub fn new(project: impl Into<String>, log_id: impl Into<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .unwrap_or_default();
        Self::from_parts(
            Arc::new(ReqwestClient::new(client)),
            Arc::new(Adc::lazy()),
            project.into(),
            log_id.into(),
            gadk_cloudtrace::ENDPOINT,
            LOGGING_BASE,
            Utc::now,
        )
    }

    /// Every part injected (tests).
    #[cfg(test)]
    pub(crate) fn with_parts(
        http: Arc<dyn HttpClient>,
        auth: Arc<dyn AuthHeaders>,
        project: &str,
        log_id: &str,
        trace_base: &str,
        logging_base: &str,
        now: fn() -> DateTime<Utc>,
    ) -> Self {
        Self::from_parts(
            http,
            auth,
            project.to_owned(),
            log_id.to_owned(),
            trace_base,
            logging_base,
            now,
        )
    }

    /// The Trace client and the Logging path share one HTTP client and one credential source.
    fn from_parts(
        http: Arc<dyn HttpClient>,
        auth: Arc<dyn AuthHeaders>,
        project: String,
        log_id: String,
        trace_base: &str,
        logging_base: &str,
        now: fn() -> DateTime<Utc>,
    ) -> Self {
        Self {
            client: CloudTraceClient::with_parts(
                project.clone(),
                http.clone(),
                auth.clone(),
                trace_base,
            ),
            http,
            auth,
            project,
            log_id,
            logging_base: logging_base.to_owned(),
            now,
        }
    }

    /// The `traces.list` request for one page of `query`: the range ending now, root spans
    /// only, newest first, [`LIST_PAGE_SIZE`] per page.
    fn list_request(&self, query: &TraceQuery) -> ListRequest {
        let end = (self.now)();
        let mut filters = Vec::new();
        if let Some(name) = &query.name {
            filters.push(Filter::Root(name.clone()));
        }
        if let Some(agent) = &query.agent {
            filters.push(Filter::LabelExact(AGENT_LABEL.to_owned(), agent.clone()));
        }
        if let Some(session) = &query.session {
            filters.push(Filter::LabelExact(
                SESSION_LABEL.to_owned(),
                session.clone(),
            ));
        }
        let mut req = ListRequest::new(end - query.range.duration(), end);
        req.view = View::RootSpan;
        req.filters = filters;
        req.start_descending = true;
        req.page_size = LIST_PAGE_SIZE;
        req.page_token = query.page_token.clone();
        req
    }

    /// Sends one Cloud Logging request; the 2xx body, or the mapped error.
    async fn call(
        &self,
        service: Service,
        method: Method,
        url: &str,
        body: Option<&Value>,
    ) -> Result<Bytes, SourceError> {
        let mut headers: HeaderMap = self
            .auth
            .headers()
            .await
            .map_err(SourceError::Credentials)?;
        let project = HeaderValue::from_str(&self.project).map_err(|e| SourceError::Transport {
            service,
            detail: format!("project id {:?}: {e}", self.project),
        })?;
        // `insert` replaces any quota project the credentials carry.
        headers.insert(QUOTA_PROJECT, project);
        let bytes = match body {
            Some(v) => {
                headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
                Bytes::from(v.to_string())
            }
            None => Bytes::new(),
        };
        let mut req = http::Request::builder()
            .method(method)
            .uri(url)
            .body(bytes)
            .map_err(|e| SourceError::Transport {
                service,
                detail: format!("request {url}: {e}"),
            })?;
        *req.headers_mut() = headers;
        let resp = self
            .http
            .send(req)
            .await
            .map_err(|e| transport(service, e))?;
        let status = resp.status();
        let body = collect_body(resp.into_body())
            .await
            .map_err(|e| transport(service, e))?;
        if status.is_success() {
            Ok(body)
        } else {
            Err(SourceError::Upstream {
                service,
                status: status.as_u16(),
                excerpt: excerpt(&body),
                reason: error_reason(&body),
            })
        }
    }

    /// Up to [`MAX_LOG_ENTRIES`] entries of trace `id`, oldest first.
    ///
    /// Paging stops after [`MAX_LOG_PAGES`] pages or when a page token repeats
    /// the previous one; either stop marks the entries truncated.
    async fn logs(&self, id: &TraceId) -> Result<LogSet, SourceError> {
        let p = &self.project;
        let filter = format!(
            "logName=\"projects/{p}/logs/{}\" AND trace=\"projects/{p}/traces/{}\"",
            percent_encode(&self.log_id),
            id.as_str(),
        );
        let first = json!({
            "resourceNames": [format!("projects/{p}")],
            "filter": filter,
            "orderBy": "timestamp asc",
            "pageSize": MAX_LOG_ENTRIES,
        });
        let url = format!("{}/v2/entries:list", self.logging_base);
        let mut entries = Vec::new();
        let mut token: Option<String> = None;
        let mut dropped = false;
        let mut cut_short = false;
        let mut pages = 0;
        loop {
            let mut body = first.clone();
            if let Some(t) = &token {
                body["pageToken"] = json!(t);
            }
            let raw = self
                .call(Service::Logging, Method::POST, &url, Some(&body))
                .await?;
            pages += 1;
            let page: EntriesPage = decode(Service::Logging, &raw)?;
            for e in page.entries {
                if entries.len() == MAX_LOG_ENTRIES {
                    dropped = true;
                    break;
                }
                entries.push(LogEntry {
                    timestamp: e.timestamp,
                    span_id: e.span_id.filter(|s| !s.is_empty()),
                    severity: e.severity,
                    payload: e.json_payload.unwrap_or(Value::Null),
                });
            }
            let next = page.next_page_token.filter(|t| !t.is_empty());
            let repeated = next.is_some() && next == token;
            token = next;
            if entries.len() >= MAX_LOG_ENTRIES || token.is_none() {
                break;
            }
            if repeated || pages >= MAX_LOG_PAGES {
                cut_short = true;
                break;
            }
        }
        let truncated =
            dropped || cut_short || (entries.len() == MAX_LOG_ENTRIES && token.is_some());
        Ok(LogSet { entries, truncated })
    }
}

#[async_trait::async_trait]
impl TraceSource for CloudTraceSource {
    async fn list(&self, query: &TraceQuery) -> Result<TracePage, SourceError> {
        let page = self
            .client
            .list(&self.list_request(query))
            .await
            .map_err(trace_error)?;
        let traces = page
            .traces
            .into_iter()
            .map(summary)
            .collect::<Result<_, _>>()?;
        Ok(TracePage {
            traces,
            next: page.next_page_token,
        })
    }

    async fn get(&self, id: &TraceId) -> Result<TraceDetail, SourceError> {
        let trace = self.client.get(id.as_str()).await.map_err(trace_error)?;
        Ok(TraceDetail {
            trace_id: id.clone(),
            spans: trace.spans.into_iter().map(raw_span).collect(),
            logs: self.logs(id).await,
        })
    }
}

/// Cloud Logging `ListLogEntriesResponse`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EntriesPage {
    #[serde(default)]
    entries: Vec<V2Entry>,
    next_page_token: Option<String>,
}

/// Cloud Logging `LogEntry` (`trace` is ignored: the filter already pins it).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct V2Entry {
    timestamp: DateTime<Utc>,
    span_id: Option<String>,
    severity: Option<String>,
    json_payload: Option<Value>,
}

/// A Cloud Trace failure as the console's error, attributed to [`Service::Trace`].
fn trace_error(err: TraceError) -> SourceError {
    match err {
        TraceError::Http {
            status,
            reason,
            excerpt,
        } => SourceError::Upstream {
            service: Service::Trace,
            status,
            excerpt,
            reason,
        },
        TraceError::NotFound => SourceError::NotFound,
        TraceError::Transport(detail) => SourceError::Transport {
            service: Service::Trace,
            detail,
        },
        TraceError::Decode(detail) => SourceError::Decode {
            service: Service::Trace,
            detail,
        },
        TraceError::Credentials(detail) => SourceError::Credentials(detail),
        // `TraceError` is non-exhaustive: a future kind reads as a transport failure.
        other => SourceError::Transport {
            service: Service::Trace,
            detail: other.to_string(),
        },
    }
}

/// A list row from a `ROOTSPAN` trace: its single span is the root. A trace with no spans has
/// an empty name, a zero duration and the Unix epoch as its start.
fn summary(trace: gadk_cloudtrace::Trace) -> Result<TraceSummary, SourceError> {
    let trace_id = parse_trace_id(&trace.trace_id)?;
    let Some(root) = trace.spans.into_iter().next() else {
        return Ok(TraceSummary {
            trace_id,
            root_name: String::new(),
            start: DateTime::<Utc>::UNIX_EPOCH,
            duration_ms: 0.0,
            agent: None,
            session: None,
        });
    };
    let micros = (root.end - root.start)
        .num_microseconds()
        .unwrap_or_default();
    Ok(TraceSummary {
        trace_id,
        duration_ms: micros as f64 / 1000.0,
        start: root.start,
        agent: label(&root.labels, AGENT_LABEL),
        session: label(&root.labels, SESSION_LABEL),
        root_name: root.name,
    })
}

fn label(labels: &BTreeMap<String, String>, key: &str) -> Option<String> {
    labels.get(key).cloned()
}

fn parse_trace_id(s: &str) -> Result<TraceId, SourceError> {
    TraceId::parse(s).ok_or_else(|| SourceError::Decode {
        service: Service::Trace,
        detail: format!("trace id {s:?} is not 32 hex digits"),
    })
}

/// A client span as the console's span (ids already hex; a parent that is absent, zero or
/// unparsable is already `None`).
fn raw_span(span: gadk_cloudtrace::Span) -> RawSpan {
    RawSpan {
        span_id: span.span_id,
        parent_id: span.parent_id,
        name: span.name,
        start: span.start,
        end: span.end,
        labels: span.labels,
    }
}

fn decode<T: DeserializeOwned>(service: Service, body: &[u8]) -> Result<T, SourceError> {
    serde_json::from_slice(body).map_err(|e| SourceError::Decode {
        service,
        detail: e.to_string(),
    })
}

fn transport(service: Service, err: ModelError) -> SourceError {
    let detail = match err {
        ModelError::Transport(e) => transport_error_detail(e),
        other => other.to_string(),
    };
    SourceError::Transport { service, detail }
}

/// `error.details[*].reason` of the first `google.rpc.ErrorInfo` detail of a Google error body.
fn error_reason(body: &[u8]) -> Option<String> {
    let v: Value = serde_json::from_slice(body).ok()?;
    v.pointer("/error/details")?
        .as_array()?
        .iter()
        .find(|d| d.get("@type").and_then(Value::as_str) == Some(ERROR_INFO))?
        .get("reason")?
        .as_str()
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;
    use crate::traces::test_http::FakeHttp;
    use crate::traces::{EXCERPT_LIMIT, TimeRange};

    /// Fixed credentials headers, including a stale quota project that must be replaced.
    struct FixedAuth;

    #[async_trait::async_trait]
    impl AuthHeaders for FixedAuth {
        async fn headers(&self) -> Result<HeaderMap, String> {
            let mut h = HeaderMap::new();
            h.insert("authorization", HeaderValue::from_static("Bearer t0k"));
            h.insert("x-goog-user-project", HeaderValue::from_static("other"));
            Ok(h)
        }
    }

    struct NoAuth;

    #[async_trait::async_trait]
    impl AuthHeaders for NoAuth {
        async fn headers(&self) -> Result<HeaderMap, String> {
            Err("no ADC".into())
        }
    }

    fn fixed_now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 16, 12, 0, 0).unwrap()
    }

    fn source_with(http: Arc<FakeHttp>, log_id: &str) -> CloudTraceSource {
        CloudTraceSource::with_parts(
            http,
            Arc::new(FixedAuth),
            "proj",
            log_id,
            "https://trace.test",
            "https://logging.test",
            fixed_now,
        )
    }

    fn source(http: Arc<FakeHttp>) -> CloudTraceSource {
        source_with(http, "gadk")
    }

    fn ok(body: Value) -> Result<(u16, String), String> {
        Ok((200, body.to_string()))
    }

    const ID: &str = "0123456789abcdef0123456789abcdef";
    const LIST_PREFIX: &str = "https://trace.test/v1/projects/proj/traces?view=ROOTSPAN\
        &orderBy=start%20desc&pageSize=50\
        &startTime=2026-11-16T11%3A00%3A00Z&endTime=2026-11-16T12%3A00%3A00Z";

    fn id() -> TraceId {
        TraceId::parse(ID).unwrap()
    }

    /// `n` Logging entries numbered from `from`, plus `nextPageToken` when given.
    fn entries_page(from: usize, n: usize, token: Option<&str>) -> Value {
        let entries: Vec<Value> = (from..from + n)
            .map(|i| {
                json!({
                    "timestamp": "2026-11-16T11:59:00Z",
                    "spanId": "0000000000003039",
                    "trace": format!("projects/proj/traces/{ID}"),
                    "jsonPayload": {"i": i},
                })
            })
            .collect();
        let mut page = json!({ "entries": entries });
        if let Some(t) = token {
            page["nextPageToken"] = json!(t);
        }
        page
    }

    // ---- list ----

    #[tokio::test]
    async fn list_default_query_url() {
        let http = FakeHttp::new(vec![ok(json!({}))]);
        let page = source(http.clone())
            .list(&TraceQuery::default())
            .await
            .unwrap();
        assert_eq!(page, TracePage::default());
        assert_eq!(http.uris(), vec![LIST_PREFIX.to_owned()]);
        let seen = http.seen.lock().unwrap();
        assert_eq!(seen[0].method, Method::GET);
        assert!(seen[0].body.is_empty());
    }

    #[tokio::test]
    async fn list_filter_per_parameter_is_quoted_and_escaped() {
        let cases = [
            (
                TraceQuery {
                    name: Some("chat turn".into()),
                    ..TraceQuery::default()
                },
                "root%3A%22chat%20turn%22",
            ),
            (
                TraceQuery {
                    agent: Some(r#"my "bot""#.into()),
                    ..TraceQuery::default()
                },
                "%2Bgen_ai.agent.name%3A%22my%20%5C%22bot%5C%22%22",
            ),
            (
                TraceQuery {
                    session: Some(r"s\1".into()),
                    ..TraceQuery::default()
                },
                "%2Bgen_ai.conversation.id%3A%22s%5C%5C1%22",
            ),
            (
                TraceQuery {
                    name: Some("a".into()),
                    agent: Some("b".into()),
                    session: Some("c".into()),
                    ..TraceQuery::default()
                },
                "root%3A%22a%22%20%2Bgen_ai.agent.name%3A%22b%22\
                 %20%2Bgen_ai.conversation.id%3A%22c%22",
            ),
        ];
        for (query, filter) in cases {
            let http = FakeHttp::new(vec![ok(json!({}))]);
            source(http.clone()).list(&query).await.unwrap();
            assert_eq!(http.uris(), vec![format!("{LIST_PREFIX}&filter={filter}")]);
        }
    }

    #[tokio::test]
    async fn list_skips_empty_filters_and_applies_the_range() {
        let http = FakeHttp::new(vec![ok(json!({}))]);
        let query = TraceQuery {
            range: TimeRange::Days7,
            name: Some(String::new()),
            agent: Some(String::new()),
            ..TraceQuery::default()
        };
        source(http.clone()).list(&query).await.unwrap();
        assert_eq!(
            http.uris(),
            vec![
                "https://trace.test/v1/projects/proj/traces?view=ROOTSPAN\
                 &orderBy=start%20desc&pageSize=50\
                 &startTime=2026-11-09T12%3A00%3A00Z&endTime=2026-11-16T12%3A00%3A00Z"
                    .to_owned()
            ]
        );
    }

    #[tokio::test]
    async fn list_passes_the_page_token_through() {
        let http = FakeHttp::new(vec![ok(json!({}))]);
        let query = TraceQuery {
            agent: Some("b".into()),
            page_token: Some("tok/1=".into()),
            ..TraceQuery::default()
        };
        source(http.clone()).list(&query).await.unwrap();
        assert_eq!(
            http.uris(),
            vec![format!(
                "{LIST_PREFIX}&filter=%2Bgen_ai.agent.name%3A%22b%22&pageToken=tok%2F1%3D"
            )]
        );
    }

    #[tokio::test]
    async fn list_maps_next_page_token() {
        let http = FakeHttp::new(vec![
            ok(json!({"traces": [], "nextPageToken": "abc"})),
            ok(json!({"nextPageToken": ""})),
        ]);
        let src = source(http);
        let q = TraceQuery::default();
        assert_eq!(src.list(&q).await.unwrap().next.as_deref(), Some("abc"));
        assert_eq!(src.list(&q).await.unwrap().next, None);
    }

    #[tokio::test]
    async fn list_maps_root_span_summaries() {
        let http = FakeHttp::new(vec![ok(json!({
            "traces": [
                {
                    "projectId": "proj",
                    "traceId": ID,
                    "spans": [{
                        "spanId": "12345",
                        "kind": "RPC_SERVER",
                        "name": "invoke_agent weather",
                        "startTime": "2026-11-16T11:30:00.000000000Z",
                        "endTime": "2026-11-16T11:30:01.250500000Z",
                        "labels": {
                            "gen_ai.agent.name": "weather",
                            "gen_ai.conversation.id": "s-1",
                            "g.co/agent": "otel"
                        }
                    }]
                },
                {
                    "projectId": "proj",
                    "traceId": "FEDCBA9876543210FEDCBA9876543210",
                    "spans": [{
                        "spanId": "7",
                        "name": "plain",
                        "startTime": "2026-11-16T11:10:00Z",
                        "endTime": "2026-11-16T11:10:00.002Z"
                    }]
                },
                {"projectId": "proj", "traceId": "00000000000000000000000000000001"}
            ]
        }))]);
        let page = source(http).list(&TraceQuery::default()).await.unwrap();
        let start = Utc.with_ymd_and_hms(2026, 11, 16, 11, 30, 0).unwrap();
        assert_eq!(
            page.traces,
            vec![
                TraceSummary {
                    trace_id: id(),
                    root_name: "invoke_agent weather".into(),
                    start,
                    duration_ms: 1250.5,
                    agent: Some("weather".into()),
                    session: Some("s-1".into()),
                },
                TraceSummary {
                    trace_id: TraceId::parse("fedcba9876543210fedcba9876543210").unwrap(),
                    root_name: "plain".into(),
                    start: Utc.with_ymd_and_hms(2026, 11, 16, 11, 10, 0).unwrap(),
                    duration_ms: 2.0,
                    agent: None,
                    session: None,
                },
                TraceSummary {
                    trace_id: TraceId::parse("00000000000000000000000000000001").unwrap(),
                    root_name: String::new(),
                    start: DateTime::<Utc>::UNIX_EPOCH,
                    duration_ms: 0.0,
                    agent: None,
                    session: None,
                },
            ]
        );
        assert_eq!(page.next, None);
    }

    #[tokio::test]
    async fn list_rejects_a_bad_trace_id() {
        let http = FakeHttp::new(vec![ok(json!({"traces": [{"traceId": "xyz"}]}))]);
        let err = source(http).list(&TraceQuery::default()).await.unwrap_err();
        assert!(
            matches!(err, SourceError::Decode { service: Service::Trace, ref detail } if detail.contains("xyz")),
            "{err:?}"
        );
    }

    // ---- get ----

    #[tokio::test]
    async fn get_maps_spans_and_fetches_logs() {
        let http = FakeHttp::new(vec![
            ok(json!({
                "projectId": "proj",
                "traceId": ID,
                "spans": [
                    {
                        "spanId": "12345",
                        "kind": "RPC_SERVER",
                        "name": "invoke_agent weather",
                        "startTime": "2026-11-16T11:30:00Z",
                        "endTime": "2026-11-16T11:30:02Z",
                        "labels": {"gen_ai.agent.name": "weather"}
                    },
                    {
                        "spanId": "18446744073709551615",
                        "kind": "SPAN_KIND_UNSPECIFIED",
                        "name": "call_llm",
                        "startTime": "2026-11-16T11:30:00.500Z",
                        "endTime": "2026-11-16T11:30:01Z",
                        "parentSpanId": "12345"
                    },
                    {
                        "spanId": "1",
                        "name": "zero parent",
                        "startTime": "2026-11-16T11:30:00Z",
                        "endTime": "2026-11-16T11:30:00Z",
                        "parentSpanId": "0"
                    }
                ]
            })),
            ok(json!({
                "entries": [{
                    "timestamp": "2026-11-16T11:30:00.750Z",
                    "spanId": "ffffffffffffffff",
                    "severity": "INFO",
                    "trace": format!("projects/proj/traces/{ID}"),
                    "jsonPayload": {"event": "gen_ai.choice"}
                }, {
                    "timestamp": "2026-11-16T11:30:01Z",
                    "trace": format!("projects/proj/traces/{ID}")
                }]
            })),
        ]);
        let detail = source(http.clone()).get(&id()).await.unwrap();
        let t = |h, m, s, ms| {
            Utc.with_ymd_and_hms(2026, 11, 16, h, m, s).unwrap()
                + chrono::TimeDelta::milliseconds(ms)
        };
        assert_eq!(
            detail,
            TraceDetail {
                trace_id: id(),
                spans: vec![
                    RawSpan {
                        span_id: "0000000000003039".into(),
                        parent_id: None,
                        name: "invoke_agent weather".into(),
                        start: t(11, 30, 0, 0),
                        end: t(11, 30, 2, 0),
                        labels: BTreeMap::from([(
                            "gen_ai.agent.name".to_owned(),
                            "weather".to_owned()
                        )]),
                    },
                    RawSpan {
                        span_id: "ffffffffffffffff".into(),
                        parent_id: Some("0000000000003039".into()),
                        name: "call_llm".into(),
                        start: t(11, 30, 0, 500),
                        end: t(11, 30, 1, 0),
                        labels: BTreeMap::new(),
                    },
                    RawSpan {
                        span_id: "0000000000000001".into(),
                        parent_id: None,
                        name: "zero parent".into(),
                        start: t(11, 30, 0, 0),
                        end: t(11, 30, 0, 0),
                        labels: BTreeMap::new(),
                    },
                ],
                logs: Ok(LogSet {
                    entries: vec![
                        LogEntry {
                            timestamp: t(11, 30, 0, 750),
                            span_id: Some("ffffffffffffffff".into()),
                            severity: Some("INFO".into()),
                            payload: json!({"event": "gen_ai.choice"}),
                        },
                        LogEntry {
                            timestamp: t(11, 30, 1, 0),
                            span_id: None,
                            severity: None,
                            payload: Value::Null,
                        },
                    ],
                    truncated: false,
                }),
            }
        );
        assert_eq!(
            http.uris(),
            vec![
                format!("https://trace.test/v1/projects/proj/traces/{ID}"),
                "https://logging.test/v2/entries:list".to_owned(),
            ]
        );
        let seen = http.seen.lock().unwrap();
        assert_eq!(seen[0].method, Method::GET);
        assert_eq!(seen[1].method, Method::POST);
        assert_eq!(seen[1].headers["content-type"], "application/json");
    }

    #[tokio::test]
    async fn get_ignores_an_unparsable_parent() {
        let http = FakeHttp::new(vec![
            ok(json!({"traceId": ID, "spans": [{
                "spanId": "5", "name": "s", "parentSpanId": "not-a-number",
                "startTime": "2026-11-16T11:30:00Z", "endTime": "2026-11-16T11:30:00Z"
            }]})),
            ok(json!({})),
        ]);
        let detail = source(http).get(&id()).await.unwrap();
        assert_eq!(detail.spans[0].parent_id, None);
        assert_eq!(detail.logs, Ok(LogSet::default()));
    }

    #[tokio::test]
    async fn get_rejects_an_unparsable_span_id() {
        let http = FakeHttp::new(vec![ok(json!({"traceId": ID, "spans": [{
            "spanId": "0x1f", "name": "s",
            "startTime": "2026-11-16T11:30:00Z", "endTime": "2026-11-16T11:30:00Z"
        }]}))]);
        let err = source(http.clone()).get(&id()).await.unwrap_err();
        assert!(
            matches!(err, SourceError::Decode { service: Service::Trace, ref detail } if detail.contains("0x1f")),
            "{err:?}"
        );
        assert_eq!(http.uris().len(), 1, "no Logging call after a bad trace");
    }

    #[tokio::test]
    async fn get_404_is_not_found() {
        let http = FakeHttp::new(vec![Ok((404, r#"{"error":{"code":404}}"#.into()))]);
        assert_eq!(
            source(http.clone()).get(&id()).await,
            Err(SourceError::NotFound)
        );
        assert_eq!(http.uris().len(), 1);
    }

    // ---- logs ----

    #[tokio::test]
    async fn logging_body_and_log_name_encoding() {
        let http = FakeHttp::new(vec![
            ok(json!({"traceId": ID})),
            ok(entries_page(0, 1, Some("next-1"))),
            ok(entries_page(1, 1, None)),
        ]);
        source_with(http.clone(), "my log/x")
            .get(&id())
            .await
            .unwrap();
        let filter = format!(
            "logName=\"projects/proj/logs/my%20log%2Fx\" AND trace=\"projects/proj/traces/{ID}\""
        );
        assert_eq!(
            http.body(1),
            json!({
                "resourceNames": ["projects/proj"],
                "filter": filter,
                "orderBy": "timestamp asc",
                "pageSize": 1000
            })
        );
        assert_eq!(
            http.body(2),
            json!({
                "resourceNames": ["projects/proj"],
                "filter": filter,
                "orderBy": "timestamp asc",
                "pageSize": 1000,
                "pageToken": "next-1"
            })
        );
    }

    #[tokio::test]
    async fn logs_stop_at_1000_and_mark_truncation_when_a_token_remains() {
        let http = FakeHttp::new(vec![
            ok(json!({"traceId": ID})),
            ok(entries_page(0, 600, Some("t1"))),
            ok(entries_page(600, 400, Some("t2"))),
        ]);
        let logs = source(http.clone()).get(&id()).await.unwrap().logs.unwrap();
        assert_eq!(logs.entries.len(), 1000);
        assert_eq!(logs.entries[999].payload, json!({"i": 999}));
        assert!(logs.truncated);
        assert_eq!(http.uris().len(), 3, "no request after 1,000 entries");
    }

    #[tokio::test]
    async fn logs_of_exactly_1000_without_a_token_are_complete() {
        let http = FakeHttp::new(vec![
            ok(json!({"traceId": ID})),
            ok(entries_page(0, 600, Some("t1"))),
            ok(entries_page(600, 400, None)),
        ]);
        let logs = source(http).get(&id()).await.unwrap().logs.unwrap();
        assert_eq!(logs.entries.len(), 1000);
        assert!(!logs.truncated);
    }

    #[tokio::test]
    async fn logs_beyond_1000_are_dropped_and_marked_truncated() {
        let http = FakeHttp::new(vec![
            ok(json!({"traceId": ID})),
            ok(entries_page(0, 600, Some("t1"))),
            ok(entries_page(600, 600, None)),
        ]);
        let logs = source(http).get(&id()).await.unwrap().logs.unwrap();
        assert_eq!(logs.entries.len(), 1000);
        assert_eq!(logs.entries[999].payload, json!({"i": 999}));
        assert!(logs.truncated);
    }

    #[tokio::test]
    async fn logs_stop_when_the_page_token_repeats() {
        let mut replies = vec![ok(json!({"traceId": ID}))];
        replies.extend((0..25).map(|_| ok(entries_page(0, 0, Some("same")))));
        let http = FakeHttp::new(replies);
        let logs = source(http.clone()).get(&id()).await.unwrap().logs.unwrap();
        assert!(logs.entries.is_empty());
        assert!(logs.truncated);
        assert_eq!(
            http.uris().len(),
            3,
            "stop after the second Logging request"
        );
    }

    #[tokio::test]
    async fn logs_stop_at_the_page_cap() {
        let mut replies = vec![ok(json!({"traceId": ID}))];
        replies.extend((0..25).map(|i| ok(entries_page(0, 0, Some(&format!("t{i}"))))));
        let http = FakeHttp::new(replies);
        let logs = source(http.clone()).get(&id()).await.unwrap().logs.unwrap();
        assert!(logs.entries.is_empty());
        assert!(logs.truncated);
        assert_eq!(http.uris().len(), 1 + 20, "exactly 20 Logging requests");
    }

    #[tokio::test]
    async fn a_logging_failure_still_returns_the_trace() {
        let http = FakeHttp::new(vec![
            ok(json!({"traceId": ID, "spans": [{
                "spanId": "5", "name": "s",
                "startTime": "2026-11-16T11:30:00Z", "endTime": "2026-11-16T11:30:00Z"
            }]})),
            Ok((
                403,
                r#"{"error":{"code":403,"status":"PERMISSION_DENIED"}}"#.into(),
            )),
        ]);
        let detail = source(http).get(&id()).await.unwrap();
        assert_eq!(detail.spans.len(), 1);
        let err = detail.logs.unwrap_err();
        assert_eq!(
            err,
            SourceError::Upstream {
                service: Service::Logging,
                status: 403,
                excerpt: r#"{"error":{"code":403,"status":"PERMISSION_DENIED"}}"#.into(),
                reason: None,
            }
        );
        assert_eq!(err.hint(), Some("needs roles/logging.viewer"));
    }

    #[tokio::test]
    async fn bad_logging_json_is_a_logging_decode_error() {
        let http = FakeHttp::new(vec![
            ok(json!({"traceId": ID})),
            Ok((200, "not json".into())),
        ]);
        let logs = source(http).get(&id()).await.unwrap().logs;
        assert!(
            matches!(
                logs,
                Err(SourceError::Decode {
                    service: Service::Logging,
                    ..
                })
            ),
            "{logs:?}"
        );
    }

    // ---- errors and headers ----

    #[tokio::test]
    async fn service_disabled_reason_comes_from_error_info() {
        let body = json!({"error": {
            "code": 403,
            "message": "Cloud Trace API has not been used in project proj before or it is disabled.",
            "status": "PERMISSION_DENIED",
            "details": [
                {"@type": "type.googleapis.com/google.rpc.Help", "links": []},
                {
                    "@type": "type.googleapis.com/google.rpc.ErrorInfo",
                    "reason": "SERVICE_DISABLED",
                    "domain": "googleapis.com",
                    "metadata": {"service": "cloudtrace.googleapis.com"}
                }
            ]
        }})
        .to_string();
        let http = FakeHttp::new(vec![Ok((403, body.clone()))]);
        let err = source(http).list(&TraceQuery::default()).await.unwrap_err();
        assert_eq!(
            err,
            SourceError::Upstream {
                service: Service::Trace,
                status: 403,
                excerpt: body,
                reason: Some("SERVICE_DISABLED".into()),
            }
        );
        assert_eq!(err.hint(), Some("enable cloudtrace.googleapis.com"));
    }

    #[tokio::test]
    async fn a_403_without_error_info_has_no_reason() {
        let http = FakeHttp::new(vec![Ok((403, r#"{"error":{"details":[]}}"#.into()))]);
        let err = source(http).list(&TraceQuery::default()).await.unwrap_err();
        assert!(
            matches!(
                err,
                SourceError::Upstream {
                    status: 403,
                    reason: None,
                    ..
                }
            ),
            "{err:?}"
        );
        assert_eq!(err.hint(), Some("needs roles/cloudtrace.user"));
    }

    #[tokio::test]
    async fn upstream_excerpt_is_capped() {
        let http = FakeHttp::new(vec![Ok((500, "x".repeat(5000)))]);
        let err = source(http).list(&TraceQuery::default()).await.unwrap_err();
        let SourceError::Upstream {
            status, excerpt, ..
        } = err
        else {
            panic!("{err:?}");
        };
        assert_eq!(status, 500);
        assert_eq!(excerpt, "x".repeat(EXCERPT_LIMIT));
    }

    #[tokio::test]
    async fn a_transport_failure_is_transport() {
        let http = FakeHttp::new(vec![Err("connection refused".into())]);
        let err = source(http).list(&TraceQuery::default()).await.unwrap_err();
        assert!(
            matches!(err, SourceError::Transport { service: Service::Trace, ref detail } if detail.contains("connection refused")),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn bad_json_is_decode() {
        let http = FakeHttp::new(vec![Ok((200, "<html>".into()))]);
        let err = source(http).list(&TraceQuery::default()).await.unwrap_err();
        assert!(
            matches!(
                err,
                SourceError::Decode {
                    service: Service::Trace,
                    ..
                }
            ),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn a_credentials_failure_sends_nothing() {
        let http = FakeHttp::new(vec![]);
        let src = CloudTraceSource::with_parts(
            http.clone(),
            Arc::new(NoAuth),
            "proj",
            "gadk",
            "https://trace.test",
            "https://logging.test",
            fixed_now,
        );
        let err = src.list(&TraceQuery::default()).await.unwrap_err();
        assert_eq!(err, SourceError::Credentials("no ADC".into()));
        assert!(http.uris().is_empty());
    }

    #[tokio::test]
    async fn every_request_carries_one_quota_project_header() {
        let http = FakeHttp::new(vec![
            ok(json!({})),
            ok(json!({"traceId": ID})),
            ok(entries_page(0, 1, Some("t"))),
            ok(entries_page(1, 1, None)),
        ]);
        let src = source(http.clone());
        src.list(&TraceQuery::default()).await.unwrap();
        src.get(&id()).await.unwrap();
        let seen = http.seen.lock().unwrap();
        assert_eq!(seen.len(), 4);
        for s in seen.iter() {
            let values: Vec<_> = s.headers.get_all("x-goog-user-project").iter().collect();
            assert_eq!(values, vec!["proj"], "{}", s.uri);
            assert_eq!(s.headers["authorization"], "Bearer t0k");
        }
    }
}
