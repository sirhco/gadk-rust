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

//! Port of Go agent/workflowagents/sequentialagent (`RunLive` is deferred to 1b-live).

use std::sync::Arc;

use futures::StreamExt;
use gadk_agent::{
    Agent, AgentConfig, AgentError, AgentState, AgentType, EventStream, InvocationContext,
};

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
