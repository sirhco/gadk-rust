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

//! Agent ends, responses, errors, retirement and settling, from ADK-Go
//! `plugin/agentanalytics/trace_manager.go`: `popDelegatingAgentSpan`, `popAgentSpan`,
//! `endRestartedNodeRuns` and `recordNodeCompletion` (attribution: node attempts started by a
//! workflow agent's scheduler), `inWorkflowNodeSubtreeLocked`, `reconcileConcurrentResponseOnPopLocked`,
//! `popTaskAgentSpan`, `finishedTaskLocked`, `noteTaskFinished`, `retireAgentLocked`,
//! `failuresLocked`, `reportedOverLocked`, `purgeActivationLocked`, `endToolRunsLocked`,
//! `markToolRunsDoneLocked` (attribution), `completeResponse`,
//! `retireDrainedDescendantsLocked`, `selectResponseOwnerLocked`, `recordError`, `clearError`,
//! `popUnclosedAgentSpans`, `responded`, `settleEndedLocked`, `settleEndedInSubtreeLocked`,
//! `canSettleInSubtreeLocked` (attribution: activations count as over),
//! `settleFinishedLocked`, `retireLateSettledLocked`, `launcherOpenLocked`,
//! `hasCanceledByStopUnderLocked`, and `popToolSpan`.
//!
//! Goroutine attribution: Go's `currentGoroutineID()` / `currentGoroutine()` read the
//! callback's fiber ([`current_goroutine_id`], [`current_goroutine`]); a record made from a
//! context that shows no activation keeps zero attribution, and the bodies take Go's
//! zero-information branches for it (an activation without goroutine information counts as
//! over, Go `:3913`).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use gadk_agent::fiber::{self, SCHEDULER_CREATOR};
use gadk_platform::go_quote;

use super::causes::{error_is, is_canceled, wrap_with_cause};
use super::invocation::{
    InvocationState, InvocationTrace, RecordedError, is_sub_node_path, prepare_root_agent_input,
    within_branch, within_path,
};
use super::manager::TraceManager;
use super::span::{
    AttemptSite, GoroutineOrigin, JoinView, NodeRunKey, RetiredCause, RowSpan, SpanKind,
    SpanRecord, SpanRef, UnclosedAgentSpan, joins_records, latency_of, parallel_branch,
};
use super::{CallbackCtx, Identity, NO_FRAME, TraceError, llm_key, tool_key_call};
use crate::context::ContextError;
use crate::error::Error;
use crate::plugin::USER_AUTHOR;

/// Go `currentGoroutineID()`: the id of the fiber the caller runs on
/// (`gadk_agent::fiber::current().id`; 0 outside any task and fiber).
pub(crate) fn current_goroutine_id() -> u64 {
    fiber::current().id
}

/// Go `currentGoroutine()` as its callers read it: only when the callback's context shows an
/// activation (Go `endedFunc(ctx) != nil`), the current fiber's id, its origin and the attempt
/// site the engine claimed for this agent run (`gadk_agent::fiber::current_attempt`); zero
/// otherwise.
// Divergence (ledger 7b-3a): Go parses the goroutine's id, creator function and frame shape out of runtime.Stack; gadk reads fiber ids set by the engine where Go starts goroutines, its creators are gadk_agent::fiber constants, and the attempt site is the engine's per-fiber slot rather than a stack-frame match.
pub(crate) fn current_goroutine(ctx: &CallbackCtx) -> (u64, GoroutineOrigin, AttemptSite) {
    if ctx.adk.is_none() {
        return (0, GoroutineOrigin::default(), AttemptSite::None);
    }
    let f = fiber::current();
    (f.id, f.origin, AttemptSite::from(fiber::current_attempt()))
}

/// Go `ctx.Err()`: `context.Canceled` once the callback's token is cancelled (Go's `Err`
/// never returns the cancel cause).
fn ctx_err(ctx: &CallbackCtx) -> Option<TraceError> {
    ctx.cancel
        .as_ref()
        .filter(|c| c.token.is_cancelled())
        .map(|_| Arc::new(Error::Context(ContextError::Canceled)))
}

/// Go `fmt.Errorf("agent %q execution failed before completion", name)`.
fn failed_before_completion(name: &str) -> TraceError {
    Arc::new(Error::msg(format!(
        "agent {} execution failed before completion",
        go_quote(name)
    )))
}

/// The kind of the record behind `r`, if any.
fn kind_of(r: Option<&SpanRef>) -> Option<SpanKind> {
    r.map(|r| r.lock().kind)
}

impl TraceManager {
    /// Go `popDelegatingAgentSpan(ctx, id)`: `Some(place of AGENT_COMPLETED)` when the new
    /// activation retires an earlier one that delegated to a completed task.
    pub(crate) fn pop_delegating_agent_span(
        &self,
        ctx: &CallbackCtx,
        id: &Identity,
    ) -> Option<RowSpan> {
        let inv = self.invocation(ctx, id);
        let now = (self.now)();
        let ambient = ctx.ambient_trace_id();
        let root_in = prepare_root_agent_input(ctx, &inv, id, None, None);
        let ended = ctx.ended_func();
        let mut guard = inv.mu.lock();
        let st = &mut *guard;
        inv.note_root_agent_locked(st, id, &root_in, true);
        for i in (1..st.agent_stack.len()).rev() {
            let rec = st.agent_stack[i].clone();
            let parent = {
                let r = rec.lock();
                if r.kind != SpanKind::Agent
                    || r.name != id.agent_name
                    || r.branch != id.branch
                    || r.path != id.path
                    || !r.task_delegated
                {
                    continue;
                }
                r.parent.clone()
            };
            if let Some(p) = parent.as_ref()
                && kind_of(Some(p)) == Some(SpanKind::Agent)
            {
                st.touch_agent_activity_locked(p, now);
            }
            // As in push_agent_span, which runs in the same callback, the runs that started on
            // this goroutine before are over only if it shows a node attempt.
            let since = if ended.is_some() && current_goroutine(ctx).2 != AttemptSite::None {
                0
            } else {
                NO_FRAME
            };
            // The finish_task event and the new activation do not show that the delegating
            // run returned, so its InvocationContext is not read.
            inv.retire_agent_locked(st, i, false, since);
            inv.retire_late_settled_locked(st);
            let r = rec.lock();
            let mut span = inv.row_span(st, &r, Some(latency_of(&r, now)), &ambient);
            span.timestamp = Some(now);
            return Some(span);
        }
        None
    }

    /// Go `popAgentSpan(ctx, id)`: the place of `AGENT_COMPLETED`.
    pub(crate) fn pop_agent_span(&self, ctx: &CallbackCtx, id: &Identity) -> RowSpan {
        let inv = self.invocation(ctx, id);
        let now = (self.now)();
        let ambient = ctx.ambient_trace_id();
        let root_in = prepare_root_agent_input(ctx, &inv, id, None, None);
        let mut guard = inv.mu.lock();
        let st = &mut *guard;
        inv.note_root_agent_locked(st, id, &root_in, true);
        let Some(i) =
            st.find_agent_for_context(ctx.activation_key(), &id.path, &id.agent_name, &id.branch)
        else {
            let child = st.new_child(&st.parent_for(id), SpanKind::Agent, now);
            let mut span = inv.row_span(st, &child.lock(), None, &ambient);
            span.timestamp = Some(now);
            return span;
        };
        let rec = st.agent_stack[i].clone();
        let (parent, non_llm, seq) = {
            let mut r = rec.lock();
            r.completed = true;
            (r.parent.clone(), r.non_llm, r.seq)
        };
        let parent_kind = kind_of(parent.as_ref());
        if let Some(p) = parent.as_ref()
            && parent_kind == Some(SpanKind::Agent)
        {
            st.touch_agent_activity_locked(p, now);
        }
        if non_llm && parent_kind == Some(SpanKind::Invocation) {
            st.clear_last_partial_locked();
        }
        inv.reconcile_concurrent_response_on_pop_locked(st, i, seq);
        inv.retire_agent_locked(st, i, true, seq);
        inv.retire_late_settled_locked(st);
        let r = rec.lock();
        let mut span = inv.row_span(st, &r, Some(latency_of(&r, now)), &ambient);
        span.timestamp = Some(now);
        span
    }

    /// Go `endRestartedNodeRuns(ctx, id)` (attribution: only node attempts that the workflow's
    /// scheduler started again are purged).
    pub(crate) fn end_restarted_node_runs(
        &self,
        ctx: &CallbackCtx,
        id: &Identity,
    ) -> Vec<UnclosedAgentSpan> {
        let inv = self.invocation(ctx, id);
        let now = (self.now)();
        let ambient = ctx.ambient_trace_id();
        let mut guard = inv.mu.lock();
        let st = &mut *guard;
        let Some(i) =
            st.find_agent_for_context(ctx.activation_key(), &id.path, &id.agent_name, &id.branch)
        else {
            return Vec::new();
        };
        let w = st.agent_stack[i].clone();
        let (w_is_workflow, w_seq) = {
            let g = w.lock();
            (g.is_workflow, g.seq)
        };
        if !w_is_workflow {
            return Vec::new();
        }
        let mut purged = Vec::new();
        // purge_activation_locked removes agent_stack[j] and the spans under it, which all
        // come after it, so the scan goes backwards.
        for j in ((i + 1)..st.agent_stack.len()).rev() {
            if j >= st.agent_stack.len() {
                continue;
            }
            let restarted = {
                let r = st.agent_stack[j].lock();
                r.kind == SpanKind::Agent
                    && r.parent.as_ref().is_some_and(|p| p.ptr_eq(&w))
                    && r.started_again
                    && r.origin
                        .started_by(SCHEDULER_CREATOR, w.lock().goroutine_id)
            };
            if !restarted {
                continue;
            }
            inv.settle_ended_in_subtree_locked(st, j, true, w_seq);
            let over = st.reported_over_locked(j);
            let failures = st.failures_locked(j, false, Some(&over), true);
            if !failures.is_empty() {
                purged.extend(inv.purge_activation_locked(st, j, &failures, now, &ambient));
            }
        }
        purged
    }

    /// Go `recordNodeCompletion(ctx, id, workflowName, path, branch, failed)` (attribution: only
    /// a node agent the workflow's scheduler started is attributed).
    pub(crate) fn record_node_completion(
        &self,
        ctx: &CallbackCtx,
        id: &Identity,
        workflow_name: &str,
        path: &str,
        branch: &str,
        failed: bool,
    ) {
        let inv = self.invocation(ctx, id);
        let guard = inv.mu.lock();
        let key = NodeRunKey {
            path: path.to_string(),
            branch: branch.to_string(),
        };
        let mut w: Option<SpanRef> = None;
        for rec in &guard.agent_stack[1..] {
            {
                let r = rec.lock();
                if r.kind != SpanKind::Agent
                    || !r.is_workflow
                    || r.name != workflow_name
                    || r.node_runs.get(&key).copied().unwrap_or(0) == 0
                {
                    continue;
                }
            }
            if w.is_some() {
                return;
            }
            w = Some(rec.clone());
        }
        let Some(w) = w else {
            return;
        };
        if w.lock().node_runs.get(&key).copied().unwrap_or(0) != 1 {
            return;
        }
        let mut run: Option<SpanRef> = None;
        for rec in &guard.agent_stack[1..] {
            if rec.ptr_eq(&w) {
                continue;
            }
            {
                let r = rec.lock();
                if !r.parent.as_ref().is_some_and(|p| p.ptr_eq(&w))
                    || r.path != path
                    || r.branch != branch
                    || r.node_run_ended
                    || !r.is_node_agent_of(&w.lock())
                {
                    continue;
                }
            }
            if run.is_some() {
                return;
            }
            run = Some(rec.clone());
        }
        match run {
            None => {
                w.lock().node_runs.remove(&key);
            }
            Some(run) if failed => {
                run.lock().node_run_ended = true;
                w.lock().node_runs.remove(&key);
            }
            Some(_) => {}
        }
    }

