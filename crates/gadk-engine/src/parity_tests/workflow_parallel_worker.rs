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

//! Port of Go workflow/parallel_worker_test.go.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::Duration;

use futures::StreamExt;
use gadk_agent::{AgentError, Context, InvocationContext};
use gadk_genai::{Content, Part};
use gadk_platform::Platform;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::workflow::scheduler::test_support::*;
use crate::workflow::scheduler::{is_cancelled, is_deadline_exceeded};
use crate::workflow::{
    Node, NodeConfig, ParallelWorker, RetryConfig, default_retry_config, function_node,
};

/// Port of Go upperNode.
fn upper_node() -> Arc<dyn Node> {
    function_node::<String, String, _, _>(
        "upper",
        |_c: Context, input: String| async move { Ok(input.to_uppercase()) },
        NodeConfig::default(),
    )
}

/// Port of Go newMockCtx with `userContent = {Parts: [{Text: text}]}`.
fn ctx_with_user_text(text: &str) -> InvocationContext {
    InvocationContext::builder()
        .invocation_id(TEST_INVOCATION_ID)
        .platform(Platform::deterministic(1))
        .user_content(Content {
            parts: vec![Part::from_text(text)],
            ..Content::default()
        })
        .build_invocation()
}

/// Port of Go `&MockInvocationContext{Context: ctx}` with a cancellable `ctx`.
fn cancellable_ctx(token: &CancellationToken) -> Context {
    InvocationContext::builder()
        .invocation_id(TEST_INVOCATION_ID)
        .platform(Platform::deterministic(1))
        .cancellation_token(token.clone())
        .build()
}

/// Go `rc := DefaultRetryConfig(); rc.MaxAttempts = n; rc.InitialDelay = 0; rc.MaxDelay = 0; rc.Jitter = 0`.
fn instant_retry(max_attempts: i32) -> RetryConfig {
    RetryConfig {
        max_attempts,
        initial_delay: Duration::ZERO,
        max_delay: Duration::ZERO,
        jitter: 0.0,
        ..default_retry_config()
    }
}

fn retry_cfg(rc: RetryConfig) -> NodeConfig {
    NodeConfig {
        retry_config: Some(rc),
        ..NodeConfig::default()
    }
}

/// Port of Go newErrYieldNode.
fn err_yield_node(name: &str, cancelled: bool) -> Arc<dyn Node> {
    TestNode::arc(name, NodeConfig::default(), move |_, _| {
        fail(if cancelled {
            AgentError::Cancelled
        } else {
            test_err("boom")
        })
    })
}

