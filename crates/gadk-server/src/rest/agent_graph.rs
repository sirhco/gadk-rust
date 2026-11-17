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

//! The Graphviz DOT text of an agent tree, in the exact format ADK servers produce for the web
//! UI's agent graph.
//!
//! [`Graph`] is the small graph model the generator needs: attributes, nodes, edges, subgraphs and
//! parent/child relations, plus the writer that renders it as DOT. The writer emits the graph
//! attributes sorted by name, then every edge in insertion order, then the subgraphs of the root
//! sorted by name (each with its attributes, then its children sorted by name, nested subgraphs
//! inline), then every node not yet written sorted by name. Each statement is `\t<stmt>;\n`, a
//! subgraph statement ends in `\n}\n` (so it is followed by `;\n`), and the graph ends in `\n}\n`.
//! Names and values are written as they are; the generator quotes its labels and colors itself.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use gadk_agent::{Agent, AgentType, Tool};

/// A highlighted node's color.
pub(crate) const DARK_GREEN: &str = "\"#0F5223\"";
/// A highlighted edge's color.
pub(crate) const LIGHT_GREEN: &str = "\"#69CB87\"";
/// Light gray.
pub(crate) const LIGHT_GRAY: &str = "\"#cccccc\"";
/// White.
pub(crate) const WHITE: &str = "\"#ffffff\"";
/// The dark canvas.
pub(crate) const BACKGROUND: &str = "\"#333537\"";
/// Dark gray.
pub(crate) const DARK_GRAY: &str = "\"#3c4043\"";
/// Mid gray.
pub(crate) const MID_GRAY: &str = "\"#5f6368\"";
/// The light canvas.
pub(crate) const LIGHT_BACKGROUND: &str = "\"#ffffff\"";

/// The palette an agent graph is drawn with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Theme {
    /// The graph canvas color.
    pub background: &'static str,
    /// Node labels, node borders and unhighlighted edges.
    pub foreground: &'static str,
    /// The outline of a workflow-agent cluster.
    pub cluster_border: &'static str,
}

/// The dark palette, ADK's long-standing default.
pub(crate) const DARK_THEME: Theme = Theme {
    background: BACKGROUND,
    foreground: LIGHT_GRAY,
    cluster_border: WHITE,
};

/// The palette for a light-mode page.
pub(crate) const LIGHT_THEME: Theme = Theme {
    background: LIGHT_BACKGROUND,
    foreground: DARK_GRAY,
    cluster_border: MID_GRAY,
};

/// Maps the web UI's `dark_mode` query parameter to a palette.
pub(crate) fn theme_for(dark_mode: bool) -> Theme {
    if dark_mode { DARK_THEME } else { LIGHT_THEME }
}

/// The workflow agent types drawn as clusters.
const SUPPORTED_CLUSTER_AGENTS: [AgentType; 3] = [
    AgentType::LoopAgent,
    AgentType::SequentialAgent,
    AgentType::ParallelAgent,
];

/// Attribute name to value, iterated sorted by name as the writer does.
pub(crate) type Attrs = BTreeMap<String, String>;

/// A directed edge (never with ports).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Edge {
    pub src: String,
    pub dst: String,
    pub attrs: Attrs,
}

/// The graph the generator builds and serialises.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Graph {
    /// The graph's name.
    pub name: String,
    /// The graph's attributes.
    pub attrs: Attrs,
    /// Node name to attributes.
    pub nodes: BTreeMap<String, Attrs>,
    /// The edges, in insertion order.
    pub edges: Vec<Edge>,
    /// Subgraph name to its attributes.
    pub sub_graphs: BTreeMap<String, Attrs>,
    /// Parent name to the names of its child nodes and subgraphs.
    pub relations: BTreeMap<String, BTreeSet<String>>,
}

