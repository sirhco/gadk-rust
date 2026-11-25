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

//! The read-only JSON API under `/api`: the `{data, truncated, next}` envelope, JSON errors,
//! the unknown-route 404 and the 405, and the cost fields of priced rows.

use axum::Json;
use axum::Router;
use axum::response::{IntoResponse, Response};
use gadk_analytics::cost::{CostSum, Usage};
use http::{Method, StatusCode};
use serde::Serialize;
use serde_json::json;

use super::AppState;
use crate::prices::{PriceTable, cost_of};
use crate::traces::SourceError;

mod openapi;
mod prompts;
mod review;
mod scores;
mod sessions;
mod usage;

/// The message of the 404 for an unknown `/api` path.
pub(crate) const NO_SUCH_ROUTE: &str = "no such API route";
/// The message of the 405 for a write method on an API path.
pub(crate) const READ_ONLY: &str = "the API is read-only";

/// Every API path, relative to `/api`: the OpenAPI document lists exactly these.
///
/// Every route constant must be listed here: the path-sync test compares the OpenAPI document
/// with this list, not with the router.
#[cfg(test)]
pub(crate) const PATHS: [&str; 10] = [
    usage::BREAKDOWN,
    usage::TREND,
    usage::TOP,
    sessions::SESSIONS,
    sessions::TURNS,
    sessions::USERS,
    prompts::VERSIONS,
    scores::SCORES,
    review::PENDING,
    openapi::PATH,
];

/// The routes of the API, relative to `/api`.
fn routes() -> Router<AppState> {
    Router::new()
        .merge(usage::routes())
        .merge(sessions::routes())
        .merge(prompts::routes())
        .merge(scores::routes())
        .merge(review::routes())
        .merge(openapi::routes())
}

/// The router nested at `/api`: [`routes`], a JSON 405 for other methods on a known route, and
/// a JSON fallback for unknown paths (405 for a write method, else 404).
pub(crate) fn router() -> Router<AppState> {
    routes()
        .method_not_allowed_fallback(method_not_allowed)
        .fallback(fallback)
}

/// `{"data": data, "truncated": truncated, "next": next}` with status 200.
pub(crate) fn ok<T: Serialize>(data: T, truncated: bool, next: Option<String>) -> Response {
    Json(json!({ "data": data, "truncated": truncated, "next": next })).into_response()
}

/// `{"error": {"kind": kind, "message": message, "hint": null}}` with `status`.
pub(crate) fn api_error(status: StatusCode, kind: &str, message: &str) -> Response {
    (
        status,
        Json(json!({ "error": { "kind": kind, "message": message, "hint": null } })),
    )
        .into_response()
}

/// The 400 `bad_request` error with `message`.
pub(crate) fn bad_request(message: &str) -> Response {
    api_error(StatusCode::BAD_REQUEST, "bad_request", message)
}

/// The 404 `needs_bigquery` error with the page's note `message`.
pub(crate) fn needs_bigquery(message: &str) -> Response {
    api_error(StatusCode::NOT_FOUND, "needs_bigquery", message)
}

/// The 404 `not_found` error with `message`.
pub(crate) fn not_found(message: &str) -> Response {
    api_error(StatusCode::NOT_FOUND, "not_found", message)
}

/// A rejected extractor: the 400 `bad_request` error `invalid request: <text>`.
pub(crate) fn rejected(text: &str) -> Response {
    bad_request(&format!("invalid request: {text}"))
}

/// A failed query: `{"error": <the SourceError>}`, 404 for [`SourceError::NotFound`] and 502
/// otherwise.
pub(crate) fn source_failure(e: &SourceError) -> Response {
    let status = match e {
        SourceError::NotFound => StatusCode::NOT_FOUND,
        _ => StatusCode::BAD_GATEWAY,
    };
    (status, Json(json!({ "error": e }))).into_response()
}

/// The 405 `method_not_allowed` error, with `Allow: GET, HEAD, OPTIONS`.
async fn method_not_allowed() -> Response {
    let mut response = api_error(
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
        READ_ONLY,
    );
    response.headers_mut().insert(
        http::header::ALLOW,
        http::HeaderValue::from_static("GET, HEAD, OPTIONS"),
    );
    response
}

/// An unknown `/api` path: 404 `not_found` for GET and HEAD, else the 405 (OPTIONS never gets
/// here: the CORS layer answers it with 204).
async fn fallback(method: Method) -> Response {
    if method == Method::GET || method == Method::HEAD {
        not_found(NO_SUCH_ROUTE)
    } else {
        method_not_allowed().await
    }
}

/// A row with its cost: the row's own JSON fields plus `cost_usd` (the sum of the priced calls,
/// `null` when no call had a price) and `cost_partial` (some calls priced, some not).
#[derive(Debug, Serialize)]
pub(crate) struct Priced<'a, T> {
    #[serde(flatten)]
    row: &'a T,
    cost_usd: Option<f64>,
    cost_partial: bool,
}

impl<'a, T> Priced<'a, T> {
    /// `row` with the cost `cost`.
    pub(crate) fn new(row: &'a T, cost: CostSum) -> Self {
        Self {
            row,
            cost_usd: cost.priced.then_some(cost.usd),
            cost_partial: cost.partial(),
        }
    }
}

/// The cost of the calls `calls` (model and tokens), each priced with [`cost_of`].
pub(crate) fn cost_sum<'a>(
    prices: Option<&PriceTable>,
    calls: impl IntoIterator<Item = (&'a str, &'a Usage)>,
) -> CostSum {
    let mut sum = CostSum::default();
    for (model, usage) in calls {
        sum.add(cost_of(prices, model, usage));
    }
    sum
}

#[cfg(test)]
mod openapi_tests;
#[cfg(test)]
mod prompts_tests;
#[cfg(test)]
mod review_tests;
#[cfg(test)]
mod scores_tests;
#[cfg(test)]
mod sessions_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod usage_tests;
