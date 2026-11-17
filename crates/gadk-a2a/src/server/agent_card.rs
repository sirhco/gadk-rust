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

//! Agent-card skills and descriptions.

use std::sync::Arc;

use gadk_agent::{Agent, AgentState, AgentType, Tool};
use gadk_engine::agents::loop_agent::LoopAgentConfig;
use gadk_engine::llm::{agent_instructions, agent_tools};

/// The skills of `agent` and its direct sub-agents, built from the agents'
/// descriptions and types, for the `skills` of an `a2a::AgentCard`. They help clients understand
/// what the agent can do.
pub fn build_agent_skills(agent: &dyn Agent) -> Vec<a2a::AgentSkill> {
    let mut skills = build_primary_skills(agent);
    skills.extend(build_sub_agent_skills(agent));
    skills
}

/// An `a2a::AgentSkill` with its four required fields set and every optional field absent.
fn skill(id: String, name: String, description: String, tags: Vec<String>) -> a2a::AgentSkill {
    a2a::AgentSkill {
        id,
        name,
        description,
        tags,
        examples: None,
        input_modes: None,
        output_modes: None,
        security_requirements: None,
    }
}

/// The skills of `agent` itself: LLM-agent skills when it has instructions, else non-LLM skills.
fn build_primary_skills(agent: &dyn Agent) -> Vec<a2a::AgentSkill> {
    match agent_instructions(agent) {
        Some(_) => {
            let tools = agent_tools(agent).unwrap_or_default();
            build_llm_agent_skills(agent, &tools)
        }
        None => build_non_llm_agent_skills(agent),
    }
}

/// The primary skills of each direct sub-agent, prefixed with its name
/// (one level only).
fn build_sub_agent_skills(agent: &dyn Agent) -> Vec<a2a::AgentSkill> {
    let mut result = Vec::with_capacity(agent.sub_agents().len());
    for sub in agent.sub_agents() {
        for sub_skill in build_primary_skills(sub.as_ref()) {
            let mut tags = vec![format!("sub_agent:{}", sub.name())];
            tags.extend(sub_skill.tags);
            result.push(skill(
                format!("{}_{}", sub.name(), sub_skill.id),
                format!("{}: {}", sub.name(), sub_skill.name),
                sub_skill.description,
                tags,
            ));
        }
    }
    result
}

/// The `model` skill, then one skill per tool.
fn build_llm_agent_skills(agent: &dyn Agent, tools: &[Arc<dyn Tool>]) -> Vec<a2a::AgentSkill> {
    let mut skills = vec![skill(
        agent.name().to_owned(),
        "model".to_owned(),
        build_llm_agent_description(agent),
        vec!["llm".to_owned()],
    )];
    for tool in tools {
        let description = if tool.description().is_empty() {
            format!("Tool: {}", tool.name())
        } else {
            tool.description().to_owned()
        };
        skills.push(skill(
            format!("{}-{}", agent.name(), tool.name()),
            tool.name().to_owned(),
            description,
            vec!["llm".to_owned(), "tools".to_owned()],
        ));
    }
    skills
}

/// The agent's own skill and, with sub-agents, an orchestration
/// skill listing them.
fn build_non_llm_agent_skills(agent: &dyn Agent) -> Vec<a2a::AgentSkill> {
    let state = agent.agent_state();
    let mut skills = vec![skill(
        agent.name().to_owned(),
        agent_skill_name(state).to_owned(),
        build_agent_description(agent, state),
        vec![agent_type_tag(state).to_owned()],
    )];

    let sub_agents = agent.sub_agents();
    if !sub_agents.is_empty() {
        let descriptions: Vec<&str> = sub_agents
            .iter()
            .map(|sub| {
                if sub.description().is_empty() {
                    "No description"
                } else {
                    sub.description()
                }
            })
            .collect();
        skills.push(skill(
            format!("{}-sub-agents", agent.name()),
            "sub-agents".to_owned(),
            format!("Orchestrates: {}", descriptions.join("; ")),
            vec![agent_type_tag(state).to_owned(), "orchestration".to_owned()],
        ));
    }
    skills
}

/// The description of a non-LLM agent: its own description followed by the workflow description
/// of its type, or the type's default description when both are empty.
fn build_agent_description(agent: &dyn Agent, state: &AgentState) -> String {
    let mut parts = Vec::new();
    if !agent.description().is_empty() {
        parts.push(agent.description().to_owned());
    }
    if !agent.sub_agents().is_empty() {
        match state.agent_type {
            AgentType::LoopAgent => parts.push(build_loop_agent_description(agent, state)),
            AgentType::ParallelAgent => parts.push(build_parallel_agent_description(agent)),
            AgentType::SequentialAgent => parts.push(build_sequential_agent_description(agent)),
            _ => {}
        }
    }
    if parts.is_empty() {
        default_agent_description(state).to_owned()
    } else {
        parts.join(" ")
    }
}

/// A sub-agent's description, or `execute the <name> agent` when it has none.
fn sub_agent_description(sub: &dyn Agent) -> String {
    if sub.description().is_empty() {
        format!("execute the {} agent", sub.name())
    } else {
        sub.description().to_owned()
    }
}

