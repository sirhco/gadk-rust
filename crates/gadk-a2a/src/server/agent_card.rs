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

//! Agent-card skills and descriptions (port of ADK-Go `server/adka2a/v2/agent_card.go`).

use std::sync::Arc;

use gadk_agent::{Agent, AgentState, AgentType, Tool};
use gadk_engine::agents::loop_agent::LoopAgentConfig;
use gadk_engine::llm::{AgentInstructions, agent_instructions, agent_tools};

/// Go `BuildAgentSkills`: the skills of `agent` and its direct sub-agents, built from the agents'
/// descriptions and types, for the `skills` of an `a2a::AgentCard`. They help clients understand
/// what the agent can do.
pub fn build_agent_skills(agent: &dyn Agent) -> Vec<a2a::AgentSkill> {
    let mut skills = build_primary_skills(agent);
    skills.extend(build_sub_agent_skills(agent));
    skills
}

/// An `a2a::AgentSkill` with Go's four fields set and every optional field absent.
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

/// Go `buildPrimarySkills`.
fn build_primary_skills(agent: &dyn Agent) -> Vec<a2a::AgentSkill> {
    match agent_instructions(agent) {
        Some(instructions) => {
            let tools = agent_tools(agent).unwrap_or_default();
            build_llm_agent_skills(agent, &instructions, &tools)
        }
        None => build_non_llm_agent_skills(agent),
    }
}

/// Go `buildSubAgentSkills`: the primary skills of each direct sub-agent, prefixed with its name
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

