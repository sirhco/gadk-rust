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

//! Tests of the API skeleton, and the helpers the route tests share.

use std::sync::Arc;

use axum::body::Body;
use axum::response::Response;
use gadk_analytics::AnalyticsSource;
use gadk_analytics::cost::Usage;
use gadk_analytics::testutil::FakeHttp;
use gadk_bigquery::{BigQueryClient, NoAuth};
use http::header::{
    CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, HOST, ORIGIN, X_CONTENT_TYPE_OPTIONS,
};
use http::{HeaderMap, Request, StatusCode};
use serde_json::{Value, json};

use super::openapi_tests::{METHOD_NOT_ALLOWED, assert_matches_pointer};
use super::{Priced, cost_sum, ok, rejected, source_failure};
use crate::prices::PriceTable;
use crate::traces::{BigQueryTable, SourceError};
use crate::web::AppState;
use crate::web::guard::CSP;
use crate::web::testutil::{FakeSource, PORT, get, send, state};

/// The price file of the API tests: `gemini-3.8-flash` only.
pub(super) const PRICES: &str =
    "- model: gemini-3.8-flash\n  input: 0.30\n  output: 2.50\n  cached_input: 0.075\n";

/// The test state with the BigQuery usage source over `fake`, and [`PRICES`] when `prices`.
pub(super) fn bq_state(fake: &Arc<FakeHttp>, prices: bool) -> AppState {
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

/// GET `path` through the router: the status, the headers and the body as JSON.
pub(super) async fn api_get(state: AppState, path: &str) -> (StatusCode, HeaderMap, Value) {
    let (status, headers, body) = get(state, path).await;
    let json = serde_json::from_str(&body).unwrap_or_else(|e| panic!("{path}: {e}: {body}"));
    (status, headers, json)
}

/// `{"error": {"kind": kind, "message": message, "hint": null}}`.
pub(super) fn error_body(kind: &str, message: &str) -> Value {
    json!({ "error": { "kind": kind, "message": message, "hint": null } })
}

/// Asserts the headers of every API response: JSON plus the guard's security headers.
pub(super) fn assert_api_headers(headers: &HeaderMap) {
    assert_eq!(headers[CONTENT_TYPE], "application/json");
    assert_eq!(headers[CONTENT_SECURITY_POLICY], CSP);
    assert_eq!(headers[X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert_eq!(headers[CACHE_CONTROL], "no-store");
}

fn fresh() -> AppState {
    state(Arc::new(FakeSource::new()), None)
}

async fn body_json(resp: Response) -> (StatusCode, String, Value) {
    let status = resp.status();
    let ct = resp.headers()[CONTENT_TYPE].to_str().unwrap().to_owned();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, ct, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn the_envelope_has_data_truncated_and_next() {
    let (status, ct, body) = body_json(ok(json!([1, 2]), true, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ct, "application/json");
    assert_eq!(
        body,
        json!({ "data": [1, 2], "truncated": true, "next": null })
    );
    let (_, _, body) = body_json(ok("x", false, Some("/api/v1/x?a=1".to_owned()))).await;
    assert_eq!(
        body,
        json!({ "data": "x", "truncated": false, "next": "/api/v1/x?a=1" })
    );
}

#[tokio::test]
async fn source_failures_are_their_own_json_with_404_or_502() {
    let (status, ct, body) = body_json(source_failure(&SourceError::NotFound)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(ct, "application/json");
    assert_eq!(body, error_body("not_found", "trace not found"));
    let e = SourceError::bigquery(Some(403), None, "denied");
    let (status, _, body) = body_json(source_failure(&e)).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(
        body,
        json!({ "error": {
            "kind": "bigquery",
            "message": "BigQuery: HTTP 403: denied",
            "hint": e.hint().unwrap(),
        } })
    );
}

#[tokio::test]
async fn a_rejection_is_a_json_400() {
    let (status, _, body) = body_json(rejected("bad bytes")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body,
        error_body("bad_request", "invalid request: bad bytes")
    );
}

#[tokio::test]
async fn an_unknown_api_path_is_a_json_404_under_the_guards() {
    let fake = FakeHttp::new(Vec::new());
    for st in [fresh(), bq_state(&fake, true)] {
        for path in ["/api", "/api/", "/api/v1/nope", "/api/v1/usage/%3Cx%3E"] {
            let (status, headers, body) = api_get(st.clone(), path).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
            assert_api_headers(&headers);
            assert_eq!(body, error_body("not_found", "no such API route"), "{path}");
        }
    }
    assert!(fake.uris().is_empty(), "nothing is queried");
    // Page paths keep the HTML 404.
    let (status, headers, _) = get(fresh(), "/apix").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(headers[CONTENT_TYPE], "text/html; charset=utf-8");
}

#[tokio::test]
async fn a_write_method_past_the_guard_is_a_json_405() {
    let origin = format!("http://127.0.0.1:{PORT}");
    for method in ["POST", "PUT", "DELETE", "PATCH"] {
        let req = Request::builder()
            .method(method)
            .uri("/api/v1/nope")
            .header(HOST, format!("127.0.0.1:{PORT}"))
            .header(ORIGIN, origin.as_str())
            .body(Body::empty())
            .unwrap();
        let (status, headers, body) = send(fresh(), req).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{method}");
        assert_api_headers(&headers);
        assert_eq!(
            headers[http::header::ALLOW],
            "GET, HEAD, OPTIONS",
            "{method}"
        );
        let body: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            body,
            error_body("method_not_allowed", "the API is read-only"),
            "{method}"
        );
        assert_matches_pointer(METHOD_NOT_ALLOWED, &body);
    }
}

#[tokio::test]
async fn a_head_on_an_unknown_api_path_is_the_404_status() {
    let req = Request::builder()
        .method("HEAD")
        .uri("/api/v1/nope")
        .header(HOST, format!("127.0.0.1:{PORT}"))
        .body(Body::empty())
        .unwrap();
    let (status, _, body) = send(fresh(), req).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.is_empty(), "{body}");
}

#[tokio::test]
async fn a_cross_origin_post_gets_the_guards_html_403() {
    let req = Request::post("/api/v1/nope")
        .header(HOST, format!("127.0.0.1:{PORT}"))
        .header(ORIGIN, "http://localhost:5173")
        .body(Body::empty())
        .unwrap();
    let (status, headers, body) = send(fresh(), req).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(headers[CONTENT_TYPE], "text/html; charset=utf-8");
    assert!(body.contains("forbidden: cross-origin request"), "{body}");
}

#[tokio::test]
async fn a_wrong_host_gets_the_guards_html_421() {
    let req = Request::get("/api/v1/nope")
        .header(HOST, "evil.example:8765")
        .body(Body::empty())
        .unwrap();
    let (status, headers, _) = send(fresh(), req).await;
    assert_eq!(status, StatusCode::MISDIRECTED_REQUEST);
    assert_eq!(headers[CONTENT_TYPE], "text/html; charset=utf-8");
}

#[test]
fn priced_rows_flatten_the_cost_fields() {
    #[derive(serde::Serialize)]
    struct Row {
        model: &'static str,
    }
    let prices = PriceTable::from_yaml(PRICES).unwrap();
    let flash = Usage::new(1_000_000, 0, 0, 0);
    let row = Row { model: "m" };
    let priced = |prices: Option<&PriceTable>, models: &[&'static str]| {
        let cost = cost_sum(prices, models.iter().map(|m| (*m, &flash)));
        serde_json::to_value(Priced::new(&row, cost)).unwrap()
    };

    assert_eq!(
        priced(None, &["gemini-3.8-flash"]),
        json!({ "model": "m", "cost_usd": null, "cost_partial": false })
    );
    assert_eq!(
        priced(Some(&prices), &["gemini-3.8-flash"]),
        json!({ "model": "m", "cost_usd": 0.3, "cost_partial": false })
    );
    assert_eq!(
        priced(Some(&prices), &["gemini-3.8-flash", "other-model"]),
        json!({ "model": "m", "cost_usd": 0.3, "cost_partial": true })
    );
    assert_eq!(
        priced(Some(&prices), &["other-model"]),
        json!({ "model": "m", "cost_usd": null, "cost_partial": false })
    );
    assert_eq!(
        priced(Some(&prices), &[]),
        json!({ "model": "m", "cost_usd": null, "cost_partial": false })
    );
}
