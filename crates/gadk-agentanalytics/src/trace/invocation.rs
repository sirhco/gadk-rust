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

//! One invocation's spans, from ADK-Go `plugin/agentanalytics/trace_manager.go`:
//! `invocationTrace` (its atomics and, behind `mu`, [`InvocationState`]), `recordedError`,
//! `newInvocationTraceWithSpan`, the open-call / open-agent syncs, `rowSpan` / `rowTraceID` /
//! `currentTraceID`, the agent tree (`agentEdge`, `rootAgentInput`, `walkAgentEdges`,
//! `prepareRootAgentInput`, `applyEdgesLocked`, `noteRootAgentLocked`, `isLLMAgent`,
//! `isBuiltInWorkflowAgent`, `agentTypeOf`, `agentChildren`, `topAgentFromMap`) and the tree
//! queries (`isAncestorAgent`, `findAgent`, `findAgentForContext`, `parentFor`,
//! `parentOutsideLocked`, `parentForRunLocked`, `ownerOf`, `withinBranch`, `withinPath`,
//! `isSubNodePath`, `subtreeLocked`, `lineageLocked`, `activationOfLocked`,
//! `openCallOwnersLocked`, `agentWithOpenCallLocked`, `newChild`, `lockLive`,
//! `touchAgentActivityLocked`, `nextSeqLocked`, `sortedErrorKeysLocked`,
//! `clearLastPartialLocked`, `clearLastPartialUnderLocked`, `markRespondedLocked`,
//! `dropRetiredLateContextsLocked`, `savedIdentity`, `restoreEvicted`).
//!
//! Go's `invocationTrace` methods named `…Locked` take the locked [`InvocationState`]: those that
//! change it or the atomics are `InvocationTrace` methods taking `st: &mut InvocationState`;
//! read-only queries (which may still update span records) are `InvocationState` methods. Go's
//! `int` indexes into `agentStack` are `usize`; Go's `-1` is `None`.

#![allow(dead_code, unused_variables)] // Task 5 implements the bodies.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicI64};
use std::sync::{Arc, OnceLock};

use chrono::{DateTime, Utc};
use gadk_agent::{ActivationId, Agent, ParentMap};
use gadk_engine::telemetry::d6::SpanIds;
use parking_lot::{Mutex, MutexGuard, RwLock};

use super::manager::EvictedTrace;
use super::span::{Latency, RowSpan, SpanKind, SpanRecord, SpanRef};
use super::{CallbackCtx, Identity, TraceError};

/// Go `recordedError`: an unrecovered error of an invocation and the span ID of the agent
/// activation that was active when it was recorded.
#[derive(Debug, Clone)]
pub(crate) struct RecordedError {
    pub(crate) err: TraceError,
    pub(crate) cause: Option<TraceError>,
    pub(crate) span_id: String,
    pub(crate) bare_key: String,
    pub(crate) seq: u64,
    pub(crate) canceled: bool,
    pub(crate) collateral: bool,
}

/// Go `agentEdge`: a child-to-parent link in an agent sub-agent tree.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct AgentEdge {
    pub(crate) child: String,
    pub(crate) parent: String,
    pub(crate) non_llm: bool,
}

/// Go `rootAgentInput`: the agent-tree metadata read before `mu` is taken (agent code never
/// runs under a lock).
#[derive(Clone, Default)]
pub(crate) struct RootAgentInput {
    pub(crate) has_context_parents: bool,
    pub(crate) context_edges: Vec<AgentEdge>,
    pub(crate) context_root: Option<Arc<dyn Agent>>,
    pub(crate) context_root_name: String,
    pub(crate) runner: Option<Arc<dyn Agent>>,
    pub(crate) runner_name: String,
    pub(crate) runner_edges: Vec<AgentEdge>,
    pub(crate) remembered: Option<Arc<dyn Agent>>,
    pub(crate) remembered_name: String,
    pub(crate) remembered_edges: Vec<AgentEdge>,
}

/// Go `agentChildren`: the sub-agents configured on a running agent and its kind.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct AgentChildren {
    pub(crate) parent: String,
    pub(crate) non_llm: bool,
    pub(crate) propagates_child_error: bool,
    pub(crate) is_parallel: bool,
    pub(crate) is_workflow: bool,
    pub(crate) task_mode: bool,
    pub(crate) names: Vec<String>,
}

