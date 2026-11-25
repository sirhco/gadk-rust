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

//! The prompt-version route: `/api/v1/prompts/{name}/versions`.

use axum::Router;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::response::Response;
use axum::routing::get;
use serde_json::json;

use super::{Priced, cost_sum, needs_bigquery, ok, rejected, source_failure};
use crate::web::AppState;
use crate::web::pages::prompt::NEEDS_BIGQUERY_METRICS;
use crate::web::prompts::metrics_range;
use crate::web::review::first;

/// The prompt versions' path, relative to `/api`.
pub(super) const VERSIONS: &str = "/v1/prompts/{name}/versions";

pub(super) fn routes() -> Router<AppState> {
    Router::new().route(VERSIONS, get(versions))
}

/// `GET /api/v1/prompts/{name}/versions?range=`: the production metrics of prompt `name` over
/// the range (7d by default, as on `/prompts/{name}`), each stats row with the cost of its
/// version's calls and each model row with its own, plus the score feedback per version. The
/// prompt store is not read: an unknown name has no rows.
async fn versions(
    State(state): State<AppState>,
    name: Result<Path<String>, PathRejection>,
    params: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Response {
    let Some(usage) = &state.usage else {
        return needs_bigquery(NEEDS_BIGQUERY_METRICS);
    };
    let Path(name) = match name {
        Ok(name) => name,
        Err(e) => return rejected(&e.body_text()),
    };
    let Query(params) = match params {
        Ok(params) => params,
        Err(e) => return rejected(&e.body_text()),
    };
    let range = metrics_range(Some(first(&params, "range")));
    let end = (state.now)();
    let start = end - range.duration();
    let metrics = match usage.version_metrics(&name, start, end).await {
        Ok(m) => m,
        Err(e) => return source_failure(&e),
    };
    let feedback = match usage.version_feedback(&name, start, end).await {
        Ok(f) => f,
        Err(e) => return source_failure(&e),
    };
    let prices = state.prices.as_deref();
    let stats: Vec<_> = metrics
        .stats
        .iter()
        .map(|s| {
            let calls = metrics
                .models
                .iter()
                .filter(|vm| vm.version == s.version)
                .map(|vm| (vm.calls.model.as_str(), &vm.calls.usage));
            Priced::new(s, cost_sum(prices, calls))
        })
        .collect();
    let models: Vec<_> = metrics
        .models
        .iter()
        .map(|vm| {
            Priced::new(
                vm,
                cost_sum(prices, [(vm.calls.model.as_str(), &vm.calls.usage)]),
            )
        })
        .collect();
    let truncated = metrics.truncated || feedback.truncated;
    ok(
        json!({
            "metrics": { "stats": stats, "models": models, "truncated": metrics.truncated },
            "feedback": feedback,
        }),
        truncated,
        None,
    )
}
