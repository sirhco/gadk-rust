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
use gadk_model::rest::test_util::FakeServer;
use gadk_platform::Platform;
use gadk_session::toolauth::{
    Config, Credential, CredentialType, OAuth2Auth, OAuthFlow, OAuthFlows, Scheme, SchemeType,
};
use gadk_session::{Event, EventActions, Session, SessionService};
use serde_json::value::RawValue;
use serde_json::{Map, Value, json};

use crate::client::VertexClient;
use crate::config::{SESSION_ENDPOINT, resolve};
use crate::parity_tests::session_vertexai_append_event_temp_state::fake_client_config;
use crate::session::client::SessionClient;
use crate::session::{VertexAiSessionService, VertexSession};
use crate::testutil::FakeAgentEngine;

/// Go `newFakeVertexAiClient`: a session client on a [`FakeAgentEngine`] (Go's
/// `fakeVertexAiSessionService`: appended events are named `{session}/events/event-N` and
/// read back with an empty `actions` when they had none).
fn new_fake_vertex_ai_client() -> (SessionClient, FakeAgentEngine) {
    let fake = FakeAgentEngine::start();
    (client_at(&fake.url()), fake)
}

/// A session client with Go's `newFakeVertexAiClient` config on any fake at `url`.
fn client_at(url: &str) -> SessionClient {
    let config = fake_client_config(url);
    let resolved = resolve(
        config.endpoint.as_deref(),
        SESSION_ENDPOINT,
        config.credentials,
        config.without_authentication,
        config.http_client,
        config.env.as_ref(),
    )
    .unwrap();
    SessionClient::new(
        VertexClient::new(resolved),
        config.project_id,
        config.location,
        config.reasoning_engine.unwrap_or_default(),
    )
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

/// The fake's stored-event name of session `owned` under Go's `newFakeVertexAiClient` engine.
const OWNED: &str =
    "projects/test-project/locations/us-central1/reasoningEngines/123/sessions/owned";

/// Go `testAuthConfig`: a credential request as a tool records it. The token endpoint auth
/// method and the scopes are set, so it reads back unchanged.
fn test_auth_config() -> Config {
    Config {
        auth_scheme: Scheme {
            r#type: SchemeType::OAUTH2,
            flows: Some(OAuthFlows {
                authorization_code: Some(OAuthFlow {
                    authorization_url: "https://example.com/authorize".into(),
                    token_url: "https://example.com/token".into(),
                    scopes: BTreeMap::from([("read".into(), "Read".into())]),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        raw_auth_credential: Some(Credential {
            auth_type: CredentialType::OAUTH2,
            oauth2: Some(OAuth2Auth {
                client_id: "cid".into(),
                token_endpoint_auth_method: "client_secret_basic".into(),
                ..Default::default()
            }),
            ..Default::default()
        }),
        credential_key: "my_tool_tokens".into(),
        ..Default::default()
    }
}

/// Go `pythonRequestedAuthConfigs`: the `actions.requested_auth_configs` value that adk-python
/// 2.12.0 writes for [`test_auth_config`] under `call-1` (`vertex_ai_session_service.py`).
const PYTHON_REQUESTED_AUTH_CONFIGS: &str = concat!(
    r#"{"call-1":{"authScheme":{"flows":{"authorizationCode":{"authorizationUrl":"https://example.com/authorize","scopes":{"read":"Read"},"tokenUrl":"https://example.com/token"}},"type":"oauth2"},"#,
    r#""credentialKey":"my_tool_tokens","rawAuthCredential":{"authType":"oauth2","oauth2":{"clientId":"cid","tokenEndpointAuthMethod":"client_secret_basic"}}}}"#
);

/// Go `pythonAuthURI`: the authorization URI adk-python 2.12.0 generated for the request in
/// [`PYTHON_RAW_EVENT_WITH_AUTH_REQUEST`].
const PYTHON_AUTH_URI: &str = concat!(
    "https://example.com/authorize?response_type=code&client_id=cid&redirect_uri=https%3A%2F%2Fapp.example%2Fcb",
    "&scope=read&state=jSeMibpnQgvDzbS8Mm6d3NL2u1iv9G&access_type=offline&prompt=consent"
);

/// Go `pythonRawEventWithAuthRequest`: the raw_event adk-python 2.12.0 writes for an event whose
/// tool called request_credential with an OAuth2 authorization code config, client secret
/// included. The stored request is AuthHandler.generate_auth_request output: the secret is gone,
/// and the exchanged credential carries the auth URI and state.
fn python_raw_event_with_auth_request() -> String {
    [
        r#"{"invocationId":"inv-1","author":"agent","actions":{"stateDelta":{},"artifactDelta":{},"#,
        r#""requestedAuthConfigs":{"call-1":{"authScheme":{"type":"oauth2","flows":{"authorizationCode":{"scopes":{"read":"Read"},"#,
        r#""authorizationUrl":"https://example.com/authorize","tokenUrl":"https://example.com/token"}}},"#,
        r#""rawAuthCredential":{"authType":"oauth2","oauth2":{"clientId":"cid","redirectUri":"https://app.example/cb","tokenEndpointAuthMethod":"client_secret_basic"}},"#,
        r#""exchangedAuthCredential":{"authType":"oauth2","oauth2":{"clientId":"cid","authUri":""#,
        PYTHON_AUTH_URI,
        r#"","#,
        r#""state":"jSeMibpnQgvDzbS8Mm6d3NL2u1iv9G","redirectUri":"https://app.example/cb","tokenEndpointAuthMethod":"client_secret_basic"}},"#,
        r#""credentialKey":"my_tool_tokens"}},"requestedToolConfirmations":{}},"nodeInfo":{"path":""},"id":"e1","timestamp":1700000000.0}"#,
    ]
    .concat()
}

/// A client whose ListEvents answers `events` (Go's `fakeVertexAiSessionService{events: …}`).
fn client_listing(events: Value) -> (SessionClient, FakeServer) {
    let server = FakeServer::json(json!({ "sessionEvents": events }).to_string());
    (client_at(&server.url()), server)
}

// parity: adapted: the write goes to the fake's seeded session owned and the read to a FakeServer answering ListEvents (Go's fake accepts any session and is seeded with events)
#[tokio::test]
async fn test_vertex_ai_client_requested_auth_configs_match_python() {
    // written_as_adk-python_writes_them
    {
        let (client, fake) = new_fake_vertex_ai_client();
        let mut event = Event::new(&Platform::default(), "invocation-1");
        event.author = "agent".to_owned();
        event.actions.requested_auth_configs =
            BTreeMap::from([("call-1".into(), test_auth_config())]);
        client
            .append_event("test-app", "owned", &event)
            .await
            .unwrap_or_else(|e| panic!("appendEvent() failed: {e}"));

        let want: Value = serde_json::from_str(PYTHON_REQUESTED_AUTH_CONFIGS).unwrap();
        let events = fake.events(OWNED);
        assert_eq!(
            events[0]["actions"]["requestedAuthConfigs"], want,
            "written_as_adk-python_writes_them: requested_auth_configs differs from adk-python"
        );
    }

    // read_as_adk-python_writes_them
    {
        let configs: Value = serde_json::from_str(PYTHON_REQUESTED_AUTH_CONFIGS).unwrap();
        let (client, _server) = client_listing(json!([{
            "name": "sessions/session-1/events/event-1",
            "author": "agent",
            "actions": {"requestedAuthConfigs": configs},
        }]));
        let got = client
            .list_session_events("test-app", "session-1", None, 0)
            .await
            .unwrap_or_else(|e| {
                panic!("read_as_adk-python_writes_them: listSessionEvents() failed: {e}")
            });
        assert_eq!(got.len(), 1, "read_as_adk-python_writes_them: len(got)");
        let want = BTreeMap::from([("call-1".to_owned(), test_auth_config())]);
        assert_eq!(
            got[0].actions.requested_auth_configs, want,
            "read_as_adk-python_writes_them: RequestedAuthConfigs mismatch"
        );
    }

    // read_from_raw_event_as_adk-python_writes_it: adk-python writes raw_event on every event,
    // and Go reads raw_event in preference to the actions column, so this is the path a request
    // written by adk-python takes.
    {
        let raw: Value = serde_json::from_str(&python_raw_event_with_auth_request()).unwrap();
        let (client, _server) = client_listing(json!([{
            "name": "sessions/session-1/events/e1",
            "author": "agent",
            "rawEvent": raw,
        }]));
        let got = client
            .list_session_events("test-app", "session-1", None, 0)
            .await
            .unwrap_or_else(|e| panic!("read_from_raw_event: listSessionEvents() failed: {e}"));
        assert_eq!(got.len(), 1, "read_from_raw_event: len(got)");
        let oauth2 = |extra: fn(&mut OAuth2Auth)| {
            let mut o = OAuth2Auth {
                client_id: "cid".into(),
                redirect_uri: "https://app.example/cb".into(),
                token_endpoint_auth_method: "client_secret_basic".into(),
                ..Default::default()
            };
            extra(&mut o);
            Some(Credential {
                auth_type: CredentialType::OAUTH2,
                oauth2: Some(o),
                ..Default::default()
            })
        };
        let want = Config {
            raw_auth_credential: oauth2(|_| {}),
            exchanged_auth_credential: oauth2(|o| {
                o.auth_uri = PYTHON_AUTH_URI.into();
                o.state = "jSeMibpnQgvDzbS8Mm6d3NL2u1iv9G".into();
            }),
            ..test_auth_config()
        };
        assert_eq!(
            got[0].actions.requested_auth_configs,
            BTreeMap::from([("call-1".to_owned(), want)]),
            "read_from_raw_event: RequestedAuthConfigs mismatch"
        );
    }
}

// Dropping a request Go cannot read would lose the pending sign-in without a trace, so the read
// fails. adk-python fails the read the same way for a request it cannot validate.
// parity: adapted: a FakeServer answers ListEvents with each case's event (Go seeds its fake)
#[tokio::test]
async fn test_vertex_ai_client_rejects_requested_auth_config_that_does_not_decode() {
    let cases = [
        // A scheme with no type, and no field to infer one from.
        (
            "actions column",
            json!({
                "name": "sessions/session-1/events/event-1",
                "author": "agent",
                "actions": {"requestedAuthConfigs": {"call-1": {"authScheme": {}}}},
            }),
        ),
        // An implicit flow without authorizationUrl, as adk-python 2.12.0 writes it. adk-python
        // reads this scheme back as a CustomAuthScheme; toolauth rejects the flow.
        (
            "raw_event",
            json!({
                "name": "sessions/session-1/events/e2",
                "author": "agent",
                "rawEvent": serde_json::from_str::<Value>(concat!(
                    r#"{"invocationId":"inv-1","author":"agent","actions":{"stateDelta":{},"artifactDelta":{},"#,
                    r#""requestedAuthConfigs":{"call-1":{"authScheme":{"type":"oauth2","flows":{"implicit":{"tokenUrl":"https://t"}}},"credentialKey":"k"}},"#,
                    r#""requestedToolConfirmations":{}},"nodeInfo":{"path":""},"id":"e2","timestamp":1700000000.0}"#
                ))
                .unwrap(),
            }),
        ),
    ];
    for (name, event) in cases {
        let (client, _server) = client_listing(json!([event]));
        let got = client
            .list_session_events("test-app", "session-1", None, 0)
            .await;
        assert!(
            got.is_err(),
            "{name}: listSessionEvents() succeeded, want an error for a requested auth config that does not decode"
        );
    }
}

// Writing an event whose request cannot be encoded or decoded back would either lose the
// request or make every later read of the session fail, so the append fails.
// parity: adapted: Go's "NaN in extra" case is dropped (an Extra value is raw JSON text, which cannot hold NaN); the events go to the fake's seeded session owned
#[tokio::test]
async fn test_vertex_ai_client_rejects_requested_auth_config_that_does_not_encode() {
    let api_key_scheme = || Scheme {
        r#type: SchemeType::API_KEY,
        r#in: "header".into(),
        name: "X-Key".into(),
        ..Default::default()
    };
    let cases = [
        (
            "zero scheme",
            Config {
                credential_key: "k".into(),
                ..Default::default()
            },
        ),
        (
            "oauth2 flow without URLs",
            Config {
                auth_scheme: Scheme {
                    r#type: SchemeType::OAUTH2,
                    flows: Some(OAuthFlows {
                        authorization_code: Some(OAuthFlow::default()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                ..Default::default()
            },
        ),
        (
            "credential without authType",
            Config {
                auth_scheme: api_key_scheme(),
                raw_auth_credential: Some(Credential {
                    api_key: "k".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ),
        // structpb keeps numbers as float64, and as a float64 this is 2^63, which does not fit
        // expiresAt on the way back.
        (
            "expiresAt that float64 rounds out of range",
            Config {
                auth_scheme: Scheme {
                    r#type: SchemeType::OAUTH2,
                    flows: Some(OAuthFlows {
                        client_credentials: Some(OAuthFlow {
                            token_url: "https://example.com/token".into(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                raw_auth_credential: Some(Credential {
                    auth_type: CredentialType::OAUTH2,
                    oauth2: Some(OAuth2Auth {
                        client_id: "cid".into(),
                        expires_at: i64::MAX,
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ),
        (
            "number in extra beyond float64",
            Config {
                auth_scheme: api_key_scheme(),
                extra: BTreeMap::from([(
                    "big".into(),
                    RawValue::from_string("1e400".into()).unwrap(),
                )]),
                ..Default::default()
            },
        ),
    ];
    for (name, config) in cases {
        let (client, _fake) = new_fake_vertex_ai_client();
        let mut event = Event::new(&Platform::default(), "invocation-1");
        event.author = "agent".to_owned();
        event.actions.requested_auth_configs = BTreeMap::from([("call-1".into(), config)]);

        assert!(
            client
                .append_event("test-app", "owned", &event)
                .await
                .is_err(),
            "{name}: appendEvent() succeeded, want an error"
        );
        let got = client
            .list_session_events("test-app", "owned", None, 0)
            .await
            .unwrap_or_else(|e| {
                panic!("{name}: listSessionEvents() after rejected appendEvent failed: {e}")
            });
        assert_eq!(got.len(), 0, "{name}: listSessionEvents() returned events");
    }
}

// The service half of the suite case that CASES_WITHOUT_REPLAY runs on the fake: a refused event
// reaches neither the backend nor the caller's session.
#[tokio::test]
async fn test_append_event_requested_auth_config_that_does_not_decode_leaves_session_untouched() {
    let fake = FakeAgentEngine::start();
    let svc = VertexAiSessionService::new(fake_client_config(&fake.url())).unwrap();
    let sess = VertexSession::new(
        "123".to_owned(),
        "user1".to_owned(),
        "owned".to_owned(),
        Map::new(),
        gadk_session::zero_time(),
    );
    let mut event = Event {
        id: "e1".into(),
        author: "agent".into(),
        invocation_id: "inv1".into(),
        ..Default::default()
    };
    event.actions.requested_auth_configs = BTreeMap::from([(
        "call-1".into(),
        Config {
            credential_key: "k".into(),
            ..Default::default()
        },
    )]);

    assert!(
        svc.append_event(&sess, &mut event).await.is_err(),
        "AppendEvent() succeeded, want an error for a request that does not decode"
    );
    assert_eq!(fake.hits("AppendEvent"), 0, "AppendEvent RPCs");
    assert_eq!(
        sess.events().len(),
        0,
        "the caller's session holds events after the refused append"
    );
}
