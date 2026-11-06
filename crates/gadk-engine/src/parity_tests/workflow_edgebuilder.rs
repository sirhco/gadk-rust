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

//! Port of Go workflow/edgebuilder_test.go.

use std::sync::Arc;

use pretty_assertions::assert_eq;

use crate::workflow::node::same_node;
use crate::workflow::node::testing::test_node;
use crate::workflow::{
    Edge, EdgeBuilder, JoinNode, MultiRoute, Node, RouteSummary, START, StringRoute, Workflow,
    WorkflowOptions,
};

fn summary(e: &Edge) -> Option<RouteSummary> {
    e.route.as_ref().map(|r| r.summary())
}

type WantEdge<'a> = (&'a Arc<dyn Node>, &'a Arc<dyn Node>, Option<RouteSummary>);

fn assert_edges(got: &[Edge], want: &[WantEdge<'_>], case: &str) {
    assert_eq!(got.len(), want.len(), "{case}: edge count");
    for (i, (g, (from, to, route))) in got.iter().zip(want).enumerate() {
        assert!(
            same_node(g.from.as_ref(), from.as_ref())
                && same_node(g.to.as_ref(), to.as_ref())
                && summary(g) == *route,
            "{case}: edge {i} = {}→{} (route {:?}), want {}→{} (route {:?})",
            g.from.name(),
            g.to.name(),
            summary(g),
            from.name(),
            to.name(),
            route
        );
    }
}

// parity: adapted: Go reflect.DeepEqual on Route values ports as equality of Route::summary()
#[test]
fn test_edge_builder() {
    let (a, b, c, d) = (
        test_node("A"),
        test_node("B"),
        test_node("C"),
        test_node("D"),
    );

    let mut eb = EdgeBuilder::new();
    eb.add(&a, &b);
    assert_edges(&eb.build(), &[(&a, &b, None)], "Add");

    let mut eb = EdgeBuilder::new();
    eb.add_route(&a, &b, Arc::new(StringRoute("test-route".into())));
    assert_edges(
        &eb.build(),
        &[(&a, &b, Some(RouteSummary::String("test-route".into())))],
        "AddRoute",
    );

    let mut eb = EdgeBuilder::new();
    eb.add_route(&a, &b, Arc::new(MultiRoute(vec![1_i64, 2, 3])));
    let multi = RouteSummary::Multi(vec![
        RouteSummary::Int(1),
        RouteSummary::Int(2),
        RouteSummary::Int(3),
    ]);
    assert_edges(&eb.build(), &[(&a, &b, Some(multi))], "AddRoute MultiRoute");

    let mut eb = EdgeBuilder::new();
    eb.add_fan_out(&a, &[Arc::clone(&b), Arc::clone(&c)]);
    assert_edges(&eb.build(), &[(&a, &b, None), (&a, &c, None)], "AddFanOut");

    let mut eb = EdgeBuilder::new();
    eb.add_fan_in(&a, &[Arc::clone(&b), Arc::clone(&c)]);
    assert_edges(&eb.build(), &[(&b, &a, None), (&c, &a, None)], "AddFanIn");

    let mut eb = EdgeBuilder::new();
    eb.add_routes(
        &a,
        [
            ("workflow_C".to_string(), Arc::clone(&c)),
            ("beta".to_string(), Arc::clone(&d)),
            ("42".to_string(), Arc::clone(&b)),
        ],
    );
    assert_edges(
        &eb.build(),
        &[
            (&a, &b, Some(RouteSummary::String("42".into()))),
            (&a, &d, Some(RouteSummary::String("beta".into()))),
            (&a, &c, Some(RouteSummary::String("workflow_C".into()))),
        ],
        "AddRoutes",
    );
}

#[test]
fn test_edge_builder_add_routes_sorts_by_route() {
    let from = test_node("router");
    let routes: Vec<(String, Arc<dyn Node>)> = vec![
        ("tech".into(), test_node("tech_node")),
        ("sales".into(), test_node("sales_node")),
        ("billing".into(), test_node("billing_node")),
        ("abuse".into(), test_node("abuse_node")),
    ];
    let mut eb = EdgeBuilder::new();
    eb.add_routes(&from, routes.clone());
    let edges = eb.build();
    assert_eq!(edges.len(), routes.len());
    let mut got = Vec::new();
    for (i, e) in edges.iter().enumerate() {
        let Some(RouteSummary::String(r)) = summary(e) else {
            panic!("edge {i}: want StringRoute")
        };
        assert!(same_node(e.from.as_ref(), from.as_ref()), "edge {i} from");
        let want_to = &routes
            .iter()
            .find(|(k, _)| *k == r)
            .expect("route was built")
            .1;
        assert!(
            same_node(e.to.as_ref(), want_to.as_ref()),
            "edge {i} route {r:?} target"
        );
        got.push(r);
    }
    assert_eq!(got, ["abuse", "billing", "sales", "tech"]);
}

#[test]
fn test_edge_builder_add_routes_sorts_bytewise() {
    let from = test_node("router");
    let (a, b, c, d) = (
        test_node("a"),
        test_node("b"),
        test_node("c"),
        test_node("d"),
    );
    let mut eb = EdgeBuilder::new();
    eb.add_routes(
        &from,
        [
            ("alpha".to_string(), a),
            ("Beta".to_string(), b),
            ("2".to_string(), d),
            ("10".to_string(), c),
        ],
    );
    let got: Vec<String> = eb
        .build()
        .iter()
        .map(|e| match summary(e) {
            Some(RouteSummary::String(r)) => r,
            other => panic!("want StringRoute, got {other:?}"),
        })
        .collect();
    assert_eq!(got, ["10", "2", "Beta", "alpha"]);
}

// parity: adapted: the scheduler (Task 10) is not available in Task 8; asserts that Workflow::new accepts the router→targets→JoinNode graph and that the router's successors_of order (the dispatch order the scheduler follows) is 10, 2, Beta, alpha
#[test]
fn test_edge_builder_add_routes_dispatch_order() {
    // Reverse-sorted, so a builder that kept declaration order would fail.
    let declared = ["alpha", "Beta", "2", "10"];
    let routes: Vec<(String, Arc<dyn Node>)> = declared
        .iter()
        .map(|r| ((*r).to_string(), test_node(&format!("target_{r}"))))
        .collect();
    let router = test_node("router");
    let join: Arc<dyn Node> = JoinNode::new("join");
    let mut b = EdgeBuilder::new();
    b.add(&START, &router).add_routes(&router, routes.clone());
    for (_, target) in &routes {
        b.add(target, &join);
    }

    let w = Workflow::new(
        "",
        b.build(),
        WorkflowOptions::default().with_max_concurrency(1),
    )
    .expect("New");
    let started: Vec<&str> = w
        .graph
        .successors_of(&router)
        .iter()
        .map(|e| e.to.name())
        .collect();
    assert_eq!(
        started,
        ["target_10", "target_2", "target_Beta", "target_alpha"]
    );
}
