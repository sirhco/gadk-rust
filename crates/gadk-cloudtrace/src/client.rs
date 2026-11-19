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

//! [`CloudTraceClient`]: `traces.list` and `traces.get` on the Cloud Trace v1 REST API.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use chrono::{DateTime, SecondsFormat, Utc};
use gadk_model::{HttpClient, ModelError, ReqwestClient, collect_body, transport_error_detail};
use http::{HeaderValue, Method};
use serde::de::DeserializeOwned;

use crate::auth::{Adc, AuthHeaders};
use crate::error::{TraceError, error_reason, excerpt};
use crate::types::{Filter, ListRequest, Trace, TracePage, TracesPage, V1Trace, View};

/// The Cloud Trace API endpoint.
pub const ENDPOINT: &str = "https://cloudtrace.googleapis.com";

/// Whole-request timeout of the default HTTP client.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

const QUOTA_PROJECT: &str = "x-goog-user-project";

/// Reads one project's traces from Cloud Trace v1.
pub struct CloudTraceClient {
    http: Arc<dyn HttpClient>,
    auth: Arc<dyn AuthHeaders>,
    project: String,
    base: String,
}

/// Shows the project and endpoint, never the credentials or a token.
impl std::fmt::Debug for CloudTraceClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CloudTraceClient")
            .field("project", &self.project)
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl CloudTraceClient {
    /// The real endpoint, a reqwest client with a 60 s timeout and lazily built Application
    /// Default Credentials ([`Adc::lazy`]).
    pub fn new(project: impl Into<String>) -> Self {
        Self::with_auth(project, Arc::new(Adc::lazy()))
    }

    /// The real endpoint and a reqwest client with a 60 s timeout, authenticated by `auth`.
    pub fn with_auth(project: impl Into<String>, auth: Arc<dyn AuthHeaders>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .unwrap_or_default();
        Self::with_parts(
            project,
            Arc::new(ReqwestClient::new(client)),
            auth,
            ENDPOINT,
        )
    }

    /// Every part given: the HTTP client, the credentials and the endpoint `base` (such as
    /// [`ENDPOINT`], without a trailing `/`).
    pub fn with_parts(
        project: impl Into<String>,
        http: Arc<dyn HttpClient>,
        auth: Arc<dyn AuthHeaders>,
        base: &str,
    ) -> Self {
        Self {
            http,
            auth,
            project: project.into(),
            base: base.to_owned(),
        }
    }

    /// The project whose traces are read.
    pub fn project(&self) -> &str {
        &self.project
    }

    /// The URL [`list`](Self::list) sends for `req`:
    /// `{base}/v1/projects/{project}/traces?view=…[&orderBy=start%20desc]&pageSize=…`
    /// `&startTime=…&endTime=…[&filter=…][&pageToken=…]`, with RFC 3339 UTC times and every value
    /// percent-encoded.
    pub fn list_url(&self, req: &ListRequest) -> String {
        let mut url = format!(
            "{}/v1/projects/{}/traces?view={}",
            self.base,
            encode(&self.project),
            req.view.as_str()
        );
        if req.start_descending {
            url.push_str("&orderBy=start%20desc");
        }
        url.push_str(&format!(
            "&pageSize={}&startTime={}&endTime={}",
            req.page_size,
            encode(&rfc3339(req.start)),
            encode(&rfc3339(req.end)),
        ));
        let terms: Vec<String> = req.filters.iter().filter_map(Filter::term).collect();
        if !terms.is_empty() {
            url.push_str("&filter=");
            url.push_str(&encode(&terms.join(" ")));
        }
        if let Some(token) = req.page_token.as_deref().filter(|t| !t.is_empty()) {
            url.push_str("&pageToken=");
            url.push_str(&encode(token));
        }
        url
    }

    /// One page of traces matching `req`.
    ///
    /// # Errors
    ///
    /// A [`TraceError`]; a 404 is [`TraceError::Http`].
    pub async fn list(&self, req: &ListRequest) -> Result<TracePage, TraceError> {
        let body = self.send(&self.list_url(req)).await?;
        let page: TracesPage = decode(&body)?;
        let strict = req.view == View::Complete;
        let traces = page
            .traces
            .into_iter()
            .map(|t| t.into_trace(strict))
            .collect::<Result<_, _>>()?;
        Ok(TracePage {
            traces,
            next_page_token: page.next_page_token.filter(|t| !t.is_empty()),
        })
    }

    /// Trace `trace_id` with all of its spans.
    ///
    /// # Errors
    ///
    /// [`TraceError::NotFound`] for an unknown trace, another [`TraceError`] otherwise.
    pub async fn get(&self, trace_id: &str) -> Result<Trace, TraceError> {
        let url = format!(
            "{}/v1/projects/{}/traces/{}",
            self.base,
            encode(&self.project),
            encode(trace_id)
        );
        let body = match self.send(&url).await {
            Err(TraceError::Http { status: 404, .. }) => return Err(TraceError::NotFound),
            other => other?,
        };
        let trace: V1Trace = decode(&body)?;
        trace.into_trace(true)
    }

    /// Sends one GET; the 2xx body, or the mapped error.
    async fn send(&self, url: &str) -> Result<Bytes, TraceError> {
        let mut headers = self.auth.headers().await.map_err(TraceError::Credentials)?;
        let project = HeaderValue::from_str(&self.project)
            .map_err(|e| TraceError::Transport(format!("project id {:?}: {e}", self.project)))?;
        // `insert` replaces any quota project the credentials carry.
        headers.insert(QUOTA_PROJECT, project);
        let mut req = http::Request::builder()
            .method(Method::GET)
            .uri(url)
            .body(Bytes::new())
            .map_err(|e| TraceError::Transport(format!("request {url}: {e}")))?;
        *req.headers_mut() = headers;
        let resp = self.http.send(req).await.map_err(transport)?;
        let status = resp.status();
        let body = collect_body(resp.into_body()).await.map_err(transport)?;
        if status.is_success() {
            Ok(body)
        } else {
            Err(TraceError::Http {
                status: status.as_u16(),
                reason: error_reason(&body),
                excerpt: excerpt(&body),
            })
        }
    }
}

