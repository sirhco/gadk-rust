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

//! The Cloud Trace transcript source: lists runs from the spans an agent exports to Cloud Trace.
//!
//! Each [`TranscriptSource::list`] call pages through `traces.list` (`view=COMPLETE`, newest
//! first, [`PAGE_SIZE`] traces a page, filter `span:"invoke_agent"` plus
//! `+gen_ai.agent.name:"<agent>"` when the query names an agent) and rebuilds each trace's runs
//! with [`build_runs`]. Runs whose root span started outside the window, or whose root agent is
//! not the query's agent, are dropped. Paging stops once `limit` runs are found, at the last
//! page, or after [`MAX_PAGES`] pages; a run listed twice (its trace on two pages, when traces
//! arrive while paging) is kept once. The runs are returned newest first, at most `limit`. The
//! agent must record message content on its spans
//! (`OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT=SPAN_ONLY` or `SPAN_AND_EVENT`).

use std::collections::HashSet;
use std::sync::Arc;

use gadk_cloudtrace::{Adc, CloudTraceClient, Filter, ListRequest, View};

use crate::error::MonitorError;
use crate::source::trace_mapping::build_runs;
use crate::source::{ProductionRun, TranscriptQuery, TranscriptSource};
use crate::spec::is_project_id;

/// Traces per `traces.list` page.
pub const PAGE_SIZE: u32 = 100;

/// The most pages one listing reads (not an error when reached).
pub const MAX_PAGES: usize = 50;

/// The span-name prefix every listed trace must have a span with.
const RUN_SPAN: &str = "invoke_agent";

const AGENT_LABEL: &str = "gen_ai.agent.name";

/// Reads production runs from the spans in one project's Cloud Trace.
pub struct CloudTraceTranscriptSource {
    client: CloudTraceClient,
}

impl CloudTraceTranscriptSource {
    /// The traces of `project`, read with Application Default Credentials. Must be called inside
    /// a Tokio runtime.
    ///
    /// # Errors
    ///
    /// [`MonitorError::Source`] when `project` is not `[A-Za-z0-9_-]+` (checked first) or when
    /// no credentials can be built; some credential failures only show on the first request
    /// (as a source error from [`TranscriptSource::list`]).
    pub fn new(project: &str) -> Result<Self, MonitorError> {
        check_project(project)?;
        let auth = Adc::resolve()
            .map_err(|e| MonitorError::Source(format!("Application Default Credentials: {e}")))?;
        Self::with_client(CloudTraceClient::with_auth(project, Arc::new(auth)))
    }

    /// Like [`CloudTraceTranscriptSource::new`], with an explicit client; the traces read are
    /// those of the client's project.
    ///
    /// # Errors
    ///
    /// [`MonitorError::Source`] when the client's project is not `[A-Za-z0-9_-]+`.
    pub fn with_client(client: CloudTraceClient) -> Result<Self, MonitorError> {
        check_project(client.project())?;
        Ok(Self { client })
    }

    /// The project whose traces are read.
    pub fn project(&self) -> &str {
        self.client.project()
    }
}

fn check_project(project: &str) -> Result<(), MonitorError> {
    if is_project_id(project) {
        Ok(())
    } else {
        Err(MonitorError::Source(format!(
            "invalid project {project:?} (expected [A-Za-z0-9_-]+)"
        )))
    }
}

/// The `traces.list` request for one page of `q`.
fn request(q: &TranscriptQuery, page_token: Option<String>) -> ListRequest {
    let mut filters = vec![Filter::Span(RUN_SPAN.to_owned())];
    if let Some(agent) = &q.agent {
        filters.push(Filter::LabelExact(AGENT_LABEL.to_owned(), agent.clone()));
    }
    let mut req = ListRequest::new(q.from, q.to);
    req.view = View::Complete;
    req.filters = filters;
    req.start_descending = true;
    req.page_size = PAGE_SIZE;
    req.page_token = page_token;
    req
}

