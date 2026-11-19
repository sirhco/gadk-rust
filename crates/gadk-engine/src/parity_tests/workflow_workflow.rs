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

//! Port of Go workflow/workflow_test.go.

use std::sync::Arc;

use gadk_agent::{AgentConfig, AgentError, Context, InvocationContext};
use gadk_genai::{Content, Part};
use gadk_platform::Platform;
use gadk_session::Event;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::tool::utils::infer_schema;
use crate::workflow::scheduler::test_support::*;
use crate::workflow::{
    AgentNode, BoolRoute, DEFAULT, Edge, IntRoute, JoinNode, MultiRoute, Node, NodeConfig, Route,
    StringRoute, Workflow, WorkflowOptions, function_node, function_node_from_state,
    function_node_with_schema,
};

fn ctx_with_parts(parts: Vec<Part>) -> InvocationContext {
    InvocationContext::builder()
        .invocation_id(TEST_INVOCATION_ID)
        .platform(Platform::deterministic(1))
        .user_content(Content {
            parts,
            ..Content::default()
        })
        .build_invocation()
}

#[tokio::test]
async fn test_function_node() {
    let node = function_node::<String, String, _, _>(
        "upper",
        |_c: Context, input: String| async move { Ok(input.to_uppercase()) },
        NodeConfig::default(),
    );
    let events = drain(node.run(top_level_ctx(), json!("hello"))).await;
    assert_eq!(events.len(), 1, "expected 1 event");
    assert_eq!(events[0].output, Some(json!("HELLO")));
}

#[tokio::test]
async fn test_sequential_workflow() {
    let node_a = function_node::<Value, String, _, _>(
        "upper",
        |_c: Context, input: Value| async move {
            match input {
                Value::String(s) => Ok(s.to_uppercase()),
                _ => Err(AgentError::other("expected string input")),
            }
        },
        NodeConfig::default(),
    );
    let node_b = function_node::<String, String, _, _>(
        "suffix",
        |_c: Context, input: String| async move { Ok(format!("{input} done")) },
        NodeConfig::default(),
    );
    let w = must_new(chain(&[&start(), &node_a, &node_b]));
    let events = drain(w.run(ctx_with_parts(vec![Part::from_text("hello")]))).await;
    assert_eq!(events.len(), 2, "expected 2 events");
    assert_eq!(
        events.iter().filter_map(|e| e.output.clone()).last(),
        Some(json!("HELLO done"))
    );
}

fn routed_event() -> Event {
    Event {
        routes: vec!["hello".into(), "42".into(), "true".into()],
        ..Event::default()
    }
}

#[test]
fn test_string_route() {
    let ev = routed_event();
    assert!(
        StringRoute("hello".into()).matches(&ev),
        "StringRoute should match"
    );
    assert!(
        !StringRoute("world".into()).matches(&ev),
        "StringRoute should not match"
    );
}

#[test]
fn test_int_route() {
    let ev = routed_event();
    assert!(IntRoute(42).matches(&ev), "IntRoute should match");
    assert!(!IntRoute(10).matches(&ev), "IntRoute should not match");
}

#[test]
fn test_bool_route() {
    let ev = routed_event();
    assert!(BoolRoute(true).matches(&ev), "BoolRoute should match");
    assert!(!BoolRoute(false).matches(&ev), "BoolRoute should not match");
}

#[test]
fn test_multi_route_string() {
    let ev = routed_event();
    assert!(
        MultiRoute(vec!["world".to_string(), "hello".to_string()]).matches(&ev),
        "MultiRoute[string] should match"
    );
    assert!(
        !MultiRoute(vec!["world".to_string(), "golang".to_string()]).matches(&ev),
        "MultiRoute[string] should not match"
    );
}

#[test]
fn test_multi_route_int() {
    let ev = routed_event();
    assert!(
        MultiRoute(vec![10, 42]).matches(&ev),
        "MultiRoute[int] should match"
    );
    assert!(
        !MultiRoute(vec![10, 20]).matches(&ev),
        "MultiRoute[int] should not match"
    );
}

