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

//! Part of the port of ADK-Go `plugin/agentanalytics/late_callbacks_test.go`: the
//! 13 Go tests from `TestTraceManager_LateOperationsKeepTheirSpans` to
//! `TestTraceManager_FullOpenCompletedCacheStaysBounded` in the Go file's order that need no runner
//! (the exact list is in the 7b-3a plan's contracts, Appendix A).
//!
//! Go's `ctx := t.Context()` is [`CallbackCtx::background`] (never cancelled during a test);
//! Go's `tm.now = func() time.Time { return now }` with a moving `now` is a [`ManualClock`];
//! Go's `trackedInvocations(tm)` (`trace_manager_test.go`) is the file-local
//! [`tracked_invocations`].

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use chrono::{TimeZone, Utc};
use gadk_engine::telemetry::d6::SpanIds;

use crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::tracked_invocations;
use crate::trace::testkit::ManualClock;
use crate::trace::{CallbackCtx, Identity, RowSpan, SWEEP_INTERVAL, TraceManager};

/// Go `identity{invocationID: inv, agentName: agent}`.
fn id(inv: &str, agent: &str) -> Identity {
    Identity {
        invocation_id: inv.into(),
        agent_name: agent.into(),
        ..Identity::default()
    }
}

/// The three late-operation kinds of Go's `for _, kind := range []string{"agent", "model",
/// "tool"}` subtests.
const KINDS: [&str; 3] = ["agent", "model", "tool"];

/// Starts a span of `kind` (Go's `pushAgentSpan` / `pushLLMSpan` / `pushToolSpan(…, "call")`).
fn push(tm: &TraceManager, ctx: &CallbackCtx, id: &Identity, kind: &str) -> RowSpan {
    match kind {
        "agent" => tm.push_agent_span(ctx, id).0,
        "model" => tm.push_llm_span(ctx, id),
        _ => tm.push_tool_span(ctx, id, "call"),
    }
}

/// Ends a span of `kind` (Go's `popAgentSpan` / `popLLMSpan(…, false)` /
/// `popToolSpan(…, "call", false)`).
fn pop(tm: &TraceManager, ctx: &CallbackCtx, id: &Identity, kind: &str) -> RowSpan {
    match kind {
        "agent" => tm.pop_agent_span(ctx, id),
        "model" => tm.pop_llm_span(ctx, id, false),
        _ => tm.pop_tool_span(ctx, id, "call", false).0,
    }
}

/// Starts and completes the invocation `inv` (agent `root`).
fn complete(tm: &TraceManager, ctx: &CallbackCtx, inv: &str) {
    let other = id(inv, "root");
    tm.invocation_span(ctx, &other, None);
    tm.end_invocation(ctx, &other);
}

#[test]
fn test_trace_manager_late_operations_keep_their_spans() {
    for kind in KINDS {
        let ctx = CallbackCtx::background();
        let tm = TraceManager::new();
        let id = id("completed", "root");
        tm.invocation_span(&ctx, &id, None);
        tm.end_invocation(&ctx, &id);
        let started = push(&tm, &ctx, &id, kind);
        let ended = pop(&tm, &ctx, &id, kind);
        assert!(
            started.span_id == ended.span_id
                && started.parent_span_id == ended.parent_span_id
                && ended.latency.is_some(),
            "{kind}: late operation lost its span identity or latency"
        );
        assert!(
            tracked_invocations(&tm).is_empty(),
            "{kind}: late operation recreated active invocation tracking"
        );
    }
}

#[test]
fn test_trace_manager_end_adopts_trace_before_retaining_identity() {
    let ctx = CallbackCtx::background();
    let tm = TraceManager::new();
    let id = id("completed", "root");
    tm.invocation_span(&ctx, &id, None);
    let trace_id: [u8; 16] = [
        0x4b, 0xf9, 0x2f, 0x35, 0x77, 0xb3, 0x4d, 0xa6, 0xa3, 0xce, 0x92, 0x9d, 0x0e, 0x0e, 0x47,
        0x36,
    ];
    let span_id: [u8; 8] = [0x00, 0xf0, 0x67, 0xaa, 0x0b, 0xa9, 0x02, 0xb7];
    let end_ctx = CallbackCtx::background().with_span(SpanIds { trace_id, span_id });
    let ended = tm.end_invocation(&end_ctx, &id);
    assert_eq!(
        ended.trace_id, "4bf92f3577b34da6a3ce929d0e0e4736",
        "terminal row did not adopt its ambient trace"
    );
    let late = tm.event_span(&ctx, &id, "user", "");
    assert!(
        late.trace_id == ended.trace_id && late.span_id == ended.span_id,
        "late callback lost the trace adopted by the terminal row"
    );
}

