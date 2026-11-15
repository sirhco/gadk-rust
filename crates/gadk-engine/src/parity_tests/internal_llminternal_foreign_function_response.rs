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

//! Parity port of Go internal/llminternal/foreign_function_response_test.go.

use gadk_genai::{Content, FunctionCall, FunctionResponse, Part, ROLE_MODEL, ROLE_USER};
use gadk_model::LlmResponse;
use gadk_session::Event;
use pretty_assertions::assert_eq;
use serde_json::json;

use crate::llm::processors::contents::{
    build_contents_current_turn_context_only, build_contents_default,
    build_contents_default_with_call_source,
};

fn event(author: &str, content: Content) -> Event {
    Event {
        author: author.into(),
        response: LlmResponse {
            content: Some(content),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn on_branch(branch: &str, mut ev: Event) -> Event {
    ev.branch = branch.into();
    ev
}

/// The role is kept as given (`Content::from_parts` would turn an empty role into "user").
fn call_with_role(role: &str, id: &str) -> Content {
    Content {
        parts: vec![Part {
            function_call: Some(FunctionCall {
                id: id.into(),
                name: "adk_request_input".into(),
                args: json!({"message": "approve"}).as_object().cloned().unwrap(),
                ..Default::default()
            }),
            ..Default::default()
        }],
        role: role.into(),
    }
}

fn call(id: &str) -> Content {
    call_with_role(ROLE_MODEL, id)
}

fn response_part(id: &str) -> Part {
    Part {
        function_response: Some(FunctionResponse {
            id: id.into(),
            name: "adk_request_input".into(),
            response: json!({"result": "ok"}).as_object().cloned().unwrap(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn response(id: &str) -> Content {
    Content::from_parts(vec![response_part(id)], ROLE_USER)
}

/// A "For context:" turn with the given lines.
fn context(lines: &[&str]) -> Content {
    let mut parts = vec![Part::from_text("For context:")];
    parts.extend(lines.iter().map(|l| Part::from_text(*l)));
    Content::from_parts(parts, ROLE_USER)
}

const CALLED_B: &str =
    r#"[b] called tool `adk_request_input` with parameters: {"message":"approve"}"#;
const ANSWERED: &str = r#"[user] `adk_request_input` tool returned result: {"result":"ok"}"#;

fn start() -> Content {
    Content::from_text("start", ROLE_USER)
}

/// A user's function response to a call another agent made reaches the current agent as
/// context, like the call itself, as in adk-python's `_get_contents`. Left as a raw response,
/// its call is missing from the history: it is dropped mid-history and fails the request when
/// it is the last event.
#[test]
fn test_build_contents_user_response_to_other_agent_call_is_context() {
    let done = context(&["[d] said: done"]);
    // (name, branch, events, want)
    let cases: Vec<(&str, &str, Vec<Event>, Vec<Content>)> = vec![
        (
            "response is the last event",
            "",
            vec![
                event("user", start()),
                event("b", call("b1")),
                event("user", response("b1")),
            ],
            vec![start(), context(&[CALLED_B]), context(&[ANSWERED])],
        ),
        (
            "response mid-history",
            "",
            vec![
                event("user", start()),
                event("b", call("b1")),
                event("user", response("b1")),
                event("d", Content::from_text("done", ROLE_MODEL)),
            ],
            vec![
                start(),
                context(&[CALLED_B]),
                context(&[ANSWERED]),
                done.clone(),
            ],
        ),
        (
            "response to the agent's own call stays a response",
            "",
            vec![
                event("user", start()),
                event("after", call("a1")),
                event("user", response("a1")),
            ],
            vec![start(), call("a1"), response("a1")],
        ),
        (
            // Some providers reuse call ids. A response pairs with the latest call before
            // it, so a later call with the same id does not turn the agent's own exchange
            // into context.
            "call id reused by another agent later",
            "",
            vec![
                event("user", start()),
                event("after", call("c0")),
                event("after", response("c0")),
                event("b", call("c0")),
                event("user", response("c0")),
            ],
            vec![
                start(),
                call("c0"),
                response("c0"),
                context(&[CALLED_B]),
                context(&[ANSWERED]),
            ],
        ),
        (
            "response to a call the user made stays a response",
            "",
            vec![
                event("user", start()),
                event("user", call("u1")),
                event("user", response("u1")),
            ],
            vec![start(), call("u1"), response("u1")],
        ),
        (
            // The call reaches the agent as text, so nothing is left for the response to
            // pair with and history rearrangement drops it.
            "response without an id is not matched",
            "",
            vec![
                event("user", start()),
                event("b", call("")),
                event("user", response("")),
                event("d", Content::from_text("done", ROLE_MODEL)),
            ],
            vec![start(), context(&[CALLED_B]), done.clone()],
        ),
        (
            // The call is on a branch this agent does not see, so the response stays a raw
            // response, and history rearrangement drops it because no call before it
            // matches. adk-python drops it too, as an orphan.
            "response to a call outside the agent's branch is not shown",
            "root.after",
            vec![
                event("user", start()),
                on_branch("root.wf", event("b", call("b1"))),
                event("user", response("b1")),
                event("d", Content::from_text("done", ROLE_MODEL)),
            ],
            vec![start(), done.clone()],
        ),
    ];
    for (name, branch, events, want) in cases {
        let got = build_contents_default("after", branch, "", &events, false, None)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(want, got, "{name}: contents");
    }
}

/// Text sent in the same message as the answer to another agent's call arrives as context with
/// it, whether the agent gets its history or only the current turn.
#[test]
fn test_build_contents_user_text_with_response_to_other_agent_call() {
    let answer = Content::from_parts(
        vec![Part::from_text("note"), response_part("b1")],
        ROLE_USER,
    );
    let events = vec![
        event("user", start()),
        event("b", call("b1")),
        event("user", answer),
    ];
    let called_b = context(&[CALLED_B]);
    let answered = context(&["[user] said: note", ANSWERED]);

    let history = build_contents_default("after", "", "", &events, false, None)
        .unwrap_or_else(|e| panic!("history: {e}"));
    assert_eq!(
        vec![start(), called_b.clone(), answered.clone()],
        history,
        "history: contents"
    );
    let current = build_contents_current_turn_context_only("after", "", "", &events, false, None)
        .unwrap_or_else(|e| panic!("current turn only: {e}"));
    assert_eq!(
        vec![called_b, answered],
        current,
        "current turn only: contents"
    );
}

/// When only a suffix of the session is built, as the compaction token estimate does, a
/// response in the suffix pairs with the call before it the same way as when the whole session
/// is built.
#[test]
fn test_build_contents_response_in_suffix_to_other_agent_call_before_it() {
    let ev =
        |author: &str, branch: &str, content: Content| on_branch(branch, event(author, content));
    let start_ev = ev("user", "", start());
    let d = ev("d", "", Content::from_text("done", ROLE_MODEL));
    let done = context(&["[d] said: done"]);
    // (name, branch, events, from, want)
    type SuffixCase = (&'static str, &'static str, Vec<Event>, usize, Vec<Content>);
    let cases: Vec<SuffixCase> = vec![
        (
            "call visible to the agent",
            "",
            vec![
                start_ev.clone(),
                ev("b", "", call("b1")),
                ev("user", "", response("b1")),
                d.clone(),
            ],
            2,
            vec![context(&[ANSWERED]), done.clone()],
        ),
        (
            // As when the whole session is built, a call the agent does not see leaves its
            // response unmatched, and it is dropped.
            "call outside the agent's branch",
            "root.after",
            vec![
                start_ev.clone(),
                ev("b", "root.wf", call("b1")),
                ev("user", "", response("b1")),
                d.clone(),
            ],
            2,
            vec![done.clone()],
        ),
        (
            // The whole build skips an event whose content has no role, so its call does not
            // pair with the response there either.
            "call event without a role",
            "",
            vec![
                start_ev.clone(),
                ev("b", "", call_with_role("", "b1")),
                ev("user", "", response("b1")),
                d.clone(),
            ],
            2,
            vec![done.clone()],
        ),
        (
            // Only calls before the suffix are recorded: b's later call with the same id must
            // not turn the agent's own response into context.
            "id reused by a call after the suffix starts",
            "",
            vec![
                start_ev.clone(),
                ev("after", "", call("c0")),
                ev("after", "", response("c0")),
                ev("b", "", call("c0")),
            ],
            2,
            vec![context(&[CALLED_B])],
        ),
    ];
    for (name, branch, events, from, want) in cases {
        let got = build_contents_default_with_call_source(
            "after",
            branch,
            "",
            &events[from..],
            &events,
            false,
            None,
        )
        .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(want, got, "{name}: contents");
    }
}
