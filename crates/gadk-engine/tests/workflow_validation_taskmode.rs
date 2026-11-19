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
//! Port of ADK-Go workflow/validation_taskmode_test.go.

use std::sync::Arc;

use gadk_agent::{Agent, Mode};
use gadk_engine::llm::LlmAgentConfig;
use gadk_engine::llm::agent as llm_agent;
use gadk_engine::workflow::{AgentNode, Edge, Node, NodeConfig, START, Workflow, WorkflowOptions};

fn new_agent(name: &str, mode: Mode) -> Arc<dyn Agent> {
    llm_agent::new(LlmAgentConfig {
        name: name.into(),
        mode,
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("llmagent.New({name:?}, {mode:?}): {e}"))
}

fn new_node(a: &Arc<dyn Agent>) -> Arc<dyn Node> {
    AgentNode::new(a.clone(), NodeConfig::default())
        .unwrap_or_else(|e| panic!("workflow.NewAgentNode({:?}): {e}", a.name()))
}

fn e(from: &Arc<dyn Node>, to: &Arc<dyn Node>) -> Edge {
    Edge {
        from: from.clone(),
        to: to.clone(),
        route: None,
    }
}

fn start() -> Arc<dyn Node> {
    Arc::clone(&START)
}

/// `workflow.New(name, edges)`: `Ok(())` on success, the error text otherwise.
fn build(name: &str, edges: Vec<Edge>) -> Result<(), String> {
    Workflow::new(name, edges, WorkflowOptions::default())
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// The task-mode graph-node validation error text.
fn task_mode_error(agent: &str) -> String {
    format!(
        "Agent \"{agent}\" has mode='task' and cannot be used as a workflow graph node. Use a chat coordinator with task sub-agents, or dispatch dynamically via run_node from a function node"
    )
}

/// Go validateChatModeWiring's error text (validation.go:233).
fn chat_wiring_error(agent: &str, from: &str) -> String {
    format!(
        "Agent \"{agent}\" has mode='chat' and cannot follow node \"{from}\": chat agents rely on conversation history and cannot consume a predecessor's node input. Use mode='single_turn', or wire the agent directly from Start"
    )
}

#[test]
fn test_validate_no_task_mode_graph_nodes() {
    // rejects task-mode agent as the only graph node
    let task_agent = new_agent("doer", Mode::Task);
    let err = build("wf-task-root", vec![e(&start(), &new_node(&task_agent))])
        .expect_err("expected error rejecting task-mode static node, got nil");
    assert_eq!(err, task_mode_error("doer"));

    // rejects task-mode agent deeper in the graph
    let chat_node = new_node(&new_agent("chatter", Mode::Chat));
    let task_node = new_node(&new_agent("doer", Mode::Task));
    let err = build(
        "wf-task-downstream",
        vec![e(&start(), &chat_node), e(&chat_node, &task_node)],
    )
    .expect_err("expected error rejecting task-mode static node, got nil");
    assert_eq!(err, task_mode_error("doer"));

    // accepts chat-mode agent as static node
    if let Err(err) = build(
        "wf-chat",
        vec![e(&start(), &new_node(&new_agent("chatter", Mode::Chat)))],
    ) {
        panic!("chat-mode agent should be accepted as static node; got {err}");
    }

    // accepts single_turn-mode agent as static node
    if let Err(err) = build(
        "wf-single-turn",
        vec![e(
            &start(),
            &new_node(&new_agent("responder", Mode::SingleTurn)),
        )],
    ) {
        panic!("single_turn-mode agent should be accepted as static node; got {err}");
    }

    // accepts mode-unset (single_turn at a node) agent as static node
    if let Err(err) = build(
        "wf-default",
        vec![e(&start(), &new_node(&new_agent("default", Mode::Unset)))],
    ) {
        panic!("mode-unset agent should be accepted as static node; got {err}");
    }
}

// A chat-mode agent builds its prompt from conversation history and ignores
// node input, so it is only allowed directly after Start.
#[test]
fn test_validate_chat_mode_wiring() {
    // rejects chat-mode agent following a non-Start node
    let responder = new_node(&new_agent("responder", Mode::SingleTurn));
    let coordinator = new_node(&new_agent("coordinator", Mode::Chat));
    let err = build(
        "wf-chat-midgraph",
        vec![e(&start(), &responder), e(&responder, &coordinator)],
    )
    .expect_err("expected error rejecting chat-mode node with a non-Start predecessor, got nil");
    assert_eq!(err, chat_wiring_error("coordinator", "responder"));

    // rejects chat-mode agent with a Start edge plus a non-Start edge
    let responder = new_node(&new_agent("responder", Mode::SingleTurn));
    let coordinator = new_node(&new_agent("coordinator", Mode::Chat));
    let err = build(
        "wf-chat-mixed-edges",
        vec![e(&start(), &coordinator), e(&start(), &responder), e(&responder, &coordinator)],
    )
    .expect_err("expected error: a non-Start edge into a chat node is rejected even when a Start edge also exists, got nil");
    assert_eq!(err, chat_wiring_error("coordinator", "responder"));

    // accepts chat-mode agent directly after Start
    let coordinator = new_node(&new_agent("coordinator", Mode::Chat));
    if let Err(err) = build("wf-chat-root", vec![e(&start(), &coordinator)]) {
        panic!("chat-mode agent after Start should be accepted; got {err}");
    }

    // allows single_turn agent following a non-Start node
    let first = new_node(&new_agent("first", Mode::SingleTurn));
    let second = new_node(&new_agent("second", Mode::SingleTurn));
    if let Err(err) = build(
        "wf-single-turn-chain",
        vec![e(&start(), &first), e(&first, &second)],
    ) {
        panic!("single_turn agent mid-graph should be accepted; got {err}");
    }
}

// An undeclared sub-agent of a chat coordinator resolves single_turn at a graph
// node, so it may sit mid-chain, whichever of the coordinator and the node is built first.
#[test]
fn test_validate_chat_mode_wiring_undeclared_sub_agent_may_follow_a_node() {
    let sub = llm_agent::new(LlmAgentConfig {
        name: "w".into(),
        ..Default::default()
    })
    .expect("llmagent.New(sub)");
    // Coordinator first, which is the order that used to decide the answer.
    llm_agent::new(LlmAgentConfig {
        name: "coordinator".into(),
        mode: Mode::Chat,
        sub_agents: vec![sub.clone()],
        ..Default::default()
    })
    .expect("llmagent.New(coordinator)");

    let other = new_agent("other", Mode::SingleTurn);
    let first = new_node(&other);
    let second = new_node(&sub);

    if let Err(err) = build(
        "wf-undeclared-midchain",
        vec![e(&start(), &first), e(&first, &second)],
    ) {
        panic!(
            "an undeclared sub-agent resolves single_turn at a node and may follow one; got {err}"
        );
    }
}
