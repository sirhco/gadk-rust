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

//! The session routes: `/api/v1/sessions`, `/api/v1/sessions/{id}/turns` and
//! `/api/v1/users`.

use axum::Router;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::response::Response;
use axum::routing::get;
use gadk_analytics::cost::{CostSum, Usage};
use gadk_analytics::sessions::{ROW_CAP, SESSION_CAP, TURN_CAP, USER_CAP};
use serde_json::json;

use super::{Priced, bad_request, needs_bigquery, not_found, ok, rejected, source_failure};
use crate::prices::PriceTable;
use crate::web::AppState;
use crate::web::pages::sessions::totals;
use crate::web::sessions::{
    NEEDS_BIGQUERY_SESSIONS, next_link, session_query, turns_query, user_query,
};

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/sessions", get(sessions))
        .route("/v1/sessions/{id}/turns", get(turns))
        .route("/v1/users", get(users))
}

/// The cost of a row's per-model tokens, as the session pages and CSVs price it.
fn tokens_cost(prices: Option<&PriceTable>, tokens: &[(String, Usage)]) -> CostSum {
    totals(tokens, prices).1
}

/// `GET /api/v1/sessions`: the `/sessions` rows (same parameters and 400 texts) with their
/// cost; `next` is the API link to the page after the last row when there is one.
async fn sessions(
    State(state): State<AppState>,
    params: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Response {
    let Some(usage) = &state.usage else {
        return needs_bigquery(NEEDS_BIGQUERY_SESSIONS);
    };
    let Query(params) = match params {
        Ok(params) => params,
        Err(e) => return rejected(&e.body_text()),
    };
    let q = match session_query(&params, (state.now)()) {
        Ok(q) => q,
        Err(text) => return bad_request(&text),
    };
    let p = match usage.sessions(&q, SESSION_CAP, ROW_CAP).await {
        Ok(p) => p,
        Err(e) => return source_failure(&e),
    };
    let prices = state.prices.as_deref();
    let rows: Vec<_> = p
        .sessions
        .iter()
        .map(|s| Priced::new(s, tokens_cost(prices, &s.tokens)))
        .collect();
    let next = p
        .sessions
        .last()
        .filter(|_| p.has_next)
        .map(|last| format!("/api/v1{}", next_link(&q, &last.session_id, last.last)));
    ok(rows, p.truncated, next)
}

/// `GET /api/v1/sessions/{id}/turns?range=`: the session's header row and its first
/// [`TURN_CAP`] turns, each with its cost, over the range (7d by default) as on
/// `/sessions/{id}`.
async fn turns(
    State(state): State<AppState>,
    id: Result<Path<String>, PathRejection>,
    params: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Response {
    let Some(usage) = &state.usage else {
        return needs_bigquery(NEEDS_BIGQUERY_SESSIONS);
    };
    let Path(id) = match id {
        Ok(id) => id,
        Err(e) => return rejected(&e.body_text()),
    };
    let Query(params) = match params {
        Ok(params) => params,
        Err(e) => return rejected(&e.body_text()),
    };
    let q = match turns_query(&id, &params, (state.now)()) {
        Ok(q) => q,
        Err(text) => return bad_request(text),
    };
    let head = match usage.sessions(&q, SESSION_CAP, ROW_CAP).await {
        Ok(p) => p.sessions.into_iter().next(),
        Err(e) => return source_failure(&e),
    };
    let Some(head) = head else {
        return not_found(&format!(
            "no activity for session {id} in the last {}",
            q.range.as_str()
        ));
    };
    let start = q.end - q.range.duration();
    let turns = match usage.session_turns(&id, start, q.end).await {
        Ok(turns) => turns,
        Err(e) => return source_failure(&e),
    };
    let prices = state.prices.as_deref();
    let shown: Vec<_> = turns
        .iter()
        .take(TURN_CAP)
        .map(|t| Priced::new(t, tokens_cost(prices, &t.tokens)))
        .collect();
    ok(
        json!({
            "session": Priced::new(&head, tokens_cost(prices, &head.tokens)),
            "turns": shown,
        }),
        turns.len() > TURN_CAP,
        None,
    )
}

/// `GET /api/v1/users`: the `/users` rows (same parameters and 400 texts) with their cost.
async fn users(
    State(state): State<AppState>,
    params: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Response {
    let Some(usage) = &state.usage else {
        return needs_bigquery(NEEDS_BIGQUERY_SESSIONS);
    };
    let Query(params) = match params {
        Ok(params) => params,
        Err(e) => return rejected(&e.body_text()),
    };
    let q = match user_query(&params, (state.now)()) {
        Ok(q) => q,
        Err(text) => return bad_request(&text),
    };
    match usage.users(&q, USER_CAP, ROW_CAP).await {
        Ok(p) => {
            let prices = state.prices.as_deref();
            let rows: Vec<_> = p
                .users
                .iter()
                .map(|u| Priced::new(u, tokens_cost(prices, &u.tokens)))
                .collect();
            ok(rows, p.more || p.truncated, None)
        }
        Err(e) => source_failure(&e),
    }
}