// parity: adapted: Go's 32 goroutines are 32 OS threads (`std::thread::scope`).
#[test]
fn test_trace_manager_late_callbacks_do_not_restart_invocation() {
    let ctx = CallbackCtx::background();
    let mut tm = TraceManager::new();
    tm.max_tracked = 1;
    let id = id("completed", "root");
    let started = tm.invocation_span(&ctx, &id, None);
    tm.end_invocation(&ctx, &id);
    let active_id = self::id("active", "root");
    let active = tm.invocation_span(&ctx, &active_id, None);
    std::thread::scope(|s| {
        for _ in 0..32 {
            s.spawn(|| {
                let row = tm.event_span(&ctx, &id, "user", "");
                assert!(
                    row.trace_id == started.trace_id
                        && row.span_id == started.span_id
                        && row.parent_span_id == started.parent_span_id,
                    "late callback changed invocation span identity"
                );
            });
        }
    });
    assert_eq!(
        tracked_invocations(&tm),
        vec![active_id.invocation_id.clone()],
        "late callbacks recreated active invocation state or evicted the live run"
    );
    let got = tm.end_invocation(&ctx, &active_id);
    assert!(
        got.span_id == active.span_id && got.latency.is_some(),
        "late callbacks discarded the live run's span or latency"
    );
}

#[test]
fn test_trace_manager_new_run_reuses_invocation_id() {
    let ctx = CallbackCtx::background();
    let tm = TraceManager::new();
    let id = id("resumed", "root");
    let first = tm.invocation_span(&ctx, &id, None);
    tm.end_invocation(&ctx, &id);
    let second = tm.invocation_span(&ctx, &id, None);
    assert_ne!(
        second.span_id, first.span_id,
        "new run reused the finished invocation's root span"
    );
    assert_eq!(tracked_invocations(&tm).len(), 1, "new run was not tracked");
    let got = tm.end_invocation(&ctx, &id);
    assert!(
        got.span_id == second.span_id && got.latency.is_some(),
        "new run lost its root span or latency"
    );
}

#[test]
fn test_trace_manager_start_restores_evicted_invocation() {
    let ctx = CallbackCtx::background();
    let mut tm = TraceManager::new();
    tm.max_tracked = 1;
    let id = id("evicted", "root");
    let first = tm.invocation_span(&ctx, &id, None);
    tm.invocation_span(&ctx, &self::id("other", ""), None);
    let got = tm.invocation_span(&ctx, &id, None);
    assert!(
        got.span_id == first.span_id && got.trace_id == first.trace_id,
        "start callback discarded the evicted active invocation's identity"
    );
}

#[test]
fn test_trace_manager_completed_tombstones_stay_bounded() {
    let ctx = CallbackCtx::background();
    let mut tm = TraceManager::new();
    tm.max_tracked = 1;
    let clock = ManualClock::new();
    tm.now = clock.clock();
    let mut ids = Vec::new();
    let mut starts = Vec::new();
    for _ in 0..65 {
        let id = id(&clock.now().to_rfc3339(), "root");
        starts.push(tm.invocation_span(&ctx, &id, None));
        tm.end_invocation(&ctx, &id);
        ids.push(id);
        clock.advance_ms(1);
    }
    assert!(
        tm.mu.lock().invocations.is_empty(),
        "completed invocations are still active"
    );
    for (i, id) in ids.iter().enumerate() {
        let got = tm.event_span(&ctx, id, "user", "");
        let retained = got.span_id == starts[i].span_id;
        assert_eq!(
            retained,
            i > 0,
            "completion {i} retained = {retained}, want {}",
            i > 0
        );
    }
}

