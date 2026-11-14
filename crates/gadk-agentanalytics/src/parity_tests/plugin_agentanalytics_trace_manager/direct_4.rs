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

//! Part of the port of ADK-Go `plugin/agentanalytics/trace_manager_test.go`: the
//! 21 Go tests from `TestLateShortcutAgentRetained` to `TestLateFailureCauseKeepsActivation` in the
//! Go file's order that need no runner (the exact list is in the 7b-3a plan's contracts, Appendix
//! A).
//!
//! Go's `context.Background()` is [`CallbackCtx::background`]; Go's `endedContext{end: true}` is
//! [`ended_ctx`] (an ADK context whose invocation ended); Go's `context.WithCancelCause` is a
//! background context with a cancellation token and its cause. Go's goroutines that race a callback
//! against the end of the run are scoped threads held at the manager's clock ([`gated_clock`]), as
//! Go holds them in `tm.now`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use chrono::{DateTime, Utc};
use gadk_genai::{Content, Part};
use gadk_session::Event;
use parking_lot::Mutex;
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use super::direct_support::{MockRun, bg, err, invocation_of, open_spans};
use crate::event_types::EVENT_AGENT_COMPLETED;
use crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::{
    event_types, rows_of_type, setup_test_plugin, until_invocation_end,
};
use crate::trace::causes::error_is;
use crate::trace::testkit::{ManualClock, custom, ident, llm, sequential};
use crate::trace::{
    CallbackCtx, Clock, Identity, InvocationState, InvocationTrace, RecordedError, RetiredCause,
    RowSpan, SpanKind, SpanRecord, SpanRef, TraceError, TraceManager, llm_key, tool_key,
};

/// Go `&endedContext{Context: ctx, end: true}`: a context whose `Ended` reports true, as for an
/// activation that a `BeforeAgentCallback` answered. The ADK context runs an agent of its own
/// name, so it reads as no running agent's (Go's `endedContext` shows none), and it cannot be
/// cancelled, as Go's `context.Background()` it wraps.
fn ended_ctx() -> CallbackCtx {
    let adk = gadk_agent::Context::builder()
        .agent(custom("ended-context", vec![]))
        .build();
    adk.end_invocation();
    CallbackCtx {
        adk: Some(adk),
        ..CallbackCtx::default()
    }
}

/// Go `context.WithCancelCause(ctx)`: the context and its cancel function's token; cancel it
/// with `token.cancel()` (the cause is `cause`).
fn with_cancel_cause(cause: &TraceError) -> (CallbackCtx, CancellationToken) {
    let token = CancellationToken::new();
    let ctx = bg()
        .with_cancel(token.clone())
        .with_cancel_cause(cause.clone());
    (ctx, token)
}

/// Go `time.Unix(1000, 0)`.
fn unix_1000() -> DateTime<Utc> {
    DateTime::from_timestamp(1000, 0).unwrap()
}

/// Go's `ready` / `release` channels around `tm.now`: a clock that reads `base`, except that its
/// `n`-th reading calls `held`, signals `ready` and waits for `release` (sent or dropped, as
/// Go's `t.Cleanup(unblock)`), then returns what `held` returned.
fn gated_clock(
    n: i64,
    base: Clock,
    held: impl Fn() -> DateTime<Utc> + Send + Sync + 'static,
) -> (Clock, Receiver<()>, Sender<()>) {
    let (ready_tx, ready_rx) = channel::<()>();
    let (release_tx, release_rx) = channel::<()>();
    let ready_tx = Mutex::new(ready_tx);
    let release_rx = Mutex::new(release_rx);
    let calls = AtomicI64::new(0);
    let clock: Clock = Arc::new(move || {
        if calls.fetch_add(1, Ordering::SeqCst) + 1 == n {
            let at = held();
            let _ = ready_tx.lock().send(());
            let _ = release_rx.lock().recv();
            return at;
        }
        base()
    });
    (clock, ready_rx, release_tx)
}

/// Go's `select { case <-ready: case <-time.After(5 * time.Second): t.Fatal(msg) }`.
fn wait_ready(ready: &Receiver<()>, msg: &str) {
    ready
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or_else(|_| panic!("{msg}"));
}

/// Go `straddleEndOfRun(t, tm, id, callback)`: runs `callback` on another thread, holds it once
/// it has looked up the invocation of `id` (the second reading of the clock), ends the run, and
/// then lets `callback` go on; returns what it returned.
fn straddle_end_of_run<R: Send>(
    tm: &mut TraceManager,
    id: &Identity,
    callback: impl FnOnce(&TraceManager) -> R + Send,
) -> R {
    let (clock, ready, release) = gated_clock(2, Arc::new(Utc::now), Utc::now);
    tm.now = clock;
    let tm = &*tm;
    std::thread::scope(|s| {
        let handle = s.spawn(move || callback(tm));
        wait_ready(&ready, "callback did not look up its invocation");
        tm.end_invocation(&bg(), id);
        drop(release);
        handle.join().unwrap()
    })
}

/// Go `lateOpenCalls(t, tm, id)`: the model and tool calls that the late state of the completed
/// run `id` holds open, and whether it has open spans.
fn late_open_calls(tm: &TraceManager, id: &Identity) -> (usize, bool) {
    let late = {
        let st = tm.mu.lock();
        let c = st
            .completed
            .get(&id.invocation_id)
            .unwrap_or_else(|| panic!("run {:?} is not completed", id.invocation_id));
        c.late.clone()
    };
    let Some(late) = late else {
        return (0, false);
    };
    let has_open_spans = late.has_open_late_spans();
    let st = late.mu.lock();
    (st.llm_spans.len() + st.tool_spans.len(), has_open_spans)
}

/// Whether the trace manager keeps a completed entry of `invocation_id`.
fn is_completed(tm: &TraceManager, invocation_id: &str) -> bool {
    tm.mu.lock().completed.contains_key(invocation_id)
}

/// Go `lateReviewEnded(tm, invID)`: completes the run of `inv` with nothing open.
fn late_review_ended(tm: &TraceManager, inv: &str) {
    let live = ident(inv, "live_agent", "", "");
    tm.invocation_span(&bg(), &live, None);
    tm.end_invocation(&bg(), &live);
}

/// Go `lateReviewTrace(t, tm, invID)`: the late state of the completed run `inv`.
fn late_review_trace(tm: &TraceManager, inv: &str) -> Arc<InvocationTrace> {
    let st = tm.mu.lock();
    st.completed
        .get(inv)
        .and_then(|c| c.late.clone())
        .unwrap_or_else(|| panic!("completed run {inv:?} has no late state"))
}

/// Go `lateReviewStack(t, tm, invID)`: the names of the activations on the late agent stack.
fn late_review_stack(tm: &TraceManager, inv: &str) -> Vec<String> {
    let late = late_review_trace(tm, inv);
    let st = late.mu.lock();
    st.agent_stack[1..]
        .iter()
        .map(|r| r.lock().name.clone())
        .collect()
}

/// Go `lateReviewAgent(t, tm, invID, name)`: the record of the late activation `name`.
fn late_review_agent(tm: &TraceManager, inv: &str, name: &str) -> SpanRef {
    let late = late_review_trace(tm, inv);
    let st = late.mu.lock();
    st.agent_stack[1..]
        .iter()
        .find(|r| r.lock().name == name)
        .cloned()
        .unwrap_or_else(|| panic!("no activation {name:?} on the late agentStack of {inv:?}"))
}

