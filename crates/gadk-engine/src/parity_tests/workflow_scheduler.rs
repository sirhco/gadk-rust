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

//! Port of Go workflow/scheduler_test.go.

use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::{Duration, Instant};

use futures::StreamExt;
use gadk_agent::{AgentError, Context, EventStream, InvocationContext};
use gadk_genai::{Content, FunctionResponse, Part};
use gadk_platform::Platform;
use gadk_session::Event;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::error::{WorkflowError, WorkflowErrorKind};
use crate::tool::utils::infer_schema;
use crate::workflow::scheduler::test_support::*;
use crate::workflow::scheduler::{Scheduler, is_cancelled, is_deadline_exceeded};
use crate::workflow::state::{NodeState, NodeStatus};
use crate::workflow::{
    BaseNode, JoinNode, Node, NodeConfig, RetryConfig, START, StringRoute, function_node,
};

// ---------- file-local fixtures (Go scheduler_test.go helpers) ----------

/// Go newSeededMockCtx with `context.WithCancel` (or a timeout) on its context.
fn seeded_ctx_with_token(token: &CancellationToken) -> InvocationContext {
    InvocationContext::builder()
        .invocation_id(TEST_INVOCATION_ID)
        .platform(Platform::deterministic(1))
        .user_content(Content {
            parts: vec![Part::from_text("seed")],
            ..Content::default()
        })
        .cancellation_token(token.clone())
        .build_invocation()
}

/// Go newMockCtx with `userContent` set to `text`.
fn mock_ctx_with_text(text: &str) -> InvocationContext {
    InvocationContext::builder()
        .invocation_id(TEST_INVOCATION_ID)
        .platform(Platform::deterministic(1))
        .user_content(Content {
            parts: vec![Part::from_text(text)],
            ..Content::default()
        })
        .build_invocation()
}

/// Drives a workflow stream to its end on a spawned task; resolves to its first error.
fn spawn_first_err(s: EventStream) -> tokio::task::JoinHandle<Option<AgentError>> {
    tokio::spawn(async move { collect(s).await.1 })
}

/// A retry config that always retries (Go `ShouldRetry: func(error) bool { return true }`).
fn always_retry(max_attempts: i32, initial_delay: Duration) -> RetryConfig {
    RetryConfig {
        max_attempts,
        initial_delay,
        backoff_factor: 1.0,
        jitter: 0.0,
        should_retry: Some(Arc::new(|_| true)),
        ..RetryConfig::default()
    }
}

/// Port of Go retryTestNode: fails `fail_count` times, then emits "<input>:<name>".
fn retry_test_node(
    name: &str,
    fail_count: i32,
    cfg: NodeConfig,
) -> (Arc<dyn Node>, Arc<AtomicI32>) {
    let calls = Arc::new(AtomicI32::new(0));
    let c = Arc::clone(&calls);
    let n = name.to_string();
    let node = TestNode::arc(name, cfg, move |ctx, input| {
        let calls = c.fetch_add(1, Ordering::SeqCst) + 1;
        if calls <= fail_count {
            return fail(AgentError::other(format!("fail attempt {calls}")));
        }
        let shown = match &input {
            Value::String(s) => s.clone(),
            v => v.to_string(),
        };
        once(output_event(&ctx, Value::String(format!("{shown}:{n}"))))
    });
    (node, calls)
}

