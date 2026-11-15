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

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};
use gadk_agent::fiber::SCHEDULER_CREATOR;
use gadk_agent::{Agent, go_quote};
use gadk_engine::telemetry::d6::SpanIds;
use parking_lot::Mutex;
use sha2::{Digest, Sha256};

use super::invocation::{InvocationTrace, prepare_root_agent_input, same_agent, unix_nanos};
use super::span::{
    AttemptSite, NodeRunKey, RowSpan, SpanKind, SpanRecord, SpanRef, UnclosedAgentSpan, latency_of,
};
use super::{
    AgentKey, CallbackCtx, Clock, INVOCATION_TTL, Identity, LogFn, MAX_TRACKED_INVOCATIONS,
    NO_FRAME, SWEEP_INTERVAL, hex, llm_key, new_span_id, new_trace_id,
};

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
        if self
            .live_pending
            .as_ref()
            .is_some_and(|p| p.load(Ordering::SeqCst) > 0)
        {
            return true;
        }
        self.late.as_ref().is_some_and(|l| l.has_open_late_spans())
    }

    /// Go `hasPendingSpans`.
    pub(crate) fn has_pending_spans(&self) -> bool {
        if self
            .live_pending
            .as_ref()
            .is_some_and(|p| p.load(Ordering::SeqCst) > 0)
        {
            return true;
        }
        self.late
            .as_ref()
            .is_some_and(|l| l.pending_spans.load(Ordering::SeqCst) > 0)
    }

    /// Go `waitsForLateCalls`: kept only for calls AfterRun handed over to the late state
    /// while one is open, and no callback is starting a span in it.
    pub(crate) fn waits_for_late_calls(&self) -> bool {
        !self.has_pending_spans()
            && self
                .late
                .as_ref()
                .is_some_and(|l| l.open_handed_over.load(Ordering::SeqCst) > 0)
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
        TraceManager {
            mu: Mutex::new(ManagerState::default()),
            trace_seed: new_trace_id(),
            logf: None,
            max_tracked: MAX_TRACKED_INVOCATIONS,
            ttl: INVOCATION_TTL,
            now: Arc::new(Utc::now),
        }
    }

    /// Go `traceIDFor(invocationID)`: the first 16 bytes of SHA-256(`seed \x00 id`) as hex, or
    /// a new trace ID for an empty ID.
    pub(crate) fn trace_id_for(&self, invocation_id: &str) -> String {
        if invocation_id.is_empty() {
            return new_trace_id();
        }
        let sum = Sha256::digest(format!("{}\x00{invocation_id}", self.trace_seed).as_bytes());
        hex(&sum[..16])
    }

    /// Go `rememberAgentTree(runner)`: records `runner`'s sub-agent links and returns the
    /// highest known ancestor of `runner` (`None` for no runner). The sub-agents are walked
    /// before `mu` is taken (agent code never runs under a lock).
    pub(crate) fn remember_agent_tree(
        &self,
        runner: Option<&Arc<dyn Agent>>,
    ) -> Option<Arc<dyn Agent>> {
        fn walk(a: &Arc<dyn Agent>, visited: &mut HashSet<AgentKey>, edges: &mut Vec<KnownParent>) {
            if !visited.insert(AgentKey::of(a)) {
                return;
            }
            for sub in a.sub_agents() {
                edges.push(KnownParent {
                    child: sub.clone(),
                    parent: a.clone(),
                });
                walk(sub, visited, edges);
            }
        }
        let runner = runner?;
        let mut edges = Vec::new();
        walk(runner, &mut HashSet::new(), &mut edges);

        let mut st = self.mu.lock();
        if !edges.is_empty() {
            if st.known_parent.len() + edges.len() > MAX_TRACKED_INVOCATIONS {
                st.known_parent.clear();
            }
            for e in edges {
                st.known_parent.insert(AgentKey::of(&e.child), e);
            }
        }
        let mut cur = runner.clone();
        for _ in 0..=st.known_parent.len() {
            match st.known_parent.get(&AgentKey::of(&cur)) {
                Some(next) if !same_agent(&next.parent, &cur) => cur = next.parent.clone(),
                _ => return Some(cur),
            }
        }
        Some(cur)
    }

    /// Go `invocation(ctx, id)` = `lookup_invocation(ctx, id, false, false)`.
    pub(crate) fn invocation(&self, ctx: &CallbackCtx, id: &Identity) -> Arc<InvocationTrace> {
        self.lookup_invocation(ctx, id, false, false)
    }

    /// Go `lookupInvocation(ctx, id, start, reserveSpan)`: the trace of `id`'s invocation,
    /// made on its first callback. Without an invocation ID the trace is untracked. A finished
    /// run's later callbacks get its late state, unless `start` begins a new run with the ID
    /// (a HITL resume), which drops the completed entry. `reserve_span` adds a
    /// `pending_spans` reservation the caller releases.
    pub(crate) fn lookup_invocation(
        &self,
        ctx: &CallbackCtx,
        id: &Identity,
        start: bool,
        reserve_span: bool,
    ) -> Arc<InvocationTrace> {
        let now = (self.now)();
        let sc = ctx.ambient_span_context();
        if id.invocation_id.is_empty() {
            let inv = Arc::new(InvocationTrace::new_with_span(sc, "", now));
            if reserve_span {
                inv.pending_spans.fetch_add(1, Ordering::SeqCst);
            }
            return inv;
        }
        let mut warn_ids = Vec::new();
        let mut max_tracked = 0;
        let mut logf = None;
        let mut guard = self.mu.lock();
        let st = &mut *guard;
        let inv = match st.invocations.get(&id.invocation_id).cloned() {
            Some(inv) => inv,
            None => {
                if !start && let Some(completed) = st.completed.get_mut(&id.invocation_id) {
                    let trace_id = self.trace_id_for(&id.invocation_id);
                    let late = self.late_locked(completed, sc, &trace_id, now);
                    late.last_active.store(unix_nanos(now), Ordering::SeqCst);
                    completed.generation = push_completion(
                        &mut st.completion_order,
                        &mut st.next_generation,
                        &id.invocation_id,
                    );
                    compact_completion_order(&mut st.completion_order, &st.completed);
                    if reserve_span {
                        late.pending_spans.fetch_add(1, Ordering::SeqCst);
                    }
                    return late;
                }
                st.completed.remove(&id.invocation_id);
                let mut inv =
                    InvocationTrace::new_with_span(sc, &self.trace_id_for(&id.invocation_id), now);
                if let Some(ev) = st.evicted.get(&id.invocation_id) {
                    inv.restore_evicted(ev);
                }
                let inv = Arc::new(inv);
                st.invocations.insert(id.invocation_id.clone(), inv.clone());
                st.inserts += 1;
                warn_ids = self.evict_locked(st, now, &id.invocation_id);
                max_tracked = self.max_tracked;
                logf = self.logf.clone();
                inv
            }
        };
        inv.last_active.store(unix_nanos(now), Ordering::SeqCst);
        if reserve_span {
            inv.pending_spans.fetch_add(1, Ordering::SeqCst);
        }
        drop(guard);
        if let Some(logf) = logf {
            for evicted_id in &warn_ids {
                logf(&format!(
                    "agentanalytics: trace manager capacity ({max_tracked}) exceeded; evicting active invocation {}",
                    go_quote(evicted_id)
                ));
            }
        }
        inv
    }

    /// Go's `now.Add(-tm.ttl).UnixNano()`.
    fn cutoff(&self, now: DateTime<Utc>) -> i64 {
        chrono::Duration::from_std(self.ttl)
            .ok()
            .and_then(|ttl| now.checked_sub_signed(ttl))
            .map_or(i64::MIN, unix_nanos)
    }

    /// Go `evictLocked(now, keep)`: drops invocations idle past the TTL without open calls
    /// (every `SWEEP_INTERVAL` insertions or when too many are tracked), then the least
    /// recently active ones while too many remain; never `keep`. Returns the IDs of active
    /// invocations evicted for capacity (logged after `mu` is released).
    pub(crate) fn evict_locked(
        &self,
        st: &mut ManagerState,
        now: DateTime<Utc>,
        keep: &str,
    ) -> Vec<String> {
        let cutoff = self.cutoff(now);
        if st.inserts.is_multiple_of(SWEEP_INTERVAL) || st.invocations.len() > self.max_tracked {
            let expired: Vec<(String, Arc<InvocationTrace>)> = st
                .invocations
                .iter()
                .filter(|(id, inv)| {
                    id.as_str() != keep
                        && inv.last_active.load(Ordering::SeqCst) < cutoff
                        && !inv.has_open_calls()
                })
                .map(|(id, inv)| (id.clone(), inv.clone()))
                .collect();
            for (id, inv) in expired {
                self.mark_evicted_locked(st, now, &id, Some(&inv));
                st.invocations.remove(&id);
            }
        }
        let mut warn_ids = Vec::new();
        while st.invocations.len() > self.max_tracked {
            let mut oldest: Option<(String, i64)> = None;
            for (id, inv) in &st.invocations {
                let t = inv.last_active.load(Ordering::SeqCst);
                if id != keep && oldest.as_ref().is_none_or(|(_, o)| t < *o) {
                    oldest = Some((id.clone(), t));
                }
            }
            let Some((oldest_id, oldest_t)) = oldest else {
                break;
            };
            let Some(evicted_inv) = st.invocations.get(&oldest_id).cloned() else {
                break;
            };
            if oldest_t >= cutoff || evicted_inv.has_open_calls() {
                warn_ids.push(oldest_id.clone());
            }
            self.mark_evicted_locked(st, now, &oldest_id, Some(&evicted_inv));
            st.invocations.remove(&oldest_id);
        }
        warn_ids
    }

    /// Go `markEvictedLocked(now, invocationID, inv)`: saves the identity of an evicted
    /// invocation, within a capacity of `max(max_tracked, 64)` (expired, then oldest first).
    pub(crate) fn mark_evicted_locked(
        &self,
        st: &mut ManagerState,
        now: DateTime<Utc>,
        invocation_id: &str,
        inv: Option<&InvocationTrace>,
    ) {
        let limit = self.max_tracked.max(64);
        if !st.evicted.contains_key(invocation_id) && st.evicted.len() >= limit {
            let cutoff = self.cutoff(now);
            st.evicted.retain(|_, ev| ev.last_active >= cutoff);
            while st.evicted.len() >= limit {
                let oldest = st
                    .evicted
                    .iter()
                    .min_by_key(|(_, ev)| ev.last_active)
                    .map(|(id, _)| id.clone());
                let Some(oldest) = oldest else {
                    break;
                };
                st.evicted.remove(&oldest);
            }
        }
        let ev = match inv {
            Some(inv) => inv.saved_identity(),
            None => EvictedTrace {
                last_active: unix_nanos(now),
                ..EvictedTrace::default()
            },
        };
        st.evicted.insert(invocation_id.to_string(), ev);
    }

    /// Go `rememberCompletedLocked(invocationID, runner, inv)`: moves a finished run to its
    /// completed entry. The model and tool calls it left open and may still end (not streamed
    /// model calls, nor calls of an activation that ended with the run, see
    /// [`SpanRecord::ended_with_run`]) move to the late state with the calls an error callback
    /// ended; `inv` is marked finished and drops all its calls. Then prunes completed runs
    /// beyond `max(max_tracked, 64)`.
    pub(crate) fn remember_completed_locked(
        &self,
        st: &mut ManagerState,
        invocation_id: &str,
        runner: &str,
        inv: &Arc<InvocationTrace>,
    ) {
        let generation = push_completion(
            &mut st.completion_order,
            &mut st.next_generation,
            invocation_id,
        );
        let completed = st.completed.entry(invocation_id.to_string()).or_default();
        completed.generation = generation;
        completed.identity = inv.saved_identity();
        if inv.pending_spans.load(Ordering::SeqCst) > 0 {
            completed.live_pending = Some(inv.pending_spans.clone());
        } else if completed
            .live_pending
            .as_ref()
            .is_some_and(|p| p.load(Ordering::SeqCst) == 0)
        {
            completed.live_pending = None;
        }
        let (last_seq, models, ended_models, tools, ended_tools) = {
            let mut guard = inv.mu.lock();
            let ist = &mut *guard;
            ist.finished = true;
            if let Some(root) = ist.agent_stack.first() {
                root.lock().drop_contexts();
            }
            ist.runner_agent = None;
            let last_seq = ist.last_seq;
            let mut models = started_calls(&ist.llm_spans);
            let ended_models = started_calls(&ist.completed_models);
            if let Some(models) = models.as_mut() {
                models.retain(|_, rec| {
                    let rec = rec.lock();
                    !(rec.streamed || rec.ended_with_run(runner))
                });
            }
            let mut tools = started_calls(&ist.tool_spans);
            let ended_tools = started_calls(&ist.completed_tools);
            if let Some(tools) = tools.as_mut() {
                tools.retain(|_, rec| !rec.lock().ended_with_run(runner));
            }
            // The late state keeps no context of the run's callbacks; the open calls are
            // handed over (see `CompletedTrace::waits_for_late_calls`).
            for rec in models.iter().flatten().chain(tools.iter().flatten()) {
                let mut rec = rec.1.lock();
                rec.drop_contexts();
                rec.handed_over = true;
            }
            // No record stays reachable from both `inv` and the late state.
            ist.llm_spans.clear();
            ist.completed_models.clear();
            ist.tool_spans.clear();
            ist.completed_tools.clear();
            inv.sync_open_calls_locked(ist);
            (last_seq, models, ended_models, tools, ended_tools)
        };
        let open = models.as_ref().map_or(0, HashMap::len) + tools.as_ref().map_or(0, HashMap::len);
        if open > 0 {
            let trace_id = completed.identity.trace_id.clone();
            self.late_locked(completed, None, &trace_id, (self.now)());
        }
        if let Some(late) = completed.late.clone() {
            let mut guard = late.mu.lock();
            let lst = &mut *guard;
            lst.last_seq = lst.last_seq.max(last_seq);
            lst.llm_spans.extend(models.unwrap_or_default());
            lst.completed_models
                .extend(ended_models.unwrap_or_default());
            lst.tool_spans.extend(tools.unwrap_or_default());
            lst.completed_tools.extend(ended_tools.unwrap_or_default());
            late.sync_open_calls_locked(lst);
        } else {
            completed.ended_models = with_calls(completed.ended_models.take(), ended_models);
            completed.ended_tools = with_calls(completed.ended_tools.take(), ended_tools);
        }
        compact_completion_order(&mut st.completion_order, &st.completed);
        let limit = self.max_tracked.max(64);
        self.prune_waiting_locked(st, limit, limit / 2);
        self.prune_completed_locked(st, limit, &|c| !c.has_pending_or_open_late_spans());
        self.prune_completed_locked(st, limit, &|c| !c.has_pending_spans());
    }

    /// Go `lateLocked(completed, sc, traceID, now)`: the late state of `completed`, made (with
    /// the ended calls `completed` holds) if it does not exist yet.
    pub(crate) fn late_locked(
        &self,
        completed: &mut CompletedTrace,
        sc: Option<SpanIds>,
        trace_id: &str,
        now: DateTime<Utc>,
    ) -> Arc<InvocationTrace> {
        if completed
            .live_pending
            .as_ref()
            .is_some_and(|p| p.load(Ordering::SeqCst) == 0)
        {
            completed.live_pending = None;
        }
        if let Some(late) = &completed.late {
            return late.clone();
        }
        let mut late = InvocationTrace::new_with_span(sc, trace_id, now);
        late.restore_evicted(&completed.identity);
        {
            let st = late.mu.get_mut();
            st.is_late = true;
            st.completed_models
                .extend(completed.ended_models.take().unwrap_or_default());
            st.completed_tools
                .extend(completed.ended_tools.take().unwrap_or_default());
        }
        let late = Arc::new(late);
        completed.late = Some(late.clone());
        late
    }

    /// Go `pruneCompletedLocked(limit, drop)`: drops the least recently active completed runs
    /// for which `drop` reports true until at most `limit` remain.
    pub(crate) fn prune_completed_locked(
        &self,
        st: &mut ManagerState,
        limit: usize,
        drop: &dyn Fn(&CompletedTrace) -> bool,
    ) {
        let mut i = 0;
        while st.completed.len() > limit && i < st.completion_order.len() {
            let (id, generation) = st.completion_order[i].clone();
            match st.completed.get(&id) {
                Some(c) if c.generation == generation => {
                    if drop(c) {
                        st.completed.remove(&id);
                        st.completion_order.remove(i);
                    } else {
                        i += 1;
                    }
                }
                // A stale entry (moved to the back, or its run dropped).
                _ => {
                    st.completion_order.remove(i);
                }
            }
        }
    }

    /// Go `pruneWaitingLocked(limit, keep)`: while more than `limit` completed runs remain,
    /// drops the least recently active runs that wait for late calls, as long as more than
    /// `keep` of them remain.
    pub(crate) fn prune_waiting_locked(&self, st: &mut ManagerState, limit: usize, keep: usize) {
        if st.completed.len() <= limit {
            return;
        }
        let mut waiting = st
            .completed
            .values()
            .filter(|c| c.waits_for_late_calls())
            .count();
        let mut i = 0;
        while st.completed.len() > limit && waiting > keep && i < st.completion_order.len() {
            let (id, generation) = st.completion_order[i].clone();
            match st.completed.get(&id) {
                Some(c) if c.generation == generation => {
                    if c.waits_for_late_calls() {
                        st.completed.remove(&id);
                        st.completion_order.remove(i);
                        waiting -= 1;
                    } else {
                        i += 1;
                    }
                }
                _ => {
                    st.completion_order.remove(i);
                }
            }
        }
    }

    /// Go `invocationSpan(ctx, id, runner)`: the place of `USER_MESSAGE_RECEIVED` /
    /// `INVOCATION_STARTING` (starts tracking; `runner` gives the root agent).
    pub(crate) fn invocation_span(
        &self,
        ctx: &CallbackCtx,
        id: &Identity,
        runner: Option<&Arc<dyn Agent>>,
    ) -> RowSpan {
        let inv = self.lookup_invocation(ctx, id, true, false);
        let now = (self.now)();
        let remembered_root = self.remember_agent_tree(runner);
        let ambient = ctx.ambient_trace_id();
        let root_in = prepare_root_agent_input(ctx, &inv, id, runner, remembered_root.as_ref());
        let mut guard = inv.mu.lock();
        let st = &mut *guard;
        st.started = true;
        inv.note_root_agent_locked(st, id, &root_in, false);
        let root = st.agent_stack[0].clone();
        let mut rec = root.lock();
        rec.canceled = ctx.canceled_func();
        let mut span = inv.row_span(st, &rec, None, &ambient);
        span.timestamp = Some(now);
        span
    }

    /// Go `endInvocation(ctx, id)`: the place of `INVOCATION_COMPLETED` / `_ERROR`, with the
    /// invocation latency if it was tracked from its start without eviction; moves the run to
    /// its completed entry.
    pub(crate) fn end_invocation(&self, ctx: &CallbackCtx, id: &Identity) -> RowSpan {
        let now = (self.now)();
        let sc = ctx.ambient_span_context();
        let ambient = ctx.ambient_trace_id();
        let mut inv = None;
        if !id.invocation_id.is_empty() {
            let st = self.mu.lock();
            inv = Some(match st.invocations.get(&id.invocation_id) {
                Some(inv) => inv.clone(),
                None => {
                    let mut t = InvocationTrace::new_with_span(
                        sc,
                        &self.trace_id_for(&id.invocation_id),
                        now,
                    );
                    if let Some(ev) = st.evicted.get(&id.invocation_id) {
                        t.restore_evicted(ev);
                    } else if let Some(completed) = st.completed.get(&id.invocation_id) {
                        t.restore_evicted(&completed.identity);
                    }
                    Arc::new(t)
                }
            });
        }
        let inv = inv.unwrap_or_else(|| {
            Arc::new(InvocationTrace::new_with_span(
                sc,
                &self.trace_id_for(&id.invocation_id),
                now,
            ))
        });
        let root_in = prepare_root_agent_input(ctx, &inv, id, None, None);
        let row = {
            let mut guard = inv.mu.lock();
            let st = &mut *guard;
            inv.note_root_agent_locked(st, id, &root_in, false);
            let root = st.agent_stack[0].lock();
            let latency = (st.started && !st.evicted).then(|| latency_of(&root, now));
            let mut row = inv.row_span(st, &root, latency, &ambient);
            row.timestamp = Some(now);
            row
        };
        if !id.invocation_id.is_empty() {
            let mut guard = self.mu.lock();
            let st = &mut *guard;
            // Publish the final identity (with any adopted trace) before dropping the live
            // state.
            self.remember_completed_locked(st, &id.invocation_id, &id.agent_name, &inv);
            st.evicted.remove(&id.invocation_id);
            st.invocations.remove(&id.invocation_id);
        }
        row
    }

    /// Go `agentNamed(ctx, id)`: the agent named `id.agent_name` in the runner's tree
    /// (`find_agent` runs agent code, so no lock is held).
    pub(crate) fn agent_named(&self, ctx: &CallbackCtx, id: &Identity) -> Option<Arc<dyn Agent>> {
        let mut inv = self.invocation(ctx, id);
        let root_in = prepare_root_agent_input(ctx, &inv, id, None, None);
        let runner = loop {
            if let Some(mut guard) = inv.lock_live() {
                let st = &mut *guard;
                inv.note_root_agent_locked(st, id, &root_in, true);
                break st.runner_agent.clone();
            }
            inv = self.invocation(ctx, id);
        };
        runner?.find_agent(&id.agent_name)
    }

    /// Go `pushAgentSpan(ctx, id)`: the place of `AGENT_STARTING` and the activations purged
    /// as failed earlier attempts (it calls
    /// `settle_ended_in_subtree_locked`, `failures_locked`, `reported_over_locked` and
    /// `purge_activation_locked` of [`super::settle`]).
    ///
    /// Go reads the goroutine of the callback (`currentGoroutine`) when the context shows its
    /// activation: here the callback's fiber, its origin and the engine's attempt site
    /// (`super::settle::current_goroutine`).
    pub(crate) fn push_agent_span(
        &self,
        ctx: &CallbackCtx,
        id: &Identity,
    ) -> (RowSpan, Vec<UnclosedAgentSpan>) {
        let inv = self.lookup_invocation(ctx, id, false, true);
        let _reservation = Reservation(inv.clone());
        let now = (self.now)();
        let ambient = ctx.ambient_trace_id();
        // `sub_agents()` is agent code, so it is read before `mu` is taken.
        let children = ctx.running_agent_children(&id.agent_name);
        let root_in = prepare_root_agent_input(ctx, &inv, id, None, None);
        let activation = ctx.activation_key();
        let ended = ctx.ended_func();
        // Go `if ended != nil { goid, origin, attempt = currentGoroutine() }`.
        let (goid, origin, attempt) = super::settle::current_goroutine(ctx);
        let same_goroutine_since = if attempt == AttemptSite::None {
            NO_FRAME
        } else {
            0
        };
        // A run that finished in between hands the agent to its late state: look it up again
        // (the first reservation is held until this call returns).
        let Some(mut guard) = inv.lock_live() else {
            return self.push_agent_span(ctx, id);
        };
        let st = &mut *guard;
        st.started = true;
        inv.note_root_agent_locked(st, id, &root_in, true);
        let mut parent = st.parent_for_run_locked(id, goid, &origin);
        let mut concurrent_peer = false;
        // Go `laterAttempt(other)`: whether `id`'s activation is a later attempt of the
        // workflow node `other` ran as its agent, and whether `other`'s run is known to have
        // returned. Locks `other`'s parent (the caller holds `other`).
        let later_attempt = |other: &SpanRecord| -> (bool, bool) {
            let Some(op_ref) = other.parent.as_ref() else {
                return (false, false);
            };
            let op = op_ref.lock();
            if !super::invocation::is_sub_node_path(&id.path, &op.path)
                || (activation.is_some() && other.activation == activation)
            {
                return (false, false);
            }
            // The synthetic-retry fallback: no activation on any span of the chain (direct
            // trace-manager tests only).
            if activation.is_none() && other.activation.is_none() && op.activation.is_none() {
                return (true, true);
            }
            if !op.is_workflow {
                return (false, false);
            }
            if attempt == AttemptSite::Worker
                && other.attempt == AttemptSite::Worker
                && goid != 0
                && other.goroutine_id == goid
            {
                return (true, true);
            }
            if attempt == AttemptSite::Node
                && other.attempt == AttemptSite::Node
                && other.node_run_ended
                && origin.started_by(SCHEDULER_CREATOR, op.goroutine_id)
                && other.origin.started_by(SCHEDULER_CREATOR, op.goroutine_id)
            {
                return (true, false);
            }
            (false, false)
        };
        let matches_run = |other: &SpanRecord, parent: &SpanRef| {
            other.kind == SpanKind::Agent
                && other.parent.as_ref().is_some_and(|p| p.ptr_eq(parent))
                && other.name == id.agent_name
                && other.branch == id.branch
                && other.path == id.path
        };
        // The scheduler starts a node again when RetryConfig retries it or a running node is
        // triggered again.
        let restarted = attempt == AttemptSite::Node && {
            let p = parent.lock();
            p.is_workflow && origin.started_by(SCHEDULER_CREATOR, p.goroutine_id)
        };
        // Register the concurrent peers that are not earlier attempts before the retry loop.
        for other_ref in st.agent_stack.iter().skip(1) {
            let mut other = other_ref.lock();
            if !matches_run(&other, &parent) {
                continue;
            }
            if restarted && other.attempt == AttemptSite::Node && other.goroutine_id != goid {
                let p = parent.lock();
                if other.origin.started_by(SCHEDULER_CREATOR, p.goroutine_id) {
                    drop(p);
                    other.started_again = true;
                }
            }
            if later_attempt(&other).0 {
                continue;
            }
            other.concurrent_peer = true;
            concurrent_peer = true;
        }
        // The workflow-retry completion-inference loop.
        let mut purged = Vec::new();
        for i in (1..st.agent_stack.len()).rev() {
            let (later, synced, rec_concurrent_peer, rec_parent) = {
                let rec = st.agent_stack[i].lock();
                if rec.kind != SpanKind::Agent
                    || rec.name != id.agent_name
                    || rec.branch != id.branch
                    || rec.path != id.path
                {
                    continue;
                }
                let (later, synced) = later_attempt(&rec);
                (later, synced, rec.concurrent_peer, rec.parent.clone())
            };
            if !later || rec_concurrent_peer {
                continue;
            }
            let outside = st.parent_outside_locked(id, &st.subtree_locked(i));
            if !outside.is(rec_parent.as_ref()) {
                continue;
            }
            if synced {
                // The earlier attempt's run returned.
                inv.settle_ended_in_subtree_locked(st, i, true, same_goroutine_since);
            }
            let mut failures = st.failures_locked(i, false, None, synced);
            if !failures.is_empty() {
                if !synced {
                    let over = st.reported_over_locked(i);
                    failures = st.failures_locked(i, false, Some(&over), false);
                }
                purged = inv.purge_activation_locked(st, i, &failures, now, &ambient);
                break;
            }
        }
        // Mark the concurrent peers among the remaining matching spans.
        parent = st.parent_for_run_locked(id, goid, &origin);
        for other_ref in st.agent_stack.iter().skip(1) {
            let mut other = other_ref.lock();
            if matches_run(&other, &parent) {
                other.concurrent_peer = true;
                concurrent_peer = true;
            }
        }
        let (parent_span_id, stop_point, parent_is_agent) = {
            let p = parent.lock();
            (
                p.span_id.clone(),
                p.kind == SpanKind::Invocation || id.path != p.path || id.branch != p.branch,
                p.kind == SpanKind::Agent,
            )
        };
        let seq = st.next_seq_locked();
        let rec = SpanRef::new(SpanRecord {
            span_id: new_span_id(),
            parent_span_id,
            kind: SpanKind::Agent,
            path: id.path.clone(),
            name: id.agent_name.clone(),
            branch: id.branch.clone(),
            start: now,
            last_activity: now,
            canceled: ctx.canceled_func(),
            ended,
            goroutine_id: goid,
            origin,
            attempt,
            activation,
            parent: Some(parent.clone()),
            stop_point,
            non_llm: children.non_llm,
            propagates_child_error: children.propagates_child_error,
            is_parallel: children.is_parallel,
            is_workflow: children.is_workflow,
            concurrent_peer,
            task_mode: children.task_mode,
            children: children.names,
            seq,
            ..SpanRecord::default()
        });
        st.agent_stack.push(rec.clone());
        {
            let r = rec.lock();
            let mut p = parent.lock();
            if r.is_node_agent_of(&p) {
                *p.node_runs
                    .entry(NodeRunKey {
                        path: r.path.clone(),
                        branch: r.branch.clone(),
                    })
                    .or_insert(0) += 1;
            }
        }
        inv.sync_open_agents_locked(st);
        st.clear_last_partial_under_locked(&rec);
        if parent_is_agent {
            st.touch_agent_activity_locked(&parent, now);
        }
        let mut span = inv.row_span(st, &rec.lock(), None, &ambient);
        span.timestamp = Some(now);
        (span, purged)
    }

    /// Go `pushLLMSpan(ctx, id)`: the place of `LLM_REQUEST`. A model call of the same agent
    /// that never ended (a BeforeModelCallback answered it) is replaced.
    pub(crate) fn push_llm_span(&self, ctx: &CallbackCtx, id: &Identity) -> RowSpan {
        let inv = self.lookup_invocation(ctx, id, false, true);
        let _reservation = Reservation(inv.clone());
        let now = (self.now)();
        let ambient = ctx.ambient_trace_id();
        let root_in = prepare_root_agent_input(ctx, &inv, id, None, None);
        let Some(mut guard) = inv.lock_live() else {
            return self.push_llm_span(ctx, id);
        };
        let st = &mut *guard;
        st.started = true;
        inv.note_root_agent_locked(st, id, &root_in, true);
        let key = ctx.call_key(&llm_key(id));
        let activation = ctx.activation_key();
        let owner = st.owner_of(activation, id);
        st.clear_last_partial_under_locked(&owner);
        let owner_is_agent = owner.lock().kind == SpanKind::Agent;
        if owner_is_agent {
            st.touch_agent_activity_locked(&owner, now);
            // A new model call clears stale pending-event credits.
            let mut o = owner.lock();
            o.awaiting_model_event = 0;
            o.awaiting_tool_event = 0;
        }
        let rec = st.new_child(&owner, SpanKind::Llm, now);
        {
            let mut r = rec.lock();
            r.canceled = ctx.canceled_func();
            r.call_started = true;
            r.own_activation = owner.lock().is_activation_of(activation);
        }
        st.llm_spans.insert(key.clone(), rec.clone());
        inv.sync_open_calls_locked(st);
        st.completed_models.remove(&key);
        let mut span = inv.row_span(st, &rec.lock(), None, &ambient);
        span.timestamp = Some(now);
        span
    }

    /// Go `recordFirstToken(ctx, id)`: the arrival time of the first streamed chunk of
    /// generated content of the agent's running model call.
    pub(crate) fn record_first_token(&self, ctx: &CallbackCtx, id: &Identity) {
        let mut inv = self.invocation(ctx, id);
        let now = (self.now)();
        loop {
            if let Some(guard) = inv.lock_live() {
                let rec = guard.llm_spans.get(&ctx.call_key(&llm_key(id))).cloned();
                if let Some(rec) = rec {
                    let first = {
                        let mut r = rec.lock();
                        let first = r.first_token.is_none();
                        if first {
                            r.first_token = Some(now);
                        }
                        first
                    };
                    if first {
                        let owner = guard.owner_of(ctx.activation_key(), id);
                        let owner_is_agent = owner.lock().kind == SpanKind::Agent;
                        if owner_is_agent {
                            guard.touch_agent_activity_locked(&owner, now);
                        }
                    }
                }
                return;
            }
            inv = self.invocation(ctx, id);
        }
    }

    /// Go `recordStreamed(ctx, id, author, branch, withContent)`: a partial session event of
    /// `author` marks its running model calls as streaming and its lineage as partial (and as
    /// responded with content).
    pub(crate) fn record_streamed(
        &self,
        ctx: &CallbackCtx,
        id: &Identity,
        author: &str,
        branch: &str,
        with_content: bool,
    ) {
        let inv = self.invocation(ctx, id);
        let now = (self.now)();
        let mut guard = inv.mu.lock();
        let st = &mut *guard;
        st.last_session_event_partial = true;
        st.last_event_partial = true;
        st.last_partial_span_id.clear();
        let call_id = Identity {
            agent_name: author.to_string(),
            branch: branch.to_string(),
            path: id.path.clone(),
            ..Identity::default()
        };
        if let Some(rec) = st.llm_spans.get(&llm_key(&call_id)) {
            rec.lock().streamed = true;
        }
        if let Some(i) = st.find_agent(&id.path, author, branch) {
            let rec = st.agent_stack[i].clone();
            let span_id = rec.lock().span_id.clone();
            for llm in st.llm_spans.values() {
                let mut llm = llm.lock();
                if llm.parent_span_id == span_id {
                    llm.streamed = true;
                }
            }
            st.last_partial_span_id = span_id;
            st.touch_agent_activity_locked(&rec, now);
            let lineage = st.lineage_locked(i);
            st.partial_lineage.extend(lineage.iter().cloned());
            if with_content {
                st.mark_responded_locked(&lineage);
            }
        }
    }

    /// Go `clearLastPartial(ctx, id)`: a non-partial session event without content arrived.
    pub(crate) fn clear_last_partial(&self, ctx: &CallbackCtx, id: &Identity) {
        let inv = self.invocation(ctx, id);
        inv.mu.lock().clear_last_partial_locked();
    }

    /// Go `popLLMSpan(ctx, id, responded)`: the place of `LLM_RESPONSE` / `LLM_ERROR` (calls
    /// `retire_late_settled_locked` of [`super::settle`], which returns at once for a live trace). With `responded`, a call that streamed nothing gets a time to first token equal
    /// to its latency; a call OnModelErrorCallback ended reuses its span without latency; an
    /// unknown call gets a span of its own.
    pub(crate) fn pop_llm_span(
        &self,
        ctx: &CallbackCtx,
        id: &Identity,
        responded: bool,
    ) -> RowSpan {
        let inv = self.invocation(ctx, id);
        let now = (self.now)();
        let ambient = ctx.ambient_trace_id();
        let root_in = prepare_root_agent_input(ctx, &inv, id, None, None);
        let Some(mut guard) = inv.lock_live() else {
            return self.pop_llm_span(ctx, id, responded);
        };
        let st = &mut *guard;
        inv.note_root_agent_locked(st, id, &root_in, true);
        let key = ctx.call_key(&llm_key(id));
        let activation = ctx.activation_key();
        let Some(rec) = st.llm_spans.get(&key).cloned() else {
            if responded && let Some(prev) = st.completed_models.remove(&key) {
                let owner = st.owner_of(activation, id);
                {
                    let mut o = owner.lock();
                    if o.kind == SpanKind::Agent {
                        o.has_response = true;
                        o.model_responded = true;
                        o.awaiting_model_event += 1;
                    }
                }
                let mut span = inv.row_span(st, &prev.lock(), None, &ambient);
                span.timestamp = Some(now);
                return span;
            }
            let owner = st.owner_of(activation, id);
            let child = st.new_child(&owner, SpanKind::Llm, now);
            let mut span = inv.row_span(st, &child.lock(), None, &ambient);
            span.timestamp = Some(now);
            return span;
        };
        st.llm_spans.remove(&key);
        inv.sync_open_calls_locked(st);
        rec.lock().drop_contexts();
        let owner = st.owner_of(activation, id);
        let owner_is_agent = owner.lock().kind == SpanKind::Agent;
        if owner_is_agent {
            st.touch_agent_activity_locked(&owner, now);
        }
        if !responded {
            // A call that did not respond leaves no pending model-event credit.
            st.completed_models.insert(key, rec.clone());
            let owner = st.owner_of(activation, id);
            let mut o = owner.lock();
            if o.kind == SpanKind::Agent {
                o.awaiting_model_event = 0;
            }
        } else {
            {
                let owner = st.owner_of(activation, id);
                let mut o = owner.lock();
                if o.kind == SpanKind::Agent {
                    o.has_response = true;
                    o.model_responded = true;
                    o.awaiting_model_event += 1;
                }
            }
            let mut r = rec.lock();
            if r.first_token.is_none() {
                r.first_token = Some(now);
            }
        }
        // The ended call no longer keeps its agents from retiring in the late state.
        inv.retire_late_settled_locked(st);
        let r = rec.lock();
        let mut span = inv.row_span(st, &r, Some(latency_of(&r, now)), &ambient);
        span.timestamp = Some(now);
        span
    }

    /// Go `pushToolSpan(ctx, id, key)`: the place of `TOOL_STARTING`.
    pub(crate) fn push_tool_span(&self, ctx: &CallbackCtx, id: &Identity, key: &str) -> RowSpan {
        let inv = self.lookup_invocation(ctx, id, false, true);
        let _reservation = Reservation(inv.clone());
        let now = (self.now)();
        let ambient = ctx.ambient_trace_id();
        let root_in = prepare_root_agent_input(ctx, &inv, id, None, None);
        let Some(mut guard) = inv.lock_live() else {
            return self.push_tool_span(ctx, id, key);
        };
        let st = &mut *guard;
        st.started = true;
        inv.note_root_agent_locked(st, id, &root_in, true);
        let key = ctx.call_key(key);
        let activation = ctx.activation_key();
        let owner = st.owner_of(activation, id);
        st.clear_last_partial_under_locked(&owner);
        let owner_is_agent = owner.lock().kind == SpanKind::Agent;
        if owner_is_agent {
            st.touch_agent_activity_locked(&owner, now);
            // Keeps `awaiting_tool_event`: the concurrent calls of one model response share
            // the merged function-response event.
            owner.lock().awaiting_model_event = 0;
        }
        let rec = st.new_child(&owner, SpanKind::Tool, now);
        let seq = st.next_seq_locked();
        {
            let mut r = rec.lock();
            r.canceled = ctx.canceled_func();
            r.seq = seq;
            r.call_started = true;
            r.own_activation = owner.lock().is_activation_of(activation);
        }
        st.tool_spans.insert(key.clone(), rec.clone());
        inv.sync_open_calls_locked(st);
        st.completed_tools.remove(&key);
        let mut span = inv.row_span(st, &rec.lock(), None, &ambient);
        span.timestamp = Some(now);
        span
    }

    /// Go `eventSpan(ctx, id, author, branch)`: the place of a session-event row — the span of
    /// the event's author if it is running, else the invocation span.
    pub(crate) fn event_span(
        &self,
        ctx: &CallbackCtx,
        id: &Identity,
        author: &str,
        branch: &str,
    ) -> RowSpan {
        let inv = self.invocation(ctx, id);
        let now = (self.now)();
        let ambient = ctx.ambient_trace_id();
        let root_in = prepare_root_agent_input(ctx, &inv, id, None, None);
        let mut guard = inv.mu.lock();
        let st = &mut *guard;
        inv.note_root_agent_locked(st, id, &root_in, false);
        let mut rec = st.agent_stack[0].clone();
        if let Some(i) = st.find_agent(&id.path, author, branch) {
            rec = st.agent_stack[i].clone();
            st.touch_agent_activity_locked(&rec, now);
        }
        let mut span = inv.row_span(st, &rec.lock(), None, &ambient);
        span.timestamp = Some(now);
        span
    }
}