/// Go `checkLateRecordDropped(t, late, rec)`.
fn check_late_record_dropped(late: &InvocationTrace, rec: &SpanRef) {
    let st = late.mu.lock();
    let r = rec.lock();
    assert!(
        !st.agent_stack.iter().any(|s| s.is_record(&r)),
        "activation {:?} is still on the late agentStack",
        r.name
    );
    assert!(
        r.canceled.is_none(),
        "retired late activation {:?} keeps the context of its callback",
        r.name
    );
    assert!(
        r.ended.is_none() && r.activation.is_none(),
        "retired late activation {:?} keeps its InvocationContext",
        r.name
    );
    assert!(
        r.parent.is_none(),
        "retired late activation {:?} keeps the span that it ran under",
        r.name
    );
    assert!(
        r.retired_causes.is_empty(),
        "retired late activation {:?} keeps a failure cause ({:?})",
        r.name,
        r.first_retired_cause()
    );
}

/// `end.span_id == start.span_id && end.latency != nil`.
fn assert_ends(end: &RowSpan, start: &RowSpan, what: &str) {
    assert!(
        end.span_id == start.span_id && end.latency.is_some(),
        "{what} place = {end:?}, want the span {:?} of its start with latency",
        start.span_id
    );
}

// parity: adapted: Go's endedContext is an ADK context whose invocation ended (`ended_ctx`)
#[test]
fn test_late_shortcut_agent_retained() {
    let mut tm = TraceManager::new();
    tm.max_tracked = 1; // Completed runs are kept up to 64 (see remember_completed_locked).
    let inv = "inv-late-shortcut";
    let live = ident(inv, "live_agent", "", "");
    let call = tool_key(&live, "call-1", "run_cached");

    tm.invocation_span(&bg(), &live, None);
    tm.push_agent_span(&bg(), &live);
    tm.push_tool_span(&bg(), &live, &call);
    tm.pop_agent_span(&bg(), &live);
    tm.end_invocation(&bg(), &live);

    let branch_a = ident(inv, "branch_a", "branch_a", "");
    let cached = ident(inv, "cached", "branch_a", "");
    tm.push_agent_span(&bg(), &branch_a);
    tm.push_agent_span(&ended_ctx(), &cached);
    let cached_rec = late_review_agent(&tm, inv, "cached");
    tm.pop_agent_span(&bg(), &branch_a);
    assert_eq!(
        late_review_stack(&tm, inv),
        vec!["cached"],
        "late agents after branch_a ended, want [cached], held back by the open tool call"
    );
    tm.pop_tool_span(&bg(), &live, &call, false);

    let late = late_review_trace(&tm, inv);
    let got = late_review_stack(&tm, inv);
    assert!(
        got.is_empty(),
        "late agents after the tool call returned = {got:?}, want none"
    );
    assert!(
        !late.open_agents.load(Ordering::SeqCst),
        "late.openAgents = true after the tool call returned, want false"
    );
    check_late_record_dropped(&late, &cached_rec);
    for i in 0..64 {
        let idle = ident(&format!("idle-{i}"), "root", "", "");
        tm.invocation_span(&bg(), &idle, None);
        tm.end_invocation(&bg(), &idle);
    }
    assert!(
        !is_completed(&tm, inv),
        "completed run with nothing open in its late state outlived the 64 idle runs that \
         completed after it, want it pruned first"
    );
}

#[test]
fn test_trace_manager_complete_response_ends_only_its_authors_model_calls() {
    let tm = TraceManager::new();
    let sub = llm("sub", vec![], gadk_agent::Mode::default());
    let coord = sequential("coord", vec![sub]);

    let coord_id = ident("inv-settled-llm", "coord", "", "coord@0");
    let sub_id = ident("inv-settled-llm", "sub", "", "coord@0/sub@1");

    tm.invocation_span(&bg(), &coord_id, Some(&coord));
    tm.push_agent_span(&bg(), &coord_id);
    tm.push_llm_span(&bg(), &coord_id);
    tm.push_agent_span(&bg(), &sub_id);
    let start = tm.push_llm_span(&bg(), &sub_id);

    // coord emits a final response settling both coord and sub.
    tm.complete_response(&bg(), &coord_id, "coord", "", &[]);

    let got = open_spans(&tm, "inv-settled-llm");
    assert_eq!(
        got.models, 1,
        "open model spans after completeResponse, want 1, the call of sub"
    );
    assert_ends(
        &tm.pop_llm_span(&bg(), &sub_id, true),
        &start,
        "LLM_RESPONSE of sub",
    );
    let (unclosed, term_err) = tm.pop_unclosed_agent_spans(&bg(), &coord_id);
    assert!(
        term_err.is_none() && unclosed.is_empty(),
        "popUnclosedAgentSpans = ({unclosed:?}, {term_err:?}), want (nil, nil)"
    );
}

#[test]
fn test_trace_manager_stopped_stream_does_not_pin_completed_run() {
    for (name, event_path) in [
        ("event without node path", ""),
        ("event with node path", "bot"),
    ] {
        let mut tm = TraceManager::new();
        tm.max_tracked = 1;
        let limit = tm.max_tracked.max(64);
        for i in 0..5 * limit {
            let id = ident(&format!("stopped-{i}"), "bot", "", "");
            tm.invocation_span(&bg(), &id, None);
            tm.push_agent_span(&bg(), &id);
            tm.push_llm_span(&bg(), &id);
            tm.record_first_token(&bg(), &id);
            let mut ev_id = id.clone();
            ev_id.path = event_path.into();
            tm.record_streamed(&bg(), &ev_id, "bot", "", true);
            let (unclosed, err) = tm.pop_unclosed_agent_spans(&bg(), &id);
            assert!(
                err.is_none() && unclosed.is_empty(),
                "{name}: popUnclosedAgentSpans = ({unclosed:?}, {err:?}), want no failure"
            );
            tm.end_invocation(&bg(), &id);
        }

        let st = tm.mu.lock();
        assert!(
            st.completed.len() <= limit,
            "{name}: completed runs = {}, want at most {limit}",
            st.completed.len()
        );
        for (id, c) in &st.completed {
            assert!(
                c.late.is_none(),
                "{name}: completed run {id:?} kept late state for a stream that its caller \
                 stopped reading"
            );
        }
    }
}

#[test]
fn test_trace_manager_partial_event_streams_only_its_authors_call() {
    let tm = TraceManager::new();
    let root = ident("two-calls", "root", "", "");
    let streaming = ident("two-calls", "streaming", "", "");
    let waiting = ident("two-calls", "waiting", "", "");
    tm.invocation_span(&bg(), &root, None);
    tm.push_agent_span(&bg(), &root);
    tm.push_agent_span(&bg(), &streaming);
    tm.push_agent_span(&bg(), &waiting);
    tm.push_llm_span(&bg(), &streaming);
    let start = tm.push_llm_span(&bg(), &waiting);
    let mut ev_id = streaming.clone();
    ev_id.path = streaming.agent_name.clone();
    tm.record_streamed(&bg(), &ev_id, &streaming.agent_name, "", true);
    tm.end_invocation(&bg(), &root);

    let late = tm
        .mu
        .lock()
        .completed
        .get(&root.invocation_id)
        .and_then(|c| c.late.clone())
        .expect("finished run kept no late state for the open call that did not stream");
    let mut kept: Vec<String> = late.mu.lock().llm_spans.keys().cloned().collect();
    kept.sort();
    assert_eq!(
        kept,
        vec![llm_key(&waiting)],
        "model calls kept for late callbacks"
    );
    assert_ends(
        &tm.pop_llm_span(&bg(), &waiting, true),
        &start,
        "LLM_RESPONSE",
    );
}

