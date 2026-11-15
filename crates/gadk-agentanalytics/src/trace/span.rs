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
//! `spanKind`, `spanRecord` (with the goroutine attribution fields, read from the callback's
//! fiber, `gadk_agent::fiber`), `retiredCause`, `rowSpan`, `unclosedAgentSpan`, `latencyOf`,
//! `goroutineOrigin` (`gadk_agent::fiber::Origin`), `attemptSite`, `nodeRunKey`, and the
//! record-local helpers that are not about failures (those are [`super::causes`]) or settling
//! (those are [`super::settle`]).
//!
//! Go's `*spanRecord` pointers are [`SpanRef`]s (`Arc<parking_lot::Mutex<SpanRecord>>`, equal
//! by address). Every record is read and written only while the `mu` of the invocation that
//! holds it is held, so its own mutex is never contended and guards of several distinct records
//! may be alive at once; never lock a record whose guard is already alive (compare with
//! [`SpanRef::ptr_eq`] first — `parking_lot` mutexes are not reentrant).

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use gadk_agent::ActivationId;
use gadk_agent::fiber::{ERRGROUP_CREATOR, SCHEDULER_CREATOR};
use parking_lot::{Mutex, MutexGuard};
use tokio_util::sync::CancellationToken;

use super::TraceError;
use crate::context::ContextError;
use crate::error::Error;

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

/// Go `goroutineOrigin`: the spawn site that started the fiber (a `gadk_agent::fiber` creator
/// constant, Go's creator function) and the fiber it ran on. Go `startedBy(creator, p)` is
/// `origin.started_by(creator, p.goroutine_id)` (false when `p` has no goroutine).
pub(crate) type GoroutineOrigin = gadk_agent::fiber::Origin;

/// Go `attemptSite`: `None` is Go's `attemptNone`, the others are the engine's claimed
/// `gadk_agent::fiber::AttemptSite`.
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

impl From<Option<gadk_agent::fiber::AttemptSite>> for AttemptSite {
    fn from(site: Option<gadk_agent::fiber::AttemptSite>) -> AttemptSite {
        match site {
            None => AttemptSite::None,
            Some(gadk_agent::fiber::AttemptSite::Node) => AttemptSite::Node,
            Some(gadk_agent::fiber::AttemptSite::Worker) => AttemptSite::Worker,
        }
    }
}

/// Go `nodeRunKey`: the node path and branch of a workflow node run.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub(crate) struct NodeRunKey {
    pub(crate) path: String,
    pub(crate) branch: String,
}

/// Go `spanRecord.canceled` (`canceledFunc(ctx)`): the cancellation of the context of the
/// callback that started the span, read later to tell how it ended (R3).
#[derive(Clone, Default)]
pub(crate) struct CancelSource {
    /// The context's token (an ADK context's `cancellation_token()`, or a test's own).
    pub(crate) token: CancellationToken,
    /// A test's cause (Go `context.WithCancelCause`) once `token` is cancelled; `None` = Go
    /// `context.Canceled`. Unused when `adk` is set.
    pub(crate) cause: Option<TraceError>,
    /// The ADK context whose `cause()` (Go `context.Cause`) is read at each
    /// [`check`](Self::check), so a cause recorded after the span started is seen.
    pub(crate) adk: Option<gadk_agent::Context>,
}

impl std::fmt::Debug for CancelSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancelSource")
            .field("token", &self.token)
            .field("cause", &self.cause)
            .field("adk", &self.adk.is_some())
            .finish()
    }
}

impl CancelSource {
    /// An ADK context's cancellation: its token, its cause read lazily.
    pub(crate) fn of_context(ctx: &gadk_agent::Context) -> CancelSource {
        CancelSource {
            token: ctx.cancellation_token().clone(),
            cause: None,
            adk: Some(ctx.clone()),
        }
    }

