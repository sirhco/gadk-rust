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

//! Port of Go agent/workflowagents/sequentialagent.

use std::any::Any;
use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use gadk_agent::{
    Agent, AgentConfig, AgentError, AgentState, AgentType, Context, ContextDelta, EventStream,
    InvocationContext, LiveAgent, LiveRequest, LiveSession, Tool, ToolError,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::llm::state::reveal;
use crate::runner::wrap;
use crate::tool::{FunctionTool, FunctionToolConfig};

/// Port of Go sequentialagent.Config: the configuration of a sequential agent.
#[derive(Clone, Default)]
pub struct SequentialAgentConfig {
    /// Basic agent setup; `run` must be `None`.
    pub agent: AgentConfig,
}

/// Port of Go sequentialagent.New: an agent that runs its sub-agents once, in the order they are
/// listed. Use it when execution must happen in a fixed, strict order.
pub fn new(cfg: SequentialAgentConfig) -> Result<Arc<dyn Agent>, AgentError> {
    if cfg.agent.run.is_some() {
        return Err(AgentError::Config(
            "SequentialAgent doesn't allow custom Run implementations".into(),
        ));
    }
    let mut agent_cfg = cfg.agent.clone();
    agent_cfg.run = Some(Arc::new(run));
    gadk_agent::new_with_state(
        agent_cfg,
        AgentState {
            agent_type: AgentType::SequentialAgent,
            config: Some(Arc::new(cfg)),
            on_before_agent_callback_event: None,
            // Go: New returns *seqAgent, which has RunLive.
            live: Some(Arc::new(SequentialLive)),
        },
    )
    .map_err(|e| AgentError::Config(format!("failed to create base agent: {e}")))
}

/// Port of Go (*sequentialAgent).Run.
fn run(ctx: InvocationContext) -> EventStream {
    Box::pin(async_stream::stream! {
        let subs: Vec<Arc<dyn Agent>> = ctx.agent().sub_agents().to_vec();
        for sub in subs {
            let mut s = sub.run(ctx.clone());
            while let Some(item) = s.next().await {
                // A failed sub-agent ends the sequence, as in adk-python: later sub-agents would
                // run without the output they depend on.
                let failed = item.is_err();
                yield item;
                if failed {
                    return;
                }
            }
        }
    })
}

/// Go's `task_completed` tool name.
const TASK_COMPLETED_NAME: &str = "task_completed";
/// Go's `task_completed` tool description.
const TASK_COMPLETED_DESCRIPTION: &str =
    "Signals that the agent has successfully completed the user's question or task.";
/// Go's `instructionSuffix` (agent.go:166), appended once to each injected LLM agent.
pub(crate) const TASK_COMPLETED_INSTRUCTION_SUFFIX: &str = "\nIf you finished the user's request according to its description, call the task_completed function to exit so the next agents can take over. When calling this function, do not generate any text other than the function call.";

// Go `type taskCompletedArgs struct{}`. A plain comment: a doc comment would become the schema's
// `description`, which Go's inferred `struct{}` schema does not have.
#[derive(Deserialize, JsonSchema)]
struct TaskCompletedArgs {}

// Go `taskCompletedResults{Result string `json:"result"`}`.
#[derive(Serialize, JsonSchema)]
struct TaskCompletedResults {
    result: String,
}

/// Go's `functiontool.New(functiontool.Config{Name: "task_completed", …})` (agent.go:139-147).
fn task_completed_tool() -> Result<Arc<dyn Tool>, ToolError> {
    FunctionTool::new(
        FunctionToolConfig::new(TASK_COMPLETED_NAME, TASK_COMPLETED_DESCRIPTION),
        |_ctx: Context, _args: TaskCompletedArgs| async move {
            Ok::<_, ToolError>(TaskCompletedResults {
                result: "Task completion signaled.".into(),
            })
        },
    )
}

/// Go `injectSubAgents` (agent.go:150-176): each LLM agent gets the tool and the suffix exactly
/// once, through its `LiveModeToolInjection` latch (a caller-provided `task_completed` is left
/// untouched); live-capable composites with children are recursed into; anything else is left
/// alone. `visited` is keyed by the agent's `Arc` address (Go map[agent.Agent]bool).
fn inject_sub_agents(
    agents: &[Arc<dyn Agent>],
    tool: &Arc<dyn Tool>,
    visited: &mut HashSet<usize>,
) {
    for sub in agents {
        if !visited.insert(Arc::as_ptr(sub).cast::<()>() as usize) {
            continue;
        }
        if let Some(state) = reveal(&**sub) {
            // Live callers must observe the completed injection before using the shared tool
            // list and instruction: do_once blocks concurrent callers until it finishes.
            state.live_injection.do_once(|| {
                let has_completion_tool = state
                    .tools
                    .read()
                    .iter()
                    .any(|t| t.name() == TASK_COMPLETED_NAME);
                if has_completion_tool {
                    return;
                }
                state.tools.write().push(Arc::clone(tool));
                state
                    .instruction_suffix
                    .write()
                    .push_str(TASK_COMPLETED_INSTRUCTION_SUFFIX);
            });
        } else if sub.as_live().is_some() && !sub.sub_agents().is_empty() {
            inject_sub_agents(sub.sub_agents(), tool, visited);
        }
    }
}

/// Go `sequentialLiveSession`: forwards `send` to the active sub-agent's session.
///
/// `close()` closes only the active sub-session; it does not stop the sequence. Once that
/// sub-agent's stream ends, the next sub-agent starts, but `send` keeps refusing with
/// "session is closed" because the closed flag stays set. This is Go's behaviour
/// (sequentialagent/agent.go:186-218); to end the whole run, drop the event stream.
#[derive(Default)]
pub(crate) struct SequentialLiveSession {
    /// (active sub-session, closed).
    state: parking_lot::Mutex<(Option<Arc<dyn LiveSession>>, bool)>,
}

impl SequentialLiveSession {
    /// Go setActiveSession.
    fn set_active(&self, sess: Arc<dyn LiveSession>) {
        self.state.lock().0 = Some(sess);
    }
}

#[async_trait]
impl LiveSession for SequentialLiveSession {
    /// Go Send. The lock is released before the forwarded send is awaited.
    async fn send(&self, req: LiveRequest) -> Result<(), AgentError> {
        let active = {
            let g = self.state.lock();
            if g.1 {
                return Err(AgentError::other("session is closed"));
            }
            match &g.0 {
                Some(s) => Arc::clone(s),
                None => return Err(AgentError::other("no active sub-agent live session")),
            }
        };
        active.send(req).await
    }

    /// Go Close: marks the session closed and closes the active sub-session.
    async fn close(&self) -> Result<(), AgentError> {
        let active = {
            let mut g = self.state.lock();
            g.1 = true;
            g.0.clone()
        };
        match active {
            Some(s) => s.close().await,
            None => Ok(()),
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Closes a sub-session: inline through [`CloseOnDrop::close`], or, when the consumer drops the
/// stream mid-sub-agent (Go: `!yield` → `subSess.Close()`), from a spawned task.
struct CloseOnDrop(Option<Arc<dyn LiveSession>>);

impl CloseOnDrop {
    async fn close(mut self) {
        if let Some(s) = self.0.take()
            && let Err(e) = s.close().await
        {
            tracing::warn!("error closing sub-session: {e}");
        }
    }
}

impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        let Some(s) = self.0.take() else {
            return;
        };
        if let Ok(h) = tokio::runtime::Handle::try_current() {
            h.spawn(async move {
                if let Err(e) = s.close().await {
                    tracing::warn!("error closing sub-session: {e}");
                }
            });
        }
    }
}

/// Go's `SequentialAgent` live entry point (`seqAgent.RunLive`), installed as `AgentState.live`
/// by [`new`]. Stateless: it reads the sub-agents from `ctx.agent()`.
pub(crate) struct SequentialLive;

#[async_trait]
impl LiveAgent for SequentialLive {
    /// Port of Go agent/workflowagents/sequentialagent/agent.go:132 (*sequentialAgent).RunLive.
    ///
    /// Before any sub-agent runs, every LLM sub-agent (recursing into live-capable composites
    /// with children) is given the `task_completed` tool and an instruction suffix telling the
    /// model to call it. As in Go this injection is permanent: it changes that agent's later
    /// non-live runs too, and it happens at most once per agent (a panicking injection is retried
    /// by the next caller).
    ///
    /// The returned stream runs each sub-agent's live session in turn; the returned session
    /// forwards `send` to the active one. A sub-agent that is not live-capable ends the stream
    /// with `sub-agent <name> does not support Live Run` when its turn comes, after the earlier
    /// sub-agents have run (Go-parity: Go checks lazily, per sub-agent). A sub-agent's error is
    /// forwarded and ends the sequence, closing that sub-agent's live session, as in Run.
    async fn run_live(
        &self,
        ctx: InvocationContext,
    ) -> Result<(Arc<dyn LiveSession>, EventStream), AgentError> {
        let sub_agents: Vec<Arc<dyn Agent>> = ctx.agent().sub_agents().to_vec();
        if sub_agents.is_empty() {
            return Err(AgentError::other("sequential agent has no sub-agents"));
        }
        let tool =
            task_completed_tool().map_err(|e| wrap("failed to create task_completed tool", e))?;
        inject_sub_agents(&sub_agents, &tool, &mut HashSet::new());

        let seq_sess = Arc::new(SequentialLiveSession::default());
        let stream = sequential_live_stream(ctx, sub_agents, Arc::clone(&seq_sess));
        Ok((seq_sess, stream))
    }
}

/// Go's `wrappedIter` (agent.go:180-218).
fn sequential_live_stream(
    ctx: InvocationContext,
    sub_agents: Vec<Arc<dyn Agent>>,
    seq_sess: Arc<SequentialLiveSession>,
) -> EventStream {
    Box::pin(async_stream::stream! {
        for sub in sub_agents {
            let Some(live) = sub.as_live() else {
                yield Err(AgentError::other(format!(
                    "sub-agent {} does not support Live Run",
                    sub.name()
                )));
                return;
            };
            let sub_ctx = ctx.with_delta(ContextDelta {
                agent: Some(Arc::clone(&sub)),
                ..Default::default()
            });
            let (sub_sess, mut inner) = match live.run_live(sub_ctx).await {
                Ok(v) => v,
                Err(e) => {
                    yield Err(wrap(format!("sub-agent {} RunLive failed", sub.name()), e));
                    return;
                }
            };
            seq_sess.set_active(Arc::clone(&sub_sess));
            // Dropping the stream here closes the sub-session from a spawned task.
            let guard = CloseOnDrop(Some(sub_sess));
            while let Some(item) = inner.next().await {
                // A failed sub-agent ends the sequence, as in Run (Go 82b64f45): its live session
                // is closed and no later sub-agent starts.
                let failed = item.is_err();
                yield item;
                if failed {
                    guard.close().await;
                    return;
                }
            }
            guard.close().await;
        }
    })
}