/// Go `invocationTrace`: the spans of one invocation. The atomics are read without `mu`
/// (eviction, pruning); everything else is [`InvocationState`] behind `mu`.
pub(crate) struct InvocationTrace {
    /// Go `lastActive` (Unix nanoseconds).
    pub(crate) last_active: AtomicI64,
    /// Go `openCalls`.
    pub(crate) open_calls: AtomicI64,
    /// Go `openHandedOver`.
    pub(crate) open_handed_over: AtomicI64,
    /// Go `openAgents`.
    pub(crate) open_agents: AtomicBool,
    /// Go `pendingSpans` (its own allocation, shared with `CompletedTrace::live_pending`).
    pub(crate) pending_spans: Arc<AtomicI64>,
    /// Go `rootAgentAtomic` (`atomic.Value` of a string).
    pub(crate) root_agent_atomic: RwLock<String>,
    /// Go `ancestryFromContextAtomic`.
    pub(crate) ancestry_from_context_atomic: AtomicBool,
    /// Go `adoptedTraceID` (`CompareAndSwap(nil, ambient)` = set once).
    pub(crate) adopted_trace_id: OnceLock<String>,
    /// Go `traceID`, `rootSpanID`, `rootParentSpanID`: read-only once published.
    pub(crate) trace_id: String,
    pub(crate) root_span_id: String,
    pub(crate) root_parent_span_id: String,
    /// Go `mu` and the fields it guards.
    pub(crate) mu: Mutex<InvocationState>,
}

/// The fields of Go's `invocationTrace` that `mu` guards (field docs are Go's,
/// `trace_manager.go:443-548`).
#[derive(Default)]
pub(crate) struct InvocationState {
    pub(crate) root_agent_name: String,
    pub(crate) configured_root: String,
    pub(crate) started: bool,
    pub(crate) evicted: bool,
    pub(crate) finished: bool,
    pub(crate) is_late: bool,
    pub(crate) runner_agent: Option<Arc<dyn Agent>>,
    pub(crate) runner_agent_name: String,
    pub(crate) parent_agent: HashMap<String, String>,
    pub(crate) sequential_parent: HashMap<String, String>,
    pub(crate) ancestry_from_context: bool,
    /// Go `agentStack`: the invocation span, then the running agents in start order.
    pub(crate) agent_stack: Vec<SpanRef>,
    pub(crate) llm_spans: HashMap<String, SpanRef>,
    pub(crate) completed_models: HashMap<String, SpanRef>,
    pub(crate) tool_spans: HashMap<String, SpanRef>,
    pub(crate) last_seq: u64,
    pub(crate) completed_tools: HashMap<String, SpanRef>,
    pub(crate) unrecovered_errors: HashMap<String, RecordedError>,
    pub(crate) unresponded_retired: HashMap<String, i64>,
    pub(crate) retired_peer_count: HashMap<String, i64>,
    pub(crate) last_session_event_partial: bool,
    pub(crate) last_event_partial: bool,
    pub(crate) last_partial_span_id: String,
    /// Go `partialLineage map[string]bool` (only `true` entries).
    pub(crate) partial_lineage: HashSet<String>,
}

impl InvocationTrace {
    /// Go `newInvocationTraceWithSpan(sc, traceID, now)`: a trace whose `agent_stack` holds its
    /// invocation span; `trace_id` falls back to a new ID; a valid `sc` gives the trace ID and
    /// the invocation span's parent.
    pub(crate) fn new_with_span(
        sc: Option<SpanIds>,
        trace_id: &str,
        now: DateTime<Utc>,
    ) -> InvocationTrace {
        todo!("Task 5")
    }

    /// Go `hasOpenCalls`.
    pub(crate) fn has_open_calls(&self) -> bool {
        todo!("Task 5")
    }

    /// Go `hasOpenLateSpans`.
    pub(crate) fn has_open_late_spans(&self) -> bool {
        todo!("Task 5")
    }

