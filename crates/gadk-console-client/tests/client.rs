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

//! The client against an in-process axum server with canned replies: each operation's request,
//! the error mapping, paging and base URL checks.

#![allow(clippy::unwrap_used)] // the helpers outside #[test] functions unwrap too

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, Uri, header::CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use gadk_console_client::{
    Client, Error, GetOpenApiQuery, GetPromptVersionsQuery, GetSessionTurnsQuery,
    GetTraceScoresQuery, GetUsageBreakdownQuery, GetUsageTopTracesQuery, GetUsageTrendQuery,
    ListPendingReviewsQuery, ListSessionsQuery, ListUsersQuery, Range, UsageRange,
};
use serde_json::{Value, json};

/// One canned reply: the exact request target (path and query, as sent), the status and the body.
type Canned = (&'static str, u16, String);

/// The server's replies and the request targets it saw, in order.
struct Server {
    replies: Vec<Canned>,
    seen: Mutex<Vec<String>>,
}

/// Answers the canned reply whose target equals the request's, else a text 404.
async fn reply(State(server): State<Arc<Server>>, uri: Uri) -> Response {
    let target = uri.to_string();
    server.seen.lock().unwrap().push(target.clone());
    match server.replies.iter().find(|(t, _, _)| *t == target) {
        Some((_, status, body)) => (
            StatusCode::from_u16(*status).unwrap(),
            [(CONTENT_TYPE, "application/json")],
            body.clone(),
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "no canned reply").into_response(),
    }
}

/// Serves `replies` on 127.0.0.1: the base URL and the server (for the targets it saw).
async fn serve(replies: Vec<Canned>) -> (String, Arc<Server>) {
    let server = Arc::new(Server {
        replies,
        seen: Mutex::default(),
    });
    let app = Router::new().fallback(reply).with_state(server.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), server)
}

fn seen(server: &Server) -> Vec<String> {
    server.seen.lock().unwrap().clone()
}

/// `{"data": data, "truncated": false, "next": next}` as text.
fn envelope(data: Value, next: Option<&str>) -> String {
    json!({ "data": data, "truncated": false, "next": next }).to_string()
}

fn usage() -> Value {
    json!({ "input": 900, "cached": 100, "output": 120, "reasoning": 8 })
}

fn calls() -> Value {
    json!({ "model": "gemini-3.8-flash", "calls": 3, "usage": usage() })
}

/// `row` with the cost fields.
fn priced(mut row: Value) -> Value {
    row["cost_usd"] = json!(0.002175);
    row["cost_partial"] = json!(false);
    row
}

fn session(id: &str) -> Value {
    priced(json!({
        "session_id": id, "user": "u-1", "agent": "triage",
        "first": "2026-11-16T11:00:00Z", "last": "2026-11-16T11:58:59.999750Z",
        "turns": 2, "errors": 0, "ups": 1, "downs": 0,
        "tokens": [["gemini-3.8-flash", usage()]]
    }))
}

fn error_body(kind: &str, message: &str, hint: Option<&str>) -> String {
    json!({ "error": { "kind": kind, "message": message, "hint": hint } }).to_string()
}

#[test]
fn range_enums_have_their_wire_values() {
    assert_eq!(UsageRange::D30.as_str(), "30d");
    assert_eq!(UsageRange::H1.to_string(), "1h");
    assert_eq!(Range::H24.to_string(), "24h");
    assert_eq!(Range::D7.as_str(), "7d");
}

