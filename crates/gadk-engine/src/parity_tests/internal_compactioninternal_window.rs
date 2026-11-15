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

//! Port of ADK-Go `internal/compactioninternal/window_test.go`.

use std::collections::HashSet;

use chrono::{Duration, FixedOffset, TimeZone, Utc};
use gadk_genai::{Content, FunctionCall, Part};
use gadk_session::compaction::Config;
use gadk_session::{Event, EventCompaction, EventRef, ToolConfirmation};
use pretty_assertions::assert_eq;

use crate::compaction::apply::{apply, excludes, has_usable_summary, in_range};
use crate::compaction::summary_event::new_summary_event;
use crate::compaction::test_support::{
    at, call_and_response_event, call_event, compaction_event, confirmation_event, excl, ids,
    model_text_event, multi_call_event, new_event, response_event, strs, text_event,
    with_auth_request,
};
use crate::compaction::window::{
    covers_all_of, latest_compaction_event, longest_self_contained_prefix, select_sliding_window,
};
use crate::compaction::{has_sliding_window, has_tail_retention};
use crate::tool::utils::{content, function_calls, function_responses};

/// Go assertWindowCoversItsRange: every non-record event inside [window[0], window[last]] is in
/// the window (membership by ID; the fixtures' IDs are unique).
fn assert_window_covers_its_range(all: &[Event], window: &[Event]) {
    let (Some(first), Some(last)) = (window.first(), window.last()) else {
        return;
    };
    let (start, end) = (first.timestamp, last.timestamp);
    let in_window: HashSet<&str> = window.iter().map(|e| e.id.as_str()).collect();
    for ev in all {
        if ev.actions.compaction.is_some() || in_window.contains(ev.id.as_str()) {
            continue;
        }
        assert!(
            ev.timestamp < start || ev.timestamp > end,
            "event {:?} at {} lies inside the summarized range [{start}, {end}] but was not summarized",
            ev.id,
            ev.timestamp
        );
    }
}

#[test]
fn test_longest_self_contained_prefix() {
    let cases: Vec<(&str, Vec<Event>, Vec<String>)> = vec![
        ("empty", vec![], vec![]),
        (
            "plain text events are all self contained",
            vec![
                text_event("a", "inv1", 1, "hi"),
                text_event("b", "inv1", 2, "hello"),
            ],
            strs(&["a", "b"]),
        ),
        (
            "call and response in range",
            vec![
                text_event("a", "inv1", 1, "hi"),
                call_event("b", "inv1", 2, "c1"),
                response_event("c", "inv1", 3, "c1"),
            ],
            strs(&["a", "b", "c"]),
        ),
        (
            "dangling call truncates the prefix",
            vec![
                text_event("a", "inv1", 1, "hi"),
                call_event("b", "inv1", 2, "c1"),
            ],
            strs(&["a"]),
        ),
        (
            "trailing events after a dangling call are also dropped",
            vec![
                text_event("a", "inv1", 1, "hi"),
                call_event("b", "inv1", 2, "c1"),
                text_event("c", "inv1", 3, "still thinking"),
            ],
            strs(&["a"]),
        ),
        (
            "parallel calls need every response",
            vec![
                multi_call_event("a", "inv1", 1, &["c1", "c2"]),
                response_event("b", "inv1", 2, "c1"),
                response_event("c", "inv1", 3, "c2"),
            ],
            strs(&["a", "b", "c"]),
        ),
        (
            "parallel calls missing one response",
            vec![
                text_event("z", "inv1", 1, "hi"),
                multi_call_event("a", "inv1", 2, &["c1", "c2"]),
                response_event("b", "inv1", 3, "c1"),
            ],
            strs(&["z"]),
        ),
        (
            "unresolved tool confirmation blocks the prefix",
            vec![
                text_event("a", "inv1", 1, "hi"),
                confirmation_event("b", "inv1", 2, "c1"),
            ],
            strs(&["a"]),
        ),
        (
            "resolved tool confirmation is fine",
            vec![
                text_event("a", "inv1", 1, "hi"),
                confirmation_event("b", "inv1", 2, "c1"),
                response_event("c", "inv1", 3, "c1"),
            ],
            strs(&["a", "b", "c"]),
        ),
        (
            // The request rides on the tool's own response, which must not close the call the
            // request is about.
            "unresolved credential request blocks the prefix",
            vec![
                text_event("a", "inv1", 1, "hi"),
                call_event("b", "inv1", 2, "c1"),
                with_auth_request(response_event("c", "inv1", 3, "c1"), "c1"),
            ],
            strs(&["a"]),
        ),
        (
            "credential request resolved by the rerun is fine",
            vec![
                text_event("a", "inv1", 1, "hi"),
                call_event("b", "inv1", 2, "c1"),
                with_auth_request(response_event("c", "inv1", 3, "c1"), "c1"),
                response_event("d", "inv1", 4, "c1"),
            ],
            strs(&["a", "b", "c", "d"]),
        ),
        (
            "response within the same event as its call still opens the obligation",
            vec![call_and_response_event("a", "inv1", 1, "c1")],
            vec![],
        ),
    ];
    for (name, events, want) in cases {
        assert_eq!(ids(&longest_self_contained_prefix(&events)), want, "{name}");
    }
}

