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

use gadk_session::{Event, Session, zero_time};
use serde_json::Map;

use crate::session::VertexSession;

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
