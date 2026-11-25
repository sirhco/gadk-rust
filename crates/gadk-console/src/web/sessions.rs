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

//! Session routes: `/sessions`, `/sessions/{id}` and `/users`, and their downloads
//! `/sessions.csv` and `/users.csv`.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::Response;
use axum::routing::get;
use chrono::{DateTime, Utc};
use gadk_analytics::sessions::{
    CSV_CAP, CSV_ROW_CAP, MAX_ID_LEN, ROW_CAP, SESSION_CAP, SessionCursor, SessionQuery, TURN_CAP,
    USER_CAP, UserQuery, is_session_id,
};
use gadk_eval_monitor::ExportSource;
use http::StatusCode;

use super::AppState;
use super::pages::error::{error_page, source_error_in};
use super::pages::layout::{Nav, html};
use super::pages::session::{self, Conversation};
use super::pages::{sessions, users};
use super::review::first;
use crate::csv::{Csv, Field, filename, response};
use crate::traces::TimeRange;
use crate::url::percent_encode;
use gadk_analytics::review::at_text;

/// The note shown by the session pages when the trace source is not BigQuery.
pub(crate) const NEEDS_BIGQUERY_SESSIONS: &str = "Sessions need the BigQuery trace source: start the console with --traces bigquery://<project>.<dataset>.<table>.";

/// The text of the 400 page for a session id that is empty, longer than 128 bytes or holds a
/// control character.
pub(crate) const INVALID_SESSION_ID: &str = "invalid session id";

/// Why the session page has no conversation text when the console has no export source.
const NO_EXPORT: &str = "the console has no transcript source";

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/sessions", get(list))
        .route("/sessions.csv", get(sessions_csv))
        .route("/sessions/{id}", get(session_page))
        .route("/users", get(list_users))
        .route("/users.csv", get(users_csv))
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

/// `/sessions.csv?range={range}[&user={user}][&agent={agent}]`: the download of the sessions
/// `q` lists.
pub(crate) fn sessions_csv_link(q: &SessionQuery) -> String {
    let mut out = format!("/sessions.csv?range={}", q.range.as_str());
    if let Some(user) = &q.user {
        out.push_str(&format!("&user={}", percent_encode(user)));
    }
    if let Some(agent) = &q.agent {
        out.push_str(&format!("&agent={}", percent_encode(agent)));
    }
    out
}

