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

//! Go `internal/configurable/conformance/loader.go`: the conformance agent loader (agent
//! names sorted, the first one is the root agent).

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::collections::HashMap;
use std::sync::Arc;

use gadk_agent::{Agent, Loader, LoaderError};

/// Go `conformanceAgentLoader`.
pub struct ConformanceAgentLoader {
    agent_map: HashMap<String, Arc<dyn Agent>>,
    agents_names: Vec<String>,
}

/// Go `NewConformanceAgentLoader`: the names sorted (Go `sort.Strings`); Go's error is always
/// nil.
pub fn new_conformance_agent_loader(
    agent_map: HashMap<String, Arc<dyn Agent>>,
) -> Result<Arc<dyn Loader>, LoaderError> {
    // Go: loader.go NewConformanceAgentLoader
    todo!("Task 11")
}

impl Loader for ConformanceAgentLoader {
    /// Go `ListAgents`: the sorted names.
    fn list_agents(&self) -> Vec<String> {
        // Go: loader.go ListAgents
        todo!("Task 11")
    }

    /// Go `LoadAgent`: `agent %s not found. Please specify one of those: %v`.
    fn load_agent(&self, name: &str) -> Result<Arc<dyn Agent>, LoaderError> {
        // Go: loader.go LoadAgent
        todo!("Task 11")
    }

    /// Go `RootAgent`: the agent of the first sorted name (Go returns nil for an empty map).
    fn root_agent(&self) -> Arc<dyn Agent> {
        // Go: loader.go RootAgent
        todo!("Task 11")
    }
}