/// `endWithOpenTool` of `TestTraceManager_CompletedRunsWithOpenLateCallsStayBounded`: runs `id`
/// until its tool call starts and then ends it.
fn end_with_open_tool(tm: &TraceManager, id: &Identity) -> RowSpan {
    tm.invocation_span(&bg(), id, None);
    tm.push_agent_span(&bg(), id);
    let span = tm.push_tool_span(&bg(), id, &tool_key(id, "call", "lookup"));
    tm.end_invocation(&bg(), id);
    span
}

// parity: adapted: Go's goroutine held in `tm.now` is a scoped thread held in `gated_clock`
#[test]
fn test_trace_manager_completed_runs_with_open_late_calls_stay_bounded() {
    // open calls beyond capacity
    {
        let mut tm = TraceManager::new();
        tm.max_tracked = 1;
        let limit = tm.max_tracked.max(64);
        let mut ids = Vec::new();
        for i in 0..limit + 6 {
            let id = ident(&format!("open-{i}"), "live", "", "");
            end_with_open_tool(&tm, &id);
            ids.push(id);
        }
        let got = tm.mu.lock().completed.len();
        assert_eq!(got, limit, "completed runs");
        assert!(
            !is_completed(&tm, &ids[0].invocation_id)
                && is_completed(&tm, &ids[ids.len() - 1].invocation_id),
            "capacity pruning did not drop the least recently active run first"
        );
    }

    // late call outlives ordinary completions
    {
        let mut tm = TraceManager::new();
        tm.max_tracked = 1;
        let limit = tm.max_tracked.max(64);
        let live = ident("live", "live", "", "");
        let start = end_with_open_tool(&tm, &live);
        for i in 0..limit + 6 {
            let other = ident(&format!("finished-{i}"), "root", "", "");
            tm.invocation_span(&bg(), &other, None);
            tm.end_invocation(&bg(), &other);
        }
        let (end, _, _) = tm.pop_tool_span(&bg(), &live, &tool_key(&live, "call", "lookup"), false);
        assert_ends(&end, &start, "TOOL_COMPLETED");
    }

    // late state in which a callback is starting a span
    {
        let mut tm = TraceManager::new();
        tm.max_tracked = 1;
        let limit = tm.max_tracked.max(64);
        let id = ident("starting-late-span", "root", "", "");
        tm.invocation_span(&bg(), &id, None);
        tm.end_invocation(&bg(), &id);
        // The second reading of the clock is the start of the late span, after its lookup
        // reserved the late state.
        let (clock, ready, release) = gated_clock(2, Arc::new(unix_1000), unix_1000);
        tm.now = clock;
        let tm = &tm;
        let span = std::thread::scope(|s| {
            let started = s.spawn(|| tm.push_tool_span(&bg(), &id, "call"));
            wait_ready(
                &ready,
                "late span did not reach its start after invocation lookup",
            );
            for i in 0..limit + 1 {
                end_with_open_tool(tm, &ident(&format!("open-{i}"), "live", "", ""));
            }
            drop(release);
            started.join().unwrap()
        });
        let (end, _, _) = tm.pop_tool_span(&bg(), &id, "call", false);
        assert_ends(&end, &span, "TOOL_COMPLETED");
    }
}

// parity: adapted: Go's goroutines held in `tm.now` are scoped threads held in `gated_clock`
#[test]
fn test_trace_manager_callbacks_across_end_of_run() {
    // start runs id with an agent span open.
    let start = |tm: &TraceManager, id: &Identity| {
        tm.invocation_span(&bg(), id, None);
        tm.push_agent_span(&bg(), id);
    };
    let id = ident("straddle", "live", "", "");
    let assert_no_late_calls = |tm: &TraceManager, what: &str| {
        let (open, has_open_spans) = late_open_calls(tm, &id);
        assert!(
            open == 0 && !has_open_spans,
            "{what}: late open calls = {open} (open spans {has_open_spans}), want 0"
        );
    };

    // tool call ends
    {
        let mut tm = TraceManager::new();
        tm.max_tracked = 1;
        let key = tool_key(&id, "call", "lookup");
        start(&tm, &id);
        let begin = tm.push_tool_span(&bg(), &id, &key);
        let (end, _, _) =
            straddle_end_of_run(&mut tm, &id, |tm| tm.pop_tool_span(&bg(), &id, &key, false));
        assert_ends(&end, &begin, "TOOL_COMPLETED");
        assert_no_late_calls(&tm, "tool call ends");
        // Ordinary completions beyond capacity prune the run first, as it waits for no call.
        for i in 0..tm.max_tracked.max(64) {
            let other = ident(&format!("finished-{i}"), "root", "", "");
            tm.invocation_span(&bg(), &other, None);
            tm.end_invocation(&bg(), &other);
        }
        assert!(
            !is_completed(&tm, &id.invocation_id),
            "capacity pruning kept the run whose call ended across AfterRun over ordinary \
             completions"
        );
    }

    // tool call fails, then AfterToolCallback
    {
        let mut tm = TraceManager::new();
        let key = tool_key(&id, "call", "lookup");
        start(&tm, &id);
        let begin = tm.push_tool_span(&bg(), &id, &key);
        let (failed, _, _) =
            straddle_end_of_run(&mut tm, &id, |tm| tm.pop_tool_span(&bg(), &id, &key, true));
        assert_ends(&failed, &begin, "TOOL_ERROR");
        let (_, _, closed_by_error) = tm.pop_tool_span(&bg(), &id, &key, false);
        assert!(
            closed_by_error,
            "AfterToolCallback did not find the call that OnToolErrorCallback ended, so it \
             writes a second TOOL_ERROR row"
        );
        assert_no_late_calls(&tm, "tool call fails");
    }

    // call of an unknown tool fails, then AfterToolCallback
    {
        let mut tm = TraceManager::new();
        let key = tool_key(&id, "call", "missing");
        start(&tm, &id);
        let (failed, _, _) =
            straddle_end_of_run(&mut tm, &id, |tm| tm.pop_tool_span(&bg(), &id, &key, true));
        let (completed, _, closed_by_error) = tm.pop_tool_span(&bg(), &id, &key, false);
        assert!(
            closed_by_error && completed.span_id == failed.span_id,
            "AfterToolCallback = ({completed:?}, {closed_by_error}), want the span {:?} of \
             TOOL_ERROR, closed by the error",
            failed.span_id
        );
    }

    // tool call starts, then ends
    {
        let mut tm = TraceManager::new();
        let key = tool_key(&id, "call", "lookup");
        start(&tm, &id);
        let begin = straddle_end_of_run(&mut tm, &id, |tm| tm.push_tool_span(&bg(), &id, &key));
        let (end, _, _) = tm.pop_tool_span(&bg(), &id, &key, false);
        assert_ends(&end, &begin, "TOOL_COMPLETED");
        assert_no_late_calls(&tm, "tool call starts");
    }

    // model call ends
    {
        let mut tm = TraceManager::new();
        start(&tm, &id);
        let begin = tm.push_llm_span(&bg(), &id);
        let end = straddle_end_of_run(&mut tm, &id, |tm| tm.pop_llm_span(&bg(), &id, true));
        assert_ends(&end, &begin, "LLM_RESPONSE");
        assert_no_late_calls(&tm, "model call ends");
    }

    // model call starts, then ends
    {
        let mut tm = TraceManager::new();
        start(&tm, &id);
        let begin = straddle_end_of_run(&mut tm, &id, |tm| tm.push_llm_span(&bg(), &id));
        assert_ends(&tm.pop_llm_span(&bg(), &id, true), &begin, "LLM_RESPONSE");
        assert_no_late_calls(&tm, "model call starts");
    }

    // agent starts, then ends
    {
        let mut tm = TraceManager::new();
        let child = ident("straddle", "child", "", "");
        start(&tm, &id);
        let (begin, _) = straddle_end_of_run(&mut tm, &id, |tm| tm.push_agent_span(&bg(), &child));
        assert_ends(&tm.pop_agent_span(&bg(), &child), &begin, "AGENT_COMPLETED");
        assert_no_late_calls(&tm, "agent starts");
    }

    // first token arrives while the call ends
    {
        // A fake clock: start (t0 = 0 ms), first token (t1 = 10 ms), end of the run (t2 =
        // 50 ms) and end of the call (t3 = 60 ms). The first token that straddles
        // end_invocation is recorded on the late state with its captured t1, so
        // time_to_first_token_ms == 10 and total_ms == 60.
        let mut tm = TraceManager::new();
        let base = unix_1000();
        let t0 = base;
        let t1 = base + chrono::Duration::milliseconds(10);
        let t2 = base + chrono::Duration::milliseconds(50);
        let t3 = base + chrono::Duration::milliseconds(60);
        let clock = Arc::new(Mutex::new(t0));
        let plain: Clock = {
            let clock = clock.clone();
            Arc::new(move || *clock.lock())
        };
        tm.now = plain.clone();
        start(&tm, &id);
        let begin = tm.push_llm_span(&bg(), &id);

        // Hold the second clock reading (the one in record_first_token after the invocation
        // lookup), end the run, then release.
        let held = {
            let clock = clock.clone();
            move || {
                *clock.lock() = t1; // first token time
                t1
            }
        };
        let (gated, ready, release) = gated_clock(2, plain.clone(), held);
        tm.now = gated;
        {
            let tm = &tm;
            std::thread::scope(|s| {
                s.spawn(|| tm.record_first_token(&bg(), &id));
                wait_ready(
                    &ready,
                    "recordFirstToken did not read clock after looking up its invocation",
                );
                *clock.lock() = t2; // end_invocation time
                tm.end_invocation(&bg(), &id);
                drop(release);
            });
        }
        *clock.lock() = t3; // pop_llm_span time
        tm.now = plain;
        let end = tm.pop_llm_span(&bg(), &id, true);
        assert_ends(&end, &begin, "LLM_RESPONSE");
        let latency = end.latency.unwrap();
        assert!(
            latency.time_to_first_token_ms == Some(10) && latency.total_ms == 60,
            "latency = {latency:?}, want time_to_first_token_ms 10 and total_ms 60"
        );
    }

    // model call fails, then AfterModelCallback
    {
        let mut tm = TraceManager::new();
        start(&tm, &id);
        let begin = tm.push_llm_span(&bg(), &id);
        let failed = straddle_end_of_run(&mut tm, &id, |tm| tm.pop_llm_span(&bg(), &id, false));
        assert_ends(&failed, &begin, "LLM_ERROR");
        // A plugin recovered from the error, so ADK runs AfterModelCallback.
        let end = tm.pop_llm_span(&bg(), &id, true);
        assert!(
            end.span_id == begin.span_id && end.latency.is_none(),
            "LLM_RESPONSE place = {end:?}, want the span {:?} of LLM_REQUEST without latency",
            begin.span_id
        );
        assert_no_late_calls(&tm, "model call fails");
    }
}

