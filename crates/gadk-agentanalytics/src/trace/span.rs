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

//! Span records and the places of rows, from ADK-Go `plugin/agentanalytics/trace_manager.go`:
//! `spanKind`, `spanRecord` (with the goroutine attribution fields, which stay zero until
//! 7b-3b), `retiredCause`, `rowSpan`, `unclosedAgentSpan`, `latencyOf`, `goroutineOrigin`,
//! `attemptSite`, `nodeRunKey`, and the record-local helpers that are not about failures
//! (those are [`super::causes`]) or settling (those are [`super::settle`]).
//!
//! Go's `*spanRecord` pointers are [`SpanRef`]s (`Arc<parking_lot::Mutex<SpanRecord>>`, equal
//! by address). Every record is read and written only while the `mu` of the invocation that
//! holds it is held, so its own mutex is never contended and guards of several distinct records
//! may be alive at once; never lock a record whose guard is already alive (compare with
//! [`SpanRef::ptr_eq`] first — `parking_lot` mutexes are not reentrant).

#![allow(dead_code, unused_variables)] // Task 5 implements the bodies.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use gadk_agent::ActivationId;
use parking_lot::{Mutex, MutexGuard};
use tokio_util::sync::CancellationToken;

use super::TraceError;

/// Go `spanKind`: the kind of operation a span covers. `None` is Go's zero value, which no
/// record that the manager makes has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub(crate) enum SpanKind {
    /// Go's zero `spanKind`.
    #[default]
    None,
    /// Go `spanKindInvocation`.
    Invocation,
    /// Go `spanKindAgent`.
    Agent,
    /// Go `spanKindLLM`.
    Llm,
    /// Go `spanKindTool`.
    Tool,
}

/// Go `goroutineOrigin`: the function that created a goroutine and the goroutine that ran it.
/// Always zero in 7b-3a (7b-3b fills it from gadk-engine's spawn origins).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct GoroutineOrigin {
    pub(crate) creator: String,
    pub(crate) parent: u64,
}

/// Go `attemptSite`. Always [`AttemptSite::None`] in 7b-3a.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub(crate) enum AttemptSite {
    /// Go `attemptNone`.
    #[default]
    None,
    /// Go `attemptNode`.
    Node,
    /// Go `attemptWorker`.
    Worker,
}

/// Go `nodeRunKey`: the node path and branch of a workflow node run.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub(crate) struct NodeRunKey {
    pub(crate) path: String,
    pub(crate) branch: String,
}

/// Go `spanRecord.canceled` (`canceledFunc(ctx)`): the cancellation of the context of the
/// callback that started the span, read later to tell how it ended (R3).
#[derive(Debug, Clone)]
pub(crate) struct CancelSource {
    /// The context's token (an ADK context's `cancellation_token()`).
    pub(crate) token: CancellationToken,
    /// Go `context.Cause` once cancelled; `None` = Go `context.Canceled` (always `None` for
    /// ADK contexts; see `CallbackCtx::cancel_cause`).
    pub(crate) cause: Option<TraceError>,
}

impl CancelSource {
    /// The Go closure `canceledFunc` returns: `(false, None)` while live; `(true, None)` when
    /// cancelled with Go's `context.Canceled` cause; `(false, Some(cause))` otherwise.
    pub(crate) fn check(&self) -> (bool, Option<TraceError>) {
        todo!("Task 5")
    }
}

/// Go `retiredCause`: the failure cause of an activation that retired under an open span.
#[derive(Debug, Clone, Default)]
pub(crate) struct RetiredCause {
    pub(crate) err: Option<TraceError>,
    pub(crate) name: String,
    pub(crate) branch: String,
    pub(crate) path: String,
    pub(crate) swallowable_by_self: bool,
}

/// Go `latencyOf`'s map: `total_ms` and, for model calls that produced a first token,
/// `time_to_first_token_ms` (the `latency_ms` column).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Latency {
    pub(crate) total_ms: i64,
    pub(crate) time_to_first_token_ms: Option<i64>,
}

