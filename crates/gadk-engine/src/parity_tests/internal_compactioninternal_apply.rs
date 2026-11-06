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

//! Port of ADK-Go `internal/compactioninternal/apply_test.go`.

use std::collections::{HashMap, HashSet};

use chrono::{Duration, TimeZone, Utc};
use gadk_genai::Content;
use gadk_session::{Event, EventActions, EventCompaction, EventRef};
use pretty_assertions::assert_eq;
use tokio_util::sync::CancellationToken;

use crate::compaction::apply::{
    CoverIndex, apply, excludes, has_compaction, has_usable_summary, in_range, known_event_ids,
    range_raced, range_raced_since, repair, repair_after_append,
};
use crate::compaction::test_support::{
    StaticSession, at, call_event, compaction_event, excl, ids, model_text_event, multi_call_event,
    response_event, strs, text_event,
};
use crate::compaction::window::{latest_compaction_event, select_sliding_window};
use crate::tool::utils::{content, text_parts};

fn contains(v: &[String], id: &str) -> bool {
    v.iter().any(|s| s == id)
}

#[test]
fn test_apply() {
    let contentless = Event {
        id: "s1".into(),
        timestamp: at(2),
        actions: EventActions {
            compaction: Some(EventCompaction {
                start_timestamp: at(1),
                end_timestamp: at(1),
                compacted_content: None,
                excluded_events: vec![],
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    let cases: Vec<(&str, Vec<Event>, Vec<String>)> = vec![
        (
            "no compaction events is a passthrough",
            vec![
                text_event("a", "inv1", 1, "hi"),
                model_text_event("b", "inv1", 2, "hello"),
            ],
            strs(&["a", "b"]),
        ),
        (
            "covered events are replaced by the summary",
            vec![
                text_event("a", "inv1", 1, "q1"),
                model_text_event("b", "inv1", 2, "a1"),
                text_event("c", "inv2", 3, "q2"),
                model_text_event("d", "inv2", 4, "a2"),
                compaction_event("s1", 5, 1, 4, "summary", vec![]),
                text_event("e", "inv3", 6, "q3"),
            ],
            strs(&["s1", "e"]),
        ),
        (
            "events after the range survive",
            vec![
                text_event("a", "inv1", 1, "q1"),
                model_text_event("b", "inv1", 2, "a1"),
                compaction_event("s1", 3, 1, 2, "summary", vec![]),
                text_event("c", "inv2", 4, "q2"),
                model_text_event("d", "inv2", 5, "a2"),
            ],
            strs(&["s1", "c", "d"]),
        ),
        (
            "an event predating the summary but outside its range survives",
            vec![
                text_event("a", "inv1", 1, "before the range"),
                text_event("b", "inv2", 3, "q2"),
                model_text_event("c", "inv2", 4, "a2"),
                compaction_event("s1", 5, 3, 4, "summary of inv2", vec![]),
            ],
            strs(&["a", "s1"]),
        ),
        (
            "a subsumed compaction is dropped along with its summary",
            vec![
                text_event("a", "inv1", 1, "q1"),
                model_text_event("b", "inv1", 2, "a1"),
                compaction_event("s1", 3, 1, 2, "narrow", vec![]),
                text_event("c", "inv2", 4, "q2"),
                model_text_event("d", "inv2", 5, "a2"),
                compaction_event("s2", 6, 1, 5, "wide", vec![]),
            ],
            strs(&["s2"]),
        ),
        (
            "partially overlapping compactions both survive",
            vec![
                text_event("a", "inv1", 1, "q1"),
                text_event("b", "inv2", 2, "q2"),
                compaction_event("s1", 3, 1, 2, "left", vec![]),
                text_event("c", "inv3", 4, "q3"),
                compaction_event("s2", 5, 2, 4, "right", vec![]),
            ],
            strs(&["s1", "s2"]),
        ),
        (
            "an event tying the end timestamp counts as covered",
            vec![
                text_event("a", "inv1", 1, "q1"),
                text_event("b", "inv1", 2, "also at 2"),
                compaction_event("s1", 3, 1, 2, "summary", vec![]),
            ],
            strs(&["s1"]),
        ),
        (
            "a compaction with no content is ignored entirely",
            vec![text_event("a", "inv1", 1, "q1"), contentless],
            strs(&["a"]),
        ),
    ];
    for (name, events, want) in cases {
        assert_eq!(ids(&apply(&events)), want, "{name}");
    }
}

#[test]
fn test_apply_materializes_summary_as_model_content() {
    let events = vec![
        text_event("a", "inv1", 1, "q1"),
        compaction_event("s1", 3, 1, 1, "the summary text", vec![]),
    ];
    let got = apply(&events);
    assert_eq!(got.len(), 1, "{:?}", ids(&got));
    let summary = &got[0];
    assert_eq!(summary.author, "model");
    assert_eq!(summary.timestamp, at(1));
    assert_eq!(text_parts(content(summary)), strs(&["the summary text"]));
}

#[test]
fn test_apply_does_not_mutate_input() {
    let stored = compaction_event("s1", 3, 1, 2, "summary", vec![]);
    let events = vec![text_event("a", "inv1", 1, "q1"), stored.clone()];

    apply(&events);

    // The stored event is what lives in the session; rewriting it in place would corrupt history.
    assert_eq!(events[1].author, "user");
    assert_eq!(events[1].timestamp, at(3));
    assert!(
        events[1].response.content.is_none(),
        "stored compaction Content = {:?}, want it left nil",
        events[1].response.content
    );
}

#[test]
fn test_apply_recovers_compacted_long_running_call() {
    let mut call = call_event("call", "inv1", 2, "c1");
    call.long_running_tool_ids = strs(&["c1"]);
    let placeholder = response_event("placeholder", "inv1", 3, "c1");
    let result = response_event("result", "inv2", 6, "c1");

    let events = vec![
        text_event("a", "inv1", 1, "please start the job"),
        call,
        placeholder,
        compaction_event("s1", 5, 1, 3, "summary", vec![]),
        result,
    ];
    assert_eq!(ids(&apply(&events)), strs(&["s1", "call", "result"]));
}

#[test]
fn test_apply_recovers_parallel_sibling_response() {
    let mut call = multi_call_event("call", "inv1", 2, &["c1", "c2"]);
    call.long_running_tool_ids = strs(&["c1", "c2"]);
    let events = vec![
        text_event("a", "inv1", 1, "start both"),
        call,
        response_event("ph1", "inv1", 3, "c1"),
        response_event("done2", "inv1", 4, "c2"),
        compaction_event("s1", 6, 1, 4, "summary", vec![]),
        response_event("done1", "inv2", 7, "c1"),
    ];
    assert_eq!(
        ids(&apply(&events)),
        strs(&["s1", "call", "done2", "done1"])
    );
}

#[test]
fn test_apply_leaves_non_long_running_orphan_alone() {
    let events = vec![
        text_event("a", "inv1", 1, "q1"),
        call_event("call", "inv1", 2, "c1"), // no long_running_tool_ids
        compaction_event("s1", 4, 1, 2, "summary", vec![]),
        response_event("result", "inv2", 5, "c1"),
    ];
    assert_eq!(ids(&apply(&events)), strs(&["s1", "result"]));
}

#[test]
fn test_apply_ignores_inverted_range() {
    let inverted = compaction_event("s1", 5, 4, 1, "bogus summary", vec![]);
    let events = vec![
        text_event("a", "inv1", 1, "q1"),
        model_text_event("b", "inv1", 2, "a1"),
        inverted,
    ];
    assert_eq!(ids(&apply(&events)), strs(&["a", "b"]));
}

#[test]
fn test_contentless_compaction_is_never_conversation() {
    let contentless = Event {
        id: "s1".into(),
        invocation_id: "e-compaction".into(),
        timestamp: at(5),
        actions: EventActions {
            compaction: Some(EventCompaction {
                start_timestamp: at(1),
                end_timestamp: at(4),
                compacted_content: None,
                excluded_events: vec![],
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(
        !has_usable_summary(&contentless),
        "HasUsableSummary() = true for a contentless compaction"
    );
    assert!(
        has_compaction(&contentless),
        "hasCompaction() = false for a contentless compaction"
    );

    let events = vec![
        text_event("a", "inv1", 1, "q1"),
        model_text_event("b", "inv1", 2, "a1"),
        text_event("c", "inv2", 3, "q2"),
        model_text_event("d", "inv2", 4, "a2"),
        contentless.clone(),
    ];
    assert!(
        select_sliding_window(&events, 3, 0).is_empty(),
        "only 2 real invocations exist, so the interval of 3 is unmet"
    );
    assert_eq!(latest_compaction_event(&events), Some(&contentless));

    let got = ids(&apply(&events));
    assert!(
        contains(&got, "a") && contains(&got, "d"),
        "Apply() = {got:?}, want the raw events kept"
    );
    let idx = CoverIndex::new(&events);
    for (i, ev) in events.iter().enumerate() {
        if has_compaction(ev) {
            continue;
        }
        assert!(
            !idx.covers_after(i, ev),
            "selection reports {:?} covered by a contentless record",
            ev.id
        );
    }
}

#[test]
fn test_apply_recovery_boundary() {
    let build = |long_running: bool| -> Vec<Event> {
        let mut call = call_event("call", "inv1", 2, "c1");
        if long_running {
            call.long_running_tool_ids = strs(&["c1"]);
        }
        vec![
            text_event("a", "inv1", 1, "start"),
            call,
            response_event("placeholder", "inv1", 3, "c1"),
            compaction_event("s1", 5, 1, 3, "summary", vec![]),
            response_event("result", "inv2", 6, "c1"),
        ]
    };
    let cases = [
        (
            "long-running call is restored so the response stays paired",
            true,
            strs(&["s1", "call", "result"]),
        ),
        (
            "non long-running call is not restored",
            false,
            strs(&["s1", "result"]),
        ),
    ];
    for (name, long_running, want) in cases {
        assert_eq!(ids(&apply(&build(long_running))), want, "{name}");
    }
}

#[test]
fn test_apply_equal_range_summaries_keep_coverage() {
    let events = vec![
        text_event("a", "inv1", 1, "TURN-ONE"),
        model_text_event("b", "inv1", 2, "TURN-TWO"),
        compaction_event("s1", 3, 1, 2, "SUM-1", vec![]),
        compaction_event("s2", 4, 1, 2, "SUM-2", vec![]),
        text_event("c", "inv2", 5, "TURN-FIVE"),
    ];
    let got = apply(&events);
    assert_eq!(ids(&got), strs(&["s2", "c"]));
    assert_eq!(text_parts(content(&got[0])), strs(&["SUM-2"]));
}

#[test]
fn test_apply_preserves_stream_order() {
    let call = call_event("call", "inv1", 9, "c1");
    let resp = response_event("resp", "inv1", 8, "c1"); // earlier timestamp than its call
    let events = vec![
        text_event("u", "inv1", 1, "q"),
        compaction_event("s1", 2, 1, 1, "SUM", vec![]),
        call,
        resp,
    ];
    assert_eq!(
        ids(&apply(&events)),
        strs(&["s1", "call", "resp"]),
        "the response must not precede its call"
    );
}

#[test]
fn test_apply_summary_precedes_uncovered_tail() {
    let events = vec![
        text_event("a", "inv1", 1, "q1"),
        model_text_event("b", "inv1", 2, "a1"),
        text_event("c", "inv2", 3, "q2"),
        model_text_event("d", "inv2", 4, "a2"),
        // Covers only the first exchange, but is appended after the second.
        compaction_event("s1", 5, 1, 2, "SUM", vec![]),
    ];
    assert_eq!(
        ids(&apply(&events)),
        strs(&["s1", "c", "d"]),
        "the summary must precede the turns it does not cover"
    );
}

#[test]
fn test_apply_contentless_record_does_not_evict_a_summary() {
    let real = compaction_event("s1", 3, 1, 2, "SUM", vec![]);
    // A wider, contentless record recorded afterwards.
    let mut blank = compaction_event("s2", 4, 1, 2, "", vec![]);
    if let Some(c) = blank.actions.compaction.as_mut() {
        c.compacted_content = None;
    }
    let events = vec![
        text_event("a", "inv1", 1, "q1"),
        model_text_event("b", "inv1", 2, "a1"),
        real,
        blank,
        text_event("c", "inv2", 5, "q2"),
    ];
    assert_eq!(
        ids(&apply(&events)),
        strs(&["s1", "c"]),
        "a contentless record must not destroy a summary already paid for"
    );
}

// parity: skipped TestApplyToleratesNilEvents: a Rust &[Event] cannot hold a nil event (Go-parity note 5)

#[test]
fn test_apply_keeps_an_event_the_summary_did_not_cover() {
    // The summary spans a..d but stands in only for a and d.
    let summary = compaction_event(
        "s1",
        9,
        1,
        4,
        "summary of a and d",
        vec![excl("inv1", 2), excl("inv1", 3)],
    );
    let events = vec![
        text_event("a", "inv1", 1, "q1"),
        text_event("b", "inv1", 2, "sibling branch"),
        text_event("c", "inv1", 3, "retained tail"),
        model_text_event("d", "inv1", 4, "a1"),
        summary,
    ];
    assert_eq!(ids(&apply(&events)), strs(&["s1", "b", "c"]));
}

#[test]
fn test_apply_keeps_an_event_tied_to_the_window_head() {
    let events = vec![
        text_event("x", "inv1", 1, "tied to the head, never summarized"),
        text_event("a", "inv1", 1, "q1"),
        model_text_event("b", "inv1", 3, "a1"),
        compaction_event("s1", 9, 1, 3, "summary of a and b", vec![excl("inv1", 1)]),
    ];
    assert_eq!(ids(&apply(&events)), strs(&["x", "a", "s1"]));
}

// parity: skipped TestUnwrapSessionStopsOnACycle: underlying_session unwraps only WrappedSession, whose inner Arc is fixed at construction, so no cycle can be built (Go-parity note 6)

#[test]
fn test_excludes_survives_a_backend_that_drops_precision() {
    let ns =
        Utc.with_ymd_and_hms(2026, 3, 4, 5, 6, 7).unwrap() + Duration::nanoseconds(123_456_789);
    let rng = EventCompaction {
        start_timestamp: ns - Duration::hours(1),
        end_timestamp: ns + Duration::hours(1),
        compacted_content: None,
        // Written at full precision, the way a record reaches storage.
        excluded_events: vec![EventRef {
            invocation_id: "inv1".into(),
            timestamp: ns,
        }],
    };
    // Read back from a store that keeps microseconds.
    let truncated =
        Utc.with_ymd_and_hms(2026, 3, 4, 5, 6, 7).unwrap() + Duration::microseconds(123_456);
    let ev = Event {
        id: "a".into(),
        invocation_id: "inv1".into(),
        timestamp: truncated,
        ..Default::default()
    };
    assert!(
        ev.timestamp >= rng.start_timestamp && ev.timestamp <= rng.end_timestamp,
        "the event is outside the interval, so this test proves nothing"
    );
    assert!(
        excludes(&rng, &ev),
        "the hole stopped matching after a round trip"
    );
    assert!(
        !in_range(&ev, &rng),
        "an event the record names as a hole is being treated as covered"
    );
    let past = Event {
        id: "b".into(),
        invocation_id: "inv1".into(),
        timestamp: rng.end_timestamp + Duration::nanoseconds(1),
        ..Default::default()
    };
    assert!(
        !in_range(&past, &rng),
        "an event after the range end is being treated as covered"
    );
}

#[test]
fn test_range_raced_since_sees_an_append_through_a_live_handle() {
    let sess = StaticSession::new(vec![
        text_event("a", "inv1", 1, "q1"),
        model_text_event("b", "inv1", 2, "a1"),
    ]);
    let summary = compaction_event("s1", 9, 1, 3, "summary of the turn", vec![]);

    let before = known_event_ids(&*sess);
    // A sibling appends inside the range while the summarizer is working.
    sess.push(text_event("straggler", "inv2", 3, "please do not lose me"));

    assert!(
        !range_raced(&*sess, &*sess, &summary),
        "RangeRaced() = true through one live handle, which it cannot actually detect"
    );
    assert!(
        range_raced_since(&*sess, &before, &summary),
        "RangeRacedSince() = false, want true: an event landed inside the range during the call"
    );
}

#[test]
fn test_range_raced_since_survives_a_backend_that_rewrites_event_i_ds() {
    let before = vec![
        text_event("client-a", "inv1", 1, "q1"),
        model_text_event("client-b", "inv1", 2, "a1"),
    ];
    let known = known_event_ids(&*StaticSession::new(before));

    // The same two events read back from a store that assigned its own IDs.
    let after = vec![
        text_event("server-a", "inv1", 1, "q1"),
        model_text_event("server-b", "inv1", 2, "a1"),
    ];
    let summary = compaction_event("s1", 9, 1, 2, "summary of the turn", vec![]);

    assert!(
        !range_raced_since(&*StaticSession::new(after.clone()), &known, &summary),
        "RangeRacedSince() = true: the backend renamed the events and they were mistaken for racers"
    );

    // A genuine racer is still caught.
    let mut with_racer = after;
    with_racer.push(text_event("server-c", "inv2", 2, "landed during the call"));
    assert!(
        range_raced_since(&*StaticSession::new(with_racer), &known, &summary),
        "RangeRacedSince() = false, want true: an event really did land inside the range"
    );
}

#[test]
fn test_repair_after_append_rescues_a_straggler_from_the_prompt() {
    let before = vec![
        text_event("a", "inv1", 1, "q1"),
        model_text_event("b", "inv1", 2, "a1"),
    ];
    let known = known_event_ids(&*StaticSession::new(before.clone()));
    let summary = compaction_event("s1", 9, 1, 4, "summary of the turn", vec![]);

    // A sibling's event, created at 3 (inside the range) and stored after the check but before
    // the summary.
    let straggler = text_event("straggler", "inv2", 3, "please do not lose me");
    let stored = vec![
        before[0].clone(),
        before[1].clone(),
        straggler,
        summary.clone(),
    ];

    let got = ids(&apply(&stored));
    assert!(
        !contains(&got, "straggler"),
        "prompt {got:?} already keeps the straggler, so this test is not reproducing the loss"
    );

    let mut repair_ev = repair_after_append(&summary, &known, &*StaticSession::new(stored.clone()))
        .expect("RepairAfterAppend() = nil, want a corrected record naming the straggler");
    repair_ev.id = "s2".into();
    // Deliberately not setting a timestamp: the correction places itself after the record.

    let mut all = stored;
    all.push(repair_ev);
    let got = ids(&apply(&all));
    assert!(
        contains(&got, "straggler"),
        "prompt {got:?} still drops the straggler after the repair"
    );
    assert!(
        !contains(&got, "s1"),
        "prompt {got:?} keeps the record the correction replaces"
    );
    assert!(
        contains(&got, "s2"),
        "prompt {got:?} lost the corrected summary"
    );

    // Nothing to repair when nothing landed.
    let untouched = vec![before[0].clone(), before[1].clone(), summary.clone()];
    let r = repair_after_append(&summary, &known, &*StaticSession::new(untouched));
    assert!(
        r.is_none(),
        "RepairAfterAppend() = {r:?}, want nil when no straggler landed"
    );
}

#[test]
fn test_two_corrections_keep_both_stragglers() {
    let stored = compaction_event("s1", 9, 1, 6, "summary", vec![]);
    let x = text_event("x", "invX", 3, "first straggler");
    let y = text_event("y", "invY", 4, "second straggler");
    let a = compaction_event("cA", 9, 1, 6, "summary", vec![excl("invX", 3)]);
    let b = compaction_event("cB", 9, 1, 6, "summary", vec![excl("invY", 4)]);

    let got = ids(&apply(&[x, y, stored, a, b]));
    for id in ["x", "y"] {
        assert!(
            contains(&got, id),
            "prompt {got:?} drops {id:?}, which no summary describes"
        );
    }
    // Exactly one of the three records over that range materializes.
    let n = ["s1", "cA", "cB"]
        .iter()
        .filter(|id| contains(&got, id))
        .count();
    assert_eq!(
        n, 1,
        "prompt {got:?} materializes {n} records over one range, want 1"
    );
}

// parity: adapted: Rust futures observe no context; apply::repair is never handed the invocation token and is bounded by REPAIR_TIMEOUT, so the test cancels a token, checks the repair still completes, and checks a wedged repair yields None after 30 s on the paused clock (Go-parity note 8)
#[tokio::test(start_paused = true)]
async fn test_repair_context_outlives_the_callers_cancellation() {
    let caller = CancellationToken::new();
    let carried = "carried".to_string();
    caller.cancel();
    // The caller is gone; the repair still runs to completion and keeps its captured values.
    let got = repair(async { carried.clone() }).await;
    assert_eq!(
        got.as_deref(),
        Some("carried"),
        "the repair stopped with its caller"
    );
    // A wedged backend is bounded by the 30 s repair deadline rather than held open for ever.
    let started = tokio::time::Instant::now();
    let wedged = repair(std::future::pending::<()>()).await;
    assert!(wedged.is_none(), "repair context carries no deadline");
    assert_eq!(started.elapsed(), std::time::Duration::from_secs(30));
}

#[test]
fn test_a_repair_does_not_cover_an_event_it_never_saw() {
    let e1 = text_event("e1", "inv1", 1, "q1");
    let e2 = model_text_event("e2", "inv1", 2, "a1");
    let x = text_event("x", "inv2", 3, "straggler the repair names");
    let stored = compaction_event("s1", 9, 1, 4, "summary", vec![]);
    // Created inside the range, stored after the repair re-read the session and before the
    // correction was appended, so no hole names it.
    let y = text_event("y", "inv2", 4, "landed in the gap");
    let correction = compaction_event("s2", 10, 1, 4, "summary", vec![excl("inv2", 3)]);

    let got = ids(&apply(&[e1, e2, x, stored, y, correction]));
    for id in ["x", "y"] {
        assert!(
            contains(&got, id),
            "prompt {got:?} drops {id:?}, which no summary describes"
        );
    }
    assert!(
        contains(&got, "s2"),
        "prompt {got:?} lost the corrected summary"
    );
    assert!(
        !contains(&got, "s1"),
        "prompt {got:?} shows both the record and its correction"
    );
}

/// splitmix64: a deterministic stand-in for Go's math/rand/v2 PCG in the property test.
struct SplitMix(u64);

impl SplitMix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Go `IntN(n)`: uniform in [0, n).
    fn int_n(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Port of Go randomSession (apply_test.go:957): events plus, per record ID, the IDs of the
/// window that record was built from.
fn random_session(rnd: &mut SplitMix) -> (Vec<Event>, HashMap<String, HashSet<String>>) {
    let mut events: Vec<Event> = Vec::new();
    let mut summarized: HashMap<String, HashSet<String>> = HashMap::new();
    let mut ts: i64 = 1;
    // A raw event may not land inside a range that already exists (the append-window case
    // RepairAfterAppend handles on the write side).
    let mut max_record_end: Option<chrono::DateTime<Utc>> = None;
    let raw_at = |ts: &mut i64, max_end: Option<chrono::DateTime<Utc>>| -> i64 {
        while let Some(end) = max_end
            && at(*ts) <= end
        {
            *ts += 1;
        }
        *ts
    };
    let n = 5 + rnd.int_n(14);
    for i in 0..n {
        match rnd.int_n(6) {
            0 => {
                // A tool pair, sometimes sharing a timestamp.
                let id = format!("c{i}");
                let t = raw_at(&mut ts, max_record_end);
                events.push(call_event(&format!("call{i}"), &format!("inv{i}"), t, &id));
                if rnd.int_n(2) == 0 {
                    ts += 1;
                }
                let t = raw_at(&mut ts, max_record_end);
                events.push(response_event(
                    &format!("resp{i}"),
                    &format!("inv{i}"),
                    t,
                    &id,
                ));
            }
            1 | 2 => {
                // Compact a window of what exists so far.
                let raw: Vec<Event> = events
                    .iter()
                    .filter(|e| !has_compaction(e))
                    .cloned()
                    .collect();
                if raw.len() >= 2 {
                    let lo = rnd.int_n(raw.len() - 1);
                    let hi = lo + 1 + rnd.int_n(raw.len() - lo - 1);
                    let mut window: HashSet<String> = HashSet::new();
                    let mut chosen: Vec<&Event> = Vec::new();
                    for (j, ev) in raw.iter().enumerate().take(hi + 1).skip(lo) {
                        // Sometimes skip one inside the span, which is what holes record.
                        if j != lo && j != hi && rnd.int_n(4) == 0 {
                            continue;
                        }
                        window.insert(ev.id.clone());
                        chosen.push(ev);
                    }
                    let (start, end) = (chosen[0].timestamp, chosen[chosen.len() - 1].timestamp);
                    let holes: Vec<EventRef> = raw
                        .iter()
                        .filter(|ev| {
                            !window.contains(&ev.id) && ev.timestamp >= start && ev.timestamp <= end
                        })
                        .map(|ev| EventRef {
                            invocation_id: ev.invocation_id.clone(),
                            timestamp: ev.timestamp,
                        })
                        .collect();
                    let rec_id = format!("s{i}");
                    events.push(Event {
                        id: rec_id.clone(),
                        invocation_id: format!("sum{rec_id}"),
                        timestamp: at(ts),
                        author: "user".into(),
                        actions: EventActions {
                            compaction: Some(EventCompaction {
                                start_timestamp: start,
                                end_timestamp: end,
                                compacted_content: Some(Content::from_text(
                                    format!("summary {rec_id}"),
                                    "model",
                                )),
                                excluded_events: holes,
                            }),
                            ..Default::default()
                        },
                        ..Default::default()
                    });
                    summarized.insert(rec_id, window);
                    if max_record_end.is_none_or(|m| end > m) {
                        max_record_end = Some(end);
                    }
                }
            }
            3 => {
                // A correction: the same range again, naming one more hole.
                let recs: Vec<Event> = events
                    .iter()
                    .filter(|e| has_compaction(e))
                    .cloned()
                    .collect();
                if !recs.is_empty() {
                    let src = recs[rnd.int_n(recs.len())].clone();
                    let Some(src_rng) = src.actions.compaction.clone() else {
                        unreachable!()
                    };
                    let src_window = summarized.get(&src.id).cloned().unwrap_or_default();
                    let straggler = events
                        .iter()
                        .find(|ev| {
                            !has_compaction(ev)
                                && !src_window.contains(&ev.id)
                                && in_range(ev, &src_rng)
                        })
                        .cloned();
                    if let Some(straggler) = straggler {
                        let rec_id = format!("fix{i}");
                        let mut fixed = src_rng.clone();
                        fixed.excluded_events.push(EventRef {
                            invocation_id: straggler.invocation_id.clone(),
                            timestamp: straggler.timestamp,
                        });
                        events.push(Event {
                            id: rec_id.clone(),
                            invocation_id: format!("sum{rec_id}"),
                            timestamp: src.timestamp,
                            author: "user".into(),
                            actions: EventActions {
                                compaction: Some(fixed),
                                ..Default::default()
                            },
                            ..Default::default()
                        });
                        summarized.insert(rec_id, src_window);
                    }
                }
            }
            _ => {
                let t = raw_at(&mut ts, max_record_end);
                events.push(text_event(&format!("e{i}"), &format!("inv{i}"), t, "text"));
            }
        }
        // Go's `break` inside `switch` leaves only the switch, so this still runs.
        if rnd.int_n(3) != 0 {
            ts += 1;
        }
    }
    (events, summarized)
}

// parity: adapted: Go's math/rand/v2 PCG(1, 2) stream is not reproduced; a splitmix64 generator seeded 1 drives the same randomSession generator over the same 3000 trials
#[test]
fn test_no_event_is_dropped_without_having_been_summarized() {
    let mut rnd = SplitMix(1);
    for trial in 0..3000 {
        let (events, summarized) = random_session(&mut rnd);
        let out = apply(&events);
        let kept: HashSet<&str> = out.iter().map(|e| e.id.as_str()).collect();
        let survived: HashSet<&str> = out
            .iter()
            .filter(|e| has_compaction(e))
            .map(|e| e.id.as_str())
            .collect();
        for ev in &events {
            if has_compaction(ev) || kept.contains(ev.id.as_str()) {
                continue;
            }
            let described = summarized
                .iter()
                .any(|(rec, window)| survived.contains(rec.as_str()) && window.contains(&ev.id));
            assert!(
                described,
                "trial {trial}: event {:?} was dropped from the prompt and no surviving summary was built from it\nsession: {:?}\nprompt:  {:?}",
                ev.id,
                ids(&events),
                ids(&out)
            );
        }
    }
}

#[test]
fn test_a_rescued_straggler_can_still_be_summarized() {
    let before = vec![
        text_event("a", "inv1", 1, "q1"),
        model_text_event("b", "inv1", 2, "a1"),
    ];
    let known = known_event_ids(&*StaticSession::new(before.clone()));
    let stored = compaction_event("s1", 9, 1, 4, "summary", vec![]);
    let straggler = text_event("straggler", "inv2", 3, "please summarize me eventually");

    let mut all = vec![
        before[0].clone(),
        before[1].clone(),
        straggler.clone(),
        stored.clone(),
    ];
    let mut repair_ev = repair_after_append(&stored, &known, &*StaticSession::new(all.clone()))
        .expect("RepairAfterAppend() = nil, want a correction naming the straggler");
    repair_ev.id = "s2".into();
    all.push(repair_ev);

    // Assembly keeps it raw.
    assert!(
        contains(&ids(&apply(&all)), "straggler"),
        "the straggler is not in the prompt, so this test is not reproducing the case"
    );

    // Selection must therefore still offer it: the record that hides it is one assembly
    // discards.
    assert!(
        !CoverIndex::new(&all).covers_after(2, &straggler),
        "selection reads the straggler as covered, so no future window can ever summarize it"
    );
}

#[test]
fn test_selection_skips_a_subsumed_record_its_holes_do_not_cover() {
    let e1 = text_event("e1", "inv1", 1, "q1");
    let x = text_event("x", "inv2", 2, "the event only one record excludes");
    let e3 = text_event("e3", "inv3", 3, "q3");
    // Same range, different summaries, so they are not corrections of each other.
    let earlier = compaction_event("s1", 9, 1, 3, "summary A", vec![]);
    let later = compaction_event("s2", 10, 1, 3, "summary B", vec![excl("inv2", 2)]);

    let events = vec![e1, x, e3, earlier, later];

    let got = ids(&apply(&events));
    assert!(
        contains(&got, "x"),
        "prompt {got:?} drops x, which the surviving record excludes"
    );

    let idx = CoverIndex::new(&events);
    for (i, ev) in events.iter().enumerate() {
        if has_compaction(ev) || ev.id != "x" {
            continue;
        }
        assert!(
            !idx.covers_after(i, ev),
            "selection reports x covered by the record that was subsumed, while assembly keeps it raw"
        );
    }
}
