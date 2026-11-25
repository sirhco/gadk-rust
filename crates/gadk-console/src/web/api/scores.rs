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

//! The trace-score route: `/api/v1/traces/{id}/scores`.

use axum::Router;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::response::Response;
use axum::routing::get;

use super::{needs_bigquery, ok, rejected, source_failure};
use crate::traces::{SourceError, TraceId};
use crate::web::AppState;
use crate::web::pages::trace::NEEDS_BIGQUERY_SCORES;
use crate::web::traces::score_lookup;

/// The trace scores' path, relative to `/api`.
pub(super) const SCORES: &str = "/v1/traces/{id}/scores";

pub(super) fn routes() -> Router<AppState> {
    Router::new().route(SCORES, get(scores))
}

/// `GET /api/v1/traces/{id}/scores`: the scores of trace `id` and of the invocations its spans
/// name, as on `/traces/{id}`. An invalid id is the trace page's not-found error without a
/// lookup; the trace is read from the trace source first.
async fn scores(
    State(state): State<AppState>,
    id: Result<Path<String>, PathRejection>,
) -> Response {
    let Some(usage) = &state.usage else {
        return needs_bigquery(NEEDS_BIGQUERY_SCORES);
    };
    let Path(id) = match id {
        Ok(id) => id,
        Err(e) => return rejected(&e.body_text()),
    };
    let Some(id) = TraceId::parse(&id) else {
        return source_failure(&SourceError::NotFound);
    };
    let detail = match state.source.get(&id).await {
        Ok(detail) => detail,
        Err(e) => return source_failure(&e),
    };
    let (ids, start) = score_lookup(&detail.spans, state.now);
    match usage.trace_scores(id.as_str(), &ids, start).await {
        Ok(scores) => {
            let truncated = scores.truncated;
            ok(scores, truncated, None)
        }
        Err(e) => source_failure(&e),
    }
}
