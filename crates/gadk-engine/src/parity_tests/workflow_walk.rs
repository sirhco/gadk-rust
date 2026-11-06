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

//! Port of Go workflow/walk_test.go.

use std::sync::Arc;

use gadk_agent::{Agent, AgentConfig, agent};
use pretty_assertions::assert_eq;

use crate::workflow::node::testing::test_node;
use crate::workflow::walk::walk_agents;
use crate::workflow::{
    AgentNode, Edge, Node, NodeConfig, ParallelWorker, START, WorkflowNode, chain,
};

fn new_agent(name: &str) -> Arc<dyn Agent> {
    agent::new(AgentConfig {
        name: name.into(),
        ..Default::default()
    })
    .expect("agent::new")
}

fn agent_node(a: &Arc<dyn Agent>) -> Arc<dyn Node> {
    AgentNode::new(Arc::clone(a), NodeConfig::default()).expect("AgentNode::new")
}

fn workflow_node(name: &str, edges: Vec<Edge>) -> Arc<dyn Node> {
    WorkflowNode::new(name, edges).expect("WorkflowNode::new")
}

fn parallel_worker(wrapped: Arc<dyn Node>) -> Arc<dyn Node> {
    ParallelWorker::new("fan_out", wrapped, 0, NodeConfig::default()).expect("ParallelWorker::new")
}

/// Names of the walked agents, sorted (the walk promises no order; Go compares with cmpopts.SortSlices).
fn walked_names(edges: &[Edge]) -> Vec<String> {
    let mut names: Vec<String> = walk_agents(edges)
        .iter()
        .map(|a| a.name().to_owned())
        .collect();
    names.sort();
    names
}

// parity: adapted: the "nodes that run no agent" case uses a no-op test node instead of NewFunctionNode (Task 10, a later batch)
#[test]
fn test_walk_agents() {
    let s = Arc::clone(&START);
    let cases: Vec<(&str, Vec<Edge>, Vec<&str>)> = vec![
        (
            "agent nodes on the edges",
            chain(&[
                Arc::clone(&s),
                agent_node(&new_agent("first")),
                agent_node(&new_agent("second")),
            ]),
            vec!["first", "second"],
        ),
        (
            "a node on two edges is visited once",
            {
                let (middle, last) = (
                    agent_node(&new_agent("middle")),
                    agent_node(&new_agent("last")),
                );
                vec![
                    Edge {
                        from: Arc::clone(&s),
                        to: Arc::clone(&middle),
                        route: None,
                    },
                    Edge {
                        from: middle,
                        to: last,
                        route: None,
                    },
                ]
            },
            vec!["last", "middle"],
        ),
        (
            "a node that only starts an edge",
            {
                let (first, second) = (
                    agent_node(&new_agent("first")),
                    agent_node(&new_agent("second")),
                );
                vec![Edge {
                    from: first,
                    to: second,
                    route: None,
                }]
            },
            vec!["first", "second"],
        ),
        (
            "agents in a nested sub-workflow",
            {
                let inner = workflow_node(
                    "inner",
                    chain(&[Arc::clone(&s), agent_node(&new_agent("buried"))]),
                );
                chain(&[Arc::clone(&s), agent_node(&new_agent("outer")), inner])
            },
            vec!["buried", "outer"],
        ),
        (
            "the agent a ParallelWorker runs",
            chain(&[
                Arc::clone(&s),
                parallel_worker(agent_node(&new_agent("per_item"))),
            ]),
            vec!["per_item"],
        ),
        (
            "agents in a sub-workflow a ParallelWorker runs",
            {
                let per_item = workflow_node(
                    "per_item_graph",
                    chain(&[Arc::clone(&s), agent_node(&new_agent("step"))]),
                );
                chain(&[Arc::clone(&s), parallel_worker(per_item)])
            },
            vec!["step"],
        ),
        (
            "an agent two nodes run is returned twice",
            {
                let shared = new_agent("shared");
                let inner = workflow_node("inner", chain(&[Arc::clone(&s), agent_node(&shared)]));
                chain(&[Arc::clone(&s), agent_node(&shared), inner])
            },
            vec!["shared", "shared"],
        ),
        (
            "nodes that run no agent",
            chain(&[Arc::clone(&s), test_node("echo")]),
            vec![],
        ),
    ];
    for (name, edges, want) in cases {
        assert_eq!(walked_names(&edges), want, "{name}");
    }
}
