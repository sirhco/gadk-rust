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

//! Ports of ADK-Go `agent/remoteagent/v2/utils_test.go`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use gadk_agent::{AgentConfig, InvocationContext, new_agent};
use gadk_genai::{
    Content, File, FunctionCall, FunctionResponse, Language, Outcome, Part, ROLE_MODEL, ROLE_USER,
};
use gadk_platform::Platform;
use gadk_session::{Event, zero_time};
use serde_json::{Map, Value, json};

use super::agent_remoteagent_v2_a2a_agent::harness::prepare_session;
use crate::convert::{GenAIPartConverter, to_a2a_meta_key, to_custom_metadata};
use crate::remoteagent::A2AConfig;
use crate::remoteagent::agent::new_message;
use gadk_engine::runner::BEFORE_RUN_REPLY_KEY;

use crate::remoteagent::history::{
    collect_remote_function_call_ids, convert_parts, get_user_function_call_at,
    marshal_function_response_json, present_as_user_message, to_missing_remote_session_parts,
    unmatched_function_response_text,
};

/// Go `newTestInvocationContext`: a session holding `events` and an agent named `agent_name`.
async fn new_test_invocation_context(agent_name: &str, events: Vec<Event>) -> InvocationContext {
    let (_, session) = prepare_session("test", events).await;
    let agent = new_agent(AgentConfig {
        name: agent_name.to_owned(),
        ..Default::default()
    })
    .unwrap();
    InvocationContext::builder()
        .agent(agent)
        .session(session)
        .build_invocation()
}

/// Go `newEventFromParts`.
fn event_from_parts(author: &str, parts: Vec<Part>) -> Event {
    let role = if author == "user" {
        ROLE_USER
    } else {
        ROLE_MODEL
    };
    let mut event = Event::new(&Platform::default(), "");
    event.author = author.to_owned();
    if !parts.is_empty() {
        event.response.content = Some(Content::from_parts(parts, role));
    }
    event
}

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        other => panic!("not an object: {other}"),
    }
}

