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

//! `LlmAgentState`, `reveal` (port of Go `internal/llminternal/agent.go` `State` and `Reveal`).

use std::sync::Arc;

use gadk_agent::{Agent, AgentState, AgentType, Tool};

use crate::llm::LlmAgentConfig;

/// Port of Go llminternal.State: the LlmAgent's state, stored as `AgentState.config`.
pub(crate) struct LlmAgentState {
    pub(crate) cfg: LlmAgentConfig,
    /// `cfg.tools` plus the task tools installed for sub-agents (Go appends to `State.Tools`).
    pub(crate) tools: parking_lot::RwLock<Vec<Arc<dyn Tool>>>,
}

impl LlmAgentState {
    /// A state whose tool list starts as `cfg.tools`.
    pub(crate) fn new(cfg: LlmAgentConfig) -> Self {
        let tools = cfg.tools.clone();
        Self {
            cfg,
            tools: parking_lot::RwLock::new(tools),
        }
    }

    /// Snapshot of the current tool list (no lock escapes).
    pub(crate) fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.read().clone()
    }
}

/// Port of Go llminternal.Reveal: the LlmAgent state of `agent`, if it is an LlmAgent.
pub(crate) fn reveal(agent: &dyn Agent) -> Option<&LlmAgentState> {
    reveal_state(agent.agent_state())
}

/// State-level variant of [`reveal`], used where only an `AgentState` is at hand (mode bindings,
/// tests).
pub(crate) fn reveal_state(state: &AgentState) -> Option<&LlmAgentState> {
    if state.agent_type != AgentType::LlmAgent {
        return None;
    }
    state.config.as_deref()?.downcast_ref::<LlmAgentState>()
}
