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

//! Port of Go runner/agent_node_test.go.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use gadk_genai::{Content, FunctionCall, FunctionResponse, Part, ROLE_USER};
use gadk_session::{Event, Session, SessionError, State};
use serde_json::{Map, Value, json};

use crate::runner::agent_node::{answered_interrupts, resume_user_content};

/// Go stubSession: serves Events only.
struct StubSession {
    events: Vec<Event>,
    state: NoState,
}

struct NoState;

impl State for NoState {
    fn get(&self, _key: &str) -> Result<Value, SessionError> {
        Err(SessionError::StateKeyNotExist)
    }
    fn set(&self, _key: &str, _value: Value) -> Result<(), SessionError> {
        Ok(())
    }
    fn all(&self) -> Vec<(String, Value)> {
        Vec::new()
    }
}

impl Session for StubSession {
    fn id(&self) -> &str {
        ""
    }
    fn app_name(&self) -> &str {
        ""
    }
    fn user_id(&self) -> &str {
        ""
    }
    fn state(&self) -> &dyn State {
        &self.state
    }
    fn events(&self) -> Vec<Event> {
        self.events.clone()
    }
    fn last_update_time(&self) -> DateTime<Utc> {
        gadk_session::zero_time()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn session_of(events: Vec<Event>) -> StubSession {
    StubSession {
        events,
        state: NoState,
    }
}

fn call_parts(ids: &[&str]) -> Vec<Part> {
    ids.iter()
        .map(|id| Part {
            function_call: Some(FunctionCall {
                id: (*id).into(),
                name: "ask".into(),
                ..Default::default()
            }),
            ..Default::default()
        })
        .collect()
}

fn response_part(id: &str) -> Part {
    let mut resp = Map::new();
    resp.insert("payload".into(), json!("approve"));
    Part {
        function_response: Some(FunctionResponse {
            id: id.into(),
            name: "ask".into(),
            response: resp,
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn user_turn(parts: Vec<Part>) -> Content {
    Content {
        role: ROLE_USER.into(),
        parts,
    }
}

/// A model turn that raised long-running tool calls.
fn interrupt_event(ids: &[&str]) -> Event {
    let mut ev = Event {
        long_running_tool_ids: ids.iter().map(|s| (*s).to_string()).collect(),
        ..Default::default()
    };
    ev.response.content = Some(Content {
        role: "model".into(),
        parts: call_parts(ids),
    });
    ev
}

/// A past user turn that already answered `ids`.
fn answer_event(ids: &[&str]) -> Event {
    let mut ev = Event::default();
    ev.response.content = Some(user_turn(ids.iter().map(|id| response_part(id)).collect()));
    ev
}

fn set(ids: &[&str]) -> BTreeSet<String> {
    ids.iter().map(|s| (*s).to_string()).collect()
}

type AnsweredCase = (
    &'static str,
    Option<StubSession>,
    Option<Content>,
    Option<BTreeSet<String>>,
);

#[test]
fn test_answered_interrupts() {
    let cases: Vec<AnsweredCase> = vec![
        (
            "nil session",
            None,
            Some(user_turn(vec![response_part("call-1")])),
            None,
        ),
        (
            "nil user content",
            Some(session_of(vec![interrupt_event(&["call-1"])])),
            None,
            None,
        ),
        (
            "answers the open interrupt",
            Some(session_of(vec![interrupt_event(&["call-1"])])),
            Some(user_turn(vec![response_part("call-1")])),
            Some(set(&["call-1"])),
        ),
        (
            "fresh text turn after a completed resume",
            Some(session_of(vec![
                interrupt_event(&["call-1"]),
                answer_event(&["call-1"]),
            ])),
            Some(user_turn(vec![Part::from_text(
                "and now do something else",
            )])),
            None,
        ),
        (
            "response to a call that was never long-running",
            Some(session_of(vec![interrupt_event(&["call-1"])])),
            Some(user_turn(vec![response_part("other-call")])),
            Some(set(&[])),
        ),
        (
            "no long-running call in history at all",
            Some(session_of(vec![answer_event(&["call-1"])])),
            Some(user_turn(vec![response_part("call-1")])),
            Some(set(&[])),
        ),
        (
            "empty response ID",
            Some(session_of(vec![interrupt_event(&["call-1"])])),
            Some(user_turn(vec![response_part("")])),
            Some(set(&[])),
        ),
        (
            "already-answered ID answered again",
            Some(session_of(vec![
                interrupt_event(&["call-1"]),
                answer_event(&["call-1"]),
            ])),
            Some(user_turn(vec![response_part("call-1")])),
            Some(set(&["call-1"])),
        ),
        (
            "duplicate IDs collapse",
            Some(session_of(vec![interrupt_event(&["call-1"])])),
            Some(user_turn(vec![
                response_part("call-1"),
                response_part("call-1"),
            ])),
            Some(set(&["call-1"])),
        ),
        (
            "only the long-running subset is reported",
            Some(session_of(vec![
                interrupt_event(&["call-1"]),
                interrupt_event(&["call-2"]),
            ])),
            Some(user_turn(vec![
                response_part("call-1"),
                response_part("call-3"),
            ])),
            Some(set(&["call-1"])),
        ),
        (
            "parallel interrupts answered together",
            Some(session_of(vec![interrupt_event(&["call-1", "call-2"])])),
            Some(user_turn(vec![
                response_part("call-1"),
                response_part("call-2"),
            ])),
            Some(set(&["call-1", "call-2"])),
        ),
    ];
    for (name, sess, content, want) in cases {
        let got = answered_interrupts(sess.as_ref().map(|s| s as &dyn Session), content.as_ref());
        assert_eq!(got, want, "{name}");
    }
}

type ResumeCase = (
    &'static str,
    Option<Content>,
    BTreeSet<String>,
    Option<Content>,
);

#[test]
fn test_resume_user_content() {
    let cases: Vec<ResumeCase> = vec![
        // The original question is still on the turn; forwarding it would re-run from START.
        (
            "drops text parts",
            Some(user_turn(vec![
                Part::from_text("ask the subagent"),
                response_part("call-1"),
            ])),
            set(&["call-1"]),
            Some(user_turn(vec![response_part("call-1")])),
        ),
        (
            "drops FunctionResponses outside resolved",
            Some(user_turn(vec![
                response_part("call-1"),
                response_part("call-2"),
            ])),
            set(&["call-1"]),
            Some(user_turn(vec![response_part("call-1")])),
        ),
        (
            "drops FunctionCall parts",
            Some(user_turn(vec![
                call_parts(&["call-1"]).remove(0),
                response_part("call-1"),
            ])),
            set(&["call-1"]),
            Some(user_turn(vec![response_part("call-1")])),
        ),
        (
            "keeps every resolved response",
            Some(user_turn(vec![
                response_part("call-1"),
                response_part("call-2"),
            ])),
            set(&["call-1", "call-2"]),
            Some(user_turn(vec![
                response_part("call-1"),
                response_part("call-2"),
            ])),
        ),
        (
            "preserves role",
            Some(Content {
                role: "custom".into(),
                parts: vec![response_part("call-1")],
            }),
            set(&["call-1"]),
            Some(Content {
                role: "custom".into(),
                parts: vec![response_part("call-1")],
            }),
        ),
        ("nil user content", None, set(&["call-1"]), None),
        // An empty resolved set filters everything out rather than waving the turn through.
        (
            "empty resolved forwards nothing",
            Some(user_turn(vec![response_part("call-1")])),
            set(&[]),
            None,
        ),
        (
            "nothing matches",
            Some(user_turn(vec![
                Part::from_text("hello"),
                response_part("call-2"),
            ])),
            set(&["call-1"]),
            None,
        ),
        // Part::default() (no function response) stands in for Go's nil part.
        (
            "skips nil parts",
            Some(user_turn(vec![Part::default(), response_part("call-1")])),
            set(&["call-1"]),
            Some(user_turn(vec![response_part("call-1")])),
        ),
    ];
    for (name, content, resolved, want) in cases {
        assert_eq!(
            resume_user_content(content.as_ref(), &resolved),
            want,
            "{name}"
        );
    }
}
