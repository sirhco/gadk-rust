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

//! Ports of ADK-Go `server/adkrest/internal/services/agentgraphgenerator_test.go`.
//!
//! The Go tests inspect gographviz's analysed graph; these inspect
//! [`Graph`](crate::rest::agent_graph::Graph), its port. The expected DOT constants at the end
//! were produced by ADK-Go (`controllers.AgentGraphAPIController.BuildGraphImageHandler`,
//! `dark_mode=true`) for the same agent trees.

use std::collections::HashSet;
use std::sync::Arc;

use gadk_agent::{Agent, AgentConfig, AgentType, Tool};
use gadk_engine::llm::LlmAgentConfig;
use gadk_engine::loop_agent::{self, LoopAgentConfig};
use gadk_engine::parallel_agent::{self, ParallelAgentConfig};
use gadk_engine::sequential_agent::{self, SequentialAgentConfig};
use gadk_engine::testutil::MockModel;
use gadk_engine::tool::AgentTool;

use crate::rest::agent_graph::{
    Attrs, DARK_GREEN, DARK_THEME, Edge, Graph, Holder, Instance, LIGHT_GRAY, LIGHT_GREEN,
    agent_graph, build_graph, draw_cluster, draw_edge, draw_node, edge_highlighted,
    get_agent_graph, highlighted, node_caption, node_name, node_shape, should_build_agent_cluster,
};

/// Go `newTestAgent`: `CustomAgent` and `LLMAgent` both build an LLM agent.
fn new_test_agent(
    name: &str,
    agent_type: AgentType,
    sub_agents: Vec<Arc<dyn Agent>>,
    tools: Vec<Arc<dyn Tool>>,
) -> Arc<dyn Agent> {
    let agent = AgentConfig {
        name: name.into(),
        sub_agents: sub_agents.clone(),
        ..Default::default()
    };
    match agent_type {
        AgentType::SequentialAgent => sequential_agent::new(SequentialAgentConfig { agent }),
        AgentType::LoopAgent => loop_agent::new(LoopAgentConfig {
            agent,
            max_iterations: 1,
        }),
        AgentType::ParallelAgent => parallel_agent::new(ParallelAgentConfig { agent }),
        AgentType::CustomAgent | AgentType::LlmAgent => {
            gadk_engine::llm_agent::new(LlmAgentConfig {
                name: name.into(),
                model: Some(MockModel::new(vec![])),
                tools,
                sub_agents,
                ..Default::default()
            })
        }
        other => panic!("Unsupported agent type: {other:?}"),
    }
    .expect("failed to create agent")
}

fn leaf(name: &str) -> Arc<dyn Agent> {
    new_test_agent(name, AgentType::LlmAgent, vec![], vec![])
}

