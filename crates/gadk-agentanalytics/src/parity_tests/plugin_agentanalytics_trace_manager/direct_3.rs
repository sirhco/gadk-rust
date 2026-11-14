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

//! Part of the port of ADK-Go `plugin/agentanalytics/trace_manager_test.go` (owner: Task 12): the
//! 27 Go tests from `TestTraceManager_PurgedActivationLatencyFromLastActivity` to
//! `TestTraceManager_ToolSpanAcrossAfterRun` in the Go file's order that need no runner (the exact
//! list is in the 7b-3a plan's contracts, Appendix A).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::Ordering;

use gadk_agent::{Agent, Context, Mode, Plugin, Tool, ToolError};
use gadk_genai::Content;
use gadk_model::{LlmRequest, LlmResponse, ModelError};
use gadk_session::{Event, NodeInfo};
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use super::direct_support::{
    MockRun, SpanCounts, context_running, fake_tool, invocation_of, open_spans, set_agent_span,
};
use crate::context::ContextError;
use crate::error::Error;
use crate::event_types::{
    EVENT_AGENT_ERROR, EVENT_AGENT_STARTING, EVENT_INVOCATION_COMPLETED, EVENT_INVOCATION_ERROR,
    EVENT_INVOCATION_STARTING, EVENT_LLM_REQUEST, EVENT_LLM_RESPONSE, EVENT_TOOL_COMPLETED,
    EVENT_TOOL_ERROR, EVENT_TOOL_STARTING,
};
use crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::{
    MockCtx, agent_error_counts, event_types, must_latency, only_row_of_type, place_of,
    rows_of_type, setup_test_plugin, str_col, until_event_type, until_invocation_end,
};
use crate::plugin::callbacks::node_completion;
use crate::row::Row;
use crate::trace::causes::error_is;
use crate::trace::testkit::{ManualClock, custom, ident, llm};
use crate::trace::{
    CallbackCtx, Identity, InvocationTrace, SpanRef, TraceError, TraceManager, UnclosedAgentSpan,
    llm_key, tool_key,
};

// ---------------------------------------------------------------------------------------------
// File-local helpers
// ---------------------------------------------------------------------------------------------

/// Go `context.Background()`.
fn bg() -> CallbackCtx {
    CallbackCtx::background()
}

/// Go `context.WithCancel(parent)`: a context with a child token of `parent`'s (or a new
/// token), and the token that cancels it.
fn with_cancel(parent: Option<&CancellationToken>) -> (CallbackCtx, CancellationToken) {
    let token = parent.map_or_else(CancellationToken::new, CancellationToken::child_token);
    (bg().with_cancel(token.clone()), token)
}

/// Go `context.WithCancelCause(parent)` whose cancel is later called with `cause`.
fn with_cancel_cause(
    parent: Option<&CancellationToken>,
    cause: &TraceError,
) -> (CallbackCtx, CancellationToken) {
    let (ctx, token) = with_cancel(parent);
    (ctx.with_cancel_cause(cause.clone()), token)
}

/// Go `t.Context()`: a context that the test could cancel, live while it runs.
fn test_ctx() -> CallbackCtx {
    with_cancel(None).0
}

/// Go `errors.New(text)`.
fn err(text: &str) -> TraceError {
    Arc::new(Error::Message(text.into()))
}

/// Go `context.Canceled`.
fn canceled() -> TraceError {
    Arc::new(Error::Context(ContextError::Canceled))
}

/// An identity of invocation `inv`.
fn id(inv: &str, agent: &str, branch: &str, path: &str) -> Identity {
    ident(inv, agent, branch, path)
}

/// The record of the agent span named `name` of `inv` (live, else late), newest first.
fn agent_record(tm: &TraceManager, inv: &str, name: &str) -> SpanRef {
    let trace = {
        let st = tm.mu.lock();
        st.invocations
            .get(inv)
            .cloned()
            .or_else(|| st.completed.get(inv).and_then(|c| c.late.clone()))
    }
    .unwrap_or_else(|| panic!("invocation {inv:?} has neither live nor late state"));
    let st = trace.mu.lock();
    st.agent_stack
        .iter()
        .rev()
        .find(|r| r.lock().name == name)
        .cloned()
        .unwrap_or_else(|| panic!("no agent span {name:?} in {inv:?}"))
}

/// Go `tm.pushAgentSpan(&endedContext{Context: context.Background(), end: end}, id)`: the span's
/// `ended` reads an ADK context whose `ended()` is `end`, and it shows no activation and no
/// cancellation, as Go's `endedContext` (no `WithICDelta`) does. Returns the record, whose
/// `context_ended` tells whether `ended` was read (as true) since.
fn push_ended(tm: &TraceManager, who: &Identity, end: bool) -> SpanRef {
    tm.push_agent_span(&bg(), who);
    let rec = agent_record(tm, &who.invocation_id, &who.agent_name);
    let ctx = Context::builder()
        .agent(custom(&who.agent_name, vec![]))
        .invocation_id(who.invocation_id.clone())
        .build();
    if end {
        ctx.end_invocation();
    }
    rec.lock().ended = Some(ctx);
    rec
}

/// Whether the `ended` context of `rec` (an `end: true` [`push_ended`] span) was read (Go
/// `src.reads == 1`; it is never read again once it reported true).
fn ended_read(rec: &SpanRef) -> bool {
    rec.lock().context_ended
}

/// Go `lateReviewTrace(t, tm, invID)`.
fn late_trace(tm: &TraceManager, inv: &str) -> Arc<InvocationTrace> {
    tm.mu
        .lock()
        .completed
        .get(inv)
        .and_then(|c| c.late.clone())
        .unwrap_or_else(|| panic!("completed run {inv:?} has no late state"))
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
        "retired late activation {:?} keeps a failure cause",
        r.name
    );
}

/// Go `requireCompletedWithHandledAgentError(t, rows, agentName, wantErr)`.
fn require_completed_with_handled_agent_error(rows: &[Row], agent_name: &str, want_err: &str) {
    assert!(
        rows_of_type(rows, EVENT_INVOCATION_ERROR).is_empty(),
        "got INVOCATION_ERROR, want INVOCATION_COMPLETED; event types: {:?}",
        event_types(rows)
    );
    let inv_row = only_row_of_type(rows, EVENT_INVOCATION_COMPLETED);
    assert_eq!(
        str_col(&inv_row, "status"),
        "OK",
        "INVOCATION_COMPLETED status"
    );
    let err_row = only_row_of_type(rows, EVENT_AGENT_ERROR);
    assert_eq!(str_col(&err_row, "agent"), agent_name, "AGENT_ERROR agent");
    let start = rows_of_type(rows, EVENT_AGENT_STARTING)
        .into_iter()
        .find(|r| str_col(r, "agent") == agent_name)
        .unwrap_or_else(|| {
            panic!(
                "no AGENT_STARTING row of {agent_name:?}; event types: {:?}",
                event_types(rows)
            )
        });
    assert_eq!(place_of(&err_row), place_of(&start), "AGENT_ERROR place");
    must_latency(&err_row);
    let msg = str_col(&err_row, "error_message");
    assert!(
        msg.contains(want_err),
        "AGENT_ERROR error_message = {msg:?}, want it to contain {want_err:?}"
    );
}

/// Go `session.NewEvent(ctx, invocationID)` by `author`.
fn new_event(inv: &str, author: &str) -> Event {
    let mut ev = Event::new(&gadk_platform::Platform::default(), inv);
    ev.author = author.into();
    ev
}

/// Go `textContent(role, text)`.
fn text(role: &str, t: &str) -> Content {
    Content::from_text(t, role)
}

// Go's `p.XxxCallback()(…)` with `errs.add` / `errs.check`: each callback must succeed.

async fn before_run(p: &Plugin, run: &MockRun) {
    p.before_run_callback().unwrap()(run.inv()).await.unwrap();
}

async fn after_run(p: &Plugin, run: &MockRun) {
    p.after_run_callback().unwrap()(run.inv()).await;
}

async fn before_agent(p: &Plugin, run: &MockRun) {
    p.before_agent_callback().unwrap()(run.ctx()).await.unwrap();
}

async fn before_model(p: &Plugin, run: &MockRun, req: LlmRequest) {
    let (_, res) = p.before_model_callback().unwrap()(run.ctx(), req).await;
    res.unwrap();
}

async fn after_model(p: &Plugin, run: &MockRun, resp: LlmResponse) {
    let (_, res) = p.after_model_callback().unwrap()(run.ctx(), resp, None).await;
    res.unwrap();
}

async fn on_model_error(p: &Plugin, run: &MockRun, msg: &str) {
    p.on_model_error_callback().unwrap()(
        run.ctx(),
        LlmRequest::default(),
        Arc::new(ModelError::Other(msg.into())),
    )
    .await
    .unwrap();
}

async fn on_event(p: &Plugin, run: &MockRun, ev: Event) {
    p.on_event_callback().unwrap()(run.inv(), ev).await.unwrap();
}

async fn before_tool(p: &Plugin, ctx: Context, t: &Arc<dyn Tool>, args: Map<String, Value>) {
    p.before_tool_callback().unwrap()(ctx, t.clone(), args)
        .await
        .unwrap();
}

async fn after_tool(
    p: &Plugin,
    ctx: Context,
    t: &Arc<dyn Tool>,
    args: Map<String, Value>,
    result: Map<String, Value>,
    e: Option<Arc<ToolError>>,
) {
    p.after_tool_callback().unwrap()(ctx, t.clone(), args, result, e)
        .await
        .unwrap();
}

async fn on_tool_error(p: &Plugin, ctx: Context, t: &Arc<dyn Tool>, e: Arc<ToolError>) {
    p.on_tool_error_callback().unwrap()(ctx, t.clone(), Map::new(), e)
        .await
        .unwrap();
}

/// Go `errors.New(msg)` as a tool error.
fn tool_err(msg: &str) -> Arc<ToolError> {
    Arc::new(ToolError::Execution(msg.into()))
}

