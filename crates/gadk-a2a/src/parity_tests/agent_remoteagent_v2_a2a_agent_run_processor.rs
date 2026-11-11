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

//! Ports of ADK-Go `agent/remoteagent/v2/a2a_agent_run_processor_test.go`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::sync::Arc;

use gadk_agent::{AgentConfig, InvocationContext, new_agent};
use gadk_genai::{Content, Part, ROLE_MODEL};
use gadk_platform::Platform;
use gadk_session::{Event, EventActions, zero_time};
use serde_json::{Map, Value};

use crate::convert::{to_a2a_meta_key, to_session_event};
use crate::remoteagent::A2AConfig;
use crate::remoteagent::run_processor::RunProcessor;

const TASK_ID: &str = "task-1";
const CONTEXT_ID: &str = "context-1";

/// Go `a2a.NewArtifactUpdateEvent(task, aid, parts...)` with the given flags.
fn artifact_update(
    aid: &str,
    append: bool,
    last_chunk: bool,
    part: a2a::Part,
) -> a2a::StreamResponse {
    a2a::StreamResponse::ArtifactUpdate(a2a::TaskArtifactUpdateEvent {
        task_id: TASK_ID.into(),
        context_id: CONTEXT_ID.into(),
        artifact: a2a::Artifact {
            artifact_id: aid.into(),
            name: None,
            description: None,
            parts: vec![part],
            metadata: None,
            extensions: None,
        },
        append: Some(append),
        last_chunk: Some(last_chunk),
        metadata: None,
    })
}

fn text_update(aid: &str, append: bool, last_chunk: bool, text: &str) -> a2a::StreamResponse {
    artifact_update(aid, append, last_chunk, a2a::Part::text(text))
}

/// Go `a2a.NewArtifactUpdateEvent(task, aid, part)`: `Append: true`, `LastChunk: false`.
fn new_update(aid: &str, text: &str) -> a2a::StreamResponse {
    text_update(aid, true, false, text)
}

/// Go `withADKPartial`.
fn with_adk_partial(mut event: a2a::StreamResponse, partial: bool) -> a2a::StreamResponse {
    if let a2a::StreamResponse::ArtifactUpdate(update) = &mut event {
        update.metadata = Some(HashMap::from([(
            to_a2a_meta_key("partial"),
            Value::Bool(partial),
        )]));
    }
    event
}

/// Go `newFinalStatusUpdate(task, TaskStateCompleted)`.
fn completed() -> a2a::StreamResponse {
    a2a::StreamResponse::StatusUpdate(a2a::TaskStatusUpdateEvent {
        task_id: TASK_ID.into(),
        context_id: CONTEXT_ID.into(),
        status: a2a::TaskStatus {
            state: a2a::TaskState::Completed,
            message: None,
            timestamp: None,
        },
        metadata: None,
    })
}

fn thought_part(text: &str) -> a2a::Part {
    let mut p = a2a::Part::text(text);
    p.metadata = Some(HashMap::from([(
        to_a2a_meta_key("thought"),
        Value::Bool(true),
    )]));
    p
}

fn event(partial: bool, turn_complete: bool, parts: Vec<Part>) -> Event {
    let mut e = Event::new(&Platform::default(), "");
    e.response.partial = partial;
    e.response.turn_complete = turn_complete;
    if !parts.is_empty() {
        e.response.content = Some(Content::from_parts(parts, ROLE_MODEL));
    }
    e
}

/// Go `newPartialEvent`.
fn partial(text: &str) -> Event {
    event(true, false, vec![Part::from_text(text)])
}

/// Go `newEvent`.
fn non_partial(parts: Vec<Part>) -> Event {
    event(false, false, parts)
}

/// Go `newCompletedEvent`.
fn completed_event(parts: Vec<Part>) -> Event {
    event(false, true, parts)
}

fn thought(text: &str) -> Part {
    Part {
        text: text.into(),
        thought: true,
        ..Default::default()
    }
}

