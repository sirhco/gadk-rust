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

//! Ports of ADK-Go `session/vertexai/append_event_temp_state_test.go`.

use gadk_session::{CreateRequest, Event, EventActions, KEY_PREFIX_TEMP, Session, SessionService};
use serde_json::{Map, Value, json};

use crate::session::{VertexAiSessionService, VertexAiSessionServiceConfig, VertexSession};
use crate::testutil::{FakeAgentEngine, session_config};

/// The config of Go's `newFakeVertexAiClient` (`vertexai_test.go`): project `test-project`,
/// location `us-central1`, reasoning engine `123`, no authentication, on a fake at `url`.
pub(super) fn fake_client_config(url: &str) -> VertexAiSessionServiceConfig {
    VertexAiSessionServiceConfig {
        project_id: "test-project".to_owned(),
        location: "us-central1".to_owned(),
        reasoning_engine: Some("123".to_owned()),
        ..session_config(url)
    }
}

/// The fake's stored-event name of session `owned` under Go's `newFakeVertexAiClient` engine.
const OWNED: &str =
    "projects/test-project/locations/us-central1/reasoningEngines/123/sessions/owned";

fn setup() -> (VertexAiSessionService, FakeAgentEngine, VertexSession) {
    let fake = FakeAgentEngine::start();
    let svc = VertexAiSessionService::new(fake_client_config(&fake.url())).unwrap();
    let sess = VertexSession::new(
        "123".to_owned(),
        "user1".to_owned(),
        "owned".to_owned(),
        Map::new(),
        gadk_session::zero_time(),
    );
    (svc, fake, sess)
}

