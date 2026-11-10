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

use gadk_session::{Event, EventActions, KEY_PREFIX_TEMP, Session, SessionService};
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