    /// Go `lockLive`: the locked state, or `None` (unlocked) once the run finished.
    pub(crate) fn lock_live(&self) -> Option<MutexGuard<'_, InvocationState>> {
        todo!("Task 5")
    }

    /// Go `currentTraceID`.
    pub(crate) fn current_trace_id(&self) -> String {
        todo!("Task 5")
    }

    /// Go `rowTraceID(ambient)`.
    pub(crate) fn row_trace_id(&self, ambient: &str) -> String {
        todo!("Task 5")
    }

    /// Go `rowSpan(rec, latency, ambient)` (`timestamp` left `None`).
    pub(crate) fn row_span(
        &self,
        st: &InvocationState,
        rec: &SpanRecord,
        latency: Option<Latency>,
        ambient: &str,
    ) -> RowSpan {
        todo!("Task 5")
    }

    /// Go `savedIdentity`.
    pub(crate) fn saved_identity(&self) -> EvictedTrace {
        todo!("Task 5")
    }

    /// Go `restoreEvicted(ev)`.
    pub(crate) fn restore_evicted(&self, st: &mut InvocationState, ev: &EvictedTrace) {
        todo!("Task 5")
    }

    /// Go `syncOpenCallsLocked`.
    pub(crate) fn sync_open_calls_locked(&self, st: &InvocationState) {
        todo!("Task 5")
    }

    /// Go `syncOpenAgentsLocked`.
    pub(crate) fn sync_open_agents_locked(&self, st: &InvocationState) {
        todo!("Task 5")
    }

    /// Go `setRootAgentLocked(name)`.
    pub(crate) fn set_root_agent_locked(&self, st: &mut InvocationState, name: &str) {
        todo!("Task 5")
    }

    /// Go `applyEdgesLocked(edges)`.
    pub(crate) fn apply_edges_locked(&self, st: &mut InvocationState, edges: &[AgentEdge]) {
        todo!("Task 5")
    }

    /// Go `noteRootAgentLocked(id, in, useBranchFallback)`.
    pub(crate) fn note_root_agent_locked(
        &self,
        st: &mut InvocationState,
        id: &Identity,
        input: &RootAgentInput,
        use_branch_fallback: bool,
    ) {
        todo!("Task 5")
    }
}

impl InvocationState {
    /// Go `isAncestorAgent(ancestor, child)`.
    pub(crate) fn is_ancestor_agent(&self, ancestor: &str, child: &str) -> bool {
        todo!("Task 5")
    }

    /// Go `findAgent(path, name, branch)`: the index of the open span (`> 0`), or `None`.
    pub(crate) fn find_agent(&self, path: &str, name: &str, branch: &str) -> Option<usize> {
        todo!("Task 5")
    }

    /// Go `findAgentForContext(ctx, path, name, branch)` with `activation` =
    /// `ctx.activation_key()`.
    pub(crate) fn find_agent_for_context(
        &self,
        activation: Option<ActivationId>,
        path: &str,
        name: &str,
        branch: &str,
    ) -> Option<usize> {
        todo!("Task 5")
    }

    /// Go `parentFor(id)`: the span `id`'s activation runs under (the invocation span at
    /// worst).
    pub(crate) fn parent_for(&self, id: &Identity) -> SpanRef {
        todo!("Task 5")
    }

    /// Go `parentOutsideLocked(id, exclude)`.
    pub(crate) fn parent_outside_locked(
        &self,
        id: &Identity,
        exclude: &HashSet<String>,
    ) -> SpanRef {
        todo!("Task 5")
    }

    /// Go `parentForRunLocked(id, goid, origin)` (attribution: with `goid == 0` it is
    /// `parent_for`).
    pub(crate) fn parent_for_run_locked(
        &self,
        id: &Identity,
        goid: u64,
        origin: &super::span::GoroutineOrigin,
    ) -> SpanRef {
        todo!("Task 5")
    }

    /// Go `ownerOf(ctx, id)`.
    pub(crate) fn owner_of(&self, activation: Option<ActivationId>, id: &Identity) -> SpanRef {
        todo!("Task 5")
    }

    /// Go `newChild(parent, kind, now)`: a new, untracked span under `parent`.
    pub(crate) fn new_child(
        &self,
        parent: &SpanRef,
        kind: SpanKind,
        now: DateTime<Utc>,
    ) -> SpanRef {
        todo!("Task 5")
    }

    /// Go `touchAgentActivityLocked(rec, now)`.
    pub(crate) fn touch_agent_activity_locked(&self, rec: &SpanRef, now: DateTime<Utc>) {
        todo!("Task 5")
    }

    /// Go `nextSeqLocked`.
    pub(crate) fn next_seq_locked(&mut self) -> u64 {
        todo!("Task 5")
    }