/// `/users.csv?range={range}[&agent={agent}]`: the download of the users `q` lists.
pub(crate) fn users_csv_link(q: &UserQuery) -> String {
    let mut out = format!("/users.csv?range={}", q.range.as_str());
    if let Some(agent) = &q.agent {
        out.push_str(&format!("&agent={}", percent_encode(agent)));
    }
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

/// The `/sessions` query of `params`, ending at `end` (shared with `/api/v1/sessions`).
///
/// # Errors
///
/// The text of the 400 page: `user is too long`, `agent is too long` or `invalid cursor`
/// (`after` and `after_id` must both be absent, or both valid).
pub(crate) fn session_query(
    params: &[(String, String)],
    end: DateTime<Utc>,
) -> Result<SessionQuery, String> {
    let (after, after_id) = (first(params, "after"), first(params, "after_id"));
    let after = if after.is_empty() && after_id.is_empty() {
        None
    } else {
        Some(SessionCursor::parse(after, after_id)?)
    };
    let mut q = SessionQuery::new(range_param(params, TimeRange::Hours24), end);
    q.user = filter_param(params, "user")?;
    q.agent = filter_param(params, "agent")?;
    q.after = after;
    Ok(q)
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
    match usage.sessions(&q, SESSION_CAP, ROW_CAP).await {
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
    let q = match user_query(&params, (state.now)()) {
        Ok(q) => q,
        Err(text) => return bad_request(&text),
    };
    match usage.users(&q, USER_CAP, ROW_CAP).await {
        Ok(p) => html(
            StatusCode::OK,
            users::render(&q, &p, state.prices.as_deref()),
        ),
        Err(e) => source_error_in(Nav::Sessions, &e),
    }
}

/// The `/users` query of `params`, ending at `end` (shared with `/api/v1/users`).
///
/// # Errors
///
/// The text of the 400 page: `agent is too long`.
pub(crate) fn user_query(
    params: &[(String, String)],
    end: DateTime<Utc>,
) -> Result<UserQuery, String> {
    let mut q = UserQuery::new(range_param(params, TimeRange::Days7), end);
    q.agent = filter_param(params, "agent")?;
    Ok(q)
}

/// The header query of session `id` on `/sessions/{id}` and `/api/v1/sessions/{id}/turns`:
/// the session's row over the `range` of `params` (7d by default), ending at `end`.
///
/// # Errors
///
/// [`INVALID_SESSION_ID`] for an id that is empty, longer than 128 bytes or holds a control
/// character.
pub(crate) fn turns_query(
    id: &str,
    params: &[(String, String)],
    end: DateTime<Utc>,
) -> Result<SessionQuery, &'static str> {
    if !is_session_id(id) {
        return Err(INVALID_SESSION_ID);
    }
    Ok(SessionQuery::new(range_param(params, TimeRange::Days7), end).with_session(id))
}

/// `GET /sessions.csv`: every session of the window `/sessions` would list (same `range`,
/// `user` and `agent`), newest last activity first, from the newest on (`after` and `after_id`
/// are not read), up to [`CSV_CAP`].
async fn sessions_csv(
    State(state): State<AppState>,
    Query(params): Query<Vec<(String, String)>>,
) -> Response {
    let Some(usage) = &state.usage else {
        return needs_bigquery();
    };
    let q = match (
        filter_param(&params, "user"),
        filter_param(&params, "agent"),
    ) {
        (Ok(user), Ok(agent)) => {
            let mut q = SessionQuery::new(range_param(&params, TimeRange::Hours24), (state.now)());
            q.user = user;
            q.agent = agent;
            q
        }
        (Err(text), _) | (_, Err(text)) => return bad_request(&text),
    };
    let p = match usage.sessions(&q, CSV_CAP, CSV_ROW_CAP).await {
        Ok(p) => p,
        Err(e) => return source_error_in(Nav::Sessions, &e),
    };
    let mut csv = Csv::new();
    csv.header(&[
        "session_id",
        "user",
        "agent",
        "first_activity",
        "last_activity",
        "turns",
        "tokens",
        "cost_usd",
        "cost_partial",
        "errors",
        "thumbs_up",
        "thumbs_down",
    ]);
    for s in &p.sessions {
        let (tokens, cost) = sessions::totals(&s.tokens, state.prices.as_deref());
        csv.row(&[
            Field::Text(s.session_id.clone()),
            Field::Text(s.user.clone()),
            Field::Text(s.agent.clone()),
            Field::Time(s.first),
            Field::Time(s.last),
            Field::Int(s.turns),
            Field::Int(tokens),
            Field::Cost(cost),
            Field::Int(s.errors),
            Field::Int(s.ups),
            Field::Int(s.downs),
        ]);
    }
    let truncated = p.has_next || p.truncated;
    let name = filename("sessions", q.range.as_str(), q.end, truncated);
    response(csv.finish(), &name, truncated)
}

/// `GET /users.csv`: every user of the window `/users` would list (same `range` and `agent`),
/// in the page's order, up to [`CSV_CAP`]. The `user` column is the raw id, empty for none.
async fn users_csv(
    State(state): State<AppState>,
    Query(params): Query<Vec<(String, String)>>,
) -> Response {
    let Some(usage) = &state.usage else {
        return needs_bigquery();
    };
    let q = match user_query(&params, (state.now)()) {
        Ok(q) => q,
        Err(text) => return bad_request(&text),
    };
    let p = match usage.users(&q, CSV_CAP, CSV_ROW_CAP).await {
        Ok(p) => p,
        Err(e) => return source_error_in(Nav::Sessions, &e),
    };
    let mut csv = Csv::new();
    csv.header(&[
        "user",
        "sessions",
        "turns",
        "errors",
        "thumbs_up",
        "thumbs_down",
        "last_activity",
        "tokens",
        "cost_usd",
        "cost_partial",
    ]);
    for u in &p.users {
        let (tokens, cost) = sessions::totals(&u.tokens, state.prices.as_deref());
        csv.row(&[
            Field::Text(u.user.clone()),
            Field::Int(u.sessions),
            Field::Int(u.turns),
            Field::Int(u.errors),
            Field::Int(u.ups),
            Field::Int(u.downs),
            Field::Time(u.last),
            Field::Int(tokens),
            Field::Cost(cost),
        ]);
    }
    let truncated = p.more || p.truncated;
    let name = filename("users", q.range.as_str(), q.end, truncated);
    response(csv.finish(), &name, truncated)
}

/// `GET /sessions/{id}`: the session's header (its row of the sessions query), then its turns
/// in the window (7d by default) with the conversation text from the export source. An invalid
/// id is a 400 page; no activity is a 404 page with links; a failed query is a 502 page; a
/// failed or missing export source is a banner on the page.
async fn session_page(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(params): Query<Vec<(String, String)>>,
) -> Response {
    let Some(usage) = &state.usage else {
        return needs_bigquery();
    };
    let q = match turns_query(&id, &params, (state.now)()) {
        Ok(q) => q,
        Err(text) => return bad_request(text),
    };
    let (range, end) = (q.range, q.end);
    let start = end - range.duration();
    let head = match usage.sessions(&q, SESSION_CAP, ROW_CAP).await {
        Ok(p) => p.sessions.into_iter().next(),
        Err(e) => return source_error_in(Nav::Sessions, &e),
    };
    let Some(head) = head else {
        return html(StatusCode::NOT_FOUND, session::render_missing(&id, range));
    };
    let turns = match usage.session_turns(&id, start, end).await {
        Ok(turns) => turns,
        Err(e) => return source_error_in(Nav::Sessions, &e),
    };
    let ids: Vec<String> = turns
        .iter()
        .take(TURN_CAP)
        .map(|t| t.invocation_id.clone())
        .collect();
    let text = conversation(state.export.as_ref(), &ids, start).await;
    html(
        StatusCode::OK,
        session::render(&head, range, &turns, &text, state.prices.as_deref()),
    )
}

/// The runs of the invocations `ids` that started at or after `from`, by invocation id; the
/// reason when the export source is missing, failed to build or fails.
async fn conversation(
    export: Option<&Result<Arc<dyn ExportSource>, String>>,
    ids: &[String],
    from: DateTime<Utc>,
) -> Conversation {
    match export {
        None => Conversation::Unavailable(NO_EXPORT.to_owned()),
        Some(Err(e)) => Conversation::Unavailable(e.clone()),
        Some(Ok(_)) if ids.is_empty() => Conversation::Runs(HashMap::new()),
        Some(Ok(source)) => match source.runs(ids, from).await {
            Ok(runs) => Conversation::Runs(
                runs.into_iter()
                    .map(|r| (r.invocation_id.clone(), r))
                    .collect(),
            ),
            Err(e) => Conversation::Unavailable(e.to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    use chrono::{DateTime, TimeDelta, Utc};
    use gadk_bigquery::{BigQueryClient, NoAuth};
    use gadk_eval::{RunTranscript, TurnRecord};
    use gadk_eval_monitor::ProductionRun;
    use http::StatusCode;
    use http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
    use serde_json::{Value, json};

    use crate::prices::PriceTable;
    use crate::traces::BigQueryTable;
    use crate::web::AppState;
    use crate::web::testutil::{FakeExport, FakeSource, fixed_now, get, state};
    use gadk_analytics::AnalyticsSource;
    use gadk_analytics::testutil::{
        FakeHttp, SESSION_FIELDS, TurnCells, USER_FIELDS, param_value, reply, session_cells,
        turn_reply, user_cells,
    };

    const ACTIVE: &str = r#"<a class="active" href="/sessions">Sessions</a>"#;
    const PRICES: &str = "- model: gemini-3.8-flash\n  input: 1\n  output: 2\n";

    fn bq_state(fake: &Arc<FakeHttp>, prices: bool) -> AppState {
        let mut st = state(Arc::new(FakeSource::new()), None);
        st.usage = Some(Arc::new(AnalyticsSource::new(
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

    #[tokio::test]
    async fn a_missing_table_is_a_502_page() {
        let detail =
            "Not found: Table data-proj:agent_analytics.agent_events was not found in location US";
        for path in ["/sessions", "/users"] {
            let fake = FakeHttp::new(vec![Ok((
                404,
                json!({ "error": { "code": 404, "message": detail, "errors": [{ "reason": "notFound" }] } }).to_string(),
            ))]);
            let (status, _, body) = get(bq_state(&fake, false), path).await;
            assert_eq!(status, StatusCode::BAD_GATEWAY, "{path}");
            assert!(body.contains(detail), "{path}: {body}");
            assert!(body.contains(ACTIVE), "{path}: {body}");
            assert!(!body.contains(r#"class="hint""#), "{path}: {body}");
        }
    }

    /// Run `inv` of session `s-1` with one turn per `(user, reply)`, complete, no error.
    fn run(inv: &str, turns: &[(&str, &str)]) -> ProductionRun {
        ProductionRun {
            invocation_id: inv.to_owned(),
            session_id: "s-1".to_owned(),
            trace_id: None,
            agent: "triage".to_owned(),
            started_at: ago(3_000),
            transcript: RunTranscript {
                turns: turns
                    .iter()
                    .map(|(user, reply)| TurnRecord {
                        user: (*user).to_owned(),
                        reply: (*reply).to_owned(),
                        calls: Vec::new(),
                    })
                    .collect(),
                error: None,
            },
            content_complete: true,
            tools_complete: true,
            notes: Vec::new(),
        }
    }

    /// The sessions-query reply of session `s-1`: 9 turns, 2 errors, 1 up, 1 down and
    /// 1,500,000 tokens of gemini-3.8-flash ($2.000000 with `PRICES`).
    fn head_reply() -> Result<(u16, String), String> {
        reply(
            &SESSION_FIELDS,
            vec![session_cells(
                "s-1",
                "u-1",
                "triage",
                ago(3_600),
                ago(60),
                [9, 2, 1, 1],
                Some(("gemini-3.8-flash", [1_000_000, 0, 500_000, 0])),
            )],
            false,
        )
    }

    /// Four turns: `inv-1` (completed, tools, a score, a 32-hex trace id in mixed case),
    /// `inv-2` (no completion, an error, a trace id that is not one), `inv-3` and `inv-4`.
    fn turns_reply() -> Result<(u16, String), String> {
        turn_reply(vec![
            TurnCells {
                trace_id: Some("0123456789ABCDEF0123456789abcdef"),
                end: Some(ago(2_998)),
                tools: &["search", "lookup", "search"],
                tokens: &[("gemini-3.8-flash", [1_000, 0, 200, 0])],
                scores: &[("helpful", "console", "true")],
                ..TurnCells::new("inv-1", ago(3_000))
            }
            .cells(),
            TurnCells {
                trace_id: Some("not-a-trace"),
                errors: 1,
                ..TurnCells::new("inv-2", ago(2_000))
            }
            .cells(),
            TurnCells::new("inv-3", ago(1_000)).cells(),
            TurnCells::new("inv-4", ago(500)).cells(),
        ])
    }

    /// Runs for `inv-1` (two turns), `inv-2` (an error), `inv-4` (incomplete) and `inv-9`
    /// (not on the page); none for `inv-3`.
    fn export() -> Arc<FakeExport> {
        let mut failed = run("inv-2", &[("break it", "trying")]);
        failed.transcript.error = Some("boom <tool>".to_owned());
        let mut partial = run("inv-4", &[("partial", "")]);
        partial.content_complete = false;
        partial.notes = vec!["no USER_MESSAGE_RECEIVED row".to_owned()];
        Arc::new(FakeExport {
            runs: vec![
                run("inv-1", &[("hi", "hello"), ("more?", "sure")]),
                failed,
                partial,
                run("inv-9", &[("other", "other")]),
            ],
            ..FakeExport::default()
        })
    }

    #[tokio::test]
    async fn without_bigquery_the_session_page_is_404_with_the_note() {
        let (status, _, body) =
            get(state(Arc::new(FakeSource::new()), None), "/sessions/s-1").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            body.contains("Sessions need the BigQuery trace source"),
            "{body}"
        );
        assert!(body.contains(ACTIVE), "{body}");
    }

    #[tokio::test]
    async fn an_invalid_session_id_is_400_and_queries_nothing() {
        for path in [
            "/sessions/a%0Ab".to_owned(),
            format!("/sessions/{}", "s".repeat(129)),
        ] {
            let fake = FakeHttp::new(vec![]);
            let (status, _, body) = get(bq_state(&fake, false), &path).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
            assert!(body.contains("invalid session id"), "{path}: {body}");
            assert!(fake.uris().is_empty(), "{path}");
        }
    }

    #[tokio::test]
    async fn a_session_without_activity_is_404_with_links() {
        let fake = FakeHttp::new(vec![reply(&SESSION_FIELDS, vec![], false)]);
        let (status, _, body) =
            get(bq_state(&fake, false), "/sessions/a%20%3Cb%3E?range=24h").await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert!(body.contains(ACTIVE), "{body}");
        assert!(
            body.contains("No activity for session a &lt;b&gt; in the last 24h."),
            "{body}"
        );
        assert!(
            body.contains(r#"<a href="/sessions/a%20%3Cb%3E?range=7d">Look in the last 7d</a>"#),
            "{body}"
        );
        assert!(
            body.contains(r#"<a href="/sessions">All sessions</a>"#),
            "{body}"
        );
        assert_eq!(fake.uris().len(), 1, "the turns are not queried");
        let sent = fake.body(0);
        assert_eq!(
            param_value(&sent, "session")["parameterValue"]["value"],
            json!("a <b>")
        );
        assert_eq!(
            param_value(&sent, "start")["parameterValue"]["value"],
            json!("2026-11-15 12:00:00.000000+00:00")
        );
    }

    #[tokio::test]
    async fn the_session_page_shows_the_header_totals_and_each_turn() {
        let fake = FakeHttp::new(vec![head_reply(), turns_reply()]);
        let ex = export();
        let mut st = bq_state(&fake, true);
        st.export = Some(Ok(ex.clone()));
        let (status, _, body) = get(st, "/sessions/s-1").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains(ACTIVE), "{body}");
        assert!(body.contains("<h1>Session s-1</h1>"), "{body}");
        for piece in [
            r#"<a href="/traces?session=s-1&amp;range=7d">Open in traces list</a>"#,
            "<dt>User</dt><dd>u-1</dd><dt>Agent</dt><dd>triage</dd>",
            r#"<dt>First activity</dt><dd class="mono">2026-11-16T10:59:59.999Z</dd>"#,
            "<dt>Turns</dt><dd>9</dd><dt>Tokens</dt><dd>1,500,000</dd><dt>Cost</dt><dd>$2.000000</dd><dt>Errors</dt><dd>2</dd><dt>👍 / 👎</dt><dd>1 / 1</dd>",
            "<div class=\"chat-turn\"><div class=\"bubble user\">hi\n\nmore?</div><div class=\"bubble agent\">hello\n\nsure</div>",
            r#"<p class="chat-meta"><span class="mono">2026-11-16T11:09:59.999Z</span> · latency 2,000 ms · 1,200 tokens · $0.001400 · tools: search, lookup · errors: 0 · scores: helpful=true · <a href="/traces/0123456789abcdef0123456789abcdef">trace →</a></p>"#,
            r#"<div class="bubble user">break it</div><div class="bubble agent">trying<div class="run-error">boom &lt;tool&gt;</div></div>"#,
            r#"<p class="chat-meta"><span class="mono">2026-11-16T11:26:39.999Z</span> · latency — · 0 tokens · — · errors: 1</p>"#,
            r#"<div class="chat-turn"><p class="empty">(no content logged)</p><p class="chat-meta">"#,
            r#"<div class="chat-note">content incomplete<ul><li>no USER_MESSAGE_RECEIVED row</li></ul></div>"#,
        ] {
            assert!(body.contains(piece), "{piece}\n{body}");
        }
        assert_eq!(
            body.matches("<div class=\"chat-turn\">").count(),
            4,
            "{body}"
        );
        assert_eq!(body.matches("(no content logged)").count(), 1, "{body}");
        assert!(!body.contains("other"), "only the page's runs: {body}");
        assert!(!body.contains("unavailable"), "{body}");
        let order: Vec<usize> = [
            "hello",
            "trying",
            "(no content logged)",
            "content incomplete",
        ]
        .iter()
        .map(|s| body.find(s).unwrap())
        .collect();
        assert!(
            order.windows(2).all(|w| w[0] < w[1]),
            "oldest first: {body}"
        );
        assert_eq!(ex.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            *ex.froms.lock().unwrap(),
            vec![fixed_now() - TimeDelta::days(7)],
            "runs from the window start"
        );
        let turns = fake.body(1);
        assert_eq!(
            param_value(&turns, "session")["parameterValue"]["value"],
            json!("s-1")
        );
        assert_eq!(
            param_value(&turns, "start")["parameterValue"]["value"],
            json!("2026-11-09 12:00:00.000000+00:00")
        );
    }

    #[tokio::test]
    async fn a_failed_runs_call_keeps_the_meta_lines_under_a_banner() {
        let fake = FakeHttp::new(vec![head_reply(), turns_reply()]);
        let ex = Arc::new(FakeExport {
            fail: Some("quota exceeded".to_owned()),
            ..FakeExport::default()
        });
        let mut st = bq_state(&fake, false);
        st.export = Some(Ok(ex.clone()));
        let (status, _, body) = get(st, "/sessions/s-1").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(
            body.contains(r#"<div class="banner error"><p>Conversation text unavailable: transcript source: quota exceeded</p></div>"#),
            "{body}"
        );
        assert_eq!(body.matches("<p class=\"chat-meta\">").count(), 4, "{body}");
        assert!(!body.contains("bubble"), "{body}");
        assert!(!body.contains("(no content logged)"), "{body}");
        assert_eq!(ex.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn an_export_build_error_is_the_banner_without_a_runs_call() {
        let fake = FakeHttp::new(vec![head_reply(), turns_reply()]);
        let mut st = bq_state(&fake, false);
        st.export = Some(Err(
            "transcript source: Application Default Credentials: none".to_owned(),
        ));
        let (status, _, body) = get(st, "/sessions/s-1").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(
            body.contains("Conversation text unavailable: transcript source: Application Default Credentials: none"),
            "{body}"
        );
        assert_eq!(body.matches("<p class=\"chat-meta\">").count(), 4, "{body}");
        assert_eq!(fake.uris().len(), 2);
    }

    #[tokio::test]
    async fn more_than_two_hundred_turns_show_the_first_two_hundred() {
        let ids: Vec<String> = (0..201).map(|i| format!("inv-{i:03}")).collect();
        let rows = ids
            .iter()
            .enumerate()
            .map(|(i, id)| TurnCells::new(id, ago(3_000 - i as i64)).cells())
            .collect();
        let fake = FakeHttp::new(vec![head_reply(), turn_reply(rows)]);
        let ex = Arc::new(FakeExport::default());
        let mut st = bq_state(&fake, false);
        st.export = Some(Ok(ex.clone()));
        let (status, _, body) = get(st, "/sessions/s-1").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(
            body.contains(r#"<p class="empty">showing the first 200 turns</p>"#),
            "{body}"
        );
        assert_eq!(
            body.matches("<div class=\"chat-turn\">").count(),
            200,
            "{body}"
        );
        assert_eq!(
            param_value(&fake.body(1), "limit")["parameterValue"]["value"],
            json!("201")
        );
    }

    #[tokio::test]
    async fn a_failed_session_query_is_a_502_page() {
        let fake = FakeHttp::new(vec![failed()]);
        let (status, _, body) = get(bq_state(&fake, false), "/sessions/s-1").await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
        assert!(body.contains("backend error"), "{body}");

        let fake = FakeHttp::new(vec![head_reply(), failed()]);
        let (status, _, body) = get(bq_state(&fake, false), "/sessions/s-1").await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
        assert!(body.contains(ACTIVE), "{body}");
    }

    /// The `Content-Disposition` of the download `name`.
    fn attachment(name: &str) -> String {
        format!(r#"attachment; filename="{name}""#)
    }

    /// Whether the query `body` sent a parameter `name`.
    fn has_param(body: &Value, name: &str) -> bool {
        body["queryParameters"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == json!(name))
    }

    #[tokio::test]
    async fn sessions_csv_lists_the_sessions_with_totals_and_cost() {
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
                    "@u-2",
                    "triage",
                    ago(950),
                    ago(130),
                    [0, 0, 0, 0],
                    None,
                ),
            ],
            false,
        )]);
        let path = "/sessions.csv?user=%20u-1%20&agent=triage&range=6h&after=bad&after_id=x";
        let (status, headers, body) = get(bq_state(&fake, true), path).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body,
            "session_id,user,agent,first_activity,last_activity,turns,tokens,cost_usd,cost_partial,errors,thumbs_up,thumbs_down\r\n\
             s/<1>,u-1,triage,2026-11-16T11:49:59.999Z,2026-11-16T11:58:59.999Z,3,1500015,2.000000,true,1,2,1\r\n\
             s-2,,,2026-11-16T11:44:59.999Z,2026-11-16T11:57:59.999Z,1,10,,false,0,0,0\r\n\
             s-3,'@u-2,triage,2026-11-16T11:44:09.999Z,2026-11-16T11:57:49.999Z,0,0,,false,0,0,0\r\n"
        );
        assert_eq!(headers[CONTENT_TYPE], "text/csv; charset=utf-8");
        assert_eq!(
            headers[CONTENT_DISPOSITION],
            attachment("gadk-sessions-6h-20261116T1200Z.csv").as_str()
        );
        assert!(headers.get("x-gadk-truncated").is_none());
        let sent = fake.body(0);
        assert_eq!(sent["maxResults"], json!(50000));
        for (name, value) in [
            ("start", "2026-11-16 06:00:00.000000+00:00"),
            ("user", "u-1"),
            ("agent", "triage"),
            ("limit", "5001"),
        ] {
            assert_eq!(
                param_value(&sent, name)["parameterValue"]["value"],
                json!(value),
                "{name}"
            );
        }
        assert_eq!(param_value(&sent, "session")["parameterValue"], json!({}));
        assert!(!has_param(&sent, "after") && !has_param(&sent, "after_id"));
        assert!(
            !sent["query"].as_str().unwrap().contains("@after"),
            "{sent}"
        );
    }

    #[tokio::test]
    async fn sessions_csv_defaults_to_24h_without_filters() {
        let fake = FakeHttp::new(vec![reply(&SESSION_FIELDS, vec![], false)]);
        let (status, headers, body) = get(bq_state(&fake, false), "/sessions.csv?range=30d").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body,
            "session_id,user,agent,first_activity,last_activity,turns,tokens,cost_usd,cost_partial,errors,thumbs_up,thumbs_down\r\n"
        );
        assert_eq!(
            headers[CONTENT_DISPOSITION],
            attachment("gadk-sessions-24h-20261116T1200Z.csv").as_str()
        );
        let sent = fake.body(0);
        assert_eq!(
            param_value(&sent, "start")["parameterValue"]["value"],
            json!("2026-11-15 12:00:00.000000+00:00")
        );
        for name in ["user", "agent", "session"] {
            assert_eq!(
                param_value(&sent, name)["parameterValue"],
                json!({}),
                "{name}"
            );
        }
    }

    #[tokio::test]
    async fn sessions_csv_is_truncated_past_its_caps() {
        let rows: Vec<_> = (0..5_001)
            .map(|i| {
                session_cells(
                    &format!("s-{i:04}"),
                    "u",
                    "a",
                    ago(10 + i),
                    ago(i),
                    [1, 0, 0, 0],
                    None,
                )
            })
            .collect();
        let fake = FakeHttp::new(vec![reply(&SESSION_FIELDS, rows, false)]);
        let (status, headers, body) = get(bq_state(&fake, false), "/sessions.csv").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body.matches("\r\n").count(),
            5_001,
            "a header and 5,000 sessions"
        );
        assert!(body.contains("\r\ns-4999,"), "the 5,000th session");
        assert!(!body.contains("s-5000"), "the 5,001st is cut");
        assert_eq!(headers["x-gadk-truncated"], "true");
        assert_eq!(
            headers[CONTENT_DISPOSITION],
            attachment("gadk-sessions-24h-20261116T1200Z-truncated.csv").as_str()
        );

        let fake = FakeHttp::new(vec![reply(
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
        )]);
        let (_, headers, body) = get(bq_state(&fake, false), "/sessions.csv").await;
        assert_eq!(headers["x-gadk-truncated"], "true");
        assert_eq!(
            headers[CONTENT_DISPOSITION],
            attachment("gadk-sessions-24h-20261116T1200Z-truncated.csv").as_str()
        );
        assert_eq!(body.matches("\r\n").count(), 2, "no comment rows: {body}");
    }

    #[tokio::test]
    async fn users_csv_lists_raw_user_ids_over_7d_by_default() {
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
        let (status, headers, body) = get(bq_state(&fake, true), "/users.csv").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body,
            "user,sessions,turns,errors,thumbs_up,thumbs_down,last_activity,tokens,cost_usd,cost_partial\r\n\
             a&b,2,5,1,3,0,2026-11-16T11:58:59.999Z,1000000,1.000000,false\r\n\
             ,1,1,0,0,1,2026-11-16T11:57:59.999Z,0,,false\r\n"
        );
        assert_eq!(headers[CONTENT_TYPE], "text/csv; charset=utf-8");
        assert_eq!(
            headers[CONTENT_DISPOSITION],
            attachment("gadk-users-7d-20261116T1200Z.csv").as_str()
        );
        assert!(headers.get("x-gadk-truncated").is_none());
        let sent = fake.body(0);
        assert_eq!(sent["maxResults"], json!(50000));
        assert_eq!(
            param_value(&sent, "start")["parameterValue"]["value"],
            json!("2026-11-09 12:00:00.000000+00:00")
        );
        assert_eq!(
            param_value(&sent, "limit")["parameterValue"]["value"],
            json!("5001")
        );
        assert_eq!(param_value(&sent, "agent")["parameterValue"], json!({}));
    }

    #[tokio::test]
    async fn users_csv_takes_the_agent_and_range_and_flags_truncation() {
        let fake = FakeHttp::new(vec![reply(
            &USER_FIELDS,
            vec![user_cells(Some("u"), [1, 1, 0, 0, 0], ago(1), None)],
            true,
        )]);
        let (status, headers, _) = get(
            bq_state(&fake, false),
            "/users.csv?agent=%20triage%20&range=1h",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers["x-gadk-truncated"], "true");
        assert_eq!(
            headers[CONTENT_DISPOSITION],
            attachment("gadk-users-1h-20261116T1200Z-truncated.csv").as_str()
        );
        let sent = fake.body(0);
        assert_eq!(
            param_value(&sent, "agent")["parameterValue"]["value"],
            json!("triage")
        );
        assert_eq!(
            param_value(&sent, "start")["parameterValue"]["value"],
            json!("2026-11-16 11:00:00.000000+00:00")
        );

        let rows = (0..5_001)
            .map(|i| user_cells(Some(&format!("u-{i:04}")), [1, 1, 0, 0, 0], ago(i), None))
            .collect();
        let fake = FakeHttp::new(vec![reply(&USER_FIELDS, rows, false)]);
        let (_, headers, body) = get(bq_state(&fake, false), "/users.csv").await;
        assert_eq!(
            body.matches("\r\n").count(),
            5_001,
            "a header and 5,000 users"
        );
        assert!(!body.contains("u-5000"), "the 5,001st is cut");
        assert_eq!(headers["x-gadk-truncated"], "true");
    }

    #[tokio::test]
    async fn csv_downloads_share_the_pages_errors() {
        let src = Arc::new(FakeSource::new());
        for path in ["/sessions.csv", "/users.csv"] {
            let (status, _, body) = get(state(src.clone(), None), path).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
            assert!(
                body.contains("Sessions need the BigQuery trace source"),
                "{path}: {body}"
            );
            assert!(body.contains(ACTIVE), "{body}");
        }
        let long = "u".repeat(129);
        for (path, text) in [
            (format!("/sessions.csv?user={long}"), "user is too long"),
            (format!("/sessions.csv?agent={long}"), "agent is too long"),
            (format!("/users.csv?agent={long}"), "agent is too long"),
        ] {
            let fake = FakeHttp::new(vec![]);
            let (status, headers, body) = get(bq_state(&fake, false), &path).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
            assert_eq!(headers[CONTENT_TYPE], "text/html; charset=utf-8");
            assert!(body.contains(text), "{path}: {body}");
            assert!(body.contains(ACTIVE), "{body}");
            assert!(fake.uris().is_empty(), "{path}");
        }
        for path in ["/sessions.csv", "/users.csv"] {
            let fake = FakeHttp::new(vec![failed()]);
            let (status, _, body) = get(bq_state(&fake, false), path).await;
            assert_eq!(status, StatusCode::BAD_GATEWAY, "{path}");
            assert!(body.contains("backend error"), "{path}: {body}");
            assert!(body.contains(ACTIVE), "{body}");
        }
    }

    #[tokio::test]
    async fn the_pages_link_their_csv_and_keep_their_caps() {
        let fake = FakeHttp::new(vec![
            reply(
                &SESSION_FIELDS,
                vec![session_cells(
                    "s-1",
                    "a&b",
                    "triage",
                    ago(9),
                    ago(1),
                    [1, 0, 0, 0],
                    None,
                )],
                false,
            ),
            reply(
                &USER_FIELDS,
                vec![user_cells(Some("u"), [1, 1, 0, 0, 0], ago(1), None)],
                false,
            ),
            head_reply(),
            turns_reply(),
        ]);
        let st = bq_state(&fake, false);
        let (_, _, body) = get(st.clone(), "/sessions?user=a%26b&agent=tri%20age&range=6h").await;
        assert!(
            body.contains(r#"</form><p><a href="/users?range=6h">View by user →</a></p><p class="empty">No price file: start the console with --prices &lt;file&gt; to see costs.</p><p class="csv"><a href="/sessions.csv?range=6h&amp;user=a%26b&amp;agent=tri%20age">Download CSV</a></p><table>"#),
            "{body}"
        );
        let (_, _, body) = get(st.clone(), "/users?agent=tri%20age").await;
        assert!(
            body.contains(r#"<p class="csv"><a href="/users.csv?range=7d&amp;agent=tri%20age">Download CSV</a></p><table>"#),
            "{body}"
        );
        let (status, _, body) = get(st, "/sessions/s-1").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(!body.contains("Download CSV"), "{body}");
        for i in 0..3 {
            let sent = fake.body(i);
            assert_eq!(sent["maxResults"], json!(5000), "request {i}");
            assert_eq!(
                param_value(&sent, "limit")["parameterValue"]["value"],
                json!("101"),
                "request {i}"
            );
        }
    }

    #[tokio::test]
    async fn the_users_link_follows_the_cut_note_and_empty_pages_have_none() {
        let rows = (0..101)
            .map(|i| user_cells(Some(&format!("u-{i:03}")), [1, 1, 0, 0, 0], ago(i), None))
            .collect();
        let fake = FakeHttp::new(vec![
            reply(&USER_FIELDS, rows, false),
            reply(&SESSION_FIELDS, vec![], false),
            reply(&USER_FIELDS, vec![], false),
        ]);
        let st = bq_state(&fake, false);
        let (_, _, body) = get(st.clone(), "/users?range=24h").await;
        assert!(
            body.contains(r#"<p>showing the 100 most recent users</p><p class="csv"><a href="/users.csv?range=24h">Download CSV</a></p><table>"#),
            "{body}"
        );
        for path in ["/sessions", "/users"] {
            let (status, _, body) = get(st.clone(), path).await;
            assert_eq!(status, StatusCode::OK, "{path}");
            assert!(!body.contains("Download CSV"), "{path}: {body}");
        }
    }
}
