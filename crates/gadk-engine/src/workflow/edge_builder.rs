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

//! `EdgeBuilder`, `chain` and `concat` (port of Go workflow/edgebuilder.go).

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::workflow::{Edge, Node, Route, StringRoute};

/// Port of Go workflow.EdgeBuilder: accumulates edges fluently.
#[derive(Clone, Default)]
pub struct EdgeBuilder {
    edges: Vec<Edge>,
}

impl EdgeBuilder {
    /// Port of Go NewEdgeBuilder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Port of Go EdgeBuilder.Add: adds an unconditional edge between two nodes.
    pub fn add(&mut self, from: &Arc<dyn Node>, to: &Arc<dyn Node>) -> &mut Self {
        self.edges.push(Edge {
            from: Arc::clone(from),
            to: Arc::clone(to),
            route: None,
        });
        self
    }

    /// Port of Go EdgeBuilder.AddRoute: adds an edge with a route condition between two nodes.
    pub fn add_route(
        &mut self,
        from: &Arc<dyn Node>,
        to: &Arc<dyn Node>,
        route: Arc<dyn Route>,
    ) -> &mut Self {
        self.edges.push(Edge {
            from: Arc::clone(from),
            to: Arc::clone(to),
            route: Some(route),
        });
        self
    }

    /// Port of Go EdgeBuilder.AddFanOut: edges from one source to each target.
    pub fn add_fan_out(&mut self, from: &Arc<dyn Node>, to: &[Arc<dyn Node>]) -> &mut Self {
        for t in to {
            self.add(from, t);
        }
        self
    }

    /// Port of Go EdgeBuilder.AddFanIn: edges from each source to one target.
    pub fn add_fan_in(&mut self, to: &Arc<dyn Node>, from: &[Arc<dyn Node>]) -> &mut Self {
        for f in from {
            self.add(f, to);
        }
        self
    }

    /// Port of Go EdgeBuilder.AddRoutes: one `StringRoute` edge per entry, added in byte-wise
    /// lexicographic order of the route key (so "10" precedes "2"). That order is observable: it
    /// drives the order successors are started in. A later duplicate key replaces an earlier one
    /// (Go map semantics).
    pub fn add_routes(
        &mut self,
        from: &Arc<dyn Node>,
        routes: impl IntoIterator<Item = (String, Arc<dyn Node>)>,
    ) -> &mut Self {
        let sorted: BTreeMap<String, Arc<dyn Node>> = routes.into_iter().collect();
        for (key, to) in sorted {
            self.add_route(from, &to, Arc::new(StringRoute(key)));
        }
        self
    }

    /// Port of Go EdgeBuilder.Build: the accumulated edges.
    pub fn build(&self) -> Vec<Edge> {
        self.edges.clone()
    }
}

/// Port of Go workflow.Chain: edges forming a chain of nodes; fewer than two nodes → no edges.
pub fn chain(nodes: &[Arc<dyn Node>]) -> Vec<Edge> {
    nodes
        .windows(2)
        .map(|w| Edge {
            from: Arc::clone(&w[0]),
            to: Arc::clone(&w[1]),
            route: None,
        })
        .collect()
}

/// Port of Go workflow.Concat: combines edge lists into a single list.
pub fn concat(parts: impl IntoIterator<Item = Vec<Edge>>) -> Vec<Edge> {
    parts.into_iter().flatten().collect()
}
