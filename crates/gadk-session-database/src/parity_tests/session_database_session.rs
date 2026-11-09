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

//! Port of ADK-Go session/database/session_test.go.

use std::sync::Arc;

use chrono::Utc;
use gadk_session::{Event, Session};
use serde_json::Map;

use crate::session::DatabaseSession;

fn ev(id: &str) -> Event {
    Event {
        id: id.to_string(),
        ..Default::default()
    }
}

fn session(events: Vec<Event>) -> DatabaseSession {
    DatabaseSession::new(
        "app".into(),
        "user".into(),
        "s".into(),
        Map::new(),
        events,
        Utc::now(),
    )
}

#[test]
fn test_local_session_events_snapshot() {
    let s = session(vec![ev("first")]);
    let snapshot = s.events();

    // Replacing a slot must not change a previously returned history snapshot.
    s.replace_event(0, ev("replacement"));
    s.append_event(&ev("second"));
    assert_eq!(snapshot.len(), 1, "snapshot.Len()");
    assert_eq!(snapshot[0].id, "first", "snapshot.At(0)");
    for event in &snapshot {
        assert_eq!(event.id, "first", "snapshot.All() yielded {}", event.id);
    }
}

#[test]
fn test_local_session_events_concurrent_append() {
    const COUNT: usize = 1000;
    let s = Arc::new(session(Vec::new()));
    let writer = {
        let s = s.clone();
        std::thread::spawn(move || {
            for i in 0..COUNT {
                s.append_event(&ev(&i.to_string()));
                std::thread::yield_now();
            }
        })
    };
    for _ in 0..COUNT {
        let snapshot = s.events();
        for (n, event) in snapshot.iter().enumerate() {
            assert_eq!(event.id, n.to_string(), "snapshot event {n}");
        }
        std::thread::yield_now();
    }
    writer.join().unwrap();
    assert_eq!(s.events().len(), COUNT, "final event count");
}
