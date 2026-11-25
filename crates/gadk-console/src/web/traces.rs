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

use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use chrono::TimeDelta;
use gadk_analytics::scores::{BQ_EDITOR_HINT, NewScore};
use http::StatusCode;
use serde::Deserialize;

use super::AppState;
use super::guard::check_csrf;
use super::pages::error::{error_page, source_error};
use super::pages::layout::{Nav, html};
use super::pages::trace::{
    Annotations, DatasetView, LogView, NEEDS_BIGQUERY_SCORES, ReviewBar, ScoresView,
};
use super::pages::{trace, traces};
use super::review::next_link;
use crate::datasets::{AddError, NEEDS_DATASETS, is_dataset_name};
use crate::model::{attach_logs, build_tree, trace_end};
use crate::review::{Cursor, ReviewQueue, at_text};
use crate::rubrics::{NO_RUBRICS, Rubrics};
use crate::traces::{RawSpan, SourceError, TimeRange, TraceId, TraceQuery};
use crate::url::percent_encode;

/// How long after a trace's last span ends its log entries may still be arriving.
const STILL_ARRIVING: TimeDelta = TimeDelta::seconds(120);

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/traces", get(list))
        .route("/traces/{id}", get(detail))
        .route("/traces/{id}/scores", post(add_score))
        .route("/traces/{id}/rubric", post(add_rubric_scores))
        .route("/traces/{id}/dataset", post(add_to_dataset))
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

/// The notice after an add to a dataset, from the `/traces/{id}` query: exactly one `added`
/// that is a count and one `dataset` that is a valid name; anything else shows no notice (and
/// never fails the page).
fn added_notice(params: &[(String, String)]) -> Option<(usize, String)> {
    let one = |key: &str| {
        let mut values = params.iter().filter(|(k, _)| k == key).map(|(_, v)| v);
        match (values.next(), values.next()) {
            (Some(v), None) => Some(v.clone()),
            _ => None,
        }
    };
    let added = one("added")?.parse::<usize>().ok()?;
    let dataset = one("dataset").filter(|d| is_dataset_name(d))?;
    Some((added, dataset))
}

/// The review bar of trace `id`, from the `/traces/{id}` query: with the BigQuery source, a
/// valid queue (`score` or a loaded `rubric`, plus optional filters) and a valid `at`; else
/// `None` (never fails the page).
fn review_bar_from_query(
    state: &AppState,
    id: &TraceId,
    params: &[(String, String)],
) -> Option<ReviewBar> {
    state.usage.as_ref()?;
    let queue = ReviewQueue::from_params(params, state.rubrics.as_deref()).ok()??;
    let at = params
        .iter()
        .find(|(k, _)| k == "at")
        .map_or("", |(_, v)| v.as_str());
    let cursor = Cursor::parse(at, id.as_str()).ok()?;
    Some(ReviewBar { queue, cursor })
}

async fn detail(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(params): Query<Vec<(String, String)>>,
) -> Response {
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
    let annotations = Annotations {
        scores: trace_scores(&state, &id, &detail.spans).await,
        dataset: DatasetView {
            names: state.datasets.as_ref().map(|d| d.names()),
            added: added_notice(&params),
        },
        review: review_bar_from_query(&state, &id, &params),
        rubrics: state.rubrics.clone(),
    };
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
            &annotations,
            &state.csrf,
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

/// The hidden `q_*` fields of the score, rubric and dataset forms: the review queue and `at` in
/// review mode, empty strings otherwise.
///
/// The fields must stay `String`: they are flattened into urlencoded forms, which deserialize
/// only string fields reliably.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ReviewFields {
    q_score: String,
    q_rubric: String,
    q_prompt: String,
    q_version: String,
    q_flagged: String,
    q_range: String,
    q_at: String,
}

impl ReviewFields {
    /// The fields of a form read as pairs: the first value of each `q_*` field.
    fn from_pairs(pairs: &[(String, String)]) -> ReviewFields {
        let first = |key: &str| {
            pairs
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        };
        ReviewFields {
            q_score: first("q_score"),
            q_rubric: first("q_rubric"),
            q_prompt: first("q_prompt"),
            q_version: first("q_version"),
            q_flagged: first("q_flagged"),
            q_range: first("q_range"),
            q_at: first("q_at"),
        }
    }

    /// The review queue (a rubric queue's rubric among `rubrics`) and the cursor of trace
    /// `id`, when the fields are a valid queue and `at`; else `None`.
    fn parse(&self, id: &TraceId, rubrics: Option<&Rubrics>) -> Option<(ReviewQueue, Cursor)> {
        let params: Vec<(String, String)> = [
            ("score", &self.q_score),
            ("rubric", &self.q_rubric),
            ("prompt", &self.q_prompt),
            ("version", &self.q_version),
            ("flagged", &self.q_flagged),
            ("range", &self.q_range),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.clone()))
        .collect();
        let queue = ReviewQueue::from_params(&params, rubrics).ok()??;
        let cursor = Cursor::parse(&self.q_at, id.as_str()).ok()?;
        Some((queue, cursor))
    }

    /// Where a successful score goes: the next trace of the review queue when the fields are a
    /// valid queue and `at`, else back to trace `id`.
    fn after_score(&self, id: &TraceId, rubrics: Option<&Rubrics>) -> String {
        match self.parse(id, rubrics) {
            Some((queue, cursor)) => next_link(&queue, &cursor),
            None => format!("/traces/{}", id.as_str()),
        }
    }
}

/// The add-score form; missing fields are empty strings. The `q_*` fields carry the review
/// queue and `at` in review mode.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ScoreForm {
    name: String,
    kind: String,
    value: String,
    comment: String,
    csrf: String,
    #[serde(flatten)]
    review: ReviewFields,
}

/// `POST /traces/{id}/scores`: checks the token, validates the score (400), reads the trace's
/// identity (404 when it has none in the last 30 days), inserts a `FEEDBACK` row and redirects
/// (303) to the trace, or in review mode to the queue's next trace. Without the BigQuery source
/// it is a 404 page.
async fn add_score(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Form(form): Form<ScoreForm>,
) -> Response {
    if let Err(resp) = check_csrf(&state, &form.csrf) {
        return resp;
    }
    let Some(id) = TraceId::parse(&id) else {
        return source_error(&SourceError::NotFound);
    };
    let Some(usage) = &state.usage else {
        return error_page(
            StatusCode::NOT_FOUND,
            Nav::Traces,
            NEEDS_BIGQUERY_SCORES,
            None,
        );
    };
    let score = match NewScore::from_form(&form.name, &form.kind, &form.value, &form.comment) {
        Ok(score) => score,
        Err(text) => return error_page(StatusCode::BAD_REQUEST, Nav::Traces, &text, None),
    };
    let root = match usage.trace_root(id.as_str()).await {
        Ok(Some(root)) => root,
        Ok(None) => return source_error(&SourceError::NotFound),
        Err(e) => return source_error(&e),
    };
    match usage
        .insert_score(id.as_str(), &root, &score, &state.author)
        .await
    {
        Ok(()) => {
            Redirect::to(&form.review.after_score(&id, state.rubrics.as_deref())).into_response()
        }
        Err(e) => insert_failed(&e),
    }
}

