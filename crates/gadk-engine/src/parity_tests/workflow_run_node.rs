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

//! Port of Go workflow/run_node_test.go.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures::{FutureExt, StreamExt};
use gadk_agent::{AgentError, Context, InvocationContext};
use gadk_platform::Platform;
use gadk_session::{Event, RequestInput};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::error::WorkflowErrorKind;
use crate::workflow::scheduler::child_event_output;
use crate::workflow::scheduler::test_support::*;
use crate::workflow::{
    Emit, Node, NodeConfig, RunNodeOptions, Workflow, WorkflowOptions, dynamic_node, run_node,
};

// --- helpers (ports of the run_node_test.go helpers) ---

/// An orchestrator body: one `run_node` of `child` with `opts`.
async fn run_once(
    ctx: Context,
    child: Arc<dyn Node>,
    opts: RunNodeOptions,
) -> Result<Value, AgentError> {
    run_node(&ctx, child, Value::Null, opts).await
}

/// Runs `run_once(child, opts)` inside an orchestrator and returns its result.
async fn orch_run(child: &Arc<dyn Node>, opts: RunNodeOptions) -> Result<Value, AgentError> {
    let child = Arc::clone(child);
    run_in_orchestrator_with_err(move |ctx| run_once(ctx, Arc::clone(&child), opts.clone())).await
}

/// A dynamic node "orch" whose body is `f`.
fn orch<F, Fut>(f: F) -> Arc<dyn Node>
where
    F: Fn(Context) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Value, AgentError>> + Send + 'static,
{
    dynamic_node::<Value, Value, _, _>(
        "orch",
        move |ctx: Context, _i: Value, _e: Emit| f(ctx),
        NodeConfig::default(),
    )
}

fn last_at_path<'a>(events: &'a [Event], node_path: &str) -> Option<&'a Event> {
    events
        .iter()
        .rev()
        .find(|ev| ev.node_info.as_ref().is_some_and(|i| i.path == node_path))
}

/// Port of Go isolationScopeAtPath.
fn isolation_scope_at_path(events: &[Event], node_path: &str) -> String {
    last_at_path(events, node_path)
        .map(|ev| ev.isolation_scope.clone())
        .unwrap_or_default()
}

/// Port of Go outputBearingPaths.
fn output_bearing_paths(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter(|ev| ev.output.is_some())
        .map(|ev| {
            ev.node_info
                .as_ref()
                .map(|i| i.path.clone())
                .unwrap_or_default()
        })
        .collect()
}

/// Port of Go outputForAtPath.
fn output_for_at_path(events: &[Event], node_path: &str) -> Vec<String> {
    last_at_path(events, node_path)
        .and_then(|ev| ev.node_info.as_ref())
        .map(|i| i.output_for.clone())
        .unwrap_or_default()
}

/// Port of Go derivedOutputAtPath.
fn derived_output_at_path(events: &[Event], node_path: &str) -> Option<Value> {
    last_at_path(events, node_path).and_then(child_event_output)
}

/// Port of Go parentTerminalOutput.
fn parent_terminal_output(events: &[Event], parent_path: &str) -> Option<Value> {
    last_at_path(events, parent_path)
        .unwrap_or_else(|| {
            panic!(
                "no event with NodeInfo.Path == {parent_path:?} found among {} events",
                events.len()
            )
        })
        .output
        .clone()
}

/// Port of Go countingStubNode: emits `out`, counting runs.
fn counting_stub_node(name: &str, out: Value) -> (Arc<dyn Node>, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&calls);
    let node = TestNode::arc(name, NodeConfig::default(), move |_ctx, _| {
        c.fetch_add(1, Ordering::SeqCst);
        once(Event {
            output: Some(out.clone()),
            ..Event::default()
        })
    });
    (node, calls)
}

