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
pub(crate) mod output_schema_binding;
pub use node_wrapper::run_llm_agent_as_node;
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

/// The configuration an LLM agent was built from (Go `llminternal.Reveal(agent)`, whose fields
/// the analytics plugin reads); `None` for any other agent. Engine-installed additions (task
/// tools, the appended instruction suffix) are not in it: see [`agent_tools`] and
/// [`agent_instructions`].
pub fn llm_agent_config(agent: &dyn Agent) -> Option<&LlmAgentConfig> {
    state::reveal(agent).map(|s| &s.cfg)
}

/// The instructions of an LLM agent (Go `llminternal.Reveal(agent).{Instruction,
/// GlobalInstruction}`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentInstructions {
    /// Go `State.Instruction`: the configured instruction plus whatever the engine appended to it
    /// at run time (the live `task_completed` suffix).
    pub instruction: String,
    /// Go `State.GlobalInstruction`: the configured global instruction.
    pub global_instruction: String,
}

/// The instructions of an LLM agent (Go `llminternal.Reveal(agent).{Instruction,
/// GlobalInstruction}`); `None` for any other agent. The A2A agent card describes an agent with
/// them.
pub fn agent_instructions(agent: &dyn Agent) -> Option<AgentInstructions> {
    state::reveal(agent).map(|s| AgentInstructions {
        instruction: s.instruction(),
        global_instruction: s.cfg.global_instruction.clone(),
    })
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

    #[test]
    fn llm_agent_config_returns_an_llm_agents_config_and_none_otherwise() {
        let llm = agent::new(LlmAgentConfig {
            name: "llm".into(),
            description: "Answers.".into(),
            instruction: "You answer questions.".into(),
            ..Default::default()
        })
        .unwrap();
        let cfg = llm_agent_config(&*llm).unwrap();
        assert_eq!(
            (
                cfg.name.as_str(),
                cfg.description.as_str(),
                cfg.instruction.as_str()
            ),
            ("llm", "Answers.", "You answer questions.")
        );

        let seq = sequential_agent::new(SequentialAgentConfig {
            agent: AgentConfig {
                name: "seq".into(),
                sub_agents: vec![llm],
                ..Default::default()
            },
        })
        .unwrap();
        assert!(llm_agent_config(&*seq).is_none());
    }

    #[test]
    fn agent_instructions_returns_an_llm_agents_instructions_and_none_otherwise() {
        let llm = agent::new(LlmAgentConfig {
            name: "llm".into(),
            instruction: "You answer questions.".into(),
            global_instruction: "You are helpful.".into(),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            agent_instructions(&*llm),
            Some(AgentInstructions {
                instruction: "You answer questions.".into(),
                global_instruction: "You are helpful.".into(),
            })
        );

        let seq = sequential_agent::new(SequentialAgentConfig {
            agent: AgentConfig {
                name: "seq".into(),
                sub_agents: vec![llm],
                ..Default::default()
            },
        })
        .unwrap();
        assert!(agent_instructions(&*seq).is_none());
    }

    #[test]
    fn agent_instructions_reads_the_configured_text_not_the_providers() {
        let provider: InstructionProvider =
            Arc::new(|_| Box::pin(async { Ok("dynamic".to_owned()) }));
        let llm = agent::new(LlmAgentConfig {
            name: "llm".into(),
            instruction_provider: Some(provider.clone()),
            global_instruction_provider: Some(provider),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            agent_instructions(&*llm),
            Some(AgentInstructions::default())
        );
    }

    #[test]
    fn agent_instructions_includes_the_appended_instruction_suffix() {
        let llm = agent::new(LlmAgentConfig {
            name: "llm".into(),
            instruction: "Base.".into(),
            ..Default::default()
        })
        .unwrap();
        state::reveal(&*llm)
            .unwrap()
            .instruction_suffix
            .write()
            .push_str(" Suffix.");
        assert_eq!(
            agent_instructions(&*llm).unwrap().instruction,
            "Base. Suffix."
        );
    }
}
