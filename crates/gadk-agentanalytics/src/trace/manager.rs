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

//! The trace manager, from ADK-Go `plugin/agentanalytics/trace_manager.go`: `traceManager`,
//! `evictedTrace`, `completedTrace`, `newTraceManager`, `traceIDFor`, `rememberAgentTree`,
//! `invocation` / `lookupInvocation`, eviction (`evictLocked`, `markEvictedLocked`), completed
//! runs and their late state (`rememberCompletedLocked`, `lateLocked`, `withCalls`,
//! `pruneCompletedLocked`, `pruneWaitingLocked`, `startedCalls`), and the span starts and the
//! model-call / event places: `invocationSpan`, `endInvocation`, `agentNamed`, `pushAgentSpan`,
//! `pushLLMSpan`, `recordFirstToken`, `recordStreamed`, `clearLastPartial`, `popLLMSpan`,
//! `pushToolSpan`, `eventSpan`. The agent ends, responses, errors and the end of a run are
//! [`super::settle`].
//!
//! Go's `container/list` completion order is a `VecDeque` of `(invocation ID, generation)`
//! entries: moving an entry to the back pushes a new generation (the entry's
//! `CompletedTrace::generation`), and entries whose generation is stale, or whose ID is no
//! longer completed, are skipped and dropped — the same order as Go's list (not observable, so
//! not ledgered; spec R9).

#![allow(dead_code, unused_variables)] // Task 5 implements the bodies.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::AtomicI64;
use std::time::Duration;

use chrono::{DateTime, Utc};
use gadk_agent::Agent;
use gadk_engine::telemetry::d6::SpanIds;
use parking_lot::Mutex;

use super::invocation::InvocationTrace;
use super::span::{RowSpan, SpanRef, UnclosedAgentSpan};
use super::{AgentKey, CallbackCtx, Clock, Identity, LogFn};

/// Go `evictedTrace`: the identity of an invocation after active tracking ends.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct EvictedTrace {
    pub(crate) trace_id: String,
    pub(crate) span_id: String,
    pub(crate) parent_span_id: String,
    pub(crate) root_agent_name: String,
    /// Go `lastActive` (Unix nanoseconds).
    pub(crate) last_active: i64,
}

/// Go `completedTrace`: what the callbacks that run after a run ended need of it.
#[derive(Default)]
pub(crate) struct CompletedTrace {
    pub(crate) identity: EvictedTrace,
    /// Go `livePending`: the finished run's `pending_spans` while a reservation is held.
    pub(crate) live_pending: Option<Arc<AtomicI64>>,
    /// Go `late`: the late state, made for a run that left open calls behind or for the first
    /// callback after `AfterRun`.
    pub(crate) late: Option<Arc<InvocationTrace>>,
    /// Go `endedModels`, `endedTools` (nil = `None`).
    pub(crate) ended_models: Option<HashMap<String, SpanRef>>,
    pub(crate) ended_tools: Option<HashMap<String, SpanRef>>,
    /// Go `position *list.Element`: the generation of this entry's live completion-order
    /// entry.
    pub(crate) generation: u64,
}

impl CompletedTrace {
    /// Go `hasPendingOrOpenLateSpans`.
    pub(crate) fn has_pending_or_open_late_spans(&self) -> bool {
        todo!("Task 5")
    }

    /// Go `hasPendingSpans`.
    pub(crate) fn has_pending_spans(&self) -> bool {
        todo!("Task 5")
    }

    /// Go `waitsForLateCalls`.
    pub(crate) fn waits_for_late_calls(&self) -> bool {
        todo!("Task 5")
    }
}

/// An entry of Go's `knownParent`: the child (keeping its address, the key, alive) and its
/// parent.
#[derive(Clone)]
pub(crate) struct KnownParent {
    pub(crate) child: Arc<dyn Agent>,
    pub(crate) parent: Arc<dyn Agent>,
}

