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

//! Ports of ADK-Go `server/adka2a/v2/processor_test.go`.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::collections::HashMap;

use gadk_genai::{Content, FunctionCall, FunctionResponse, Language, Outcome, Part, ROLE_MODEL};
use gadk_model::LlmResponse;
use gadk_session::{Event, EventActions};
use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};

use crate::convert::{
    DATA_PART_META_LONG_RUNNING_KEY, DATA_PART_META_TYPE_KEY, DATA_PART_TYPE_CODE_EXEC_RESULT,
    DATA_PART_TYPE_EXECUTABLE_CODE, DATA_PART_TYPE_FUNCTION_CALL, DATA_PART_TYPE_FUNCTION_RESPONSE,
    METADATA_ESCALATE_KEY, METADATA_PARTIAL_KEY, METADATA_TRANSFER_TO_AGENT_KEY, to_a2a_meta_key,
};
use crate::server::context::InvocationMeta;
use crate::server::processor::{
    EventProcessor, TaskInfo, error_from_response, new_artifact_event, new_status_update,
    to_task_failed_update_event,
};
use crate::server::task_artifact::{LegacyArtifactMaker, new_legacy_partial_artifact_update};

/// Go `modelResponseFromParts`.
pub(crate) fn model_response_from_parts(parts: Vec<Part>) -> LlmResponse {
    LlmResponse {
        content: Some(Content {
            role: ROLE_MODEL.into(),
            parts,
        }),
        ..Default::default()
    }
}

/// Go `modelPartialResponseFromParts`.
pub(crate) fn model_partial_response_from_parts(parts: Vec<Part>) -> LlmResponse {
    LlmResponse {
        partial: true,
        ..model_response_from_parts(parts)
    }
}

fn response_event(response: LlmResponse) -> Event {
    Event {
        response,
        ..Default::default()
    }
}

fn error_response(code: &str, message: &str) -> LlmResponse {
    LlmResponse {
        error_code: code.into(),
        error_message: message.into(),
        ..Default::default()
    }
}

fn meta(v: Value) -> HashMap<String, Value> {
    match v {
        Value::Object(m) => m.into_iter().collect(),
        other => panic!("not an object: {other}"),
    }
}

fn args(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        other => panic!("not an object: {other}"),
    }
}

/// Go `p.SetMeta(k, v)`.
fn with_meta(mut part: a2a::Part, key: &str, value: Value) -> a2a::Part {
    part.metadata
        .get_or_insert_with(HashMap::new)
        .insert(key.to_owned(), value);
    part
}

fn data_part(kind: &str, data: Value) -> a2a::Part {
    with_meta(a2a::Part::data(data), DATA_PART_META_TYPE_KEY, json!(kind))
}

/// Go `newNonPartialArtifactEvent`: events are explicitly marked as ADK partial or non-partial,
/// which tells consumers the remote agent runs its own aggregation.
fn new_non_partial_artifact_event(task: &TaskInfo, parts: Vec<a2a::Part>) -> a2a::StreamResponse {
    let mut ev = new_artifact_event(task, parts);
    ev.metadata = Some(meta(json!({ METADATA_PARTIAL_KEY: false })));
    a2a::StreamResponse::ArtifactUpdate(ev)
}

/// Go `newNonPartialArtifactUpdateEvent`.
fn new_non_partial_artifact_update_event(
    task: &TaskInfo,
    parts: Vec<a2a::Part>,
) -> a2a::StreamResponse {
    let a2a::StreamResponse::ArtifactUpdate(mut ev) = new_non_partial_artifact_event(task, parts)
    else {
        unreachable!()
    };
    ev.append = Some(true);
    a2a::StreamResponse::ArtifactUpdate(ev)
}

/// Go `newDiscardPartialArtifactUpdate`.
fn new_discard_partial_artifact_update(task: &TaskInfo) -> a2a::StreamResponse {
    let mut ev = new_legacy_partial_artifact_update(task, "", vec![a2a::Part::data(json!({}))]);
    ev.last_chunk = Some(true);
    a2a::StreamResponse::ArtifactUpdate(ev)
}

