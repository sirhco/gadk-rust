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

//! Review routes: `/review` (the queue) and `/review/next` (the trace after a cursor).

use axum::Router;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use chrono::{DateTime, Utc};
use gadk_analytics::review::{Cursor, at_text};
use http::StatusCode;

use super::AppState;
use super::pages::error::{error_page, source_error_in};
use super::pages::layout::{Nav, html};
use super::pages::review;
use crate::review::{NEEDS_BIGQUERY_REVIEW, ReviewQueue};
use crate::url::percent_encode;

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/review", get(list))
        .route("/review/next", get(next))
}

/// `/traces/{id}?{queue}&at={first}`: trace `trace_id` in review mode.
pub(crate) fn review_link(q: &ReviewQueue, trace_id: &str, first: DateTime<Utc>) -> String {
    format!(
        "/traces/{}?{}&at={}",
        percent_encode(trace_id),
        q.query_string(),
        percent_encode(&at_text(first))
    )
}

/// `/review/next?{queue}&after={at}&after_id={id}`: the trace that comes after `after`.
pub(crate) fn next_link(q: &ReviewQueue, after: &Cursor) -> String {
    format!(
        "/review/next?{}&after={}&after_id={}",
        q.query_string(),
        percent_encode(&at_text(after.at)),
        percent_encode(&after.trace_id)
    )
}

fn needs_bigquery() -> Response {
    error_page(
        StatusCode::NOT_FOUND,
        Nav::Review,
        NEEDS_BIGQUERY_REVIEW,
        None,
    )
}

fn bad_request(text: &str) -> Response {
    error_page(StatusCode::BAD_REQUEST, Nav::Review, text, None)
}

/// The first value of URL parameter `key` (empty when absent).
pub(super) fn first<'a>(params: &'a [(String, String)], key: &str) -> &'a str {
    params
        .iter()
        .find(|(k, _)| k == key)
        .map_or("", |(_, v)| v.as_str())
}

/// `GET /review`: the form alone without a score or rubric, else the queue's pending traces
/// (`done=1` marks a finished review). An invalid queue is a 400 page, a failed query a 502
/// page.
async fn list(
    State(state): State<AppState>,
    Query(params): Query<Vec<(String, String)>>,
) -> Response {
    let Some(usage) = &state.usage else {
        return needs_bigquery();
    };
    let rubrics = state.rubrics.as_deref();
    let queue = match ReviewQueue::from_params(&params, rubrics) {
        Ok(Some(q)) => q,
        Ok(None) => return html(StatusCode::OK, review::render(None, None, false, rubrics)),
        Err(text) => return bad_request(&text),
    };
    match usage
        .pending(&queue.analytics(), &queue.names(rubrics), (state.now)())
        .await
    {
        Ok(rows) => html(
            StatusCode::OK,
            review::render(
                Some(&queue),
                Some(&rows),
                first(&params, "done") == "1",
                rubrics,
            ),
        ),
        Err(e) => source_error_in(Nav::Review, &e),
    }
}

