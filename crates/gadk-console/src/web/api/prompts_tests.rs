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

//! Tests of the prompt-version API route.

use std::sync::Arc;

use gadk_analytics::testutil::{FakeHttp, param_value, reply};
use http::StatusCode;
use serde_json::{Value, json};

use super::tests::{api_get, assert_api_headers, bq_state, error_body};
use super::usage_tests::{denied, denied_body};
use crate::web::pages::prompt::NEEDS_BIGQUERY_METRICS;
use crate::web::testutil::{FakeSource, state};

fn stats_reply(page_token: bool) -> Result<(u16, String), String> {
    reply(
        &[
            ("version", "INT64"),
            ("calls", "INT64"),
            ("mean_input", "FLOAT64"),
            ("mean_output", "FLOAT64"),
            ("p50_ms", "FLOAT64"),
            ("p95_ms", "FLOAT64"),
        ],
        vec![
            vec![
                json!("2"),
                json!("4"),
                json!("1250"),
                json!("300.5"),
                json!("820"),
                json!("1900"),
            ],
            vec![
                json!("1"),
                json!("2"),
                json!("900"),
                json!("200"),
                json!("700"),
                Value::Null,
            ],
        ],
        page_token,
    )
}

/// v2: four `gemini-3.8-flash` calls; v1: one `gemini-3.8-flash` call and one `other-model`
/// call.
fn models_reply() -> Result<(u16, String), String> {
    reply(
        &[
            ("version", "INT64"),
            ("model", "STRING"),
            ("calls", "INT64"),
            ("input", "INT64"),
            ("cached", "INT64"),
            ("output", "INT64"),
            ("reasoning", "INT64"),
        ],
        vec![
            vec![
                json!("2"),
                json!("gemini-3.8-flash"),
                json!("4"),
                json!("5000"),
                json!("1000"),
                json!("1202"),
                json!("0"),
            ],
            vec![
                json!("1"),
                json!("gemini-3.8-flash"),
                json!("1"),
                json!("900"),
                json!("0"),
                json!("200"),
                json!("0"),
            ],
            vec![
                json!("1"),
                json!("other-model"),
                json!("1"),
                json!("900"),
                json!("0"),
                json!("200"),
                json!("0"),
            ],
        ],
        false,
    )
}

const FEEDBACK_FIELDS: [(&str, &str); 8] = [
    ("version", "INT64"),
    ("name", "STRING"),
    ("source", "STRING"),
    ("data_type", "STRING"),
    ("category", "STRING"),
    ("count", "INT64"),
    ("positive", "INT64"),
    ("total", "FLOAT64"),
];

/// One `helpful` bool score row for v2: 3 scores, 2 positive.
fn feedback_reply(page_token: bool) -> Result<(u16, String), String> {
    reply(
        &FEEDBACK_FIELDS,
        vec![vec![
            json!("2"),
            json!("helpful"),
            json!("app"),
            json!("bool"),
            Value::Null,
            json!("3"),
            json!("2"),
            Value::Null,
        ]],
        page_token,
    )
}

fn calls(model: &str, calls: u64, input: u64, cached: u64, output: u64) -> Value {
    json!({
        "model": model,
        "calls": calls,
        "usage": { "input": input, "cached": cached, "output": output, "reasoning": 0 },
    })
}

#[tokio::test]
async fn versions_are_the_metrics_with_costs_and_the_feedback() {
    let fake = FakeHttp::new(vec![
        stats_reply(false),
        models_reply(),
        feedback_reply(false),
    ]);
    let (status, headers, body) =
        api_get(bq_state(&fake, true), "/api/v1/prompts/greet/versions").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_api_headers(&headers);
    assert_eq!(
        body,
        json!({
            "data": {
                "metrics": {
                    "stats": [
                        {
                            "version": 2,
                            "calls": 4,
                            "mean_input": 1250.0,
                            "mean_output": 300.5,
                            "p50_ms": 820.0,
                            "p95_ms": 1900.0,
                            "cost_usd": 0.00428,
                            "cost_partial": false,
                        },
                        {
                            "version": 1,
                            "calls": 2,
                            "mean_input": 900.0,
                            "mean_output": 200.0,
                            "p50_ms": 700.0,
                            "p95_ms": null,
                            "cost_usd": 0.00077,
                            "cost_partial": true,
                        },
                    ],
                    "models": [
                        {
                            "version": 2,
                            "calls": calls("gemini-3.8-flash", 4, 5000, 1000, 1202),
                            "cost_usd": 0.00428,
                            "cost_partial": false,
                        },
                        {
                            "version": 1,
                            "calls": calls("gemini-3.8-flash", 1, 900, 0, 200),
                            "cost_usd": 0.00077,
                            "cost_partial": false,
                        },
                        {
                            "version": 1,
                            "calls": calls("other-model", 1, 900, 0, 200),
                            "cost_usd": null,
                            "cost_partial": false,
                        },
                    ],
                    "truncated": false,
                },
                "feedback": {
                    "rows": [
                        {
                            "version": 2,
                            "name": "helpful",
                            "source": "app",
                            "data_type": "bool",
                            "count": 3,
                            "positive": 2,
                            "mean": null,
                            "mode": null,
                        },
                    ],
                    "truncated": false,
                },
            },
            "truncated": false,
            "next": null,
        })
    );
    // The prompt page's 7d default, for all three queries.
    for i in 0..3 {
        let sent = fake.body(i);
        assert_eq!(
            param_value(&sent, "prompt")["parameterValue"]["value"],
            json!("greet")
        );
        assert_eq!(
            param_value(&sent, "start")["parameterValue"]["value"],
            json!("2026-11-09 12:00:00.000000+00:00")
        );
    }
}