/// A JSON object literal as a `Map`.
fn obj(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}

/// The `total_ms` of a purged activation.
fn total_ms(u: &UnclosedAgentSpan) -> i64 {
    u.span.latency.as_ref().map_or(-1, |l| l.total_ms)
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

type Step<'a> = Box<dyn Fn(&TraceManager) + 'a>;

#[test]
fn test_trace_manager_purged_activation_latency_from_last_activity() {
    const INV: &str = "inv-activity";
    let node = || id(INV, "node", "", "wf@1/node@1");
    let child = || id(INV, "child", "", "wf@1/node@1");
    let none: fn() -> Step<'static> = || Box::new(|_| {});
    let tests: Vec<(&str, Step, Step)> = vec![
        (
            "pushAgentSpan",
            none(),
            Box::new(move |tm| {
                tm.push_agent_span(&bg(), &child());
            }),
        ),
        (
            "popAgentSpan",
            Box::new(move |tm| {
                tm.push_agent_span(&bg(), &child());
                tm.push_llm_span(&bg(), &child());
                tm.pop_llm_span(&bg(), &child(), true);
            }),
            Box::new(move |tm| {
                tm.pop_agent_span(&bg(), &child());
            }),
        ),
        (
            "popTaskAgentSpan",
            Box::new(move |tm| {
                tm.push_agent_span(&bg(), &child());
                // A background context shows no running agent, so pushAgentSpan leaves
                // task_mode false: set it so popTaskAgentSpan pops the child.
                let inv = invocation_of(tm, INV);
                inv.mu.lock().agent_stack.last().unwrap().lock().task_mode = true;
                tm.push_llm_span(&bg(), &child());
                tm.pop_llm_span(&bg(), &child(), true);
            }),
            Box::new(move |tm| {
                tm.pop_task_agent_span(&bg(), &child(), &[]);
            }),
        ),
        (
            "pushLLMSpan",
            none(),
            Box::new(move |tm| {
                tm.push_llm_span(&bg(), &node());
            }),
        ),
        (
            "recordFirstToken",
            Box::new(move |tm| {
                tm.push_llm_span(&bg(), &node());
            }),
            Box::new(move |tm| tm.record_first_token(&bg(), &node())),
        ),
        (
            "recordStreamed without content",
            Box::new(move |tm| {
                tm.push_llm_span(&bg(), &node());
            }),
            Box::new(move |tm| tm.record_streamed(&bg(), &node(), "node", "", false)),
        ),
        (
            "popLLMSpan not responded",
            Box::new(move |tm| {
                tm.push_llm_span(&bg(), &node());
            }),
            Box::new(move |tm| {
                tm.pop_llm_span(&bg(), &node(), false);
            }),
        ),
        (
            "pushToolSpan",
            none(),
            Box::new(move |tm| {
                tm.push_tool_span(&bg(), &node(), &tool_key(&node(), "call-1", "lookup"));
            }),
        ),
        (
            "popToolSpan",
            Box::new(move |tm| {
                tm.push_tool_span(&bg(), &node(), &tool_key(&node(), "call-1", "lookup"));
            }),
            Box::new(move |tm| {
                tm.pop_tool_span(
                    &bg(),
                    &node(),
                    &tool_key(&node(), "call-1", "lookup"),
                    false,
                );
            }),
        ),
        (
            "recordError",
            none(),
            Box::new(move |tm| {
                tm.record_error(&bg(), &node(), &llm_key(&node()), err("model failed"));
            }),
        ),
        (
            "completeResponse with open span",
            Box::new(move |tm| {
                tm.push_agent_span(&bg(), &child());
                tm.record_error(&bg(), &node(), &llm_key(&node()), err("tool failed"));
            }),
            Box::new(move |tm| tm.complete_response(&bg(), &child(), "child", "", &[])),
        ),
        (
            "completeResponse with no open span",
            Box::new(move |tm| {
                tm.record_error(&bg(), &node(), &llm_key(&node()), err("tool failed"));
            }),
            // completeResponse with an author that has no open agent span touches the
            // activation that the author would have run under.
            Box::new(move |tm| tm.complete_response(&bg(), &node(), "ghost", "", &[])),
        ),
        (
            "eventSpan",
            Box::new(move |tm| {
                tm.record_error(&bg(), &node(), &llm_key(&node()), err("tool failed"));
            }),
            Box::new(move |tm| {
                tm.event_span(&bg(), &node(), "node", "");
            }),
        ),
    ];
    for (name, setup, touch) in tests {
        let clock = ManualClock::new();
        let t0 = clock.now();
        let mut tm = TraceManager::new();
        tm.now = clock.clock();
        tm.invocation_span(&bg(), &node(), None);
        tm.push_agent_span(&bg(), &node());
        setup(&tm);
        clock.advance_ms(50);
        let t1 = clock.now();
        touch(&tm);
        clock.advance_ms(1950);
        let (_, purged) = tm.push_agent_span(&bg(), &node());
        let p = purged
            .iter()
            .find(|u| u.agent_name == "node")
            .unwrap_or_else(|| {
                panic!(
                    "{name}: pushAgentSpan returned no purged entry for node; entries: {}",
                    purged.len()
                )
            });
        assert_eq!(
            total_ms(p),
            (t1 - t0).num_milliseconds(),
            "{name}: latency total_ms, want start→t1, not start→t2"
        );
        assert_eq!(p.timestamp, Some(t1), "{name}: timestamp, want t1, not t2");
    }
}

#[test]
fn test_trace_manager_touch_agent_activity_monotonic() {
    let node = id("inv-mono", "node", "", "wf@1/node@1");
    let clock = ManualClock::new();
    let start = clock.now();
    let mut tm = TraceManager::new();
    tm.now = clock.clock();
    tm.invocation_span(&bg(), &node, None);
    tm.push_agent_span(&bg(), &node);
    // Touch at a later time via pushLLMSpan.
    clock.advance_ms(100);
    let want = clock.now();
    tm.push_llm_span(&bg(), &node);
    tm.pop_llm_span(&bg(), &node, true);
    // Touch at an earlier time via recordError.
    clock.advance_ms(-50);
    tm.record_error(&bg(), &node, &llm_key(&node), err("model failed"));
    // Purge at an even later time.
    clock.advance_ms(5000);
    let (_, purged) = tm.push_agent_span(&bg(), &node);
    assert_eq!(purged.len(), 1, "pushAgentSpan purged entries");
    let p = &purged[0];
    // last_activity is the pushLLMSpan time, not the earlier recordError time.
    assert_eq!(
        total_ms(p),
        (want - start).num_milliseconds(),
        "latency total_ms (monotonic, not regressed by earlier recordError)"
    );
    assert_eq!(p.timestamp, Some(want), "timestamp (monotonic)");
}

#[test]
fn test_trace_manager_responded_branch_failure_cause_fallback() {
    // custom_cause
    {
        let err_branch = err("branch exploded");
        let (branch_ctx, cancel_branch) = with_cancel_cause(None, &err_branch);
        let tm = TraceManager::new();
        let par = id("inv-cause-fallback-custom", "par", "", "");
        let branch = id("inv-cause-fallback-custom", "branch", "par.branch", "");
        tm.invocation_span(&bg(), &par, None);
        tm.push_agent_span(&bg(), &par);
        tm.push_agent_span(&branch_ctx, &branch);
        tm.record_streamed(&bg(), &branch, "branch", &branch.branch, true);
        cancel_branch.cancel();

        let (unclosed, term_err) = tm.pop_unclosed_agent_spans(&bg(), &par);
        let term_err = term_err.expect("terminal error");
        assert!(
            error_is(&term_err, &err_branch),
            "popUnclosedAgentSpans terminal error = {term_err}, want {err_branch}"
        );
        assert!(
            unclosed.is_empty(),
            "popUnclosedAgentSpans unclosed = {}",
            unclosed.len()
        );
    }
    // wrapped_deadline_exceeded
    {
        let err_branch: TraceError = Arc::new(Error::wrap_text(
            "sub-agent timed out: context deadline exceeded",
            Error::Context(ContextError::DeadlineExceeded),
        ));
        let (branch_ctx, cancel_branch) = with_cancel_cause(None, &err_branch);
        let tm = TraceManager::new();
        let par = id("inv-cause-fallback-deadline", "par", "", "");
        let branch = id("inv-cause-fallback-deadline", "branch", "par.branch", "");
        tm.invocation_span(&bg(), &par, None);
        tm.push_agent_span(&bg(), &par);
        tm.push_agent_span(&branch_ctx, &branch);
        tm.record_streamed(&bg(), &branch, "branch", &branch.branch, true);
        cancel_branch.cancel();

        let (unclosed, term_err) = tm.pop_unclosed_agent_spans(&bg(), &par);
        let term_err = term_err.expect("terminal error");
        assert!(
            term_err.is_context(ContextError::DeadlineExceeded)
                && term_err.to_string().contains("sub-agent timed out"),
            "popUnclosedAgentSpans terminal error = {term_err}, want wrapped DeadlineExceeded"
        );
        assert!(
            unclosed.is_empty(),
            "popUnclosedAgentSpans unclosed = {}",
            unclosed.len()
        );
    }
}

#[test]
fn test_trace_manager_complete_response_clears_retired_cause() {
    let err_branch = err("branch exploded");
    let (branch_ctx, cancel_branch) = with_cancel_cause(None, &err_branch);
    let tm = TraceManager::new();
    const INV: &str = "inv-clear-retired-cause";
    let guard = id(INV, "guard", "", "");
    let par = id(INV, "par", "guard.par", "");
    let branch = id(INV, "branch", "guard.par.branch", "");
    tm.invocation_span(&bg(), &guard, None);
    tm.push_agent_span(&bg(), &guard);
    tm.push_agent_span(&bg(), &par);
    tm.push_agent_span(&branch_ctx, &branch);
    tm.record_streamed(&bg(), &branch, "branch", &branch.branch, true);
    cancel_branch.cancel();

    tm.pop_agent_span(&bg(), &branch);
    tm.pop_agent_span(&bg(), &par);
    tm.complete_response(&bg(), &guard, "guard", "", &[]);
    tm.pop_agent_span(&bg(), &guard);

    let (unclosed, term_err) = tm.pop_unclosed_agent_spans(&bg(), &guard);
    assert!(
        term_err.is_none(),
        "terminal error = {term_err:?}, want none"
    );
    assert!(unclosed.is_empty(), "unclosed = {}", unclosed.len());
}

// parity: adapted: Go's contextRunning shows the trace manager no activation; gadk's ADK context
// of the running agent carries one (the agent lookup is the same).
#[test]
fn test_trace_manager_pop_agent_span_clears_partial_only_for_direct_workflow_root() {
    const INV: &str = "inv-root-end";
    let llm_root = llm("root", vec![], Mode::default());
    let author = custom("author", vec![]);
    let failer = custom("failer", vec![]);
    let workflow_root = custom("root", vec![author.clone(), failer.clone()]);
    let root_id = id(INV, "root", "", "");
    let author_id = id(INV, "author", "root.author", "");
    let failer_id = id(INV, "failer", "root.failer", "");
    let cases: Vec<(&str, &Arc<dyn Agent>, &Identity, bool)> = vec![
        (
            "workflow agent that the runner runs",
            &workflow_root,
            &root_id,
            true,
        ),
        ("llmagent that the runner runs", &llm_root, &root_id, false),
        (
            "workflow agent below another agent",
            &workflow_root,
            &author_id,
            false,
        ),
    ];
    for (name, root, ends, want_err) in cases {
        let tm = TraceManager::new();
        tm.invocation_span(&context_running(root), &root_id, Some(root));
        tm.push_agent_span(&context_running(root), &root_id);
        tm.push_agent_span(&context_running(&author), &author_id);
        tm.push_agent_span(&context_running(&failer), &failer_id);
        tm.record_streamed(&bg(), &author_id, "author", &author_id.branch, true);

        tm.pop_agent_span(&bg(), ends);

        let (_, e) = tm.pop_unclosed_agent_spans(&bg(), &root_id);
        assert_eq!(
            e.is_some(),
            want_err,
            "{name}: popUnclosedAgentSpans error = {e:?}"
        );
    }
}

#[test]
fn test_trace_manager_partial_event_without_content_is_no_response() {
    for (name, with_content, want_err) in [
        ("partial event without content", false, true),
        ("partial event with content", true, false),
    ] {
        let tm = TraceManager::new();
        let worker = id("inv-ping", "worker", "", "");
        tm.invocation_span(&bg(), &worker, None);
        tm.push_agent_span(&bg(), &worker);
        tm.record_streamed(&bg(), &worker, "worker", "", with_content);
        tm.clear_last_partial(&bg(), &worker);
        let (_, e) = tm.pop_unclosed_agent_spans(&bg(), &worker);
        assert_eq!(
            e.is_some(),
            want_err,
            "{name}: popUnclosedAgentSpans error = {e:?}"
        );
    }
}

#[test]
fn test_trace_manager_stop_on_progress_event_of_parallel_branch() {
    const INV: &str = "inv-progress";
    let par = id(INV, "par", "", "");
    let author = id(INV, "author", "par.author", "");
    let child = id(INV, "child", "par.author", "");
    let pinger = id(INV, "pinger", "par.pinger", "");
    let other = id(INV, "other", "par.other", "");
    type Hook<'a> = Option<Box<dyn Fn(&CallbackCtx, &TraceManager) + 'a>>;
    let cases: Vec<(&str, Hook, Hook, bool)> = vec![
        ("caller stops on the event", None, None, false),
        (
            "session event after the event",
            None,
            Some(Box::new(|ctx, tm| tm.clear_last_partial(ctx, &par))),
            true,
        ),
        (
            "earlier partial event of another branch",
            Some(Box::new(|ctx, tm| {
                tm.record_streamed(ctx, &pinger, "pinger", &pinger.branch, false);
                tm.complete_response(ctx, &other, "other", &other.branch, &[]);
            })),
            None,
            true,
        ),
    ];
    for (name, before, after, want_err) in cases {
        let ctx = bg();
        let pinger_ctx = bg();
        let (branch_ctx, cancel) = with_cancel(None);
        let tm = TraceManager::new();
        tm.invocation_span(&ctx, &par, None);
        tm.push_agent_span(&ctx, &par);
        tm.push_agent_span(&pinger_ctx, &pinger);
        tm.push_agent_span(&branch_ctx, &other);
        tm.push_agent_span(&branch_ctx, &author);
        if let Some(f) = &before {
            f(&ctx, &tm);
        }
        tm.record_streamed(&ctx, &author, "author", &author.branch, false);
        tm.push_agent_span(&branch_ctx, &child);
        if let Some(f) = &after {
            f(&ctx, &tm);
        }
        cancel.cancel();
        if before.is_none() {
            // The pinger branch ended without failing.
            tm.pop_agent_span(&pinger_ctx, &pinger);
        }
        let (_, e) = tm.pop_unclosed_agent_spans(&ctx, &par);
        assert_eq!(
            e.is_some(),
            want_err,
            "{name}: popUnclosedAgentSpans error = {e:?}"
        );
    }
}

#[tokio::test]
async fn test_on_event_progress_event_is_no_response() {
    let tp = setup_test_plugin().await;
    let p = &tp.plugin;
    let run = MockRun::new(&tp.ctx);
    before_agent(p, &run).await;
    let mut ping = new_event(&tp.ctx.invocation_id, &tp.ctx.agent_name);
    ping.response.partial = true;
    on_event(p, &run, ping).await;
    let state_only = new_event(&tp.ctx.invocation_id, &tp.ctx.agent_name);
    on_event(p, &run, state_only).await;
    after_run(p, &run).await;

    let rows = tp.collect_rows(until_invocation_end).await;
    assert!(
        rows_of_type(&rows, EVENT_INVOCATION_COMPLETED).is_empty(),
        "got INVOCATION_COMPLETED, want INVOCATION_ERROR: the author never responded; event types: {:?}",
        event_types(&rows)
    );
    only_row_of_type(&rows, EVENT_INVOCATION_ERROR);
    assert_eq!(
        agent_error_counts(&rows),
        [(tp.ctx.agent_name.clone(), 1)].into(),
        "AGENT_ERROR rows per agent"
    );
}

/// Go's `with(agentName, branch, path, "")` of the two tool-run tests: a copy of `m`.
fn mock_with(m: &MockCtx, agent: &str, branch: &str, path: &str) -> MockCtx {
    MockCtx {
        agent_name: agent.into(),
        branch: branch.into(),
        path: path.into(),
        ..m.clone()
    }
}

#[tokio::test]
async fn test_after_tool_failed_tool_run_ends_with_agent_error() {
    let tp = setup_test_plugin().await;
    let p = &tp.plugin;
    let coord = MockRun::new(&mock_with(&tp.ctx, &tp.ctx.agent_name, "", ""));
    let (call_a, call_b) = (coord.tool("call-a"), coord.tool("call-b"));
    let helper = MockRun::new(&mock_with(&tp.ctx, "helper", "helper@1", "/helper@1"));
    let helper_tool = fake_tool("helper");

    before_agent(p, &coord).await;
    before_tool(p, call_a.clone(), &helper_tool, Map::new()).await;
    before_tool(p, call_b.clone(), &helper_tool, Map::new()).await;
    before_agent(p, &helper).await;
    before_model(p, &helper, LlmRequest::default()).await;
    on_model_error(p, &helper, "helper model failed").await;
    on_tool_error(p, call_a, &helper_tool, tool_err("helper failed")).await;
    after_tool(
        p,
        call_b,
        &helper_tool,
        Map::new(),
        obj(json!({"answer": "ok"})),
        None,
    )
    .await;
    let mut answer = new_event(&tp.ctx.invocation_id, &tp.ctx.agent_name);
    answer.response.content = Some(text("model", "done"));
    on_event(p, &coord, answer).await;
    after_run(p, &coord).await;

    let rows = tp.collect_rows(until_invocation_end).await;
    require_completed_with_handled_agent_error(&rows, "helper", "helper model failed");
}

#[tokio::test]
async fn test_on_tool_error_failed_tool_run_ends_with_agent_error() {
    let tp = setup_test_plugin().await;
    let p = &tp.plugin;
    let coord = MockRun::new(&mock_with(&tp.ctx, &tp.ctx.agent_name, "", ""));
    let call = coord.tool("call-a");
    let helper = MockRun::new(&mock_with(&tp.ctx, "helper", "helper@1", "/helper@1"));
    let helper_tool = fake_tool("helper");

    before_agent(p, &coord).await;
    before_tool(p, call.clone(), &helper_tool, Map::new()).await;
    before_agent(p, &helper).await;
    before_model(p, &helper, LlmRequest::default()).await;
    on_model_error(p, &helper, "helper model failed").await;
    on_tool_error(p, call, &helper_tool, tool_err("helper failed")).await;
    let mut answer = new_event(&tp.ctx.invocation_id, &tp.ctx.agent_name);
    answer.response.content = Some(text("model", "done"));
    on_event(p, &coord, answer).await;
    after_run(p, &coord).await;

    let rows = tp.collect_rows(until_invocation_end).await;
    require_completed_with_handled_agent_error(&rows, "helper", "helper model failed");
}

#[test]
fn test_trace_manager_tool_runs_purged_once_no_tool_call_is_open() {
    let ctx = bg();
    let tm = TraceManager::new();
    const INV: &str = "inv-tool-runs";
    let coord = id(INV, "coordinator", "", "");
    let helper = |run: &str| {
        id(
            INV,
            "helper",
            &format!("helper@{run}"),
            &format!("/helper@{run}"),
        )
    };
    let (failed, running) = (helper("1"), helper("2"));
    let (call_a, call_b) = (
        tool_key(&coord, "call-a", "helper"),
        tool_key(&coord, "call-b", "helper"),
    );

    tm.invocation_span(&ctx, &coord, None);
    tm.push_agent_span(&ctx, &coord);
    tm.push_tool_span(&ctx, &coord, &call_a);
    tm.push_tool_span(&ctx, &coord, &call_b);
    tm.push_agent_span(&ctx, &failed);
    tm.push_agent_span(&ctx, &running);
    tm.push_llm_span(&ctx, &failed);
    tm.record_error(&ctx, &failed, &llm_key(&failed), err("helper model failed"));
    tm.pop_llm_span(&ctx, &failed, false);

    tm.pop_tool_span(&ctx, &coord, &call_a, true);
    assert_eq!(
        open_spans(&tm, INV).agents,
        3,
        "open agent spans while call-b is open"
    );
    let end = tm.pop_agent_span(&ctx, &running);
    assert!(
        end.latency.is_some(),
        "AGENT_COMPLETED of the running helper = {end:?}, want its span with latency"
    );

    tm.pop_tool_span(&ctx, &coord, &call_b, false);
    let got = open_spans(&tm, INV);
    assert!(
        got.agents == 1 && got.models == 0,
        "open spans after the last tool call ended = {got:?}, want the coordinator alone"
    );
    tm.complete_response(&ctx, &coord, "coordinator", "", &[]);
    let (unclosed, e) = tm.pop_unclosed_agent_spans(&ctx, &coord);
    assert!(
        e.is_none() && unclosed.is_empty(),
        "popUnclosedAgentSpans = ({} spans, {e:?}), want no failure",
        unclosed.len()
    );
}

#[test]
fn test_trace_manager_node_reentry_purges_only_failed_attempt() {
    let ctx = bg();
    let flaky = id("inv-retry", "flaky", "", "wf@1/flaky@1");
    let tests: Vec<(&str, Step)> = vec![
        ("never responded", Box::new(|_| {})),
        (
            "unrecovered error after a partial event",
            Box::new(|tm| {
                tm.push_llm_span(&bg(), &flaky);
                tm.record_streamed(&bg(), &flaky, "flaky", "", true);
                tm.record_error(&bg(), &flaky, &llm_key(&flaky), err("stream broken"));
                tm.pop_llm_span(&bg(), &flaky, false);
            }),
        ),
        (
            "open tool call",
            Box::new(|tm| {
                tm.push_llm_span(&bg(), &flaky);
                tm.pop_llm_span(&bg(), &flaky, true);
                tm.push_tool_span(&bg(), &flaky, &tool_key(&flaky, "call-1", "lookup"));
            }),
        ),
    ];
    for (name, attempt) in &tests {
        let tm = TraceManager::new();
        tm.invocation_span(&ctx, &flaky, None);
        tm.push_agent_span(&ctx, &flaky);
        attempt(&tm);

        tm.push_agent_span(&ctx, &flaky);
        assert_eq!(
            open_spans(&tm, &flaky.invocation_id),
            SpanCounts {
                agents: 1,
                ..SpanCounts::default()
            },
            "{name}: open spans after the retry started, want the retry alone"
        );
        // Nothing of the failed attempt, its partial event included, hides that the retry
        // never responded.
        let (_, e) = tm.pop_unclosed_agent_spans(&ctx, &flaky);
        assert!(
            e.as_ref()
                .is_some_and(|e| e.to_string().contains("execution failed")),
            "{name}: popUnclosedAgentSpans error = {e:?}, want the failure of the retry"
        );
    }

    // running transfer-back chain
    {
        let tm = TraceManager::new();
        let planner = id("inv-chain", "planner", "", "wf@1/planner@1");
        let helper = id("inv-chain", "helper", "", "wf@1/planner@1");
        tm.invocation_span(&ctx, &planner, None);
        tm.push_agent_span(&ctx, &planner);
        tm.push_llm_span(&ctx, &planner);
        tm.pop_llm_span(&ctx, &planner, true);
        tm.push_agent_span(&ctx, &helper);
        tm.push_llm_span(&ctx, &helper);
        tm.pop_llm_span(&ctx, &helper, true);

        tm.push_agent_span(&ctx, &planner);
        assert_eq!(
            open_spans(&tm, "inv-chain").agents,
            3,
            "open agent spans after planner re-entered: the first planner and helper still run"
        );
    }

    // failed activation outside a node: a custom agent that stops reading its sub-agent at an
    // error and runs it again may have yielded the error, so the activation that failed keeps
    // it.
    {
        let tm = TraceManager::new();
        let bot = id("inv-custom-retry", "bot", "", "");
        tm.invocation_span(&ctx, &bot, None);
        tm.push_agent_span(&ctx, &bot);
        tm.push_llm_span(&ctx, &bot);
        tm.record_error(&ctx, &bot, &llm_key(&bot), err("model failed"));
        tm.pop_llm_span(&ctx, &bot, false);

        tm.push_agent_span(&ctx, &bot);
        tm.push_llm_span(&ctx, &bot);
        tm.pop_llm_span(&ctx, &bot, true);
        tm.pop_agent_span(&ctx, &bot);
        let (_, e) = tm.pop_unclosed_agent_spans(&ctx, &bot);
        assert!(
            e.is_some(),
            "popUnclosedAgentSpans error = None, want the error of the first activation"
        );
    }
}

#[test]
fn test_trace_manager_last_partial_cleared_by_work_under_its_author() {
    const INV: &str = "inv-partial";
    let par = id(INV, "par", "", "");
    let author = id(INV, "author", "par.author", "");
    let sibling = id(INV, "sibling", "par.sibling", "");
    let child = id(INV, "child", "par.author", "");
    let tests: Vec<(&str, Step, bool)> = vec![
        (
            "sub-agent of the author starts",
            Box::new(|tm| {
                tm.push_agent_span(&bg(), &child);
            }),
            true,
        ),
        (
            "author starts a model call",
            Box::new(|tm| {
                tm.push_llm_span(&bg(), &author);
            }),
            true,
        ),
        (
            "author starts a tool call",
            Box::new(|tm| {
                tm.push_tool_span(&bg(), &author, &tool_key(&author, "call-1", "lookup"));
            }),
            true,
        ),
        (
            "sibling starts a model call",
            Box::new(|tm| {
                tm.push_llm_span(&bg(), &sibling);
            }),
            false,
        ),
    ];
    for (name, work, want_err) in &tests {
        let tm = TraceManager::new();
        tm.invocation_span(&bg(), &par, None);
        tm.push_agent_span(&bg(), &par);
        tm.push_agent_span(&bg(), &author);
        tm.push_agent_span(&bg(), &sibling);
        tm.record_streamed(&bg(), &author, "author", &author.branch, true);

        work(&tm);

        let (_, e) = tm.pop_unclosed_agent_spans(&bg(), &par);
        assert_eq!(
            e.is_some(),
            *want_err,
            "{name}: popUnclosedAgentSpans error = {e:?}"
        );
    }
}

#[test]
fn test_trace_manager_canceled_work_after_partial_is_not_a_failure() {
    const INV: &str = "inv-canceled-work";
    let par = id(INV, "par", "", "");
    let author = id(INV, "author", "par.author", "");
    let sibling = id(INV, "sibling", "par.sibling", "");
    let child = id(INV, "child", "par.author", "");
    type Work<'a> = Box<dyn Fn(&CallbackCtx, &dyn Fn(), &TraceManager) + 'a>;
    let works: Vec<(&str, Work)> = vec![
        (
            "sub-agent of the author starts",
            Box::new(|ctx, _, tm| {
                tm.push_agent_span(ctx, &child);
            }),
        ),
        (
            "author starts a model call",
            Box::new(|ctx, _, tm| {
                tm.push_llm_span(ctx, &author);
            }),
        ),
        (
            "author starts a tool call",
            Box::new(|ctx, _, tm| {
                tm.push_tool_span(ctx, &author, &tool_key(&author, "call-1", "lookup"));
            }),
        ),
        (
            "sub-agent of the author fails with the context's error",
            Box::new(|ctx, stop, tm| {
                tm.push_agent_span(ctx, &child);
                tm.push_llm_span(ctx, &child);
                stop();
                tm.record_error(ctx, &child, &llm_key(&child), canceled());
                tm.pop_llm_span(ctx, &child, false);
            }),
        ),
    ];
    // (name, cancel, event_after, want_err)
    let cases = [
        ("context canceled", true, false, false),
        ("context live", false, false, true),
        ("session event after the partial one", true, true, true),
    ];
    for (wname, work) in &works {
        for (cname, cancel, event_after, want_err) in cases {
            let ctx = bg();
            let (branch_ctx, token) = with_cancel(None);
            let stop = || {
                if cancel {
                    token.cancel();
                }
            };
            let tm = TraceManager::new();
            tm.invocation_span(&ctx, &par, None);
            tm.push_agent_span(&ctx, &par);
            tm.push_agent_span(&branch_ctx, &author);
            tm.push_agent_span(&branch_ctx, &sibling);
            tm.record_streamed(&ctx, &author, "author", &author.branch, true);

            work(&branch_ctx, &stop, &tm);
            if event_after {
                tm.complete_response(&ctx, &sibling, "sibling", &sibling.branch, &[]);
            }
            stop();

            let (_, e) = tm.pop_unclosed_agent_spans(&ctx, &par);
            assert_eq!(
                e.is_some(),
                want_err,
                "{wname}/{cname}: popUnclosedAgentSpans error = {e:?}"
            );
        }
    }
}

#[test]
fn test_trace_manager_tool_end_keeps_failed_activation_outside_tool_run() {
    let ctx = bg();
    let tm = TraceManager::new();
    let coord = id("inv-peer", "coordinator", "", "");
    let peer = id("inv-peer", "peer", "", "");
    let call = tool_key(&coord, "call-1", "lookup");
    tm.invocation_span(&ctx, &coord, None);
    tm.push_agent_span(&ctx, &coord);
    tm.push_llm_span(&ctx, &coord);
    tm.pop_llm_span(&ctx, &coord, true);
    tm.push_agent_span(&ctx, &peer);
    tm.push_llm_span(&ctx, &peer);
    tm.record_error(&ctx, &peer, &llm_key(&peer), err("peer model failed"));
    tm.pop_llm_span(&ctx, &peer, false);

    tm.push_tool_span(&ctx, &coord, &call);
    tm.pop_tool_span(&ctx, &coord, &call, false);
    let (_, e) = tm.pop_unclosed_agent_spans(&ctx, &coord);
    assert!(
        e.is_some(),
        "popUnclosedAgentSpans error = None, want the error of peer"
    );
}

#[test]
fn test_trace_manager_canceled_model_call_after_stop_mid_stream() {
    let (canceled_ctx, token) = with_cancel(None);
    token.cancel();
    let background = bg();
    // (name, ctx, partial, err, err_ctx, slow_ctx, want_unclosed)
    let tests: Vec<(
        &str,
        &CallbackCtx,
        bool,
        TraceError,
        &CallbackCtx,
        &CallbackCtx,
        usize,
    )> = vec![
        (
            "stopped mid-stream",
            &background,
            true,
            canceled(),
            &canceled_ctx,
            &canceled_ctx,
            0,
        ),
        (
            "model canceled its own request",
            &background,
            true,
            canceled(),
            &background,
            &background,
            2,
        ),
        (
            "agent canceled its sub-agent's context",
            &background,
            true,
            canceled(),
            &canceled_ctx,
            &background,
            2,
        ),
        (
            "no partial event",
            &background,
            false,
            canceled(),
            &canceled_ctx,
            &canceled_ctx,
            2,
        ),
        (
            "other error after a partial event",
            &background,
            true,
            err("model failed"),
            &canceled_ctx,
            &canceled_ctx,
            2,
        ),
        (
            "invocation canceled",
            &canceled_ctx,
            true,
            canceled(),
            &canceled_ctx,
            &canceled_ctx,
            2,
        ),
    ];
    for (name, ctx, partial, e, err_ctx, slow_ctx, want_unclosed) in tests {
        let tm = TraceManager::new();
        let par = id("inv-cancel", "par", "", "");
        let bot = id("inv-cancel", "bot", "par.bot", "");
        let slow = id("inv-cancel", "slow", "par.slow", "");
        tm.invocation_span(ctx, &par, None);
        tm.push_agent_span(ctx, &par);
        tm.push_agent_span(ctx, &bot);
        tm.push_agent_span(slow_ctx, &slow);
        // slow responded once, so only its error can fail it.
        tm.push_llm_span(ctx, &slow);
        tm.pop_llm_span(ctx, &slow, true);
        tm.push_llm_span(ctx, &slow);
        if partial {
            tm.record_streamed(ctx, &bot, "bot", &bot.branch, true);
        } else {
            tm.complete_response(ctx, &bot, "bot", &bot.branch, &[]);
        }
        tm.record_error(err_ctx, &slow, &llm_key(&slow), e);
        tm.pop_llm_span(ctx, &slow, false);

        let (unclosed, got) = tm.pop_unclosed_agent_spans(ctx, &par);
        assert!(
            got.is_some() == (want_unclosed > 0) && unclosed.len() == want_unclosed,
            "{name}: popUnclosedAgentSpans = ({} spans, {got:?}), want {want_unclosed} spans",
            unclosed.len()
        );
    }
}

#[test]
fn test_trace_manager_stop_point_node_path_differs_from_parent() {
    let ctx = bg();
    const INV: &str = "inv-stoppoint-path";
    // agent runs right under the invocation with no branch of its own.
    let agent = id(INV, "agent", "", "");
    // helper runs on a distinct node path under agent, in the same branch: only
    // id.path != parent.path makes it a stop point.
    let helper = id(INV, "helper", "", "agent/helper@1");

    // agent's context is not canceled, but helper's own context is.
    let (agent_ctx, agent_token) = with_cancel(None);
    let (helper_ctx, helper_token) = with_cancel(Some(&agent_token));

    let tm = TraceManager::new();
    tm.invocation_span(&ctx, &agent, None);
    tm.push_agent_span(&agent_ctx, &agent);
    // agent streams a partial event with content.
    tm.push_llm_span(&agent_ctx, &agent);
    tm.record_streamed(&ctx, &agent, "agent", &agent.branch, true);
    // helper runs on its own node path and then gets canceled.
    tm.push_agent_span(&helper_ctx, &helper);
    tm.push_llm_span(&helper_ctx, &helper);
    helper_token.cancel();
    tm.record_error(&helper_ctx, &helper, &llm_key(&helper), canceled());
    tm.pop_llm_span(&helper_ctx, &helper, false);

    let (_, e) = tm.pop_unclosed_agent_spans(&ctx, &agent);
    assert!(
        e.is_none(),
        "popUnclosedAgentSpans error = {e:?}, want None: helper's context.Canceled should be attributed to the stop, not a failure"
    );
}

#[test]
fn test_trace_manager_record_error_wraps_non_collateral_custom_cancel_cause() {
    let ctx = bg();
    const INV: &str = "inv-noncollateral-cause";
    let root = id(INV, "root", "", "");
    let sub = id(INV, "sub", "", "");

    let err_custom = err("custom sub-agent timeout");
    let (sub_ctx, cancel_sub) = with_cancel_cause(None, &err_custom);

    let tm = TraceManager::new();
    tm.invocation_span(&ctx, &root, None);
    tm.push_agent_span(&ctx, &root);
    tm.push_agent_span(&sub_ctx, &sub);
    tm.push_llm_span(&sub_ctx, &sub);
    tm.record_streamed(&ctx, &sub, "sub", &sub.branch, true);
    cancel_sub.cancel();
    tm.record_error(&sub_ctx, &sub, &llm_key(&sub), canceled());
    tm.pop_llm_span(&sub_ctx, &sub, false);

    let (unclosed, term_err) = tm.pop_unclosed_agent_spans(&ctx, &root);
    let term_err = term_err.expect("terminal error");
    assert!(
        term_err.is_context(ContextError::Canceled) && error_is(&term_err, &err_custom),
        "popUnclosedAgentSpans terminal error = {term_err}, want wrapped context.Canceled and {err_custom}"
    );
    let text = term_err.to_string();
    assert!(
        text.contains("context canceled") && text.contains(&err_custom.to_string()),
        "popUnclosedAgentSpans terminal error = {text:?}, want both \"context canceled\" and {err_custom:?}"
    );
    assert_eq!(
        unclosed.len(),
        2,
        "popUnclosedAgentSpans unclosed, want [root, sub]"
    );
}

#[test]
fn test_trace_manager_push_agent_span_returns_only_own_node_purged_attempt() {
    let tm = TraceManager::new();
    let ctx = test_ctx();
    let wf = id("inv-purge-own", "wf", "", "");
    let node_a = id("inv-purge-own", "agent_a", "", "wf@1/node_a@1");
    let node_b = id("inv-purge-own", "agent_b", "", "wf@1/node_b@1");
    let err_a = err("node A attempt 1 boom");
    let err_b = err("node B attempt 1 boom");

    tm.push_agent_span(&ctx, &wf);
    let (span_a1, purged_init_a) = tm.push_agent_span(&ctx, &node_a);
    assert!(
        purged_init_a.is_empty(),
        "initial pushAgentSpan(nodeA) purged = {purged_init_a:?}"
    );
    tm.record_error(&ctx, &node_a, &llm_key(&node_a), err_a.clone());

    let (span_b1, purged_init_b) = tm.push_agent_span(&ctx, &node_b);
    assert!(
        purged_init_b.is_empty(),
        "initial pushAgentSpan(nodeB) purged = {purged_init_b:?}"
    );
    tm.record_error(&ctx, &node_b, &llm_key(&node_b), err_b.clone());

    let (span_a2, purged_a) = tm.push_agent_span(&ctx, &node_a);
    assert!(
        purged_a.len() == 1
            && purged_a[0].agent_name == "agent_a"
            && purged_a[0].span.span_id == span_a1.span_id
            && purged_a[0]
                .err
                .as_ref()
                .is_some_and(|e| error_is(e, &err_a)),
        "retry pushAgentSpan(nodeA) purged = {purged_a:?}, want [agent_a span={} err={err_a}]",
        span_a1.span_id
    );
    assert_ne!(
        span_a2.span_id, span_a1.span_id,
        "nodeA attempt 2 reused attempt 1 spanID"
    );

    let (span_b2, purged_b) = tm.push_agent_span(&ctx, &node_b);
    assert!(
        purged_b.len() == 1
            && purged_b[0].agent_name == "agent_b"
            && purged_b[0].span.span_id == span_b1.span_id
            && purged_b[0]
                .err
                .as_ref()
                .is_some_and(|e| error_is(e, &err_b)),
        "retry pushAgentSpan(nodeB) purged = {purged_b:?}, want [agent_b span={} err={err_b}]",
        span_b1.span_id
    );
    assert_ne!(
        span_b2.span_id, span_b1.span_id,
        "nodeB attempt 2 reused attempt 1 spanID"
    );
}

#[test]
fn test_trace_manager_complete_response_preserves_collateral_error() {
    let tm = TraceManager::new();
    let (ctx, test_token) = with_cancel(None);
    let guard = id("inv-m2", "guard", "", "");
    let par = id("inv-m2", "par", "", "");
    let streamer = id("inv-m2", "streamer", "par.streamer", "");
    let err_sibling = err("sibling branch failed");

    tm.push_agent_span(&ctx, &guard);
    tm.push_agent_span(&ctx, &par);
    let (branch_ctx, cancel_branch) = with_cancel_cause(Some(&test_token), &err_sibling);
    tm.push_agent_span(&branch_ctx, &streamer);

    cancel_branch.cancel();
    tm.record_error(&branch_ctx, &streamer, &llm_key(&streamer), canceled());

    // A final event from streamer settles streamer, and later guard's own final event clears
    // retiredCause after par retires; only the preserved collateral error retains errSibling
    // for popUnclosedAgentSpans.
    tm.complete_response(&ctx, &streamer, "streamer", "par.streamer", &[]);
    tm.pop_agent_span(&branch_ctx, &streamer);
    tm.pop_agent_span(&ctx, &par);
    tm.complete_response(&ctx, &guard, "guard", "", &[]);

    let (_, term_err) = tm.pop_unclosed_agent_spans(&ctx, &guard);
    assert!(
        term_err.as_ref().is_some_and(|e| error_is(e, &err_sibling)),
        "popUnclosedAgentSpans terminal error = {term_err:?}, want {err_sibling}"
    );
}

#[test]
fn test_trace_manager_failures_locked_uses_retired_cause_on_responded_span() {
    let tm = TraceManager::new();
    let (ctx, test_token) = with_cancel(None);
    let wf = id("inv-m3", "wf", "", "");
    let outer = id("inv-m3", "outer", "", "wf@1/outer@1");
    let guard = id("inv-m3", "guard", "", "wf@1/outer@1");
    let branch = id("inv-m3", "branch", "par.branch", "wf@1/outer@1");
    let err_branch = err("parallel branch failed");

    tm.push_agent_span(&ctx, &wf);
    tm.push_agent_span(&ctx, &outer);
    tm.push_agent_span(&ctx, &guard);
    let (branch_ctx, cancel_branch) = with_cancel_cause(Some(&test_token), &err_branch);
    tm.push_agent_span(&branch_ctx, &branch);

    tm.complete_response(&branch_ctx, &branch, "branch", "par.branch", &[]);
    cancel_branch.cancel();
    tm.pop_agent_span(&branch_ctx, &branch);

    let (_, purged) = tm.push_agent_span(&ctx, &outer);
    assert_eq!(purged.len(), 2, "purged = {purged:?}, want [guard, outer]");
    assert!(
        purged[0].agent_name == "guard"
            && purged[0]
                .err
                .as_ref()
                .is_some_and(|e| error_is(e, &err_branch)),
        "purged[0] = {:?}, want guard with {err_branch}",
        purged[0]
    );
    assert!(
        purged[1].agent_name == "outer"
            && purged[1]
                .err
                .as_ref()
                .is_some_and(|e| error_is(e, &err_branch)),
        "purged[1] = {:?}, want outer with {err_branch}",
        purged[1]
    );
}

#[test]
fn test_trace_manager_retire_agent_skips_collateral_error() {
    let tm = TraceManager::new();
    let (_, test_token) = with_cancel(None);
    let (outer_ctx, cancel_outer) = with_cancel(Some(&test_token));

    let par = id("inv-m5", "par", "", "");
    let streamer = id("inv-m5", "streamer", "par.streamer", "");
    let err_sibling = err("sibling branch failed");

    tm.push_agent_span(&outer_ctx, &par);
    let (branch_ctx, cancel_branch) = with_cancel_cause(Some(&cancel_outer), &err_sibling);
    tm.push_agent_span(&branch_ctx, &streamer);
    tm.complete_response(&branch_ctx, &streamer, "streamer", "par.streamer", &[]);

    cancel_branch.cancel();
    tm.record_error(&branch_ctx, &streamer, &llm_key(&streamer), canceled());

    tm.pop_agent_span(&outer_ctx, &par);
    let inv = tm.invocation(&outer_ctx, &par);
    assert!(
        !inv.has_open_late_spans(),
        "inv.has_open_late_spans() = true after popAgentSpan(par); responded child with collateral error was not retired"
    );

    cancel_outer.cancel();
    let (unclosed, _) = tm.pop_unclosed_agent_spans(&outer_ctx, &par);
    assert!(
        unclosed.is_empty(),
        "popUnclosedAgentSpans unclosed = {unclosed:?}, want empty"
    );
}

// parity: adapted: Go counts the reads of an `endedContext`'s `Ended`; gadk's span reads an ADK
// context (Go's `endedContext` shows no activation: pushed with a plain context, then given it) and
// records the one read that reports true as `context_ended`.
#[test]
fn test_trace_manager_settle_waits_for_launcher() {
    let ctx = bg();
    const INV: &str = "inv-launcher";
    let par = id(INV, "par", "", "");
    let branch_a = id(INV, "branch_a", "par.branch_a", "");
    let sibling = id(INV, "sibling", "par.branch_a", "");
    let branch_b = id(INV, "branch_b", "par.branch_b", "");
    let cached = id(INV, "cached", "par.branch_a", "");
    let call = tool_key(&branch_b, "call-1", "run_cached");
    let non_llm = |rec: &mut crate::trace::SpanRecord| rec.non_llm = true;
    type Hook<'a> = Option<Box<dyn Fn(&TraceManager) + 'a>>;
    struct Case<'a> {
        name: &'a str,
        before: Hook<'a>,
        after: Hook<'a>,
        end: Box<dyn Fn(&TraceManager) + 'a>,
        retire: Option<&'a Identity>,
        blocked: bool,
    }
    let cases: Vec<Case> = vec![
        Case {
            name: "tool call started before",
            before: Some(Box::new(|tm| {
                tm.push_tool_span(&ctx, &branch_b, &call);
            })),
            after: None,
            end: Box::new(|tm| {
                tm.pop_tool_span(&ctx, &branch_b, &call, false);
            }),
            retire: None,
            blocked: true,
        },
        Case {
            name: "tool call started before, ended by a function response",
            before: Some(Box::new(|tm| {
                tm.push_tool_span(&ctx, &branch_b, &call);
            })),
            after: None,
            end: Box::new(|tm| {
                tm.complete_response(&ctx, &par, "branch_b", "par.branch_b", &["call-1".into()]);
            }),
            retire: None,
            blocked: true,
        },
        Case {
            name: "tool call started after",
            before: None,
            after: Some(Box::new(|tm| {
                tm.push_tool_span(&ctx, &branch_b, &call);
            })),
            end: Box::new(|tm| {
                tm.pop_tool_span(&ctx, &branch_b, &call, false);
            }),
            retire: None,
            blocked: false,
        },
        Case {
            name: "custom agent of another branch started before",
            before: Some(Box::new(|tm| set_agent_span(tm, INV, "branch_b", non_llm))),
            after: None,
            end: Box::new(|tm| {
                tm.pop_agent_span(&ctx, &branch_b);
            }),
            retire: None,
            blocked: true,
        },
        Case {
            name: "llmagent of another branch started before",
            before: None,
            after: None,
            end: Box::new(|tm| {
                tm.pop_agent_span(&ctx, &branch_b);
            }),
            retire: None,
            blocked: false,
        },
        Case {
            name: "custom agent of another branch started after",
            before: None,
            after: Some(Box::new(|tm| {
                tm.pop_agent_span(&ctx, &branch_b);
                tm.push_agent_span(&ctx, &branch_b);
                set_agent_span(tm, INV, "branch_b", non_llm);
            })),
            end: Box::new(|tm| {
                tm.pop_agent_span(&ctx, &branch_b);
            }),
            retire: None,
            blocked: false,
        },
        Case {
            name: "workflow agent above it",
            before: Some(Box::new(|tm| {
                set_agent_span(tm, INV, "par", |rec| {
                    rec.non_llm = true;
                    rec.is_parallel = true;
                    rec.propagates_child_error = true;
                    rec.children = vec!["branch_a".into(), "branch_b".into()];
                });
            })),
            after: None,
            end: Box::new(|tm| {
                tm.pop_agent_span(&ctx, &branch_b);
            }),
            retire: None,
            blocked: false,
        },
        Case {
            name: "custom non-LLM agent above parallel-looking branch without SubAgents",
            before: Some(Box::new(|tm| set_agent_span(tm, INV, "par", non_llm))),
            after: None,
            end: Box::new(|tm| {
                tm.pop_agent_span(&ctx, &branch_b);
                tm.pop_agent_span(&ctx, &par);
            }),
            retire: None,
            blocked: true,
        },
        Case {
            name: "custom non-LLM agent above parallel-looking branch with SubAgents",
            before: Some(Box::new(|tm| {
                set_agent_span(tm, INV, "par", |rec| {
                    rec.non_llm = true;
                    rec.children = vec!["branch_a".into(), "branch_b".into()];
                });
            })),
            after: None,
            end: Box::new(|tm| {
                tm.pop_agent_span(&ctx, &branch_b);
                tm.pop_agent_span(&ctx, &par);
            }),
            retire: None,
            blocked: true,
        },
        Case {
            name: "custom parent on same branch without SubAgents declared",
            before: Some(Box::new(|tm| {
                set_agent_span(tm, INV, "branch_a", non_llm);
                tm.push_agent_span(&ctx, &sibling);
            })),
            after: None,
            end: Box::new(|tm| {
                tm.pop_agent_span(&ctx, &branch_b);
                tm.pop_agent_span(&ctx, &branch_a);
            }),
            retire: Some(&sibling),
            blocked: true,
        },
        Case {
            name: "custom parent on same branch with SubAgents declared on direct parent",
            before: Some(Box::new(|tm| {
                set_agent_span(tm, INV, "branch_a", non_llm);
                tm.push_agent_span(&ctx, &sibling);
                set_agent_span(tm, INV, "sibling", |rec| {
                    rec.non_llm = true;
                    rec.children = vec!["cached".into()];
                });
            })),
            after: None,
            end: Box::new(|tm| {
                tm.pop_agent_span(&ctx, &branch_b);
                tm.pop_agent_span(&ctx, &branch_a);
            }),
            retire: Some(&sibling),
            blocked: false,
        },
    ];
    for tc in &cases {
        let name = tc.name;
        let tm = TraceManager::new();
        tm.invocation_span(&ctx, &par, None);
        tm.push_agent_span(&ctx, &par);
        tm.push_agent_span(&ctx, &branch_a);
        tm.push_agent_span(&ctx, &branch_b);
        if let Some(f) = &tc.before {
            f(&tm);
        }
        let src = push_ended(&tm, &cached, true);
        if let Some(f) = &tc.after {
            f(&tm);
        }
        assert!(
            !ended_read(&src),
            "{name}: the sub-agent's ended InvocationContext was read before its direct parent ended"
        );
        let retire = tc.retire.unwrap_or(&branch_a);
        tm.pop_agent_span(&ctx, retire);
        let at_end = ended_read(&src);
        (tc.end)(&tm);
        assert!(
            at_end != tc.blocked && ended_read(&src),
            "{name}: read of the sub-agent's ended InvocationContext at direct parent end and once launcher ended = {at_end}, {}; want {}, true",
            ended_read(&src),
            !tc.blocked
        );
        if tc.retire.is_some() {
            assert_eq!(
                invocation_of(&tm, INV).mu.lock().agent_stack.len(),
                2,
                "{name}: open spans left after enclosing parent retired, want [root, par]"
            );
        }
    }
}

