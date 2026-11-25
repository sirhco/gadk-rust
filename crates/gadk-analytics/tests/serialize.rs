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

//! The JSON shape of the public types (`serde::Serialize`).

use chrono::{TimeZone, Utc};
use gadk_analytics::cost::{CostSum, Usage};
use gadk_analytics::sessions::{
    SessionCursor, SessionPage, SessionRow, TurnRow, TurnScore, UserPage, UserRow,
};
use gadk_analytics::usage::{Breakdown, Cell, ModelCalls, TopTrace, Trend, TrendCell};
use gadk_analytics::{BigQueryTable, Service, SourceError, TimeRange, TraceId};
use serde_json::json;

#[test]
fn the_shared_types_serialize() {
    assert_eq!(
        serde_json::to_value(BigQueryTable::new("p", "d", "t")).unwrap(),
        json!({ "project": "p", "dataset": "d", "table": "t" })
    );
    assert_eq!(
        serde_json::to_value(TimeRange::Hours24).unwrap(),
        json!("Hours24")
    );
    assert_eq!(
        serde_json::to_value(TraceId::parse("0123456789ABCDEF0123456789ABCDEF").unwrap()).unwrap(),
        json!("0123456789abcdef0123456789abcdef")
    );
    assert_eq!(
        serde_json::to_value(Service::Trace).unwrap(),
        json!("Trace")
    );
    assert_eq!(
        serde_json::to_value(SourceError::bigquery(Some(403), None, "Access Denied")).unwrap(),
        json!({ "BigQuery": { "status": 403, "reason": null, "detail": "Access Denied" } })
    );
    assert_eq!(
        serde_json::to_value(SourceError::NotFound).unwrap(),
        json!("NotFound")
    );
}

#[test]
fn the_cost_types_serialize() {
    assert_eq!(
        serde_json::to_value(Usage::new(1200, 200, 300, 50)).unwrap(),
        json!({ "input": 1200, "cached": 200, "output": 300, "reasoning": 50 })
    );
    let mut sum = CostSum::default();
    sum.add(Some(0.5));
    sum.add(None);
    assert_eq!(
        serde_json::to_value(sum).unwrap(),
        json!({ "usd": 0.5, "priced": true, "unpriced": true })
    );
}

#[test]
fn the_usage_types_serialize() {
    let calls = ModelCalls::new("m".into(), 2, Usage::new(10, 0, 5, 1));
    let calls_json = json!({
        "model": "m",
        "calls": 2,
        "usage": { "input": 10, "cached": 0, "output": 5, "reasoning": 1 },
    });
    let cell = Cell::new("triage".into(), Some("triage@2".into()), calls.clone());
    let cell_json = json!({ "agent": "triage", "prompt": "triage@2", "calls": calls_json });
    assert_eq!(
        serde_json::to_value(Breakdown::new(vec![cell.clone()], true)).unwrap(),
        json!({ "cells": [cell_json], "truncated": true })
    );
    assert_eq!(
        serde_json::to_value(Trend::new(vec![TrendCell::new(3, cell)], false)).unwrap(),
        json!({ "cells": [{ "bucket": 3, "cell": cell_json }], "truncated": false })
    );
    assert_eq!(
        serde_json::to_value(TopTrace::new("t".into(), 16, vec![calls])).unwrap(),
        json!({ "trace_id": "t", "total": 16, "models": [calls_json] })
    );
}

#[test]
fn the_session_types_serialize_timestamps_as_rfc3339() {
    let at = Utc.with_ymd_and_hms(2026, 11, 20, 12, 0, 0).unwrap();
    let tokens = vec![("m".to_owned(), Usage::new(10, 0, 5, 0))];
    let tokens_json = json!([["m", { "input": 10, "cached": 0, "output": 5, "reasoning": 0 }]]);
    let session = SessionRow::new(
        "s1".into(),
        "u1".into(),
        "triage".into(),
        at,
        at,
        2,
        0,
        1,
        0,
        tokens.clone(),
    );
    assert_eq!(
        serde_json::to_value(SessionPage::new(vec![session], true, false)).unwrap(),
        json!({
            "sessions": [{
                "session_id": "s1", "user": "u1", "agent": "triage",
                "first": "2026-11-20T12:00:00Z", "last": "2026-11-20T12:00:00Z",
                "turns": 2, "errors": 0, "ups": 1, "downs": 0, "tokens": tokens_json,
            }],
            "has_next": true,
            "truncated": false,
        })
    );
    let user = UserRow::new("u1".into(), 1, 2, 0, 1, 0, at, tokens.clone());
    assert_eq!(
        serde_json::to_value(UserPage::new(vec![user], false, true)).unwrap(),
        json!({
            "users": [{
                "user": "u1", "sessions": 1, "turns": 2, "errors": 0, "ups": 1, "downs": 0,
                "last": "2026-11-20T12:00:00Z", "tokens": tokens_json,
            }],
            "more": false,
            "truncated": true,
        })
    );
    let turn = TurnRow::new(
        "inv-1".into(),
        None,
        at,
        Some(at),
        0,
        vec!["search".into()],
        tokens,
        vec![TurnScore::new(
            "helpful".into(),
            "console".into(),
            "true".into(),
        )],
    );
    assert_eq!(
        serde_json::to_value(turn).unwrap(),
        json!({
            "invocation_id": "inv-1", "trace_id": null,
            "start": "2026-11-20T12:00:00Z", "end": "2026-11-20T12:00:00Z",
            "errors": 0, "tools": ["search"], "tokens": tokens_json,
            "scores": [{ "name": "helpful", "source": "console", "value": "true" }],
        })
    );
    assert_eq!(
        serde_json::to_value(SessionCursor::new(at, "s1".into())).unwrap(),
        json!({ "at": "2026-11-20T12:00:00Z", "id": "s1" })
    );
}
