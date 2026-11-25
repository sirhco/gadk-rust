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

//! Tests of the usage API routes.

use std::sync::Arc;

use axum::body::Body;
use gadk_analytics::testutil::{FakeHttp, TREND_FIELDS, param_value, reply};
use http::header::{HOST, ORIGIN};
use http::{Request, StatusCode};
use serde_json::{Value, json};

use super::openapi_tests::{METHOD_NOT_ALLOWED, assert_matches_pointer};
use super::tests::{api_get, assert_api_headers, bq_state, error_body};
use crate::web::pages::usage::NEEDS_BIGQUERY;
use crate::web::testutil::{FakeSource, PORT, send, state};

/// The 403 reply of a denied BigQuery query.
pub(super) fn denied() -> Result<(u16, String), String> {
    Ok((
        403,
        json!({ "error": { "code": 403, "message": "Access Denied", "errors": [{ "reason": "accessDenied" }] } }).to_string(),
    ))
}

/// The 502 body of [`denied`].
pub(super) fn denied_body() -> Value {
    json!({ "error": {
        "kind": "bigquery",
        "message": "BigQuery: HTTP 403: Access Denied",
        "hint": "needs roles/bigquery.dataViewer and roles/bigquery.jobUser",
    } })
}

fn breakdown_reply(page_token: bool) -> Result<(u16, String), String> {
    let fields = [
        ("model", "STRING"),
        ("agent", "STRING"),
        ("prompt", "STRING"),
        ("calls", "INT64"),
        ("input", "INT64"),
        ("cached", "INT64"),
        ("output", "INT64"),
        ("reasoning", "INT64"),
    ];
    reply(
        &fields,
        vec![
            vec![
                json!("gemini-3.8-flash"),
                json!("triage"),
                json!("triage@2"),
                json!("3"),
                json!("3000"),
                json!("1000"),
                json!("600"),
                json!("0"),
            ],
            vec![
                json!("other-model"),
                json!("writer"),
                Value::Null,
                json!("2"),
                json!("500"),
                json!("0"),
                json!("100"),
                json!("0"),
            ],
        ],
        page_token,
    )
}

fn top_reply() -> Result<(u16, String), String> {
    let fields = [
        ("trace_id", "STRING"),
        ("total", "INT64"),
        ("model", "STRING"),
        ("calls", "INT64"),
        ("input", "INT64"),
        ("cached", "INT64"),
        ("output", "INT64"),
        ("reasoning", "INT64"),
    ];
    reply(
        &fields,
        vec![
            vec![
                json!("0af7651916cd43dd8448eb211c80319c"),
                json!("4200"),
                json!("gemini-3.8-flash"),
                json!("3"),
                json!("3000"),
                json!("1000"),
                json!("600"),
                json!("0"),
            ],
            vec![
                json!("0af7651916cd43dd8448eb211c80319c"),
                json!("4200"),
                json!("other-model"),
                json!("2"),
                json!("500"),
                json!("0"),
                json!("100"),
                json!("0"),
            ],
        ],
        false,
    )
}

fn trend_reply(page_token: bool) -> Result<(u16, String), String> {
    reply(
        &TREND_FIELDS,
        vec![
            vec![
                json!("0"),
                json!("gemini-3.8-flash"),
                json!("triage"),
                json!("triage@2"),
                json!("3"),
                json!("3000"),
                json!("1000"),
                json!("600"),
                json!("0"),
            ],
            vec![
                json!("23"),
                json!("other-model"),
                json!("writer"),
                Value::Null,
                json!("2"),
                json!("500"),
                json!("0"),
                json!("100"),
                json!("0"),
            ],
        ],
        page_token,
    )
}

/// The `calls` object of the priced fixture group.
fn flash_calls() -> Value {
    json!({
        "model": "gemini-3.8-flash",
        "calls": 3,
        "usage": { "input": 3000, "cached": 1000, "output": 600, "reasoning": 0 },
    })
}

/// The `calls` object of the unpriced fixture group.
fn other_calls() -> Value {
    json!({
        "model": "other-model",
        "calls": 2,
        "usage": { "input": 500, "cached": 0, "output": 100, "reasoning": 0 },
    })
}

/// $0.002175: 2,000 uncached input, 1,000 cached and 600 output tokens of `gemini-3.8-flash`.
const FLASH_COST: f64 = 0.002175;

fn start_param(fake: &FakeHttp, i: usize) -> Value {
    param_value(&fake.body(i), "start")["parameterValue"]["value"].clone()
}

#[tokio::test]
async fn breakdown_is_the_cells_with_their_cost() {
    let fake = FakeHttp::new(vec![breakdown_reply(false)]);
    let (status, headers, body) = api_get(bq_state(&fake, true), "/api/v1/usage/breakdown").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_api_headers(&headers);
    assert_eq!(
        body,
        json!({
            "data": {
                "cells": [
                    {
                        "agent": "triage",
                        "prompt": "triage@2",
                        "calls": flash_calls(),
                        "cost_usd": FLASH_COST,
                        "cost_partial": false,
                    },
                    {
                        "agent": "writer",
                        "prompt": null,
                        "calls": other_calls(),
                        "cost_usd": null,
                        "cost_partial": false,
                    },
                ],
                "truncated": false,
            },
            "truncated": false,
            "next": null,
        })
    );
    // The default range is 24h.
    assert_eq!(
        start_param(&fake, 0),
        json!("2026-11-15 12:00:00.000000+00:00")
    );
    assert_eq!(
        param_value(&fake.body(0), "end")["parameterValue"]["value"],
        json!("2026-11-16 12:00:00.000000+00:00")
    );
}