// parity: adapted: Go's two goroutines released together are two scoped threads behind a barrier
#[test]
fn test_trace_manager_streamed_event_racing_end_of_run_shares_no_call() {
    let tm = TraceManager::new();
    for i in 0..200 {
        let id = ident(&format!("streamed-race-{i}"), "live", "", "");
        tm.invocation_span(&bg(), &id, None);
        tm.push_agent_span(&bg(), &id);
        tm.push_llm_span(&bg(), &id);
        let start = std::sync::Barrier::new(2);
        std::thread::scope(|s| {
            s.spawn(|| {
                start.wait();
                tm.record_streamed(&bg(), &id, &id.agent_name, &id.branch, true);
            });
            s.spawn(|| {
                start.wait();
                tm.end_invocation(&bg(), &id);
                let late = tm
                    .mu
                    .lock()
                    .completed
                    .get(&id.invocation_id)
                    .expect("completed run")
                    .late
                    .clone();
                let Some(late) = late else {
                    // record_streamed ran first, so the streaming call did not move.
                    return;
                };
                late.mu.lock().open_call_owners_locked(None);
                tm.pop_llm_span(&bg(), &id, true);
            });
        });
    }
}

/// `holdsNoContext` of `TestTraceManager_LateStateKeepsNoCallbackContext`: asserts that no call
/// in `calls` keeps a context of its callback, and returns the number of calls.
fn holds_no_context(where_: &str, calls: Option<&HashMap<String, SpanRef>>) -> usize {
    let Some(calls) = calls else {
        return 0;
    };
    for (key, rec) in calls {
        let r = rec.lock();
        assert!(
            r.canceled.is_none(),
            "{where_} call {key:?} keeps the context of its callback"
        );
        assert!(
            r.ended.is_none() && r.activation.is_none(),
            "{where_} call {key:?} keeps the InvocationContext of an activation"
        );
        assert!(
            r.parent.is_none(),
            "{where_} call {key:?} keeps the span that it started under"
        );
        assert!(
            r.retired_causes.is_empty() && r.open_failure_cause().is_none(),
            "{where_} call {key:?} keeps a failure cause ({:?})",
            r.open_failure_cause()
        );
    }
    calls.len()
}

// parity: adapted: Go's `ended` closure / `activation` pointer of a record are an ADK context and
// its activation ID
#[test]
fn test_trace_manager_late_state_keeps_no_callback_context() {
    // late state
    {
        let tm = TraceManager::new();
        let live = ident("late-context", "live", "", "");
        let failed = ident("late-context", "failed", "", "");
        tm.invocation_span(&bg(), &live, None);
        let inv = tm.invocation(&bg(), &live);
        // with_activation gives the record of a call what the record of an activation keeps of
        // its run's InvocationContext, so that the test sees the call drop it too.
        let adk = gadk_agent::Context::builder()
            .agent(custom("live", vec![]))
            .build();
        let with_activation = |call: &dyn Fn(&InvocationState) -> SpanRef| {
            let st = inv.mu.lock();
            let rec = call(&st);
            let mut r = rec.lock();
            r.ended = Some(adk.clone());
            r.activation = Some(adk.activation_id());
        };
        let open_key = tool_key(&live, "open", "lookup");
        let failed_key = tool_key(&live, "failed", "lookup");
        tm.push_agent_span(&bg(), &live);
        tm.push_llm_span(&bg(), &live);
        tm.push_tool_span(&bg(), &live, &open_key);
        tm.push_tool_span(&bg(), &live, &failed_key);
        with_activation(&|st| st.tool_spans[&failed_key].clone());
        tm.pop_tool_span(&bg(), &live, &failed_key, true);
        tm.push_agent_span(&bg(), &failed);
        tm.push_llm_span(&bg(), &failed);
        with_activation(&|st| st.llm_spans[&llm_key(&failed)].clone());
        tm.pop_llm_span(&bg(), &failed, false);
        with_activation(&|st| st.llm_spans[&llm_key(&live)].clone());
        with_activation(&|st| st.tool_spans[&open_key].clone());
        inv.mu.lock().tool_spans[&open_key].lock().retired_causes = vec![RetiredCause {
            err: Some(err("retired branch failed")),
            name: "branch".into(),
            ..RetiredCause::default()
        }];
        tm.end_invocation(&bg(), &live);

        let late = tm
            .mu
            .lock()
            .completed
            .get(&live.invocation_id)
            .and_then(|c| c.late.clone())
            .expect("the run's open calls did not move to the late state");
        let st = late.mu.lock();
        let got = holds_no_context("open model", Some(&st.llm_spans))
            + holds_no_context("open tool", Some(&st.tool_spans))
            + holds_no_context("ended model", Some(&st.completed_models))
            + holds_no_context("ended tool", Some(&st.completed_tools));
        assert_eq!(got, 4, "late calls");
    }

    // ended calls waiting in the completed entry
    {
        let tm = TraceManager::new();
        let id = ident("ended-context", "bot", "", "");
        tm.invocation_span(&bg(), &id, None);
        tm.push_agent_span(&bg(), &id);
        tm.push_tool_span(&bg(), &id, &tool_key(&id, "call", "lookup"));
        tm.pop_tool_span(&bg(), &id, &tool_key(&id, "call", "lookup"), true);
        tm.push_llm_span(&bg(), &id);
        tm.pop_llm_span(&bg(), &id, false);
        tm.end_invocation(&bg(), &id);

        let st = tm.mu.lock();
        let completed = st
            .completed
            .get(&id.invocation_id)
            .filter(|c| c.late.is_none())
            .expect(
                "a run that left only ended calls behind did not keep them in its completed \
                 entry, without late state",
            );
        let got = holds_no_context("ended model", completed.ended_models.as_ref())
            + holds_no_context("ended tool", completed.ended_tools.as_ref());
        assert_eq!(got, 2, "ended calls");
    }

    // span made up for a call that was not seen to start
    {
        let tm = TraceManager::new();
        let id = ident("made-up-context", "bot", "", "");
        tm.invocation_span(&bg(), &id, None);
        tm.push_agent_span(&bg(), &id);
        // ADK runs OnToolErrorCallback alone for a call of a tool that does not exist.
        tm.pop_tool_span(&bg(), &id, &tool_key(&id, "call", "missing"), true);

        let inv = tm.invocation(&bg(), &id);
        let st = inv.mu.lock();
        assert_eq!(
            holds_no_context("made-up", Some(&st.completed_tools)),
            1,
            "made-up calls"
        );
    }
}