// parity: adapted: Go's `endedContext` (no activation, `Ended` = end) is a span pushed with a plain
// context whose `ended` reads an ADK context that ended (or not).
#[test]
fn test_trace_manager_tool_run_ends_while_other_tool_call_is_open() {
    let ctx = bg();
    const INV: &str = "inv-tool-run-other-call";
    let coord = id(INV, "coordinator", "", "");
    let other = id(INV, "other", "other", "");
    let helper = id(INV, "helper", "helper@1", "/helper@1");
    let sub = id(INV, "sub", "helper@1", "/helper@1");
    let (other_call, helper_call) = (
        tool_key(&other, "call-o", "wait"),
        tool_key(&coord, "call-h", "helper"),
    );
    let cases: [(&str, bool, bool, &[&str]); 3] = [
        ("helper answered", true, false, &[]),
        ("helper failed silently", false, false, &["helper"]),
        (
            "helper failed silently after an answered sub-agent",
            false,
            true,
            &["helper"],
        ),
    ];
    for (name, helper_ended, with_sub, want_purged) in cases {
        let tm = TraceManager::new();
        tm.invocation_span(&ctx, &coord, None);
        tm.push_agent_span(&ctx, &coord);
        tm.push_agent_span(&ctx, &other);
        tm.push_tool_span(&ctx, &other, &other_call);
        tm.push_tool_span(&ctx, &coord, &helper_call);
        push_ended(&tm, &helper, helper_ended);
        if with_sub {
            push_ended(&tm, &sub, true);
        }
        let (_, purged, _) = tm.pop_tool_span(&ctx, &coord, &helper_call, false);
        let got: Vec<&str> = purged.iter().map(|s| s.agent_name.as_str()).collect();
        assert_eq!(
            got, want_purged,
            "{name}: activations purged with an AGENT_ERROR row when helper's tool call ended"
        );
    }
}

