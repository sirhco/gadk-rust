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

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Port of ADK-Go agent/agent_name_test.go (every agent constructor, so it lives in the façade,
//! which reaches all of them; needs feature `a2a` for the remote agent).

use std::sync::Arc;

use gadk::a2a::remoteagent::{A2AConfig, new_a2a};
use gadk::agent::{Agent, AgentConfig, Context, new_agent};
use gadk::engine::agents::loop_agent::{self, LoopAgentConfig};
use gadk::engine::agents::parallel_agent::{self, ParallelAgentConfig};
use gadk::engine::agents::sequential_agent::{self, SequentialAgentConfig};
use gadk::engine::agents::workflow_agent::{self, WorkflowAgentConfig};
use gadk::engine::llm::{LlmAgentConfig, agent as llm_agent};
use gadk::engine::workflow::{NodeConfig, START, chain, function_node};

/// A constructor under test: builds an agent named `name`; `Err` carries the error text.
type Build = fn(&str) -> Result<Arc<dyn Agent>, String>;

fn agent_cfg(name: &str) -> AgentConfig {
    AgentConfig {
        name: name.into(),
        ..Default::default()
    }
}

/// Go `&a2a.AgentCard{}`: a2a-rs's `AgentCard` has no `Default`, so the card is decoded from
/// the fewest fields it requires.
fn empty_card() -> gadk::a2a::a2a::AgentCard {
    serde_json::from_value(serde_json::json!({
        "name": "", "version": "", "capabilities": {}, "supportedInterfaces": []
    }))
    .unwrap()
}

// Every constructor builds its agent through agent.New, so each one rejects an empty name. A
// nameless agent wrapped by agenttool would otherwise send the model a function declaration with
// an empty name, which Vertex AI rejects for the whole request.
// parity: adapted: Go's `a != nil` check is implied by Result (an Err carries no agent); the remote agent's empty card is decoded JSON
#[test]
fn test_constructors_reject_empty_name() {
    let tests: Vec<(&str, Build)> = vec![
        ("agent.New", |name| {
            new_agent(agent_cfg(name)).map_err(|e| e.to_string())
        }),
        ("llmagent.New", |name| {
            llm_agent::new(LlmAgentConfig {
                name: name.into(),
                ..Default::default()
            })
            .map_err(|e| e.to_string())
        }),
        ("remoteagent.NewA2A", |name| {
            new_a2a(A2AConfig {
                name: name.into(),
                agent_card: Some(empty_card()),
                ..Default::default()
            })
            .map_err(|e| e.to_string())
        }),
        ("loopagent.New", |name| {
            loop_agent::new(LoopAgentConfig {
                agent: agent_cfg(name),
                ..Default::default()
            })
            .map_err(|e| e.to_string())
        }),
        ("parallelagent.New", |name| {
            parallel_agent::new(ParallelAgentConfig {
                agent: agent_cfg(name),
            })
            .map_err(|e| e.to_string())
        }),
        ("sequentialagent.New", |name| {
            sequential_agent::new(SequentialAgentConfig {
                agent: agent_cfg(name),
            })
            .map_err(|e| e.to_string())
        }),
        ("workflowagent.New", |name| {
            let node = function_node::<String, String, _, _>(
                "echo",
                |_ctx: Context, input: String| async move { Ok(input) },
                NodeConfig::default(),
            );
            workflow_agent::new(WorkflowAgentConfig {
                name: name.into(),
                edges: chain(&[Arc::clone(&START), node]),
                ..Default::default()
            })
            .map_err(|e| e.to_string())
        }),
    ];
    for (case, build) in tests {
        // The named build passing shows the config is otherwise valid, so the error below comes
        // from the empty name.
        if let Err(e) = build("named") {
            panic!("{case}: build(\"named\") error = {e}, want nil");
        }
        assert!(
            build("").is_err(),
            "{case}: build(\"\") error = nil, want an error"
        );
    }
}