    /// Go `popTaskAgentSpan(ctx, id, calls)`: `Some(place of AGENT_COMPLETED)` for a task-mode
    /// activation that a `finish_task` response ends.
    pub(crate) fn pop_task_agent_span(
        &self,
        ctx: &CallbackCtx,
        id: &Identity,
        calls: &[String],
    ) -> Option<RowSpan> {
        let inv = self.invocation(ctx, id);
        let now = (self.now)();
        let ambient = ctx.ambient_trace_id();
        let root_in = prepare_root_agent_input(ctx, &inv, id, None, None);
        let mut guard = inv.mu.lock();
        let st = &mut *guard;
        inv.note_root_agent_locked(st, id, &root_in, true);
        let i = match inv.finished_task_locked(st, id, calls) {
            Some(i) => i,
            None => st.find_agent_for_context(
                ctx.activation_key(),
                &id.path,
                &id.agent_name,
                &id.branch,
            )?,
        };
        let rec = st.agent_stack[i].clone();
        let (parent, parent_span_id, name) = {
            let r = rec.lock();
            if !r.task_mode {
                return None;
            }
            (r.parent.clone(), r.parent_span_id.clone(), r.name.clone())
        };
        if let Some(p) = parent.as_ref()
            && kind_of(Some(p)) == Some(SpanKind::Agent)
        {
            st.touch_agent_activity_locked(p, now);
        }
        // Only a delegating LLM activation that configured this task as its child is marked
        // (a resumed task can start under a still-running ancestor instead).
        for d in &st.agent_stack {
            if d.ptr_eq(&rec) {
                continue;
            }
            let mut d = d.lock();
            if d.span_id == parent_span_id
                && d.kind == SpanKind::Agent
                && !d.non_llm
                && d.children.contains(&name)
            {
                d.task_delegated = true;
                break;
            }
        }
        inv.retire_agent_locked(st, i, false, NO_FRAME);
        inv.retire_late_settled_locked(st);
        let r = rec.lock();
        let mut span = inv.row_span(st, &r, Some(latency_of(&r, now)), &ambient);
        span.timestamp = Some(now);
        Some(span)
    }

    /// Go `noteTaskFinished(ctx, id, callID)`.
    pub(crate) fn note_task_finished(&self, ctx: &CallbackCtx, id: &Identity, call_id: &str) {
        if call_id.is_empty() {
            return;
        }
        let mut inv = self.invocation(ctx, id);
        loop {
            if let Some(guard) = inv.lock_live() {
                if let Some(i) = guard.find_agent_for_context(
                    ctx.activation_key(),
                    &id.path,
                    &id.agent_name,
                    &id.branch,
                ) {
                    guard.agent_stack[i]
                        .lock()
                        .finished_tasks
                        .push(call_id.to_string());
                }
                return;
            }
            inv = self.invocation(ctx, id);
        }
    }

    /// Go `completeResponse(ctx, id, author, branch, calls)`.
    pub(crate) fn complete_response(
        &self,
        ctx: &CallbackCtx,
        id: &Identity,
        author: &str,
        branch: &str,
        calls: &[String],
    ) {
        let inv = self.invocation(ctx, id);
        let now = (self.now)();
        let mut guard = inv.mu.lock();
        let st = &mut *guard;
        let was_partial = st.last_session_event_partial;
        let partial_span_id = st.last_partial_span_id.clone();
        st.clear_last_partial_locked();
        let Some(i) = st.find_agent(&id.path, author, branch) else {
            if author.is_empty() || author == USER_AUTHOR {
                return;
            }
            // Recovery content of a custom agent's own AfterAgentCallback arrives once its
            // activation is gone: scan every open span, as a concurrent undeclared sibling can
            // sit above the ancestor that holds the swallowable cause.
            for rec in &st.agent_stack {
                rec.lock().clear_retired_cause_for(author, branch, &id.path);
            }
            let path = id.path.as_str();
            inv.retire_drained_descendants_locked(st, &|p, open_set| {
                !open_set.contains(&p.span_id)
                    && !p.propagates_child_error
                    && p.context_failure_cause().is_none()
                    && p.name == author
                    && p.branch == branch
                    && (path.is_empty() || p.path.is_empty() || p.path == path)
            });
            let parent = st.parent_for(&Identity {
                agent_name: author.to_string(),
                branch: branch.to_string(),
                path: id.path.clone(),
                ..Identity::default()
            });
            if author == id.agent_name {
                return;
            }
            if let Some(j) = st.agent_stack.iter().position(|r| r.ptr_eq(&parent))
                && j > 0
            {
                st.touch_agent_activity_locked(&st.agent_stack[j].clone(), now);
                let lineage = st.lineage_locked(j);
                st.mark_responded_locked(&lineage);
            }
            return;
        };
        let (i, matched_pending, absorbed) =
            inv.select_response_owner_locked(st, i, calls, was_partial, &partial_span_id, now);
        if absorbed {
            return;
        }
        let owner = st.agent_stack[i].clone();
        st.touch_agent_activity_locked(&owner, now);
        let (owner_span_id, owner_concurrent_peer) = {
            let o = owner.lock();
            (o.span_id.clone(), o.concurrent_peer)
        };
        // An activation is created after the one it runs under, so one pass over the later
        // spans finds all that run under the owner.
        let mut under = HashSet::from([owner_span_id.clone()]);
        for rec in &st.agent_stack[i + 1..] {
            let r = rec.lock();
            if under.contains(&r.parent_span_id)
                && within_branch(&r.branch, branch)
                && within_path(&r.path, &id.path, false)
            {
                under.insert(r.span_id.clone());
            }
        }
        for rec in &st.agent_stack[i..] {
            let mut r = rec.lock();
            if under.contains(&r.span_id) {
                r.retired_causes.clear();
            }
        }
        if !owner_concurrent_peer || matched_pending {
            let cleared: Vec<String> = st
                .unrecovered_errors
                .iter()
                .filter(|(_, e)| !e.collateral && st.activation_of_locked(e) == Some(i))
                .map(|(k, _)| k.clone())
                .collect();
            for k in cleared {
                st.unrecovered_errors.remove(&k);
            }
        }
        st.llm_spans
            .retain(|_, rec| rec.lock().parent_span_id != owner_span_id);
        st.tool_spans.retain(|key, rec| {
            !(rec.lock().parent_span_id == owner_span_id
                && calls.iter().any(|c| c == tool_key_call(key)))
        });
        inv.sync_open_calls_locked(st);
        let lineage = st.lineage_locked(i);
        st.mark_responded_locked(&lineage);
        st.mark_responded_locked(&under);
        let settles = {
            let o = owner.lock();
            !o.propagates_child_error && o.context_failure_cause().is_none()
        };
        if settles {
            inv.retire_drained_descendants_locked(st, &|p, _| owner.is_record(p));
        }
        // Removing the matched tool calls may release activations already known to be over;
        // the event does not show that any run those calls made returned.
        inv.settle_finished_locked(st);
        inv.retire_late_settled_locked(st);
    }

    /// Go `recordError(ctx, id, key, err)`.
    pub(crate) fn record_error(
        &self,
        ctx: &CallbackCtx,
        id: &Identity,
        key: &str,
        err: TraceError,
    ) {
        let bare_key = key.to_string();
        let key = ctx.call_key(key);
        let inv = self.invocation(ctx, id);
        let mut guard = inv.mu.lock();
        let st = &mut *guard;
        let activation = ctx.activation_key();
        let span_id = st
            .find_agent_for_context(activation, &id.path, &id.agent_name, &id.branch)
            .map(|i| st.agent_stack[i].lock().span_id.clone())
            .unwrap_or_default();
        let mut seq = st.next_seq_locked();
        if let Some(prev) = st.unrecovered_errors.get(&key).cloned() {
            if prev.span_id.is_empty() || prev.span_id == span_id {
                seq = prev.seq;
            } else {
                st.unrecovered_errors
                    .insert(format!("{}\x00{key}", prev.span_id), prev);
            }
        }
        // The error is checked like a call that ctx starts under the agent: the contexts that a
        // workflow or a parallel agent canceled are canceled by now.
        let owner = st.owner_of(activation, id);
        let owner_kind = owner.lock().kind;
        if owner_kind == SpanKind::Agent {
            st.touch_agent_activity_locked(&owner, (self.now)());
            // Stale model- and tool-event credits go, so a peer's response cannot claim this
            // activation's events.
            let mut o = owner.lock();
            o.awaiting_model_event = 0;
            o.awaiting_tool_event = 0;
        }
        let call = SpanRecord {
            canceled: ctx.canceled_func(),
            parent: Some(owner),
            stop_point: owner_kind == SpanKind::Invocation,
            ..SpanRecord::default()
        };
        let mut err = err;
        let mut cause = None;
        let mut collateral = false;
        if is_canceled(&err) {
            let (c, coll) = call.collateral_failure_cause();
            collateral = coll;
            if let Some(c) = &c
                && !error_is(&err, c)
            {
                err = wrap_with_cause(&err, c);
            }
            cause = c;
        }
        let canceled = call.canceled_by_stop();
        st.unrecovered_errors.insert(
            key,
            RecordedError {
                err,
                cause,
                span_id,
                bare_key,
                seq,
                canceled,
                collateral,
            },
        );
    }

    /// Go `clearError(ctx, id, key)`.
    pub(crate) fn clear_error(&self, ctx: &CallbackCtx, id: &Identity, key: &str) {
        let key = ctx.call_key(key);
        let inv = self.invocation(ctx, id);
        let mut st = inv.mu.lock();
        if st.completed_models.contains_key(&key) {
            st.unrecovered_errors.remove(&key);
        }
    }

    /// Go `popUnclosedAgentSpans(ctx, id)`: the `AGENT_ERROR` activations of the run's end and
    /// the invocation's terminal error.
    pub(crate) fn pop_unclosed_agent_spans(
        &self,
        ctx: &CallbackCtx,
        id: &Identity,
    ) -> (Vec<UnclosedAgentSpan>, Option<TraceError>) {
        let inv = self.invocation(ctx, id);
        let now = (self.now)();
        let ambient = ctx.ambient_trace_id();
        let root_in = prepare_root_agent_input(ctx, &inv, id, None, None);
        let mut guard = inv.mu.lock();
        let st = &mut *guard;
        inv.note_root_agent_locked(st, id, &root_in, false);
        let cancel_err = ctx_err(ctx);
        // A caller that stopped reading on a partial last event stopped the run's workflow and
        // any parallel agent too, which cancel the work that still runs.
        let stopped_mid_stream = cancel_err.is_none() && st.last_session_event_partial;
        let canceled_by_stop = |r: &SpanRecord| stopped_mid_stream && r.canceled_by_stop();

        let mut union_lineage: HashSet<String> = HashSet::new();
        let mut span_errors: HashMap<String, TraceError> = HashMap::new();
        let mut primary_err: Option<TraceError> = None;
        let mut collateral_cause: Option<TraceError> = None;
        let attribute = |st: &InvocationState,
                         idx: usize,
                         err: &TraceError,
                         union_lineage: &mut HashSet<String>,
                         span_errors: &mut HashMap<String, TraceError>| {
            let span_id = st.agent_stack[idx].lock().span_id.clone();
            span_errors.insert(span_id.clone(), err.clone());
            union_lineage.insert(span_id);
            for ancestor in st.lineage_locked(idx) {
                span_errors
                    .entry(ancestor.clone())
                    .or_insert_with(|| err.clone());
                union_lineage.insert(ancestor);
            }
        };
        for k in st.sorted_error_keys_locked() {
            let e = &st.unrecovered_errors[&k];
            if stopped_mid_stream && e.canceled && is_canceled(&e.err) {
                continue;
            }
            if e.collateral && cancel_err.is_none() {
                collateral_cause = collateral_cause.or_else(|| e.cause.clone());
                continue;
            }
            primary_err.get_or_insert_with(|| e.err.clone());
            if let Some(idx) = st.activation_of_locked(e) {
                attribute(st, idx, &e.err, &mut union_lineage, &mut span_errors);
            }
        }
        let primary_err = primary_err.or_else(|| cancel_err.clone());
        let sibling_cause = st.open_failure_cause_locked().or(collateral_cause);
        if st.agent_stack.len() <= 1 {
            let primary = match primary_err {
                None if !ctx.invocation_ended() => sibling_cause,
                other => other,
            };
            st.agent_stack.truncate(1);
            return (Vec::new(), primary);
        }

        let mut primary_err = primary_err;
        if primary_err.is_none() && !ctx.invocation_ended() {
            // Go settles the runner root's subtree when AfterRun runs on the goroutine that
            // started it (`currentGoroutineID()`).
            let goid = current_goroutine_id();
            if goid != 0 && st.agent_stack[1].lock().goroutine_id == goid {
                inv.settle_ended_in_subtree_locked(st, 1, true, 0);
            }
            let partial_suppress = st.last_event_partial && sibling_cause.is_none();
            let mut failing_idx = {
                let st_ro: &InvocationState = st;
                let ignore_call = |call: &SpanRecord| {
                    canceled_by_stop(call)
                        || (partial_suppress
                            && !call
                                .parent
                                .as_ref()
                                .is_some_and(|p| st_ro.in_workflow_node_subtree_locked(p)))
                };
                st_ro.agent_with_open_call_locked(Some(&ignore_call))
            };
            if failing_idx.is_none() {
                for i in (1..st.agent_stack.len()).rev() {
                    let rec = st.agent_stack[i].clone();
                    if partial_suppress && !st.in_workflow_node_subtree_locked(&rec) {
                        continue;
                    }
                    // partial_lineage is empty unless the last session event is partial.
                    let (span_id, responded, stopped) = {
                        let r = rec.lock();
                        (r.span_id.clone(), r.responded(), canceled_by_stop(&r))
                    };
                    let stopped_on = st.partial_lineage.contains(&span_id)
                        && (st.last_event_partial
                            || st.has_canceled_by_stop_under_locked(i, &|r: &SpanRef| {
                                stopped_mid_stream && r.canceled_by_stop()
                            }));
                    if !responded && !stopped && !stopped_on {
                        failing_idx = Some(i);
                        break;
                    }
                }
            }
            if let Some(idx) = failing_idx {
                let err = sibling_cause
                    .clone()
                    .unwrap_or_else(|| failed_before_completion(&st.agent_stack[idx].lock().name));
                attribute(st, idx, &err, &mut union_lineage, &mut span_errors);
                primary_err = Some(err);
            } else if sibling_cause.is_some() {
                primary_err = sibling_cause;
            }
        }

        let Some(primary_err) = primary_err else {
            st.agent_stack.truncate(1);
            inv.sync_open_agents_locked(st);
            return (Vec::new(), None);
        };
        // Without a known failing activation, a canceled invocation interrupted every open
        // span; otherwise the open spans did not fail.
        let attribute_all = union_lineage.is_empty() && cancel_err.is_some();
        let mut popped = Vec::new();
        for rec in st.agent_stack[1..].iter().rev() {
            let r = rec.lock();
            if !attribute_all && !union_lineage.contains(&r.span_id) {
                continue;
            }
            let err = span_errors
                .get(&r.span_id)
                .cloned()
                .unwrap_or_else(|| primary_err.clone());
            popped.push(UnclosedAgentSpan {
                span: inv.row_span(st, &r, Some(latency_of(&r, now)), &ambient),
                agent_name: r.name.clone(),
                branch: r.branch.clone(),
                path: r.path.clone(),
                err: Some(err),
                timestamp: None,
            });
        }
        st.agent_stack.truncate(1);
        inv.sync_open_agents_locked(st);
        (popped, Some(primary_err))
    }