#[test]
fn test_trace_manager_tool_end_before_end_of_run_returns_purged_activations() {
    let tm = TraceManager::new();
    let coord = ident("purged-late", "coordinator", "", "");
    let helper = ident("purged-late", "helper", "helper@1", "/helper@1");
    let key = tool_key(&coord, "call", "helper");
    tm.invocation_span(&bg(), &coord, None);
    tm.push_agent_span(&bg(), &coord);
    tm.push_tool_span(&bg(), &coord, &key);
    tm.push_agent_span(&bg(), &helper);
    let (tool_err, purged, closed_by_error) = tm.pop_tool_span(&bg(), &coord, &key, true);
    assert!(
        !closed_by_error,
        "OnToolErrorCallback reported closedByError = true"
    );
    assert!(
        purged.len() == 1 && purged[0].agent_name == "helper" && purged[0].err.is_some(),
        "activations purged on popToolSpan = {purged:?}, want the helper that the tool call \
         ran, with an error"
    );
    tm.end_invocation(&bg(), &coord);

    {
        let st = tm.mu.lock();
        let c = st.completed.get(&coord.invocation_id);
        assert!(
            c.is_some_and(
                |c| c.late.is_none() && c.ended_tools.as_ref().map_or(0, HashMap::len) == 1
            ),
            "completed entry, want nil late state with 1 lightweight endedTools entry"
        );
    }
    let (end, again, closed_by_error) = tm.pop_tool_span(&bg(), &coord, &key, false);
    assert!(
        closed_by_error && end.span_id == tool_err.span_id && again.is_empty(),
        "late AfterToolCallback = ({end:?}, {again:?}, {closed_by_error}), want span {:?} with \
         no purged entries and closedByError = true",
        tool_err.span_id
    );
}

#[test]
fn test_trace_manager_runs_with_only_ended_calls_keep_no_late_state() {
    let tm = TraceManager::new();
    let runs = 2000;
    let mut last = None;
    for i in 0..runs {
        let id = ident(&format!("failed-{i}"), "bot", "", "");
        tm.invocation_span(&bg(), &id, None);
        tm.push_agent_span(&bg(), &id);
        let tool = tool_key(&id, "call", "lookup");
        tm.push_tool_span(&bg(), &id, &tool);
        let (tool_error, _, _) = tm.pop_tool_span(&bg(), &id, &tool, true);
        let begin = tm.push_llm_span(&bg(), &id);
        tm.record_error(&bg(), &id, &llm_key(&id), err("model unavailable"));
        tm.pop_llm_span(&bg(), &id, false);
        let _ = tm.pop_unclosed_agent_spans(&bg(), &id);
        tm.end_invocation(&bg(), &id);
        last = Some((id, begin, tool, tool_error));
    }
    let (last, last_begin, last_tool, last_tool_error) = last.unwrap();

    let with_late_state = tm
        .mu
        .lock()
        .completed
        .values()
        .filter(|c| c.late.is_some())
        .count();
    assert_eq!(
        with_late_state, 0,
        "completed runs with late state (of {runs})"
    );
    // A plugin that recovered from the error late makes ADK run AfterModelCallback after
    // AfterRun.
    let end = tm.pop_llm_span(&bg(), &last, true);
    assert!(
        end.span_id == last_begin.span_id && end.latency.is_none(),
        "LLM_RESPONSE place = {end:?}, want the span {:?} of LLM_REQUEST without latency",
        last_begin.span_id
    );
    let (end, _, closed_by_error) = tm.pop_tool_span(&bg(), &last, &last_tool, false);
    assert!(
        closed_by_error && end.span_id == last_tool_error.span_id,
        "AfterToolCallback = ({end:?}, {closed_by_error}), want the span {:?} of TOOL_ERROR, \
         closed by the error",
        last_tool_error.span_id
    );
}

// parity: adapted: Go's shared `mCtx` is a `MockRun` (one activation for all its callbacks)
#[tokio::test]
async fn test_on_event_finish_task_of_agent_without_span_writes_no_agent_completed() {
    let tp = setup_test_plugin().await;
    let run = MockRun::new(&tp.ctx);
    (tp.plugin.before_agent_callback().unwrap())(run.ctx())
        .await
        .unwrap();
    let mut ev = Event::new(
        &gadk_platform::Platform::default(),
        tp.ctx.invocation_id.clone(),
    );
    ev.author = "unstarted_helper".into();
    let response: Map<String, Value> = [("result".to_owned(), json!("Task completed."))]
        .into_iter()
        .collect();
    ev.response.content = Some(Content::from_parts(
        vec![Part::from_function_response("finish_task", response)],
        "user",
    ));
    (tp.plugin.on_event_callback().unwrap())(run.inv(), ev)
        .await
        .unwrap();
    (tp.plugin.after_run_callback().unwrap())(run.inv()).await;

    let rows = tp.collect_rows(until_invocation_end).await;
    let n = rows_of_type(&rows, EVENT_AGENT_COMPLETED).len();
    assert_eq!(
        n,
        0,
        "AGENT_COMPLETED rows; event types: {:?}",
        event_types(&rows)
    );
}