/// The description of a sequential agent: "First, ...", "Then, ..." and "Finally, ..." clauses
/// for its sub-agents in order.
fn build_sequential_agent_description(agent: &dyn Agent) -> String {
    let sub_agents = agent.sub_agents();
    let last = sub_agents.len() - 1;
    let descriptions: Vec<String> = sub_agents
        .iter()
        .enumerate()
        .map(|(i, sub)| {
            let description = sub_agent_description(sub.as_ref());
            // The first test wins, so a single sub-agent is "First, ...".
            if i == 0 {
                format!("First, this agent will {description}.")
            } else if i == last {
                format!("Finally, this agent will {description}.")
            } else {
                format!("Then, this agent will {description}.")
            }
        })
        .collect();
    descriptions.join(" ")
}

/// The sub-agent clause shared by the parallel and loop descriptions: `This agent will A , B and
/// C` (the clauses are joined with a space, so a comma is preceded by one).
fn concurrent_sub_agent_clause(agent: &dyn Agent) -> String {
    let sub_agents = agent.sub_agents();
    let last = sub_agents.len() - 1;
    let descriptions: Vec<String> = sub_agents
        .iter()
        .enumerate()
        .map(|(i, sub)| {
            let description = sub_agent_description(sub.as_ref());
            if i == 0 {
                format!("This agent will {description}")
            } else if i == last {
                format!("and {description}")
            } else {
                format!(", {description}")
            }
        })
        .collect();
    descriptions.join(" ")
}

/// The description of a parallel agent: its sub-agents run simultaneously.
fn build_parallel_agent_description(agent: &dyn Agent) -> String {
    format!("{} simultaneously.", concurrent_sub_agent_clause(agent))
}

/// The description of a loop agent: empty when the state carries no loop-agent config.
fn build_loop_agent_description(agent: &dyn Agent, state: &AgentState) -> String {
    let Some(cfg) = state
        .config
        .as_ref()
        .and_then(|c| c.downcast_ref::<LoopAgentConfig>())
    else {
        return String::new();
    };
    let max_iterations = if cfg.max_iterations > 0 {
        cfg.max_iterations.to_string()
    } else {
        "unlimited".to_owned()
    };
    format!(
        "{} in a loop (max {max_iterations} iterations).",
        concurrent_sub_agent_clause(agent)
    )
}

/// An LLM agent is described by its own description alone,
/// else the type's default description. The card is served without authentication, so it never
/// repeats the agent's instructions.
fn build_llm_agent_description(agent: &dyn Agent) -> String {
    if agent.description().is_empty() {
        default_agent_description(agent.agent_state()).to_owned()
    } else {
        agent.description().to_owned()
    }
}

/// The description of an agent of this type that has none.
fn default_agent_description(state: &AgentState) -> &'static str {
    match state.agent_type {
        AgentType::LoopAgent => "A loop workflow agent",
        AgentType::SequentialAgent => "A sequential workflow agent",
        AgentType::ParallelAgent => "A parallel workflow agent",
        AgentType::LlmAgent => "An LLM-based agent",
        _ => "A custom agent",
    }
}

/// The skill tag of the agent type.
fn agent_type_tag(state: &AgentState) -> &'static str {
    match state.agent_type {
        AgentType::LoopAgent => "loop_workflow",
        AgentType::SequentialAgent => "sequential_workflow",
        AgentType::ParallelAgent => "parallel_workflow",
        AgentType::LlmAgent => "llm_agent",
        _ => "custom_agent",
    }
}

/// The skill name for the agent type.
fn agent_skill_name(state: &AgentState) -> &'static str {
    if state.agent_type == AgentType::LlmAgent {
        return "model";
    }
    if is_workflow_agent(state) {
        return "workflow";
    }
    "custom"
}

/// Loop, sequential and parallel agents (`WorkflowAgent` is not one).
fn is_workflow_agent(state: &AgentState) -> bool {
    matches!(
        state.agent_type,
        AgentType::LoopAgent | AgentType::SequentialAgent | AgentType::ParallelAgent
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_llm_custom_agent_types_fall_back_to_custom() {
        let state = AgentState {
            agent_type: AgentType::RemoteAgent,
            config: None,
            on_before_agent_callback_event: None,
            live: None,
        };
        assert_eq!(default_agent_description(&state), "A custom agent");
        assert_eq!(agent_type_tag(&state), "custom_agent");
        assert_eq!(agent_skill_name(&state), "custom");
        let workflow = AgentState {
            agent_type: AgentType::WorkflowAgent,
            ..state
        };
        assert_eq!(agent_skill_name(&workflow), "custom");
        assert!(!is_workflow_agent(&workflow));
    }

    #[test]
    fn loop_description_is_empty_without_a_loop_config() {
        let agent = gadk_agent::new_with_state(
            gadk_agent::AgentConfig {
                name: "loop".into(),
                sub_agents: vec![
                    gadk_agent::new_agent(gadk_agent::AgentConfig {
                        name: "a".into(),
                        ..Default::default()
                    })
                    .unwrap(),
                ],
                ..Default::default()
            },
            AgentState {
                agent_type: AgentType::LoopAgent,
                config: None,
                on_before_agent_callback_event: None,
                live: None,
            },
        )
        .unwrap();
        assert_eq!(
            build_loop_agent_description(&*agent, agent.agent_state()),
            ""
        );
        // Go appends the empty loop clause, so the parts are [""] and the description is "".
        assert_eq!(build_agent_description(&*agent, agent.agent_state()), "");
    }
}
