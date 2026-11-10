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

//! Ports of the client tests in ADK-Go `session/vertexai/vertexai_test.go`.

use std::collections::BTreeMap;

use gadk_genai::Transcription;
use gadk_model::LlmResponse;
use gadk_platform::Platform;
use gadk_session::{Event, EventActions};
use serde_json::{Map, Value, json};

use crate::client::VertexClient;
use crate::config::{SESSION_ENDPOINT, resolve};
use crate::parity_tests::session_vertexai_append_event_temp_state::fake_client_config;
use crate::session::client::SessionClient;
use crate::testutil::FakeAgentEngine;

/// Go `newFakeVertexAiClient`: a session client on a [`FakeAgentEngine`] (Go's
/// `fakeVertexAiSessionService`: appended events are named `{session}/events/event-N` and
/// read back with an empty `actions` when they had none).
fn new_fake_vertex_ai_client() -> (SessionClient, FakeAgentEngine) {
    let fake = FakeAgentEngine::start();
    let config = fake_client_config(&fake.url());
    let resolved = resolve(
        config.endpoint.as_deref(),
        SESSION_ENDPOINT,
        config.credentials,
        config.without_authentication,
        config.http_client,
        config.env.as_ref(),
    )
    .unwrap();
    let client = SessionClient::new(
        VertexClient::new(resolved),
        config.project_id,
        config.location,
        config.reasoning_engine.unwrap_or_default(),
    );
    (client, fake)
}

/// A client with only a configured engine (Go's `&vertexAiClient{agentEngineData: …}`).
fn engine_client(reasoning_engine: &str) -> SessionClient {
    let resolved = resolve(
        Some("http://127.0.0.1:9"),
        SESSION_ENDPOINT,
        None,
        true,
        None,
        &gadk_platform::env::MapEnv(Default::default()),
    )
    .unwrap();
    SessionClient::new(
        VertexClient::new(resolved),
        String::new(),
        String::new(),
        reasoning_engine.to_owned(),
    )
}

#[test]
fn test_get_reasoning_engine_id() {
    struct Case {
        name: &'static str,
        existing_engine_id: &'static str,
        input_app_name: &'static str,
        expected_id: &'static str,
        expect_error: bool,
    }
    let tests = [
        Case {
            name: "Client already has engine ID configured",
            existing_engine_id: "999",
            input_app_name: "irrelevant-input",
            expected_id: "999",
            expect_error: false,
        },
        Case {
            name: "Input is a direct numeric ID",
            existing_engine_id: "",
            input_app_name: "123456",
            expected_id: "123456",
            expect_error: false,
        },
        Case {
            name: "Input is a valid full resource path",
            existing_engine_id: "",
            input_app_name: "projects/my-project/locations/us-central1/reasoningEngines/555123",
            expected_id: "555123",
            expect_error: false,
        },
        Case {
            name: "Input is valid path with dashes and underscores in project/location",
            existing_engine_id: "",
            input_app_name: "projects/my_project-1/locations/us_central-1/reasoningEngines/888",
            expected_id: "888",
            expect_error: false,
        },
        Case {
            name: "Input is malformed (ID is not numeric)",
            existing_engine_id: "",
            input_app_name: "projects/proj/locations/loc/reasoningEngines/abc",
            expected_id: "",
            expect_error: true,
        },
        Case {
            name: "Input is malformed (missing path components)",
            existing_engine_id: "",
            input_app_name: "locations/us-central1/reasoningEngines/123",
            expected_id: "",
            expect_error: true,
        },
        Case {
            name: "Input is random text",
            existing_engine_id: "",
            input_app_name: "some-random-app-name",
            expected_id: "",
            expect_error: true,
        },
    ];
    for tt in tests {
        let c = engine_client(tt.existing_engine_id);
        let got = c.reasoning_engine_id(tt.input_app_name);
        assert_eq!(
            got.is_err(),
            tt.expect_error,
            "{}: reasoning_engine_id() = {got:?}",
            tt.name
        );
        assert_eq!(
            got.unwrap_or_default(),
            tt.expected_id,
            "{}: reasoning_engine_id()",
            tt.name
        );
    }
}