/// The fields of Go's `traceManager` that `tm.mu` guards.
#[derive(Default)]
pub(crate) struct ManagerState {
    pub(crate) invocations: HashMap<String, Arc<InvocationTrace>>,
    pub(crate) evicted: HashMap<String, EvictedTrace>,
    pub(crate) completed: HashMap<String, CompletedTrace>,
    /// Go `completionOrder list.List`, front = least recently active.
    pub(crate) completion_order: VecDeque<(String, u64)>,
    /// The next completion-order generation.
    pub(crate) next_generation: u64,
    /// Go `knownParent map[agent.Agent]agent.Agent`, keyed by the child's address.
    pub(crate) known_parent: HashMap<AgentKey, KnownParent>,
    pub(crate) inserts: usize,
}

/// Go `traceManager`. Go's test-settable fields (`maxTracked`, `ttl`, `now`, `logf`) are
/// plain fields set before the manager is shared.
pub(crate) struct TraceManager {
    pub(crate) mu: Mutex<ManagerState>,
    /// Go `traceSeed`: a random trace ID seeding `trace_id_for`.
    pub(crate) trace_seed: String,
    pub(crate) logf: Option<LogFn>,
    pub(crate) max_tracked: usize,
    pub(crate) ttl: Duration,
    /// Go `now` (`time.Now`): call as `(tm.now)()`.
    pub(crate) now: Clock,
}

impl TraceManager {
    /// Go `newTraceManager()`: `MAX_TRACKED_INVOCATIONS`, `INVOCATION_TTL`, `Utc::now`, a new
    /// trace seed, no `logf`.
    pub(crate) fn new() -> TraceManager {
        todo!("Task 5")
    }

    /// Go `traceIDFor(invocationID)`: the first 16 bytes of SHA-256(`seed \x00 id`) as hex, or
    /// a new trace ID for an empty ID.
    pub(crate) fn trace_id_for(&self, invocation_id: &str) -> String {
        todo!("Task 5")
    }

    /// Go `rememberAgentTree(runner)`: records `runner`'s sub-agent links and returns the
    /// highest known ancestor of `runner` (`None` for no runner).
    pub(crate) fn remember_agent_tree(
        &self,
        runner: Option<&Arc<dyn Agent>>,
    ) -> Option<Arc<dyn Agent>> {
        todo!("Task 5")
    }

    /// Go `invocation(ctx, id)` = `lookup_invocation(ctx, id, false, false)`.
    pub(crate) fn invocation(&self, ctx: &CallbackCtx, id: &Identity) -> Arc<InvocationTrace> {
        todo!("Task 5")
    }

    /// Go `lookupInvocation(ctx, id, start, reserveSpan)`.
    pub(crate) fn lookup_invocation(
        &self,
        ctx: &CallbackCtx,
        id: &Identity,
        start: bool,
        reserve_span: bool,
    ) -> Arc<InvocationTrace> {
        todo!("Task 5")
    }

    /// Go `evictLocked(now, keep)`: the IDs of active invocations evicted for capacity.
    pub(crate) fn evict_locked(
        &self,
        st: &mut ManagerState,
        now: DateTime<Utc>,
        keep: &str,
    ) -> Vec<String> {
        todo!("Task 5")
    }

    /// Go `markEvictedLocked(now, invocationID, inv)`.
    pub(crate) fn mark_evicted_locked(
        &self,
        st: &mut ManagerState,
        now: DateTime<Utc>,
        invocation_id: &str,
        inv: Option<&InvocationTrace>,
    ) {
        todo!("Task 5")
    }

    /// Go `rememberCompletedLocked(invocationID, runner, inv)`.
    pub(crate) fn remember_completed_locked(
        &self,
        st: &mut ManagerState,
        invocation_id: &str,
        runner: &str,
        inv: &Arc<InvocationTrace>,
    ) {
        todo!("Task 5")
    }

    /// Go `lateLocked(completed, sc, traceID, now)`.
    pub(crate) fn late_locked(
        &self,
        completed: &mut CompletedTrace,
        sc: Option<SpanIds>,
        trace_id: &str,
        now: DateTime<Utc>,
    ) -> Arc<InvocationTrace> {
        todo!("Task 5")
    }

    /// Go `pruneCompletedLocked(limit, drop)`.
    pub(crate) fn prune_completed_locked(
        &self,
        st: &mut ManagerState,
        limit: usize,
        drop: &dyn Fn(&CompletedTrace) -> bool,
    ) {
        todo!("Task 5")
    }

    /// Go `pruneWaitingLocked(limit, keep)`.
    pub(crate) fn prune_waiting_locked(&self, st: &mut ManagerState, limit: usize, keep: usize) {
        todo!("Task 5")
    }

