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

//! Port of Go workflow/join_node_test.go.

use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

use gadk_agent::Context;
use parking_lot::Mutex;
use serde_json::{Value, json};

use crate::workflow::scheduler::test_support::*;
use crate::workflow::{JoinNode, Node, NodeConfig, function_node};

/// Port of Go constNode.
fn const_node(name: &str, value: &'static str) -> Arc<dyn Node> {
    function_node::<Value, String, _, _>(
        name,
        move |_c: Context, _i: Value| async move { Ok(value.to_string()) },
        NodeConfig::default(),
    )
}

/// Port of Go inputRecorder.
fn input_recorder(name: &str, seen: &Arc<Mutex<Option<Value>>>) -> Arc<dyn Node> {
    let s = Arc::clone(seen);
    function_node::<Value, String, _, _>(
        name,
        move |_c: Context, input: Value| {
            let s = Arc::clone(&s);
            async move {
                *s.lock() = Some(input);
                Ok("done".to_string())
            }
        },
        NodeConfig::default(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_join_node_e2_e_fan_in_two_branches() {
    let (a, b) = (
        const_node("branchA", "A-result"),
        const_node("branchB", "B-result"),
    );
    let join: Arc<dyn Node> = JoinNode::new("join");
    let seen: Arc<Mutex<Option<Value>>> = Arc::default();
    let handler = input_recorder("handler", &seen);
    let w = must_new(vec![
        edge(&start(), &a),
        edge(&start(), &b),
        edge(&a, &join),
        edge(&b, &join),
        edge(&join, &handler),
    ]);
    drain(w.run(seeded_mock_ctx())).await;
    assert_eq!(
        seen.lock().clone(),
        Some(json!({"branchA": "A-result", "branchB": "B-result"}))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_join_node_e2_e_fan_in_three_branches() {
    let (a, b, c) = (
        const_node("a", "va"),
        const_node("b", "vb"),
        const_node("c", "vc"),
    );
    let join: Arc<dyn Node> = JoinNode::new("join");
    let seen: Arc<Mutex<Option<Value>>> = Arc::default();
    let handler = input_recorder("handler", &seen);
    let w = must_new(vec![
        edge(&start(), &a),
        edge(&start(), &b),
        edge(&start(), &c),
        edge(&a, &join),
        edge(&b, &join),
        edge(&c, &join),
        edge(&join, &handler),
    ]);
    drain(w.run(seeded_mock_ctx())).await;
    let seen = seen.lock().clone().unwrap_or_default();
    let mut keys: Vec<&String> = seen
        .as_object()
        .map(|m| m.keys().collect())
        .unwrap_or_default();
    keys.sort();
    assert_eq!(keys, vec!["a", "b", "c"], "handler input keys");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_join_node_barrier_skips_until_all_predecessors_complete() {
    let b_blocker = Gate::new();
    let release = Arc::clone(&b_blocker);
    // A releases B from its body; the barrier must hold until B also completes.
    let branch_a = function_node::<Value, String, _, _>(
        "branchA",
        move |_c: Context, _i: Value| {
            release.release();
            async { Ok("a".to_string()) }
        },
        NodeConfig::default(),
    );
    let wait = Arc::clone(&b_blocker);
    let branch_b = function_node::<Value, String, _, _>(
        "branchB",
        move |_c: Context, _i: Value| {
            let wait = Arc::clone(&wait);
            async move {
                wait.wait().await;
                Ok("b".to_string())
            }
        },
        NodeConfig::default(),
    );
    let join: Arc<dyn Node> = JoinNode::new("join");
    let calls = Arc::new(AtomicI32::new(0));
    let seen: Arc<Mutex<Option<Value>>> = Arc::default();
    let (c, s) = (Arc::clone(&calls), Arc::clone(&seen));
    let handler = function_node::<Value, String, _, _>(
        "handler",
        move |_c: Context, input: Value| {
            c.fetch_add(1, Ordering::SeqCst);
            *s.lock() = Some(input);
            async { Ok("ok".to_string()) }
        },
        NodeConfig::default(),
    );
    let w = must_new(vec![
        edge(&start(), &branch_a),
        edge(&start(), &branch_b),
        edge(&branch_a, &join),
        edge(&branch_b, &join),
        edge(&join, &handler),
    ]);
    drain(w.run(seeded_mock_ctx())).await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "handler run count, want exactly 1"
    );
    assert_eq!(
        seen.lock().clone(),
        Some(json!({"branchA": "a", "branchB": "b"}))
    );
}

#[tokio::test]
async fn test_join_node_run_is_passthrough() {
    let n: Arc<dyn Node> = JoinNode::new("join");
    let input = json!({"a": "x", "b": 42});
    let events = drain(n.run(seeded_mock_ctx().into_context(), input.clone())).await;
    assert_eq!(events.len(), 1, "expected 1 event");
    assert_eq!(events[0].output, Some(input), "Output (verbatim)");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_join_node_predecessor_with_nil_output() {
    let branch_a = const_node("branchA", "A-result");
    let branch_nil = function_node::<Value, Value, _, _>(
        "branchNil",
        |_c: Context, _i: Value| async { Ok(Value::Null) },
        NodeConfig::default(),
    );
    let join: Arc<dyn Node> = JoinNode::new("join");
    let seen: Arc<Mutex<Option<Value>>> = Arc::default();
    let handler = input_recorder("handler", &seen);
    let w = must_new(vec![
        edge(&start(), &branch_a),
        edge(&start(), &branch_nil),
        edge(&branch_a, &join),
        edge(&branch_nil, &join),
        edge(&join, &handler),
    ]);
    drain(w.run(seeded_mock_ctx())).await;
    assert_eq!(
        seen.lock().clone(),
        Some(json!({"branchA": "A-result", "branchNil": null}))
    );
}

#[rstest::rstest]
#[case::all_match_schema(true, json!({"pred1": "val1", "pred2": "val2", "pred3": "val3"}), Ok(json!({"pred1": "val1", "pred2": "val2", "pred3": "val3"})))]
#[case::one_fails_validation(true, json!({"pred1": "val1", "pred2": 42, "pred3": "val3"}), Err("predecessor \"pred2\":"))]
#[case::not_a_map_falls_back(true, json!("raw string value"), Ok(json!("raw string value")))]
#[case::nil_value_in_map(true, json!({"pred1": null}), Ok(json!({"pred1": null})))]
#[case::empty_map(true, json!({}), Ok(json!({})))]
#[case::nil_schema_returns_as_is(false, json!({"pred1": 42}), Ok(json!({"pred1": 42})))]
fn test_join_node_validate_input(
    #[case] with_schema: bool,
    #[case] input: Value,
    #[case] want: Result<Value, &str>,
) {
    let node: Arc<dyn Node> = if with_schema {
        JoinNode::with_schema("join", json!({"type": "string"})).expect("schema")
    } else {
        JoinNode::new("join")
    };
    let got = node.validate_input(input);
    match want {
        Ok(v) => assert_eq!(got.expect("unexpected error"), v, "ValidateInput()"),
        Err(sub) => {
            let err = got.expect_err("expected an error");
            assert!(
                err.to_string().contains(sub),
                "expected error containing {sub:?}, got {err}"
            );
        }
    }
}
