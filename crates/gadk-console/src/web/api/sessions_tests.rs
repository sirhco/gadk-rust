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

//! Tests of the session, turn and user API routes.

use std::sync::Arc;

use chrono::{DateTime, TimeDelta, Utc};
use gadk_analytics::testutil::{
    FakeHttp, SESSION_FIELDS, TurnCells, USER_FIELDS, param_value, reply, session_cells,
    turn_reply, user_cells,
};
use http::StatusCode;
use serde_json::{Value, json};

use super::tests::{api_get, assert_api_headers, bq_state, error_body};
use super::usage_tests::{denied, denied_body};
use crate::web::sessions::NEEDS_BIGQUERY_SESSIONS;
use crate::web::testutil::{FakeSource, fixed_now, state};

/// `fixed_now` minus `secs` seconds and 250 microseconds.
fn ago(secs: i64) -> DateTime<Utc> {
    fixed_now() - TimeDelta::seconds(secs) - TimeDelta::microseconds(250)
}

fn param(fake: &FakeHttp, i: usize, name: &str) -> Value {
    param_value(&fake.body(i), name)["parameterValue"].clone()
}

/// Session `s-1` with 1,000,000 input and 500,000 output tokens of `gemini-3.8-flash`
/// ($1.55 with the test prices).
fn head_reply() -> Result<(u16, String), String> {
    reply(
        &SESSION_FIELDS,
        vec![session_cells(
            "s-1",
            "u-1",
            "triage",
            ago(3_600),
            ago(60),
            [9, 2, 1, 1],
            Some(("gemini-3.8-flash", [1_000_000, 0, 500_000, 0])),
        )],
        false,
    )
}

/// The JSON of [`head_reply`]'s row, priced.
fn head_json() -> Value {
    json!({
        "session_id": "s-1",
        "user": "u-1",
        "agent": "triage",
        "first": "2026-11-16T10:59:59.999750Z",
        "last": "2026-11-16T11:58:59.999750Z",
        "turns": 9,
        "errors": 2,
        "ups": 1,
        "downs": 1,
        "tokens": [["gemini-3.8-flash", { "input": 1_000_000, "cached": 0, "output": 500_000, "reasoning": 0 }]],
        "cost_usd": 1.55,
        "cost_partial": false,
    })
}

#[tokio::test]
async fn sessions_are_the_page_rows_with_their_cost() {
    let mut mixed = session_cells(
        "s-2",
        "",
        "writer",
        ago(7_200),
        ago(120),
        [1, 0, 0, 0],
        Some(("gemini-3.8-flash", [1_000_000, 0, 0, 0])),
    );
    let mut other = mixed.clone();
    other[9] = json!("other-model");
    mixed[0] = json!("s-2");
    let fake = FakeHttp::new(vec![reply(
        &SESSION_FIELDS,
        vec![
            session_cells(
                "s-1",
                "u-1",
                "triage",
                ago(3_600),
                ago(60),
                [9, 2, 1, 1],
                Some(("gemini-3.8-flash", [1_000_000, 0, 500_000, 0])),
            ),
            mixed,
            other,
        ],
        false,
    )]);
    let (status, headers, body) = api_get(bq_state(&fake, true), "/api/v1/sessions").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_api_headers(&headers);
    assert_eq!(
        body,
        json!({
            "data": [
                head_json(),
                {
                    "session_id": "s-2",
                    "user": "",
                    "agent": "writer",
                    "first": "2026-11-16T09:59:59.999750Z",
                    "last": "2026-11-16T11:57:59.999750Z",
                    "turns": 1,
                    "errors": 0,
                    "ups": 0,
                    "downs": 0,
                    "tokens": [
                        ["gemini-3.8-flash", { "input": 1_000_000, "cached": 0, "output": 0, "reasoning": 0 }],
                        ["other-model", { "input": 1_000_000, "cached": 0, "output": 0, "reasoning": 0 }],
                    ],
                    "cost_usd": 0.3,
                    "cost_partial": true,
                },
            ],
            "truncated": false,
            "next": null,
        })
    );
    // The page's defaults: 24h, no filters, no session, 101 rows.
    assert_eq!(
        param(&fake, 0, "start")["value"],
        json!("2026-11-15 12:00:00.000000+00:00")
    );
    assert_eq!(param(&fake, 0, "user"), json!({}));
    assert_eq!(param(&fake, 0, "session"), json!({}));
    assert_eq!(param(&fake, 0, "limit")["value"], json!("101"));
}