    /// A test's token with an optional cause (Go `context.WithCancel` /
    /// `context.WithCancelCause`).
    #[cfg(test)]
    pub(crate) fn of_token(token: CancellationToken, cause: Option<TraceError>) -> CancelSource {
        CancelSource {
            token,
            cause,
            adk: None,
        }
    }

    /// Go `context.Cause(ctx)` once the token is cancelled (`None` while live): the ADK
    /// context's cause (`Canceled` → Go `context.Canceled`, `DeadlineExceeded` → Go
    /// `context.DeadlineExceeded`, `Error(e)` → `e`'s text with any cancellation or deadline in
    /// its chain as Go's sentinel), else the test cause, else Go `context.Canceled`.
    pub(crate) fn cause(&self) -> Option<TraceError> {
        if !self.token.is_cancelled() {
            return None;
        }
        let cause = match &self.adk {
            Some(ctx) => ctx.cause().map(|c| trace_error_of_cause(&c)),
            None => self.cause.clone(),
        };
        Some(cause.unwrap_or_else(|| Arc::new(Error::Context(ContextError::Canceled))))
    }

    /// The Go closure `canceledFunc` returns: `(false, None)` while live; `(true, None)` when
    /// cancelled with Go's `context.Canceled` cause; `(false, Some(cause))` otherwise.
    pub(crate) fn check(&self) -> (bool, Option<TraceError>) {
        match self.cause() {
            None => (false, None),
            Some(cause) if matches!(*cause, Error::Context(ContextError::Canceled)) => (true, None),
            Some(cause) => (false, Some(cause)),
        }
    }
}

/// A gadk cancel cause as the trace manager keeps errors (Go `context.Cause`'s value).
pub(crate) fn trace_error_of_cause(cause: &gadk_agent::CancelCause) -> TraceError {
    match cause {
        gadk_agent::CancelCause::DeadlineExceeded => {
            Arc::new(Error::Context(ContextError::DeadlineExceeded))
        }
        gadk_agent::CancelCause::Error(e) => crate::plugin::callbacks::trace_error_of(e.as_ref()),
        // `Canceled` and any later variant read as Go's plain `context.Canceled`.
        _ => Arc::new(Error::Context(ContextError::Canceled)),
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
    /// Go `goroutineID`, `origin`, `attempt`: the fiber of the callback that started the span
    /// (`settle::current_goroutine`), zero when its context showed no activation.
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

    /// Whether `rec` is this record (Go `p == rec` where only the locked record is at hand):
    /// compares addresses without locking, so it is safe while `rec`'s guard is alive.
    pub(crate) fn is_record(&self, rec: &SpanRecord) -> bool {
        std::ptr::eq(self.0.data_ptr().cast_const(), rec)
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
        self.retired_causes.iter().find_map(|c| c.err.clone())
    }

    /// Go `clearRetiredCauseFor(name, branch, path)`: drops the swallowable causes that
    /// activations of `name` on `branch` (and on `path`; an empty path on either side matches
    /// any) retired with.
    pub(crate) fn clear_retired_cause_for(&mut self, name: &str, branch: &str, path: &str) {
        self.retired_causes.retain(|c| {
            !(c.swallowable_by_self
                && c.name == name
                && c.branch == branch
                && (path.is_empty() || c.path.is_empty() || c.path == path))
        });
    }

    /// Go `dropContexts`: drops `canceled`, `ended`, `activation`, `parent` and
    /// `retired_causes`.
    pub(crate) fn drop_contexts(&mut self) {
        self.canceled = None;
        self.ended = None;
        self.activation = None;
        self.parent = None;
        self.retired_causes = Vec::new();
    }

    /// Go `isActivationOf(ctx)`: an agent span whose activation is `activation` (non-nil).
    pub(crate) fn is_activation_of(&self, activation: Option<ActivationId>) -> bool {
        self.kind == SpanKind::Agent && self.activation.is_some() && self.activation == activation
    }

    /// Go `endedWithRun(runner)`: false whenever the parent's `goroutine_id` is 0. Locks the records above `self` one at a
    /// time (never `self`, whose guard the caller holds): each step reads `a` into a
    /// [`JoinView`] before it locks `a`'s parent (R10).
    pub(crate) fn ended_with_run(&self, runner: &str) -> bool {
        let Some(mut a_ref) = self.parent.clone() else {
            return false;
        };
        {
            let a = a_ref.lock();
            if !self.own_activation
                || a.kind != SpanKind::Agent
                || a.activation.is_none()
                || a.goroutine_id == 0
            {
                return false;
            }
            if a.run_done {
                return true;
            }
        }
        loop {
            let (a, a_has_activation, parent) = {
                let g = a_ref.lock();
                (JoinView::of(&g), g.activation.is_some(), g.parent.clone())
            };
            let Some(p_ref) = parent else {
                return false;
            };
            {
                let p = p_ref.lock();
                if p.kind == SpanKind::Invocation {
                    return a.name == runner && a_has_activation && a.goroutine_id != 0;
                }
                if p.kind != SpanKind::Agent
                    || a.goroutine_id == 0
                    || (a.goroutine_id != p.goroutine_id && !joins_once_returned_records(&p, &a))
                {
                    return false;
                }
            }
            a_ref = p_ref;
        }
    }

    /// Go `isNodeAgentOf(w)` (attribution).
    pub(crate) fn is_node_agent_of(&self, w: &SpanRecord) -> bool {
        if !w.is_workflow
            || !self.parent.as_ref().is_some_and(|p| p.is_record(w))
            || self.attempt != AttemptSite::Node
            || !self.origin.started_by(SCHEDULER_CREATOR, w.goroutine_id)
        {
            return false;
        }
        let node = match self.path.rfind('/') {
            Some(i) => &self.path[i + 1..],
            None => self.path.as_str(),
        };
        let node = match node.rfind('@') {
            Some(at) => &node[..at],
            None => node,
        };
        node == self.name
    }
}

/// The fields of a child record `c` that Go's `p.joins(c, …)` and `p.joinsOnceReturned(c)`
/// read, taken while only `c` is locked: the walks up a parent chain never hold a parent's
/// guard while locking its child (R10), so a record met again on the walk is never locked
/// twice.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct JoinView {
    pub(crate) goroutine_id: u64,
    pub(crate) origin: GoroutineOrigin,
    pub(crate) attempt: AttemptSite,
    pub(crate) name: String,
    pub(crate) branch: String,
}