#[async_trait::async_trait]
impl TranscriptSource for CloudTraceTranscriptSource {
    async fn list(&self, q: &TranscriptQuery) -> Result<Vec<ProductionRun>, MonitorError> {
        if q.limit == 0 {
            return Ok(Vec::new());
        }
        let mut runs: Vec<ProductionRun> = Vec::new();
        // (trace id, invocation id) of the runs kept: new traces arriving while paging newest
        // first push older ones onto the next page, so a trace can be listed twice.
        let mut seen: HashSet<(Option<String>, String)> = HashSet::new();
        let mut token: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let page = self
                .client
                .list(&request(q, token.take()))
                .await
                .map_err(|e| MonitorError::Source(e.to_string()))?;
            for trace in &page.traces {
                runs.extend(build_runs(trace).into_iter().filter(|r| {
                    r.started_at >= q.from
                        && r.started_at < q.to
                        && q.agent.as_ref().is_none_or(|a| &r.agent == a)
                        && seen.insert((r.trace_id.clone(), r.invocation_id.clone()))
                }));
            }
            token = page.next_page_token;
            if runs.len() >= q.limit || token.is_none() {
                break;
            }
        }
        runs.sort_by(|a, b| {
            b.started_at
                .cmp(&a.started_at)
                .then_with(|| a.invocation_id.cmp(&b.invocation_id))
        });
        runs.truncate(q.limit);
        Ok(runs)
    }
}

#[cfg(test)]
mod tests {
    use chrono::SecondsFormat;
    use gadk_cloudtrace::NoAuth;
    use gadk_model::HttpClient;
    use serde_json::{Value, json};

    use super::*;
    use crate::testutil::{Exchange, at, exchange, replay};

    /// 2025-11-17T00:00:00Z.
    const FROM: i64 = 1_763_337_600;
    /// 2025-11-18T00:00:00Z.
    const TO: i64 = 1_763_424_000;
    const LIST: &str = "https://cloudtrace.googleapis.com/v1/projects/proj/traces?view=COMPLETE\
        &orderBy=start%20desc&pageSize=100\
        &startTime=2025-11-17T00%3A00%3A00Z&endTime=2025-11-18T00%3A00%3A00Z\
        &filter=span%3A%22invoke_agent%22";
    const LIST_SUPPORT: &str = "https://cloudtrace.googleapis.com/v1/projects/proj/traces\
        ?view=COMPLETE&orderBy=start%20desc&pageSize=100\
        &startTime=2025-11-17T00%3A00%3A00Z&endTime=2025-11-18T00%3A00%3A00Z\
        &filter=span%3A%22invoke_agent%22%20%2Bgen_ai.agent.name%3A%22support%22";

    fn client(project: &str, http: Arc<dyn HttpClient>) -> CloudTraceClient {
        CloudTraceClient::with_parts(project, http, Arc::new(NoAuth), gadk_cloudtrace::ENDPOINT)
    }

    fn source(http: Arc<dyn HttpClient>) -> CloudTraceTranscriptSource {
        CloudTraceTranscriptSource::with_client(client("proj", http)).unwrap()
    }

    fn query(agent: Option<&str>, limit: usize) -> TranscriptQuery {
        TranscriptQuery {
            from: at(FROM),
            to: at(TO),
            agent: agent.map(str::to_owned),
            limit,
        }
    }

    fn time(secs: i64) -> String {
        at(secs).to_rfc3339_opts(SecondsFormat::Secs, true)
    }

    /// The v1 spans of one complete run: `invoke_agent` span `n0` (decimal id `n*10`) and its
    /// `generate_content` child (`n*10+1`) asking "hi" and answering "hello".
    fn v1_run(n: u64, invocation: &str, agent: &str, secs: i64) -> Vec<Value> {
        let input = json!([{"role": "user", "parts": [{"type": "text", "content": "hi"}]}]);
        let output = json!([{"role": "assistant", "parts": [{"type": "text", "content": "hello"}],
                             "finish_reason": "stop"}]);
        vec![
            json!({
                "spanId": (n * 10).to_string(),
                "kind": "RPC_SERVER",
                "name": format!("invoke_agent {agent}"),
                "startTime": time(secs),
                "endTime": time(secs + 2),
                "labels": {
                    "gen_ai.operation.name": "invoke_agent",
                    "gen_ai.agent.name": agent,
                    "gen_ai.conversation.id": "s-1",
                    "gcp.vertex.agent.invocation_id": invocation
                }
            }),
            json!({
                "spanId": (n * 10 + 1).to_string(),
                "parentSpanId": (n * 10).to_string(),
                "name": "generate_content gemini-2.5-flash",
                "startTime": time(secs),
                "endTime": time(secs + 1),
                "labels": {
                    "gen_ai.operation.name": "generate_content",
                    "gen_ai.input.messages": input.to_string(),
                    "gen_ai.output.messages": output.to_string()
                }
            }),
        ]
    }

