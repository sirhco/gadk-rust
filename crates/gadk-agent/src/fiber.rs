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

//! Fibers: the execution identity of agent runs, gadk's substitute for the goroutine identity
//! ADK-Go's `plugin/agentanalytics/trace_manager.go` reads from `runtime.Stack` —
//! `currentGoroutine` / `currentGoroutineID` (`:4258-4278`, `:4421-4480`), `goroutineOrigin` /
//! `startedBy` (`:4280-4295`), the creators `schedulerCreator` / `errgroupCreator` (`:68-76`) and
//! `attemptSite` / `attemptSiteOfTrace` (`:4296-4412`).
//!
//! The engine gives every task on which it starts agent runs concurrently (Go: every goroutine it
//! starts) a fresh [`Fiber`] with [`spawn_child`], naming the spawn site with one of the creator
//! constants below; the runner's consumer stream carries one with [`FiberStream::consumer`]. Code
//! running under a fiber reads it with [`current`]. Agent runs that Go runs as push iterators on
//! the consumer's goroutine (nested agents, tools, the runner) are polled in the consumer's task
//! here too, so they share its fiber as they share Go's goroutine.
//!
//! The attempt site (Go `attemptSite`) is a slot on the fiber: the engine marks one pending
//! attempt with [`with_attempt`] around a workflow node attempt whose node is an agent node, and
//! the first agent run started on the fiber while no other agent run is active there claims it for
//! the duration of its before-agent callbacks ([`current_attempt`]).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context as TaskContext, Poll};

use futures::Stream;
use parking_lot::Mutex;

/// Go `schedulerCreator`: the spawn site of the task on which a workflow agent's scheduler runs a
/// node (gadk-engine `workflow::scheduler` `start_node`).
pub const SCHEDULER_CREATOR: &str = "gadk_engine::workflow::scheduler::start_node";
/// Go `errgroupCreator`: the spawn site of a ParallelAgent branch's task (gadk-engine
/// `agents::parallel_agent`, Go `errgroup.(*Group).Go`).
pub const ERRGROUP_CREATOR: &str = "gadk_engine::agents::parallel_agent::branch";
/// The spawn site of a ParallelWorker item's task (gadk-engine `workflow::nodes::parallel_worker`,
/// Go `(*ParallelWorker).Run`'s `go n.runWorker`); retries of the item stay on it.
pub const PARALLEL_WORKER_CREATOR: &str = "gadk_engine::workflow::nodes::parallel_worker::run";
/// The spawn site of a live session's task (gadk-engine `llm::live::flow`, Go `base_flow.go`
/// `RunLive`'s anonymous session goroutine).
pub const LIVE_SESSION_CREATOR: &str = "gadk_engine::llm::live::flow::run_live";

/// The bit that marks a fallback id (a tokio task id, see [`current`]); fiber ids are below it.
const FALLBACK_BIT: u64 = 1 << 63;

/// The next fiber id: process-wide, starting at 1, never reused (Go: goroutine ids are never
/// reused). Reaching [`FALLBACK_BIT`] would take 2^63 fibers.
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

tokio::task_local! {
    /// The fiber the running code belongs to.
    static CURRENT: Arc<FiberState>;
}

/// Go `goroutineOrigin`: the spawn site that started a fiber and the fiber it was started from.
/// The zero value (empty creator, parent 0) is Go's unknown origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Origin {
    /// The spawn site: one of this module's creator constants, or empty.
    pub creator: &'static str,
    /// The id of the fiber that was current when this one was started (0 = unknown).
    pub parent: u64,
}

impl Origin {
    /// Whether this is Go's zero `goroutineOrigin`.
    pub fn is_zero(&self) -> bool {
        self.creator.is_empty() && self.parent == 0
    }

    /// Go `goroutineOrigin.startedBy`: `creator` started this fiber while running on the fiber
    /// `parent` (never for an unknown parent, 0).
    pub fn started_by(&self, creator: &str, parent: u64) -> bool {
        parent != 0 && self.parent == parent && self.creator == creator
    }
}

