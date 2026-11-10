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

//! Ports of ADK-Go `server/adkrest/internal/services/debugtelemetry_test.go`.
//!
//! Go feeds the store from an OTel tracer and logger; these tests emit the equivalent `tracing`
//! spans and D6 log events (target `gadk_engine::telemetry::log`, fields `event.name`,
//! `otel.log.body`, `otel.log.attributes`) under a subscriber with the [`DebugTelemetry`] layer.
//! A Go root span (`tracer.Start` from a context without a span) is a span with `parent: None`.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Map, Value, json};
use tracing_subscriber::layer::SubscriberExt;

use crate::rest::debug_telemetry::{EVENT_ID_ATTRIBUTE, SpanRecord, convert_records};
use crate::rest::{DebugLog, DebugSpan, DebugTelemetry, DebugTelemetryConfig};

const CONVERSATION_ID: &str = "gen_ai.conversation.id";

/// Go `setupWithConfig`: a store with `trace_capacity` (0 = default).
fn setup_with_capacity(trace_capacity: i32) -> DebugTelemetry {
    DebugTelemetry::new(DebugTelemetryConfig { trace_capacity })
}

/// Go `setup`.
fn setup() -> DebugTelemetry {
    setup_with_capacity(0)
}

/// Runs `f` with a subscriber holding `dt` (Go's tracer and logger providers).
fn with_layer(dt: &DebugTelemetry, f: impl FnOnce()) {
    let sub = tracing_subscriber::registry().with(dt.clone());
    tracing::subscriber::with_default(sub, f);
}

/// Go `logger.Emit(ctx, record)` under `span`: `body` and `attributes` are JSON text.
fn emit_log(span: &tracing::Span, event_name: &str, body: &str, attributes: Option<&str>) {
    tracing::event!(
        target: "gadk_engine::telemetry::log",
        parent: span,
        tracing::Level::INFO,
        event.name = event_name,
        otel.log.body = body,
        otel.log.attributes = attributes
    );
}

/// A log as Go's tests compare it: `ObservedTimestamp`, `TraceID` and `SpanID` ignored, an empty
/// attribute map equal to none (`cmpopts.EquateEmpty`).
fn want_log(body: Value, attributes: Option<Value>, event_name: &str) -> DebugLog {
    DebugLog {
        body,
        attributes: attributes.and_then(|a| match a {
            Value::Object(m) => Some(m),
            _ => None,
        }),
        event_name: event_name.into(),
        ..Default::default()
    }
}

fn want_span(name: &str, attributes: &[(&str, &str)], logs: Vec<DebugLog>) -> DebugSpan {
    DebugSpan {
        name: name.into(),
        attributes: attributes
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
        logs,
        ..Default::default()
    }
}

/// Go's `cmpopts.IgnoreFields` (times and IDs), `EquateEmpty` and `SortSlices(compareDebugSpans)`.
fn comparable(spans: Vec<DebugSpan>) -> Vec<DebugSpan> {
    let mut spans: Vec<DebugSpan> = spans
        .into_iter()
        .map(|s| DebugSpan {
            start_time: 0,
            end_time: 0,
            span_id: String::new(),
            trace_id: String::new(),
            parent_span_id: String::new(),
            logs: s
                .logs
                .into_iter()
                .map(|l| DebugLog {
                    observed_timestamp: String::new(),
                    trace_id: String::new(),
                    span_id: String::new(),
                    attributes: l.attributes.filter(|a| !a.is_empty()),
                    ..l
                })
                .collect(),
            ..s
        })
        .collect();
    spans.sort_by(compare_debug_spans);
    spans
}

/// Go `compareDebugSpans`.
fn compare_debug_spans(a: &DebugSpan, b: &DebugSpan) -> std::cmp::Ordering {
    let attr = |s: &DebugSpan, k: &str| s.attributes.get(k).cloned().unwrap_or_default();
    a.name
        .cmp(&b.name)
        .then_with(|| a.parent_span_id.cmp(&b.parent_span_id))
        .then_with(|| attr(a, CONVERSATION_ID).cmp(&attr(b, CONVERSATION_ID)))
        .then_with(|| attr(a, EVENT_ID_ATTRIBUTE).cmp(&attr(b, EVENT_ID_ATTRIBUTE)))
        .then_with(|| attr(a, "genai.operation.name").cmp(&attr(b, "genai.operation.name")))
}