fn decode<T: DeserializeOwned>(body: &[u8]) -> Result<T, TraceError> {
    serde_json::from_slice(body).map_err(|e| TraceError::Decode(e.to_string()))
}

fn transport(err: ModelError) -> TraceError {
    TraceError::Transport(match err {
        ModelError::Transport(e) => transport_error_detail(e),
        other => other.to_string(),
    })
}

/// RFC 3339 in UTC with a `Z` suffix and only the fractional digits needed.
fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::AutoSi, true)
}

/// Percent-encodes every byte except the RFC 3986 unreserved characters
/// (`A-Z a-z 0-9 - . _ ~`), as uppercase `%XX`.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, VecDeque};
    use std::sync::Mutex;

    use chrono::TimeZone;
    use futures::StreamExt as _;
    use gadk_model::BodyStream;
    use http::{HeaderMap, StatusCode};
    use serde_json::{Value, json};

    use super::*;
    use crate::error::EXCERPT_LIMIT;
    use crate::types::Span;

    /// One request the fake client saw.
    struct Seen {
        method: Method,
        uri: String,
        headers: HeaderMap,
        body: Bytes,
    }

    /// Records requests; answers each with the next scripted reply
    /// (`Ok((status, body))`, or `Err(detail)` for a transport failure).
    #[derive(Default)]
    struct FakeHttp {
        replies: Mutex<VecDeque<Result<(u16, String), String>>>,
        seen: Mutex<Vec<Seen>>,
    }

    impl FakeHttp {
        fn new(replies: Vec<Result<(u16, String), String>>) -> Arc<Self> {
            Arc::new(Self {
                replies: Mutex::new(replies.into()),
                seen: Mutex::default(),
            })
        }

        fn uris(&self) -> Vec<String> {
            self.seen
                .lock()
                .unwrap()
                .iter()
                .map(|s| s.uri.clone())
                .collect()
        }
    }

    #[async_trait::async_trait]
    impl HttpClient for FakeHttp {
        async fn send(
            &self,
            req: http::Request<Bytes>,
        ) -> Result<http::Response<BodyStream>, ModelError> {
            let (parts, body) = req.into_parts();
            self.seen.lock().unwrap().push(Seen {
                method: parts.method,
                uri: parts.uri.to_string(),
                headers: parts.headers,
                body,
            });
            let reply = self
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unscripted request");
            match reply {
                Ok((status, text)) => {
                    let stream: BodyStream =
                        futures::stream::once(async move { Ok(Bytes::from(text)) }).boxed();
                    let mut resp = http::Response::new(stream);
                    *resp.status_mut() = StatusCode::from_u16(status).unwrap();
                    Ok(resp)
                }
                Err(detail) => Err(ModelError::Transport(detail.into())),
            }
        }
    }

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

    struct FailingAuth;

    #[async_trait::async_trait]
    impl AuthHeaders for FailingAuth {
        async fn headers(&self) -> Result<HeaderMap, String> {
            Err("no ADC".into())
        }
    }

    const ID: &str = "0123456789abcdef0123456789abcdef";
    /// The URL `gadk-console` sends for its default query (last hour, `ROOTSPAN`, 50 per page).
    const LIST_PREFIX: &str = "https://trace.test/v1/projects/proj/traces?view=ROOTSPAN\
        &orderBy=start%20desc&pageSize=50\
        &startTime=2026-11-16T11%3A00%3A00Z&endTime=2026-11-16T12%3A00%3A00Z";

    fn client(http: Arc<FakeHttp>) -> CloudTraceClient {
        CloudTraceClient::with_parts("proj", http, Arc::new(FixedAuth), "https://trace.test")
    }

    fn ok(body: Value) -> Result<(u16, String), String> {
        Ok((200, body.to_string()))
    }

    fn end() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 16, 12, 0, 0).unwrap()
    }

    fn time(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    /// The console's list request: the hour before `end()`, `ROOTSPAN`, newest first, 50 a page.
    fn console_list(filters: Vec<Filter>) -> ListRequest {
        ListRequest {
            start: end() - chrono::TimeDelta::hours(1),
            end: end(),
            view: View::RootSpan,
            filters,
            start_descending: true,
            page_size: 50,
            page_token: None,
        }
    }

    fn label(key: &str, value: &str) -> Filter {
        Filter::LabelExact(key.into(), value.into())
    }

    #[test]
    fn debug_shows_the_project_and_no_credentials() {
        let out = format!("{:?}", client(Arc::new(FakeHttp::default())));
        assert_eq!(
            out,
            r#"CloudTraceClient { project: "proj", base: "https://trace.test", .. }"#
        );
        assert!(!out.contains("t0k") && !out.contains("Bearer"), "{out}");
    }

    // ---- list URLs (the cases gadk-console's own tests use) ----

    #[tokio::test]
    async fn list_sends_the_console_default_url() {
        let http = FakeHttp::new(vec![ok(json!({}))]);
        let page = client(http.clone())
            .list(&console_list(Vec::new()))
            .await
            .unwrap();
        assert_eq!(page, TracePage::default());
        assert_eq!(http.uris(), vec![LIST_PREFIX.to_owned()]);
        let seen = http.seen.lock().unwrap();
        assert_eq!(seen[0].method, Method::GET);
        assert!(seen[0].body.is_empty());
    }

    #[tokio::test]
    async fn list_filter_terms_are_quoted_and_escaped() {
        let cases = [
            (
                vec![Filter::Root("chat turn".into())],
                "root%3A%22chat%20turn%22",
            ),
            (
                vec![label("gen_ai.agent.name", r#"my "bot""#)],
                "%2Bgen_ai.agent.name%3A%22my%20%5C%22bot%5C%22%22",
            ),
            (
                vec![label("gen_ai.conversation.id", r"s\1")],
                "%2Bgen_ai.conversation.id%3A%22s%5C%5C1%22",
            ),
            (
                vec![
                    Filter::Root("a".into()),
                    label("gen_ai.agent.name", "b"),
                    label("gen_ai.conversation.id", "c"),
                ],
                "root%3A%22a%22%20%2Bgen_ai.agent.name%3A%22b%22\
                 %20%2Bgen_ai.conversation.id%3A%22c%22",
            ),
        ];
        for (filters, filter) in cases {
            let http = FakeHttp::new(vec![ok(json!({}))]);
            client(http.clone())
                .list(&console_list(filters))
                .await
                .unwrap();
            assert_eq!(http.uris(), vec![format!("{LIST_PREFIX}&filter={filter}")]);
        }
    }

    #[tokio::test]
    async fn list_skips_empty_filters_and_applies_the_window() {
        let http = FakeHttp::new(vec![ok(json!({}))]);
        let req = ListRequest {
            start: end() - chrono::TimeDelta::days(7),
            ..console_list(vec![
                Filter::Root(String::new()),
                label("gen_ai.agent.name", ""),
            ])
        };
        client(http.clone()).list(&req).await.unwrap();
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
        let http = FakeHttp::new(vec![ok(json!({})), ok(json!({}))]);
        let c = client(http.clone());
        let req = ListRequest {
            page_token: Some("tok/1=".into()),
            ..console_list(vec![label("gen_ai.agent.name", "b")])
        };
        c.list(&req).await.unwrap();
        let empty = ListRequest {
            page_token: Some(String::new()),
            ..console_list(Vec::new())
        };
        c.list(&empty).await.unwrap();
        assert_eq!(
            http.uris(),
            vec![
                format!("{LIST_PREFIX}&filter=%2Bgen_ai.agent.name%3A%22b%22&pageToken=tok%2F1%3D"),
                LIST_PREFIX.to_owned(),
            ]
        );
    }

    #[test]
    fn list_url_with_a_span_filter_complete_view_and_no_order() {
        let req = ListRequest {
            view: View::Complete,
            start_descending: false,
            page_size: 100,
            ..console_list(vec![
                Filter::Span("invoke_agent".into()),
                label("gen_ai.agent.name", "support"),
            ])
        };
        assert_eq!(
            client(FakeHttp::new(Vec::new())).list_url(&req),
            "https://trace.test/v1/projects/proj/traces?view=COMPLETE&pageSize=100\
             &startTime=2026-11-16T11%3A00%3A00Z&endTime=2026-11-16T12%3A00%3A00Z\
             &filter=span%3A%22invoke_agent%22%20%2Bgen_ai.agent.name%3A%22support%22"
        );
    }

    #[tokio::test]
    async fn list_maps_the_next_page_token() {
        let http = FakeHttp::new(vec![
            ok(json!({"traces": [], "nextPageToken": "abc"})),
            ok(json!({"nextPageToken": ""})),
        ]);
        let c = client(http);
        let req = console_list(Vec::new());
        assert_eq!(
            c.list(&req).await.unwrap().next_page_token.as_deref(),
            Some("abc")
        );
        assert_eq!(c.list(&req).await.unwrap().next_page_token, None);
    }

    // ---- decoding ----

    #[tokio::test]
    async fn a_root_span_list_keeps_unparsable_span_ids() {
        let http = FakeHttp::new(vec![ok(json!({"traces": [
            {
                "projectId": "proj",
                "traceId": ID,
                "spans": [{
                    "spanId": "12345",
                    "kind": "RPC_SERVER",
                    "name": "invoke_agent weather",
                    "startTime": "2026-11-16T11:30:00Z",
                    "endTime": "2026-11-16T11:30:02Z",
                    "labels": {"gen_ai.agent.name": "weather"}
                }]
            },
            {
                "projectId": "proj",
                "traceId": "xyz",
                "spans": [{
                    "spanId": "0x1f",
                    "name": "odd",
                    "parentSpanId": "not-a-number",
                    "startTime": "2026-11-16T11:10:00Z",
                    "endTime": "2026-11-16T11:10:00Z"
                }]
            },
            {"projectId": "proj", "traceId": "00000000000000000000000000000001"}
        ]}))]);
        let page = client(http).list(&console_list(Vec::new())).await.unwrap();
        assert_eq!(
            page.traces,
            vec![
                Trace {
                    trace_id: ID.into(),
                    spans: vec![Span {
                        span_id: "0000000000003039".into(),
                        parent_id: None,
                        name: "invoke_agent weather".into(),
                        start: time("2026-11-16T11:30:00Z"),
                        end: time("2026-11-16T11:30:02Z"),
                        labels: BTreeMap::from([(
                            "gen_ai.agent.name".to_owned(),
                            "weather".to_owned()
                        )]),
                    }],
                },
                Trace {
                    trace_id: "xyz".into(),
                    spans: vec![Span {
                        span_id: "0x1f".into(),
                        parent_id: None,
                        name: "odd".into(),
                        start: time("2026-11-16T11:10:00Z"),
                        end: time("2026-11-16T11:10:00Z"),
                        labels: BTreeMap::new(),
                    }],
                },
                Trace {
                    trace_id: "00000000000000000000000000000001".into(),
                    spans: Vec::new(),
                },
            ]
        );
    }

    #[tokio::test]
    async fn a_complete_list_rejects_an_unparsable_span_id() {
        let http = FakeHttp::new(vec![ok(json!({"traces": [{"traceId": ID, "spans": [{
            "spanId": "0x1f", "name": "s",
            "startTime": "2026-11-16T11:30:00Z", "endTime": "2026-11-16T11:30:00Z"
        }]}]}))]);
        let req = ListRequest {
            view: View::Complete,
            ..console_list(Vec::new())
        };
        let err = client(http).list(&req).await.unwrap_err();
        assert!(
            matches!(&err, TraceError::Decode(d) if d.contains("0x1f")),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn get_converts_decimal_ids_and_drops_zero_or_bad_parents() {
        let http = FakeHttp::new(vec![ok(json!({
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
                    "name": "generate_content",
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
                },
                {
                    "spanId": "2",
                    "name": "bad parent",
                    "startTime": "2026-11-16T11:30:00Z",
                    "endTime": "2026-11-16T11:30:00Z",
                    "parentSpanId": "not-a-number"
                }
            ]
        }))]);
        let trace = client(http.clone()).get(ID).await.unwrap();
        let at = time("2026-11-16T11:30:00Z");
        assert_eq!(
            trace,
            Trace {
                trace_id: ID.into(),
                spans: vec![
                    Span {
                        span_id: "0000000000003039".into(),
                        parent_id: None,
                        name: "invoke_agent weather".into(),
                        start: at,
                        end: time("2026-11-16T11:30:02Z"),
                        labels: BTreeMap::from([(
                            "gen_ai.agent.name".to_owned(),
                            "weather".to_owned()
                        )]),
                    },
                    Span {
                        span_id: "ffffffffffffffff".into(),
                        parent_id: Some("0000000000003039".into()),
                        name: "generate_content".into(),
                        start: time("2026-11-16T11:30:00.500Z"),
                        end: time("2026-11-16T11:30:01Z"),
                        labels: BTreeMap::new(),
                    },
                    Span {
                        span_id: "0000000000000001".into(),
                        parent_id: None,
                        name: "zero parent".into(),
                        start: at,
                        end: at,
                        labels: BTreeMap::new(),
                    },
                    Span {
                        span_id: "0000000000000002".into(),
                        parent_id: None,
                        name: "bad parent".into(),
                        start: at,
                        end: at,
                        labels: BTreeMap::new(),
                    },
                ],
            }
        );
        assert_eq!(
            http.uris(),
            vec![format!("https://trace.test/v1/projects/proj/traces/{ID}")]
        );
    }

    #[tokio::test]
    async fn get_rejects_an_unparsable_span_id() {
        let http = FakeHttp::new(vec![ok(json!({"traceId": ID, "spans": [{
            "spanId": "0x1f", "name": "s",
            "startTime": "2026-11-16T11:30:00Z", "endTime": "2026-11-16T11:30:00Z"
        }]}))]);
        let err = client(http).get(ID).await.unwrap_err();
        assert!(
            matches!(&err, TraceError::Decode(d) if d.contains("0x1f")),
            "{err:?}"
        );
    }

    // ---- errors and headers ----

    #[tokio::test]
    async fn a_404_is_not_found_on_get_but_http_on_list() {
        let body = r#"{"error":{"code":404}}"#;
        let http = FakeHttp::new(vec![Ok((404, body.into())), Ok((404, body.into()))]);
        let c = client(http.clone());
        assert_eq!(c.get(ID).await, Err(TraceError::NotFound));
        assert_eq!(
            c.list(&console_list(Vec::new())).await,
            Err(TraceError::Http {
                status: 404,
                reason: None,
                excerpt: body.into(),
            })
        );
        assert_eq!(http.uris().len(), 2);
    }

    #[tokio::test]
    async fn the_reason_comes_from_error_info() {
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
        let http = FakeHttp::new(vec![
            Ok((403, body.clone())),
            Ok((403, r#"{"error":{"details":[]}}"#.into())),
        ]);
        let c = client(http);
        let req = console_list(Vec::new());
        assert_eq!(
            c.list(&req).await,
            Err(TraceError::Http {
                status: 403,
                reason: Some("SERVICE_DISABLED".into()),
                excerpt: body,
            })
        );
        assert!(matches!(
            c.list(&req).await,
            Err(TraceError::Http {
                status: 403,
                reason: None,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn the_excerpt_is_capped() {
        let http = FakeHttp::new(vec![Ok((500, "x".repeat(5000)))]);
        let err = client(http)
            .list(&console_list(Vec::new()))
            .await
            .unwrap_err();
        let TraceError::Http {
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
        let err = client(http)
            .list(&console_list(Vec::new()))
            .await
            .unwrap_err();
        assert!(
            matches!(&err, TraceError::Transport(d) if d.contains("connection refused")),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn bad_json_is_decode() {
        let http = FakeHttp::new(vec![Ok((200, "<html>".into()))]);
        let err = client(http)
            .list(&console_list(Vec::new()))
            .await
            .unwrap_err();
        assert!(matches!(err, TraceError::Decode(_)), "{err:?}");
    }

    #[tokio::test]
    async fn a_credentials_failure_sends_nothing() {
        let http = FakeHttp::new(Vec::new());
        let c = CloudTraceClient::with_parts(
            "proj",
            http.clone(),
            Arc::new(FailingAuth),
            "https://trace.test",
        );
        assert_eq!(
            c.get(ID).await,
            Err(TraceError::Credentials("no ADC".into()))
        );
        assert!(http.uris().is_empty());
    }

    #[tokio::test]
    async fn an_invalid_project_header_is_transport() {
        let http = FakeHttp::new(Vec::new());
        let c = CloudTraceClient::with_parts(
            "bad\nproj",
            http.clone(),
            Arc::new(FixedAuth),
            "https://trace.test",
        );
        let err = c.get(ID).await.unwrap_err();
        assert!(
            matches!(&err, TraceError::Transport(d) if d.contains("project id")),
            "{err:?}"
        );
        assert!(http.uris().is_empty());
    }

    #[tokio::test]
    async fn every_request_carries_auth_and_one_quota_project_header() {
        let http = FakeHttp::new(vec![ok(json!({})), ok(json!({"traceId": ID}))]);
        let c = client(http.clone());
        assert_eq!(c.project(), "proj");
        c.list(&console_list(Vec::new())).await.unwrap();
        c.get(ID).await.unwrap();
        let seen = http.seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        for s in seen.iter() {
            let values: Vec<_> = s.headers.get_all("x-goog-user-project").iter().collect();
            assert_eq!(values, vec!["proj"], "{}", s.uri);
            assert_eq!(s.headers["authorization"], "Bearer t0k");
        }
    }
}
