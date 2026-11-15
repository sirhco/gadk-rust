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
//! binding), plus the bound output schema (Go `boundOutputSchemaKey`, be529e7b), which rides the
//! same per-invocation map.

use std::collections::HashMap;
use std::sync::Arc;

use gadk_genai::Schema;

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
/// `boundModeKey{agent, state}`. A second map under the same key holds the output schema a typed
/// workflow node binds for the agent (Go `boundOutputSchemaKey{agent, state}`).
#[derive(Debug, Clone, Default)]
pub struct BoundModes {
    map: Arc<HashMap<(String, usize), Mode>>,
    output_schemas: Arc<HashMap<(String, usize), Option<Schema>>>,
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
        Self {
            map: Arc::new(map),
            output_schemas: Arc::clone(&self.output_schemas),
        }
    }

    /// The mode bound to the agent, if any.
    pub fn get(&self, agent_name: &str, state_key: usize) -> Option<Mode> {
        self.map.get(&(agent_name.to_string(), state_key)).copied()
    }

    /// Go `WithBoundOutputSchema` (be529e7b): a copy in which the agent is asked to answer with
    /// `schema`. `None` is bound too, so an inner placement shadows an outer one.
    pub fn with_output_schema(
        &self,
        agent_name: &str,
        state_key: usize,
        schema: Option<Schema>,
    ) -> Self {
        let mut output_schemas = (*self.output_schemas).clone();
        output_schemas.insert((agent_name.to_string(), state_key), schema);
        Self {
            map: Arc::clone(&self.map),
            output_schemas: Arc::new(output_schemas),
        }
    }

    /// The output schema bound to the agent: `None` when nothing is bound, `Some(None)` when a
    /// placement bound no schema (shadowing any outer binding).
    pub fn output_schema(&self, agent_name: &str, state_key: usize) -> Option<Option<&Schema>> {
        self.output_schemas
            .get(&(agent_name.to_string(), state_key))
            .map(Option::as_ref)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn object(prop: &str) -> Schema {
        serde_json::from_value(serde_json::json!({
            "type": "OBJECT",
            "properties": {prop: {"type": "STRING"}}
        }))
        .unwrap()
    }

    #[test]
    fn output_schema_binding_is_keyed_shadowable_and_kept_by_mode_bindings() {
        let outer = BoundModes::default().with_output_schema("a", 1, Some(object("x")));
        assert_eq!(outer.output_schema("a", 1), Some(Some(&object("x"))));
        assert_eq!(outer.output_schema("a", 2), None, "another state");
        assert_eq!(outer.output_schema("b", 1), None, "another name");
        // A mode binding keeps the schema binding and vice versa.
        let both = outer.with("a", 1, Mode::SingleTurn);
        assert_eq!(both.output_schema("a", 1), Some(Some(&object("x"))));
        assert_eq!(both.get("a", 1), Some(Mode::SingleTurn));
        // A None binding shadows the outer one; the outer copy is unchanged.
        let inner = both.with_output_schema("a", 1, None);
        assert_eq!(inner.output_schema("a", 1), Some(None));
        assert_eq!(inner.get("a", 1), Some(Mode::SingleTurn));
        assert_eq!(both.output_schema("a", 1), Some(Some(&object("x"))));
    }
}
