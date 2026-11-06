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

//! Agent type and internal state (port of Go `internal/agent/state.go`).

use std::any::Any;
use std::sync::Arc;

use gadk_session::Event;

use crate::agent::LiveAgent;

/// Go `agentinternal.Type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentType {
    /// `"LLMAgent"`.
    LlmAgent,
    /// `"LoopAgent"`.
    LoopAgent,
    /// `"SequentialAgent"`.
    SequentialAgent,
    /// `"ParallelAgent"`.
    ParallelAgent,
    /// `"WorkflowAgent"`.
    WorkflowAgent,
    /// `"CustomAgent"`.
    CustomAgent,
    /// `"RemoteAgent"`.
    RemoteAgent,
}

impl AgentType {
    /// Go's string value of the type.
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentType::LlmAgent => "LLMAgent",
            AgentType::LoopAgent => "LoopAgent",
            AgentType::SequentialAgent => "SequentialAgent",
            AgentType::ParallelAgent => "ParallelAgent",
            AgentType::WorkflowAgent => "WorkflowAgent",
            AgentType::CustomAgent => "CustomAgent",
            AgentType::RemoteAgent => "RemoteAgent",
        }
    }
}

/// Go `agentinternal.State`: engine-owned per-agent state.
#[doc(hidden)]
#[derive(Clone)]
pub struct AgentState {
    /// The agent's type.
    pub agent_type: AgentType,
    /// The engine's per-type configuration.
    pub config: Option<Arc<dyn Any + Send + Sync>>,
    /// Go `State.OnBeforeAgentCallbackEvent`: called with the before-agent callback event before it
    /// is yielded.
    #[allow(clippy::type_complexity)] // the exact Appendix C3 type
    pub on_before_agent_callback_event: Option<Arc<dyn Fn(&mut Event) + Send + Sync>>,
    /// The live entry point (Go's optional `RunLive`); engine agents built with `new_with_state`
    /// set it, every other agent leaves it `None`.
    pub live: Option<Arc<dyn LiveAgent>>,
}

impl AgentState {
    /// Stable identity of the config allocation (Go uses the `*State` pointer); 0 when there is none.
    pub fn key(&self) -> usize {
        self.config
            .as_ref()
            .map_or(0, |c| Arc::as_ptr(c).cast::<()>() as usize)
    }
}

impl std::fmt::Debug for AgentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentState")
            .field("agent_type", &self.agent_type)
            .field("key", &self.key())
            .field("live", &self.live.is_some())
            .finish_non_exhaustive()
    }
}
