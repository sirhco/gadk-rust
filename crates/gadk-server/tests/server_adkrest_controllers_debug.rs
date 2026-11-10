// Copyright 2026 Chris Olson
// Portions derived from ADK-Go (https://github.com/google/adk-go), Copyright 2026 Google LLC.
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

//! Port of Go `server/adkrest/controllers/debug_test.go`, plus the debug wire fixtures that do
//! not need the full scenario (`main_34`, `main_37`, `main_38`, `main_39`) replayed against the
//! controller directly.
//!
//! Go feeds the store from an OTel tracer and logger; these tests emit the equivalent `tracing`
//! spans and log events (plan decision D6: target `gadk_engine::telemetry::log`, fields
//! `event.name`, `otel.log.body`, `otel.log.attributes`) under a subscriber holding the
//! [`DebugTelemetry`] layer.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use chrono::Utc;
use gadk_agent::{Agent, AgentConfig, SingleLoader, Tool};
use gadk_engine::llm::LlmAgentConfig;
use gadk_genai::{Content, FunctionCall, FunctionResponse, Part};
use gadk_model::LlmResponse;
use gadk_server::rest::controllers::DebugApiController;
use gadk_server::rest::testutil::{FakeSessionService, SessionKey, TestSession};
use gadk_server::rest::{DebugTelemetry, DebugTelemetryConfig, PathVars};
use gadk_session::{Event, InMemorySessionService, SessionService};
use http::StatusCode;
use serde_json::{Map, Value, json};
use tracing_subscriber::layer::SubscriberExt;

type Request = http::Request<Body>;
type Response = axum::response::Response;

/// Go `testTelemetry` / `setupTestTelemetry`.
fn setup_test_telemetry() -> DebugTelemetry {
    DebugTelemetry::new(DebugTelemetryConfig::default())
}

/// Runs `f` with a subscriber holding `dt` (Go's tracer and logger providers).
fn with_layer(dt: &DebugTelemetry, f: impl FnOnce()) {
    let sub = tracing_subscriber::registry().with(dt.clone());
    tracing::subscriber::with_default(sub, f);
}

/// Go `logger.Emit(ctx, record)` under `span`; `body` is the record body's JSON text.
fn emit_log(span: &tracing::Span, event_name: &str, body: &str) {
    tracing::event!(
        target: "gadk_engine::telemetry::log",
        parent: span,
        tracing::Level::INFO,
        event.name = event_name,
        otel.log.body = body
    );
}

/// A span named `name` carrying Go's three test attributes.
fn test_span(name: &'static str, session_id: &str, event_id: &str, op_name: &str) -> tracing::Span {
    tracing::info_span!(
        parent: None,
        "span",
        otel.name = name,
        gcp.vertex.agent.event_id = event_id,
        gen_ai.conversation.id = session_id,
        gen_ai.operation.name = op_name
    )
}

