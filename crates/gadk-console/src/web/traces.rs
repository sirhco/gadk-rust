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

//! Trace routes: `/traces` and `/traces/{id}`.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::Response;
use axum::routing::get;
use chrono::TimeDelta;
use http::StatusCode;
use serde::Deserialize;

use super::AppState;
use super::pages::error::source_error;
use super::pages::layout::html;
use super::pages::trace::{LogView, ScoresView};
use super::pages::{trace, traces};
use crate::model::{attach_logs, build_tree, trace_end};
use crate::traces::{RawSpan, SourceError, TimeRange, TraceId, TraceQuery};

/// How long after a trace's last span ends its log entries may still be arriving.
const STILL_ARRIVING: TimeDelta = TimeDelta::seconds(120);

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/traces", get(list))
        .route("/traces/{id}", get(detail))
}

/// `/traces` query parameters, as sent by the filter form and the next-page link.
#[derive(Debug, Default, Deserialize)]
struct ListParams {
    range: Option<String>,
    name: Option<String>,
    agent: Option<String>,
    session: Option<String>,
    page: Option<String>,
}

/// Empty strings become `None`.
fn non_empty(v: Option<String>) -> Option<String> {
    v.filter(|s| !s.is_empty())
}

impl ListParams {
    /// An unknown or missing `range` falls back to the default (1h).
    fn into_query(self) -> TraceQuery {
        TraceQuery {
            range: self
                .range
                .as_deref()
                .and_then(TimeRange::parse)
                .unwrap_or_default(),
            name: non_empty(self.name),
            agent: non_empty(self.agent),
            session: non_empty(self.session),
            page_token: non_empty(self.page),
        }
    }
}

async fn list(State(state): State<AppState>, Query(params): Query<ListParams>) -> Response {
    let query = params.into_query();
    match state.source.list(&query).await {
        Ok(page) => html(StatusCode::OK, traces::render(&query, &page)),
        Err(e) => source_error(&e),
    }
}

async fn detail(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    // A malformed id never reaches the source.
    let Some(id) = TraceId::parse(&id) else {
        return source_error(&SourceError::NotFound);
    };
    let detail = match state.source.get(&id).await {
        Ok(detail) => detail,
        Err(e) => return source_error(&e),
    };
    let rows = build_tree(&detail.spans);
    let still_arriving =
        trace_end(&detail.spans).is_some_and(|end| (state.now)() - end < STILL_ARRIVING);
    let scores = trace_scores(&state, &id, &detail.spans).await;
    let logs = detail.logs.map(|set| LogView {
        shown: set.entries.len(),
        attached: attach_logs(&detail.spans, set.entries),
        truncated: set.truncated,
    });
    html(
        StatusCode::OK,
        trace::render(
            &id,
            &rows,
            &logs,
            still_arriving,
            state.prices.as_deref(),
            &scores,
        ),
    )
}