// parity: adapted: Go's `nodeCompletion(nil)` case ("nil event") has no counterpart: gadk's
// `node_completion` takes a `&Event`.
#[test]
fn test_node_completion() {
    let event = |path: &str, partial: bool, meta: Value| {
        let mut ev = Event::default();
        ev.response.custom_metadata = meta.as_object().cloned().unwrap_or_default();
        ev.response.partial = partial;
        if !path.is_empty() {
            ev.node_info = Some(NodeInfo {
                path: path.into(),
                ..NodeInfo::default()
            });
        }
        ev
    };
    // (name, event, want_name, want_failed, want_ok)
    let cases = [
        (
            "failed run of a nested workflow's node",
            event(
                "inner@1/flaky@1",
                false,
                json!({"adk.workflow.base_path.inner": "inner@1", "adk.workflow.base_path.outer": "outer@1", "adk.workflow.node_outcome": "failed"}),
            ),
            "inner",
            true,
            true,
        ),
        (
            "cancelled run",
            event(
                "outer@1/inner@1",
                false,
                json!({"adk.workflow.base_path.outer": "outer@1", "adk.workflow.node_outcome": "cancelled"}),
            ),
            "outer",
            true,
            true,
        ),
        (
            "completed run",
            event(
                "outer@1/inner@1",
                false,
                json!({"adk.workflow.base_path.outer": "outer@1", "adk.workflow.node_completed": true}),
            ),
            "outer",
            false,
            true,
        ),
        (
            "workflow without a node path",
            event(
                "x",
                false,
                json!({"adk.workflow.base_path.wf": "", "adk.workflow.node_completed": true}),
            ),
            "wf",
            false,
            true,
        ),
        (
            "event of a node",
            event(
                "inner@1/flaky@1",
                false,
                json!({"adk.workflow.base_path.inner": "inner@1"}),
            ),
            "",
            false,
            false,
        ),
        (
            "dynamic child of a node",
            event(
                "wf@1/fn@1/child@1",
                false,
                json!({"adk.workflow.base_path.wf": "wf@1", "adk.workflow.node_outcome": "failed"}),
            ),
            "",
            false,
            false,
        ),
        (
            "two workflows at the parent path",
            event(
                "wf@1/x@1",
                false,
                json!({"adk.workflow.base_path.a": "wf@1", "adk.workflow.base_path.b": "wf@1", "adk.workflow.node_outcome": "failed"}),
            ),
            "",
            false,
            false,
        ),
        (
            "no node path",
            event(
                "",
                false,
                json!({"adk.workflow.base_path.wf": "", "adk.workflow.node_outcome": "failed"}),
            ),
            "",
            false,
            false,
        ),
        (
            "partial",
            event(
                "wf@1/x@1",
                true,
                json!({"adk.workflow.base_path.wf": "wf@1", "adk.workflow.node_outcome": "failed"}),
            ),
            "",
            false,
            false,
        ),
    ];
    for (name, ev, want_name, want_failed, want_ok) in cases {
        let got = node_completion(&ev);
        let (got_name, got_failed, got_ok) = match &got {
            Some((n, f)) => (n.as_str(), *f, true),
            None => ("", false, false),
        };
        assert_eq!(
            (got_name, got_failed, got_ok),
            (want_name, want_failed, want_ok),
            "{name}: nodeCompletion()"
        );
    }
}