/// Go's `defer inv.pendingSpans.Add(-1)` after a reserving lookup.
struct Reservation(Arc<InvocationTrace>);

impl Drop for Reservation {
    fn drop(&mut self) {
        self.0.pending_spans.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Go `completionOrder.PushBack` / `MoveToBack`: a new back entry for `id`; the returned
/// generation goes to its `CompletedTrace::generation` (earlier entries of `id` go stale).
/// The caller stores the generation, then calls [`compact_completion_order`].
fn push_completion(order: &mut VecDeque<(String, u64)>, next: &mut u64, id: &str) -> u64 {
    let generation = *next;
    *next += 1;
    order.push_back((id.to_string(), generation));
    generation
}

/// Drops the stale completion-order entries once they outnumber the live ones: only the
/// entries whose ID is still completed with that generation stay (Go's list holds exactly
/// the completed runs, removing a run's element when the run is deleted, e.g. on a resume).
fn compact_completion_order(
    order: &mut VecDeque<(String, u64)>,
    completed: &HashMap<String, CompletedTrace>,
) {
    if order.len() > 2 * MAX_TRACKED_INVOCATIONS {
        order.retain(|(id, g)| completed.get(id).map(|c| c.generation) == Some(*g));
    }
}

/// Go `withCalls(calls, more)`: `calls` with `more` added; `None` if both are empty.
pub(crate) fn with_calls(
    calls: Option<HashMap<String, SpanRef>>,
    more: Option<HashMap<String, SpanRef>>,
) -> Option<HashMap<String, SpanRef>> {
    let Some(more) = more.filter(|m| !m.is_empty()) else {
        return calls;
    };
    match calls {
        None => Some(more),
        Some(mut calls) => {
            calls.extend(more);
            Some(calls)
        }
    }
}

/// Go `startedCalls(calls)`: the calls `push_llm_span` / `push_tool_span` started, or `None`.
pub(crate) fn started_calls(calls: &HashMap<String, SpanRef>) -> Option<HashMap<String, SpanRef>> {
    let started: HashMap<String, SpanRef> = calls
        .iter()
        .filter(|(_, rec)| rec.lock().call_started)
        .map(|(k, rec)| (k.clone(), rec.clone()))
        .collect();
    (!started.is_empty()).then_some(started)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::super::span::Latency;
    use super::super::testkit::{ManualClock, adk_ctx, custom, ident, sequential};
    use super::super::tool_key;
    use super::*;

    fn manager(clock: &ManualClock) -> TraceManager {
        let mut tm = TraceManager::new();
        tm.now = clock.clock();
        tm
    }

    /// A runner tree `root` (sequential) → `a` (custom), and the callback contexts of both.
    struct Run {
        root: Arc<dyn Agent>,
        a: Arc<dyn Agent>,
        root_ctx: CallbackCtx,
        a_ctx: CallbackCtx,
    }

    fn run(inv: &str) -> Run {
        let a = custom("a", vec![]);
        let root = sequential("root", vec![a.clone()]);
        let root_ctx = CallbackCtx::from_context(&adk_ctx(&root, inv, Some(&root)));
        let a_ctx = CallbackCtx::from_context(&adk_ctx(&a, inv, Some(&root)));
        Run {
            root,
            a,
            root_ctx,
            a_ctx,
        }
    }

    #[test]
    fn trace_id_for_is_deterministic_per_manager() {
        let (tm1, tm2) = (TraceManager::new(), TraceManager::new());
        let id = tm1.trace_id_for("inv");
        assert_eq!(id.len(), 32);
        assert_eq!(id, tm1.trace_id_for("inv"));
        assert_ne!(id, tm1.trace_id_for("other"));
        assert_ne!(id, tm2.trace_id_for("inv"));
        assert_ne!(tm1.trace_id_for(""), tm1.trace_id_for(""));
        let sum = Sha256::digest(format!("{}\x00inv", tm1.trace_seed).as_bytes());
        assert_eq!(id, hex(&sum[..16]));
    }

    #[test]
    fn a_run_places_its_rows_in_one_tree() {
        let clock = ManualClock::new();
        let tm = manager(&clock);
        let r = run("inv");
        let root_id = ident("inv", "root", "root", "");
        let a_id = ident("inv", "a", "root.a", "");
        let start = clock.now();

        let inv_span = tm.invocation_span(&r.root_ctx, &root_id, Some(&r.root));
        assert_eq!(inv_span.trace_id, tm.trace_id_for("inv"));
        assert_eq!(inv_span.parent_span_id, "");
        assert_eq!(inv_span.root_agent_name, "root");
        assert_eq!(inv_span.timestamp, Some(start));
        assert!(inv_span.latency.is_none());

        let (root_span, purged) = tm.push_agent_span(&r.root_ctx, &root_id);
        assert!(purged.is_empty());
        assert_eq!(root_span.parent_span_id, inv_span.span_id);
        let (a_span, _) = tm.push_agent_span(&r.a_ctx, &a_id);
        assert_eq!(a_span.parent_span_id, root_span.span_id);

        let llm = tm.push_llm_span(&r.a_ctx, &a_id);
        assert_eq!(llm.parent_span_id, a_span.span_id);
        clock.advance_ms(100);
        tm.record_first_token(&r.a_ctx, &a_id);
        clock.advance_ms(200);
        let resp = tm.pop_llm_span(&r.a_ctx, &a_id, true);
        assert_eq!(resp.span_id, llm.span_id);
        assert_eq!(
            resp.latency,
            Some(Latency {
                total_ms: 300,
                time_to_first_token_ms: Some(100)
            })
        );

        let tool = tm.push_tool_span(&r.a_ctx, &a_id, &tool_key(&a_id, "call_1", "t"));
        assert_eq!(tool.parent_span_id, a_span.span_id);

        let ev = tm.event_span(&r.root_ctx, &root_id, "a", "root.a");
        assert_eq!(ev.span_id, a_span.span_id);
        let ev = tm.event_span(&r.root_ctx, &root_id, "nobody", "root.nobody");
        assert_eq!(ev.span_id, inv_span.span_id);

        clock.advance_ms(700);
        let end = tm.end_invocation(&r.root_ctx, &root_id);
        assert_eq!(end.span_id, inv_span.span_id);
        assert_eq!(end.latency.unwrap().total_ms, 1000);
        for span in [&root_span, &a_span, &llm, &resp, &tool, &ev, &end] {
            assert_eq!(span.trace_id, inv_span.trace_id);
            assert_eq!(span.root_agent_name, "root");
        }
        let st = tm.mu.lock();
        assert!(st.invocations.is_empty());
        assert_eq!(st.completed["inv"].identity.span_id, inv_span.span_id);
    }

    #[test]
    fn model_calls_ended_by_errors_and_unknown_calls() {
        let clock = ManualClock::new();
        let tm = manager(&clock);
        let r = run("inv");
        let a_id = ident("inv", "a", "root.a", "");
        tm.invocation_span(
            &r.root_ctx,
            &ident("inv", "root", "root", ""),
            Some(&r.root),
        );
        let (a_span, _) = tm.push_agent_span(&r.a_ctx, &a_id);
        let llm = tm.push_llm_span(&r.a_ctx, &a_id);
        clock.advance_ms(50);
        // OnModelErrorCallback ends the call; a recovering AfterModelCallback reuses its span.
        let failed = tm.pop_llm_span(&r.a_ctx, &a_id, false);
        assert_eq!(failed.span_id, llm.span_id);
        assert_eq!(failed.latency.unwrap().time_to_first_token_ms, None);
        let recovered = tm.pop_llm_span(&r.a_ctx, &a_id, true);
        assert_eq!(recovered.span_id, llm.span_id);
        assert!(recovered.latency.is_none());
        // An unknown call gets a span of its own under the agent.
        let unknown = tm.pop_llm_span(&r.a_ctx, &a_id, true);
        assert_ne!(unknown.span_id, llm.span_id);
        assert_eq!(unknown.parent_span_id, a_span.span_id);
        assert!(unknown.latency.is_none());
        let inv = tm.invocation(&r.a_ctx, &a_id);
        assert_eq!(inv.pending_spans.load(Ordering::SeqCst), 0);
        assert!(!inv.has_open_calls());
    }

    #[test]
    fn concurrent_activations_are_peers_and_calls_find_their_own() {
        let clock = ManualClock::new();
        let tm = manager(&clock);
        let r = run("inv");
        let a_id = ident("inv", "a", "root.a", "");
        tm.invocation_span(
            &r.root_ctx,
            &ident("inv", "root", "root", ""),
            Some(&r.root),
        );
        let first_ctx = r.a_ctx.clone();
        let second_ctx = CallbackCtx::from_context(&adk_ctx(&r.a, "inv", Some(&r.root)));
        let (first, _) = tm.push_agent_span(&first_ctx, &a_id);
        let (second, _) = tm.push_agent_span(&second_ctx, &a_id);
        let inv = tm.invocation(&first_ctx, &a_id);
        {
            let st = inv.mu.lock();
            assert_eq!(st.agent_stack.len(), 3);
            assert!(st.agent_stack[1].lock().concurrent_peer);
            assert!(st.agent_stack[2].lock().concurrent_peer);
        }
        let llm = tm.push_llm_span(&first_ctx, &a_id);
        assert_eq!(llm.parent_span_id, first.span_id);
        let llm2 = tm.push_llm_span(&second_ctx, &a_id);
        assert_eq!(llm2.parent_span_id, second.span_id);
        let st = inv.mu.lock();
        assert_eq!(st.llm_spans.len(), 2);
        assert!(st.llm_spans.values().all(|r| r.lock().own_activation));
    }

    #[test]
    fn streamed_events_mark_calls_and_lineage() {
        let clock = ManualClock::new();
        let tm = manager(&clock);
        let r = run("inv");
        let root_id = ident("inv", "root", "root", "");
        let a_id = ident("inv", "a", "root.a", "");
        tm.invocation_span(&r.root_ctx, &root_id, Some(&r.root));
        let (root_span, _) = tm.push_agent_span(&r.root_ctx, &root_id);
        let (a_span, _) = tm.push_agent_span(&r.a_ctx, &a_id);
        tm.push_llm_span(&r.a_ctx, &a_id);
        tm.record_streamed(&r.root_ctx, &root_id, "a", "root.a", true);
        let inv = tm.invocation(&r.root_ctx, &root_id);
        {
            let st = inv.mu.lock();
            assert!(st.last_session_event_partial && st.last_event_partial);
            assert_eq!(st.last_partial_span_id, a_span.span_id);
            assert_eq!(
                st.partial_lineage,
                HashSet::from([a_span.span_id.clone(), root_span.span_id.clone()])
            );
            assert!(st.llm_spans.values().all(|r| r.lock().streamed));
            assert!(st.agent_stack[1].lock().has_response && st.agent_stack[2].lock().has_response);
        }
        tm.clear_last_partial(&r.root_ctx, &root_id);
        let st = inv.mu.lock();
        assert!(!st.last_session_event_partial && st.partial_lineage.is_empty());
    }

    #[test]
    fn open_calls_move_to_the_late_state_at_the_end_of_the_run() {
        let clock = ManualClock::new();
        let tm = manager(&clock);
        let r = run("inv");
        let root_id = ident("inv", "root", "root", "");
        let a_id = ident("inv", "a", "root.a", "");
        let inv_span = tm.invocation_span(&r.root_ctx, &root_id, Some(&r.root));
        tm.push_agent_span(&r.a_ctx, &a_id);
        let open = tm.push_llm_span(&r.a_ctx, &a_id);
        let end = tm.end_invocation(&r.root_ctx, &root_id);
        {
            let st = tm.mu.lock();
            let completed = &st.completed["inv"];
            let late = completed.late.as_ref().unwrap();
            assert!(late.mu.lock().is_late);
            assert_eq!(late.open_handed_over.load(Ordering::SeqCst), 1);
            assert!(completed.waits_for_late_calls());
            assert!(completed.has_pending_or_open_late_spans());
        }
        // The call's completion callback after AfterRun ends the call's own span.
        clock.advance_ms(40);
        let resp = tm.pop_llm_span(&r.a_ctx, &a_id, true);
        assert_eq!(resp.span_id, open.span_id);
        assert_eq!(resp.latency.unwrap().total_ms, 40);
        assert_eq!(resp.trace_id, end.trace_id);
        // A later callback starts in the late state, under the run's invocation span.
        let late_call = tm.push_llm_span(&r.a_ctx, &a_id);
        assert_eq!(late_call.parent_span_id, inv_span.span_id);
        assert_eq!(late_call.root_agent_name, "root");
        {
            let st = tm.mu.lock();
            assert!(!st.completed["inv"].waits_for_late_calls());
        }
        // A new run with the same ID (a resume) starts over.
        let resumed = tm.invocation_span(&r.root_ctx, &root_id, Some(&r.root));
        assert_ne!(resumed.span_id, inv_span.span_id);
        assert_eq!(resumed.trace_id, inv_span.trace_id);
        assert!(!tm.mu.lock().completed.contains_key("inv"));
    }

    #[test]
    fn streamed_calls_are_not_handed_over() {
        let clock = ManualClock::new();
        let tm = manager(&clock);
        let r = run("inv");
        let root_id = ident("inv", "root", "root", "");
        let a_id = ident("inv", "a", "root.a", "");
        tm.invocation_span(&r.root_ctx, &root_id, Some(&r.root));
        tm.push_agent_span(&r.a_ctx, &a_id);
        let open = tm.push_llm_span(&r.a_ctx, &a_id);
        tm.record_streamed(&r.root_ctx, &root_id, "a", "root.a", false);
        tm.end_invocation(&r.root_ctx, &root_id);
        assert!(tm.mu.lock().completed["inv"].late.is_none());
        let resp = tm.pop_llm_span(&r.a_ctx, &a_id, true);
        assert_ne!(resp.span_id, open.span_id);
        assert!(resp.latency.is_none());
    }

    #[test]
    fn capacity_evicts_the_least_recently_active_and_logs() {
        let clock = ManualClock::new();
        let mut tm = manager(&clock);
        tm.max_tracked = 2;
        let lines = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = lines.clone();
        tm.logf = Some(Arc::new(move |l: &str| sink.lock().push(l.to_string())));
        let ctx = CallbackCtx::background();
        let mut spans = Vec::new();
        for inv in ["i1", "i2", "i3"] {
            clock.advance_ms(10);
            spans.push(tm.invocation_span(&ctx, &ident(inv, "root", "", ""), None));
        }
        assert_eq!(
            *lines.lock(),
            vec![
                "agentanalytics: trace manager capacity (2) exceeded; evicting active invocation \"i1\""
                    .to_string()
            ]
        );
        {
            let st = tm.mu.lock();
            assert!(!st.invocations.contains_key("i1"));
            assert_eq!(st.evicted["i1"].span_id, spans[0].span_id);
        }
        // The evicted run keeps its identity, without latency.
        let end = tm.end_invocation(&ctx, &ident("i1", "root", "", ""));
        assert_eq!(end.span_id, spans[0].span_id);
        assert_eq!(end.trace_id, spans[0].trace_id);
        assert!(end.latency.is_none());
        assert!(!tm.mu.lock().evicted.contains_key("i1"));
    }

    #[test]
    fn idle_invocations_expire_without_a_warning() {
        let clock = ManualClock::new();
        let mut tm = manager(&clock);
        tm.max_tracked = 1;
        tm.ttl = Duration::from_secs(60);
        let lines = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = lines.clone();
        tm.logf = Some(Arc::new(move |l: &str| sink.lock().push(l.to_string())));
        let ctx = CallbackCtx::background();
        tm.invocation_span(&ctx, &ident("old", "root", "", ""), None);
        clock.advance_ms(61_000);
        tm.invocation_span(&ctx, &ident("new", "root", "", ""), None);
        assert!(lines.lock().is_empty());
        let st = tm.mu.lock();
        assert!(st.evicted.contains_key("old"));
        assert_eq!(st.invocations.len(), 1);
    }

    #[test]
    fn completed_runs_are_pruned_beyond_capacity() {
        let clock = ManualClock::new();
        let mut tm = manager(&clock);
        // Completed runs keep max(max_tracked, 64) identities.
        tm.max_tracked = 2;
        let ctx = CallbackCtx::background();
        for n in 0..70 {
            let id = ident(&format!("run{n}"), "root", "", "");
            tm.invocation_span(&ctx, &id, None);
            tm.end_invocation(&ctx, &id);
        }
        // Runs 0-5 went first. A late callback on `run6` moves it to the back of the order (and
        // leaves a call open in its late state), so the next completion prunes `run7`.
        tm.push_llm_span(&ctx, &ident("run6", "root", "", ""));
        let id = ident("run70", "root", "", "");
        tm.invocation_span(&ctx, &id, None);
        tm.end_invocation(&ctx, &id);
        let st = tm.mu.lock();
        assert_eq!(st.completed.len(), 64);
        for n in [0, 1, 2, 3, 4, 5, 7] {
            assert!(!st.completed.contains_key(&format!("run{n}")), "run{n}");
        }
        assert!(st.completed.contains_key("run6"));
        assert!(st.completed.contains_key("run8"));
    }

    #[test]
    fn untracked_invocations_get_ids_of_their_own() {
        let clock = ManualClock::new();
        let tm = manager(&clock);
        let ctx = CallbackCtx::background();
        let id = ident("", "a", "", "");
        let a = tm.push_llm_span(&ctx, &id);
        let b = tm.push_llm_span(&ctx, &id);
        assert_ne!(a.trace_id, b.trace_id);
        assert!(tm.mu.lock().invocations.is_empty());
    }

    #[test]
    fn agent_tree_lookups() {
        let clock = ManualClock::new();
        let tm = manager(&clock);
        let r = run("inv");
        assert!(tm.remember_agent_tree(None).is_none());
        let top = tm.remember_agent_tree(Some(&r.root)).unwrap();
        assert!(same_agent(&top, &r.root));
        let top = tm.remember_agent_tree(Some(&r.a)).unwrap();
        assert!(same_agent(&top, &r.root));
        let found = tm
            .agent_named(&r.a_ctx, &ident("inv", "a", "root.a", ""))
            .unwrap();
        assert!(same_agent(&found, &r.a));
        assert!(
            tm.agent_named(&CallbackCtx::background(), &ident("x", "a", "", ""))
                .is_none()
        );
    }

    /// A completed run whose ID starts again (a HITL resume) leaves the completion order
    /// (Go removes its list element); the queue stays bounded however many such runs there
    /// are, and the live entries keep their generations, so pruning still finds them.
    #[test]
    fn completion_order_stays_bounded_with_resumed_runs() {
        let clock = ManualClock::new();
        let mut tm = manager(&clock);
        tm.max_tracked = 64;
        let r = run("x");
        let complete = |id: &str| {
            let inv = Arc::new(InvocationTrace::new_with_span(None, "", clock.now()));
            let mut st = tm.mu.lock();
            tm.remember_completed_locked(&mut st, id, "root", &inv);
        };
        for i in 0..2 * MAX_TRACKED_INVOCATIONS + 10 {
            let id = format!("resumed-{i}");
            complete(&id);
            tm.lookup_invocation(&r.root_ctx, &ident(&id, "root", "root", ""), true, false);
            tm.mu.lock().invocations.remove(&id);
        }
        for j in 0..3 {
            complete(&format!("live-{j}"));
        }
        let mut st = tm.mu.lock();
        assert_eq!(st.completed.len(), 3);
        assert!(
            st.completion_order.len() <= 2 * MAX_TRACKED_INVOCATIONS + 1,
            "completion order grew to {}",
            st.completion_order.len()
        );
        for (id, c) in &st.completed {
            assert!(
                st.completion_order.contains(&(id.clone(), c.generation)),
                "live entry {id} lost its generation"
            );
        }
        tm.prune_completed_locked(&mut st, 1, &|_| true);
        assert_eq!(st.completed.keys().collect::<Vec<_>>(), vec!["live-2"]);
    }

    #[test]
    fn call_maps_helpers() {
        let started = SpanRef::new(SpanRecord {
            call_started: true,
            ..SpanRecord::default()
        });
        let made_up = SpanRef::default();
        let calls = HashMap::from([
            ("s".to_string(), started.clone()),
            ("m".to_string(), made_up),
        ]);
        let got = started_calls(&calls).unwrap();
        assert_eq!(got.len(), 1);
        assert!(got["s"].ptr_eq(&started));
        assert!(started_calls(&HashMap::new()).is_none());
        assert!(with_calls(None, None).is_none());
        assert!(with_calls(None, Some(HashMap::new())).is_none());
        let merged = with_calls(
            Some(got.clone()),
            Some(HashMap::from([("x".to_string(), started)])),
        );
        assert_eq!(merged.unwrap().len(), 2);
        assert_eq!(with_calls(None, Some(got)).unwrap().len(), 1);
    }
}