#[test]
fn test_trace_manager_late_callback_refreshes_retention() {
    let ctx = CallbackCtx::background();
    let mut tm = TraceManager::new();
    tm.max_tracked = 1;
    let id = id("still-receiving", "root");
    let started = tm.invocation_span(&ctx, &id, None);
    tm.end_invocation(&ctx, &id);
    for i in 0..63 {
        complete(&tm, &ctx, &format!("finished-{i}"));
    }
    tm.event_span(&ctx, &id, "user", "");
    complete(&tm, &ctx, "new-completion");
    assert_eq!(
        tm.event_span(&ctx, &id, "user", "").span_id,
        started.span_id,
        "recent late callback did not refresh completed identity retention"
    );
    assert!(
        tracked_invocations(&tm).is_empty(),
        "late callback recreated active invocation tracking"
    );
}

#[test]
fn test_trace_manager_repeated_end_preserves_late_call() {
    let ctx = CallbackCtx::background();
    let tm = TraceManager::new();
    let id = id("completed", "root");
    let started = tm.invocation_span(&ctx, &id, None);
    let first_end = tm.end_invocation(&ctx, &id);
    let tool_start = tm.push_tool_span(&ctx, &id, "call");
    let second_end = tm.end_invocation(&ctx, &id);
    let (tool_end, _, _) = tm.pop_tool_span(&ctx, &id, "call", false);
    assert!(
        second_end.span_id == started.span_id && first_end.span_id == started.span_id,
        "repeated completion changed the invocation's identity"
    );
    assert!(
        second_end.latency.is_none(),
        "repeated completion reported a second invocation latency"
    );
    assert!(
        tool_end.span_id == tool_start.span_id && tool_end.latency.is_some(),
        "repeated completion discarded the open late tool call"
    );
    assert!(
        tracked_invocations(&tm).is_empty(),
        "repeated completion recreated active invocation tracking"
    );
}

#[test]
fn test_trace_manager_completions_preserve_mid_run_eviction() {
    let ctx = CallbackCtx::background();
    let mut tm = TraceManager::new();
    let clock = ManualClock::new();
    tm.now = clock.clock();
    let id = id("still-running", "root");
    let started = tm.invocation_span(&ctx, &id, None);
    clock.advance_ms(ttl_ms(&tm) + 1000);
    tm.mu.lock().inserts = SWEEP_INTERVAL - 1;
    complete(&tm, &ctx, "sweep");
    assert!(
        tm.mu.lock().evicted.contains_key(&id.invocation_id),
        "fixture did not evict the unfinished invocation"
    );
    // Use the reviewed default-capacity workload, independent of maxTracked.
    for i in 0..4096 {
        clock.advance_ms(1);
        complete(&tm, &ctx, &format!("finished-{i}"));
    }
    assert!(
        tm.mu.lock().evicted.contains_key(&id.invocation_id),
        "ordinary completions discarded an unfinished invocation's identity"
    );
    let ended = tm.end_invocation(&ctx, &id);
    assert!(
        ended.trace_id == started.trace_id
            && ended.span_id == started.span_id
            && ended.parent_span_id == started.parent_span_id,
        "completion lost the mid-run evicted invocation's root span"
    );
    assert!(
        ended.latency.is_none(),
        "completion reported a latency after mid-run eviction"
    );
}

/// Go `tm.ttl` in milliseconds.
fn ttl_ms(tm: &TraceManager) -> i64 {
    i64::try_from(tm.ttl.as_millis()).expect("ttl fits i64")
}

#[test]
fn test_trace_manager_open_late_call_survives_completion_pressure() {
    for kind in KINDS {
        let ctx = CallbackCtx::background();
        let mut tm = TraceManager::new();
        tm.max_tracked = 1;
        let clock = ManualClock::new();
        tm.now = clock.clock();
        let id = id("late-operation", "root");
        tm.invocation_span(&ctx, &id, None);
        tm.end_invocation(&ctx, &id);
        let started = push(&tm, &ctx, &id, kind);
        clock.advance_ms(ttl_ms(&tm) + 1000);
        for i in 0..65 {
            complete(&tm, &ctx, &format!("finished-{i}"));
        }
        let ended = pop(&tm, &ctx, &id, kind);
        assert!(
            ended.span_id == started.span_id
                && ended.parent_span_id == started.parent_span_id
                && ended.latency.is_some(),
            "{kind}: completion pressure split an in-flight late operation"
        );
        assert!(
            tracked_invocations(&tm).is_empty(),
            "{kind}: late completion recreated active invocation tracking"
        );
    }
}