    /// Go `popToolSpan(ctx, id, key, fromError)`: `(place, purged, closed_by_error)`.
    pub(crate) fn pop_tool_span(
        &self,
        ctx: &CallbackCtx,
        id: &Identity,
        key: &str,
        from_error: bool,
    ) -> (RowSpan, Vec<UnclosedAgentSpan>, bool) {
        let inv = self.invocation(ctx, id);
        let now = (self.now)();
        let ambient = ctx.ambient_trace_id();
        let root_in = prepare_root_agent_input(ctx, &inv, id, None, None);
        let Some(mut guard) = inv.lock_live() else {
            return self.pop_tool_span(ctx, id, key, from_error);
        };
        let st = &mut *guard;
        inv.note_root_agent_locked(st, id, &root_in, true);
        let key = ctx.call_key(key);
        let activation = ctx.activation_key();
        let open = st.tool_spans.remove(&key);
        inv.sync_open_calls_locked(st);
        if let Some(open) = open.as_ref() {
            inv.mark_tool_runs_done_locked(st, open);
        }
        if !from_error
            && let Some(i) =
                st.find_agent_for_context(activation, &id.path, &id.agent_name, &id.branch)
        {
            st.agent_stack[i].lock().awaiting_tool_event += 1;
        }
        let mut since = NO_FRAME;
        if let Some(open) = open.as_ref() {
            let (parent, seq) = {
                let o = open.lock();
                (o.parent.clone(), o.seq)
            };
            if let Some(p) = parent.as_ref()
                && kind_of(Some(p)) == Some(SpanKind::Agent)
            {
                st.touch_agent_activity_locked(p, now);
            }
            since = seq;
        }
        let stopped = st.last_session_event_partial;
        let purged = inv.end_tool_runs_locked(st, ctx, id, stopped, now, &ambient, since);
        // The ended tool call may have run open activations under other spans.
        inv.settle_finished_locked(st);
        inv.retire_late_settled_locked(st);
        if let Some(open) = open {
            open.lock().drop_contexts();
            if from_error {
                st.completed_tools.insert(key, open.clone());
            }
            let o = open.lock();
            let mut span = inv.row_span(st, &o, Some(latency_of(&o, now)), &ambient);
            span.timestamp = Some(now);
            return (span, purged, false);
        }
        if !from_error && let Some(rec) = st.completed_tools.remove(&key) {
            let mut span = inv.row_span(st, &rec.lock(), None, &ambient);
            span.timestamp = Some(now);
            return (span, purged, true);
        }
        let owner = st.owner_of(activation, id);
        let rec = st.new_child(&owner, SpanKind::Tool, now);
        if from_error {
            rec.lock().drop_contexts();
            st.completed_tools.insert(key, rec.clone());
        }
        let mut span = inv.row_span(st, &rec.lock(), None, &ambient);
        span.timestamp = Some(now);
        (span, purged, false)
    }
}

impl InvocationTrace {
    /// Go `reconcileConcurrentResponseOnPopLocked(i, since)`.
    pub(crate) fn reconcile_concurrent_response_on_pop_locked(
        &self,
        st: &mut InvocationState,
        i: usize,
        since: u64,
    ) {
        let rec = st.agent_stack[i].clone();
        self.settle_ended_in_subtree_locked(st, i, true, since);
        let (path, name, branch, concurrent_peer) = {
            let r = rec.lock();
            (
                r.path.clone(),
                r.name.clone(),
                r.branch.clone(),
                r.concurrent_peer,
            )
        };
        let live_peers: Vec<SpanRef> = st
            .agent_stack
            .iter()
            .enumerate()
            .filter(|&(j, other)| {
                j != 0 && j != i && {
                    let o = other.lock();
                    o.kind == SpanKind::Agent
                        && o.path == path
                        && o.name == name
                        && o.branch == branch
                }
            })
            .map(|(_, other)| other.clone())
            .collect();
        let k = llm_key(&Identity {
            path,
            agent_name: name,
            branch,
            ..Identity::default()
        });
        if concurrent_peer || !live_peers.is_empty() {
            *st.retired_peer_count.entry(k.clone()).or_insert(0) += 1;
        }
        if live_peers.is_empty() {
            return;
        }
        if !rec.lock().responded() {
            for other in &live_peers {
                let mut o = other.lock();
                if o.extra_responses > 0 {
                    o.extra_responses -= 1;
                    drop(o);
                    rec.lock().has_response = true;
                    return;
                }
            }
            for other in &live_peers {
                let mut o = other.lock();
                if o.has_response && !o.model_responded {
                    o.has_response = false;
                    drop(o);
                    rec.lock().has_response = true;
                    return;
                }
            }
            *st.unresponded_retired.entry(k).or_insert(0) += 1;
            return;
        }
        let pending = {
            let r = rec.lock();
            r.awaiting_model_event + r.awaiting_tool_event
        };
        if pending > 0 {
            *st.unresponded_retired.entry(k).or_insert(0) += pending;
        }
    }

    /// Go `finishedTaskLocked(id, calls)`: the index of the activation the response ends.
    pub(crate) fn finished_task_locked(
        &self,
        st: &mut InvocationState,
        id: &Identity,
        calls: &[String],
    ) -> Option<usize> {
        let mut found = None;
        for i in (1..st.agent_stack.len()).rev() {
            let r = st.agent_stack[i].lock();
            if r.kind != SpanKind::Agent
                || r.name != id.agent_name
                || r.branch != id.branch
                || (!id.path.is_empty() && !r.path.is_empty() && r.path != id.path)
                || !calls.iter().any(|c| r.finished_tasks.contains(c))
            {
                continue;
            }
            if found.is_some() {
                return None;
            }
            found = Some(i);
        }
        found
    }

    /// Go `retireAgentLocked(i, done, since)`.
    pub(crate) fn retire_agent_locked(
        &self,
        st: &mut InvocationState,
        i: usize,
        done: bool,
        since: u64,
    ) {
        let rec = st.agent_stack[i].clone();
        let subtree = st.subtree_locked(i);
        // The activations of the subtree whose runs are known to be over are settled, unless a
        // tool call or an agent that is still open ran them instead.
        self.settle_ended_in_subtree_locked(st, i, done, since);
        // The failed activations and the open spans they run under are attributed while the
        // spans are still open.
        let mut failed = st.open_call_owners_locked(None);
        for j in 0..st.agent_stack.len() {
            let d = &st.agent_stack[j];
            let (span_id, run_done, responded) = {
                let g = d.lock();
                (g.span_id.clone(), g.run_done, g.responded())
            };
            if !subtree.contains(&span_id) {
                continue;
            }
            let unresponded = (run_done || st.launcher_open_locked(d)) && !responded;
            if failed.contains(&span_id) || unresponded {
                failed.extend(st.lineage_locked(j));
            }
        }
        st.extend_with_error_lineages(&mut failed);
        let own_ctx_cause = rec.context_failure_cause();
        let mut inherited_cause = rec.lock().first_retired_cause();
        st.agent_stack.remove(i);
        // Retire the unclosed descendant spans of rec's subtree too, except the failed ones and
        // what runs under them.
        let mut descendant_open_cause: Option<TraceError> = None;
        let mut retired: Vec<SpanRef> = Vec::new();
        let mut idx = i;
        while idx < st.agent_stack.len() {
            let d = st.agent_stack[idx].clone();
            let (d_span_id, d_parent) = {
                let g = d.lock();
                (g.span_id.clone(), g.parent.clone())
            };
            let mut can_retire = subtree.contains(&d_span_id) && !failed.contains(&d_span_id);
            let mut p = d_parent.clone();
            while can_retire {
                let Some(pr) = p else { break };
                if pr.ptr_eq(&rec) {
                    break;
                }
                let g = pr.lock();
                if g.kind != SpanKind::Agent {
                    break;
                }
                if failed.contains(&g.span_id) {
                    can_retire = false;
                }
                p = g.parent.clone();
            }
            if !can_retire {
                idx += 1;
                continue;
            }
            let mut drained_parent = d_parent.as_ref().is_some_and(|dp| !dp.ptr_eq(&rec));
            let mut p = d_parent;
            while drained_parent {
                let Some(pr) = p else { break };
                if pr.ptr_eq(&rec) {
                    break;
                }
                let g = pr.lock();
                if g.kind != SpanKind::Agent {
                    break;
                }
                if subtree.contains(&g.span_id) {
                    drained_parent = false;
                }
                p = g.parent.clone();
            }
            if drained_parent {
                inherited_cause = inherited_cause.or_else(|| d.open_failure_cause());
            } else if descendant_open_cause.is_none() {
                descendant_open_cause = d.open_failure_cause();
            }
            if st.is_late {
                retired.push(d);
            }
            st.agent_stack.remove(idx);
        }
        let cause = own_ctx_cause
            .clone()
            .or_else(|| inherited_cause.clone())
            .or_else(|| descendant_open_cause.clone());
        if let Some(cause) = cause {
            let (propagates, name, branch, path, mut anc) = {
                let r = rec.lock();
                (
                    r.propagates_child_error,
                    r.name.clone(),
                    r.branch.clone(),
                    r.path.clone(),
                    r.parent.clone(),
                )
            };
            let swallowable = !propagates
                && own_ctx_cause.is_none()
                && inherited_cause.is_some()
                && descendant_open_cause.is_none();
            while let Some(a) = anc {
                if st.agent_stack.iter().any(|r| r.ptr_eq(&a)) {
                    // The cause stays linked to rec, so recovery content from a custom agent's
                    // own AfterAgentCallback clears it (see complete_response).
                    a.lock().retired_causes.push(RetiredCause {
                        err: Some(cause),
                        name,
                        branch,
                        path,
                        swallowable_by_self: swallowable,
                    });
                    break;
                }
                anc = a.lock().parent.clone();
            }
        }
        if st.is_late {
            // Nothing can end a retired late activation anymore.
            retired.push(rec);
            st.drop_retired_late_contexts_locked(&retired);
        }
        self.sync_open_agents_locked(st);
    }