/// The Scores section of trace `id`: with the BigQuery source, the scores of the trace and of
/// the invocations its spans name (`invocation_id` labels) since a day before its first span.
async fn trace_scores(state: &AppState, id: &TraceId, spans: &[RawSpan]) -> ScoresView {
    let Some(usage) = &state.usage else {
        return ScoresView::NeedsBigQuery;
    };
    let mut ids: Vec<String> = spans
        .iter()
        .filter_map(|s| s.labels.get("invocation_id").cloned())
        .collect();
    ids.sort();
    ids.dedup();
    let start = spans
        .iter()
        .map(|s| s.start)
        .min()
        .unwrap_or_else(state.now);
    match usage.trace_scores(id.as_str(), &ids, start).await {
        Ok(scores) => ScoresView::Loaded(scores),
        Err(e) => ScoresView::Failed(e.to_string(), e.hint()),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex};

    use chrono::{DateTime, TimeDelta, Utc};
    use http::StatusCode;
    use serde_json::{Value, json};

    use crate::prices::PriceTable;
    use crate::traces::BigQueryTable;
    use crate::traces::test_http::{FakeHttp, param_value, reply};
    use crate::traces::{
        LogEntry, LogSet, RawSpan, Service, SourceError, TimeRange, TraceDetail, TraceId,
        TracePage, TraceQuery, TraceSource, TraceSummary,
    };
    use crate::usage::UsageSource;
    use crate::web::AppState;
    use crate::web::testutil::{FakeSource, fixed_now, get, state};
    use gadk_bigquery::{BigQueryClient, NoAuth};

    const ID: &str = "0123456789abcdef0123456789abcdef";
    const JSON_OPEN: &str = r#"<script type="application/json" id="trace-data">"#;

    /// A source that records the query `list` was called with.
    struct Recording {
        seen: Mutex<Option<TraceQuery>>,
    }

    #[async_trait::async_trait]
    impl TraceSource for Recording {
        async fn list(&self, query: &TraceQuery) -> Result<TracePage, SourceError> {
            *self.seen.lock().unwrap() = Some(query.clone());
            Ok(TracePage::default())
        }

        async fn get(&self, _id: &TraceId) -> Result<TraceDetail, SourceError> {
            Err(SourceError::NotFound)
        }
    }

    async fn recorded(path: &str) -> TraceQuery {
        let rec = Arc::new(Recording {
            seen: Mutex::new(None),
        });
        let mut st: AppState = state(Arc::new(FakeSource::new()), None);
        st.source = rec.clone();
        let (status, _, body) = get(st, path).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        rec.seen.lock().unwrap().clone().expect("list was called")
    }

    fn with_list(page: Result<TracePage, SourceError>) -> AppState {
        let src = FakeSource::new();
        *src.list.lock().unwrap() = page;
        state(Arc::new(src), None)
    }

    fn with_get(detail: Result<TraceDetail, SourceError>) -> (Arc<FakeSource>, AppState) {
        let src = Arc::new(FakeSource::new());
        *src.get.lock().unwrap() = detail;
        (src.clone(), state(src, None))
    }

    fn summary(id: &str, name: &str) -> TraceSummary {
        TraceSummary {
            trace_id: TraceId::parse(id).unwrap(),
            root_name: name.into(),
            start: fixed_now() - TimeDelta::seconds(60),
            duration_ms: 1234.5,
            agent: Some("weather_bot".into()),
            session: Some("s-42".into()),
        }
    }

    fn span(id: &str, parent: Option<&str>, name: &str, start: DateTime<Utc>, ms: i64) -> RawSpan {
        RawSpan {
            span_id: id.into(),
            parent_id: parent.map(Into::into),
            name: name.into(),
            start,
            end: start + TimeDelta::milliseconds(ms),
            labels: BTreeMap::new(),
        }
    }

    const ROOT: &str = "000000000000000a";
    const PROMPT: &str = "000000000000000b";
    const LLM: &str = "000000000000000c";

    /// A 3 s trace that ended `ended_ago` seconds before `fixed_now`: the root (0–3 s), a
    /// `render_prompt greet` child (1–2 s) and a `call_llm` child (2–2.5 s).
    fn spans(ended_ago: i64) -> Vec<RawSpan> {
        let t0 = fixed_now() - TimeDelta::seconds(ended_ago + 3);
        let mut root = span(ROOT, None, "invoke_agent weather_bot", t0, 3000);
        root.labels
            .insert("gen_ai.agent.name".into(), "weather_bot".into());
        let mut prompt = span(
            PROMPT,
            Some(ROOT),
            "render_prompt greet",
            t0 + TimeDelta::seconds(1),
            1000,
        );
        prompt.labels.insert("prompt.name".into(), "greet".into());
        prompt.labels.insert("prompt.version".into(), "3".into());
        let llm = span(LLM, Some(ROOT), "call_llm", t0 + TimeDelta::seconds(2), 500);
        vec![root, prompt, llm]
    }

    fn entry(span_id: Option<&str>, body: &str) -> LogEntry {
        LogEntry {
            timestamp: fixed_now() - TimeDelta::seconds(700),
            span_id: span_id.map(Into::into),
            severity: Some("INFO".into()),
            payload: json!({
                "event.name": "gen_ai.user.message",
                "body": body,
                "attributes": {"gen_ai.system": "gemini"},
            }),
        }
    }

    fn detail(ended_ago: i64, logs: Result<LogSet, SourceError>) -> TraceDetail {
        TraceDetail {
            trace_id: TraceId::parse(ID).unwrap(),
            spans: spans(ended_ago),
            logs,
        }
    }

    fn logs(entries: Vec<LogEntry>, truncated: bool) -> Result<LogSet, SourceError> {
        Ok(LogSet { entries, truncated })
    }

    async fn trace_page(d: TraceDetail) -> (StatusCode, String) {
        let (_, st) = with_get(Ok(d));
        let (status, _, body) = get(st, &format!("/traces/{ID}")).await;
        (status, body)
    }

    /// The text between the JSON block's opening tag and the first `</script` after it.
    fn json_block(page: &str) -> &str {
        let start = page.find(JSON_OPEN).expect("JSON block") + JSON_OPEN.len();
        let len = page[start..].find("</script").expect("closing tag");
        &page[start..start + len]
    }

    /// The `<section id="<id>">…</section>` of a page.
    fn section<'a>(page: &'a str, id: &str) -> &'a str {
        let start = page
            .find(&format!(r#"<section class="span-logs" id="{id}">"#))
            .unwrap_or_else(|| panic!("section {id}: {page}"));
        let len = page[start..].find("</section>").expect("closing tag");
        &page[start..start + len]
    }

    // ---- /traces ----

    #[tokio::test]
    async fn list_renders_rows_that_link_to_the_trace() {
        let other = "fedcba9876543210fedcba9876543210";
        let st = with_list(Ok(TracePage {
            traces: vec![
                summary(ID, "invoke_agent weather_bot"),
                summary(other, "run"),
            ],
            next: None,
        }));
        let (status, _, body) = get(st, "/traces").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            body.contains(&format!(
                r#"<a href="/traces/{ID}">invoke_agent weather_bot</a>"#
            )),
            "{body}"
        );
        assert!(
            body.contains(&format!(r#"<a href="/traces/{other}">run</a>"#)),
            "{body}"
        );
        assert!(body.contains("2026-11-16T11:59:00Z"), "{body}");
        assert!(body.contains("1234.5 ms"), "{body}");
        assert!(
            body.contains("weather_bot") && body.contains("s-42"),
            "{body}"
        );
        assert!(
            body.contains(r#"<form class="filters" method="get" action="/traces">"#),
            "{body}"
        );
        assert!(!body.contains("Next page"), "{body}");
        assert!(!body.contains("No traces in this range"), "{body}");
    }

    #[tokio::test]
    async fn list_passes_the_filters_to_the_source() {
        let q =
            recorded("/traces?range=6h&name=invoke%20agent&agent=bot&session=s-1&page=tok").await;
        assert_eq!(
            q,
            TraceQuery {
                range: TimeRange::Hours6,
                name: Some("invoke agent".into()),
                agent: Some("bot".into()),
                session: Some("s-1".into()),
                page_token: Some("tok".into()),
            }
        );
    }

    #[tokio::test]
    async fn list_defaults_unknown_ranges_and_drops_empty_values() {
        let q = recorded("/traces?range=2y&name=&agent=&session=&page=").await;
        assert_eq!(q, TraceQuery::default());
        assert_eq!(recorded("/traces").await, TraceQuery::default());
        let q = recorded("/traces?range=7d").await;
        assert_eq!(q.range, TimeRange::Days7);
    }

    #[tokio::test]
    async fn list_form_keeps_the_current_filters() {
        let (_, _, body) = get(
            with_list(Ok(TracePage::default())),
            "/traces?range=24h&agent=a%22b",
        )
        .await;
        assert!(body.contains(r#"<option value="24h" selected>"#), "{body}");
        assert!(body.contains(r#"<option value="1h">"#), "{body}");
        assert!(body.contains(r#"name="agent" value="a&quot;b""#), "{body}");
    }

    #[tokio::test]
    async fn next_link_keeps_the_filters_and_only_appears_with_a_token() {
        let st = with_list(Ok(TracePage {
            traces: vec![summary(ID, "run")],
            next: Some("tok/2 +".into()),
        }));
        let (_, _, body) = get(st, "/traces?range=6h&agent=my%20bot&page=old").await;
        assert!(
            body.contains(r#"<a href="/traces?range=6h&amp;agent=my%20bot&amp;page=tok%2F2%20%2B">Next page</a>"#),
            "{body}"
        );

        let st = with_list(Ok(TracePage {
            traces: vec![summary(ID, "run")],
            next: None,
        }));
        let (_, _, body) = get(st, "/traces?range=6h&page=old").await;
        assert!(!body.contains("Next page"), "{body}");
    }

    #[tokio::test]
    async fn empty_list_says_so() {
        let (status, _, body) = get(with_list(Ok(TracePage::default())), "/traces").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            body.contains(r#"<p class="empty">No traces in this range</p>"#),
            "{body}"
        );
        assert!(!body.contains("<table"), "{body}");
    }

    #[tokio::test]
    async fn list_failure_is_a_502_with_the_hint() {
        let st = with_list(Err(SourceError::Upstream {
            service: Service::Trace,
            status: 403,
            excerpt: "denied".into(),
            reason: None,
        }));
        let (status, _, body) = get(st, "/traces").await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(body.contains("Cloud Trace: HTTP 403: denied"), "{body}");
        assert!(body.contains("needs roles/cloudtrace.user"), "{body}");
    }

    // ---- /traces/{id} ----

    #[tokio::test]
    async fn rows_carry_offsets_and_widths_in_tree_order() {
        let (status, body) = trace_page(detail(600, logs(vec![], false))).await;
        assert_eq!(status, StatusCode::OK);
        let root = format!(
            r#"<div class="span-row" data-span="{ROOT}" data-depth="0" data-offset="0.00" data-width="100.00">"#
        );
        let prompt = format!(
            r#"<div class="span-row" data-span="{PROMPT}" data-depth="1" data-offset="33.33" data-width="33.33">"#
        );
        let llm = format!(
            r#"<div class="span-row" data-span="{LLM}" data-depth="1" data-offset="66.67" data-width="16.67">"#
        );
        let (a, b, c) = (
            body.find(&root).expect(&root),
            body.find(&prompt).expect(&prompt),
            body.find(&llm).expect(&llm),
        );
        assert!(a < b && b < c, "tree order");
        assert!(body.contains(r#"<div class="bar"></div>"#), "{body}");
        assert!(!body.contains("style="), "no inline styles: {body}");
        assert!(
            body.contains("3000.0 ms") && body.contains("500.0 ms"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn json_block_holds_labels_and_entries_render_once_under_their_span() {
        let d = detail(
            600,
            logs(
                vec![entry(Some(PROMPT), "hello"), entry(None, "stray")],
                false,
            ),
        );
        let (_, body) = trace_page(d).await;
        let data: Value = serde_json::from_str(json_block(&body)).unwrap();
        assert_eq!(data["trace_id"], ID);
        let spans = data["spans"].as_array().unwrap();
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[1]["id"], PROMPT);
        assert_eq!(spans[1]["name"], "render_prompt greet");
        assert_eq!(spans[1]["parent"], ROOT);
        assert_eq!(spans[1]["labels"]["prompt.name"], "greet");
        assert_eq!(spans[1]["labels"]["prompt.version"], "3");
        assert_eq!(spans[0]["labels"]["gen_ai.agent.name"], "weather_bot");
        // Log entries live only in the server-rendered sections (the span pane clones them).
        assert!(
            data.get("logs").is_none() && data.get("unattached").is_none(),
            "{data}"
        );
        let attached = section(&body, &format!("logs-{PROMPT}"));
        assert!(attached.contains("gen_ai.user.message"), "{attached}");
        assert!(attached.contains("INFO"), "{attached}");
        assert!(
            attached.contains("<pre class=\"payload\">hello</pre>"),
            "{attached}"
        );
        assert!(
            attached.contains(r#"<div class="meta">attributes</div>"#),
            "{attached}"
        );
        let unattached = section(&body, "logs-unattached");
        assert!(
            unattached.contains("Unattached") && unattached.contains("stray"),
            "{unattached}"
        );
        assert!(!body.contains(&format!("logs-{ROOT}")), "{body}");
        assert_eq!(body.matches("hello").count(), 1, "{body}");
    }

    #[tokio::test]
    async fn truncation_note_follows_the_flag() {
        let full = vec![entry(Some(ROOT), "x"); 1000];
        let (_, body) = trace_page(detail(600, logs(full, true))).await;
        assert!(
            body.contains("Showing the first 1,000 log entries; the trace has more."),
            "{body}"
        );
        let (_, body) = trace_page(detail(600, logs(vec![entry(Some(ROOT), "x")], false))).await;
        assert!(
            !body.contains("Showing the first 1,000 log entries"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn truncation_note_counts_the_entries_shown() {
        let some = vec![entry(Some(ROOT), "x"); 300];
        let (_, body) = trace_page(detail(600, logs(some, true))).await;
        assert!(
            body.contains("Showing the first 300 log entries; the trace has more."),
            "{body}"
        );
        assert!(!body.contains("1,000"), "{body}");
    }

    #[tokio::test]
    async fn still_arriving_note_within_two_minutes_of_the_end() {
        let (_, body) = trace_page(detail(30, logs(vec![], false))).await;
        assert!(body.contains("Log entries may still be arriving"), "{body}");
        let (_, body) = trace_page(detail(600, logs(vec![], false))).await;
        assert!(
            !body.contains("Log entries may still be arriving"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn bad_id_is_404_without_a_source_call() {
        for id in [
            "xyz",
            "0123456789abcdef0123456789abcde",
            "0123456789abcdef0123456789abcdeg",
        ] {
            let (src, st) = with_get(Ok(detail(600, logs(vec![], false))));
            let (status, _, body) = get(st, &format!("/traces/{id}")).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{id}");
            assert!(body.contains("trace not found"), "{body}");
            assert_eq!(src.calls.load(Ordering::SeqCst), 0, "{id}");
        }
    }

    #[tokio::test]
    async fn unknown_trace_is_404() {
        let (src, st) = with_get(Err(SourceError::NotFound));
        let (status, _, body) = get(st, &format!("/traces/{ID}")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.contains("trace not found"), "{body}");
        assert_eq!(src.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn trace_403_is_a_502_with_the_hint() {
        let (_, st) = with_get(Err(SourceError::Upstream {
            service: Service::Trace,
            status: 403,
            excerpt: "permission denied".into(),
            reason: None,
        }));
        let (status, _, body) = get(st, &format!("/traces/{ID}")).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(
            body.contains("Cloud Trace: HTTP 403: permission denied"),
            "{body}"
        );
        assert!(
            body.contains(r#"<p class="hint">needs roles/cloudtrace.user</p>"#),
            "{body}"
        );
    }

    #[tokio::test]
    async fn log_failure_shows_a_banner_and_keeps_the_waterfall() {
        let d = detail(
            600,
            Err(SourceError::Upstream {
                service: Service::Logging,
                status: 403,
                excerpt: "no".into(),
                reason: None,
            }),
        );
        let (status, body) = trace_page(d).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            body.contains("Log entries could not be loaded: Cloud Logging: HTTP 403: no"),
            "{body}"
        );
        assert!(
            body.contains(r#"<p class="hint">needs roles/logging.viewer</p>"#),
            "{body}"
        );
        for id in [ROOT, PROMPT, LLM] {
            assert!(body.contains(&format!(r#"data-span="{id}""#)), "{body}");
        }
        let data: Value = serde_json::from_str(json_block(&body)).unwrap();
        assert_eq!(data["spans"].as_array().unwrap().len(), 3);
        assert!(data.get("logs").is_none(), "{data}");
    }

    #[tokio::test]
    async fn long_payloads_render_collapsed_with_an_expand_button() {
        let long = format!("{}{}", "a".repeat(4096), "b".repeat(904));
        let (_, body) = trace_page(detail(600, logs(vec![entry(Some(ROOT), &long)], false))).await;
        let preview = format!(r#"<pre class="preview">{}…</pre>"#, "a".repeat(4096));
        let full = format!(r#"<pre class="full">{long}</pre>"#);
        assert!(
            body.contains(r#"<div class="payload collapsed" data-full>"#),
            "{body}"
        );
        assert!(body.contains(&preview), "preview is the first 4,096 chars");
        assert!(
            body.contains(r#"<button class="expand" type="button">expand (5000 chars)</button>"#),
            "{body}"
        );
        assert!(body.contains(&full), "the full text is in the page");
        assert_eq!(
            body.matches(&long).count(),
            1,
            "the full text is in the page once"
        );

        let exact = "c".repeat(4096);
        let (_, body) = trace_page(detail(600, logs(vec![entry(Some(ROOT), &exact)], false))).await;
        assert!(
            !body.contains("payload collapsed"),
            "4,096 chars stay expanded"
        );
        assert!(body.contains(&format!(r#"<pre class="payload">{exact}</pre>"#)));
    }

    #[tokio::test]
    async fn render_prompt_span_links_to_its_prompt_version() {
        let (_, body) = trace_page(detail(600, logs(vec![], false))).await;
        assert!(body.contains(r#"href="/prompts/greet?v=3""#), "{body}");
        assert_eq!(body.matches("href=\"/prompts/greet").count(), 1, "{body}");
    }

    #[tokio::test]
    async fn nested_prompt_names_are_one_encoded_path_segment() {
        let mut d = detail(600, logs(vec![], false));
        d.spans[1]
            .labels
            .insert("prompt.name".into(), "support/greet".into());
        let (_, body) = trace_page(d).await;
        assert!(
            body.contains(r#"href="/prompts/support%2Fgreet?v=3""#),
            "{body}"
        );
        assert!(body.contains("prompt support/greet v3"), "{body}");
    }

    #[tokio::test]
    async fn script_payloads_cannot_break_out_of_the_page() {
        let evil = "</script><script>alert(1)</script>";
        let (_, plain) = trace_page(detail(600, logs(vec![entry(Some(ROOT), "ok")], false))).await;
        let mut d = detail(600, logs(vec![entry(Some(ROOT), evil)], false));
        d.spans[0].labels.insert("note".into(), evil.into());
        d.spans[0].name = evil.into();
        let (_, body) = trace_page(d).await;

        assert_eq!(
            body.matches("<script").count(),
            2,
            "layout script + JSON block: {body}"
        );
        assert_eq!(
            body.matches("<script").count(),
            plain.matches("<script").count()
        );
        let block = json_block(&body);
        assert!(
            block.contains(r"\u003c/script\u003e\u003cscript\u003ealert(1)"),
            "{block}"
        );
        assert!(
            !block.contains('<') && !block.contains('>') && !block.contains('&'),
            "{block}"
        );
        let data: Value = serde_json::from_str(block).expect("the block is whole JSON");
        assert_eq!(data["spans"][0]["name"], evil);
        assert_eq!(data["spans"][0]["labels"]["note"], evil);
        assert!(
            body.contains("&lt;/script&gt;&lt;script&gt;alert(1)&lt;/script&gt;"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn waterfall_script_writes_text_only() {
        let (status, _, js) = get(
            state(Arc::new(FakeSource::new()), None),
            "/assets/waterfall.js",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        for needle in [
            "textContent",
            "style.left",
            "style.width",
            "trace-data",
            "data-confirm",
            "confirm(",
            "button.expand",
        ] {
            assert!(js.contains(needle), "waterfall.js lacks {needle}");
        }
        for banned in [
            "innerHTML",
            "outerHTML",
            "insertAdjacentHTML",
            "document.write",
            "eval(",
            "new Function",
            "setAttribute(\"on",
            "http",
        ] {
            assert!(!js.contains(banned), "waterfall.js uses {banned}");
        }
    }

    // ---- LLM calls on /traces/{id} ----

    const LLM_B: &str = "000000000000000d";
    const COMPACT: &str = "000000000000000e";
    const PRICES: &str =
        "- model: gemini-3.8-flash\n  input: 0.30\n  output: 2.50\n  cached_input: 0.075\n";

    fn labelled(mut s: RawSpan, labels: &[(&str, &str)]) -> RawSpan {
        s.labels = labels
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        s
    }

    /// Cloud Trace spans: two `generate_content` calls and a compaction span that also carries
    /// usage labels.
    fn llm_spans() -> Vec<RawSpan> {
        let t0 = fixed_now() - TimeDelta::seconds(600);
        vec![
            span(ROOT, None, "invoke_agent weather_bot", t0, 3000),
            labelled(
                span(
                    LLM,
                    Some(ROOT),
                    "generate_content gemini-3.8-flash",
                    t0 + TimeDelta::seconds(1),
                    500,
                ),
                &[
                    ("gen_ai.operation.name", "generate_content"),
                    ("gen_ai.request.model", "gemini-3.8-flash"),
                    ("gen_ai.usage.input_tokens", "1200"),
                    ("gen_ai.usage.output_tokens", "350"),
                    ("gen_ai.usage.cache_read.input_tokens", "200"),
                    ("gen_ai.usage.reasoning.output_tokens", "50"),
                ],
            ),
            labelled(
                span(
                    LLM_B,
                    Some(ROOT),
                    "generate_content other-model",
                    t0 + TimeDelta::seconds(2),
                    300,
                ),
                &[
                    ("gen_ai.operation.name", "generate_content"),
                    ("gen_ai.request.model", "other-model"),
                    ("gen_ai.usage.input_tokens", "100"),
                    ("gen_ai.usage.output_tokens", "20"),
                ],
            ),
            labelled(
                span(
                    COMPACT,
                    Some(ROOT),
                    "compact_events",
                    t0 + TimeDelta::milliseconds(2500),
                    100,
                ),
                &[
                    ("gen_ai.operation.name", "compact_events"),
                    ("gen_ai.usage.input_tokens", "999"),
                    ("gen_ai.usage.output_tokens", "99"),
                ],
            ),
        ]
    }

    async fn llm_page(prices: Option<&str>) -> String {
        let (_, mut st) = with_get(Ok(TraceDetail {
            trace_id: TraceId::parse(ID).unwrap(),
            spans: llm_spans(),
            logs: logs(vec![], false),
        }));
        st.prices = prices.map(|p| Arc::new(PriceTable::from_yaml(p).unwrap()));
        let (status, _, body) = get(st, &format!("/traces/{ID}")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }

    /// The `LLM calls` heading through the end of its table.
    fn calls_section(page: &str) -> &str {
        let start = page
            .find("<h2>LLM calls</h2>")
            .unwrap_or_else(|| panic!("no LLM calls section: {page}"));
        let len = page[start..].find("</table>").expect("table end");
        &page[start..start + len]
    }

    #[tokio::test]
    async fn llm_calls_list_tokens_and_cost_per_call_and_in_total() {
        let page = llm_page(Some(PRICES)).await;
        let calls = calls_section(&page);
        assert!(
            calls.contains(r#"<tr><td>generate_content gemini-3.8-flash</td><td>gemini-3.8-flash</td><td class="num">1,200</td><td class="num">200</td><td class="num">300</td><td class="num">50</td><td class="num">$0.001190</td></tr>"#),
            "{calls}"
        );
        assert!(
            calls.contains(r#"<tr><td>generate_content other-model</td><td>other-model</td><td class="num">100</td><td class="num">0</td><td class="num">20</td><td class="num">0</td><td class="num">—</td></tr>"#),
            "{calls}"
        );
        assert!(
            calls.contains(r#"<tr class="total"><td>Total</td><td>2 calls</td><td class="num">1,300</td><td class="num">200</td><td class="num">320</td><td class="num">50</td><td class="num">$0.001190*</td></tr>"#),
            "{calls}"
        );
        assert!(
            !calls.contains("compact_events"),
            "compaction is not a call: {calls}"
        );
        assert!(
            page.contains("* Costs exclude calls to models without a price."),
            "{page}"
        );
        assert!(!page.contains("No price file"), "{page}");
    }

    #[tokio::test]
    async fn without_prices_llm_calls_show_tokens_and_dashes() {
        let page = llm_page(None).await;
        let calls = calls_section(&page);
        assert!(
            calls.contains(r#"<td class="num">1,200</td><td class="num">200</td><td class="num">300</td><td class="num">50</td><td class="num">—</td></tr>"#),
            "{calls}"
        );
        assert!(
            calls.contains(r#"<tr class="total"><td>Total</td><td>2 calls</td><td class="num">1,300</td><td class="num">200</td><td class="num">320</td><td class="num">50</td><td class="num">—</td></tr>"#),
            "{calls}"
        );
        assert!(page.contains("No price file"), "{page}");
        assert!(!page.contains("* Costs exclude"), "{page}");
    }

    #[tokio::test]
    async fn traces_without_llm_calls_have_no_llm_section() {
        let (status, body) = trace_page(detail(600, logs(vec![], false))).await;
        assert_eq!(status, StatusCode::OK);
        assert!(!body.contains("<h2>LLM calls</h2>"), "{body}");
    }

    // ---- scores ----

    const SCORE_FIELDS: [(&str, &str); 7] = [
        ("timestamp", "TIMESTAMP"),
        ("source", "STRING"),
        ("author", "STRING"),
        ("name", "STRING"),
        ("data_type", "STRING"),
        ("value", "STRING"),
        ("comment", "STRING"),
    ];

    /// `with_get(Ok(d))` plus the BigQuery usage source over `fake`.
    fn bq_get(d: TraceDetail, fake: &Arc<FakeHttp>) -> AppState {
        let (_, mut st) = with_get(Ok(d));
        st.usage = Some(Arc::new(UsageSource::with_client(
            BigQueryClient::with_client(fake.clone(), Arc::new(NoAuth)),
            "billing-proj",
            BigQueryTable::new("data-proj", "agent_analytics", "agent_events"),
            1_000,
        )));
        st
    }

    /// `detail(600, …)` whose root and LLM spans name invocation `e-1` and whose prompt span
    /// names `e-2`.
    fn invocation_detail() -> TraceDetail {
        let mut d = detail(600, logs(vec![], false));
        for (span, inv) in d.spans.iter_mut().zip(["e-1", "e-2", "e-1"]) {
            span.labels.insert("invocation_id".into(), inv.into());
        }
        d
    }

    /// The `Scores` heading through the `Log entries` heading.
    fn scores_section(page: &str) -> &str {
        let start = page
            .find("<h2>Scores</h2>")
            .unwrap_or_else(|| panic!("no Scores section: {page}"));
        let len = page[start..]
            .find("<h2>Log entries</h2>")
            .expect("log entries");
        &page[start..start + len]
    }

    /// A score row of `SCORE_FIELDS` written `secs_ago` seconds before `fixed_now`.
    fn score_cells(secs_ago: i64, cells: [Value; 6]) -> Vec<Value> {
        let at = fixed_now() - TimeDelta::seconds(secs_ago);
        let mut row = vec![json!(at.timestamp_micros().to_string())];
        row.extend(cells);
        row
    }

    #[tokio::test]
    async fn scores_list_newest_first_with_source_author_and_value() {
        let fake = FakeHttp::new(vec![reply(
            &SCORE_FIELDS,
            vec![
                score_cells(
                    120,
                    [
                        json!("console"),
                        json!("chris"),
                        json!("quality"),
                        json!("category"),
                        json!("helpful"),
                        json!("clear"),
                    ],
                ),
                score_cells(
                    300,
                    [
                        json!("app"),
                        json!("user-1"),
                        json!("thumbs"),
                        json!("bool"),
                        json!("true"),
                        Value::Null,
                    ],
                ),
            ],
            false,
        )]);
        let (status, _, page) =
            get(bq_get(invocation_detail(), &fake), &format!("/traces/{ID}")).await;
        assert_eq!(status, StatusCode::OK, "{page}");
        let scores = scores_section(&page);
        assert!(
            scores.contains(r#"<tr><td class="mono">2026-11-16T11:58:00.000Z</td><td>console</td><td>chris</td><td>quality</td><td>helpful</td><td>clear</td></tr><tr><td class="mono">2026-11-16T11:55:00.000Z</td><td>app</td><td>user-1</td><td>thumbs</td><td>thumbs up</td><td></td></tr>"#),
            "{scores}"
        );
        assert_eq!(fake.uris().len(), 1);
        let body = fake.body(0);
        assert_eq!(
            param_value(&body, "trace_id")["parameterValue"]["value"],
            json!(ID)
        );
        assert_eq!(
            param_value(&body, "ids")["parameterValue"],
            json!({ "arrayValues": [{ "value": "e-1" }, { "value": "e-2" }] })
        );
        assert_eq!(
            param_value(&body, "start")["parameterValue"]["value"],
            json!("2026-11-16 11:49:57.000000+00:00")
        );
    }

    #[tokio::test]
    async fn a_trace_without_scores_says_so() {
        let fake = FakeHttp::new(vec![reply(&SCORE_FIELDS, vec![], false)]);
        let (status, _, page) =
            get(bq_get(invocation_detail(), &fake), &format!("/traces/{ID}")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            scores_section(&page).contains("No scores for this trace."),
            "{page}"
        );
    }

    #[tokio::test]
    async fn a_failed_score_query_is_a_banner_and_the_trace_still_renders() {
        let fake = FakeHttp::new(vec![Ok((
            403,
            json!({ "error": { "code": 403, "message": "Access Denied", "errors": [{ "reason": "accessDenied" }] } }).to_string(),
        ))]);
        let (status, _, page) =
            get(bq_get(invocation_detail(), &fake), &format!("/traces/{ID}")).await;
        assert_eq!(status, StatusCode::OK);
        let scores = scores_section(&page);
        assert!(
            scores.contains("Scores could not be loaded: BigQuery: HTTP 403: Access Denied"),
            "{scores}"
        );
        assert!(
            scores.contains("needs roles/bigquery.dataViewer and roles/bigquery.jobUser"),
            "{scores}"
        );
        assert!(page.contains(r#"<div class="waterfall">"#), "{page}");
    }

    #[tokio::test]
    async fn with_the_cloud_trace_source_scores_are_a_note() {
        let (status, page) = trace_page(detail(600, logs(vec![], false))).await;
        assert_eq!(status, StatusCode::OK);
        let scores = scores_section(&page);
        assert!(
            scores.contains("Scores need the BigQuery trace source"),
            "{scores}"
        );
    }
}