/// Go `newFinalStatusUpdate`.
pub(crate) fn new_final_status_update(
    task: &TaskInfo,
    state: a2a::TaskState,
    msg: Option<a2a::Message>,
) -> a2a::StreamResponse {
    a2a::StreamResponse::StatusUpdate(new_status_update(task, state, msg))
}

fn failed(task: &TaskInfo, code: &str, message: &str, meta: Value) -> a2a::StreamResponse {
    a2a::StreamResponse::StatusUpdate(to_task_failed_update_event(
        task,
        &error_from_response(&error_response(code, message)),
        Some(self::meta(meta)),
    ))
}

/// A long-running `weather` call part, as the input-required message carries it.
fn long_running_call_part() -> a2a::Part {
    with_meta(
        data_part(
            DATA_PART_TYPE_FUNCTION_CALL,
            json!({"id": "get_weather", "name": "weather", "args": {"city": "Warsaw"}}),
        ),
        DATA_PART_META_LONG_RUNNING_KEY,
        json!(true),
    )
}

fn input_required(task: &TaskInfo, parts: Vec<a2a::Part>) -> a2a::StreamResponse {
    new_final_status_update(
        task,
        a2a::TaskState::InputRequired,
        Some(a2a::Message::new(a2a::Role::Agent, parts)),
    )
}

