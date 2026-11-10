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

//! Ports of ADK-Go `session/vertexai/service_test.go`.

use std::sync::Arc;

use bytes::Bytes;
use chrono::Utc;
use gadk_model::{BodyStream, HttpClient, ModelError, ReqwestClient};
use gadk_session::state_util::trim_temp_delta_state;
use gadk_session::testsuite::{SuiteOptions, run_named};
use gadk_session::{CreateRequest, Event, SessionError, SessionService, zero_time};
use parking_lot::Mutex;
use serde_json::{Map, json};

use crate::session::{VertexAiSessionService, VertexSession};
use crate::testutil::{
    ENGINE_ID, ENGINE_ID2, FakeAgentEngine, ReplayServer, fixture_stem, session_config,
};

/// Go `UserID`.
const USER_ID: &str = "test-user";

/// The shared-suite cases Go skips for lack of a matching recording (Go `casesWithoutReplay`);
/// they run on [`FakeAgentEngine`] instead.
const CASES_WITHOUT_REPLAY: [&str; 2] = [
    // Pinned offline in Go by TestAppendEvent_missingSession_wrapsErrNotFound.
    "AppendEvent/when_session_deleted_returns_ErrNotFound",
    // Go's recording predates trimming temp: keys before AppendEvent.
    "StateManagement/temp_state_is_not_persisted",
];

fn service(url: &str) -> Arc<dyn SessionService> {
    Arc::new(VertexAiSessionService::new(session_config(url)).unwrap())
}