    /// Go `invocationSpan(ctx, id, runner)`: the place of `USER_MESSAGE_RECEIVED` /
    /// `INVOCATION_STARTING` (starts tracking).
    pub(crate) fn invocation_span(
        &self,
        ctx: &CallbackCtx,
        id: &Identity,
        runner: Option<&Arc<dyn Agent>>,
    ) -> RowSpan {
        todo!("Task 5")
    }

    /// Go `endInvocation(ctx, id)`: the place of `INVOCATION_COMPLETED` / `_ERROR`; moves the
    /// run to its completed entry.
    pub(crate) fn end_invocation(&self, ctx: &CallbackCtx, id: &Identity) -> RowSpan {
        todo!("Task 5")
    }

    /// Go `agentNamed(ctx, id)`: the agent named `id.agent_name` in the runner's tree.
    pub(crate) fn agent_named(&self, ctx: &CallbackCtx, id: &Identity) -> Option<Arc<dyn Agent>> {
        todo!("Task 5")
    }

    /// Go `pushAgentSpan(ctx, id)`: the place of `AGENT_STARTING` and the activations purged
    /// as failed earlier attempts (only Go's synthetic-retry fallback purges in 7b-3a; it calls
    /// `settle_ended_in_subtree_locked`, `failures_locked`, `reported_over_locked` and
    /// `purge_activation_locked` of [`super::settle`]).
    pub(crate) fn push_agent_span(
        &self,
        ctx: &CallbackCtx,
        id: &Identity,
    ) -> (RowSpan, Vec<UnclosedAgentSpan>) {
        todo!("Task 5")
    }

    /// Go `pushLLMSpan(ctx, id)`: the place of `LLM_REQUEST`.
    pub(crate) fn push_llm_span(&self, ctx: &CallbackCtx, id: &Identity) -> RowSpan {
        todo!("Task 5")
    }

    /// Go `recordFirstToken(ctx, id)`.
    pub(crate) fn record_first_token(&self, ctx: &CallbackCtx, id: &Identity) {
        todo!("Task 5")
    }

    /// Go `recordStreamed(ctx, id, author, branch, withContent)`.
    pub(crate) fn record_streamed(
        &self,
        ctx: &CallbackCtx,
        id: &Identity,
        author: &str,
        branch: &str,
        with_content: bool,
    ) {
        todo!("Task 5")
    }

    /// Go `clearLastPartial(ctx, id)`.
    pub(crate) fn clear_last_partial(&self, ctx: &CallbackCtx, id: &Identity) {
        todo!("Task 5")
    }

    /// Go `popLLMSpan(ctx, id, responded)`: the place of `LLM_RESPONSE` / `LLM_ERROR` (calls
    /// `retire_late_settled_locked` of [`super::settle`], whose live-trace early return Task 4
    /// wrote).
    pub(crate) fn pop_llm_span(
        &self,
        ctx: &CallbackCtx,
        id: &Identity,
        responded: bool,
    ) -> RowSpan {
        todo!("Task 5")
    }

    /// Go `pushToolSpan(ctx, id, key)`: the place of `TOOL_STARTING`.
    pub(crate) fn push_tool_span(&self, ctx: &CallbackCtx, id: &Identity, key: &str) -> RowSpan {
        todo!("Task 5")
    }

    /// Go `eventSpan(ctx, id, author, branch)`: the place of a session-event row.
    pub(crate) fn event_span(
        &self,
        ctx: &CallbackCtx,
        id: &Identity,
        author: &str,
        branch: &str,
    ) -> RowSpan {
        todo!("Task 5")
    }
}

/// Go `withCalls(calls, more)`: `calls` with `more` added; `None` if both are empty.
pub(crate) fn with_calls(
    calls: Option<HashMap<String, SpanRef>>,
    more: Option<HashMap<String, SpanRef>>,
) -> Option<HashMap<String, SpanRef>> {
    todo!("Task 5")
}

/// Go `startedCalls(calls)`: the calls `push_llm_span` / `push_tool_span` started, or `None`.
pub(crate) fn started_calls(calls: &HashMap<String, SpanRef>) -> Option<HashMap<String, SpanRef>> {
    todo!("Task 5")
}
