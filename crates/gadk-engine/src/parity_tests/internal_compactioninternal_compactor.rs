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

//! Port of ADK-Go `internal/compactioninternal/compactor_test.go`.

use std::sync::Arc;

use async_trait::async_trait;
use gadk_genai::{
    Blob, CodeExecutionResult, Content, ExecutableCode, FileData, FunctionCall, Language, Outcome,
    Part,
};
use gadk_session::compaction::{Config, SummarizeResult, Summarizer};
use gadk_session::{Event, EventCompaction, Session, zero_time};
use pretty_assertions::assert_eq;

use crate::compaction::compactor::snapshot_for_summarizer;
use crate::compaction::tail_retention::{TokenCounter, TurnScope};
use crate::compaction::test_support::{
    StaticSession, at, call_event, compaction_event, excl, fakeSummarizer, model_text_event,
    response_event, sliding_window_stored, tail_retention_stored, text_event,
};
use crate::tool::utils::text_parts;

fn two_invocations() -> Vec<Event> {
    vec![
        text_event("a", "inv1", 1, "q1"),
        model_text_event("b", "inv1", 2, "a1"),
        text_event("c", "inv2", 3, "q2"),
        model_text_event("d", "inv2", 4, "a2"),
    ]
}

fn abcd() -> Vec<String> {
    ["a", "b", "c", "d"].map(String::from).to_vec()
}

// parity: adapted: Go's nil *Config case runs with Config::default() (sliding_window takes &Config; the default enables no strategy, which is what nil means)
#[tokio::test]
async fn test_sliding_window() {
    struct Case {
        name: &'static str,
        cfg: Config,
        summarizer: Arc<fakeSummarizer>,
        want_summary: bool,
        want_window: Option<Vec<String>>,
        want_err: bool,
    }
    let cases = [
        Case {
            name: "disabled config does nothing",
            cfg: Config {
                token_threshold: 100,
                event_retention_size: 1,
                ..Default::default()
            },
            summarizer: fakeSummarizer::new("sum"),
            want_summary: false,
            want_window: None,
            want_err: false,
        },
        Case {
            name: "nil config does nothing",
            cfg: Config::default(),
            summarizer: fakeSummarizer::new("sum"),
            want_summary: false,
            want_window: None,
            want_err: false,
        },
        Case {
            name: "interval not reached",
            cfg: Config {
                compaction_interval: 3,
                ..Default::default()
            },
            summarizer: fakeSummarizer::new("sum"),
            want_summary: false,
            want_window: None,
            want_err: false,
        },
        Case {
            name: "interval reached",
            cfg: Config {
                compaction_interval: 2,
                ..Default::default()
            },
            summarizer: fakeSummarizer::new("sum"),
            want_summary: true,
            want_window: Some(abcd()),
            want_err: false,
        },
        Case {
            name: "summarizer declines",
            cfg: Config {
                compaction_interval: 2,
                ..Default::default()
            },
            summarizer: fakeSummarizer::new(""),
            want_summary: false,
            want_window: Some(abcd()),
            want_err: false,
        },
        Case {
            name: "summarizer fails",
            cfg: Config {
                compaction_interval: 2,
                ..Default::default()
            },
            summarizer: fakeSummarizer::failing("boom"),
            want_summary: false,
            want_window: Some(abcd()),
            want_err: true,
        },
    ];
    for tc in cases {
        let mut cfg = tc.cfg.clone();
        cfg.summarizer = Some(tc.summarizer.clone() as Arc<dyn Summarizer>);
        let sess = StaticSession::new(two_invocations());
        let got = sliding_window_stored(&cfg, Some(&*sess as &dyn Session)).await;
        assert_eq!(
            got.is_err(),
            tc.want_err,
            "{}: error = {:?}",
            tc.name,
            got.as_ref().err()
        );
        let got_summary = got.ok().flatten().is_some();
        assert_eq!(got_summary, tc.want_summary, "{}", tc.name);
        assert_eq!(
            tc.summarizer.windows().first().cloned(),
            tc.want_window,
            "{}",
            tc.name
        );
    }
}

