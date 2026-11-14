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
//! `endRestartedNodeRuns` and `recordNodeCompletion` (attribution: no-ops with zero goroutine
//! information), `inWorkflowNodeSubtreeLocked`, `reconcileConcurrentResponseOnPopLocked`,
//! `popTaskAgentSpan`, `finishedTaskLocked`, `noteTaskFinished`, `retireAgentLocked`,
//! `failuresLocked`, `reportedOverLocked`, `purgeActivationLocked`, `endToolRunsLocked`,
//! `markToolRunsDoneLocked` (attribution), `completeResponse`,
//! `retireDrainedDescendantsLocked`, `selectResponseOwnerLocked`, `recordError`, `clearError`,
//! `popUnclosedAgentSpans`, `responded`, `settleEndedLocked`, `settleEndedInSubtreeLocked`,
//! `canSettleInSubtreeLocked` (attribution: activations count as over),
//! `settleFinishedLocked`, `retireLateSettledLocked`, `launcherOpenLocked`,
//! `hasCanceledByStopUnderLocked`, and `popToolSpan`.

#![allow(dead_code, unused_variables)] // Task 8 implements the bodies.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};

use super::invocation::{InvocationState, InvocationTrace};
use super::manager::TraceManager;
use super::span::{RowSpan, SpanRecord, SpanRef, UnclosedAgentSpan};
use super::{CallbackCtx, Identity, TraceError};

impl TraceManager {
    /// Go `popDelegatingAgentSpan(ctx, id)`: `Some(place of AGENT_COMPLETED)` when the new
    /// activation retires an earlier one that delegated to a completed task.
    pub(crate) fn pop_delegating_agent_span(
        &self,
        ctx: &CallbackCtx,
        id: &Identity,
    ) -> Option<RowSpan> {
        todo!("Task 8")
    }

    /// Go `popAgentSpan(ctx, id)`: the place of `AGENT_COMPLETED`.
    pub(crate) fn pop_agent_span(&self, ctx: &CallbackCtx, id: &Identity) -> RowSpan {
        todo!("Task 8")
    }

    /// Go `endRestartedNodeRuns(ctx, id)` (attribution: nothing is restarted with zero
    /// goroutine information, so it returns no spans).
    pub(crate) fn end_restarted_node_runs(
        &self,
        ctx: &CallbackCtx,
        id: &Identity,
    ) -> Vec<UnclosedAgentSpan> {
        todo!("Task 8")
    }

    /// Go `recordNodeCompletion(ctx, id, workflowName, path, branch, failed)` (attribution: no
    /// activation is a node agent with zero goroutine information).
    pub(crate) fn record_node_completion(
        &self,
        ctx: &CallbackCtx,
        id: &Identity,
        workflow_name: &str,
        path: &str,
        branch: &str,
        failed: bool,
    ) {
        todo!("Task 8")
    }

    /// Go `popTaskAgentSpan(ctx, id, calls)`: `Some(place of AGENT_COMPLETED)` for a task-mode
    /// activation that a `finish_task` response ends.
    pub(crate) fn pop_task_agent_span(
        &self,
        ctx: &CallbackCtx,
        id: &Identity,
        calls: &[String],
    ) -> Option<RowSpan> {
        todo!("Task 8")
    }

    /// Go `noteTaskFinished(ctx, id, callID)`.
    pub(crate) fn note_task_finished(&self, ctx: &CallbackCtx, id: &Identity, call_id: &str) {
        todo!("Task 8")
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
        todo!("Task 8")
    }

    /// Go `recordError(ctx, id, key, err)`.
    pub(crate) fn record_error(
        &self,
        ctx: &CallbackCtx,
        id: &Identity,
        key: &str,
        err: TraceError,
    ) {
        todo!("Task 8")
    }

    /// Go `clearError(ctx, id, key)`.
    pub(crate) fn clear_error(&self, ctx: &CallbackCtx, id: &Identity, key: &str) {
        todo!("Task 8")
    }

    /// Go `popUnclosedAgentSpans(ctx, id)`: the `AGENT_ERROR` activations of the run's end and
    /// the invocation's terminal error.
    pub(crate) fn pop_unclosed_agent_spans(
        &self,
        ctx: &CallbackCtx,
        id: &Identity,
    ) -> (Vec<UnclosedAgentSpan>, Option<TraceError>) {
        todo!("Task 8")
    }