fn weather_call() -> Part {
    Part {
        function_call: Some(FunctionCall {
            id: "get_weather".into(),
            name: "weather".into(),
            args: args(json!({"city": "Warsaw"})),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Go's cmp options ignore `Message.ID`, `Artifact.ID` and `TaskStatus.Timestamp`: compared as
/// JSON (plan D5) without those keys.
pub(crate) fn normalized(value: &impl serde::Serialize) -> Value {
    fn strip(v: &mut Value) {
        match v {
            Value::Object(m) => {
                m.remove("messageId");
                m.remove("artifactId");
                m.remove("timestamp");
                m.values_mut().for_each(strip);
            }
            Value::Array(items) => items.iter_mut().for_each(strip),
            _ => {}
        }
    }
    let mut v = serde_json::to_value(value).unwrap();
    strip(&mut v);
    v
}

fn new_processor(task: &TaskInfo) -> EventProcessor {
    EventProcessor::new(
        task.clone(),
        InvocationMeta::default(),
        None,
        Box::new(LegacyArtifactMaker::new(task.clone())),
    )
}

/// Go `makeTerminalEvents`.
fn make_terminal_events(processor: &EventProcessor) -> Vec<a2a::StreamResponse> {
    let mut result = Vec::with_capacity(2);
    if let Some(update) = processor.make_final_artifact_update() {
        result.push(a2a::StreamResponse::ArtifactUpdate(update));
    }
    result.push(a2a::StreamResponse::StatusUpdate(
        processor.make_final_status_update(),
    ));
    result
}

fn task_info() -> TaskInfo {
    TaskInfo {
        task_id: a2a::new_task_id(),
        context_id: a2a::new_context_id(),
    }
}

#[tokio::test]
async fn test_event_processor_process() {
    let task = task_info();
    let text = |t: &str| a2a::Part::text(t);

    struct Case {
        name: &'static str,
        events: Vec<Event>,
        processed: Vec<a2a::StreamResponse>,
        terminal: Vec<a2a::StreamResponse>,
    }
    let completed = || new_final_status_update(&task, a2a::TaskState::Completed, None);
    let partial = |p: a2a::Part| with_meta(p, &to_a2a_meta_key("partial"), json!(true));
    let cases = vec![
        Case {
            name: "skip if no response",
            events: vec![
                Event {
                    id: "125".into(),
                    invocation_id: "345".into(),
                    ..Default::default()
                },
                Event {
                    id: "127".into(),
                    invocation_id: "345".into(),
                    branch: "b".into(),
                    author: "a".into(),
                    ..Default::default()
                },
            ],
            processed: vec![],
            terminal: vec![completed()],
        },
        Case {
            name: "skip if no content parts",
            events: vec![
                response_event(LlmResponse {
                    content: Some(Content {
                        role: ROLE_MODEL.into(),
                        parts: vec![],
                    }),
                    ..Default::default()
                }),
                response_event(LlmResponse {
                    interrupted: true,
                    ..Default::default()
                }),
            ],
            processed: vec![],
            terminal: vec![completed()],
        },
        Case {
            name: "multi-part artifact update",
            events: vec![response_event(model_response_from_parts(vec![
                Part::from_text("Hello"),
                Part::from_text(", world!"),
            ]))],
            processed: vec![new_non_partial_artifact_event(
                &task,
                vec![text("Hello"), text(", world!")],
            )],
            terminal: vec![completed()],
        },
        Case {
            name: "multiple artifact updates",
            events: vec![
                response_event(model_response_from_parts(vec![Part::from_executable_code(
                    "get_the_answer()",
                    Language::Python,
                )])),
                response_event(model_response_from_parts(vec![
                    Part::from_code_execution_result(Outcome::Ok, "42"),
                ])),
                response_event(model_response_from_parts(vec![Part::from_text(
                    "The answer is 42",
                )])),
            ],
            processed: vec![
                new_non_partial_artifact_event(
                    &task,
                    vec![data_part(
                        DATA_PART_TYPE_EXECUTABLE_CODE,
                        json!({"code": "get_the_answer()", "language": "PYTHON"}),
                    )],
                ),
                new_non_partial_artifact_update_event(
                    &task,
                    vec![data_part(
                        DATA_PART_TYPE_CODE_EXEC_RESULT,
                        json!({"outcome": "OUTCOME_OK", "output": "42"}),
                    )],
                ),
                new_non_partial_artifact_update_event(&task, vec![text("The answer is 42")]),
            ],
            terminal: vec![completed()],
        },
        Case {
            name: "failed without artifacts",
            events: vec![response_event(error_response("1", "failed"))],
            processed: vec![],
            terminal: vec![failed(
                &task,
                "1",
                "failed",
                json!({ to_a2a_meta_key("error_code"): "1" }),
            )],
        },
        Case {
            name: "the first failure is returned",
            events: vec![
                response_event(error_response("1", "failed 1")),
                response_event(error_response("2", "failed 2")),
            ],
            processed: vec![],
            terminal: vec![failed(
                &task,
                "1",
                "failed 1",
                json!({ to_a2a_meta_key("error_code"): "1" }),
            )],
        },
        Case {
            name: "failed with artifacts",
            events: vec![
                response_event(model_response_from_parts(vec![Part::from_text(
                    "The answer is",
                )])),
                response_event(model_response_from_parts(vec![Part::from_text("42")])),
                response_event(error_response("1", "failed")),
            ],
            processed: vec![
                new_non_partial_artifact_event(&task, vec![text("The answer is")]),
                new_non_partial_artifact_update_event(&task, vec![text("42")]),
            ],
            terminal: vec![failed(
                &task,
                "1",
                "failed",
                json!({ to_a2a_meta_key("error_code"): "1" }),
            )],
        },
        Case {
            name: "failed before receiving all parts",
            events: vec![
                response_event(model_response_from_parts(vec![Part::from_text(
                    "The answer is",
                )])),
                response_event(error_response("1", "failed")),
                response_event(model_response_from_parts(vec![Part::from_text("42")])),
            ],
            processed: vec![
                new_non_partial_artifact_event(&task, vec![text("The answer is")]),
                new_non_partial_artifact_update_event(&task, vec![text("42")]),
            ],
            terminal: vec![failed(
                &task,
                "1",
                "failed",
                json!({ to_a2a_meta_key("error_code"): "1" }),
            )],
        },
        Case {
            name: "input_required not produced for a normal function call",
            events: vec![response_event(model_response_from_parts(vec![
                Part::from_function_call("get_weather", args(json!({"city": "Warsaw"}))),
            ]))],
            processed: vec![new_non_partial_artifact_event(
                &task,
                vec![with_meta(
                    data_part(
                        DATA_PART_TYPE_FUNCTION_CALL,
                        json!({"name": "get_weather", "args": {"city": "Warsaw"}}),
                    ),
                    DATA_PART_META_LONG_RUNNING_KEY,
                    json!(false),
                )],
            )],
            terminal: vec![completed()],
        },
        Case {
            name: "input_required produced for long running function",
            events: vec![Event {
                long_running_tool_ids: vec!["get_weather".into()],
                ..response_event(model_response_from_parts(vec![weather_call()]))
            }],
            processed: vec![],
            terminal: vec![input_required(&task, vec![long_running_call_part()])],
        },
        Case {
            name: "long running tool call before receiving all parts",
            events: vec![
                Event {
                    long_running_tool_ids: vec!["get_weather".into()],
                    ..response_event(model_response_from_parts(vec![weather_call()]))
                },
                response_event(model_response_from_parts(vec![Part::from_text(
                    "This will take a while",
                )])),
            ],
            processed: vec![new_non_partial_artifact_event(
                &task,
                vec![text("This will take a while")],
            )],
            terminal: vec![input_required(&task, vec![long_running_call_part()])],
        },
        Case {
            name: "long running tool response without ID",
            events: vec![
                Event {
                    long_running_tool_ids: vec!["get_weather".into()],
                    ..response_event(model_response_from_parts(vec![weather_call()]))
                },
                // No LongRunningToolIDs here
                response_event(model_response_from_parts(vec![Part {
                    function_response: Some(FunctionResponse {
                        id: "get_weather".into(),
                        name: "weather".into(),
                        response: args(json!({"status": "pending"})),
                        ..Default::default()
                    }),
                    ..Default::default()
                }])),
            ],
            processed: vec![],
            terminal: vec![input_required(
                &task,
                vec![
                    long_running_call_part(),
                    data_part(
                        DATA_PART_TYPE_FUNCTION_RESPONSE,
                        json!({"id": "get_weather", "name": "weather", "response": {"status": "pending"}}),
                    ),
                ],
            )],
        },
        Case {
            name: "actions in completed event meta",
            events: vec![Event {
                id: "125".into(),
                invocation_id: "345".into(),
                actions: EventActions {
                    escalate: true,
                    transfer_to_agent: "a-2".into(),
                    ..Default::default()
                },
                ..Default::default()
            }],
            processed: vec![],
            terminal: vec![a2a::StreamResponse::StatusUpdate(
                a2a::TaskStatusUpdateEvent {
                    task_id: task.task_id.clone(),
                    context_id: task.context_id.clone(),
                    status: a2a::TaskStatus {
                        state: a2a::TaskState::Completed,
                        message: None,
                        timestamp: None,
                    },
                    metadata: Some(meta(json!({
                        METADATA_ESCALATE_KEY: true,
                        METADATA_TRANSFER_TO_AGENT_KEY: "a-2",
                    }))),
                },
            )],
        },
        Case {
            name: "last agent transfer is returned",
            events: vec![
                Event {
                    id: "125".into(),
                    invocation_id: "345".into(),
                    actions: EventActions {
                        transfer_to_agent: "a-2".into(),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                Event {
                    id: "126".into(),
                    invocation_id: "346".into(),
                    actions: EventActions {
                        transfer_to_agent: "a-3".into(),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            ],
            processed: vec![],
            terminal: vec![a2a::StreamResponse::StatusUpdate(
                a2a::TaskStatusUpdateEvent {
                    task_id: task.task_id.clone(),
                    context_id: task.context_id.clone(),
                    status: a2a::TaskStatus {
                        state: a2a::TaskState::Completed,
                        message: None,
                        timestamp: None,
                    },
                    metadata: Some(meta(json!({ METADATA_TRANSFER_TO_AGENT_KEY: "a-3" }))),
                },
            )],
        },
        Case {
            name: "actions not overwritten by subsequent events",
            events: vec![
                Event {
                    actions: EventActions {
                        escalate: true,
                        transfer_to_agent: "a-2".into(),
                        ..Default::default()
                    },
                    ..response_event(model_response_from_parts(vec![Part::from_text(
                        "The answer is",
                    )]))
                },
                response_event(error_response("1", "failed")),
            ],
            processed: vec![new_non_partial_artifact_event(
                &task,
                vec![text("The answer is")],
            )],
            terminal: vec![failed(
                &task,
                "1",
                "failed",
                json!({
                    to_a2a_meta_key("error_code"): "1",
                    METADATA_ESCALATE_KEY: true,
                    METADATA_TRANSFER_TO_AGENT_KEY: "a-2",
                }),
            )],
        },
        Case {
            name: "partial events parts marked",
            events: vec![
                response_event(model_partial_response_from_parts(vec![
                    Part::from_text("The answer is"),
                    Part::from_executable_code("get_the_answer()", Language::Python),
                ])),
                response_event(model_partial_response_from_parts(vec![
                    Part::from_code_execution_result(Outcome::Ok, "42"),
                    Part::from_text("42"),
                ])),
            ],
            processed: vec![
                a2a::StreamResponse::ArtifactUpdate(new_legacy_partial_artifact_update(
                    &task,
                    "placeholder",
                    vec![
                        partial(text("The answer is")),
                        partial(data_part(
                            DATA_PART_TYPE_EXECUTABLE_CODE,
                            json!({"code": "get_the_answer()", "language": "PYTHON"}),
                        )),
                    ],
                )),
                a2a::StreamResponse::ArtifactUpdate(new_legacy_partial_artifact_update(
                    &task,
                    "placeholder",
                    vec![
                        partial(data_part(
                            DATA_PART_TYPE_CODE_EXEC_RESULT,
                            json!({"outcome": "OUTCOME_OK", "output": "42"}),
                        )),
                        partial(text("42")),
                    ],
                )),
            ],
            terminal: vec![new_discard_partial_artifact_update(&task), completed()],
        },
        Case {
            name: "metadata merging",
            events: vec![Event {
                invocation_id: "test-invocation-id".into(),
                ..response_event(model_response_from_parts(vec![Part::from_text("Hello")]))
            }],
            processed: vec![{
                let a2a::StreamResponse::ArtifactUpdate(mut ev) =
                    new_non_partial_artifact_event(&task, vec![text("Hello")])
                else {
                    unreachable!()
                };
                ev.metadata.as_mut().unwrap().insert(
                    to_a2a_meta_key("invocation_id"),
                    json!("test-invocation-id"),
                );
                a2a::StreamResponse::ArtifactUpdate(ev)
            }],
            terminal: vec![completed()],
        },
    ];

    for tc in cases {
        let mut processor = new_processor(&task);
        let mut got = Vec::new();
        for event in &tc.events {
            if let Some(update) = processor.process(event).await.unwrap() {
                got.push(a2a::StreamResponse::ArtifactUpdate(update));
            }
        }
        assert_eq!(
            normalized(&got),
            normalized(&tc.processed),
            "{}: processor.process() wrong result",
            tc.name
        );
        assert_eq!(
            normalized(&make_terminal_events(&processor)),
            normalized(&tc.terminal),
            "{}: makeTerminalEvents() wrong result",
            tc.name
        );
    }
}

#[tokio::test]
async fn test_event_processor_artifact_updates() {
    let task = task_info();
    let events = [
        response_event(model_response_from_parts(vec![Part::from_executable_code(
            "find_cat()",
            Language::Python,
        )])),
        response_event(model_response_from_parts(vec![
            Part::from_code_execution_result(Outcome::Ok, "https://cats.com/image.png"),
            Part::from_text("A cat image was downloaded to /home/me/cat.png"),
            Part::from_function_call(
                "download",
                args(json!({"from": "https://cats.com/image.png", "to": "/home/me/cat.png"})),
            ),
        ])),
        response_event(model_response_from_parts(vec![
            Part::from_function_response("download", args(json!({"status": "ok"}))),
        ])),
        response_event(model_response_from_parts(vec![Part::from_text(
            "A cat image was downloaded to /home/me/cat.png",
        )])),
    ];

    let mut processor = new_processor(&task);
    let mut got = Vec::new();
    for (i, event) in events.iter().enumerate() {
        let processed = processor.process(event).await;
        let processed = processed.unwrap_or_else(|e| panic!("process() error for {i}-th = {e}"));
        got.push(processed.unwrap_or_else(|| panic!("process() for {i}-th = None")));
    }

    assert_eq!(got.len(), events.len());
    assert!(
        got[0].append.is_none() && got[0].last_chunk.is_none(),
        "process()[0] = {:?}, want {{Append=false, LastChunk=false}}",
        got[0]
    );
    let want_id = got[0].artifact.artifact_id.clone();
    for (i, event) in got.iter().enumerate().skip(1) {
        assert!(
            event.last_chunk.is_none(),
            "process()[{i}] = {event:?}, want LastChunk=false"
        );
        assert_eq!(event.artifact.artifact_id, want_id, "process()[{i}] ID");
    }

    let terminal = make_terminal_events(&processor);
    assert_eq!(
        terminal.len(),
        1,
        "makeTerminalEvents() = {terminal:?}, want [finalStatusUpdate]"
    );
}

#[tokio::test]
async fn test_event_processor_partial_events_are_discarded_as_an_artifact() {
    let task = task_info();
    let events = vec![
        response_event(model_partial_response_from_parts(vec![Part::from_text(
            "Hello, ",
        )])),
        response_event(model_partial_response_from_parts(vec![Part::from_text(
            "world!",
        )])),
        response_event(model_response_from_parts(vec![Part::from_text(
            "Hello, world!",
        )])),
    ];

    let mut processor = new_processor(&task);
    let mut got = Vec::new();
    for event in &events {
        got.push(processor.process(event).await.unwrap().unwrap());
    }
    assert_eq!(got.len(), events.len());

    let last = events.len() - 1;
    for i in [0, last] {
        assert!(
            got[i].append.is_none() && got[i].last_chunk.is_none(),
            "process()[{i}] = {:?}, want {{Append=false, LastChunk=false}}",
            got[i]
        );
    }
    assert_eq!(
        got[0].artifact.artifact_id, got[1].artifact.artifact_id,
        "partial events applied to different artifact"
    );
    assert_ne!(
        got[0].artifact.artifact_id, got[last].artifact.artifact_id,
        "final event applied to partial artifact"
    );
    for (i, event) in got.iter().enumerate() {
        assert!(
            event.last_chunk.is_none(),
            "process()[{i}] = {event:?}, want LastChunk=false"
        );
    }

    let terminal = make_terminal_events(&processor);
    assert_eq!(
        terminal.len(),
        2,
        "makeTerminalEvents() = {terminal:?}, want [finalArtifactChunk, finalStatusUpdate]"
    );
    let a2a::StreamResponse::ArtifactUpdate(final_update) = &terminal[0] else {
        panic!(
            "makeTerminalEvents()[0] = {:?}, want an artifact update",
            terminal[0]
        );
    };
    let want = a2a::TaskArtifactUpdateEvent {
        task_id: task.task_id.clone(),
        context_id: task.context_id.clone(),
        artifact: a2a::Artifact {
            artifact_id: got[0].artifact.artifact_id.clone(),
            name: None,
            description: None,
            parts: vec![with_meta(
                a2a::Part::data(json!({})),
                METADATA_PARTIAL_KEY,
                json!(true),
            )],
            metadata: Some(meta(json!({ METADATA_PARTIAL_KEY: true }))),
            extensions: None,
        },
        append: None,
        last_chunk: Some(true),
        metadata: Some(meta(json!({ METADATA_PARTIAL_KEY: true }))),
    };
    assert_eq!(final_update, &want);
}