/// Port of Go CustomRouteNode: records its run (when tracked) and emits an event with `routes`.
fn custom_route_node(
    name: &str,
    routes: Vec<String>,
    tracker: Option<Arc<Mutex<Vec<String>>>>,
) -> Arc<dyn Node> {
    let n = name.to_string();
    TestNode::arc(name, NodeConfig::default(), move |ctx, _| {
        if let Some(t) = &tracker {
            t.lock().push(n.clone());
        }
        let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
        ev.routes = routes.clone();
        once(ev)
    })
}

fn sr(s: &str) -> Option<Arc<dyn Route>> {
    Some(Arc::new(StringRoute(s.into())))
}

fn mr(v: &[&str]) -> Option<Arc<dyn Route>> {
    Some(Arc::new(MultiRoute(
        v.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
    )))
}

#[rstest::rstest]
#[case::all_edges_dont_have_routing("all", &["branchA", "branchB"], &["A", "B", "C", "D"])]
#[case::only_one_edge_has_correct_routing("one_correct", &["branchA"], &["A", "B", "C", "D"])]
#[case::one_edge_has_no_routing_rest_correct("rest_correct", &["branchA", "branchB"], &["A", "B", "C", "D"])]
#[case::any_edge_has_incorrect_routing("incorrect", &["invalid"], &[])]
#[case::fallback_to_default_route("default_fallback", &["unmatched"], &["B"])]
#[case::default_route_suppressed_by_concrete_match("default_suppressed", &["branchA"], &["A"])]
#[case::unconditional_edge_does_not_suppress_default("unconditional_default", &["unmatched"], &["A", "B"])]
#[case::correct_multi_route("multi_correct", &["branchA"], &["A", "B", "C", "D"])]
#[case::no_multi_route_matches("multi_none", &["invalid"], &[])]
#[case::multi_route_with_multiple_matching_routes("multi_many", &["branchA", "branchB"], &["A"])]
#[tokio::test]
async fn test_workflow_routing(
    #[case] case: &str,
    #[case] start_routes: &[&str],
    #[case] expected_exec: &[&str],
) {
    let tracker: Arc<Mutex<Vec<String>>> = Arc::default();
    let t = || Some(Arc::clone(&tracker));
    let x = custom_route_node(
        "X",
        start_routes.iter().map(|s| s.to_string()).collect(),
        None,
    );
    let a = custom_route_node("A", Vec::new(), t());
    let b = custom_route_node("B", Vec::new(), t());
    let c = custom_route_node("C", vec!["branchD".into()], t());
    let d = custom_route_node("D", Vec::new(), t());
    let e = |from: &Arc<dyn Node>, to: &Arc<dyn Node>, route: Option<Arc<dyn Route>>| Edge {
        from: Arc::clone(from),
        to: Arc::clone(to),
        route,
    };
    let s = start();
    let edges = match case {
        "all" => vec![
            e(&s, &x, None),
            e(&x, &a, None),
            e(&x, &b, None),
            e(&x, &c, None),
            e(&c, &d, None),
        ],
        "one_correct" => vec![
            e(&s, &x, None),
            e(&x, &a, sr("branchA")),
            e(&x, &b, None),
            e(&x, &c, None),
            e(&c, &d, None),
        ],
        "rest_correct" => vec![
            e(&s, &x, None),
            e(&x, &a, sr("branchA")),
            e(&x, &b, sr("branchB")),
            e(&x, &c, None),
            e(&c, &d, sr("branchD")),
        ],
        "incorrect" => vec![
            e(&s, &x, None),
            e(&x, &a, sr("branchA")),
            e(&x, &b, sr("branchB")),
            e(&x, &c, sr("branchC")),
            e(&c, &d, None),
        ],
        "default_fallback" | "default_suppressed" => vec![
            e(&s, &x, None),
            e(&x, &a, sr("branchA")),
            e(&x, &b, Some(Arc::clone(&DEFAULT))),
        ],
        "unconditional_default" => vec![
            e(&s, &x, None),
            e(&x, &a, None),
            e(&x, &b, Some(Arc::clone(&DEFAULT))),
        ],
        "multi_correct" => vec![
            e(&s, &x, None),
            e(&x, &a, mr(&["branchX", "branchA"])),
            e(&x, &b, None),
            e(&x, &c, None),
            e(&c, &d, None),
        ],
        "multi_none" => vec![
            e(&s, &x, None),
            e(&x, &a, mr(&["branchX", "branchY"])),
            e(&x, &b, mr(&["branchZ"])),
        ],
        _ => vec![e(&s, &x, None), e(&x, &a, mr(&["branchA", "branchB"]))],
    };
    let w = must_new(edges);
    let (_, err) = collect(w.run(mock_ctx())).await;
    assert!(err.is_none(), "unexpected error: {err:?}");
    let mut executed = tracker.lock().clone();
    executed.sort();
    assert_eq!(executed, expected_exec, "executed nodes");
}