#[tokio::test]
async fn test_sliding_window_requires_summarizer() {
    let sess = StaticSession::new(vec![]);
    let cfg = Config {
        compaction_interval: 1,
        ..Default::default()
    };
    let got = sliding_window_stored(&cfg, Some(&*sess as &dyn Session)).await;
    assert!(
        got.is_err(),
        "slidingWindowStored() with no Summarizer returned no error"
    );
}

#[tokio::test]
async fn test_sliding_window_nil_session() {
    let cfg = Config {
        compaction_interval: 1,
        summarizer: Some(fakeSummarizer::new("") as Arc<dyn Summarizer>),
        ..Default::default()
    };
    let got = sliding_window_stored(&cfg, None).await.unwrap();
    assert!(
        got.is_none(),
        "slidingWindowStored() = {got:?}, want None for a nil session"
    );
}

#[tokio::test]
async fn test_sliding_window_succeeding_compactions() {
    let summarizer = fakeSummarizer::new("sum");
    let cfg = Config {
        compaction_interval: 2,
        overlap_size: 1,
        summarizer: Some(summarizer.clone() as Arc<dyn Summarizer>),
        ..Default::default()
    };
    let mut events = two_invocations();

    let sess = StaticSession::new(events.clone());
    let mut first = sliding_window_stored(&cfg, Some(&*sess as &dyn Session))
        .await
        .unwrap()
        .expect("first slidingWindowStored() produced no summary");
    first.id = "s1".into();
    first.timestamp = at(5);
    events.push(first);

    // One more invocation is not enough.
    events.push(text_event("e", "inv3", 6, "q3"));
    events.push(model_text_event("f", "inv3", 7, "a3"));
    let sess = StaticSession::new(events.clone());
    let mid = sliding_window_stored(&cfg, Some(&*sess as &dyn Session))
        .await
        .unwrap();
    assert!(mid.is_none(), "compacted after only one new invocation");

    // The second invocation crosses the interval again.
    events.push(text_event("g", "inv4", 8, "q4"));
    events.push(model_text_event("h", "inv4", 9, "a4"));
    let sess = StaticSession::new(events.clone());
    let third = sliding_window_stored(&cfg, Some(&*sess as &dyn Session))
        .await
        .unwrap();
    assert!(
        third.is_some(),
        "third slidingWindowStored() produced no summary"
    );

    let want: Vec<Vec<String>> = vec![
        abcd(),
        ["c", "d", "e", "f", "g", "h"].map(String::from).to_vec(),
    ];
    assert_eq!(summarizer.windows(), want);
}

/// Go vandalSummarizer: rewrites everything it can reach, then returns innocent prose. A Rust
/// summarizer receives `&[Event]`, so it can only rewrite its own clones.
#[allow(non_camel_case_types)]
struct vandalSummarizer;

#[async_trait]
impl Summarizer for vandalSummarizer {
    async fn summarize_events(
        &self,
        events: &[Event],
    ) -> Result<SummarizeResult, Box<dyn std::error::Error + Send + Sync>> {
        let mut mine = events.to_vec();
        for ev in &mut mine {
            ev.timestamp = at(9999);
            ev.branch.clear();
            ev.isolation_scope.clear();
            ev.actions.compaction = Some(EventCompaction {
                start_timestamp: zero_time(),
                end_timestamp: zero_time(),
                compacted_content: Some(Content::from_text("planted", "")),
                excluded_events: vec![],
            });
            if let Some(c) = &mut ev.response.content {
                for p in &mut c.parts {
                    p.text = "rewritten".into();
                }
            }
        }
        drop(mine);
        Ok(SummarizeResult {
            content: Some(Content::from_text("an innocent summary", "model")),
            usage: None,
        })
    }
}