#[tokio::test]
async fn usage_breakdown_omits_defaults_and_sends_the_range() {
    let body = envelope(
        json!({ "cells": [priced(json!({ "agent": "triage", "prompt": "triage@2", "calls": calls() }))], "truncated": false }),
        None,
    );
    let (base, server) = serve(vec![
        ("/api/v1/usage/breakdown", 200, body.clone()),
        ("/api/v1/usage/breakdown?range=30d", 200, body),
    ])
    .await;
    let client = Client::new(&base).unwrap();
    let got = client
        .get_usage_breakdown(&GetUsageBreakdownQuery::new())
        .await
        .unwrap();
    let cell = &got.data.cells[0];
    assert_eq!(cell.prompt.as_deref(), Some("triage@2"));
    assert_eq!(cell.calls.usage.reasoning, 8);
    assert_eq!(cell.cost_usd, Some(0.002175));
    assert!(!got.truncated && got.next.is_none());
    client
        .get_usage_breakdown(&GetUsageBreakdownQuery::new().range(UsageRange::D30))
        .await
        .unwrap();
    assert_eq!(
        seen(&server),
        [
            "/api/v1/usage/breakdown",
            "/api/v1/usage/breakdown?range=30d"
        ]
    );
}

#[tokio::test]
async fn usage_trend_and_top_traces() {
    let trend = envelope(
        json!({
            "start": "2026-11-16T06:00:00Z", "bucket_seconds": 900,
            "cells": [priced(json!({ "bucket": 3, "cell": { "agent": "", "prompt": null, "calls": calls() } }))]
        }),
        None,
    );
    let top = envelope(
        json!([priced(
            json!({ "trace_id": "0af7651916cd43dd8448eb211c80319c", "total": 1028, "models": [calls()] })
        )]),
        None,
    );
    let (base, server) = serve(vec![
        ("/api/v1/usage/trend?range=6h", 200, trend),
        ("/api/v1/usage/top-traces", 200, top),
    ])
    .await;
    let client = Client::new(&base).unwrap();
    let t = client
        .get_usage_trend(&GetUsageTrendQuery::new().range(UsageRange::H6))
        .await
        .unwrap();
    assert_eq!(t.data.bucket_seconds, 900);
    assert_eq!(t.data.cells[0].bucket, 3);
    assert_eq!(t.data.cells[0].cell.prompt, None);
    let top = client
        .get_usage_top_traces(&GetUsageTopTracesQuery::new())
        .await
        .unwrap();
    assert_eq!(top.data[0].total, 1028);
    assert_eq!(
        seen(&server),
        ["/api/v1/usage/trend?range=6h", "/api/v1/usage/top-traces"]
    );
}

#[tokio::test]
async fn sessions_send_every_parameter_form_encoded() {
    let target = "/api/v1/sessions?range=1h&user=a+b&agent=triage&after=2026-11-16T11%3A58%3A59.999750Z&after_id=s-1";
    let (base, server) = serve(vec![(target, 200, envelope(json!([session("s-2")]), None))]).await;
    let after: DateTime<Utc> = "2026-11-16T11:58:59.999750Z".parse().unwrap();
    let q = ListSessionsQuery::new()
        .range(Range::H1)
        .user("a b")
        .agent("triage")
        .after(after)
        .after_id("s-1");
    let page = Client::new(&base).unwrap().list_sessions(&q).await.unwrap();
    let row = &page.data[0];
    assert_eq!(row.session_id, "s-2");
    assert_eq!(row.last, after);
    assert_eq!(row.tokens[0].0, "gemini-3.8-flash");
    assert_eq!(row.tokens[0].1.cached, 100);
    assert_eq!(seen(&server), [target]);
}