fn state(v: Value) -> Option<Map<String, Value>> {
    v.as_object().cloned()
}

fn artifacts(entries: &[(&str, i64)]) -> Option<BTreeMap<String, i64>> {
    Some(entries.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect())
}

// parity: adapted: the events go to the fake's seeded session owned (FakeAgentEngine keeps events per existing session; Go's fake accepts any session)
#[tokio::test]
async fn test_vertex_ai_client_event_actions_round_trip() {
    let tests = [
        (
            "artifact delta only",
            EventActions {
                artifact_delta: artifacts(&[("chart.html", 1)]),
                ..Default::default()
            },
            EventActions {
                state_delta: state(json!({})),
                artifact_delta: artifacts(&[("chart.html", 1)]),
                ..Default::default()
            },
        ),
        (
            "state delta only",
            EventActions {
                state_delta: state(json!({"user:theme": "dark"})),
                ..Default::default()
            },
            EventActions {
                state_delta: state(json!({"user:theme": "dark"})),
                ..Default::default()
            },
        ),
        (
            "both deltas",
            EventActions {
                state_delta: state(json!({"user:theme": "dark"})),
                artifact_delta: artifacts(&[("chart.html", 1)]),
                ..Default::default()
            },
            EventActions {
                state_delta: state(json!({"user:theme": "dark"})),
                artifact_delta: artifacts(&[("chart.html", 1)]),
                ..Default::default()
            },
        ),
        (
            "empty deltas",
            EventActions {
                state_delta: state(json!({})),
                artifact_delta: artifacts(&[]),
                ..Default::default()
            },
            EventActions {
                state_delta: state(json!({})),
                ..Default::default()
            },
        ),
        (
            "no actions at all",
            EventActions::default(),
            EventActions {
                state_delta: state(json!({})),
                ..Default::default()
            },
        ),
    ];
    for (name, input, want) in tests {
        let (client, _fake) = new_fake_vertex_ai_client();

        let mut event = Event::new(&Platform::default(), "invocation-1");
        event.author = "agent".to_owned();
        event.actions = input;

        client
            .append_event("test-app", "owned", &event)
            .await
            .unwrap_or_else(|e| panic!("{name}: append_event() failed: {e}"));
        let got = client
            .list_session_events("test-app", "owned", None, 0)
            .await
            .unwrap_or_else(|e| panic!("{name}: list_session_events() failed: {e}"));
        assert_eq!(got.len(), 1, "{name}: len(got)");
        assert_eq!(got[0].actions, want, "{name}: Actions mismatch");
    }
}

// TestVertexAiClientTranscriptionRoundTrip pins that live audio transcriptions survive
// storage.
// parity: adapted: the events go to the fake's seeded session owned (FakeAgentEngine keeps events per existing session; Go's fake accepts any session)
#[tokio::test]
async fn test_vertex_ai_client_transcription_round_trip() {
    let transcription = |text: &str| Transcription {
        text: text.to_owned(),
        finished: true,
        ..Default::default()
    };
    let tests = [
        (
            "input transcription",
            LlmResponse {
                input_transcription: Some(transcription("what time is it")),
                ..Default::default()
            },
        ),
        (
            "output transcription",
            LlmResponse {
                output_transcription: Some(transcription("it is noon")),
                ..Default::default()
            },
        ),
    ];
    for (name, input) in tests {
        let (client, _fake) = new_fake_vertex_ai_client();

        let mut event = Event::new(&Platform::default(), "invocation-1");
        event.author = "agent".to_owned();
        event.response = input.clone();

        client
            .append_event("test-app", "owned", &event)
            .await
            .unwrap_or_else(|e| panic!("{name}: append_event() failed: {e}"));
        let got = client
            .list_session_events("test-app", "owned", None, 0)
            .await
            .unwrap_or_else(|e| panic!("{name}: list_session_events() failed: {e}"));
        assert_eq!(got.len(), 1, "{name}: len(got)");
        assert_eq!(
            got[0].response.input_transcription, input.input_transcription,
            "{name}: InputTranscription mismatch"
        );
        assert_eq!(
            got[0].response.output_transcription, input.output_transcription,
            "{name}: OutputTranscription mismatch"
        );
    }
}
