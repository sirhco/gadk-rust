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

//! Ports of ADK-Go `session/vertexai/session_test.go`.

use gadk_model::rest::test_util::FakeServer;
use gadk_session::{Event, ListRequest, Session, SessionService, zero_time};
use serde_json::{Map, json};

use crate::session::{VertexAiSessionService, VertexSession};

fn event(id: &str) -> Event {
    Event {
        id: id.into(),
        ..Default::default()
    }
}

fn empty_session() -> VertexSession {
    VertexSession::new(
        String::new(),
        String::new(),
        String::new(),
        Map::new(),
        zero_time(),
    )
}

// parity: adapted: Go replaces a slot of the live slice under the session lock; gadk replaces the stored events with set_events
#[test]
fn test_local_session_events_snapshot() {
    let first = event("first");
    let s = empty_session();
    s.set_events(vec![first.clone()]);
    let snapshot = s.events();

    // Replacing a slot must not change a previously returned history snapshot.
    s.set_events(vec![event("replacement")]);
    s.append_event(&event("second")).unwrap();
    assert_eq!(snapshot.len(), 1, "snapshot.len()");
    assert_eq!(snapshot[0], first, "snapshot[0] is not the original event");
    for e in &snapshot {
        assert_eq!(
            *e, first,
            "snapshot iteration yielded {e:?}, want original event"
        );
    }
}

#[test]
fn test_local_session_events_concurrent_append() {
    const COUNT: usize = 1000;
    let s = empty_session();
    let start = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            start.wait();
            for i in 0..COUNT {
                s.append_event(&event(&i.to_string())).unwrap();
                std::thread::yield_now();
            }
        });
        start.wait();
        for _ in 0..COUNT {
            let snapshot = s.events();
            let mut n = 0;
            for e in &snapshot {
                assert_eq!(e.id, n.to_string(), "snapshot event {n}");
                n += 1;
            }
            assert_eq!(n, snapshot.len(), "snapshot iteration count");
            std::thread::yield_now();
        }
    });
    assert_eq!(s.events().len(), COUNT, "final event count");
}

// parity: adapted: gadk has no sortSessionsByLastUpdate; a fake ListSessions answers the sessions in Go's input order and the test checks the order List returns
#[tokio::test]
async fn test_sort_sessions_by_last_update() {
    let engine = "projects/test-project/locations/us-central1/reasoningEngines/123";
    // At time 0, u2/a ties with u1/a and u1/b. Its session ID sorts before u1/b's but its user ID
    // after, so only user ID then session ID gives the want.
    let sessions: Vec<_> = [
        ("u2", "x", "2026-01-01T00:01:00Z"),
        ("u2", "a", "2026-01-01T00:00:00Z"),
        ("u1", "b", "2026-01-01T00:00:00Z"),
        ("u1", "c", "2026-01-01T01:00:00Z"),
        ("u1", "a", "2026-01-01T00:00:00Z"),
    ]
    .into_iter()
    .map(|(user, id, at)| {
        json!({"name": format!("{engine}/sessions/{id}"), "userId": user, "updateTime": at})
    })
    .collect();
    let server = FakeServer::json(json!({ "sessions": sessions }).to_string());
    let svc = VertexAiSessionService::new(
        super::session_vertexai_append_event_temp_state::fake_client_config(&server.url()),
    )
    .unwrap();

    let got: Vec<String> = svc
        .list(ListRequest {
            app_name: "123".into(),
            user_id: String::new(),
        })
        .await
        .unwrap()
        .iter()
        .map(|s| format!("{}/{}", s.user_id(), s.id()))
        .collect();
    assert_eq!(
        got,
        ["u1/a", "u1/b", "u2/a", "u2/x", "u1/c"],
        "sortSessionsByLastUpdate()"
    );
}
