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

//! Port of Go workflow/validation_test.go.

use std::any::Any;
use std::fmt;
use std::sync::Arc;

use futures::StreamExt;
use gadk_agent::{AgentError, Context, EventStream};
use gadk_session::Event;
use pretty_assertions::assert_eq;
use rstest::rstest;
use serde_json::{Value, json};

use crate::error::{WorkflowError, WorkflowErrorKind as K};
use crate::tool::utils::CompiledSchema;
use crate::workflow::graph::Graph;
use crate::workflow::node::testing::{edge, route_edge, test_node, test_node_with_schemas};
use crate::workflow::route::{OutgoingSummary, RouteKind, RouteValue, register_route_validator};
use crate::workflow::validation::{
    default_validate_input, schema_is_string, summarize_outgoing_edges, validate_connectivity,
    validate_cycles, validate_default_route, validate_fan_in, validate_start_node_no_incoming,
    validate_start_node_present, validate_sub_workflow_names, validate_unique_edges,
    validate_unique_names,
};
use crate::workflow::{
    BaseNode, BoolRoute, DEFAULT, Edge, IntRoute, JoinNode, MultiRoute, Node, NodeConfig, Route,
    START, StringRoute, Workflow, WorkflowNode, WorkflowOptions,
};

fn compiled(v: Value) -> CompiledSchema {
    CompiledSchema::new(v).expect("schema compiles")
}

fn string_route(s: &str) -> Arc<dyn Route> {
    Arc::new(StringRoute(s.into()))
}

#[rstest]
#[case::unique_names(("A", "B", "C"), None)]
#[case::duplicate_node_names_in_from(("A", "A", "C"), Some("duplicate node name: A"))]
#[case::duplicate_node_names_in_to(("A", "B", "A"), Some("duplicate node name: A"))]
fn test_unique_names(#[case] names: (&str, &str, &str), #[case] expect: Option<&str>) {
    let (a, b, c) = (test_node(names.0), test_node(names.1), test_node(names.2));
    let edges = vec![edge(&a, &b), edge(&b, &c)];
    let res = validate_unique_names(&edges);
    match expect {
        Some(msg) => {
            let e = res.expect_err("want duplicate-name error");
            assert!(e.to_string().contains(msg), "got {e}");
            assert_eq!(e.kind(), K::DuplicateNodeName);
        }
        None => assert!(res.is_ok(), "{res:?}"),
    }
}

#[test]
fn test_start_node_present() {
    let s = Arc::clone(&START);
    let (a, b) = (test_node("A"), test_node("B"));
    assert!(
        validate_start_node_present(&[edge(&s, &a), edge(&a, &b)]).is_ok(),
        "with start node"
    );
    let (a, b) = (test_node("A"), test_node("B"));
    let e = validate_start_node_present(&[edge(&a, &b)]).expect_err("no start node");
    assert!(e.to_string().contains("no start node found"), "got {e}");
    assert_eq!(e.kind(), K::NoStartNode);
}

#[test]
fn test_start_node_no_incoming_edges() {
    let s = Arc::clone(&START);
    let (a, b) = (test_node("A"), test_node("B"));
    assert!(validate_start_node_no_incoming(&[edge(&s, &a), edge(&a, &b)]).is_ok());
    let (a, b) = (test_node("A"), test_node("B"));
    let e = validate_start_node_no_incoming(&[edge(&a, &s), edge(&s, &b)])
        .expect_err("node points to start");
    assert!(
        e.to_string().contains("node points to start node: A"),
        "got {e}"
    );
    assert_eq!(e.kind(), K::NodePointsToStart);
}

#[test]
fn test_duplicate_edges() {
    let (a, b) = (test_node("A"), test_node("B"));
    let check = |edges: Vec<Edge>| validate_unique_edges(&Graph::new(edges));

    assert!(check(vec![edge(&a, &b)]).is_ok(), "single edge");

    let e = check(vec![edge(&a, &b), edge(&a, &b)]).expect_err("duplicate edge");
    assert_eq!(e.kind(), K::DuplicateEdge);

    let e = check(vec![
        route_edge(&a, &b, string_route("test1")),
        route_edge(&a, &b, string_route("test2")),
    ])
    .expect_err("duplicate routed edge");
    assert_eq!(e.kind(), K::DuplicateEdge);

    let e = check(vec![
        route_edge(&a, &b, string_route("test1")),
        edge(&a, &b),
    ])
    .expect_err("routed + unrouted");
    assert_eq!(e.kind(), K::DuplicateEdge);

    assert!(check(vec![]).is_ok(), "empty");
}