fn call(id: &str, name: &str) -> Part {
    Part {
        function_call: Some(FunctionCall {
            id: id.to_owned(),
            name: name.to_owned(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn response(id: &str, name: &str, resp: Value) -> Part {
    Part {
        function_response: Some(FunctionResponse {
            id: id.to_owned(),
            name: name.to_owned(),
            response: obj(resp),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn text(t: &str) -> Part {
    Part::from_text(t)
}

fn thought(t: &str) -> Part {
    Part {
        text: t.to_owned(),
        thought: true,
        ..Default::default()
    }
}

fn part_text(part: &a2a::Part) -> &str {
    part.as_text().unwrap_or_default()
}

fn is_function_response(part: &a2a::Part) -> bool {
    part.metadata
        .as_ref()
        .and_then(|m| m.get(&to_a2a_meta_key("type")))
        == Some(&Value::String("function_response".to_owned()))
}

async fn missing_parts(ictx: &InvocationContext) -> (Vec<a2a::Part>, String) {
    let events = ictx.session().events();
    to_missing_remote_session_parts(ictx, &events, &A2AConfig::default()).await
}

#[tokio::test]
async fn test_get_user_function_call_at() {
    let remote = "test-agent";
    struct Case {
        name: &'static str,
        events: Vec<Event>,
        at_index: isize,
        success: bool,
    }
    let cases = vec![
        Case {
            name: "success",
            events: vec![
                event_from_parts(remote, vec![call("id-1", "")]),
                event_from_parts("user", vec![response("id-1", "", json!({}))]),
            ],
            at_index: 1,
            success: true,
        },
        Case {
            name: "success with event in-between",
            events: vec![
                event_from_parts(remote, vec![call("id-1", "")]),
                event_from_parts(remote, vec![text("another event")]),
                event_from_parts("user", vec![response("id-1", "", json!({}))]),
            ],
            at_index: 2,
            success: true,
        },
        Case {
            name: "success with multiple parts in-between",
            events: vec![
                event_from_parts(
                    remote,
                    vec![text("calling"), call("id-1", ""), text("called")],
                ),
                event_from_parts(
                    "user",
                    vec![
                        text("responding"),
                        response("id-1", "", json!({})),
                        text("responded"),
                    ],
                ),
            ],
            at_index: 1,
            success: true,
        },
        Case {
            name: "failf if not response index",
            events: vec![
                event_from_parts(remote, vec![call("id-1", "")]),
                event_from_parts(remote, vec![text("another event")]),
                event_from_parts("user", vec![response("id-1", "", json!({}))]),
            ],
            at_index: 1,
            success: false,
        },
        Case {
            name: "fail if not user author",
            events: vec![
                event_from_parts(remote, vec![call("id-1", "")]),
                event_from_parts(remote, vec![response("id-1", "", json!({}))]),
            ],
            at_index: 0,
            success: false,
        },
        Case {
            name: "fail if no matching function call",
            events: vec![
                event_from_parts(remote, vec![call("id-2", "")]),
                event_from_parts("user", vec![response("id-1", "", json!({}))]),
            ],
            at_index: 0,
            success: false,
        },
        Case {
            name: "fail if call authored by another agent",
            events: vec![
                event_from_parts("coordinator", vec![call("id-1", "")]),
                event_from_parts("user", vec![response("id-1", "", json!({}))]),
            ],
            at_index: 1,
            success: false,
        },
        Case {
            name: "fail if function call ID is empty",
            events: vec![
                event_from_parts(remote, vec![call("", "peer_tool")]),
                event_from_parts("user", vec![response("", "peer_tool", json!({}))]),
            ],
            at_index: 1,
            success: false,
        },
    ];
    for tc in cases {
        let ictx = new_test_invocation_context(remote, tc.events).await;
        let got = get_user_function_call_at(&ictx.session().events(), tc.at_index, remote, "");
        assert_eq!(got.is_some(), tc.success, "{}: got {got:?}", tc.name);
    }
}

#[tokio::test]
async fn test_to_missing_remote_session_parts() {
    let remote = "remote-agent";
    let mut with_context = event_from_parts(remote, vec![text("hi")]);
    with_context.response.custom_metadata = to_custom_metadata(&a2a::new_task_id(), "ctxID-123");
    let cases: Vec<(&str, Vec<Event>, Vec<a2a::Part>, &str)> = vec![
        (
            "all message parts collected",
            vec![
                event_from_parts("user", vec![text("hello")]),
                event_from_parts("user", vec![text("foo"), text("bar")]),
            ],
            vec![
                a2a::Part::text("hello"),
                a2a::Part::text("foo"),
                a2a::Part::text("bar"),
            ],
            "",
        ),
        (
            "other agent messages are rephrased",
            vec![
                event_from_parts("another-agent", vec![text("foo")]),
                event_from_parts("user", vec![text("bar")]),
            ],
            vec![
                a2a::Part::text("For context:"),
                a2a::Part::text("[another-agent] said: foo"),
                a2a::Part::text("bar"),
            ],
            "",
        ),
        (
            "other agent thoughts are skipped",
            vec![
                event_from_parts("another-agent", vec![thought("foo")]),
                event_from_parts("user", vec![text("bar")]),
            ],
            vec![a2a::Part::text("bar")],
            "",
        ),
        (
            "events before the last remote response excluded",
            vec![
                event_from_parts("user", vec![text("hello")]),
                event_from_parts(remote, vec![text("hi")]),
                event_from_parts("user", vec![text("foo")]),
                event_from_parts("user", vec![text("bar")]),
            ],
            vec![a2a::Part::text("foo"), a2a::Part::text("bar")],
            "",
        ),
        (
            "contextID of the last remote agent response returned",
            vec![with_context],
            vec![],
            "ctxID-123",
        ),
    ];
    for (name, events, want_parts, want_context_id) in cases {
        let ictx = new_test_invocation_context(remote, events).await;
        let (got_parts, got_context_id) = missing_parts(&ictx).await;
        assert_eq!(got_context_id, want_context_id, "{name}");
        assert_eq!(got_parts, want_parts, "{name}");
    }
}

#[tokio::test]
async fn test_present_as_user_message() {
    let cases: Vec<(&str, Event, Event)> = vec![
        (
            "text presented",
            event_from_parts("some agent", vec![text("hello")]),
            event_from_parts(
                "user",
                vec![text("For context:"), text("[some agent] said: hello")],
            ),
        ),
        (
            "function call presented",
            event_from_parts(
                "some agent",
                vec![Part::from_function_call(
                    "get_weather",
                    obj(json!({"city": "Warsaw"})),
                )],
            ),
            event_from_parts(
                "user",
                vec![
                    text("For context:"),
                    // Go fmt.Sprintf("%v", map[string]any{"city": "Warsaw"})
                    text("[some agent] called tool get_weather with parameters: map[city:Warsaw]"),
                ],
            ),
        ),
        (
            "function call result presented",
            event_from_parts(
                "some agent",
                vec![Part::from_function_response(
                    "get_weather",
                    obj(json!({"temp": "1C"})),
                )],
            ),
            event_from_parts(
                "user",
                vec![
                    text("For context:"),
                    text("[some agent] get_weather tool returned result: map[temp:1C]"),
                ],
            ),
        ),
        (
            "other part types unmodified",
            event_from_parts(
                "some agent",
                vec![
                    Part::from_file(&File {
                        name: "cat.png".into(),
                        ..Default::default()
                    }),
                    Part::from_executable_code("print('hello, world!')", Language::Python),
                    Part::from_code_execution_result(Outcome::Ok, "hello, world!"),
                ],
            ),
            event_from_parts(
                "user",
                vec![
                    text("For context:"),
                    Part::from_file(&File {
                        name: "cat.png".into(),
                        ..Default::default()
                    }),
                    Part::from_executable_code("print('hello, world!')", Language::Python),
                    Part::from_code_execution_result(Outcome::Ok, "hello, world!"),
                ],
            ),
        ),
        (
            "thought skipped",
            event_from_parts("some agent", vec![thought("hello")]),
            event_from_parts("user", vec![]),
        ),
        (
            "thought with other parts",
            event_from_parts("some agent", vec![thought("thinking..."), text("done")]),
            event_from_parts(
                "user",
                vec![text("For context:"), text("[some agent] said: done")],
            ),
        ),
    ];
    // Go ignores ID, InvocationID and Timestamp.
    let normalize = |mut e: Event| {
        e.id = String::new();
        e.invocation_id = String::new();
        e.timestamp = zero_time();
        e
    };
    for (name, input, want) in cases {
        let ictx = new_test_invocation_context("test", vec![]).await;
        let got = present_as_user_message(&ictx, &input);
        assert_eq!(normalize(got), normalize(want), "{name}");
    }
}

#[tokio::test]
async fn test_probe_unmatched_function_response_sent_raw() {
    // A function response answering a call the remote peer never made.
    let fr = event_from_parts(
        "user",
        vec![response("fc-local", "local_tool", json!({"v": 1}))],
    );
    // Not the last event, so the get_user_function_call_at resume path is not taken.
    let tail = event_from_parts("user", vec![text("and now ask the peer")]);
    let ictx = new_test_invocation_context("remote-agent", vec![fr, tail]).await;

    let msg = new_message(&ictx, &A2AConfig::default()).await.unwrap();
    assert_eq!(msg.parts.len(), 2);
    // Unmatched function response must be rendered as text, not a raw A2A function_response.
    let got0 = part_text(&msg.parts[0]);
    assert!(
        got0.starts_with("Tool local_tool returned:"),
        "{:?}",
        msg.parts[0]
    );
    assert!(got0.contains(r#""v":1"#), "{got0}");
    assert!(!is_function_response(&msg.parts[0]));
    assert_eq!(part_text(&msg.parts[1]), "and now ask the peer");
}

#[tokio::test]
async fn test_to_missing_remote_session_parts_keeps_matched_function_response() {
    let remote = "remote-agent";
    let events = vec![
        event_from_parts(remote, vec![call("fc-remote", "peer_tool")]),
        event_from_parts(
            "user",
            vec![response("fc-remote", "peer_tool", json!({"ok": true}))],
        ),
        event_from_parts("user", vec![text("continue")]),
    ];
    // Last remote event is the function call, so missing parts start at the response.
    let ictx = new_test_invocation_context(remote, events).await;
    let (got, _) = missing_parts(&ictx).await;
    assert!(got.len() >= 2, "{got:?}");
    // Matched response should remain a data part with function_response type.
    assert!(is_function_response(&got[0]), "{:?}", got[0]);
    assert_eq!(part_text(&got[1]), "continue");
}

#[tokio::test]
async fn test_to_missing_remote_session_parts_coordinator_call_still_rewritten() {
    let remote = "remote-agent";
    // Coordinator authored the matching call ID: the author filter must exclude it from the
    // remote call IDs so the response is still rewritten to text.
    let events = vec![
        event_from_parts("coordinator", vec![call("fc-shared", "local_tool")]),
        event_from_parts(remote, vec![text("hi")]),
        event_from_parts(
            "user",
            vec![response("fc-shared", "local_tool", json!({"v": 1}))],
        ),
        event_from_parts("user", vec![text("ask peer")]),
    ];
    let ictx = new_test_invocation_context(remote, events).await;
    let (got, _) = missing_parts(&ictx).await;
    assert!(got.len() >= 2, "{got:?}");
    assert!(
        part_text(&got[0]).starts_with("Tool local_tool returned:"),
        "coordinator call must not count as remote: {:?}",
        got[0]
    );
    assert!(!is_function_response(&got[0]));
}

#[tokio::test]
async fn test_to_missing_remote_session_parts_empty_agent_name_keeps_own_response() {
    // Anonymous agent (empty name): peer events also use author "". Own call+response must stay
    // a function_response, not be flattened.
    let events = vec![
        event_from_parts("", vec![call("fc-own", "peer_tool")]),
        event_from_parts(
            "user",
            vec![response("fc-own", "peer_tool", json!({"ok": true}))],
        ),
        event_from_parts("user", vec![text("continue")]),
    ];
    let ictx = new_test_invocation_context("", events).await;
    let (got, _) = missing_parts(&ictx).await;
    assert!(got.len() >= 2, "{got:?}");
    assert!(
        is_function_response(&got[0]),
        "empty-name own response flattened incorrectly: {:?}",
        got[0]
    );
}

#[tokio::test]
async fn test_to_missing_remote_session_parts_empty_call_id_not_collected() {
    let remote = "remote-agent";
    let events = vec![
        event_from_parts(remote, vec![call("", "peer_tool")]),
        event_from_parts("user", vec![response("", "peer_tool", json!({"ok": true}))]),
        event_from_parts("user", vec![text("continue")]),
    ];
    let ictx = new_test_invocation_context(remote, events).await;
    let (got, _) = missing_parts(&ictx).await;
    assert!(!got.is_empty());
    assert!(
        part_text(&got[0]).starts_with("Tool peer_tool returned:"),
        "empty call ID response should be rewritten to text, got {:?}",
        got[0]
    );
}

#[tokio::test]
async fn test_to_missing_remote_session_parts_mixed_function_response_and_sibling_parts() {
    let remote = "remote-agent";
    let events = vec![event_from_parts(
        "user",
        vec![
            text("before"),
            response("fc-foreign", "local_tool", json!({"x": "y"})),
            text("after"),
        ],
    )];
    let ictx = new_test_invocation_context(remote, events).await;
    let (got, _) = missing_parts(&ictx).await;
    assert_eq!(got.len(), 3);
    assert_eq!(part_text(&got[0]), "before");
    let got1 = part_text(&got[1]);
    assert!(got1.starts_with("Tool local_tool returned:"), "{got1}");
    assert!(got1.contains(r#""x":"y""#), "{got1}");
    assert_eq!(part_text(&got[2]), "after");
}

#[tokio::test]
async fn test_convert_parts_rewritten_skips_gen_ai_part_converter() {
    let event = event_from_parts(
        "user",
        vec![response(
            "fc-foreign",
            "local_tool",
            json!({"url": "https://x/?a=1&b=2"}),
        )],
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let converter: GenAIPartConverter = Arc::new(move |_, _| {
        counter.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(Some(a2a::Part::text("converter-should-not-run"))) })
    });
    let cfg = A2AConfig {
        genai_part_converter: Some(converter),
        ..Default::default()
    };
    let parts = convert_parts(&cfg, &event, Some(&HashSet::new()))
        .await
        .unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "GenAIPartConverter must not run on the rewritten path"
    );
    assert_eq!(parts.len(), 1);
    let got = part_text(&parts[0]);
    assert!(got.starts_with("Tool local_tool returned:"), "{got}");
    // HTML must not be escaped.
    assert!(!got.contains(r"\u0026") && got.contains('&'), "{got}");
    assert!(got.contains(r#""url":"https://x/?a=1&b=2""#), "{got}");
}

#[test]
fn test_marshal_function_response_json() {
    let got = marshal_function_response_json(&obj(json!({"result": "done", "url": "a&b<c>"})));
    assert!(
        !got.contains(r"\u0026") && !got.contains(r"\u003c"),
        "HTML-escaped unexpectedly: {got}"
    );
    assert!(got.contains(r#""url":"a&b<c>""#), "{got}");
}

// parity: adapted: a serde_json payload cannot hold NaN or a cycle (Go's two unencodable shapes); the nearest shape, a NaN float, becomes JSON null when stored, so the text never leaks NaN or a Go map dump
#[test]
fn test_marshal_function_response_json_unserializable() {
    // NaN: serde_json stores a non-finite float as null.
    let mut payload = Map::new();
    payload.insert("score".to_owned(), Value::from(f64::NAN));
    let got = marshal_function_response_json(&payload);
    assert_eq!(got, r#"{"score":null}"#);
    assert!(!got.contains("map[") && !got.contains("NaN"), "{got}");
}

#[test]
fn test_unmatched_function_response_text_empty_name() {
    let got = unmatched_function_response_text(&FunctionResponse {
        name: String::new(),
        response: obj(json!({"ok": true})),
        ..Default::default()
    });
    assert!(!got.contains("Tool  returned:"), "double space in {got}");
    assert!(got.starts_with("Tool <unnamed> returned:"), "{got}");
}

#[tokio::test]
async fn test_new_message_resume_keeps_mixed_function_responses_as_data() {
    // Parallel tool merge can put a peer FR and a local-tool FR in one user event. Resume must
    // keep both as data, never flatten the local one to text alongside.
    let remote = "remote-agent";
    let events = vec![
        event_from_parts(remote, vec![call("fc-remote", "peer_tool")]),
        event_from_parts(
            "user",
            vec![
                response("fc-remote", "peer_tool", json!({"ok": true})),
                response("fc-local", "local_tool", json!({"secret": "s"})),
            ],
        ),
    ];
    let ictx = new_test_invocation_context(remote, events).await;
    let msg = new_message(&ictx, &A2AConfig::default()).await.unwrap();
    assert_eq!(msg.parts.len(), 2);
    for (i, p) in msg.parts.iter().enumerate() {
        assert!(
            p.as_text().is_none(),
            "part[{i}] was rewritten to text; resume must keep all FRs as data"
        );
        assert!(is_function_response(p), "part[{i}] = {p:?}");
    }
}

#[tokio::test]
async fn test_new_message_before_run_call_reuses_remote_call_id() {
    let remote = "remote-agent";
    let mut remote_call = event_from_parts(remote, vec![call("reused", "lookup")]);
    remote_call.response.custom_metadata = to_custom_metadata("completed-task", "remote-context");
    let mut completed = event_from_parts(remote, vec![text("completed reply")]);
    completed.response.custom_metadata = to_custom_metadata("completed-task", "remote-context");
    let resp = || {
        event_from_parts(
            "user",
            vec![response("reused", "lookup", json!({"ok": true}))],
        )
    };
    let mut cached_call = event_from_parts(remote, vec![call("reused", "lookup")]);
    cached_call.response.custom_metadata = obj(json!({BEFORE_RUN_REPLY_KEY: true}));
    let ictx = new_test_invocation_context(
        remote,
        vec![
            remote_call,
            resp(),
            completed,
            event_from_parts("user", vec![text("cached question")]),
            cached_call,
            resp(),
        ],
    )
    .await;
    let msg = new_message(&ictx, &A2AConfig::default()).await.unwrap();
    assert!(
        msg.task_id.is_none() && msg.context_id.as_deref() == Some("remote-context"),
        "local call resumed a completed task or lost the remote context"
    );
    let want = vec![
        a2a::Part::text("cached question"),
        a2a::Part::text("For context:"),
        a2a::Part::text("[remote-agent] called tool lookup with parameters: map[]"),
        a2a::Part::text(r#"Tool lookup returned: {"ok":true}"#),
    ];
    assert_eq!(
        msg.parts, want,
        "local call and response were not replayed as contextual text"
    );
}

#[tokio::test]
async fn test_collect_remote_function_call_i_ds_empty_name_collects_any_author() {
    // Anonymous agent: author gate skipped; coordinator-authored call IDs are collected so
    // matched responses are not incorrectly flattened on the history path.
    let events = vec![
        event_from_parts("coordinator", vec![call("fc-coord", "tool")]),
        event_from_parts(
            "user",
            vec![response("fc-coord", "tool", json!({"ok": true}))],
        ),
        event_from_parts("user", vec![text("continue")]),
    ];
    let ictx = new_test_invocation_context("", events).await;
    let ids = collect_remote_function_call_ids(&ictx.session().events(), "", "");
    assert!(ids.contains("fc-coord"), "ids={ids:?}");
    let (got, _) = missing_parts(&ictx).await;
    assert!(
        got.iter().any(is_function_response),
        "coordinator-matched response flattened with empty agent name: {got:?}"
    );
}

// parity: adapted: a Rust content holds no nil parts; the remaining contract (an unmatched function response on the history path becomes one rewritten text part) is pinned
#[tokio::test]
async fn test_convert_parts_nil_part_skipped() {
    let event = event_from_parts(
        "user",
        vec![response("fc-foreign", "local_tool", json!({"ok": true}))],
    );
    let parts = convert_parts(&A2AConfig::default(), &event, Some(&HashSet::new()))
        .await
        .unwrap();
    assert_eq!(parts.len(), 1);
    assert!(
        part_text(&parts[0]).starts_with("Tool local_tool returned:"),
        "{:?}",
        parts[0]
    );
}