/// Go `buildLLMAgentSkills`: the `model` skill, then one skill per tool.
fn build_llm_agent_skills(
    agent: &dyn Agent,
    instructions: &AgentInstructions,
    tools: &[Arc<dyn Tool>],
) -> Vec<a2a::AgentSkill> {
    let mut skills = vec![skill(
        agent.name().to_owned(),
        "model".to_owned(),
        build_description_from_instructions(agent, instructions),
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

/// Go `buildNonLLMAgentSkills`: the agent's own skill and, with sub-agents, an orchestration
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

/// Go `buildAgentDescription`.
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

/// A sub-agent's description, or Go's `execute the <name> agent` when it has none.
fn sub_agent_description(sub: &dyn Agent) -> String {
    if sub.description().is_empty() {
        format!("execute the {} agent", sub.name())
    } else {
        sub.description().to_owned()
    }
}

/// Go `buildSequentialAgentDescription`.
fn build_sequential_agent_description(agent: &dyn Agent) -> String {
    let sub_agents = agent.sub_agents();
    let last = sub_agents.len() - 1;
    let descriptions: Vec<String> = sub_agents
        .iter()
        .enumerate()
        .map(|(i, sub)| {
            let description = sub_agent_description(sub.as_ref());
            // Go's `switch` tests `case 0` first, so a single sub-agent is "First, ...".
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

/// The sub-agent clause shared by Go's parallel and loop descriptions: `This agent will A , B and
/// C` (Go joins `", B"` with a space, so the comma is preceded by one).
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

/// Go `buildParallelAgentDescription`.
fn build_parallel_agent_description(agent: &dyn Agent) -> String {
    format!("{} simultaneously.", concurrent_sub_agent_clause(agent))
}

/// Go `buildLoopAgentDescription`: empty when the state carries no loop-agent config.
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

/// Go `buildDescriptionFromInstructions`: the description, the instruction and the global
/// instruction (pronouns turned to the first person), else the type's default description.
fn build_description_from_instructions(
    agent: &dyn Agent,
    instructions: &AgentInstructions,
) -> String {
    let mut parts = Vec::new();
    if !agent.description().is_empty() {
        parts.push(agent.description().to_owned());
    }
    if !instructions.instruction.is_empty() {
        parts.push(replace_pronouns(&instructions.instruction));
    }
    if !instructions.global_instruction.is_empty() {
        parts.push(replace_pronouns(&instructions.global_instruction));
    }
    if parts.is_empty() {
        default_agent_description(agent.agent_state()).to_owned()
    } else {
        parts.join(" ")
    }
}

/// Go `replacePronouns`: replaces second-person pronouns and conjugates common verbs for an agent
/// description ("You are" → "I am", "your" → "my").
///
/// Go applies `(?i)\b<original>\b` with `ReplaceAllString` for each pair in turn; the matcher
/// below reproduces RE2's semantics for these literals: ASCII word boundaries, Unicode simple case
/// folding (which for these letters adds only `ſ`, U+017F, to `s`), leftmost non-overlapping
/// matches.
pub(crate) fn replace_pronouns(instruction: &str) -> String {
    // Keep sorted by length of the original, descending, so longer phrases are matched first,
    // which prevents "you" in "you are" from being replaced on its own.
    const SUBSTITUTIONS: [(&str, &str); 7] = [
        ("you were", "I was"),
        ("you are", "I am"),
        ("you're", "I am"),
        ("you've", "I have"),
        ("yours", "mine"),
        ("your", "my"),
        ("you", "I"),
    ];
    let mut text = instruction.to_owned();
    for (original, target) in SUBSTITUTIONS {
        text = replace_word_case_insensitive(&text, original, target);
    }
    text
}

/// Go RE2 `\b`: a boundary between an ASCII word character (`[0-9A-Za-z_]`) and anything else.
fn is_word_char(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Whether `c` matches the lowercase ASCII pattern byte `p` under RE2's `(?i)` folding.
fn folds_to(c: char, p: u8) -> bool {
    (c.is_ascii() && c.to_ascii_lowercase() as u32 == u32::from(p)) || (p == b's' && c == 'ſ')
}

/// The byte length of the match of `pattern` at the start of `s`, if `\b<pattern>\b` matches
/// there; `prev` is the character before `s`.
fn match_at(s: &str, prev: Option<char>, pattern: &str) -> Option<usize> {
    let mut chars = s.char_indices();
    let mut first = None;
    let mut last = None;
    for p in pattern.bytes() {
        let (_, c) = chars.next()?;
        if !folds_to(c, p) {
            return None;
        }
        first.get_or_insert(c);
        last = Some(c);
    }
    let end = chars.next().map_or(s.len(), |(i, _)| i);
    let next = s[end..].chars().next();
    let starts_on_boundary = is_word_char(prev) != is_word_char(first);
    let ends_on_boundary = is_word_char(last) != is_word_char(next);
    (starts_on_boundary && ends_on_boundary).then_some(end)
}

/// Go `regexp.MustCompile("(?i)\\b" + pattern + "\\b").ReplaceAllString(s, target)` for a
/// lowercase literal `pattern` without regex metacharacters.
fn replace_word_case_insensitive(s: &str, pattern: &str, target: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev = None;
    let mut i = 0;
    while i < s.len() {
        let rest = &s[i..];
        if let Some(len) = match_at(rest, prev, pattern) {
            out.push_str(target);
            prev = rest[..len].chars().next_back();
            i += len;
            continue;
        }
        let Some(c) = rest.chars().next() else { break };
        out.push(c);
        prev = Some(c);
        i += c.len_utf8();
    }
    out
}

/// Go `getDefaultAgentDescription`.
fn default_agent_description(state: &AgentState) -> &'static str {
    match state.agent_type {
        AgentType::LoopAgent => "A loop workflow agent",
        AgentType::SequentialAgent => "A sequential workflow agent",
        AgentType::ParallelAgent => "A parallel workflow agent",
        AgentType::LlmAgent => "An LLM-based agent",
        _ => "A custom agent",
    }
}

/// Go `getAgentTypeTag`.
fn agent_type_tag(state: &AgentState) -> &'static str {
    match state.agent_type {
        AgentType::LoopAgent => "loop_workflow",
        AgentType::SequentialAgent => "sequential_workflow",
        AgentType::ParallelAgent => "parallel_workflow",
        AgentType::LlmAgent => "llm_agent",
        _ => "custom_agent",
    }
}

/// Go `getAgentSkillName`.
fn agent_skill_name(state: &AgentState) -> &'static str {
    if state.agent_type == AgentType::LlmAgent {
        return "model";
    }
    if is_workflow_agent(state) {
        return "workflow";
    }
    "custom"
}

/// Go `isWorkflowAgent`: loop, sequential and parallel agents (not `WorkflowAgent`, which Go's
/// list leaves out).
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
    fn replace_pronouns_respects_ascii_word_boundaries_and_unicode_folding() {
        // `\b` is ASCII-only in RE2: `é` is not a word character, so "youé" holds a match.
        assert_eq!(replace_pronouns("youé"), "Ié");
        assert_eq!(replace_pronouns("_you"), "_you");
        assert_eq!(replace_pronouns("you1"), "you1");
        // (?i) folds `ſ` to `s`; after `ſ` (a non-word character) `\b` needs a word character.
        assert_eq!(replace_pronouns("yourſa"), "minea");
        assert_eq!(replace_pronouns("yourſ "), "myſ ");
        assert_eq!(replace_pronouns("YoU WeRe"), "I was");
        assert_eq!(replace_pronouns(""), "");
    }

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