/// Port of Go validParams (`state:"Foo"`, `state:"node_input"`).
#[derive(Deserialize, schemars::JsonSchema)]
#[allow(dead_code)]
struct ValidParams {
    #[serde(rename = "Foo")]
    foo: String,
    node_input: String,
}

/// Port of Go invalidParams (`state:"foo"`: a case-mismatch typo).
#[derive(Deserialize, schemars::JsonSchema)]
#[allow(dead_code)]
struct InvalidParams {
    foo: String,
}

/// Port of Go fanOutToJoin (max_concurrency_test.go).
fn fan_out_to_join(nodes: &[Arc<dyn Node>]) -> Vec<Edge> {
    let join: Arc<dyn Node> = JoinNode::new("join");
    nodes
        .iter()
        .flat_map(|n| [edge(&start(), n), edge(n, &join)])
        .collect()
}

#[rstest::rstest]
#[case::valid_schema_matches_exactly("valid", true, None)]
#[case::typo_in_tag_causes_error(
    "invalid",
    true,
    Some(
        r#"node "invalid_node" references state field "foo" which is not declared in state_schema"#
    )
)]
#[case::nil_schema_skips_validation("invalid", false, None)]
#[case::non_state_params_aware_nodes_are_ignored("agent", true, None)]
fn test_workflow_state_schema_consistency(
    #[case] node: &str,
    #[case] with_schema: bool,
    #[case] want_err: Option<&str>,
) {
    let schema = json!({"type": "object", "properties": {"Foo": {"type": "string"}}});
    let valid_node = function_node_from_state::<ValidParams, String, _, _>(
        "valid_node",
        |_c: InvocationContext, _p: ValidParams| async { Ok("ok".to_string()) },
        NodeConfig::default(),
    )
    .expect("NewFunctionNodeFromState");
    let invalid_node = function_node_from_state::<InvalidParams, String, _, _>(
        "invalid_node",
        |_c: InvocationContext, _p: InvalidParams| async { Ok("ok".to_string()) },
        NodeConfig::default(),
    )
    .expect("NewFunctionNodeFromState");
    let nodes = match node {
        "valid" => vec![valid_node],
        "invalid" => vec![invalid_node],
        _ => vec![dummy_node("agentNode")],
    };
    let res = Workflow::new(
        "wf",
        fan_out_to_join(&nodes),
        WorkflowOptions {
            state_schema: with_schema.then_some(schema),
            ..WorkflowOptions::default()
        },
    );
    match want_err {
        Some(want) => {
            let err = res
                .err()
                .unwrap_or_else(|| panic!("expected error containing {want:?}, got nil"));
            assert!(
                err.to_string().contains(want),
                "expected error to contain {want:?}, got: {err}"
            );
        }
        None => assert!(
            res.is_ok(),
            "unexpected error: {:?}",
            res.err().map(|e| e.to_string())
        ),
    }
}

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
struct InitInput {
    user_query: String,
}
#[derive(Serialize, Deserialize, schemars::JsonSchema)]
struct ParsedQuery {
    intent: String,
}
#[derive(Serialize, Deserialize, schemars::JsonSchema)]
#[allow(dead_code)]
struct AgentInput {
    parser: ParsedQuery,
}

