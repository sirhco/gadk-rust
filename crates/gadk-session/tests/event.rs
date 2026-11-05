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

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Port of ADK-Go `session/event_test.go`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use chrono::{DateTime, TimeZone, Utc};
use gadk_platform::Platform;
use gadk_session::Event;
use pretty_assertions::assert_eq;

fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// The default platform uses the wall clock and a random UUID.
#[test]
fn test_new_event_defaults() {
    let before = Utc::now();
    let ev = Event::new(&Platform::default(), "inv-1");
    let after = Utc::now();

    assert_eq!(ev.invocation_id, "inv-1");
    assert!(is_uuid(&ev.id), "ID = {:?}, not a valid UUID", ev.id);
    assert!(
        ev.timestamp >= before && ev.timestamp <= after,
        "Timestamp = {}, want within [{before}, {after}]",
        ev.timestamp
    );
}

#[test]
fn test_new_event_uses_providers() {
    let fixed_time = Utc.with_ymd_and_hms(2024, 1, 2, 3, 4, 5).unwrap();
    let p = Platform::default()
        .with_time(move || fixed_time)
        .with_uuid(|| "fixed-event-id".to_string());

    let ev = Event::new(&p, "inv-1");

    assert_eq!(ev.id, "fixed-event-id");
    assert_eq!(ev.timestamp, fixed_time);
    assert_eq!(ev.invocation_id, "inv-1");
}

/// Replaying the same sequence of provider values produces identical events.
#[test]
fn test_new_event_deterministic_replay() {
    let new_platform = || {
        let ids = Arc::new(AtomicU32::new(0));
        let times = Arc::new(AtomicU32::new(0));
        Platform::default()
            .with_uuid(move || format!("event-{}", ids.fetch_add(1, Ordering::SeqCst) + 1))
            .with_time(move || -> DateTime<Utc> {
                let n = times.fetch_add(1, Ordering::SeqCst) + 1;
                Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, n).unwrap()
            })
    };
    let run = |p: &Platform| vec![Event::new(p, "inv"), Event::new(p, "inv")];

    let first = run(&new_platform());
    let second = run(&new_platform());

    for (i, (a, b)) in first.iter().zip(&second).enumerate() {
        assert_eq!(a.id, b.id, "event {i} ID");
        assert_eq!(a.timestamp, b.timestamp, "event {i} Timestamp");
    }
}
