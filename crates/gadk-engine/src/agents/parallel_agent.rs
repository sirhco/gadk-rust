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

//! Port of Go agent/workflowagents/parallelagent.

use std::sync::Arc;

use futures::StreamExt;
use gadk_agent::fiber::{self, ERRGROUP_CREATOR};
use gadk_agent::{
    Agent, AgentConfig, AgentError, AgentState, AgentType, CancelCause, ContextDelta, EventStream,
    InvocationContext, go_quote,
};
use gadk_session::Event;
use parking_lot::Mutex;
use tokio::sync::{mpsc, oneshot};
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

/// Port of Go parallelagent.Config: the configuration of a parallel agent.
#[derive(Clone, Default)]
pub struct ParallelAgentConfig {
    /// Basic agent setup; `run` must be `None`.
    pub agent: AgentConfig,
}

/// Port of Go parallelagent.New: an agent that runs its sub-agents in parallel, each in an
/// isolated branch. Useful when a task benefits from multiple perspectives or attempts, such as
/// running different algorithms simultaneously or generating several responses for a later
/// evaluation agent. The base-agent error is returned unwrapped, as Go does.
pub fn new(cfg: ParallelAgentConfig) -> Result<Arc<dyn Agent>, AgentError> {
    if cfg.agent.run.is_some() {
        return Err(AgentError::Config(
            "ParallelAgent doesn't allow custom Run implementations".into(),
        ));
    }
    let mut agent_cfg = cfg.agent.clone();
    agent_cfg.run = Some(Arc::new(run));
    gadk_agent::new_with_state(
        agent_cfg,
        AgentState {
            agent_type: AgentType::ParallelAgent,
            config: Some(Arc::new(cfg)),
            on_before_agent_callback_event: None,
            live: None,
        },
    )
}

/// Port of Go result: a sub-agent event with its acknowledgement channel, or the group's error.
/// `Panic` carries a sub-agent task's panic payload to the consumer, which re-raises it (Go: a
/// panicking sub-agent goroutine crashes the process; it never ends the run "successfully").
enum SubResult {
    Event(Box<Event>, oneshot::Sender<()>),
    Err(AgentError),
    Panic(Box<dyn std::any::Any + Send>),
}

/// Go `fmt.Errorf("failed to run sub-agent %q: %w", name, err)`.
#[derive(Debug)]
struct SubAgentError {
    name: String,
    source: AgentError,
}

impl std::fmt::Display for SubAgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "failed to run sub-agent {}: {}",
            go_quote(&self.name),
            self.source
        )
    }
}

impl std::error::Error for SubAgentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Go's deferred cleanup in the run iterator (agent.go:118-128, ADK #1186): when the run stream
/// is dropped, the funnel task (which ends only after every sub-agent task has returned, its
/// teardown included) is handed to the invocation's teardown registry instead of being aborted,
/// so the runner's after-run callbacks wait for every branch. Without a registry it is detached.
struct AwaitBranchesOnDrop {
    ctx: InvocationContext,
    funnel: Option<JoinHandle<()>>,
}

impl Drop for AwaitBranchesOnDrop {
    fn drop(&mut self) {
        if let Some(funnel) = self.funnel.take() {
            self.ctx.register_teardown(funnel);
        }
    }
}

/// Go `close(res.ackChan)` after `yield` returned, whether it returned true or false: sent when
/// the consumer asks for the next item, or when it drops the stream at this event.
struct AckOnDrop(Option<oneshot::Sender<()>>);

impl Drop for AckOnDrop {
    fn drop(&mut self) {
        if let Some(ack) = self.0.take() {
            let _ = ack.send(());
        }
    }
}