    /// Go `purgeActivationLocked(i, failures, now, ambient)`.
    pub(crate) fn purge_activation_locked(
        &self,
        st: &mut InvocationState,
        i: usize,
        failures: &HashMap<String, TraceError>,
        now: DateTime<Utc>,
        ambient: &str,
    ) -> Vec<UnclosedAgentSpan> {
        let purged = st.subtree_locked(i);
        let mut purged_spans = Vec::new();
        for rec in st.agent_stack[i..].iter().rev() {
            let r = rec.lock();
            let Some(err) = failures.get(&r.span_id) else {
                continue;
            };
            // Go's zero `lastActivity` (never touched) is the record's default here.
            let end = if r.last_activity == DateTime::<Utc>::default() {
                now
            } else {
                r.last_activity
            };
            purged_spans.push(UnclosedAgentSpan {
                span: self.row_span(st, &r, Some(latency_of(&r, end)), ambient),
                agent_name: r.name.clone(),
                branch: r.branch.clone(),
                path: r.path.clone(),
                err: Some(err.clone()),
                timestamp: Some(end),
            });
        }
        // In the late state nothing can end a purged activation anymore; only the removed
        // records qualify (the span agent_stack[i] runs under may still be open).
        let late_purged: Vec<SpanRef> = if st.is_late {
            st.agent_stack[i..]
                .iter()
                .filter(|r| purged.contains(&r.lock().span_id))
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        st.agent_stack
            .retain(|r| !purged.contains(&r.lock().span_id));
        self.sync_open_agents_locked(st);
        for calls in [
            &mut st.llm_spans,
            &mut st.completed_models,
            &mut st.tool_spans,
            &mut st.completed_tools,
        ] {
            calls.retain(|_, r| !purged.contains(&r.lock().parent_span_id));
        }
        st.unrecovered_errors
            .retain(|_, e| !purged.contains(&e.span_id));
        if purged.contains(&st.last_partial_span_id) {
            // Purging a handled activation is not reading past the partial event: reset only
            // the per-span partial state.
            st.last_event_partial = false;
            st.last_partial_span_id.clear();
        }
        self.sync_open_calls_locked(st);
        if !late_purged.is_empty() {
            st.drop_retired_late_contexts_locked(&late_purged);
        }
        purged_spans
    }

    /// Go `endToolRunsLocked(ctx, id, stopped, now, ambient, since)`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn end_tool_runs_locked(
        &self,
        st: &mut InvocationState,
        ctx: &CallbackCtx,
        id: &Identity,
        stopped: bool,
        now: DateTime<Utc>,
        ambient: &str,
        since: u64,
    ) -> Vec<UnclosedAgentSpan> {
        // In a live run the root agent has no span, so the activations a Live tool ran sit
        // under the invocation span (index 0).
        let i = st
            .find_agent_for_context(ctx.activation_key(), &id.path, &id.agent_name, &id.branch)
            .unwrap_or(0);
        let (owner_span_id, owner_path) = {
            let o = st.agent_stack[i].lock();
            (o.span_id.clone(), o.path.clone())
        };
        if st
            .tool_spans
            .values()
            .any(|r| r.lock().parent_span_id == owner_span_id)
        {
            return Vec::new();
        }
        let mut purged = Vec::new();
        // Purging or retiring agent_stack[j] removes only it and spans after it, so the scan
        // goes backwards.
        for j in ((i + 1)..st.agent_stack.len()).rev() {
            let done = {
                let r = st.agent_stack[j].lock();
                if r.parent_span_id != owner_span_id || !is_sub_node_path(&r.path, &owner_path) {
                    continue;
                }
                // Only a run on the tool call's own goroutine is known to be over with it.
                r.run_done || r.activation.is_none() || r.goroutine_id == 0
            };
            self.settle_ended_in_subtree_locked(st, j, done, since);
            let failures = st.failures_locked(j, stopped, None, true);
            if failures.is_empty() {
                self.retire_agent_locked(st, j, done, since);
            } else if done {
                purged.extend(self.purge_activation_locked(st, j, &failures, now, ambient));
            }
            // A run not known to be over that still owns an open call or has a failure
            // recorded stays open.
        }
        purged
    }

    /// Go `markToolRunsDoneLocked(call)` (attribution: no activation has a goroutine).
    pub(crate) fn mark_tool_runs_done_locked(&self, st: &mut InvocationState, call: &SpanRef) {
        let call_seq = call.lock().seq;
        let mut goid = 0;
        for rec in &st.agent_stack[1..] {
            if rec.ptr_eq(call) {
                continue;
            }
            let mut r = rec.lock();
            if r.run_done || r.goroutine_id == 0 || r.seq <= call_seq {
                continue;
            }
            if goid == 0 {
                goid = current_goroutine_id();
                if goid == 0 {
                    return;
                }
            }
            if r.goroutine_id == goid {
                r.run_done = true;
            }
        }
    }

    /// Go `retireDrainedDescendantsLocked(matchAnc)`.
    pub(crate) fn retire_drained_descendants_locked(
        &self,
        st: &mut InvocationState,
        match_anc: &dyn Fn(&SpanRecord, &HashSet<String>) -> bool,
    ) {
        let open_set: HashSet<String> = st.agent_stack[1..]
            .iter()
            .map(|r| r.lock().span_id.clone())
            .collect();
        let mut failed = st.open_call_owners_locked(None);
        for j in 1..st.agent_stack.len() {
            let d = &st.agent_stack[j];
            let (span_id, run_done, responded) = {
                let g = d.lock();
                (g.span_id.clone(), g.run_done, g.responded())
            };
            let unresponded = (run_done || st.launcher_open_locked(d)) && !responded;
            if failed.contains(&span_id) || unresponded {
                failed.extend(st.lineage_locked(j));
            }
        }
        st.extend_with_error_lineages(&mut failed);
        let remove: Vec<bool> = st
            .agent_stack
            .iter()
            .map(|d| {
                let (kind, span_id, parent) = {
                    let g = d.lock();
                    (g.kind, g.span_id.clone(), g.parent.clone())
                };
                if kind != SpanKind::Agent || failed.contains(&span_id) {
                    return false;
                }
                let Some(dp) = parent else {
                    return false;
                };
                {
                    let g = dp.lock();
                    if g.kind != SpanKind::Agent || open_set.contains(&g.span_id) {
                        return false;
                    }
                }
                let mut p = Some(dp.clone());
                while let Some(pr) = p {
                    let g = pr.lock();
                    if g.kind != SpanKind::Agent {
                        break;
                    }
                    if !pr.ptr_eq(&dp) && match_anc(&g, &open_set) {
                        return true;
                    }
                    if open_set.contains(&g.span_id) {
                        return false;
                    }
                    p = g.parent.clone();
                }
                false
            })
            .collect();
        let mut remove = remove.into_iter();
        st.agent_stack.retain(|_| !remove.next().unwrap_or(false));
        self.sync_open_agents_locked(st);
    }

    /// Go `selectResponseOwnerLocked(i, calls, wasPartial, partialSpanID, now)`.
    pub(crate) fn select_response_owner_locked(
        &self,
        st: &mut InvocationState,
        i: usize,
        calls: &[String],
        was_partial: bool,
        partial_span_id: &str,
        now: DateTime<Utc>,
    ) -> (usize, bool, bool) {
        let base = st.agent_stack[i].clone();
        let (path, name, branch, base_parent, base_concurrent_peer) = {
            let b = base.lock();
            (
                b.path.clone(),
                b.name.clone(),
                b.branch.clone(),
                b.parent.clone(),
                b.concurrent_peer,
            )
        };
        let k = llm_key(&Identity {
            path: path.clone(),
            agent_name: name.clone(),
            branch: branch.clone(),
            ..Identity::default()
        });
        let absorb_onto_parent = |st: &mut InvocationState| {
            if let Some(p) = base_parent.as_ref()
                && kind_of(Some(p)) == Some(SpanKind::Agent)
            {
                st.touch_agent_activity_locked(p, now);
                if let Some(p_idx) = st.agent_stack.iter().position(|r| r.ptr_eq(p))
                    && p_idx > 0
                {
                    let lineage = st.lineage_locked(p_idx);
                    st.mark_responded_locked(&lineage);
                }
            }
            (0, false, true)
        };
        let peer_idxs: Vec<usize> = (1..st.agent_stack.len())
            .rev()
            .filter(|&j| {
                let r = st.agent_stack[j].lock();
                r.kind == SpanKind::Agent && r.path == path && r.name == name && r.branch == branch
            })
            .collect();
        let count = |m: &HashMap<String, i64>| m.get(&k).copied().unwrap_or(0);
        if peer_idxs.len() <= 1
            && !base_concurrent_peer
            && count(&st.unresponded_retired) == 0
            && count(&st.retired_peer_count) == 0
        {
            let mut b = base.lock();
            if !calls.is_empty() {
                b.awaiting_tool_event = (b.awaiting_tool_event - calls.len() as i64).max(0);
            } else if b.awaiting_model_event > 0 {
                b.awaiting_model_event -= 1;
            }
            return (i, true, false);
        }
        // Whether activation idx has a non-collateral unrecovered error recorded, so that an
        // event credit does not let a peer's response claim its events.
        let has_unrec_err = |st: &InvocationState, idx: usize| {
            st.unrecovered_errors
                .values()
                .any(|e| !e.collateral && st.activation_of_locked(e) == Some(idx))
        };
        if !calls.is_empty() {
            for &j in &peer_idxs {
                let cand = st.agent_stack[j].clone();
                if cand.lock().awaiting_tool_event > 0 && !has_unrec_err(st, j) {
                    let mut c = cand.lock();
                    c.awaiting_tool_event = (c.awaiting_tool_event - calls.len() as i64).max(0);
                    return (j, true, false);
                }
                let cand_span_id = cand.lock().span_id.clone();
                if st.tool_spans.iter().any(|(key, rec)| {
                    rec.lock().parent_span_id == cand_span_id
                        && calls.iter().any(|c| c == tool_key_call(key))
                }) {
                    return (j, true, false);
                }
            }
        } else {
            for &j in &peer_idxs {
                let cand = st.agent_stack[j].clone();
                if cand.lock().awaiting_model_event > 0 && !has_unrec_err(st, j) {
                    cand.lock().awaiting_model_event -= 1;
                    return (j, true, false);
                }
            }
        }
        if count(&st.unresponded_retired) > 0 {
            if let Some(n) = st.unresponded_retired.get_mut(&k) {
                *n -= 1;
            }
            return absorb_onto_parent(st);
        }
        if was_partial && !partial_span_id.is_empty() {
            for &j in &peer_idxs {
                let c = st.agent_stack[j].lock();
                if c.span_id == partial_span_id && c.has_response && !c.model_responded {
                    return (j, false, false);
                }
            }
        }
        for &j in &peer_idxs {
            if !st.agent_stack[j].lock().has_response && !has_unrec_err(st, j) {
                return (j, false, false);
            }
        }
        for &j in &peer_idxs {
            if !has_unrec_err(st, j) {
                st.agent_stack[j].lock().extra_responses += 1;
                return (j, false, false);
            }
        }
        if count(&st.retired_peer_count) > 0 {
            for rec in &st.agent_stack {
                rec.lock().clear_retired_cause_for(&name, &branch, &path);
            }
            return absorb_onto_parent(st);
        }
        for &j in &peer_idxs {
            if !st.agent_stack[j].lock().has_response {
                return (j, false, false);
            }
        }
        base.lock().extra_responses += 1;
        (i, false, false)
    }