#[test]
fn test_debug_telemetry_get_spans_by_session_id() {
    type Setup = fn();
    let cases: Vec<(&str, Setup, &str, Vec<DebugSpan>)> = vec![
        (
            "root span with conversation id",
            || {
                let root = tracing::info_span!(parent: None, "root-span", gen_ai.conversation.id = "session-1");
                let child = tracing::info_span!(parent: &root, "child-span");
                emit_log(&child, "child-log-event", r#""child-log-body""#, None);
                drop(child);
                emit_log(
                    &root,
                    "root-log-event",
                    r#""root-log-body""#,
                    Some(r#"{"gen_ai.input.messages":[{"role":"user"}]}"#),
                );
            },
            "session-1",
            vec![
                want_span(
                    "root-span",
                    &[(CONVERSATION_ID, "session-1")],
                    vec![want_log(
                        json!("root-log-body"),
                        Some(json!({"gen_ai.input.messages": [{"role": "user"}]})),
                        "root-log-event",
                    )],
                ),
                want_span(
                    "child-span",
                    &[],
                    vec![want_log(json!("child-log-body"), None, "child-log-event")],
                ),
            ],
        ),
        (
            "child span with conversation id",
            || {
                let root = tracing::info_span!(parent: None, "root");
                let child = tracing::info_span!(parent: &root, "child");
                let second_child = tracing::info_span!(parent: &root, "child-2");
                let third_child = tracing::info_span!(parent: &child, "grandchild", gen_ai.conversation.id = "test-session-id");
                drop(third_child);
                drop(second_child);
                drop(child);
                drop(root);
                // Another trace with a different session ID (should not be returned).
                let root3 = tracing::info_span!(parent: None, "root-3", gen_ai.conversation.id = "test-session-id-1");
                drop(root3);
            },
            "test-session-id",
            vec![
                want_span("root", &[], vec![]),
                want_span("child", &[], vec![]),
                want_span("child-2", &[], vec![]),
                want_span(
                    "grandchild",
                    &[(CONVERSATION_ID, "test-session-id")],
                    vec![],
                ),
            ],
        ),
        (
            "multiple traces with same session id",
            || {
                // Trace 1.
                let root1 = tracing::info_span!(parent: None, "root-1", gen_ai.conversation.id = "session-1");
                drop(tracing::info_span!(parent: &root1, "child-1"));
                drop(root1);
                // Trace 2 (different trace ID, same session ID on the child span).
                let root2 = tracing::info_span!(parent: None, "root-2");
                drop(
                    tracing::info_span!(parent: &root2, "child-2", gen_ai.conversation.id = "session-1"),
                );
                drop(root2);
            },
            "session-1",
            vec![
                want_span("root-1", &[(CONVERSATION_ID, "session-1")], vec![]),
                want_span("child-1", &[], vec![]),
                want_span("root-2", &[], vec![]),
                want_span("child-2", &[(CONVERSATION_ID, "session-1")], vec![]),
            ],
        ),
        (
            "trace with spans with mixed session ids session-1",
            mixed_session_ids,
            "session-1",
            vec![
                want_span("mixed-root", &[(CONVERSATION_ID, "session-1")], vec![]),
                want_span("mixed-child", &[(CONVERSATION_ID, "session-2")], vec![]),
            ],
        ),
        (
            "trace with spans with mixed session ids session-2",
            mixed_session_ids,
            "session-2",
            vec![
                want_span("mixed-root", &[(CONVERSATION_ID, "session-1")], vec![]),
                want_span("mixed-child", &[(CONVERSATION_ID, "session-2")], vec![]),
            ],
        ),
        (
            "no matching session id",
            || {
                drop(tracing::info_span!(
                    parent: None,
                    "root-1",
                    gen_ai.conversation.id = "session-1",
                    gcp.vertex.agent.event_id = "event-1"
                ));
            },
            "non-existent-session",
            vec![],
        ),
        (
            "log without span",
            || {
                tracing::event!(
                    target: "gadk_engine::telemetry::log",
                    parent: None,
                    tracing::Level::INFO,
                    event.name = "test_event",
                    otel.log.body = r#""test body""#
                );
            },
            "session-1",
            vec![],
        ),
    ];
    for (name, setup_fn, query_session_id, want) in cases {
        let dt = setup();
        with_layer(&dt, setup_fn);
        let got = comparable(dt.get_spans_by_session_id(query_session_id));
        assert_eq!(
            comparable(want),
            got,
            "{name}: GetSpansBySessionID() mismatch"
        );
    }
}

fn mixed_session_ids() {
    let root =
        tracing::info_span!(parent: None, "mixed-root", gen_ai.conversation.id = "session-1");
    drop(tracing::info_span!(parent: &root, "mixed-child", gen_ai.conversation.id = "session-2"));
    drop(root);
}

#[test]
fn test_debug_telemetry_get_spans_by_event_id() {
    type Setup = fn();
    let cases: Vec<(&str, Setup, &str, Vec<DebugSpan>)> = vec![
        (
            "single span and log",
            || {
                let span = tracing::info_span!(
                    parent: None,
                    "root-1",
                    gcp.vertex.agent.event_id = "event-1",
                    genai.operation.name = "generate_content"
                );
                emit_log(&span, "test_event", r#""test body""#, None);
            },
            "event-1",
            vec![want_span(
                "root-1",
                &[
                    (EVENT_ID_ATTRIBUTE, "event-1"),
                    ("genai.operation.name", "generate_content"),
                ],
                vec![want_log(json!("test body"), None, "test_event")],
            )],
        ),
        (
            "multiple spans",
            || {
                let span1 = tracing::info_span!(
                    parent: None,
                    "root-1",
                    gcp.vertex.agent.event_id = "event-1",
                    genai.operation.name = "generate_content"
                );
                let span2 = tracing::info_span!(
                    parent: &span1,
                    "root-2",
                    gcp.vertex.agent.event_id = "event-1",
                    genai.operation.name = "execute_tool"
                );
                drop(span2);
                drop(span1);
            },
            "event-1",
            vec![
                want_span(
                    "root-1",
                    &[
                        (EVENT_ID_ATTRIBUTE, "event-1"),
                        ("genai.operation.name", "generate_content"),
                    ],
                    vec![],
                ),
                want_span(
                    "root-2",
                    &[
                        (EVENT_ID_ATTRIBUTE, "event-1"),
                        ("genai.operation.name", "execute_tool"),
                    ],
                    vec![],
                ),
            ],
        ),
        (
            "no matching span",
            || {
                drop(tracing::info_span!(
                    parent: None,
                    "root-1",
                    gcp.vertex.agent.event_id = "event-1",
                    genai.operation.name = "generate_content"
                ));
            },
            "non-existent-event",
            vec![],
        ),
        (
            "log without span",
            || {
                tracing::event!(
                    target: "gadk_engine::telemetry::log",
                    parent: None,
                    tracing::Level::INFO,
                    event.name = "test_event",
                    otel.log.body = r#""test body""#
                );
            },
            "event-1",
            vec![],
        ),
    ];
    for (name, setup_fn, query_event_id, want) in cases {
        let dt = setup();
        with_layer(&dt, setup_fn);
        let got = comparable(dt.get_spans_by_event_id(query_event_id));
        assert_eq!(
            comparable(want),
            got,
            "{name}: GetSpansByEventID() mismatch"
        );
    }
}

#[test]
fn test_debug_telemetry_lru() {
    let dt = setup_with_capacity(2);
    let root = |name: &'static str, session: &'static str, event: &'static str| {
        with_layer(&dt, || {
            drop(tracing::info_span!(
                parent: None,
                "root",
                otel.name = name,
                gen_ai.conversation.id = session,
                gcp.vertex.agent.event_id = event
            ));
        });
    };
    let count = |session: &str| dt.get_spans_by_session_id(session).len();

    // 1–2. Add traces 1 and 2.
    root("root-1", "session-1", "event-1");
    root("root-2", "session-2", "event-2");
    // 3. Both traces are present.
    assert_eq!(count("session-1"), 1, "expected 1 span for session-1");
    assert_eq!(count("session-2"), 1, "expected 1 span for session-2");
    // 4. Access session-2, making it the most recently used.
    let _ = dt.get_spans_by_session_id("session-2");
    // 5. Add trace 3: evicts trace 1, the least recently used.
    root("root-3", "session-3", "event-3");
    // 6. Trace 1 is evicted; traces 2 and 3 are present.
    assert_eq!(count("session-1"), 0, "expected 0 spans for session-1");
    assert_eq!(count("session-2"), 1, "expected 1 span for session-2");
    assert_eq!(count("session-3"), 1, "expected 1 span for session-3");
    // 7. Trace 1's spans left the event index.
    assert_eq!(
        dt.get_spans_by_event_id("event-1").len(),
        0,
        "expected 0 spans for event-1"
    );
    // 8. Access trace 2 by event ID, making it the most recently used.
    let _ = dt.get_spans_by_event_id("event-2");
    // 9. Add trace 4: evicts trace 3.
    root("root-4", "session-4", "event-4");
    // 10. Trace 3 is evicted; traces 2 and 4 are present.
    assert_eq!(count("session-2"), 1, "expected 1 span for session-2");
    assert_eq!(count("session-4"), 1, "expected 1 span for session-4");
    assert_eq!(count("session-3"), 0, "expected 0 spans for session-3");
}

/// A span record with Go's `trace.SpanContext{TraceID: {t}, SpanID: {s}}`.
fn record(name: &str, t: u8, s: u8) -> SpanRecord {
    let mut trace_id = [0; 16];
    trace_id[0] = t;
    let mut span_id = [0; 8];
    span_id[0] = s;
    SpanRecord {
        name: name.into(),
        trace_id,
        span_id,
        ..Default::default()
    }
}

/// Asserts on the serialized bytes: an absent container would be JSON `null`, which the web
/// UI's schema rejects.
#[test]
fn test_convert_records_builds_empty_containers_not_nil() {
    let cases: [(&str, Vec<SpanRecord>, &str); 2] = [
        (
            "record with nil logs and nil attributes",
            vec![record("span", 0x01, 0x02)],
            r#"[{"name":"span","start_time":-6795364578871345152,"end_time":-6795364578871345152,"span_id":"0200000000000000","trace_id":"01000000000000000000000000000000","parent_span_id":"0000000000000000","attributes":{},"logs":[]}]"#,
        ),
        ("no records", vec![], "[]"),
    ];
    for (name, records, want) in cases {
        let got = convert_records(records.into_iter().map(Arc::new).collect());
        let got = String::from_utf8(gadk_model::to_go_json(&got).unwrap()).unwrap();
        assert_eq!(got, want, "{name}: convertRecords JSON");
    }
}

/// Go `emitSpanWithLog`: a span of `session_id` carrying one log record.
fn emit_span_with_log(
    dt: &DebugTelemetry,
    span_name: &'static str,
    session_id: &'static str,
    event_name: &'static str,
    body: &'static str,
) {
    with_layer(dt, || {
        let span = tracing::info_span!(parent: None, "span", otel.name = span_name, gen_ai.conversation.id = session_id);
        emit_log(&span, event_name, body, None);
    });
}

fn session_json(dt: &DebugTelemetry, session_id: &str) -> String {
    String::from_utf8(gadk_model::to_go_json(&dt.get_spans_by_session_id(session_id)).unwrap())
        .unwrap()
}

/// With content capture off the body's `content` is the string `<elided>`; the web UI requires
/// an object with a `parts` array for user messages and choices.
#[test]
fn test_message_log_body_serializes_content_as_object() {
    let cases = [
        (
            "elided user message content becomes an object",
            "gen_ai.user.message",
            r#"{"content":"<elided>"}"#,
            r#""body":{"content":{"parts":[{"text":"\u003celided\u003e"}],"role":"user"}}"#,
        ),
        (
            "elided choice content becomes an object and keeps its other fields",
            "gen_ai.choice",
            r#"{"content":"<elided>","index":0,"finish_reason":"STOP"}"#,
            r#""body":{"content":{"parts":[{"text":"\u003celided\u003e"}],"role":"model"},"finish_reason":"STOP","index":0}"#,
        ),
        (
            "captured user message content is unchanged",
            "gen_ai.user.message",
            r#"{"content":{"parts":[{"text":"hello"}],"role":"user"}}"#,
            r#""body":{"content":{"parts":[{"text":"hello"}],"role":"user"}}"#,
        ),
        (
            "elided system message keeps its string content",
            "gen_ai.system.message",
            r#"{"content":"<elided>"}"#,
            r#""body":{"content":"\u003celided\u003e"}"#,
        ),
    ];
    for (name, event_name, body, want) in cases {
        let dt = setup();
        emit_span_with_log(&dt, "span", "session-1", event_name, body);
        let got = session_json(&dt, "session-1");
        assert!(
            got.contains(want),
            "{name}: spans JSON does not contain\n{want}\ngot\n{got}"
        );
    }
}

// parity: adapted: a NaN float cannot be JSON text, so the D6 record carries body text that is
// not JSON (a NaN as Go's encoders would print it); the layer treats it as Go's unmarshalable body
#[test]
fn test_unrepresentable_log_body_does_not_poison_other_spans() {
    let cases = [
        (
            "message record with an unmarshalable field",
            "gen_ai.choice",
            r#"{"content":"<elided>","index":NaN}"#,
            r#""body":{"content":{"parts":[{"text":"\u003cunrepresentable\u003e"}],"role":"model"}}"#,
        ),
        (
            "other record with an unmarshalable body",
            "gen_ai.system.message",
            "NaN",
            r#""body":{"content":"\u003cunrepresentable\u003e"}"#,
        ),
    ];
    for (name, event_name, body, want) in cases {
        let dt = setup();
        emit_span_with_log(
            &dt,
            "good-span",
            "session-1",
            "gen_ai.user.message",
            r#"{"content":"<elided>"}"#,
        );
        emit_span_with_log(&dt, "bad-span", "session-1", event_name, body);
        let raw = session_json(&dt, "session-1");
        let want_good =
            r#""body":{"content":{"parts":[{"text":"\u003celided\u003e"}],"role":"user"}}"#;
        assert!(
            raw.contains(want_good),
            "{name}: the good span lost its body:\n{raw}"
        );
        assert!(
            raw.contains(want),
            "{name}: the bad span was not repaired: want\n{want}\ngot\n{raw}"
        );
        for span_name in [r#""name":"good-span""#, r#""name":"bad-span""#] {
            assert!(
                raw.contains(span_name),
                "{name}: spans JSON is missing {span_name}:\n{raw}"
            );
        }
    }
}

/// The span ID stands in for the event ID of a `generate_content` span without one.
#[test]
fn test_convert_records_synthesizes_event_id_for_failed_generate_content() {
    // failed generate_content keeps its span and gains an event id
    let records = vec![Arc::new(record("generate_content gemini", 0x02, 0x02))];
    let got = convert_records(records.clone());
    assert_eq!(got.len(), 1, "the span must be kept, not dropped");
    assert_eq!(
        got[0].attributes.get(EVENT_ID_ATTRIBUTE),
        Some(&got[0].span_id)
    );
    // The stored record is not touched: it is shared with the store.
    assert!(!records[0].attributes.contains_key(EVENT_ID_ATTRIBUTE));

    // a real event id is left alone
    let mut real = record("generate_content gemini", 0x03, 0x03);
    real.attributes = BTreeMap::from([(EVENT_ID_ATTRIBUTE.to_owned(), "event-1".to_owned())]);
    let got = convert_records(vec![Arc::new(real)]);
    assert_eq!(got[0].attributes[EVENT_ID_ATTRIBUTE], "event-1");

    // other spans do not gain the attribute
    let got = convert_records(vec![Arc::new(record("invoke_agent weather", 0x04, 0x04))]);
    assert!(!got[0].attributes.contains_key(EVENT_ID_ATTRIBUTE));
}

// parity: adapted: Go drives telemetry.InstrumentGenerateContent; this emits the D6
// inference-details record that call produces (gen_ai message parts as gadk-engine's
// genai_messages writes them, NaN payloads already "<unserializable>") and checks the web-UI
// rewrite of its parts
#[test]
fn test_inference_details_parts_match_web_ui() {
    let dt = setup();
    let attributes = json!({
        "gen_ai.system_instructions": [
            {"type": "uri", "mime_type": "text/plain", "modality": "text", "uri": "gs://b/rules"}
        ],
        "gen_ai.input.messages": [
            {"role": "user", "parts": [
                {"type": "blob", "modality": "", "content": "eA=="},
                {"type": "uri", "mime_type": "image/png", "modality": "image", "uri": "gs://b/o"}
            ]},
            {"role": "tool", "parts": [
                {"type": "tool_call_response", "name": "f", "response": "<unserializable>"}
            ]}
        ],
        "gen_ai.output.messages": [
            {"role": "assistant", "finish_reason": "", "parts": [
                {"type": "reasoning", "content": "thinking"},
                {"type": "tool_call", "name": "f", "arguments": "<unserializable>"},
                {"type": "tool_call", "name": "g"}
            ]}
        ]
    })
    .to_string();
    with_layer(&dt, || {
        let agent =
            tracing::info_span!(parent: None, "invoke_agent", gen_ai.conversation.id = "session-1");
        let gc = tracing::info_span!(parent: &agent, "generate_content", otel.name = "generate_content m");
        tracing::event!(
            target: "gadk_engine::telemetry::log",
            parent: &gc,
            tracing::Level::DEBUG,
            event.name = "gen_ai.client.inference.operation.details",
            otel.log.body = "null",
            otel.log.attributes = attributes.as_str()
        );
    });
    let logs: Vec<DebugLog> = dt
        .get_spans_by_session_id("session-1")
        .into_iter()
        .filter(|s| s.name.starts_with("generate_content"))
        .flat_map(|s| s.logs)
        .collect();
    assert_eq!(logs.len(), 1, "expected 1 log, got {}", logs.len());
    let want = json!({
        "gen_ai.system_instructions": [
            {"type": "file_data", "mime_type": "text/plain", "uri": "gs://b/rules"}
        ],
        "gen_ai.input.messages": [
            {"role": "user", "parts": [
                {"type": "blob", "mime_type": "", "data": "eA=="},
                {"type": "file_data", "mime_type": "image/png", "uri": "gs://b/o"}
            ]},
            {"role": "tool", "parts": [
                {"type": "tool_call_response", "name": "f", "response": {"value": "<unserializable>"}}
            ]}
        ],
        "gen_ai.output.messages": [
            {"role": "assistant", "finish_reason": "", "parts": [
                {"type": "text", "content": "thinking"},
                {"type": "tool_call", "name": "f", "arguments": {"value": "<unserializable>"}},
                {"type": "tool_call", "name": "g", "arguments": null}
            ]}
        ]
    });
    let attrs = logs[0].attributes.clone().unwrap_or_default();
    let got: Map<String, Value> = want
        .as_object()
        .unwrap()
        .keys()
        .map(|k| (k.clone(), attrs.get(k).cloned().unwrap_or(Value::Null)))
        .collect();
    assert_eq!(Value::Object(got), want, "message attributes mismatch");
}

// parity: adapted: a NaN attribute cannot be JSON text; the record's attribute text is not JSON
#[test]
fn test_unrepresentable_log_attributes_are_dropped() {
    let dt = setup();
    with_layer(&dt, || {
        let span = tracing::info_span!(parent: None, "span", gen_ai.conversation.id = "session-1");
        emit_log(&span, "some.event", "null", Some(r#"{"nan":NaN}"#));
    });
    let spans = dt.get_spans_by_session_id("session-1");
    assert!(gadk_model::to_go_json(&spans).is_ok());
    assert_eq!(spans[0].logs[0].attributes, None, "attributes = want none");
}