/// Go `attemptSite` (non-`attemptNone` values): the agent run is the agent of a workflow node
/// attempt, run alone on its fiber.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AttemptSite {
    /// Go `attemptNode`: the agent of an agent node started by the workflow scheduler.
    Node,
    /// Go `attemptWorker`: the agent of an agent node wrapped by a ParallelWorker item attempt.
    Worker,
}

/// The attempt slot of a fiber.
#[derive(Debug, Default)]
struct AttemptSlot {
    /// Marked by [`with_attempt`] for one attempt, not yet claimed.
    pending: Option<AttemptSite>,
    /// The claimed site, visible through [`current_attempt`] during the claiming run's
    /// before-agent callbacks.
    current: Option<AttemptSite>,
    /// The agent runs started and not yet finished on this fiber (Go: `(*agent).Run` frames on
    /// the goroutine's stack).
    active_runs: usize,
}

/// The shared state of a fiber: what the task-local holds. It holds no [`Fiber`], so it drops
/// with the last fiber, scope or stream that refers to it.
#[derive(Debug)]
struct FiberState {
    id: u64,
    origin: Origin,
    slot: Mutex<AttemptSlot>,
}

impl FiberState {
    fn new(id: u64, origin: Origin) -> Arc<FiberState> {
        Arc::new(FiberState {
            id,
            origin,
            slot: Mutex::new(AttemptSlot::default()),
        })
    }
}

/// An execution identity (Go: a goroutine): its id, its origin and, for fibers made by this
/// module, the attempt slot it shares with every clone and every re-entry.
#[derive(Debug, Clone)]
pub struct Fiber {
    /// The fiber id: from 1 below `1 << 63` for fibers made by this module, `(1 << 63) | <tokio
    /// task id>` for a fallback, 0 when unknown.
    pub id: u64,
    /// Where the fiber was started (zero for roots and fallbacks).
    pub origin: Origin,
    /// `None` for fallback fibers.
    state: Option<Arc<FiberState>>,
}

impl Fiber {
    /// A new fiber with a new id and a zero origin.
    pub fn root() -> Fiber {
        Fiber::with_origin(Origin::default())
    }

    fn with_origin(origin: Origin) -> Fiber {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        Fiber {
            id,
            origin,
            state: Some(FiberState::new(id, origin)),
        }
    }

    /// The state to make current: a fallback fiber is given one with its id first.
    fn into_state(self) -> Arc<FiberState> {
        match self.state {
            Some(state) => state,
            None => FiberState::new(self.id, self.origin),
        }
    }

    /// Runs `fut` with this fiber current (every poll of it, and everything it polls in the same
    /// task).
    pub fn scope<F: Future>(self, fut: F) -> impl Future<Output = F::Output> {
        CURRENT.scope(self.into_state(), fut)
    }

    /// Wraps `stream` so that this fiber is current for every poll of it, whichever task polls it.
    pub fn instrument<S>(self, stream: S) -> FiberStream<S> {
        FiberStream {
            fiber: Some(self.into_state()),
            inner: stream,
        }
    }
}

/// The fiber the running code belongs to: the fiber of the innermost [`Fiber::scope`] or
/// [`FiberStream`]; else, in a tokio task, a fallback `{ id: (1 << 63) | <task id>, origin: zero }`
/// with no attempt slot; else `{ id: 0, origin: zero }` (Go's unknown goroutine).
// Divergence (ledger 7b-3a): fallback ids are tokio task ids, unique only among running tasks, and the id is 0 outside any task or fiber, where Go always has a goroutine id.
pub fn current() -> Fiber {
    CURRENT
        .try_with(|state| Fiber {
            id: state.id,
            origin: state.origin,
            state: Some(Arc::clone(state)),
        })
        .unwrap_or_else(|_| fallback())
}

fn fallback() -> Fiber {
    let id = tokio::task::try_id().map_or(0, |task| {
        // `tokio::task::Id` exposes its number only through `Display`; a parse failure still
        // yields a non-zero fallback id.
        FALLBACK_BIT | task.to_string().parse::<u64>().unwrap_or(0)
    });
    Fiber {
        id,
        origin: Origin::default(),
        state: None,
    }
}

