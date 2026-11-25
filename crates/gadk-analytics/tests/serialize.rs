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
use gadk_analytics::prompts::{
    FeedbackRow, InvocationVersions, VersionFeedback, VersionMetrics, VersionModel, VersionStats,
};
use gadk_analytics::review::{Cursor, PendingTrace};
use gadk_analytics::scores::{ScoreRow, TraceRoot, TraceScores};
use gadk_analytics::sessions::{
    SessionCursor, SessionPage, SessionRow, TurnRow, TurnScore, UserPage, UserRow,
};
use gadk_analytics::usage::{Breakdown, Cell, ModelCalls, TopTrace, Trend, TrendCell, UsageRange};
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
        json!("24h")
    );
    assert_eq!(
        serde_json::to_value(UsageRange::Days30).unwrap(),
        json!("30d")
    );
    assert_eq!(
        serde_json::to_value(TraceId::parse("0123456789ABCDEF0123456789ABCDEF").unwrap()).unwrap(),
        json!("0123456789abcdef0123456789abcdef")
    );
    assert_eq!(
        serde_json::to_value(Service::Trace).unwrap(),
        json!("Cloud Trace")
    );
    assert_eq!(
        serde_json::to_value(SourceError::bigquery(
            Some(403),
            Some("accessDenied".into()),
            "Access Denied"
        ))
        .unwrap(),
        json!({
            "kind": "bigquery",
            "message": "BigQuery: HTTP 403: Access Denied",
            "hint": "needs roles/bigquery.dataViewer and roles/bigquery.jobUser",
        })
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

#[test]
fn the_prompt_types_serialize() {
    let stats = VersionStats::new(2, 4, 1250.0, 300.5, Some(820.0), None);
    let model = VersionModel::new(
        2,
        ModelCalls::new("m".into(), 4, Usage::new(5000, 1000, 1202, 0)),
    );
    assert_eq!(
        serde_json::to_value(VersionMetrics::new(vec![stats], vec![model], false)).unwrap(),
        json!({
            "stats": [{
                "version": 2, "calls": 4, "mean_input": 1250.0, "mean_output": 300.5,
                "p50_ms": 820.0, "p95_ms": null,
            }],
            "models": [{
                "version": 2,
                "calls": {
                    "model": "m", "calls": 4,
                    "usage": { "input": 5000, "cached": 1000, "output": 1202, "reasoning": 0 },
                },
            }],
            "truncated": false,
        })
    );
    assert_eq!(
        serde_json::to_value(InvocationVersions::new(vec![("inv-1".into(), 2)], true)).unwrap(),
        json!({ "pairs": [["inv-1", 2]], "truncated": true })
    );
    let row = FeedbackRow::new(
        3,
        "quality".into(),
        "app".into(),
        "bool".into(),
        5,
        4,
        None,
        None,
    );
    assert_eq!(
        serde_json::to_value(VersionFeedback::new(vec![row], false)).unwrap(),
        json!({
            "rows": [{
                "version": 3, "name": "quality", "source": "app", "data_type": "bool",
                "count": 5, "positive": 4, "mean": null, "mode": null,
            }],
            "truncated": false,
        })
    );
}

#[test]
fn the_score_types_serialize_timestamps_as_rfc3339() {
    let at = Utc.with_ymd_and_hms(2026, 11, 20, 12, 0, 0).unwrap();
    let row = ScoreRow::new(
        at,
        "console".into(),
        "chris".into(),
        "quality".into(),
        "bool".into(),
        "true".into(),
        String::new(),
    );
    assert_eq!(
        serde_json::to_value(TraceScores::new(vec![row], false)).unwrap(),
        json!({
            "rows": [{
                "timestamp": "2026-11-20T12:00:00Z", "source": "console", "author": "chris",
                "name": "quality", "data_type": "bool", "value": "true", "comment": "",
            }],
            "truncated": false,
        })
    );
    assert_eq!(
        serde_json::to_value(TraceRoot::new(
            None,
            Some("s1".into()),
            "inv-1".into(),
            None
        ))
        .unwrap(),
        json!({ "user_id": null, "session_id": "s1", "invocation_id": "inv-1", "app_name": null })
    );
}

#[test]
fn the_review_types_serialize_timestamps_as_rfc3339() {
    let at = Utc.with_ymd_and_hms(2026, 11, 23, 11, 0, 0).unwrap();
    assert_eq!(
        serde_json::to_value(PendingTrace::new(
            "0123456789abcdef0123456789abcdef".into(),
            at,
            vec!["triage@2".into()],
            1,
        ))
        .unwrap(),
        json!({
            "trace_id": "0123456789abcdef0123456789abcdef",
            "first": "2026-11-23T11:00:00Z",
            "prompts": ["triage@2"],
            "thumbs_down": 1,
        })
    );
    assert_eq!(
        serde_json::to_value(Cursor::new(at, "0123456789abcdef0123456789abcdef".into())).unwrap(),
        json!({ "at": "2026-11-23T11:00:00Z", "trace_id": "0123456789abcdef0123456789abcdef" })
    );
}
