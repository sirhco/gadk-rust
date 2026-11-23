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

//! Session routes: `/sessions` and `/users`.

use axum::Router;
use axum::extract::{Query, State};
use axum::response::Response;
use axum::routing::get;
use chrono::{DateTime, Utc};
use http::StatusCode;

use super::AppState;
use super::pages::error::{error_page, source_error_in};
use super::pages::layout::{Nav, html};
use super::pages::{sessions, users};
use super::review::first;
use crate::review::at_text;
use crate::sessions::{MAX_ID_LEN, SessionCursor, SessionQuery, UserQuery};
use crate::traces::TimeRange;
use crate::url::percent_encode;

/// The note shown by the session pages when the trace source is not BigQuery.
pub(crate) const NEEDS_BIGQUERY_SESSIONS: &str = "Sessions need the BigQuery trace source: start the console with --traces bigquery://<project>.<dataset>.<table>.";

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/sessions", get(list))
        .route("/users", get(list_users))
}

/// `/sessions/{id}?range={range}`: the session page.
pub(crate) fn session_link(id: &str, range: TimeRange) -> String {
    format!("/sessions/{}?range={}", percent_encode(id), range.as_str())
}

/// `/sessions?range={range}[&user={user}][&agent={agent}]&after={last}&after_id={id}`: the
/// sessions after the session `id` whose last activity is `last`.
pub(crate) fn next_link(q: &SessionQuery, id: &str, last: DateTime<Utc>) -> String {
    let mut out = format!("/sessions?range={}", q.range.as_str());
    if let Some(user) = &q.user {
        out.push_str(&format!("&user={}", percent_encode(user)));
    }
    if let Some(agent) = &q.agent {
        out.push_str(&format!("&agent={}", percent_encode(agent)));
    }
    out.push_str(&format!(
        "&after={}&after_id={}",
        percent_encode(&at_text(last)),
        percent_encode(id)
    ));
    out
}

fn needs_bigquery() -> Response {
    error_page(
        StatusCode::NOT_FOUND,
        Nav::Sessions,
        NEEDS_BIGQUERY_SESSIONS,
        None,
    )
}

fn bad_request(text: &str) -> Response {
    error_page(StatusCode::BAD_REQUEST, Nav::Sessions, text, None)
}

/// The `range` parameter; absent or unknown is `default`.
fn range_param(params: &[(String, String)], default: TimeRange) -> TimeRange {
    TimeRange::parse(first(params, "range").trim()).unwrap_or(default)
}

/// The filter `key`, trimmed; blank is absent.
///
/// # Errors
///
/// `{key} is too long` beyond [`MAX_ID_LEN`] bytes.
fn filter_param(params: &[(String, String)], key: &str) -> Result<Option<String>, String> {
    let value = first(params, key).trim();
    if value.is_empty() {
        Ok(None)
    } else if value.len() > MAX_ID_LEN {
        Err(format!("{key} is too long"))
    } else {
        Ok(Some(value.to_owned()))
    }
}

/// The `/sessions` query of `params`, ending at `end`.
///
/// # Errors
///
/// The text of the 400 page: `user is too long`, `agent is too long` or `invalid cursor`
/// (`after` and `after_id` must both be absent, or both valid).
fn session_query(params: &[(String, String)], end: DateTime<Utc>) -> Result<SessionQuery, String> {
    let (after, after_id) = (first(params, "after"), first(params, "after_id"));
    let after = if after.is_empty() && after_id.is_empty() {
        None
    } else {
        Some(SessionCursor::parse(after, after_id)?)
    };
    Ok(SessionQuery {
        range: range_param(params, TimeRange::Hours24),
        end,
        user: filter_param(params, "user")?,
        agent: filter_param(params, "agent")?,
        session: None,
        after,
    })
}

/// `GET /sessions`: at most 100 sessions of the window (24h by default), filtered by user and
/// agent, after the cursor. Bad parameters are a 400 page, a failed query a 502 page.
async fn list(
    State(state): State<AppState>,
    Query(params): Query<Vec<(String, String)>>,
) -> Response {
    let Some(usage) = &state.usage else {
        return needs_bigquery();
    };
    let q = match session_query(&params, (state.now)()) {
        Ok(q) => q,
        Err(text) => return bad_request(&text),
    };
    match usage.sessions(&q).await {
        Ok(p) => html(
            StatusCode::OK,
            sessions::render(&q, &p, state.prices.as_deref()),
        ),
        Err(e) => source_error_in(Nav::Sessions, &e),
    }
}

