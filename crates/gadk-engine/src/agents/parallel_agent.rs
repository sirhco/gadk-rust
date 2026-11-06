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
use gadk_agent::{
    Agent, AgentConfig, AgentError, AgentState, AgentType, ContextDelta, EventStream,
    InvocationContext, go_quote,
};
use gadk_session::Event;
use parking_lot::Mutex;
use tokio::sync::{mpsc, oneshot};
use tokio::task::{AbortHandle, JoinSet};
use tokio_util::sync::CancellationToken;

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

/// Aborts the funnel task when the run stream is dropped.
struct AbortOnDrop(AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Port of Go parallelagent run.
///
/// Every sub-agent runs in its own task on a context with branch `<parent>.<sub>` (prefixed with
/// the current branch when there is one), the same invocation, and a shared group cancellation
/// token. Each event is handed to the consumer with an acknowledgement, and the sub-agent does not
/// continue until the consumer asks for the next item, i.e. after it has processed the event (for
/// the runner: after plugins and the session append). The first sub-agent error cancels the group
/// and is yielded once every sub-agent has finished. A sub-agent panic cancels the group and is
/// re-raised on the consumer's poll (`resume_unwind`, as the dynamic scheduler surfaces node
/// panics) once every sub-agent has finished. Dropping the stream cancels the group and aborts
/// every sub-agent task, which drops (tears down) its stream.
fn run(ctx: InvocationContext) -> EventStream {
    Box::pin(async_stream::stream! {
        let cur = ctx.agent();
        // Go: context.WithCancel(ctx) + errgroup.WithContext; cancelled on early stop.
        let group = ctx.cancellation_token().child_token();
        let _cancel_on_drop = group.clone().drop_guard();
        let (tx, mut rx) = mpsc::channel::<SubResult>(1);
        let first_err: Arc<Mutex<Option<AgentError>>> = Arc::new(Mutex::new(None));
        let mut set = JoinSet::new();
        for sa in cur.sub_agents() {
            let mut branch = format!("{}.{}", cur.name(), sa.name());
            if !ctx.branch().is_empty() {
                branch = format!("{}.{}", ctx.branch(), branch);
            }
            // Go builds each sub-context with icontext.NewInvocationContext from only Artifacts,
            // Memory, Session, Branch, Agent, UserContent, RunConfig and InvocationID: a fresh,
            // not-ended end flag (ending one branch does not end its siblings) and zero
            // isolation scope, path, run id, resumed inputs and output-for-ancestors.
            let sub_ctx = ctx.with_delta(ContextDelta {
                fresh_invocation: true,
                agent: Some(Arc::clone(sa)),
                branch: Some(branch),
                cancellation_token: Some(group.clone()),
                ..Default::default()
            });
            let (sa, tx, group, first_err) = (Arc::clone(sa), tx.clone(), group.clone(), first_err.clone());
            set.spawn(async move {
                if let Err(e) = run_sub_agent(sub_ctx, sa.clone(), tx, group.clone()).await {
                    let wrapped = AgentError::Other(Box::new(SubAgentError {
                        name: sa.name().to_string(),
                        source: e,
                    }));
                    // errgroup keeps the first error and cancels the group context.
                    first_err.lock().get_or_insert(wrapped);
                    group.cancel();
                }
            });
        }
        // The funnel: once every sub-agent has finished, report the first error; the channel
        // closes when the funnel and every sub-agent have dropped their senders (Go
        // close(resultsChan)).
        let funnel_group = group.clone();
        let funnel = tokio::spawn(async move {
            let mut panic = None;
            while let Some(res) = set.join_next().await {
                if let Err(je) = res
                    && je.is_panic()
                {
                    // Stop the siblings; the panic is re-raised once they have torn down.
                    funnel_group.cancel();
                    panic.get_or_insert(je.into_panic());
                }
            }
            if let Some(p) = panic {
                let _ = tx.send(SubResult::Panic(p)).await;
                return;
            }
            let err = first_err.lock().take();
            if let Some(e) = err {
                let _ = tx.send(SubResult::Err(e)).await;
            }
        });
        // Aborting the funnel drops `set`, which aborts every sub-agent task.
        let _abort_on_drop = AbortOnDrop(funnel.abort_handle());
        while let Some(res) = rx.recv().await {
            match res {
                SubResult::Event(ev, ack) => {
                    yield Ok(*ev);
                    // Reached only when the consumer asks for the next item, i.e. after it has
                    // processed this one (Go close(res.ackChan)).
                    let _ = ack.send(());
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
        // Wait for the consumer to finish processing before continuing.
        tokio::select! {
            biased;
            _ = group.cancelled() => return Err(AgentError::Cancelled),
            acked = ack_rx => if acked.is_err() { return Ok(()); },
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
        // The parent is scoped, ended and carries a node path; Go's parallel child sees none of it.
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
            Some((String::new(), false, String::new(), "par.sub".to_string()))
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
}
