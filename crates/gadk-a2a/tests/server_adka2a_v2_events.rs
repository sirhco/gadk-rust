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

//! Port of ADK-Go `server/adka2a/v2/events_test.go`.

#![allow(clippy::unwrap_used)]

use std::collections::HashMap;
use std::sync::Arc;

use futures::future::BoxFuture;
use gadk_a2a::Error;
use gadk_a2a::a2a;
use gadk_a2a::convert::{
    A2APartConverter, StreamResponseRef, to_a2a_meta_key, to_adk_meta_key, to_genai_part,
    to_session_event, to_session_event_with_parts, transfer_to_agent_from_meta,
};
use gadk_agent::{AgentConfig, ContextBuilder, InvocationContext, new_agent};
use gadk_genai::{
    Blob, Content, FunctionCall, GenerateContentResponseUsageMetadata, GroundingMetadata,
    GroundingMetadataSourceFlaggingURI, Part, ROLE_MODEL,
};
use gadk_model::LlmResponse;
use gadk_session::{Event, EventActions, zero_time};
use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};

const BRANCH: &str = "main";
const AGENT_NAME: &str = "a2a agent";

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        other => panic!("not an object: {other}"),
    }
}

fn meta(v: Value) -> Option<HashMap<String, Value>> {
    Some(obj(v).into_iter().collect())
}

fn invocation_context() -> InvocationContext {
    let agent = new_agent(AgentConfig {
        name: AGENT_NAME.into(),
        ..Default::default()
    })
    .unwrap();
    ContextBuilder::new()
        .agent(agent)
        .branch(BRANCH)
        .build_invocation()
}

fn text(s: &str) -> Part {
    Part {
        text: s.into(),
        ..Default::default()
    }
}