#[tokio::test]
async fn without_prices_session_costs_are_null() {
    let fake = FakeHttp::new(vec![head_reply()]);
    let (_, _, body) = api_get(bq_state(&fake, false), "/api/v1/sessions").await;
    assert_eq!(body["data"][0]["cost_usd"], Value::Null);
    assert_eq!(body["data"][0]["cost_partial"], false);
}

#[tokio::test]
async fn a_full_page_links_the_next_one_and_the_cursor_round_trips() {
    let session = |i: i64| {
        session_cells(
            &format!("s {i:03}"),
            "u",
            "a",
            ago(10 + i),
            ago(i),
            [1, 0, 0, 0],
            Some(("m", [1, 0, 1, 0])),
        )
    };
    let rows: Vec<_> = (0..101).map(session).collect();
    let fake = FakeHttp::new(vec![reply(&SESSION_FIELDS, rows, false)]);
    let (status, _, body) = api_get(
        bq_state(&fake, false),
        "/api/v1/sessions?user=a%26b&agent=triage&range=6h",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"].as_array().unwrap().len(), 100);
    assert_eq!(body["truncated"], false);
    let next = "/api/v1/sessions?range=6h&user=a%26b&agent=triage&after=2026-11-16T11%3A58%3A20.999750Z&after_id=s%20099";
    assert_eq!(body["next"], next);

    let fake = FakeHttp::new(vec![reply(&SESSION_FIELDS, vec![], false)]);
    let (status, _, body) = api_get(bq_state(&fake, false), next).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body,
        json!({ "data": [], "truncated": false, "next": null })
    );
    assert_eq!(param(&fake, 0, "user")["value"], json!("a&b"));
    assert_eq!(param(&fake, 0, "agent")["value"], json!("triage"));
    assert_eq!(param(&fake, 0, "after_id")["value"], json!("s 099"));
    assert_eq!(
        param(&fake, 0, "after")["value"],
        json!("2026-11-16 11:58:20.999750+00:00")
    );
    assert_eq!(
        param(&fake, 0, "start")["value"],
        json!("2026-11-16 06:00:00.000000+00:00")
    );
}

#[tokio::test]
async fn the_row_cap_flag_is_truncated() {
    let fake = FakeHttp::new(vec![reply(
        &SESSION_FIELDS,
        vec![session_cells(
            "s-1",
            "u",
            "a",
            ago(9),
            ago(1),
            [1, 0, 0, 0],
            None,
        )],
        true,
    )]);
    let (_, _, body) = api_get(bq_state(&fake, false), "/api/v1/sessions").await;
    assert_eq!(body["truncated"], true, "{body}");
    assert_eq!(body["next"], Value::Null, "{body}");
}

#[tokio::test]
async fn bad_parameters_are_the_pages_400_texts_and_query_nothing() {
    let long = "u".repeat(129);
    for (path, text) in [
        (format!("/api/v1/sessions?user={long}"), "user is too long"),
        (
            format!("/api/v1/sessions?agent={long}"),
            "agent is too long",
        ),
        (format!("/api/v1/users?agent={long}"), "agent is too long"),
        (
            "/api/v1/sessions?after=2026-11-16T11:00:00Z".to_owned(),
            "invalid cursor",
        ),
        ("/api/v1/sessions?after_id=s-1".to_owned(), "invalid cursor"),
        (
            "/api/v1/sessions?after=yesterday&after_id=s-1".to_owned(),
            "invalid cursor",
        ),
        (
            "/api/v1/sessions/a%0Ab/turns".to_owned(),
            "invalid session id",
        ),
        (
            format!("/api/v1/sessions/{}/turns", "s".repeat(129)),
            "invalid session id",
        ),
    ] {
        let fake = FakeHttp::new(vec![]);
        let (status, headers, body) = api_get(bq_state(&fake, false), &path).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
        assert_api_headers(&headers);
        assert_eq!(body, error_body("bad_request", text), "{path}");
        assert!(fake.uris().is_empty(), "{path}");
    }
}