/// A new fiber started by `creator` from the current fiber: a new id and `origin = { creator,
/// parent: current().id }` (Go: `created by <creator> in goroutine <parent>`).
pub fn spawn_child(creator: &'static str) -> Fiber {
    Fiber::with_origin(Origin {
        creator,
        parent: current().id,
    })
}

/// A stream polled under a fiber: [`Fiber::instrument`] fixes the fiber when wrapping,
/// [`FiberStream::consumer`] at the first poll. The fiber is current for every poll, so the
/// stream keeps it when it is moved to and polled from another task.
// Divergence (ledger 7b-3a): a started agent stream moved to another task keeps its fiber only when wrapped in a FiberStream, and agent streams polled concurrently in one task share that task's fiber (Go would run them on separate goroutines).
#[derive(Debug)]
#[must_use = "streams do nothing unless polled"]
pub struct FiberStream<S> {
    fiber: Option<Arc<FiberState>>,
    inner: S,
}

impl<S> FiberStream<S> {
    /// Wraps a consumer stream (Go: the iterator the caller ranges over on its goroutine): the
    /// first poll adopts the current fiber if one is set, else a new [`Fiber::root`], and every
    /// later poll re-enters it.
    pub fn consumer(stream: S) -> FiberStream<S> {
        FiberStream {
            fiber: None,
            inner: stream,
        }
    }

    /// The stream's fiber (`None` for a consumer stream not polled yet).
    pub fn fiber(&self) -> Option<Fiber> {
        self.fiber.as_ref().map(|state| Fiber {
            id: state.id,
            origin: state.origin,
            state: Some(Arc::clone(state)),
        })
    }
}

