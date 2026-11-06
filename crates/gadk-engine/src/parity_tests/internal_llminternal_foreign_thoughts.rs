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

//! Port of ADK-Go `internal/llminternal/foreign_thoughts_test.go`.

use chrono::{DateTime, TimeZone, Utc};
use gadk_genai::{Blob, Content, Part, ROLE_MODEL, ROLE_USER};
use gadk_session::{Event, EventActions};
use pretty_assertions::assert_eq;
use serde_json::json;

use super::internal_llminternal_contents_processor::{
    ctx_builder, ev, llm_agent, run_contents, text, with_continuation,
};
use crate::llm::{IncludeContents, LlmAgentConfig};

/// Go compactionAt (contents_processor_compaction_test.go:38–42): epoch 2026-01-01 + n seconds.
/// That Go file is a 1c (compaction) target, so the helper is defined here.
fn at(n: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap() + chrono::Duration::seconds(n)
}

/// Go compactionTextEvent: role "user" for author "user", else "model".
fn text_event(author: &str, ts: i64, t: &str) -> Event {
    let role = if author == "user" {
        ROLE_USER
    } else {
        ROLE_MODEL
    };
    Event {
        timestamp: at(ts),
        ..ev(author, text(t, role))
    }
}

fn thought(t: &str) -> Part {
    Part {
        text: t.into(),
        thought: true,
        ..Default::default()
    }
}

/// Port of Go `foreignThoughtRequestContents`: runs the processor and checks that the
/// session events were not mutated.
async fn foreign_thought_request_contents(
    events: &[Event],
    ctx: &gadk_agent::InvocationContext,
) -> Vec<Content> {
    let before = serde_json::to_string(events).expect("could not snapshot session events");
    let got = run_contents(ctx)
        .await
        .expect("contents processing yielded an unexpected error");
    let after =
        serde_json::to_string(events).expect("could not snapshot session events after processing");
    assert_eq!(
        before, after,
        "contents processing mutated the original session events"
    );
    got
}

#[tokio::test]
async fn test_contents_request_processor_foreign_thoughts() {
    let public_part = Part {
        inline_data: Some(Blob {
            mime_type: "image/png".into(),
            data: vec![1u8, 2, 3].into(),
            ..Default::default()
        }),
        thought_signature: vec![4u8, 5].into(),
        ..Default::default()
    };
    let cases: Vec<(&str, &str, Vec<Part>, Vec<Content>)> = vec![
        (
            "thought-only foreign event is omitted",
            "otherAgent",
            vec![thought("private reasoning")],
            vec![],
        ),
        (
            "mixed foreign event retains public parts",
            "otherAgent",
            vec![
                thought("private reasoning"),
                Part::from_text("public answer"),
                public_part.clone(),
            ],
            vec![Content::from_parts(
                vec![
                    Part::from_text("For context:"),
                    Part::from_text("[otherAgent] said: public answer"),
                    public_part.clone(),
                ],
                ROLE_USER,
            )],
        ),
        (
            "own-agent thoughts are retained",
            "testAgent",
            vec![Part {
                thought_signature: vec![6u8, 7].into(),
                ..thought("own reasoning")
            }],
            vec![Content::from_parts(
                vec![Part {
                    thought_signature: vec![6u8, 7].into(),
                    ..thought("own reasoning")
                }],
                ROLE_MODEL,
            )],
        ),
    ];
    for (name, author, parts, tail) in cases {
        let user = text_event("user", 1, "request");
        let mut state_delta = serde_json::Map::new();
        state_delta.insert("state".into(), json!("unchanged"));
        let event = Event {
            id: "response-event".into(),
            invocation_id: "response-invocation".into(),
            timestamp: at(2),
            actions: EventActions {
                state_delta: Some(state_delta),
                ..Default::default()
            },
            ..ev(author, Content::from_parts(parts, ROLE_MODEL))
        };
        let events = vec![user.clone(), event];
        // Go compactionInvocationCtx(t, "testAgent", events, false): no compaction configured.
        let agent = llm_agent(LlmAgentConfig {
            name: "testAgent".into(),
            ..Default::default()
        });
        let ctx = ctx_builder(&agent, events.clone()).build_invocation();
        let got = foreign_thought_request_contents(&events, &ctx).await;
        let mut want = vec![user.response.content.clone().unwrap()];
        want.extend(tail);
        assert_eq!(
            with_continuation(want),
            got,
            "{name}: request contents did not preserve the expected public and own-agent parts"
        );
    }
}