    /// Go `settleEndedInSubtreeLocked(i, done, since)`.
    pub(crate) fn settle_ended_in_subtree_locked(
        &self,
        st: &mut InvocationState,
        i: usize,
        done: bool,
        since: u64,
    ) {
        if done {
            let mut root = st.agent_stack[i].lock();
            root.settle_ended_locked();
            root.run_done = true;
        }
        let subtree = st.subtree_locked(i);
        let pending = |r: &SpanRef| {
            let g = r.lock();
            subtree.contains(&g.span_id) && !g.run_done
        };
        if st.agent_stack[i + 1..].iter().any(pending) {
            let cur_goid = if since == NO_FRAME {
                0
            } else {
                current_goroutine_id()
            };
            for rec in &st.agent_stack[i + 1..] {
                if pending(rec) && st.can_settle_in_subtree_locked(i, rec, cur_goid, since) {
                    rec.lock().run_done = true;
                }
            }
        }
        self.settle_finished_locked(st);
    }

    /// Go `settleFinishedLocked`.
    pub(crate) fn settle_finished_locked(&self, st: &mut InvocationState) {
        for rec in &st.agent_stack[1..] {
            let run_done = rec.lock().run_done;
            if run_done && !st.launcher_open_locked(rec) {
                rec.lock().settle_ended_locked();
            }
        }
    }

    /// Go `retireLateSettledLocked` (a live trace returns at once, Go's first statement).
    pub(crate) fn retire_late_settled_locked(&self, st: &mut InvocationState) {
        if !st.is_late || st.agent_stack.len() <= 1 {
            return;
        }
        // The lineage of a failed activation follows parent even across an activation that
        // already retired, as subtree_locked does.
        let mut failed = st.open_call_owners_locked(None);
        let mark_failed = |failed: &mut HashSet<String>, rec: &SpanRef| {
            let mut cur = Some(rec.clone());
            while let Some(c) = cur {
                let g = c.lock();
                if g.kind != SpanKind::Agent {
                    break;
                }
                failed.insert(g.span_id.clone());
                cur = g.parent.clone();
            }
        };
        for d in &st.agent_stack[1..] {
            let (span_id, run_done, responded) = {
                let g = d.lock();
                (g.span_id.clone(), g.run_done, g.responded())
            };
            let unresponded = (run_done || st.launcher_open_locked(d)) && !responded;
            if failed.contains(&span_id) || unresponded || d.open_failure_cause().is_some() {
                mark_failed(&mut failed, d);
            }
        }
        for e in st.unrecovered_errors.values() {
            if e.collateral {
                continue;
            }
            if let Some(j) = st.activation_of_locked(e) {
                mark_failed(&mut failed, &st.agent_stack[j]);
            }
        }
        let mut retired: Vec<SpanRef> = Vec::new();
        loop {
            let open_agents: HashSet<String> = st.agent_stack[1..]
                .iter()
                .map(|r| r.lock().span_id.clone())
                .collect();
            // Retiring an activation can release those under it, which come after it, so the
            // scan starts over.
            let mut found = None;
            for idx in (1..st.agent_stack.len()).rev() {
                let rec = &st.agent_stack[idx];
                let (span_id, parent, run_done, context_ended) = {
                    let g = rec.lock();
                    (
                        g.span_id.clone(),
                        g.parent.clone(),
                        g.run_done,
                        g.context_ended,
                    )
                };
                let parent_agent = parent.filter(|p| kind_of(Some(p)) == Some(SpanKind::Agent));
                let parent_open = parent_agent
                    .as_ref()
                    .is_some_and(|p| open_agents.contains(&p.lock().span_id));
                if !parent_open
                    && run_done
                    && context_ended
                    && !st.launcher_open_locked(rec)
                    && !failed.contains(&span_id)
                {
                    found = Some((idx, parent_agent));
                    break;
                }
            }
            let Some((idx, parent_agent)) = found else {
                break;
            };
            retired.push(st.agent_stack.remove(idx));
            if let Some(p) = parent_agent {
                retired.push(p);
            }
        }
        if !retired.is_empty() {
            st.drop_retired_late_contexts_locked(&retired);
            self.sync_open_agents_locked(st);
        }
    }
}

impl InvocationState {
    /// Go's loop adding the lineage of each non-collateral unrecovered error's activation to
    /// `failed` (`retireAgentLocked`, `retireDrainedDescendantsLocked`).
    fn extend_with_error_lineages(&self, failed: &mut HashSet<String>) {
        for e in self.unrecovered_errors.values() {
            if e.collateral {
                continue;
            }
            if let Some(j) = self.activation_of_locked(e) {
                failed.extend(self.lineage_locked(j));
            }
        }
    }

    /// Go `inWorkflowNodeSubtreeLocked(rec)` (attribution).
    pub(crate) fn in_workflow_node_subtree_locked(&self, rec: &SpanRef) -> bool {
        if !rec.lock().run_done {
            return false;
        }
        let mut cur = Some(rec.clone());
        while let Some(c) = cur {
            let g = c.lock();
            if g.kind != SpanKind::Agent {
                break;
            }
            // A record that is its own parent is compared with its own guard (never relocked).
            if let Some(p) = g.parent.as_ref()
                && (if p.ptr_eq(&c) {
                    g.is_node_agent_of(&g)
                } else {
                    g.is_node_agent_of(&p.lock())
                })
            {
                return true;
            }
            cur = g.parent.clone();
        }
        false
    }

    /// Go `failuresLocked(i, stopped, over, openCalls)`: span ID → failure.
    pub(crate) fn failures_locked(
        &self,
        i: usize,
        stopped: bool,
        over: Option<&HashSet<String>>,
        open_calls: bool,
    ) -> HashMap<String, TraceError> {
        let subtree = self.subtree_locked(i);
        let ignore = |r: &SpanRecord| r.canceled_by_stop();
        let owners = self.open_call_owners_locked(if stopped { Some(&ignore) } else { None });
        let mut errs: HashMap<String, TraceError> = HashMap::new();
        let mut first_err: Option<TraceError> = None;
        let mut collateral_cause: Option<TraceError> = None;
        for k in self.sorted_error_keys_locked() {
            let e = &self.unrecovered_errors[&k];
            if !subtree.contains(&e.span_id) || (stopped && e.canceled && is_canceled(&e.err)) {
                continue;
            }
            if e.collateral {
                collateral_cause = collateral_cause.or_else(|| e.cause.clone());
                continue;
            }
            errs.entry(e.span_id.clone())
                .or_insert_with(|| e.err.clone());
            first_err.get_or_insert_with(|| e.err.clone());
        }
        let parent = self.agent_stack[i].lock().parent.clone();
        let parent_alive = parent.is_none_or(|p| p.context_failure_cause().is_none());
        let in_over = |span_id: &str| over.is_some_and(|o| o.contains(span_id));
        let mut failed: HashSet<String> = HashSet::new();
        let mut subtree_cause: Option<TraceError> = None;
        for j in i..self.agent_stack.len() {
            let rec = &self.agent_stack[j];
            let (span_id, retired_cause, run_done, responded, has_ended) = {
                let g = rec.lock();
                (
                    g.span_id.clone(),
                    g.first_retired_cause(),
                    g.run_done,
                    g.responded(),
                    g.ended.is_some(),
                )
            };
            if !subtree.contains(&span_id) {
                continue;
            }
            if let Some(cause) = retired_cause
                && parent_alive
                && !errs.contains_key(&span_id)
            {
                errs.insert(span_id.clone(), cause);
            }
            subtree_cause = subtree_cause.or_else(|| rec.open_failure_cause());
            let unresponded = (run_done || in_over(&span_id))
                && !responded
                && !(stopped && rec.canceled_by_stop())
                && !(j != i && has_ended && self.launcher_open_locked(rec));
            let open_call =
                owners.contains(&span_id) && (open_calls || run_done || in_over(&span_id));
            if errs.contains_key(&span_id) || open_call || unresponded {
                failed.extend(self.lineage_locked(j));
            }
        }
        let first_err = first_err.or(subtree_cause).or(collateral_cause);
        if parent_alive && failed.is_empty() && first_err.is_some() {
            failed.insert(self.agent_stack[i].lock().span_id.clone());
        }
        // An activation is created after the ones it runs under, so the lineage of a failed
        // one leaves the subtree only above agent_stack[i].
        let mut failures = HashMap::with_capacity(failed.len());
        for rec in &self.agent_stack[i..] {
            let r = rec.lock();
            if !failed.contains(&r.span_id) {
                continue;
            }
            let err = errs
                .get(&r.span_id)
                .cloned()
                .or_else(|| first_err.clone())
                .unwrap_or_else(|| failed_before_completion(&r.name));
            failures.insert(r.span_id.clone(), err);
        }
        failures
    }

    /// Go `reportedOverLocked(i)`. Locks one record at a time: the child's [`JoinView`] is
    /// read before its parent is locked (R10).
    pub(crate) fn reported_over_locked(&self, i: usize) -> HashSet<String> {
        let subtree = self.subtree_locked(i);
        let mut over = HashSet::from([self.agent_stack[i].lock().span_id.clone()]);
        for d in &self.agent_stack[i + 1..] {
            let (span_id, no_info, parent) = {
                let g = d.lock();
                if !subtree.contains(&g.span_id) || g.run_done {
                    continue;
                }
                (
                    g.span_id.clone(),
                    g.activation.is_none() || g.goroutine_id == 0,
                    g.parent.clone(),
                )
            };
            if no_info {
                over.insert(span_id);
                continue;
            }
            let mut c = d.clone();
            let mut p = parent;
            while let Some(pr) = p {
                let cv = JoinView::of(&c.lock());
                let (is_over, next) = {
                    let pg = pr.lock();
                    if pg.kind != SpanKind::Agent || !joins_records(&pg, &cv, true) {
                        break;
                    }
                    (pg.run_done || over.contains(&pg.span_id), pg.parent.clone())
                };
                if is_over {
                    over.insert(span_id);
                    break;
                }
                c = pr;
                p = next;
            }
        }
        over
    }

    /// Go `canSettleInSubtreeLocked(rootIdx, d, curGoid, since)` (attribution: with no
    /// goroutine information every activation counts as over, Go `:3913`). Locks one record at
    /// a time, as [`InvocationState::reported_over_locked`] (R10).
    pub(crate) fn can_settle_in_subtree_locked(
        &self,
        root_idx: usize,
        d: &SpanRef,
        cur_goid: u64,
        since: u64,
    ) -> bool {
        let root = &self.agent_stack[root_idx];
        let returned_here =
            |r: &SpanRecord| cur_goid != 0 && r.goroutine_id == cur_goid && r.seq > since;
        let mut p = {
            let g = d.lock();
            if g.activation.is_none() || g.goroutine_id == 0 || returned_here(&g) {
                return true;
            }
            g.parent.clone()
        };
        let mut c = d.clone();
        while let Some(pr) = p {
            let cv = JoinView::of(&c.lock());
            let next = {
                let pg = pr.lock();
                if pg.kind != SpanKind::Agent {
                    break;
                }
                if !joins_records(&pg, &cv, false) {
                    return false;
                }
                if pg.run_done || returned_here(&pg) {
                    return true;
                }
                if pr.ptr_eq(root) {
                    return false;
                }
                pg.parent.clone()
            };
            c = pr;
            p = next;
        }
        false
    }

    /// Go `launcherOpenLocked(rec)`.
    pub(crate) fn launcher_open_locked(&self, rec: &SpanRef) -> bool {
        let (rec_seq, rec_branch, rec_path) = {
            let r = rec.lock();
            (r.seq, r.branch.clone(), r.path.clone())
        };
        if self
            .tool_spans
            .values()
            .any(|call| call.lock().seq < rec_seq)
        {
            return true;
        }
        for a_ref in &self.agent_stack[1..] {
            let (a_seq, a_branch, a_path, a_name, a_is_parallel, a_children) = {
                let a = a_ref.lock();
                if !a.non_llm || a.run_done || a.seq >= rec_seq {
                    continue;
                }
                (
                    a.seq,
                    a.branch.clone(),
                    a.path.clone(),
                    a.name.clone(),
                    a.is_parallel,
                    a.children.clone(),
                )
            };
            if !(within_branch(&rec_branch, &a_branch) && within_path(&rec_path, &a_path, false)) {
                return true;
            }
            let mut anc = Some(rec.clone());
            while let Some(anc_ref) = anc {
                if anc_ref.ptr_eq(a_ref) {
                    break;
                }
                let (anc_name, parent) = {
                    let g = anc_ref.lock();
                    if g.kind != SpanKind::Agent || g.seq <= a_seq {
                        break;
                    }
                    (g.name.clone(), g.parent.clone())
                };
                if let Some(p) = parent.as_ref()
                    && !p.ptr_eq(a_ref)
                {
                    let pg = p.lock();
                    let parallel_child = pg.parent.as_ref().is_some_and(|pp| pp.ptr_eq(a_ref))
                        && a_is_parallel
                        && a_children.contains(&pg.name)
                        && pg.branch == parallel_branch(&a_ref.lock(), &pg.name);
                    if pg.kind == SpanKind::Agent
                        && within_branch(&pg.branch, &a_branch)
                        && within_path(&pg.path, &a_path, a_name == pg.name)
                        && !parallel_child
                        && !pg.children.contains(&anc_name)
                    {
                        return true;
                    }
                }
                anc = parent;
            }
        }
        false
    }

