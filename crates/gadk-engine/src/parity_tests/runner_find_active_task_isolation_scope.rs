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

//! Port of Go runner/find_active_task_isolation_scope_test.go.

use std::sync::Arc;

use gadk_genai::{Content, FunctionCall, FunctionResponse, Part};
use gadk_model::LlmResponse;
use gadk_session::{CreateRequest, Event, InMemorySessionService, Session, SessionService};
use serde_json::{Map, Value, json};

use crate::runner::find_agent::find_active_task_isolation_scope;
use crate::tool::finish_task::{FINISH_TASK_SUCCESS_RESULT, FINISH_TASK_TOOL_NAME};

const SCOPE_A: &str = "adk-task-A";
const SCOPE_B: &str = "adk-task-B";

fn model_event_with_fc(scope: &str, name: &str, id: &str) -> Event {
    Event {
        author: "task_agent".into(),
        isolation_scope: scope.into(),
        response: LlmResponse {
            content: Some(Content {
                role: "model".into(),
                parts: vec![Part {
                    function_call: Some(FunctionCall {
                        name: name.into(),
                        id: id.into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
            }),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn user_event_with_fr(scope: &str, name: &str, id: &str, resp: Map<String, Value>) -> Event {
    Event {
        author: "user".into(),
        isolation_scope: scope.into(),
        response: LlmResponse {
            content: Some(Content {
                role: "user".into(),
                parts: vec![Part {
                    function_response: Some(FunctionResponse {
                        name: name.into(),
                        id: id.into(),
                        response: resp,
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
            }),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn finish_success_fr(scope: &str, id: &str) -> Event {
    let Value::Object(m) = json!({"result": FINISH_TASK_SUCCESS_RESULT}) else {
        unreachable!()
    };
    user_event_with_fr(scope, FINISH_TASK_TOOL_NAME, id, m)
}

fn finish_error_fr(scope: &str, id: &str) -> Event {
    let Value::Object(m) = json!({"error": "validation failed: ..."}) else {
        unreachable!()
    };
    user_event_with_fr(scope, FINISH_TASK_TOOL_NAME, id, m)
}

async fn build_session_with_events(events: Vec<Event>) -> Arc<dyn Session> {
    let svc = InMemorySessionService::new();
    let sess = svc
        .create(CreateRequest {
            app_name: "test-app".into(),
            user_id: "test-user".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    for mut ev in events {
        svc.append_event(&*sess, &mut ev).await.unwrap();
    }
    sess
}

#[tokio::test]
async fn test_find_active_task_isolation_scope() {
    let cases: Vec<(&str, Vec<Event>, &str)> = vec![
        ("empty_session", vec![], ""),
        (
            "no_scoped_events",
            vec![
                Event {
                    author: "user".into(),
                    ..Default::default()
                },
                Event {
                    author: "coordinator".into(),
                    ..Default::default()
                },
            ],
            "",
        ),
        (
            "single_open_scope",
            vec![
                Event {
                    author: "user".into(),
                    ..Default::default()
                },
                model_event_with_fc(SCOPE_A, "confirmation", "fc-A"),
            ],
            SCOPE_A,
        ),
        (
            "scope_closed_by_finish_task_success",
            vec![
                model_event_with_fc(SCOPE_A, "finish_task", "ft-A"),
                finish_success_fr(SCOPE_A, "ft-A"),
            ],
            "",
        ),
        (
            "error_finish_task_fr_keeps_scope_open",
            vec![
                model_event_with_fc(SCOPE_A, "finish_task", "ft-A"),
                finish_error_fr(SCOPE_A, "ft-A"),
            ],
            SCOPE_A,
        ),
        (
            "newer_open_older_closed",
            vec![
                model_event_with_fc(SCOPE_A, "finish_task", "ft-A"),
                finish_success_fr(SCOPE_A, "ft-A"),
                model_event_with_fc(SCOPE_B, "confirmation", "fc-B"),
            ],
            SCOPE_B,
        ),
        (
            "newer_closed_older_open",
            vec![
                model_event_with_fc(SCOPE_A, "confirmation", "fc-A"),
                model_event_with_fc(SCOPE_B, "finish_task", "ft-B"),
                finish_success_fr(SCOPE_B, "ft-B"),
            ],
            SCOPE_A,
        ),
    ];
    for (name, events, want) in cases {
        let sess = build_session_with_events(events).await;
        assert_eq!(
            find_active_task_isolation_scope(Some(&*sess)),
            want,
            "{name}"
        );
    }
}

#[test]
fn test_find_active_task_isolation_scope_nil_session() {
    assert_eq!(find_active_task_isolation_scope(None), "");
}