#[test]
fn test_trace_manager_stale_retired_peer_trailer_does_not_clear_later_activation_error() {
    let ctx = bg();
    const INV: &str = "inv-stale-trailer";
    let parent = id(INV, "parent", "", "");
    let worker = id(INV, "worker", "", "");
    let tm = TraceManager::new();
    tm.invocation_span(&ctx, &parent, None);
    tm.push_agent_span(&ctx, &parent);

    // Two overlapping runs of worker respond and retire.
    tm.push_agent_span(&ctx, &worker);
    tm.push_agent_span(&ctx, &worker);
    tm.complete_response(&ctx, &parent, "worker", "", &[]);
    tm.complete_response(&ctx, &parent, "worker", "", &[]);
    tm.pop_agent_span(&ctx, &worker);
    tm.pop_agent_span(&ctx, &worker);

    // A later run of worker, which overlaps no peer, fails its model call.
    let model_err = err("later worker model failed");
    tm.push_agent_span(&ctx, &worker);
    tm.push_llm_span(&ctx, &worker);
    tm.record_error(&ctx, &worker, &llm_key(&worker), model_err.clone());
    tm.pop_llm_span(&ctx, &worker, false);

    // The trailer of a retired run arrives late.
    tm.complete_response(&ctx, &parent, "worker", "", &[]);

    let (_, e) = tm.pop_unclosed_agent_spans(&ctx, &parent);
    assert!(
        e.as_ref().is_some_and(|e| error_is(e, &model_err)),
        "popUnclosedAgentSpans error = {e:?}, want {model_err}: the late trailer must not clear the error of the later activation"
    );
}