// parity: adapted: Go's goroutine held in `tm.now` is a scoped thread held in `gated_clock`
#[test]
fn test_trace_manager_live_to_late_pending_reservation_survives_completion_pressure() {
    for (name, stuck, tool) in [
        ("tool_call_ordinary_pressure", false, true),
        ("tool_call_open_late_pressure", true, true),
        ("model_call_ordinary_pressure", false, false),
        ("model_call_open_late_pressure", true, false),
    ] {
        let begin_call = |tm: &TraceManager, id: &Identity| {
            if tool {
                tm.push_tool_span(&bg(), id, &tool_key(id, "call-1", "lookup"))
            } else {
                tm.push_llm_span(&bg(), id)
            }
        };
        let complete_call = |tm: &TraceManager, id: &Identity| {
            if tool {
                tm.pop_tool_span(&bg(), id, &tool_key(id, "call-1", "lookup"), false)
                    .0
            } else {
                tm.pop_llm_span(&bg(), id, true)
            }
        };

        let mut tm = TraceManager::new();
        tm.max_tracked = 1;
        let handoff = ident("inv-handoff", "live", "", "");
        let inv_start = tm.invocation_span(&bg(), &handoff, None);
        let (clock, ready, release) = gated_clock(2, Arc::new(Utc::now), Utc::now);
        tm.now = clock;
        let tm = &tm;
        let begin = std::thread::scope(|s| {
            let started = s.spawn(|| begin_call(tm, &handoff));
            wait_ready(&ready, "callback did not pause after lookupInvocation");

            // Move inv-handoff to completed while its live pending_spans == 1, then flood
            // completed past max(max_tracked, 64) = 64 before the paused callback reaches
            // lock_live().
            tm.end_invocation(&bg(), &handoff);
            for i in 0..70 {
                let other = ident(&format!("other-{i}"), "root", "", "");
                tm.invocation_span(&bg(), &other, None);
                if stuck {
                    tm.push_tool_span(&bg(), &other, &tool_key(&other, "stuck", "bg"));
                }
                tm.end_invocation(&bg(), &other);
            }
            {
                let st = tm.mu.lock();
                assert!(
                    !st.invocations.contains_key(&handoff.invocation_id),
                    "{name}: inv-handoff remained in invocations after endInvocation"
                );
                assert!(
                    st.completed.contains_key(&handoff.invocation_id),
                    "{name}: inv-handoff was pruned from completed while live pendingSpans was \
                     held"
                );
            }
            drop(release);
            started.join().unwrap()
        });

        assert!(
            begin.trace_id == inv_start.trace_id && begin.parent_span_id == inv_start.span_id,
            "{name}: start span = {begin:?}, want traceID={:?} parentSpanID={:?}",
            inv_start.trace_id,
            inv_start.span_id
        );
        assert_ends(&complete_call(tm, &handoff), &begin, name);

        // Once both live pending and late open calls are 0, later completions prune
        // inv-handoff normally.
        for i in 0..65 {
            let later = ident(&format!("later-{i}"), "root", "", "");
            tm.invocation_span(&bg(), &later, None);
            tm.end_invocation(&bg(), &later);
        }
        assert!(
            !is_completed(tm, &handoff.invocation_id),
            "{name}: inv-handoff remained in completed after its late call finished and \
             capacity rolled over"
        );
    }
}

#[test]
fn test_trace_manager_pop_task_agent_span_rejects_non_task_agent() {
    let inv_id = "inv-task-reject";
    let parent = ident(inv_id, "parent", "", "");
    let child = ident(inv_id, "child", "", "parent.child@1");

    let clock = ManualClock::new();
    let mut tm = TraceManager::new();
    tm.now = clock.clock();
    tm.invocation_span(&bg(), &parent, None);

    // Push parent and child. A background context shows no running agent (see
    // running_agent_children), so push_agent_span leaves task_mode false on child.
    tm.push_agent_span(&bg(), &parent);
    tm.push_agent_span(&bg(), &child);

    // Record parent's last_activity at t1 via a model span.
    clock.advance_ms(100);
    let t1 = clock.now();
    tm.push_llm_span(&bg(), &parent);
    tm.pop_llm_span(&bg(), &parent, true);

    // The parent agent record (agent_stack[1], not [0], the invocation span).
    let inv = invocation_of(&tm, inv_id);
    let parent_rec = inv.mu.lock().agent_stack[1].clone();
    assert_eq!(
        parent_rec.lock().last_activity,
        t1,
        "parent.lastActivity after the model call ended"
    );

    // pop_task_agent_span on the non-task child at t2 returns None and does not advance
    // parent.last_activity.
    clock.advance_ms(100);
    assert!(
        tm.pop_task_agent_span(&bg(), &child, &[]).is_none(),
        "popTaskAgentSpan returned ok=true for non-task child, want false"
    );
    assert_eq!(
        parent_rec.lock().last_activity,
        t1,
        "parent.lastActivity after rejected popTaskAgentSpan (must not advance)"
    );
}

// parity: adapted: Go's weak pointers / runtime.GC are `Weak` references (the root callback's
// context value is the agent of its ADK context)
#[test]
fn test_trace_manager_straddling_push_does_not_retain_invocation_trace() {
    let tm = TraceManager::new();
    let id = ident("inv-straddle", "root", "", "");
    let root_payload = {
        let payload = custom("payload", vec![]);
        let adk = gadk_agent::Context::builder()
            .agent(payload.clone())
            .invocation_id("inv-straddle")
            .build();
        tm.invocation_span(&CallbackCtx::from_context(&adk), &id, None);
        Arc::downgrade(&payload)
    };
    let live_inv = {
        let inv = tm.lookup_invocation(&bg(), &id, false, true);
        tm.end_invocation(&bg(), &id);
        {
            let st = tm.mu.lock();
            let completed = st
                .completed
                .get(&id.invocation_id)
                .expect("completed trace not found after endInvocation");
            assert!(
                completed
                    .live_pending
                    .as_ref()
                    .is_some_and(|p| p.load(Ordering::SeqCst) == 1),
                "completed.livePending does not hold the straddling callback's reservation"
            );
        }
        assert!(
            inv.lock_live().is_none(),
            "lockLive reported the run as live after endInvocation"
        );
        tm.push_tool_span(&bg(), &id, "straddle-call");
        inv.pending_spans.fetch_sub(1, Ordering::SeqCst);
        Arc::downgrade(&inv)
    };
    let pending = tm
        .mu
        .lock()
        .completed
        .get(&id.invocation_id)
        .and_then(|c| c.live_pending.as_ref().map(|p| p.load(Ordering::SeqCst)));
    assert!(
        pending.is_none_or(|n| n == 0),
        "completed.livePending = {pending:?} after the straddling callback released its \
         reservation, want 0"
    );
    assert!(
        live_inv.upgrade().is_none(),
        "the finished run's invocationTrace is still reachable"
    );
    assert!(
        root_payload.upgrade().is_none(),
        "the context of the run's root callback is still reachable"
    );
    tm.pop_tool_span(&bg(), &id, "straddle-call", false);
}

#[test]
fn test_trace_manager_closed_handoff_then_late_call_is_not_pruned_as_waiting() {
    let mut tm = TraceManager::new();
    tm.max_tracked = 1;
    let limit = tm.max_tracked.max(64);
    // end_with_open_tool runs id until its tool call key starts and then ends the run, which
    // hands the call over to the late state.
    let end_with_open_tool = |tm: &TraceManager, id: &Identity, key: &str| {
        tm.invocation_span(&bg(), id, None);
        tm.push_agent_span(&bg(), id);
        tm.push_tool_span(&bg(), id, key);
        tm.end_invocation(&bg(), id);
    };
    let victim = ident("victim", "live", "", "");
    let handed = tool_key(&victim, "handed", "lookup");
    end_with_open_tool(&tm, &victim, &handed);
    let (end, _, _) = tm.pop_tool_span(&bg(), &victim, &handed, false);
    assert!(
        end.latency.is_some(),
        "TOOL_COMPLETED of the handed-over call = {end:?}, want a latency"
    );
    let later = tool_key(&victim, "later", "lookup");
    let start = tm.push_tool_span(&bg(), &victim, &later);
    for i in 0..limit - 1 {
        let id = ident(&format!("waiting-{i}"), "live", "", "");
        end_with_open_tool(&tm, &id, &tool_key(&id, "call", "lookup"));
    }
    let idle = ident("idle", "root", "", "");
    tm.invocation_span(&bg(), &idle, None);
    tm.end_invocation(&bg(), &idle);
    let victim_kept = is_completed(&tm, &victim.invocation_id);
    let first_waiting_kept = is_completed(&tm, "waiting-0");
    assert!(
        victim_kept && !first_waiting_kept,
        "after capacity was exceeded: run with only a post-AfterRun call kept = {victim_kept}, \
         least recently active run with a handed-over call kept = {first_waiting_kept}; want \
         true and false"
    );
    let (end, _, _) = tm.pop_tool_span(&bg(), &victim, &later, false);
    assert_ends(&end, &start, "TOOL_COMPLETED");
}