    /// Go `hasCanceledByStopUnderLocked(i, canceledByStop)`.
    pub(crate) fn has_canceled_by_stop_under_locked(
        &self,
        i: usize,
        canceled_by_stop: &dyn Fn(&SpanRef) -> bool,
    ) -> bool {
        let subtree = self.subtree_locked(i);
        for rec in &self.agent_stack[i..] {
            let in_subtree = subtree.contains(&rec.lock().span_id);
            if in_subtree && canceled_by_stop(rec) {
                return true;
            }
        }
        for calls in [&self.llm_spans, &self.tool_spans] {
            for call in calls.values() {
                let in_subtree = subtree.contains(&call.lock().parent_span_id);
                if in_subtree && canceled_by_stop(call) {
                    return true;
                }
            }
        }
        false
    }
}

impl SpanRecord {
    /// Go `responded`.
    pub(crate) fn responded(&self) -> bool {
        self.has_response || self.context_ended
    }

    /// Go `settleEndedLocked`: reads `ended` once the run is known to be over.
    pub(crate) fn settle_ended_locked(&mut self) {
        if self.ended.as_ref().is_some_and(|c| c.ended()) {
            self.context_ended = true;
            self.ended = None;
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use gadk_agent::Mode;
    use tokio_util::sync::CancellationToken;

    use super::super::span::CancelSource;
    use super::super::testkit::{ManualClock, adk_ctx, custom, ident, llm, sequential};
    use super::super::{llm_key, tool_key};
    use super::*;
    use crate::trace::causes::is_canceled;

    fn manager(clock: &ManualClock) -> TraceManager {
        let mut tm = TraceManager::new();
        tm.now = clock.clock();
        tm
    }

    fn open_agents(tm: &TraceManager, inv: &str) -> usize {
        let inv = tm.invocation(&CallbackCtx::background(), &ident(inv, "", "", ""));
        inv.mu.lock().agent_stack.len() - 1
    }

    fn state<R>(tm: &TraceManager, inv: &str, f: impl FnOnce(&mut InvocationState) -> R) -> R {
        let inv = tm.invocation(&CallbackCtx::background(), &ident(inv, "", "", ""));
        let mut guard = inv.mu.lock();
        f(&mut guard)
    }

    fn err(text: &str) -> TraceError {
        Arc::new(Error::msg(text))
    }

    fn canceled_token() -> CancellationToken {
        let token = CancellationToken::new();
        token.cancel();
        token
    }

    // Go TestTraceManager_ShortCircuitedLastChildRetiredWhenParentPops.
    #[test]
    fn answered_child_retires_when_its_parent_pops() {
        let clock = ManualClock::new();
        let tm = manager(&clock);
        let child = llm("shortcut_last", vec![], Mode::Unset);
        let seq = sequential("sequence", vec![child]);
        let bg = CallbackCtx::background();
        let seq_id = ident("inv", "sequence", "", "sequence@0");
        let child_id = ident("inv", "shortcut_last", "", "sequence@0");
        tm.invocation_span(&bg, &seq_id, Some(&seq));
        let (seq_start, _) = tm.push_agent_span(&bg, &seq_id);
        tm.push_agent_span(&bg, &child_id);
        assert_eq!(open_agents(&tm, "inv"), 2);
        // ADK writes the BeforeAgentCallback's content as an event of the child.
        tm.complete_response(&bg, &child_id, "shortcut_last", "", &[]);
        clock.advance_ms(30);
        let end = tm.pop_agent_span(&bg, &seq_id);
        assert_eq!(end.span_id, seq_start.span_id);
        assert_eq!(end.latency.unwrap().total_ms, 30);
        assert_eq!(end.timestamp, Some(clock.now()));
        assert_eq!(open_agents(&tm, "inv"), 0);
        assert_eq!(
            tm.pop_unclosed_agent_spans(&bg, &seq_id)
                .1
                .map(|e| e.to_string()),
            None
        );
    }

    #[test]
    fn after_agent_without_before_agent_gets_a_span_of_its_own() {
        let clock = ManualClock::new();
        let tm = manager(&clock);
        let bg = CallbackCtx::background();
        let root_id = ident("inv", "root", "", "root");
        let inv_span = tm.invocation_span(&bg, &root_id, None);
        let end = tm.pop_agent_span(&bg, &root_id);
        assert_eq!(end.parent_span_id, inv_span.span_id);
        assert_ne!(end.span_id, inv_span.span_id);
        assert!(end.latency.is_none());
    }

    /// Runs `root` (sequential) → `child` (custom) with ADK contexts; the child's
    /// BeforeAgentCallback answered (ending its InvocationContext) when `answered`. The child
    /// gets no AfterAgentCallback; the root ends normally, then the run ends.
    fn run_with_silent_child(answered: bool) -> (Vec<UnclosedAgentSpan>, Option<TraceError>) {
        let clock = ManualClock::new();
        let tm = manager(&clock);
        let child = custom("child", vec![]);
        let root = sequential("root", vec![child.clone()]);
        let root_ctx = CallbackCtx::from_context(&adk_ctx(&root, "inv", Some(&root)));
        let child_adk = adk_ctx(&child, "inv", Some(&root));
        let child_ctx = CallbackCtx::from_context(&child_adk);
        let root_id = ident("inv", "root", "", "root");
        let child_id = ident("inv", "child", "", "root/child");
        tm.invocation_span(&root_ctx, &root_id, Some(&root));
        tm.push_agent_span(&root_ctx, &root_id);
        let (child_start, _) = tm.push_agent_span(&child_ctx, &child_id);
        if answered {
            child_adk.end_invocation();
        }
        tm.pop_agent_span(&root_ctx, &root_id);
        let open = open_agents(&tm, "inv");
        assert_eq!(open, usize::from(!answered));
        let (unclosed, err) = tm.pop_unclosed_agent_spans(&root_ctx, &root_id);
        if !answered {
            assert_eq!(unclosed.len(), 1);
            assert_eq!(unclosed[0].span.span_id, child_start.span_id);
        }
        assert_eq!(open_agents(&tm, "inv"), 0);
        (unclosed, err)
    }

    #[test]
    fn ended_context_settles_a_child_once_its_parent_ran() {
        let (unclosed, err) = run_with_silent_child(true);
        assert!(unclosed.is_empty());
        assert!(err.is_none());
        let (unclosed, err) = run_with_silent_child(false);
        let want = "agent \"child\" execution failed before completion";
        assert_eq!(err.unwrap().to_string(), want);
        assert_eq!(unclosed[0].agent_name, "child");
        assert_eq!(unclosed[0].err.as_ref().unwrap().to_string(), want);
        assert!(unclosed[0].timestamp.is_none());
        assert!(unclosed[0].span.latency.is_some());
    }

    // Go TestTraceManager_CancellationAttributesEveryOpenSpan.
    #[test]
    fn cancellation_attributes_every_open_span() {
        let clock = ManualClock::new();
        let tm = manager(&clock);
        let bg = CallbackCtx::background();
        let root_id = ident("inv-canceled", "root", "", "root");
        let child_id = ident("inv-canceled", "child", "", "root/child");
        let (root_start, _) = tm.push_agent_span(&bg, &root_id);
        let (child_start, _) = tm.push_agent_span(&bg, &child_id);
        tm.push_llm_span(&bg, &child_id);
        tm.pop_llm_span(&bg, &child_id, true);
        let ctx = CallbackCtx::background().with_cancel(canceled_token());
        let (unclosed, term) = tm.pop_unclosed_agent_spans(&ctx, &root_id);
        assert!(is_canceled(&term.unwrap()));
        let got: Vec<&str> = unclosed.iter().map(|u| u.span.span_id.as_str()).collect();
        assert_eq!(
            got,
            [child_start.span_id.as_str(), root_start.span_id.as_str()]
        );
        assert!(
            unclosed
                .iter()
                .all(|u| is_canceled(u.err.as_ref().unwrap()))
        );
    }

    // Go TestTraceManager_ErrorOfUnknownActivation_FailsInvocationWithoutAgentErrors.
    #[test]
    fn error_of_unknown_activation_fails_without_agent_errors() {
        let clock = ManualClock::new();
        let tm = manager(&clock);
        let bg = CallbackCtx::background();
        let root_id = ident("inv", "root", "", "root");
        let sibling_id = ident("inv", "branch_b", "", "root/b");
        let unknown_id = ident("inv", "unknown", "", "root/unknown");
        tm.push_agent_span(&bg, &root_id);
        tm.push_agent_span(&bg, &sibling_id);
        tm.push_llm_span(&bg, &sibling_id);
        tm.pop_llm_span(&bg, &sibling_id, true);
        let model_err = err("model failure 500");
        tm.record_error(&bg, &unknown_id, &llm_key(&unknown_id), model_err.clone());
        let (unclosed, term) = tm.pop_unclosed_agent_spans(&bg, &root_id);
        assert!(Arc::ptr_eq(&term.unwrap(), &model_err));
        assert!(unclosed.is_empty());
    }

    #[test]
    fn errors_are_cleared_only_for_recovered_calls() {
        let clock = ManualClock::new();
        let tm = manager(&clock);
        let bg = CallbackCtx::background();
        let a_id = ident("inv", "a", "", "a");
        let key = llm_key(&a_id);
        let (a_start, _) = tm.push_agent_span(&bg, &a_id);
        tm.push_llm_span(&bg, &a_id);
        // OnModelErrorCallback: the call ends with an error, and a plugin recovers.
        tm.pop_llm_span(&bg, &a_id, false);
        tm.record_error(&bg, &a_id, &key, err("boom"));
        tm.clear_error(&bg, &a_id, &key);
        assert!(state(&tm, "inv", |st| st.unrecovered_errors.is_empty()));
        // A later call of the agent does not clear an unrecovered error.
        tm.push_llm_span(&bg, &a_id);
        tm.record_error(&bg, &a_id, &key, err("again"));
        tm.clear_error(&bg, &a_id, &key);
        let (unclosed, term) = tm.pop_unclosed_agent_spans(&bg, &a_id);
        assert_eq!(term.unwrap().to_string(), "again");
        assert_eq!(unclosed.len(), 1);
        assert_eq!(unclosed[0].span.span_id, a_start.span_id);
        assert_eq!(unclosed[0].err.as_ref().unwrap().to_string(), "again");
    }

    #[test]
    fn recorded_cancellations_keep_their_cause_and_stop_point() {
        let clock = ManualClock::new();
        let tm = manager(&clock);
        let bg = CallbackCtx::background();
        let a_id = ident("inv", "a", "", "a");
        tm.push_agent_span(&bg, &a_id);
        let cause = err("budget exhausted");
        let ctx = CallbackCtx::background()
            .with_cancel(canceled_token())
            .with_cancel_cause(cause.clone());
        let canceled: TraceError = Arc::new(Error::Context(ContextError::Canceled));
        tm.record_error(&ctx, &a_id, "custom", canceled.clone());
        // An agent's own cancellation of a context it runs a call with: no stop point.
        tm.record_error(
            &CallbackCtx::background().with_cancel(canceled_token()),
            &a_id,
            "own",
            canceled.clone(),
        );
        // The runner's cancellation of a call outside any agent span: a stop point.
        let nobody = ident("inv", "nobody", "", "nobody");
        tm.record_error(
            &CallbackCtx::background().with_cancel(canceled_token()),
            &nobody,
            "stop",
            canceled,
        );
        state(&tm, "inv", |st| {
            let e = &st.unrecovered_errors["custom"];
            assert_eq!(e.err.to_string(), "context canceled: budget exhausted");
            assert!(is_canceled(&e.err));
            assert!(Arc::ptr_eq(e.cause.as_ref().unwrap(), &cause));
            assert!(!e.collateral && !e.canceled);
            let own = &st.unrecovered_errors["own"];
            assert!(own.cause.is_none() && !own.canceled);
            let stop = &st.unrecovered_errors["stop"];
            assert!(stop.canceled && stop.span_id.is_empty());
            // A second error of the same key under the same activation keeps its order.
            assert!(e.seq < own.seq && own.seq < stop.seq);
        });
        // The caller stopped reading on a partial event: the stop's cancellation is no
        // failure, the others are.
        tm.record_streamed(&bg, &a_id, "a", "", true);
        let (_, term) = tm.pop_unclosed_agent_spans(&bg, &a_id);
        assert_eq!(
            term.unwrap().to_string(),
            "context canceled: budget exhausted"
        );
    }

    // Go TestTraceManager_FunctionResponseRetiresOnlyItsToolCall.
    #[test]
    fn function_response_ends_only_its_tool_calls() {
        let clock = ManualClock::new();
        let tm = manager(&clock);
        let bg = CallbackCtx::background();
        let a_id = ident("inv", "a", "", "a");
        tm.push_agent_span(&bg, &a_id);
        tm.push_llm_span(&bg, &a_id);
        let (key_a, key_b) = (
            tool_key(&a_id, "call_a", "t"),
            tool_key(&a_id, "call_b", "t"),
        );
        tm.push_tool_span(&bg, &a_id, &key_a);
        tm.push_tool_span(&bg, &a_id, &key_b);
        tm.complete_response(&bg, &a_id, "a", "", &["call_a".to_string()]);
        let inv = tm.invocation(&bg, &a_id);
        let st = inv.mu.lock();
        assert!(st.llm_spans.is_empty());
        assert_eq!(st.tool_spans.keys().collect::<Vec<_>>(), [&key_b]);
        assert_eq!(inv.open_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(st.agent_stack[1].lock().has_response);
    }

    #[test]
    fn response_of_an_author_without_a_span_marks_where_it_would_run() {
        let clock = ManualClock::new();
        let tm = manager(&clock);
        let bg = CallbackCtx::background();
        let root_id = ident("inv", "root", "", "root");
        tm.push_agent_span(&bg, &root_id);
        tm.complete_response(&bg, &root_id, "user", "", &[]);
        assert!(!state(&tm, "inv", |st| st.agent_stack[1]
            .lock()
            .has_response));
        // An earlier plugin answered for `helper` before this plugin saw it start.
        let helper = ident("inv", "root", "", "root/helper");
        tm.complete_response(&bg, &helper, "helper", "", &[]);
        assert!(state(&tm, "inv", |st| st.agent_stack[1]
            .lock()
            .has_response));
    }

    #[test]
    fn tool_runs_end_with_their_tool_call() {
        for responded in [false, true] {
            let clock = ManualClock::new();
            let tm = manager(&clock);
            let bg = CallbackCtx::background();
            let owner = ident("inv", "owner", "", "owner");
            let helper = ident("inv", "helper", "", "owner/helper@1");
            tm.push_agent_span(&bg, &owner);
            let key = tool_key(&owner, "call_1", "run_helper");
            let tool = tm.push_tool_span(&bg, &owner, &key);
            let (helper_start, _) = tm.push_agent_span(&bg, &helper);
            if responded {
                tm.push_llm_span(&bg, &helper);
                tm.pop_llm_span(&bg, &helper, true);
            }
            clock.advance_ms(10);
            let (span, purged, closed_by_error) = tm.pop_tool_span(&bg, &owner, &key, false);
            assert_eq!(span.span_id, tool.span_id);
            assert_eq!(span.latency.unwrap().total_ms, 10);
            assert!(!closed_by_error);
            assert_eq!(open_agents(&tm, "inv"), 1, "responded {responded}");
            if responded {
                assert!(purged.is_empty());
            } else {
                // The agent ran as a tool failed: ADK returned its failure as the tool's
                // error, so it is purged without failing the invocation.
                assert_eq!(purged.len(), 1);
                assert_eq!(purged[0].span.span_id, helper_start.span_id);
                assert_eq!(
                    purged[0].err.as_ref().unwrap().to_string(),
                    "agent \"helper\" execution failed before completion"
                );
                assert_eq!(purged[0].timestamp, helper_start.timestamp);
            }
            state(&tm, "inv", |st| {
                assert_eq!(st.agent_stack[1].lock().awaiting_tool_event, 1);
            });
        }
    }

    #[test]
    fn tool_errors_close_the_call_once() {
        let clock = ManualClock::new();
        let tm = manager(&clock);
        let bg = CallbackCtx::background();
        let a_id = ident("inv", "a", "", "a");
        let (a_start, _) = tm.push_agent_span(&bg, &a_id);
        let key = tool_key(&a_id, "call_1", "t");
        let tool = tm.push_tool_span(&bg, &a_id, &key);
        // OnToolErrorCallback, then the AfterToolCallback ADK runs for the same call.
        let (on_error, _, closed) = tm.pop_tool_span(&bg, &a_id, &key, true);
        assert_eq!(
            (on_error.span_id.as_str(), closed),
            (tool.span_id.as_str(), false)
        );
        assert!(on_error.latency.is_some());
        let (after, _, closed) = tm.pop_tool_span(&bg, &a_id, &key, false);
        assert_eq!(
            (after.span_id.as_str(), closed),
            (tool.span_id.as_str(), true)
        );
        assert!(after.latency.is_none());
        // A call of a tool that does not exist gets a span of its own under the agent.
        let unknown = tool_key(&a_id, "call_2", "missing");
        let (span, _, closed) = tm.pop_tool_span(&bg, &a_id, &unknown, true);
        assert_eq!(span.parent_span_id, a_start.span_id);
        assert!(!closed && span.latency.is_none());
        assert!(state(&tm, "inv", |st| st
            .completed_tools
            .contains_key(&unknown)));
    }

    #[test]
    fn task_mode_completion_and_delegating_reentry() {
        let clock = ManualClock::new();
        let tm = manager(&clock);
        let task = llm("task", vec![], Mode::Task);
        let planner = llm("planner", vec![task.clone()], Mode::Chat);
        let planner_ctx = CallbackCtx::from_context(&adk_ctx(&planner, "inv", Some(&planner)));
        let task_ctx = CallbackCtx::from_context(&adk_ctx(&task, "inv", Some(&planner)));
        let planner_id = ident("inv", "planner", "planner", "");
        let task_id = ident("inv", "task", "planner.task", "");
        tm.invocation_span(&planner_ctx, &planner_id, Some(&planner));
        let (planner_start, _) = tm.push_agent_span(&planner_ctx, &planner_id);
        let (task_start, _) = tm.push_agent_span(&task_ctx, &task_id);
        assert_eq!(task_start.parent_span_id, planner_start.span_id);
        // A response that is not a finished task of a task-mode agent ends nothing.
        assert!(
            tm.pop_task_agent_span(&planner_ctx, &planner_id, &[])
                .is_none()
        );
        assert!(
            tm.pop_delegating_agent_span(&planner_ctx, &planner_id)
                .is_none()
        );
        tm.note_task_finished(&task_ctx, &task_id, "call_1");
        tm.note_task_finished(&task_ctx, &task_id, "");
        clock.advance_ms(5);
        let done = tm
            .pop_task_agent_span(&planner_ctx, &task_id, &["call_1".to_string()])
            .unwrap();
        assert_eq!(done.span_id, task_start.span_id);
        assert_eq!(done.latency.unwrap().total_ms, 5);
        assert_eq!(open_agents(&tm, "inv"), 1);
        assert!(state(&tm, "inv", |st| st.agent_stack[1]
            .lock()
            .task_delegated));
        // The planner runs again: its interrupted activation ends.
        let again = CallbackCtx::from_context(&adk_ctx(&planner, "inv", Some(&planner)));
        let ended = tm.pop_delegating_agent_span(&again, &planner_id).unwrap();
        assert_eq!(ended.span_id, planner_start.span_id);
        assert_eq!(open_agents(&tm, "inv"), 0);
    }

    #[test]
    fn concurrent_peer_events_are_absorbed_after_an_unresponded_peer_retired() {
        let clock = ManualClock::new();
        let tm = manager(&clock);
        let a = custom("a", vec![]);
        let root = sequential("root", vec![a.clone()]);
        let root_ctx = CallbackCtx::from_context(&adk_ctx(&root, "inv", Some(&root)));
        let first = CallbackCtx::from_context(&adk_ctx(&a, "inv", Some(&root)));
        let second = CallbackCtx::from_context(&adk_ctx(&a, "inv", Some(&root)));
        let root_id = ident("inv", "root", "root", "");
        let a_id = ident("inv", "a", "root.a", "");
        tm.invocation_span(&root_ctx, &root_id, Some(&root));
        tm.push_agent_span(&first, &a_id);
        tm.push_agent_span(&second, &a_id);
        tm.push_llm_span(&second, &a_id);
        tm.pop_llm_span(&second, &a_id, true);
        tm.record_error(&second, &a_id, &llm_key(&a_id), err("second failed"));
        // The first peer retires cleanly before its buffered event arrives.
        tm.pop_agent_span(&first, &a_id);
        let k = llm_key(&ident("", "a", "root.a", ""));
        state(&tm, "inv", |st| {
            assert_eq!(st.unresponded_retired.get(&k), Some(&1));
            assert_eq!(st.retired_peer_count.get(&k), Some(&1));
            assert_eq!(st.agent_stack.len(), 2);
        });
        // Its event arrives: absorbed rather than clearing the failing peer's error.
        tm.complete_response(&root_ctx, &root_id, "a", "root.a", &[]);
        state(&tm, "inv", |st| {
            assert_eq!(st.unresponded_retired.get(&k), Some(&0));
            assert_eq!(st.unrecovered_errors.len(), 1);
        });
        let (unclosed, term) = tm.pop_unclosed_agent_spans(&root_ctx, &root_id);
        assert_eq!(term.unwrap().to_string(), "second failed");
        assert_eq!(unclosed.len(), 1);
    }

    #[test]
    fn partial_stop_suppresses_open_work() {
        for streamed in [false, true] {
            let clock = ManualClock::new();
            let tm = manager(&clock);
            let bg = CallbackCtx::background();
            let root_id = ident("inv", "root", "", "root");
            let child_id = ident("inv", "child", "", "root/child");
            tm.push_agent_span(&bg, &root_id);
            tm.push_agent_span(&bg, &child_id);
            tm.push_llm_span(&bg, &child_id);
            if streamed {
                tm.record_streamed(&bg, &root_id, "child", "", false);
            }
            let (unclosed, term) = tm.pop_unclosed_agent_spans(&bg, &root_id);
            if streamed {
                assert!(term.is_none() && unclosed.is_empty());
            } else {
                assert_eq!(
                    term.unwrap().to_string(),
                    "agent \"child\" execution failed before completion"
                );
                let names: Vec<&str> = unclosed.iter().map(|u| u.agent_name.as_str()).collect();
                assert_eq!(names, ["child", "root"]);
            }
            assert_eq!(open_agents(&tm, "inv"), 0);
        }
    }

    #[test]
    fn handled_retry_purges_the_failed_attempt() {
        let clock = ManualClock::new();
        let tm = manager(&clock);
        let bg = CallbackCtx::background();
        let wf_id = ident("inv", "wf", "", "wf");
        let node_id = ident("inv", "n", "", "wf/n@1");
        let (wf_start, _) = tm.push_agent_span(&bg, &wf_id);
        let (first, _) = tm.push_agent_span(&bg, &node_id);
        tm.push_llm_span(&bg, &node_id);
        clock.advance_ms(20);
        tm.pop_llm_span(&bg, &node_id, false);
        tm.record_error(&bg, &node_id, &llm_key(&node_id), err("attempt 1 failed"));
        clock.advance_ms(100);
        let (second, purged) = tm.push_agent_span(&bg, &node_id);
        assert_eq!(second.parent_span_id, wf_start.span_id);
        assert_eq!(purged.len(), 1);
        let p = &purged[0];
        assert_eq!(p.span.span_id, first.span_id);
        assert_eq!(p.err.as_ref().unwrap().to_string(), "attempt 1 failed");
        // The purged attempt ends at its last activity, not at the retry.
        assert_eq!(p.span.latency.unwrap().total_ms, 20);
        assert_eq!(
            p.timestamp,
            Some(first.timestamp.unwrap() + chrono::Duration::milliseconds(20))
        );
        state(&tm, "inv", |st| {
            assert!(st.unrecovered_errors.is_empty());
            assert!(st.completed_models.is_empty());
            assert_eq!(st.agent_stack.len(), 3);
        });
    }

    #[test]
    fn late_settled_activations_retire_unless_failed_or_held() {
        let clock = ManualClock::new();
        let inv = InvocationTrace::new_with_span(None, "trace", clock.now());
        let mut guard = inv.mu.lock();
        let st = &mut *guard;
        st.is_late = true;
        let root = st.agent_stack[0].clone();
        let agent = |parent: &SpanRef, seq: u64, settled: bool| {
            let rec = st.new_child(parent, SpanKind::Agent, clock.now());
            {
                let mut r = rec.lock();
                r.span_id = format!("s{seq}");
                r.seq = seq;
                r.run_done = true;
                r.context_ended = settled;
                r.canceled = Some(CancelSource::of_token(CancellationToken::new(), None));
            }
            rec
        };
        let settled = agent(&root, 1, true);
        let failed = agent(&root, 2, false);
        let held = agent(&failed, 3, true);
        st.agent_stack
            .extend([settled.clone(), failed.clone(), held.clone()]);
        inv.retire_late_settled_locked(st);
        assert_eq!(st.agent_stack.len(), 3);
        assert!(st.agent_stack[1].ptr_eq(&failed) && st.agent_stack[2].ptr_eq(&held));
        assert!(settled.lock().canceled.is_none());
        assert!(held.lock().canceled.is_some());
        // Once the failed parent is gone (a purge), the held one retires.
        st.agent_stack.remove(1);
        inv.retire_late_settled_locked(st);
        assert_eq!(st.agent_stack.len(), 1);
        assert!(!inv.open_agents.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn launchers_hold_back_settling() {
        let clock = ManualClock::new();
        let inv = InvocationTrace::new_with_span(None, "trace", clock.now());
        let mut guard = inv.mu.lock();
        let st = &mut *guard;
        let root = st.agent_stack[0].clone();
        let custom_agent = st.new_child(&root, SpanKind::Agent, clock.now());
        {
            let mut c = custom_agent.lock();
            (c.seq, c.non_llm, c.branch) = (1, true, "c".into());
        }
        // Run on another branch than the open custom agent's: it may have launched it.
        let other = st.new_child(&root, SpanKind::Agent, clock.now());
        {
            let mut o = other.lock();
            (o.seq, o.branch) = (2, "x".into());
        }
        st.agent_stack.extend([custom_agent.clone(), other.clone()]);
        assert!(st.launcher_open_locked(&other));
        custom_agent.lock().run_done = true;
        assert!(!st.launcher_open_locked(&other));
        // An open tool call that started earlier may have run it too.
        let call = st.new_child(&root, SpanKind::Tool, clock.now());
        call.lock().seq = 1;
        st.tool_spans.insert("k".into(), call);
        assert!(st.launcher_open_locked(&other));
    }

    #[test]
    fn attribution_bodies_with_goroutines() {
        let clock = ManualClock::new();
        let tm = manager(&clock);
        let bg = CallbackCtx::background();
        let wf_id = ident("inv", "wf", "", "wf");
        let node_id = ident("inv", "n", "", "wf/n");
        tm.push_agent_span(&bg, &wf_id);
        tm.push_agent_span(&bg, &node_id);
        let key = NodeRunKey {
            path: "wf/n".into(),
            branch: String::new(),
        };
        let activation = adk_ctx(&custom("n", vec![]), "inv", None).activation_id();
        let (w, n) = state(&tm, "inv", |st| {
            let (w, n) = (st.agent_stack[1].clone(), st.agent_stack[2].clone());
            {
                let mut g = w.lock();
                g.is_workflow = true;
                g.propagates_child_error = true;
                g.goroutine_id = 1;
                g.node_runs.insert(key.clone(), 1);
            }
            {
                let mut g = n.lock();
                g.goroutine_id = 2;
                g.attempt = AttemptSite::Node;
                g.activation = Some(activation);
                g.origin = GoroutineOrigin {
                    creator: SCHEDULER_CREATOR,
                    parent: 1,
                };
            }
            // The scheduler waits for the node: over only once the workflow agent is.
            assert!(!st.can_settle_in_subtree_locked(1, &n, 0, NO_FRAME));
            assert!(st.reported_over_locked(1).contains(&n.lock().span_id));
            w.lock().run_done = true;
            assert!(st.can_settle_in_subtree_locked(1, &n, 0, NO_FRAME));
            assert!(!st.in_workflow_node_subtree_locked(&n));
            n.lock().run_done = true;
            assert!(st.in_workflow_node_subtree_locked(&n));
            (w, n)
        });
        // A failed node run reported by the scheduler marks its agent.
        tm.record_node_completion(&bg, &wf_id, "wf", "wf/n", "", true);
        assert!(n.lock().node_run_ended);
        assert!(w.lock().node_runs.is_empty());
        // With zero attribution, nothing was restarted.
        assert!(tm.end_restarted_node_runs(&bg, &wf_id).is_empty());
    }

    /// Runs `f` on its own thread and returns its result, failing the test when it does not
    /// return within 5 s (a walk that deadlocks on a record lock never returns).
    fn within_5s<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> R {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        match rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(r) => r,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                panic!("deadlock: the attribution walk did not return within 5 s")
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the attribution walk panicked")
            }
        }
    }

    /// R10: the attribution walks lock one record at a time, so a chain whose joins must be
    /// evaluated (a record that is both a `d` and a parent on another `d`'s walk, and a record
    /// that is its own parent) settles without deadlocking.
    #[test]
    fn attribution_walks_lock_one_record_at_a_time() {
        let clock = ManualClock::new();
        let tm = manager(&clock);
        let bg = CallbackCtx::background();
        for (agent, path) in [("wf", "wf"), ("n", "wf/n"), ("l", "wf/n/l")] {
            tm.push_agent_span(&bg, &ident("inv", agent, "", path));
        }
        let activation = adk_ctx(&custom("n", vec![]), "inv", None).activation_id();
        let inv = tm.invocation(&bg, &ident("inv", "", "", ""));
        let (w, n, l) = {
            let st = inv.mu.lock();
            let s = &st.agent_stack;
            let (w, n, l) = (s[1].clone(), s[2].clone(), s[3].clone());
            {
                let mut g = w.lock();
                g.parent = Some(s[0].clone());
                g.is_workflow = true;
                g.propagates_child_error = true;
                g.goroutine_id = 1;
                g.activation = Some(activation);
            }
            {
                let mut g = n.lock();
                g.parent = Some(w.clone());
                g.propagates_child_error = true;
                g.goroutine_id = 2;
                g.attempt = AttemptSite::Node;
                g.activation = Some(activation);
                g.origin = GoroutineOrigin {
                    creator: SCHEDULER_CREATOR,
                    parent: 1,
                };
            }
            {
                let mut g = l.lock();
                g.parent = Some(n.clone());
                g.goroutine_id = 2;
                g.activation = Some(activation);
            }
            (w, n, l)
        };
        let ids: Vec<String> = [&w, &n, &l]
            .iter()
            .map(|r| r.lock().span_id.clone())
            .collect();
        let (over, settles, ends) = within_5s({
            let (inv, l) = (inv.clone(), l.clone());
            move || {
                let st = inv.mu.lock();
                let call = SpanRecord {
                    kind: SpanKind::Tool,
                    parent: Some(l.clone()),
                    own_activation: true,
                    ..SpanRecord::default()
                };
                (
                    st.reported_over_locked(1),
                    st.can_settle_in_subtree_locked(1, &l, 0, NO_FRAME),
                    (call.ended_with_run("wf"), call.ended_with_run("other")),
                )
            }
        });
        // `n` joins the workflow agent (scheduler origin) and is over with it; `l` joins `n`
        // (same goroutine) and is over because `n` is.
        assert_eq!(over, ids.iter().cloned().collect::<HashSet<_>>());
        // The scheduler is still waiting on the node: not settled until the workflow agent ran.
        assert!(!settles);
        // The call's activation ran on the runner's agent's run: `l` → `n` (same goroutine) →
        // `wf` (joins once returned) → the invocation.
        assert_eq!(ends, (true, false));
        w.lock().run_done = true;
        let settles = within_5s({
            let (inv, l) = (inv.clone(), l.clone());
            move || {
                inv.mu
                    .lock()
                    .can_settle_in_subtree_locked(1, &l, 0, NO_FRAME)
            }
        });
        assert!(settles);

        // A record that is its own parent: the walk compares it with itself.
        let a = {
            let st = inv.mu.lock();
            let a = st.agent_stack[1].clone();
            a.lock().run_done = false;
            a.lock().parent = Some(a.clone());
            a
        };
        let settles = within_5s({
            let (inv, a) = (inv.clone(), a.clone());
            move || {
                inv.mu
                    .lock()
                    .can_settle_in_subtree_locked(1, &a, 0, NO_FRAME)
            }
        });
        // It joins itself, has not run, and is the subtree's root.
        assert!(!settles);
        a.lock().parent = None;
    }

    /// R9: Go `currentGoroutineID()` is the fiber the caller runs on, and `currentGoroutine()`
    /// (read only for a context that shows an activation) its id, origin and the attempt site
    /// the engine claimed for the agent run.
    #[tokio::test]
    async fn current_goroutine_reads_the_callback_fiber() {
        assert_eq!(
            current_goroutine_id(),
            0,
            "a #[tokio::test] body has no fiber"
        );
        let adk = adk_ctx(&custom("a", vec![]), "inv", None);
        let root = fiber::Fiber::root();
        let root_id = root.id;
        root.scope(async move {
            assert_eq!(current_goroutine_id(), root_id);
            assert_eq!(
                current_goroutine(&CallbackCtx::background()),
                (0, GoroutineOrigin::default(), AttemptSite::None)
            );
            assert_eq!(
                current_goroutine(&CallbackCtx::from_context(&adk)),
                (root_id, GoroutineOrigin::default(), AttemptSite::None)
            );
            let child = fiber::spawn_child(SCHEDULER_CREATOR);
            let child_id = child.id;
            child
                .scope(async move {
                    let (id, origin, attempt) = current_goroutine(&CallbackCtx::from_context(&adk));
                    assert_eq!(id, child_id);
                    assert!(origin.started_by(SCHEDULER_CREATOR, root_id));
                    assert_eq!(attempt, AttemptSite::None);
                })
                .await;
        })
        .await;
    }

    /// R9: the attempt site the engine marks for a node attempt reaches the before-agent
    /// callbacks of the run that claims it, and only those.
    #[tokio::test]
    async fn current_goroutine_sees_the_claimed_attempt() {
        use futures::{FutureExt, StreamExt};
        use gadk_agent::callback::BeforeAgentCallback;

        type Seen = Arc<parking_lot::Mutex<Vec<(u64, AttemptSite)>>>;
        let seen = Seen::default();
        let record: BeforeAgentCallback = {
            let seen = seen.clone();
            Arc::new(move |ctx| {
                let (id, _, attempt) = current_goroutine(&CallbackCtx::from_context(&ctx));
                seen.lock().push((id, attempt));
                async { Ok(None) }.boxed()
            })
        };
        let agent = gadk_agent::new_agent(gadk_agent::AgentConfig {
            name: "node_agent".into(),
            before_agent_callbacks: vec![record],
            ..Default::default()
        })
        .unwrap();
        let ctx = adk_ctx(&agent, "inv", None);
        let f = fiber::Fiber::root();
        let id = f.id;
        f.scope(fiber::with_attempt(fiber::AttemptSite::Worker, async {
            let _: Vec<_> = agent.run(ctx.as_invocation().clone()).collect().await;
            let _: Vec<_> = agent.run(ctx.as_invocation().clone()).collect().await;
        }))
        .await;
        assert_eq!(
            *seen.lock(),
            vec![(id, AttemptSite::Worker), (id, AttemptSite::None)]
        );
    }
}
