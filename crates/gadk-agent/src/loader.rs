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

//! Port of ADK-Go `agent/loader.go`: the [`Loader`] a server uses to find the agent behind an
//! application name, with a single-agent ([`SingleLoader`]) and a multi-agent ([`MultiLoader`])
//! implementation.

use std::sync::Arc;

use crate::agent::Agent;

/// Port of Go `agent.Loader`: resolves application names to agents.
pub trait Loader: Send + Sync {
    /// The names of every agent this loader serves.
    fn list_agents(&self) -> Vec<String>;
    /// The agent named `name`, or an error naming the agents that exist.
    fn load_agent(&self, name: &str) -> Result<Arc<dyn Agent>, LoaderError>;
    /// The root agent.
    fn root_agent(&self) -> Arc<dyn Agent>;
}

/// A [`Loader`] failure; the text is Go's error text verbatim.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct LoaderError(pub String);

/// Port of Go `agent.NewSingleLoader`: a loader serving one root agent, under its own name and
/// under the empty name.
pub struct SingleLoader {
    root: Arc<dyn Agent>,
}

impl SingleLoader {
    /// A loader serving only `root`.
    pub fn new(root: Arc<dyn Agent>) -> Self {
        Self { root }
    }
}

impl Loader for SingleLoader {
    fn list_agents(&self) -> Vec<String> {
        vec![self.root.name().to_owned()]
    }

    fn load_agent(&self, name: &str) -> Result<Arc<dyn Agent>, LoaderError> {
        if name.is_empty() || name == self.root.name() {
            return Ok(self.root.clone());
        }
        Err(LoaderError(format!(
            "cannot load agent '{name}' - provide an empty string or use '{}'",
            self.root.name()
        )))
    }

    fn root_agent(&self) -> Arc<dyn Agent> {
        self.root.clone()
    }
}

/// Port of Go `agent.NewMultiLoader`: a loader serving a root agent and further agents by name.
///
/// [`Loader::list_agents`] and the not-found error's list name the agents (root included) sorted
/// alphabetically (Go #2164).
pub struct MultiLoader {
    agents: Vec<(String, Arc<dyn Agent>)>,
    root: Arc<dyn Agent>,
}

impl MultiLoader {
    /// A loader serving `root` and `agents`; fails when two of them share a name (Go
    /// `duplicate agent name: <name>`).
    pub fn new(root: Arc<dyn Agent>, agents: Vec<Arc<dyn Agent>>) -> Result<Self, LoaderError> {
        let mut all: Vec<(String, Arc<dyn Agent>)> = vec![(root.name().to_owned(), root.clone())];
        for a in agents {
            if all.iter().any(|(name, _)| name == a.name()) {
                return Err(LoaderError(format!("duplicate agent name: {}", a.name())));
            }
            all.push((a.name().to_owned(), a));
        }
        Ok(Self { agents: all, root })
    }
}

impl Loader for MultiLoader {
    fn list_agents(&self) -> Vec<String> {
        let mut names: Vec<String> = self.agents.iter().map(|(name, _)| name.clone()).collect();
        names.sort();
        names
    }

    fn load_agent(&self, name: &str) -> Result<Arc<dyn Agent>, LoaderError> {
        match self.agents.iter().find(|(n, _)| n == name) {
            Some((_, a)) => Ok(a.clone()),
            None => Err(LoaderError(format!(
                "agent {name} not found. Please specify one of those: [{}]",
                self.list_agents().join(" ")
            ))),
        }
    }

    fn root_agent(&self) -> Arc<dyn Agent> {
        self.root.clone()
    }
}