#[test]
fn test_finished_task_locked_requires_one_run_on_the_event_path() {
    let run = |path: &str, calls: &[&str]| {
        SpanRef::new(SpanRecord {
            kind: SpanKind::Agent,
            name: "task".into(),
            branch: "b".into(),
            path: path.into(),
            finished_tasks: calls.iter().map(|c| (*c).to_owned()).collect(),
            ..SpanRecord::default()
        })
    };
    let cases: Vec<(&str, &str, Vec<SpanRef>, Option<usize>)> = vec![
        (
            "run on the event path",
            "left/task@1",
            vec![
                run("left/task@1", &["call_0"]),
                run("right/task@1", &["call_0"]),
            ],
            Some(1),
        ),
        (
            "run on another path only",
            "left/task@1",
            vec![run("right/task@1", &["call_0"])],
            None,
        ),
        (
            "two runs on the event path",
            "left/task@1",
            vec![
                run("left/task@1", &["call_0"]),
                run("left/task@1", &["call_0"]),
            ],
            None,
        ),
        (
            "one of two runs on the event path saw the call",
            "left/task@1",
            vec![
                run("left/task@1", &["call_0"]),
                run("left/task@1", &["call_1"]),
            ],
            Some(1),
        ),
        (
            "event without a node path",
            "",
            vec![run("left/task@1", &["call_0"])],
            Some(1),
        ),
        (
            "run without a node path",
            "left/task@1",
            vec![run("", &["call_0"])],
            Some(1),
        ),
    ];
    for (name, path, stack, want) in cases {
        let inv = InvocationTrace::new_with_span(None, "trace", unix_1000());
        let mut st = inv.mu.lock();
        st.agent_stack = vec![SpanRef::new(SpanRecord {
            kind: SpanKind::Agent,
            name: "root".into(),
            ..SpanRecord::default()
        })];
        st.agent_stack.extend(stack);
        let id = ident("", "task", "b", path);
        let got = inv.finished_task_locked(&mut st, &id, &["call_0".to_owned()]);
        assert_eq!(got, want, "{name}: finishedTaskLocked");
    }
}

#[test]
fn test_failures_locked_open_call_fails_without_open_calls_only_a_run_that_is_over() {
    struct Case {
        name: &'static str,
        recorded: bool,
        helper_done: bool,
        open_calls: bool,
        over: &'static [&'static str],
        want: &'static [&'static str],
    }
    let cases = [
        Case {
            name: "nothing recorded",
            recorded: false,
            helper_done: false,
            open_calls: false,
            over: &[],
            want: &[],
        },
        Case {
            name: "nothing recorded with openCalls",
            recorded: false,
            helper_done: false,
            open_calls: true,
            over: &[],
            want: &["attempt", "helper"],
        },
        Case {
            name: "run known to be over",
            recorded: false,
            helper_done: true,
            open_calls: false,
            over: &[],
            want: &["attempt", "helper"],
        },
        Case {
            name: "recorded failure, run ended with the attempt",
            recorded: true,
            helper_done: false,
            open_calls: false,
            over: &["attempt", "helper"],
            want: &["attempt", "helper"],
        },
        Case {
            name: "recorded failure, run not shown to have ended",
            recorded: true,
            helper_done: false,
            open_calls: false,
            over: &["attempt"],
            want: &["attempt"],
        },
    ];
    for tc in cases {
        let workflow_rec = SpanRef::new(SpanRecord {
            span_id: "workflow".into(),
            kind: SpanKind::Agent,
            name: "root".into(),
            ..SpanRecord::default()
        });
        let attempt = SpanRef::new(SpanRecord {
            span_id: "attempt".into(),
            parent_span_id: "workflow".into(),
            parent: Some(workflow_rec.clone()),
            kind: SpanKind::Agent,
            name: "child".into(),
            ..SpanRecord::default()
        });
        // The helper responded before its open call, so only that call can show a failure
        // for it.
        let helper = SpanRef::new(SpanRecord {
            span_id: "helper".into(),
            parent_span_id: "attempt".into(),
            parent: Some(attempt.clone()),
            kind: SpanKind::Agent,
            name: "helper".into(),
            has_response: true,
            run_done: tc.helper_done,
            ..SpanRecord::default()
        });
        let mut st = InvocationState {
            agent_stack: vec![
                SpanRef::new(SpanRecord {
                    span_id: "invocation".into(),
                    kind: SpanKind::Invocation,
                    ..SpanRecord::default()
                }),
                workflow_rec,
                attempt,
                helper,
            ],
            ..InvocationState::default()
        };
        st.llm_spans.insert(
            "helper".into(),
            SpanRef::new(SpanRecord {
                span_id: "call".into(),
                parent_span_id: "helper".into(),
                kind: SpanKind::Llm,
                ..SpanRecord::default()
            }),
        );
        if tc.recorded {
            st.unrecovered_errors.insert(
                "attempt".into(),
                RecordedError {
                    err: err("model failed"),
                    cause: None,
                    span_id: "attempt".into(),
                    bare_key: String::new(),
                    seq: 0,
                    canceled: false,
                    collateral: false,
                },
            );
        }
        let over: Option<HashSet<String>> = if tc.over.is_empty() {
            None
        } else {
            Some(tc.over.iter().map(|s| (*s).to_owned()).collect())
        };
        let mut got: Vec<String> = st
            .failures_locked(2, false, over.as_ref(), tc.open_calls)
            .into_keys()
            .collect();
        got.sort();
        assert_eq!(got, tc.want, "{}: failed activations", tc.name);
    }
}

#[test]
fn test_late_review_child_outlives_parent() {
    for (name, children) in [
        ("sub-agent", vec!["x"]),
        ("sub-agent and a sibling after it", vec!["x", "y"]),
    ] {
        let tm = TraceManager::new();
        let inv = "inv-llm";
        late_review_ended(&tm, inv);
        let p = ident(inv, "p", "", "");
        tm.push_agent_span(&bg(), &p);
        let mut starts = HashMap::new();
        for child in &children {
            let id = ident(inv, child, "", "");
            starts.insert(*child, tm.push_agent_span(&bg(), &id).0);
            tm.push_llm_span(&bg(), &id);
        }
        tm.pop_agent_span(&bg(), &p);
        for child in &children {
            let id = ident(inv, child, "", "");
            tm.pop_llm_span(&bg(), &id, true);
            let end = tm.pop_agent_span(&bg(), &id);
            assert_eq!(
                end.span_id, starts[child].span_id,
                "{name}: AGENT_COMPLETED span of {child:?}, want the span of its AGENT_STARTING row"
            );
            assert!(
                end.latency.is_some(),
                "{name}: AGENT_COMPLETED row of {child:?} has no latency"
            );
        }
        let got = late_review_stack(&tm, inv);
        assert!(
            got.is_empty(),
            "{name}: late agents at the end = {got:?}, want none"
        );
    }
}