/// Port of Go waitForCalls.
async fn wait_for_calls(calls: &AtomicI32, want: i32) {
    let deadline = Instant::now() + Duration::from_secs(1);
    while calls.load(Ordering::SeqCst) < want {
        assert!(
            Instant::now() < deadline,
            "timeout waiting for {want} node attempts, got {}",
            calls.load(Ordering::SeqCst)
        );
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

/// Port of Go releasableErroringNode: signals start, waits for release, then fails.
fn releasable_erroring_node(
    name: &str,
    msg: &'static str,
) -> (Arc<dyn Node>, Arc<Gate>, Arc<Gate>) {
    let (started, release) = (Gate::new(), Gate::new());
    let (s, r) = (Arc::clone(&started), Arc::clone(&release));
    let node = TestNode::arc(name, NodeConfig::default(), move |_, _| {
        let (s, r) = (Arc::clone(&s), Arc::clone(&r));
        async_stream::stream! {
            s.release();
            r.wait().await;
            yield Err(test_err(msg));
        }
        .boxed()
    });
    (node, started, release)
}

/// Port of Go multiOutputNode.
fn multi_output_node(name: &str, outputs: &'static [&'static str]) -> Arc<dyn Node> {
    TestNode::arc(name, NodeConfig::default(), move |ctx, _| {
        let items: Vec<Result<Event, AgentError>> = outputs
            .iter()
            .map(|o| Ok(output_event(&ctx, json!(o))))
            .collect();
        futures::stream::iter(items).boxed()
    })
}

/// Port of Go progressThenOutputNode (and progressThenSchemaOutputNode, via `base`).
fn progress_then_output_node(
    name: &str,
    progress: usize,
    output: Value,
    output_schema: Option<Value>,
) -> Arc<dyn Node> {
    TestNode::with_schemas(
        name,
        None,
        output_schema,
        NodeConfig::default(),
        move |ctx, _| {
            let mut items: Vec<Result<Event, AgentError>> = (0..progress)
                .map(|_| Ok(Event::new(ctx.platform(), ctx.invocation_id())))
                .collect();
            items.push(Ok(output_event(&ctx, output.clone())));
            futures::stream::iter(items).boxed()
        },
    )
}

/// Port of Go schemaValidatedNode.
fn schema_validated_node(name: &str, schema: Value, output: Value) -> Arc<dyn Node> {
    TestNode::with_schemas(
        name,
        None,
        Some(schema),
        NodeConfig::default(),
        move |ctx, _| once(output_event(&ctx, output.clone())),
    )
}

/// Go base_node_test.go testSchemaInput.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[allow(dead_code)]
struct TestSchemaInput {
    value: String,
}

/// Go resolveTestSchema[testSchemaInput].
fn test_schema() -> Value {
    infer_schema::<TestSchemaInput>()
}

/// Go outputFnNode.
fn output_fn_node(name: &str, out: &'static str) -> Arc<dyn Node> {
    function_node::<Value, String, _, _>(
        name,
        move |_c: Context, _i: Value| async move { Ok(out.to_string()) },
        NodeConfig::default(),
    )
}

/// Go noOutputNode.
fn no_output_node(name: &str) -> Arc<dyn Node> {
    TestNode::arc(name, NodeConfig::default(), |ctx, _| {
        once(Event::new(ctx.platform(), ctx.invocation_id()))
    })
}

/// A node emitting one event built by `f` (Go roleTestNode / preRoledNode / funcResponseNode).
fn content_node(name: &str, f: fn(&Context) -> Event) -> Arc<dyn Node> {
    TestNode::arc(name, NodeConfig::default(), move |ctx, _| once(f(&ctx)))
}

/// Go workflow_test.go CustomRouteNode: one event carrying `routes`.
fn custom_route_node(name: &str, routes: &'static [&'static str]) -> Arc<dyn Node> {
    TestNode::arc(name, NodeConfig::default(), move |ctx, _| {
        let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
        ev.routes = routes.iter().map(|r| (*r).to_string()).collect();
        once(ev)
    })
}

/// Port of Go validationTestNode: counts ValidateInput and Run calls, records the run input.
struct ValidationTestNode {
    base: BaseNode,
    run_input: Arc<Mutex<Option<Value>>>,
    calls: Arc<AtomicI32>,
    validates: AtomicI32,
}