    fn trace(n: u64, spans: Vec<Value>) -> Value {
        json!({"projectId": "proj", "traceId": format!("{n:032x}"), "spans": spans})
    }

    /// A `traces.list` exchange for `url` answered with `traces` and `next`.
    fn page(url: &str, traces: Vec<Value>, next: Option<&str>) -> Exchange {
        let mut body = json!({ "traces": traces });
        if let Some(token) = next {
            body["nextPageToken"] = json!(token);
        }
        exchange("GET", url, "", 200, &body.to_string())
    }

    fn ids(runs: &[ProductionRun]) -> Vec<&str> {
        runs.iter().map(|r| r.invocation_id.as_str()).collect()
    }

    #[test]
    fn the_project_is_validated() {
        let http: Arc<dyn HttpClient> = Arc::new(gadk_model::ReqwestClient::default());
        for bad in ["", "p.q", "a b"] {
            let err = CloudTraceTranscriptSource::with_client(client(bad, http.clone()))
                .err()
                .unwrap();
            assert!(
                matches!(&err, MonitorError::Source(m) if m.contains(&format!("{bad:?}"))),
                "{err}"
            );
        }
        let ok = CloudTraceTranscriptSource::with_client(client("my-proj_1", http)).unwrap();
        assert_eq!(ok.project(), "my-proj_1");
    }

    #[tokio::test]
    async fn lists_runs_newest_first_across_pages() {
        let dir = tempfile::tempdir().unwrap();
        let mut first = v1_run(1, "inv-a", "support", FROM + 100);
        first.extend(v1_run(2, "inv-b", "support", FROM + 300));
        let http = replay(
            dir.path(),
            &[
                page(LIST, vec![trace(1, first)], Some("p2")),
                page(
                    &format!("{LIST}&pageToken=p2"),
                    vec![trace(2, v1_run(3, "inv-c", "support", FROM + 200))],
                    None,
                ),
            ],
        );
        let runs = source(http).list(&query(None, 10)).await.unwrap();
        assert_eq!(ids(&runs), ["inv-b", "inv-c", "inv-a"]);
        let run = &runs[0];
        assert_eq!(
            run.trace_id.as_deref(),
            Some("00000000000000000000000000000001")
        );
        assert_eq!(
            (run.agent.as_str(), run.session_id.as_str(), run.started_at),
            ("support", "s-1", at(FROM + 300))
        );
        let turn = &run.transcript.turns[0];
        assert_eq!((turn.user.as_str(), turn.reply.as_str()), ("hi", "hello"));
        assert!(
            run.content_complete && run.tools_complete,
            "{:?}",
            run.notes
        );
    }

    #[tokio::test]
    async fn a_trace_listed_on_two_pages_gives_one_run() {
        // A newer trace arriving between the requests pushes trace 2 onto the second page too.
        let dir = tempfile::tempdir().unwrap();
        let http = replay(
            dir.path(),
            &[
                page(
                    LIST,
                    vec![
                        trace(1, v1_run(1, "inv-a", "support", FROM + 300)),
                        trace(2, v1_run(2, "inv-b", "support", FROM + 200)),
                    ],
                    Some("p2"),
                ),
                page(
                    &format!("{LIST}&pageToken=p2"),
                    vec![
                        trace(2, v1_run(2, "inv-b", "support", FROM + 200)),
                        trace(3, v1_run(3, "inv-c", "support", FROM + 100)),
                    ],
                    None,
                ),
            ],
        );
        let runs = source(http).list(&query(None, 10)).await.unwrap();
        assert_eq!(ids(&runs), ["inv-a", "inv-b", "inv-c"]);
    }

