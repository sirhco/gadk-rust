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

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, OnceLock};

use chrono::{DateTime, Utc};
use gadk_agent::{ActivationId, Agent, ParentMap};
use gadk_engine::llm::llm_agent_config;
use gadk_engine::telemetry::d6::SpanIds;
use parking_lot::{Mutex, MutexGuard, RwLock};

use super::manager::EvictedTrace;
use super::span::{
    GoroutineOrigin, Latency, RowSpan, SCHEDULER_CREATOR, SpanKind, SpanRecord, SpanRef,
};
use super::{AgentKey, CallbackCtx, Identity, TraceError, hex, new_span_id, new_trace_id};

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
        let mut root = SpanRecord {
            span_id: new_span_id(),
            kind: SpanKind::Invocation,
            start: now,
            ..SpanRecord::default()
        };
        let mut trace_id = if trace_id.is_empty() {
            new_trace_id()
        } else {
            trace_id.to_string()
        };
        let mut root_parent_span_id = String::new();
        // Continue the caller's OpenTelemetry trace with an invocation span that is a child of
        // the caller's span (Go's comment: only the trace ID and this parent follow OTel).
        if let Some(sc) = sc.filter(SpanIds::is_valid) {
            trace_id = hex(&sc.trace_id);
            root.parent_span_id = hex(&sc.span_id);
            root_parent_span_id = root.parent_span_id.clone();
        }
        let root_span_id = root.span_id.clone();
        InvocationTrace {
            last_active: AtomicI64::new(unix_nanos(now)),
            open_calls: AtomicI64::new(0),
            open_handed_over: AtomicI64::new(0),
            open_agents: AtomicBool::new(false),
            pending_spans: Arc::new(AtomicI64::new(0)),
            root_agent_atomic: RwLock::new(String::new()),
            ancestry_from_context_atomic: AtomicBool::new(false),
            adopted_trace_id: OnceLock::new(),
            trace_id,
            root_span_id,
            root_parent_span_id,
            mu: Mutex::new(InvocationState {
                agent_stack: vec![SpanRef::new(root)],
                ..InvocationState::default()
            }),
        }
    }

    /// Go `hasOpenCalls`: an in-flight model or tool call waits for its completion callback.
    pub(crate) fn has_open_calls(&self) -> bool {
        self.open_calls.load(Ordering::SeqCst) > 0
    }

    /// Go `hasOpenLateSpans`: a callback is starting a span, or a call or an activation is
    /// open (atomics only; never takes `mu`).
    pub(crate) fn has_open_late_spans(&self) -> bool {
        self.pending_spans.load(Ordering::SeqCst) > 0
            || self.has_open_calls()
            || self.open_agents.load(Ordering::SeqCst)
    }

    /// Go `lockLive`: the locked state, or `None` (unlocked) once the run finished.
    pub(crate) fn lock_live(&self) -> Option<MutexGuard<'_, InvocationState>> {
        let st = self.mu.lock();
        if st.finished { None } else { Some(st) }
    }

    /// Go `currentTraceID`: the trace ID of rows of callbacks that run in no span (the
    /// adopted one, else the invocation's own). Needs no lock.
    pub(crate) fn current_trace_id(&self) -> String {
        match self.adopted_trace_id.get() {
            Some(adopted) => adopted.clone(),
            None => self.trace_id.clone(),
        }
    }

    /// Go `rowTraceID(ambient)`: the ambient trace wins; the first one is adopted unless the
    /// caller's span already gave the invocation its trace.
    pub(crate) fn row_trace_id(&self, ambient: &str) -> String {
        if ambient.is_empty() {
            return self.current_trace_id();
        }
        if self.root_parent_span_id.is_empty() {
            let _ = self.adopted_trace_id.set(ambient.to_string());
        }
        ambient.to_string()
    }

    /// Go `rowSpan(rec, latency, ambient)` (`timestamp` left `None`).
    pub(crate) fn row_span(
        &self,
        st: &InvocationState,
        rec: &SpanRecord,
        latency: Option<Latency>,
        ambient: &str,
    ) -> RowSpan {
        RowSpan {
            trace_id: self.row_trace_id(ambient),
            span_id: rec.span_id.clone(),
            parent_span_id: rec.parent_span_id.clone(),
            latency,
            root_agent_name: st.root_agent_name.clone(),
            timestamp: None,
        }
    }

    /// Go `savedIdentity`.
    pub(crate) fn saved_identity(&self) -> EvictedTrace {
        EvictedTrace {
            trace_id: self.current_trace_id(),
            span_id: self.root_span_id.clone(),
            parent_span_id: self.root_parent_span_id.clone(),
            root_agent_name: self.root_agent_atomic.read().clone(),
            last_active: self.last_active.load(Ordering::SeqCst),
        }
    }

    /// Go `restoreEvicted(ev)`: restores the root span identity and trace metadata saved when
    /// the invocation was evicted or completed. Go calls it only on a trace that is not
    /// published yet, and it rewrites the read-only-once-published `trace_id` /
    /// `root_span_id` / `root_parent_span_id`, so it takes the unshared trace (`&mut self`).
    pub(crate) fn restore_evicted(&mut self, ev: &EvictedTrace) {
        if !ev.trace_id.is_empty() {
            self.trace_id = ev.trace_id.clone();
        }
        if !ev.span_id.is_empty() {
            self.root_span_id = ev.span_id.clone();
        }
        self.root_parent_span_id = ev.parent_span_id.clone();
        let st = self.mu.get_mut();
        st.evicted = true;
        if let Some(root) = st.agent_stack.first() {
            let mut root = root.lock();
            if !ev.span_id.is_empty() {
                root.span_id = ev.span_id.clone();
            }
            root.parent_span_id = ev.parent_span_id.clone();
        }
        st.root_agent_name = ev.root_agent_name.clone();
        if !ev.root_agent_name.is_empty() {
            *self.root_agent_atomic.write() = ev.root_agent_name.clone();
        }
    }

    /// Go `syncOpenCallsLocked`: publishes the number of open model and tool calls, and of
    /// those AfterRun handed over, for the readers that do not hold `mu`.
    pub(crate) fn sync_open_calls_locked(&self, st: &InvocationState) {
        let open = st.llm_spans.len() + st.tool_spans.len();
        self.open_calls.store(open as i64, Ordering::SeqCst);
        let handed_over = st
            .llm_spans
            .values()
            .chain(st.tool_spans.values())
            .filter(|rec| rec.lock().handed_over)
            .count();
        self.open_handed_over
            .store(handed_over as i64, Ordering::SeqCst);
    }

    /// Go `syncOpenAgentsLocked`.
    pub(crate) fn sync_open_agents_locked(&self, st: &InvocationState) {
        self.open_agents
            .store(st.agent_stack.len() > 1, Ordering::SeqCst);
    }

    /// Go `setRootAgentLocked(name)`.
    pub(crate) fn set_root_agent_locked(&self, st: &mut InvocationState, name: &str) {
        st.root_agent_name = name.to_string();
        if !name.is_empty() {
            *self.root_agent_atomic.write() = name.to_string();
        }
    }

    /// Go `applyEdgesLocked(edges)`.
    pub(crate) fn apply_edges_locked(&self, st: &mut InvocationState, edges: &[AgentEdge]) {
        for e in edges {
            st.parent_agent.insert(e.child.clone(), e.parent.clone());
            if e.non_llm {
                st.sequential_parent
                    .insert(e.child.clone(), e.parent.clone());
            }
        }
    }

    /// Go `noteRootAgentLocked(id, in, useBranchFallback)`: records the invocation's root
    /// agent and agent hierarchy from `input`, collected before `mu` was taken. The context's
    /// parent map is authoritative even when empty; remembered trees count only without one.
    pub(crate) fn note_root_agent_locked(
        &self,
        st: &mut InvocationState,
        id: &Identity,
        input: &RootAgentInput,
        use_branch_fallback: bool,
    ) {
        if !st.ancestry_from_context && input.has_context_parents {
            st.ancestry_from_context = true;
            self.ancestry_from_context_atomic
                .store(true, Ordering::SeqCst);
            self.apply_edges_locked(st, &input.context_edges);
            if let Some(root) = &input.context_root {
                st.runner_agent = Some(root.clone());
                st.runner_agent_name = input.context_root_name.clone();
                if !input.context_root_name.is_empty() {
                    st.configured_root = input.context_root_name.clone();
                    self.set_root_agent_locked(st, &input.context_root_name);
                }
            }
        }
        if let Some(runner) = &input.runner {
            self.apply_edges_locked(st, &input.runner_edges);
            if let Some(remembered) = &input.remembered
                && !same_agent(remembered, runner)
                && !st.ancestry_from_context
            {
                self.apply_edges_locked(st, &input.remembered_edges);
                if st.runner_agent.is_none() {
                    st.runner_agent = Some(remembered.clone());
                    st.runner_agent_name = input.remembered_name.clone();
                }
                if !input.remembered_name.is_empty() && st.configured_root.is_empty() {
                    st.configured_root = input.remembered_name.clone();
                    self.set_root_agent_locked(st, &input.remembered_name);
                }
            }
            if st.runner_agent.is_none()
                || st.is_ancestor_agent(&input.runner_name, &st.runner_agent_name)
            {
                st.runner_agent = Some(runner.clone());
                st.runner_agent_name = input.runner_name.clone();
            }
            let below =
                first_non_empty(&[&st.configured_root, &st.root_agent_name, &id.agent_name])
                    .to_string();
            if !input.runner_name.is_empty() && st.is_ancestor_agent(&input.runner_name, &below) {
                st.configured_root = input.runner_name.clone();
                self.set_root_agent_locked(st, &input.runner_name);
            }
        }
        if !st.configured_root.is_empty() {
            let root = st.configured_root.clone();
            self.set_root_agent_locked(st, &root);
            return;
        }
        if st.root_agent_name.is_empty() {
            if input.runner.is_some() {
                let name = first_non_empty(&[&id.agent_name, &input.runner_name]).to_string();
                self.set_root_agent_locked(st, &name);
            } else if use_branch_fallback && !id.branch.is_empty() {
                let root = id.branch.split('.').next().unwrap_or_default().to_string();
                self.set_root_agent_locked(st, &root);
            } else {
                self.set_root_agent_locked(st, &id.agent_name);
            }
        }
    }
}