#[tokio::test]
async fn versions_take_the_range_and_an_encoded_name_and_report_truncation() {
    let fake = FakeHttp::new(vec![
        stats_reply(true),
        models_reply(),
        feedback_reply(false),
    ]);
    let (status, _, body) = api_get(
        bq_state(&fake, false),
        "/api/v1/prompts/team%2Fgreet/versions?range=24h",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["truncated"], true);
    assert_eq!(body["data"]["metrics"]["truncated"], true);
    assert_eq!(body["data"]["metrics"]["stats"][0]["cost_usd"], Value::Null);
    let sent = fake.body(0);
    assert_eq!(
        param_value(&sent, "prompt")["parameterValue"]["value"],
        json!("team/greet")
    );
    assert_eq!(
        param_value(&sent, "start")["parameterValue"]["value"],
        json!("2026-11-15 12:00:00.000000+00:00")
    );

    let fake = FakeHttp::new(vec![
        stats_reply(false),
        models_reply(),
        feedback_reply(true),
    ]);
    let (_, _, body) = api_get(
        bq_state(&fake, false),
        "/api/v1/prompts/greet/versions?range=2w",
    )
    .await;
    assert_eq!(body["truncated"], true, "the feedback was truncated");
    assert_eq!(body["data"]["metrics"]["truncated"], false);
    assert_eq!(body["data"]["feedback"]["truncated"], true);
    assert_eq!(
        param_value(&fake.body(0), "start")["parameterValue"]["value"],
        json!("2026-11-09 12:00:00.000000+00:00"),
        "an unknown range is 7d"
    );
}

#[tokio::test]
async fn an_unknown_prompt_has_empty_data() {
    let fake = FakeHttp::new(vec![
        reply(&[("version", "INT64")], vec![], false),
        reply(&[("version", "INT64")], vec![], false),
        reply(&FEEDBACK_FIELDS, vec![], false),
    ]);
    let (status, _, body) = api_get(bq_state(&fake, true), "/api/v1/prompts/nope/versions").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["data"],
        json!({
            "metrics": { "stats": [], "models": [], "truncated": false },
            "feedback": { "rows": [], "truncated": false },
        })
    );
}

#[tokio::test]
async fn a_failed_metrics_or_feedback_query_is_a_502() {
    for replies in [
        vec![denied(), denied()],
        vec![stats_reply(false), models_reply(), denied()],
    ] {
        let fake = FakeHttp::new(replies);
        let (status, headers, body) =
            api_get(bq_state(&fake, true), "/api/v1/prompts/greet/versions").await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
        assert_api_headers(&headers);
        assert_eq!(body, denied_body());
    }
}

#[tokio::test]
async fn without_bigquery_it_is_the_metrics_note_404() {
    let (status, headers, body) = api_get(
        state(Arc::new(FakeSource::new()), None),
        "/api/v1/prompts/greet/versions",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_api_headers(&headers);
    assert_eq!(body, error_body("needs_bigquery", NEEDS_BIGQUERY_METRICS));
}

#[tokio::test]
async fn a_rejected_name_is_a_json_400() {
    let fake = FakeHttp::new(vec![]);
    let (status, headers, body) =
        api_get(bq_state(&fake, true), "/api/v1/prompts/%FF/versions").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_api_headers(&headers);
    assert_eq!(
        body,
        error_body(
            "bad_request",
            "invalid request: Invalid URL: Invalid UTF-8 in `name`"
        )
    );
    assert!(fake.uris().is_empty(), "nothing is queried");
}