#[tokio::test]
async fn a_rejected_path_is_a_json_400_but_bigquery_is_checked_first() {
    let fake = FakeHttp::new(vec![]);
    let (status, _, body) = api_get(bq_state(&fake, false), "/api/v1/sessions/%FF/turns").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body,
        error_body(
            "bad_request",
            "invalid request: Invalid URL: Invalid UTF-8 in `id`"
        )
    );
    let (status, _, body) = api_get(
        state(Arc::new(FakeSource::new()), None),
        "/api/v1/sessions/%FF/turns",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, error_body("needs_bigquery", NEEDS_BIGQUERY_SESSIONS));
}

#[tokio::test]
async fn without_bigquery_every_session_route_is_the_sessions_note_404() {
    for path in [
        "/api/v1/sessions",
        "/api/v1/sessions?user=u",
        "/api/v1/sessions/s-1/turns",
        "/api/v1/users",
    ] {
        let (status, headers, body) = api_get(state(Arc::new(FakeSource::new()), None), path).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert_api_headers(&headers);
        assert_eq!(
            body,
            error_body("needs_bigquery", NEEDS_BIGQUERY_SESSIONS),
            "{path}"
        );
    }
}

#[tokio::test]
async fn users_are_the_page_rows_over_7d_with_their_cost() {
    let fake = FakeHttp::new(vec![reply(
        &USER_FIELDS,
        vec![
            user_cells(
                Some("u-1"),
                [2, 3, 1, 1, 0],
                ago(60),
                Some(("gemini-3.8-flash", [1_000_000, 0, 0, 0])),
            ),
            user_cells(None, [1, 1, 0, 0, 1], ago(120), None),
        ],
        false,
    )]);
    let (status, headers, body) =
        api_get(bq_state(&fake, true), "/api/v1/users?agent=triage").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_api_headers(&headers);
    assert_eq!(
        body,
        json!({
            "data": [
                {
                    "user": "u-1",
                    "sessions": 2,
                    "turns": 3,
                    "errors": 1,
                    "ups": 1,
                    "downs": 0,
                    "last": "2026-11-16T11:58:59.999750Z",
                    "tokens": [["gemini-3.8-flash", { "input": 1_000_000, "cached": 0, "output": 0, "reasoning": 0 }]],
                    "cost_usd": 0.3,
                    "cost_partial": false,
                },
                {
                    "user": "",
                    "sessions": 1,
                    "turns": 1,
                    "errors": 0,
                    "ups": 0,
                    "downs": 1,
                    "last": "2026-11-16T11:57:59.999750Z",
                    "tokens": [],
                    "cost_usd": null,
                    "cost_partial": false,
                },
            ],
            "truncated": false,
            "next": null,
        })
    );
    assert_eq!(
        param(&fake, 0, "start")["value"],
        json!("2026-11-09 12:00:00.000000+00:00")
    );
    assert_eq!(param(&fake, 0, "agent")["value"], json!("triage"));
}

#[tokio::test]
async fn more_users_than_the_cap_or_the_row_cap_are_truncated() {
    let rows: Vec<_> = (0..101)
        .map(|i| user_cells(Some(&format!("u-{i:03}")), [1, 1, 0, 0, 0], ago(i), None))
        .collect();
    let fake = FakeHttp::new(vec![
        reply(&USER_FIELDS, rows, false),
        reply(
            &USER_FIELDS,
            vec![user_cells(Some("u"), [1, 1, 0, 0, 0], ago(1), None)],
            true,
        ),
    ]);
    let st = bq_state(&fake, false);
    let (_, _, body) = api_get(st.clone(), "/api/v1/users").await;
    assert_eq!(body["data"].as_array().unwrap().len(), 100);
    assert_eq!(body["truncated"], true);
    let (_, _, body) = api_get(st, "/api/v1/users").await;
    assert_eq!(body["data"].as_array().unwrap().len(), 1);
    assert_eq!(body["truncated"], true);
}