impl JoinView {
    /// The join fields of `c`.
    pub(crate) fn of(c: &SpanRecord) -> JoinView {
        JoinView {
            goroutine_id: c.goroutine_id,
            origin: c.origin,
            attempt: c.attempt,
            name: c.name.clone(),
            branch: c.branch.clone(),
        }
    }
}

/// Go `(p *spanRecord).joinsOnceReturned(c)` (attribution). Locks `c`, then `p`, never both.
/// The trace manager's walks use [`joins_once_returned_records`]; this form is for tests.
#[cfg(test)]
pub(crate) fn joins_once_returned(p: &SpanRef, c: &SpanRef) -> bool {
    let c = JoinView::of(&c.lock());
    joins_once_returned_records(&p.lock(), &c)
}

/// Go `(p *spanRecord).joinsOnceReturned(c)` over the locked parent and the child's [`JoinView`].
pub(crate) fn joins_once_returned_records(p: &SpanRecord, c: &JoinView) -> bool {
    joins_records(p, c, false)
        || (p.is_parallel
            && p.goroutine_id != 0
            && c.origin.started_by(ERRGROUP_CREATOR, p.goroutine_id)
            && p.children.contains(&c.name)
            && c.branch == parallel_branch(p, &c.name))
        || (p.is_workflow && c.attempt == AttemptSite::Worker)
}

/// Go `(p *spanRecord).joins(c, reported)` (attribution; false when either has no goroutine).
/// Locks `c`, then `p`, never both. The trace manager's walks use [`joins_records`]; this form
/// is for tests.
#[cfg(test)]
pub(crate) fn joins(p: &SpanRef, c: &SpanRef, reported: bool) -> bool {
    let c = JoinView::of(&c.lock());
    joins_records(&p.lock(), &c, reported)
}