impl Node for ValidationTestNode {
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
        self.validates.fetch_add(1, Ordering::SeqCst);
        self.base.validate_input(v)
    }
    fn validate_output(&self, v: Value) -> Result<Value, WorkflowError> {
        self.base.validate_output(v)
    }
    fn run(&self, ctx: Context, input: Value) -> EventStream {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.run_input.lock() = Some(input);
        once(output_event(&ctx, json!("ok")))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn validation_test_node(cfg: NodeConfig) -> Arc<ValidationTestNode> {
    Arc::new(ValidationTestNode {
        base: BaseNode::with_schemas("val_node", "", Some(json!({"type": "integer"})), None, cfg)
            .expect("schema"),
        run_input: Arc::default(),
        calls: Arc::default(),
        validates: AtomicI32::new(0),
    })
}

// ---------- tests ----------

#[tokio::test]
async fn test_scheduler_linear_chain() {
    let (a, ga) = recording_node("A");
    let (b, gb) = recording_node("B");
    let (c, gc) = recording_node("C");
    ga.release();
    gb.release();
    gc.release();
    let w = must_new(chain(&[&start(), &a, &b, &c]));
    let events = drain(w.run(seeded_mock_ctx())).await;
    assert_eq!(
        outputs_of(&events),
        vec!["seed:A", "seed:A:B", "seed:A:B:C"]
    );
}

#[tokio::test]
async fn test_scheduler_message_as_output_feeds_successor() {
    let a = message_as_output_node("A", "hello");
    let (b, gb) = recording_node("B");
    gb.release();
    let w = must_new(chain(&[&start(), &a, &b]));
    let events = drain(w.run(seeded_mock_ctx())).await;
    assert_eq!(
        outputs_of(&events),
        vec!["hello:B"],
        "A's message text fed to B"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_scheduler_fan_out_concurrency() {
    const FAN_OUT: i32 = 3;
    let concurrent = Arc::new(AtomicI32::new(0));
    let peak = Arc::new(AtomicI32::new(0));
    let barrier = Gate::new();
    let nodes: Vec<Arc<dyn Node>> = (0..FAN_OUT)
        .map(|i| {
            let (concurrent, peak, barrier) = (
                Arc::clone(&concurrent),
                Arc::clone(&peak),
                Arc::clone(&barrier),
            );
            let name = format!("N{i}");
            let n = name.clone();
            TestNode::arc(&name, NodeConfig::default(), move |ctx, _| {
                let (concurrent, peak, barrier, n) = (
                    Arc::clone(&concurrent),
                    Arc::clone(&peak),
                    Arc::clone(&barrier),
                    n.clone(),
                );
                async_stream::stream! {
                    let now = concurrent.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    barrier.wait().await;
                    concurrent.fetch_sub(1, Ordering::SeqCst);
                    yield Ok(output_event(&ctx, json!(n)));
                }
                .boxed()
            })
        })
        .collect();
    let join: Arc<dyn Node> = JoinNode::new("join");
    let edges = nodes
        .iter()
        .flat_map(|n| [edge(&start(), n), edge(n, &join)])
        .collect();
    let w = must_new(edges);
    let run = tokio::spawn(async move { drain(w.run(seeded_mock_ctx())).await });
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && peak.load(Ordering::SeqCst) != FAN_OUT {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    barrier.release();
    run.await.expect("run task");
    assert_eq!(
        peak.load(Ordering::SeqCst),
        FAN_OUT,
        "peak concurrent nodes (fan-out children should run in parallel)"
    );
}

// parity: adapted: Go drains Workflow.Run; Rust drives the Scheduler directly (as Go's newScheduler/run) so the RunState (Failed + Cancelled x2) is also checked.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_scheduler_failed_siblings_cancelled() {
    let (a, obs_a) = cancel_observing_node("A", NodeConfig::default());
    let b = erroring_node("B", "B failed intentionally");
    let (c, obs_c) = cancel_observing_node("C", NodeConfig::default());
    let w = must_new(vec![
        edge(&start(), &a),
        edge(&start(), &b),
        edge(&start(), &c),
    ]);
    let ctx = seeded_mock_ctx().into_context();
    let mut s = Scheduler::new(ctx.clone(), Arc::clone(&w.graph), w.max_concurrency);
    s.state.ensure_node(START.name()).input = Some(json!("seed"));
    s.schedule_node(
        Arc::clone(&START),
        Some(json!("seed")),
        String::new(),
        ctx.branch(),
    );
    let mut first_err = None;
    while let Some(item) = s.next().await {
        if let Err(e) = item
            && first_err.is_none()
        {
            first_err = Some(e);
        }
    }
    let err = first_err.expect("Run error");
    assert_eq!(
        find_cause::<TestError>(&err),
        Some(&TestError("B failed intentionally")),
        "Run error = {err}, want it to wrap the node error"
    );
    assert!(
        obs_a.observed.load(Ordering::SeqCst),
        "node A: ctx.Done() not observed (sibling cancellation broken)"
    );
    assert!(
        obs_c.observed.load(Ordering::SeqCst),
        "node C: ctx.Done() not observed (sibling cancellation broken)"
    );
    let status = |n: &str| s.state.nodes.get(n).map(|ns| ns.status);
    assert_eq!(status("B"), Some(NodeStatus::Failed));
    assert_eq!(status("A"), Some(NodeStatus::Cancelled));
    assert_eq!(status("C"), Some(NodeStatus::Cancelled));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_scheduler_draining_does_not_retry_failed_sibling() {
    let primary = erroring_node("primary", "primary node failed");
    let sibling_calls = Arc::new(AtomicI32::new(0));
    let sc = Arc::clone(&sibling_calls);
    let sibling = function_node::<Value, Value, _, _>(
        "sibling",
        move |ctx: Context, _i: Value| {
            sc.fetch_add(1, Ordering::SeqCst);
            async move {
                ctx.cancellation_token().cancelled().await;
                Err::<Value, _>(test_err("cancelled sibling failed during cleanup"))
            }
        },
        NodeConfig {
            retry_config: Some(always_retry(3, Duration::from_secs(3600))),
            ..NodeConfig::default()
        },
    );
    let w = must_new(vec![edge(&start(), &primary), edge(&start(), &sibling)]);
    let err = drain_err(w.run(seeded_mock_ctx())).await;
    assert_eq!(
        find_cause::<TestError>(&err),
        Some(&TestError("primary node failed")),
        "Run error = {err}, want it to wrap the primary error"
    );
    assert_eq!(
        sibling_calls.load(Ordering::SeqCst),
        1,
        "sibling calls (draining must suppress retries)"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_scheduler_external_cancellation_fails_run() {
    let token = CancellationToken::new();
    let (n, obs) = cancel_observing_node("n", NodeConfig::default());
    let w = must_new(vec![edge(&start(), &n)]);
    let run = spawn_first_err(w.run(seeded_ctx_with_token(&token)));
    tokio::time::timeout(Duration::from_secs(1), obs.started.wait())
        .await
        .expect("timeout waiting for node to start");
    token.cancel();
    let err = tokio::time::timeout(Duration::from_secs(1), run)
        .await
        .expect("timeout waiting for workflow to finish")
        .expect("run task")
        .expect("Workflow.Run ended cleanly after external cancellation");
    assert!(
        is_cancelled(&err),
        "Run error = {err}, want context.Canceled"
    );
}

// parity: adapted: Go sleeps 50 ms for the retry timer to arm; Rust waits on the should_retry decision, which the scheduler makes synchronously right before arming the timer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_scheduler_external_cancellation_during_pending_retry() {
    let token = CancellationToken::new();
    let retry_decided = Gate::new();
    let decided = Arc::clone(&retry_decided);
    let (n, calls) = retry_test_node(
        "flaky",
        100,
        NodeConfig {
            retry_config: Some(RetryConfig {
                should_retry: Some(Arc::new(move |_| {
                    decided.release();
                    true
                })),
                ..always_retry(5, Duration::from_secs(3600))
            }),
            ..NodeConfig::default()
        },
    );
    let w = must_new(vec![edge(&start(), &n)]);
    let run = spawn_first_err(w.run(seeded_ctx_with_token(&token)));
    wait_for_calls(&calls, 1).await;
    tokio::time::timeout(Duration::from_secs(5), retry_decided.wait())
        .await
        .expect("timeout waiting for the failed attempt to arm its retry");
    token.cancel();
    let err = tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .expect("timeout waiting for workflow to finish")
        .expect("run task");
    assert!(
        err.as_ref().is_some_and(is_cancelled),
        "Run error = {err:?}, want context.Canceled"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "node attempts (the retry must not run under a dead context)"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_scheduler_external_cancellation_prefers_node_error() {
    let token = CancellationToken::new();
    let (n, started, release) = releasable_erroring_node("n", "node failed on its own terms");
    let w = must_new(vec![edge(&start(), &n)]);
    let run = spawn_first_err(w.run(seeded_ctx_with_token(&token)));
    tokio::time::timeout(Duration::from_secs(1), started.wait())
        .await
        .expect("timeout waiting for node to start");
    token.cancel();
    release.release();
    let err = tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .expect("timeout waiting for workflow to finish")
        .expect("run task")
        .expect("Run error");
    assert_eq!(
        find_cause::<TestError>(&err),
        Some(&TestError("node failed on its own terms")),
        "Run error = {err}, want it to wrap the node error"
    );
}

#[rstest::rstest]
#[case::cancel(false)]
#[case::deadline(true)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
// parity: adapted: an invocation CancellationToken carries no deadline, so the "deadline" case cancels the token from a task after a 100 ms sleep; the Scheduler is driven directly as Go's newScheduler/run.
async fn test_scheduler_external_cancellation_marks_node_cancelled(#[case] deadline: bool) {
    let token = CancellationToken::new();
    let (n, obs) = cancel_observing_node("n", NodeConfig::default());
    let w = must_new(vec![edge(&start(), &n)]);
    let c = seeded_ctx_with_token(&token).into_context();
    let mut s = Scheduler::new(c.clone(), Arc::clone(&w.graph), w.max_concurrency);
    s.state.ensure_node(START.name()).input = None;
    s.schedule_node(Arc::clone(&START), None, String::new(), c.branch());
    let kill = async {
        tokio::time::timeout(Duration::from_secs(1), obs.started.wait())
            .await
            .expect("timeout waiting for node to start");
        if deadline {
            let t = token.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(100)).await;
                t.cancel();
            })
            .await
            .expect("deadline task");
        } else {
            token.cancel();
        }
    };
    let run = async { while s.next().await.is_some() {} };
    tokio::time::timeout(Duration::from_secs(5), async { tokio::join!(run, kill) })
        .await
        .expect("timeout waiting for scheduler to finish");
    let ns = s.state.nodes.get("n").expect("no NodeState recorded for n");
    assert_eq!(ns.status, NodeStatus::Cancelled, "node status");
}

// parity: adapted: an invocation deadline is modelled as cancelling the token after 50 ms; a token carries no cause, so the run error is AgentError::Cancelled (node-level DeadlineExceeded is covered by test_scheduler_node_timeout).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_scheduler_external_deadline_surfaces_cause() {
    let token = CancellationToken::new();
    let (n, _obs) = cancel_observing_node("n", NodeConfig::default());
    let w = must_new(vec![edge(&start(), &n)]);
    let run = spawn_first_err(w.run(seeded_ctx_with_token(&token)));
    let t = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        t.cancel();
    });
    let err = tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .expect("timeout waiting for workflow to finish")
        .expect("run task");
    assert!(
        err.as_ref().is_some_and(is_cancelled),
        "Run error = {err:?}, want the cancellation"
    );
}

// parity: adapted: Go's caller break (yield returning false) is dropping the stream after its first event.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_scheduler_caller_break_stops_scheduling() {
    let (b_ran, c_ran) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );
    let a = output_fn_node("A", "a-out");
    let (br, cr) = (Arc::clone(&b_ran), Arc::clone(&c_ran));
    let b = function_node::<Value, String, _, _>(
        "B",
        move |_c: Context, _i: Value| {
            br.store(true, Ordering::SeqCst);
            async { Ok("b-out".to_string()) }
        },
        NodeConfig::default(),
    );
    let c = function_node::<Value, String, _, _>(
        "C",
        move |_c: Context, _i: Value| {
            cr.store(true, Ordering::SeqCst);
            async { Ok("c-out".to_string()) }
        },
        NodeConfig::default(),
    );
    let w = must_new(chain(&[&start(), &a, &b, &c]));
    let mut s = w.run(seeded_mock_ctx());
    let first = s.next().await;
    assert!(first.is_some_and(|r| r.is_ok()), "first event");
    drop(s);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !b_ran.load(Ordering::SeqCst),
        "node B ran after caller break; draining should stop further scheduling"
    );
    assert!(
        !c_ran.load(Ordering::SeqCst),
        "node C ran after caller break; draining should stop further scheduling"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_scheduler_node_timeout() {
    let (slow, obs) = cancel_observing_node(
        "slow",
        NodeConfig {
            timeout: Some(Duration::from_millis(50)),
            ..NodeConfig::default()
        },
    );
    let w = must_new(vec![edge(&start(), &slow)]);
    let err = drain_err(w.run(seeded_mock_ctx())).await;
    assert!(
        is_deadline_exceeded(&err),
        "Run error = {err}, want context.DeadlineExceeded"
    );
    assert!(
        obs.observed.load(Ordering::SeqCst),
        "slow node: ctx.Done() not observed (timeout cancellation broken)"
    );
}

#[tokio::test]
async fn test_scheduler_multiple_outputs_fail_node() {
    let bad = multi_output_node("bad", &["first", "second"]);
    let w = must_new(vec![edge(&start(), &bad)]);
    let err = drain_err(w.run(seeded_mock_ctx())).await;
    assert!(
        is_kind(&err, WorkflowErrorKind::MultipleOutputs),
        "Run error = {err}, want it to wrap ErrMultipleOutputs"
    );
    assert_eq!(
        err.to_string(),
        format!(
            "{}: node \"bad\"",
            WorkflowError::sentinel_text(WorkflowErrorKind::MultipleOutputs)
        )
    );
}

#[tokio::test]
async fn test_scheduler_progress_events_then_single_output_succeed() {
    let n = progress_then_output_node("n", 3, json!("final"), None);
    let w = must_new(vec![edge(&start(), &n)]);
    let events = drain(w.run(seeded_mock_ctx())).await;
    assert_eq!(events.len(), 4, "event count");
    assert_eq!(events[3].output, Some(json!("final")), "last event Output");
}

#[tokio::test]
async fn test_scheduler_retry_node() {
    let (n, calls) = retry_test_node(
        "retryNode",
        2,
        NodeConfig {
            retry_config: Some(always_retry(3, Duration::from_millis(1))),
            ..NodeConfig::default()
        },
    );
    let w = must_new(vec![edge(&start(), &n)]);
    let events = drain(w.run(seeded_mock_ctx())).await;
    assert_eq!(calls.load(Ordering::SeqCst), 3, "node calls");
    assert_eq!(events.len(), 3, "expected 2 failure records and 1 result");
    for ev in &events[..2] {
        assert!(
            ev.response.custom_metadata.get("adk.workflow.node_outcome") == Some(&json!("failed"))
                && ev.response.error_code.is_empty()
                && ev.output.is_none()
                && ev.response.error_message.is_empty(),
            "retry failure record has an unexpected outcome, output or error text"
        );
    }
    assert_eq!(events[2].output, Some(json!("seed:retryNode")), "output");
}

#[tokio::test]
async fn test_scheduler_retry_node_exhausted() {
    let (n, calls) = retry_test_node(
        "retryNode",
        10,
        NodeConfig {
            retry_config: Some(always_retry(3, Duration::from_millis(1))),
            ..NodeConfig::default()
        },
    );
    let w = must_new(vec![edge(&start(), &n)]);
    let err = drain_err(w.run(seeded_mock_ctx())).await;
    assert_eq!(calls.load(Ordering::SeqCst), 3, "node calls");
    assert_eq!(
        err.to_string(),
        "fail attempt 3",
        "the last attempt's error"
    );
}

#[tokio::test]
async fn test_scheduler_retry_in_chain() {
    let (a, ga) = recording_node("A");
    ga.release();
    let (b, b_calls) = retry_test_node(
        "B",
        2,
        NodeConfig {
            retry_config: Some(always_retry(3, Duration::from_millis(1))),
            ..NodeConfig::default()
        },
    );
    let (c, gc) = recording_node("C");
    gc.release();
    let w = must_new(chain(&[&start(), &a, &b, &c]));
    let events = drain(w.run(seeded_mock_ctx())).await;
    assert_eq!(
        outputs_of(&events),
        vec!["seed:A", "seed:A:B", "seed:A:B:C"]
    );
    assert_eq!(b_calls.load(Ordering::SeqCst), 3, "node B calls");
}

// parity: adapted: the retry delay is 1 h (Go: 100 ms) so the retry cannot fire before the cancellation regardless of scheduling.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_scheduler_retry_cancelled() {
    let token = CancellationToken::new();
    let (n, calls) = retry_test_node(
        "retryNode",
        2,
        NodeConfig {
            retry_config: Some(always_retry(3, Duration::from_secs(3600))),
            ..NodeConfig::default()
        },
    );
    let w = must_new(vec![edge(&start(), &n)]);
    let run = spawn_first_err(w.run(seeded_ctx_with_token(&token)));
    wait_for_calls(&calls, 1).await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "node calls before cancellation"
    );
    token.cancel();
    let err = tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .expect("timeout waiting for workflow to finish")
        .expect("run task");
    if let Some(e) = &err {
        assert!(is_cancelled(e), "unexpected error: {e}");
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "node calls (retry should not have triggered)"
    );
}

