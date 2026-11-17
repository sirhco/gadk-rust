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

//! The agent tree's parent map.

use crate::go_fmt::go_quote;
use std::collections::HashMap;
use std::sync::Arc;

use crate::agent::Agent;
use crate::error::AgentError;

/// Child agent name → parent agent, plus the root.
#[derive(Clone, Default)]
pub struct ParentMap {
    parents: HashMap<String, Arc<dyn Agent>>,
    root: Option<Arc<dyn Agent>>,
}

impl std::fmt::Debug for ParentMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        struct Parents<'a>(&'a HashMap<String, Arc<dyn Agent>>);
        impl std::fmt::Debug for Parents<'_> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_map()
                    .entries(self.0.iter().map(|(child, parent)| (child, parent.name())))
                    .finish()
            }
        }
        f.debug_struct("ParentMap")
            .field("parents", &Parents(&self.parents))
            .field("root", &self.root.as_ref().map(|r| r.name()))
            .finish()
    }
}

impl ParentMap {
    /// Builds the map: at most one parent per agent, names unique across the tree.
    pub fn new(root: &Arc<dyn Agent>) -> Result<Self, AgentError> {
        let mut parents = HashMap::new();
        let mut seen: HashMap<usize, String> =
            HashMap::from([(ptr(root), "is root agent".to_string())]);
        walk(root, root.name(), &mut parents, &mut seen)?;
        Ok(Self {
            parents,
            root: Some(root.clone()),
        })
    }

    /// The parent of the agent named `agent_name`; `None` for the root or an unknown name.
    pub fn parent(&self, agent_name: &str) -> Option<Arc<dyn Agent>> {
        self.parents.get(agent_name).cloned()
    }

    /// The root agent the map was built from.
    pub fn root(&self) -> Option<Arc<dyn Agent>> {
        self.root.clone()
    }

    /// Follows parents from `cur` to the top.
    pub fn root_agent(&self, cur: &Arc<dyn Agent>) -> Arc<dyn Agent> {
        let mut cur = cur.clone();
        while let Some(parent) = self.parents.get(cur.name()) {
            cur = parent.clone();
        }
        cur
    }
}

fn ptr(a: &Arc<dyn Agent>) -> usize {
    Arc::as_ptr(a).cast::<()>() as usize
}

fn walk(
    cur: &Arc<dyn Agent>,
    root_name: &str,
    parents: &mut HashMap<String, Arc<dyn Agent>>,
    seen: &mut HashMap<usize, String>,
) -> Result<(), AgentError> {
    for sub in cur.sub_agents() {
        if let Some(p) = seen.get(&ptr(sub)) {
            return Err(AgentError::Config(format!(
                "{} agent cannot have >1 parents, found: {}, {}",
                go_quote(sub.name()),
                go_quote(p),
                go_quote(cur.name())
            )));
        }
        if parents.contains_key(sub.name()) || sub.name() == root_name {
            return Err(AgentError::Config(format!(
                "agent names must be unique in the agent tree, found duplicate: {}",
                go_quote(sub.name())
            )));
        }
        parents.insert(sub.name().to_string(), cur.clone());
        seen.insert(ptr(sub), cur.name().to_string());
        walk(sub, root_name, parents, seen)?;
    }
    Ok(())
}