/// `GET /users`: at most 100 users of the window (7d by default), filtered by agent. Bad
/// parameters are a 400 page, a failed query a 502 page.
async fn list_users(
    State(state): State<AppState>,
    Query(params): Query<Vec<(String, String)>>,
) -> Response {
    let Some(usage) = &state.usage else {
        return needs_bigquery();
    };
    let q = match filter_param(&params, "agent") {
        Ok(agent) => UserQuery {
            range: range_param(&params, TimeRange::Days7),
            end: (state.now)(),
            agent,
        },
        Err(text) => return bad_request(&text),
    };
    match usage.users(&q).await {
        Ok(p) => html(
            StatusCode::OK,
            users::render(&q, &p, state.prices.as_deref()),
        ),
        Err(e) => source_error_in(Nav::Sessions, &e),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::{DateTime, TimeDelta, Utc};
    use gadk_bigquery::{BigQueryClient, NoAuth};
    use http::StatusCode;
    use serde_json::json;

    use crate::prices::PriceTable;
    use crate::sessions::{SESSION_FIELDS, USER_FIELDS, session_cells, user_cells};
    use crate::traces::BigQueryTable;
    use crate::traces::test_http::{FakeHttp, param_value, reply};
    use crate::usage::UsageSource;
    use crate::web::AppState;
    use crate::web::testutil::{FakeSource, fixed_now, get, state};

    const ACTIVE: &str = r#"<a class="active" href="/sessions">Sessions</a>"#;
    const PRICES: &str = "- model: gemini-3.8-flash\n  input: 1\n  output: 2\n";

    fn bq_state(fake: &Arc<FakeHttp>, prices: bool) -> AppState {
        let mut st = state(Arc::new(FakeSource::new()), None);
        st.usage = Some(Arc::new(UsageSource::with_client(
            BigQueryClient::with_client(fake.clone(), Arc::new(NoAuth)),
            "billing-proj",
            BigQueryTable::new("data-proj", "agent_analytics", "agent_events"),
            1_000,
        )));
        if prices {
            st.prices = Some(Arc::new(PriceTable::from_yaml(PRICES).unwrap()));
        }
        st
    }

    /// `fixed_now` minus `secs` seconds and 250 microseconds.
    fn ago(secs: i64) -> DateTime<Utc> {
        fixed_now() - TimeDelta::seconds(secs) - TimeDelta::microseconds(250)
    }

    fn failed() -> Result<(u16, String), String> {
        Ok((
            500,
            json!({ "error": { "code": 500, "message": "backend error", "errors": [{ "reason": "backendError" }] } }).to_string(),
        ))
    }

    #[tokio::test]
    async fn without_bigquery_the_pages_are_404_with_the_note() {
        let src = Arc::new(FakeSource::new());
        for path in ["/sessions", "/sessions?user=u", "/users"] {
            let (status, _, body) = get(state(src.clone(), None), path).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
            assert!(
                body.contains("Sessions need the BigQuery trace source: start the console with --traces bigquery://&lt;project&gt;.&lt;dataset&gt;.&lt;table&gt;."),
                "{path}: {body}"
            );
            assert!(body.contains(ACTIVE), "{body}");
        }
    }

    #[tokio::test]
    async fn sessions_list_their_rows_with_links_totals_and_cost() {
        let fake = FakeHttp::new(vec![reply(
            &SESSION_FIELDS,
            vec![
                session_cells(
                    "s/<1>",
                    "u-1",
                    "triage",
                    ago(600),
                    ago(60),
                    [3, 1, 2, 1],
                    Some(("gemini-3.8-flash", [1_000_000, 0, 500_000, 0])),
                ),
                session_cells(
                    "s/<1>",
                    "u-1",
                    "triage",
                    ago(600),
                    ago(60),
                    [3, 1, 2, 1],
                    Some(("other-model", [10, 0, 5, 0])),
                ),
                session_cells(
                    "s-2",
                    "",
                    "",
                    ago(900),
                    ago(120),
                    [1, 0, 0, 0],
                    Some(("other-model", [7, 0, 3, 0])),
                ),
                session_cells(
                    "s-3",
                    "u-2",
                    "triage",
                    ago(950),
                    ago(130),
                    [0, 0, 0, 0],
                    None,
                ),
            ],
            false,
        )]);
        let (status, _, body) = get(bq_state(&fake, true), "/sessions").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains(ACTIVE), "{body}");
        assert!(body.contains("<h1>Sessions</h1>"), "{body}");
        assert!(
            body.contains(r#"<a href="/users?range=24h">View by user →</a>"#),
            "{body}"
        );
        assert!(
            body.contains(r#"<option value="24h" selected>24h</option>"#),
            "{body}"
        );
        for row in [
            r#"<tr><td class="mono"><a href="/sessions/s%2F%3C1%3E?range=24h">s/&lt;1&gt;</a></td><td>u-1</td><td>triage</td><td class="mono">2026-11-16T11:49:59.999Z</td><td class="mono">2026-11-16T11:58:59.999Z</td><td class="num">3</td><td class="num">1,500,015</td><td class="num">$2.000000*</td><td class="num">1</td><td class="num">2</td><td class="num">1</td></tr>"#,
            r#"<tr><td class="mono"><a href="/sessions/s-2?range=24h">s-2</a></td><td>(none)</td><td></td><td class="mono">2026-11-16T11:44:59.999Z</td><td class="mono">2026-11-16T11:57:59.999Z</td><td class="num">1</td><td class="num">10</td><td class="num">—</td><td class="num">0</td><td class="num">0</td><td class="num">0</td></tr>"#,
            r#"<td class="num">0</td><td class="num">—</td>"#,
        ] {
            assert!(body.contains(row), "{row}\n{body}");
        }
        assert!(body.contains("* Costs exclude calls"), "{body}");
        assert!(!body.contains("Next →"), "{body}");
        assert!(!body.contains("results truncated"), "{body}");
        let sent = fake.body(0);
        assert_eq!(
            param_value(&sent, "start")["parameterValue"]["value"],
            json!("2026-11-15 12:00:00.000000+00:00")
        );
        assert_eq!(param_value(&sent, "user")["parameterValue"], json!({}));
    }

    #[tokio::test]
    async fn filters_are_trimmed_blank_is_absent_and_the_cursor_passes_as_given() {
        let fake = FakeHttp::new(vec![reply(&SESSION_FIELDS, vec![], false)]);
        let path = "/sessions?user=%20u-1%20&agent=%20&range=7d&after=2026-11-16T11%3A58%3A59.999750Z&after_id=Sess-AB";
        let (status, _, body) = get(bq_state(&fake, false), path).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(
            body.contains(r#"<p class="empty">No sessions in the last 7d.</p>"#),
            "{body}"
        );
        assert!(
            body.contains(r#"<input type="text" name="user" maxlength="128" value="u-1">"#),
            "{body}"
        );
        let sent = fake.body(0);
        assert_eq!(
            param_value(&sent, "user")["parameterValue"]["value"],
            json!("u-1")
        );
        assert_eq!(param_value(&sent, "agent")["parameterValue"], json!({}));
        assert_eq!(
            param_value(&sent, "after_id")["parameterValue"]["value"],
            json!("Sess-AB")
        );
        assert_eq!(
            param_value(&sent, "start")["parameterValue"]["value"],
            json!("2026-11-09 12:00:00.000000+00:00")
        );
        let fake = FakeHttp::new(vec![reply(&SESSION_FIELDS, vec![], false)]);
        let (status, _, _) = get(bq_state(&fake, false), "/sessions?range=30d").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            param_value(&fake.body(0), "start")["parameterValue"]["value"],
            json!("2026-11-15 12:00:00.000000+00:00"),
            "an unknown range is the default"
        );
    }

    #[tokio::test]
    async fn bad_parameters_are_400_and_query_nothing() {
        let long = "u".repeat(129);
        for (path, text) in [
            (format!("/sessions?user={long}"), "user is too long"),
            (format!("/sessions?agent={long}"), "agent is too long"),
            (format!("/users?agent={long}"), "agent is too long"),
            (
                "/sessions?after=2026-11-16T11:00:00Z".to_owned(),
                "invalid cursor",
            ),
            ("/sessions?after_id=s-1".to_owned(), "invalid cursor"),
            (
                "/sessions?after=yesterday&after_id=s-1".to_owned(),
                "invalid cursor",
            ),
            (
                "/sessions?after=2026-11-16T11:00:00Z&after_id=a%0Ab".to_owned(),
                "invalid cursor",
            ),
        ] {
            let fake = FakeHttp::new(vec![]);
            let (status, _, body) = get(bq_state(&fake, false), &path).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
            assert!(body.contains(text), "{path}: {body}");
            assert!(body.contains(ACTIVE), "{body}");
            assert!(fake.uris().is_empty(), "{path}");
        }
        let fake = FakeHttp::new(vec![reply(&SESSION_FIELDS, vec![], false)]);
        let ok = format!("/sessions?user={}", "u".repeat(128));
        assert_eq!(get(bq_state(&fake, false), &ok).await.0, StatusCode::OK);
    }

    #[tokio::test]
    async fn more_than_a_hundred_sessions_link_to_the_next_page() {
        let session = |i: i64| {
            session_cells(
                &format!("s {i:03}"),
                "u",
                "a",
                ago(10 + i),
                ago(i),
                [1, 0, 0, 0],
                Some(("m", [1, 0, 1, 0])),
            )
        };
        let mut rows: Vec<_> = (0..101).map(session).collect();
        rows.insert(1, {
            let mut r = session(0);
            r[9] = json!("m2");
            r
        });
        let fake = FakeHttp::new(vec![reply(&SESSION_FIELDS, rows.clone(), false)]);
        let (status, _, body) = get(
            bq_state(&fake, false),
            "/sessions?user=a%26b&agent=triage&range=6h",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body.matches("<tr><td").count(), 100, "{body}");
        assert!(
            body.contains(r#"<p><a href="/sessions?range=6h&amp;user=a%26b&amp;agent=triage&amp;after=2026-11-16T11%3A58%3A20.999750Z&amp;after_id=s%20099">Next →</a></p>"#),
            "{body}"
        );

        rows.truncate(101);
        let fake = FakeHttp::new(vec![reply(&SESSION_FIELDS, rows, false)]);
        let (_, _, body) = get(bq_state(&fake, false), "/sessions").await;
        assert_eq!(body.matches("<tr><td").count(), 100, "{body}");
        assert!(!body.contains("Next →"), "{body}");
    }

    #[tokio::test]
    async fn more_rows_than_returned_show_results_truncated() {
        let fake = FakeHttp::new(vec![
            reply(
                &SESSION_FIELDS,
                vec![session_cells(
                    "s-1",
                    "u",
                    "a",
                    ago(9),
                    ago(1),
                    [1, 0, 0, 0],
                    None,
                )],
                true,
            ),
            reply(
                &USER_FIELDS,
                vec![user_cells(Some("u"), [1, 1, 0, 0, 0], ago(1), None)],
                true,
            ),
        ]);
        let st = bq_state(&fake, false);
        let (_, _, body) = get(st.clone(), "/sessions").await;
        assert!(body.contains("results truncated"), "{body}");
        let (_, _, body) = get(st, "/users").await;
        assert!(body.contains("results truncated"), "{body}");
    }

    #[tokio::test]
    async fn users_link_to_their_sessions_except_none() {
        let fake = FakeHttp::new(vec![reply(
            &USER_FIELDS,
            vec![
                user_cells(
                    Some("a&b"),
                    [2, 5, 1, 3, 0],
                    ago(60),
                    Some(("gemini-3.8-flash", [1_000_000, 0, 0, 0])),
                ),
                user_cells(None, [1, 1, 0, 0, 1], ago(120), None),
            ],
            false,
        )]);
        let (status, _, body) = get(bq_state(&fake, true), "/users?agent=triage").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains(ACTIVE), "{body}");
        assert!(body.contains("<h1>Users</h1>"), "{body}");
        assert!(
            body.contains(r#"<a href="/sessions?range=7d">View by session →</a>"#),
            "{body}"
        );
        for row in [
            r#"<tr><td><a href="/sessions?user=a%26b&amp;range=7d">a&amp;b</a></td><td class="num">2</td><td class="num">5</td><td class="num">1,000,000</td><td class="num">$1.000000</td><td class="num">1</td><td class="num">3</td><td class="num">0</td><td class="mono">2026-11-16T11:58:59.999Z</td></tr>"#,
            r#"<tr><td>(none)</td><td class="num">1</td><td class="num">1</td><td class="num">0</td><td class="num">—</td><td class="num">0</td><td class="num">0</td><td class="num">1</td><td class="mono">2026-11-16T11:57:59.999Z</td></tr>"#,
        ] {
            assert!(body.contains(row), "{row}\n{body}");
        }
        assert!(!body.contains("most recent users"), "{body}");
        let sent = fake.body(0);
        assert_eq!(
            param_value(&sent, "start")["parameterValue"]["value"],
            json!("2026-11-09 12:00:00.000000+00:00")
        );
        assert_eq!(
            param_value(&sent, "agent")["parameterValue"]["value"],
            json!("triage")
        );
    }

    #[tokio::test]
    async fn more_than_a_hundred_users_show_the_cut_note() {
        let rows = (0..101)
            .map(|i| user_cells(Some(&format!("u-{i:03}")), [1, 1, 0, 0, 0], ago(i), None))
            .collect();
        let fake = FakeHttp::new(vec![reply(&USER_FIELDS, rows, false)]);
        let (_, _, body) = get(bq_state(&fake, false), "/users").await;
        assert!(
            body.contains("<p>showing the 100 most recent users</p>"),
            "{body}"
        );
        assert_eq!(body.matches("<tr><td").count(), 100, "{body}");
    }

    #[tokio::test]
    async fn a_failed_query_is_a_502_page() {
        for path in ["/sessions", "/users"] {
            let fake = FakeHttp::new(vec![failed()]);
            let (status, _, body) = get(bq_state(&fake, false), path).await;
            assert_eq!(status, StatusCode::BAD_GATEWAY, "{path}");
            assert!(body.contains("backend error"), "{path}: {body}");
            assert!(body.contains(ACTIVE), "{body}");
        }
    }
}