fn to_attrs(pairs: &[(&str, &str)]) -> Attrs {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

impl Graph {
    /// An empty graph called `name`.
    pub(crate) fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            ..Self::default()
        }
    }

    /// Whether `name` is a node.
    pub(crate) fn is_node(&self, name: &str) -> bool {
        self.nodes.contains_key(name)
    }

    /// Whether `name` is a subgraph.
    pub(crate) fn is_sub_graph(&self, name: &str) -> bool {
        self.sub_graphs.contains_key(name)
    }

    fn is_cluster_sub_graph(&self, name: &str) -> bool {
        self.is_sub_graph(name) && name.starts_with("cluster")
    }

    /// Sets an attribute: on the root graph when `graph` is its name, else on that subgraph. An
    /// unknown graph name is ignored; the generator never passes one.
    fn add_attr(&mut self, graph: &str, field: &str, value: &str) {
        let attrs = if graph == self.name {
            &mut self.attrs
        } else if let Some(attrs) = self.sub_graphs.get_mut(graph) {
            attrs
        } else {
            return;
        };
        attrs.insert(field.to_owned(), value.to_owned());
    }

    fn add_relation(&mut self, parent: &str, child: &str) {
        self.relations
            .entry(parent.to_owned())
            .or_default()
            .insert(child.to_owned());
    }

    /// Adds a node under `parent`: a node already present has its attributes extended (overwritten
    /// per name).
    pub(crate) fn add_node(&mut self, parent: &str, name: &str, attrs: Attrs) {
        self.nodes.entry(name.to_owned()).or_default().extend(attrs);
        self.add_relation(parent, name);
    }

    /// Adds a subgraph under `parent`.
    pub(crate) fn add_sub_graph(&mut self, parent: &str, name: &str, attrs: Attrs) {
        self.add_relation(parent, name);
        self.sub_graphs.entry(name.to_owned()).or_default();
        for (k, v) in &attrs {
            self.add_attr(name, k, v);
        }
    }

    /// Adds a directed edge.
    pub(crate) fn add_edge(&mut self, src: &str, dst: &str, attrs: Attrs) {
        self.edges.push(Edge {
            src: src.to_owned(),
            dst: dst.to_owned(),
            attrs,
        });
    }

    /// The edges from `src` to `dst`.
    #[cfg(test)]
    pub(crate) fn edges_between(&self, src: &str, dst: &str) -> Vec<&Edge> {
        self.edges
            .iter()
            .filter(|e| e.src == src && e.dst == dst)
            .collect()
    }

    /// The parents of `child`.
    #[cfg(test)]
    pub(crate) fn parents_of(&self, child: &str) -> BTreeSet<&str> {
        self.relations
            .iter()
            .filter(|(_, children)| children.contains(child))
            .map(|(parent, _)| parent.as_str())
            .collect()
    }

    /// The DOT text, or `error: <err>` when an edge names something that is neither a node nor a
    /// subgraph.
    pub(crate) fn render(&self) -> String {
        match Writer::new(self).write() {
            Ok(s) => s,
            Err(e) => format!("error: {e}"),
        }
    }
}

/// An attribute list, sorted by name: `[ a=b, c=d ] `, or nothing when empty.
fn attr_list(attrs: &Attrs) -> String {
    if attrs.is_empty() {
        return String::new();
    }
    let list: Vec<String> = attrs.iter().map(|(k, v)| format!("{k}={v}")).collect();
    format!("[ {} ] ", list.join(", "))
}

/// The non-empty statements, each as `\t<stmt>;\n`.
fn stmt_list(stmts: &[String]) -> String {
    stmts
        .iter()
        .filter(|s| !s.is_empty())
        .map(|s| format!("\t{s};\n"))
        .collect()
}

/// The DOT writer.
struct Writer<'g> {
    graph: &'g Graph,
    written: HashSet<String>,
}

impl<'g> Writer<'g> {
    fn new(graph: &'g Graph) -> Self {
        Self {
            graph,
            written: HashSet::new(),
        }
    }

    fn node_stmt(&mut self, name: &str) -> String {
        self.written.insert(name.to_owned());
        let attrs = self
            .graph
            .nodes
            .get(name)
            .map(attr_list)
            .unwrap_or_default();
        format!("{name} {attrs}").trim().to_owned()
    }

    fn sub_graph(&mut self, name: &str) -> Result<String, String> {
        self.written.insert(name.to_owned());
        let mut stmts: Vec<String> = self
            .graph
            .sub_graphs
            .get(name)
            .into_iter()
            .flatten()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        let children: Vec<&String> = self
            .graph
            .relations
            .get(name)
            .into_iter()
            .flatten()
            .collect();
        for child in children {
            if self.graph.is_node(child) {
                stmts.push(self.node_stmt(child));
            } else if self.graph.is_sub_graph(child) {
                stmts.push(self.sub_graph(child)?);
            } else {
                return Err(format!("{child} is not a node or a subgraph"));
            }
        }
        Ok(format!("subgraph {name} {{\n{}\n}}\n", stmt_list(&stmts)))
    }

