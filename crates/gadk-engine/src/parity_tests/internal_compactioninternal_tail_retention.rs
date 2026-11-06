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

//! Port of ADK-Go `internal/compactioninternal/tail_retention_test.go`.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use gadk_genai::{
    Content, FunctionCall, FunctionResponse, GenerateContentResponseUsageMetadata, Part,
};
use gadk_platform::Platform;
use gadk_session::compaction::{Config, Summarizer, content};
use gadk_session::{Event, Session, zero_time};
use pretty_assertions::assert_eq;
use serde_json::{Map, Value};

use crate::compaction::ProgressGate;
use crate::compaction::apply::apply;
use crate::compaction::compactor::Finish;
use crate::compaction::summary_event::new_summary_event;
use crate::compaction::tail_retention::{
    TokenCounter, TurnScope, estimate_tokens_from_contents, prompt_token_count,
    select_tail_retention_window, tail_retention,
};
use crate::compaction::test_support::{
    StaticSession, at, call_event, compaction_event, fakeSummarizer, ids, model_text_event,
    multi_call_event, response_event, strs, tail_retention_stored, text_event,
};
use crate::compaction::window::skip_blocked_head;
use crate::tool::utils::text_parts;

/// Go withUsage: tags an event with an observed prompt token count.
fn with_usage(mut ev: Event, prompt_tokens: i32) -> Event {
    ev.response.usage_metadata = Some(GenerateContentResponseUsageMetadata {
        prompt_token_count: prompt_tokens,
        ..Default::default()
    });
    ev
}

fn on_branch(mut ev: Event, branch: &str) -> Event {
    ev.branch = branch.to_string();
    ev
}

fn scoped(mut ev: Event, scope: &str) -> Event {
    ev.isolation_scope = scope.to_string();
    ev
}

/// Go recordingGate: captures the ProgressGate calls tail retention makes.
#[derive(Default)]
struct RecordingGate {
    allow: bool,
    recorded: parking_lot::Mutex<Vec<i64>>,
    recovered: AtomicUsize,
    failed: AtomicUsize,
}

impl RecordingGate {
    fn allowing() -> Arc<RecordingGate> {
        Arc::new(RecordingGate {
            allow: true,
            ..Default::default()
        })
    }
    fn recorded(&self) -> Vec<i64> {
        self.recorded.lock().clone()
    }
    fn recovered(&self) -> usize {
        self.recovered.load(Ordering::SeqCst)
    }
    fn failed(&self) -> usize {
        self.failed.load(Ordering::SeqCst)
    }
}

impl ProgressGate for RecordingGate {
    fn allow_at(&self) -> bool {
        self.allow
    }
    fn record_at(&self, tokens: i64) {
        self.recorded.lock().push(tokens);
    }
    fn recovered(&self) {
        self.recovered.fetch_add(1, Ordering::SeqCst);
    }
    fn failed(&self) {
        self.failed.fetch_add(1, Ordering::SeqCst);
    }
}

fn gate_arc(g: &Arc<RecordingGate>) -> Option<Arc<dyn ProgressGate>> {
    Some(g.clone() as Arc<dyn ProgressGate>)
}

fn cfg_with(token_threshold: i32, event_retention_size: i32, s: Arc<dyn Summarizer>) -> Config {
    Config {
        token_threshold,
        event_retention_size,
        summarizer: Some(s),
        ..Default::default()
    }
}

fn four_events(last_usage: i32) -> Vec<Event> {
    vec![
        text_event("a", "inv1", 1, "q1"),
        model_text_event("b", "inv1", 2, "a1"),
        text_event("c", "inv2", 3, "q2"),
        with_usage(model_text_event("d", "inv2", 4, "a2"), last_usage),
    ]
}

/// Go errTestAppend / errTestSummarizer.
const ERR_TEST_APPEND: &str = "append failed";
const ERR_TEST_SUMMARIZER: &str = "summarizer unavailable";

fn session(events: Vec<Event>) -> Arc<StaticSession> {
    StaticSession::new(events)
}