/// Go's `cmpopts.IgnoreFields(session.Event{}, "ID", "Timestamp", "InvocationID", "Author",
/// "Branch", "CustomMetadata", "Actions")`.
fn normalize(mut e: Event) -> Event {
    e.id = String::new();
    e.timestamp = zero_time();
    e.invocation_id = String::new();
    e.author = String::new();
    e.branch = String::new();
    e.response.custom_metadata = Map::new();
    e.actions = EventActions::default();
    e
}

#[tokio::test]
async fn test_a2_a_agent_run_processor_aggregate_partial() {
    let (aid1, aid2) = ("aid-1", "aid-2");
    let snapshot = |artifacts: Option<Vec<a2a::Artifact>>, state: a2a::TaskState| {
        a2a::StreamResponse::Task(a2a::Task {
            id: TASK_ID.into(),
            context_id: String::new(),
            status: a2a::TaskStatus {
                state,
                message: None,
                timestamp: None,
            },
            artifacts,
            history: None,
            metadata: None,
        })
    };
    let done_artifact = a2a::Artifact {
        artifact_id: String::new(),
        name: None,
        description: None,
        parts: vec![a2a::Part::text("done")],
        metadata: None,
        extensions: None,
    };
    let cases: Vec<(&str, Vec<a2a::StreamResponse>, Vec<Event>)> = vec![
        (
            "do not aggregate when ADK events",
            vec![
                with_adk_partial(new_update(aid1, "Hel"), true),
                with_adk_partial(new_update(aid1, "lo"), true),
                with_adk_partial(new_update(aid1, "Hello"), false),
                completed(),
            ],
            vec![
                partial("Hel"),
                partial("lo"),
                non_partial(vec![Part::from_text("Hello")]),
                completed_event(vec![]),
            ],
        ),
        (
            "aggregation reset by final snapshot",
            vec![
                new_update("a1", "ignore me"),
                snapshot(Some(vec![done_artifact]), a2a::TaskState::Completed),
            ],
            vec![
                partial("ignore me"),
                completed_event(vec![Part::from_text("done")]),
            ],
        ),
        (
            "aggregation reset by non-final snapshot",
            vec![
                new_update("a1", "foo"),
                snapshot(None, a2a::TaskState::Unspecified),
                new_update("a1", "bar"),
                completed(),
            ],
            vec![
                partial("foo"),
                partial("bar"),
                non_partial(vec![Part::from_text("bar")]),
                completed_event(vec![]),
            ],
        ),
        (
            "[append=true, lastChunk=false] emit aggregated on final status",
            vec![
                text_update(aid1, true, false, "Hel"),
                text_update(aid1, true, false, "lo"),
                completed(),
            ],
            vec![
                partial("Hel"),
                partial("lo"),
                non_partial(vec![Part::from_text("Hello")]),
                completed_event(vec![]),
            ],
        ),
        (
            "[append=true, lastChunk=false] emit multiple aggregated on final status",
            vec![
                text_update(aid1, true, false, "Foo"),
                text_update(aid2, true, false, "Bar"),
                completed(),
            ],
            vec![
                partial("Foo"),
                partial("Bar"),
                non_partial(vec![Part::from_text("Foo")]),
                non_partial(vec![Part::from_text("Bar")]),
                completed_event(vec![]),
            ],
        ),
        (
            "last updated aggregation is emitted last",
            vec![
                text_update(aid1, true, false, "Foo"),
                text_update(aid2, true, false, "Bar"),
                text_update(aid1, true, false, "Baz"),
                completed(),
            ],
            vec![
                partial("Foo"),
                partial("Bar"),
                partial("Baz"),
                non_partial(vec![Part::from_text("Bar")]),
                non_partial(vec![Part::from_text("FooBaz")]),
                completed_event(vec![]),
            ],
        ),
        (
            "[append=true, lastChunk=true] results in partial followed by non-partial",
            vec![
                text_update(aid1, true, false, "Hel"),
                text_update(aid1, true, true, "lo"),
                text_update(aid2, true, false, "bar"),
                completed(),
            ],
            vec![
                partial("Hel"),
                partial("lo"),
                non_partial(vec![Part::from_text("Hello")]),
                partial("bar"),
                non_partial(vec![Part::from_text("bar")]),
                completed_event(vec![]),
            ],
        ),
        (
            "[append=false, lastChunk=true] results in non-partial",
            vec![
                text_update(aid1, true, false, "Hel"),
                text_update(aid1, false, true, "Hello"),
                text_update(aid2, true, false, "bar"),
                completed(),
            ],
            vec![
                partial("Hel"),
                non_partial(vec![Part::from_text("Hello")]),
                partial("bar"),
                non_partial(vec![Part::from_text("bar")]),
                completed_event(vec![]),
            ],
        ),
        (
            "[append=false, lastChunk=true] as first event non-partial",
            vec![
                text_update(aid1, false, true, "Hello"),
                text_update(aid2, true, false, "bar"),
                completed(),
            ],
            vec![
                non_partial(vec![Part::from_text("Hello")]),
                partial("bar"),
                non_partial(vec![Part::from_text("bar")]),
                completed_event(vec![]),
            ],
        ),
        (
            "[append=false, lastChunk=false] resets aggregation",
            vec![
                text_update(aid1, true, false, "Foo"),
                text_update(aid1, false, false, "Bar"),
                completed(),
            ],
            vec![
                partial("Foo"),
                partial("Bar"),
                non_partial(vec![Part::from_text("Bar")]),
                completed_event(vec![]),
            ],
        ),
        (
            "thoughts aggregation",
            vec![
                artifact_update("a1", true, false, thought_part("thinking...")),
                new_update("a1", "done"),
                completed(),
            ],
            vec![
                event(true, false, vec![thought("thinking...")]),
                partial("done"),
                non_partial(vec![thought("thinking..."), Part::from_text("done")]),
                completed_event(vec![]),
            ],
        ),
        (
            "interleaved thought and text",
            vec![
                artifact_update("a1", true, false, thought_part("thinking1")),
                new_update("a1", "text1"),
                artifact_update("a1", true, false, thought_part("thinking2")),
                new_update("a1", "text2"),
                completed(),
            ],
            vec![
                event(true, false, vec![thought("thinking1")]),
                partial("text1"),
                event(true, false, vec![thought("thinking2")]),
                partial("text2"),
                non_partial(vec![
                    thought("thinking1"),
                    Part::from_text("text1"),
                    thought("thinking2"),
                    Part::from_text("text2"),
                ]),
                completed_event(vec![]),
            ],
        ),
    ];

    for (name, events, want) in cases {
        let agent = new_agent(AgentConfig::default()).unwrap();
        let ctx = InvocationContext::builder().agent(agent).build_invocation();
        let request = a2a::SendMessageRequest {
            message: a2a::Message::new(a2a::Role::User, vec![]),
            configuration: None,
            metadata: None,
            tenant: None,
        };
        let mut p = RunProcessor::new(Arc::new(A2AConfig::default()), request);
        let mut got = Vec::new();
        for ev in &events {
            let adk_event = to_session_event(&ctx, ev).await.unwrap();
            let Some(adk_event) = adk_event else {
                // A Task snapshot resets the aggregation even without an event.
                if matches!(ev, a2a::StreamResponse::Task(_)) {
                    // Go passes a nil event; the returned list is discarded either way.
                    let _ =
                        p.aggregate_partial(&ctx, Some(ev), Event::new(&Platform::default(), ""));
                }
                continue;
            };
            got.extend(
                p.aggregate_partial(&ctx, Some(ev), adk_event)
                    .into_iter()
                    .map(|e| normalize(e.event)),
            );
        }
        let want: Vec<Event> = want.into_iter().map(normalize).collect();
        pretty_assertions::assert_eq!(got, want, "{name}");
    }
}