impl InvocationState {
    /// Go `isAncestorAgent(ancestor, child)`: `ancestor` is a configured ancestor of `child`
    /// in `parent_agent`.
    pub(crate) fn is_ancestor_agent(&self, ancestor: &str, child: &str) -> bool {
        if ancestor.is_empty() || child.is_empty() {
            return false;
        }
        let mut cur = self.parent_agent.get(child);
        let mut steps = 0;
        while let Some(c) = cur.filter(|c| !c.is_empty()) {
            if steps > self.parent_agent.len() {
                break;
            }
            if c == ancestor {
                return true;
            }
            cur = self.parent_agent.get(c.as_str());
            steps += 1;
        }
        false
    }

    /// Go `findAgent(path, name, branch)`: the index of the most recent open span of `name`
    /// on `path` and `branch`, falling back to `branch` and `name`, or `name` alone; never the
    /// invocation span.
    pub(crate) fn find_agent(&self, path: &str, name: &str, branch: &str) -> Option<usize> {
        let mut branch_match = None;
        let mut name_match = None;
        for i in (1..self.agent_stack.len()).rev() {
            let rec = self.agent_stack[i].lock();
            if rec.kind != SpanKind::Agent
                || rec.name != name
                || (!path.is_empty() && !rec.path.is_empty() && rec.path != path)
            {
                continue;
            }
            if !path.is_empty() && rec.path == path && rec.branch == branch {
                return Some(i);
            }
            if rec.branch == branch && branch_match.is_none() {
                if path.is_empty() || rec.path.is_empty() {
                    return Some(i);
                }
                branch_match = Some(i);
            }
            if name_match.is_none() {
                name_match = Some(i);
            }
        }
        branch_match.or(name_match)
    }

    /// Go `findAgentForContext(ctx, path, name, branch)` with `activation` =
    /// `ctx.activation_key()`: the open span of `name` on `branch` that the activation
    /// started, else as [`find_agent`](Self::find_agent).
    pub(crate) fn find_agent_for_context(
        &self,
        activation: Option<ActivationId>,
        path: &str,
        name: &str,
        branch: &str,
    ) -> Option<usize> {
        if let Some(act) = activation {
            for i in (1..self.agent_stack.len()).rev() {
                let rec = self.agent_stack[i].lock();
                if rec.kind == SpanKind::Agent
                    && rec.activation == Some(act)
                    && rec.name == name
                    && rec.branch == branch
                {
                    return Some(i);
                }
            }
        }
        self.find_agent(path, name, branch)
    }