#[tokio::test]
async fn without_prices_every_cost_is_null() {
    let fake = FakeHttp::new(vec![breakdown_reply(true)]);
    let (status, _, body) =
        api_get(bq_state(&fake, false), "/api/v1/usage/breakdown?range=7d").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    for cell in body["data"]["cells"].as_array().unwrap() {
        assert_eq!(cell["cost_usd"], Value::Null, "{cell}");
        assert_eq!(cell["cost_partial"], false, "{cell}");
    }
    assert_eq!(body["truncated"], true, "{body}");
    assert_eq!(body["data"]["truncated"], true, "{body}");
    assert_eq!(
        start_param(&fake, 0),
        json!("2026-11-09 12:00:00.000000+00:00")
    );
}

#[tokio::test]
async fn trend_has_the_start_the_bucket_width_and_priced_cells() {
    let fake = FakeHttp::new(vec![trend_reply(false)]);
    let (status, headers, body) = api_get(bq_state(&fake, true), "/api/v1/usage/trend").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_api_headers(&headers);
    assert_eq!(
        body,
        json!({
            "data": {
                "start": "2026-11-15T12:00:00Z",
                "bucket_seconds": 3600,
                "cells": [
                    {
                        "bucket": 0,
                        "cell": { "agent": "triage", "prompt": "triage@2", "calls": flash_calls() },
                        "cost_usd": FLASH_COST,
                        "cost_partial": false,
                    },
                    {
                        "bucket": 23,
                        "cell": { "agent": "writer", "prompt": null, "calls": other_calls() },
                        "cost_usd": null,
                        "cost_partial": false,
                    },
                ],
            },
            "truncated": false,
            "next": null,
        })
    );
    assert_eq!(
        param_value(&fake.body(0), "bucket")["parameterValue"]["value"],
        json!("3600")
    );
}

#[tokio::test]
async fn trend_takes_the_range_and_reports_truncation() {
    let fake = FakeHttp::new(vec![trend_reply(true)]);
    let (status, _, body) = api_get(bq_state(&fake, true), "/api/v1/usage/trend?range=7d").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["start"], "2026-11-09T12:00:00Z");
    assert_eq!(body["data"]["bucket_seconds"], 21600);
    assert_eq!(body["truncated"], true);
    assert_eq!(
        param_value(&fake.body(0), "bucket")["parameterValue"]["value"],
        json!("21600")
    );
}

#[tokio::test]
async fn top_traces_are_priced_and_never_truncated() {
    let fake = FakeHttp::new(vec![top_reply()]);
    let (status, headers, body) =
        api_get(bq_state(&fake, true), "/api/v1/usage/top-traces?range=30d").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_api_headers(&headers);
    assert_eq!(
        body,
        json!({
            "data": [
                {
                    "trace_id": "0af7651916cd43dd8448eb211c80319c",
                    "total": 4200,
                    "models": [flash_calls(), other_calls()],
                    "cost_usd": FLASH_COST,
                    "cost_partial": true,
                },
            ],
            "truncated": false,
            "next": null,
        })
    );
    assert_eq!(
        start_param(&fake, 0),
        json!("2026-10-17 12:00:00.000000+00:00")
    );
    assert_eq!(
        param_value(&fake.body(0), "limit")["parameterValue"]["value"],
        json!("20")
    );
}

#[tokio::test]
async fn an_unknown_range_is_24h() {
    let fake = FakeHttp::new(vec![breakdown_reply(false)]);
    let (status, _, _) = api_get(bq_state(&fake, true), "/api/v1/usage/breakdown?range=2w").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        start_param(&fake, 0),
        json!("2026-11-15 12:00:00.000000+00:00")
    );
}

#[tokio::test]
async fn without_bigquery_every_usage_route_is_the_usage_note_404() {
    let src = Arc::new(FakeSource::new());
    for path in [
        "/api/v1/usage/breakdown",
        "/api/v1/usage/trend?range=7d",
        "/api/v1/usage/top-traces",
    ] {
        let (status, headers, body) = api_get(state(src.clone(), None), path).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert_api_headers(&headers);
        assert_eq!(body, error_body("needs_bigquery", NEEDS_BIGQUERY), "{path}");
    }
}

#[tokio::test]
async fn a_failed_query_is_a_502_with_the_source_error() {
    for path in [
        "/api/v1/usage/breakdown",
        "/api/v1/usage/trend",
        "/api/v1/usage/top-traces",
    ] {
        let fake = FakeHttp::new(vec![denied()]);
        let (status, headers, body) = api_get(bq_state(&fake, true), path).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{path}");
        assert_api_headers(&headers);
        assert_eq!(body, denied_body(), "{path}");
    }
}

#[tokio::test]
async fn a_write_method_on_a_route_is_the_405_and_head_follows_get() {
    let fake = FakeHttp::new(vec![breakdown_reply(false)]);
    let req = Request::post("/api/v1/usage/breakdown")
        .header(HOST, format!("127.0.0.1:{PORT}"))
        .header(ORIGIN, format!("http://127.0.0.1:{PORT}"))
        .body(Body::empty())
        .unwrap();
    let (status, headers, body) = send(bq_state(&fake, true), req).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(headers[http::header::ALLOW], "GET, HEAD, OPTIONS");
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        body,
        error_body("method_not_allowed", "the API is read-only")
    );
    assert_matches_pointer(METHOD_NOT_ALLOWED, &body);
    assert!(fake.uris().is_empty(), "nothing is queried");

    let req = Request::head("/api/v1/usage/breakdown")
        .header(HOST, format!("127.0.0.1:{PORT}"))
        .body(Body::empty())
        .unwrap();
    let (status, _, body) = send(bq_state(&fake, true), req).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_empty(), "{body}");
}
