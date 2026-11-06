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

//! Port of Go internal/agent/parentmap/map_test.go.

use std::sync::Arc;

use gadk_agent::{Agent, AgentConfig, ParentMap};

use crate::llm::LlmAgentConfig;
use crate::llm::agent as llm_agent;
use crate::testutil::MockModel;

fn custom(name: &str, subs: Vec<Arc<dyn Agent>>) -> Arc<dyn Agent> {
    gadk_agent::new_agent(AgentConfig {
        name: name.into(),
        sub_agents: subs,
        ..Default::default()
    })
    .unwrap()
}

// parity: adapted: gadk_agent::ParentMap keeps its map private to gadk-agent (no iteration), so the parent of every agent is checked by name
#[test]
fn test_new() {
    let child1_1 = custom("child1_1", vec![]);
    let child1 = custom("child1", vec![child1_1]);
    let child2 = custom("child2", vec![]);
    let root = custom("root", vec![child1, child2]);
    let got = ParentMap::new(&root).unwrap();
    for (child, parent) in [
        ("child1_1", "child1"),
        ("child1", "root"),
        ("child2", "root"),
    ] {
        assert_eq!(
            got.parent(child).map(|p| p.name().to_string()).as_deref(),
            Some(parent),
            "parent({child})"
        );
    }
    assert!(got.parent("root").is_none());
}

// parity: adapted: the Go {nil, nil} subtest passes a nil agent interface, which has no Rust counterpart
#[test]
fn test_map_root_agent() {
    // Go struct{ model.LLM }{}: a model that is never called.
    let model = MockModel::new(vec![]);
    let non_llm = custom("mock", vec![]);
    let b = llm_agent::new(LlmAgentConfig {
        name: "b".into(),
        model: Some(model.clone()),
        sub_agents: vec![non_llm.clone()],
        ..Default::default()
    })
    .unwrap();
    let a = llm_agent::new(LlmAgentConfig {
        name: "a".into(),
        model: Some(model.clone()),
        sub_agents: vec![b.clone()],
        ..Default::default()
    })
    .unwrap();
    let root = llm_agent::new(LlmAgentConfig {
        name: "root".into(),
        model: Some(model),
        sub_agents: vec![a.clone()],
        ..Default::default()
    })
    .unwrap();
    let parents = ParentMap::new(&root).unwrap();
    for agent in [&root, &a, &b, &non_llm] {
        assert_eq!(
            parents.root_agent(agent).name(),
            "root",
            "rootAgent({:?})",
            agent.name()
        );
    }
}