#[tokio::test]
async fn test_scheduler_input_validation_success() {
    let node = validation_test_node(NodeConfig::default());
    let as_node: Arc<dyn Node> = node.clone();
    let w = must_new(vec![edge(&start(), &as_node)]);
    let events = drain(w.run(mock_ctx_with_text("123"))).await;
    assert_eq!(events.len(), 1, "expected 1 event");
    assert_eq!(
        node.run_input.lock().clone(),
        Some(json!(123)),
        "expected node to receive the validated number 123"
    );
}

#[tokio::test]
async fn test_scheduler_input_validation_failure() {
    let node = validation_test_node(NodeConfig {
        retry_config: Some(RetryConfig {
            max_attempts: 3,
            initial_delay: Duration::from_millis(1),
            backoff_factor: 1.0,
            jitter: 0.0,
            ..RetryConfig::default()
        }),
        ..NodeConfig::default()
    });
    let as_node: Arc<dyn Node> = node.clone();
    let w = must_new(vec![edge(&start(), &as_node)]);
    let err = drain_err(w.run(mock_ctx_with_text("not-an-integer"))).await;
    assert!(
        is_kind(&err, WorkflowErrorKind::InputValidation),
        "expected run error to wrap ErrInputValidation, got: {err}"
    );
    assert!(
        err.to_string()
            .starts_with("workflow: input validation failed for node \"val_node\": "),
        "error text = {err}"
    );
    assert_eq!(node.calls.load(Ordering::SeqCst), 0, "node calls");
    assert_eq!(
        node.validates.load(Ordering::SeqCst),
        1,
        "node validates (meaning no retries)"
    );
}

