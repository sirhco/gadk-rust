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

//! Port of ADK-Go `internal/compactioninternal/summary_event_test.go`.

use chrono::{DateTime, TimeZone, Utc};
use gadk_genai::{Content, FunctionCall, Part};
use gadk_session::{Event, EventRef};
use pretty_assertions::assert_eq;

use crate::compaction::apply::apply;
use crate::compaction::summary_event::new_summary_event;
use crate::compaction::test_support::{at, ids, model_text_event, text_event};

/// Go `time.Unix(n, 0)`.
fn unix(n: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(n, 0).unwrap()
}

/// Go `&session.Event{Timestamp: time.Unix(n, 0)}`.
fn bare(n: i64) -> Event {
    Event {
        timestamp: unix(n),
        ..Default::default()
    }
}

#[test]
fn test_new_summary_event() {
    let events = vec![
        text_event("a", "inv1", 3, "q1"),
        model_text_event("b", "inv1", 7, "a1"),
    ];
    let summary_content = Content {
        role: "user".into(),
        parts: vec![Part {
            text: "the summary".into(),
            thought_signature: b"sig".to_vec().into(),
            ..Default::default()
        }],
    };

    let got = new_summary_event(&events, &events, &summary_content, None).unwrap();

    assert_eq!(got.author, "user");
    let c = got
        .actions
        .compaction
        .as_ref()
        .expect("Actions.Compaction is nil, want a compaction range");
    assert_eq!(c.start_timestamp, at(3));
    assert_eq!(c.end_timestamp, at(7));
    let stored = c.compacted_content.as_ref().unwrap();
    assert_eq!(stored.role, "model");
    assert!(
        stored.parts[0].thought_signature.is_empty(),
        "CompactedContent kept a thought signature, want it stripped"
    );
    // The summarizer's own content must not be rewritten underneath it.
    assert_eq!(summary_content.role, "user");
    assert_eq!(&summary_content.parts[0].thought_signature[..], b"sig");
}

// parity: adapted: Go's "nil summary" case passes an empty Content, because the summary is a &Content in Rust; both fail the prose check
#[test]
fn test_new_summary_event_rejects_bad_input() {
    let ordered = vec![
        text_event("a", "inv1", 1, "q1"),
        model_text_event("b", "inv1", 4, "a1"),
    ];
    let content = Content::from_text("summary", "model");
    let empty = Content::default();
    let out_of_order = vec![
        model_text_event("b", "inv1", 4, "a1"),
        text_event("a", "inv1", 1, "q1"),
    ];
    let cases: Vec<(&str, &[Event], &Content, bool)> = vec![
        ("ok", &ordered, &content, false),
        (
            "single event is a valid degenerate range",
            &ordered[..1],
            &content,
            false,
        ),
        ("no events", &[], &content, true),
        ("nil summary", &ordered, &empty, true),
        (
            "events out of chronological order",
            &out_of_order,
            &content,
            false,
        ),
    ];
    for (name, events, summary, want_err) in cases {
        let got = new_summary_event(events, events, summary, None);
        assert_eq!(
            got.is_err(),
            want_err,
            "{name}: newSummaryEvent() = {got:?}"
        );
    }
}

#[test]
fn test_new_summary_event_drops_the_thought_signature() {
    let events = vec![bare(1), bare(2)];
    let summary = Content {
        role: "model".into(),
        parts: vec![Part {
            text: "the summary".into(),
            thought_signature: b"opaque-signature".to_vec().into(),
            ..Default::default()
        }],
    };

    let got = new_summary_event(&events, &events, &summary, None).unwrap();
    let parts = &got
        .actions
        .compaction
        .as_ref()
        .unwrap()
        .compacted_content
        .as_ref()
        .unwrap()
        .parts;
    assert_eq!(parts.len(), 1);
    assert!(
        parts[0].thought_signature.is_empty(),
        "ThoughtSignature not dropped"
    );
    assert_eq!(parts[0].text, "the summary");
    assert!(
        !summary.parts[0].thought_signature.is_empty(),
        "the signature was cleared on the caller's content, not just on the copy"
    );
}