    /// Go `parentFor(id)`: the span `id`'s activation runs under (the invocation span at
    /// worst).
    pub(crate) fn parent_for(&self, id: &Identity) -> SpanRef {
        self.parent_in(&self.agent_stack, id)
    }

    /// Go `parentFor` over `stack` (Go's `parentOutsideLocked` swaps `agentStack` for a copy
    /// without the excluded spans; this takes the copy instead).
    fn parent_in(&self, stack: &[SpanRef], id: &Identity) -> SpanRef {
        let mut top_candidate: Option<SpanRef> = None;
        for rec_ref in stack.iter().skip(1).rev() {
            let rec = rec_ref.lock();
            if !within_branch(&id.branch, &rec.branch)
                || !within_path(&id.path, &rec.path, rec.name == id.agent_name)
            {
                continue;
            }
            if top_candidate.is_none() {
                top_candidate = Some(rec_ref.clone());
            }
            if rec.children.contains(&id.agent_name) {
                if rec.non_llm {
                    return rec_ref.clone();
                }
                return top_candidate.unwrap_or_else(|| rec_ref.clone());
            }
        }
        let has_sequential_parent = self
            .sequential_parent
            .get(&id.agent_name)
            .is_some_and(|p| !p.is_empty());
        for rec_ref in stack.iter().skip(1).rev() {
            let rec = rec_ref.lock();
            if !within_branch(&id.branch, &rec.branch)
                || !within_path(&id.path, &rec.path, rec.name == id.agent_name)
            {
                continue;
            }
            if has_sequential_parent && !self.is_ancestor_agent(&rec.name, &id.agent_name) {
                continue;
            }
            return rec_ref.clone();
        }
        stack.first().cloned().unwrap_or_default()
    }

    /// Go `parentOutsideLocked(id, exclude)`: the span an agent starting with `id` would run
    /// under if the spans in `exclude` had ended.
    pub(crate) fn parent_outside_locked(
        &self,
        id: &Identity,
        exclude: &HashSet<String>,
    ) -> SpanRef {
        let stack: Vec<SpanRef> = self
            .agent_stack
            .iter()
            .filter(|r| !exclude.contains(&r.lock().span_id))
            .cloned()
            .collect();
        self.parent_in(&stack, id)
    }

    /// Go `parentForRunLocked(id, goid, origin)` (attribution: with `goid == 0` it is
    /// `parent_for`): leaves out the open activations of `id`'s agent, branch and node path
    /// that the workflow scheduler started on other goroutines, with their subtrees.
    pub(crate) fn parent_for_run_locked(
        &self,
        id: &Identity,
        goid: u64,
        origin: &GoroutineOrigin,
    ) -> SpanRef {
        let mut exclude: Option<HashSet<String>> = None;
        for i in 1..self.agent_stack.len() {
            let skip = {
                let other = self.agent_stack[i].lock();
                match &other.parent {
                    None => true,
                    Some(parent_ref) => {
                        let parent = parent_ref.lock();
                        other.kind != SpanKind::Agent
                            || other.name != id.agent_name
                            || other.branch != id.branch
                            || other.path != id.path
                            || other.goroutine_id == goid
                            || !parent.is_workflow
                            || !origin.started_by(SCHEDULER_CREATOR, Some(&parent))
                            || !other.origin.started_by(SCHEDULER_CREATOR, Some(&parent))
                    }
                }
            };
            if skip {
                continue;
            }
            exclude
                .get_or_insert_with(HashSet::new)
                .extend(self.subtree_locked(i));
        }
        match exclude {
            None => self.parent_for(id),
            Some(exclude) => self.parent_outside_locked(id, &exclude),
        }
    }

    /// Go `ownerOf(ctx, id)`: the span the model and tool calls of `id` run under — the
    /// agent's own span (preferring the activation's), else the span it would run under.
    pub(crate) fn owner_of(&self, activation: Option<ActivationId>, id: &Identity) -> SpanRef {
        match self.find_agent_for_context(activation, &id.path, &id.agent_name, &id.branch) {
            Some(i) => self.agent_stack[i].clone(),
            None => self.parent_for(id),
        }
    }

    /// Go `newChild(parent, kind, now)`: a new, untracked span under `parent`.
    pub(crate) fn new_child(
        &self,
        parent: &SpanRef,
        kind: SpanKind,
        now: DateTime<Utc>,
    ) -> SpanRef {
        let (parent_span_id, stop_point) = {
            let p = parent.lock();
            (p.span_id.clone(), p.kind == SpanKind::Invocation)
        };
        SpanRef::new(SpanRecord {
            span_id: new_span_id(),
            parent_span_id,
            kind,
            start: now,
            parent: Some(parent.clone()),
            stop_point,
            ..SpanRecord::default()
        })
    }

    /// Go `touchAgentActivityLocked(rec, now)`: moves `last_activity` of `rec` and the agent
    /// activations above it (up to the first non-agent span) forward to `now`. Locks each
    /// record itself: call it with no guard of the chain alive.
    pub(crate) fn touch_agent_activity_locked(&self, rec: &SpanRef, now: DateTime<Utc>) {
        let mut cur = Some(rec.clone());
        while let Some(r) = cur {
            let mut g = r.lock();
            if g.kind != SpanKind::Agent {
                break;
            }
            if now > g.last_activity {
                g.last_activity = now;
            }
            cur = g.parent.clone();
        }
    }

    /// Go `nextSeqLocked`.
    pub(crate) fn next_seq_locked(&mut self) -> u64 {
        self.last_seq += 1;
        self.last_seq
    }

    /// Go `sortedErrorKeysLocked`: the keys of `unrecovered_errors` by (bare key, seq, key).
    pub(crate) fn sorted_error_keys_locked(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.unrecovered_errors.keys().cloned().collect();
        keys.sort_by(|a, b| {
            let (ea, eb) = (&self.unrecovered_errors[a], &self.unrecovered_errors[b]);
            ea.bare_key
                .cmp(&eb.bare_key)
                .then(ea.seq.cmp(&eb.seq))
                .then(a.cmp(b))
        });
        keys
    }

    /// Go `clearLastPartialLocked`: the caller read past the last partial event.
    pub(crate) fn clear_last_partial_locked(&mut self) {
        self.last_session_event_partial = false;
        self.last_event_partial = false;
        self.last_partial_span_id.clear();
        self.partial_lineage.clear();
    }

