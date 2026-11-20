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
use super::pages::trace::LogView;
use super::pages::{trace, traces};
use crate::model::{attach_logs, build_tree, trace_end};
use crate::traces::{SourceError, TimeRange, TraceId, TraceQuery};

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
    let logs = detail.logs.map(|set| LogView {
        attached: attach_logs(&detail.spans, set.entries),
        truncated: set.truncated,
    });
    html(
        StatusCode::OK,
        trace::render(&id, &rows, &logs, still_arriving),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex};

    use chrono::{DateTime, TimeDelta, Utc};
    use http::StatusCode;
    use serde_json::{Value, json};

    use crate::traces::{
        LogEntry, LogSet, RawSpan, Service, SourceError, TimeRange, TraceDetail, TraceId,
        TracePage, TraceQuery, TraceSource, TraceSummary,
    };
    use crate::web::AppState;
    use crate::web::testutil::{FakeSource, fixed_now, get, state};

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
        let (_, body) = trace_page(detail(600, logs(vec![entry(Some(ROOT), "x")], true))).await;
        assert!(
            body.contains("Showing the first 1,000 log entries"),
            "{body}"
        );
        let (_, body) = trace_page(detail(600, logs(vec![entry(Some(ROOT), "x")], false))).await;
        assert!(
            !body.contains("Showing the first 1,000 log entries"),
            "{body}"
        );
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
}
