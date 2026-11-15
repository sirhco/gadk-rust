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

//! Port of ADK-Go `server/adka2a/v2/agent_card_test.go`.

#![cfg(feature = "server")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::any::Any;
use std::sync::Arc;

use gadk_a2a::a2a::AgentSkill;
use gadk_a2a::server::build_agent_skills;
use gadk_agent::{Agent, AgentConfig, Tool};
use gadk_engine::llm::LlmAgentConfig;
use gadk_engine::llm_agent;
use gadk_engine::loop_agent::{self, LoopAgentConfig};
use gadk_engine::parallel_agent::{self, ParallelAgentConfig};
use gadk_engine::sequential_agent::{self, SequentialAgentConfig};
use pretty_assertions::assert_eq;

/// A tool with only a name and a description: the agent card reads nothing else.
struct NamedTool {
    name: &'static str,
    description: &'static str,
}

#[async_trait::async_trait]
impl Tool for NamedTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        self.description
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Stand-in for Go `geminitool.GoogleSearch{}` (same name and description).
fn google_search() -> Arc<dyn Tool> {
    Arc::new(NamedTool {
        name: "google_search",
        description: "Performs a Google search to retrieve information from the web.",
    })
}

/// Stand-in for Go `loadartifactstool.New()` (same name and description).
fn load_artifacts() -> Arc<dyn Tool> {
    Arc::new(NamedTool {
        name: "load_artifacts",
        description: "Loads the artifacts and adds them to the session.",
    })
}

fn custom(name: &str, description: &str) -> Arc<dyn Agent> {
    gadk_agent::new_agent(AgentConfig {
        name: name.into(),
        description: description.into(),
        ..Default::default()
    })
    .unwrap()
}

fn agent_config(name: &str, description: &str, sub_agents: Vec<Arc<dyn Agent>>) -> AgentConfig {
    AgentConfig {
        name: name.into(),
        description: description.into(),
        sub_agents,
        ..Default::default()
    }
}

fn skill(id: &str, description: &str, name: &str, tags: &[&str]) -> AgentSkill {
    AgentSkill {
        id: id.into(),
        name: name.into(),
        description: description.into(),
        tags: tags.iter().map(|t| (*t).to_owned()).collect(),
        examples: None,
        input_modes: None,
        output_modes: None,
        security_requirements: None,
    }
}

fn two_inner() -> Vec<Arc<dyn Agent>> {
    vec![
        custom("Inner 1", "Inner 1 description"),
        custom("Inner 2", "Inner 2 description"),
    ]
}