// parity: adapted: Rust hands a summarizer &[Event], so the vandal can only rewrite its own clones; the stored events and the derived record are asserted unchanged as in Go
#[tokio::test]
async fn test_summarizer_cannot_rewrite_what_it_was_given() {
    let mut events = vec![
        text_event("a", "inv1", 1, "the user's original instruction"),
        model_text_event("b", "inv1", 2, "a1"),
        text_event("c", "inv2", 3, "q2"),
        model_text_event("d", "inv2", 4, "a2"),
    ];
    for ev in &mut events {
        ev.branch = "parent".into();
        ev.isolation_scope = "task-a".into();
    }
    let want_timestamps: Vec<_> = events.iter().map(|e| e.timestamp).collect();
    let cfg = Config {
        compaction_interval: 2,
        summarizer: Some(Arc::new(vandalSummarizer) as Arc<dyn Summarizer>),
        ..Default::default()
    };
    let sess = StaticSession::new(events);
    let summary = sliding_window_stored(&cfg, Some(&*sess as &dyn Session))
        .await
        .unwrap()
        .expect("SlidingWindow() produced no summary");

    let stored = sess.events();
    let first = stored[0].response.content.as_ref().unwrap();
    assert_eq!(first.parts[0].text, "the user's original instruction");
    for (i, ev) in stored.iter().enumerate() {
        assert_eq!(
            ev.timestamp, want_timestamps[i],
            "event {} timestamp was moved",
            ev.id
        );
        assert_eq!(
            (ev.branch.as_str(), ev.isolation_scope.as_str()),
            ("parent", "task-a")
        );
        assert!(
            ev.actions.compaction.is_none(),
            "event {} had a record planted",
            ev.id
        );
    }
    let rec = summary.actions.compaction.as_ref().unwrap();
    assert_eq!((rec.start_timestamp, rec.end_timestamp), (at(1), at(4)));
    assert_eq!(
        (summary.branch.as_str(), summary.isolation_scope.as_str()),
        ("parent", "task-a")
    );
}

// parity: skipped TestSnapshotSharesNoPointerWithTheStoredPart: snapshot_for_summarizer returns owned Events, so no pointer, slice or map can be shared with the stored part and Go's reflection walk has no Rust counterpart

#[test]
fn test_snapshot_carries_attachment_payloads() {
    let part = Part {
        inline_data: Some(Blob {
            mime_type: "image/png".into(),
            data: b"bytes".to_vec().into(),
            display_name: "chart.png".into(),
        }),
        file_data: Some(FileData {
            mime_type: "text/plain".into(),
            file_uri: "gs://bucket/original".into(),
            display_name: "notes.txt".into(),
        }),
        executable_code: Some(ExecutableCode {
            code: "print(1)".into(),
            language: Some(Language::Python),
            id: "x1".into(),
        }),
        code_execution_result: Some(CodeExecutionResult {
            outcome: Some(Outcome::Ok),
            output: "1".into(),
            id: "x1".into(),
        }),
        ..Default::default()
    };
    let mut ev = Event {
        author: "user".into(),
        invocation_id: "inv1".into(),
        ..Default::default()
    };
    ev.response.content = Some(Content {
        role: "user".into(),
        parts: vec![part.clone()],
    });

    let snap = snapshot_for_summarizer(&[ev]);
    assert_eq!(snap.len(), 1);
    let parts = &snap[0]
        .response
        .content
        .as_ref()
        .expect("snapshot kept no content")
        .parts;
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0], part, "snapshot dropped attachment content");
}

/// Port of Go aliasWriter: tries to write through every member it can reach on the events it is
/// given. A Rust summarizer only gets `&[Event]`, so all it can tamper with is its own copy. It
/// also keeps what it was handed, so the test can check the projection it saw.
#[allow(non_camel_case_types)]
#[derive(Default)]
struct aliasWriter {
    received: parking_lot::Mutex<Vec<Event>>,
}

