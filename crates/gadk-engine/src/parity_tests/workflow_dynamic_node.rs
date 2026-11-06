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

//! Port of Go workflow/dynamic_node_test.go.

use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

use futures::StreamExt;
use gadk_agent::{AgentError, Context, InvocationContext};
use gadk_platform::Platform;
use gadk_session::{Event, EventActions};
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use crate::error::{WorkflowError, WorkflowErrorKind};
use crate::workflow::nodes::dynamic::make_emit;
use crate::workflow::scheduler::test_support::*;
use crate::workflow::{
    Emit, NodeConfig, RunNodeOptions, Workflow, WorkflowOptions, dynamic_node,
    dynamic_node_with_schema, function_node, run_node,
};

#[test]
fn test_new_dynamic_node_defaults_rerun_on_resume() {
    let n = dynamic_node::<String, String, _, _>(
        "d",
        |_c: Context, _i: String, _e: Emit| async { Ok(String::new()) },
        NodeConfig::default(),
    );
    assert_eq!(
        n.config().rerun_on_resume,
        Some(true),
        "RerunOnResume default"
    );
}

#[test]
fn test_new_dynamic_node_respects_explicit_false() {
    let n = dynamic_node::<String, String, _, _>(
        "d",
        |_c: Context, _i: String, _e: Emit| async { Ok(String::new()) },
        NodeConfig {
            rerun_on_resume: Some(false),
            ..NodeConfig::default()
        },
    );
    assert_eq!(n.config().rerun_on_resume, Some(false), "explicit override");
}

#[tokio::test]
async fn test_dynamic_node_sequential_run_node_chain() {
    let (step_a, _) = stub_node("stepA", json!("from A"));
    let (step_b, _) = stub_node("stepB", json!("from B"));
    let orchestrator = dynamic_node::<String, String, _, _>(
        "orch",
        move |ctx: Context, _i: String, _e: Emit| {
            let (a, b) = (Arc::clone(&step_a), Arc::clone(&step_b));
            async move {
                let out_a = run_node(&ctx, a, json!("ignored"), RunNodeOptions::default()).await?;
                let out_b = run_node(&ctx, b, out_a.clone(), RunNodeOptions::default()).await?;
                Ok(format!(
                    "{} | {}",
                    out_a.as_str().unwrap_or_default(),
                    out_b.as_str().unwrap_or_default()
                ))
            }
        },
        NodeConfig::default(),
    );
    let events = drain_dynamic(&orchestrator, json!("input")).await;
    assert_eq!(
        events.last().and_then(|e| e.output.clone()),
        Some(json!("from A | from B")),
        "terminal Output"
    );
}

#[derive(Deserialize, schemars::JsonSchema, Default, Clone, Debug, PartialEq)]
struct Req {
    #[serde(rename = "Name")]
    name: String,
}

// parity: adapted: Go passes a Req struct value; the Rust input is its JSON form `{"Name": ..}`.
#[tokio::test]
async fn test_dynamic_node_typed_input() {
    let observed = Arc::new(Mutex::new(Req::default()));
    let o = Arc::clone(&observed);
    let orchestrator = dynamic_node::<Req, String, _, _>(
        "orch",
        move |_c: Context, input: Req, _e: Emit| {
            *o.lock() = input;
            async { Ok("ok".to_string()) }
        },
        NodeConfig::default(),
    );
    drain_dynamic(&orchestrator, json!({"Name": "alice"})).await;
    assert_eq!(observed.lock().name, "alice", "observed Req.Name");
}

#[derive(Deserialize, schemars::JsonSchema, Default, Clone, Debug, PartialEq)]
struct JsonReq {
    name: String,
    n: i64,
}

#[tokio::test]
async fn test_dynamic_node_typed_input_json_fallback() {
    let observed = Arc::new(Mutex::new(JsonReq::default()));
    let o = Arc::clone(&observed);
    let orchestrator = dynamic_node::<JsonReq, String, _, _>(
        "orch",
        move |_c: Context, input: JsonReq, _e: Emit| {
            *o.lock() = input;
            async { Ok("ok".to_string()) }
        },
        NodeConfig::default(),
    );
    drain_dynamic(&orchestrator, json!({"name": "bob", "n": 7})).await;
    assert_eq!(
        *observed.lock(),
        JsonReq {
            name: "bob".into(),
            n: 7
        },
        "observed"
    );
}