    /// Go `sortedErrorKeysLocked`.
    pub(crate) fn sorted_error_keys_locked(&self) -> Vec<String> {
        todo!("Task 5")
    }

    /// Go `clearLastPartialLocked`.
    pub(crate) fn clear_last_partial_locked(&mut self) {
        todo!("Task 5")
    }

    /// Go `clearLastPartialUnderLocked(rec)`.
    pub(crate) fn clear_last_partial_under_locked(&mut self, rec: &SpanRef) {
        todo!("Task 5")
    }

    /// Go `markRespondedLocked(spans)`.
    pub(crate) fn mark_responded_locked(&self, spans: &HashSet<String>) {
        todo!("Task 5")
    }

    /// Go `subtreeLocked(i)`: the span IDs of `agent_stack[i]` and the open spans under it.
    pub(crate) fn subtree_locked(&self, i: usize) -> HashSet<String> {
        todo!("Task 5")
    }

    /// Go `lineageLocked(i)`.
    pub(crate) fn lineage_locked(&self, i: usize) -> HashSet<String> {
        todo!("Task 5")
    }

    /// Go `activationOfLocked(e)`.
    pub(crate) fn activation_of_locked(&self, e: &RecordedError) -> Option<usize> {
        todo!("Task 5")
    }

    /// Go `openCallOwnersLocked(ignore)` (`ignore` may be nil).
    pub(crate) fn open_call_owners_locked(
        &self,
        ignore: Option<&dyn Fn(&SpanRecord) -> bool>,
    ) -> HashSet<String> {
        todo!("Task 5")
    }

    /// Go `agentWithOpenCallLocked(ignore)`.
    pub(crate) fn agent_with_open_call_locked(
        &self,
        ignore: Option<&dyn Fn(&SpanRecord) -> bool>,
    ) -> Option<usize> {
        todo!("Task 5")
    }

    /// Go `dropRetiredLateContextsLocked(retired...)`.
    pub(crate) fn drop_retired_late_contexts_locked(&self, retired: &[SpanRef]) {
        todo!("Task 5")
    }
}

/// Go `walkAgentEdges(root)`.
pub(crate) fn walk_agent_edges(root: Option<&Arc<dyn Agent>>) -> Vec<AgentEdge> {
    todo!("Task 5")
}

/// Go `prepareRootAgentInput(ctx, inv, id, runner, rememberedRoot)`.
pub(crate) fn prepare_root_agent_input(
    ctx: &CallbackCtx,
    inv: &InvocationTrace,
    id: &Identity,
    runner: Option<&Arc<dyn Agent>>,
    remembered_root: Option<&Arc<dyn Agent>>,
) -> RootAgentInput {
    todo!("Task 5")
}

/// Go `isLLMAgent(a)`: `gadk_engine::llm::llm_agent_config(a).is_some()` (R1(c)).
pub(crate) fn is_llm_agent(a: &dyn Agent) -> bool {
    todo!("Task 5")
}

/// Go `isBuiltInWorkflowAgent(a)`: Parallel, Sequential, Loop or Workflow agent.
pub(crate) fn is_built_in_workflow_agent(a: &dyn Agent) -> bool {
    todo!("Task 5")
}

/// Go `agentTypeOf(a)`: the agent's `agent_state().agent_type` name (`AgentType::as_str`).
pub(crate) fn agent_type_of(a: &dyn Agent) -> String {
    todo!("Task 5")
}

/// Go `topAgentFromMap(start, agentName, parents)`.
pub(crate) fn top_agent_from_map(
    start: Option<&Arc<dyn Agent>>,
    agent_name: &str,
    parents: &ParentMap,
) -> Option<Arc<dyn Agent>> {
    todo!("Task 5")
}

/// Go `withinBranch(branch, ancestor)`.
pub(crate) fn within_branch(branch: &str, ancestor: &str) -> bool {
    todo!("Task 5")
}

/// Go `withinPath(path, ancestor, sameAgent)`.
pub(crate) fn within_path(path: &str, ancestor: &str, same_agent: bool) -> bool {
    todo!("Task 5")
}

/// Go `isSubNodePath(path, owner)`.
pub(crate) fn is_sub_node_path(path: &str, owner: &str) -> bool {
    todo!("Task 5")
}