    #[tokio::test]
    async fn paging_stops_once_limit_runs_are_found() {
        // Replay fails any request it has no exchange for, so a further page would fail the test.
        let dir = tempfile::tempdir().unwrap();
        let http = replay(
            dir.path(),
            &[
                page(
                    LIST,
                    vec![
                        trace(1, v1_run(1, "inv-a", "support", FROM + 100)),
                        trace(2, v1_run(2, "inv-b", "support", FROM + 200)),
                    ],
                    Some("p2"),
                ),
                page(
                    &format!("{LIST}&pageToken=p2"),
                    vec![trace(3, v1_run(3, "inv-c", "support", FROM + 300))],
                    Some("p3"),
                ),
            ],
        );
        let runs = source(http).list(&query(None, 3)).await.unwrap();
        assert_eq!(ids(&runs), ["inv-c", "inv-b", "inv-a"]);

        let dir = tempfile::tempdir().unwrap();
        let http = replay(
            dir.path(),
            &[page(
                LIST,
                vec![
                    trace(1, v1_run(1, "inv-a", "support", FROM + 100)),
                    trace(2, v1_run(2, "inv-b", "support", FROM + 200)),
                    trace(3, v1_run(3, "inv-c", "support", FROM + 300)),
                ],
                Some("p2"),
            )],
        );
        let runs = source(http).list(&query(None, 2)).await.unwrap();
        assert_eq!(ids(&runs), ["inv-c", "inv-b"]);
    }

    #[tokio::test]
    async fn runs_outside_the_window_or_of_another_agent_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let mut spans = v1_run(1, "inv-early", "support", FROM - 1);
        spans.extend(v1_run(2, "inv-start", "support", FROM));
        spans.extend(v1_run(3, "inv-end", "support", TO));
        spans.extend(v1_run(4, "inv-other", "billing", FROM + 10));
        let http = replay(
            dir.path(),
            &[page(LIST_SUPPORT, vec![trace(1, spans)], None)],
        );
        let runs = source(http)
            .list(&query(Some("support"), 10))
            .await
            .unwrap();
        assert_eq!(ids(&runs), ["inv-start"]);
    }

    #[tokio::test]
    async fn paging_stops_after_max_pages() {
        let dir = tempfile::tempdir().unwrap();
        let mut exchanges = vec![page(LIST, Vec::new(), Some("t1"))];
        for i in 1..MAX_PAGES {
            let next = format!("t{}", i + 1);
            exchanges.push(page(
                &format!("{LIST}&pageToken=t{i}"),
                Vec::new(),
                Some(&next),
            ));
        }
        assert_eq!(exchanges.len(), 50);
        // A 51st request (pageToken=t50) has no exchange and would fail the listing.
        let http = replay(dir.path(), &exchanges);
        let runs = source(http).list(&query(None, 10)).await.unwrap();
        assert!(runs.is_empty());
    }

    #[tokio::test]
    async fn a_trace_error_is_a_source_error() {
        let body = r#"{"error":{"code":403,"message":"denied","status":"PERMISSION_DENIED"}}"#;
        let dir = tempfile::tempdir().unwrap();
        let http = replay(dir.path(), &[exchange("GET", LIST, "", 403, body)]);
        let err = source(http).list(&query(None, 10)).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            format!("transcript source: Cloud Trace: HTTP 403: {body}")
        );

        let dir = tempfile::tempdir().unwrap();
        let http = replay(dir.path(), &[exchange("GET", LIST, "", 200, "<html>")]);
        let err = source(http).list(&query(None, 10)).await.unwrap_err();
        assert!(
            err.to_string()
                .starts_with("transcript source: Cloud Trace: unexpected response: "),
            "{err}"
        );
    }

    #[tokio::test]
    async fn a_zero_limit_sends_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let http = replay(dir.path(), &[]);
        assert!(source(http).list(&query(None, 0)).await.unwrap().is_empty());
    }
}
