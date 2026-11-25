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

//! The review route: `/api/v1/review/pending`.

use axum::Router;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::response::Response;
use axum::routing::get;
use gadk_analytics::review::REVIEW_CAP;

use super::{bad_request, needs_bigquery, ok, rejected, source_failure};
use crate::review::{NEEDS_BIGQUERY_REVIEW, ReviewQueue};
use crate::web::AppState;
use crate::web::review::SCORE_REQUIRED;

pub(super) fn routes() -> Router<AppState> {
    Router::new().route("/v1/review/pending", get(pending))
}

/// `GET /api/v1/review/pending`: the pending traces of the `/review` queue (same parameters and
/// 400 texts, with the loaded rubrics), at most [`REVIEW_CAP`]; a queue needs a `score` or a
/// `rubric`.
async fn pending(
    State(state): State<AppState>,
    params: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Response {
    let Some(usage) = &state.usage else {
        return needs_bigquery(NEEDS_BIGQUERY_REVIEW);
    };
    let Query(params) = match params {
        Ok(params) => params,
        Err(e) => return rejected(&e.body_text()),
    };
    let rubrics = state.rubrics.as_deref();
    let queue = match ReviewQueue::from_params(&params, rubrics) {
        Ok(Some(q)) => q,
        Ok(None) => return bad_request(SCORE_REQUIRED),
        Err(text) => return bad_request(&text),
    };
    match usage
        .pending(&queue.analytics(), &queue.names(rubrics), (state.now)())
        .await
    {
        Ok(rows) => {
            let truncated = rows.len() > REVIEW_CAP;
            ok(&rows[..rows.len().min(REVIEW_CAP)], truncated, None)
        }
        Err(e) => source_failure(&e),
    }
}
