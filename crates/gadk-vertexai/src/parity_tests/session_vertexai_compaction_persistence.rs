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

//! Ports of ADK-Go `session/vertexai/compaction_persistence_test.go`.
//!
//! A context-compaction summary carries its content only on `actions.compaction`: its content
//! is empty and it has no state or artifact delta, so `rawEvent` is the only slot that can
//! hold it, and `event_needs_raw_event` decides whether `rawEvent` is written at all.

use chrono::{DateTime, Utc};
use gadk_genai::Content;
use gadk_session::{Event, EventActions, EventCompaction};

use crate::session::convert::{event_from_raw_event, event_needs_raw_event, event_to_raw_event};

fn unix(secs: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(secs, 0).unwrap()
}

fn compaction_event() -> Event {
    Event {
        id: "summary".into(),
        author: "user".into(),
        invocation_id: "inv-compaction".into(),
        actions: EventActions {
            compaction: Some(EventCompaction {
                start_timestamp: unix(1),
                end_timestamp: unix(5),
                compacted_content: Some(Content::from_text("summary of earlier turns", "model")),
                excluded_events: Vec::new(),
            }),
            ..Default::default()
        },
        ..Default::default()
    }
}

#[test]
fn test_event_needs_raw_event_for_compaction() {
    assert!(
        event_needs_raw_event(&compaction_event()),
        "event_needs_raw_event() = false for a compaction summary, so it would be written nowhere and lost on reload"
    );

    // An ordinary event must still stay on the legacy wire format, so existing replay
    // recordings remain valid.
    let plain = Event {
        id: "plain".into(),
        author: "user".into(),
        invocation_id: "inv1".into(),
        ..Default::default()
    };
    assert!(
        !event_needs_raw_event(&plain),
        "event_needs_raw_event() = true for a plain event, which would change the wire format for unrelated events"
    );
}

#[test]
fn test_compaction_round_trips_through_raw_event() {
    let want = compaction_event();
    let raw = event_to_raw_event(&want).unwrap();
    let got = event_from_raw_event(&raw).unwrap();

    let c = got
        .actions
        .compaction
        .expect("Actions.Compaction did not survive the raw_event round trip");
    assert!(
        c.start_timestamp == unix(1) && c.end_timestamp == unix(5),
        "range = [{}, {}], want [{}, {}]",
        c.start_timestamp,
        c.end_timestamp,
        unix(1),
        unix(5)
    );
    let content = c
        .compacted_content
        .filter(|c| !c.parts.is_empty())
        .expect("compacted content did not survive the round trip");
    assert_eq!(
        content.parts[0].text, "summary of earlier turns",
        "compacted content"
    );
}