// parity: adapted: NewAgentNodeTyped[AgentInput, string] → AgentNode::with_schemas(inferred AgentInput, string).
#[tokio::test]
async fn test_end_to_end_input_validation_flow() {
    let fn_node = function_node_with_schema::<InitInput, ParsedQuery, _, _>(
        "parser",
        |_c: Context, input: InitInput| async move {
            Ok(ParsedQuery {
                intent: format!("{}_intent", input.user_query),
            })
        },
        Some(infer_schema::<InitInput>()),
        None,
        NodeConfig::default(),
    )
    .expect("NewFunctionNodeWithSchema");
    let join_node: Arc<dyn Node> = JoinNode::with_schema("join", infer_schema::<ParsedQuery>())
        .expect("NewJoinNodeWithSchema");
    let received = Arc::new(Mutex::new(String::new()));
    let r = Arc::clone(&received);
    let dummy_agent = gadk_agent::new_agent(AgentConfig {
        name: "e2e_agent".into(),
        run: Some(Arc::new(move |ctx: InvocationContext| {
            if let Some(p) = ctx.user_content().and_then(|uc| uc.parts.first()) {
                *r.lock() = p.text.clone();
            }
            let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
            ev.output = Some(json!("ok"));
            once(ev)
        })),
        ..AgentConfig::default()
    })
    .expect("agent.New");
    let agent_node: Arc<dyn Node> = AgentNode::with_schemas(
        dummy_agent,
        Some(infer_schema::<AgentInput>()),
        Some(json!({"type": "string"})),
        NodeConfig::default(),
    )
    .expect("NewAgentNodeTyped");
    let wf = must_new(vec![
        edge(&start(), &fn_node),
        edge(&fn_node, &join_node),
        edge(&join_node, &agent_node),
    ]);
    let (_, err) = collect(wf.run(ctx_with_parts(vec![Part::from_text(
        r#"{"user_query": "hello"}"#,
    )])))
    .await;
    assert!(
        err.is_none(),
        "expected successful end-to-end run, got error: {err:?}"
    );
    let got = received.lock().clone();
    assert!(
        got.contains("hello_intent"),
        "expected json payload at the end to contain 'hello_intent', got {got:?}"
    );
}

// parity: adapted: Rust Content.parts cannot hold nil; an empty-text part stands in for the nil part (it contributes nothing to the seed).
#[rstest::rstest]
#[case::leading(vec!["", "hi"], "hi")]
#[case::middle(vec!["hi", "", "hi"], "hihi")]
#[case::trailing(vec!["hi", ""], "hi")]
#[case::only(vec![""], "")]
#[tokio::test]
async fn test_workflow_run_skips_nil_user_content_parts(
    #[case] parts: Vec<&str>,
    #[case] want: &str,
) {
    let echo = function_node::<Value, Value, _, _>(
        "echo",
        |_c: Context, input: Value| async move { Ok(input) },
        NodeConfig::default(),
    );
    let w = must_new(chain(&[&start(), &echo]));
    let events = drain(w.run(ctx_with_parts(
        parts.into_iter().map(Part::from_text).collect(),
    )))
    .await;
    assert_eq!(
        events.iter().filter_map(|e| e.output.clone()).last(),
        Some(json!(want)),
        "seed input"
    );
}

// A workflow with no edges builds, and running it ends without an error or any event, matching
// adk-python's Workflow(edges=[]).
// parity: adapted: Go's nil and empty []Edge are both an empty Vec in Rust; one case covers both.
#[tokio::test]
async fn test_workflow_with_no_edges_runs_to_completion() {
    let w = Workflow::new("wf", Vec::new(), WorkflowOptions::default())
        .unwrap_or_else(|e| panic!("New() error = {e}, want nil"));
    let (events, err) = collect(w.run(seeded_mock_ctx())).await;
    assert!(err.is_none(), "Run() error = {err:?}");
    assert!(
        events.is_empty(),
        "Run() yielded {} events, want none: {events:?}",
        events.len()
    );
}
