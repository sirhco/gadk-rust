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

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use crate::context::builder::test_support::{SwappingSession, named_agent, session};
use crate::context::{ContextDelta, InvocationContext};

// parity: skipped TestIdentityReadsTheSessionOnceAndNeverTears: Go's generationalInvocation overrides Session() to hand out a fresh session per call; a Rust context holds one Arc<dyn Session> fixed when it is built or derived, so there is no per-call session accessor to count

/// Go generationOf: the number after the first '-'.
fn generation_of(v: &str) -> u64 {
    let (_, tag) = v
        .split_once('-')
        .unwrap_or_else(|| panic!("{v:?} has no generation tag"));
    tag.parse()
        .unwrap_or_else(|e| panic!("{v:?}: generation {tag:?} is not a number: {e}"))
}

// parity: adapted: readers on a multi-thread runtime read ids through every derivation at once (Go: goroutines + -race)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_identity_is_race_free_across_concurrent_readers() {
    let ic = InvocationContext::builder()
        .session(session("app-alice", "alice", "sid-alice"))
        .build_invocation();
    let ctx = ic.clone().into_context();
    let derived = vec![
        ic.clone(),
        ctx.as_invocation().clone(),
        ic.with_delta(ContextDelta {
            agent: Some(named_agent("a")),
            ..Default::default()
        }),
        ctx.with_delta(ContextDelta {
            function_call_id: Some("fc".into()),
            ..Default::default()
        })
        .as_invocation()
        .clone(),
        ctx.with_agent_cancel().0.as_invocation().clone(),
    ];
    let mut tasks = Vec::new();
    for c in derived {
        for _ in 0..4 {
            let c = c.clone();
            tasks.push(tokio::spawn(async move {
                for _ in 0..200 {
                    assert_eq!(
                        (c.user_id(), c.app_name(), c.session_id()),
                        ("alice", "app-alice", "sid-alice")
                    );
                    tokio::task::yield_now().await;
                }
            }));
        }
    }
    for t in tasks {
        t.await.unwrap();
    }
}

// Port of Go TestIdentityAgainstASessionThatChangesUnderIt: every field must name a generation in (before, after].
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_identity_against_a_session_that_changes_under_it() {
    const READERS: i64 = 64;
    const ROUNDS: i64 = 50;
    let sess = Arc::new(SwappingSession::default());
    let ctx = InvocationContext::builder()
        .session(sess.clone())
        .build_invocation()
        .into_context();
    let spanned = Arc::new(AtomicI64::new(0));
    let mut readers = Vec::new();
    for i in 0..READERS {
        let (sess, ctx, spanned) = (sess.clone(), ctx.clone(), spanned.clone());
        readers.push(tokio::spawn(async move {
            for _ in 0..ROUNDS {
                let before = sess.now();
                let got = [
                    ("UserID", "user", ctx.user_id()),
                    ("AppName", "app", ctx.app_name()),
                    ("SessionID", "sid", ctx.session_id()),
                ];
                let after = sess.now();
                for (name, want, v) in got {
                    let (prefix, _) = v
                        .split_once('-')
                        .unwrap_or_else(|| panic!("reader {i}: {name} = {v:?}, want prefix {want:?}"));
                    assert_eq!(prefix, want, "reader {i}: {name} = {v:?}");
                    let generation = generation_of(v);
                    assert!(
                        generation > before && generation <= after,
                        "reader {i}: {name} = {v:?}, generation {generation} is outside ({before},{after}]"
                    );
                }
                if after > before {
                    spanned.fetch_add(1, Ordering::SeqCst);
                }
                tokio::task::yield_now().await;
            }
        }));
    }
    for r in readers {
        r.await.unwrap();
    }
    assert_eq!(
        spanned.load(Ordering::SeqCst),
        READERS * ROUNDS,
        "the fixture stopped advancing on every accessor call"
    );
}

// Port of Go TestIdentityIsNotCached: after a swap, all three fields are read again and name a later generation.
#[test]
fn test_identity_is_not_cached() {
    let sess = Arc::new(SwappingSession::default());
    let ctx = InvocationContext::builder()
        .session(sess.clone())
        .build_invocation()
        .into_context();
    let read = || {
        [
            ctx.user_id().to_string(),
            ctx.app_name().to_string(),
            ctx.session_id().to_string(),
        ]
    };
    let first = read();
    sess.swap();
    let second = read();
    assert_ne!(first, second, "the identity is being cached");
    for (name, a, b) in [
        ("UserID", &first[0], &second[0]),
        ("AppName", &first[1], &second[1]),
        ("SessionID", &first[2], &second[2]),
    ] {
        assert!(
            generation_of(b) > generation_of(a),
            "{name}: {a:?} then {b:?}"
        );
    }
}