    /// Go `clearLastPartialUnderLocked(rec)`: clears `last_event_partial` when work starts
    /// in `rec` under the last partial event's author, or after the author ended.
    pub(crate) fn clear_last_partial_under_locked(&mut self, rec: &SpanRef) {
        if self.last_partial_span_id.is_empty() {
            return;
        }
        let author_idx = self
            .agent_stack
            .iter()
            .position(|r| r.lock().span_id == self.last_partial_span_id);
        let under_author = self
            .agent_stack
            .iter()
            .position(|r| r.ptr_eq(rec))
            .is_some_and(|i| self.lineage_locked(i).contains(&self.last_partial_span_id));
        if author_idx.is_none() || under_author {
            self.last_event_partial = false;
            self.last_partial_span_id.clear();
        }
    }

    /// Go `markRespondedLocked(spans)`.
    pub(crate) fn mark_responded_locked(&self, spans: &HashSet<String>) {
        for rec in self.agent_stack.iter().skip(1) {
            let mut rec = rec.lock();
            if spans.contains(&rec.span_id) {
                rec.has_response = true;
            }
        }
    }

    /// Go `subtreeLocked(i)`: the span IDs of `agent_stack[i]` and the open spans under it.
    pub(crate) fn subtree_locked(&self, i: usize) -> HashSet<String> {
        let mut subtree = HashSet::from([self.agent_stack[i].lock().span_id.clone()]);
        for rec_ref in &self.agent_stack[i + 1..] {
            let (span_id, mut p) = {
                let rec = rec_ref.lock();
                (rec.span_id.clone(), rec.parent.clone())
            };
            while let Some(p_ref) = p {
                let pr = p_ref.lock();
                if pr.kind != SpanKind::Agent {
                    break;
                }
                if subtree.contains(&pr.span_id) {
                    subtree.insert(span_id);
                    break;
                }
                p = pr.parent.clone();
            }
        }
        subtree
    }

    /// Go `lineageLocked(i)`: the span IDs of `agent_stack[i]` and of the open agent spans it
    /// runs under; empty if `i` is not an agent span index.
    pub(crate) fn lineage_locked(&self, i: usize) -> HashSet<String> {
        let mut lineage = HashSet::new();
        if i == 0 || i >= self.agent_stack.len() {
            return lineage;
        }
        let by_id: HashMap<String, &SpanRef> = self.agent_stack[1..]
            .iter()
            .map(|r| (r.lock().span_id.clone(), r))
            .collect();
        let mut cur = Some(&self.agent_stack[i]);
        while let Some(r) = cur {
            let (span_id, parent_span_id) = {
                let g = r.lock();
                (g.span_id.clone(), g.parent_span_id.clone())
            };
            if !lineage.insert(span_id) {
                break;
            }
            cur = by_id.get(&parent_span_id).copied();
        }
        lineage
    }

    /// Go `activationOfLocked(e)`: the index of the open activation `e` is attributed to.
    pub(crate) fn activation_of_locked(&self, e: &RecordedError) -> Option<usize> {
        if e.span_id.is_empty() {
            return None;
        }
        (1..self.agent_stack.len())
            .rev()
            .find(|&i| self.agent_stack[i].lock().span_id == e.span_id)
    }

    /// Go `openCallOwnersLocked(ignore)` (`ignore` may be nil): the parent span IDs of the
    /// open calls, without streamed model calls.
    pub(crate) fn open_call_owners_locked(
        &self,
        ignore: Option<&dyn Fn(&SpanRecord) -> bool>,
    ) -> HashSet<String> {
        let mut owners = HashSet::new();
        for rec in self.llm_spans.values() {
            let rec = rec.lock();
            if !rec.streamed && !ignore.is_some_and(|f| f(&rec)) {
                owners.insert(rec.parent_span_id.clone());
            }
        }
        for rec in self.tool_spans.values() {
            let rec = rec.lock();
            if !ignore.is_some_and(|f| f(&rec)) {
                owners.insert(rec.parent_span_id.clone());
            }
        }
        owners
    }

    /// Go `agentWithOpenCallLocked(ignore)`: the most recent open agent span an open call
    /// runs under.
    pub(crate) fn agent_with_open_call_locked(
        &self,
        ignore: Option<&dyn Fn(&SpanRecord) -> bool>,
    ) -> Option<usize> {
        let owners = self.open_call_owners_locked(ignore);
        (1..self.agent_stack.len())
            .rev()
            .find(|&i| owners.contains(&self.agent_stack[i].lock().span_id))
    }

    // `SpanRef` hashes by address, never by the record behind its lock.
    #[allow(clippy::mutable_key_type)]
    /// Go `dropRetiredLateContextsLocked(retired...)`: drops the callback contexts of agent
    /// spans retired from a late trace, and the `parent` of those no open span or call still
    /// reaches through its parent chain.
    pub(crate) fn drop_retired_late_contexts_locked(&self, retired: &[SpanRef]) {
        for r in retired {
            let mut r = r.lock();
            r.canceled = None;
            r.ended = None;
            r.activation = None;
            r.retired_causes = Vec::new();
        }
        let mut keep_parent: HashSet<SpanRef> = HashSet::new();
        let mut mark_chain = |s: &SpanRef| {
            let mut p = s.lock().parent.clone();
            while let Some(p_ref) = p {
                if keep_parent.contains(&p_ref) {
                    break;
                }
                let g = p_ref.lock();
                if g.kind != SpanKind::Agent {
                    break;
                }
                let next = g.parent.clone();
                drop(g);
                keep_parent.insert(p_ref);
                p = next;
            }
        };
        for s in self.agent_stack.iter().skip(1) {
            mark_chain(s);
        }
        for s in self.llm_spans.values().chain(self.tool_spans.values()) {
            mark_chain(s);
        }
        for r in retired {
            if !keep_parent.contains(r) {
                r.lock().parent = None;
            }
        }
    }
}

/// Go `walkAgentEdges(root)`: the child-to-parent links of `root`'s sub-agent tree (each
/// name visited once).
pub(crate) fn walk_agent_edges(root: Option<&Arc<dyn Agent>>) -> Vec<AgentEdge> {
    fn walk(a: &Arc<dyn Agent>, visited: &mut HashSet<String>, edges: &mut Vec<AgentEdge>) {
        let parent_name = a.name();
        if parent_name.is_empty() || !visited.insert(parent_name.to_string()) {
            return;
        }
        let non_llm = !is_llm_agent(&**a);
        for sub in a.sub_agents() {
            let sub_name = sub.name();
            if !sub_name.is_empty() {
                edges.push(AgentEdge {
                    child: sub_name.to_string(),
                    parent: parent_name.to_string(),
                    non_llm,
                });
                walk(sub, visited, edges);
            }
        }
    }
    let mut edges = Vec::new();
    if let Some(root) = root {
        walk(root, &mut HashSet::new(), &mut edges);
    }
    edges
}