#[tokio::test]
async fn test_scheduler_validate_output_valid_passes() {
    let n = schema_validated_node("n", test_schema(), json!({"value": "hello"}));
    let w = must_new(vec![edge(&start(), &n)]);
    let events = drain(w.run(seeded_mock_ctx())).await;
    assert_eq!(events.len(), 1, "event count");
    assert_eq!(events[0].output, Some(json!({"value": "hello"})));
}

#[tokio::test]
async fn test_scheduler_validate_output_invalid_ends_activation() {
    let n = schema_validated_node("n", test_schema(), json!({"value": 123}));
    let w = must_new(vec![edge(&start(), &n)]);
    let err = drain_err(w.run(seeded_mock_ctx())).await;
    assert!(
        is_kind(&err, WorkflowErrorKind::NodeFailed),
        "err = {err}, want errors.Is(err, ErrNodeFailed)"
    );
    assert!(
        is_kind(&err, WorkflowErrorKind::OutputValidation),
        "err = {err}, want the OutputValidation kind too"
    );
    assert!(
        err.to_string().starts_with(
            "workflow: dynamic child failed: output validation failed for node \"n\": "
        ),
        "error = {err}"
    );
}

#[tokio::test]
async fn test_scheduler_validate_output_no_output_skips_validation() {
    let n = progress_then_output_node("n", 3, json!({"value": "hello"}), Some(test_schema()));
    let w = must_new(vec![edge(&start(), &n)]);
    let events = drain(w.run(seeded_mock_ctx())).await;
    assert_eq!(events.len(), 4, "event count");
    for (i, ev) in events[..3].iter().enumerate() {
        assert_eq!(ev.output, None, "event {i} Output (progress)");
    }
    assert_eq!(
        events[3].output,
        Some(json!({"value": "hello"})),
        "validated map"
    );
}