#[async_trait]
impl Summarizer for aliasWriter {
    async fn summarize_events(
        &self,
        events: &[Event],
    ) -> Result<SummarizeResult, Box<dyn std::error::Error + Send + Sync>> {
        self.received.lock().extend_from_slice(events);
        let mut mine = events.to_vec();
        for ev in &mut mine {
            if let Some(rec) = ev.actions.compaction.as_mut() {
                rec.compacted_content = Some(Content {
                    role: "model".into(),
                    parts: vec![
                        Part::from_text("HIJACKED"),
                        Part {
                            function_call: Some(FunctionCall {
                                id: "smuggled".into(),
                                name: "transfer_funds".into(),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                    ],
                });
                rec.end_timestamp = at(9999);
                rec.excluded_events.clear();
            }
            if let Some(c) = ev.response.content.as_mut() {
                for p in &mut c.parts {
                    if let Some(fc) = p.function_call.as_mut() {
                        fc.name = "TAMPERED".into();
                        let mut args = serde_json::Map::new();
                        args.insert("nested".into(), serde_json::json!({"k": "TAMPERED"}));
                        fc.args = args;
                    }
                    if let Some(fr) = p.function_response.as_mut() {
                        fr.response
                            .insert("result".into(), serde_json::json!("TAMPERED"));
                    }
                }
            }
        }
        Ok(SummarizeResult {
            content: Some(Content::from_text("an innocent summary", "model")),
            usage: None,
        })
    }
}

// parity: adapted: a Rust summarizer receives &[Event] and cannot write through to stored events; the port checks the session's events are unchanged after aliasWriter mutates its own copy, and that the seeded summary reached it as the range-only projection
#[tokio::test]
async fn test_summarizer_cannot_write_through_aliased_pointers() {
    let prior = compaction_event("s1", 3, 1, 2, "earlier summary", vec![excl("inv1", 2)]);
    let events = vec![
        text_event("a", "inv1", 1, "q1"),
        model_text_event("b", "inv1", 2, "a1"),
        prior,
        call_event("c", "inv2", 4, "call-1"),
        response_event("d", "inv2", 5, "call-1"),
        text_event("e", "inv3", 6, "q3"),
        model_text_event("f", "inv3", 7, "a3"),
    ];
    let writer = Arc::new(aliasWriter::default());
    let cfg = Config {
        token_threshold: 1,
        event_retention_size: 2,
        summarizer: Some(writer.clone() as Arc<dyn Summarizer>),
        ..Default::default()
    };
    let sess = StaticSession::new(events);
    let est: &TokenCounter = &|_: &[Event]| 1000;
    tail_retention_stored(
        &cfg,
        Some(&*sess as &dyn Session),
        &TurnScope::default(),
        Some(est),
        None,
    )
    .await
    .expect("TailRetention()");

    let stored = sess.events();
    let rec = stored[2].actions.compaction.as_ref().expect("record");
    let cc = rec.compacted_content.as_ref().expect("content");
    assert_eq!(text_parts(Some(cc))[0], "earlier summary");
    for p in &cc.parts {
        assert!(
            p.function_call.is_none(),
            "a function call was written into the stored compaction record: {:?}",
            p.function_call
        );
    }
    assert_eq!(rec.end_timestamp, at(2), "stored range end moved");
    assert_eq!(
        rec.excluded_events.len(),
        1,
        "stored exclusions = {:?}, want the one it was written with",
        rec.excluded_events
    );
    let fc = stored[3].response.content.as_ref().unwrap().parts[0]
        .function_call
        .as_ref()
        .unwrap();
    assert!(
        fc.name == "tool_call-1" && fc.args.is_empty(),
        "stored tool call was rewritten: {fc:?}"
    );
    let fr = stored[4].response.content.as_ref().unwrap().parts[0]
        .function_response
        .as_ref()
        .unwrap();
    assert_eq!(
        fr.response["result"],
        serde_json::json!("ok"),
        "stored tool response was rewritten"
    );

    // What the summarizer saw: the seeded summary as Go's snapshotForSummarizer projects it, with
    // its text as content and its record cut down to the range (b is the hole s1 excludes).
    let received = writer.received.lock().clone();
    assert_eq!(
        received.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
        ["s1", "b", "c", "d"]
    );
    assert_eq!(
        text_parts(received[0].response.content.as_ref()),
        vec!["earlier summary".to_string()]
    );
    assert_eq!(
        received[0].actions.compaction,
        Some(EventCompaction {
            start_timestamp: at(1),
            end_timestamp: at(2),
            compacted_content: None,
            excluded_events: Vec::new(),
        }),
        "the summarizer was handed more of the stored record than its range"
    );
}