/// The entries (child name, parent) of a [`ParentMap`], Go's `map[string]agent.Agent`: gadk's
/// map exposes lookups and its root only, and it is built by walking the root's sub-agents
/// (names unique, one parent each), so the same walk lists its entries (in tree order where
/// Go's map order is random).
pub(crate) fn parent_map_entries(parents: &ParentMap) -> Vec<(String, Arc<dyn Agent>)> {
    fn walk(
        cur: &Arc<dyn Agent>,
        seen: &mut HashSet<AgentKey>,
        out: &mut Vec<(String, Arc<dyn Agent>)>,
    ) {
        if !seen.insert(AgentKey::of(cur)) {
            return;
        }
        for sub in cur.sub_agents() {
            out.push((sub.name().to_string(), cur.clone()));
            walk(sub, seen, out);
        }
    }
    let mut out = Vec::new();
    if let Some(root) = parents.root() {
        walk(&root, &mut HashSet::new(), &mut out);
    }
    out
}

/// Go `prepareRootAgentInput(ctx, inv, id, runner, rememberedRoot)`: the agent-tree metadata
/// of the context's parent map, the runner and the remembered root, read before `mu` (agent
/// code never runs under a lock).
pub(crate) fn prepare_root_agent_input(
    ctx: &CallbackCtx,
    inv: &InvocationTrace,
    id: &Identity,
    runner: Option<&Arc<dyn Agent>>,
    remembered_root: Option<&Arc<dyn Agent>>,
) -> RootAgentInput {
    let mut input = RootAgentInput::default();
    if !inv.ancestry_from_context_atomic.load(Ordering::SeqCst)
        && let Some(parents) = ctx.parent_map()
    {
        input.has_context_parents = true;
        for (child, parent) in parent_map_entries(&parents) {
            let p_name = parent.name();
            if !child.is_empty() && !p_name.is_empty() {
                input.context_edges.push(AgentEdge {
                    child,
                    parent: p_name.to_string(),
                    non_llm: !is_llm_agent(&*parent),
                });
            }
            input.context_edges.extend(walk_agent_edges(Some(&parent)));
        }
        if let Some(root) = top_agent_from_map(runner, &id.agent_name, &parents) {
            input.context_root_name = root.name().to_string();
            input.context_root = Some(root);
        }
    }
    if let Some(runner) = runner {
        input.runner = Some(runner.clone());
        input.runner_name = runner.name().to_string();
        input.runner_edges = walk_agent_edges(Some(runner));
        if let Some(remembered) = remembered_root
            && !same_agent(remembered, runner)
        {
            input.remembered = Some(remembered.clone());
            input.remembered_name = remembered.name().to_string();
            input.remembered_edges = walk_agent_edges(Some(remembered));
        }
    }
    input
}

/// Go `isLLMAgent(a)`: `gadk_engine::llm::llm_agent_config(a).is_some()` (R1(c)).
pub(crate) fn is_llm_agent(a: &dyn Agent) -> bool {
    llm_agent_config(a).is_some()
}

/// Go `isBuiltInWorkflowAgent(a)`: Parallel, Sequential, Loop or Workflow agent.
pub(crate) fn is_built_in_workflow_agent(a: &dyn Agent) -> bool {
    matches!(
        agent_type_of(a).as_str(),
        "ParallelAgent" | "SequentialAgent" | "LoopAgent" | "WorkflowAgent"
    )
}

/// Go `agentTypeOf(a)`: the agent's `agent_state().agent_type` name (`AgentType::as_str`).
pub(crate) fn agent_type_of(a: &dyn Agent) -> String {
    a.agent_state().agent_type.as_str().to_string()
}

/// Go `topAgentFromMap(start, agentName, parents)`: walks `parents` upward from `start` (or
/// the parent of `agent_name`, or any parent) to the configured root of the runner's tree.
pub(crate) fn top_agent_from_map(
    start: Option<&Arc<dyn Agent>>,
    agent_name: &str,
    parents: &ParentMap,
) -> Option<Arc<dyn Agent>> {
    let entries = parent_map_entries(parents);
    let mut cur = start.cloned();
    if cur.is_none() && !agent_name.is_empty() {
        cur = parents.parent(agent_name);
    }
    if cur.is_none() {
        cur = entries.first().map(|(_, p)| p.clone());
    }
    for _ in 0..=entries.len() {
        let Some(c) = cur.clone() else {
            break;
        };
        match parents.parent(c.name()) {
            Some(next) if !same_agent(&next, &c) => cur = Some(next),
            _ => return Some(c),
        }
    }
    cur
}

/// Go `withinBranch(branch, ancestor)`: `branch` is `ancestor` or one of its descendants
/// (`"parent.parallel.sub"`).
pub(crate) fn within_branch(branch: &str, ancestor: &str) -> bool {
    ancestor.is_empty()
        || branch == ancestor
        || branch
            .strip_prefix(ancestor)
            .is_some_and(|rest| rest.starts_with('.'))
}

/// Go `withinPath(path, ancestor, sameAgent)`: `path` runs inside the node activation
/// `ancestor` (an activation of the same agent on the same path runs beside it).
pub(crate) fn within_path(path: &str, ancestor: &str, same_agent: bool) -> bool {
    if path == ancestor {
        return !same_agent;
    }
    if path.is_empty() || ancestor.is_empty() {
        return true;
    }
    path.strip_prefix(ancestor)
        .is_some_and(|rest| rest.starts_with('/'))
}

/// Go `isSubNodePath(path, owner)`.
pub(crate) fn is_sub_node_path(path: &str, owner: &str) -> bool {
    if owner.is_empty() {
        return !path.is_empty();
    }
    path.strip_prefix(owner)
        .is_some_and(|rest| rest.starts_with('/'))
}

/// Go `a == b` on `agent.Agent` interfaces.
pub(crate) fn same_agent(a: &Arc<dyn Agent>, b: &Arc<dyn Agent>) -> bool {
    AgentKey::of(a) == AgentKey::of(b)
}

/// Go `cmp.Or` over strings: the first non-empty one, or "".
fn first_non_empty<'a>(values: &[&'a str]) -> &'a str {
    values.iter().copied().find(|v| !v.is_empty()).unwrap_or("")
}