#[test]
fn test_select_tail_retention_window() {
    struct Case {
        name: &'static str,
        events: Vec<Event>,
        retention: i32,
        want: Vec<&'static str>,
    }
    let cases = vec![
        Case {
            name: "fewer events than the retention size",
            events: vec![
                text_event("a", "inv1", 1, "q1"),
                text_event("b", "inv1", 2, "a1"),
            ],
            retention: 5,
            want: vec![],
        },
        Case {
            name: "exactly the retention size keeps everything raw",
            events: vec![
                text_event("a", "inv1", 1, "q1"),
                text_event("b", "inv1", 2, "a1"),
            ],
            retention: 2,
            want: vec![],
        },
        Case {
            name: "older events are compacted, the tail stays raw",
            events: vec![
                text_event("a", "inv1", 1, "q1"),
                model_text_event("b", "inv1", 2, "a1"),
                text_event("c", "inv2", 3, "q2"),
                model_text_event("d", "inv2", 4, "a2"),
            ],
            retention: 2,
            want: vec!["a", "b"],
        },
        Case {
            name: "zero retention compacts everything",
            events: vec![
                text_event("a", "inv1", 1, "q1"),
                model_text_event("b", "inv1", 2, "a1"),
            ],
            retention: 0,
            want: vec!["a", "b"],
        },
        Case {
            name: "the cut moves back past a same-timestamp group",
            events: vec![
                text_event("a", "inv1", 1, "q1"),
                model_text_event("b", "inv1", 2, "a1"),
                model_text_event("c", "inv1", 2, "a2"),
                model_text_event("d", "inv1", 2, "a3"),
            ],
            retention: 2,
            want: vec!["a"],
        },
        Case {
            name: "a whole same-timestamp tail leaves nothing to compact",
            events: vec![
                model_text_event("a", "inv1", 2, "a1"),
                model_text_event("b", "inv1", 2, "a2"),
                model_text_event("c", "inv1", 2, "a3"),
            ],
            retention: 1,
            want: vec![],
        },
        Case {
            name: "window is trimmed so a call is not split from its response",
            events: vec![
                text_event("a", "inv1", 1, "q1"),
                call_event("b", "inv1", 2, "c1"),
                response_event("c", "inv1", 3, "c1"),
                model_text_event("d", "inv1", 4, "a1"),
            ],
            retention: 1,
            want: vec!["a", "b", "c"],
        },
        Case {
            name: "nil when the compactable prefix is entirely an open call",
            events: vec![
                call_event("a", "inv1", 1, "c1"),
                response_event("b", "inv1", 2, "c1"),
                model_text_event("c", "inv1", 3, "a1"),
            ],
            retention: 2,
            want: vec![],
        },
        Case {
            name: "only events after the previous compaction are candidates",
            events: vec![
                text_event("a", "inv1", 1, "q1"),
                model_text_event("b", "inv1", 2, "a1"),
                compaction_event("s1", 3, 1, 2, "earlier summary", vec![]),
                text_event("c", "inv2", 4, "q2"),
                model_text_event("d", "inv2", 5, "a2"),
                text_event("e", "inv3", 6, "q3"),
                model_text_event("f", "inv3", 7, "a3"),
            ],
            retention: 2,
            want: vec!["s1", "c", "d"],
        },
    ];
    for tc in cases {
        let got = ids(&select_tail_retention_window(
            &tc.events,
            tc.retention,
            &TurnScope::default(),
        ));
        assert_eq!(
            got,
            strs(&tc.want),
            "{} (retention={})",
            tc.name,
            tc.retention
        );
    }
}

#[test]
fn test_select_tail_retention_window_seeds_previous_summary() {
    let events = vec![
        text_event("a", "inv1", 1, "q1"),
        model_text_event("b", "inv1", 2, "a1"),
        compaction_event("s1", 3, 1, 2, "earlier summary", vec![]),
        text_event("c", "inv2", 4, "q2"),
        model_text_event("d", "inv2", 5, "a2"),
        text_event("e", "inv3", 6, "q3"),
        model_text_event("f", "inv3", 7, "a3"),
    ];
    let window = select_tail_retention_window(&events, 2, &TurnScope::default());
    assert!(
        !window.is_empty(),
        "select_tail_retention_window() returned nothing"
    );
    let seed = &window[0];
    assert_eq!(
        seed.timestamp,
        at(1),
        "seed timestamp, want the previous compaction's start"
    );
    assert_eq!(seed.author, "model");
    assert_eq!(
        text_parts(content(seed)),
        vec!["earlier summary".to_string()]
    );

    let mut summary = new_summary_event(
        &window,
        &events,
        &Content::from_text("new summary", "model"),
        None,
    )
    .expect("new_summary_event");
    summary.id = "s2".into();
    summary.timestamp = at(8);
    let rec = summary.actions.compaction.as_ref().unwrap();
    assert_eq!(
        rec.start_timestamp,
        at(1),
        "new summary must start at the old range's start"
    );
    assert!(
        rec.excluded_events.is_empty(),
        "new summary excludes {:?}, want nothing",
        rec.excluded_events
    );

    let mut all = events.clone();
    all.push(summary);
    assert_eq!(ids(&apply(&all)), strs(&["s2", "e", "f"]));
}