// parity: skipped TestContentsRequestProcessor_ForeignThoughtCompactionSummary: compaction summaries are deferred to sub-project 1c
// parity: skipped TestContentsRequestProcessor_ForeignThoughtCompactionHole: compaction excluded-event holes are deferred to sub-project 1c

// parity: adapted: the Go case "usable foreign summary still starts a turn" needs a compaction summary event (deferred to sub-project 1c) and is omitted
#[tokio::test]
async fn test_contents_request_processor_foreign_thought_current_turn() {
    struct Case {
        name: &'static str,
        author: &'static str,
        parts: Vec<Part>,
        successive_thought: bool,
        keep_current_user: bool,
        want_tail: Vec<Content>,
    }
    let empty_thought = Part {
        thought: true,
        ..Default::default()
    };
    let cases = vec![
        Case {
            name: "foreign text thought does not start a turn",
            author: "otherAgent",
            parts: vec![thought("private reasoning")],
            successive_thought: false,
            keep_current_user: true,
            want_tail: vec![],
        },
        Case {
            name: "foreign empty thought does not start a turn",
            author: "otherAgent",
            parts: vec![empty_thought.clone()],
            successive_thought: false,
            keep_current_user: true,
            want_tail: vec![],
        },
        Case {
            name: "multiple foreign thought parts do not start a turn",
            author: "otherAgent",
            parts: vec![thought("private reasoning"), empty_thought.clone()],
            successive_thought: false,
            keep_current_user: true,
            want_tail: vec![],
        },
        Case {
            name: "successive foreign thoughts do not start a turn",
            author: "otherAgent",
            parts: vec![thought("private reasoning")],
            successive_thought: true,
            keep_current_user: true,
            want_tail: vec![],
        },
        Case {
            name: "mixed foreign response still starts a turn",
            author: "otherAgent",
            parts: vec![
                thought("private reasoning"),
                Part::from_text("public answer"),
            ],
            successive_thought: false,
            keep_current_user: false,
            want_tail: vec![Content::from_parts(
                vec![
                    Part::from_text("For context:"),
                    Part::from_text("[otherAgent] said: public answer"),
                ],
                ROLE_USER,
            )],
        },
        Case {
            name: "own thoughts remain in the current user turn",
            author: "testAgent",
            parts: vec![thought("own reasoning")],
            successive_thought: false,
            keep_current_user: true,
            want_tail: vec![Content::from_parts(
                vec![thought("own reasoning")],
                ROLE_MODEL,
            )],
        },
        Case {
            name: "user thought content starts its own turn",
            author: "user",
            parts: vec![thought("user input")],
            successive_thought: false,
            keep_current_user: false,
            want_tail: vec![Content::from_parts(vec![thought("user input")], ROLE_USER)],
        },
    ];
    for c in cases {
        let current = text_event("user", 3, "current request");
        let role = if c.author == "user" {
            ROLE_USER
        } else {
            ROLE_MODEL
        };
        let event = Event {
            timestamp: at(4),
            ..ev(c.author, Content::from_parts(c.parts, role))
        };
        let mut events = vec![
            text_event("user", 1, "older request"),
            text_event("testAgent", 2, "older answer"),
            current.clone(),
            event,
        ];
        if c.successive_thought {
            events.push(Event {
                timestamp: at(5),
                ..ev(
                    "anotherAgent",
                    Content::from_parts(vec![empty_thought.clone()], ROLE_MODEL),
                )
            });
        }
        let agent = llm_agent(LlmAgentConfig {
            name: "testAgent".into(),
            include_contents: IncludeContents::None,
            ..Default::default()
        });
        let ctx = ctx_builder(&agent, events.clone()).build_invocation();
        let got = foreign_thought_request_contents(&events, &ctx).await;
        let mut want = Vec::new();
        if c.keep_current_user {
            want.push(current.response.content.clone().unwrap());
        }
        want.extend(c.want_tail);
        assert_eq!(
            with_continuation(want),
            got,
            "{}: request contents selected the wrong current-turn boundary",
            c.name
        );
    }
}