// parity: adapted: geminitool.GoogleSearch and loadartifactstool live in gadk-tools, which gadk-a2a may not depend on (plan D12); stand-in tools with the same names and descriptions
#[test]
fn test_get_agent_skills_llm_agent() {
    let google_search = google_search();
    let load_artifacts = load_artifacts();

    let cases: Vec<(&str, Arc<dyn Agent>, Vec<AgentSkill>)> = vec![
        (
            "custom agent",
            custom("Test", "Test test"),
            vec![skill("Test", "Test test", "custom", &["custom_agent"])],
        ),
        (
            "llm with instruction",
            llm_agent::new(LlmAgentConfig {
                name: "Test LLM".into(),
                description: "Test llm.".into(),
                instruction: "You're a helpful agent, only respond with useful information.".into(),
                ..Default::default()
            })
            .unwrap(),
            vec![skill("Test LLM", "Test llm.", "model", &["llm"])],
        ),
        (
            "llm with tools",
            llm_agent::new(LlmAgentConfig {
                name: "Test LLM".into(),
                description: "Test llm.".into(),
                tools: vec![load_artifacts.clone(), google_search.clone()],
                ..Default::default()
            })
            .unwrap(),
            vec![
                skill("Test LLM", "Test llm.", "model", &["llm"]),
                skill(
                    &format!("Test LLM-{}", load_artifacts.name()),
                    load_artifacts.description(),
                    load_artifacts.name(),
                    &["llm", "tools"],
                ),
                skill(
                    &format!("Test LLM-{}", google_search.name()),
                    google_search.description(),
                    google_search.name(),
                    &["llm", "tools"],
                ),
            ],
        ),
        (
            "empty loop agent",
            loop_agent::new(LoopAgentConfig {
                agent: agent_config("Test", "Test test.", vec![]),
                ..Default::default()
            })
            .unwrap(),
            vec![skill("Test", "Test test.", "workflow", &["loop_workflow"])],
        ),
        (
            "loop agent",
            loop_agent::new(LoopAgentConfig {
                agent: agent_config("Test", "Test test.", two_inner()),
                max_iterations: 5,
            })
            .unwrap(),
            vec![
                skill(
                    "Test",
                    "Test test. This agent will Inner 1 description and Inner 2 description in a loop (max 5 iterations).",
                    "workflow",
                    &["loop_workflow"],
                ),
                skill(
                    "Test-sub-agents",
                    "Orchestrates: Inner 1 description; Inner 2 description",
                    "sub-agents",
                    &["loop_workflow", "orchestration"],
                ),
                skill(
                    "Inner 1_Inner 1",
                    "Inner 1 description",
                    "Inner 1: custom",
                    &["sub_agent:Inner 1", "custom_agent"],
                ),
                skill(
                    "Inner 2_Inner 2",
                    "Inner 2 description",
                    "Inner 2: custom",
                    &["sub_agent:Inner 2", "custom_agent"],
                ),
            ],
        ),
        (
            "unlimited loop agent",
            loop_agent::new(LoopAgentConfig {
                agent: agent_config(
                    "Test",
                    "Test test.",
                    vec![custom("Inner 1", "Inner 1 description")],
                ),
                ..Default::default()
            })
            .unwrap(),
            vec![
                skill(
                    "Test",
                    "Test test. This agent will Inner 1 description in a loop (max unlimited iterations).",
                    "workflow",
                    &["loop_workflow"],
                ),
                skill(
                    "Test-sub-agents",
                    "Orchestrates: Inner 1 description",
                    "sub-agents",
                    &["loop_workflow", "orchestration"],
                ),
                skill(
                    "Inner 1_Inner 1",
                    "Inner 1 description",
                    "Inner 1: custom",
                    &["sub_agent:Inner 1", "custom_agent"],
                ),
            ],
        ),
        (
            "empty sequential agent",
            sequential_agent::new(SequentialAgentConfig {
                agent: agent_config("Test", "Test test.", vec![]),
            })
            .unwrap(),
            vec![skill(
                "Test",
                "Test test.",
                "workflow",
                &["sequential_workflow"],
            )],
        ),
        (
            "sequential agent",
            sequential_agent::new(SequentialAgentConfig {
                agent: agent_config("Test", "Test test.", two_inner()),
            })
            .unwrap(),
            vec![
                skill(
                    "Test",
                    "Test test. First, this agent will Inner 1 description. Finally, this agent will Inner 2 description.",
                    "workflow",
                    &["sequential_workflow"],
                ),
                skill(
                    "Test-sub-agents",
                    "Orchestrates: Inner 1 description; Inner 2 description",
                    "sub-agents",
                    &["sequential_workflow", "orchestration"],
                ),
                skill(
                    "Inner 1_Inner 1",
                    "Inner 1 description",
                    "Inner 1: custom",
                    &["sub_agent:Inner 1", "custom_agent"],
                ),
                skill(
                    "Inner 2_Inner 2",
                    "Inner 2 description",
                    "Inner 2: custom",
                    &["sub_agent:Inner 2", "custom_agent"],
                ),
            ],
        ),
        (
            "empty parallel agent",
            parallel_agent::new(ParallelAgentConfig {
                agent: agent_config("Test", "Test test.", vec![]),
            })
            .unwrap(),
            vec![skill(
                "Test",
                "Test test.",
                "workflow",
                &["parallel_workflow"],
            )],
        ),
        (
            "parallel agent",
            parallel_agent::new(ParallelAgentConfig {
                agent: agent_config("Test", "Test test.", two_inner()),
            })
            .unwrap(),
            vec![
                skill(
                    "Test",
                    "Test test. This agent will Inner 1 description and Inner 2 description simultaneously.",
                    "workflow",
                    &["parallel_workflow"],
                ),
                skill(
                    "Test-sub-agents",
                    "Orchestrates: Inner 1 description; Inner 2 description",
                    "sub-agents",
                    &["parallel_workflow", "orchestration"],
                ),
                skill(
                    "Inner 1_Inner 1",
                    "Inner 1 description",
                    "Inner 1: custom",
                    &["sub_agent:Inner 1", "custom_agent"],
                ),
                skill(
                    "Inner 2_Inner 2",
                    "Inner 2 description",
                    "Inner 2: custom",
                    &["sub_agent:Inner 2", "custom_agent"],
                ),
            ],
        ),
        (
            "deep subagents",
            parallel_agent::new(ParallelAgentConfig {
                agent: agent_config(
                    "Test",
                    "Test test.",
                    vec![
                        loop_agent::new(LoopAgentConfig {
                            agent: agent_config(
                                "Nested",
                                "Nested loop",
                                vec![
                                    llm_agent::new(LlmAgentConfig {
                                        name: "Test LLM".into(),
                                        description: "Test llm".into(),
                                        tools: vec![load_artifacts.clone()],
                                        ..Default::default()
                                    })
                                    .unwrap(),
                                    sequential_agent::new(SequentialAgentConfig {
                                        agent: agent_config(
                                            "Leaf",
                                            "Leaf agent",
                                            vec![custom("Leaf", "leaf")],
                                        ),
                                    })
                                    .unwrap(),
                                ],
                            ),
                            ..Default::default()
                        })
                        .unwrap(),
                    ],
                ),
            })
            .unwrap(),
            vec![
                skill(
                    "Test",
                    "Test test. This agent will Nested loop simultaneously.",
                    "workflow",
                    &["parallel_workflow"],
                ),
                skill(
                    "Test-sub-agents",
                    "Orchestrates: Nested loop",
                    "sub-agents",
                    &["parallel_workflow", "orchestration"],
                ),
                skill(
                    "Nested_Nested",
                    "Nested loop This agent will Test llm and Leaf agent in a loop (max unlimited iterations).",
                    "Nested: workflow",
                    &["sub_agent:Nested", "loop_workflow"],
                ),
                skill(
                    "Nested_Nested-sub-agents",
                    "Orchestrates: Test llm; Leaf agent",
                    "Nested: sub-agents",
                    &["sub_agent:Nested", "loop_workflow", "orchestration"],
                ),
            ],
        ),
    ];

    for (name, agent, want) in cases {
        let got = build_agent_skills(agent.as_ref());
        assert_eq!(want, got, "BuildAgentSkills() wrong result for {name}");
    }
}