#[tokio::test]
async fn test_dynamic_node_emit_mid_body() {
    let orchestrator = dynamic_node::<String, String, _, _>(
        "orch",
        |_c: Context, _i: String, emit: Emit| async move {
            let mut delta = Map::new();
            delta.insert("progress".into(), json!("halfway"));
            emit(Event {
                actions: EventActions {
                    state_delta: Some(delta),
                    ..EventActions::default()
                },
                ..Event::default()
            })
            .await?;
            Ok("done".to_string())
        },
        NodeConfig::default(),
    );
    let events = drain_dynamic(&orchestrator, json!("")).await;
    assert!(
        events.len() >= 2,
        "got {} events, want >= 2 (mid-body emit + terminal)",
        events.len()
    );
    assert_eq!(
        events[0]
            .actions
            .state_delta
            .as_ref()
            .and_then(|d| d.get("progress")),
        Some(&json!("halfway")),
        "first event StateDelta progress"
    );
    assert_eq!(
        events.last().and_then(|e| e.output.clone()),
        Some(json!("done")),
        "terminal Output"
    );
}

// parity: adapted: Go "yield returned false" is dropping the stream after the first event; the body future is dropped with it, so nothing after the pending emit runs (counted) and nothing panics.
#[rstest::rstest]
#[case::returns_emit_error("returns emit error")]
#[case::ignores_emit_error("ignores emit error")]
#[case::returns_independent_error("returns independent error")]
#[case::returns_wait_for_output("returns wait for output")]
#[tokio::test]
async fn test_dynamic_node_stops_after_consumer_exits(#[case] case: &'static str) {
    let after_emit = Arc::new(AtomicI32::new(0));
    let a = Arc::clone(&after_emit);
    let orchestrator = dynamic_node::<String, String, _, _>(
        "orch",
        move |_c: Context, _i: String, emit: Emit| {
            let a = Arc::clone(&a);
            async move {
                let res = emit(Event::default()).await;
                a.fetch_add(1, Ordering::SeqCst);
                match case {
                    "returns emit error" => {
                        res?;
                        Ok("done".to_string())
                    }
                    "ignores emit error" => Ok("done".to_string()),
                    "returns independent error" => Err(AgentError::other("body failed")),
                    _ => {
                        Err(WorkflowError::sentinel(WorkflowErrorKind::NodeWaitingForOutput).into())
                    }
                }
            }
        },
        NodeConfig::default(),
    );
    let mut count = 0;
    {
        let mut s = orchestrator.run(top_level_ctx(), json!(""));
        if let Some(item) = s.next().await {
            item.unwrap_or_else(|e| panic!("unexpected error: {e}"));
            count += 1;
        }
    }
    assert_eq!(count, 1, "consumed {count} events, want 1");
    tokio::task::yield_now().await;
    assert_eq!(
        after_emit.load(Ordering::SeqCst),
        0,
        "body kept running after consumer exit"
    );
}

// parity: adapted: Go's yield callback is the receiver; "yield returns false" is receiving the first event and dropping the receiver (and the event's ack) without resuming.
#[tokio::test]
async fn test_make_emit_stops_calling_yield_after_consumer_exits() {
    let (emit, mut rx) = make_emit(&top_level_ctx());
    let mut yield_calls = 0;
    let (first, ()) = tokio::join!(emit(Event::default()), async {
        let msg = rx.recv().await;
        yield_calls += 1;
        drop(msg);
        drop(rx);
    });
    assert!(
        matches!(first, Err(AgentError::Cancelled)),
        "first emit error = {first:?}, want context.Canceled"
    );
    let second = emit(Event::default()).await;
    assert!(
        matches!(second, Err(AgentError::Cancelled)),
        "second emit error = {second:?}, want context.Canceled"
    );
    assert_eq!(yield_calls, 1, "yield calls");
}

// parity: adapted: the second emit is polled until it is parked behind the first (deterministic, no goroutine race); dropping the receiver releases both with Cancelled.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_make_emit_stops_concurrent_waiter_after_consumer_exits() {
    let (emit, mut rx) = make_emit(&top_level_ctx());
    let mut first = emit(Event::default());
    assert!(
        futures::poll!(&mut first).is_pending(),
        "first emit should wait for the consumer"
    );
    let first_msg = rx.recv().await.expect("first yield entered");
    let yield_calls = 1;
    let mut second = emit(Event::default());
    assert!(
        futures::poll!(&mut second).is_pending(),
        "second emit should wait behind the first"
    );
    // Release the first yield, which returns false: the consumer is gone.
    drop(first_msg);
    drop(rx);
    for (i, res) in [first.await, second.await].into_iter().enumerate() {
        assert!(
            matches!(res, Err(AgentError::Cancelled)),
            "emit #{i} error = {res:?}, want context.Canceled"
        );
    }
    assert_eq!(yield_calls, 1, "yield calls");
}