/// `POST /traces/{id}/rubric`: checks the token, the trace id, the BigQuery source and the
/// loaded rubrics (404), then the rubric and each criterion's `c_<criterion>` value (400, the
/// first failing criterion in file order), reads the trace's identity (404 when it has none in
/// the last 30 days), inserts one `FEEDBACK` row per criterion in one statement and redirects
/// (303) to the trace, or in review mode to the queue's next trace.
async fn add_rubric_scores(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Form(form): Form<Vec<(String, String)>>,
) -> Response {
    let field = |key: &str| {
        form.iter()
            .find(|(k, _)| k == key)
            .map_or("", |(_, v)| v.as_str())
    };
    if let Err(resp) = check_csrf(&state, field("csrf")) {
        return resp;
    }
    let Some(id) = TraceId::parse(&id) else {
        return source_error(&SourceError::NotFound);
    };
    let Some(usage) = &state.usage else {
        return error_page(
            StatusCode::NOT_FOUND,
            Nav::Traces,
            NEEDS_BIGQUERY_SCORES,
            None,
        );
    };
    let Some(rubrics) = state.rubrics.as_deref() else {
        return error_page(StatusCode::NOT_FOUND, Nav::Traces, NO_RUBRICS, None);
    };
    let name = field("rubric").trim();
    let Some(rubric) = rubrics.get(name) else {
        let text = format!("rubric: unknown rubric `{name}`");
        return error_page(StatusCode::BAD_REQUEST, Nav::Traces, &text, None);
    };
    let scores = match rubric.scores_from_form(&form, field("comment")) {
        Ok(scores) => scores,
        Err(text) => return error_page(StatusCode::BAD_REQUEST, Nav::Traces, &text, None),
    };
    let root = match usage.trace_root(id.as_str()).await {
        Ok(Some(root)) => root,
        Ok(None) => return source_error(&SourceError::NotFound),
        Err(e) => return source_error(&e),
    };
    match usage
        .insert_scores(id.as_str(), &root, &scores, &state.author)
        .await
    {
        Ok(()) => {
            let to = ReviewFields::from_pairs(&form).after_score(&id, Some(rubrics));
            Redirect::to(&to).into_response()
        }
        Err(e) => insert_failed(&e),
    }
}

/// A failed score insert: 502 with the error; a permission failure hints at
/// `roles/bigquery.dataEditor`.
fn insert_failed(e: &SourceError) -> Response {
    let hint = match e {
        SourceError::BigQuery {
            status: Some(403),
            reason,
            ..
        } if matches!(reason.as_deref(), None | Some("accessDenied")) => Some(BQ_EDITOR_HINT),
        _ => e.hint(),
    };
    error_page(StatusCode::BAD_GATEWAY, Nav::Traces, &e.to_string(), hint)
}

/// The add-to-dataset form; missing fields are empty strings. The `q_*` fields carry the
/// review queue and `at` in review mode.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct DatasetForm {
    dataset: String,
    rubric: String,
    csrf: String,
    #[serde(flatten)]
    review: ReviewFields,
}

impl DatasetForm {
    /// Where a successful add of `added` cases goes: trace `id` with the added notice, and in
    /// review mode (valid `q_*` fields, a rubric queue's rubric among `rubrics`) the queue and
    /// `at` too, so the page stays in review mode.
    fn redirect(&self, id: &TraceId, added: usize, rubrics: Option<&Rubrics>) -> String {
        let mut url = format!(
            "/traces/{}?added={added}&dataset={}",
            id.as_str(),
            percent_encode(&self.dataset)
        );
        if let Some((queue, cursor)) = self.review.parse(id, rubrics) {
            url.push_str(&format!(
                "&{}&at={}",
                queue.query_string(),
                percent_encode(&at_text(cursor.at))
            ));
        }
        url
    }
}

/// `POST /traces/{id}/dataset`: checks the token, adds the trace's runs as cases to
/// `<dir>/<dataset>.yaml` and redirects to the trace with the count (303), keeping review mode.
/// Without `--datasets` it is a 404 page.
async fn add_to_dataset(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Form(form): Form<DatasetForm>,
) -> Response {
    if let Err(resp) = check_csrf(&state, &form.csrf) {
        return resp;
    }
    let Some(id) = TraceId::parse(&id) else {
        return source_error(&SourceError::NotFound);
    };
    let Some(datasets) = &state.datasets else {
        return error_page(StatusCode::NOT_FOUND, Nav::Traces, NEEDS_DATASETS, None);
    };
    let rubric = Some(form.rubric.as_str()).filter(|r| !r.trim().is_empty());
    match datasets.add_trace(id.as_str(), &form.dataset, rubric).await {
        Ok(added) => {
            Redirect::to(&form.redirect(&id, added, state.rubrics.as_deref())).into_response()
        }
        Err(e) => dataset_failed(&e),
    }
}