    /// Go `popToolSpan(ctx, id, key, fromError)`: `(place, purged, closed_by_error)`.
    pub(crate) fn pop_tool_span(
        &self,
        ctx: &CallbackCtx,
        id: &Identity,
        key: &str,
        from_error: bool,
    ) -> (RowSpan, Vec<UnclosedAgentSpan>, bool) {
        todo!("Task 8")
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
        todo!("Task 8")
    }

    /// Go `finishedTaskLocked(id, calls)`: the index of the activation the response ends.
    pub(crate) fn finished_task_locked(
        &self,
        st: &mut InvocationState,
        id: &Identity,
        calls: &[String],
    ) -> Option<usize> {
        todo!("Task 8")
    }

    /// Go `retireAgentLocked(i, done, since)`.
    pub(crate) fn retire_agent_locked(
        &self,
        st: &mut InvocationState,
        i: usize,
        done: bool,
        since: u64,
    ) {
        todo!("Task 8")
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
        todo!("Task 8")
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
        todo!("Task 8")
    }

    /// Go `markToolRunsDoneLocked(call)` (attribution: no activation has a goroutine).
    pub(crate) fn mark_tool_runs_done_locked(&self, st: &mut InvocationState, call: &SpanRef) {
        todo!("Task 8")
    }

    /// Go `retireDrainedDescendantsLocked(matchAnc)`.
    pub(crate) fn retire_drained_descendants_locked(
        &self,
        st: &mut InvocationState,
        match_anc: &dyn Fn(&SpanRecord, &HashSet<String>) -> bool,
    ) {
        todo!("Task 8")
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
        todo!("Task 8")
    }

    /// Go `settleEndedInSubtreeLocked(i, done, since)`.
    pub(crate) fn settle_ended_in_subtree_locked(
        &self,
        st: &mut InvocationState,
        i: usize,
        done: bool,
        since: u64,
    ) {
        todo!("Task 8")
    }

    /// Go `settleFinishedLocked`.
    pub(crate) fn settle_finished_locked(&self, st: &mut InvocationState) {
        todo!("Task 8")
    }

    /// Go `retireLateSettledLocked`. The early return for live traces (Go's first statement)
    /// is written by Task 4 so that Task 5's `pop_llm_span` works before Task 8.
    pub(crate) fn retire_late_settled_locked(&self, st: &mut InvocationState) {
        if !st.is_late || st.agent_stack.len() <= 1 {
            return;
        }
        todo!("Task 8")
    }
}

impl InvocationState {
    /// Go `inWorkflowNodeSubtreeLocked(rec)` (attribution).
    pub(crate) fn in_workflow_node_subtree_locked(&self, rec: &SpanRef) -> bool {
        todo!("Task 8")
    }

    /// Go `failuresLocked(i, stopped, over, openCalls)`: span ID → failure.
    pub(crate) fn failures_locked(
        &self,
        i: usize,
        stopped: bool,
        over: Option<&HashSet<String>>,
        open_calls: bool,
    ) -> HashMap<String, TraceError> {
        todo!("Task 8")
    }

    /// Go `reportedOverLocked(i)`.
    pub(crate) fn reported_over_locked(&self, i: usize) -> HashSet<String> {
        todo!("Task 8")
    }

    /// Go `canSettleInSubtreeLocked(rootIdx, d, curGoid, since)` (attribution: with no
    /// goroutine information every activation counts as over, Go `:3923`).
    pub(crate) fn can_settle_in_subtree_locked(
        &self,
        root_idx: usize,
        d: &SpanRef,
        cur_goid: u64,
        since: u64,
    ) -> bool {
        todo!("Task 8")
    }

    /// Go `launcherOpenLocked(rec)`.
    pub(crate) fn launcher_open_locked(&self, rec: &SpanRef) -> bool {
        todo!("Task 8")
    }

    /// Go `hasCanceledByStopUnderLocked(i, canceledByStop)`.
    pub(crate) fn has_canceled_by_stop_under_locked(
        &self,
        i: usize,
        canceled_by_stop: &dyn Fn(&SpanRef) -> bool,
    ) -> bool {
        todo!("Task 8")
    }
}

impl SpanRecord {
    /// Go `responded`.
    pub(crate) fn responded(&self) -> bool {
        todo!("Task 8")
    }

    /// Go `settleEndedLocked`: reads `ended` once the run is known to be over.
    pub(crate) fn settle_ended_locked(&mut self) {
        todo!("Task 8")
    }
}