// parity: adapted: each case replays Go's recording of it on a strict 127.0.0.1 REST fake (ReplayServer); the two cases Go skips (casesWithoutReplay) run on FakeAgentEngine
#[tokio::test]
async fn test_vertexai_service() {
    let replays: Arc<Mutex<Vec<(&'static str, ReplayServer)>>> = Arc::default();
    let engines: Arc<Mutex<Vec<FakeAgentEngine>>> = Arc::default();
    let opts = SuiteOptions {
        supports_user_provided_session_id: true,
        provides_server_assigned_event_id: true,
        app_name: Some(ENGINE_ID.to_owned()),
    };
    run_named(opts, |name| {
        let replays = replays.clone();
        let engines = engines.clone();
        async move {
            if CASES_WITHOUT_REPLAY.contains(&name) {
                let engine = FakeAgentEngine::start();
                let svc = service(&engine.url());
                engines.lock().push(engine);
                svc
            } else {
                let replay = ReplayServer::start(&fixture_stem(name));
                let svc = service(&replay.url());
                replays.lock().push((name, replay));
                svc
            }
        }
    })
    .await;
    let replays = replays.lock();
    assert_eq!(replays.len(), 22, "replayed cases");
    assert_eq!(engines.lock().len(), 2, "cases on FakeAgentEngine");
    let unmatched: Vec<String> = replays
        .iter()
        .flat_map(|(name, replay)| {
            replay
                .unmatched()
                .into_iter()
                .map(move |u| format!("{name}: {u}"))
        })
        .collect();
    assert!(
        unmatched.is_empty(),
        "unmatched requests:\n{}",
        unmatched.join("\n")
    );
}

/// A local session as Go's tests build one (`&localSession{…}`).
fn local(app_name: &str, user_id: &str, session_id: &str) -> VertexSession {
    VertexSession::new(
        app_name.to_owned(),
        user_id.to_owned(),
        session_id.to_owned(),
        Map::new(),
        zero_time(),
    )
}

// parity: adapted: nil_event is N/A (append_event takes &mut Event, which cannot be null); missing_author and missing_invocation_id run on FakeAgentEngine, which answers them as the real engine does (400 INVALID_ARGUMENT), instead of on recordings
#[tokio::test]
async fn test_vertexai_service_append_event_structural_validation() {
    struct Case {
        name: &'static str,
        session: VertexSession,
        event: Event,
        /// Whether the request reaches the backend (Go's `offline: false`).
        online: bool,
        want: &'static str,
    }
    let cases = [
        Case {
            name: "missing_session_id",
            session: local(ENGINE_ID, USER_ID, ""),
            event: Event::default(),
            online: false,
            want: "invalid request: session_id and event are required, got session_id: \"\", \
                   event_id: false",
        },
        // nil_event: N/A, an `&mut Event` cannot be null.
        Case {
            name: "missing_author",
            session: local(ENGINE_ID2, "user2", "session2"),
            event: Event {
                timestamp: Utc::now(),
                invocation_id: "4b1f2c3d-uuid".to_owned(),
                ..Default::default()
            },
            online: true,
            want: "invalid request: failed to append event: error appending event: rpc error: \
                   code = InvalidArgument desc = Request contains an invalid argument.",
        },
        Case {
            name: "missing_invocation_id",
            session: local(ENGINE_ID2, "user2", "session2"),
            event: Event {
                timestamp: Utc::now(),
                author: USER_ID.to_owned(),
                ..Default::default()
            },
            online: true,
            want: "invalid request: failed to append event: error appending event: rpc error: \
                   code = InvalidArgument desc = Request contains an invalid argument.",
        },
    ];
    for mut case in cases {
        let fake = FakeAgentEngine::start();
        let svc = service(&fake.url());
        let err = svc
            .append_event(&case.session, &mut case.event)
            .await
            .expect_err(case.name);
        assert!(
            matches!(err, SessionError::InvalidRequest(_)),
            "{}: {err:?}",
            case.name
        );
        assert_eq!(err.to_string(), case.want, "{}", case.name);
        assert_eq!(
            fake.hits("AppendEvent"),
            usize::from(case.online),
            "{}: AppendEvent RPCs",
            case.name
        );
    }
}

#[test]
fn test_trim_temp_delta_state_preserves_input_event() {
    let event = Event {
        id: "event1".to_owned(),
        actions: gadk_session::EventActions {
            state_delta: Some(
                json!({"temp:k1": "v1", "sk": "v2"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
            ..Default::default()
        },
        ..Default::default()
    };

    let trimmed = trim_temp_delta_state(&event);

    let input = event.actions.state_delta.as_ref().unwrap();
    assert!(
        input.contains_key("temp:k1"),
        "expected temp:k1 to be preserved on input event, but it was removed: {input:?}"
    );
    let out = trimmed.actions.state_delta.as_ref().unwrap();
    assert!(
        !out.contains_key("temp:k1"),
        "expected temp:k1 to be stripped from trimmed event copy, but it still exists: {out:?}"
    );
    assert_eq!(
        out.get("sk"),
        Some(&json!("v2")),
        "expected non-temp key sk on trimmed event, got: {out:?}"
    );
}

/// An [`HttpClient`] that records every request URI before sending it with reqwest.
#[derive(Default)]
struct RecordingClient {
    uris: Mutex<Vec<http::Uri>>,
    inner: ReqwestClient,
}

#[async_trait::async_trait]
impl HttpClient for RecordingClient {
    async fn send(
        &self,
        req: http::Request<Bytes>,
    ) -> Result<http::Response<BodyStream>, ModelError> {
        self.uris.lock().push(req.uri().clone());
        self.inner.send(req).await
    }
}

// parity: adapted: Go checks that a replay-backed gRPC client never dials the network; the REST client opens no connection until a call, so the port runs the Create_full_key conversation through a replay-backed service and checks that every request went to the 127.0.0.1 replay server and matched a recording
#[tokio::test]
async fn test_setup_replay_initializes_without_external_network() {
    let replay = ReplayServer::start("Test_vertexaiService_Create_full_key");
    let http = Arc::new(RecordingClient::default());
    let mut config = session_config(&replay.url());
    config.reasoning_engine = Some(ENGINE_ID.to_owned());
    config.http_client = Some(http.clone());
    let svc = VertexAiSessionService::new(config)
        .expect("replay client must initialize without a network connection");
    // Initializing sends nothing.
    assert!(http.uris.lock().is_empty(), "requests sent by new");

    svc.create(CreateRequest {
        app_name: ENGINE_ID.to_owned(),
        user_id: "testUserID".to_owned(),
        session_id: Some("test-session-id".to_owned()),
        state: Some(json!({"k": 5}).as_object().unwrap().clone()),
    })
    .await
    .unwrap();

    let origin: http::Uri = replay.url().parse().unwrap();
    let uris = http.uris.lock();
    assert_eq!(uris.len(), 2, "CreateSession and GetSession: {uris:?}");
    for uri in uris.iter() {
        assert_eq!(uri.host(), Some("127.0.0.1"), "{uri}");
        assert_eq!(uri.authority(), origin.authority(), "{uri}");
    }
    assert_eq!(replay.requests().len(), uris.len());
    assert!(replay.unmatched().is_empty(), "{:?}", replay.unmatched());
}
