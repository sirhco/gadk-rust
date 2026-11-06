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
enum SubResult {
    Event(Box<Event>, oneshot::Sender<()>),
    Err(AgentError),
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
/// and is yielded once every sub-agent has finished. Dropping the stream cancels the group and
/// aborts every sub-agent task, which drops (tears down) its stream.
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
            // Go builds each sub-context with icontext.NewInvocationContext, so ending the
            // invocation in one branch does not end its siblings. Setting `agent`/`branch` in the
            // delta forks the end flag (a fresh flag initialised from the parent's), likewise.
            let sub_ctx = ctx.with_delta(ContextDelta {
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
        let funnel = tokio::spawn(async move {
            while set.join_next().await.is_some() {}
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
