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

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use gadk_agent::ActivationId;
use parking_lot::{Mutex, MutexGuard};
use tokio_util::sync::CancellationToken;

use super::TraceError;
use crate::context::ContextError;
use crate::error::Error;

/// Go `schedulerCreator`: the spawn site of the goroutine (task) on which a workflow agent's
/// scheduler runs a node. Goroutine attribution is 7b-3b: no record carries an origin yet, so
/// [`GoroutineOrigin::started_by`] never matches it in 7b-3a.
pub(crate) const SCHEDULER_CREATOR: &str = "gadk_engine::workflow::scheduler::start_node";
/// Go `errgroupCreator`: the spawn site of a ParallelAgent's branch (7b-3b, as above).
pub(crate) const ERRGROUP_CREATOR: &str = "gadk_engine::agents::parallel_agent::branch";

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
        if !self.token.is_cancelled() {
            return (false, None);
        }
        match &self.cause {
            None => (true, None),
            Some(cause) if matches!(**cause, Error::Context(ContextError::Canceled)) => {
                (true, None)
            }
            Some(cause) => (false, Some(cause.clone())),
        }
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

    /// Go `endedWithRun(runner)`: false whenever the parent's `goroutine_id` is 0, i.e.
    /// always in 7b-3a (the body is ported whole). Locks the records above `self` one pair at
    /// a time (never `self`, whose guard the caller holds).
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
            let Some(p_ref) = a_ref.lock().parent.clone() else {
                return false;
            };
            {
                let a = a_ref.lock();
                let p = p_ref.lock();
                if p.kind == SpanKind::Invocation {
                    return a.name == runner && a.activation.is_some() && a.goroutine_id != 0;
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

    /// Go `isNodeAgentOf(w)` (attribution; false with zero goroutine information).
    pub(crate) fn is_node_agent_of(&self, w: &SpanRecord) -> bool {
        if !w.is_workflow
            || !self.parent.as_ref().is_some_and(|p| p.is_record(w))
            || self.attempt != AttemptSite::Node
            || !self.origin.started_by(SCHEDULER_CREATOR, Some(w))
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

/// Go `(p *spanRecord).joinsOnceReturned(c)` (attribution).
#[allow(dead_code)] // consumer: 7b-3b (goroutine attribution)
pub(crate) fn joins_once_returned(p: &SpanRef, c: &SpanRef) -> bool {
    let (p, c) = (p.lock(), c.lock());
    joins_once_returned_records(&p, &c)
}

/// [`joins_once_returned`] over locked records.
pub(crate) fn joins_once_returned_records(p: &SpanRecord, c: &SpanRecord) -> bool {
    joins_records(p, c, false)
        || (p.is_parallel
            && p.goroutine_id != 0
            && c.origin.started_by(ERRGROUP_CREATOR, Some(p))
            && p.children.contains(&c.name)
            && c.branch == parallel_branch(p, &c.name))
        || (p.is_workflow && c.attempt == AttemptSite::Worker)
}

/// Go `(p *spanRecord).joins(c, reported)` (attribution; with zero goroutine information only
/// the `completed` / `runDone` and parallel-branch fallbacks apply).
#[allow(dead_code)] // consumer: 7b-3b (goroutine attribution)
pub(crate) fn joins(p: &SpanRef, c: &SpanRef, reported: bool) -> bool {
    let (p, c) = (p.lock(), c.lock());
    joins_records(&p, &c, reported)
}

/// [`joins`] over locked records.
pub(crate) fn joins_records(p: &SpanRecord, c: &SpanRecord, reported: bool) -> bool {
    if !p.propagates_child_error || p.goroutine_id == 0 || c.goroutine_id == 0 {
        return false;
    }
    if c.goroutine_id == p.goroutine_id {
        return true;
    }
    if p.is_workflow {
        return c.origin.started_by(SCHEDULER_CREATOR, Some(p));
    }
    if p.is_parallel {
        return c.origin.started_by(ERRGROUP_CREATOR, Some(p))
            && p.children.contains(&c.name)
            && c.branch == parallel_branch(p, &c.name)
            && (p.completed || (reported && p.node_run_ended));
    }
    false
}

impl GoroutineOrigin {
    /// Go `startedBy(creator, p)`: false when `p` is `None` or has no goroutine.
    pub(crate) fn started_by(&self, creator: &str, p: Option<&SpanRecord>) -> bool {
        p.is_some_and(|p| {
            p.goroutine_id != 0 && self.parent == p.goroutine_id && self.creator == creator
        })
    }
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
        let live = CancelSource {
            token: token.clone(),
            cause: Some(err("deadline")),
        };
        assert!(matches!(live.check(), (false, None)));
        token.cancel();
        let (canceled, cause) = live.check();
        assert!(!canceled);
        assert!(cause.unwrap().is_context(ContextError::DeadlineExceeded));
        let explicit = CancelSource {
            token: token.clone(),
            cause: Some(err("canceled")),
        };
        assert!(matches!(explicit.check(), (true, None)));
        let bare = CancelSource { token, cause: None };
        assert!(matches!(bare.check(), (true, None)));
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
            canceled: Some(CancelSource {
                token: CancellationToken::new(),
                cause: None,
            }),
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
            creator: SCHEDULER_CREATOR.into(),
            parent: 7,
        };
        let p = SpanRecord {
            goroutine_id: 7,
            ..SpanRecord::default()
        };
        assert!(origin.started_by(SCHEDULER_CREATOR, Some(&p)));
        assert!(!origin.started_by(ERRGROUP_CREATOR, Some(&p)));
        assert!(!origin.started_by(SCHEDULER_CREATOR, None));
        assert!(!GoroutineOrigin::default().started_by("", Some(&SpanRecord::default())));
    }

    /// With zero goroutine information (7b-3a) no activation joins another and no open call
    /// ends with the run.
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

    /// The ported bodies, driven with goroutine information set by hand (as 7b-3b will).
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
                creator: SCHEDULER_CREATOR.into(),
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