#[tokio::test]
async fn turns_are_the_header_and_the_turns_with_their_cost() {
    let fake = FakeHttp::new(vec![
        head_reply(),
        turn_reply(vec![
            TurnCells {
                trace_id: Some("0123456789abcdef0123456789abcdef"),
                end: Some(ago(2_998)),
                tools: &["search"],
                tokens: &[
                    ("gemini-3.8-flash", [1_000_000, 0, 0, 0]),
                    ("other-model", [5, 0, 1, 0]),
                ],
                scores: &[("helpful", "console", "true")],
                ..TurnCells::new("inv-1", ago(3_000))
            }
            .cells(),
            TurnCells {
                errors: 1,
                ..TurnCells::new("inv-2", ago(2_000))
            }
            .cells(),
        ]),
    ]);
    let (status, headers, body) =
        api_get(bq_state(&fake, true), "/api/v1/sessions/s-1/turns").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_api_headers(&headers);
    assert_eq!(
        body,
        json!({
            "data": {
                "session": head_json(),
                "turns": [
                    {
                        "invocation_id": "inv-1",
                        "trace_id": "0123456789abcdef0123456789abcdef",
                        "start": "2026-11-16T11:09:59.999750Z",
                        "end": "2026-11-16T11:10:01.999750Z",
                        "errors": 0,
                        "tools": ["search"],
                        "tokens": [
                            ["gemini-3.8-flash", { "input": 1_000_000, "cached": 0, "output": 0, "reasoning": 0 }],
                            ["other-model", { "input": 5, "cached": 0, "output": 1, "reasoning": 0 }],
                        ],
                        "scores": [{ "name": "helpful", "source": "console", "value": "true" }],
                        "cost_usd": 0.3,
                        "cost_partial": true,
                    },
                    {
                        "invocation_id": "inv-2",
                        "trace_id": null,
                        "start": "2026-11-16T11:26:39.999750Z",
                        "end": null,
                        "errors": 1,
                        "tools": [],
                        "tokens": [],
                        "scores": [],
                        "cost_usd": null,
                        "cost_partial": false,
                    },
                ],
            },
            "truncated": false,
            "next": null,
        })
    );
    // The session page's 7d default, for the header and the turns.
    assert_eq!(param(&fake, 0, "session")["value"], json!("s-1"));
    for i in 0..2 {
        assert_eq!(
            param(&fake, i, "start")["value"],
            json!("2026-11-09 12:00:00.000000+00:00")
        );
    }
}

#[tokio::test]
async fn an_unknown_session_is_a_not_found_404_naming_the_range() {
    let fake = FakeHttp::new(vec![reply(&SESSION_FIELDS, vec![], false)]);
    let (status, headers, body) = api_get(
        bq_state(&fake, false),
        "/api/v1/sessions/a%20%3Cb%3E/turns?range=24h",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_api_headers(&headers);
    assert_eq!(
        body,
        error_body("not_found", "no activity for session a <b> in the last 24h")
    );
    assert_eq!(fake.uris().len(), 1, "the turns are not queried");
}

#[tokio::test]
async fn more_than_two_hundred_turns_are_cut_and_truncated() {
    let ids: Vec<String> = (0..201).map(|i| format!("inv-{i:03}")).collect();
    let rows = ids
        .iter()
        .enumerate()
        .map(|(i, id)| TurnCells::new(id, ago(3_000 - i as i64)).cells())
        .collect();
    let fake = FakeHttp::new(vec![head_reply(), turn_reply(rows)]);
    let (status, _, body) = api_get(bq_state(&fake, false), "/api/v1/sessions/s-1/turns").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let turns = body["data"]["turns"].as_array().unwrap();
    assert_eq!(turns.len(), 200);
    assert_eq!(turns[199]["invocation_id"], "inv-199");
    assert_eq!(body["truncated"], true);
}

#[tokio::test]
async fn failed_queries_are_502s_with_the_source_error() {
    for (path, replies) in [
        ("/api/v1/sessions", vec![denied()]),
        ("/api/v1/users", vec![denied()]),
        ("/api/v1/sessions/s-1/turns", vec![denied()]),
        ("/api/v1/sessions/s-1/turns", vec![head_reply(), denied()]),
    ] {
        let fake = FakeHttp::new(replies);
        let (status, headers, body) = api_get(bq_state(&fake, false), path).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{path}");
        assert_api_headers(&headers);
        assert_eq!(body, denied_body(), "{path}");
    }
}
