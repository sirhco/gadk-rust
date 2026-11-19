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

//! The conformance agent loader (agent names sorted, the first one is the root agent).

use std::collections::HashMap;
use std::sync::Arc;

use gadk_agent::{Agent, Loader, LoaderError};

/// The loader over the conformance agents.
pub struct ConformanceAgentLoader {
    agent_map: HashMap<String, Arc<dyn Agent>>,
    agents_names: Vec<String>,
}

/// Builds the loader with the names sorted (byte order); never fails. An empty map is
/// accepted (see [`Loader::root_agent`]).
pub fn new_conformance_agent_loader(
    agent_map: HashMap<String, Arc<dyn Agent>>,
) -> Result<Arc<dyn Loader>, LoaderError> {
    let mut agents_names: Vec<String> = agent_map.keys().cloned().collect();
    agents_names.sort();
    Ok(Arc::new(ConformanceAgentLoader {
        agent_map,
        agents_names,
    }))
}

/// Formats names as `[a b]`.
fn go_sprint_strings(names: &[String]) -> String {
    format!("[{}]", names.join(" "))
}

impl Loader for ConformanceAgentLoader {
    /// The sorted names.
    fn list_agents(&self) -> Vec<String> {
        self.agents_names.clone()
    }

    /// `agent {name} not found. Please specify one of those: {names}`.
    fn load_agent(&self, name: &str) -> Result<Arc<dyn Agent>, LoaderError> {
        match self.agent_map.get(name) {
            Some(agent) => Ok(agent.clone()),
            None => Err(LoaderError(format!(
                "agent {name} not found. Please specify one of those: {}",
                go_sprint_strings(&self.list_agents())
            ))),
        }
    }

    /// The agent of the first sorted name.
    ///
    /// # Panics
    ///
    /// On an empty loader: there is no agent to return, and `Loader::root_agent` returns an
    /// `Arc<dyn Agent>` with no empty value, so the empty case panics at the call. (A console
    /// launcher would reject the missing root agent with `root agent is required`.)
    /// `list_agents` / `load_agent` work on an empty loader.
    fn root_agent(&self) -> Arc<dyn Agent> {
        match self.agents_names.first() {
            Some(name) => self.agent_map[name].clone(),
            #[allow(clippy::panic)] // no root agent to return; see the doc comment
            None => panic!("conformance agent loader has no agents"),
        }
    }
}

#[cfg(test)]
mod tests {
    use gadk_agent::{AgentConfig, new_agent};
    use pretty_assertions::assert_eq;

    use super::*;

    fn agent(name: &str) -> Arc<dyn Agent> {
        new_agent(AgentConfig {
            name: name.to_owned(),
            ..Default::default()
        })
        .unwrap()
    }

    fn loader(names: &[&str]) -> Arc<dyn Loader> {
        // Map keys are folder names; the agents carry their own (here: `agent-<key>`).
        new_conformance_agent_loader(
            names
                .iter()
                .map(|n| ((*n).to_owned(), agent(&format!("agent-{n}"))))
                .collect(),
        )
        .unwrap()
    }

    #[test]
    fn names_are_sorted_bytewise_and_the_first_is_the_root() {
        let l = loader(&["tools_agent_002", "callback_agent_001", "Zeta", "_x", "a"]);
        assert_eq!(
            l.list_agents(),
            ["Zeta", "_x", "a", "callback_agent_001", "tools_agent_002"]
        );
        assert_eq!(l.root_agent().name(), "agent-Zeta");
    }

    #[test]
    fn load_agent_returns_the_mapped_agent() {
        let l = loader(&["b", "a"]);
        assert_eq!(l.load_agent("b").unwrap().name(), "agent-b");
        assert_eq!(l.load_agent("a").unwrap().name(), "agent-a");
    }

    #[test]
    fn load_agent_not_found_lists_the_names_with_go_v() {
        let l = loader(&["b", "a"]);
        assert_eq!(
            l.load_agent("c").err().unwrap().to_string(),
            "agent c not found. Please specify one of those: [a b]"
        );
        // Lookup is by key, not by the agent's own name.
        assert_eq!(
            l.load_agent("agent-a").err().unwrap().0,
            "agent agent-a not found. Please specify one of those: [a b]"
        );
    }

    #[test]
    fn empty_loader_lists_nothing_and_load_fails_like_go() {
        let l = loader(&[]);
        assert!(l.list_agents().is_empty());
        assert_eq!(
            l.load_agent("x").err().unwrap().to_string(),
            "agent x not found. Please specify one of those: []"
        );
    }

    #[test]
    #[should_panic(expected = "conformance agent loader has no agents")]
    fn empty_loader_root_agent_panics() {
        let _ = loader(&[]).root_agent();
    }
}