#[tokio::test]
async fn path_parameters_are_one_encoded_segment() {
    let turns = envelope(
        json!({
            "session": session("s 1/x"),
            "turns": [priced(json!({
                "invocation_id": "e-1", "trace_id": null, "start": "2026-11-16T11:00:00Z",
                "end": null, "errors": 0, "tools": ["lookup"], "tokens": [],
                "scores": [{ "name": "helpful", "source": "app", "value": "true" }]
            }))]
        }),
        None,
    );
    let versions = envelope(
        json!({
            "metrics": {
                "stats": [priced(json!({ "version": 2, "calls": 3, "mean_input": 300.0, "mean_output": 40.5, "p50_ms": null, "p95_ms": 812.0 }))],
                "models": [priced(json!({ "version": 2, "calls": calls() }))],
                "truncated": false
            },
            "feedback": { "rows": [{ "version": 2, "name": "helpful", "source": "app", "data_type": "bool", "count": 4, "positive": 3, "mean": null, "mode": null }], "truncated": false }
        }),
        None,
    );
    let scores = envelope(
        json!({ "rows": [{ "timestamp": "2026-11-16T12:00:00Z", "source": "console", "author": "ana", "name": "helpful", "data_type": "bool", "value": "true", "comment": "" }], "truncated": false }),
        None,
    );
    let (base, server) = serve(vec![
        ("/api/v1/sessions/s%201%2Fx/turns", 200, turns),
        (
            "/api/v1/prompts/team%2Fgreet/versions?range=24h",
            200,
            versions,
        ),
        (
            "/api/v1/traces/0af7651916cd43dd8448eb211c80319c/scores",
            200,
            scores,
        ),
    ])
    .await;
    let client = Client::new(&base).unwrap();
    let t = client
        .get_session_turns(&GetSessionTurnsQuery::new("s 1/x"))
        .await
        .unwrap();
    assert_eq!(t.data.session.session_id, "s 1/x");
    assert_eq!(t.data.turns[0].end, None);
    assert_eq!(t.data.turns[0].scores[0].value, "true");
    let v = client
        .get_prompt_versions(&GetPromptVersionsQuery::new("team/greet").range(Range::H24))
        .await
        .unwrap();
    assert_eq!(v.data.metrics.stats[0].p95_ms, Some(812.0));
    assert_eq!(v.data.feedback.rows[0].positive, 3);
    let s = client
        .get_trace_scores(&GetTraceScoresQuery::new(
            "0af7651916cd43dd8448eb211c80319c",
        ))
        .await
        .unwrap();
    assert_eq!(s.data.rows[0].author, "ana");
    assert_eq!(
        seen(&server),
        [
            "/api/v1/sessions/s%201%2Fx/turns",
            "/api/v1/prompts/team%2Fgreet/versions?range=24h",
            "/api/v1/traces/0af7651916cd43dd8448eb211c80319c/scores",
        ]
    );
}

#[tokio::test]
async fn users_reviews_and_the_document() {
    let users = envelope(
        json!([priced(json!({
            "user": "u-1", "sessions": 2, "turns": 5, "errors": 1, "ups": 1, "downs": 0,
            "last": "2026-11-16T11:58:00Z", "tokens": [["gemini-3.8-flash", usage()]]
        }))]),
        None,
    );
    let pending = envelope(
        json!([{ "trace_id": "0af7651916cd43dd8448eb211c80319c", "first": "2026-11-16T11:00:00Z", "prompts": ["triage@2"], "thumbs_down": 1, "judge": [{ "name": "support/llm_judge", "score": 4.0, "pass": true }] }]),
        None,
    );
    let (base, server) = serve(vec![
        ("/api/v1/users?range=7d&agent=triage", 200, users),
        (
            "/api/v1/review/pending?score=helpful&prompt=triage&version=2&flagged=1&range=24h",
            200,
            pending.clone(),
        ),
        ("/api/v1/review/pending?rubric=tone", 200, pending),
        (
            "/api/v1/openapi.json",
            200,
            json!({ "openapi": "3.1.0" }).to_string(),
        ),
    ])
    .await;
    let client = Client::new(&base).unwrap();
    let u = client
        .list_users(&ListUsersQuery::new().range(Range::D7).agent("triage"))
        .await
        .unwrap();
    assert_eq!(u.data[0].sessions, 2);
    let q = ListPendingReviewsQuery::new()
        .score("helpful")
        .prompt("triage")
        .version(2)
        .flagged(true)
        .range(Range::H24);
    let p = client.list_pending_reviews(&q).await.unwrap();
    assert_eq!(p.data[0].thumbs_down, 1);
    let judge = p.data[0].judge.as_ref().unwrap();
    assert_eq!(judge[0].name, "support/llm_judge");
    assert_eq!(judge[0].score, 4.0);
    assert!(judge[0].pass);
    let q = ListPendingReviewsQuery::new().rubric("tone").flagged(false);
    client.list_pending_reviews(&q).await.unwrap();
    let doc = client.get_open_api(&GetOpenApiQuery::new()).await.unwrap();
    assert_eq!(doc["openapi"], "3.1.0");
    assert_eq!(
        seen(&server),
        [
            "/api/v1/users?range=7d&agent=triage",
            "/api/v1/review/pending?score=helpful&prompt=triage&version=2&flagged=1&range=24h",
            "/api/v1/review/pending?rubric=tone",
            "/api/v1/openapi.json",
        ]
    );
}