// parity: adapted: the caller o runs under a cancellable context, whose record keeps it (Go's
// background context gives every record a `canceled` closure; gadk's keeps none for it)
#[test]
fn test_late_review_tool_run_failed_grandchild() {
    // check_owner_open fails unless o, the agent that made the call, is the only late
    // activation left and keeps the context of its callback and the span that it runs under.
    let check_owner_open = |tm: &TraceManager, inv: &str| {
        assert_eq!(
            late_review_stack(tm, inv),
            vec!["o"],
            "late agents after the tool call returned"
        );
        let o = late_review_agent(tm, inv, "o");
        let late = late_review_trace(tm, inv);
        let _st = late.mu.lock();
        let r = o.lock();
        assert!(
            r.canceled.is_some() && r.parent.is_some(),
            "open late agent o keeps the context of its callback = {} and its parent = {}, \
             want both",
            r.canceled.is_some(),
            r.parent.is_some()
        );
    };
    let o_ctx = bg().with_cancel(CancellationToken::new());

    // model error of a grandchild
    {
        let tm = TraceManager::new();
        let inv = "inv-tool-run";
        late_review_ended(&tm, inv);
        let o = ident(inv, "o", "", "");
        let g = ident(inv, "g", "", "g");
        let s = ident(inv, "s", "", "g/s");
        let c = ident(inv, "c", "", "g/s/c");
        let call = tool_key(&o, "call-1", "g");
        let boom = err("boom");
        tm.push_agent_span(&o_ctx, &o);
        tm.push_tool_span(&o_ctx, &o, &call);
        tm.push_agent_span(&bg(), &g);
        tm.push_agent_span(&bg(), &s);
        tm.push_agent_span(&bg(), &c);
        tm.push_llm_span(&bg(), &c);
        tm.record_error(&bg(), &c, &llm_key(&c), boom.clone());
        tm.pop_llm_span(&bg(), &c, false);
        tm.pop_agent_span(&bg(), &s);
        let c_rec = late_review_agent(&tm, inv, "c");
        let g_rec = late_review_agent(&tm, inv, "g");
        let (_, purged, _) = tm.pop_tool_span(&o_ctx, &o, &call, false);
        let errs: HashMap<String, Option<TraceError>> = purged
            .iter()
            .map(|p| (p.agent_name.clone(), p.err.clone()))
            .collect();
        let is_boom = |name: &str| {
            errs.get(name)
                .cloned()
                .flatten()
                .is_some_and(|e| error_is(&e, &boom))
        };
        assert!(
            purged.len() == 2 && is_boom("c") && is_boom("g"),
            "AGENT_ERROR rows when the tool call returned = {errs:?}, want c and g, each with \
             boom"
        );
        check_owner_open(&tm, inv);
        let late = late_review_trace(&tm, inv);
        check_late_record_dropped(&late, &c_rec);
        check_late_record_dropped(&late, &g_rec);
    }

    // failure cause of a sub-agent that ended
    {
        let tm = TraceManager::new();
        let inv = "inv-tool-cause";
        late_review_ended(&tm, inv);
        let o = ident(inv, "o", "", "");
        let g = ident(inv, "g", "", "g");
        // In direct trace-manager tests, an agent on a branch of its own counts as a parallel
        // branch (see is_parallel_branch), whose context failure moves up to the agent above it
        // when it ends.
        let c = ident(inv, "c", "g.c", "g/c");
        let call = tool_key(&o, "call-1", "g");
        let sibling_err = err("sibling failed");
        tm.push_agent_span(&o_ctx, &o);
        tm.push_tool_span(&o_ctx, &o, &call);
        tm.push_agent_span(&bg(), &g);
        let (cctx, cancel) = with_cancel_cause(&sibling_err);
        tm.push_agent_span(&cctx, &c);
        cancel.cancel();
        tm.pop_agent_span(&cctx, &c);
        let g_rec = late_review_agent(&tm, inv, "g");
        let (_, purged, _) = tm.pop_tool_span(&o_ctx, &o, &call, false);
        let got: Vec<String> = purged
            .iter()
            .map(|p| {
                format!(
                    "{}: {:?}",
                    p.agent_name,
                    p.err.as_ref().map(|e| e.to_string())
                )
            })
            .collect();
        assert!(
            purged.len() == 1
                && purged[0].agent_name == "g"
                && purged[0]
                    .err
                    .as_ref()
                    .is_some_and(|e| error_is(e, &sibling_err)),
            "AGENT_ERROR rows when the tool call returned = {got:?}, want only g, with {sibling_err}"
        );
        check_owner_open(&tm, inv);
        check_late_record_dropped(&late_review_trace(&tm, inv), &g_rec);
    }
}

// parity: skipped TestLateReviewShortcutUnderInvocationSpan: deferred to 7b-3b (goroutine attribution): Go's pushAgentSpan reads the goroutine of a context with Ended (currentGoroutine), and only popToolSpan's markToolRunsDoneLocked, which marks the runs started on the tool callback's goroutine after the call, makes the shortcut sub-agent's run done so that retireLateSettledLocked retires it; with zero attribution it stays on the late stack
// parity: adapted: Go's endedContext is an ADK context whose invocation ended (`ended_ctx`)
#[test]
fn test_late_failure_cause_keeps_activation() {
    let tm = TraceManager::new();
    let inv = "inv-late-cause";
    late_review_ended(&tm, inv);
    let o = ident(inv, "o", "", "");
    let p = ident(inv, "p", "", "p");
    let x = ident(inv, "x", "", "p/x");
    // c is a parallel branch, as in test_late_review_tool_run_failed_grandchild.
    let c = ident(inv, "c", "x.c", "p/x/c");
    let sibling_err = err("sibling failed");
    tm.push_agent_span(&bg(), &o);
    tm.push_agent_span(&bg(), &p);
    let xctx = ended_ctx();
    tm.push_agent_span(&xctx, &x);
    let (cctx, cancel) = with_cancel_cause(&sibling_err);
    tm.push_agent_span(&cctx, &c);
    cancel.cancel();
    tm.pop_agent_span(&cctx, &c);
    // x's open model call keeps it open when p ends.
    tm.push_llm_span(&xctx, &x);
    tm.pop_agent_span(&bg(), &p);
    tm.pop_llm_span(&xctx, &x, true);
    assert_eq!(
        late_review_stack(&tm, inv),
        vec!["o", "x"],
        "late agents after x's model call ended"
    );
    let x_rec = late_review_agent(&tm, inv, "x");
    let late = late_review_trace(&tm, inv);
    let (settled, cause) = {
        let _st = late.mu.lock();
        let r = x_rec.lock();
        (r.run_done && r.context_ended, r.open_failure_cause())
    };
    assert!(
        settled && cause.as_ref().is_some_and(|e| error_is(e, &sibling_err)),
        "x settled = {settled} with failure cause {cause:?}, want true with {sibling_err}"
    );

    tm.pop_agent_span(&bg(), &o);
    let got = late_review_stack(&tm, inv);
    assert!(
        got.is_empty(),
        "late agents after o ended = {got:?}, want none"
    );
    check_late_record_dropped(&late, &x_rec);
    let st = late.mu.lock();
    let got = st.agent_stack[0].lock().first_retired_cause();
    assert!(
        got.as_ref().is_some_and(|e| error_is(e, &sibling_err)),
        "failure cause that reached the invocation span = {got:?}, want {sibling_err}"
    );
}
