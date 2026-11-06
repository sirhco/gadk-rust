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

//! Parity port of Go internal/llminternal/contents_processor_internal_test.go.

use bytes::Bytes;
use gadk_genai::{Content, FinishReason, FunctionCall, FunctionResponse, Part};
use gadk_model::LlmResponse;
use gadk_session::Event;
use pretty_assertions::assert_eq;
use serde_json::json;

use crate::llm::processors::{
    capture_logs,
    contents::{drop_orphaned_function_calls, drop_orphaned_function_responses},
};

fn ev(role: &str, parts: Vec<Part>) -> Event {
    Event {
        response: LlmResponse {
            content: Some(Content {
                role: role.into(),
                parts,
            }),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn text(t: &str) -> Part {
    Part {
        text: t.into(),
        ..Default::default()
    }
}

// parity: adapted: Go log.Printf is captured from tracing (target "adk"); pointer-identity checks become value checks on the untouched input
#[tokio::test]
async fn test_drop_orphaned_function_responses_log_and_clone() {
    let event = Event {
        response: LlmResponse {
            model_version: "model-version".into(),
            finish_reason: Some(FinishReason::Stop),
            turn_complete: true,
            content: Some(Content {
                role: "user".into(),
                parts: vec![
                    Part {
                        function_response: Some(FunctionResponse {
                            id: "stale\nforged".into(),
                            response: json!({"secret": "payload"}).as_object().unwrap().clone(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    Part {
                        text: "Keep this".into(),
                        function_response: Some(FunctionResponse {
                            id: "other".into(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    text("Keep this too"),
                ],
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut want = event.clone();
    want.response.content = Some(Content {
        role: "user".into(),
        parts: vec![text("Keep this"), text("Keep this too")],
    });
    let input = vec![event.clone()];
    let ((got, _remnants), log) =
        capture_logs(async { drop_orphaned_function_responses(input, &[]) }).await;
    assert_eq!(got, vec![want]);
    assert_eq!(
        log,
        "adk: dropping function responses with no matching function call: [\"stale\\nforged\" \"other\"]\n"
    );
    // The caller's event is untouched (Rust: we passed a clone; the original still has 3 parts).
    assert_eq!(event.response.content.as_ref().unwrap().parts.len(), 3);
}

// parity: adapted: Go pointer identity (clone vs stored event) is checked as value equality; log captured from tracing
#[tokio::test]
async fn test_drop_orphaned_function_calls() {
    let call = |id: &str| Part {
        function_call: Some(FunctionCall {
            id: id.into(),
            name: "tool".into(),
            ..Default::default()
        }),
        thought_signature: Bytes::from(format!("sig-{id}")),
        ..Default::default()
    };
    let response = |id: &str| Part {
        function_response: Some(FunctionResponse {
            id: id.into(),
            name: "tool".into(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let parallel = ev(
        "model",
        vec![
            text("Calling tools."),
            call("answered"),
            call("orphan_1"),
            call(""),
        ],
    );
    let only_orphan = ev("model", vec![call("orphan_2")]);
    let mut long_running = ev("model", vec![call("long_running"), call("confirmation")]);
    long_running.long_running_tool_ids = vec!["long_running".into(), "confirmation".into()];
    let answer = ev("user", vec![response("answered")]);
    let user = ev("user", vec![text("Are you still there?")]);
    let events = vec![
        parallel.clone(),
        answer.clone(),
        only_orphan,
        long_running.clone(),
        user.clone(),
    ];

    let mut want_parallel = parallel.clone();
    want_parallel.response.content.as_mut().unwrap().parts =
        vec![text("Calling tools."), call("answered"), call("")];
    let (got, log) = capture_logs(async { drop_orphaned_function_calls(events) }).await;
    assert_eq!(got, vec![want_parallel, answer, long_running, user]);
    assert_eq!(
        log,
        "adk: dropping function calls with no matching function response: [\"orphan_1\" \"orphan_2\"]\n"
    );
}

// parity: adapted: Go slices.Equal pointer identity becomes value equality; log captured from tracing
#[tokio::test]
async fn test_drop_orphaned_function_calls_nothing_to_drop() {
    let events = vec![
        // Go genai.NewContentFromFunctionCall("tool", nil, "model"): a call with an empty ID.
        ev(
            "model",
            vec![Part {
                function_call: Some(FunctionCall {
                    name: "tool".into(),
                    ..Default::default()
                }),
                ..Default::default()
            }],
        ),
        ev(
            "model",
            vec![Part {
                function_call: Some(FunctionCall {
                    id: "answered".into(),
                    name: "tool".into(),
                    ..Default::default()
                }),
                ..Default::default()
            }],
        ),
        ev(
            "user",
            vec![Part {
                function_response: Some(FunctionResponse {
                    id: "answered".into(),
                    name: "tool".into(),
                    ..Default::default()
                }),
                ..Default::default()
            }],
        ),
        ev("user", vec![text("hello")]),
    ];
    let (got, log) = capture_logs(async { drop_orphaned_function_calls(events.clone()) }).await;
    assert_eq!(
        got, events,
        "drop_orphaned_function_calls changed a history with no orphaned call"
    );
    assert_eq!(log, "", "log = {log:?}, want nothing");
}

// parity: skipped TestPromptTokenEstimator_SuffixDoesNotLogPairedResponseAsOrphan: promptTokenEstimator belongs to compaction (deferred to 1c)