#[rstest::rstest]
#[case::success(json!(["a", "b", "c"]), false, Some(json!(["A", "B", "C"])), None)]
#[case::empty_list(json!([]), false, Some(json!([])), None)]
#[case::invalid_input_not_slice(json!("not a slice"), false, None, Some("expects an array input"))]
#[case::worker_error(json!(["a", "b", "c"]), true, None, Some("failed on b"))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_parallel_worker_run(
    #[case] input: Value,
    #[case] failing: bool,
    #[case] want_output: Option<Value>,
    #[case] err_substr: Option<&str>,
) {
    let wrapped = if failing {
        function_node::<String, String, _, _>(
            "error_node",
            |_c: Context, input: String| async move {
                if input == "b" {
                    Err(AgentError::other("failed on b"))
                } else {
                    Ok(input)
                }
            },
            NodeConfig::default(),
        )
    } else {
        upper_node()
    };
    let pw: Arc<dyn Node> =
        ParallelWorker::new("parallel", wrapped, 0, NodeConfig::default()).expect("new");
    let (events, err) = collect(pw.run(top_level_ctx(), input)).await;
    match err_substr {
        Some(sub) => {
            let err = err.expect("expected error, got nil");
            assert!(
                err.to_string().contains(sub),
                "expected error containing {sub:?}, got {err}"
            );
        }
        None => {
            assert!(err.is_none(), "unexpected error: {err:?}");
            assert_eq!(
                events.last().and_then(|e| e.output.clone()),
                want_output,
                "output mismatch"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_parallel_worker_concurrency() {
    let counter = Arc::new(AtomicI32::new(0));
    let block = Gate::new();
    let (started_tx, mut started_rx) = tokio::sync::mpsc::channel::<()>(4);
    let (c, b) = (Arc::clone(&counter), Arc::clone(&block));
    let wrapped = function_node::<i64, i64, _, _>(
        "blocking",
        move |_ctx: Context, input: i64| {
            let (c, b, tx) = (Arc::clone(&c), Arc::clone(&b), started_tx.clone());
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                let _ = tx.send(()).await;
                b.wait().await;
                Ok(input)
            }
        },
        NodeConfig::default(),
    );
    let pw: Arc<dyn Node> =
        ParallelWorker::new("parallel", wrapped, 2, NodeConfig::default()).expect("new");
    let run = tokio::spawn(async move {
        let mut s = pw.run(top_level_ctx(), json!([1, 2, 3, 4]));
        while s.next().await.is_some() {}
    });
    // At most 2 workers start because max_concurrency is 2.
    started_rx.recv().await;
    started_rx.recv().await;
    assert_eq!(
        counter.load(Ordering::SeqCst),
        2,
        "expected counter to be 2"
    );
    assert!(
        started_rx.try_recv().is_err(),
        "expected only 2 workers to start, but more did"
    );
    block.release();
    tokio::time::timeout(Duration::from_secs(2), run)
        .await
        .expect("timeout waiting for execution to complete")
        .expect("run task");
    assert_eq!(
        counter.load(Ordering::SeqCst),
        4,
        "expected final counter to be 4"
    );
}

#[tokio::test]
async fn test_parallel_worker_suppress_intermediate_events() {
    let wrapped = function_node::<Value, Value, _, _>(
        "wrapped",
        |_c: Context, input: Value| async move { Ok(input) },
        NodeConfig::default(),
    );
    let pw: Arc<dyn Node> =
        ParallelWorker::new("parallel", wrapped, 0, NodeConfig::default()).expect("new");
    let events = drain(pw.run(top_level_ctx(), json!([1, 2]))).await;
    let mut non_output = 0;
    let mut has_aggregated_output = false;
    for ev in &events {
        if let Some(out) = ev.output.as_ref().filter(|v| !v.is_null()) {
            has_aggregated_output = true;
            assert_eq!(out, &json!([1, 2]), "output mismatch");
        } else if ev
            .response
            .content
            .as_ref()
            .and_then(|c| c.parts.first())
            .is_some_and(|p| p.text == "progress")
        {
            non_output += 1;
        }
    }
    assert_eq!(non_output, 0, "expected 0 progress events");
    assert!(
        has_aggregated_output,
        "expected final aggregated output event"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_parallel_worker_workflow_integration() {
    let split_node = function_node::<String, Vec<Value>, _, _>(
        "split",
        |_c: Context, input: String| async move {
            Ok(input.split(',').map(|p| Value::String(p.into())).collect())
        },
        NodeConfig::default(),
    );
    let pw: Arc<dyn Node> =
        ParallelWorker::new("parallel", upper_node(), 0, NodeConfig::default()).expect("new");
    let join_node = function_node::<Vec<Value>, String, _, _>(
        "join",
        |_c: Context, input: Vec<Value>| async move {
            Ok(input
                .iter()
                .map(|v| v.as_str().unwrap_or_default())
                .collect::<Vec<_>>()
                .join(":"))
        },
        NodeConfig::default(),
    );
    let w = must_new(vec![
        edge(&start(), &split_node),
        edge(&split_node, &pw),
        edge(&pw, &join_node),
    ]);
    let events = drain(w.run(ctx_with_user_text("a,b,c"))).await;
    let last_output = events
        .iter()
        .filter_map(|e| e.output.clone().filter(|v| !v.is_null()))
        .last();
    assert_eq!(last_output, Some(json!("A:B:C")), "output");
}

#[test]
fn test_new_parallel_worker_error_on_wrapped_retry_config() {
    let wrapped = function_node::<Value, Value, _, _>(
        "wrapped",
        |_c: Context, input: Value| async move { Ok(input) },
        retry_cfg(default_retry_config()),
    );
    let err = ParallelWorker::new("parallel", wrapped, 0, NodeConfig::default())
        .err()
        .expect("expected error when wrapped node has RetryConfig, got nil");
    assert!(
        err.to_string().contains("cannot have RetryConfig"),
        "expected error containing 'cannot have RetryConfig', got {err}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_parallel_worker_retry() {
    let attempts: Arc<Mutex<HashMap<String, i32>>> = Arc::default();
    let a = Arc::clone(&attempts);
    let wrapped = function_node::<String, String, _, _>(
        "retry_node",
        move |_c: Context, input: String| {
            let count = {
                let mut g = a.lock();
                let c = g.entry(input.clone()).or_default();
                *c += 1;
                *c
            };
            async move {
                if input == "b" && count <= 2 {
                    return Err(test_err("temporary failure"));
                }
                Ok(input)
            }
        },
        NodeConfig::default(),
    );
    let rc = RetryConfig {
        should_retry: Some(Arc::new(|_: &AgentError| true)),
        ..instant_retry(3)
    };
    let pw: Arc<dyn Node> =
        ParallelWorker::new("parallel", wrapped, 0, retry_cfg(rc)).expect("new");
    let (events, err) = collect(pw.run(top_level_ctx(), json!(["a", "b"]))).await;
    assert!(err.is_none(), "unexpected error: {err:?}");
    assert_eq!(
        events.iter().filter_map(|e| e.output.clone()).last(),
        Some(json!(["a", "b"])),
        "output mismatch"
    );
    let attempts = attempts.lock();
    assert_eq!(attempts.get("a"), Some(&1), "attempts for 'a'");
    assert_eq!(attempts.get("b"), Some(&3), "attempts for 'b'");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_parallel_worker_fail_fast() {
    let worker_c_cancelled = Arc::new(AtomicBool::new(false));
    let cancelled_gate = Gate::new();
    let (wc, cg) = (Arc::clone(&worker_c_cancelled), Arc::clone(&cancelled_gate));
    let wrapped = function_node::<String, String, _, _>(
        "fail_fast_node",
        move |ctx: Context, input: String| {
            let (wc, cg) = (Arc::clone(&wc), Arc::clone(&cg));
            async move {
                match input.as_str() {
                    "b" => Err(test_err("error b")),
                    "c" => {
                        // Block until cancelled.
                        ctx.cancellation_token().cancelled().await;
                        wc.store(true, Ordering::SeqCst);
                        cg.release();
                        Err(AgentError::Cancelled)
                    }
                    _ => Ok(input),
                }
            }
        },
        NodeConfig::default(),
    );
    let pw: Arc<dyn Node> =
        ParallelWorker::new("parallel", wrapped, 0, NodeConfig::default()).expect("new");
    let (_, err) = collect(pw.run(top_level_ctx(), json!(["a", "b", "c"]))).await;
    let err = err.expect("expected error, got nil");
    assert_eq!(err.to_string(), "error b", "expected error 'error b'");
    tokio::time::timeout(Duration::from_secs(2), cancelled_gate.wait())
        .await
        .expect("timeout waiting for worker C to be canceled");
    assert!(
        worker_c_cancelled.load(Ordering::SeqCst),
        "expected worker c to be canceled"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_parallel_worker_cancel_during_execution() {
    let block = Gate::new();
    let (started_tx, mut started_rx) = tokio::sync::mpsc::channel::<()>(2);
    let b = Arc::clone(&block);
    let wrapped = function_node::<Value, Value, _, _>(
        "blocking",
        move |ctx: Context, input: Value| {
            let (b, tx) = (Arc::clone(&b), started_tx.clone());
            async move {
                let _ = tx.send(()).await;
                tokio::select! {
                    () = ctx.cancellation_token().cancelled() => Err(AgentError::Cancelled),
                    () = b.wait() => Ok(input),
                }
            }
        },
        NodeConfig::default(),
    );
    let pw: Arc<dyn Node> =
        ParallelWorker::new("parallel", wrapped, 0, NodeConfig::default()).expect("new");
    let token = CancellationToken::new();
    let ctx = cancellable_ctx(&token);
    let run = tokio::spawn(async move { collect(pw.run(ctx, json!([1, 2]))).await });
    // Wait for the workers to start before cancelling.
    started_rx.recv().await;
    started_rx.recv().await;
    token.cancel();
    let (events, err) = tokio::time::timeout(Duration::from_secs(2), run)
        .await
        .expect("timeout waiting for cancellation to complete")
        .expect("run task");
    let err = err.expect("expected error on cancellation, got nil");
    assert!(is_cancelled(&err), "expected context.Canceled, got {err}");
    assert!(
        !events.iter().any(|e| e.output.is_some()),
        "expected no final result to be yielded"
    );
    block.release();
}

// parity: adapted: Go sleeps 300ms so the worker is parked in its 2s retry delay; the attempt signals its failure instead (deterministic), and the cancellation lands in the delay.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_parallel_worker_cancel_during_retry_delay() {
    let failed = Gate::new();
    let f = Arc::clone(&failed);
    let wrapped = function_node::<Value, Value, _, _>(
        "always_fails",
        move |_c: Context, _i: Value| {
            let f = Arc::clone(&f);
            async move {
                f.release();
                Err::<Value, _>(test_err("boom"))
            }
        },
        NodeConfig::default(),
    );
    let rc = RetryConfig {
        max_attempts: 5,
        initial_delay: Duration::from_secs(2),
        max_delay: Duration::from_secs(2),
        jitter: 0.0,
        should_retry: Some(Arc::new(|_: &AgentError| true)),
        ..default_retry_config()
    };
    let pw: Arc<dyn Node> =
        ParallelWorker::new("parallel", wrapped, 0, retry_cfg(rc)).expect("new");
    let token = CancellationToken::new();
    let ctx = cancellable_ctx(&token);
    let run = tokio::spawn(async move { collect(pw.run(ctx, json!([1]))).await });
    failed.wait().await;
    token.cancel();
    let (events, err) = tokio::time::timeout(Duration::from_secs(2), run)
        .await
        .expect("timeout waiting for cancellation during retry delay to complete")
        .expect("run task");
    let err = err.expect("expected error on cancellation during retry delay, got nil");
    assert!(is_cancelled(&err), "expected context.Canceled, got {err}");
    assert!(
        !events.iter().any(|e| e.output.is_some()),
        "expected no final result to be yielded when cancelled mid-retry"
    );
}

/// Port of Go ignoresCancellation: waits for its context to end and then succeeds, like a wrapped
/// node that does not check ctx.Err().
fn ignores_cancellation(started: Option<tokio::sync::mpsc::Sender<()>>) -> Arc<dyn Node> {
    function_node::<Value, Value, _, _>(
        "ignores_cancellation",
        move |ctx: Context, input: Value| {
            let started = started.clone();
            async move {
                if let Some(tx) = started {
                    let _ = tx.send(()).await;
                }
                ctx.cancellation_token().cancelled().await;
                Ok(input)
            }
        },
        NodeConfig::default(),
    )
}

/// Runs `pw` over `input` on a spawned task (Go's draining goroutine): the last error and the last
/// non-nil output.
fn spawn_run(
    pw: Arc<dyn Node>,
    ctx: Context,
    input: Value,
) -> tokio::task::JoinHandle<(Option<AgentError>, Option<Value>)> {
    tokio::spawn(async move {
        let mut s = pw.run(ctx, input);
        let (mut got_err, mut got_output) = (None, None);
        while let Some(item) = s.next().await {
            match item {
                Err(e) => got_err = Some(e),
                Ok(ev) => {
                    if let Some(out) = ev.output.filter(|v| !v.is_null()) {
                        got_output = Some(out);
                    }
                }
            }
        }
        (got_err, got_output)
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_parallel_worker_cancel_during_dispatch() {
    // With maxConcurrency 1, items 2 and 3 are still waiting for a slot when the caller cancels.
    // The dispatch loop skips them without reporting an error, and item 1 succeeds, so no worker
    // reports a failure.
    let (started_tx, mut started_rx) = tokio::sync::mpsc::channel::<()>(1);
    let wrapped = ignores_cancellation(Some(started_tx));
    let pw: Arc<dyn Node> =
        ParallelWorker::new("parallel", wrapped, 1, NodeConfig::default()).expect("new");
    let token = CancellationToken::new();
    let run = spawn_run(pw, cancellable_ctx(&token), json!([1, 2, 3]));
    started_rx.recv().await;
    token.cancel();
    let (got_err, got_output) = tokio::time::timeout(Duration::from_secs(2), run)
        .await
        .expect("timeout waiting for cancellation during dispatch to complete")
        .expect("run task");
    assert!(
        got_err.as_ref().is_some_and(is_cancelled),
        "Run() error = {got_err:?}, want context.Canceled"
    );
    assert_eq!(got_output, None, "Run() yielded output, want none");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_parallel_worker_cancel_after_dispatch() {
    // Every item is running when the caller cancels, and each one then returns no output and no
    // error. No item is skipped at dispatch and no worker reports a failure.
    let (started_tx, mut started_rx) = tokio::sync::mpsc::channel::<()>(3);
    let wrapped = function_node::<Value, Value, _, _>(
        "no_output_on_cancel",
        move |ctx: Context, _input: Value| {
            let tx = started_tx.clone();
            async move {
                let _ = tx.send(()).await;
                ctx.cancellation_token().cancelled().await;
                Ok(Value::Null)
            }
        },
        NodeConfig::default(),
    );
    let pw: Arc<dyn Node> =
        ParallelWorker::new("parallel", wrapped, 0, NodeConfig::default()).expect("new");
    let token = CancellationToken::new();
    let run = spawn_run(pw, cancellable_ctx(&token), json!([1, 2, 3]));
    for _ in 0..3 {
        started_rx.recv().await;
    }
    token.cancel();
    let (got_err, got_output) = tokio::time::timeout(Duration::from_secs(2), run)
        .await
        .expect("timeout waiting for cancellation after dispatch to complete")
        .expect("run task");
    assert!(
        got_err.as_ref().is_some_and(is_cancelled),
        "Run() error = {got_err:?}, want context.Canceled"
    );
    assert_eq!(got_output, None, "Run() yielded output, want none");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_parallel_worker_failure_before_cancel_is_returned() {
    // Item 1 fails and cancels item 2. The caller cancels only after that, while item 2 is still
    // running, so the failure has to win over ctx.Err(). Item 1 fails only once item 2 has
    // started, because the dispatch loop skips any item it has not reached by then.
    let (item2_started, saw_fail_fast, release) = (Gate::new(), Gate::new(), Gate::new());
    let (i2, sf, rel) = (
        Arc::clone(&item2_started),
        Arc::clone(&saw_fail_fast),
        Arc::clone(&release),
    );
    let wrapped = function_node::<i64, i64, _, _>(
        "fails_then_cancels",
        move |ctx: Context, input: i64| {
            let (i2, sf, rel) = (Arc::clone(&i2), Arc::clone(&sf), Arc::clone(&rel));
            async move {
                if input == 1 {
                    i2.wait().await;
                    return Err(test_err("boom"));
                }
                i2.release();
                ctx.cancellation_token().cancelled().await;
                sf.release();
                rel.wait().await;
                Ok(input)
            }
        },
        NodeConfig::default(),
    );
    let pw: Arc<dyn Node> =
        ParallelWorker::new("parallel", wrapped, 0, NodeConfig::default()).expect("new");
    let token = CancellationToken::new();
    let run = spawn_run(pw, cancellable_ctx(&token), json!([1, 2]));
    tokio::time::timeout(Duration::from_secs(2), saw_fail_fast.wait())
        .await
        .expect("timeout waiting for item 1's failure to cancel item 2");
    token.cancel();
    release.release();
    let (got_err, _) = tokio::time::timeout(Duration::from_secs(2), run)
        .await
        .expect("timeout waiting for Run to return")
        .expect("run task");
    assert!(
        got_err.as_ref().is_some_and(|e| e.to_string() == "boom"),
        "Run() error = {got_err:?}, want boom"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_parallel_worker_timeout_during_dispatch_in_workflow() {
    // The node timeout cancels the ParallelWorker while the workflow's own context stays live, so
    // the scheduler is not draining and would pass a partial result on to the caller. Whether it
    // does depends on a select in runNode, so the scenario runs several times.
    for _ in 0..20 {
        let items_node = function_node::<Value, Value, _, _>(
            "items",
            |_c: Context, _i: Value| async move { Ok(json!([1, 2, 3])) },
            NodeConfig::default(),
        );
        let pw: Arc<dyn Node> = ParallelWorker::new(
            "parallel",
            ignores_cancellation(None),
            1,
            NodeConfig {
                timeout: Some(Duration::from_millis(10)),
                ..NodeConfig::default()
            },
        )
        .expect("new");
        let w = must_new(chain(&[&start(), &items_node, &pw]));
        let mut s = w.run(mock_ctx());
        let mut got_err = None;
        while let Some(item) = s.next().await {
            match item {
                Err(e) => got_err = Some(e),
                Ok(ev) => {
                    if let Some(Value::Array(items)) = &ev.output {
                        assert!(
                            !items.iter().any(Value::is_null),
                            "workflow yielded partial ParallelWorker output {items:?} after the node timed out"
                        );
                    }
                }
            }
        }
        assert!(
            got_err.as_ref().is_some_and(is_deadline_exceeded),
            "workflow error = {got_err:?}, want context.DeadlineExceeded"
        );
    }
}

/// Port of Go delayedMultiOutputTestNode: signals start, waits for its item's release, then
/// emits two outputs (`s`, `s_2`).
fn delayed_multi_output_node(
    releases: HashMap<String, Arc<Gate>>,
    started: tokio::sync::mpsc::Sender<()>,
) -> Arc<dyn Node> {
    let releases = Arc::new(releases);
    TestNode::arc(
        "delayed_multi_output",
        NodeConfig::default(),
        move |ctx, input| {
            let (releases, started) = (Arc::clone(&releases), started.clone());
            async_stream::stream! {
                let s = input.as_str().unwrap_or_default().to_string();
                let _ = started.send(()).await;
                if let Some(g) = releases.get(&s) {
                    g.wait().await;
                }
                yield Ok(output_event(&ctx, json!(s)));
                yield Ok(output_event(&ctx, json!(format!("{s}_2"))));
            }
            .boxed()
        },
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_parallel_worker_concurrent_multi_output_order() {
    let gates: Vec<(String, Arc<Gate>)> = ["a", "b", "c"]
        .iter()
        .map(|s| (s.to_string(), Gate::new()))
        .collect();
    let (started_tx, mut started_rx) = tokio::sync::mpsc::channel::<()>(3);
    let wrapped = delayed_multi_output_node(gates.iter().cloned().collect(), started_tx);
    let pw: Arc<dyn Node> =
        ParallelWorker::new("parallel", wrapped, 0, NodeConfig::default()).expect("new");
    let run =
        tokio::spawn(async move { drain(pw.run(top_level_ctx(), json!(["a", "b", "c"]))).await });
    for _ in 0..3 {
        started_rx.recv().await;
    }
    // Release in reverse order to simulate out-of-order completion.
    for (_, g) in gates.iter().rev() {
        g.release();
    }
    let events = run.await.expect("run task");
    assert_eq!(
        events.iter().filter_map(|e| e.output.clone()).last(),
        Some(json!([["a", "a_2"], ["b", "b_2"], ["c", "c_2"]])),
        "output mismatch"
    );
}

#[tokio::test]
async fn test_parallel_worker_scheduler_does_not_retry_on_failure() {
    let wrapped_attempts = Arc::new(AtomicI32::new(0));
    let a = Arc::clone(&wrapped_attempts);
    let wrapped = function_node::<String, String, _, _>(
        "worker",
        move |_c: Context, _i: String| {
            a.fetch_add(1, Ordering::SeqCst);
            async { Err::<String, _>(test_err("persistent failure")) }
        },
        NodeConfig::default(),
    );
    let pw: Arc<dyn Node> =
        ParallelWorker::new("parallel", wrapped, 0, retry_cfg(instant_retry(2))).expect("new");
    let split_node = function_node::<String, Vec<Value>, _, _>(
        "split",
        |_c: Context, input: String| async move { Ok(vec![Value::String(input)]) },
        NodeConfig::default(),
    );
    let w = must_new(vec![edge(&start(), &split_node), edge(&split_node, &pw)]);
    let (_, err) = collect(w.run(ctx_with_user_text("a"))).await;
    assert!(err.is_some(), "expected error, got nil");
    assert_eq!(
        wrapped_attempts.load(Ordering::SeqCst),
        2,
        "expected 2 attempts for wrapped node (scheduler likely retried the node)"
    );
}

// parity: adapted: the OTel in-memory exporter becomes the SpanCapture tracing layer (current-thread runtime); codes.Error/Unset are "ERROR"/"".
#[rstest::rstest]
#[case::one_span_per_item_on_success("success", json!(["a", "b", "c"]), "invoke_node upper", 3, "")]
#[case::span_marked_error_on_failure("failure", json!(["x"]), "invoke_node w", 1, "ERROR")]
#[case::control_flow_error_not_marked("cancelled", json!(["x"]), "invoke_node c", 1, "")]
#[case::wrapped_emitting_own_span_is_not_double_wrapped("own_span", json!(["a", "b"]), "", 0, "")]
#[tokio::test]
async fn test_parallel_worker_per_item_spans(
    #[case] case: &str,
    #[case] input: Value,
    #[case] want_span_name: &str,
    #[case] want_count: usize,
    #[case] want_status: &str,
) {
    let (cap, _guard) = SpanCapture::install();
    let wrapped = match case {
        "success" => upper_node(),
        "failure" => err_yield_node("w", false),
        "cancelled" => err_yield_node("c", true),
        _ => function_node::<String, String, _, _>(
            "agentish",
            |_c: Context, input: String| async move { Ok(input) },
            NodeConfig {
                emits_own_span: true,
                ..NodeConfig::default()
            },
        ),
    };
    let pw: Arc<dyn Node> =
        ParallelWorker::new("parallel", wrapped, 0, NodeConfig::default()).expect("new");
    // Drain; span outcomes asserted below.
    let _ = collect(pw.run(top_level_ctx(), input)).await;
    let spans = cap.node_spans();
    assert_eq!(spans.len(), want_count, "got {} spans", spans.len());
    for s in spans {
        assert_eq!(s.otel_name, want_span_name, "span name");
        assert_eq!(s.status, want_status, "span {:?} status", s.otel_name);
    }
}

// parity: adapted: the OTel in-memory exporter becomes the SpanCapture tracing layer (current-thread runtime).
#[tokio::test]
async fn test_parallel_worker_retry_emits_span_per_attempt() {
    let (cap, _guard) = SpanCapture::install();
    let attempts = Arc::new(AtomicI32::new(0));
    let a = Arc::clone(&attempts);
    let wrapped = function_node::<String, String, _, _>(
        "flaky",
        move |_c: Context, input: String| {
            let first = a.fetch_add(1, Ordering::SeqCst) == 0;
            async move {
                if first {
                    Err(test_err("boom"))
                } else {
                    Ok(input)
                }
            }
        },
        NodeConfig::default(),
    );
    let pw: Arc<dyn Node> =
        ParallelWorker::new("parallel", wrapped, 0, retry_cfg(instant_retry(2))).expect("new");
    let _ = collect(pw.run(top_level_ctx(), json!(["x"]))).await;
    let spans = cap.node_spans();
    assert_eq!(
        spans.len(),
        2,
        "got {} spans, want 2 (one per attempt)",
        spans.len()
    );
    let (mut err_count, mut unset_count) = (0, 0);
    for s in &spans {
        assert_eq!(s.otel_name, "invoke_node flaky", "span name");
        match s.status.as_str() {
            "ERROR" => err_count += 1,
            "" => unset_count += 1,
            _ => {}
        }
    }
    assert!(
        err_count == 1 && unset_count == 1,
        "status multiset = {{Error:{err_count}, Unset:{unset_count}}}, want {{Error:1, Unset:1}}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_parallel_worker_max_concurrency_fail_fast() {
    const N_ITEMS: i32 = 5;
    let started = Arc::new(AtomicI32::new(0));
    let s = Arc::clone(&started);
    let wrapped = function_node::<i64, i64, _, _>(
        "slow_or_fail",
        move |_c: Context, input: i64| {
            let s = Arc::clone(&s);
            async move {
                if input == 0 {
                    return Err(test_err("error 0"));
                }
                s.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(50)).await;
                Ok(input)
            }
        },
        NodeConfig::default(),
    );
    // max_concurrency=1 makes every item after the first wait on the dispatch loop's semaphore.
    let pw: Arc<dyn Node> =
        ParallelWorker::new("parallel", wrapped, 1, NodeConfig::default()).expect("new");
    let (_, err) = tokio::time::timeout(
        Duration::from_secs(2),
        collect(pw.run(top_level_ctx(), json!([0, 1, 2, 3, 4]))),
    )
    .await
    .expect("timeout waiting for execution to complete");
    let err = err.expect("expected error, got nil");
    assert_eq!(err.to_string(), "error 0", "expected error 'error 0'");
    let got = started.load(Ordering::SeqCst);
    assert!(
        got < N_ITEMS - 1,
        "started = {got}, want well under {} (fail-fast should stop dispatch of items still waiting on the semaphore)",
        N_ITEMS - 1
    );
}