#[tokio::test]
async fn a_json_error_is_api_and_anything_else_is_http() {
    let (base, _server) = serve(vec![
        (
            "/api/v1/usage/breakdown",
            404,
            error_body("needs_bigquery", "usage needs the BigQuery source", None),
        ),
        (
            "/api/v1/usage/trend",
            502,
            error_body(
                "bigquery",
                "BigQuery: HTTP 403: denied",
                Some("grant roles/bigquery.jobUser"),
            ),
        ),
        ("/api/v1/usage/top-traces", 502, "bad gateway".to_owned()),
        ("/api/v1/users", 200, "{}".to_owned()),
    ])
    .await;
    let client = Client::new(&base).unwrap();

    let e = client
        .get_usage_breakdown(&GetUsageBreakdownQuery::new())
        .await
        .unwrap_err();
    assert!(e.is_needs_bigquery(), "{e:?}");
    assert_eq!(
        e.to_string(),
        "api 404 needs_bigquery: usage needs the BigQuery source"
    );

    match client
        .get_usage_trend(&GetUsageTrendQuery::new())
        .await
        .unwrap_err()
    {
        Error::Api {
            status, kind, hint, ..
        } => {
            assert_eq!((status, kind.as_str()), (502, "bigquery"));
            assert_eq!(hint.as_deref(), Some("grant roles/bigquery.jobUser"));
        }
        other => panic!("{other:?}"),
    }

    match client
        .get_usage_top_traces(&GetUsageTopTracesQuery::new())
        .await
        .unwrap_err()
    {
        Error::Http { status, body, .. } => {
            assert_eq!((status, body.as_str()), (502, "bad gateway"))
        }
        other => panic!("{other:?}"),
    }

    match client.list_users(&ListUsersQuery::new()).await.unwrap_err() {
        Error::Decode {
            status, message, ..
        } => {
            assert_eq!(status, 200);
            assert!(message.contains("missing field `data`"), "{message}");
        }
        other => panic!("{other:?}"),
    }

    // An unknown route answers the server's text 404 here: not the console's JSON, so Http.
    let e = client
        .get_trace_scores(&GetTraceScoresQuery::new("x"))
        .await
        .unwrap_err();
    assert!(
        matches!(e, Error::Http { status: 404, .. }) && !e.is_not_found(),
        "{e:?}"
    );
}

#[tokio::test]
async fn a_closed_port_is_a_transport_error() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let e = Client::new(&format!("http://{addr}"))
        .unwrap()
        .get_open_api(&GetOpenApiQuery::new())
        .await
        .unwrap_err();
    assert!(matches!(e, Error::Transport(_)), "{e:?}");
    assert!(std::error::Error::source(&e).is_some());
}

#[tokio::test]
async fn the_base_url_must_be_an_http_root() {
    for bad in [
        "http://127.0.0.1:1/x",
        "ftp://h",
        "not a url",
        "http://127.0.0.1:1/?a=1",
        "http://127.0.0.1:1/#top",
    ] {
        assert!(
            matches!(Client::new(bad), Err(Error::InvalidUrl(_))),
            "{bad}"
        );
    }
    let (base, server) = serve(vec![("/api/v1/openapi.json", 200, "{}".to_owned())]).await;
    let client = Client::new(&format!("{base}/")).unwrap();
    client.get_open_api(&GetOpenApiQuery::new()).await.unwrap();
    assert!(Client::new("https://console.example").is_ok());
    assert_eq!(seen(&server), ["/api/v1/openapi.json"]);
}

