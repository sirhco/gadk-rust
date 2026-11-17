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

//! `LlmAgentState` and `reveal`: the per-agent state of an LLM agent and access to it.

use std::sync::Arc;

use gadk_agent::{Agent, AgentState, AgentType, Tool};

use crate::llm::LlmAgentConfig;

/// Allows one successful
/// live-mode tool injection per agent. Concurrent callers wait for the injection to finish; a
/// panicking injection unwinds through the guard with the flag still false, so a later caller
/// retries (`parking_lot` does not poison). `inject` must not re-enter `do_once` on the same
/// latch.
#[derive(Debug, Default)]
pub(crate) struct LiveModeToolInjection {
    done: parking_lot::Mutex<bool>,
}

impl LiveModeToolInjection {
    /// Runs `inject` at most once successfully, holding the lock while it runs.
    pub(crate) fn do_once(&self, inject: impl FnOnce()) {
        let mut done = self.done.lock();
        if *done {
            return;
        }
        inject();
        *done = true;
    }
}

/// The LLM agent's state, stored as `AgentState.config`.
pub(crate) struct LlmAgentState {
    pub(crate) cfg: LlmAgentConfig,
    /// `cfg.tools` plus the task tools installed for sub-agents.
    pub(crate) tools: parking_lot::RwLock<Vec<Arc<dyn Tool>>>,
    /// Guards the single live-mode tool injection; its lifetime
    /// follows the agent.
    pub(crate) live_injection: LiveModeToolInjection,
    /// What is appended to the instruction at run time (the live `task_completed` suffix). Kept
    pub(crate) instruction_suffix: parking_lot::RwLock<String>,
}

impl LlmAgentState {
    /// A state whose tool list starts as `cfg.tools`.
    pub(crate) fn new(cfg: LlmAgentConfig) -> Self {
        let tools = cfg.tools.clone();
        Self {
            cfg,
            tools: parking_lot::RwLock::new(tools),
            live_injection: LiveModeToolInjection::default(),
            instruction_suffix: parking_lot::RwLock::new(String::new()),
        }
    }

    /// The configured instruction plus whatever was appended to it.
    pub(crate) fn instruction(&self) -> String {
        let suffix = self.instruction_suffix.read();
        if suffix.is_empty() {
            return self.cfg.instruction.clone();
        }
        format!("{}{}", self.cfg.instruction, suffix.as_str())
    }

    /// Snapshot of the current tool list (no lock escapes).
    pub(crate) fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.read().clone()
    }
}

/// The LLM agent state of `agent`, if it is an LLM agent.
pub(crate) fn reveal(agent: &dyn Agent) -> Option<&LlmAgentState> {
    reveal_state(agent.agent_state())
}

/// Owned form of [`reveal`]: the agent's `LlmAgentState` behind its `Arc`, for streams that
/// outlive the borrow of the agent.
pub(crate) fn reveal_arc(agent: &dyn Agent) -> Option<Arc<LlmAgentState>> {
    let state = agent.agent_state();
    if state.agent_type != AgentType::LlmAgent {
        return None;
    }
    state.config.clone()?.downcast::<LlmAgentState>().ok()
}

/// State-level variant of [`reveal`], used where only an `AgentState` is at hand (mode bindings,
/// tests).
pub(crate) fn reveal_state(state: &AgentState) -> Option<&LlmAgentState> {
    if state.agent_type != AgentType::LlmAgent {
        return None;
    }
    state.config.as_deref()?.downcast_ref::<LlmAgentState>()
}