/// Go `rowSpan`: the place of a row in the trace of its invocation.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct RowSpan {
    pub(crate) trace_id: String,
    pub(crate) span_id: String,
    pub(crate) parent_span_id: String,
    /// Go `latency map[string]any` (nil = `None`): set on rows that end a span.
    pub(crate) latency: Option<Latency>,
    pub(crate) root_agent_name: String,
    /// Go `timestamp time.Time` (zero = `None`): the manager clock's reading by the callback.
    pub(crate) timestamp: Option<DateTime<Utc>>,
}

/// Go `unclosedAgentSpan`: an activation ended without its AfterAgentCallback (an
/// `AGENT_ERROR` row).
#[derive(Debug, Clone, Default)]
pub(crate) struct UnclosedAgentSpan {
    pub(crate) span: RowSpan,
    pub(crate) agent_name: String,
    pub(crate) branch: String,
    pub(crate) path: String,
    pub(crate) err: Option<TraceError>,
    /// Go `timestamp` (zero = `None`): the latest callback under the activation.
    pub(crate) timestamp: Option<DateTime<Utc>>,
}

/// Go `spanRecord`: a span that has started. Field docs are Go's (`trace_manager.go:99-350`);
/// Go's closures over the callback's context are kept as that context (R3).
#[derive(Default)]
pub(crate) struct SpanRecord {
    pub(crate) span_id: String,
    pub(crate) parent_span_id: String,
    pub(crate) kind: SpanKind,
    /// Go `path`, `name`, `branch`: identify the activation of agent spans.
    pub(crate) path: String,
    pub(crate) name: String,
    pub(crate) branch: String,
    pub(crate) start: DateTime<Utc>,
    /// Go `firstToken` (zero = `None`).
    pub(crate) first_token: Option<DateTime<Utc>>,
    pub(crate) streamed: bool,
    /// Go `canceled func() (bool, error)`; `None` once dropped (`drop_contexts`) or for a
    /// context that cannot be cancelled.
    pub(crate) canceled: Option<CancelSource>,
    pub(crate) retired_causes: Vec<RetiredCause>,
    /// Go `parent *spanRecord`.
    pub(crate) parent: Option<SpanRef>,
    pub(crate) stop_point: bool,
    pub(crate) has_response: bool,
    pub(crate) model_responded: bool,
    pub(crate) extra_responses: i64,
    pub(crate) awaiting_model_event: i64,
    pub(crate) awaiting_tool_event: i64,
    pub(crate) concurrent_peer: bool,
    /// Go `goroutineID`, `origin`, `attempt`: zero in 7b-3a (7b-3b).
    pub(crate) goroutine_id: u64,
    pub(crate) origin: GoroutineOrigin,
    pub(crate) attempt: AttemptSite,
    pub(crate) completed: bool,
    pub(crate) node_run_ended: bool,
    pub(crate) started_again: bool,
    /// Go `nodeRuns map[nodeRunKey]int` (nil = empty).
    pub(crate) node_runs: HashMap<NodeRunKey, i64>,
    /// Go `ended func() bool`: the ADK context of the activation (its `ended()` flag; R3);
    /// `None` once it reported true, once dropped, or for a plain context.
    pub(crate) ended: Option<gadk_agent::Context>,
    pub(crate) context_ended: bool,
    pub(crate) run_done: bool,
    pub(crate) seq: u64,
    pub(crate) task_delegated: bool,
    pub(crate) finished_tasks: Vec<String>,
    pub(crate) non_llm: bool,
    pub(crate) propagates_child_error: bool,
    pub(crate) is_parallel: bool,
    pub(crate) is_workflow: bool,
    pub(crate) last_activity: DateTime<Utc>,
    pub(crate) task_mode: bool,
    pub(crate) call_started: bool,
    pub(crate) handed_over: bool,
    pub(crate) own_activation: bool,
    pub(crate) children: Vec<String>,
    /// Go `activation any` (the run's InvocationContext pointer): the context's
    /// [`ActivationId`]; `None` when the context shows none, or once dropped.
    pub(crate) activation: Option<ActivationId>,
}