/// The second page's target: the first page's `next` link.
const NEXT: &str = "/api/v1/sessions?range=24h&after=2026-11-16T11%3A58%3A59.999750Z&after_id=s-1";

#[tokio::test]
async fn pages_follow_next_until_it_is_null() {
    let (base, server) = serve(vec![
        (
            "/api/v1/sessions",
            200,
            envelope(json!([session("s-1")]), Some(NEXT)),
        ),
        (NEXT, 200, envelope(json!([session("s-2")]), None)),
    ])
    .await;
    let client = Client::new(&base).unwrap();
    let mut pages = client.list_sessions_pages(&ListSessionsQuery::new());
    let first = pages.next_page().await.unwrap().unwrap();
    assert_eq!(first.data[0].session_id, "s-1");
    assert_eq!(first.next.as_deref(), Some(NEXT));
    let second = pages.next_page().await.unwrap().unwrap();
    assert_eq!(second.data[0].session_id, "s-2");
    assert!(pages.next_page().await.unwrap().is_none());
    assert!(pages.next_page().await.unwrap().is_none());
    assert_eq!(seen(&server), ["/api/v1/sessions", NEXT]);

    let all = client
        .list_sessions_all(&ListSessionsQuery::new(), 10)
        .await
        .unwrap();
    let ids: Vec<&str> = all.iter().map(|r| r.session_id.as_str()).collect();
    assert_eq!(ids, ["s-1", "s-2"]);
}

#[tokio::test]
async fn a_failed_page_errors_once_and_ends_the_pages() {
    let (base, server) = serve(vec![
        (
            "/api/v1/sessions",
            200,
            envelope(json!([session("s-1")]), Some(NEXT)),
        ),
        (
            NEXT,
            502,
            error_body("bigquery", "BigQuery: HTTP 500: boom", None),
        ),
    ])
    .await;
    let client = Client::new(&base).unwrap();
    let mut pages = client.list_sessions_pages(&ListSessionsQuery::new());
    assert!(pages.next_page().await.unwrap().is_some());
    let e = pages.next_page().await.unwrap_err();
    assert!(matches!(e, Error::Api { status: 502, .. }), "{e:?}");
    assert!(pages.next_page().await.unwrap().is_none());
    assert_eq!(seen(&server), ["/api/v1/sessions", NEXT]);
}

#[tokio::test]
async fn list_sessions_all_stops_at_max_pages() {
    let (base, server) = serve(vec![
        (
            "/api/v1/sessions",
            200,
            envelope(json!([session("s-1")]), Some(NEXT)),
        ),
        (NEXT, 200, envelope(json!([session("s-2")]), None)),
    ])
    .await;
    let client = Client::new(&base).unwrap();
    let rows = client
        .list_sessions_all(&ListSessionsQuery::new(), 1)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].session_id, "s-1");
    assert!(
        client
            .list_sessions_all(&ListSessionsQuery::new(), 0)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(seen(&server), ["/api/v1/sessions"]);
}

#[tokio::test]
async fn a_next_link_to_another_origin_is_refused() {
    let (base, server) = serve(vec![(
        "/api/v1/sessions",
        200,
        envelope(json!([]), Some("http://evil.example/api/v1/sessions")),
    )])
    .await;
    let client = Client::new(&base).unwrap();
    let mut pages = client.list_sessions_pages(&ListSessionsQuery::new());
    assert!(pages.next_page().await.unwrap().is_some());
    let e = pages.next_page().await.unwrap_err();
    assert!(matches!(e, Error::InvalidUrl(_)), "{e:?}");
    assert!(pages.next_page().await.unwrap().is_none());
    assert_eq!(seen(&server), ["/api/v1/sessions"]);
}