/// Port of Go parallelagent run.
///
/// Every sub-agent runs in its own task (on a fresh fiber spawned with [`ERRGROUP_CREATOR`], Go's
/// `errgroup.Go` goroutine) on a context with branch `<parent>.<sub>` (prefixed with the current
/// branch when there is one), the same invocation, and the group's cancel scope (Go
/// `errgroup.WithContext`, a `context.WithCancelCause` child of the run context). Each event is handed to the consumer with an acknowledgement, and the sub-agent does not
/// continue until the consumer asks for the next item, i.e. after it has processed the event (for
/// the runner: after plugins and the session append). The first sub-agent error, wrapped as
/// `failed to run sub-agent "<name>": <err>`, cancels the group with that error as its cause
/// (`CancelCause::Error`, the same `Arc` the yielded error carries) and is yielded once every
/// sub-agent has finished; once every sub-agent has returned, the group is cancelled plainly
/// (errgroup `Wait`; the first cause wins). A sub-agent panic cancels the group plainly and is
/// re-raised on the consumer's poll (`resume_unwind`, as the dynamic scheduler surfaces node
/// panics) once every sub-agent has finished. Dropping the stream (Go's early stop, agent.go:118-142)
/// acknowledges the event the consumer stopped on, cancels the group plainly and hands the
/// funnel, which waits for every sub-agent task to return, to the invocation's teardown
/// registry ([`InvocationContext::register_teardown`]): no sub-agent task is aborted, and the
/// runner's after-run callbacks run once they have all torn down.
fn run(ctx: InvocationContext) -> EventStream {
    Box::pin(async_stream::stream! {
        let cur = ctx.agent();
        // Go: context.WithCancel(ctx) + errgroup.WithContext (one scope here: the early-stop
        // cancel is plain, so the group reads `Canceled` either way); the guard cancels it
        // plainly on early stop.
        let (group_ctx, _cancel_on_drop) = ctx.clone().into_context().with_agent_cancel();
        let group = group_ctx.cancellation_token().clone();
        let (tx, mut rx) = mpsc::channel::<SubResult>(1);
        // Go errgroup `err` (first error, kept once): the shared value the cause carries.
        let first_err: Arc<Mutex<Option<CancelCause>>> = Arc::new(Mutex::new(None));
        let mut set = JoinSet::new();
        for sa in cur.sub_agents() {
            let mut branch = format!("{}.{}", cur.name(), sa.name());
            if !ctx.branch().is_empty() {
                branch = format!("{}.{}", ctx.branch(), branch);
            }
            // Go builds each sub-context with icontext.NewInvocationContext from only Artifacts,
            // Memory, Session, Branch, IsolationScope, Agent, UserContent, RunConfig and
            // InvocationID: a fresh, not-ended end flag (ending one branch does not end its
            // siblings) and zero path, run id, resumed inputs and output-for-ancestors. The
            // parent's isolation scope is kept (Go 543edd3d): a workflow node stamps it on every
            // event the branches emit and the LLM history filter matches it exactly, so an
            // unscoped branch would lose its own turns.
            // The context shares the group's cancel scope (Go `NewInvocationContext(errGroupCtx, …)`).
            let sub_ctx = group_ctx.as_invocation().with_delta(ContextDelta {
                fresh_invocation: true,
                agent: Some(Arc::clone(sa)),
                branch: Some(branch),
                isolation_scope: Some(ctx.isolation_scope()),
                ..Default::default()
            });
            let (sa, tx, group, first_err) = (Arc::clone(sa), tx.clone(), group.clone(), first_err.clone());
            // Go `errGroup.Go`: a goroutine started here, by the goroutine running the agent.
            let branch_fiber = fiber::spawn_child(ERRGROUP_CREATOR);
            set.spawn(branch_fiber.scope(async move {
                if let Err(e) = run_sub_agent(sub_ctx.clone(), sa.clone(), tx, group).await {
                    let wrapped = CancelCause::Error(Arc::new(SubAgentError {
                        name: sa.name().to_string(),
                        source: e,
                    }));
                    // errgroup `errOnce`: the first error is kept and cancels the group with it as
                    // the cause (a no-op if the group was already cancelled).
                    let mut first = first_err.lock();
                    if first.is_none() {
                        *first = Some(wrapped.clone());
                        drop(first);
                        sub_ctx.cancel_with_cause(wrapped);
                    }
                }
            }).in_current_span());
        }
        // The funnel: once every sub-agent has finished, report the first error; the channel
        // closes when the funnel and every sub-agent have dropped their senders (Go
        // close(resultsChan)).
        let funnel_group = group_ctx.clone();
        let funnel = tokio::spawn(async move {
            let mut panic = None;
            while let Some(res) = set.join_next().await {
                if let Err(je) = res
                    && je.is_panic()
                {
                    // Stop the siblings; the panic is re-raised once they have torn down.
                    funnel_group.cancel_with_cause(CancelCause::Canceled);
                    panic.get_or_insert(je.into_panic());
                }
            }
            // errgroup `Wait`: `cancel(g.err)` once every branch returned — plain when no branch
            // failed; the first cause wins otherwise.
            funnel_group.cancel_with_cause(CancelCause::Canceled);
            if let Some(p) = panic {
                if tx.send(SubResult::Panic(p)).await.is_err() {
                    // Stopped early: nobody is left to re-raise it.
                    tracing::warn!("adk: a parallel agent branch panicked after an early stop");
                }
                return;
            }
            let err = first_err.lock().take();
            if let Some(e) = err {
                // The yielded error holds the same `Arc` as the group's cause.
                let _ = tx.send(SubResult::Err(e.to_agent_error())).await;
            }
        }.in_current_span());
        // Declared after `_cancel_on_drop`, so a drop registers the funnel before the group is
        // cancelled (locals drop in reverse order; both happen before the runner's guard).
        let _await_branches = AwaitBranchesOnDrop { ctx: ctx.clone(), funnel: Some(funnel) };
        while let Some(res) = rx.recv().await {
            match res {
                SubResult::Event(ev, ack) => {
                    // Released when the consumer asks for the next item, i.e. after it has
                    // processed this one, or when it drops the stream here: the innermost
                    // local, so the ack is sent before the group is cancelled (Go closes the
                    // ack channel before its deferred `cancelSubAgents`).
                    let _ack = AckOnDrop(Some(ack));
                    yield Ok(*ev);
                }
                SubResult::Err(e) => yield Err(e),
                SubResult::Panic(p) => std::panic::resume_unwind(p),
            }
        }
    })
}

