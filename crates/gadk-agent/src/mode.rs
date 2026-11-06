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

//! Agent modes (Go `llminternal.Mode`, hoisted so `ContextDelta` can name it, and the `boundModeKey`
//! binding).

use std::collections::HashMap;
use std::sync::Arc;

/// Go `llminternal.Mode`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Mode {
    /// No mode set (`""`).
    #[default]
    Unset,
    /// `"chat"`.
    Chat,
    /// `"task"`.
    Task,
    /// `"single_turn"`.
    SingleTurn,
}

impl Mode {
    /// Go's string value of the mode.
    pub fn as_str(&self) -> &'static str {
        match self {
            Mode::Unset => "",
            Mode::Chat => "chat",
            Mode::Task => "task",
            Mode::SingleTurn => "single_turn",
        }
    }
}

/// Immutable (copy-on-write) map (agent name, `AgentState::key()`) → [`Mode`]; Go
/// `boundModeKey{agent, state}`.
#[derive(Debug, Clone, Default)]
pub struct BoundModes {
    map: Arc<HashMap<(String, usize), Mode>>,
}

impl BoundModes {
    /// Go `WithBoundMode`: a copy with `mode` bound to the agent; [`Mode::Unset`] leaves the map
    /// unchanged.
    pub fn with(&self, agent_name: &str, state_key: usize, mode: Mode) -> Self {
        if mode == Mode::Unset {
            return self.clone();
        }
        let mut map = (*self.map).clone();
        map.insert((agent_name.to_string(), state_key), mode);
        Self { map: Arc::new(map) }
    }

    /// The mode bound to the agent, if any.
    pub fn get(&self, agent_name: &str, state_key: usize) -> Option<Mode> {
        self.map.get(&(agent_name.to_string(), state_key)).copied()
    }
}