/// Go `emitTestSignals`: a span with one log record.
fn emit_test_signals(dt: &DebugTelemetry, session_id: &str, event_id: &str, op_name: &str) {
    with_layer(dt, || {
        let span = test_span("test-span", session_id, event_id, op_name);
        emit_log(&span, "test-log-event", r#"{"message":"test log message"}"#);
    });
}

/// Go `emitSpanWithoutLogs`.
fn emit_span_without_logs(dt: &DebugTelemetry, session_id: &str, event_id: &str, op_name: &str) {
    with_layer(dt, || {
        drop(test_span(
            "test-span-without-logs",
            session_id,
            event_id,
            op_name,
        ));
    });
}

/// Go `emitElidedMessageLogs`: the two records the server emits with content capture off.
fn emit_elided_message_logs(dt: &DebugTelemetry, session_id: &str, event_id: &str, op_name: &str) {
    with_layer(dt, || {
        let span = test_span("test-span", session_id, event_id, op_name);
        for event_name in ["gen_ai.system.message", "gen_ai.user.message"] {
            emit_log(&span, event_name, r#"{"content":"<elided>"}"#);
        }
    });
}

/// Go `mux.SetURLVars(httptest.NewRequest(GET, …), vars)`.
fn request(vars: &[(&str, &str)]) -> Request {
    let mut req = Request::new(Body::empty());
    req.extensions_mut().insert(PathVars(
        vars.iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
    ));
    req
}

async fn read(resp: Response) -> (StatusCode, String) {
    let status = resp.status();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

/// Go `ignoreDynamicFields`: drops the time and ID entries from every object.
fn ignore_dynamic_fields(v: Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.into_iter()
                .filter(|(k, _)| {
                    !matches!(
                        k.as_str(),
                        "end_time"
                            | "observed_timestamp"
                            | "span_id"
                            | "start_time"
                            | "trace_id"
                            | "parent_span_id"
                    )
                })
                .map(|(k, v)| (k, ignore_dynamic_fields(v)))
                .collect::<Map<_, _>>(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(ignore_dynamic_fields).collect()),
        other => other,
    }
}

fn controller(dt: &DebugTelemetry) -> DebugApiController {
    DebugApiController::new(Arc::new(InMemorySessionService::new()), None, dt.clone())
}

#[tokio::test]
async fn test_session_spans_handler() {
    let found = json!([{
        "name": "test-span",
        "attributes": {
            "gcp.vertex.agent.event_id": "test-event",
            "gen_ai.conversation.id": "test-session",
            "gen_ai.operation.name": "execute_tool"
        },
        "logs": [{"event_name": "test-log-event", "body": {"message": "test log message"}}]
    }]);
    let cases = [
        (
            "spans_found_for_session",
            "test-session",
            "test-session",
            StatusCode::OK,
            Some(found),
        ),
        (
            "spans_not_found_for_session",
            "test-session",
            "other-session",
            StatusCode::OK,
            Some(json!([])),
        ),
        (
            "empty_session_id_param",
            "test-session",
            "",
            StatusCode::BAD_REQUEST,
            None,
        ),
    ];
    for (name, session_id, req_session_id, want_status, want_body) in cases {
        let dt = setup_test_telemetry();
        let c = controller(&dt);
        emit_test_signals(&dt, session_id, "test-event", "execute_tool");
        let (status, body) = read(
            c.session_spans_handler(request(&[("session_id", req_session_id)]))
                .await,
        )
        .await;
        assert_eq!(status, want_status, "{name}: status");
        if let Some(want) = want_body {
            let got: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(ignore_dynamic_fields(got), want, "{name}: body");
        }
    }
}

#[tokio::test]
async fn test_event_span_handler() {
    let want = |op: &str| {
        json!({
            "name": "test-span",
            "gcp.vertex.agent.event_id": "test-event",
            "gen_ai.conversation.id": "test-session",
            "gen_ai.operation.name": op,
            "logs": [{"event_name": "test-log-event", "body": {"message": "test log message"}}]
        })
    };
    let cases = [
        (
            "span_with_generate_content_operation",
            "test-event",
            "test-event",
            "generate_content",
            StatusCode::OK,
            Some(want("generate_content")),
        ),
        (
            "span_with_execute_tool_operation",
            "test-event",
            "test-event",
            "execute_tool",
            StatusCode::OK,
            Some(want("execute_tool")),
        ),
        (
            "span_not_found_for_event_id",
            "test-event",
            "other-event",
            "execute_tool",
            StatusCode::NOT_FOUND,
            None,
        ),
        (
            "span_with_different_operation_name",
            "test-event",
            "test-event",
            "other-op",
            StatusCode::NOT_FOUND,
            None,
        ),
        (
            "empty_event_id_param",
            "test-event",
            "",
            "execute_tool",
            StatusCode::BAD_REQUEST,
            None,
        ),
    ];
    for (name, event_id, req_event_id, op_name, want_status, want_body) in cases {
        let dt = setup_test_telemetry();
        let c = controller(&dt);
        emit_test_signals(&dt, "test-session", event_id, op_name);
        let (status, body) = read(
            c.event_span_handler(request(&[("event_id", req_event_id)]))
                .await,
        )
        .await;
        assert_eq!(status, want_status, "{name}: status");
        if let Some(want) = want_body {
            let got: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(ignore_dynamic_fields(got), want, "{name}: body");
        }
    }
}

/// Asserts on the raw bytes: JSON `null` where the web UI expects an array or object makes it
/// discard the whole response.
#[tokio::test]
async fn test_session_spans_handler_empty_containers_serialize_as_json_containers() {
    let cases: [(&str, bool, &[&str], &str); 2] = [
        (
            "span_without_logs_serializes_logs_as_empty_array",
            true,
            &[r#""logs":[]"#, r#""attributes":{"#],
            "",
        ),
        (
            "session_without_spans_serializes_as_empty_array",
            false,
            &[],
            "[]",
        ),
    ];
    for (name, emit_span, want_contains, want_exact) in cases {
        let dt = setup_test_telemetry();
        if emit_span {
            emit_span_without_logs(&dt, "test-session", "test-event", "execute_tool");
        }
        let (status, body) = read(
            controller(&dt)
                .session_spans_handler(request(&[("session_id", "test-session")]))
                .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{name}: status");
        let raw = body.trim();
        if !want_exact.is_empty() {
            assert_eq!(raw, want_exact, "{name}: raw body");
        }
        for want in want_contains {
            assert!(
                raw.contains(want),
                "{name}: raw body does not contain {want}:\n{raw}"
            );
        }
        assert!(
            !raw.contains("null"),
            "{name}: raw body contains a JSON null:\n{raw}"
        );
    }
}

/// The flattened per-event span path (Go `convertEventSpan`).
#[tokio::test]
async fn test_event_span_handler_empty_logs_serialize_as_json_array() {
    let dt = setup_test_telemetry();
    emit_span_without_logs(&dt, "test-session", "test-event", "generate_content");
    let (status, body) = read(
        controller(&dt)
            .event_span_handler(request(&[("event_id", "test-event")]))
            .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body {body:?}");
    let raw = body.trim();
    assert!(
        raw.contains(r#""logs":[]"#),
        "raw body does not contain \"logs\":[]:\n{raw}"
    );
    assert!(
        !raw.contains("null"),
        "raw body contains a JSON null:\n{raw}"
    );
}

/// Go `maskVolatileJSONFields`: `"start_time":<digits>`, `"end_time":<digits>`,
/// `"trace_id"|"span_id"|"parent_span_id":"<hex>"` and `"observed_timestamp":"<any>"` become
/// `"<name>":"masked"`.
fn mask_volatile_json_fields(raw: &str) -> String {
    let mut out = String::new();
    let mut rest = raw;
    'scan: while !rest.is_empty() {
        for (key, quoted, allowed) in [
            ("start_time", false, "0123456789"),
            ("end_time", false, "0123456789"),
            ("trace_id", true, "0123456789abcdef"),
            ("span_id", true, "0123456789abcdef"),
            ("parent_span_id", true, "0123456789abcdef"),
            ("observed_timestamp", true, ""),
        ] {
            let prefix = format!("\"{key}\":");
            let Some(after) = rest.strip_prefix(prefix.as_str()) else {
                continue;
            };
            let value_len = if quoted {
                let Some(inner) = after.strip_prefix('"') else {
                    continue;
                };
                let end = if allowed.is_empty() {
                    inner.find('"')
                } else {
                    inner
                        .find(|c: char| !allowed.contains(c))
                        .filter(|i| inner[*i..].starts_with('"'))
                };
                let Some(end) = end else { continue };
                end + 2
            } else {
                let end = after
                    .find(|c: char| !allowed.contains(c))
                    .unwrap_or(after.len());
                if end == 0 {
                    continue;
                }
                end
            };
            out.push_str(&format!("\"{key}\":\"masked\""));
            rest = &after[value_len..];
            continue 'scan;
        }
        let c = rest.chars().next().unwrap();
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

/// Asserts on the raw bytes: an elided user message becomes a `parts` object, a system message
/// keeps its string.
#[tokio::test]
async fn test_debug_handlers_serialize_elided_content_as_object() {
    let want_logs = concat!(
        r#""logs":["#,
        r#"{"body":{"content":"\u003celided\u003e"},"observed_timestamp":"masked","trace_id":"masked","span_id":"masked","event_name":"gen_ai.system.message"},"#,
        r#"{"body":{"content":{"parts":[{"text":"\u003celided\u003e"}],"role":"user"}},"observed_timestamp":"masked","trace_id":"masked","span_id":"masked","event_name":"gen_ai.user.message"}]"#
    );
    let session_want = format!(
        "{}{}{}",
        concat!(
            r#"[{"name":"test-span","start_time":"masked","end_time":"masked","span_id":"masked","trace_id":"masked","parent_span_id":"masked","#,
            r#""attributes":{"gcp.vertex.agent.event_id":"test-event","gen_ai.conversation.id":"test-session","gen_ai.operation.name":"generate_content"},"#
        ),
        want_logs,
        "}]"
    );
    let event_want = format!(
        "{}{}{}",
        r#"{"end_time":"masked","gcp.vertex.agent.event_id":"test-event","gen_ai.conversation.id":"test-session","gen_ai.operation.name":"generate_content","#,
        want_logs,
        r#","name":"test-span","parent_span_id":"masked","span_id":"masked","start_time":"masked","trace_id":"masked"}"#
    );
    for (name, vars, want) in [
        (
            "session_spans",
            ("session_id", "test-session"),
            session_want,
        ),
        ("event_span", ("event_id", "test-event"), event_want),
    ] {
        let dt = setup_test_telemetry();
        emit_elided_message_logs(&dt, "test-session", "test-event", "generate_content");
        let c = controller(&dt);
        let req = request(&[vars]);
        let resp = if name == "session_spans" {
            c.session_spans_handler(req).await
        } else {
            c.event_span_handler(req).await
        };
        let (status, body) = read(resp).await;
        assert_eq!(status, StatusCode::OK, "{name}: body {body:?}");
        assert_eq!(
            mask_volatile_json_fields(body.trim()),
            want,
            "{name}: raw body"
        );
    }
}

// parity: adapted: a NaN float cannot be JSON text, so the bad record's body text is not JSON
// (a NaN as Go's encoders would print it); the layer repairs it as Go repairs an unmarshalable body
#[tokio::test]
async fn test_session_spans_handler_unrepresentable_log_keeps_other_spans() {
    let dt = setup_test_telemetry();
    emit_elided_message_logs(&dt, "test-session", "good-event", "generate_content");
    with_layer(&dt, || {
        let bad = tracing::info_span!(parent: None, "bad-span", otel.name = "bad-span", gen_ai.conversation.id = "test-session");
        emit_log(
            &bad,
            "gen_ai.user.message",
            r#"{"content":"<elided>","index":NaN}"#,
        );
    });
    let (status, body) = read(
        controller(&dt)
            .session_spans_handler(request(&[("session_id", "test-session")]))
            .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let raw = body.trim();
    let spans: Vec<Value> = serde_json::from_str(raw).unwrap_or_else(|e| {
        panic!("response is not valid JSON, the bad record truncated it: {e}\n{raw}")
    });
    assert_eq!(
        spans.len(),
        2,
        "want 2 spans (test-span and bad-span):\n{raw}"
    );
    let want_good = r#""body":{"content":{"parts":[{"text":"\u003celided\u003e"}],"role":"user"}}"#;
    assert!(
        raw.contains(want_good),
        "the good span lost its body:\n{raw}"
    );
    let want_repaired =
        r#""body":{"content":{"parts":[{"text":"\u003cunrepresentable\u003e"}],"role":"user"}}"#;
    assert!(
        raw.contains(want_repaired),
        "the bad record was not repaired:\n{raw}"
    );
}

/// The event graph response is a single-key object holding DOT source.
#[tokio::test]
async fn test_event_graph_handler_response_shape() {
    let (app_name, user_id, session_id, event_id) =
        ("test-app", "test-user", "test-session", "test-event");
    let root_agent = gadk_agent::agent::new(AgentConfig {
        name: app_name.into(),
        description: "graph test agent".into(),
        ..Default::default()
    })
    .unwrap();
    let key = SessionKey {
        app_name: app_name.into(),
        user_id: user_id.into(),
        session_id: session_id.into(),
    };
    let event = Event {
        id: event_id.into(),
        author: app_name.into(),
        ..Default::default()
    };
    let sessions =
        FakeSessionService::new([TestSession::new(key, Map::new(), vec![event], Utc::now())]);
    let c = DebugApiController::new(
        Arc::new(sessions),
        Some(Arc::new(SingleLoader::new(root_agent))),
        setup_test_telemetry(),
    );
    let req = request(&[
        ("app_name", app_name),
        ("user_id", user_id),
        ("session_id", session_id),
        ("event_id", event_id),
    ]);
    let (status, body) = read(c.event_graph_handler(req).await).await;
    assert_eq!(status, StatusCode::OK, "body {body:?}");
    let got: BTreeMap<String, Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(got.len(), 1, "want exactly 1 key (dotSrc): {got:?}");
    let dot_src = got["dotSrc"].as_str().expect("dotSrc is a string");
    assert!(
        dot_src.contains("digraph"),
        "dotSrc is not DOT source: {dot_src:?}"
    );
    assert!(
        !body.contains("null"),
        "raw body contains a JSON null:\n{body}"
    );
}

// parity: skipped TestEventGraphHandlerRejectsANilAgent: a gadk Loader returns Arc<dyn Agent>, which cannot be null, so the nil-agent guard has nothing to guard

/// A tool that only has a name (the probe's `save_note` as the graph sees it).
struct NamedTool(&'static str);

impl Tool for NamedTool {
    fn name(&self) -> &str {
        self.0
    }
    fn description(&self) -> &str {
        ""
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// The probe's `assistant` (`scripts/gen-go-adkrest/SCENARIO.md`) as the graph sees it.
fn scenario_assistant() -> Arc<dyn Agent> {
    gadk_engine::llm_agent::new(LlmAgentConfig {
        name: "assistant".into(),
        description: "Answers greetings and saves notes.".into(),
        tools: vec![Arc::new(NamedTool("save_note"))],
        ..Default::default()
    })
    .unwrap()
}

fn event_with_parts(id: &str, parts: Vec<Part>) -> Event {
    Event {
        id: id.into(),
        author: "assistant".into(),
        response: LlmResponse {
            content: Some(Content {
                parts,
                role: "model".into(),
            }),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Fixtures `main_34`, `main_37`, `main_38`, `main_39`: status, Content-Type and body byte for
/// byte. Session `s1` holds the probe's function-call (`{call}`) and function-response
/// (`{resp}`) events; no span is stored, so `/debug/trace/nope` is the not-found answer.
#[tokio::test]
async fn debug_wire_fixtures_match_go() {
    let key = SessionKey {
        app_name: "assistant".into(),
        user_id: "alice".into(),
        session_id: "s1".into(),
    };
    let call = event_with_parts(
        "00000000-0000-4000-8000-000000000012",
        vec![Part {
            function_call: Some(FunctionCall {
                name: "save_note".into(),
                ..Default::default()
            }),
            ..Default::default()
        }],
    );
    let resp = Event {
        author: "assistant".into(),
        ..event_with_parts(
            "00000000-0000-4000-8000-000000000015",
            vec![Part {
                function_response: Some(FunctionResponse {
                    name: "save_note".into(),
                    ..Default::default()
                }),
                ..Default::default()
            }],
        )
    };
    let sessions: Arc<dyn SessionService> = Arc::new(FakeSessionService::new([TestSession::new(
        key,
        Map::new(),
        vec![call, resp],
        Utc::now(),
    )]));
    let c = DebugApiController::new(
        sessions,
        Some(Arc::new(SingleLoader::new(scenario_assistant()))),
        setup_test_telemetry(),
    );
    for fixture in [
        include_str!("../testdata/wire/main_34_debug_trace_event_missing.json"),
        include_str!("../testdata/wire/main_37_event_graph.json"),
        include_str!("../testdata/wire/main_38_event_graph_dev.json"),
        include_str!("../testdata/wire/main_39_event_graph_missing_event.json"),
    ] {
        let f: Value = serde_json::from_str(fixture).unwrap();
        let name = f["name"].as_str().unwrap();
        let target = f["request"]["target"].as_str().unwrap();
        let segments: Vec<&str> = target.trim_start_matches('/').split('/').collect();
        let resp = if target.starts_with("/debug/trace/") {
            c.event_span_handler(request(&[("event_id", segments[2])]))
                .await
        } else {
            // `[/dev]/apps/{app_name}/users/{user_id}/sessions/{session_id}/events/{event_id}/graph`
            let s = if segments[0] == "dev" {
                &segments[1..]
            } else {
                &segments[..]
            };
            c.event_graph_handler(request(&[
                ("app_name", s[1]),
                ("user_id", s[3]),
                ("session_id", s[5]),
                ("event_id", s[7]),
            ]))
            .await
        };
        let content_type = resp.headers()[http::header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .to_owned();
        let (status, body) = read(resp).await;
        let want = &f["response"];
        assert_eq!(
            u64::from(status.as_u16()),
            want["status"].as_u64().unwrap(),
            "{name}: status"
        );
        let want_ct = want["headers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h[0] == "Content-Type")
            .and_then(|h| h[1].as_str())
            .unwrap();
        assert_eq!(content_type, want_ct, "{name}: Content-Type");
        pretty_assertions::assert_eq!(body, want["body"].as_str().unwrap(), "{name}: body");
    }
}