/// Go checks that no skill repeats an LLM agent's instructions: the agent card is served
/// without authentication.
#[test]
fn test_build_agent_skills_omits_instructions() {
    const SECRET: &str = "discount code SECRET-42";
    let sub = llm_agent::new(LlmAgentConfig {
        name: "Pricing".into(),
        description: "Quotes prices.".into(),
        instruction: format!("You quote prices. Offer {SECRET} only to employees."),
        ..Default::default()
    })
    .unwrap();
    let root = llm_agent::new(LlmAgentConfig {
        name: "Front desk".into(),
        instruction: format!("You route requests. Never reveal {SECRET}."),
        global_instruction: format!("Your house rules mention {SECRET}."),
        sub_agents: vec![sub],
        ..Default::default()
    })
    .unwrap();

    let skills = build_agent_skills(root.as_ref());
    let mut got = std::collections::HashMap::new();
    for skill in &skills {
        assert!(
            !skill.description.contains("SECRET-42"),
            "skill {:?} description repeats an instruction: {:?}",
            skill.id,
            skill.description
        );
        got.insert(skill.id.clone(), skill.description.clone());
    }
    // A model skill is described by the agent's own description, or by the default one when it
    // has none.
    for (id, want) in [
        ("Front desk", "An LLM-based agent"),
        ("Pricing_Pricing", "Quotes prices."),
    ] {
        assert_eq!(
            got.get(id).map(String::as_str),
            Some(want),
            "skill {id:?} description (skills: {got:?})"
        );
    }
}