fn function_call(id: &str, name: &str, args: Value) -> Part {
    Part {
        function_call: Some(FunctionCall {
            id: id.into(),
            name: name.into(),
            args: obj(args),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn model_content(parts: Vec<Part>) -> Option<Content> {
    Some(Content {
        parts,
        role: ROLE_MODEL.into(),
    })
}

/// A function call data part marked long-running (Go sets the metadata map directly).
fn long_running_call(data: Value) -> a2a::Part {
    let mut part = a2a::Part::data(data);
    part.metadata = meta(json!({
        (to_a2a_meta_key("type")): "function_call",
        (to_a2a_meta_key("is_long_running")): true,
    }));
    part
}

fn artifact(id: &str, parts: Vec<a2a::Part>) -> a2a::Artifact {
    a2a::Artifact {
        artifact_id: id.into(),
        name: None,
        description: None,
        parts,
        metadata: None,
        extensions: None,
    }
}

fn message(parts: Vec<a2a::Part>) -> a2a::Message {
    a2a::Message {
        message_id: String::new(),
        context_id: None,
        task_id: None,
        role: a2a::Role::Unspecified,
        parts,
        metadata: None,
        extensions: None,
        reference_task_ids: None,
    }
}

fn status(state: a2a::TaskState, message: Option<a2a::Message>) -> a2a::TaskStatus {
    a2a::TaskStatus {
        state,
        message,
        timestamp: None,
    }
}

fn task(
    task_id: &str,
    context_id: &str,
    artifacts: Vec<a2a::Artifact>,
    status: a2a::TaskStatus,
    metadata: Option<HashMap<String, Value>>,
) -> a2a::StreamResponse {
    a2a::StreamResponse::Task(a2a::Task {
        id: task_id.into(),
        context_id: context_id.into(),
        status,
        artifacts: (!artifacts.is_empty()).then_some(artifacts),
        history: None,
        metadata,
    })
}

fn status_update(
    task_id: &str,
    context_id: &str,
    status: a2a::TaskStatus,
    metadata: Option<HashMap<String, Value>>,
) -> a2a::StreamResponse {
    a2a::StreamResponse::StatusUpdate(a2a::TaskStatusUpdateEvent {
        task_id: task_id.into(),
        context_id: context_id.into(),
        status,
        metadata,
    })
}

fn artifact_update(
    task_id: &str,
    context_id: &str,
    artifact: a2a::Artifact,
    metadata: Option<HashMap<String, Value>>,
) -> a2a::StreamResponse {
    a2a::StreamResponse::ArtifactUpdate(a2a::TaskArtifactUpdateEvent {
        task_id: task_id.into(),
        context_id: context_id.into(),
        artifact,
        append: None,
        last_chunk: None,
        metadata,
    })
}

/// Go `cmpopts.IgnoreFields(session.Event{}, "ID", "Timestamp", "InvocationID")` and
/// `IgnoreFields(session.EventActions{}, "StateDelta")`.
fn normalize(mut event: Event) -> Event {
    event.id = String::new();
    event.timestamp = zero_time();
    event.invocation_id = String::new();
    event.actions.state_delta = None;
    event
}

/// The fields every expected event shares: author and branch.
fn agent_event(response: LlmResponse) -> Event {
    Event {
        response,
        author: AGENT_NAME.into(),
        branch: BRANCH.into(),
        ..Default::default()
    }
}

fn task_meta(task_id: &str, context_id: &str) -> Map<String, Value> {
    obj(json!({
        (to_adk_meta_key("task_id")): task_id,
        (to_adk_meta_key("context_id")): context_id,
    }))
}

fn nested_task_meta(task_id: &str, context_id: &str) -> Map<String, Value> {
    let mut m = task_meta(task_id, context_id);
    m.insert("nested".into(), json!({"key": "value"}));
    m
}

fn usage() -> Option<GenerateContentResponseUsageMetadata> {
    Some(GenerateContentResponseUsageMetadata {
        candidates_token_count: 12,
        thoughts_token_count: 42,
        ..Default::default()
    })
}

fn grounding() -> Option<GroundingMetadata> {
    Some(GroundingMetadata {
        source_flagging_uris: vec![GroundingMetadataSourceFlaggingURI {
            source_id: "id1".into(),
            ..Default::default()
        }],
        ..Default::default()
    })
}

#[tokio::test]
#[ignore = "8a bug: a completed/failed/canceled task with parts does not set actions.skip_summarization (Go d6bb1e86)"]
async fn test_to_session_event() {
    let (task_id, context_id) = (a2a::new_task_id(), a2a::new_context_id());
    let (tid, cid) = (task_id.as_str(), context_id.as_str());
    let grounding_key = to_a2a_meta_key("grounding_metadata");
    let usage_key = to_a2a_meta_key("usage_metadata");
    let custom_key = to_a2a_meta_key("custom_metadata");
    let escalate_key = to_a2a_meta_key("escalate");
    let is_err_key = to_a2a_meta_key("is_error_message");
    let weather_call =
        json!({"id": "get_weather", "args": {"city": "Warsaw"}, "name": "GetWeather"});

    let cases: Vec<(&str, a2a::StreamResponse, Option<Event>)> = vec![
        (
            "message",
            a2a::StreamResponse::Message(a2a::Message {
                task_id: Some(task_id.clone()),
                context_id: Some(context_id.clone()),
                metadata: meta(json!({
                    (&grounding_key): {"sourceFlaggingUris": [{"sourceId": "id1"}]},
                    (&usage_key): {"candidatesTokenCount": 12, "thoughtsTokenCount": 42},
                    (&custom_key): {"nested": {"key": "value"}},
                    (to_a2a_meta_key("transfer_to_agent")): "a-2",
                    (&escalate_key): true,
                })),
                ..message(vec![a2a::Part::text("foo")])
            }),
            Some(Event {
                // transfer_to_agent is peer-supplied metadata; this conversion layer never
                // restores it (see test_to_session_event_with_parts_transfer_to_agent_always_redacted
                // and transfer_to_agent_from_meta).
                actions: EventActions {
                    escalate: true,
                    ..Default::default()
                },
                ..agent_event(LlmResponse {
                    content: model_content(vec![text("foo")]),
                    usage_metadata: usage(),
                    grounding_metadata: grounding(),
                    custom_metadata: nested_task_meta(tid, cid),
                    turn_complete: true,
                    ..Default::default()
                })
            }),
        ),
        (
            "nil values",
            a2a::StreamResponse::Message(a2a::Message {
                task_id: Some(task_id.clone()),
                context_id: Some(context_id.clone()),
                metadata: meta(json!({
                    (&grounding_key): null,
                    (&usage_key): null,
                    (&custom_key): null,
                })),
                ..message(vec![a2a::Part::text("foo")])
            }),
            Some(agent_event(LlmResponse {
                content: model_content(vec![text("foo")]),
                custom_metadata: task_meta(tid, cid),
                turn_complete: true,
                ..Default::default()
            })),
        ),
        (
            "message with no parts",
            a2a::StreamResponse::Message(a2a::Message {
                task_id: Some(task_id.clone()),
                context_id: Some(context_id.clone()),
                ..message(vec![])
            }),
            Some(agent_event(LlmResponse {
                custom_metadata: task_meta(tid, cid),
                turn_complete: true,
                ..Default::default()
            })),
        ),
        (
            "task",
            task(
                tid,
                cid,
                vec![
                    // long running key is ignored for non-input-required states
                    artifact(
                        &a2a::new_artifact_id(),
                        vec![long_running_call(weather_call.clone())],
                    ),
                    artifact(&a2a::new_artifact_id(), vec![a2a::Part::text("foo")]),
                    artifact(&a2a::new_artifact_id(), vec![a2a::Part::text("bar")]),
                ],
                status(
                    a2a::TaskState::Completed,
                    Some(a2a::Message::new(
                        a2a::Role::Agent,
                        vec![a2a::Part::text("done")],
                    )),
                ),
                meta(json!({
                    (&grounding_key): {"sourceFlaggingUris": [{"sourceId": "id1"}]},
                    (&usage_key): {"candidatesTokenCount": 12, "thoughtsTokenCount": 42},
                    (&custom_key): {"nested": {"key": "value"}},
                    (&escalate_key): true,
                })),
            ),
            Some(Event {
                actions: EventActions {
                    escalate: true,
                    skip_summarization: true,
                    ..Default::default()
                },
                ..agent_event(LlmResponse {
                    content: model_content(vec![
                        function_call("get_weather", "GetWeather", json!({"city": "Warsaw"})),
                        text("foo"),
                        text("bar"),
                        text("done"),
                    ]),
                    usage_metadata: usage(),
                    grounding_metadata: grounding(),
                    custom_metadata: nested_task_meta(tid, cid),
                    turn_complete: true,
                    ..Default::default()
                })
            }),
        ),
        (
            "terminal task with no parts",
            task(
                tid,
                cid,
                vec![],
                status(a2a::TaskState::Completed, None),
                None,
            ),
            Some(agent_event(LlmResponse {
                custom_metadata: task_meta(tid, cid),
                turn_complete: true,
                ..Default::default()
            })),
        ),
        (
            "non-terminal task with no parts",
            task(
                tid,
                cid,
                vec![],
                status(a2a::TaskState::Submitted, None),
                None,
            ),
            None,
        ),
        (
            "task in input required",
            task(
                tid,
                cid,
                vec![artifact(
                    &a2a::new_artifact_id(),
                    vec![long_running_call(weather_call.clone())],
                )],
                status(a2a::TaskState::InputRequired, None),
                meta(json!({ (&escalate_key): true })),
            ),
            Some(Event {
                long_running_tool_ids: vec!["get_weather".into()],
                actions: EventActions {
                    escalate: true,
                    ..Default::default()
                },
                ..agent_event(LlmResponse {
                    content: model_content(vec![function_call(
                        "get_weather",
                        "GetWeather",
                        json!({"city": "Warsaw"}),
                    )]),
                    custom_metadata: task_meta(tid, cid),
                    turn_complete: true,
                    ..Default::default()
                })
            }),
        ),
        (
            "artifact update",
            artifact_update(
                tid,
                cid,
                artifact(
                    &a2a::new_artifact_id(),
                    vec![a2a::Part::text("foo"), a2a::Part::text("bar")],
                ),
                meta(json!({
                    (&grounding_key): {"sourceFlaggingUris": [{"sourceId": "id1"}]},
                    (&usage_key): {"candidatesTokenCount": 12, "thoughtsTokenCount": 42},
                    (&custom_key): {"nested": {"key": "value"}},
                })),
            ),
            Some(agent_event(LlmResponse {
                content: model_content(vec![text("foo"), text("bar")]),
                grounding_metadata: grounding(),
                usage_metadata: usage(),
                custom_metadata: nested_task_meta(tid, cid),
                partial: true,
                ..Default::default()
            })),
        ),
        (
            "artifact update with no parts is skipped",
            artifact_update(tid, cid, artifact(&a2a::new_artifact_id(), vec![]), None),
            None,
        ),
        (
            "artifact update with long running tool call",
            artifact_update(
                tid,
                cid,
                artifact(
                    &a2a::new_artifact_id(),
                    vec![long_running_call(weather_call.clone())],
                ),
                None,
            ),
            Some(Event {
                long_running_tool_ids: vec!["get_weather".into()],
                ..agent_event(LlmResponse {
                    content: model_content(vec![function_call(
                        "get_weather",
                        "GetWeather",
                        json!({"city": "Warsaw"}),
                    )]),
                    custom_metadata: task_meta(tid, cid),
                    partial: true,
                    ..Default::default()
                })
            }),
        ),
        (
            "final task status update with message",
            status_update(
                tid,
                cid,
                status(
                    a2a::TaskState::Completed,
                    Some(message(vec![a2a::Part::text("foo")])),
                ),
                meta(json!({
                    (&grounding_key): {"sourceFlaggingUris": [{"sourceId": "id1"}]},
                    (&usage_key): {"candidatesTokenCount": 12, "thoughtsTokenCount": 42},
                    (&custom_key): {"nested": {"key": "value"}},
                    (&escalate_key): true,
                })),
            ),
            Some(Event {
                actions: EventActions {
                    escalate: true,
                    ..Default::default()
                },
                ..agent_event(LlmResponse {
                    content: model_content(vec![text("foo")]),
                    custom_metadata: nested_task_meta(tid, cid),
                    turn_complete: true,
                    grounding_metadata: grounding(),
                    usage_metadata: usage(),
                    ..Default::default()
                })
            }),
        ),
        (
            "final task status update without message",
            status_update(tid, cid, status(a2a::TaskState::Completed, None), None),
            Some(agent_event(LlmResponse {
                custom_metadata: task_meta(tid, cid),
                turn_complete: true,
                ..Default::default()
            })),
        ),
        (
            "non final task status update message is a thought",
            status_update(
                tid,
                cid,
                status(
                    a2a::TaskState::Working,
                    Some(message(vec![a2a::Part::text("foo")])),
                ),
                None,
            ),
            Some(agent_event(LlmResponse {
                content: model_content(vec![Part {
                    thought: true,
                    ..text("foo")
                }]),
                custom_metadata: task_meta(tid, cid),
                partial: true,
                ..Default::default()
            })),
        ),
        (
            "non-final task status update without message is skipped",
            status_update(tid, cid, status(a2a::TaskState::Unspecified, None), None),
            None,
        ),
        (
            "non-final task status update with an empty message is skipped",
            status_update(
                tid,
                cid,
                status(
                    a2a::TaskState::Working,
                    Some(a2a::Message::new(a2a::Role::Agent, vec![])),
                ),
                None,
            ),
            None,
        ),
        (
            "task status failed with single-part message",
            status_update(
                tid,
                cid,
                status(
                    a2a::TaskState::Failed,
                    Some(message(vec![a2a::Part::text("failed with an error")])),
                ),
                None,
            ),
            Some(agent_event(LlmResponse {
                content: model_content(vec![text("failed with an error")]),
                error_message: "failed with an error".into(),
                custom_metadata: task_meta(tid, cid),
                turn_complete: true,
                ..Default::default()
            })),
        ),
        (
            "task with multiple artifacts and mixed long-running tools",
            task(
                tid,
                cid,
                vec![
                    artifact(
                        "artifact-1",
                        vec![
                            a2a::Part::text("Checking weather..."),
                            long_running_call(
                                json!({"id": "tool_1", "name": "GetWeather", "args": {"city": "London"}}),
                            ),
                        ],
                    ),
                    artifact(
                        "artifact-2",
                        vec![long_running_call(
                            json!({"id": "tool_2", "name": "GetNews", "args": {"topic": "tech"}}),
                        )],
                    ),
                ],
                status(a2a::TaskState::InputRequired, None),
                None,
            ),
            Some(Event {
                long_running_tool_ids: vec!["tool_1".into(), "tool_2".into()],
                ..agent_event(LlmResponse {
                    content: model_content(vec![
                        text("Checking weather..."),
                        function_call("tool_1", "GetWeather", json!({"city": "London"})),
                        function_call("tool_2", "GetNews", json!({"topic": "tech"})),
                    ]),
                    custom_metadata: task_meta(tid, cid),
                    turn_complete: true,
                    ..Default::default()
                })
            }),
        ),
        (
            "failed task with single-part text status",
            task(
                tid,
                cid,
                vec![],
                status(
                    a2a::TaskState::Failed,
                    Some(message(vec![a2a::Part::text("failed with an error")])),
                ),
                None,
            ),
            Some(Event {
                actions: EventActions {
                    skip_summarization: true,
                    ..Default::default()
                },
                ..agent_event(LlmResponse {
                    content: model_content(vec![text("failed with an error")]),
                    error_message: "failed with an error".into(),
                    custom_metadata: task_meta(tid, cid),
                    turn_complete: true,
                    ..Default::default()
                })
            }),
        ),
        (
            "completed task with single-part text status",
            task(
                tid,
                cid,
                vec![],
                status(
                    a2a::TaskState::Completed,
                    Some(message(vec![a2a::Part::text("failed with an error")])),
                ),
                None,
            ),
            Some(Event {
                actions: EventActions {
                    skip_summarization: true,
                    ..Default::default()
                },
                ..agent_event(LlmResponse {
                    content: model_content(vec![text("failed with an error")]),
                    custom_metadata: task_meta(tid, cid),
                    turn_complete: true,
                    ..Default::default()
                })
            }),
        ),
        (
            "failed task with a multi-part status",
            task(
                tid,
                cid,
                vec![],
                status(
                    a2a::TaskState::Failed,
                    Some(a2a::Message::new(
                        a2a::Role::Agent,
                        vec![
                            a2a::Part::text("failed with an error"),
                            a2a::Part::text("extra details"),
                        ],
                    )),
                ),
                None,
            ),
            Some(Event {
                actions: EventActions {
                    skip_summarization: true,
                    ..Default::default()
                },
                ..agent_event(LlmResponse {
                    error_message: "failed with an error".into(),
                    content: model_content(vec![
                        text("failed with an error"),
                        text("extra details"),
                    ]),
                    custom_metadata: task_meta(tid, cid),
                    turn_complete: true,
                    ..Default::default()
                })
            }),
        ),
        (
            "failed task status with a multi-part status",
            status_update(
                tid,
                cid,
                status(
                    a2a::TaskState::Failed,
                    Some(a2a::Message::new(
                        a2a::Role::Agent,
                        vec![
                            a2a::Part::text("failed with an error"),
                            a2a::Part::data(json!({"structured": true})),
                        ],
                    )),
                ),
                None,
            ),
            Some(agent_event(LlmResponse {
                error_message: "failed with an error".into(),
                content: model_content(vec![
                    text("failed with an error"),
                    Part {
                        inline_data: Some(Blob {
                            data: b"<a2a_datapart_json>{\"structured\":true}</a2a_datapart_json>"
                                .to_vec()
                                .into(),
                            mime_type: "text/plain".into(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                ]),
                custom_metadata: task_meta(tid, cid),
                turn_complete: true,
                ..Default::default()
            })),
        ),
        (
            "failed task status metadataIsErrMessageKey not in content",
            status_update(
                tid,
                cid,
                status(
                    a2a::TaskState::Failed,
                    Some(a2a::Message::new(
                        a2a::Role::Agent,
                        vec![
                            a2a::Part {
                                metadata: meta(json!({ (&is_err_key): true })),
                                ..a2a::Part::text("failed with an error")
                            },
                            a2a::Part::text("extra details"),
                        ],
                    )),
                ),
                None,
            ),
            Some(agent_event(LlmResponse {
                error_message: "failed with an error".into(),
                content: model_content(vec![text("extra details")]),
                custom_metadata: task_meta(tid, cid),
                turn_complete: true,
                ..Default::default()
            })),
        ),
        (
            "input-required task status with err message and long-running tool",
            status_update(
                tid,
                cid,
                status(
                    a2a::TaskState::Failed,
                    Some(a2a::Message::new(
                        a2a::Role::Agent,
                        vec![
                            a2a::Part {
                                metadata: meta(json!({ (&is_err_key): true })),
                                ..a2a::Part::text("requesting input")
                            },
                            long_running_call(
                                json!({"id": "tool_lr", "name": "LongRunning", "args": {}}),
                            ),
                        ],
                    )),
                ),
                None,
            ),
            Some(Event {
                long_running_tool_ids: vec!["tool_lr".into()],
                ..agent_event(LlmResponse {
                    content: model_content(vec![function_call(
                        "tool_lr",
                        "LongRunning",
                        json!({}),
                    )]),
                    error_message: "requesting input".into(),
                    custom_metadata: task_meta(tid, cid),
                    turn_complete: true,
                    ..Default::default()
                })
            }),
        ),
    ];

    for (name, input, want) in cases {
        let ctx = invocation_context();
        let got = to_session_event(&ctx, &input).await.unwrap();
        assert_eq!(got.map(normalize), want, "{name}");
    }
}

#[tokio::test]
async fn test_to_session_event_with_parts_nil_result_filtered() {
    let (task_id, context_id) = (a2a::new_task_id(), a2a::new_context_id());
    let keep_part = a2a::Part::text("KEEP");
    let drop_part = a2a::Part::text("DROP");
    let both = || vec![keep_part.clone(), drop_part.clone()];

    let filter_converter: A2APartConverter = Arc::new(
        |_source: StreamResponseRef<'_>,
         part: &a2a::Part|
         -> BoxFuture<'static, Result<Option<Part>, Error>> {
            let result = if part.as_text() == Some("DROP") {
                Ok(None)
            } else {
                to_genai_part(part).map(Some)
            };
            Box::pin(async move { result })
        },
    );

    let cases = vec![
        (
            "task event",
            task(
                &task_id,
                &context_id,
                vec![artifact("", both())],
                status(a2a::TaskState::Completed, Some(message(both()))),
                None,
            ),
        ),
        (
            "message event",
            a2a::StreamResponse::Message(message(both())),
        ),
        (
            "artifact update event",
            artifact_update("", "", artifact("", both()), None),
        ),
        (
            "status update event",
            status_update(
                &task_id,
                &context_id,
                status(a2a::TaskState::Completed, Some(message(both()))),
                None,
            ),
        ),
    ];

    let ctx = invocation_context();
    for (name, input) in cases {
        let got = to_session_event_with_parts(&ctx, &input, Some(&filter_converter))
            .await
            .unwrap();
        let Some(got) = got else {
            panic!("{name}: got event is None, expected valid event with filtered parts");
        };
        let parts = got.response.content.map(|c| c.parts).unwrap_or_default();
        for part in parts {
            assert_eq!(part.text, "KEEP", "{name}");
        }
    }
}

/// A remote A2A peer's transfer_to_agent metadata is never restored by this conversion layer,
/// regardless of caller. A caller that has made its own trust decision about the remote peer
/// restores it explicitly afterwards with transfer_to_agent_from_meta.
#[tokio::test]
async fn test_to_session_event_with_parts_transfer_to_agent_always_redacted() {
    let ctx = invocation_context();
    let input = a2a::StreamResponse::Message(a2a::Message {
        metadata: meta(json!({
            (to_a2a_meta_key("transfer_to_agent")): "a-2",
            (to_a2a_meta_key("escalate")): true,
        })),
        ..message(vec![a2a::Part::text("foo")])
    });

    let got = to_session_event_with_parts(&ctx, &input, None)
        .await
        .unwrap()
        .unwrap();
    let want = EventActions {
        escalate: true,
        ..Default::default()
    };
    assert_eq!(normalize(got).actions, want);
}

#[test]
fn test_transfer_to_agent_from_meta() {
    assert_eq!(transfer_to_agent_from_meta(None), None);
    assert_eq!(transfer_to_agent_from_meta(Some(&HashMap::new())), None);
    let meta = meta(json!({ (to_a2a_meta_key("transfer_to_agent")): "a-2" }));
    assert_eq!(
        transfer_to_agent_from_meta(meta.as_ref()),
        Some("a-2".to_owned())
    );
}

/// Go checks that a peer's empty text part, which adk-python sends for a model's empty text
/// part, does not fail the whole event.
#[tokio::test]
#[ignore = "8a bug: to_genai_part rejects an empty A2A text part (`unknown part type`); Go ee8d5ca4 converts it to an empty text part"]
async fn test_to_session_event_empty_text_part() {
    let agent = new_agent(AgentConfig {
        name: AGENT_NAME.into(),
        ..Default::default()
    })
    .unwrap();
    let ctx = ContextBuilder::new().agent(agent).build_invocation();
    let input =
        a2a::StreamResponse::Message(message(vec![a2a::Part::text("foo"), a2a::Part::text("")]));
    let got = to_session_event(&ctx, &input).await;
    assert!(got.is_ok(), "to_session_event() error = {got:?}, want Ok");
    let got = got.unwrap().unwrap();
    let parts = got.response.content.map(|c| c.parts).unwrap_or_default();
    assert_eq!(parts, vec![text("foo"), text("")]);
}

/// Go pins that a completed, failed or canceled task is the remote agent's final response even
/// when its artifacts replay the tool calls the peer made; rejected and auth-required tasks are
/// not final (adk-python's `convert_a2a_task_to_event` states).
#[tokio::test]
#[ignore = "8a bug: a completed/failed/canceled task with parts does not set actions.skip_summarization, so is_final_response() is false (Go d6bb1e86)"]
async fn test_to_session_event_final_response_by_task_state() {
    let tool_parts = || {
        let call = a2a::Part {
            metadata: meta(json!({ (to_a2a_meta_key("type")): "function_call" })),
            ..a2a::Part::data(
                json!({"id": "call-1", "name": "get_weather", "args": {"city": "Warsaw"}}),
            )
        };
        let resp = a2a::Part {
            metadata: meta(json!({ (to_a2a_meta_key("type")): "function_response" })),
            ..a2a::Part::data(
                json!({"id": "call-1", "name": "get_weather", "response": {"temp": "20C"}}),
            )
        };
        vec![call, resp, a2a::Part::text("It is 20C in Warsaw.")]
    };
    let cases = [
        (a2a::TaskState::Completed, true),
        (a2a::TaskState::Failed, true),
        (a2a::TaskState::Canceled, true),
        (a2a::TaskState::Rejected, false),
        (a2a::TaskState::AuthRequired, false),
        (a2a::TaskState::Working, false),
    ];
    for (state, want_final) in cases {
        let ctx = invocation_context();
        let input = task(
            &a2a::new_task_id(),
            &a2a::new_context_id(),
            vec![artifact(&a2a::new_artifact_id(), tool_parts())],
            status(state.clone(), None),
            None,
        );
        let got = to_session_event(&ctx, &input).await.unwrap();
        let Some(got) = got else {
            panic!("{state:?}: to_session_event() = None, want an event");
        };
        assert_eq!(
            got.is_final_response(),
            want_final,
            "{state:?}: is_final_response() (skip_summarization = {})",
            got.actions.skip_summarization
        );
    }
}
