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

//! The workflow graph (port of Go workflow/graph.go).
//!
//! Go keys maps by Node interface identity; Rust keys by [`NodeId`] (the node's data address —
//! every graph node lives in an `Arc`, so the address is stable and unique while the graph holds
//! it). Go map iteration order is random; Rust iterates in first-appearance order
//! (`source_order`/`target_order`/`nodes`) so validation errors are deterministic.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::workflow::{Edge, Node, START};

/// Identity of a node: the address of its data (Go `==` on Node interface values).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct NodeId(usize);

impl NodeId {
    /// The identity of `n`.
    pub(crate) fn of(n: &dyn Node) -> NodeId {
        NodeId((n as *const dyn Node).cast::<()>() as usize)
    }
}

/// Port of Go workflow.graph: edges indexed by source and target node.
pub(crate) struct Graph {
    /// The edges, in input order.
    pub(crate) edges: Vec<Edge>,
    /// Distinct nodes in first-appearance order (`from` then `to` per edge).
    pub(crate) nodes: Vec<Arc<dyn Node>>,
    /// Position of each node in `nodes`.
    pub(crate) index: HashMap<NodeId, usize>,
    /// Outgoing edges per source node, in edge order.
    pub(crate) successors: HashMap<NodeId, Vec<Edge>>,
    /// Incoming edges per target node, in edge order.
    pub(crate) predecessors: HashMap<NodeId, Vec<Edge>>,
    /// Source nodes in first-appearance order.
    pub(crate) source_order: Vec<NodeId>,
    /// Target nodes in first-appearance order.
    pub(crate) target_order: Vec<NodeId>,
    /// Marks a synthetic single-node wrapper created by the runner to drive a standalone agent.
    pub(crate) is_root_wrapper: bool,
}

impl Graph {
    /// Port of Go newGraph (graph.go:28).
    pub(crate) fn new(edges: Vec<Edge>) -> Graph {
        let mut g = Graph {
            edges: Vec::new(),
            nodes: Vec::new(),
            index: HashMap::new(),
            successors: HashMap::new(),
            predecessors: HashMap::new(),
            source_order: Vec::new(),
            target_order: Vec::new(),
            is_root_wrapper: false,
        };
        for e in &edges {
            for n in [&e.from, &e.to] {
                let id = NodeId::of(n.as_ref());
                if !g.index.contains_key(&id) {
                    g.index.insert(id, g.nodes.len());
                    g.nodes.push(Arc::clone(n));
                }
            }
            let (f, t) = (NodeId::of(e.from.as_ref()), NodeId::of(e.to.as_ref()));
            if !g.successors.contains_key(&f) {
                g.source_order.push(f);
            }
            g.successors.entry(f).or_default().push(e.clone());
            if !g.predecessors.contains_key(&t) {
                g.target_order.push(t);
            }
            g.predecessors.entry(t).or_default().push(e.clone());
        }
        g.edges = edges;
        g
    }

    /// Port of Go graph.allEdges (graph.go:43): every edge, grouped by source in source order.
    pub(crate) fn all_edges(&self) -> Vec<Edge> {
        self.source_order
            .iter()
            .flat_map(|id| self.successors[id].iter().cloned())
            .collect()
    }

    /// Port of Go graph.successorsOf (graph.go:55): the outgoing edges of `n`; empty when `n` is
    /// terminal or not in the graph.
    pub(crate) fn successors_of(&self, n: &Arc<dyn Node>) -> &[Edge] {
        self.successors
            .get(&NodeId::of(n.as_ref()))
            .map_or(&[], Vec::as_slice)
    }

    /// Port of Go graph.predecessorsOf (graph.go:63): the incoming edges of `n`; empty when `n`
    /// has none or is not in the graph.
    pub(crate) fn predecessors_of(&self, n: &Arc<dyn Node>) -> &[Edge] {
        self.predecessors
            .get(&NodeId::of(n.as_ref()))
            .map_or(&[], Vec::as_slice)
    }

    /// Port of Go graph.allNodes (graph.go:68): every node, in first-appearance order.
    pub(crate) fn all_nodes(&self) -> &[Arc<dyn Node>] {
        &self.nodes
    }

    /// The first node named `name`.
    pub(crate) fn node_by_name(&self, name: &str) -> Option<&Arc<dyn Node>> {
        self.nodes.iter().find(|n| n.name() == name)
    }

    /// Source nodes with their outgoing edges, in source order.
    pub(crate) fn sources(&self) -> impl Iterator<Item = (&Arc<dyn Node>, &[Edge])> {
        self.source_order
            .iter()
            .map(|id| (&self.nodes[self.index[id]], self.successors[id].as_slice()))
    }

    /// Target nodes with their incoming edges, in target order.
    pub(crate) fn targets(&self) -> impl Iterator<Item = (&Arc<dyn Node>, &[Edge])> {
        self.target_order.iter().map(|id| {
            (
                &self.nodes[self.index[id]],
                self.predecessors[id].as_slice(),
            )
        })
    }

    /// Port of Go graph.terminalNodeNames (graph.go:86): names of nodes with no outgoing edges,
    /// excluding the START sentinel.
    pub(crate) fn terminal_node_names(&self) -> HashSet<String> {
        let sources: HashSet<&str> = self.sources().map(|(n, _)| n.name()).collect();
        let start_name = START.name();
        self.nodes
            .iter()
            .map(|n| n.name())
            .filter(|name| *name != start_name && !sources.contains(name))
            .map(str::to_owned)
            .collect()
    }
}