impl<S: Stream + Unpin> Stream for FiberStream<S> {
    type Item = S::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<S::Item>> {
        let this = self.get_mut();
        let state = this
            .fiber
            .get_or_insert_with(|| {
                CURRENT
                    .try_with(Arc::clone)
                    .unwrap_or_else(|_| Fiber::root().into_state())
            })
            .clone();
        let inner = &mut this.inner;
        CURRENT.sync_scope(state, || Pin::new(inner).poll_next(cx))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

/// Runs `fut` with `site` marked as the pending attempt of the current fiber (engine: around one
/// workflow node attempt whose node is an agent node). The mark is cleared (the previous one
/// restored) when `fut` completes or is dropped. Without a current fiber it does nothing.
pub async fn with_attempt<F: Future>(site: AttemptSite, fut: F) -> F::Output {
    let _pending = PendingAttempt::mark(site);
    fut.await
}

/// The pending mark of one [`with_attempt`].
struct PendingAttempt {
    state: Option<Arc<FiberState>>,
    prev: Option<AttemptSite>,
}

impl PendingAttempt {
    fn mark(site: AttemptSite) -> PendingAttempt {
        let state = CURRENT.try_with(Arc::clone).ok();
        let prev = state
            .as_ref()
            .and_then(|s| s.slot.lock().pending.replace(site));
        PendingAttempt { state, prev }
    }
}

impl Drop for PendingAttempt {
    fn drop(&mut self) {
        if let Some(state) = &self.state {
            state.slot.lock().pending = self.prev;
        }
    }
}

/// The attempt site claimed by the agent run whose before-agent callbacks are running on the
/// current fiber (Go `attemptSiteOfTrace` for a trace taken in `runBeforeAgentCallbacks`), else
/// `None`.
pub fn current_attempt() -> Option<AttemptSite> {
    CURRENT
        .try_with(|state| state.slot.lock().current)
        .ok()
        .flatten()
}

/// Go `attemptSiteOfTrace`'s "`(*agent).Run` exactly once on the stack": registers an agent run on
/// the current fiber and, if no other agent run is active there, moves the pending attempt to the
/// current one (else the current one is `None` for this nested run). Dropping the claim restores
/// the previous current attempt; [`AttemptClaim::into_active_run`] keeps the run registered.
pub(crate) fn claim_attempt() -> AttemptClaim {
    let Ok(state) = CURRENT.try_with(Arc::clone) else {
        return AttemptClaim {
            state: None,
            prev: None,
            run: None,
        };
    };
    let prev = {
        let mut slot = state.slot.lock();
        let prev = slot.current;
        slot.current = if slot.active_runs == 0 {
            slot.pending.take()
        } else {
            None
        };
        slot.active_runs += 1;
        prev
    };
    AttemptClaim {
        state: Some(Arc::clone(&state)),
        prev,
        run: Some(ActiveRun { state: Some(state) }),
    }
}

/// A claimed attempt, held for the duration of an agent run's before-agent callbacks.
#[must_use = "dropping the claim ends the attempt and the run's registration"]
pub(crate) struct AttemptClaim {
    state: Option<Arc<FiberState>>,
    prev: Option<AttemptSite>,
    run: Option<ActiveRun>,
}

impl AttemptClaim {
    /// Ends the attempt (restores the previous current attempt) and keeps the run registered as
    /// active on its fiber for as long as the returned guard lives.
    pub(crate) fn into_active_run(mut self) -> ActiveRun {
        self.run.take().unwrap_or_default()
    }
}

impl Drop for AttemptClaim {
    fn drop(&mut self) {
        if let Some(state) = &self.state {
            state.slot.lock().current = self.prev;
        }
    }
}

/// An agent run registered as active on a fiber; dropping it unregisters the run.
#[derive(Default)]
pub(crate) struct ActiveRun {
    state: Option<Arc<FiberState>>,
}

impl Drop for ActiveRun {
    fn drop(&mut self) {
        if let Some(state) = &self.state {
            let mut slot = state.slot.lock();
            slot.active_runs = slot.active_runs.saturating_sub(1);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use futures::StreamExt;

    use super::*;

    /// A stream whose items are the fiber current while each is produced.
    fn ids(n: usize) -> futures::stream::BoxStream<'static, u64> {
        futures::stream::iter(0..n).map(|_| current().id).boxed()
    }

    #[test]
    fn creators_are_the_engine_spawn_sites() {
        assert_eq!(
            SCHEDULER_CREATOR,
            "gadk_engine::workflow::scheduler::start_node"
        );
        assert_eq!(
            ERRGROUP_CREATOR,
            "gadk_engine::agents::parallel_agent::branch"
        );
        assert_eq!(
            PARALLEL_WORKER_CREATOR,
            "gadk_engine::workflow::nodes::parallel_worker::run"
        );
        assert_eq!(
            LIVE_SESSION_CREATOR,
            "gadk_engine::llm::live::flow::run_live"
        );
    }

    #[test]
    fn ids_strictly_increase_below_the_fallback_range() {
        let a = Fiber::root();
        let b = Fiber::root();
        let c = spawn_child(SCHEDULER_CREATOR);
        assert!(0 < a.id && a.id < b.id && b.id < c.id && c.id < FALLBACK_BIT);
        assert!(a.origin.is_zero() && b.origin.is_zero());
    }

    #[test]
    fn current_outside_any_runtime_is_unknown() {
        let f = current();
        assert_eq!(f.id, 0);
        assert!(f.origin.is_zero());
        assert_eq!(current_attempt(), None);
    }

    #[tokio::test]
    async fn current_in_a_test_body_is_unknown() {
        // A `#[tokio::test]` body runs in `block_on`, not in a task.
        assert_eq!(current().id, 0);
        assert!(current().origin.is_zero());
    }

    #[tokio::test]
    async fn current_in_a_spawned_task_is_a_fallback() {
        let (a, b) = tokio::join!(
            tokio::spawn(async { current() }),
            tokio::spawn(async { current() })
        );
        let (a, b) = (a.unwrap(), b.unwrap());
        assert!(
            a.id > FALLBACK_BIT && b.id > FALLBACK_BIT,
            "{} {}",
            a.id,
            b.id
        );
        assert!(a.origin.is_zero());
        let same = tokio::spawn(async { (current().id, current().id) })
            .await
            .unwrap();
        assert_eq!(same.0, same.1, "stable within one task");
    }

    #[tokio::test]
    async fn scope_makes_the_fiber_current() {
        let outer = Fiber::root();
        let inner = Fiber::root();
        let (outer_id, inner_id) = (outer.id, inner.id);
        let seen = outer
            .scope(async move {
                let before = current().id;
                let nested = inner.scope(async { current().id }).await;
                (before, nested, current().id)
            })
            .await;
        assert_eq!(seen, (outer_id, inner_id, outer_id));
        assert_eq!(current().id, 0, "scope ended");
    }

    #[tokio::test]
    async fn scoping_a_fallback_fiber_keeps_its_id() {
        let (fallback_id, scoped_id) = tokio::spawn(async {
            let f = current();
            let id = f.id;
            (id, f.scope(async { current().id }).await)
        })
        .await
        .unwrap();
        assert!(fallback_id > FALLBACK_BIT);
        assert_eq!(scoped_id, fallback_id);
    }

    #[tokio::test]
    async fn spawn_child_records_creator_and_parent() {
        let root = Fiber::root();
        let root_id = root.id;
        let child = root.scope(async { spawn_child(ERRGROUP_CREATOR) }).await;
        assert!(child.id > root_id);
        assert_eq!(
            child.origin,
            Origin {
                creator: ERRGROUP_CREATOR,
                parent: root_id,
            }
        );
        assert!(child.origin.started_by(ERRGROUP_CREATOR, root_id));
        assert!(!child.origin.started_by(SCHEDULER_CREATOR, root_id));
        assert!(!child.origin.started_by(ERRGROUP_CREATOR, 0));

        // Outside any fiber or task the parent is unknown.
        assert_eq!(spawn_child(SCHEDULER_CREATOR).origin.parent, 0);
        // In a task without a fiber the parent is the task's fallback id.
        let (task_id, parent) =
            tokio::spawn(async { (current().id, spawn_child(SCHEDULER_CREATOR).origin.parent) })
                .await
                .unwrap();
        assert!(task_id > FALLBACK_BIT);
        assert_eq!(parent, task_id);
    }

    #[tokio::test]
    async fn instrumented_stream_keeps_its_fiber_on_another_task() {
        let f = Fiber::root();
        let id = f.id;
        let mut s = f.instrument(ids(3));
        assert_eq!(s.fiber().map(|f| f.id), Some(id));
        assert_eq!(s.next().await, Some(id));
        let rest = tokio::spawn(async move { s.collect::<Vec<_>>().await })
            .await
            .unwrap();
        assert_eq!(rest, vec![id, id]);
    }

    #[tokio::test]
    async fn consumer_stream_adopts_the_current_fiber_at_first_poll() {
        let f = Fiber::root();
        let id = f.id;
        let mut s = FiberStream::consumer(ids(3));
        assert!(s.fiber().is_none(), "fixed at first poll, not when wrapped");
        let first = f.scope(async { s.next().await }).await;
        assert_eq!(first, Some(id));
        let rest = tokio::spawn(async move { s.collect::<Vec<_>>().await })
            .await
            .unwrap();
        assert_eq!(rest, vec![id, id]);
    }

    #[tokio::test]
    async fn consumer_stream_without_a_fiber_keeps_a_new_root() {
        let mut s = FiberStream::consumer(ids(3));
        let first = s.next().await.unwrap();
        assert!(0 < first && first < FALLBACK_BIT);
        let fiber = s.fiber().unwrap();
        assert_eq!(fiber.id, first);
        assert!(fiber.origin.is_zero());
        let rest = tokio::spawn(async move { s.collect::<Vec<_>>().await })
            .await
            .unwrap();
        assert_eq!(rest, vec![first, first]);
        assert_eq!(
            current().id,
            0,
            "the stream's fiber is current only while it is polled"
        );
    }

    #[tokio::test]
    async fn fiber_state_drops_with_its_last_user() {
        let f = Fiber::root();
        let weak = Arc::downgrade(f.state.as_ref().unwrap());
        f.scope(async {
            let again = current();
            again.scope(async {}).await;
        })
        .await;
        assert!(
            weak.upgrade().is_none(),
            "no reference cycle keeps the state"
        );

        let f = Fiber::root();
        let weak = Arc::downgrade(f.state.as_ref().unwrap());
        let mut s = f.instrument(ids(1));
        assert!(s.next().await.is_some());
        drop(s);
        assert!(weak.upgrade().is_none());
    }

    #[tokio::test]
    async fn with_attempt_without_a_fiber_does_nothing() {
        let claimed = with_attempt(AttemptSite::Node, async {
            let _claim = claim_attempt();
            current_attempt()
        })
        .await;
        assert_eq!(claimed, None);
    }

    #[tokio::test]
    async fn pending_attempt_is_visible_only_once_claimed() {
        let seen = Fiber::root()
            .scope(with_attempt(AttemptSite::Node, async {
                let before = current_attempt();
                let claim = claim_attempt();
                let during = current_attempt();
                drop(claim);
                (before, during, current_attempt())
            }))
            .await;
        assert_eq!(seen, (None, Some(AttemptSite::Node), None));
    }

    #[tokio::test]
    async fn a_consumed_attempt_is_not_claimed_again() {
        let seen = Fiber::root()
            .scope(with_attempt(AttemptSite::Node, async {
                let first = {
                    let _claim = claim_attempt();
                    current_attempt()
                };
                let second = {
                    let _claim = claim_attempt();
                    current_attempt()
                };
                (first, second)
            }))
            .await;
        assert_eq!(seen, (Some(AttemptSite::Node), None));
    }

    #[tokio::test]
    async fn a_nested_run_sees_none_and_restores_the_outer_attempt() {
        let seen = Fiber::root()
            .scope(with_attempt(AttemptSite::Worker, async {
                let outer = claim_attempt();
                let outer_site = current_attempt();
                let inner_site = {
                    let _inner = claim_attempt();
                    current_attempt()
                };
                (outer_site, inner_site, current_attempt(), outer)
            }))
            .await;
        assert_eq!(
            (seen.0, seen.1, seen.2),
            (Some(AttemptSite::Worker), None, Some(AttemptSite::Worker))
        );
    }

    #[tokio::test]
    async fn an_active_run_blocks_claims_until_it_ends() {
        let f = Fiber::root();
        let seen = f
            .scope(async {
                let run = claim_attempt().into_active_run();
                // The run's callbacks are over, the run is still active (Go: its frame is still
                // on the stack while it yields).
                let while_active = with_attempt(AttemptSite::Node, async {
                    let _claim = claim_attempt();
                    current_attempt()
                })
                .await;
                drop(run);
                let after = with_attempt(AttemptSite::Node, async {
                    let _claim = claim_attempt();
                    current_attempt()
                })
                .await;
                (while_active, after)
            })
            .await;
        assert_eq!(seen, (None, Some(AttemptSite::Node)));
    }

    #[tokio::test]
    async fn re_entering_a_fiber_shares_its_slot() {
        let f = Fiber::root();
        let run = f
            .clone()
            .scope(async { claim_attempt().into_active_run() })
            .await;
        let while_active = f
            .clone()
            .scope(with_attempt(AttemptSite::Node, async {
                let _claim = claim_attempt();
                current_attempt()
            }))
            .await;
        drop(run);
        let after = f
            .scope(with_attempt(AttemptSite::Node, async {
                let _claim = claim_attempt();
                current_attempt()
            }))
            .await;
        assert_eq!((while_active, after), (None, Some(AttemptSite::Node)));
    }

    #[tokio::test]
    async fn the_mark_is_cleared_after_the_attempt() {
        let seen = Fiber::root()
            .scope(async {
                with_attempt(AttemptSite::Worker, async {}).await;
                let _claim = claim_attempt();
                current_attempt()
            })
            .await;
        assert_eq!(seen, None);
    }

    #[tokio::test]
    async fn each_retry_attempt_is_claimed() {
        let seen = Fiber::root()
            .scope(async {
                let mut seen = Vec::new();
                for _ in 0..2 {
                    seen.push(
                        with_attempt(AttemptSite::Worker, async {
                            let _claim = claim_attempt();
                            current_attempt()
                        })
                        .await,
                    );
                }
                seen
            })
            .await;
        assert_eq!(seen, vec![Some(AttemptSite::Worker); 2]);
    }
}