/// Go `*spanRecord`: a shared span record, equal (and hashed) by address.
#[derive(Clone, Default)]
pub(crate) struct SpanRef(Arc<Mutex<SpanRecord>>);

impl SpanRef {
    /// Go `&spanRecord{…}`.
    pub(crate) fn new(rec: SpanRecord) -> SpanRef {
        SpanRef(Arc::new(Mutex::new(rec)))
    }

    /// The record, locked (see the module docs for the locking rule).
    pub(crate) fn lock(&self) -> MutexGuard<'_, SpanRecord> {
        self.0.lock()
    }

    /// Go `a == b` on `*spanRecord`.
    pub(crate) fn ptr_eq(&self, other: &SpanRef) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// Go `a == b` where `b` may be nil.
    pub(crate) fn is(&self, other: Option<&SpanRef>) -> bool {
        other.is_some_and(|o| self.ptr_eq(o))
    }
}

impl PartialEq for SpanRef {
    fn eq(&self, other: &SpanRef) -> bool {
        self.ptr_eq(other)
    }
}

impl Eq for SpanRef {}

impl Hash for SpanRef {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (Arc::as_ptr(&self.0) as usize).hash(state);
    }
}

impl std::fmt::Debug for SpanRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SpanRef({:p})", Arc::as_ptr(&self.0))
    }
}

impl SpanRecord {
    /// Go `firstRetiredCause`: the first non-nil retired cause.
    pub(crate) fn first_retired_cause(&self) -> Option<TraceError> {
        todo!("Task 5")
    }

    /// Go `clearRetiredCauseFor(name, branch, path)`.
    pub(crate) fn clear_retired_cause_for(&mut self, name: &str, branch: &str, path: &str) {
        todo!("Task 5")
    }

    /// Go `dropContexts`: drops `canceled`, `ended`, `activation`, `parent` and
    /// `retired_causes`.
    pub(crate) fn drop_contexts(&mut self) {
        todo!("Task 5")
    }

    /// Go `isActivationOf(ctx)`: an agent span whose activation is `activation` (non-nil).
    pub(crate) fn is_activation_of(&self, activation: Option<ActivationId>) -> bool {
        todo!("Task 5")
    }

    /// Go `endedWithRun(runner)`: false whenever the parent's `goroutine_id` is 0, i.e.
    /// always in 7b-3a (the body is ported whole).
    pub(crate) fn ended_with_run(&self, runner: &str) -> bool {
        todo!("Task 5")
    }

    /// Go `isNodeAgentOf(w)` (attribution; false with zero goroutine information).
    pub(crate) fn is_node_agent_of(&self, w: &SpanRecord) -> bool {
        todo!("Task 5")
    }
}

/// Go `(p *spanRecord).joinsOnceReturned(c)` (attribution).
pub(crate) fn joins_once_returned(p: &SpanRef, c: &SpanRef) -> bool {
    todo!("Task 5")
}

/// Go `(p *spanRecord).joins(c, reported)` (attribution; with zero goroutine information only
/// the `completed` / `runDone` and parallel-branch fallbacks apply).
pub(crate) fn joins(p: &SpanRef, c: &SpanRef, reported: bool) -> bool {
    todo!("Task 5")
}

impl GoroutineOrigin {
    /// Go `startedBy(creator, p)`: false when `p` is `None` or has no goroutine.
    pub(crate) fn started_by(&self, creator: &str, p: Option<&SpanRecord>) -> bool {
        todo!("Task 5")
    }
}

/// Go `parallelBranch(a, childName)`: the branch of a ParallelAgent `a`'s branch running
/// `child_name`.
pub(crate) fn parallel_branch(a: &SpanRecord, child_name: &str) -> String {
    todo!("Task 5")
}

/// Go `latencyOf(rec, end)`.
pub(crate) fn latency_of(rec: &SpanRecord, end: DateTime<Utc>) -> Latency {
    todo!("Task 5")
}