#[test]
fn test_prompt_token_count() {
    let est_123: &TokenCounter = &|_: &[Event]| 123;
    let est_0: &TokenCounter = &|_: &[Event]| 0;
    type Case<'a> = (&'a str, Vec<Event>, Option<&'a TokenCounter>, Option<i64>);
    let cases: Vec<Case<'_>> = vec![
        ("no events and no estimator", vec![], None, None),
        (
            "estimator used when nothing reported a count",
            vec![text_event("a", "inv1", 1, "q1")],
            Some(est_123),
            Some(123),
        ),
        (
            "estimator returning zero means unknown",
            vec![text_event("a", "inv1", 1, "q1")],
            Some(est_0),
            None,
        ),
        (
            "observed count wins over the estimator",
            vec![with_usage(model_text_event("a", "inv1", 1, "a1"), 500)],
            Some(est_123),
            Some(500),
        ),
        (
            "the most recent observed count wins",
            vec![
                with_usage(model_text_event("a", "inv1", 1, "a1"), 500),
                text_event("b", "inv2", 2, "q2"),
                with_usage(model_text_event("c", "inv2", 3, "a2"), 900),
            ],
            None,
            Some(900),
        ),
    ];
    for (name, events, est, want) in cases {
        assert_eq!(
            prompt_token_count(&events, &TurnScope::default(), est),
            want,
            "{name}"
        );
    }
}