// parity: adapted: a cancelled CancellationToken carries no DeadlineExceeded cause, so the preserved context error is AgentError::Cancelled; the "yield" cancels the token, then the consumer exits.
#[tokio::test]
async fn test_make_emit_preserves_context_error_after_consumer_exits() {
    let token = CancellationToken::new();
    let parent = InvocationContext::builder()
        .invocation_id(TEST_INVOCATION_ID)
        .platform(Platform::deterministic(1))
        .cancellation_token(token.clone())
        .build();
    let (emit, mut rx) = make_emit(&parent);
    let mut yield_calls = 0;
    let (first, ()) = tokio::join!(emit(Event::default()), async {
        let msg = rx.recv().await;
        yield_calls += 1;
        token.cancel();
        drop(msg);
        drop(rx);
    });
    assert!(
        matches!(first, Err(AgentError::Cancelled)),
        "first emit error = {first:?}, want the context error"
    );
    let second = emit(Event::default()).await;
    assert!(
        matches!(second, Err(AgentError::Cancelled)),
        "second emit error = {second:?}, want the context error"
    );
    assert_eq!(yield_calls, 1, "yield calls");
}

#[tokio::test]
async fn test_dynamic_node_hitl_swallows_interrupt() {
    let asker = request_input_node("asker", "approve?");
    let orchestrator = dynamic_node::<String, String, _, _>(
        "orch",
        move |ctx: Context, _i: String, _e: Emit| {
            let asker = Arc::clone(&asker);
            async move {
                run_node(&ctx, asker, Value::Null, RunNodeOptions::default()).await?;
                Ok(String::new())
            }
        },
        NodeConfig::default(),
    );
    let (events, run_err) = drain_dynamic_with_err(&orchestrator, json!("")).await;
    assert!(
        run_err.is_none(),
        "Run yielded error {run_err:?}; want nil (HITL swallowed)"
    );
    assert!(
        events.iter().any(|e| e.requested_input.is_some()),
        "expected RequestedInput event in stream, got {events:?}"
    );
}

#[tokio::test]
async fn test_dynamic_node_child_failure_propagates_error() {
    let failer = erroring_node("failer", "boom");
    let orchestrator = dynamic_node::<String, String, _, _>(
        "orch",
        move |ctx: Context, _i: String, _e: Emit| {
            let failer = Arc::clone(&failer);
            async move {
                run_node(&ctx, failer, Value::Null, RunNodeOptions::default()).await?;
                Ok(String::new())
            }
        },
        NodeConfig::default(),
    );
    let (_, run_err) = drain_dynamic_with_err(&orchestrator, json!("")).await;
    let err = run_err.expect("want an error");
    assert!(
        is_kind(&err, WorkflowErrorKind::NodeFailed),
        "Run error = {err}, want errors.Is ErrNodeFailed"
    );
}

#[tokio::test]
async fn test_dynamic_node_terminal_output_event() {
    let orchestrator = dynamic_node::<String, i64, _, _>(
        "orch",
        |_c: Context, _i: String, _e: Emit| async { Ok(42) },
        NodeConfig::default(),
    );
    let events = drain_dynamic(&orchestrator, json!("")).await;
    assert_eq!(
        events.last().and_then(|e| e.output.clone()),
        Some(json!(42)),
        "Output"
    );
}

#[tokio::test]
async fn test_dynamic_node_integration_child_and_parent_outputs() {
    let hello_node = function_node::<String, String, _, _>(
        "hello_node",
        |_c: Context, _i: String| async { Ok("Hello World".to_string()) },
        NodeConfig::default(),
    );
    let orch = dynamic_node::<String, String, _, _>(
        "my_workflow",
        move |ctx: Context, _i: String, _e: Emit| {
            let hello = Arc::clone(&hello_node);
            async move {
                let v = run_node(&ctx, hello, json!("hello"), RunNodeOptions::default()).await?;
                Ok(v.as_str().unwrap_or_default().to_string())
            }
        },
        NodeConfig::default(),
    );
    let w = Workflow::new(
        "root",
        chain(&[&start(), &orch]),
        WorkflowOptions::default(),
    )
    .expect("workflow.New");
    let events = drain(w.run(mock_ctx())).await;
    let outputs: Vec<Value> = events.iter().filter_map(|e| e.output.clone()).collect();
    assert_eq!(
        outputs.len(),
        2,
        "got {} output events, want 2 (child + parent terminal); outputs={outputs:?}",
        outputs.len()
    );
    for out in outputs {
        assert_eq!(out, json!("Hello World"), "output");
    }
}

#[test]
fn test_new_dynamic_node_with_schema_nil_schemas_ok() {
    let res = dynamic_node_with_schema::<String, String, _, _>(
        "d",
        |_c: Context, _i: String, _e: Emit| async { Ok(String::new()) },
        None,
        None,
        NodeConfig::default(),
    );
    assert!(
        res.is_ok(),
        "nil schemas should construct cleanly, got {:?}",
        res.err()
    );
}
