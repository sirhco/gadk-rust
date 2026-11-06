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

//! Port of Go workflow/graph_test.go.

use std::collections::HashMap;
use std::sync::Arc;

use pretty_assertions::assert_eq;

use crate::workflow::graph::Graph;
use crate::workflow::node::testing::{edge, test_node};
use crate::workflow::{Edge, START};

/// Renders edges as "From→To" strings using node names (Go edgeIDs).
fn edge_ids(es: &[Edge]) -> Vec<String> {
    es.iter()
        .map(|e| format!("{}→{}", e.from.name(), e.to.name()))
        .collect()
}

#[test]
fn test_new_graph_successors_of() {
    let a = test_node("A");
    let b = test_node("B");
    let c = test_node("C");
    let s = Arc::clone(&START);
    type Want<'a> = Vec<(&'a str, Vec<&'a str>)>;
    let cases: Vec<(&str, Vec<Edge>, Want)> = vec![
        ("empty graph", vec![], vec![]),
        (
            "single edge",
            vec![edge(&s, &a)],
            vec![("START", vec!["START→A"])],
        ),
        (
            "linear chain",
            vec![edge(&s, &a), edge(&a, &b), edge(&b, &c)],
            vec![
                ("START", vec!["START→A"]),
                ("A", vec!["A→B"]),
                ("B", vec!["B→C"]),
            ],
        ),
        (
            "fan-out from Start to three nodes",
            vec![edge(&s, &a), edge(&s, &b), edge(&s, &c)],
            vec![("START", vec!["START→A", "START→B", "START→C"])],
        ),
        (
            "fan-in: two upstreams converge on c",
            vec![edge(&s, &a), edge(&s, &b), edge(&a, &c), edge(&b, &c)],
            vec![
                ("START", vec!["START→A", "START→B"]),
                ("A", vec!["A→C"]),
                ("B", vec!["B→C"]),
            ],
        ),
        (
            "cycle a → b → a",
            vec![edge(&a, &b), edge(&b, &a)],
            vec![("A", vec!["A→B"]), ("B", vec!["B→A"])],
        ),
    ];
    for (name, edges, want) in cases {
        let want: HashMap<&str, Vec<&str>> = want.into_iter().collect();
        let g = Graph::new(edges);
        for n in [&s, &a, &b, &c] {
            let want_ids: Vec<String> = want
                .get(n.name())
                .map(|v| v.iter().map(|s| s.to_string()).collect())
                .unwrap_or_default();
            assert_eq!(
                edge_ids(g.successors_of(n)),
                want_ids,
                "{name}: successors_of({})",
                n.name()
            );
        }
    }
}

#[test]
fn test_new_graph_successors_of_node_not_in_graph() {
    let a = test_node("A");
    let b = test_node("B");
    let stranger = test_node("stranger");
    let g = Graph::new(vec![edge(&a, &b)]);
    assert!(g.successors_of(&stranger).is_empty());
    assert!(g.successors_of(&b).is_empty(), "b is terminal");
}

#[test]
fn test_new_graph_preserves_edge_order() {
    let a = test_node("A");
    let b = test_node("B");
    let c = test_node("C");
    let d = test_node("D");
    let g = Graph::new(vec![edge(&a, &c), edge(&a, &d), edge(&a, &b)]);
    assert_eq!(edge_ids(g.successors_of(&a)), ["A→C", "A→D", "A→B"]);
}