#[test]
fn test_select_sliding_window() {
    let base4 = || {
        vec![
            text_event("a", "inv1", 1, "q1"),
            text_event("b", "inv1", 2, "a1"),
            text_event("c", "inv2", 3, "q2"),
            text_event("d", "inv2", 4, "a2"),
        ]
    };
    let with_s1 = |more: Vec<Event>| {
        let mut v = base4();
        v.push(compaction_event("s1", 5, 1, 4, "summary of 1-2", vec![]));
        v.extend(more);
        v
    };
    let inv3_inv4 = || {
        vec![
            text_event("e", "inv3", 6, "q3"),
            text_event("f", "inv3", 7, "a3"),
            text_event("g", "inv4", 8, "q4"),
            text_event("h", "inv4", 9, "a4"),
        ]
    };
    // (name, events, interval, overlap, want)
    type Case = (&'static str, Vec<Event>, i32, i32, Vec<String>);
    let cases: Vec<Case> = vec![
        (
            "interval not reached",
            vec![
                text_event("a", "inv1", 1, "hi"),
                text_event("b", "inv1", 2, "hello"),
            ],
            2,
            0,
            vec![],
        ),
        (
            "first compaction covers both invocations",
            base4(),
            2,
            0,
            strs(&["a", "b", "c", "d"]),
        ),
        (
            "interval zero disables selection",
            vec![
                text_event("a", "inv1", 1, "q1"),
                text_event("b", "inv2", 2, "q2"),
            ],
            0,
            0,
            vec![],
        ),
        (
            "only one new invocation since the last compaction",
            with_s1(vec![
                text_event("e", "inv3", 6, "q3"),
                text_event("f", "inv3", 7, "a3"),
            ]),
            2,
            1,
            vec![],
        ),
        (
            "second compaction pulls one invocation back via overlap",
            with_s1(inv3_inv4()),
            2,
            1,
            strs(&["c", "d", "e", "f", "g", "h"]),
        ),
        (
            "zero overlap starts after the previous compaction",
            with_s1(inv3_inv4()),
            2,
            0,
            strs(&["e", "f", "g", "h"]),
        ),
        (
            "window is trimmed so an open call is never summarized alone",
            vec![
                text_event("a", "inv1", 1, "q1"),
                text_event("b", "inv1", 2, "a1"),
                text_event("c", "inv2", 3, "q2"),
                call_event("d", "inv2", 4, "c1"),
            ],
            2,
            0,
            strs(&["a", "b", "c"]),
        ),
        (
            "nil when the whole window is one open call",
            vec![
                call_event("a", "inv1", 1, "c1"),
                call_event("b", "inv2", 2, "c2"),
            ],
            2,
            0,
            vec![],
        ),
        (
            "events without an invocation ID are ignored",
            vec![
                text_event("a", "", 1, "orphan"),
                text_event("b", "inv1", 2, "q1"),
                text_event("c", "inv2", 3, "q2"),
            ],
            2,
            0,
            strs(&["b", "c"]),
        ),
    ];
    for (name, events, interval, overlap, want) in cases {
        assert_eq!(
            ids(&select_sliding_window(&events, interval, overlap)),
            want,
            "{name} (interval={interval}, overlap={overlap})"
        );
    }
}

#[test]
fn test_latest_compaction_event() {
    let cases: Vec<(&str, Vec<Event>, &str)> = vec![
        ("no compactions", vec![text_event("a", "inv1", 1, "hi")], ""),
        (
            "single compaction",
            vec![compaction_event("s1", 5, 1, 4, "sum", vec![])],
            "s1",
        ),
        (
            "wider compaction wins over the narrower one it contains",
            vec![
                compaction_event("s1", 5, 1, 4, "narrow", vec![]),
                compaction_event("s2", 9, 1, 8, "wide", vec![]),
            ],
            "s2",
        ),
        (
            "a later narrower compaction survives an earlier wider one",
            vec![
                compaction_event("s1", 9, 1, 8, "wide", vec![]),
                compaction_event("s2", 10, 3, 6, "narrow", vec![]),
            ],
            "s2",
        ),
        (
            "identical ranges keep the later event",
            vec![
                compaction_event("s1", 5, 1, 4, "first", vec![]),
                compaction_event("s2", 6, 1, 4, "second", vec![]),
            ],
            "s2",
        ),
        (
            "partially overlapping compactions both survive, latest wins",
            vec![
                compaction_event("s1", 5, 1, 4, "left", vec![]),
                compaction_event("s2", 9, 3, 8, "right", vec![]),
            ],
            "s2",
        ),
    ];
    for (name, events, want) in cases {
        let got = latest_compaction_event(&events).map_or("", |e| e.id.as_str());
        assert_eq!(got, want, "{name}: LatestCompactionEvent()");
    }
}

// parity: adapted: Go's nil *Config is Option::None, which Rust validates by not calling validate
#[test]
fn test_config_validate() {
    let none: Option<Config> = None;
    assert!(
        none.map_or(Ok(()), |c| c.validate()).is_ok(),
        "nil is valid"
    );
    let cfg = |ci: i32, os: i32, tt: i32, ers: i32| Config {
        compaction_interval: ci,
        overlap_size: os,
        token_threshold: tt,
        event_retention_size: ers,
        summarizer: None,
    };
    let cases = [
        ("empty but non-nil is a mistake", cfg(0, 0, 0, 0), true),
        ("sliding window", cfg(3, 1, 0, 0), false),
        ("sliding window with zero overlap", cfg(3, 0, 0, 0), false),
        ("tail retention", cfg(0, 0, 1000, 5), false),
        ("both strategies", cfg(3, 1, 1000, 5), false),
        ("negative interval", cfg(-1, 0, 0, 0), true),
        ("negative overlap", cfg(1, -1, 0, 0), true),
        ("negative token threshold", cfg(0, 0, -1, 0), true),
        ("negative retention size", cfg(0, 0, 1, -1), true),
        ("overlap without interval", cfg(0, 2, 10, 0), true),
        ("retention without threshold", cfg(1, 0, 0, 2), true),
    ];
    for (name, c, want_err) in cases {
        assert_eq!(
            c.validate().is_err(),
            want_err,
            "{name}: {:?}",
            c.validate()
        );
    }
}

#[test]
fn test_has_sliding_window() {
    assert!(
        !has_sliding_window(None),
        "a nil Config must report sliding window disabled"
    );
    assert!(
        has_sliding_window(Some(&Config {
            compaction_interval: 2,
            ..Default::default()
        })),
        "HasSlidingWindow() = false, want true when CompactionInterval > 0"
    );
    assert!(
        !has_sliding_window(Some(&Config {
            token_threshold: 10,
            ..Default::default()
        })),
        "HasSlidingWindow() = true, want false when CompactionInterval is 0"
    );
}

#[test]
fn test_has_tail_retention() {
    assert!(
        !has_tail_retention(None),
        "a nil Config must report tail retention disabled"
    );
    assert!(
        has_tail_retention(Some(&Config {
            token_threshold: 10,
            ..Default::default()
        })),
        "HasTailRetention() = false, want true when TokenThreshold > 0"
    );
    assert!(
        !has_tail_retention(Some(&Config {
            compaction_interval: 2,
            ..Default::default()
        })),
        "HasTailRetention() = true, want false when TokenThreshold is 0"
    );
}

// parity: adapted: the Go nil-event case has no Rust input (Go-parity note 5)
#[test]
fn test_has_usable_summary() {
    let contentless = Event {
        id: "s1".into(),
        actions: gadk_session::EventActions {
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
    let cases = [
        ("plain event", text_event("a", "inv1", 1, "hi"), false),
        (
            "compaction",
            compaction_event("s1", 5, 1, 4, "sum", vec![]),
            true,
        ),
        (
            "compaction with no content is not usable",
            contentless,
            false,
        ),
    ];
    for (name, ev, want) in cases {
        assert_eq!(has_usable_summary(&ev), want, "{name}: HasUsableSummary()");
    }
}

#[test]
fn test_confirmation_event_opens_obligation() {
    let ev = confirmation_event("b", "inv1", 2, "c1");
    assert!(
        ev.actions.requested_tool_confirmations.contains_key("c1"),
        "confirmationEvent() produced no RequestedToolConfirmations entry, got {:?}",
        ev.actions.requested_tool_confirmations
    );
    let _: &ToolConfirmation = &ev.actions.requested_tool_confirmations["c1"];
}

#[test]
fn test_select_sliding_window_covers_everything_in_its_range() {
    let cases: Vec<(&str, Vec<Event>)> = vec![
        (
            "event with no invocation ID sits between two invocations",
            vec![
                text_event("a", "inv1", 1, "q1"),
                model_text_event("b", "inv1", 2, "a1"),
                text_event("orphan", "", 3, "side note"),
                text_event("c", "inv2", 4, "q2"),
                model_text_event("d", "inv2", 5, "a2"),
            ],
        ),
        (
            "several ID-less events interleaved",
            vec![
                text_event("x", "", 1, "before"),
                text_event("a", "inv1", 2, "q1"),
                text_event("y", "", 3, "middle"),
                model_text_event("b", "inv1", 4, "a1"),
                text_event("c", "inv2", 5, "q2"),
                text_event("z", "", 6, "later"),
                model_text_event("d", "inv2", 7, "a2"),
            ],
        ),
        (
            "trim boundary lands on a timestamp tie",
            vec![
                text_event("a", "inv1", 1, "q1"),
                model_text_event("b", "inv1", 2, "a1"),
                text_event("c", "inv2", 3, "q2"),
                model_text_event("d", "inv2", 4, "a2"),
                call_event("e", "inv2", 4, "c1"),
                model_text_event("f", "inv2", 4, "trailing"),
            ],
        ),
        (
            "overlap reaches back across an ID-less event",
            vec![
                text_event("a", "inv1", 1, "q1"),
                text_event("orphan", "", 2, "side note"),
                text_event("b", "inv2", 3, "q2"),
                compaction_event("s1", 4, 1, 3, "earlier summary", vec![]),
                text_event("c", "inv3", 5, "q3"),
                text_event("d", "inv4", 6, "q4"),
            ],
        ),
    ];
    for (name, events) in cases {
        for overlap in [0, 1, 2] {
            let window = select_sliding_window(&events, 2, overlap);
            assert!(
                !window.is_empty(),
                "{name}: selectSlidingWindow(overlap={overlap}) = empty, want a window over two complete invocations"
            );
            assert_window_covers_its_range(&events, &window);
        }
    }
}

#[test]
fn test_select_sliding_window_includes_i_dless_events() {
    let events = vec![
        text_event("a", "inv1", 1, "q1"),
        model_text_event("b", "inv1", 2, "a1"),
        text_event("orphan", "", 3, "side note"),
        text_event("c", "inv2", 4, "q2"),
        model_text_event("d", "inv2", 5, "a2"),
    ];
    assert_eq!(
        ids(&select_sliding_window(&events, 2, 0)),
        strs(&["a", "b", "orphan", "c", "d"])
    );
}

#[test]
fn test_select_sliding_window_survives_blocked_head() {
    let cases: Vec<(&str, i32, Vec<Event>, Vec<String>)> = vec![
        (
            "unanswered call at the head is stepped over",
            3,
            vec![
                call_event("stuck", "inv1", 1, "c1"),
                text_event("a", "inv2", 2, "q2"),
                model_text_event("b", "inv2", 3, "a2"),
                text_event("c", "inv3", 4, "q3"),
                model_text_event("d", "inv3", 5, "a3"),
            ],
            strs(&["a", "b", "c", "d"]),
        ),
        (
            "unanswered confirmation at the head is stepped over",
            3,
            vec![
                confirmation_event("stuck", "inv1", 1, "c1"),
                text_event("a", "inv2", 2, "q2"),
                text_event("b", "inv3", 3, "q3"),
            ],
            strs(&["a", "b"]),
        ),
        (
            "unanswered credential request at the head is stepped over",
            3,
            vec![
                with_auth_request(new_event("stuck", "inv1", 1, "model", vec![]), "c1"),
                text_event("a", "inv2", 2, "q2"),
                text_event("b", "inv3", 3, "q3"),
            ],
            strs(&["a", "b"]),
        ),
        (
            // c0 never answers, so only stepping over the head can compact here, and the rerun
            // that answers c1 must stay with its request.
            "a tail answering a credential request left in the head is refused",
            3,
            vec![
                multi_call_event("calls", "inv1", 1, &["c0", "c1"]),
                with_auth_request(response_event("ask", "inv1", 2, "c1"), "c1"),
                text_event("a", "inv2", 3, "q2"),
                response_event("rerun", "inv2", 4, "c1"),
                text_event("b", "inv3", 5, "q3"),
            ],
            strs(&["b"]),
        ),
        (
            "still nil when nothing after the blockage is self-contained",
            2,
            vec![
                call_event("stuck1", "inv1", 1, "c1"),
                call_event("stuck2", "inv2", 2, "c2"),
            ],
            vec![],
        ),
        (
            "a resolvable call is trimmed normally, not stepped over",
            2,
            vec![
                text_event("a", "inv1", 1, "q1"),
                model_text_event("b", "inv1", 2, "a1"),
                text_event("c", "inv2", 3, "q2"),
                call_event("pending", "inv2", 4, "c1"),
            ],
            strs(&["a", "b", "c"]),
        ),
    ];
    for (name, interval, events, want) in cases {
        let window = select_sliding_window(&events, interval, 0);
        assert_eq!(ids(&window), want, "{name}: selectSlidingWindow()");
        assert_window_covers_its_range(&events, &window);
    }
}

#[test]
fn test_longest_self_contained_prefix_i_dless_call() {
    let events = vec![
        text_event("a", "inv1", 1, "q1"),
        new_event(
            "idless",
            "inv1",
            2,
            "model",
            vec![Part {
                function_call: Some(FunctionCall {
                    name: "tool_without_id".into(),
                    ..Default::default()
                }),
                ..Default::default()
            }],
        ),
        response_event("resp", "inv1", 3, ""),
        model_text_event("d", "inv1", 4, "done"),
    ];
    assert_eq!(ids(&longest_self_contained_prefix(&events)), strs(&["a"]));
}

#[test]
fn test_select_sliding_window_is_bounded_by_interval() {
    let events: Vec<Event> = (0..10)
        .map(|i| text_event(&format!("q{i}"), &format!("inv{i}"), i + 1, "q"))
        .collect();
    assert_eq!(
        ids(&select_sliding_window(&events, 3, 0)),
        strs(&["q0", "q1", "q2"]),
        "the window must not run to the end of the session"
    );
}

#[test]
fn test_select_sliding_window_retry_does_not_grow() {
    let mut events: Vec<Event> = (0..3)
        .map(|i| text_event(&format!("q{i}"), &format!("inv{i}"), i + 1, "q"))
        .collect();
    let first = select_sliding_window(&events, 2, 0);

    // The attempt failed, so nothing was recorded. Two more turns arrive.
    for i in 3..5 {
        events.push(text_event(&format!("q{i}"), &format!("inv{i}"), i + 1, "q"));
    }
    let retry = select_sliding_window(&events, 2, 0);

    assert_eq!(
        retry.len(),
        first.len(),
        "retry window has {} events, want the same {} as the attempt that failed: {:?} then {:?}",
        retry.len(),
        first.len(),
        ids(&first),
        ids(&retry)
    );
    assert_eq!(ids(&first), ids(&retry), "retry window mismatch");
}

#[test]
fn test_sliding_window_makes_progress_across_a_branch_change() {
    let branched = |id: &str, inv: &str, ts: i64, branch: &str, text: &str| Event {
        branch: branch.into(),
        ..text_event(id, inv, ts, text)
    };
    let mut all = vec![
        text_event("a", "inv1", 1, "q1"),
        model_text_event("b", "inv1", 2, "a1"),
        text_event("c", "inv2", 3, "q2"),
        branched("d", "inv2", 4, "child", "sub-agent work"),
        branched("e", "inv2", 5, "child", "more sub-agent work"),
    ];
    let mut chosen: Vec<Vec<String>> = Vec::new();
    for pass in 1..=4 {
        let w = select_sliding_window(&all, 1, 0);
        if w.is_empty() {
            break;
        }
        chosen.push(ids(&w));
        let mut summary = new_summary_event(&w, &w, &Content::from_text("summary", "model"), None)
            .unwrap_or_else(|e| panic!("pass {pass}: newSummaryEvent() error = {e}"));
        summary.id = format!("s{pass}");
        summary.invocation_id = format!("e-compaction-{pass}");
        summary.timestamp = at(10 + pass);
        all.push(summary);
    }
    assert_eq!(
        chosen,
        vec![strs(&["a", "b"]), strs(&["c"]), strs(&["d", "e"])],
        "windows chosen across passes"
    );
}

#[test]
fn test_sliding_window_recovers_from_a_blocked_invocation() {
    let mut all = vec![
        call_event("blocked", "inv1", 1, "c-pending"),
        text_event("q2", "inv2", 2, "q2"),
        model_text_event("a2", "inv2", 3, "a2"),
        text_event("q3", "inv3", 4, "q3"),
        model_text_event("a3", "inv3", 5, "a3"),
    ];
    let mut chosen: Vec<Vec<String>> = Vec::new();
    for pass in 1..=4 {
        let w = select_sliding_window(&all, 2, 0);
        if w.is_empty() {
            break;
        }
        chosen.push(ids(&w));
        let mut summary =
            new_summary_event(&w, &all, &Content::from_text("summary", "model"), None)
                .unwrap_or_else(|e| panic!("pass {pass}: newSummaryEvent() error = {e}"));
        summary.id = format!("s{pass}");
        summary.invocation_id = format!("e-compaction-{pass}");
        summary.timestamp = at(10 + pass);
        all.push(summary);
    }

    assert!(
        !chosen.is_empty(),
        "selectSlidingWindow() never chose a window, so compaction is stalled"
    );
    for w in &chosen {
        assert!(
            !w.contains(&"blocked".to_string()),
            "window {w:?} covers the pending call"
        );
    }
    let covered: Vec<String> = chosen.iter().flatten().cloned().collect();
    for want in ["q2", "a2", "q3", "a3"] {
        assert!(
            covered.contains(&want.to_string()),
            "event {want:?} was never summarized across {chosen:?}"
        );
    }
}

#[test]
fn test_covers_all_of_absorbs_a_genuine_superset() {
    let a = EventCompaction {
        start_timestamp: at(10),
        end_timestamp: at(50),
        compacted_content: None,
        excluded_events: vec![EventRef {
            invocation_id: "inv-x".into(),
            timestamp: at(15),
        }],
    };
    let b = EventCompaction {
        start_timestamp: at(20),
        end_timestamp: at(40),
        compacted_content: None,
        excluded_events: vec![],
    };
    assert!(
        covers_all_of(&a, &b),
        "coversAllOf() = false, want true: a spans b, and its hole is outside b entirely"
    );
    let c = EventCompaction {
        start_timestamp: at(10),
        end_timestamp: at(50),
        compacted_content: None,
        excluded_events: vec![EventRef {
            invocation_id: "inv-y".into(),
            timestamp: at(30),
        }],
    };
    assert!(
        !covers_all_of(&c, &b),
        "coversAllOf() = true, want false: the hole is inside b and b does not name it"
    );
}

// parity: adapted: chrono DateTime<Utc> carries no location or monotonic reading, so the second reference is the same instant round-tripped through a +01:00 FixedOffset instead of Europe/Warsaw
#[test]
fn test_covers_all_of_compares_instants_not_clock_readings() {
    let utc = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
    let same = utc
        .with_timezone(&FixedOffset::east_opt(3600).unwrap())
        .with_timezone(&Utc);
    let a = EventCompaction {
        start_timestamp: utc - Duration::hours(1),
        end_timestamp: utc + Duration::hours(1),
        compacted_content: None,
        excluded_events: vec![EventRef {
            invocation_id: "inv".into(),
            timestamp: utc,
        }],
    };
    let b = EventCompaction {
        start_timestamp: utc - Duration::minutes(1),
        end_timestamp: utc + Duration::minutes(1),
        compacted_content: None,
        excluded_events: vec![EventRef {
            invocation_id: "inv".into(),
            timestamp: same,
        }],
    };
    assert!(
        covers_all_of(&a, &b),
        "coversAllOf() = false, want true: the two references name the same instant"
    );
}

#[test]
fn test_apply_keeps_one_summary_when_a_record_is_superseded() {
    let events = vec![
        text_event("a", "inv1", 1, "q1"),
        model_text_event("b", "inv1", 2, "a1"),
        compaction_event("s1", 3, 1, 2, "older summary", vec![]),
        text_event("c", "inv2", 4, "q2"),
        model_text_event("d", "inv2", 5, "a2"),
        compaction_event("s2", 6, 1, 5, "newer summary", vec![excl("inv2", 4)]),
    ];
    let got = ids(&apply(&events));
    assert!(
        !got.contains(&"s1".to_string()),
        "prompt = {got:?}, want the superseded summary gone: both summaries describe the same turns"
    );
}

#[test]
fn test_two_records_over_one_range_leave_exactly_one() {
    let a = compaction_event("a", 9, 1, 5, "summary A", vec![excl("inv1", 2)]);
    let b = compaction_event("b", 10, 1, 5, "summary B", vec![excl("inv2", 3)]);
    let events = vec![
        text_event("e1", "inv1", 1, "one"),
        text_event("e2", "inv1", 2, "two"),
        text_event("e3", "inv2", 3, "three"),
        a,
        b,
    ];

    let got = ids(&apply(&events));
    let kept = ["a", "b"]
        .iter()
        .filter(|id| got.contains(&(**id).to_string()))
        .count();
    assert_eq!(
        kept, 1,
        "prompt {got:?} keeps {kept} of the two records over one range, want exactly 1"
    );
    assert!(
        got.contains(&"b".to_string()),
        "prompt {got:?} does not keep the later record, which is the better informed one"
    );
    assert!(
        got.contains(&"e3".to_string()),
        "prompt {got:?} drops an event the surviving record names as a hole"
    );
    assert!(
        latest_compaction_event(&events).is_some(),
        "LatestCompactionEvent() = nil, so the next window has no seed and no boundary"
    );
}

#[test]
fn test_an_earlier_record_cannot_evict_a_later_one_it_cannot_cover() {
    let events = vec![
        text_event("e1", "inv1", 1, "one"),
        compaction_event("cWide", 3, 1, 9, "wide", vec![]),
        text_event("e5", "inv5", 5, "five"),
        text_event("e6", "inv6", 6, "six"),
        compaction_event("cNarrow", 7, 5, 6, "narrow", vec![]),
    ];

    let got = ids(&apply(&events));
    assert!(
        got.contains(&"cNarrow".to_string()),
        "prompt {got:?} evicted the later summary"
    );
    for id in ["e5", "e6"] {
        assert!(
            !got.contains(&id.to_string()),
            "prompt {got:?} carries {id} raw, though the surviving summary covers it"
        );
    }
    assert!(
        !got.contains(&"e1".to_string()),
        "prompt {got:?} carries e1 raw, though the wide record covers it"
    );
}

#[test]
fn test_covers_all_of_filters_holes_at_the_resolution_they_are_matched_at() {
    let base = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let event_ts = base + Duration::nanoseconds(2_600);
    let hole_ts = base + Duration::nanoseconds(2_100);
    let a = EventCompaction {
        start_timestamp: base,
        end_timestamp: base + Duration::microseconds(10),
        compacted_content: None,
        excluded_events: vec![EventRef {
            invocation_id: "inv".into(),
            timestamp: hole_ts,
        }],
    };
    let b = EventCompaction {
        start_timestamp: base + Duration::nanoseconds(2_500),
        end_timestamp: base + Duration::microseconds(8),
        compacted_content: None,
        excluded_events: vec![],
    };

    let ev = Event {
        invocation_id: "inv".into(),
        timestamp: event_ts,
        ..Default::default()
    };
    assert!(
        excludes(&a, &ev),
        "setup: the reference does not name the event, so this is not the case under test"
    );
    assert!(
        in_range(&ev, &b) || excludes(&b, &ev),
        "setup: the event is not inside b's range, so this is not the case under test"
    );

    assert!(
        !covers_all_of(&a, &b),
        "coversAllOf() = true: a may replace b, but a excludes an event that lands inside b's range"
    );
}

#[test]
fn test_window_never_ends_between_a_call_and_its_response() {
    let call = call_event("call", "inv1", 1, "c1");
    let response = response_event("response", "inv1", 2, "c1");
    let call2 = call_event("call2", "inv1", 2, "c2");

    let got = longest_self_contained_prefix(&[call, response, call2]);

    let mut open: HashSet<String> = HashSet::new();
    for ev in &got {
        for r in function_responses(content(ev)) {
            open.remove(&r.id);
        }
        for c in function_calls(content(ev)) {
            open.insert(c.id.clone());
        }
    }
    assert!(
        open.is_empty(),
        "window {:?} ends with {} unanswered call(s)",
        ids(&got),
        open.len()
    );
    let got_ids = ids(&got);
    assert!(
        !(got_ids.contains(&"response".to_string()) && !got_ids.contains(&"call2".to_string())),
        "window {got_ids:?} keeps one of two events sharing a timestamp"
    );
}