// parity: adapted: Rust Content has no nil content or nil part, so the "nil content is skipped" and "nil part is skipped" cases are dropped
#[test]
fn test_estimate_tokens_from_contents() {
    let text = |n: usize| Content {
        parts: vec![Part::from_text("x".repeat(n))],
        ..Default::default()
    };
    let mut response = Map::new();
    response.insert("result".into(), Value::String("y".repeat(4000)));
    let cases: Vec<(&str, Vec<Content>, i64)> = vec![
        ("nil", vec![], 0),
        ("empty text", vec![text(0)], 0),
        ("below one token", vec![text(3)], 0),
        ("exactly one token", vec![text(4)], 1),
        ("summed across contents", vec![text(2000), text(2000)], 1000),
        (
            "a function call counts its name",
            vec![Content {
                parts: vec![Part {
                    function_call: Some(FunctionCall {
                        name: "search".into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            1,
        ),
        (
            "a function response counts its payload",
            vec![Content {
                parts: vec![Part {
                    function_response: Some(FunctionResponse {
                        name: "search".into(),
                        response,
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            1004,
        ),
    ];
    for (name, contents, want) in cases {
        assert_eq!(estimate_tokens_from_contents(&contents), want, "{name}");
    }
}

// parity: adapted: tail_retention takes &Config, so Go's nil-config case runs with Config::default(), which reaches the same early return
#[tokio::test]
async fn test_tail_retention() {
    struct Case {
        name: &'static str,
        cfg: Option<Config>,
        events: Vec<Event>,
        summarizer: Arc<fakeSummarizer>,
        want_summary: bool,
        want_window: Option<Vec<&'static str>>,
        want_err: bool,
    }
    let tr = |t, r| Config {
        token_threshold: t,
        event_retention_size: r,
        ..Default::default()
    };
    let cases = vec![
        Case {
            name: "nil config does nothing",
            cfg: None,
            events: four_events(900),
            summarizer: fakeSummarizer::new("sum"),
            want_summary: false,
            want_window: None,
            want_err: false,
        },
        Case {
            name: "sliding-window-only config does nothing",
            cfg: Some(Config {
                compaction_interval: 2,
                ..Default::default()
            }),
            events: four_events(900),
            summarizer: fakeSummarizer::new("sum"),
            want_summary: false,
            want_window: None,
            want_err: false,
        },
        Case {
            name: "below the threshold",
            cfg: Some(tr(1000, 2)),
            events: four_events(900),
            summarizer: fakeSummarizer::new("sum"),
            want_summary: false,
            want_window: None,
            want_err: false,
        },
        Case {
            name: "at the threshold",
            cfg: Some(tr(900, 2)),
            events: four_events(900),
            summarizer: fakeSummarizer::new("sum"),
            want_summary: true,
            want_window: Some(vec!["a", "b"]),
            want_err: false,
        },
        Case {
            name: "above the threshold",
            cfg: Some(tr(100, 2)),
            events: four_events(900),
            summarizer: fakeSummarizer::new("sum"),
            want_summary: true,
            want_window: Some(vec!["a", "b"]),
            want_err: false,
        },
        Case {
            name: "threshold reached but the tail retains everything",
            cfg: Some(tr(100, 10)),
            events: four_events(900),
            summarizer: fakeSummarizer::new("sum"),
            want_summary: false,
            want_window: None,
            want_err: false,
        },
        Case {
            name: "summarizer declines",
            cfg: Some(tr(100, 2)),
            events: four_events(900),
            summarizer: fakeSummarizer::new(""),
            want_summary: false,
            want_window: Some(vec!["a", "b"]),
            want_err: false,
        },
        Case {
            name: "summarizer fails",
            cfg: Some(tr(100, 2)),
            events: four_events(900),
            summarizer: fakeSummarizer::failing("boom"),
            want_summary: false,
            want_window: Some(vec!["a", "b"]),
            want_err: true,
        },
        Case {
            name: "no observed token count and no estimate",
            cfg: Some(tr(1, 1)),
            events: vec![
                text_event("a", "inv1", 1, "q1"),
                text_event("b", "inv1", 2, "q2"),
            ],
            summarizer: fakeSummarizer::new("sum"),
            want_summary: false,
            want_window: None,
            want_err: false,
        },
    ];
    for tc in cases {
        // Go copies the config and sets the summarizer; a nil config stays nil.
        let cfg = match tc.cfg {
            Some(mut c) => {
                c.summarizer = Some(tc.summarizer.clone() as Arc<dyn Summarizer>);
                c
            }
            None => Config::default(),
        };
        let sess = session(tc.events);
        let got = tail_retention_stored(
            &cfg,
            Some(&*sess as &dyn Session),
            &TurnScope::default(),
            None,
            None,
        )
        .await;
        assert_eq!(
            got.is_err(),
            tc.want_err,
            "{}: error = {:?}",
            tc.name,
            got.as_ref().err().map(|e| e.to_string())
        );
        let got_summary = matches!(got, Ok(Some(_)));
        assert_eq!(got_summary, tc.want_summary, "{}: returned event", tc.name);
        let got_window = tc.summarizer.windows().first().cloned();
        let want_window = tc.want_window.map(|w| strs(&w));
        assert_eq!(got_window, want_window, "{}: summarizer window", tc.name);
    }
}

#[tokio::test]
async fn test_tail_retention_uses_the_estimator() {
    let events = vec![
        text_event("a", "inv1", 1, "q1"),
        model_text_event("b", "inv1", 2, "a1"),
        text_event("c", "inv2", 3, "q2"),
        model_text_event("d", "inv2", 4, "a2"),
    ];
    let cfg = cfg_with(500, 2, fakeSummarizer::new("sum"));

    let low: &TokenCounter = &|_: &[Event]| 100;
    let sess = session(events.clone());
    let got = tail_retention_stored(
        &cfg,
        Some(&*sess as &dyn Session),
        &TurnScope::default(),
        Some(low),
        None,
    )
    .await
    .expect("tail_retention_stored");
    assert!(
        got.is_none(),
        "compacted despite an estimate below the threshold"
    );

    let high: &TokenCounter = &|_: &[Event]| 700;
    let sess = session(events);
    let got = tail_retention_stored(
        &cfg,
        Some(&*sess as &dyn Session),
        &TurnScope::default(),
        Some(high),
        None,
    )
    .await
    .expect("tail_retention_stored");
    assert!(
        got.is_some(),
        "did not compact despite an estimate above the threshold"
    );
}

#[tokio::test]
async fn test_tail_retention_requires_summarizer() {
    let cfg = Config {
        token_threshold: 1,
        event_retention_size: 0,
        ..Default::default()
    };
    let sess = session(vec![with_usage(model_text_event("a", "inv1", 1, "a"), 10)]);
    let err = tail_retention_stored(
        &cfg,
        Some(&*sess as &dyn Session),
        &TurnScope::default(),
        None,
        None,
    )
    .await
    .expect_err("tail_retention_stored() with no Summarizer returned no error");
    assert_eq!(err.to_string(), "no Summarizer configured");
}

#[tokio::test]
async fn test_tail_retention_stamps_the_summary() {
    let events = vec![
        text_event("a", "inv1", 1, "q1"),
        with_usage(model_text_event("b", "inv1", 2, "a1"), 900),
    ];
    let cfg = cfg_with(100, 0, fakeSummarizer::new("sum"));
    let sess = session(events.clone());
    let got = tail_retention_stored(
        &cfg,
        Some(&*sess as &dyn Session),
        &TurnScope::default(),
        None,
        None,
    )
    .await
    .expect("tail_retention_stored")
    .expect("tail_retention_stored() produced no summary");
    assert!(!got.id.is_empty(), "summary has no ID");
    assert!(!got.invocation_id.is_empty(), "summary has no InvocationID");
    assert_ne!(got.timestamp, zero_time(), "summary has no Timestamp");
    for ev in &events {
        assert_ne!(
            got.invocation_id, ev.invocation_id,
            "summary reuses invocation ID from a covered event"
        );
    }
}

#[tokio::test]
async fn test_tail_retention_then_apply_shrinks_history() {
    let events = vec![
        text_event("a", "inv1", 1, "q1"),
        model_text_event("b", "inv1", 2, "a1"),
        text_event("c", "inv1", 3, "q2"),
        with_usage(model_text_event("d", "inv1", 4, "a2"), 5000),
    ];
    let cfg = cfg_with(1000, 2, fakeSummarizer::new("SUMMARY"));
    let sess = session(events.clone());
    let mut summary = tail_retention_stored(
        &cfg,
        Some(&*sess as &dyn Session),
        &TurnScope::default(),
        None,
        None,
    )
    .await
    .expect("tail_retention_stored")
    .expect("tail_retention_stored() produced no summary");
    summary.id = "s1".into();

    let mut all = events;
    all.push(summary);
    let got = apply(&all);
    assert_eq!(ids(&got), strs(&["s1", "c", "d"]));
    assert_eq!(text_parts(content(&got[0])), vec!["SUMMARY".to_string()]);
}

#[test]
fn test_select_tail_retention_window_stays_in_one_scope() {
    let mut sub = text_event("c", "inv2", 3, "SUB-AGENT-SECRET");
    sub.branch = "root.sub".into();
    sub.isolation_scope = "scope-1".into();
    let events = vec![
        text_event("a", "inv1", 1, "q1"),
        model_text_event("b", "inv1", 2, "a1"),
        sub,
        text_event("d", "inv3", 4, "q3"),
        model_text_event("e", "inv3", 5, "a3"),
    ];
    let window = select_tail_retention_window(&events, 2, &TurnScope::default());
    assert_eq!(
        ids(&window),
        strs(&["a", "b"]),
        "the window must stop at the scope change"
    );
    for ev in &window {
        assert!(
            ev.branch.is_empty() && ev.isolation_scope.is_empty(),
            "event {:?} carries branch {:?} scope {:?}, so the window is not homogeneous",
            ev.id,
            ev.branch,
            ev.isolation_scope
        );
    }
}

#[test]
fn test_select_tail_retention_window_keeps_a_tied_boundary_event() {
    let events = vec![
        text_event("a", "inv1", 1, "q1"),
        model_text_event("b", "inv1", 2, "a1"),
        compaction_event("s1", 3, 1, 3, "EARLIER", vec![]),
        text_event("tied", "inv2", 3, "NEVER-SUMMARIZED"),
        text_event("c", "inv3", 4, "q3"),
        model_text_event("d", "inv3", 5, "a3"),
        text_event("e", "inv4", 6, "q4"),
    ];
    let got = ids(&select_tail_retention_window(
        &events,
        1,
        &TurnScope::default(),
    ));
    assert!(
        got.contains(&"tied".to_string()),
        "window {got:?} does not include the boundary event"
    );
}

#[test]
fn test_prompt_token_count_adds_events_since_the_last_report() {
    let events = vec![
        with_usage(model_text_event("a", "inv1", 1, "answer"), 100),
        text_event("b", "inv2", 2, &"x".repeat(400)),
    ];
    // The estimator stands in for the real one: four characters per token.
    let estimate: &TokenCounter = &|evs: &[Event]| {
        let n: usize = evs
            .iter()
            .filter_map(content)
            .flat_map(|c| c.parts.iter())
            .map(|p| p.text.len())
            .sum();
        (n / 4) as i64
    };
    let got = prompt_token_count(&events, &TurnScope::default(), Some(estimate))
        .expect("prompt_token_count() reported nothing");
    assert!(
        got > 100,
        "prompt_token_count() = {got}, want more than the reported 100"
    );
}

#[tokio::test]
async fn test_tail_retention_re_arms_the_gate_below_the_threshold() {
    let gate = RecordingGate::allowing();
    let cfg = cfg_with(1000, 2, fakeSummarizer::new("sum"));
    let sess = session(four_events(100));
    let got = tail_retention_stored(
        &cfg,
        Some(&*sess as &dyn Session),
        &TurnScope::default(),
        None,
        gate_arc(&gate),
    )
    .await
    .expect("tail_retention_stored");
    assert!(
        got.is_none(),
        "returned a summary at 100 tokens against a 1000 threshold"
    );
    assert_eq!(
        gate.recovered(),
        1,
        "a prompt under the threshold means the last compaction worked"
    );
}

#[tokio::test]
async fn test_tail_retention_does_not_record_a_failed_attempt() {
    let gate = RecordingGate::allowing();
    let cfg = cfg_with(100, 2, fakeSummarizer::failing("boom"));
    let sess = session(four_events(900));
    let got = tail_retention_stored(
        &cfg,
        Some(&*sess as &dyn Session),
        &TurnScope::default(),
        None,
        gate_arc(&gate),
    )
    .await;
    assert!(got.is_err(), "want the summarizer failure");
    assert!(
        gate.recorded().is_empty(),
        "record_at called {:?} after a failed summarization",
        gate.recorded()
    );
}

#[tokio::test]
async fn test_tail_retention_records_a_successful_compaction() {
    let gate = RecordingGate::allowing();
    let cfg = cfg_with(100, 2, fakeSummarizer::new("sum"));
    let sess = session(four_events(900));
    let got = tail_retention_stored(
        &cfg,
        Some(&*sess as &dyn Session),
        &TurnScope::default(),
        None,
        gate_arc(&gate),
    )
    .await
    .expect("tail_retention_stored");
    assert!(got.is_some(), "want a summary");
    assert_eq!(gate.recorded(), vec![900]);
}

#[test]
fn test_select_tail_retention_window_keeps_the_live_question() {
    let events = vec![
        text_event("q1", "inv1", 1, "older question"),
        model_text_event("a1", "inv1", 2, "older answer"),
        text_event("q2", "inv2", 3, "the question being answered"),
        model_text_event("t1", "inv2", 4, "tool step 1"),
        model_text_event("t2", "inv2", 5, "tool step 2"),
        model_text_event("t3", "inv2", 6, "tool step 3"),
    ];
    let scope = TurnScope {
        invocation_id: "inv2".into(),
        ..Default::default()
    };
    let got = ids(&select_tail_retention_window(&events, 2, &scope));
    assert!(
        !got.contains(&"q2".to_string()),
        "the window covers the question the turn is answering"
    );
    assert_eq!(got, strs(&["q1", "a1", "t1"]));
}

#[test]
fn test_select_tail_retention_window_steps_past_a_blocked_head() {
    let events = vec![
        call_event("blocked", "inv1", 1, "c-pending"),
        call_event("call", "inv2", 2, "c-done"),
        response_event("resp", "inv2", 3, "c-done"),
        text_event("q", "inv3", 4, "another question"),
        model_text_event("a", "inv3", 5, "another answer"),
    ];
    let got = ids(&select_tail_retention_window(
        &events,
        2,
        &TurnScope::default(),
    ));
    assert!(
        !got.is_empty(),
        "select_tail_retention_window() gave up because the head is blocked"
    );
    assert!(
        !got.contains(&"blocked".to_string()),
        "the window covers the pending call"
    );
    assert_eq!(got, strs(&["call", "resp"]));
}

#[test]
fn test_skip_blocked_head_keeps_a_call_with_its_response() {
    let window = vec![
        multi_call_event("head", "inv1", 1, &["c-pending", "c-two"]),
        response_event("resp2", "inv1", 2, "c-two"),
        text_event("q", "inv2", 3, "later question"),
        model_text_event("a", "inv2", 4, "later answer"),
    ];
    let got = ids(&skip_blocked_head(window));
    assert!(
        !got.contains(&"resp2".to_string()),
        "window {got:?} summarizes a response whose call stays raw in the skipped head"
    );
}

#[test]
fn test_prompt_token_count_ignores_other_branches() {
    let events = vec![
        with_usage(
            on_branch(model_text_event("mine", "inv1", 1, "small"), "parent.child"),
            40,
        ),
        with_usage(
            on_branch(
                model_text_event("sibling", "inv2", 2, "huge"),
                "parent.other",
            ),
            200000,
        ),
    ];
    let scope = TurnScope {
        branch: "parent.child".into(),
        ..Default::default()
    };
    assert_eq!(
        prompt_token_count(&events, &scope, None),
        Some(40),
        "the reading came from another branch"
    );
}

#[test]
fn test_prompt_token_count_ignores_other_isolation_scopes() {
    let events = vec![
        with_usage(
            scoped(model_text_event("mine", "inv1", 1, "small"), "task-a"),
            40,
        ),
        with_usage(
            scoped(model_text_event("other", "inv2", 2, "huge"), "task-b"),
            200000,
        ),
    ];
    let scope = TurnScope {
        isolation_scope: "task-a".into(),
        ..Default::default()
    };
    assert_eq!(
        prompt_token_count(&events, &scope, None),
        Some(40),
        "the reading came from another isolation scope"
    );
}

#[tokio::test]
async fn test_tail_retention_does_not_record_a_discarded_summary() {
    struct Case {
        name: &'static str,
        finish_err: Option<&'static str>,
        discard_reason: &'static str,
        want_recorded: bool,
    }
    let cases = [
        Case {
            name: "stored",
            finish_err: None,
            discard_reason: "",
            want_recorded: true,
        },
        Case {
            name: "discarded by the caller",
            finish_err: None,
            discard_reason: "a competing compaction landed",
            want_recorded: false,
        },
        Case {
            name: "failed on the way to the session",
            finish_err: Some(ERR_TEST_APPEND),
            discard_reason: "",
            want_recorded: false,
        },
    ];
    for tc in cases {
        // Per case: the summarizer counts its calls.
        let cfg = cfg_with(100, 2, fakeSummarizer::new("sum"));
        let gate = RecordingGate::allowing();
        let sess = session(four_events(900));
        let (summary, finish) = tail_retention(
            &Platform::default(),
            &cfg,
            Some(&*sess as &dyn Session),
            &TurnScope::default(),
            None,
            gate_arc(&gate),
        )
        .await
        .unwrap_or_else(|e| panic!("{}: tail_retention() error = {e}", tc.name));
        assert!(summary.is_some(), "{}: want a summary", tc.name);
        let err: Option<Box<dyn std::error::Error + Send + Sync>> = tc.finish_err.map(|e| e.into());
        finish.finish(
            err.as_deref()
                .map(|e| e as &(dyn std::error::Error + 'static)),
            tc.discard_reason,
        );
        assert_eq!(
            !gate.recorded().is_empty(),
            tc.want_recorded,
            "{}: gate recorded {:?}",
            tc.name,
            gate.recorded()
        );
    }
}

#[test]
fn test_skip_blocked_head_still_compacts_past_an_answered_sibling() {
    let window = vec![
        multi_call_event("head", "inv1", 1, &["c-longrunning", "c-two"]),
        response_event("resp2", "inv1", 2, "c-two"),
        text_event("q", "inv2", 3, "later question"),
        model_text_event("a", "inv2", 4, "later answer"),
        text_event("q2", "inv3", 5, "later question 2"),
        model_text_event("a2", "inv3", 6, "later answer 2"),
    ];
    let got = ids(&skip_blocked_head(window));
    assert!(
        !got.is_empty(),
        "skip_blocked_head() = empty: one unanswered call in a parallel pair stalls compaction"
    );
    assert!(
        !got.contains(&"resp2".to_string()),
        "window {got:?} summarizes a response whose call stays raw in the skipped head"
    );
    assert_eq!(got, strs(&["q", "a", "q2", "a2"]));
}

#[tokio::test]
async fn test_tail_retention_closes_the_gate_only_when_trying_again_is_pointless() {
    let events = vec![
        with_usage(model_text_event("a", "inv1", 1, "a1"), 900),
        text_event("b", "inv1", 2, "q2"),
        model_text_event("c", "inv1", 3, "a2"),
        text_event("d", "inv1", 4, "q3"),
    ];
    async fn run(
        events: &[Event],
        sum: Arc<dyn Summarizer>,
        finish_with: Option<Box<dyn FnOnce(Finish) + Send>>,
    ) -> Arc<RecordingGate> {
        let gate = RecordingGate::allowing();
        let cfg = cfg_with(100, 1, sum);
        let sess = StaticSession::new(events.to_vec());
        let res = tail_retention(
            &Platform::default(),
            &cfg,
            Some(&*sess as &dyn Session),
            &TurnScope::default(),
            None,
            gate_arc(&gate),
        )
        .await;
        if let (Ok((_, finish)), Some(f)) = (res, finish_with) {
            f(finish);
        }
        gate
    }

    // the summarizer errored: the caller never gets a live Finish on this path.
    let g = run(&events, fakeSummarizer::failing(ERR_TEST_SUMMARIZER), None).await;
    assert_eq!(
        g.failed(),
        1,
        "a failing summarizer must not be retried every model call"
    );

    // the summarizer declined
    let g = run(&events, fakeSummarizer::new(""), None).await;
    assert_eq!(g.failed(), 1, "a decline gets no Finish either");

    // the append failed
    let g = run(
        &events,
        fakeSummarizer::new("a summary"),
        Some(Box::new(|f: Finish| {
            let err: Box<dyn std::error::Error + Send + Sync> = ERR_TEST_APPEND.into();
            f.finish(Some(&*err as &(dyn std::error::Error + 'static)), "");
        })),
    )
    .await;
    assert_eq!(g.failed(), 1);
    assert!(
        g.recorded().is_empty(),
        "gate recorded {:?} for a summary that was never stored",
        g.recorded()
    );

    // the caller discarded it
    let g = run(
        &events,
        fakeSummarizer::new("a summary"),
        Some(Box::new(|f: Finish| {
            f.finish(None, "a competing compaction landed");
        })),
    )
    .await;
    assert_eq!(
        g.failed(),
        0,
        "a discard means the picture changed, not that trying again is pointless"
    );
    assert!(
        g.recorded().is_empty(),
        "gate recorded {:?} for a summary that was never stored",
        g.recorded()
    );

    // it was stored
    let g = run(
        &events,
        fakeSummarizer::new("a summary"),
        Some(Box::new(|f: Finish| f.finish(None, ""))),
    )
    .await;
    assert_eq!(
        g.recorded().len(),
        1,
        "gate.recorded = {:?}, want one entry",
        g.recorded()
    );
    assert_eq!(g.failed(), 0);
}

#[test]
fn test_select_tail_retention_window_seeds_while_holding_back_the_live_head() {
    let events = vec![
        text_event("a", "inv1", 1, "q1"),
        model_text_event("b", "inv1", 2, "a1"),
        compaction_event("s1", 3, 1, 2, "earlier summary", vec![]),
        text_event("c", "inv2", 4, "q2"),
        model_text_event("d", "inv2", 5, "a2"),
        text_event("live-q", "inv3", 6, "the question being answered now"),
        model_text_event("live-a", "inv3", 7, "partial work"),
        text_event("live-b", "inv3", 8, "more partial work"),
    ];
    let scope = TurnScope {
        invocation_id: "inv3".into(),
        ..Default::default()
    };
    let window = select_tail_retention_window(&events, 1, &scope);
    let got = ids(&window);
    assert!(
        !got.contains(&"live-q".to_string()),
        "window {got:?} summarizes the question the turn is answering"
    );
    assert!(
        got.first().map(String::as_str) == Some("s1"),
        "window {got:?} does not open with the previous summary"
    );

    let mut summary = new_summary_event(
        &window,
        &events,
        &Content::from_text("new summary", "model"),
        None,
    )
    .expect("new_summary_event");
    summary.id = "s2".into();
    summary.timestamp = at(9);
    let mut all = events;
    all.push(summary);
    let after = ids(&apply(&all));
    assert!(
        !after.contains(&"s1".to_string()),
        "prompt {after:?} still carries the superseded summary"
    );
    assert!(
        after.contains(&"live-q".to_string()),
        "prompt {after:?} lost the question the turn is answering"
    );
}

#[tokio::test]
async fn test_tail_retention_empty_window_is_not_yet_not_never() {
    let gate = RecordingGate::allowing();
    let cfg = cfg_with(100, 4, fakeSummarizer::new("s"));
    let sess = session(vec![with_usage(
        model_text_event("a", "inv1", 1, "a1"),
        900,
    )]);
    let (summary, _finish) = tail_retention(
        &Platform::default(),
        &cfg,
        Some(&*sess as &dyn Session),
        &TurnScope::default(),
        None,
        gate_arc(&gate),
    )
    .await
    .expect("tail_retention");
    assert!(summary.is_none(), "want no summary");
    assert_eq!(gate.failed(), 0, "an empty window is not a spent attempt");
}

#[test]
fn test_seed_is_taken_from_the_windows_own_branch() {
    let events = vec![
        on_branch(text_event("a1", "invA1", 1, "A one"), "root.A"),
        on_branch(model_text_event("a2", "invA1", 2, "A two"), "root.A"),
        on_branch(
            compaction_event("SA", 3, 1, 2, "summary of A", vec![]),
            "root.A",
        ),
        on_branch(text_event("b1", "invB1", 4, "B one"), "root.B"),
        on_branch(
            compaction_event("SB", 5, 4, 4, "summary of B", vec![]),
            "root.B",
        ),
        on_branch(text_event("a3", "invA2", 6, "A three"), "root.A"),
        on_branch(model_text_event("a4", "invA2", 7, "A four"), "root.A"),
        on_branch(text_event("a5", "invA3", 8, "A five"), "root.A"),
    ];
    let scope = TurnScope {
        branch: "root.A".into(),
        ..Default::default()
    };
    let got = ids(&select_tail_retention_window(&events, 1, &scope));
    assert!(
        got.contains(&"SA".to_string()),
        "window {got:?} does not open with branch A's own summary"
    );
    assert!(
        !got.contains(&"SB".to_string()),
        "window {got:?} seeded from a sibling branch"
    );
}

#[test]
fn test_tail_retention_window_stays_inside_the_turns_scope() {
    let events = vec![
        on_branch(
            text_event("b1", "invB", 1, "the sibling's only event"),
            "root.B",
        ),
        on_branch(text_event("a1", "invA1", 2, "A one"), "root.A"),
        on_branch(model_text_event("a2", "invA1", 3, "A two"), "root.A"),
        on_branch(text_event("a3", "invA2", 4, "A three"), "root.A"),
        on_branch(model_text_event("a4", "invA2", 5, "A four"), "root.A"),
    ];
    let scope = TurnScope {
        branch: "root.A".into(),
        ..Default::default()
    };
    let got = ids(&select_tail_retention_window(&events, 2, &scope));
    assert!(
        !got.is_empty(),
        "select_tail_retention_window() = empty, want root.A's own eligible events"
    );
    assert!(
        !got.contains(&"b1".to_string()),
        "window {got:?} summarizes a sibling branch"
    );
    assert_eq!(got, strs(&["a1", "a2"]));
}