/// Port of Go runSubAgent. `Ok(())` also covers "the consumer went away" (Go `<-done`).
async fn run_sub_agent(
    ctx: InvocationContext,
    agent: Arc<dyn Agent>,
    tx: mpsc::Sender<SubResult>,
    group: CancellationToken,
) -> Result<(), AgentError> {
    let mut events = agent.run(ctx);
    while let Some(item) = events.next().await {
        let event = item?;
        let (ack_tx, ack_rx) = oneshot::channel();
        tokio::select! {
            biased;
            _ = group.cancelled() => return Err(AgentError::Cancelled),
            sent = tx.send(SubResult::Event(Box::new(event), ack_tx)) => if sent.is_err() { return Ok(()); },
        }
        // Wait for the consumer to finish processing before continuing. A ready ack wins over
        // the group's cancellation: a consumer that stops on this event acknowledges it before
        // cancelling the group, and Go's goroutine, parked in the select, wakes on the ack.
        tokio::select! {
            biased;
            acked = ack_rx => if acked.is_err() { return Ok(()); },
            _ = group.cancelled() => return Err(AgentError::Cancelled),
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Arc;

    use futures::StreamExt;
    use gadk_agent::{Agent, AgentConfig, ContextDelta, InvocationContext};
    use gadk_genai::{Content, ROLE_MODEL};
    use gadk_session::Event;
    use parking_lot::Mutex;

    use gadk_agent::fiber::{self, ERRGROUP_CREATOR, Fiber};
    use gadk_agent::{AgentError, CancelCause, SharedCauseError};

    use super::{ParallelAgentConfig, new, run};

    fn parallel(sub_agents: Vec<Arc<dyn Agent>>) -> Arc<dyn Agent> {
        new(ParallelAgentConfig {
            agent: AgentConfig {
                name: "par".into(),
                sub_agents,
                ..Default::default()
            },
        })
        .unwrap()
    }

    /// What a sub-agent's run observed: (isolation scope, ended, path, branch).
    type Seen = Arc<Mutex<Option<(String, bool, String, String)>>>;

    #[tokio::test]
    async fn sub_agent_context_is_a_fresh_invocation() {
        let seen: Seen = Arc::new(Mutex::new(None));
        let seen2 = seen.clone();
        let sub = gadk_agent::new_agent(AgentConfig {
            name: "sub".into(),
            run: Some(Arc::new(move |ctx: InvocationContext| {
                let c = ctx.clone().into_context();
                *seen2.lock() = Some((ctx.isolation_scope(), ctx.ended(), c.path(), ctx.branch()));
                let mut ev = Event::default();
                ev.response.content = Some(Content::from_text("hi", ROLE_MODEL));
                futures::stream::iter(vec![Ok(ev)]).boxed()
            })),
            ..Default::default()
        })
        .unwrap();
        let par = parallel(vec![sub]);
        // The parent is scoped, ended and carries a node path; Go's parallel child keeps only the
        // isolation scope (Go 543edd3d).
        let parent = InvocationContext::builder()
            .agent(par.clone())
            .isolation_scope("scope-1")
            .build()
            .with_delta(ContextDelta {
                path: Some("wf@1/par@1".into()),
                ..Default::default()
            });
        parent.end_invocation();
        // Call the run function directly: the agent wrapper would skip an ended invocation.
        let items: Vec<_> = run(parent.as_invocation().clone()).collect().await;
        assert_eq!(items.len(), 1);
        assert!(items[0].is_ok());
        assert_eq!(
            seen.lock().clone(),
            Some((
                "scope-1".to_string(),
                false,
                String::new(),
                "par.sub".to_string()
            ))
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn panicking_sub_agent_panics_the_run() {
        let sub = gadk_agent::new_agent(AgentConfig {
            name: "boom".into(),
            run: Some(Arc::new(|_ctx: InvocationContext| {
                Box::pin(async_stream::stream! {
                    if true {
                        panic!("sub-agent exploded");
                    }
                    yield Ok(Event::default());
                })
            })),
            ..Default::default()
        })
        .unwrap();
        let par = parallel(vec![sub]);
        let ictx = InvocationContext::builder()
            .agent(par.clone())
            .build_invocation();
        let handle = tokio::spawn(async move { par.run(ictx).collect::<Vec<_>>().await });
        let err = handle.await.expect_err("the run must not end successfully");
        assert!(err.is_panic());
        let payload = err.into_panic();
        let msg = payload
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_default();
        assert_eq!(msg, "sub-agent exploded");
    }

    /// A sub-agent that records the fiber and context its run sees, then ends (`fail`: with an
    /// error) or waits for the group to be cancelled first (`wait`).
    fn recorder(
        name: &str,
        behaviour: &'static str,
        seen: Arc<Mutex<Vec<(String, Fiber, InvocationContext)>>>,
    ) -> Arc<dyn Agent> {
        let name_owned = name.to_string();
        gadk_agent::new_agent(AgentConfig {
            name: name.into(),
            run: Some(Arc::new(move |ctx: InvocationContext| {
                let (seen, name) = (seen.clone(), name_owned.clone());
                Box::pin(async_stream::stream! {
                    seen.lock().push((name, fiber::current(), ctx.clone()));
                    match behaviour {
                        "fail" => yield Err(AgentError::other("boom")),
                        "wait" => ctx.cancellation_token().cancelled().await,
                        _ => {}
                    }
                })
            })),
            ..Default::default()
        })
        .unwrap()
    }

    /// R7: a failing branch cancels the group with `CancelCause::Error` carrying the wrapped
    /// `failed to run sub-agent "<name>": <err>` error — the same value the run yields.
    #[tokio::test]
    async fn a_failing_branch_cancels_the_group_with_the_wrapped_error() {
        let seen = Arc::default();
        let par = parallel(vec![
            recorder("waiter", "wait", Arc::clone(&seen)),
            recorder("failer", "fail", Arc::clone(&seen)),
        ]);
        let parent = InvocationContext::builder()
            .agent(par.clone())
            .build_invocation();
        let items: Vec<_> = run(parent.clone()).collect().await;
        assert_eq!(items.len(), 1);
        let err = items[0].as_ref().unwrap_err();
        let want = r#"failed to run sub-agent "failer": boom"#;
        assert_eq!(err.to_string(), want);
        let seen = seen.lock();
        assert_eq!(seen.len(), 2);
        for (name, _, ctx) in seen.iter() {
            match ctx.cause() {
                Some(CancelCause::Error(e)) => {
                    assert_eq!(e.to_string(), want, "{name}");
                    let AgentError::Other(yielded) = err else {
                        panic!("{err:?}");
                    };
                    let shared = yielded.downcast_ref::<SharedCauseError>().unwrap();
                    assert!(
                        Arc::ptr_eq(&shared.0, &e),
                        "{name}: the cause is the yielded error"
                    );
                }
                other => panic!("{name}: cause = {other:?}"),
            }
        }
        assert!(parent.cause().is_none(), "the run context stays live");
    }

    /// R7 (errgroup `Wait`): once every branch returned without error the group reads a plain
    /// `Canceled`, while the run context stays live.
    #[tokio::test]
    async fn the_group_is_cancelled_plainly_after_every_branch_returned() {
        let seen = Arc::default();
        let par = parallel(vec![
            recorder("a", "ok", Arc::clone(&seen)),
            recorder("b", "ok", Arc::clone(&seen)),
        ]);
        let parent = InvocationContext::builder()
            .agent(par.clone())
            .build_invocation();
        let items: Vec<_> = run(parent.clone()).collect().await;
        assert!(items.is_empty());
        for (name, _, ctx) in seen.lock().iter() {
            assert!(matches!(ctx.cause(), Some(CancelCause::Canceled)), "{name}");
        }
        assert!(parent.cause().is_none());
    }

    /// R3: each branch runs on its own fiber started (`ERRGROUP_CREATOR`) by the fiber polling
    /// the run.
    #[tokio::test]
    async fn every_branch_runs_on_an_errgroup_fiber() {
        let seen = Arc::default();
        let par = parallel(vec![
            recorder("a", "ok", Arc::clone(&seen)),
            recorder("b", "ok", Arc::clone(&seen)),
        ]);
        let parent = InvocationContext::builder()
            .agent(par.clone())
            .build_invocation();
        let outer = Fiber::root();
        let outer_id = outer.id;
        let _: Vec<_> = outer.scope(run(parent).collect::<Vec<_>>()).await;
        let seen = seen.lock();
        assert_eq!(seen.len(), 2);
        for (name, f, _) in seen.iter() {
            assert_eq!(f.origin.creator, ERRGROUP_CREATOR, "{name}");
            assert_eq!(f.origin.parent, outer_id, "{name}");
        }
        assert_ne!(seen[0].1.id, seen[1].1.id);
    }
}