/// The page for a trace that was not added: 400 for the request or the file, 404 for an
/// unknown trace, 502 for BigQuery, 500 for a failed write.
fn dataset_failed(e: &AddError) -> Response {
    let (status, text) = match e {
        AddError::BadName => (
            StatusCode::BAD_REQUEST,
            "dataset: must be 1-64 of A-Z, a-z, 0-9, _ and -".to_owned(),
        ),
        AddError::NoTrace => return source_error(&SourceError::NotFound),
        AddError::NothingAdded(reasons) => (
            StatusCode::BAD_REQUEST,
            format!("no case added: {}", reasons.join("; ")),
        ),
        AddError::File(text) => (StatusCode::BAD_REQUEST, text.clone()),
        AddError::Source(text) => (StatusCode::BAD_GATEWAY, text.clone()),
        AddError::Write(text) => (StatusCode::INTERNAL_SERVER_ERROR, text.clone()),
    };
    error_page(status, Nav::Traces, &text, None)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex};

    use chrono::{DateTime, TimeDelta, Utc};
    use gadk_eval::{EvalSet, RunTranscript, TurnRecord};
    use gadk_eval_monitor::{ExportContext, ProductionRun, RunScore, TraceInvocation};
    use http::StatusCode;
    use serde_json::{Value, json};

    use crate::datasets::Datasets;
    use crate::prices::PriceTable;
    use crate::traces::BigQueryTable;
    use crate::traces::{
        LogEntry, LogSet, RawSpan, Service, SourceError, TimeRange, TraceDetail, TraceId,
        TracePage, TraceQuery, TraceSource, TraceSummary,
    };
    use crate::usage::UsageSource;
    use crate::web::AppState;
    use crate::web::testutil::{AUTHOR, CSRF, FakeExport, post_form, test_rubrics};
    use crate::web::testutil::{FakeSource, fixed_now, get, state};
    use gadk_analytics::testutil::{FakeHttp, param_value, reply};
    use gadk_bigquery::{BigQueryClient, NoAuth};
    use http::header::LOCATION;

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

    // ---- adding scores ----

    const ORIGIN: &str = "http://127.0.0.1:8765";

    const ROOT_FIELDS: [(&str, &str); 4] = [
        ("user_id", "STRING"),
        ("session_id", "STRING"),
        ("invocation_id", "STRING"),
        ("app_name", "STRING"),
    ];

    /// The trace's identity: user `u-1`, session `s-1`, invocation `e-1`, app `shop`.
    fn root_reply() -> Result<(u16, String), String> {
        reply(
            &ROOT_FIELDS,
            vec![vec![
                json!("u-1"),
                json!("s-1"),
                json!("e-1"),
                json!("shop"),
            ]],
            false,
        )
    }

    /// A completed DML reply.
    fn dml_reply() -> Result<(u16, String), String> {
        Ok((
            200,
            json!({
                "jobComplete": true,
                "numDmlAffectedRows": "1",
                "jobReference": { "projectId": "billing-proj", "jobId": "job_2" }
            })
            .to_string(),
        ))
    }

    fn denied() -> Result<(u16, String), String> {
        Ok((
            403,
            json!({ "error": { "code": 403, "message": "Access Denied", "errors": [{ "reason": "accessDenied" }] } }).to_string(),
        ))
    }

    fn score_path() -> String {
        format!("/traces/{ID}/scores")
    }

    #[tokio::test]
    async fn the_score_form_posts_with_the_token_and_a_confirmation() {
        let fake = FakeHttp::new(vec![reply(&SCORE_FIELDS, vec![], false)]);
        let (_, _, page) = get(bq_get(invocation_detail(), &fake), &format!("/traces/{ID}")).await;
        let scores = scores_section(&page);
        let open = format!(
            r#"<form method="post" action="/traces/{ID}/scores" data-confirm="Add this score to the trace?" class="inline"><input type="hidden" name="csrf" value="{CSRF}">"#
        );
        assert!(scores.contains(&open), "{scores}");
        for piece in [
            r#"<input type="text" name="name" placeholder="score name" required maxlength="100">"#,
            r#"<select name="kind"><option value="up">thumbs up</option><option value="down">thumbs down</option><option value="number">number</option><option value="category">category</option></select>"#,
            r#"<input type="text" name="value" placeholder="number or category" maxlength="100">"#,
            r#"<input type="text" name="comment" placeholder="comment (optional)" maxlength="1000">"#,
            r#"<button type="submit">Add score</button>"#,
        ] {
            assert!(scores.contains(piece), "{piece}: {scores}");
        }
        let (_, page) = trace_page(detail(600, logs(vec![], false))).await;
        assert!(
            !page.contains("/scores\""),
            "no form without BigQuery: {page}"
        );
    }

    #[tokio::test]
    async fn a_valid_score_reads_the_trace_identity_then_inserts_and_redirects() {
        let fake = FakeHttp::new(vec![root_reply(), dml_reply()]);
        let body = format!("name=quality&kind=number&value=3.75&comment=clear&csrf={CSRF}");
        let st = bq_get(invocation_detail(), &fake);
        let (status, headers, page) = post_form(st, &score_path(), &body, Some(ORIGIN)).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "{page}");
        assert_eq!(headers[LOCATION], format!("/traces/{ID}"));
        assert_eq!(fake.uris().len(), 2);

        let root = fake.body(0);
        let sql = root["query"].as_str().unwrap();
        assert!(
            sql.starts_with("SELECT user_id, session_id, invocation_id,"),
            "{sql}"
        );
        assert_eq!(root["maximumBytesBilled"], json!("1000"));
        assert_eq!(
            param_value(&root, "trace_id")["parameterValue"]["value"],
            json!(ID)
        );

        let insert = fake.body(1);
        let sql = insert["query"].as_str().unwrap();
        assert!(
            sql.starts_with("INSERT INTO `data-proj.agent_analytics.agent_events`"),
            "{sql}"
        );
        assert_eq!(insert["maximumBytesBilled"], json!("10485760"));
        for (name, want) in [
            ("trace_id", ID),
            ("session_id", "s-1"),
            ("invocation_id", "e-1"),
            ("user_id", "u-1"),
        ] {
            assert_eq!(
                param_value(&insert, name)["parameterValue"]["value"],
                json!(want),
                "{name}"
            );
        }
        let json_param = |name: &str| -> Value {
            serde_json::from_str(
                param_value(&insert, name)["parameterValue"]["value"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap()
        };
        assert_eq!(
            json_param("content"),
            json!({"name": "quality", "value": 3.75, "data_type": "number", "comment": "clear"})
        );
        assert_eq!(
            json_param("attributes"),
            json!({"source": "console", "author": AUTHOR, "adk": {"app_name": "shop", "invocation_id": "e-1"}})
        );
        for value in ["quality", "clear", AUTHOR, "s-1", "u-1"] {
            assert!(!sql.contains(value), "{value} is a parameter: {sql}");
        }
    }

    #[tokio::test]
    async fn a_padded_score_name_is_inserted_trimmed() {
        let fake = FakeHttp::new(vec![root_reply(), dml_reply()]);
        let body = format!("name=%20quality%20&kind=up&csrf={CSRF}");
        let st = bq_get(invocation_detail(), &fake);
        let (status, _, page) = post_form(st, &score_path(), &body, Some(ORIGIN)).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "{page}");
        let insert = fake.body(1);
        let content: Value = serde_json::from_str(
            param_value(&insert, "content")["parameterValue"]["value"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            content,
            json!({"name": "quality", "value": true, "data_type": "bool"})
        );
    }

    #[tokio::test]
    async fn score_posts_without_the_token_are_403_and_query_nothing() {
        for csrf in [String::new(), "b".repeat(64)] {
            let fake = FakeHttp::new(vec![]);
            let body = format!("name=q&kind=up&csrf={csrf}");
            let st = bq_get(invocation_detail(), &fake);
            let (status, _, _) = post_form(st, &score_path(), &body, Some(ORIGIN)).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{csrf:?}");
            assert!(fake.uris().is_empty());
        }
        let fake = FakeHttp::new(vec![]);
        let body = format!("name=q&kind=up&csrf={CSRF}");
        let st = bq_get(invocation_detail(), &fake);
        let (status, _, _) = post_form(st, &score_path(), &body, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "no Origin");
    }

    #[tokio::test]
    async fn invalid_scores_are_400_and_query_nothing() {
        let cases = [
            ("name=&kind=up", "score name is empty"),
            (
                "name=q&kind=maybe",
                "kind: must be up, down, number or category",
            ),
            ("name=q&kind=number&value=abc", "value: not a number"),
            ("name=q&kind=number&value=inf", "score number is not finite"),
            ("name=q&kind=category&value=+", "score category is empty"),
        ];
        for (form, text) in cases {
            let fake = FakeHttp::new(vec![]);
            let body = format!("{form}&csrf={CSRF}");
            let st = bq_get(invocation_detail(), &fake);
            let (status, _, page) = post_form(st, &score_path(), &body, Some(ORIGIN)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{form}");
            assert!(page.contains(text), "{form}: {page}");
            assert!(fake.uris().is_empty(), "{form}");
        }
    }

    #[tokio::test]
    async fn a_trace_without_an_identity_is_404_and_inserts_nothing() {
        let fake = FakeHttp::new(vec![reply(&ROOT_FIELDS, vec![], false)]);
        let body = format!("name=q&kind=up&csrf={CSRF}");
        let st = bq_get(invocation_detail(), &fake);
        let (status, _, page) = post_form(st, &score_path(), &body, Some(ORIGIN)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(page.contains("trace not found"), "{page}");
        assert_eq!(fake.uris().len(), 1, "no insert");

        let fake = FakeHttp::new(vec![]);
        let st = bq_get(invocation_detail(), &fake);
        let (status, _, _) = post_form(st, "/traces/not-a-trace/scores", &body, Some(ORIGIN)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(fake.uris().is_empty());
    }

    #[tokio::test]
    async fn a_refused_insert_is_a_502_with_the_data_editor_hint() {
        let fake = FakeHttp::new(vec![root_reply(), denied()]);
        let body = format!("name=q&kind=down&csrf={CSRF}");
        let st = bq_get(invocation_detail(), &fake);
        let (status, _, page) = post_form(st, &score_path(), &body, Some(ORIGIN)).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(page.contains("BigQuery: HTTP 403: Access Denied"), "{page}");
        assert!(
            page.contains(r#"<p class="hint">needs roles/bigquery.dataEditor on the table and roles/bigquery.jobUser</p>"#),
            "{page}"
        );
    }

    #[tokio::test]
    async fn without_bigquery_a_score_post_is_404_with_the_note() {
        let (_, st) = with_get(Ok(detail(600, logs(vec![], false))));
        let body = format!("name=q&kind=up&csrf={CSRF}");
        let (status, _, page) = post_form(st, &score_path(), &body, Some(ORIGIN)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            page.contains("Scores need the BigQuery trace source"),
            "{page}"
        );
    }

    // ---- review mode ----

    const AT: &str = "2026-11-16T11%3A58%3A59.999750Z";

    /// GET `/traces/{ID}{query}` with the BigQuery source answering the scores query with no
    /// rows; returns the page.
    async fn review_page(query: &str) -> String {
        let fake = FakeHttp::new(vec![reply(&SCORE_FIELDS, vec![], false)]);
        let (status, _, page) = get(
            bq_get(invocation_detail(), &fake),
            &format!("/traces/{ID}{query}"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{query}: {page}");
        page
    }

    #[tokio::test]
    async fn a_queue_and_at_render_the_review_bar_and_fill_the_score_form() {
        let page = review_page(&format!(
            "?score=quality&prompt=triage&version=2&flagged=1&range=24h&at={AT}"
        ))
        .await;
        let qs = "score=quality&amp;prompt=triage&amp;version=2&amp;flagged=1&amp;range=24h";
        let bar = format!(
            r#"<div class="banner info"><p>Reviewing <strong>quality</strong> · <a href="/review/next?{qs}&amp;after=2026-11-16T11%3A58%3A59.999750Z&amp;after_id={ID}">Skip →</a> · <a href="/review?{qs}">Back to queue</a></p></div><h1>Trace "#
        );
        assert!(page.contains(&bar), "{page}");
        let scores = scores_section(&page);
        let fields = format!(
            r#"<input type="hidden" name="csrf" value="{CSRF}"><input type="hidden" name="q_score" value="quality"><input type="hidden" name="q_prompt" value="triage"><input type="hidden" name="q_version" value="2"><input type="hidden" name="q_flagged" value="1"><input type="hidden" name="q_range" value="24h"><input type="hidden" name="q_at" value="2026-11-16T11:58:59.999750Z"><input type="text" name="name" placeholder="score name" value="quality" required maxlength="100">"#
        );
        assert!(scores.contains(&fields), "{scores}");

        // Absent filters are empty hidden fields and the default range is 7d.
        let page = review_page(&format!("?score=quality&at={AT}")).await;
        assert!(
            page.contains(r#"<a href="/review?score=quality">Back to queue</a>"#),
            "{page}"
        );
        assert!(
            page.contains(r#"<input type="hidden" name="q_prompt" value=""><input type="hidden" name="q_version" value=""><input type="hidden" name="q_flagged" value=""><input type="hidden" name="q_range" value="7d">"#),
            "{page}"
        );
    }

    #[tokio::test]
    async fn without_a_valid_queue_and_at_the_page_is_unchanged() {
        let plain = review_page("").await;
        assert!(!plain.contains("Reviewing"), "{plain}");
        for query in [
            "?score=quality".to_owned(),
            format!("?at={AT}"),
            format!("?score=%20&at={AT}"),
            "?score=quality&at=yesterday".to_owned(),
            format!("?score=quality&version=0&prompt=p&at={AT}"),
            format!("?score=quality&version=2&at={AT}"),
            format!("?score=quality&range=30d&at={AT}"),
        ] {
            assert_eq!(review_page(&query).await, plain, "{query}");
        }
    }

    #[tokio::test]
    async fn with_the_cloud_trace_source_there_is_no_review_bar() {
        let (_, plain) = trace_page(detail(600, logs(vec![], false))).await;
        let (_, st) = with_get(Ok(detail(600, logs(vec![], false))));
        let (status, _, page) = get(st, &format!("/traces/{ID}?score=quality&at={AT}")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(page, plain);
    }

    #[tokio::test]
    async fn hostile_queue_values_are_escaped_in_the_bar_and_encoded_in_links() {
        let page = review_page(&format!(
            "?score=%3Cscript%3E%26%22&prompt=%22%3E%3Cscript%3E&at={AT}"
        ))
        .await;
        assert!(!page.contains("<script>"), "{page}");
        assert!(
            page.contains("Reviewing <strong>&lt;script&gt;&amp;&quot;</strong>"),
            "{page}"
        );
        assert!(
            page.contains(r#"<a href="/review?score=%3Cscript%3E%26%22&amp;prompt=%22%3E%3Cscript%3E">Back to queue</a>"#),
            "{page}"
        );
        assert!(
            page.contains(
                r#"<input type="hidden" name="q_prompt" value="&quot;&gt;&lt;script&gt;">"#
            ),
            "{page}"
        );
        assert!(
            page.contains(r#"<input type="text" name="name" placeholder="score name" value="&lt;script&gt;&amp;&quot;" required maxlength="100">"#),
            "{page}"
        );
    }

    #[tokio::test]
    async fn a_score_in_review_mode_redirects_to_the_next_trace() {
        let fake = FakeHttp::new(vec![root_reply(), dml_reply()]);
        let body = format!(
            "name=quality&kind=up&csrf={CSRF}&q_score=quality&q_prompt=team%2Fgreet&q_version=3&q_flagged=1&q_range=6h&q_at=2026-11-16T11%3A58%3A59.999750Z"
        );
        let st = bq_get(invocation_detail(), &fake);
        let (status, headers, page) = post_form(st, &score_path(), &body, Some(ORIGIN)).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "{page}");
        assert_eq!(
            headers[LOCATION],
            format!(
                "/review/next?score=quality&prompt=team%2Fgreet&version=3&flagged=1&range=6h&after=2026-11-16T11%3A58%3A59.999750Z&after_id={ID}"
            )
        );
        assert_eq!(fake.uris().len(), 2, "the score is inserted first");
    }

    #[tokio::test]
    async fn a_score_without_valid_review_fields_redirects_to_the_trace() {
        for review in [
            "",
            "&q_score=quality",
            "&q_score=quality&q_at=yesterday",
            "&q_score=&q_at=2026-11-16T11%3A58%3A59.999750Z",
            "&q_score=quality&q_version=0&q_prompt=p&q_at=2026-11-16T11%3A58%3A59.999750Z",
            "&q_score=quality&q_range=30d&q_at=2026-11-16T11%3A58%3A59.999750Z",
        ] {
            let fake = FakeHttp::new(vec![root_reply(), dml_reply()]);
            let body = format!("name=quality&kind=up&csrf={CSRF}{review}");
            let st = bq_get(invocation_detail(), &fake);
            let (status, headers, _) = post_form(st, &score_path(), &body, Some(ORIGIN)).await;
            assert_eq!(status, StatusCode::SEE_OTHER, "{review}");
            assert_eq!(headers[LOCATION], format!("/traces/{ID}"), "{review}");
        }
    }

    #[tokio::test]
    async fn review_fields_do_not_change_score_errors() {
        let review = "&q_score=quality&q_at=2026-11-16T11%3A58%3A59.999750Z";
        let fake = FakeHttp::new(vec![]);
        let body = format!("name=&kind=up&csrf={CSRF}{review}");
        let st = bq_get(invocation_detail(), &fake);
        let (status, _, page) = post_form(st, &score_path(), &body, Some(ORIGIN)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(page.contains("score name is empty"), "{page}");

        let fake = FakeHttp::new(vec![root_reply(), denied()]);
        let body = format!("name=q&kind=down&csrf={CSRF}{review}");
        let st = bq_get(invocation_detail(), &fake);
        let (status, _, _) = post_form(st, &score_path(), &body, Some(ORIGIN)).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn the_default_range_score_form_redirects_without_a_range() {
        let fake = FakeHttp::new(vec![root_reply(), dml_reply()]);
        let body = format!(
            "name=quality&kind=up&csrf={CSRF}&q_score=quality&q_prompt=&q_version=&q_flagged=&q_range=7d&q_at=2026-11-16T11%3A58%3A59.999750Z"
        );
        let st = bq_get(invocation_detail(), &fake);
        let (status, headers, page) = post_form(st, &score_path(), &body, Some(ORIGIN)).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "{page}");
        assert_eq!(
            headers[LOCATION],
            format!("/review/next?score=quality&after={AT}&after_id={ID}")
        );
    }

    // ---- adding to a dataset ----

    /// A complete run `id` of trace `ID`, started `secs_ago` seconds before `fixed_now`, with
    /// one turn `user` → `reply`.
    fn trace_run(id: &str, secs_ago: i64, user: &str, reply: &str) -> ProductionRun {
        ProductionRun {
            invocation_id: id.into(),
            session_id: "s-1".into(),
            trace_id: Some(ID.into()),
            agent: "shop".into(),
            started_at: fixed_now() - TimeDelta::seconds(secs_ago),
            transcript: RunTranscript {
                turns: vec![TurnRecord {
                    user: user.into(),
                    reply: reply.into(),
                    calls: Vec::new(),
                }],
                error: None,
            },
            content_complete: true,
            tools_complete: true,
            notes: Vec::new(),
        }
    }

    /// Trace `ID` with invocations e-1 (a thumbs up) and e-2 (no score).
    fn trace_export() -> FakeExport {
        let mut contexts = HashMap::new();
        contexts.insert(
            "e-1".to_owned(),
            ExportContext::new(vec![RunScore::new(
                "thumbs",
                json!(true),
                "bool",
                "app",
                "u-1",
                fixed_now(),
            )]),
        );
        FakeExport {
            invocations: vec![
                TraceInvocation::new(ID, "e-1", fixed_now() - TimeDelta::seconds(120)),
                TraceInvocation::new(ID, "e-2", fixed_now() - TimeDelta::seconds(60)),
            ],
            runs: vec![
                trace_run("e-1", 120, "Where is order 7?", "It ships today."),
                trace_run("e-2", 60, "And order 8?", "Unknown."),
            ],
            contexts,
            ..FakeExport::default()
        }
    }

    /// `with_get(Ok(detail(600, …)))` with `--datasets dir` read through `fake`.
    fn with_datasets(dir: &std::path::Path, fake: &Arc<FakeExport>) -> AppState {
        let (_, mut st) = with_get(Ok(detail(600, logs(vec![], false))));
        st.datasets = Some(Arc::new(Datasets::new(dir.to_path_buf(), fake.clone())));
        st
    }

    fn dataset_path() -> String {
        format!("/traces/{ID}/dataset")
    }

    #[tokio::test]
    async fn adding_a_trace_creates_the_dataset_then_skips_what_it_holds() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Arc::new(trace_export());
        let body = format!("dataset=regressions&rubric=Be+right.&csrf={CSRF}");
        let (status, headers, page) = post_form(
            with_datasets(dir.path(), &fake),
            &dataset_path(),
            &body,
            Some(ORIGIN),
        )
        .await;
        assert_eq!(status, StatusCode::SEE_OTHER, "{page}");
        assert_eq!(
            headers[LOCATION],
            format!("/traces/{ID}?added=2&dataset=regressions")
        );
        let path = dir.path().join("regressions.yaml");
        let set = EvalSet::load(&path).unwrap();
        assert_eq!(set.name, "regressions");
        assert_eq!(set.judge_rubric.as_deref(), Some("Be right."));
        let cases: Vec<(&str, &str)> = set
            .cases
            .iter()
            .map(|c| (c.id.as_str(), c.expect[0].kind()))
            .collect();
        assert_eq!(cases, [("inv-e-1", "reference"), ("inv-e-2", "llm_judge")]);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(&format!("  # trace {ID}\n")), "{text}");

        let (status, _, page) = post_form(
            with_datasets(dir.path(), &fake),
            &dataset_path(),
            &body,
            Some(ORIGIN),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            page.contains("no case added: inv-e-1: already in regressions.yaml; inv-e-2: already in regressions.yaml"),
            "{page}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }

    #[tokio::test]
    async fn adding_appends_with_the_files_rubric() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("evals.yaml");
        let before = "name: evals\njudge_rubric: \"File rubric.\"\ncases:\n- id: kept\n  turns: [hi]\n  expect: [contains: x]\n";
        std::fs::write(&path, before).unwrap();
        let fake = Arc::new(trace_export());
        let body = format!("dataset=evals&rubric=+&csrf={CSRF}");
        let (status, headers, page) = post_form(
            with_datasets(dir.path(), &fake),
            &dataset_path(),
            &body,
            Some(ORIGIN),
        )
        .await;
        assert_eq!(status, StatusCode::SEE_OTHER, "{page}");
        assert_eq!(
            headers[LOCATION],
            format!("/traces/{ID}?added=2&dataset=evals")
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(before), "{text}");
        assert!(text.ends_with("    - llm_judge: {}\n"), "{text}");
        let set = EvalSet::load(&path).unwrap();
        assert_eq!(set.cases.len(), 3);
    }

    #[tokio::test]
    async fn bad_names_tokens_and_rubric_less_judges_add_nothing() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["", "a%20b", "..%2Fx", &"a".repeat(65)] {
            let fake = Arc::new(trace_export());
            let body = format!("dataset={name}&csrf={CSRF}");
            let (status, _, page) = post_form(
                with_datasets(dir.path(), &fake),
                &dataset_path(),
                &body,
                Some(ORIGIN),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{name}");
            assert!(page.contains("dataset: must be 1-64 of"), "{page}");
            assert_eq!(fake.calls.load(Ordering::SeqCst), 0, "{name}");
        }
        let fake = Arc::new(trace_export());
        let body = format!("dataset=x&csrf={}", "b".repeat(64));
        let (status, _, _) = post_form(
            with_datasets(dir.path(), &fake),
            &dataset_path(),
            &body,
            Some(ORIGIN),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let body = format!("dataset=x&csrf={CSRF}");
        let (status, _, _) = post_form(
            with_datasets(dir.path(), &fake),
            &dataset_path(),
            &body,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "no Origin");
        assert_eq!(fake.calls.load(Ordering::SeqCst), 0);

        let mut unscored = trace_export();
        unscored.contexts.clear();
        let body = format!("dataset=x&csrf={CSRF}");
        let (status, _, page) = post_form(
            with_datasets(dir.path(), &Arc::new(unscored)),
            &dataset_path(),
            &body,
            Some(ORIGIN),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            page.contains("no case added: inv-e-1: needs a rubric; inv-e-2: needs a rubric"),
            "{page}"
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn unknown_traces_and_a_console_without_datasets_are_404() {
        let dir = tempfile::tempdir().unwrap();
        let body = format!("dataset=x&csrf={CSRF}");
        let none = Arc::new(FakeExport::default());
        let (status, _, page) = post_form(
            with_datasets(dir.path(), &none),
            &dataset_path(),
            &body,
            Some(ORIGIN),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(page.contains("trace not found"), "{page}");
        let (status, _, _) = post_form(
            with_datasets(dir.path(), &none),
            "/traces/not-a-trace/dataset",
            &body,
            Some(ORIGIN),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (_, st) = with_get(Ok(detail(600, logs(vec![], false))));
        let (status, _, page) = post_form(st, &dataset_path(), &body, Some(ORIGIN)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            page.contains("Adding traces to a dataset needs --datasets"),
            "{page}"
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn source_failures_are_502_and_unappendable_files_400() {
        let dir = tempfile::tempdir().unwrap();
        let body = format!("dataset=evals&rubric=r&csrf={CSRF}");
        let mut failing = trace_export();
        failing.fail = Some("HTTP 403: Access Denied".into());
        let (status, _, page) = post_form(
            with_datasets(dir.path(), &Arc::new(failing)),
            &dataset_path(),
            &body,
            Some(ORIGIN),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(
            page.contains("transcript source: HTTP 403: Access Denied"),
            "{page}"
        );

        let path = dir.path().join("evals.yaml");
        let flow = "name: s\ncases: [{id: a, turns: [hi], expect: [contains: x]}]\n";
        std::fs::write(&path, flow).unwrap();
        let (status, _, page) = post_form(
            with_datasets(dir.path(), &Arc::new(trace_export())),
            &dataset_path(),
            &body,
            Some(ORIGIN),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            page.contains(
                "evals.yaml: eval set: cannot append: `cases:` must be the last top-level key"
            ),
            "{page}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), flow);
    }

    #[tokio::test]
    async fn an_unappendable_dataset_is_400_before_any_query() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("evals.yaml");
        let ok = "name: s\ncases:\n- id: a\n  turns: [hi]\n  expect: [contains: x]\n";
        for (text, message) in [
            (
                "name: s\ncases: [{id: a, turns: [hi], expect: [contains: x]}]\n".to_owned(),
                "evals.yaml: eval set: cannot append: `cases:` must be the last top-level key",
            ),
            (
                format!("{ok}...\n"),
                "evals.yaml: eval set: cannot append: the file has a `...` document-end marker",
            ),
            ("name: [\n".to_owned(), "evals.yaml: "),
        ] {
            std::fs::write(&path, &text).unwrap();
            let fake = Arc::new(trace_export());
            let body = format!("dataset=evals&rubric=r&csrf={CSRF}");
            let (status, _, page) = post_form(
                with_datasets(dir.path(), &fake),
                &dataset_path(),
                &body,
                Some(ORIGIN),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{text}: {page}");
            assert!(page.contains(message), "{page}");
            assert_eq!(fake.calls.load(Ordering::SeqCst), 0, "{text}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        }
    }

    #[tokio::test]
    async fn the_dataset_form_offers_the_existing_datasets() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "b.yaml",
            "a.yaml",
            "notes.txt",
            "bad name.yaml",
            ".a.yaml.1.2.tmp",
        ] {
            std::fs::write(dir.path().join(name), "x").unwrap();
        }
        let fake = Arc::new(FakeExport::default());
        let (status, _, page) =
            get(with_datasets(dir.path(), &fake), &format!("/traces/{ID}")).await;
        assert_eq!(status, StatusCode::OK);
        let form = format!(
            r#"<h2>Add to dataset</h2><form method="post" action="/traces/{ID}/dataset" data-confirm="Add this trace's runs to the dataset?" class="inline"><input type="hidden" name="csrf" value="{CSRF}"><input type="text" name="dataset" list="datasets" placeholder="dataset" required maxlength="64"><datalist id="datasets"><option value="a"></option><option value="b"></option></datalist> <input type="text" name="rubric" placeholder="judge rubric (optional)" maxlength="2000"> <button type="submit">Add to dataset</button></form>"#
        );
        assert!(page.contains(&form), "{page}");
        let scores = page.find("<h2>Scores</h2>").unwrap();
        let add = page.find("<h2>Add to dataset</h2>").unwrap();
        assert!(scores < add, "the form is below the scores");
        assert_eq!(fake.calls.load(Ordering::SeqCst), 0, "a GET reads no runs");

        let (_, page) = trace_page(detail(600, logs(vec![], false))).await;
        assert!(
            !page.contains("Add to dataset"),
            "no form without --datasets: {page}"
        );
    }

    #[tokio::test]
    async fn the_page_names_the_cases_just_added() {
        let (_, st) = with_get(Ok(detail(600, logs(vec![], false))));
        let (status, _, page) = get(st, &format!("/traces/{ID}?added=2&dataset=regressions")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            page.contains(
                r#"<div class="banner info"><p>Added 2 case(s) to regressions.yaml</p></div>"#
            ),
            "{page}"
        );
        for query in [
            "added=2&dataset=%3Cscript%3E",
            "added=x&dataset=a",
            "dataset=a",
            "added=1",
            "added=1&added=2&dataset=a",
            "added=1&dataset=a&dataset=b",
        ] {
            let (_, st) = with_get(Ok(detail(600, logs(vec![], false))));
            let (status, _, page) = get(st, &format!("/traces/{ID}?{query}")).await;
            assert_eq!(status, StatusCode::OK, "{query}");
            assert!(!page.contains("case(s) to"), "{query}: {page}");
            assert!(!page.contains("<script>"), "{query}");
        }
    }

    // ---- adding to a dataset in review mode ----

    /// The review fields of the queue `quality`, prompt `team/greet` v3, flagged, 6h, at `AT`.
    const REVIEW_FIELDS: &str = "&q_score=quality&q_prompt=team%2Fgreet&q_version=3&q_flagged=1&q_range=6h&q_at=2026-11-16T11%3A58%3A59.999750Z";

    /// The `Add to dataset` heading through the end of its form.
    fn dataset_section(page: &str) -> &str {
        let start = page
            .find("<h2>Add to dataset</h2>")
            .unwrap_or_else(|| panic!("no dataset section: {page}"));
        let len = page[start..].find("</form>").expect("form end");
        &page[start..start + len]
    }

    /// POST `dataset=regressions` plus `review` to a fresh `--datasets` directory; returns the
    /// status and the `Location` header (empty when there is none).
    async fn add_regressions(review: &str) -> (StatusCode, String) {
        let dir = tempfile::tempdir().unwrap();
        let fake = Arc::new(trace_export());
        let body = format!("dataset=regressions&rubric=Be+right.&csrf={CSRF}{review}");
        let (status, headers, page) = post_form(
            with_datasets(dir.path(), &fake),
            &dataset_path(),
            &body,
            Some(ORIGIN),
        )
        .await;
        let location = headers
            .get(LOCATION)
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        assert!(
            status != StatusCode::SEE_OTHER || !location.is_empty(),
            "{page}"
        );
        (status, location)
    }

    #[tokio::test]
    async fn in_review_mode_the_dataset_form_carries_the_review_fields() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeHttp::new(vec![reply(&SCORE_FIELDS, vec![], false)]);
        let mut st = bq_get(invocation_detail(), &fake);
        st.datasets = Some(Arc::new(Datasets::new(
            dir.path().to_path_buf(),
            Arc::new(FakeExport::default()),
        )));
        let (status, _, page) = get(
            st,
            &format!(
                "/traces/{ID}?score=quality&prompt=triage&version=2&flagged=1&range=24h&at={AT}"
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{page}");
        let fields = format!(
            r#"<input type="hidden" name="csrf" value="{CSRF}"><input type="hidden" name="q_score" value="quality"><input type="hidden" name="q_prompt" value="triage"><input type="hidden" name="q_version" value="2"><input type="hidden" name="q_flagged" value="1"><input type="hidden" name="q_range" value="24h"><input type="hidden" name="q_at" value="2026-11-16T11:58:59.999750Z"><input type="text" name="dataset""#
        );
        let section = dataset_section(&page);
        assert!(section.contains(&fields), "{section}");
        let scores = scores_section(&page);
        let hidden = |s: &str| {
            s[s.find("name=\"q_score\"").unwrap()..s.find("name=\"q_at\"").unwrap()].to_owned()
        };
        assert_eq!(
            hidden(section),
            hidden(scores),
            "the same fields as the score form"
        );

        let dir = tempfile::tempdir().unwrap();
        let (_, _, page) = get(
            with_datasets(dir.path(), &Arc::new(FakeExport::default())),
            &format!("/traces/{ID}"),
        )
        .await;
        let section = dataset_section(&page);
        assert!(!section.contains("name=\"q_"), "{section}");
    }

    #[tokio::test]
    async fn a_dataset_add_in_review_mode_redirects_with_the_queue_and_at() {
        let (status, location) = add_regressions(REVIEW_FIELDS).await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(
            location,
            format!(
                "/traces/{ID}?added=2&dataset=regressions&score=quality&prompt=team%2Fgreet&version=3&flagged=1&range=6h&at={AT}"
            )
        );
    }

    #[tokio::test]
    async fn a_dataset_add_without_valid_review_fields_redirects_to_the_plain_trace() {
        for review in [
            "",
            "&q_score=quality",
            "&q_score=quality&q_at=yesterday",
            "&q_score=&q_at=2026-11-16T11%3A58%3A59.999750Z",
            "&q_score=quality&q_version=2&q_at=2026-11-16T11%3A58%3A59.999750Z",
            "&q_score=quality&q_range=30d&q_at=2026-11-16T11%3A58%3A59.999750Z",
        ] {
            let (status, location) = add_regressions(review).await;
            assert_eq!(status, StatusCode::SEE_OTHER, "{review}");
            assert_eq!(
                location,
                format!("/traces/{ID}?added=2&dataset=regressions"),
                "{review}"
            );
        }
    }

    #[tokio::test]
    async fn review_fields_do_not_change_dataset_errors() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Arc::new(trace_export());
        let body = format!("dataset=a%20b&csrf={CSRF}{REVIEW_FIELDS}");
        let (status, headers, page) = post_form(
            with_datasets(dir.path(), &fake),
            &dataset_path(),
            &body,
            Some(ORIGIN),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(headers.get(LOCATION).is_none());
        assert!(page.contains("dataset: must be 1-64 of"), "{page}");
        let body = format!("dataset=x&csrf={}{REVIEW_FIELDS}", "b".repeat(64));
        let (status, _, _) = post_form(
            with_datasets(dir.path(), &fake),
            &dataset_path(),
            &body,
            Some(ORIGIN),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn the_page_after_a_review_mode_add_shows_the_notice_and_the_bar() {
        let (_, location) =
            add_regressions("&q_score=quality&q_at=2026-11-16T11%3A58%3A59.999750Z").await;
        assert_eq!(
            location,
            format!("/traces/{ID}?added=2&dataset=regressions&score=quality&at={AT}")
        );
        let fake = FakeHttp::new(vec![reply(&SCORE_FIELDS, vec![], false)]);
        let (status, _, page) = get(bq_get(invocation_detail(), &fake), &location).await;
        assert_eq!(status, StatusCode::OK, "{page}");
        assert!(
            page.contains(
                r#"<div class="banner info"><p>Added 2 case(s) to regressions.yaml</p></div>"#
            ),
            "{page}"
        );
        assert!(
            page.contains("Reviewing <strong>quality</strong>"),
            "{page}"
        );
    }

    // ---- rubrics ----

    /// [`bq_get`] with the test rubrics loaded.
    fn rubric_get(d: TraceDetail, fake: &Arc<FakeHttp>) -> AppState {
        let mut st = bq_get(d, fake);
        st.rubrics = Some(test_rubrics());
        st
    }

    /// GET `/traces/{ID}{query}` with the test rubrics loaded and the BigQuery source answering
    /// the scores query with no rows; returns the page.
    async fn rubric_page(query: &str) -> String {
        let fake = FakeHttp::new(vec![reply(&SCORE_FIELDS, vec![], false)]);
        let (status, _, page) = get(
            rubric_get(invocation_detail(), &fake),
            &format!("/traces/{ID}{query}"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{query}: {page}");
        page
    }

    /// The `support` rubric form's fields after its hidden fields: the description, the two
    /// criteria and the comment and submit button.
    const SUPPORT_FIELDS: &str = r#"<p>Answers to support questions</p><fieldset><legend>accuracy — Is the answer right?</legend><label><input type="radio" name="c_accuracy" value="1" required>1</label><label><input type="radio" name="c_accuracy" value="2" required>2</label><label><input type="radio" name="c_accuracy" value="3" required>3</label><label><input type="radio" name="c_accuracy" value="4" required>4</label><label><input type="radio" name="c_accuracy" value="5" required>5</label></fieldset><fieldset><legend>tone</legend><label><input type="radio" name="c_tone" value="0" required>0</label><label><input type="radio" name="c_tone" value="1" required>1</label><label><input type="radio" name="c_tone" value="2" required>2</label></fieldset><input type="text" name="comment" placeholder="comment (optional)" maxlength="1000"> <button type="submit">Save scores</button></form>"#;

    fn rubric_form_open() -> String {
        format!(
            r#"<form method="post" action="/traces/{ID}/rubric" data-confirm="Save these scores to the trace?" class="rubric"><input type="hidden" name="csrf" value="{CSRF}">"#
        )
    }

    #[tokio::test]
    async fn outside_review_mode_each_rubric_is_a_collapsed_form_after_the_score_form() {
        let page = rubric_page("").await;
        let scores = scores_section(&page);
        let open = rubric_form_open();
        let support = format!(
            r#"<details><summary>Score with rubric support</summary>{open}<input type="hidden" name="rubric" value="support">{SUPPORT_FIELDS}</details>"#
        );
        let safety = format!(
            r#"<details><summary>Score with rubric safety</summary>{open}<input type="hidden" name="rubric" value="safety"><fieldset><legend>harmless</legend><label><input type="radio" name="c_harmless" value="1" required>1</label><label><input type="radio" name="c_harmless" value="2" required>2</label><label><input type="radio" name="c_harmless" value="3" required>3</label><label><input type="radio" name="c_harmless" value="4" required>4</label><label><input type="radio" name="c_harmless" value="5" required>5</label></fieldset><input type="text" name="comment" placeholder="comment (optional)" maxlength="1000"> <button type="submit">Save scores</button></form></details>"#
        );
        assert!(
            scores.contains(&format!(
                r#"<button type="submit">Add score</button></form>{support}{safety}"#
            )),
            "{scores}"
        );
        assert!(!scores.contains("name=\"q_"), "{scores}");
    }

    #[tokio::test]
    async fn a_rubric_review_shows_its_bar_and_form_above_a_blank_score_form() {
        let page = rubric_page(&format!("?rubric=support&prompt=triage&range=24h&at={AT}")).await;
        let qs = "rubric=support&amp;prompt=triage&amp;range=24h";
        let bar = format!(
            r#"<div class="banner info"><p>Reviewing rubric <strong>support</strong> · <a href="/review/next?{qs}&amp;after=2026-11-16T11%3A58%3A59.999750Z&amp;after_id={ID}">Skip →</a> · <a href="/review?{qs}">Back to queue</a></p></div><h1>Trace "#
        );
        assert!(page.contains(&bar), "{page}");
        let scores = scores_section(&page);
        let hidden = r#"<input type="hidden" name="q_rubric" value="support"><input type="hidden" name="q_prompt" value="triage"><input type="hidden" name="q_version" value=""><input type="hidden" name="q_flagged" value=""><input type="hidden" name="q_range" value="24h"><input type="hidden" name="q_at" value="2026-11-16T11:58:59.999750Z">"#;
        let form = format!(
            r#"{}<input type="hidden" name="rubric" value="support">{hidden}{SUPPORT_FIELDS}<form method="post" action="/traces/{ID}/scores""#,
            rubric_form_open()
        );
        assert!(scores.contains(&form), "{scores}");
        assert!(
            scores.contains(&format!(
                r#"<input type="hidden" name="csrf" value="{CSRF}">{hidden}<input type="text" name="name" placeholder="score name" required maxlength="100">"#
            )),
            "the score form keeps the queue with a blank name: {scores}"
        );
        assert!(!scores.contains("q_score"), "{scores}");
        assert!(!scores.contains("<details>"), "{scores}");
        assert!(!scores.contains(r#"value="safety""#), "{scores}");
    }

    #[tokio::test]
    async fn a_score_review_with_rubrics_loaded_has_no_rubric_forms() {
        let page = rubric_page(&format!("?score=quality&at={AT}")).await;
        assert!(
            page.contains("Reviewing <strong>quality</strong>"),
            "{page}"
        );
        let scores = scores_section(&page);
        assert!(!scores.contains("/rubric\""), "{scores}");
        assert!(!scores.contains("<details>"), "{scores}");
        assert!(
            scores.contains(r#"<input type="text" name="name" placeholder="score name" value="quality" required maxlength="100">"#),
            "{scores}"
        );
    }

    #[tokio::test]
    async fn without_rubrics_a_rubric_queue_is_no_review_and_the_page_is_unchanged() {
        let plain = review_page("").await;
        assert!(!plain.contains("/rubric\""), "{plain}");
        assert!(!plain.contains("<details>"), "{plain}");
        assert_eq!(
            review_page(&format!("?rubric=support&at={AT}")).await,
            plain
        );
        // An unknown rubric or a rubric with a score is no review either.
        let plain = rubric_page("").await;
        for query in [
            format!("?rubric=nope&at={AT}"),
            format!("?rubric=support&score=quality&at={AT}"),
        ] {
            assert_eq!(rubric_page(&query).await, plain, "{query}");
        }
    }

    #[tokio::test]
    async fn rubric_forms_need_the_bigquery_source() {
        let (_, mut st) = with_get(Ok(detail(600, logs(vec![], false))));
        st.rubrics = Some(test_rubrics());
        let (status, _, page) = get(st, &format!("/traces/{ID}")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            page.contains("Scores need the BigQuery trace source"),
            "{page}"
        );
        assert!(!page.contains("/rubric\""), "{page}");
        assert!(!page.contains("<details>"), "{page}");
    }

    fn rubric_path() -> String {
        format!("/traces/{ID}/rubric")
    }

    #[tokio::test]
    async fn a_rubric_submit_inserts_one_row_per_criterion_in_one_statement() {
        let fake = FakeHttp::new(vec![root_reply(), dml_reply()]);
        let body =
            format!("csrf={CSRF}&rubric=support&c_tone=2&c_accuracy=4&comment=clear%20and%20kind");
        let st = rubric_get(invocation_detail(), &fake);
        let (status, headers, page) = post_form(st, &rubric_path(), &body, Some(ORIGIN)).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "{page}");
        assert_eq!(headers[LOCATION], format!("/traces/{ID}"));
        assert_eq!(fake.uris().len(), 2, "the identity, then one insert");
        let insert = fake.body(1);
        let sql = insert["query"].as_str().unwrap();
        assert!(
            sql.starts_with("INSERT INTO `data-proj.agent_analytics.agent_events`"),
            "{sql}"
        );
        assert!(sql.ends_with("FROM UNNEST(@contents) AS c"), "{sql}");
        assert_eq!(insert["maximumBytesBilled"], json!("10485760"));
        assert_eq!(
            param_value(&insert, "contents")["parameterValue"],
            json!({ "arrayValues": [
                { "value": r#"{"comment":"clear and kind","data_type":"number","name":"support/accuracy","value":4.0}"# },
                { "value": r#"{"comment":"clear and kind","data_type":"number","name":"support/tone","value":2.0}"# },
            ] })
        );
        let attributes: Value = serde_json::from_str(
            param_value(&insert, "attributes")["parameterValue"]["value"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            attributes,
            json!({"source": "console", "author": AUTHOR, "adk": {"app_name": "shop", "invocation_id": "e-1"}})
        );
        for (name, want) in [("trace_id", ID), ("session_id", "s-1"), ("user_id", "u-1")] {
            assert_eq!(
                param_value(&insert, name)["parameterValue"]["value"],
                json!(want),
                "{name}"
            );
        }
    }

    #[tokio::test]
    async fn a_rubric_submit_in_review_mode_redirects_to_the_next_trace() {
        let fake = FakeHttp::new(vec![root_reply(), dml_reply()]);
        let body = format!(
            "csrf={CSRF}&rubric=support&c_accuracy=5&c_tone=0&comment=&q_score=&q_rubric=support&q_prompt=team%2Fgreet&q_version=3&q_flagged=1&q_range=6h&q_at=2026-11-16T11%3A58%3A59.999750Z"
        );
        let st = rubric_get(invocation_detail(), &fake);
        let (status, headers, page) = post_form(st, &rubric_path(), &body, Some(ORIGIN)).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "{page}");
        assert_eq!(
            headers[LOCATION],
            format!(
                "/review/next?rubric=support&prompt=team%2Fgreet&version=3&flagged=1&range=6h&after={AT}&after_id={ID}"
            )
        );
        assert_eq!(fake.uris().len(), 2);
        let contents = param_value(&fake.body(1), "contents");
        assert_eq!(
            contents["parameterValue"]["arrayValues"][1]["value"],
            json!(r#"{"data_type":"number","name":"support/tone","value":0.0}"#),
            "a blank comment is left out"
        );

        // Invalid review fields go back to the trace.
        let fake = FakeHttp::new(vec![root_reply(), dml_reply()]);
        let body = format!(
            "csrf={CSRF}&rubric=support&c_accuracy=5&c_tone=0&q_rubric=nope&q_at=2026-11-16T11%3A58%3A59.999750Z"
        );
        let st = rubric_get(invocation_detail(), &fake);
        let (status, headers, _) = post_form(st, &rubric_path(), &body, Some(ORIGIN)).await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(headers[LOCATION], format!("/traces/{ID}"));
    }

    #[tokio::test]
    async fn rubric_posts_without_the_token_are_403_and_query_nothing() {
        for csrf in [String::new(), "b".repeat(64)] {
            let fake = FakeHttp::new(vec![]);
            let body = format!("csrf={csrf}&rubric=nope");
            let st = rubric_get(invocation_detail(), &fake);
            let (status, _, _) = post_form(st, &rubric_path(), &body, Some(ORIGIN)).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{csrf:?}");
            assert!(fake.uris().is_empty());
        }
        let fake = FakeHttp::new(vec![]);
        let body = format!("csrf={CSRF}&rubric=support&c_accuracy=1&c_tone=1");
        let st = rubric_get(invocation_detail(), &fake);
        let (status, _, _) = post_form(st, &rubric_path(), &body, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "no Origin");
        assert!(fake.uris().is_empty());
    }

    #[tokio::test]
    async fn a_bad_id_no_bigquery_or_no_rubrics_is_404_and_queries_nothing() {
        let body = format!("csrf={CSRF}&rubric=nope");
        let fake = FakeHttp::new(vec![]);
        let st = rubric_get(invocation_detail(), &fake);
        let (status, _, page) =
            post_form(st, "/traces/not-a-trace/rubric", &body, Some(ORIGIN)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(page.contains("trace not found"), "{page}");
        assert!(fake.uris().is_empty());

        let (_, mut st) = with_get(Ok(detail(600, logs(vec![], false))));
        st.rubrics = Some(test_rubrics());
        let (status, _, page) = post_form(st, &rubric_path(), &body, Some(ORIGIN)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            page.contains("Scores need the BigQuery trace source"),
            "{page}"
        );

        let fake = FakeHttp::new(vec![]);
        let st = bq_get(invocation_detail(), &fake);
        let (status, _, page) = post_form(st, &rubric_path(), &body, Some(ORIGIN)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            page.contains("no rubrics are loaded (start the console with --rubrics &lt;file&gt;)"),
            "{page}"
        );
        assert!(fake.uris().is_empty());
    }

    #[tokio::test]
    async fn invalid_rubric_submits_are_400_and_query_nothing() {
        for (form, text) in [
            ("rubric=nope&c_accuracy=1", "rubric: unknown rubric `nope`"),
            ("c_accuracy=1&c_tone=1", "rubric: unknown rubric ``"),
            (
                "rubric=Support&c_accuracy=1&c_tone=1",
                "rubric: unknown rubric `Support`",
            ),
            ("rubric=support&c_tone=1", "`accuracy`: choose a score"),
            (
                "rubric=support&c_accuracy=+&c_tone=1",
                "`accuracy`: choose a score",
            ),
            ("rubric=support&c_accuracy=3", "`tone`: choose a score"),
            (
                "rubric=support&c_accuracy=6&c_tone=9",
                "`accuracy`: must be an integer from 1 to 5",
            ),
            (
                "rubric=support&c_accuracy=2.5&c_tone=1",
                "`accuracy`: must be an integer from 1 to 5",
            ),
            (
                "rubric=support&c_accuracy=2&c_tone=3",
                "`tone`: must be an integer from 0 to 2",
            ),
        ] {
            let fake = FakeHttp::new(vec![]);
            let body = format!("csrf={CSRF}&{form}");
            let st = rubric_get(invocation_detail(), &fake);
            let (status, _, page) = post_form(st, &rubric_path(), &body, Some(ORIGIN)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{form}");
            assert!(page.contains(text), "{form}: {page}");
            assert!(fake.uris().is_empty(), "{form}");
        }
    }

    #[tokio::test]
    async fn a_rubric_submit_for_a_trace_without_an_identity_inserts_nothing() {
        let fake = FakeHttp::new(vec![reply(&ROOT_FIELDS, vec![], false)]);
        let body = format!("csrf={CSRF}&rubric=safety&c_harmless=5");
        let st = rubric_get(invocation_detail(), &fake);
        let (status, _, page) = post_form(st, &rubric_path(), &body, Some(ORIGIN)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(page.contains("trace not found"), "{page}");
        assert_eq!(fake.uris().len(), 1, "no insert");

        let fake = FakeHttp::new(vec![root_reply(), denied()]);
        let st = rubric_get(invocation_detail(), &fake);
        let (status, _, page) = post_form(st, &rubric_path(), &body, Some(ORIGIN)).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(
            page.contains(r#"<p class="hint">needs roles/bigquery.dataEditor on the table and roles/bigquery.jobUser</p>"#),
            "{page}"
        );
    }

    #[tokio::test]
    async fn a_score_in_a_rubric_review_redirects_to_the_rubric_queue() {
        let fake = FakeHttp::new(vec![root_reply(), dml_reply()]);
        let body = format!(
            "name=extra&kind=up&csrf={CSRF}&q_score=&q_rubric=support&q_range=7d&q_at=2026-11-16T11%3A58%3A59.999750Z"
        );
        let st = rubric_get(invocation_detail(), &fake);
        let (status, headers, page) = post_form(st, &score_path(), &body, Some(ORIGIN)).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "{page}");
        assert_eq!(
            headers[LOCATION],
            format!("/review/next?rubric=support&after={AT}&after_id={ID}")
        );
    }

    #[tokio::test]
    async fn a_dataset_add_in_a_rubric_review_keeps_the_rubric_queue() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Arc::new(trace_export());
        let mut st = with_datasets(dir.path(), &fake);
        st.rubrics = Some(test_rubrics());
        let body = format!(
            "dataset=regressions&rubric=Be+right.&csrf={CSRF}&q_rubric=support&q_range=6h&q_at=2026-11-16T11%3A58%3A59.999750Z"
        );
        let (status, headers, page) = post_form(st, &dataset_path(), &body, Some(ORIGIN)).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "{page}");
        assert_eq!(
            headers[LOCATION],
            format!("/traces/{ID}?added=2&dataset=regressions&rubric=support&range=6h&at={AT}")
        );
        let text = std::fs::read_to_string(dir.path().join("regressions.yaml")).unwrap();
        assert!(
            text.contains("Be right."),
            "the judge rubric is the dataset's: {text}"
        );
    }
}
