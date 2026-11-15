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

//! Port of ADK-Go agent/llmagent/llmagent_saveoutput_test.go.

use std::sync::Arc;

use gadk_agent::Mode;
use gadk_genai::{Content, FunctionCall, FunctionResponse, Part};
use gadk_model::LlmResponse;
use gadk_platform::Platform;
use gadk_session::{
    CreateRequest, Event, EventActions, GetRequest, InMemorySessionService, Session, SessionService,
};
use serde_json::{Map, Value, json};

use crate::llm::agent::{self, WrappedSession, maybe_save_output_to_state};
use crate::llm::config::LlmAgentConfig;
use crate::llm::state::reveal;

/// Port of createTestEvent.
fn create_test_event(author: &str, content_text: &str, is_final: bool) -> Event {
    let content = (!content_text.is_empty()).then(|| Content {
        role: "model".into(),
        parts: vec![Part::from_text(content_text)],
    });
    Event {
        invocation_id: "test_invocation".into(),
        author: author.into(),
        response: LlmResponse {
            content,
            partial: !is_final,
            ..Default::default()
        },
        actions: EventActions {
            state_delta: Some(Map::new()),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => panic!("want object"),
    }
}

struct Case {
    name: &'static str,
    agent_name: &'static str,
    output_key: &'static str,
    mode: Mode,
    event: Event,
    custom_parts: Option<Vec<Part>>,
    want: Map<String, Value>,
}

fn case(
    name: &'static str,
    agent_name: &'static str,
    output_key: &'static str,
    event: Event,
    custom_parts: Option<Vec<Part>>,
    want: Map<String, Value>,
) -> Case {
    Case {
        name,
        agent_name,
        output_key,
        mode: Mode::Unset,
        event,
        custom_parts,
        want,
    }
}

#[test]
fn test_llm_agent_maybe_save_output_to_state() {
    let fc_part = Part {
        function_call: Some(FunctionCall {
            id: "call_1".into(),
            name: "read_state".into(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let fr_part = Part {
        function_response: Some(FunctionResponse {
            name: "read_state".into(),
            response: obj(json!({"result": "SECRET_42"})),
            ..Default::default()
        }),
        ..Default::default()
    };
    let cases = vec![
        case(
            "skips when event author differs from agentConfig name",
            "agent_a",
            "result",
            create_test_event("agent_b", "Response from B", true),
            None,
            Map::new(),
        ),
        case(
            "saves when event author matches agentConfig name",
            "test_agent",
            "result",
            create_test_event("test_agent", "Test response", true),
            None,
            obj(json!({"result": "Test response"})),
        ),
        Case {
            mode: Mode::Task,
            ..case(
                "skips task mode: finish_task delivers the result, not a text turn",
                "test_agent",
                "result",
                create_test_event("test_agent", "Which language?", true),
                None,
                Map::new(),
            )
        },
        case(
            "skips when output_key is not set",
            "test_agent",
            "",
            create_test_event("test_agent", "Test response", true),
            None,
            Map::new(),
        ),
        case(
            "skips for non-final responses",
            "test_agent",
            "result",
            create_test_event("test_agent", "*genai.Partial response", false),
            None,
            Map::new(),
        ),
        case(
            "skips function call events",
            "test_agent",
            "result",
            create_test_event("test_agent", "", true),
            Some(vec![fc_part]),
            Map::new(),
        ),
        case(
            "skips function response events",
            "test_agent",
            "result",
            create_test_event("test_agent", "", true),
            Some(vec![fr_part]),
            Map::new(),
        ),
        case(
            "skips when event has no content text",
            "test_agent",
            "result",
            create_test_event("test_agent", "", true),
            None,
            Map::new(),
        ),
        case(
            "skips thought-only text",
            "test_agent",
            "result",
            create_test_event("test_agent", "", true),
            Some(vec![Part {
                text: "hidden thought".into(),
                thought: true,
                ..Default::default()
            }]),
            Map::new(),
        ),
        case(
            "concatenates multiple text parts",
            "test_agent",
            "result",
            create_test_event("test_agent", "", true),
            Some(vec![
                Part::from_text("Hello "),
                Part::from_text("world"),
                Part::from_text("!"),
            ]),
            obj(json!({"result": "Hello world!"})),
        ),
        case(
            "skips on case-sensitive name mismatch",
            "TestAgent",
            "result",
            create_test_event("testagent", "Test response", true),
            None,
            Map::new(),
        ),
    ];
    for mut tc in cases {
        if let Some(parts) = tc.custom_parts.take() {
            tc.event.response.content = Some(Content {
                role: "model".into(),
                parts,
            });
        }
        let a = agent::new(LlmAgentConfig {
            name: tc.agent_name.into(),
            output_key: tc.output_key.into(),
            mode: tc.mode,
            ..Default::default()
        })
        .unwrap();
        let state = reveal(a.as_ref()).expect("an LlmAgent");
        maybe_save_output_to_state(state, &mut tc.event);
        assert_eq!(
            tc.event.actions.state_delta,
            Some(tc.want),
            "{}: stateDelta mismatch",
            tc.name
        );
        assert!(
            tc.event.output.is_none(),
            "{}: event.output must stay None (only state_delta may be written here)",
            tc.name
        );
    }
}

/// Port of newSessionWithEvent.
async fn new_session_with_event(text: &str) -> Arc<dyn Session> {
    let svc = InMemorySessionService::new();
    let created = svc
        .create(CreateRequest {
            app_name: "app".into(),
            user_id: "u".into(),
            session_id: Some("s".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let mut ev = Event::new(&Platform::default(), "inv-existing");
    ev.author = "user".into();
    ev.response.content = Some(Content::from_text(text, "user"));
    svc.append_event(created.as_ref(), &mut ev).await.unwrap();
    svc.get(GetRequest {
        app_name: "app".into(),
        user_id: "u".into(),
        session_id: "s".into(),
        ..Default::default()
    })
    .await
    .unwrap()
}

/// Port of seedEvent.
fn seed_event(text: &str) -> Event {
    let mut ev = Event::new(&Platform::default(), "inv-seed");
    ev.author = "user".into();
    ev.response.content = Some(Content::from_text(text, "user"));
    ev
}

// parity: adapted: Go compares event pointers; Rust compares event IDs (Event::new assigns a fresh UUID)
#[tokio::test]
async fn test_wrapped_session_seed_not_persisted() {
    let base = new_session_with_event("existing turn").await;
    let base_len = base.events().len();
    let seed = seed_event("transient node input");
    let wrapped = WrappedSession::new(base.clone(), seed.clone());

    let evs = wrapped.events();
    assert_eq!(evs.len(), base_len + 1, "wrapped.events().len()");
    assert_eq!(
        evs.last().map(|e| e.id.clone()),
        Some(seed.id.clone()),
        "last wrapped event must be the seed"
    );

    assert_eq!(
        base.events().len(),
        base_len,
        "seed must not persist in the underlying session"
    );
    assert!(
        base.events().iter().all(|e| e.id != seed.id),
        "seed leaked into the underlying session history"
    );
}