#[test]
fn test_default_route() {
    let (a, b, c) = (test_node("A"), test_node("B"), test_node("C"));
    assert!(
        validate_default_route(&Graph::new(vec![route_edge(&a, &b, Arc::clone(&DEFAULT))])).is_ok()
    );
    let e = validate_default_route(&Graph::new(vec![
        route_edge(&a, &b, Arc::clone(&DEFAULT)),
        route_edge(&a, &c, Arc::clone(&DEFAULT)),
    ]))
    .expect_err("multiple default routes");
    assert!(e.is(K::MultipleDefaultRoutes), "got {e}");
}

#[test]
fn test_connectivity() {
    let s = Arc::clone(&START);
    let (a, b, c) = (test_node("A"), test_node("B"), test_node("C"));
    assert!(
        validate_connectivity(&Graph::new(vec![edge(&s, &a), edge(&a, &b), edge(&b, &c)])).is_ok()
    );
    let (a, b, c) = (test_node("A"), test_node("B"), test_node("C"));
    let e = validate_connectivity(&Graph::new(vec![edge(&s, &a), edge(&b, &c)]))
        .expect_err("unreachable");
    assert!(
        e.to_string()
            .contains(r#"nodes not reachable from start node: "B, C""#),
        "got {e}"
    );
}

#[test]
fn test_validate_cycles() {
    let s = Arc::clone(&START);
    let n = test_node;
    let cases: Vec<(&str, Vec<Edge>, bool)> = vec![
        {
            let (a, b) = (n("A"), n("B"));
            ("no cycles", vec![edge(&s, &a), edge(&a, &b)], false)
        },
        {
            let (a, b, c, d) = (n("A"), n("B"), n("C"), n("D"));
            (
                "no cycles diamond graph",
                vec![
                    edge(&s, &a),
                    edge(&a, &b),
                    edge(&a, &c),
                    edge(&b, &d),
                    edge(&c, &d),
                ],
                false,
            )
        },
        {
            let (a, b) = (n("A"), n("B"));
            (
                "only conditional cycle",
                vec![
                    edge(&s, &a),
                    edge(&a, &b),
                    route_edge(&b, &a, string_route("back")),
                ],
                false,
            )
        },
        {
            let (a, b, c, d) = (n("A"), n("B"), n("C"), n("D"));
            (
                "both conditional and unconditional cycles",
                vec![
                    edge(&s, &a),
                    edge(&a, &b),
                    route_edge(&b, &a, string_route("back")),
                    edge(&s, &c),
                    edge(&c, &d),
                    edge(&d, &c), // Unconditional
                ],
                true,
            )
        },
        {
            let (a, b) = (n("A"), n("B"));
            (
                "cycle with default route",
                vec![
                    edge(&s, &a),
                    edge(&a, &b),
                    route_edge(&b, &a, Arc::clone(&DEFAULT)),
                ],
                false,
            )
        },
        ("empty graph", vec![], false),
    ];
    for (name, edges, expect_err) in cases {
        let res = validate_cycles(&Graph::new(edges));
        assert_eq!(res.is_err(), expect_err, "{name}: {res:?}");
        if let Err(e) = res {
            assert_eq!(e.kind(), K::UnconditionalCycle, "{name}");
        }
    }
}

#[test]
fn test_validate_fan_in() {
    let s = Arc::clone(&START);
    let n = test_node;

    // non-Join diamond fan-in rejected
    let (a, b, c, d) = (n("A"), n("B"), n("C"), n("D"));
    let e = validate_fan_in(&Graph::new(vec![
        edge(&s, &a),
        edge(&a, &b),
        edge(&a, &c),
        edge(&b, &d),
        edge(&c, &d),
    ]))
    .expect_err("non-Join diamond");
    assert!(e.is(K::UnsupportedFanIn), "got {e}");

    // JoinNode diamond fan-in allowed
    let (a, b, c) = (n("A"), n("B"), n("C"));
    let j: Arc<dyn Node> = JoinNode::new("J");
    assert!(
        validate_fan_in(&Graph::new(vec![
            edge(&s, &a),
            edge(&a, &b),
            edge(&a, &c),
            edge(&b, &j),
            edge(&c, &j),
        ]))
        .is_ok()
    );

    // conditional loop-back not rejected
    let (a, b) = (n("A"), n("B"));
    assert!(
        validate_fan_in(&Graph::new(vec![
            edge(&s, &a),
            edge(&a, &b),
            route_edge(&b, &a, string_route("retry")),
        ]))
        .is_ok()
    );

    // conditional fan-in not rejected
    let (a, b, c, d) = (n("A"), n("B"), n("C"), n("D"));
    assert!(
        validate_fan_in(&Graph::new(vec![
            edge(&s, &a),
            edge(&a, &b),
            edge(&a, &c),
            route_edge(&b, &d, string_route("x")),
            route_edge(&c, &d, string_route("y")),
        ]))
        .is_ok()
    );
}

#[test]
fn test_new_non_join_fan_in_rejected() {
    let s = Arc::clone(&START);
    let (a, b, c, d) = (
        test_node("A"),
        test_node("B"),
        test_node("C"),
        test_node("D"),
    );
    let res = Workflow::new(
        "wf",
        vec![
            edge(&s, &a),
            edge(&a, &b),
            edge(&a, &c),
            edge(&b, &d),
            edge(&c, &d),
        ],
        WorkflowOptions::default(),
    );
    match res {
        Err(e) => assert!(e.is(K::UnsupportedFanIn), "got {e}"),
        Ok(_) => panic!("New() succeeded, want UnsupportedFanIn"),
    }
}

#[test]
fn test_validate_sub_workflow_names() {
    let sub = Workflow::new(
        "inner_wf",
        vec![edge(&START, &test_node("A"))],
        WorkflowOptions::default(),
    )
    .expect("sub-workflow");
    let wf_node: Arc<dyn Node> = Arc::new(WorkflowNode {
        base: BaseNode::new("nested_node", "", NodeConfig::default()),
        sub_workflow: sub,
    });
    let edges = vec![edge(&START, &wf_node)];

    // no collision
    assert!(validate_sub_workflow_names("outer_wf", &edges).is_ok());
    // collision with sub-workflow name
    let e = validate_sub_workflow_names("inner_wf", &edges).expect_err("collision");
    assert!(
        e.to_string()
            .contains(r#"sub-workflow name collision: "inner_wf""#),
        "got {e}"
    );
}

// parity: adapted: schemas are JSON literals; Go float64 JSON numbers are serde_json numbers (json!(123), json!(42))
#[test]
fn test_default_validate_input() {
    let integer = compiled(json!({"type": "integer"}));
    let enum_schema = compiled(json!({"type": "string", "enum": ["yes", "no"]}));
    let struct_schema = compiled(json!({
        "type": "object",
        "properties": {"x": {"type": "integer"}, "y": {"type": "string"}},
        "required": ["x", "y"],
        "additionalProperties": false
    }));
    let string_schema = compiled(json!({"type": "string"}));

    // nil schema returns input unchanged
    assert_eq!(
        default_validate_input(json!("hello"), None).expect("no schema"),
        json!("hello")
    );
    // nil input returns nil
    assert_eq!(
        default_validate_input(Value::Null, Some(&integer)).expect("nil input"),
        Value::Null
    );
    // string parsed as JSON integer
    assert_eq!(
        default_validate_input(json!("123"), Some(&integer)).expect("parsed integer"),
        json!(123)
    );
    // string matches enum schema
    assert_eq!(
        default_validate_input(json!("yes"), Some(&enum_schema)).expect("enum"),
        json!("yes")
    );
    // string fails integer schema
    assert!(default_validate_input(json!("plain text"), Some(&integer)).is_err());
    // map validates against struct schema
    let m = json!({"x": 42, "y": "hello"});
    assert_eq!(
        default_validate_input(m.clone(), Some(&struct_schema)).expect("struct"),
        m
    );
    // string schema accepts string
    assert_eq!(
        default_validate_input(json!("hello"), Some(&string_schema)).expect("string"),
        json!("hello")
    );
    // JSON string missing required field fails
    assert!(default_validate_input(json!(r#"{"x":1}"#), Some(&struct_schema)).is_err());
}

// parity: adapted: jsonschema-go Types ports as a JSON "type" array
#[test]
fn test_schema_is_string() {
    assert!(!schema_is_string(None), "nil schema");
    assert!(schema_is_string(Some(&compiled(json!({"type": "string"})))));
    assert!(!schema_is_string(Some(&compiled(
        json!({"type": "integer"})
    ))));
    assert!(schema_is_string(Some(&compiled(
        json!({"type": ["integer", "string"]})
    ))));
    assert!(!schema_is_string(Some(&compiled(
        json!({"type": ["integer", "boolean"]})
    ))));
}

// parity: adapted: jsonschema.For outputs are JSON literals; PropertyOrder (a Go rendering hint) is modelled as differing key insertion order
#[test]
fn test_static_schema_validation() {
    // jsonschema.For[schemaTypeA] / [schemaTypeB].
    let schema_a = json!({"type": "object", "properties": {"x": {"type": "integer"}, "y": {"type": "string"}}, "required": ["x", "y"], "additionalProperties": false});
    let schema_b = json!({"type": "object", "properties": {"val": {"type": "string"}}, "required": ["val"], "additionalProperties": false});
    // schemaA1 / schemaA2: same properties, different order.
    let schema_a1 = json!({"type": "object", "properties": {"foo": {"type": "string"}, "bar": {"type": "integer"}}});
    let schema_a2 = json!({"type": "object", "properties": {"bar": {"type": "integer"}, "foo": {"type": "string"}}});
    // jsonschema.For[compatTypeA] / [compatTypeB].
    let compat = json!({"type": "object", "properties": {"foo": {"type": "string"}, "bar": {"type": "integer"}}, "required": ["foo", "bar"], "additionalProperties": false});
    // jsonschema.For[reorderTypeA] / [reorderTypeB]: "required" in field order.
    let reorder_a = compat.clone();
    let reorder_b = json!({"type": "object", "properties": {"bar": {"type": "integer"}, "foo": {"type": "string"}}, "required": ["bar", "foo"], "additionalProperties": false});

    type Case<'a> = (&'a str, Option<Value>, Option<Value>, Option<&'a str>);
    let cases: Vec<Case> = vec![
        (
            "same Go type -> success",
            Some(schema_a.clone()),
            Some(schema_a.clone()),
            None,
        ),
        (
            "different Go types -> error",
            Some(schema_a.clone()),
            Some(schema_b),
            Some("schema mismatch on edge A -> B"),
        ),
        (
            "only one endpoint has schema -> success",
            Some(schema_a),
            None,
            None,
        ),
        (
            "same schema with different PropertyOrder -> success",
            Some(schema_a1),
            Some(schema_a2),
            None,
        ),
        (
            "different Go types with same fields -> success",
            Some(compat.clone()),
            Some(compat),
            None,
        ),
        (
            "differ only in required order -> success",
            Some(reorder_a),
            Some(reorder_b),
            None,
        ),
    ];
    for (name, a_out, b_in, expect) in cases {
        let a = test_node_with_schemas("A", None, a_out);
        let b = test_node_with_schemas("B", b_in, None);
        let res = Workflow::new(
            "test_wf",
            vec![edge(&START, &a), edge(&a, &b)],
            WorkflowOptions::default(),
        );
        match (expect, res) {
            (Some(msg), Err(e)) => assert!(e.to_string().contains(msg), "{name}: got {e}"),
            (Some(msg), Ok(_)) => panic!("{name}: expected error containing {msg:?}, got nil"),
            (None, Err(e)) => panic!("{name}: expected no error, got: {e}"),
            (None, Ok(_)) => {}
        }
    }
}

struct CustomTestRoute;

impl Route for CustomTestRoute {
    fn matches(&self, _: &Event) -> bool {
        false
    }
}

struct CustomBoolStringer;

impl fmt::Display for CustomBoolStringer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("yes")
    }
}

// parity: adapted: Go MultiRoute[any]{…, Default} has no Rust equivalent (a Vec<Value> cannot hold the DEFAULT route) and Go %T names become std::any::type_name
#[test]
fn test_summarize_outgoing_edges() {
    let from = test_node("from");
    let routes: Vec<Option<Arc<dyn Route>>> = vec![
        None,
        Some(Arc::clone(&DEFAULT)),
        Some(string_route("billing")),
        Some(Arc::new(BoolRoute(true))),
        Some(Arc::new(IntRoute(7))),
        Some(Arc::new(MultiRoute(vec!["a".to_string(), "b".to_string()]))),
        Some(Arc::new(MultiRoute(vec![false]))),
        Some(Arc::new(MultiRoute(vec![8_i64]))),
        Some(Arc::new(MultiRoute(vec![
            json!("dyn"),
            json!(true),
            Value::Null,
        ]))),
        Some(Arc::new(MultiRoute(vec![StringRoute("sr".into())]))),
        Some(Arc::new(MultiRoute(vec![BoolRoute(true)]))),
        Some(Arc::new(MultiRoute(vec![CustomBoolStringer]))),
        Some(Arc::new(MultiRoute::<String>(vec![]))),
        Some(Arc::new(CustomTestRoute)),
    ];
    let edges: Vec<Edge> = routes
        .into_iter()
        .enumerate()
        .map(|(i, route)| Edge {
            from: Arc::clone(&from),
            to: test_node(&format!("n{}", "x".repeat(i + 1))),
            route,
        })
        .collect();

    let v = |kind: RouteKind, label: &str| RouteValue {
        kind,
        label: label.to_owned(),
    };
    let want = OutgoingSummary {
        has_unrouted: true,
        has_default: true,
        routes: vec![
            v(RouteKind::String, "\"billing\""),
            v(RouteKind::Bool, "true"),
            v(RouteKind::Other, "7"),
            v(RouteKind::String, "\"a\""),
            v(RouteKind::String, "\"b\""),
            v(RouteKind::Bool, "false"),
            v(RouteKind::Other, "8"),
            v(RouteKind::String, "\"dyn\""),
            v(RouteKind::Bool, "true"),
            v(RouteKind::Other, "<nil>"),
            v(RouteKind::String, "\"sr\""),
            v(RouteKind::Bool, "true"),
            v(
                RouteKind::Other,
                std::any::type_name::<CustomBoolStringer>(),
            ),
            v(RouteKind::Other, "[]"),
            v(RouteKind::Other, std::any::type_name::<CustomTestRoute>()),
        ],
    };
    assert_eq!(summarize_outgoing_edges(&edges), want);
}

struct FailingEdgeValidatorNode {
    base: BaseNode,
}

impl Node for FailingEdgeValidatorNode {
    fn name(&self) -> &str {
        self.base.name()
    }
    fn description(&self) -> &str {
        self.base.description()
    }
    fn config(&self) -> &NodeConfig {
        self.base.config()
    }
    fn input_schema(&self) -> Option<&Value> {
        self.base.input_schema()
    }
    fn output_schema(&self) -> Option<&Value> {
        self.base.output_schema()
    }
    fn validate_input(&self, v: Value) -> Result<Value, WorkflowError> {
        self.base.validate_input(v)
    }
    fn validate_output(&self, v: Value) -> Result<Value, WorkflowError> {
        self.base.validate_output(v)
    }
    fn run(&self, _ctx: Context, _input: Value) -> EventStream {
        futures::stream::empty::<Result<Event, AgentError>>().boxed()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[test]
fn test_validate_node_outgoing_edges_deterministic_order() {
    register_route_validator(Arc::new(|n: &dyn Node, out: &OutgoingSummary| {
        if n.as_any().is::<FailingEdgeValidatorNode>() && !out.routes.is_empty() {
            Err(WorkflowError::new(
                K::Other,
                format!("edge validation failed for {}", n.name()),
            ))
        } else {
            Ok(())
        }
    }));

    let failing = |name: &str| -> Arc<dyn Node> {
        Arc::new(FailingEdgeValidatorNode {
            base: BaseNode::new(name, "", NodeConfig::default()),
        })
    };
    let (alpha, zeta, sink) = (failing("alpha"), failing("zeta"), test_node("sink"));
    let res = Workflow::new(
        "wf",
        vec![
            edge(&START, &zeta),
            edge(&START, &alpha),
            route_edge(&zeta, &sink, string_route("z")),
            route_edge(&alpha, &sink, string_route("a")),
        ],
        WorkflowOptions::default(),
    );
    match res {
        Err(e) => assert!(
            e.to_string().contains("edge validation failed for alpha"),
            "New() error = {e}, want deterministic alpha failure first"
        ),
        Ok(_) => panic!("New() succeeded, want deterministic alpha failure first"),
    }
}