    fn location(&mut self, name: &str) -> Result<String, String> {
        if self.graph.is_node(name) || self.graph.is_cluster_sub_graph(name) {
            Ok(name.to_owned())
        } else if self.graph.is_sub_graph(name) {
            self.sub_graph(name)
        } else {
            Err(format!("{name} is not a node or a subgraph"))
        }
    }

    fn write(mut self) -> Result<String, String> {
        let g = self.graph;
        let mut stmts: Vec<String> = g.attrs.iter().map(|(k, v)| format!("{k}={v}")).collect();
        for edge in &g.edges {
            let src = self.location(&edge.src)?;
            let dst = self.location(&edge.dst)?;
            let rhs = format!("->{dst}");
            let stmt = format!("{src}{}{}", rhs.trim(), attr_list(&edge.attrs));
            stmts.push(stmt.trim().to_owned());
        }
        let root_children = g.relations.get(&g.name);
        for name in g.sub_graphs.keys() {
            if !self.written.contains(name) && root_children.is_some_and(|c| c.contains(name)) {
                stmts.push(self.sub_graph(name)?);
            }
        }
        for name in g.nodes.keys() {
            if !self.written.contains(name) {
                stmts.push(self.node_stmt(name));
            }
        }
        Ok(format!(
            "digraph {} {{\n{}\n}}\n",
            g.name,
            stmt_list(&stmts)
        ))
    }
}

/// What the generator draws: an agent, a tool, or anything else.
#[derive(Clone, Copy)]
pub(crate) enum Instance<'a> {
    Agent(&'a dyn Agent),
    Tool(&'a dyn Tool),
    /// A value that is neither; only tests pass one.
    #[cfg_attr(not(test), allow(dead_code))]
    Other,
}

/// The graph a member is drawn "into": the serialised graph itself at the top level, or a
/// workflow cluster, which is represented only by its name.
#[derive(Clone, Copy)]
pub(crate) enum Holder<'a> {
    /// The serialised graph itself.
    Parent,
    /// A cluster's name holder.
    Cluster(&'a str),
}

impl Holder<'_> {
    fn name<'g>(&'g self, parent: &'g Graph) -> &'g str {
        match self {
            Holder::Parent => &parent.name,
            Holder::Cluster(name) => name,
        }
    }
}

/// The node's name: an agent's or tool's name, else `Unknown instance type`.
pub(crate) fn node_name(instance: Instance<'_>) -> String {
    match instance {
        Instance::Agent(a) => a.name().to_owned(),
        Instance::Tool(t) => t.name().to_owned(),
        Instance::Other => "Unknown instance type".to_owned(),
    }
}

fn cluster_type(agent: &dyn Agent) -> Option<AgentType> {
    let t = agent.agent_state().agent_type;
    SUPPORTED_CLUSTER_AGENTS.contains(&t).then_some(t)
}

/// The quoted label.
pub(crate) fn node_caption(instance: Instance<'_>) -> String {
    let caption = match instance {
        Instance::Agent(a) => match cluster_type(a) {
            Some(t) => format!("{} ({})", a.name(), t.as_str()),
            None => format!("🤖 {}", a.name()),
        },
        Instance::Tool(t) => format!("🔧 {}", t.name()),
        Instance::Other => "Unsupported agent or tool type".to_owned(),
    };
    format!("\"{caption}\"")
}

