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

//! Parity port of Go internal/llminternal/functions_test.go.

use std::collections::BTreeMap;

use gadk_agent::{AgentConfig, InvocationContext, REQUEST_CONFIRMATION_FUNCTION_CALL_NAME, agent};
use gadk_genai::{Content, FunctionCall, FunctionResponse, Part};
use gadk_model::LlmResponse;
use gadk_session::{Event, EventActions, ToolConfirmation, toolauth};
use pretty_assertions::assert_eq;
use serde_json::{Map, json};

use crate::llm::functions::{generate_auth_event, generate_request_confirmation_event};
use crate::tool::utils::function_calls;

/// Go mockInvocationContext{invocationID, agentName, branch}.
fn ctx(branch: &str) -> InvocationContext {
    InvocationContext::builder()
        .agent(
            agent::new(AgentConfig {
                name: "agent_1".into(),
                ..Default::default()
            })
            .unwrap(),
        )
        .invocation_id("inv_1")
        .branch(branch)
        .build_invocation()
}

fn fc_event(parts: Vec<Part>) -> Event {
    Event {
        response: LlmResponse {
            content: Some(Content {
                parts,
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn confirming_call(id: &str) -> FunctionCall {
    FunctionCall {
        id: id.into(),
        name: "test_tool".into(),
        args: json!({"arg": "val"}).as_object().unwrap().clone(),
        ..Default::default()
    }
}

fn fc_part(fc: FunctionCall, sig: &[u8]) -> Part {
    Part {
        function_call: Some(fc),
        thought_signature: sig.to_vec().into(),
        ..Default::default()
    }
}

fn resp_event(confs: &[(&str, &str)]) -> Event {
    Event {
        actions: EventActions {
            requested_tool_confirmations: confs
                .iter()
                .map(|(id, h)| {
                    (
                        id.to_string(),
                        ToolConfirmation {
                            hint: h.to_string(),
                            ..Default::default()
                        },
                    )
                })
                .collect::<BTreeMap<_, _>>(),
            ..Default::default()
        },
        ..Default::default()
    }
}

#[test]
fn test_generate_request_confirmation_event() {
    // case "no confirmation requested"
    assert_eq!(
        generate_request_confirmation_event(
            &ctx(""),
            &fc_event(vec![fc_part(confirming_call("call_1"), b"")]),
            &resp_event(&[])
        ),
        None
    );
    // case "confirmation requested but no matching function call"
    let other = FunctionCall {
        id: "other_call".into(),
        ..Default::default()
    };
    assert_eq!(
        generate_request_confirmation_event(
            &ctx(""),
            &fc_event(vec![fc_part(other, b"")]),
            &resp_event(&[("call_1", "Are you sure?")])
        ),
        None
    );
    // case "confirmation requested and matching function call": compare everything except the
    // generated ID/timestamp/long-running IDs/function-call ID, as Go does with
    // cmpopts.IgnoreFields(Event{Timestamp, LongRunningToolIDs, ID}, FunctionCall{ID}).
    let mut got = generate_request_confirmation_event(
        &ctx("main"),
        &fc_event(vec![fc_part(confirming_call("call_1"), b"")]),
        &resp_event(&[("call_1", "Are you sure?")]),
    )
    .unwrap();
    for id in &got.long_running_tool_ids {
        assert!(!id.is_empty(), "empty long running tool id");
    }
    let mut args = Map::new();
    args.insert(
        "originalFunctionCall".into(),
        serde_json::to_value(confirming_call("call_1")).unwrap(),
    );
    args.insert(
        "toolConfirmation".into(),
        serde_json::to_value(ToolConfirmation {
            hint: "Are you sure?".into(),
            ..Default::default()
        })
        .unwrap(),
    );
    let want = Event {
        invocation_id: "inv_1".into(),
        author: "agent_1".into(),
        branch: "main".into(),
        actions: EventActions {
            state_delta: Some(Map::new()),
            artifact_delta: Some(BTreeMap::new()),
            ..Default::default()
        },
        response: LlmResponse {
            content: Some(Content {
                role: "model".into(),
                parts: vec![Part {
                    function_call: Some(FunctionCall {
                        name: REQUEST_CONFIRMATION_FUNCTION_CALL_NAME.into(),
                        args,
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    // Ignored fields.
    let want = Event {
        id: got.id.clone(),
        timestamp: got.timestamp,
        long_running_tool_ids: got.long_running_tool_ids.clone(),
        ..want
    };
    got.response.content.as_mut().unwrap().parts[0]
        .function_call
        .as_mut()
        .unwrap()
        .id
        .clear();
    assert_eq!(got, want);
    // The wire shape the model sees (Go marshals the *genai.FunctionCall and ToolConfirmation).
    let fc = got.response.content.as_ref().unwrap().parts[0]
        .function_call
        .as_ref()
        .unwrap();
    assert_eq!(
        fc.args["originalFunctionCall"],
        json!({"id": "call_1", "name": "test_tool", "args": {"arg": "val"}})
    );
    assert_eq!(
        fc.args["toolConfirmation"],
        json!({"hint": "Are you sure?", "confirmed": false, "payload": null})
    );
}

#[test]
fn test_generate_request_confirmation_event_has_id() {
    let got = generate_request_confirmation_event(
        &ctx("main"),
        &fc_event(vec![fc_part(confirming_call("call_1"), b"")]),
        &resp_event(&[("call_1", "Are you sure?")]),
    )
    .expect("expected non-nil event");
    assert!(
        !got.id.is_empty(),
        "event ID is empty; events must have a UUID for time-travel and session lookup"
    );
    assert_eq!(got.invocation_id, "inv_1");
}

#[test]
fn test_generate_request_confirmation_event_preserves_thought_signature() {
    let got = generate_request_confirmation_event(
        &ctx(""),
        &fc_event(vec![fc_part(
            confirming_call("call_1"),
            b"test-thought-signature",
        )]),
        &resp_event(&[("call_1", "Are you sure?")]),
    )
    .unwrap();
    let parts = &got.response.content.as_ref().unwrap().parts;
    assert_eq!(parts.len(), 1);
    assert_eq!(&parts[0].thought_signature[..], b"test-thought-signature");
}

#[test]
fn test_generate_request_confirmation_event_response_order() {
    let response_order = ["call_c", "call_a", "call_d", "call_b"];
    let parts: Vec<Part> = response_order
        .iter()
        .map(|id| {
            fc_part(
                FunctionCall {
                    id: (*id).into(),
                    name: "test_tool".into(),
                    args: json!({"arg": id}).as_object().unwrap().clone(),
                    ..Default::default()
                },
                b"",
            )
        })
        .collect();
    let hints: Vec<(String, String)> = response_order
        .iter()
        .map(|id| (id.to_string(), format!("confirm {id}")))
        .collect();
    let hint_refs: Vec<(&str, &str)> = hints
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let call_ev = fc_event(parts);
    let resp_ev = resp_event(&hint_refs);
    let ctx = ctx("main");

    let order_of = |ev: Option<Event>| -> Vec<String> {
        let ev = ev.expect("expected non-nil event with content");
        let parts = &ev.response.content.as_ref().unwrap().parts;
        assert_eq!(parts.len(), response_order.len());
        assert_eq!(ev.long_running_tool_ids.len(), response_order.len());
        parts
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let fc = p.function_call.as_ref().unwrap();
                assert_eq!(fc.name, REQUEST_CONFIRMATION_FUNCTION_CALL_NAME);
                // LongRunningToolIDs[i] is the request-confirmation call ID at the same index.
                assert_eq!(ev.long_running_tool_ids[i], fc.id);
                serde_json::from_value::<FunctionCall>(fc.args["originalFunctionCall"].clone())
                    .unwrap()
                    .id
            })
            .collect()
    };

    let first = order_of(generate_request_confirmation_event(
        &ctx, &call_ev, &resp_ev,
    ));
    assert_eq!(first, response_order);
    for _ in 0..10 {
        assert_eq!(
            order_of(generate_request_confirmation_event(
                &ctx, &call_ev, &resp_ev
            )),
            first
        );
    }
}

#[test]
fn test_generate_request_confirmation_event_no_thought_signature() {
    let got = generate_request_confirmation_event(
        &ctx(""),
        &fc_event(vec![fc_part(confirming_call("call_1"), b"")]),
        &resp_event(&[("call_1", "Are you sure?")]),
    )
    .unwrap();
    let parts = &got.response.content.as_ref().unwrap().parts;
    assert_eq!(parts.len(), 1);
    assert!(parts[0].thought_signature.is_empty());
}

/// Go toolResponses: a user event with one function response per ID, in that order, carrying
/// `requests`.
fn tool_responses(requests: &[(&str, toolauth::Config)], ids: &[&str]) -> Event {
    Event {
        response: LlmResponse {
            content: Some(Content {
                role: "user".into(),
                parts: ids
                    .iter()
                    .map(|id| Part {
                        function_response: Some(FunctionResponse {
                            id: id.to_string(),
                            name: "calendar".into(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    })
                    .collect(),
            }),
            ..Default::default()
        },
        actions: EventActions {
            requested_auth_configs: requests
                .iter()
                .map(|(id, c)| (id.to_string(), c.clone()))
                .collect(),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Go apiKeyRequest: a credential request for an API key stored under `key`.
fn api_key_request(key: &str) -> toolauth::Config {
    toolauth::Config {
        auth_scheme: toolauth::Scheme {
            r#type: toolauth::SchemeType::API_KEY,
            r#in: "header".into(),
            name: "X-Key".into(),
            ..Default::default()
        },
        credential_key: key.into(),
        ..Default::default()
    }
}

#[test]
fn test_generate_auth_event() {
    let responses = tool_responses(
        &[
            ("fc-3", api_key_request("key-3")),
            ("fc-1", api_key_request("key-1")),
        ],
        &["fc-1", "fc-2", "fc-3"],
    );

    let mut got = generate_auth_event(&ctx("main"), &responses).expect("generateAuthEvent() = nil");

    let scheme = json!({"type": "apiKey", "in": "header", "name": "X-Key"});
    let request = |id: &str, key: &str| Part {
        function_call: Some(FunctionCall {
            name: "adk_request_credential".into(),
            args: json!({
                "functionCallId": id,
                "authConfig": {"authScheme": scheme, "credentialKey": key},
            })
            .as_object()
            .unwrap()
            .clone(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let want = Event {
        invocation_id: "inv_1".into(),
        author: "agent_1".into(),
        branch: "main".into(),
        actions: EventActions {
            state_delta: Some(Map::new()),
            artifact_delta: Some(BTreeMap::new()),
            ..Default::default()
        },
        response: LlmResponse {
            content: Some(Content {
                role: "user".into(),
                parts: vec![request("fc-1", "key-1"), request("fc-3", "key-3")],
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(
        !got.id.is_empty(),
        "event ID is empty, want one from session.NewEvent"
    );
    let call_ids: Vec<String> = function_calls(got.response.content.as_ref())
        .iter()
        .map(|c| c.id.clone())
        .collect();
    assert!(
        !call_ids[0].is_empty() && call_ids[0] != call_ids[1],
        "request call IDs = {call_ids:?}, want two distinct non-empty IDs"
    );
    assert_eq!(call_ids, got.long_running_tool_ids, "LongRunningToolIDs");
    // Ignored fields (Go cmpopts.IgnoreFields(Event{ID, Timestamp, LongRunningToolIDs}, FunctionCall{ID})).
    let want = Event {
        id: got.id.clone(),
        timestamp: got.timestamp,
        long_running_tool_ids: got.long_running_tool_ids.clone(),
        ..want
    };
    for p in &mut got.response.content.as_mut().unwrap().parts {
        p.function_call.as_mut().unwrap().id.clear();
    }
    assert_eq!(got, want);
}

#[test]
fn test_generate_auth_event_one_call_per_credential_key() {
    // (name, requests, functionCallId of each request in order)
    type Case = (
        &'static str,
        Vec<(&'static str, toolauth::Config)>,
        Vec<&'static str>,
    );
    let cases: Vec<Case> = vec![
        (
            "same key",
            vec![
                ("fc-1", api_key_request("k")),
                ("fc-2", api_key_request("k")),
            ],
            vec!["fc-1"],
        ),
        (
            "different keys",
            vec![
                ("fc-1", api_key_request("a")),
                ("fc-2", api_key_request("b")),
            ],
            vec!["fc-1", "fc-2"],
        ),
        (
            "no keys",
            vec![("fc-1", api_key_request("")), ("fc-2", api_key_request(""))],
            vec!["fc-1", "fc-2"],
        ),
    ];
    for (name, requests, want) in cases {
        let got = generate_auth_event(&ctx(""), &tool_responses(&requests, &["fc-1", "fc-2"]))
            .unwrap_or_else(|| panic!("{name}: generateAuthEvent() = nil"));
        let got_ids: Vec<&str> = function_calls(got.response.content.as_ref())
            .iter()
            .map(|c| c.args["functionCallId"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(got_ids, want, "{name}: requested function call IDs");
    }
}

#[test]
fn test_generate_auth_event_without_requests() {
    let got = generate_auth_event(&ctx(""), &tool_responses(&[], &["fc-1"]));
    assert_eq!(got, None);
}

// parity: skipped TestGenerateAuthEventRequestThatDoesNotEncode: a toolauth::Config always encodes (its unknown keys are kept as valid JSON text, so a NaN cannot be put in one); gadk's generate_auth_event cannot fail