#[tokio::test]
async fn a_cancelled_page_fetch_does_not_end_the_pages() {
    use std::sync::atomic::{AtomicBool, Ordering};
    // The first request stalls; later ones answer the first page at once.
    let stalled = Arc::new(AtomicBool::new(false));
    let app = Router::new().fallback({
        let stalled = stalled.clone();
        move || {
            let stalled = stalled.clone();
            async move {
                if !stalled.swap(true, Ordering::SeqCst) {
                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                }
                (
                    [(CONTENT_TYPE, "application/json")],
                    envelope(json!([session("s-1")]), None),
                )
            }
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let client = Client::new(&format!("http://{addr}")).unwrap();
    let mut pages = client.list_sessions_pages(&ListSessionsQuery::new());
    let timed_out =
        tokio::time::timeout(std::time::Duration::from_millis(200), pages.next_page()).await;
    assert!(timed_out.is_err(), "the first fetch should time out");
    let page = pages
        .next_page()
        .await
        .unwrap()
        .expect("the first page again");
    assert_eq!(page.data[0].session_id, "s-1");
    assert!(pages.next_page().await.unwrap().is_none());
}

#[tokio::test]
async fn a_base_url_with_credentials_is_refused_without_echoing_them() {
    for bad in [
        "http://user:hunter2pass@127.0.0.1:1",
        "http://user@127.0.0.1:1",
        "https://:hunter2pass@console.example/",
    ] {
        match Client::new(bad) {
            Err(Error::InvalidUrl(m)) => {
                assert!(!m.contains("pass") && !m.contains("user"), "{m}");
                assert!(m.contains("credentials"), "{m}");
            }
            other => panic!("{bad}: {other:?}"),
        }
    }
    // The other base URL errors do not echo the input either.
    for bad in [
        "http://u:hunter2pass@127.0.0.1:1/x",
        "ftp://u:hunter2pass@h",
        "not a url hunter2pass",
    ] {
        match Client::new(bad) {
            Err(Error::InvalidUrl(m)) => assert!(!m.contains("hunter2"), "{m}"),
            other => panic!("{bad}: {other:?}"),
        }
    }
}

#[tokio::test]
async fn a_redirect_is_not_followed() {
    let app = Router::new().fallback(|uri: Uri| async move {
        if uri.path() == "/api/v1/openapi.json" {
            (
                StatusCode::FOUND,
                [(axum::http::header::LOCATION, "/moved")],
                "",
            )
                .into_response()
        } else {
            ([(CONTENT_TYPE, "application/json")], "{}").into_response()
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let e = Client::new(&format!("http://{addr}"))
        .unwrap()
        .get_open_api(&GetOpenApiQuery::new())
        .await
        .unwrap_err();
    assert!(matches!(e, Error::Http { status: 302, .. }), "{e:?}");
}

#[tokio::test]
async fn dot_and_empty_path_parameters_are_refused() {
    let (base, server) = serve(vec![]).await;
    let client = Client::new(&base).unwrap();
    for bad in [".", "..", ""] {
        let e = client
            .get_session_turns(&GetSessionTurnsQuery::new(bad))
            .await
            .unwrap_err();
        assert!(matches!(e, Error::InvalidUrl(_)), "{bad:?}: {e:?}");
        let e = client
            .get_prompt_versions(&GetPromptVersionsQuery::new(bad))
            .await
            .unwrap_err();
        assert!(matches!(e, Error::InvalidUrl(_)), "{bad:?}: {e:?}");
    }
    assert!(seen(&server).is_empty(), "{:?}", seen(&server));
}

#[test]
fn a_pending_trace_from_an_older_console_has_no_judge() {
    let p: gadk_console_client::PendingTrace = serde_json::from_value(json!({
        "trace_id": "0af7651916cd43dd8448eb211c80319c",
        "first": "2026-11-16T11:00:00Z",
        "prompts": [],
        "thumbs_down": 0,
    }))
    .unwrap();
    assert_eq!(p.judge, None);
    let p: gadk_console_client::PendingTrace = serde_json::from_value(json!({
        "trace_id": "0af7651916cd43dd8448eb211c80319c",
        "first": "2026-11-16T11:00:00Z",
        "prompts": [],
        "thumbs_down": 0,
        "judge": [],
    }))
    .unwrap();
    assert_eq!(p.judge, Some(vec![]));
}
