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

//! Tests of the trace-score API route.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use chrono::TimeDelta;
use gadk_analytics::testutil::{FakeHttp, param_value, reply};
use http::StatusCode;
use serde_json::{Value, json};

use super::tests::{api_get, assert_api_headers, bq_state, error_body};
use super::usage_tests::{denied, denied_body};
use crate::traces::{RawSpan, SourceError, TraceDetail, TraceId};
use crate::web::AppState;
use crate::web::pages::trace::NEEDS_BIGQUERY_SCORES;
use crate::web::testutil::{FakeSource, fixed_now, state};

const ID: &str = "0af7651916cd43dd8448eb211c80319c";

const SCORE_FIELDS: [(&str, &str); 7] = [
    ("timestamp", "TIMESTAMP"),
    ("source", "STRING"),
    ("author", "STRING"),
    ("name", "STRING"),
    ("data_type", "STRING"),
    ("value", "STRING"),
    ("comment", "STRING"),
];

/// Three spans starting 600, 500 and 400 seconds before `fixed_now`, naming invocations `e-2`,
/// `e-1` and `e-1`.
fn detail() -> TraceDetail {
    let spans = [("a", "e-2", 600), ("b", "e-1", 500), ("c", "e-1", 400)]
        .into_iter()
        .map(|(id, inv, ago)| {
            let start = fixed_now() - TimeDelta::seconds(ago);
            RawSpan {
                span_id: format!("000000000000000{id}"),
                parent_id: None,
                name: "span".into(),
                start,
                end: start + TimeDelta::seconds(1),
                labels: BTreeMap::from([("invocation_id".to_owned(), inv.to_owned())]),
            }
        })
        .collect();
    TraceDetail {
        trace_id: TraceId::parse(ID).unwrap(),
        spans,
        logs: Ok(Default::default()),
    }
}

/// [`bq_state`] whose trace source answers `get` with `detail`.
fn with_trace(fake: &Arc<FakeHttp>, detail: Result<TraceDetail, SourceError>) -> AppState {
    let src = Arc::new(FakeSource::new());
    *src.get.lock().unwrap() = detail;
    AppState {
        source: src,
        ..bq_state(fake, false)
    }
}

#[tokio::test]
async fn scores_are_looked_up_by_the_spans_invocations_and_first_start() {
    let at = fixed_now() - TimeDelta::seconds(120);
    let fake = FakeHttp::new(vec![reply(
        &SCORE_FIELDS,
        vec![vec![
            json!(at.timestamp_micros().to_string()),
            json!("console"),
            json!("chris"),
            json!("quality"),
            json!("category"),
            json!("helpful"),
            Value::Null,
        ]],
        false,
    )]);
    let (status, headers, body) = api_get(
        with_trace(&fake, Ok(detail())),
        &format!("/api/v1/traces/{ID}/scores"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_api_headers(&headers);
    assert_eq!(
        body,
        json!({
            "data": {
                "rows": [
                    {
                        "timestamp": "2026-11-16T11:58:00Z",
                        "source": "console",
                        "author": "chris",
                        "name": "quality",
                        "data_type": "category",
                        "value": "helpful",
                        "comment": "",
                    },
                ],
                "truncated": false,
            },
            "truncated": false,
            "next": null,
        })
    );
    let sent = fake.body(0);
    assert_eq!(
        param_value(&sent, "trace_id")["parameterValue"]["value"],
        json!(ID)
    );
    assert_eq!(
        param_value(&sent, "ids")["parameterValue"],
        json!({ "arrayValues": [{ "value": "e-1" }, { "value": "e-2" }] })
    );
    assert_eq!(
        param_value(&sent, "start")["parameterValue"]["value"],
        json!("2026-11-16 11:50:00.000000+00:00")
    );
}

#[tokio::test]
async fn an_invalid_id_is_the_trace_not_found_404_without_a_lookup() {
    for id in ["nope", "0AF7651916CD43DD8448EB211C80319C0", "%3Cx%3E"] {
        let fake = FakeHttp::new(vec![]);
        let src_calls = Arc::new(FakeSource::new());
        let st = AppState {
            source: src_calls.clone(),
            ..bq_state(&fake, false)
        };
        let (status, headers, body) = api_get(st, &format!("/api/v1/traces/{id}/scores")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{id}");
        assert_api_headers(&headers);
        assert_eq!(body, error_body("not_found", "trace not found"), "{id}");
        assert_eq!(src_calls.calls.load(Ordering::SeqCst), 0, "{id}");
        assert!(fake.uris().is_empty(), "{id}");
    }
}

#[tokio::test]
async fn an_unknown_trace_is_a_not_found_404_and_other_failures_are_502s() {
    let fake = FakeHttp::new(vec![]);
    let (status, _, body) = api_get(
        with_trace(&fake, Err(SourceError::NotFound)),
        &format!("/api/v1/traces/{ID}/scores"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, error_body("not_found", "trace not found"));
    assert!(fake.uris().is_empty());

    let e = SourceError::bigquery(Some(500), None, "backend error");
    let (status, _, body) = api_get(
        with_trace(&fake, Err(e)),
        &format!("/api/v1/traces/{ID}/scores"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(
        body,
        json!({ "error": { "kind": "bigquery", "message": "BigQuery: HTTP 500: backend error", "hint": null } })
    );

    let fake = FakeHttp::new(vec![denied()]);
    let (status, _, body) = api_get(
        with_trace(&fake, Ok(detail())),
        &format!("/api/v1/traces/{ID}/scores"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body, denied_body());
}

#[tokio::test]
async fn without_bigquery_it_is_the_scores_note_404_before_the_id_check() {
    let src = Arc::new(FakeSource::new());
    for id in [ID, "nope"] {
        let (status, headers, body) = api_get(
            state(src.clone(), None),
            &format!("/api/v1/traces/{id}/scores"),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{id}");
        assert_api_headers(&headers);
        assert_eq!(
            body,
            error_body("needs_bigquery", NEEDS_BIGQUERY_SCORES),
            "{id}"
        );
    }
    assert_eq!(src.calls.load(Ordering::SeqCst), 0);
}
