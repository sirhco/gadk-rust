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

//! The LLM agent and its flow (port of ADK-Go `agent/llmagent` and `internal/llminternal`).

pub mod agent;
pub mod config;
#[allow(unused_imports)]
pub use config::*;

pub(crate) mod flow;
pub(crate) mod functions;
pub(crate) mod live;
pub(crate) mod mode;
pub(crate) mod node_wrapper;
pub(crate) mod processors;
pub(crate) mod state;
pub(crate) mod transfer;

use std::sync::Arc;

use gadk_agent::{Agent, Tool};

/// The tools of an LLM agent (Go `llminternal.Reveal(agent).Tools`: the configured tools plus any
/// the engine installed, such as task tools for sub-agents); `None` for any other agent. The
/// REST server's agent graph draws them.
pub fn agent_tools(agent: &dyn Agent) -> Option<Vec<Arc<dyn Tool>>> {
    state::reveal(agent).map(|s| s.tools())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use gadk_agent::AgentConfig;

    use super::*;
    use crate::agents::sequential_agent::{self, SequentialAgentConfig};
    use crate::tool::exit_loop;

    #[test]
    fn agent_tools_returns_an_llm_agents_tools_and_none_otherwise() {
        let llm = agent::new(LlmAgentConfig {
            name: "llm".into(),
            tools: vec![exit_loop()],
            ..Default::default()
        })
        .unwrap();
        let names: Vec<String> = agent_tools(&*llm)
            .unwrap()
            .iter()
            .map(|t| t.name().to_owned())
            .collect();
        assert_eq!(names, ["exit_loop"]);

        let seq = sequential_agent::new(SequentialAgentConfig {
            agent: AgentConfig {
                name: "seq".into(),
                sub_agents: vec![llm],
                ..Default::default()
            },
        })
        .unwrap();
        assert!(agent_tools(&*seq).is_none());
    }
}