// parity: adapted: Go's `endedContext` read count is the span's `context_ended` (see
// `test_trace_manager_settle_waits_for_launcher`); the plugin subtests share one activation per
// Go `mockInvocationContext` (`MockRun`).
#[tokio::test]
async fn test_trace_manager_tool_span_across_after_run() {
    // direct pushToolSpan across endInvocation
    {
        let ctx = bg();
        let clock = ManualClock::new();
        let mut tm = TraceManager::new();
        tm.now = clock.clock();
        let id = id("inv-live-tool", "live_agent", "", "");
        let inv_start = tm.invocation_span(&ctx, &id, None);
        let (agent_start, _) = tm.push_agent_span(&ctx, &id);
        let key = tool_key(&id, "call-1", "search");
        let tool_start = tm.push_tool_span(&ctx, &id, &key);

        clock.advance_ms(25);
        tm.pop_agent_span(&ctx, &id);
        let inv_end = tm.end_invocation(&ctx, &id);
        assert_eq!(inv_end.span_id, inv_start.span_id, "endInvocation spanID");

        clock.advance_ms(40);
        let (tool_end, _, closed_by_error) = tm.pop_tool_span(&ctx, &id, &key, false);
        assert!(
            !closed_by_error,
            "popToolSpan closedByError = true, want false"
        );
        assert_eq!(
            tool_end.trace_id, tool_start.trace_id,
            "popToolSpan traceID"
        );
        assert_eq!(
            tool_end.span_id, tool_start.span_id,
            "popToolSpan spanID, want original TOOL_STARTING spanID"
        );
        assert_eq!(
            tool_end.parent_span_id, agent_start.span_id,
            "popToolSpan parentSpanID, want agent spanID"
        );
        assert_eq!(
            tool_end.latency.as_ref().map(|l| l.total_ms),
            Some(65),
            "popToolSpan total_ms (latency={:?})",
            tool_end.latency
        );
    }

    // model error before endInvocation and recovered response after
    {
        let ctx = bg();
        let tm = TraceManager::new();
        let id = id("inv-live-model", "live_agent", "", "");
        tm.invocation_span(&ctx, &id, None);
        tm.push_agent_span(&ctx, &id);
        let start = tm.push_llm_span(&ctx, &id);
        // OnModelErrorCallback ends the call, and a plugin recovers from the error, so ADK runs
        // AfterModelCallback for it after the run ended.
        tm.pop_llm_span(&ctx, &id, false);
        tm.pop_agent_span(&ctx, &id);
        tm.end_invocation(&ctx, &id);

        let end = tm.pop_llm_span(&ctx, &id, true);
        assert!(
            end.span_id == start.span_id && end.latency.is_none(),
            "LLM_RESPONSE place = {end:?}, want span {:?} of LLM_REQUEST without latency",
            start.span_id
        );
    }

    // made-up span of an unseen call stays behind
    {
        let ctx = bg();
        let tm = TraceManager::new();
        let id = id("inv-live-unseen", "live_agent", "", "");
        tm.invocation_span(&ctx, &id, None);
        tm.push_agent_span(&ctx, &id);
        // ADK runs OnToolErrorCallback alone for a call of a tool that does not exist, so the
        // call has no completion callback to wait for.
        tm.pop_tool_span(&ctx, &id, &tool_key(&id, "call-missing", "missing"), true);
        tm.pop_agent_span(&ctx, &id);
        tm.end_invocation(&ctx, &id);

        let st = tm.mu.lock();
        if let Some(entry) = st.completed.get(&id.invocation_id) {
            assert!(
                entry.late.is_none(),
                "endInvocation kept late state for a run whose only call this plugin did not see start"
            );
            let tools = entry.ended_tools.as_ref().map_or(0, |m| m.len());
            let models = entry.ended_models.as_ref().map_or(0, |m| m.len());
            assert!(
                tools == 0 && models == 0,
                "endedTools = {tools}, endedModels = {models}, want 0 and 0: a call this plugin did not see start must not wait in the lightweight maps"
            );
        }
    }

    // plugin AfterToolCallback after AfterRunCallback
    {
        let tp = setup_test_plugin().await;
        let p = &tp.plugin;
        let run = MockRun::new(&tp.ctx);
        let tool_ctx = run.tool("call-live-ok");
        let lookup = fake_tool("lookup");

        before_run(p, &run).await;
        before_agent(p, &run).await;
        before_tool(p, tool_ctx.clone(), &lookup, obj(json!({"q": "go"}))).await;
        let mut ev = new_event(&tp.ctx.invocation_id, &tp.ctx.agent_name);
        ev.response.content = Some(text("model", "streaming answer"));
        ev.response.partial = true;
        on_event(p, &run, ev).await;

        // Consumer stops reading in RunLive: AfterRun runs while tool is in flight.
        after_run(p, &run).await;

        after_tool(
            p,
            tool_ctx,
            &lookup,
            obj(json!({"q": "go"})),
            obj(json!({"ok": true})),
            None,
        )
        .await;

        let rows = tp
            .collect_rows(until_event_type(EVENT_TOOL_COMPLETED))
            .await;
        let agent_start = place_of(&only_row_of_type(&rows, EVENT_AGENT_STARTING));
        let tool_start = place_of(&only_row_of_type(&rows, EVENT_TOOL_STARTING));
        let tool_comp_row = only_row_of_type(&rows, EVENT_TOOL_COMPLETED);
        let tool_comp = place_of(&tool_comp_row);
        assert_eq!(
            tool_comp, tool_start,
            "TOOL_COMPLETED place, want TOOL_STARTING place"
        );
        assert_eq!(
            tool_comp.parent, agent_start.span,
            "TOOL_COMPLETED parent_span_id, want AGENT_STARTING span_id"
        );
        must_latency(&tool_comp_row);
    }

    // plugin OnToolErrorCallback and AfterToolCallback after AfterRunCallback
    {
        let tp = setup_test_plugin().await;
        let p = &tp.plugin;
        let run = MockRun::new(&tp.ctx);
        let tool_ctx = run.tool("call-live-err");
        let lookup = fake_tool("lookup");
        let boom = tool_err("live tool failed");

        before_run(p, &run).await;
        before_agent(p, &run).await;
        before_tool(p, tool_ctx.clone(), &lookup, Map::new()).await;
        let mut ev = new_event(&tp.ctx.invocation_id, &tp.ctx.agent_name);
        ev.response.content = Some(text("model", "partial"));
        ev.response.partial = true;
        on_event(p, &run, ev).await;

        after_run(p, &run).await;

        on_tool_error(p, tool_ctx.clone(), &lookup, boom.clone()).await;
        after_tool(p, tool_ctx, &lookup, Map::new(), Map::new(), Some(boom)).await;

        let rows = tp.collect_rows(until_event_type(EVENT_TOOL_ERROR)).await;
        assert_eq!(
            rows_of_type(&rows, EVENT_TOOL_ERROR).len(),
            1,
            "TOOL_ERROR rows"
        );
        let tool_start = place_of(&only_row_of_type(&rows, EVENT_TOOL_STARTING));
        let tool_err_row = only_row_of_type(&rows, EVENT_TOOL_ERROR);
        assert_eq!(
            place_of(&tool_err_row),
            tool_start,
            "TOOL_ERROR place, want TOOL_STARTING place"
        );
        must_latency(&tool_err_row);
    }

    // plugin AfterModelCallback after AfterRunCallback
    {
        let tp = setup_test_plugin().await;
        let p = &tp.plugin;
        let run = MockRun::new(&tp.ctx);

        before_run(p, &run).await;
        before_agent(p, &run).await;
        before_model(
            p,
            &run,
            LlmRequest {
                model: "gemini-live".into(),
                ..LlmRequest::default()
            },
        )
        .await;

        // The call has streamed nothing, so it moves to the late state: the model call of an
        // agent that a tool of RunLive runs can end after AfterRun. AfterRun reports the open
        // call; only the late row matters here.
        after_run(p, &run).await;

        after_model(
            p,
            &run,
            LlmResponse {
                content: Some(text("model", "final")),
                ..LlmResponse::default()
            },
        )
        .await;

        let rows = tp.collect_rows(until_event_type(EVENT_LLM_RESPONSE)).await;
        let llm_req = place_of(&only_row_of_type(&rows, EVENT_LLM_REQUEST));
        let llm_resp_row = only_row_of_type(&rows, EVENT_LLM_RESPONSE);
        assert_eq!(
            place_of(&llm_resp_row),
            llm_req,
            "LLM_RESPONSE place, want LLM_REQUEST place"
        );
        must_latency(&llm_resp_row);
    }

    // plugin OnToolErrorCallback before AfterRunCallback and AfterToolCallback after
    {
        let tp = setup_test_plugin().await;
        let p = &tp.plugin;
        let run = MockRun::new(&tp.ctx);
        let tool_ctx = run.tool("call-split-err");
        let lookup = fake_tool("lookup");
        let boom = tool_err("tool failed before stop");

        before_run(p, &run).await;
        before_agent(p, &run).await;
        before_tool(p, tool_ctx.clone(), &lookup, Map::new()).await;
        on_tool_error(p, tool_ctx.clone(), &lookup, boom.clone()).await;
        let mut ev = new_event(&tp.ctx.invocation_id, &tp.ctx.agent_name);
        ev.response.content = Some(text("model", "partial"));
        ev.response.partial = true;
        on_event(p, &run, ev).await;

        after_run(p, &run).await;
        after_tool(p, tool_ctx, &lookup, Map::new(), Map::new(), Some(boom)).await;

        // Flush with a sentinel row after AfterToolCallback so any duplicate TOOL_ERROR would
        // be observed before collection stops.
        let sentinel = MockRun::new(&MockCtx {
            agent_name: "sentinel".into(),
            session_id: "sess-sentinel".into(),
            invocation_id: "inv-sentinel".into(),
            user_id: "user-sentinel".into(),
            cancel: tp.ctx.cancel.clone(),
            ..MockCtx::default()
        });
        before_run(p, &sentinel).await;

        let rows = tp
            .collect_rows(|rows| rows_of_type(rows, EVENT_INVOCATION_STARTING).len() >= 2)
            .await;
        assert_eq!(
            rows_of_type(&rows, EVENT_TOOL_ERROR).len(),
            1,
            "TOOL_ERROR rows"
        );
        let tool_start = place_of(&only_row_of_type(&rows, EVENT_TOOL_STARTING));
        let tool_err_row = only_row_of_type(&rows, EVENT_TOOL_ERROR);
        assert_eq!(
            place_of(&tool_err_row),
            tool_start,
            "TOOL_ERROR place, want TOOL_STARTING place"
        );
    }

    // handed-over tool call holds back late settle until popToolSpan
    {
        let ctx = bg();
        let tm = TraceManager::new();
        const INV: &str = "inv-live-tool-seq";
        let live = id(INV, "live_agent", "", "");
        let early = id(INV, "early", "", "");
        let call = tool_key(&live, "call-1", "run_cached");

        tm.invocation_span(&ctx, &live, None);
        tm.push_agent_span(&ctx, &live);
        tm.push_agent_span(&ctx, &early);
        tm.pop_agent_span(&ctx, &early);
        tm.push_tool_span(&ctx, &live, &call);
        tm.pop_agent_span(&ctx, &live);
        tm.end_invocation(&ctx, &live);

        let branch_a = id(INV, "branch_a", "branch_a", "");
        let cached = id(INV, "cached", "branch_a", "");
        tm.push_agent_span(&ctx, &branch_a);
        let src = push_ended(&tm, &cached, true);
        let branch_a_rec = agent_record(&tm, INV, "branch_a");
        let cached_rec = agent_record(&tm, INV, "cached");
        tm.pop_agent_span(&ctx, &branch_a);
        let at_branch_end = ended_read(&src);
        tm.pop_tool_span(&ctx, &live, &call, false);
        assert!(
            !at_branch_end && ended_read(&src),
            "read of late sub-agent's ended InvocationContext at branch_a end and after popToolSpan = {at_branch_end}, {}; want false, true",
            ended_read(&src)
        );
        let late = late_trace(&tm, INV);
        // Nothing can end either activation anymore, so neither record keeps a context of its
        // callbacks or the span that it ran under.
        check_late_record_dropped(&late, &branch_a_rec);
        check_late_record_dropped(&late, &cached_rec);
        assert_eq!(late.mu.lock().agent_stack.len(), 1, "len(late.agentStack)");
        assert!(
            !late.open_agents.load(Ordering::SeqCst),
            "late.openAgents = true, want false"
        );
    }
}
