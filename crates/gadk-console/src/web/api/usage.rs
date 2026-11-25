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

//! The usage routes: `/api/v1/usage/breakdown`, `/api/v1/usage/trend` and
//! `/api/v1/usage/top-traces`.

use axum::Router;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::response::Response;
use axum::routing::get;
use chrono::{DateTime, Utc};
use gadk_analytics::AnalyticsSource;
use gadk_analytics::usage::{ModelCalls, TOP_TRACES};
use serde_json::json;

use super::{Priced, cost_sum, needs_bigquery, ok, rejected, source_failure};
use crate::prices::PriceTable;
use crate::web::AppState;
use crate::web::pages::usage::NEEDS_BIGQUERY;
use crate::web::review::first;
use crate::web::usage::usage_range;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/usage/breakdown", get(breakdown))
        .route("/v1/usage/trend", get(trend))
        .route("/v1/usage/top-traces", get(top_traces))
}

/// The usage source and the window of one request.
struct Window<'a> {
    source: &'a AnalyticsSource,
    /// The trend bucket width of the range.
    bucket_seconds: i64,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
}

/// The usage source and the window `[start, end)` of the `range` parameter (as on `/usage`),
/// or the error response: `needs_bigquery` without the BigQuery source, else a rejected query
/// string.
#[allow(clippy::result_large_err)] // the Err is the response the handler returns as is
fn window(
    state: &AppState,
    params: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Window<'_>, Response> {
    let Some(source) = state.usage.as_deref() else {
        return Err(needs_bigquery(NEEDS_BIGQUERY));
    };
    let Query(params) = params.map_err(|e| rejected(&e.body_text()))?;
    let range = usage_range(Some(first(&params, "range")));
    let end = (state.now)();
    let (bucket_seconds, _, _) = range.bucket();
    Ok(Window {
        source,
        bucket_seconds,
        start: end - range.duration(),
        end,
    })
}

/// The cost of one group's calls.
fn calls_cost(prices: Option<&PriceTable>, calls: &ModelCalls) -> gadk_analytics::cost::CostSum {
    cost_sum(prices, [(calls.model.as_str(), &calls.usage)])
}

/// `GET /api/v1/usage/breakdown?range=`: the breakdown, each cell with its cost.
async fn breakdown(
    State(state): State<AppState>,
    params: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Response {
    let w = match window(&state, params) {
        Ok(w) => w,
        Err(resp) => return resp,
    };
    match w.source.breakdown(w.start, w.end).await {
        Ok(b) => {
            let prices = state.prices.as_deref();
            let cells: Vec<_> = b
                .cells
                .iter()
                .map(|c| Priced::new(c, calls_cost(prices, &c.calls)))
                .collect();
            ok(
                json!({ "cells": cells, "truncated": b.truncated }),
                b.truncated,
                None,
            )
        }
        Err(e) => source_failure(&e),
    }
}

/// `GET /api/v1/usage/trend?range=`: the window start, the bucket width and the trend cells,
/// each with its cost.
async fn trend(
    State(state): State<AppState>,
    params: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Response {
    let w = match window(&state, params) {
        Ok(w) => w,
        Err(resp) => return resp,
    };
    match w.source.trend(w.start, w.end, w.bucket_seconds).await {
        Ok(t) => {
            let prices = state.prices.as_deref();
            let cells: Vec<_> = t
                .cells
                .iter()
                .map(|c| Priced::new(c, calls_cost(prices, &c.cell.calls)))
                .collect();
            ok(
                json!({ "start": w.start, "bucket_seconds": w.bucket_seconds, "cells": cells }),
                t.truncated,
                None,
            )
        }
        Err(e) => source_failure(&e),
    }
}

/// `GET /api/v1/usage/top-traces?range=`: the top traces by tokens, each with the cost of its
/// models' calls.
async fn top_traces(
    State(state): State<AppState>,
    params: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Response {
    let w = match window(&state, params) {
        Ok(w) => w,
        Err(resp) => return resp,
    };
    match w.source.top_traces(w.start, w.end, TOP_TRACES).await {
        Ok(top) => {
            let prices = state.prices.as_deref();
            let rows: Vec<_> = top
                .iter()
                .map(|t| {
                    let cost = cost_sum(
                        prices,
                        t.models.iter().map(|m| (m.model.as_str(), &m.usage)),
                    );
                    Priced::new(t, cost)
                })
                .collect();
            ok(rows, false, None)
        }
        Err(e) => source_failure(&e),
    }
}