/// The node shape: `ellipse` for an agent, `box` for a tool, `cylinder` otherwise.
pub(crate) fn node_shape(instance: Instance<'_>) -> &'static str {
    match instance {
        Instance::Agent(_) => "ellipse",
        Instance::Tool(_) => "box",
        Instance::Other => "cylinder",
    }
}

/// Whether the instance is a workflow agent drawn as a cluster.
pub(crate) fn should_build_agent_cluster(instance: Instance<'_>) -> bool {
    matches!(instance, Instance::Agent(a) if cluster_type(a).is_some())
}

/// Whether `node_name` appears in any highlighted pair.
pub(crate) fn highlighted(node_name: &str, highlighted_pairs: &[Vec<String>]) -> bool {
    highlighted_pairs
        .iter()
        .any(|pair| pair.iter().any(|n| n == node_name))
}

/// `Some(true)` for a pair in this direction, `Some(false)` for a pair in
/// the reverse direction, `None` when the edge is not highlighted.
pub(crate) fn edge_highlighted(
    from: &str,
    to: &str,
    highlighted_pairs: &[Vec<String>],
) -> Option<bool> {
    for pair in highlighted_pairs {
        if pair.len() == 2 {
            if pair[0] == from && pair[1] == to {
                return Some(true);
            }
            if pair[0] == to && pair[1] == from {
                return Some(false);
            }
        }
    }
    None
}

/// The node an edge touches, and the cluster to clip it at (`ltail`/`lhead`)
/// when the member is a drawn, non-empty cluster.
fn edge_anchor(graph: &Graph, instance: Instance<'_>, from_cluster_end: bool) -> (String, String) {
    let Instance::Agent(agent) = instance else {
        return (node_name(instance), String::new());
    };
    if !should_build_agent_cluster(instance) {
        return (node_name(instance), String::new());
    }
    let cluster_id = format!("cluster_{}", agent.name());
    if !graph.is_sub_graph(&cluster_id) {
        return (node_name(instance), String::new());
    }
    let subs = agent.sub_agents();
    let child = if from_cluster_end {
        subs.last()
    } else {
        subs.first()
    };
    let Some(child) = child else {
        return (node_name(instance), String::new());
    };
    let (node_id, _) = edge_anchor(graph, Instance::Agent(&**child), from_cluster_end);
    if !graph.is_node(&node_id) {
        return (node_name(instance), String::new());
    }
    (node_id, cluster_id)
}

/// Draws the edge between two cluster members, clipped at their clusters when they are drawn
/// clusters.
fn draw_cluster_edge(
    graph: &mut Graph,
    from: Instance<'_>,
    to: Instance<'_>,
    highlighted_pairs: &[Vec<String>],
    theme: Theme,
) {
    let (src, ltail) = edge_anchor(graph, from, true);
    let (dst, lhead) = edge_anchor(graph, to, false);
    // cluster_<name> is a subgraph, not a node: skip the edge when either side was never drawn
    // as a node, which is what an empty cluster leaves.
    if !graph.is_node(&src) || !graph.is_node(&dst) {
        return;
    }
    draw_edge(graph, &src, &dst, highlighted_pairs, theme);
    if ltail.is_empty() && lhead.is_empty() {
        return;
    }
    let name = graph.name.clone();
    graph.add_attr(&name, "compound", "true");
    if let Some(edge) = graph.edges.last_mut() {
        if !ltail.is_empty() {
            edge.attrs.insert("ltail".to_owned(), ltail);
        }
        if !lhead.is_empty() {
            edge.attrs.insert("lhead".to_owned(), lhead);
        }
    }
}

/// Draws every member, then the sequential or loop edges between them.
pub(crate) fn draw_cluster(
    parent: &mut Graph,
    cluster: &str,
    agent: &dyn Agent,
    highlighted_pairs: &[Vec<String>],
    visited: &mut HashSet<String>,
    theme: Theme,
) {
    let subs = agent.sub_agents();
    // Every member is drawn before any edge: an edge anchor has to see whether the destination
    // cluster was actually added.
    for sub in subs {
        build_graph(
            Holder::Cluster(cluster),
            parent,
            Instance::Agent(&**sub),
            highlighted_pairs,
            visited,
            theme,
        );
    }
    match agent.agent_state().agent_type {
        AgentType::SequentialAgent => {
            for pair in subs.windows(2) {
                draw_cluster_edge(
                    parent,
                    Instance::Agent(&*pair[0]),
                    Instance::Agent(&*pair[1]),
                    highlighted_pairs,
                    theme,
                );
            }
        }
        AgentType::LoopAgent => {
            for (i, sub) in subs.iter().enumerate() {
                let next = subs.get(i + 1).unwrap_or(&subs[0]);
                draw_cluster_edge(
                    parent,
                    Instance::Agent(&**sub),
                    Instance::Agent(&**next),
                    highlighted_pairs,
                    theme,
                );
            }
        }
        // Parallel sub-agents are not connected; they are part of the subgraph.
        _ => {}
    }
}

/// Draws a cluster subgraph for a workflow agent, else a node, attached to `holder`.
pub(crate) fn draw_node(
    holder: Holder<'_>,
    parent: &mut Graph,
    instance: Instance<'_>,
    highlighted_pairs: &[Vec<String>],
    visited: &mut HashSet<String>,
    theme: Theme,
) {
    let name = node_name(instance);
    let caption = node_caption(instance);
    visited.insert(name.clone());
    if should_build_agent_cluster(instance) {
        let Instance::Agent(agent) = instance else {
            return;
        };
        let cluster = format!("cluster_{name}");
        let holder_name = holder.name(parent).to_owned();
        parent.add_sub_graph(
            &holder_name,
            &cluster,
            to_attrs(&[
                ("style", "rounded"),
                ("color", theme.cluster_border),
                ("label", &caption),
                ("fontcolor", theme.foreground),
            ]),
        );
        draw_cluster(parent, &cluster, agent, highlighted_pairs, visited, theme);
    } else {
        let (color, style) = if highlighted(&name, highlighted_pairs) {
            (DARK_GREEN, "filled")
        } else {
            (theme.foreground, "rounded")
        };
        let holder_name = holder.name(parent).to_owned();
        parent.add_node(
            &holder_name,
            &name,
            to_attrs(&[
                ("label", &caption),
                ("shape", node_shape(instance)),
                ("fontcolor", theme.foreground),
                ("color", color),
                ("style", style),
            ]),
        );
    }
}

/// Draws the edge from `from` to `to`, highlighted per `highlighted_pairs`.
pub(crate) fn draw_edge(
    graph: &mut Graph,
    from: &str,
    to: &str,
    highlighted_pairs: &[Vec<String>],
    theme: Theme,
) {
    let attrs = match edge_highlighted(from, to, highlighted_pairs) {
        Some(true) => to_attrs(&[("color", LIGHT_GREEN), ("arrowhead", "normal")]),
        Some(false) => to_attrs(&[
            ("color", LIGHT_GREEN),
            ("arrowhead", "normal"),
            ("dir", "back"),
        ]),
        None => to_attrs(&[("color", theme.foreground), ("arrowhead", "none")]),
    };
    graph.add_edge(from, to, attrs);
}

/// Draws `instance` unless already visited, then an LLM agent's tools and
/// every sub-agent.
pub(crate) fn build_graph(
    holder: Holder<'_>,
    parent: &mut Graph,
    instance: Instance<'_>,
    highlighted_pairs: &[Vec<String>],
    visited: &mut HashSet<String>,
    theme: Theme,
) {
    if matches!(instance, Instance::Other) || visited.contains(&node_name(instance)) {
        return;
    }
    draw_node(holder, parent, instance, highlighted_pairs, visited, theme);
    let Instance::Agent(agent) = instance else {
        return;
    };
    if let Some(tools) = gadk_engine::llm::agent_tools(agent) {
        for tool in &tools {
            let tool = Instance::Tool(&**tool);
            draw_node(holder, parent, tool, highlighted_pairs, visited, theme);
            // Tool edges are drawn only at the top level: the edge would be added to the cluster's
            // name holder, which is never serialised, so an LLM agent inside a workflow cluster
            // shows its tool nodes but no edges to them.
            if let Holder::Parent = holder {
                draw_edge(
                    parent,
                    &node_name(instance),
                    &node_name(tool),
                    highlighted_pairs,
                    theme,
                );
            }
        }
    }
    for sub in agent.sub_agents() {
        build_graph(
            holder,
            parent,
            Instance::Agent(&**sub),
            highlighted_pairs,
            visited,
            theme,
        );
    }
}

/// The agent tree as Graphviz DOT in the dark theme (the event graph's palette).
pub(crate) fn get_agent_graph(agent: &dyn Agent, highlighted_pairs: &[Vec<String>]) -> String {
    get_agent_graph_with_theme(agent, highlighted_pairs, DARK_THEME)
}

/// The agent tree as Graphviz DOT in `theme`. Rendering cannot fail for a graph built here, so the
/// DOT text is returned directly.
pub(crate) fn get_agent_graph_with_theme(
    agent: &dyn Agent,
    highlighted_pairs: &[Vec<String>],
    theme: Theme,
) -> String {
    agent_graph(agent, highlighted_pairs, theme).render()
}

/// The graph [`get_agent_graph_with_theme`] builds before serialising it.
pub(crate) fn agent_graph(
    agent: &dyn Agent,
    highlighted_pairs: &[Vec<String>],
    theme: Theme,
) -> Graph {
    let mut graph = Graph::new("AgentGraph");
    graph.add_attr("AgentGraph", "rankdir", "LR");
    graph.add_attr("AgentGraph", "bgcolor", theme.background);
    let mut visited = HashSet::new();
    build_graph(
        Holder::Parent,
        &mut graph,
        Instance::Agent(agent),
        highlighted_pairs,
        &mut visited,
        theme,
    );
    graph
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Arc;

    use gadk_agent::AgentConfig;
    use gadk_engine::llm::LlmAgentConfig;
    use gadk_engine::loop_agent::{self, LoopAgentConfig};
    use gadk_engine::parallel_agent::{self, ParallelAgentConfig};
    use gadk_engine::sequential_agent::{self, SequentialAgentConfig};
    use pretty_assertions::assert_eq;

    use super::*;

    struct NamedTool(&'static str);

    impl Tool for NamedTool {
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

    fn llm(name: &str, tools: &[&'static str], sub_agents: Vec<Arc<dyn Agent>>) -> Arc<dyn Agent> {
        gadk_engine::llm_agent::new(LlmAgentConfig {
            name: name.into(),
            tools: tools
                .iter()
                .map(|t| Arc::new(NamedTool(t)) as Arc<dyn Tool>)
                .collect(),
            sub_agents,
            ..Default::default()
        })
        .unwrap()
    }

    fn workflow(name: &str, sub_agents: Vec<Arc<dyn Agent>>) -> AgentConfig {
        AgentConfig {
            name: name.into(),
            sub_agents,
            ..Default::default()
        }
    }

    /// The `dotSrc` of a wire fixture's response body; `key` names the map entry when the body
    /// is keyed by agent name.
    fn fixture_dot(fixture: &str, key: Option<&str>) -> String {
        let fixture: serde_json::Value = serde_json::from_str(fixture).unwrap();
        let body: serde_json::Value =
            serde_json::from_str(fixture["response"]["body"].as_str().unwrap()).unwrap();
        let graph = match key {
            Some(k) => &body[k],
            None => &body,
        };
        graph["dotSrc"].as_str().unwrap().to_owned()
    }

    #[test]
    fn writer_reports_an_edge_to_nothing_as_go_does() {
        let mut g = Graph::new("G");
        g.add_node("G", "a", Attrs::new());
        g.add_edge("a", "b", Attrs::new());
        assert_eq!(g.render(), "error: b is not a node or a subgraph");
    }

    #[test]
    fn writer_quotes_nothing_and_writes_an_empty_graph_like_gographviz() {
        let mut g = Graph::new("G");
        g.add_node("G", "a b", Attrs::new());
        assert_eq!(g.render(), "digraph G {\n\ta b;\n\n}\n");
        assert_eq!(Graph::new("E").render(), "digraph E {\n\n}\n");
    }

    #[test]
    fn theme_for_maps_dark_mode() {
        assert_eq!(theme_for(true), DARK_THEME);
        assert_eq!(theme_for(false), LIGHT_THEME);
    }

    /// ADK-Go's DOT for a loop holding an LLM agent with tools and a parallel cluster, light
    /// theme: the tool nodes join the loop's cluster but their edges are lost with Go's
    /// name-holder graph.
    #[test]
    fn tools_inside_a_cluster_match_go() {
        let parallel = parallel_agent::new(ParallelAgentConfig {
            agent: workflow("P", vec![llm("p1", &[], vec![]), llm("p2", &[], vec![])]),
        })
        .unwrap();
        let root = loop_agent::new(LoopAgentConfig {
            agent: workflow(
                "L",
                vec![
                    llm("worker", &["t1", "t2"], vec![]),
                    parallel,
                    llm("last", &[], vec![]),
                ],
            ),
            max_iterations: 1,
        })
        .unwrap();
        assert_eq!(
            get_agent_graph_with_theme(&*root, &[], LIGHT_THEME),
            TOOLS_INSIDE_CLUSTER_LIGHT
        );
    }

    /// The probe's `assistant` (an LLM agent with tool `save_note`) highlighted as the event
    /// graph highlights a function call event (`[tool, author]`): fixtures `main_37`/`main_38`.
    #[test]
    fn event_graph_fixture_dot_matches() {
        let assistant = llm("assistant", &["save_note"], vec![]);
        let got = get_agent_graph(&*assistant, &[vec!["save_note".into(), "assistant".into()]]);
        for fixture in [
            include_str!("../../testdata/wire/main_37_event_graph.json"),
            include_str!("../../testdata/wire/main_38_event_graph_dev.json"),
        ] {
            assert_eq!(got, fixture_dot(fixture, None));
        }
    }

    /// The probe's `assistant` and `pipeline` trees: fixtures `main_41`, `main_42`, `main_43`.
    #[test]
    fn build_graph_image_fixture_dots_match() {
        let assistant = llm("assistant", &["save_note"], vec![]);
        let reviewer = llm("reviewer", &[], vec![]);
        let pipeline = sequential_agent::new(SequentialAgentConfig {
            agent: workflow(
                "pipeline",
                vec![llm("drafter", &[], vec![]), reviewer.clone()],
            ),
        })
        .unwrap();
        assert_eq!(
            get_agent_graph_with_theme(&*assistant, &[], LIGHT_THEME),
            fixture_dot(
                include_str!("../../testdata/wire/main_41_build_graph_image.json"),
                Some("assistant")
            )
        );
        assert_eq!(
            get_agent_graph_with_theme(&*pipeline, &[], DARK_THEME),
            fixture_dot(
                include_str!("../../testdata/wire/main_42_build_graph_image_pipeline_dark.json"),
                Some("pipeline")
            )
        );
        assert_eq!(
            get_agent_graph_with_theme(&*reviewer, &[], LIGHT_THEME),
            fixture_dot(
                include_str!("../../testdata/wire/main_43_build_graph_image_node.json"),
                None
            )
        );
    }

    const TOOLS_INSIDE_CLUSTER_LIGHT: &str = "digraph AgentGraph {\n\
\tbgcolor=\"#ffffff\";\n\
\tcompound=true;\n\
\trankdir=LR;\n\
\tworker->p1[ arrowhead=none, color=\"#3c4043\", lhead=cluster_P ];\n\
\tp2->last[ arrowhead=none, color=\"#3c4043\", ltail=cluster_P ];\n\
\tlast->worker[ arrowhead=none, color=\"#3c4043\" ];\n\
\tsubgraph cluster_L {\n\
\tcolor=\"#5f6368\";\n\
\tfontcolor=\"#3c4043\";\n\
\tlabel=\"L (LoopAgent)\";\n\
\tstyle=rounded;\n\
\tsubgraph cluster_P {\n\
\tcolor=\"#5f6368\";\n\
\tfontcolor=\"#3c4043\";\n\
\tlabel=\"P (ParallelAgent)\";\n\
\tstyle=rounded;\n\
\tp1 [ color=\"#3c4043\", fontcolor=\"#3c4043\", label=\"🤖 p1\", shape=ellipse, style=rounded ];\n\
\tp2 [ color=\"#3c4043\", fontcolor=\"#3c4043\", label=\"🤖 p2\", shape=ellipse, style=rounded ];\n\
\n\
}\n\
;\n\
\tlast [ color=\"#3c4043\", fontcolor=\"#3c4043\", label=\"🤖 last\", shape=ellipse, style=rounded ];\n\
\tt1 [ color=\"#3c4043\", fontcolor=\"#3c4043\", label=\"🔧 t1\", shape=box, style=rounded ];\n\
\tt2 [ color=\"#3c4043\", fontcolor=\"#3c4043\", label=\"🔧 t2\", shape=box, style=rounded ];\n\
\tworker [ color=\"#3c4043\", fontcolor=\"#3c4043\", label=\"🤖 worker\", shape=ellipse, style=rounded ];\n\
\n\
}\n\
;\n\
\n\
}\n\
";
}