#[tokio::test]
async fn test_scheduler_stamps_content_role() {
    let node = content_node("emitter", |ctx| {
        let mut ev = output_event(ctx, json!("hi"));
        ev.response.content = Some(Content {
            parts: vec![Part::from_text("hi")],
            ..Content::default()
        });
        ev
    });
    let w = must_new(vec![edge(&start(), &node)]);
    let events = drain(w.run(seeded_mock_ctx())).await;
    let contents: Vec<&Content> = events
        .iter()
        .filter_map(|e| e.response.content.as_ref())
        .collect();
    assert!(!contents.is_empty(), "no event with Content observed");
    for c in contents {
        assert_eq!(c.role, "model", "Content.Role");
    }
}

#[tokio::test]
async fn test_scheduler_preserves_explicit_content_role() {
    let node = content_node("preroled", |ctx| {
        let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
        ev.response.content = Some(Content::from_text("x", "user"));
        ev
    });
    let w = must_new(vec![edge(&start(), &node)]);
    let events = drain(w.run(seeded_mock_ctx())).await;
    for c in events.iter().filter_map(|e| e.response.content.as_ref()) {
        assert_eq!(c.role, "user", "Content.Role preserved");
    }
}

#[tokio::test]
async fn test_scheduler_stamps_function_response_role_user() {
    let node = content_node("fr", |ctx| {
        let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
        let mut response = serde_json::Map::new();
        response.insert("ok".into(), json!(true));
        ev.response.content = Some(Content {
            parts: vec![Part {
                function_response: Some(FunctionResponse {
                    name: "f".into(),
                    response,
                    ..FunctionResponse::default()
                }),
                ..Part::default()
            }],
            ..Content::default()
        });
        ev
    });
    let w = must_new(vec![edge(&start(), &node)]);
    let events = drain(w.run(seeded_mock_ctx())).await;
    let contents: Vec<&Content> = events
        .iter()
        .filter_map(|e| e.response.content.as_ref())
        .collect();
    assert!(!contents.is_empty(), "no event with Content observed");
    for c in contents {
        assert_eq!(c.role, "user", "Content.Role");
    }
}