/// `GET /review/next`: 303 to the review link of the pending trace after `(after, after_id)`,
/// or to `/review?{queue}&done=1` when none is left. An invalid queue (or none), `after` or
/// `after_id` is a 400 page, a failed query a 502 page.
async fn next(
    State(state): State<AppState>,
    Query(params): Query<Vec<(String, String)>>,
) -> Response {
    let Some(usage) = &state.usage else {
        return needs_bigquery();
    };
    let rubrics = state.rubrics.as_deref();
    let queue = match ReviewQueue::from_params(&params, rubrics) {
        Ok(Some(q)) => q,
        Ok(None) => return bad_request("score: required"),
        Err(text) => return bad_request(&text),
    };
    let after = match Cursor::parse(first(&params, "after"), first(&params, "after_id")) {
        Ok(c) => c,
        Err(text) => return bad_request(&text),
    };
    match usage
        .next_pending(
            &queue.analytics(),
            &queue.names(rubrics),
            (state.now)(),
            &after,
        )
        .await
    {
        Ok(Some(t)) => Redirect::to(&review_link(&queue, &t.trace_id, t.first)).into_response(),
        Ok(None) => {
            Redirect::to(&format!("/review?{}&done=1", queue.query_string())).into_response()
        }
        Err(e) => source_error_in(Nav::Review, &e),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::{DateTime, TimeDelta, Utc};
    use gadk_bigquery::{BigQueryClient, NoAuth};
    use http::StatusCode;
    use http::header::LOCATION;
    use serde_json::json;

    use super::{next_link, review_link};
    use crate::review::ReviewQueue;
    use crate::traces::BigQueryTable;
    use crate::web::AppState;
    use crate::web::testutil::{FakeSource, fixed_now, get, state, test_rubrics};
    use gadk_analytics::AnalyticsSource;
    use gadk_analytics::review::Cursor;
    use gadk_analytics::testutil::{FakeHttp, param_value, pending_cells, pending_reply};

    const ACTIVE: &str = r#"<a class="active" href="/review">Review</a>"#;
    const A: &str = "0123456789abcdef0123456789abcdef";
    const B: &str = "fedcba9876543210fedcba9876543210";

    fn bq_state(fake: &Arc<FakeHttp>) -> AppState {
        let mut st = state(Arc::new(FakeSource::new()), None);
        st.usage = Some(Arc::new(AnalyticsSource::new(
            BigQueryClient::with_client(fake.clone(), Arc::new(NoAuth)),
            "billing-proj",
            BigQueryTable::new("data-proj", "agent_analytics", "agent_events"),
            1_000,
        )));
        st
    }

    /// `fixed_now` minus `secs` seconds and 250 microseconds.
    fn ago(secs: i64) -> DateTime<Utc> {
        fixed_now() - TimeDelta::seconds(secs) - TimeDelta::microseconds(250)
    }

    fn queue(pairs: &[(&str, &str)]) -> ReviewQueue {
        let params: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        ReviewQueue::from_params(&params, None).unwrap().unwrap()
    }

    fn denied() -> Result<(u16, String), String> {
        Ok((
            403,
            json!({ "error": { "code": 403, "message": "Access Denied", "errors": [{ "reason": "accessDenied" }] } }).to_string(),
        ))
    }

    #[test]
    fn links_carry_the_queue_and_the_cursor_percent_encoded() {
        let q = queue(&[("score", "a&b"), ("prompt", "team/greet"), ("range", "24h")]);
        assert_eq!(
            review_link(&q, A, ago(60)),
            format!(
                "/traces/{A}?score=a%26b&prompt=team%2Fgreet&range=24h&at=2026-11-16T11%3A58%3A59.999750Z"
            )
        );
        let after = Cursor::new(ago(60), B.into());
        assert_eq!(
            next_link(&q, &after),
            format!(
                "/review/next?score=a%26b&prompt=team%2Fgreet&range=24h&after=2026-11-16T11%3A58%3A59.999750Z&after_id={B}"
            )
        );
    }

    #[tokio::test]
    async fn without_a_score_the_page_is_the_form_and_queries_nothing() {
        let fake = FakeHttp::new(vec![]);
        let (status, _, body) = get(bq_state(&fake), "/review").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains(ACTIVE), "{body}");
        assert!(body.contains("<h1>Review</h1>"), "{body}");
        for piece in [
            r#"<form class="filters" method="get" action="/review">"#,
            r#"<input type="text" name="score" required maxlength="100">"#,
            r#"<input type="text" name="prompt">"#,
            r#"<input type="number" name="version" min="1">"#,
            r#"<input type="checkbox" name="flagged" value="1">"#,
            r#"<option value="7d" selected>7d</option>"#,
            r#"<button type="submit">Show queue</button>"#,
            "Enter a score name",
        ] {
            assert!(body.contains(piece), "{piece}: {body}");
        }
        assert!(fake.uris().is_empty());
    }

    #[tokio::test]
    async fn a_queue_lists_its_pending_traces_with_links_prompts_and_thumbs_down() {
        let fake = FakeHttp::new(vec![pending_reply(vec![
            pending_cells(A, ago(60), &["<b>&\"x@1", "triage@2"], 2),
            pending_cells(B, ago(120), &[], 0),
        ])]);
        let path = "/review?score=quality&prompt=triage&version=2&flagged=1&range=24h";
        let (status, _, body) = get(bq_state(&fake), path).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains(ACTIVE), "{body}");
        assert!(body.contains("<h1>Review: quality</h1>"), "{body}");
        assert!(body.contains("<p>2 pending</p>"), "{body}");
        let qs = "score=quality&amp;prompt=triage&amp;version=2&amp;flagged=1&amp;range=24h";
        assert!(
            body.contains(&format!(
                r#"<p><a href="/traces/{A}?{qs}&amp;at=2026-11-16T11%3A58%3A59.999750Z">Start reviewing</a></p>"#
            )),
            "{body}"
        );
        for row in [
            format!(
                r#"<tr><td class="mono">2026-11-16T11:58:59.999Z</td><td class="mono"><a href="/traces/{A}?{qs}&amp;at=2026-11-16T11%3A58%3A59.999750Z">{A}</a></td><td>&lt;b&gt;&amp;&quot;x@1, triage@2</td><td class="num">2</td></tr>"#
            ),
            format!(
                r#"<tr><td class="mono">2026-11-16T11:57:59.999Z</td><td class="mono"><a href="/traces/{B}?{qs}&amp;at=2026-11-16T11%3A57%3A59.999750Z">{B}</a></td><td></td><td class="num">0</td></tr>"#
            ),
        ] {
            assert!(body.contains(&row), "{row}\n{body}");
        }
        // The form keeps the queue.
        for piece in [
            r#"<input type="text" name="score" value="quality" required maxlength="100">"#,
            r#"<input type="text" name="prompt" value="triage">"#,
            r#"<input type="number" name="version" min="1" value="2">"#,
            r#"<input type="checkbox" name="flagged" value="1" checked>"#,
            r#"<option value="24h" selected>24h</option>"#,
        ] {
            assert!(body.contains(piece), "{piece}: {body}");
        }
        let sent = fake.body(0);
        assert_eq!(
            param_value(&sent, "start")["parameterValue"]["value"],
            json!("2026-11-15 12:00:00.000000+00:00")
        );
        assert_eq!(
            param_value(&sent, "flagged")["parameterValue"]["value"],
            json!("true")
        );
    }

    #[tokio::test]
    async fn more_than_the_cap_shows_the_first_hundred() {
        let rows = (0..101)
            .map(|i| pending_cells(&format!("{i:032x}"), ago(i), &[], 0))
            .collect();
        let fake = FakeHttp::new(vec![pending_reply(rows)]);
        let (status, _, body) = get(bq_state(&fake), "/review?score=q").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("<p>100+ pending (showing 100)</p>"), "{body}");
        assert_eq!(body.matches("<tr><td").count(), 100, "{body}");
        assert!(!body.contains(&format!("{:032x}", 100)), "{body}");
    }

    #[tokio::test]
    async fn an_empty_queue_says_so_and_caught_up_only_when_done() {
        let fake = FakeHttp::new(vec![pending_reply(vec![])]);
        let (_, _, body) = get(bq_state(&fake), "/review?score=q").await;
        assert!(
            body.contains(r#"<p class="empty">Queue empty</p>"#),
            "{body}"
        );

        let fake = FakeHttp::new(vec![pending_reply(vec![])]);
        let (_, _, body) = get(bq_state(&fake), "/review?score=q&done=1").await;
        assert!(
            body.contains(r#"<p class="empty">Queue empty — all caught up</p>"#),
            "{body}"
        );

        let fake = FakeHttp::new(vec![pending_reply(vec![pending_cells(A, ago(60), &[], 0)])]);
        let (_, _, body) = get(bq_state(&fake), "/review?score=q&done=1").await;
        assert!(!body.contains("Queue empty"), "{body}");
        assert!(body.contains("<p>1 pending</p>"), "{body}");
    }

    #[tokio::test]
    async fn hostile_queue_values_are_escaped_and_percent_encoded() {
        let fake = FakeHttp::new(vec![pending_reply(vec![pending_cells(A, ago(60), &[], 0)])]);
        let path = "/review?score=%3Cscript%3E%26%22&prompt=%22%3E%3Cscript%3E";
        let (status, _, body) = get(bq_state(&fake), path).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(!body.contains("<script>"), "{body}");
        assert!(
            body.contains("<h1>Review: &lt;script&gt;&amp;&quot;</h1>"),
            "{body}"
        );
        assert!(
            body.contains(r#"<input type="text" name="score" value="&lt;script&gt;&amp;&quot;" required maxlength="100">"#),
            "{body}"
        );
        assert!(
            body.contains(&format!(
                r#"href="/traces/{A}?score=%3Cscript%3E%26%22&amp;prompt=%22%3E%3Cscript%3E&amp;at="#
            )),
            "{body}"
        );
        let sent = fake.body(0);
        assert_eq!(
            param_value(&sent, "names")["parameterValue"],
            json!({ "arrayValues": [{ "value": "<script>&\"" }] })
        );
        let sql = sent["query"].as_str().unwrap();
        assert!(!sql.contains("<script>"), "{sql}");
    }

    #[tokio::test]
    async fn invalid_queues_are_400_and_query_nothing() {
        for (path, text) in [
            (
                "/review?score=q&version=0&prompt=p",
                "version: must be a positive integer",
            ),
            (
                "/review?score=q&prompt=p&version=%2B3",
                "version: must be a positive integer",
            ),
            ("/review?score=q&version=2", "version: needs a prompt"),
            (
                "/review?score=q&range=30d",
                "range: must be 1h, 6h, 24h or 7d",
            ),
        ] {
            let fake = FakeHttp::new(vec![]);
            let (status, _, body) = get(bq_state(&fake), path).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
            assert!(body.contains(text), "{path}: {body}");
            assert!(body.contains(ACTIVE), "{body}");
            assert!(fake.uris().is_empty(), "{path}");
        }
    }

    #[tokio::test]
    async fn a_literal_plus_in_version_decodes_to_a_space_and_is_accepted() {
        let fake = FakeHttp::new(vec![pending_reply(vec![])]);
        let (status, _, body) = get(bq_state(&fake), "/review?score=q&prompt=p&version=+3").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(!body.contains("must be a positive integer"), "{body}");
        assert_eq!(fake.uris().len(), 1);
    }

    #[tokio::test]
    async fn a_failed_pending_query_is_a_502_with_the_hint() {
        let fake = FakeHttp::new(vec![denied()]);
        let (status, _, body) = get(bq_state(&fake), "/review?score=q").await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(body.contains("BigQuery: HTTP 403: Access Denied"), "{body}");
        assert!(
            body.contains("needs roles/bigquery.dataViewer and roles/bigquery.jobUser"),
            "{body}"
        );
        assert!(body.contains(ACTIVE), "{body}");

        let fake = FakeHttp::new(vec![denied()]);
        let path = format!("/review/next?score=q&after=2026-11-16T11:00:00Z&after_id={A}");
        let (status, _, body) = get(bq_state(&fake), &path).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(body.contains("BigQuery: HTTP 403: Access Denied"), "{body}");
    }

    #[tokio::test]
    async fn without_bigquery_both_routes_are_404_with_the_note() {
        let src = Arc::new(FakeSource::new());
        for path in [
            "/review",
            "/review?score=q",
            "/review/next?score=q&after=2026-11-16T11:00:00Z&after_id=0123456789abcdef0123456789abcdef",
        ] {
            let (status, _, body) = get(state(src.clone(), None), path).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
            assert!(
                body.contains("Review needs the BigQuery trace source: start the console with --traces bigquery://&lt;project&gt;.&lt;dataset&gt;.&lt;table&gt;."),
                "{path}: {body}"
            );
            assert!(body.contains(ACTIVE), "{body}");
        }
    }

    #[tokio::test]
    async fn next_redirects_to_the_next_trace_with_the_cursor_sent() {
        let fake = FakeHttp::new(vec![pending_reply(vec![pending_cells(
            B,
            ago(120),
            &[],
            0,
        )])]);
        let path = format!(
            "/review/next?score=quality&prompt=triage&after=2026-11-16T11%3A58%3A59.999750Z&after_id={}",
            A.to_uppercase()
        );
        let (status, headers, body) = get(bq_state(&fake), &path).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
        assert_eq!(
            headers[LOCATION],
            format!("/traces/{B}?score=quality&prompt=triage&at=2026-11-16T11%3A57%3A59.999750Z")
        );
        let sent = fake.body(0);
        assert_eq!(
            param_value(&sent, "after")["parameterValue"]["value"],
            json!("2026-11-16 11:58:59.999750+00:00")
        );
        assert_eq!(
            param_value(&sent, "after_id")["parameterValue"]["value"],
            json!(A)
        );
        assert_eq!(
            param_value(&sent, "limit")["parameterValue"]["value"],
            json!("1")
        );
        assert!(
            sent["query"].as_str().unwrap().contains(
                "AND (t.first < @after OR (t.first = @after AND t.trace_id < @after_id))"
            )
        );
    }

    #[tokio::test]
    async fn next_at_the_end_of_the_queue_redirects_to_done() {
        let fake = FakeHttp::new(vec![pending_reply(vec![])]);
        let path =
            format!("/review/next?score=a%26b&flagged=1&after=2026-11-16T11:00:00Z&after_id={A}");
        let (status, headers, _) = get(bq_state(&fake), &path).await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(headers[LOCATION], "/review?score=a%26b&flagged=1&done=1");
    }

    #[tokio::test]
    async fn next_with_an_invalid_queue_or_cursor_is_400_and_queries_nothing() {
        for (path, text) in [
            (
                format!("/review/next?after=2026-11-16T11:00:00Z&after_id={A}"),
                "score: required",
            ),
            (
                format!(
                    "/review/next?score=q&version=x&prompt=p&after=2026-11-16T11:00:00Z&after_id={A}"
                ),
                "version: must be a positive integer",
            ),
            (
                format!("/review/next?score=q&after_id={A}"),
                "after: must be an RFC 3339 time",
            ),
            (
                format!("/review/next?score=q&after=2026-11-16&after_id={A}"),
                "after: must be an RFC 3339 time",
            ),
            (
                "/review/next?score=q&after=2026-11-16T11:00:00Z&after_id=xyz".to_owned(),
                "after_id: must be 32 hex digits",
            ),
            (
                "/review/next?score=q&after=2026-11-16T11:00:00Z".to_owned(),
                "after_id: must be 32 hex digits",
            ),
        ] {
            let fake = FakeHttp::new(vec![]);
            let (status, _, body) = get(bq_state(&fake), &path).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
            assert!(body.contains(text), "{path}: {body}");
            assert!(fake.uris().is_empty(), "{path}");
        }
    }

    /// [`bq_state`] with the test rubrics loaded.
    fn rubric_state(fake: &Arc<FakeHttp>) -> AppState {
        let mut st = bq_state(fake);
        st.rubrics = Some(test_rubrics());
        st
    }

    #[tokio::test]
    async fn with_rubrics_the_start_form_offers_them_and_needs_neither_field() {
        let fake = FakeHttp::new(vec![]);
        let (status, _, body) = get(rubric_state(&fake), "/review").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("<h1>Review</h1>"), "{body}");
        assert!(
            body.contains(r#"<label>Score name<input type="text" name="score" maxlength="100"></label><label>Rubric<select name="rubric"><option value=""></option><option value="support">support</option><option value="safety">safety</option></select></label><label>Prompt"#),
            "{body}"
        );
        assert!(
            body.contains(r#"<p class="empty">Enter a score name or choose a rubric to list the recent traces that are not yet scored.</p>"#),
            "{body}"
        );
        assert!(fake.uris().is_empty());

        // Blank score and rubric (the form submitted empty) is the start form too.
        let fake = FakeHttp::new(vec![]);
        let (status, _, blank) = get(rubric_state(&fake), "/review?score=&rubric=").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(blank, body);
        assert!(fake.uris().is_empty());
    }

    #[tokio::test]
    async fn without_rubrics_the_start_form_has_no_rubric_select() {
        let fake = FakeHttp::new(vec![]);
        let (_, _, body) = get(bq_state(&fake), "/review").await;
        assert!(
            body.contains(r#"<label>Score name<input type="text" name="score" required maxlength="100"></label><label>Prompt"#),
            "{body}"
        );
        assert!(!body.contains("rubric"), "{body}");
        assert!(
            body.contains(r#"<p class="empty">Enter a score name to list the recent traces that have no console score with that name yet.</p>"#),
            "{body}"
        );
    }

    #[tokio::test]
    async fn a_rubric_queue_lists_its_pending_traces_and_sends_the_criterion_names() {
        let fake = FakeHttp::new(vec![pending_reply(vec![
            pending_cells(A, ago(60), &["triage@2"], 0),
            pending_cells(B, ago(120), &[], 1),
        ])]);
        let (status, _, body) = get(rubric_state(&fake), "/review?rubric=support&range=24h").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("<h1>Review: rubric support</h1>"), "{body}");
        assert!(body.contains("<title>Review: rubric support"), "{body}");
        assert!(body.contains("<p>2 pending</p>"), "{body}");
        for piece in [
            r#"<input type="text" name="score" maxlength="100">"#.to_owned(),
            r#"<option value="support" selected>support</option><option value="safety">safety</option>"#.to_owned(),
            r#"<option value="24h" selected>24h</option>"#.to_owned(),
            format!(
                r#"<p><a href="/traces/{A}?rubric=support&amp;range=24h&amp;at=2026-11-16T11%3A58%3A59.999750Z">Start reviewing</a></p>"#
            ),
            format!(
                r#"<a href="/traces/{B}?rubric=support&amp;range=24h&amp;at=2026-11-16T11%3A57%3A59.999750Z">{B}</a>"#
            ),
        ] {
            assert!(body.contains(&piece), "{piece}: {body}");
        }
        let sent = fake.body(0);
        assert_eq!(
            param_value(&sent, "names")["parameterValue"],
            json!({ "arrayValues": [{ "value": "support/accuracy" }, { "value": "support/tone" }] })
        );
        assert_eq!(
            param_value(&sent, "start")["parameterValue"]["value"],
            json!("2026-11-15 12:00:00.000000+00:00")
        );
    }

    #[tokio::test]
    async fn a_score_queue_with_rubrics_loaded_selects_no_rubric() {
        let fake = FakeHttp::new(vec![pending_reply(vec![])]);
        let (status, _, body) = get(rubric_state(&fake), "/review?score=quality&rubric=").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("<h1>Review: quality</h1>"), "{body}");
        assert!(
            body.contains(r#"<input type="text" name="score" value="quality" maxlength="100">"#),
            "{body}"
        );
        assert!(
            body.contains(r#"<option value=""></option><option value="support">support</option>"#),
            "{body}"
        );
        assert_eq!(
            param_value(&fake.body(0), "names")["parameterValue"],
            json!({ "arrayValues": [{ "value": "quality" }] })
        );
    }

    #[tokio::test]
    async fn invalid_rubric_queues_are_400_and_query_nothing() {
        for (with_rubrics, path, text) in [
            (
                true,
                "/review?score=q&rubric=support",
                "score and rubric: choose one",
            ),
            (
                false,
                "/review?score=q&rubric=support",
                "score and rubric: choose one",
            ),
            (true, "/review?rubric=nope", "rubric: unknown rubric `nope`"),
            (
                false,
                "/review?rubric=support",
                "rubric: unknown rubric `support`",
            ),
            (
                true,
                "/review?rubric=nope&version=x",
                "rubric: unknown rubric `nope`",
            ),
            (
                true,
                "/review/next?score=q&rubric=support&after=2026-11-16T11:00:00Z",
                "score and rubric: choose one",
            ),
            (
                false,
                "/review/next?rubric=support&after=2026-11-16T11:00:00Z",
                "rubric: unknown rubric `support`",
            ),
        ] {
            let fake = FakeHttp::new(vec![]);
            let st = if with_rubrics {
                rubric_state(&fake)
            } else {
                bq_state(&fake)
            };
            let (status, _, body) = get(st, path).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
            assert!(body.contains(text), "{path}: {body}");
            assert!(body.contains(ACTIVE), "{body}");
            assert!(fake.uris().is_empty(), "{path}");
        }
    }

    #[tokio::test]
    async fn next_in_a_rubric_queue_keeps_the_rubric() {
        let fake = FakeHttp::new(vec![
            pending_reply(vec![pending_cells(B, ago(120), &[], 0)]),
            pending_reply(vec![]),
        ]);
        let path = format!(
            "/review/next?rubric=support&flagged=1&after=2026-11-16T11%3A58%3A59.999750Z&after_id={A}"
        );
        let (status, headers, body) = get(rubric_state(&fake), &path).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
        assert_eq!(
            headers[LOCATION],
            format!("/traces/{B}?rubric=support&flagged=1&at=2026-11-16T11%3A57%3A59.999750Z")
        );
        assert_eq!(
            param_value(&fake.body(0), "names")["parameterValue"],
            json!({ "arrayValues": [{ "value": "support/accuracy" }, { "value": "support/tone" }] })
        );
        let (status, headers, _) = get(rubric_state(&fake), &path).await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(headers[LOCATION], "/review?rubric=support&flagged=1&done=1");
    }

    #[test]
    fn rubric_links_carry_the_rubric_first() {
        let params = vec![
            ("range".to_owned(), "1h".to_owned()),
            ("rubric".to_owned(), "safety".to_owned()),
        ];
        let q = ReviewQueue::from_params(&params, Some(&test_rubrics()))
            .unwrap()
            .unwrap();
        assert_eq!(
            review_link(&q, A, ago(60)),
            format!("/traces/{A}?rubric=safety&range=1h&at=2026-11-16T11%3A58%3A59.999750Z")
        );
        let after = Cursor::new(ago(60), B.into());
        assert_eq!(
            next_link(&q, &after),
            format!(
                "/review/next?rubric=safety&range=1h&after=2026-11-16T11%3A58%3A59.999750Z&after_id={B}"
            )
        );
    }
}