/// Go `(p *spanRecord).joins(c, reported)` over the locked parent and the child's [`JoinView`].
pub(crate) fn joins_records(p: &SpanRecord, c: &JoinView, reported: bool) -> bool {
    if !p.propagates_child_error || p.goroutine_id == 0 || c.goroutine_id == 0 {
        return false;
    }
    if c.goroutine_id == p.goroutine_id {
        return true;
    }
    if p.is_workflow {
        return c.origin.started_by(SCHEDULER_CREATOR, p.goroutine_id);
    }
    if p.is_parallel {
        return c.origin.started_by(ERRGROUP_CREATOR, p.goroutine_id)
            && p.children.contains(&c.name)
            && c.branch == parallel_branch(p, &c.name)
            && (p.completed || (reported && p.node_run_ended));
    }
    false
}

/// Go `parallelBranch(a, childName)`: the branch of a ParallelAgent `a`'s branch running
/// `child_name`.
pub(crate) fn parallel_branch(a: &SpanRecord, child_name: &str) -> String {
    if a.branch.is_empty() {
        format!("{}.{child_name}", a.name)
    } else {
        format!("{}.{}.{child_name}", a.branch, a.name)
    }
}

/// Go `latencyOf(rec, end)`.
pub(crate) fn latency_of(rec: &SpanRecord, end: DateTime<Utc>) -> Latency {
    Latency {
        total_ms: (end - rec.start).num_milliseconds(),
        time_to_first_token_ms: rec.first_token.map(|t| (t - rec.start).num_milliseconds()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use chrono::{Duration, TimeZone};

    use super::super::testkit::{adk_ctx, custom};
    use super::*;

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()
    }

    fn err(text: &str) -> TraceError {
        Arc::new(Error::Context(if text == "canceled" {
            ContextError::Canceled
        } else {
            ContextError::DeadlineExceeded
        }))
    }

    #[test]
    fn cancel_source_reports_how_the_context_ended() {
        let token = CancellationToken::new();
        let live = CancelSource::of_token(token.clone(), Some(err("deadline")));
        assert!(matches!(live.check(), (false, None)));
        token.cancel();
        let (canceled, cause) = live.check();
        assert!(!canceled);
        assert!(cause.unwrap().is_context(ContextError::DeadlineExceeded));
        let explicit = CancelSource::of_token(token.clone(), Some(err("canceled")));
        assert!(matches!(explicit.check(), (true, None)));
        let bare = CancelSource::of_token(token, None);
        assert!(matches!(bare.check(), (true, None)));
    }

    /// An ADK context's cause is read at each check (R9): a cause recorded after the record
    /// was made is seen, and each gadk cause maps to Go's.
    #[test]
    fn cancel_source_reads_an_adk_context_cause_lazily() {
        let base = adk_ctx(&custom("a", vec![]), "inv", None);
        let (ctx, guard) = base.with_agent_cancel();
        let src = CancelSource::of_context(&ctx);
        assert!(matches!(src.check(), (false, None)));
        assert!(src.cause().is_none());
        let boom: Arc<dyn std::error::Error + Send + Sync> =
            Arc::new(std::io::Error::other("failed to run sub-agent \"b\": boom"));
        ctx.cancel_with_cause(gadk_agent::CancelCause::Error(boom));
        let (canceled, cause) = src.check();
        assert!(!canceled);
        assert_eq!(
            cause.unwrap().to_string(),
            "failed to run sub-agent \"b\": boom"
        );
        drop(guard);

        let (ctx, _guard) = base.with_agent_timeout(std::time::Duration::ZERO);
        let (canceled, cause) = CancelSource::of_context(&ctx).check();
        assert!(!canceled);
        assert!(matches!(
            *cause.unwrap(),
            Error::Context(ContextError::DeadlineExceeded)
        ));

        let (ctx, guard) = base.with_agent_cancel();
        guard.cancel();
        assert!(matches!(
            CancelSource::of_context(&ctx).check(),
            (true, None)
        ));
    }

    #[test]
    fn retired_causes_first_and_cleared_for_an_activation() {
        let cause =
            |name: &str, path: &str, swallowable: bool, e: Option<TraceError>| RetiredCause {
                err: e,
                name: name.into(),
                branch: "b".into(),
                path: path.into(),
                swallowable_by_self: swallowable,
            };
        let first = err("deadline");
        let mut rec = SpanRecord {
            retired_causes: vec![
                cause("a", "p", true, None),
                cause("a", "p", true, Some(first.clone())),
                cause("a", "", true, Some(err("deadline"))),
                cause("a", "q", true, Some(err("deadline"))),
                cause("a", "p", false, Some(err("deadline"))),
                cause("z", "p", true, Some(err("deadline"))),
            ],
            ..SpanRecord::default()
        };
        assert!(Arc::ptr_eq(&rec.first_retired_cause().unwrap(), &first));
        rec.clear_retired_cause_for("a", "b", "p");
        let left: Vec<(&str, &str, bool)> = rec
            .retired_causes
            .iter()
            .map(|c| (c.name.as_str(), c.path.as_str(), c.swallowable_by_self))
            .collect();
        assert_eq!(
            left,
            vec![("a", "q", true), ("a", "p", false), ("z", "p", true)]
        );
        rec.clear_retired_cause_for("a", "b", "");
        assert_eq!(rec.retired_causes.len(), 2);
        assert!(SpanRecord::default().first_retired_cause().is_none());
    }

    #[test]
    fn drop_contexts_forgets_the_run() {
        let adk = adk_ctx(&custom("a", vec![]), "inv", None);
        let mut rec = SpanRecord {
            canceled: Some(CancelSource::of_token(CancellationToken::new(), None)),
            ended: Some(adk.clone()),
            activation: Some(adk.activation_id()),
            parent: Some(SpanRef::default()),
            retired_causes: vec![RetiredCause::default()],
            ..SpanRecord::default()
        };
        rec.drop_contexts();
        assert!(rec.canceled.is_none() && rec.ended.is_none() && rec.activation.is_none());
        assert!(rec.parent.is_none() && rec.retired_causes.is_empty());
    }

    #[test]
    fn is_activation_of_needs_an_agent_span_and_a_key() {
        let act = adk_ctx(&custom("a", vec![]), "inv", None).activation_id();
        let other = adk_ctx(&custom("a", vec![]), "inv", None).activation_id();
        let rec = SpanRecord {
            kind: SpanKind::Agent,
            activation: Some(act),
            ..SpanRecord::default()
        };
        assert!(rec.is_activation_of(Some(act)));
        assert!(!rec.is_activation_of(Some(other)));
        assert!(!rec.is_activation_of(None));
        let call = SpanRecord {
            kind: SpanKind::Llm,
            activation: Some(act),
            ..SpanRecord::default()
        };
        assert!(!call.is_activation_of(Some(act)));
        assert!(
            !SpanRecord {
                kind: SpanKind::Agent,
                ..SpanRecord::default()
            }
            .is_activation_of(None)
        );
    }

    #[test]
    fn latency_has_a_first_token_only_when_one_arrived() {
        let mut rec = SpanRecord {
            start: t0(),
            ..SpanRecord::default()
        };
        let end = t0() + Duration::milliseconds(1500);
        assert_eq!(
            latency_of(&rec, end),
            Latency {
                total_ms: 1500,
                time_to_first_token_ms: None
            }
        );
        rec.first_token = Some(t0() + Duration::milliseconds(250));
        assert_eq!(latency_of(&rec, end).time_to_first_token_ms, Some(250));
    }

    #[test]
    fn parallel_branch_names() {
        let mut a = SpanRecord {
            name: "par".into(),
            ..SpanRecord::default()
        };
        assert_eq!(parallel_branch(&a, "x"), "par.x");
        a.branch = "root".into();
        assert_eq!(parallel_branch(&a, "x"), "root.par.x");
    }

    #[test]
    fn started_by_needs_the_parent_goroutine_and_creator() {
        let origin = GoroutineOrigin {
            creator: SCHEDULER_CREATOR,
            parent: 7,
        };
        let p = SpanRecord {
            goroutine_id: 7,
            ..SpanRecord::default()
        };
        assert!(origin.started_by(SCHEDULER_CREATOR, p.goroutine_id));
        assert!(!origin.started_by(ERRGROUP_CREATOR, p.goroutine_id));
        assert!(!GoroutineOrigin::default().started_by("", SpanRecord::default().goroutine_id));
    }

    #[test]
    fn attempt_site_maps_the_engine_slot() {
        use gadk_agent::fiber::AttemptSite as Slot;
        assert_eq!(AttemptSite::from(None), AttemptSite::None);
        assert_eq!(AttemptSite::from(Some(Slot::Node)), AttemptSite::Node);
        assert_eq!(AttemptSite::from(Some(Slot::Worker)), AttemptSite::Worker);
    }

    /// With zero goroutine information no activation joins another and no open call ends with
    /// the run (Go's zero-information branches).
    #[test]
    fn attribution_is_false_without_goroutines() {
        let act = adk_ctx(&custom("root", vec![]), "inv", None).activation_id();
        let inv = SpanRef::new(SpanRecord {
            kind: SpanKind::Invocation,
            ..SpanRecord::default()
        });
        let agent = SpanRef::new(SpanRecord {
            kind: SpanKind::Agent,
            name: "root".into(),
            activation: Some(act),
            parent: Some(inv.clone()),
            propagates_child_error: true,
            is_parallel: true,
            completed: true,
            ..SpanRecord::default()
        });
        let child = SpanRef::new(SpanRecord {
            kind: SpanKind::Agent,
            parent: Some(agent.clone()),
            ..SpanRecord::default()
        });
        assert!(!joins(&agent, &child, true));
        assert!(!joins_once_returned(&agent, &child));
        let call = SpanRecord {
            kind: SpanKind::Llm,
            parent: Some(agent.clone()),
            own_activation: true,
            ..SpanRecord::default()
        };
        assert!(!call.ended_with_run("root"));
        assert!(!child.lock().is_node_agent_of(&agent.lock()));
    }

    /// The ported bodies, driven with goroutine information set by hand.
    #[test]
    fn attribution_bodies_with_goroutines() {
        let act = adk_ctx(&custom("root", vec![]), "inv", None).activation_id();
        let inv = SpanRef::new(SpanRecord {
            kind: SpanKind::Invocation,
            ..SpanRecord::default()
        });
        let root = SpanRef::new(SpanRecord {
            kind: SpanKind::Agent,
            name: "root".into(),
            activation: Some(act),
            goroutine_id: 1,
            parent: Some(inv.clone()),
            propagates_child_error: true,
            is_workflow: true,
            ..SpanRecord::default()
        });
        let node = SpanRef::new(SpanRecord {
            kind: SpanKind::Agent,
            name: "node".into(),
            path: "wf/node@1".into(),
            activation: Some(act),
            goroutine_id: 2,
            origin: GoroutineOrigin {
                creator: SCHEDULER_CREATOR,
                parent: 1,
            },
            attempt: AttemptSite::Node,
            parent: Some(root.clone()),
            ..SpanRecord::default()
        });
        assert!(joins(&root, &node, false));
        assert!(joins_once_returned(&root, &node));
        assert!(node.lock().is_node_agent_of(&root.lock()));
        let call = SpanRecord {
            kind: SpanKind::Tool,
            parent: Some(node.clone()),
            own_activation: true,
            ..SpanRecord::default()
        };
        assert!(call.ended_with_run("root"));
        assert!(!call.ended_with_run("other"));
        node.lock().run_done = true;
        assert!(call.ended_with_run("other"));
    }
}