// The clock blocks on its second reading as Go's does.
// parity: adapted: Go's goroutine is an OS thread and its channels `std::sync::mpsc`
#[test]
fn test_trace_manager_starting_late_span_survives_completion_pressure() {
    for kind in KINDS {
        let ctx = CallbackCtx::background();
        let mut tm = TraceManager::new();
        tm.max_tracked = 1;
        let id = id("starting-late-span", "root");
        tm.invocation_span(&ctx, &id, None);
        tm.end_invocation(&ctx, &id);
        let (ready_tx, ready_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let ready_tx = parking_lot::Mutex::new(Some(ready_tx));
        let release_rx = parking_lot::Mutex::new(release_rx);
        let clock_calls = AtomicI64::new(0);
        let fixed = Utc.timestamp_opt(1000, 0).unwrap();
        tm.now = Arc::new(move || {
            if clock_calls.fetch_add(1, Ordering::SeqCst) + 1 == 2 {
                if let Some(tx) = ready_tx.lock().take() {
                    let _ = tx.send(());
                }
                let _ = release_rx.lock().recv();
            }
            fixed
        });
        let tm = Arc::new(tm);
        let starter = {
            let tm = Arc::clone(&tm);
            let ctx = ctx.clone();
            let id = id.clone();
            std::thread::spawn(move || push(&tm, &ctx, &id, kind))
        };
        ready_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap_or_else(|_| {
                panic!("{kind}: late span did not reach its start after invocation lookup")
            });
        for i in 0..65 {
            complete(&tm, &ctx, &format!("finished-{i}"));
        }
        let _ = release_tx.send(());
        let span = starter.join().expect("starter thread");
        let ended = pop(&tm, &ctx, &id, kind);
        assert!(
            span.span_id == ended.span_id && ended.latency.is_some(),
            "{kind}: pruning between invocation lookup and span start split the late operation"
        );
        assert!(
            tracked_invocations(&tm).is_empty(),
            "{kind}: late operation returned to active invocation tracking"
        );
    }
}

#[test]
fn test_trace_manager_closed_late_state_can_be_pruned() {
    let ctx = CallbackCtx::background();
    let mut tm = TraceManager::new();
    tm.max_tracked = 1;
    let id = id("idle-late-state", "root");
    let first = tm.invocation_span(&ctx, &id, None);
    tm.end_invocation(&ctx, &id);
    tm.push_tool_span(&ctx, &id, "call");
    tm.pop_tool_span(&ctx, &id, "call", false);
    for i in 0..64 {
        complete(&tm, &ctx, &format!("finished-{i}"));
    }
    assert_ne!(
        tm.event_span(&ctx, &id, "user", "").span_id,
        first.span_id,
        "idle initialized late state was retained beyond completed capacity"
    );
}

#[test]
fn test_trace_manager_full_open_completed_cache_stays_bounded() {
    let ctx = CallbackCtx::background();
    let mut tm = TraceManager::new();
    tm.max_tracked = 1;
    let mut ids = Vec::new();
    let mut spans = Vec::new();
    for i in 0..64 {
        let id = id(&format!("open-{i}"), "root");
        tm.invocation_span(&ctx, &id, None);
        tm.end_invocation(&ctx, &id);
        spans.push(tm.push_tool_span(&ctx, &id, "call"));
        ids.push(id);
    }
    let id = id("new-idle-completion", "root");
    let started = tm.invocation_span(&ctx, &id, None);
    tm.end_invocation(&ctx, &id);
    assert_ne!(
        tm.event_span(&ctx, &id, "user", "").span_id,
        started.span_id,
        "full cache retained a new idle completion beyond its capacity"
    );
    for (i, open_id) in ids.iter().enumerate() {
        let (ended, _, _) = tm.pop_tool_span(&ctx, open_id, "call", false);
        assert!(
            ended.span_id == spans[i].span_id && ended.latency.is_some(),
            "open late operation {i} was discarded to retain an idle completion"
        );
    }
}
