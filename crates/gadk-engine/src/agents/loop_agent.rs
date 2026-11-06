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

//! Port of Go agent/workflowagents/loopagent.

use std::sync::Arc;

use futures::StreamExt;
use gadk_agent::{
    Agent, AgentConfig, AgentError, AgentState, AgentType, EventStream, InvocationContext,
};

/// Port of Go loopagent.Config: the configuration of a loop agent.
#[derive(Clone, Default)]
pub struct LoopAgentConfig {
    /// Basic agent setup; `run` must be `None`.
    pub agent: AgentConfig,
    /// With 0 the loop runs indefinitely or until a sub-agent escalates. A sub-agent error ends
    /// the loop regardless. The context is checked before each pass, and if it is cancelled the
    /// loop ends with the context's error. A loop agent with no sub-agents returns immediately
    /// without yielding any event or error.
    pub max_iterations: u32,
}

/// Port of Go loopagent.New: an agent that runs its sub-agents in sequence repeatedly, for
/// `max_iterations` passes or until a sub-agent escalates. Use it for repetition or iterative
/// refinement, such as revising code.
pub fn new(cfg: LoopAgentConfig) -> Result<Arc<dyn Agent>, AgentError> {
    if cfg.agent.run.is_some() {
        return Err(AgentError::Config(
            "LoopAgent doesn't allow custom Run implementations".into(),
        ));
    }
    let max = cfg.max_iterations;
    let mut agent_cfg = cfg.agent.clone();
    agent_cfg.run = Some(Arc::new(move |ctx: InvocationContext| run(ctx, max)));
    gadk_agent::new_with_state(
        agent_cfg,
        AgentState {
            agent_type: AgentType::LoopAgent,
            config: Some(Arc::new(cfg)),
            on_before_agent_callback_event: None,
            live: None,
        },
    )
    .map_err(|e| AgentError::Config(format!("failed to create base agent: {e}")))
}

/// Port of Go (*loopAgent).Run.
fn run(ctx: InvocationContext, max_iterations: u32) -> EventStream {
    Box::pin(async_stream::stream! {
        let subs: Vec<Arc<dyn Agent>> = ctx.agent().sub_agents().to_vec();
        // Without sub-agents there is nothing to run, escalate or fail: return at once in both
        // the bounded and unbounded case, as adk-python does.
        if subs.is_empty() {
            return;
        }
        let mut count = max_iterations;
        loop {
            // A pass can yield no events (for example when every sub-agent is empty), so the
            // consumer never gets a chance to stop the loop: stop on cancellation here (Go
            // ctx.Err()).
            if ctx.cancellation_token().is_cancelled() {
                yield Err(AgentError::Cancelled);
                return;
            }
            // An all-empty pass awaits nothing pending; give the runtime a turn so a cancellation
            // can land.
            tokio::task::yield_now().await;
            let mut should_exit = false;
            for sub in &subs {
                let mut s = sub.run(ctx.clone());
                while let Some(item) = s.next().await {
                    match item {
                        // A failed sub-agent ends the loop, as in adk-python: retrying it would
                        // repeat the same failure, forever without max_iterations.
                        Err(e) => {
                            yield Err(e);
                            return;
                        }
                        Ok(ev) => {
                            let escalate = ev.actions.escalate;
                            yield Ok(ev);
                            if escalate {
                                should_exit = true;
                            }
                        }
                    }
                }
                // Escalation ends the loop once the escalating sub-agent has finished.
                if should_exit {
                    return;
                }
            }
            if count > 0 {
                count -= 1;
                if count == 0 {
                    return;
                }
            }
        }
    })
}