#[test]
fn test_new_summary_event_rejects_proseless_summary() {
    let events = vec![bare(1), bare(2)];
    let summary = Content {
        role: "model".into(),
        parts: vec![Part {
            text: "transferring now".into(),
            function_call: Some(FunctionCall {
                name: "transfer_funds".into(),
                ..Default::default()
            }),
            ..Default::default()
        }],
    };
    assert!(
        new_summary_event(&events, &events, &summary, None).is_err(),
        "newSummaryEvent() accepted a summary with no prose"
    );
}

#[test]
fn test_compaction_event_is_not_a_final_response() {
    let events = vec![bare(1), bare(2)];
    let got = new_summary_event(
        &events,
        &events,
        &Content::from_text("the summary", "model"),
        None,
    )
    .unwrap();
    assert!(
        !got.is_final_response(),
        "a compaction event reports IsFinalResponse() = true"
    );
}

#[test]
fn test_new_summary_event_bounds_an_out_of_order_window() {
    let events = vec![
        Event {
            id: "a".into(),
            ..bare(1)
        },
        Event {
            id: "b".into(),
            ..bare(9)
        },
        Event {
            id: "c".into(),
            ..bare(5)
        },
    ];
    let got = new_summary_event(&events, &events, &Content::from_text("s", "model"), None).unwrap();
    let c = got.actions.compaction.unwrap();
    assert_eq!((c.start_timestamp, c.end_timestamp), (unix(1), unix(9)));
    assert!(c.excluded_events.is_empty(), "the window has no holes");
}

#[test]
fn test_new_summary_event_rejects_thought_only_summary() {
    let events = vec![bare(1), bare(2)];
    let summary = Content {
        role: "model".into(),
        parts: vec![Part {
            text: "thinking about it".into(),
            thought: true,
            ..Default::default()
        }],
    };
    assert!(new_summary_event(&events, &events, &summary, None).is_err());
}

#[test]
fn test_new_summary_event_drops_thoughts_from_a_mixed_summary() {
    let events = vec![bare(1), bare(2)];
    let summary = Content {
        role: "model".into(),
        parts: vec![
            Part {
                text: "the user asked about the weather".into(),
                thought: true,
                ..Default::default()
            },
            Part::from_text("The user asked about the weather in Zurich."),
        ],
    };
    let got = new_summary_event(&events, &events, &summary, None).unwrap();
    let stored = got
        .actions
        .compaction
        .unwrap()
        .compacted_content
        .unwrap()
        .parts;
    assert_eq!(stored.len(), 1, "the thought must be dropped");
    assert!(!stored[0].thought);
    assert_eq!(
        stored[0].text,
        "The user asked about the weather in Zurich."
    );
}

#[test]
fn test_new_summary_event_records_a_hole_that_collides_with_the_window() {
    let in_window = text_event("a", "inv1", 1, "summarized");
    let collides = text_event(
        "x",
        "inv1",
        1,
        "never summarized, same invocation and timestamp",
    );
    let tail = model_text_event("b", "inv1", 3, "a1");

    let window = vec![in_window.clone(), tail.clone()];
    let all = vec![collides, in_window, tail];

    let mut summary =
        new_summary_event(&window, &all, &Content::from_text("summary", "model"), None).unwrap();
    let rec = summary.actions.compaction.as_ref().unwrap();
    assert_eq!(rec.excluded_events.len(), 1, "{:?}", rec.excluded_events);
    assert_eq!(
        rec.excluded_events[0],
        EventRef {
            invocation_id: "inv1".into(),
            timestamp: at(1)
        }
    );

    // End to end: the event that was never summarized survives into the prompt.
    summary.id = "s1".into();
    summary.timestamp = at(4);
    let mut with_summary = all.clone();
    with_summary.push(summary);
    let got = ids(&apply(&with_summary));
    assert!(got.contains(&"x".to_string()), "prompt = {got:?}");
}