/// Go `mockTool`.
struct MockTool(&'static str);

impl Tool for MockTool {
    fn name(&self) -> &str {
        self.0
    }
    fn description(&self) -> &str {
        ""
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn mock_tool(name: &'static str) -> Arc<dyn Tool> {
    Arc::new(MockTool(name))
}

fn pairs(p: &[&[&str]]) -> Vec<Vec<String>> {
    p.iter()
        .map(|pair| pair.iter().map(|s| (*s).to_owned()).collect())
        .collect()
}

fn attrs(p: &[(&str, &str)]) -> Attrs {
    p.iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

/// Go `lookupEdge`: the one edge from `src` to `dst`, if any.
fn lookup_edge<'g>(graph: &'g Graph, src: &str, dst: &str) -> Option<&'g Edge> {
    let edges = graph.edges_between(src, dst);
    assert!(edges.len() <= 1, "Expected 1 edge, got {}", edges.len());
    edges.first().copied()
}

#[test]
fn test_node_name() {
    let agent = new_test_agent("TestAgent", AgentType::CustomAgent, vec![], vec![]);
    let tool = mock_tool("TestTool");
    let cases = [
        ("agent", Instance::Agent(&*agent), "TestAgent"),
        ("tool", Instance::Tool(&*tool), "TestTool"),
        ("unknown", Instance::Other, "Unknown instance type"),
    ];
    for (name, instance, expected) in cases {
        assert_eq!(node_name(instance), expected, "{name}");
    }
}

#[test]
fn test_node_caption() {
    let llm = new_test_agent("LLMAgent", AgentType::LlmAgent, vec![], vec![]);
    let seq = new_test_agent("SeqAgent", AgentType::SequentialAgent, vec![], vec![]);
    let lp = new_test_agent("LoopAgent", AgentType::LoopAgent, vec![], vec![]);
    let par = new_test_agent("ParAgent", AgentType::ParallelAgent, vec![], vec![]);
    let tool = mock_tool("TestTool");
    let cases = [
        ("llm agent", Instance::Agent(&*llm), "\"🤖 LLMAgent\""),
        (
            "sequential agent",
            Instance::Agent(&*seq),
            "\"SeqAgent (SequentialAgent)\"",
        ),
        (
            "loop agent",
            Instance::Agent(&*lp),
            "\"LoopAgent (LoopAgent)\"",
        ),
        (
            "parallel agent",
            Instance::Agent(&*par),
            "\"ParAgent (ParallelAgent)\"",
        ),
        ("tool", Instance::Tool(&*tool), "\"🔧 TestTool\""),
        (
            "unknown",
            Instance::Other,
            "\"Unsupported agent or tool type\"",
        ),
    ];
    for (name, instance, expected) in cases {
        assert_eq!(node_caption(instance), expected, "{name}");
    }
}

#[test]
fn test_node_shape() {
    let agent = new_test_agent("TestAgent", AgentType::CustomAgent, vec![], vec![]);
    let tool = mock_tool("TestTool");
    let cases = [
        ("agent", Instance::Agent(&*agent), "ellipse"),
        ("tool", Instance::Tool(&*tool), "box"),
        ("unknown", Instance::Other, "cylinder"),
    ];
    for (name, instance, expected) in cases {
        assert_eq!(node_shape(instance), expected, "{name}");
    }
}

#[test]
fn test_should_build_agent_cluster() {
    let llm = new_test_agent("LLMAgent", AgentType::LlmAgent, vec![], vec![]);
    let seq = new_test_agent("SeqAgent", AgentType::SequentialAgent, vec![], vec![]);
    let lp = new_test_agent("LoopAgent", AgentType::LoopAgent, vec![], vec![]);
    let par = new_test_agent("ParAgent", AgentType::ParallelAgent, vec![], vec![]);
    let tool = mock_tool("TestTool");
    let cases = [
        ("llm agent", Instance::Agent(&*llm), false),
        ("sequential agent", Instance::Agent(&*seq), true),
        ("loop agent", Instance::Agent(&*lp), true),
        ("parallel agent", Instance::Agent(&*par), true),
        ("tool", Instance::Tool(&*tool), false),
        ("unknown", Instance::Other, false),
    ];
    for (name, instance, expected) in cases {
        assert_eq!(should_build_agent_cluster(instance), expected, "{name}");
    }
}

#[test]
fn test_highlighted() {
    let cases = [
        ("no highlight", "NodeA", pairs(&[]), false),
        ("node in pair", "NodeA", pairs(&[&["NodeA", "NodeB"]]), true),
        (
            "node not in pair",
            "NodeC",
            pairs(&[&["NodeA", "NodeB"]]),
            false,
        ),
        (
            "multiple pairs",
            "NodeB",
            pairs(&[&["NodeA", "NodeB"], &["NodeC", "NodeD"]]),
            true,
        ),
    ];
    for (name, node, highlighted_pairs, expected) in cases {
        assert_eq!(highlighted(node, &highlighted_pairs), expected, "{name}");
    }
}

#[test]
fn test_edge_highlighted() {
    let cases = [
        ("no highlight pairs", "A", "B", pairs(&[]), None),
        (
            "matching forward",
            "A",
            "B",
            pairs(&[&["A", "B"]]),
            Some(true),
        ),
        (
            "matching backward",
            "B",
            "A",
            pairs(&[&["A", "B"]]),
            Some(false),
        ),
        ("no match", "C", "D", pairs(&[&["A", "B"]]), None),
        ("partial match", "A", "C", pairs(&[&["A", "B"]]), None),
    ];
    for (name, from, to, highlighted_pairs, expected) in cases {
        assert_eq!(
            edge_highlighted(from, to, &highlighted_pairs),
            expected,
            "{name}"
        );
    }
}

#[test]
fn test_draw_node() {
    let my_agent = new_test_agent("MyAgent", AgentType::CustomAgent, vec![], vec![]);
    let highlighted_agent =
        new_test_agent("HighlightedAgent", AgentType::CustomAgent, vec![], vec![]);
    let my_tool = mock_tool("MyTool");
    let cases = [
        (
            "draw agent node",
            Instance::Agent(&*my_agent),
            pairs(&[]),
            attrs(&[
                ("color", LIGHT_GRAY),
                ("label", "\"🤖 MyAgent\""),
                ("shape", "ellipse"),
                ("fontcolor", LIGHT_GRAY),
                ("style", "rounded"),
            ]),
        ),
        (
            "draw agent node highlighted",
            Instance::Agent(&*highlighted_agent),
            pairs(&[&["HighlightedAgent", "Tool1"]]),
            attrs(&[
                ("color", DARK_GREEN),
                ("label", "\"🤖 HighlightedAgent\""),
                ("shape", "ellipse"),
                ("fontcolor", LIGHT_GRAY),
                ("style", "filled"),
            ]),
        ),
        (
            "draw tool node",
            Instance::Tool(&*my_tool),
            pairs(&[]),
            attrs(&[
                ("color", LIGHT_GRAY),
                ("label", "\"🔧 MyTool\""),
                ("shape", "box"),
                ("fontcolor", LIGHT_GRAY),
                ("style", "rounded"),
            ]),
        ),
    ];
    for (name, instance, highlighted_pairs, expected) in cases {
        let mut graph = Graph::new("G");
        let mut visited = HashSet::new();
        draw_node(
            Holder::Parent,
            &mut graph,
            instance,
            &highlighted_pairs,
            &mut visited,
            DARK_THEME,
        );
        let node_name = node_name(instance);
        let node = graph
            .nodes
            .get(&node_name)
            .unwrap_or_else(|| panic!("{name}: Agent node not found in graph"));
        assert_eq!(node, &expected, "{name}: drawNode mismatch");
        assert!(
            visited.contains(&node_name),
            "{name}: Agent node not marked as visited"
        );
    }
}

#[test]
fn test_draw_cluster_node() {
    let mut graph = Graph::new("G");
    let mut visited = HashSet::new();
    let agent = new_test_agent("MyClusterAgent", AgentType::SequentialAgent, vec![], vec![]);
    draw_node(
        Holder::Parent,
        &mut graph,
        Instance::Agent(&*agent),
        &[],
        &mut visited,
        DARK_THEME,
    );
    let cluster = graph
        .sub_graphs
        .get("cluster_MyClusterAgent")
        .expect("Cluster not found in graph");
    assert_eq!(
        cluster["label"], "\"MyClusterAgent (SequentialAgent)\"",
        "Cluster label mismatch"
    );
    assert_eq!(cluster["style"], "rounded", "Cluster style mismatch");
    assert!(
        visited.contains("MyClusterAgent"),
        "Cluster agent not marked as visited"
    );
}

#[test]
fn test_draw_edge() {
    let cases = [
        (
            "draw unhighlighted edge",
            "NodeA",
            "NodeB",
            pairs(&[]),
            attrs(&[("color", LIGHT_GRAY), ("arrowhead", "none")]),
        ),
        (
            "draw highlighted edge",
            "NodeC",
            "NodeD",
            pairs(&[&["NodeC", "NodeD"]]),
            attrs(&[("color", LIGHT_GREEN), ("arrowhead", "normal")]),
        ),
        (
            "draw highlighted backward edge",
            "NodeE",
            "NodeF",
            pairs(&[&["NodeF", "NodeE"]]),
            attrs(&[
                ("color", LIGHT_GREEN),
                ("arrowhead", "normal"),
                ("dir", "back"),
            ]),
        ),
    ];
    for (name, from, to, highlighted_pairs, expected) in cases {
        let mut graph = Graph::new("G");
        for node in [from, to] {
            graph.add_node("G", node, Attrs::new());
        }
        draw_edge(&mut graph, from, to, &highlighted_pairs, DARK_THEME);
        let edge = lookup_edge(&graph, from, to)
            .unwrap_or_else(|| panic!("{name}: Edge between {from} and {to} not found"));
        assert_eq!(edge.attrs, expected, "{name}: drawEdge mismatch");
    }
}

#[test]
fn test_draw_cluster() {
    let cases = [
        ("sequential agent cluster", AgentType::SequentialAgent),
        ("parallel agent cluster", AgentType::ParallelAgent),
        ("loop agent cluster", AgentType::LoopAgent),
    ];
    for (name, agent_type) in cases {
        let mut parent = Graph::new("ParentG");
        let mut visited = HashSet::new();
        let sub_agent1 = leaf("SubAgent1");
        let sub_agent2 = leaf("SubAgent2");
        let parent_agent = new_test_agent(
            "ParentAgent",
            agent_type,
            vec![sub_agent1, sub_agent2],
            vec![],
        );

        // Go passes a fresh, unnamed gographviz graph as the cluster.
        draw_cluster(
            &mut parent,
            "",
            &*parent_agent,
            &[],
            &mut visited,
            DARK_THEME,
        );

        assert!(
            parent.is_node("SubAgent1") && parent.is_node("SubAgent2"),
            "{name}: Sub-agents not drawn as nodes in parent graph"
        );
        match agent_type {
            AgentType::SequentialAgent => {
                let edge = lookup_edge(&parent, "SubAgent1", "SubAgent2").unwrap_or_else(|| {
                    panic!("{name}: Edge between SubAgent1 and SubAgent2 not found")
                });
                assert_eq!(
                    edge.attrs["arrowhead"], "none",
                    "{name}: Sequential agent edge arrowhead mismatch"
                );
            }
            AgentType::ParallelAgent => {
                assert!(
                    lookup_edge(&parent, "SubAgent1", "SubAgent2").is_none()
                        && lookup_edge(&parent, "ParSubAgent2", "ParSubAgent1").is_none(),
                    "{name}: Unexpected edge found between parallel sub-agents"
                );
            }
            AgentType::LoopAgent => {
                assert!(
                    lookup_edge(&parent, "SubAgent1", "SubAgent2").is_some(),
                    "{name}: Edge between SubAgent1 and SubAgent2 not found"
                );
                assert!(
                    lookup_edge(&parent, "SubAgent1", "SubAgent2").is_some(),
                    "{name}: Edge between SubAgent1 and LoopSubAgent1 not found"
                );
            }
            other => panic!("Wrong agent type provided: {other:?}"),
        }
    }
}

#[test]
fn test_build_graph() {
    let mut graph = Graph::new("G");
    let mut visited = HashSet::new();
    let sub_agent1 = new_test_agent(
        "SubAgent1",
        AgentType::LlmAgent,
        vec![],
        vec![mock_tool("Tool1")],
    );
    let sub_agent2 = leaf("SubAgent2");
    let main_agent = new_test_agent(
        "MainAgent",
        AgentType::LlmAgent,
        vec![sub_agent1, sub_agent2],
        vec![mock_tool("Tool2")],
    );

    build_graph(
        Holder::Parent,
        &mut graph,
        Instance::Agent(&*main_agent),
        &[],
        &mut visited,
        DARK_THEME,
    );

    for node_name in ["MainAgent", "SubAgent1", "SubAgent2", "Tool1", "Tool2"] {
        assert!(
            graph.is_node(node_name),
            "Node {node_name} not found in graph"
        );
        assert!(
            visited.contains(node_name),
            "Node {node_name} not marked as visited"
        );
    }
    assert!(
        lookup_edge(&graph, "MainAgent", "Tool2").is_some(),
        "Edge from MainAgent to Tool2 not found"
    );
    assert!(
        lookup_edge(&graph, "SubAgent1", "Tool1").is_some(),
        "Edge from SubAgent1 to Tool1 not found"
    );
    // The same tree's DOT text, as ADK-Go writes it.
    assert_eq!(get_agent_graph(&*main_agent, &[]), BUILD_GRAPH_TREE);
}

/// Go `GetAgentGraph` plus `parseGraph`: the DOT text (checked against ADK-Go's) and the graph it
/// was written from.
fn graph_of(root: &Arc<dyn Agent>, want_dot: &str) -> Graph {
    let dot = get_agent_graph(&**root, &[]);
    assert!(!dot.starts_with("error:"), "GetAgentGraph: {dot}");
    pretty_assertions::assert_eq!(dot, want_dot, "DOT differs from ADK-Go's");
    agent_graph(&**root, &[], DARK_THEME)
}

// parity: adapted: inspects the generator's graph model instead of re-parsing the DOT (gadk has no DOT parser), and also checks the DOT bytes against ADK-Go's output for each tree
#[test]
fn test_nested_workflow_graph() {
    // sequential with nested parallel
    {
        let fan_out = new_test_agent(
            "fan_out",
            AgentType::ParallelAgent,
            vec![leaf("a"), leaf("b")],
            vec![],
        );
        let pipeline = new_test_agent(
            "pipeline",
            AgentType::SequentialAgent,
            vec![fan_out, leaf("merge")],
            vec![],
        );
        let g = graph_of(&pipeline, SEQUENTIAL_WITH_NESTED_PARALLEL);
        assert!(
            g.is_sub_graph("cluster_pipeline") && g.is_sub_graph("cluster_fan_out"),
            "missing nested clusters"
        );
        assert!(g.parents_of("cluster_fan_out").contains("cluster_pipeline"));
        for name in ["a", "b"] {
            assert!(
                g.is_node(name) && g.parents_of(name).contains("cluster_fan_out"),
                "node {name}"
            );
        }
        assert!(g.is_node("merge") && g.parents_of("merge").contains("cluster_pipeline"));
        let edge =
            lookup_edge(&g, "b", "merge").expect("missing edge from the nested cluster to merge");
        assert_eq!(
            edge.attrs.get("ltail").map(String::as_str),
            Some("cluster_fan_out")
        );
        assert!(
            !g.is_node("cluster_fan_out"),
            "cluster id was emitted as a node"
        );
        assert!(
            g.render().contains("compound=true"),
            "missing compound=true"
        );
    }

    // loop with nested sequential
    {
        let inner = new_test_agent(
            "inner",
            AgentType::SequentialAgent,
            vec![leaf("x"), leaf("y")],
            vec![],
        );
        let cycle = new_test_agent(
            "cycle",
            AgentType::LoopAgent,
            vec![inner, leaf("tail")],
            vec![],
        );
        let g = graph_of(&cycle, LOOP_WITH_NESTED_SEQUENTIAL);
        let forward = lookup_edge(&g, "y", "tail");
        assert!(
            forward
                .is_some_and(|e| e.attrs.get("ltail").map(String::as_str) == Some("cluster_inner")),
            "forward edge = {forward:?}, want y -> tail ltail=cluster_inner"
        );
        let back = lookup_edge(&g, "tail", "x");
        assert!(
            back.is_some_and(|e| e.attrs.get("lhead").map(String::as_str) == Some("cluster_inner")),
            "back edge = {back:?}, want tail -> x lhead=cluster_inner"
        );
        assert!(
            lookup_edge(&g, "x", "y").is_some(),
            "missing edge x -> y inside nested sequential"
        );
        assert!(
            !g.is_node("cluster_inner"),
            "cluster id was emitted as a node"
        );
    }

    // three level nesting
    {
        let fan = new_test_agent(
            "fan",
            AgentType::ParallelAgent,
            vec![leaf("a"), leaf("b")],
            vec![],
        );
        let mid = new_test_agent("mid", AgentType::SequentialAgent, vec![fan], vec![]);
        let outer = new_test_agent(
            "outer",
            AgentType::SequentialAgent,
            vec![mid, leaf("merge")],
            vec![],
        );
        let g = graph_of(&outer, THREE_LEVEL_NESTING);
        assert!(
            g.is_sub_graph("cluster_mid") && g.parents_of("cluster_mid").contains("cluster_outer")
        );
        assert!(
            g.is_sub_graph("cluster_fan") && g.parents_of("cluster_fan").contains("cluster_mid")
        );
        for name in ["a", "b"] {
            assert!(
                g.is_node(name) && g.parents_of(name).contains("cluster_fan"),
                "node {name}"
            );
        }
        let edge = lookup_edge(&g, "b", "merge");
        assert!(
            edge.is_some_and(|e| e.attrs.get("ltail").map(String::as_str) == Some("cluster_mid")),
            "edge = {edge:?}, want b -> merge ltail=cluster_mid"
        );
        assert!(
            !g.is_node("fan") && !g.is_node("cluster_fan") && !g.is_node("cluster_mid"),
            "cluster id was emitted as a node"
        );
    }

    // empty cluster
    {
        let fan_out = new_test_agent("fan_out", AgentType::ParallelAgent, vec![], vec![]);
        let s = new_test_agent(
            "s",
            AgentType::SequentialAgent,
            vec![fan_out, leaf("merge")],
            vec![],
        );
        let g = graph_of(&s, EMPTY_CLUSTER);
        assert!(
            g.is_sub_graph("cluster_fan_out")
                && g.parents_of("cluster_fan_out").contains("cluster_s")
        );
        assert!(
            !g.is_node("cluster_fan_out"),
            "cluster id was emitted as a node"
        );
        assert!(
            lookup_edge(&g, "cluster_fan_out", "merge").is_none(),
            "empty cluster was used as an edge endpoint"
        );
        assert!(g.is_node("merge") && g.parents_of("merge").contains("cluster_s"));
    }

    // workflow also used as a tool
    {
        let research = new_test_agent(
            "research",
            AgentType::ParallelAgent,
            vec![leaf("web"), leaf("docs")],
            vec![],
        );
        let triage = new_test_agent(
            "triage",
            AgentType::LlmAgent,
            vec![],
            vec![AgentTool::new(research.clone(), None)],
        );
        let root = new_test_agent(
            "root",
            AgentType::SequentialAgent,
            vec![triage, research, leaf("writer")],
            vec![],
        );
        let g = graph_of(&root, WORKFLOW_ALSO_USED_AS_A_TOOL);
        for (src, dst) in [("triage", "research"), ("research", "writer")] {
            let edge =
                lookup_edge(&g, src, dst).unwrap_or_else(|| panic!("missing edge {src} -> {dst}"));
            assert!(
                !edge.attrs.contains_key("ltail") && !edge.attrs.contains_key("lhead"),
                "edge {src} -> {dst} attrs = {:?}, want no ltail or lhead",
                edge.attrs
            );
        }
        assert_eq!(
            g.nodes
                .get("research")
                .and_then(|a| a.get("shape"))
                .map(String::as_str),
            Some("box"),
            "research node, want the tool box"
        );
        assert!(
            !g.is_sub_graph("cluster_research") && !g.is_node("web") && !g.is_node("docs"),
            "undrawn workflow was anchored inside its cluster"
        );
    }
}

const BUILD_GRAPH_TREE: &str = "digraph AgentGraph {\n\
\tbgcolor=\"#333537\";\n\
\trankdir=LR;\n\
\tMainAgent->Tool2[ arrowhead=none, color=\"#cccccc\" ];\n\
\tSubAgent1->Tool1[ arrowhead=none, color=\"#cccccc\" ];\n\
\tMainAgent [ color=\"#cccccc\", fontcolor=\"#cccccc\", label=\"🤖 MainAgent\", shape=ellipse, style=rounded ];\n\
\tSubAgent1 [ color=\"#cccccc\", fontcolor=\"#cccccc\", label=\"🤖 SubAgent1\", shape=ellipse, style=rounded ];\n\
\tSubAgent2 [ color=\"#cccccc\", fontcolor=\"#cccccc\", label=\"🤖 SubAgent2\", shape=ellipse, style=rounded ];\n\
\tTool1 [ color=\"#cccccc\", fontcolor=\"#cccccc\", label=\"🔧 Tool1\", shape=box, style=rounded ];\n\
\tTool2 [ color=\"#cccccc\", fontcolor=\"#cccccc\", label=\"🔧 Tool2\", shape=box, style=rounded ];\n\
\n\
}\n\
";

const EMPTY_CLUSTER: &str = "digraph AgentGraph {\n\
\tbgcolor=\"#333537\";\n\
\trankdir=LR;\n\
\tsubgraph cluster_s {\n\
\tcolor=\"#ffffff\";\n\
\tfontcolor=\"#cccccc\";\n\
\tlabel=\"s (SequentialAgent)\";\n\
\tstyle=rounded;\n\
\tsubgraph cluster_fan_out {\n\
\tcolor=\"#ffffff\";\n\
\tfontcolor=\"#cccccc\";\n\
\tlabel=\"fan_out (ParallelAgent)\";\n\
\tstyle=rounded;\n\
\n\
}\n\
;\n\
\tmerge [ color=\"#cccccc\", fontcolor=\"#cccccc\", label=\"🤖 merge\", shape=ellipse, style=rounded ];\n\
\n\
}\n\
;\n\
\n\
}\n\
";

const LOOP_WITH_NESTED_SEQUENTIAL: &str = "digraph AgentGraph {\n\
\tbgcolor=\"#333537\";\n\
\tcompound=true;\n\
\trankdir=LR;\n\
\tx->y[ arrowhead=none, color=\"#cccccc\" ];\n\
\ty->tail[ arrowhead=none, color=\"#cccccc\", ltail=cluster_inner ];\n\
\ttail->x[ arrowhead=none, color=\"#cccccc\", lhead=cluster_inner ];\n\
\tsubgraph cluster_cycle {\n\
\tcolor=\"#ffffff\";\n\
\tfontcolor=\"#cccccc\";\n\
\tlabel=\"cycle (LoopAgent)\";\n\
\tstyle=rounded;\n\
\tsubgraph cluster_inner {\n\
\tcolor=\"#ffffff\";\n\
\tfontcolor=\"#cccccc\";\n\
\tlabel=\"inner (SequentialAgent)\";\n\
\tstyle=rounded;\n\
\tx [ color=\"#cccccc\", fontcolor=\"#cccccc\", label=\"🤖 x\", shape=ellipse, style=rounded ];\n\
\ty [ color=\"#cccccc\", fontcolor=\"#cccccc\", label=\"🤖 y\", shape=ellipse, style=rounded ];\n\
\n\
}\n\
;\n\
\ttail [ color=\"#cccccc\", fontcolor=\"#cccccc\", label=\"🤖 tail\", shape=ellipse, style=rounded ];\n\
\n\
}\n\
;\n\
\n\
}\n\
";

const SEQUENTIAL_WITH_NESTED_PARALLEL: &str = "digraph AgentGraph {\n\
\tbgcolor=\"#333537\";\n\
\tcompound=true;\n\
\trankdir=LR;\n\
\tb->merge[ arrowhead=none, color=\"#cccccc\", ltail=cluster_fan_out ];\n\
\tsubgraph cluster_pipeline {\n\
\tcolor=\"#ffffff\";\n\
\tfontcolor=\"#cccccc\";\n\
\tlabel=\"pipeline (SequentialAgent)\";\n\
\tstyle=rounded;\n\
\tsubgraph cluster_fan_out {\n\
\tcolor=\"#ffffff\";\n\
\tfontcolor=\"#cccccc\";\n\
\tlabel=\"fan_out (ParallelAgent)\";\n\
\tstyle=rounded;\n\
\ta [ color=\"#cccccc\", fontcolor=\"#cccccc\", label=\"🤖 a\", shape=ellipse, style=rounded ];\n\
\tb [ color=\"#cccccc\", fontcolor=\"#cccccc\", label=\"🤖 b\", shape=ellipse, style=rounded ];\n\
\n\
}\n\
;\n\
\tmerge [ color=\"#cccccc\", fontcolor=\"#cccccc\", label=\"🤖 merge\", shape=ellipse, style=rounded ];\n\
\n\
}\n\
;\n\
\n\
}\n\
";

const THREE_LEVEL_NESTING: &str = "digraph AgentGraph {\n\
\tbgcolor=\"#333537\";\n\
\tcompound=true;\n\
\trankdir=LR;\n\
\tb->merge[ arrowhead=none, color=\"#cccccc\", ltail=cluster_mid ];\n\
\tsubgraph cluster_outer {\n\
\tcolor=\"#ffffff\";\n\
\tfontcolor=\"#cccccc\";\n\
\tlabel=\"outer (SequentialAgent)\";\n\
\tstyle=rounded;\n\
\tsubgraph cluster_mid {\n\
\tcolor=\"#ffffff\";\n\
\tfontcolor=\"#cccccc\";\n\
\tlabel=\"mid (SequentialAgent)\";\n\
\tstyle=rounded;\n\
\tsubgraph cluster_fan {\n\
\tcolor=\"#ffffff\";\n\
\tfontcolor=\"#cccccc\";\n\
\tlabel=\"fan (ParallelAgent)\";\n\
\tstyle=rounded;\n\
\ta [ color=\"#cccccc\", fontcolor=\"#cccccc\", label=\"🤖 a\", shape=ellipse, style=rounded ];\n\
\tb [ color=\"#cccccc\", fontcolor=\"#cccccc\", label=\"🤖 b\", shape=ellipse, style=rounded ];\n\
\n\
}\n\
;\n\
\n\
}\n\
;\n\
\tmerge [ color=\"#cccccc\", fontcolor=\"#cccccc\", label=\"🤖 merge\", shape=ellipse, style=rounded ];\n\
\n\
}\n\
;\n\
\n\
}\n\
";

const WORKFLOW_ALSO_USED_AS_A_TOOL: &str = "digraph AgentGraph {\n\
\tbgcolor=\"#333537\";\n\
\trankdir=LR;\n\
\ttriage->research[ arrowhead=none, color=\"#cccccc\" ];\n\
\tresearch->writer[ arrowhead=none, color=\"#cccccc\" ];\n\
\tsubgraph cluster_root {\n\
\tcolor=\"#ffffff\";\n\
\tfontcolor=\"#cccccc\";\n\
\tlabel=\"root (SequentialAgent)\";\n\
\tstyle=rounded;\n\
\tresearch [ color=\"#cccccc\", fontcolor=\"#cccccc\", label=\"🔧 research\", shape=box, style=rounded ];\n\
\ttriage [ color=\"#cccccc\", fontcolor=\"#cccccc\", label=\"🤖 triage\", shape=ellipse, style=rounded ];\n\
\twriter [ color=\"#cccccc\", fontcolor=\"#cccccc\", label=\"🤖 writer\", shape=ellipse, style=rounded ];\n\
\n\
}\n\
;\n\
\n\
}\n\
";
