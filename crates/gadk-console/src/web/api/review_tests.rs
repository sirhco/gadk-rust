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

//! Tests of the review API route.

use std::sync::Arc;

use chrono::{DateTime, TimeDelta, Utc};
use gadk_analytics::testutil::{
    FakeHttp, param_value, pending_cells, pending_judge_cells, pending_reply,
};
use http::StatusCode;
use serde_json::json;

use super::openapi_tests::assert_matches_pointer;
use super::tests::{api_get, assert_api_headers, bq_state, error_body};
use super::usage_tests::{denied, denied_body};
use crate::review::NEEDS_BIGQUERY_REVIEW;
use crate::web::testutil::{FakeSource, fixed_now, state, test_rubrics};

const A: &str = "0123456789abcdef0123456789abcdef";
const B: &str = "fedcba9876543210fedcba9876543210";

/// `fixed_now` minus `secs` seconds and 250 microseconds.
fn ago(secs: i64) -> DateTime<Utc> {
    fixed_now() - TimeDelta::seconds(secs) - TimeDelta::microseconds(250)
}

#[tokio::test]
async fn a_score_queue_lists_its_pending_traces() {
    let fake = FakeHttp::new(vec![pending_reply(vec![
        pending_cells(A, ago(60), &["triage@2", "writer@1"], 2),
        pending_cells(B, ago(120), &[], 0),
    ])]);
    let path = "/api/v1/review/pending?score=quality&prompt=triage&version=2&flagged=1&range=24h";
    let (status, headers, body) = api_get(bq_state(&fake, false), path).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_api_headers(&headers);
    assert_eq!(
        body,
        json!({
            "data": [
                {
                    "trace_id": A,
                    "first": "2026-11-16T11:58:59.999750Z",
                    "prompts": ["triage@2", "writer@1"],
                    "thumbs_down": 2,
                    "judge": [],
                },
                {
                    "trace_id": B,
                    "first": "2026-11-16T11:57:59.999750Z",
                    "prompts": [],
                    "thumbs_down": 0,
                    "judge": [],
                },
            ],
            "truncated": false,
            "next": null,
        })
    );
    let sent = fake.body(0);
    assert_eq!(
        param_value(&sent, "start")["parameterValue"]["value"],
        json!("2026-11-15 12:00:00.000000+00:00")
    );
    assert_eq!(
        param_value(&sent, "flagged")["parameterValue"]["value"],
        json!("true")
    );
    assert_eq!(
        param_value(&sent, "names")["parameterValue"],
        json!({ "arrayValues": [{ "value": "quality" }] })
    );
}

#[tokio::test]
async fn pending_items_carry_their_judge_scores() {
    let fake = FakeHttp::new(vec![pending_reply(vec![
        pending_judge_cells(
            A,
            ago(60),
            &[],
            0,
            &[("m/llm_judge", 4.0, true), ("m/reference", 2.5, false)],
        ),
        pending_cells(B, ago(120), &[], 0),
    ])]);
    let (status, _, body) = api_get(bq_state(&fake, false), "/api/v1/review/pending?score=q").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["data"][0]["judge"],
        json!([
            { "name": "m/llm_judge", "score": 4.0, "pass": true },
            { "name": "m/reference", "score": 2.5, "pass": false },
        ])
    );
    assert_eq!(body["data"][1]["judge"], json!([]));
    // Each item matches the documented JudgeScore schema.
    for item in body["data"][0]["judge"].as_array().unwrap() {
        assert_matches_pointer("#/components/schemas/JudgeScore", item);
    }
}

#[tokio::test]
async fn a_rubric_queue_waits_for_every_criterion() {
    let fake = FakeHttp::new(vec![pending_reply(vec![])]);
    let mut st = bq_state(&fake, false);
    st.rubrics = Some(test_rubrics());
    let (status, _, body) = api_get(st, "/api/v1/review/pending?rubric=support").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body,
        json!({ "data": [], "truncated": false, "next": null })
    );
    let sent = fake.body(0);
    assert_eq!(
        param_value(&sent, "names")["parameterValue"],
        json!({ "arrayValues": [{ "value": "support/accuracy" }, { "value": "support/tone" }] })
    );
    assert_eq!(
        param_value(&sent, "start")["parameterValue"]["value"],
        json!("2026-11-09 12:00:00.000000+00:00"),
        "the queue's 7d default"
    );
}

#[tokio::test]
async fn more_than_the_cap_is_cut_to_a_hundred_and_truncated() {
    let rows = (0..101)
        .map(|i| pending_cells(&format!("{i:032x}"), ago(i), &[], 0))
        .collect();
    let fake = FakeHttp::new(vec![pending_reply(rows)]);
    let (status, _, body) = api_get(bq_state(&fake, false), "/api/v1/review/pending?score=q").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let rows = body["data"].as_array().unwrap();
    assert_eq!(rows.len(), 100);
    assert_eq!(rows[99]["trace_id"], format!("{:032x}", 99));
    assert_eq!(body["truncated"], true);
}

#[tokio::test]
async fn bad_queues_are_the_pages_400_texts_and_query_nothing() {
    for (path, text) in [
        ("/api/v1/review/pending", "score: required"),
        (
            "/api/v1/review/pending?score=%20&range=7d",
            "score: required",
        ),
        (
            "/api/v1/review/pending?score=q&rubric=support",
            "score and rubric: choose one",
        ),
        (
            "/api/v1/review/pending?rubric=nope",
            "rubric: unknown rubric `nope`",
        ),
        (
            "/api/v1/review/pending?score=q&version=0&prompt=p",
            "version: must be a positive integer",
        ),
        (
            "/api/v1/review/pending?score=q&version=2",
            "version: needs a prompt",
        ),
        (
            "/api/v1/review/pending?score=q&range=30d",
            "range: must be 1h, 6h, 24h or 7d",
        ),
    ] {
        let fake = FakeHttp::new(vec![]);
        let mut st = bq_state(&fake, false);
        st.rubrics = Some(test_rubrics());
        let (status, headers, body) = api_get(st, path).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
        assert_api_headers(&headers);
        assert_eq!(body, error_body("bad_request", text), "{path}");
        assert!(fake.uris().is_empty(), "{path}");
    }
    let long = "q".repeat(101);
    let fake = FakeHttp::new(vec![]);
    let (status, _, body) = api_get(
        bq_state(&fake, false),
        &format!("/api/v1/review/pending?score={long}"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body,
        error_body("bad_request", "score: must be at most 100 bytes")
    );
}

#[tokio::test]
async fn a_failed_query_is_a_502_and_no_bigquery_is_the_review_note_404() {
    let fake = FakeHttp::new(vec![denied()]);
    let (status, _, body) = api_get(bq_state(&fake, false), "/api/v1/review/pending?score=q").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body, denied_body());

    let (status, headers, body) = api_get(
        state(Arc::new(FakeSource::new()), None),
        "/api/v1/review/pending?score=q",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_api_headers(&headers);
    assert_eq!(body, error_body("needs_bigquery", NEEDS_BIGQUERY_REVIEW));
}
