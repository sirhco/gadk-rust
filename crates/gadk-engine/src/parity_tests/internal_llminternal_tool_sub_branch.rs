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

//! Parity port of Go internal/llminternal/tool_sub_branch_test.go.

use gadk_genai::{Content, FunctionCall, FunctionResponse, Part, ROLE_MODEL, ROLE_USER};
use gadk_model::LlmResponse;
use gadk_session::Event;
use pretty_assertions::assert_eq;
use serde_json::json;

use crate::llm::processors::contents::{
    build_contents_current_turn_context_only, build_contents_default,
};

fn sub_branch_event(author: &str, branch: &str, content: Content) -> Event {
    Event {
        author: author.into(),
        branch: branch.into(),
        response: LlmResponse {
            content: Some(content),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// A root agent that ran a single_turn sub-agent as a tool gets only the tool's function
/// response, not the sub-agent's own transcript from the tool's sub-branch.
#[test]
#[ignore = "8a bug: the contents processor shows a tool sub-agent's transcript from its <agent>@<call id> sub-branch to the calling agent, and in current-turn mode it starts the turn there; Go ab78c5ae skips tool sub-branch events"]
fn test_build_contents_skips_tool_sub_branch_events() {
    let call = Content::from_parts(
        vec![Part {
            function_call: Some(FunctionCall {
                id: "fc1".into(),
                name: "translator".into(),
                args: json!({"request": "hello"}).as_object().cloned().unwrap(),
                ..Default::default()
            }),
            ..Default::default()
        }],
        ROLE_MODEL,
    );
    let resp = Content::from_parts(
        vec![Part {
            function_response: Some(FunctionResponse {
                id: "fc1".into(),
                name: "translator".into(),
                response: json!({"result": "hola"}).as_object().cloned().unwrap(),
                ..Default::default()
            }),
            ..Default::default()
        }],
        ROLE_USER,
    );
    let events = vec![
        sub_branch_event("user", "", Content::from_text("turn1", ROLE_USER)),
        sub_branch_event("coordinator", "", call.clone()),
        sub_branch_event(
            "translator",
            "translator@fc1",
            Content::from_text("hola", ROLE_MODEL),
        ),
        sub_branch_event("coordinator", "", resp.clone()),
        sub_branch_event("coordinator", "", Content::from_text("done1", ROLE_MODEL)),
        sub_branch_event("user", "", Content::from_text("turn2", ROLE_USER)),
    ];
    let want = vec![
        Content::from_text("turn1", ROLE_USER),
        call,
        resp,
        Content::from_text("done1", ROLE_MODEL),
        Content::from_text("turn2", ROLE_USER),
    ];

    let got = build_contents_default("coordinator", "", "", &events, false, None)
        .unwrap_or_else(|e| panic!("default: {e}"));
    assert_eq!(want, got, "default: contents");

    // With no history, the sub-agent's reply must not become the start of the current turn,
    // or the call and its response would be cut off.
    let got =
        build_contents_current_turn_context_only("coordinator", "", "", &events[..4], false, None)
            .unwrap_or_else(|e| panic!("current turn only: {e}"));
    assert_eq!(want[..3].to_vec(), got, "current turn only: contents");
}