/// Go `t.UnixNano()` (saturating outside chrono's nanosecond range).
pub(crate) fn unix_nanos(t: DateTime<Utc>) -> i64 {
    t.timestamp_nanos_opt().unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use chrono::TimeZone;
    use gadk_agent::Mode;

    use super::super::testkit::{adk_ctx, custom, ident, llm, parallel, sequential};
    use super::*;
    use crate::context::ContextError;
    use crate::error::Error;

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()
    }

    /// Pushes an agent span `name` on `branch` / `path` under `parent` (the invocation span
    /// when `None`), as `push_agent_span` records it.
    fn push(
        st: &mut InvocationState,
        parent: Option<&SpanRef>,
        name: &str,
        branch: &str,
        path: &str,
        children: &[&str],
        non_llm: bool,
    ) -> SpanRef {
        let parent = parent.cloned().unwrap_or_else(|| st.agent_stack[0].clone());
        let rec = st.new_child(&parent, SpanKind::Agent, t0());
        {
            let mut r = rec.lock();
            r.name = name.into();
            r.branch = branch.into();
            r.path = path.into();
            r.children = children.iter().map(|c| c.to_string()).collect();
            r.non_llm = non_llm;
        }
        st.agent_stack.push(rec.clone());
        rec
    }

    fn span_id(r: &SpanRef) -> String {
        r.lock().span_id.clone()
    }

    #[test]
    fn branch_and_path_containment() {
        assert!(within_branch("anything", ""));
        assert!(within_branch("root", "root"));
        assert!(within_branch("root.par.a", "root"));
        assert!(!within_branch("rooted", "root"));
        assert!(within_path("wf/a", "wf/a", false));
        assert!(!within_path("wf/a", "wf/a", true));
        assert!(!within_path("", "", true));
        assert!(within_path("", "wf", true));
        assert!(within_path("wf/a@1", "wf", true));
        assert!(!within_path("wfx/a", "wf", false));
        assert!(is_sub_node_path("wf/a", "wf"));
        assert!(!is_sub_node_path("wf", "wf"));
        assert!(is_sub_node_path("wf", ""));
        assert!(!is_sub_node_path("", ""));
    }

    #[test]
    fn new_trace_continues_a_valid_ambient_span() {
        let sc = SpanIds {
            trace_id: [0x11; 16],
            span_id: [0x22; 8],
        };
        let inv = InvocationTrace::new_with_span(Some(sc), "seeded", t0());
        assert_eq!(inv.trace_id, "11".repeat(16));
        assert_eq!(inv.root_parent_span_id, "22".repeat(8));
        let st = inv.mu.lock();
        assert_eq!(st.agent_stack.len(), 1);
        let root = st.agent_stack[0].lock();
        assert_eq!(root.kind, SpanKind::Invocation);
        assert_eq!(root.span_id, inv.root_span_id);
        assert_eq!(root.parent_span_id, "22".repeat(8));
        assert_eq!(inv.last_active.load(Ordering::SeqCst), unix_nanos(t0()));

        let inv = InvocationTrace::new_with_span(None, "seeded", t0());
        assert_eq!(inv.trace_id, "seeded");
        assert!(inv.root_parent_span_id.is_empty());
        let inv = InvocationTrace::new_with_span(Some(SpanIds::default()), "", t0());
        assert_eq!(inv.trace_id.len(), 32);
    }

    #[test]
    fn rows_adopt_the_first_ambient_trace() {
        let inv = InvocationTrace::new_with_span(None, "own", t0());
        assert_eq!(inv.row_trace_id(""), "own");
        assert_eq!(inv.row_trace_id("amb1"), "amb1");
        assert_eq!(inv.row_trace_id("amb2"), "amb2");
        assert_eq!(inv.row_trace_id(""), "amb1");
        assert_eq!(inv.current_trace_id(), "amb1");
        assert_eq!(inv.saved_identity().trace_id, "amb1");

        let sc = SpanIds {
            trace_id: [0x11; 16],
            span_id: [0x22; 8],
        };
        let inv = InvocationTrace::new_with_span(Some(sc), "", t0());
        assert_eq!(inv.row_trace_id("amb"), "amb");
        assert_eq!(inv.row_trace_id(""), "11".repeat(16));
    }

    #[test]
    fn restore_evicted_keeps_the_saved_identity() {
        let mut inv = InvocationTrace::new_with_span(None, "fresh", t0());
        inv.restore_evicted(&EvictedTrace {
            trace_id: "saved".into(),
            span_id: "root-span".into(),
            parent_span_id: "caller".into(),
            root_agent_name: "root".into(),
            last_active: 1,
        });
        assert_eq!(inv.trace_id, "saved");
        assert_eq!(inv.root_span_id, "root-span");
        let st = inv.mu.lock();
        assert!(st.evicted);
        assert_eq!(st.root_agent_name, "root");
        assert_eq!(st.agent_stack[0].lock().span_id, "root-span");
        assert_eq!(st.agent_stack[0].lock().parent_span_id, "caller");
        drop(st);
        let saved = inv.saved_identity();
        assert_eq!(
            (saved.span_id.as_str(), saved.parent_span_id.as_str()),
            ("root-span", "caller")
        );
        assert_eq!(saved.root_agent_name, "root");
    }

    #[test]
    fn find_agent_prefers_path_then_branch_then_name() {
        let inv = InvocationTrace::new_with_span(None, "", t0());
        let mut st = inv.mu.lock();
        push(&mut st, None, "a", "x", "p2", &[], false);
        let a_branch = push(&mut st, None, "a", "b", "", &[], false);
        let a_exact = push(&mut st, None, "a", "b", "p", &[], false);
        assert_eq!(st.find_agent("p", "a", "b"), Some(3));
        assert!(st.agent_stack[3].ptr_eq(&a_exact));
        assert_eq!(st.find_agent("", "a", "b"), Some(3));
        assert_eq!(st.find_agent("q", "a", "b"), Some(2));
        assert!(st.agent_stack[2].ptr_eq(&a_branch));
        assert_eq!(st.find_agent("", "a", "zz"), Some(3));
        assert_eq!(st.find_agent("", "missing", "b"), None);

        let act = adk_ctx(&custom("a", vec![]), "inv", None).activation_id();
        st.agent_stack[1].lock().activation = Some(act);
        assert_eq!(st.find_agent_for_context(Some(act), "p", "a", "x"), Some(1));
        assert_eq!(st.find_agent_for_context(Some(act), "p", "a", "b"), Some(3));
    }

    #[test]
    fn parent_for_follows_declared_children_and_the_tree() {
        let inv = InvocationTrace::new_with_span(None, "", t0());
        let mut st = inv.mu.lock();
        // A sequential agent declares `a` and `b`; `a` is still open when `b` starts.
        let seq = push(&mut st, None, "seq", "", "", &["a", "b"], true);
        let a = push(&mut st, Some(&seq), "a", "", "", &[], false);
        let b_id = ident("inv", "b", "", "");
        assert!(st.parent_for(&b_id).ptr_eq(&seq));
        // An LLM parent keeps a peer transfer under the active peer.
        st.agent_stack[1].lock().non_llm = false;
        assert!(st.parent_for(&b_id).ptr_eq(&a));
        // Without a declaring activation, the configured tree decides.
        st.agent_stack[1].lock().children.clear();
        st.sequential_parent.insert("b".into(), "seq".into());
        st.parent_agent.insert("b".into(), "seq".into());
        assert!(st.parent_for(&b_id).ptr_eq(&seq));
        // An agent outside every open branch runs under the invocation.
        let other = ident("inv", "z", "elsewhere", "");
        st.agent_stack[1].lock().branch = "seq".into();
        st.agent_stack[2].lock().branch = "seq".into();
        assert!(st.parent_for(&other).ptr_eq(&st.agent_stack[0]));
        // Excluding the subtree of `seq` leaves the invocation span.
        let exclude = st.subtree_locked(1);
        assert!(
            st.parent_outside_locked(&b_id, &exclude)
                .ptr_eq(&st.agent_stack[0])
        );
        // Without goroutine information it is `parent_for`.
        let run_parent = st.parent_for_run_locked(&b_id, 0, &GoroutineOrigin::default());
        assert!(run_parent.ptr_eq(&st.parent_for(&b_id)));
        assert!(st.owner_of(None, &ident("inv", "a", "seq", "")).ptr_eq(&a));
    }

    #[test]
    fn subtree_and_lineage() {
        let inv = InvocationTrace::new_with_span(None, "", t0());
        let mut st = inv.mu.lock();
        let r = push(&mut st, None, "r", "", "", &[], false);
        let a = push(&mut st, Some(&r), "a", "", "", &[], false);
        let other = push(&mut st, None, "o", "", "", &[], false);
        let a1 = push(&mut st, Some(&a), "a1", "", "", &[], false);
        let sub = st.subtree_locked(2);
        assert_eq!(sub, HashSet::from([span_id(&a), span_id(&a1)]));
        assert_eq!(
            st.lineage_locked(4),
            HashSet::from([span_id(&a1), span_id(&a), span_id(&r)])
        );
        assert!(st.lineage_locked(0).is_empty());
        assert!(st.lineage_locked(9).is_empty());
        assert!(!st.subtree_locked(1).contains(&span_id(&other)));
        let e = RecordedError {
            err: Arc::new(Error::Context(ContextError::Canceled)),
            cause: None,
            span_id: span_id(&a),
            bare_key: String::new(),
            seq: 0,
            canceled: false,
            collateral: false,
        };
        assert_eq!(st.activation_of_locked(&e), Some(2));
        assert_eq!(
            st.activation_of_locked(&RecordedError {
                span_id: String::new(),
                ..e
            }),
            None
        );
        st.mark_responded_locked(&HashSet::from([span_id(&a)]));
        assert!(a.lock().has_response && !r.lock().has_response);
    }

    #[test]
    fn touch_moves_activity_forward_up_the_agents() {
        let inv = InvocationTrace::new_with_span(None, "", t0());
        let mut st = inv.mu.lock();
        let r = push(&mut st, None, "r", "", "", &[], false);
        let a = push(&mut st, Some(&r), "a", "", "", &[], false);
        let later = t0() + chrono::Duration::seconds(5);
        st.touch_agent_activity_locked(&a, later);
        assert_eq!(a.lock().last_activity, later);
        assert_eq!(r.lock().last_activity, later);
        st.touch_agent_activity_locked(&a, t0());
        assert_eq!(a.lock().last_activity, later);
        assert_eq!(
            st.agent_stack[0].lock().last_activity,
            DateTime::<Utc>::default()
        );
    }

    #[test]
    fn error_keys_sort_by_bare_key_then_seq() {
        let inv = InvocationTrace::new_with_span(None, "", t0());
        let mut st = inv.mu.lock();
        let rec = |bare: &str, seq: u64| RecordedError {
            err: Arc::new(Error::Context(ContextError::Canceled)),
            cause: None,
            span_id: String::new(),
            bare_key: bare.into(),
            seq,
            canceled: false,
            collateral: false,
        };
        st.unrecovered_errors.insert("k3".into(), rec("b", 1));
        st.unrecovered_errors.insert("k2".into(), rec("a", 2));
        st.unrecovered_errors.insert("k1".into(), rec("a", 2));
        st.unrecovered_errors.insert("k0".into(), rec("a", 5));
        assert_eq!(st.sorted_error_keys_locked(), vec!["k1", "k2", "k0", "k3"]);
        assert_eq!(st.next_seq_locked(), 1);
        assert_eq!(st.next_seq_locked(), 2);
    }

    #[test]
    fn open_call_owners_skip_streamed_and_ignored_calls() {
        let inv = InvocationTrace::new_with_span(None, "", t0());
        let mut guard = inv.mu.lock();
        let st = &mut *guard;
        let a = push(st, None, "a", "", "", &[], false);
        let b = push(st, None, "b", "", "", &[], false);
        let streamed = st.new_child(&a, SpanKind::Llm, t0());
        streamed.lock().streamed = true;
        st.llm_spans.insert("m".into(), streamed);
        let tool = st.new_child(&b, SpanKind::Tool, t0());
        tool.lock().handed_over = true;
        st.tool_spans.insert("t".into(), tool);
        assert_eq!(
            st.open_call_owners_locked(None),
            HashSet::from([span_id(&b)])
        );
        assert_eq!(st.agent_with_open_call_locked(None), Some(2));
        let ignore_tools = |r: &SpanRecord| r.kind == SpanKind::Tool;
        assert_eq!(st.agent_with_open_call_locked(Some(&ignore_tools)), None);
        inv.sync_open_calls_locked(st);
        assert_eq!(inv.open_calls.load(Ordering::SeqCst), 2);
        assert_eq!(inv.open_handed_over.load(Ordering::SeqCst), 1);
        assert!(inv.has_open_calls());
        inv.sync_open_agents_locked(st);
        assert!(inv.has_open_late_spans());
    }

    #[test]
    fn last_partial_clears_under_its_author_or_after_it_ended() {
        let inv = InvocationTrace::new_with_span(None, "", t0());
        let mut st = inv.mu.lock();
        let author = push(&mut st, None, "author", "", "", &[], false);
        let sibling = push(&mut st, None, "sibling", "", "", &[], false);
        st.last_event_partial = true;
        st.last_partial_span_id = span_id(&author);
        st.clear_last_partial_under_locked(&sibling);
        assert!(st.last_event_partial);
        let child = push(&mut st, Some(&author), "child", "", "", &[], false);
        st.clear_last_partial_under_locked(&child);
        assert!(!st.last_event_partial && st.last_partial_span_id.is_empty());
        st.last_event_partial = true;
        st.last_partial_span_id = "gone".into();
        st.clear_last_partial_under_locked(&sibling);
        assert!(!st.last_event_partial);
        st.last_session_event_partial = true;
        st.partial_lineage.insert("x".into());
        st.clear_last_partial_locked();
        assert!(!st.last_session_event_partial && st.partial_lineage.is_empty());
    }

    #[test]
    fn retired_late_contexts_keep_reachable_parents() {
        let inv = InvocationTrace::new_with_span(None, "", t0());
        let mut st = inv.mu.lock();
        let kept = push(&mut st, None, "kept", "", "", &[], false);
        let lone = push(&mut st, None, "lone", "", "", &[], false);
        push(&mut st, Some(&kept), "child", "", "", &[], false);
        // `kept` and `lone` retire; `child` stays open under `kept`.
        st.agent_stack
            .retain(|r| !r.ptr_eq(&kept) && !r.ptr_eq(&lone));
        kept.lock().activation = adk_ctx(&custom("k", vec![]), "inv", None)
            .activation_id()
            .into();
        st.drop_retired_late_contexts_locked(&[kept.clone(), lone.clone()]);
        assert!(kept.lock().activation.is_none());
        assert!(kept.lock().parent.is_some());
        assert!(lone.lock().parent.is_none());
    }

    #[test]
    fn agent_tree_edges_and_kinds() {
        let leaf = custom("leaf", vec![]);
        let chat = llm("chat", vec![leaf.clone()], Mode::Chat);
        let par = parallel("par", vec![custom("x", vec![])]);
        let root: Arc<dyn Agent> = sequential("root", vec![chat.clone(), par.clone()]);
        let edges = walk_agent_edges(Some(&root));
        let edge = |c: &str, p: &str, n: bool| AgentEdge {
            child: c.into(),
            parent: p.into(),
            non_llm: n,
        };
        assert_eq!(
            edges,
            vec![
                edge("chat", "root", true),
                edge("leaf", "chat", false),
                edge("par", "root", true),
                edge("x", "par", true),
            ]
        );
        assert!(walk_agent_edges(None).is_empty());
        assert!(is_llm_agent(&*chat) && !is_llm_agent(&*root));
        assert_eq!(agent_type_of(&*par), "ParallelAgent");
        assert_eq!(agent_type_of(&*leaf), "CustomAgent");
        assert!(is_built_in_workflow_agent(&*root) && is_built_in_workflow_agent(&*par));
        assert!(!is_built_in_workflow_agent(&*chat) && !is_built_in_workflow_agent(&*leaf));

        let pm = ParentMap::new(&root).unwrap();
        let entries: Vec<String> = parent_map_entries(&pm)
            .into_iter()
            .map(|(c, p)| format!("{c}<{}", p.name()))
            .collect();
        assert_eq!(entries, vec!["chat<root", "leaf<chat", "par<root", "x<par"]);
        let top = top_agent_from_map(Some(&leaf), "", &pm).unwrap();
        assert!(same_agent(&top, &root));
        assert!(same_agent(
            &top_agent_from_map(None, "leaf", &pm).unwrap(),
            &root
        ));
        assert!(same_agent(
            &top_agent_from_map(None, "", &pm).unwrap(),
            &root
        ));
        assert!(top_agent_from_map(None, "", &ParentMap::default()).is_none());
    }

    #[test]
    fn root_agent_from_the_runner_the_context_or_the_branch() {
        let leaf = custom("leaf", vec![]);
        let root: Arc<dyn Agent> = sequential("root", vec![leaf.clone()]);
        // The context's parent map is authoritative.
        let ctx = CallbackCtx::from_context(&adk_ctx(&leaf, "inv", Some(&root)));
        let inv = InvocationTrace::new_with_span(None, "", t0());
        let id = ident("inv", "leaf", "root.leaf", "");
        let input = prepare_root_agent_input(&ctx, &inv, &id, None, None);
        assert!(input.has_context_parents);
        assert_eq!(input.context_root_name, "root");
        let mut st = inv.mu.lock();
        inv.note_root_agent_locked(&mut st, &id, &input, true);
        assert_eq!(st.root_agent_name, "root");
        assert!(st.ancestry_from_context);
        assert!(inv.ancestry_from_context_atomic.load(Ordering::SeqCst));
        assert!(st.is_ancestor_agent("root", "leaf"));
        assert_eq!(
            st.sequential_parent.get("leaf").map(String::as_str),
            Some("root")
        );
        drop(st);
        assert!(!prepare_root_agent_input(&ctx, &inv, &id, None, None).has_context_parents);

        // The runner's agent, and a remembered ancestor of it.
        let inv = InvocationTrace::new_with_span(None, "", t0());
        let bg = CallbackCtx::background();
        let input = prepare_root_agent_input(&bg, &inv, &id, Some(&leaf), Some(&root));
        assert_eq!(input.remembered_name, "root");
        let mut st = inv.mu.lock();
        inv.note_root_agent_locked(&mut st, &id, &input, false);
        assert_eq!(st.root_agent_name, "root");
        assert_eq!(st.runner_agent_name, "root");
        drop(st);
        assert_eq!(inv.saved_identity().root_agent_name, "root");

        // No tree: the branch's first element, or the agent's own name.
        let inv = InvocationTrace::new_with_span(None, "", t0());
        let mut st = inv.mu.lock();
        inv.note_root_agent_locked(&mut st, &id, &RootAgentInput::default(), true);
        assert_eq!(st.root_agent_name, "root");
        let inv = InvocationTrace::new_with_span(None, "", t0());
        let mut st = inv.mu.lock();
        inv.note_root_agent_locked(&mut st, &id, &RootAgentInput::default(), false);
        assert_eq!(st.root_agent_name, "leaf");
    }
}