/// What a gated node does once released.
#[derive(Clone)]
enum GatedOutcome {
    Output(Value),
    Fail(&'static str),
    Interrupt,
    /// Emits the partial output, then panics (Go gatedPanicNode).
    Panic(Value),
}

/// Port of Go gatedRunCountNode / gatedInterruptNode / gatedPanicNode: counts runs and blocks
/// until the gate is released before acting.
fn gated_node(name: &str, outcome: GatedOutcome) -> (Arc<dyn Node>, Arc<AtomicUsize>, Arc<Gate>) {
    let runs = Arc::new(AtomicUsize::new(0));
    let gate = Gate::new();
    let (r, g) = (Arc::clone(&runs), Arc::clone(&gate));
    let node = TestNode::arc(name, NodeConfig::default(), move |ctx, _| {
        let (r, g, outcome) = (Arc::clone(&r), Arc::clone(&g), outcome.clone());
        async_stream::stream! {
            r.fetch_add(1, Ordering::SeqCst);
            g.wait().await;
            match outcome {
                GatedOutcome::Output(v) => yield Ok(output_event(&ctx, v)),
                GatedOutcome::Fail(msg) => yield Err(test_err(msg)),
                GatedOutcome::Interrupt => yield Ok(Event {
                    requested_input: Some(RequestInput {
                        interrupt_id: "iid".into(),
                        message: "ask".into(),
                        ..RequestInput::default()
                    }),
                    ..Event::default()
                }),
                GatedOutcome::Panic(v) => {
                    yield Ok(output_event(&ctx, v));
                    panic!("child exploded");
                }
            }
        }
        .boxed()
    });
    (node, runs, gate)
}

fn dup_opts() -> RunNodeOptions {
    RunNodeOptions {
        run_id: Some("dup".into()),
        ..RunNodeOptions::default()
    }
}

type Results = Arc<Mutex<Vec<Option<Result<Value, String>>>>>;

/// Port of Go runConcurrentDup: two `run_node` calls sharing one run id. Go parks the leader in
/// `child.Run`, then the waiter on the gate (synctest.Wait), then releases. Rust polls a
/// `join!` of leader, waiter and releaser on one task: the first poll of each runs in that
/// order, the leader up to the child's gate and the waiter up to the single-flight wait, so the
/// release happens only once both are parked. Results are indexed leader-first.
async fn run_concurrent_dup(
    child: &Arc<dyn Node>,
    runs: &Arc<AtomicUsize>,
    gate: &Arc<Gate>,
) -> Vec<Result<Value, String>> {
    let results: Results = Arc::new(Mutex::new(vec![None, None]));
    let (child, runs, gate, res) = (
        Arc::clone(child),
        Arc::clone(runs),
        Arc::clone(gate),
        Arc::clone(&results),
    );
    let n = orch(move |ctx| {
        let (child, runs, gate, res) = (
            Arc::clone(&child),
            Arc::clone(&runs),
            Arc::clone(&gate),
            Arc::clone(&res),
        );
        async move {
            let leader = run_node(&ctx, Arc::clone(&child), json!("x"), dup_opts());
            let waiter = run_node(&ctx, Arc::clone(&child), json!("x"), dup_opts());
            let release = async {
                assert_eq!(
                    runs.load(Ordering::SeqCst),
                    1,
                    "leader is not parked inside child.run"
                );
                gate.release();
            };
            let (a, b, ()) = futures::join!(leader, waiter, release);
            let mut r = res.lock();
            r[0] = Some(a.map_err(|e| e.to_string()));
            r[1] = Some(b.map_err(|e| e.to_string()));
            Ok(Value::Null)
        }
    });
    let (_, err) = drain_dynamic_with_err(&n, json!("")).await;
    assert!(err.is_none(), "orchestrator Run error: {err:?}");
    results
        .lock()
        .drain(..)
        .map(|r| r.expect("caller result"))
        .collect()
}

fn use_as_output() -> RunNodeOptions {
    RunNodeOptions {
        use_as_output: true,
        ..RunNodeOptions::default()
    }
}

/// An "orch" dynamic node that delegates its output to `child`, then returns "parent_value".
fn delegating_orch(child: &Arc<dyn Node>) -> Arc<dyn Node> {
    let child = Arc::clone(child);
    orch(move |ctx| {
        let child = Arc::clone(&child);
        async move {
            run_node(&ctx, child, Value::Null, use_as_output()).await?;
            Ok(json!("parent_value"))
        }
    })
}

// --- tests ---

#[tokio::test]
async fn test_run_node_err_invalid_run_node_context_on_static_context() {
    let (child, _) = stub_node("c", json!("x"));
    let err = run_node(
        &top_level_ctx(),
        child,
        Value::Null,
        RunNodeOptions::default(),
    )
    .await
    .expect_err("want error");
    assert!(
        is_kind(&err, WorkflowErrorKind::InvalidRunNodeContext),
        "err = {err}, want ErrInvalidRunNodeContext"
    );
    assert_eq!(
        err.to_string(),
        "workflow: RunNode called outside a dynamic node"
    );
}

#[tokio::test]
async fn test_run_node_dynamic_node_under_scheduler() {
    let (child, _) = stub_node("c", json!("ok"));
    let got: Arc<Mutex<Option<Result<Value, String>>>> = Arc::default();
    let g = Arc::clone(&got);
    let orch = dynamic_node::<String, Value, _, _>(
        "orch",
        move |ctx: Context, _i: String, _e: Emit| {
            let (child, g) = (Arc::clone(&child), Arc::clone(&g));
            async move {
                let r = run_node(&ctx, child, Value::Null, RunNodeOptions::default()).await;
                *g.lock() = Some(r.as_ref().map(Clone::clone).map_err(|e| e.to_string()));
                r
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
    let (_, err) = collect(w.run(mock_ctx())).await;
    assert!(err.is_none(), "workflow.Run error: {err:?}");
    assert_eq!(got.lock().clone(), Some(Ok(json!("ok"))), "RunNode output");
}

#[tokio::test]
async fn test_run_node_returns_typed_output() {
    let (child, _) = stub_node("c", json!("hello"));
    let got = orch_run(&child, RunNodeOptions::default())
        .await
        .expect("orchestrator error");
    assert_eq!(got, json!("hello"));
}

// parity: adapted: Rust run_node returns Value (Go RunNode[OUT=any]); the caller's typed conversion is where a mismatch surfaces, so Rust asserts serde_json::from_value::<String> of the returned 42 fails.
#[tokio::test]
async fn test_run_node_output_type_mismatch() {
    let (child, _) = stub_node("c", json!(42));
    let got = orch_run(&child, RunNodeOptions::default())
        .await
        .expect("orchestrator error");
    assert_eq!(got, json!(42));
    assert!(
        serde_json::from_value::<String>(got).is_err(),
        "expected error for type mismatch"
    );
}

#[tokio::test]
async fn test_run_node_propagates_err_node_interrupted() {
    let asker = request_input_node("asker", "approve?");
    let err = orch_run(&asker, RunNodeOptions::default())
        .await
        .expect_err("want error");
    assert!(
        is_kind(&err, WorkflowErrorKind::NodeInterrupted),
        "err = {err}, want ErrNodeInterrupted"
    );
}

#[tokio::test]
async fn test_run_node_wait_for_output_child_with_no_output_parks_parent() {
    // Port of Go waitForOutputNode: WaitForOutput, a state-only event (no output).
    let cfg = NodeConfig {
        wait_for_output: Some(true),
        ..NodeConfig::default()
    };
    let child = TestNode::arc("waiter", cfg, |_, _| once(Event::default()));
    let err = orch_run(&child, RunNodeOptions::default())
        .await
        .expect_err("want error");
    assert!(
        is_kind(&err, WorkflowErrorKind::NodeInterrupted),
        "err = {err}, want ErrNodeInterrupted"
    );
    assert!(
        is_kind(&err, WorkflowErrorKind::NodeWaitingForOutput),
        "err = {err}, want ErrNodeWaitingForOutput"
    );
}

#[tokio::test]
async fn test_run_node_wait_for_output_child_with_output_completes() {
    // Port of Go waitForOutputWithValueNode.
    let cfg = NodeConfig {
        wait_for_output: Some(true),
        ..NodeConfig::default()
    };
    let child = TestNode::arc("waiter", cfg, |_, _| {
        once(Event {
            output: Some(json!("done")),
            ..Event::default()
        })
    });
    let got = orch_run(&child, RunNodeOptions::default())
        .await
        .expect("orchestrator error");
    assert_eq!(got, json!("done"));
}

#[tokio::test]
async fn test_run_node_no_wait_for_output_child_with_no_output_returns_zero() {
    let (child, _) = stub_node("c", Value::Null);
    let got = orch_run(&child, RunNodeOptions::default())
        .await
        .expect("unexpected error");
    assert_eq!(got, Value::Null, "RunNode output = {got}, want zero value");
}

#[tokio::test]
async fn test_run_node_propagates_err_node_failed() {
    let failer = erroring_node("failer", "boom");
    let err = orch_run(&failer, RunNodeOptions::default())
        .await
        .expect_err("want error");
    assert!(
        is_kind(&err, WorkflowErrorKind::NodeFailed),
        "err = {err}, want ErrNodeFailed"
    );
    assert_eq!(
        err.to_string(),
        "workflow: dynamic child orch/failer@1: workflow: dynamic child failed: boom"
    );
}

#[tokio::test]
async fn test_run_node_with_run_id_appears_in_child_path() {
    let (child, seen) = stub_node("processor", json!(""));
    let opts = RunNodeOptions {
        run_id: Some("order-42".into()),
        ..RunNodeOptions::default()
    };
    orch_run(&child, opts).await.expect("orchestrator error");
    assert_eq!(seen.lock().path, "orch/processor@order-42");
}

#[tokio::test]
async fn test_run_node_with_run_id_invalid_rejected() {
    let (child, _) = stub_node("c", json!(""));
    let opts = RunNodeOptions {
        run_id: Some("123".into()),
        ..RunNodeOptions::default()
    };
    let err = orch_run(&child, opts).await.expect_err("want error");
    assert!(
        is_kind(&err, WorkflowErrorKind::InvalidRunId),
        "err = {err}, want ErrInvalidRunID"
    );
    // Go wraps the plain resolveRunID error: errors.Is(err, ErrNodeFailed) is false.
    assert!(
        !is_kind(&err, WorkflowErrorKind::NodeFailed),
        "err = {err}: an invalid run id is not ErrNodeFailed"
    );
    assert_eq!(
        err.to_string(),
        "workflow: dynamic child c: workflow: invalid run id: \"123\" is purely numeric (reserved for auto-counter)"
    );
}

#[tokio::test]
async fn test_run_node_nil_child_output_returns_zero() {
    let (child, _) = stub_node("c", Value::Null);
    let got = orch_run(&child, RunNodeOptions::default())
        .await
        .expect("orchestrator error");
    assert_eq!(got, Value::Null, "nil child output → {got}, want zero");
}

#[tokio::test]
async fn test_run_node_default_inherits_parent_branch() {
    let (child, seen) = stub_node("c", json!("x"));
    orch_run(&child, RunNodeOptions::default())
        .await
        .expect("orchestrator error");
    assert_eq!(
        seen.lock().branch,
        "",
        "child branch (inherits parent at root)"
    );
}

#[tokio::test]
async fn test_run_node_with_use_sub_branch_appends_segment() {
    let (child, seen) = stub_node("c", json!("x"));
    let opts = RunNodeOptions {
        use_sub_branch: true,
        ..RunNodeOptions::default()
    };
    orch_run(&child, opts).await.expect("orchestrator error");
    assert_eq!(seen.lock().branch, "c@1", "use_sub_branch at root");
}

#[tokio::test]
async fn test_run_node_with_use_sub_branch_plus_custom_run_id() {
    let (child, seen) = stub_node("c", json!("x"));
    let opts = RunNodeOptions {
        use_sub_branch: true,
        run_id: Some("order-42".into()),
        ..RunNodeOptions::default()
    };
    orch_run(&child, opts).await.expect("orchestrator error");
    assert_eq!(seen.lock().branch, "c@order-42");
}

#[tokio::test]
async fn test_run_node_with_override_branch_replaces_base() {
    let (child, seen) = stub_node("c", json!("x"));
    let opts = RunNodeOptions {
        override_branch: Some("custom_branch".into()),
        ..RunNodeOptions::default()
    };
    orch_run(&child, opts).await.expect("orchestrator error");
    assert_eq!(seen.lock().branch, "custom_branch");
}

#[tokio::test]
async fn test_run_node_with_override_branch_plus_use_sub_branch_appends_to_override() {
    let (child, seen) = stub_node("c", json!("x"));
    let opts = RunNodeOptions {
        override_branch: Some("base".into()),
        use_sub_branch: true,
        ..RunNodeOptions::default()
    };
    orch_run(&child, opts).await.expect("orchestrator error");
    assert_eq!(
        seen.lock().branch,
        "base.c@1",
        "override is base, use_sub_branch appends"
    );
}

#[tokio::test]
async fn test_run_node_with_override_branch_empty_treated_as_no_override() {
    let (child, seen) = stub_node("c", json!("x"));
    let opts = RunNodeOptions {
        override_branch: Some(String::new()),
        use_sub_branch: true,
        ..RunNodeOptions::default()
    };
    orch_run(&child, opts).await.expect("orchestrator error");
    assert_eq!(seen.lock().branch, "c@1", "empty override is a no-op");
}

#[tokio::test]
async fn test_run_node_with_use_as_output_child_output_becomes_parent_output() {
    let (child, _) = stub_node("c", json!("child_value"));
    let events = drain_dynamic(&delegating_orch(&child), json!("")).await;
    // Full suppression: the delegated output rides on the child's own event.
    assert_eq!(
        output_bearing_paths(&events),
        vec!["orch/c@1"],
        "paths of events with Output"
    );
    assert_eq!(
        parent_terminal_output(&events, "orch/c@1"),
        Some(json!("child_value")),
        "delegated child Output"
    );
    assert_eq!(
        output_for_at_path(&events, "orch/c@1"),
        vec!["orch/c@1", "orch"],
        "OutputFor"
    );
    // The parent records its completion with the delegated value instead of a second output.
    let marker = last_at_path(&events, "orch").expect("delegated-completion marker");
    assert_eq!(marker.output, None);
    let md = &marker.response.custom_metadata;
    assert_eq!(md.get("adk.workflow.node_completed"), Some(&json!(true)));
    assert_eq!(
        md.get("adk.workflow.delegated_output"),
        Some(&json!("child_value"))
    );
}

#[tokio::test]
async fn test_run_node_with_use_as_output_multi_level_stamps_all_ancestors() {
    let (grandchild, _) = stub_node("gc", json!("deep_value"));
    let mid = dynamic_node::<Value, Value, _, _>(
        "mid",
        move |ctx: Context, _i: Value, _e: Emit| {
            let gc = Arc::clone(&grandchild);
            async move { run_node(&ctx, gc, Value::Null, use_as_output()).await }
        },
        NodeConfig::default(),
    );
    let top = dynamic_node::<Value, Value, _, _>(
        "top",
        move |ctx: Context, _i: Value, _e: Emit| {
            let mid = Arc::clone(&mid);
            async move { run_node(&ctx, mid, Value::Null, use_as_output()).await }
        },
        NodeConfig::default(),
    );
    let events = drain_dynamic(&top, json!("")).await;
    // One output event, on the grandchild, suppressing both delegating ancestors.
    assert_eq!(
        output_bearing_paths(&events),
        vec!["top/mid@1/gc@1"],
        "output-bearing paths"
    );
    assert_eq!(
        output_for_at_path(&events, "top/mid@1/gc@1"),
        vec!["top/mid@1/gc@1", "top/mid@1", "top"],
        "OutputFor"
    );
}

#[tokio::test]
async fn test_run_node_with_use_as_output_message_as_output_child_becomes_parent_output() {
    let child = message_as_output_node("c", "child_text");
    let events = drain_dynamic(&delegating_orch(&child), json!("")).await;
    assert_eq!(
        derived_output_at_path(&events, "orch/c@1"),
        Some(json!("child_text")),
        "delegated child derived output"
    );
}

#[tokio::test]
async fn test_run_node_with_use_as_output_message_as_output_empty_text_is_valid_output() {
    let child = message_as_output_node("c", "");
    let events = drain_dynamic(&delegating_orch(&child), json!("")).await;
    assert_eq!(
        derived_output_at_path(&events, "orch/c@1"),
        Some(json!("")),
        "empty model text is an output, not none"
    );
}

#[tokio::test]
async fn test_run_node_with_use_as_output_second_delegation_returns_error() {
    let (c1, _) = stub_node("c1", json!("v1"));
    let (c2, _) = stub_node("c2", json!("v2"));
    let err = run_in_orchestrator_with_err(move |ctx| {
        let (c1, c2) = (Arc::clone(&c1), Arc::clone(&c2));
        async move {
            run_node(&ctx, c1, Value::Null, use_as_output()).await?;
            run_node(&ctx, c2, Value::Null, use_as_output()).await
        }
    })
    .await
    .expect_err("want error");
    assert!(
        is_kind(&err, WorkflowErrorKind::OutputAlreadyDelegated),
        "err = {err}, want ErrOutputAlreadyDelegated"
    );
    assert!(
        err.to_string().contains("already delegates to"),
        "err = {err}"
    );
    assert_eq!(
        err.to_string(),
        "workflow: dynamic child orch/c2@1: workflow: parent already has a use_as_output child: orch already delegates to c1"
    );
}

#[tokio::test]
async fn test_run_node_with_run_id_idempotent_replay() {
    let (child, calls) = counting_stub_node("c", json!("the_value"));
    let got: Arc<Mutex<Vec<Value>>> = Arc::default();
    let g = Arc::clone(&got);
    run_in_orchestrator(move |ctx| {
        let (child, g) = (Arc::clone(&child), Arc::clone(&g));
        async move {
            let opts = RunNodeOptions {
                run_id: Some("stable-id".into()),
                ..RunNodeOptions::default()
            };
            let first = run_node(&ctx, Arc::clone(&child), Value::Null, opts.clone()).await?;
            g.lock().push(first);
            let second = run_node(&ctx, child, Value::Null, opts).await?;
            g.lock().push(second);
            Ok(Value::Null)
        }
    })
    .await;
    assert_eq!(*got.lock(), vec![json!("the_value"), json!("the_value")]);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "child.Run invocations");
}

// parity: adapted: Go goroutines + synctest.Wait become a deterministic join! of leader, waiter and releaser polled in order on one task (see run_concurrent_dup).
#[tokio::test]
async fn test_run_node_with_run_id_concurrent_single_flight() {
    let (child, runs, gate) = gated_node("dupchild", GatedOutcome::Output(json!("shared_value")));
    let results = run_concurrent_dup(&child, &runs, &gate).await;
    assert_eq!(runs.load(Ordering::SeqCst), 1, "child ran more than once");
    for (i, r) in results.into_iter().enumerate() {
        assert_eq!(r, Ok(json!("shared_value")), "caller {i}");
    }
}

// parity: adapted: Go goroutines + synctest.Wait become a deterministic join! of leader, waiter and releaser polled in order on one task.
#[tokio::test]
async fn test_run_node_with_run_id_concurrent_shares_interrupt() {
    let (child, runs, gate) = gated_node("dupchild", GatedOutcome::Interrupt);
    let errs: Arc<Mutex<Vec<bool>>> = Arc::default();
    let (c, r, g, e) = (
        Arc::clone(&child),
        Arc::clone(&runs),
        Arc::clone(&gate),
        Arc::clone(&errs),
    );
    let n = orch(move |ctx| {
        let (child, runs, gate, errs) = (
            Arc::clone(&c),
            Arc::clone(&r),
            Arc::clone(&g),
            Arc::clone(&e),
        );
        async move {
            let leader = run_node(&ctx, Arc::clone(&child), json!("x"), dup_opts());
            let waiter = run_node(&ctx, Arc::clone(&child), json!("x"), dup_opts());
            let release = async {
                assert_eq!(runs.load(Ordering::SeqCst), 1);
                gate.release();
            };
            let (a, b, ()) = futures::join!(leader, waiter, release);
            for res in [a, b] {
                errs.lock()
                    .push(res.is_err_and(|e| is_kind(&e, WorkflowErrorKind::NodeInterrupted)));
            }
            Ok(Value::Null)
        }
    });
    let (events, _) = drain_dynamic_with_err(&n, json!("")).await;
    assert_eq!(
        runs.load(Ordering::SeqCst),
        1,
        "an interrupt is shared, not re-run"
    );
    let asks = events
        .iter()
        .filter(|ev| ev.requested_input.is_some())
        .count();
    assert_eq!(asks, 1, "RequestedInput events");
    assert_eq!(
        *errs.lock(),
        vec![true, true],
        "both callers get ErrNodeInterrupted"
    );
}

// parity: adapted: Go goroutines + synctest.Wait become a deterministic join! of leader, waiter and releaser polled in order on one task.
#[tokio::test]
async fn test_run_node_with_run_id_concurrent_shares_failure() {
    let (child, runs, gate) = gated_node("dupchild", GatedOutcome::Fail("boom"));
    let results = run_concurrent_dup(&child, &runs, &gate).await;
    assert_eq!(
        runs.load(Ordering::SeqCst),
        1,
        "a failure is shared, not re-run"
    );
    let want = "workflow: dynamic child orch/dupchild@dup: workflow: dynamic child failed: boom";
    for (i, r) in results.into_iter().enumerate() {
        assert_eq!(
            r,
            Err(want.to_string()),
            "caller {i}: the waiter receives the leader's error"
        );
    }
}

#[tokio::test]
async fn test_run_node_with_run_id_sequential_rerun_after_failure() {
    let (child, runs, gate) = gated_node("dupchild", GatedOutcome::Fail("boom"));
    gate.release();
    let n = orch(move |ctx| {
        let child = Arc::clone(&child);
        async move {
            for _ in 0..2 {
                let err = run_node(&ctx, Arc::clone(&child), json!("x"), dup_opts())
                    .await
                    .expect_err("want error");
                assert!(
                    is_kind(&err, WorkflowErrorKind::NodeFailed),
                    "error = {err}, want ErrNodeFailed"
                );
            }
            Ok(Value::Null)
        }
    });
    let (_, err) = drain_dynamic_with_err(&n, json!("")).await;
    assert!(err.is_none(), "orchestrator Run error: {err:?}");
    assert_eq!(
        runs.load(Ordering::SeqCst),
        2,
        "a failure must not be cached"
    );
}

// parity: adapted: Go goroutines + synctest.Wait become one-task join! ordering; the invocation is cancelled through its CancellationToken (Go context.WithCancel) and Go's context.Canceled is AgentError::Cancelled.
#[tokio::test]
async fn test_run_node_with_run_id_waiter_unblocks_on_cancel() {
    let (child, runs, gate) = gated_node("dupchild", GatedOutcome::Output(json!("shared_value")));
    let token = CancellationToken::new();
    let checked = Arc::new(AtomicUsize::new(0));
    let (tok, chk) = (token.clone(), Arc::clone(&checked));
    let n = orch(move |ctx| {
        let (child, runs, gate, token, checked) = (
            Arc::clone(&child),
            Arc::clone(&runs),
            Arc::clone(&gate),
            tok.clone(),
            Arc::clone(&chk),
        );
        async move {
            let waiter_done = Gate::new();
            let waiter_err: Arc<Mutex<Option<AgentError>>> = Arc::default();
            let leader = run_node(&ctx, Arc::clone(&child), json!("x"), dup_opts());
            let waiter = {
                let (child, done, slot) = (
                    Arc::clone(&child),
                    Arc::clone(&waiter_done),
                    Arc::clone(&waiter_err),
                );
                let ctx = ctx.clone();
                async move {
                    let r = run_node(&ctx, child, json!("x"), dup_opts()).await;
                    *slot.lock() = r.err();
                    done.release();
                }
            };
            let control = async {
                assert_eq!(
                    runs.load(Ordering::SeqCst),
                    1,
                    "leader is not parked inside child.run"
                );
                token.cancel();
                // The waiter returns without the leader finishing.
                waiter_done.wait().await;
                let err = waiter_err.lock().take().expect("waiter error");
                assert!(
                    matches!(err, AgentError::Cancelled),
                    "waiter error = {err}, want context canceled"
                );
                assert!(
                    !gate.is_released(),
                    "the leader finished before the waiter returned"
                );
                checked.fetch_add(1, Ordering::SeqCst);
                gate.release();
            };
            let _ = futures::join!(leader, waiter, control);
            Ok(Value::Null)
        }
    });
    let ctx = InvocationContext::builder()
        .invocation_id(TEST_INVOCATION_ID)
        .platform(Platform::deterministic(1))
        .cancellation_token(token)
        .build();
    let mut s = n.run(ctx, json!(""));
    while s.next().await.is_some() {}
    assert_eq!(
        checked.load(Ordering::SeqCst),
        1,
        "the cancellation check did not run"
    );
}

// parity: adapted: Go recover() → AssertUnwindSafe(run_node(..)).catch_unwind().await around each call.
#[tokio::test]
async fn test_run_node_with_run_id_panic_not_cached_as_success() {
    let (child, runs, gate) = gated_node("boom", GatedOutcome::Panic(json!("partial_value")));
    gate.release();
    let second_panicked = Arc::new(AtomicUsize::new(0));
    let sp = Arc::clone(&second_panicked);
    let n = orch(move |ctx| {
        let (child, sp) = (Arc::clone(&child), Arc::clone(&sp));
        async move {
            let first = run_node(&ctx, Arc::clone(&child), json!("x"), dup_opts());
            let _ = AssertUnwindSafe(first).catch_unwind().await;
            let second = AssertUnwindSafe(run_node(&ctx, child, json!("x"), dup_opts()))
                .catch_unwind()
                .await;
            match second {
                Err(_) => {
                    sp.fetch_add(1, Ordering::SeqCst);
                }
                Ok(r) => panic!("second call returned {r:?} instead of re-running the child"),
            }
            Ok(Value::Null)
        }
    });
    let (_, err) = drain_dynamic_with_err(&n, json!("")).await;
    assert!(err.is_none(), "orchestrator Run error: {err:?}");
    assert_eq!(runs.load(Ordering::SeqCst), 2, "a panic must not be cached");
    assert_eq!(
        second_panicked.load(Ordering::SeqCst),
        1,
        "the re-run panics again"
    );
}

// parity: adapted: Go goroutines + recover() → one-task join! with the leader wrapped in AssertUnwindSafe(..).catch_unwind().
#[tokio::test]
async fn test_run_node_with_run_id_concurrent_shares_panic_as_failure() {
    let (child, runs, gate) = gated_node("boom", GatedOutcome::Panic(json!("partial_value")));
    let waiter_res: Arc<Mutex<Option<Result<Value, AgentError>>>> = Arc::default();
    let (c, r, g, w) = (
        Arc::clone(&child),
        Arc::clone(&runs),
        Arc::clone(&gate),
        Arc::clone(&waiter_res),
    );
    let n = orch(move |ctx| {
        let (child, runs, gate, slot) = (
            Arc::clone(&c),
            Arc::clone(&r),
            Arc::clone(&g),
            Arc::clone(&w),
        );
        async move {
            let leader =
                AssertUnwindSafe(run_node(&ctx, Arc::clone(&child), json!("x"), dup_opts()))
                    .catch_unwind();
            let waiter = run_node(&ctx, Arc::clone(&child), json!("x"), dup_opts());
            let release = async {
                assert_eq!(runs.load(Ordering::SeqCst), 1);
                gate.release();
            };
            let (leader, waiter, ()) = futures::join!(leader, waiter, release);
            assert!(leader.is_err(), "the leader's panic propagates");
            *slot.lock() = Some(waiter);
            Ok(Value::Null)
        }
    });
    let (_, err) = drain_dynamic_with_err(&n, json!("")).await;
    assert!(err.is_none(), "orchestrator Run error: {err:?}");
    assert_eq!(runs.load(Ordering::SeqCst), 1, "child ran more than once");
    let waiter = waiter_res.lock().take().expect("waiter result");
    let err = waiter.expect_err("waiter got success instead of an ErrNodeFailed outcome");
    assert!(
        is_kind(&err, WorkflowErrorKind::NodeFailed),
        "waiter error = {err}"
    );
    assert_eq!(
        err.to_string(),
        "workflow: dynamic child orch/boom@dup: workflow: dynamic child failed: child did not complete"
    );
}

#[tokio::test]
async fn test_run_node_with_run_id_and_use_as_output_idempotent_replay() {
    let (child, calls) = counting_stub_node("c", json!("delegated_value"));
    let n = orch(move |ctx| {
        let child = Arc::clone(&child);
        async move {
            let opts = RunNodeOptions {
                run_id: Some("stable-id".into()),
                use_as_output: true,
                ..RunNodeOptions::default()
            };
            run_node(&ctx, Arc::clone(&child), Value::Null, opts.clone()).await?;
            run_node(&ctx, child, Value::Null, opts).await?;
            Ok(json!("parent_value"))
        }
    });
    let events = drain_dynamic(&n, json!("")).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1, "child.Run invocations");
    assert_eq!(
        derived_output_at_path(&events, "orch/c@stable-id"),
        Some(json!("delegated_value")),
        "delegated child output"
    );
    // The cached replay commits the delegation: the parent's marker carries the value.
    let marker = last_at_path(&events, "orch").expect("delegated-completion marker");
    assert_eq!(
        marker
            .response
            .custom_metadata
            .get("adk.workflow.delegated_output"),
        Some(&json!("delegated_value"))
    );
}

#[tokio::test]
async fn test_run_node_sequential_fan_out_per_sibling_distinct_branches() {
    let (c1, seen) = stub_node("c", json!("first"));
    let got: Arc<Mutex<Vec<String>>> = Arc::default();
    let (g, s) = (Arc::clone(&got), Arc::clone(&seen));
    run_in_orchestrator(move |ctx| {
        let (c1, g, s) = (Arc::clone(&c1), Arc::clone(&g), Arc::clone(&s));
        async move {
            let opts = RunNodeOptions {
                use_sub_branch: true,
                ..RunNodeOptions::default()
            };
            for _ in 0..2 {
                run_node(&ctx, Arc::clone(&c1), Value::Null, opts.clone()).await?;
                let branch = s.lock().branch.clone();
                g.lock().push(branch);
            }
            Ok(Value::Null)
        }
    })
    .await;
    assert_eq!(
        *got.lock(),
        vec!["c@1", "c@2"],
        "per-name auto-counter must produce distinct sub-branches"
    );
}

#[tokio::test]
async fn test_run_node_isolation_scope_from_node_path() {
    let (child, seen) = stub_node("c", json!("v"));
    let opts = RunNodeOptions {
        isolation_scope_from_node_path: true,
        ..RunNodeOptions::default()
    };
    let n = orch(move |ctx| run_once(ctx, Arc::clone(&child), opts.clone()));
    let events = drain_dynamic(&n, json!("")).await;
    assert_eq!(seen.lock().scope, "orch/c@1", "child ctx isolation scope");
    assert_eq!(
        isolation_scope_at_path(&events, "orch/c@1"),
        "orch/c@1",
        "emitted event isolation scope"
    );
    let child_events: Vec<&Event> = events
        .iter()
        .filter(|ev| ev.node_info.as_ref().is_some_and(|i| i.path == "orch/c@1"))
        .collect();
    assert!(!child_events.is_empty(), "no forwarded child event");
    for ev in child_events {
        assert_eq!(
            ev.isolation_scope, "orch/c@1",
            "every forwarded child event carries the scope"
        );
    }
}

#[tokio::test]
async fn test_run_node_isolation_scope_explicit() {
    let (child, seen) = stub_node("c", json!("v"));
    let opts = RunNodeOptions {
        isolation_scope: Some("fc-123".into()),
        ..RunNodeOptions::default()
    };
    let n = orch(move |ctx| run_once(ctx, Arc::clone(&child), opts.clone()));
    let events = drain_dynamic(&n, json!("")).await;
    assert_eq!(seen.lock().scope, "fc-123", "child ctx isolation scope");
    assert_eq!(
        isolation_scope_at_path(&events, "orch/c@1"),
        "fc-123",
        "emitted event isolation scope"
    );
}

#[tokio::test]
async fn test_run_node_explicit_scope_takes_precedence_over_node_path() {
    let (child, seen) = stub_node("c", json!("v"));
    let opts = RunNodeOptions {
        isolation_scope: Some("fc-123".into()),
        isolation_scope_from_node_path: true,
        ..RunNodeOptions::default()
    };
    orch_run(&child, opts).await.expect("orchestrator error");
    assert_eq!(seen.lock().scope, "fc-123", "explicit must win");
}

#[tokio::test]
async fn test_run_node_no_isolation_scope_inherits_parent() {
    let (child, seen) = stub_node("c", json!("v"));
    orch_run(&child, RunNodeOptions::default())
        .await
        .expect("orchestrator error");
    assert_eq!(seen.lock().scope, "", "inherited");
}

#[tokio::test]
async fn test_run_node_isolation_scope_distinct_per_node_path() {
    let (child, seen) = stub_node("c", json!("v"));
    let scopes: Arc<Mutex<Vec<String>>> = Arc::default();
    let (sc, s) = (Arc::clone(&scopes), Arc::clone(&seen));
    run_in_orchestrator(move |ctx| {
        let (child, sc, s) = (Arc::clone(&child), Arc::clone(&sc), Arc::clone(&s));
        async move {
            let opts = RunNodeOptions {
                isolation_scope_from_node_path: true,
                ..RunNodeOptions::default()
            };
            for _ in 0..2 {
                run_node(&ctx, Arc::clone(&child), Value::Null, opts.clone()).await?;
                let scope = s.lock().scope.clone();
                sc.lock().push(scope);
            }
            Ok(Value::Null)
        }
    })
    .await;
    assert_eq!(*scopes.lock(), vec!["orch/c@1", "orch/c@2"]);
}

// parity: adapted: Go goroutines + recover() become futures::future::join_all of 8 run_node calls on one task; a panic would fail the test directly.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_run_node_concurrent_children_no_race() {
    const N: usize = 8;
    let n = orch(|ctx| async move {
        let calls = (0..N).map(|i| {
            let (child, _) = stub_node("child", json!("out"));
            let opts = RunNodeOptions {
                use_sub_branch: true,
                run_id: Some(format!("c{i}")),
                ..RunNodeOptions::default()
            };
            let ctx = ctx.clone();
            async move { run_node(&ctx, child, Value::Null, opts).await }
        });
        for r in futures::future::join_all(calls).await {
            r?;
        }
        Ok(json!("done"))
    });
    let (events, err) = drain_dynamic_with_err(&n, json!("")).await;
    assert!(err.is_none(), "orchestrator error: {err:?}");
    // N child outputs forwarded up + the parent's own terminal output.
    let outputs = events.iter().filter(|ev| ev.output.is_some()).count();
    assert_eq!(outputs, N + 1, "output-bearing events");
}

#[tokio::test]
async fn test_run_node_dynamic_child_recovers_own_context() {
    let captured: Arc<Mutex<Option<Context>>> = Arc::default();
    let c = Arc::clone(&captured);
    // Port of Go fnNode: captures its context, emits one Output event.
    let inner = TestNode::arc("inner", NodeConfig::default(), move |ctx, _| {
        *c.lock() = Some(ctx);
        once(Event {
            output: Some(json!("ok")),
            ..Event::default()
        })
    });
    let n = orch(move |ctx| run_once(ctx, Arc::clone(&inner), RunNodeOptions::default()));
    let (_, err) = drain_dynamic_with_err(&n, json!("")).await;
    assert!(err.is_none(), "orchestrator error: {err:?}");
    let ctx = captured
        .lock()
        .clone()
        .expect("inner did not recover any agent.Context");
    assert_eq!(ctx.path(), "orch/inner@1");
    assert!(
        ctx.sub_scheduler().is_some(),
        "the child's context reaches the sub-scheduler"
    );
}