fn temp_event(id: &str) -> Event {
    Event {
        id: id.to_owned(),
        author: "user".to_owned(),
        invocation_id: "inv1".to_owned(),
        actions: EventActions {
            state_delta: json!({"temp:k1": "v1", "sk": "v2"}).as_object().cloned(),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// The caller's event keeps its temp key (AppendEvent must not mutate the input).
fn assert_input_untouched(event: &Event) {
    let delta = event.actions.state_delta.as_ref().unwrap();
    assert!(
        delta.contains_key("temp:k1"),
        "caller event lost temp:k1; AppendEvent must not mutate the input: {delta:?}"
    );
    assert_eq!(
        delta.get("sk"),
        Some(&json!("v2")),
        "caller event lost sk: {delta:?}"
    );
}

/// The local session still sees the untrimmed delta.
fn assert_local_state(sess: &VertexSession) {
    assert!(
        sess.state().get("temp:k1").is_ok(),
        "local session state missing temp:k1 after AppendEvent"
    );
    assert_eq!(
        sess.state().get("sk").unwrap(),
        json!("v2"),
        "local session state sk"
    );
}

fn assert_persisted_delta_has_no_temp(delta: &Map<String, Value>, place: &str) {
    for k in delta.keys() {
        assert!(
            !k.starts_with(KEY_PREFIX_TEMP),
            "temp key reached {place}: key {k:?}, delta {delta:?}"
        );
    }
    assert_eq!(
        delta.get("sk"),
        Some(&json!("v2")),
        "non-temp key missing from {place}: got {delta:?}, want sk=v2"
    );
}

// TestAppendEvent_stripsTempKeysBeforePersist pins the contract offline: temp: keys are absent
// from the AppendEvent payload, while the caller's event and the local session still see them.
#[tokio::test]
async fn test_append_event_strips_temp_keys_before_persist() {
    let (svc, fake, sess) = setup();
    let mut event = temp_event("event1");

    svc.append_event(&sess, &mut event).await.unwrap();

    assert_input_untouched(&event);
    let events = fake.events(OWNED);
    assert_eq!(events.len(), 1, "AppendEvent RPCs stored events");
    let delta = events[0]
        .pointer("/actions/stateDelta")
        .and_then(Value::as_object)
        .expect("persisted event actions.stateDelta = nil, want non-temp keys retained");
    assert_persisted_delta_has_no_temp(delta, "AppendEvent RPC payload");
    assert_local_state(&sess);
}

// TestAppendEvent_stripsTempKeysFromRawEvent pins the other stored copy of the delta: the
// rawEvent blob, which Get prefers.
#[tokio::test]
async fn test_append_event_strips_temp_keys_from_raw_event() {
    let (svc, fake, sess) = setup();
    let mut event = temp_event("event2");
    event.output = Some(json!({"node": "out"})); // forces rawEvent

    svc.append_event(&sess, &mut event).await.unwrap();

    assert_input_untouched(&event);
    let events = fake.events(OWNED);
    assert_eq!(events.len(), 1, "AppendEvent RPCs stored events");
    let column = events[0]
        .pointer("/actions/stateDelta")
        .and_then(Value::as_object)
        .expect("persisted event actions.stateDelta = nil, want non-temp keys retained");
    assert_persisted_delta_has_no_temp(column, "actions.stateDelta");

    let raw = events[0].get("rawEvent").expect(
        "rawEvent is nil; Output must populate it so this test can see the blob copy of StateDelta",
    );
    let raw_actions = raw
        .get("actions")
        .filter(|a| a.is_object())
        .unwrap_or_else(|| panic!("rawEvent actions = {:?}, want map", raw.get("actions")));
    let raw_delta = raw_actions
        .get("stateDelta")
        .and_then(Value::as_object)
        .unwrap_or_else(|| {
            panic!(
                "rawEvent actions.stateDelta = {:?}, want map",
                raw_actions.get("stateDelta")
            )
        });
    assert_persisted_delta_has_no_temp(raw_delta, "rawEvent");
    assert_local_state(&sess);
}

// TestCreate_dropsTempKeysFromInitialState pins that temp: keys in CreateRequest.State never reach
// Agent Engine, so they are absent from the created session and every later read of it.
// AppendEvent already trims them from state deltas; adk-python drops them in create_session too.
// parity: adapted: the CreateSession payload is read from the fake's recorded request body (Go's fake stores the RPC's session); the caller's map is checked on the clone it kept
#[ignore = "8a bug: SessionClient::create_session sends CreateRequest.state with its temp: keys as sessionState, so they reach Agent Engine and the created session (Go #2100 0c109f7d)"]
#[tokio::test]
async fn test_create_drops_temp_keys_from_initial_state() {
    let cases: [(&str, Value, Option<Value>); 2] = [
        (
            "temp and session keys",
            json!({"temp:k1": "v1", "sk": "v2"}),
            Some(json!({"sk": "v2"})),
        ),
        // None: no session_state sent at all.
        ("only temp keys", json!({"temp:k1": "v1"}), None),
    ];
    for (name, state, want_state) in cases {
        let fake = FakeAgentEngine::start();
        let svc = VertexAiSessionService::new(fake_client_config(&fake.url())).unwrap();
        let state = state.as_object().cloned().unwrap();
        let request_state = state.clone();

        let created = svc
            .create(CreateRequest {
                app_name: "123".into(),
                user_id: "user1".into(),
                session_id: Some("s1".into()),
                state: Some(request_state),
            })
            .await
            .unwrap_or_else(|e| panic!("{name}: Create() error = {e}"));

        assert!(
            state.contains_key("temp:k1"),
            "{name}: Create removed temp:k1 from the caller's map; it must not mutate the request"
        );

        let create_rpc = fake
            .requests()
            .into_iter()
            .find(|r| r.method == "POST" && r.path.ends_with("/sessions"))
            .unwrap_or_else(|| panic!("{name}: no CreateSession request"));
        let sent = create_rpc.json().get("sessionState").cloned();
        match &want_state {
            None => assert!(
                sent.is_none(),
                "{name}: CreateSession RPC session_state = {sent:?}, want none"
            ),
            Some(want) => {
                let got = sent.as_ref().and_then(Value::as_object).unwrap_or_else(|| {
                    panic!("{name}: CreateSession RPC session_state = {sent:?}")
                });
                for k in got.keys() {
                    assert!(
                        !k.starts_with(KEY_PREFIX_TEMP),
                        "{name}: temp key reached the CreateSession RPC payload: key {k:?}, state {got:?}"
                    );
                }
                for (k, w) in want.as_object().unwrap() {
                    assert_eq!(
                        got.get(k),
                        Some(w),
                        "{name}: CreateSession RPC session_state[{k:?}]"
                    );
                }
            }
        }

        assert!(
            created.state().get("temp:k1").is_err(),
            "{name}: created session State().Get(\"temp:k1\") found the key, want ErrStateKeyNotExist"
        );
    }
}