#[rstest::rstest]
#[case::single_terminal_output_succeeds("single")]
#[case::multiple_terminals_but_only_one_produces_output_succeeds("one_of_many")]
#[case::zero_terminals_producing_output_succeeds("zero")]
#[case::multiple_terminals_producing_output_errors("many")]
#[case::conditional_routing_to_one_of_two_output_terminals_succeeds("routed")]
#[case::non_completed_terminal_with_leftover_output_is_not_counted("leftover")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_scheduler_finalize_single_terminal_output(#[case] case: &str) {
    let edges = match case {
        "single" => {
            let a = output_fn_node("a", "A");
            vec![edge(&start(), &a)]
        }
        "one_of_many" => {
            let (a, b, c) = (
                output_fn_node("a", "A"),
                no_output_node("b"),
                no_output_node("c"),
            );
            vec![edge(&start(), &a), edge(&start(), &b), edge(&start(), &c)]
        }
        "zero" => {
            let (a, b) = (no_output_node("a"), no_output_node("b"));
            vec![edge(&start(), &a), edge(&start(), &b)]
        }
        "many" => {
            let (a, b, c) = (
                output_fn_node("a", "A"),
                output_fn_node("b", "B"),
                output_fn_node("c", "C"),
            );
            let w = must_new(vec![
                edge(&start(), &a),
                edge(&start(), &b),
                edge(&start(), &c),
            ]);
            let err = drain_err(w.run(mock_ctx())).await;
            assert!(
                is_kind(&err, WorkflowErrorKind::MultipleTerminalOutputs),
                "got error {err}, want ErrMultipleTerminalOutputs"
            );
            assert_eq!(
                err.to_string(),
                format!(
                    "{}: a, b, c",
                    WorkflowError::sentinel_text(WorkflowErrorKind::MultipleTerminalOutputs)
                ),
                "error names every terminal, sorted"
            );
            return;
        }
        "routed" => {
            let router = custom_route_node("router", &["branchA"]);
            let (a, b) = (output_fn_node("a", "A"), output_fn_node("b", "B"));
            vec![
                edge(&start(), &router),
                route_edge(&router, &a, Arc::new(StringRoute("branchA".into()))),
                route_edge(&router, &b, Arc::new(StringRoute("branchB".into()))),
            ]
        }
        _ => {
            let (a, b) = (output_fn_node("a", "A"), output_fn_node("b", "B"));
            let w = must_new(vec![edge(&start(), &a), edge(&start(), &b)]);
            let mut s = Scheduler::new(top_level_ctx(), Arc::clone(&w.graph), 0);
            s.state.nodes.insert(
                "a".into(),
                NodeState {
                    status: NodeStatus::Completed,
                    output: Some(json!("A")),
                    ..NodeState::default()
                },
            );
            s.state.nodes.insert(
                "b".into(),
                NodeState {
                    status: NodeStatus::Pending,
                    output: Some(json!("B")),
                    ..NodeState::default()
                },
            );
            s.finalize().expect("finalize: unexpected error");
            return;
        }
    };
    let w = must_new(edges);
    drain(w.run(mock_ctx())).await;
}
