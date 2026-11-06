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

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Port of ADK-Go runner/workflow_error_identity_test.go.

use std::any::Any;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering::SeqCst};
use std::time::Duration;

use futures::StreamExt;
use gadk_agent::{Agent, AgentError, Context, EventStream, RunConfig};
use gadk_engine::agents::workflow_agent::{self, WorkflowAgentConfig};
use gadk_engine::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use gadk_engine::workflow::{
    BaseNode, Edge, Emit, Node, NodeConfig, RetryConfig, RunNodeOptions, START, dynamic_node,
    emitting_function_node, function_node, run_node,
};
use gadk_engine::{NodeRunError, WorkflowError, workflow_error};
use gadk_genai::Content;
use gadk_platform::Platform;
use gadk_session::{Event, InMemorySessionService};
use serde_json::{Value, json};

fn start() -> Arc<dyn Node> {
    Arc::clone(&START)
}

fn edge(from: &Arc<dyn Node>, to: &Arc<dyn Node>) -> Edge {
    Edge {
        from: from.clone(),
        to: to.clone(),
        route: None,
    }
}

/// Every event and every error, in order (Go loops that record `err` and continue).
async fn run_all(mut s: EventStream) -> (Vec<Event>, Vec<AgentError>) {
    let (mut events, mut errs) = (Vec::new(), Vec::new());
    while let Some(item) = s.next().await {
        match item {
            Ok(ev) => events.push(ev),
            Err(e) => errs.push(e),
        }
    }
    (events, errs)
}

/// A `RetryConfig::should_retry` predicate.
type ShouldRetry = Arc<dyn Fn(&AgentError) -> bool + Send + Sync>;

/// Retry config with Go's zero-valued delays (`&workflow.RetryConfig{MaxAttempts: n}`).
fn retry(max_attempts: i32, should_retry: Option<ShouldRetry>) -> RetryConfig {
    RetryConfig {
        max_attempts,
        initial_delay: Duration::ZERO,
        max_delay: Duration::ZERO,
        backoff_factor: 0.0,
        jitter: 0.0,
        should_retry,
    }
}

/// Go `newJoinResumeRunner`: root workflow agent "root", app "repro", auto-created session.
fn new_join_resume_runner(edges: Vec<Edge>) -> Runner {
    let root: Arc<dyn Agent> = workflow_agent::new(WorkflowAgentConfig {
        name: "root".into(),
        edges,
        ..Default::default()
    })
    .expect("workflowagent.New()");
    Runner::new(RunnerConfig {
        app_name: "repro".into(),
        agent: root,
        session_service: Arc::new(InMemorySessionService::new()),
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig::default(),
        auto_create_session: true,
        platform: Platform::default(),
        compaction: None,
    })
    .expect("runner.New()")
}

/// Go `r.Run(ctx, "u", "s", NewContentFromText("start", RoleUser), RunConfig{})`.
fn run_start(r: &Runner) -> EventStream {
    r.run(
        "u",
        "s",
        Some(Content::from_text("start", "user")),
        RunConfig::default(),
        RunOptions::default(),
    )
}

#[derive(Debug)]
struct SentinelError; // Go errors.New("scripted sentinel")
impl fmt::Display for SentinelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("scripted sentinel")
    }
}
impl std::error::Error for SentinelError {}

#[derive(Debug)]
struct WorkflowBodyError; // Go *workflowBodyError
impl fmt::Display for WorkflowBodyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("scripted body failure")
    }
}
impl std::error::Error for WorkflowBodyError {}

#[derive(Debug)]
struct ScriptedWrapper(SentinelError); // Go fmt.Errorf("scripted wrapper: %w", sentinel)
impl fmt::Display for ScriptedWrapper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "scripted wrapper: {}", self.0)
    }
}
impl std::error::Error for ScriptedWrapper {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

#[derive(Clone, Copy, Debug)]
enum Case {
    Sentinel,
    Typed,
    UserWrapper,
    NodeRunError,
}

impl Case {
    fn name(self) -> &'static str {
        match self {
            Case::Sentinel => "sentinel",
            Case::Typed => "typed",
            Case::UserWrapper => "user_wrapper",
            Case::NodeRunError => "node_run_error",
        }
    }
    /// A fresh error of this case (Go reuses one `tc.err` value; Rust errors are not Clone).
    fn make(self) -> AgentError {
        match self {
            Case::Sentinel => AgentError::Other(Box::new(SentinelError)),
            Case::Typed => AgentError::Other(Box::new(WorkflowBodyError)),
            Case::UserWrapper => AgentError::Other(Box::new(ScriptedWrapper(SentinelError))),
            // Go &workflow.NodeRunError{ChildName: "child", Cause: sentinel}: "workflow: dynamic child child: scripted sentinel".
            Case::NodeRunError => WorkflowError::node_run(NodeRunError {
                child_name: "child".into(),
                child_path: String::new(),
                run_id: String::new(),
                cause: Some(Arc::new(AgentError::Other(Box::new(SentinelError)))),
            })
            .into(),
        }
    }
    /// Rust stand-in for Go `err == tc.err`: same concrete type and same text, unwrapped.
    fn same(self, err: &AgentError) -> bool {
        let text = err.to_string() == self.make().to_string();
        let typed = match (self, err) {
            (Case::Sentinel, AgentError::Other(b)) => b.downcast_ref::<SentinelError>().is_some(),
            (Case::Typed, AgentError::Other(b)) => b.downcast_ref::<WorkflowBodyError>().is_some(),
            (Case::UserWrapper, AgentError::Other(b)) => {
                b.downcast_ref::<ScriptedWrapper>().is_some()
            }
            // Go errors.As(err, &*workflow.NodeRunError) with the same child.
            (Case::NodeRunError, e) => workflow_error(e)
                .and_then(|w| w.node_run_error())
                .is_some_and(|n| n.child_name == "child"),
            _ => false,
        };
        text && typed
    }
}

// parity: adapted: Go compares error pointer identity; Rust compares concrete type + Display text of the unwrapped error
#[tokio::test]
async fn test_runner_workflow_error_identity() {
    for case in [
        Case::Sentinel,
        Case::Typed,
        Case::UserWrapper,
        Case::NodeRunError,
    ] {
        for mode in ["recovered", "exhausted", "no_retry"] {
            let label = format!("{}/{mode}", case.name());
            let attempts = Arc::new(AtomicI32::new(0));
            let predicate_calls = Arc::new(AtomicUsize::new(0));
            let predicate_mismatch = Arc::new(AtomicBool::new(false));
            let mut cfg = NodeConfig::default();
            if mode != "no_retry" {
                let (calls, mismatch) = (predicate_calls.clone(), predicate_mismatch.clone());
                cfg.retry_config = Some(retry(
                    2,
                    Some(Arc::new(move |err: &AgentError| {
                        calls.fetch_add(1, SeqCst);
                        let same = case.same(err);
                        if !same {
                            mismatch.store(true, SeqCst);
                        }
                        same
                    })),
                ));
            }
            let a2 = attempts.clone();
            let n = dynamic_node::<Value, Value, _, _>(
                "body",
                move |_ctx: Context, _in: Value, _emit: Emit| {
                    let attempt = a2.fetch_add(1, SeqCst) + 1;
                    async move {
                        if attempt == 1 || mode != "recovered" {
                            Err(case.make())
                        } else {
                            Ok(json!("ok"))
                        }
                    }
                },
                cfg,
            );
            let r = new_join_resume_runner(vec![edge(&start(), &n)]);
            let (events, errs) = run_all(run_start(&r)).await;
            let completed = events.iter().any(|e| e.output == Some(json!("ok")));

            assert!(
                !predicate_mismatch.load(SeqCst),
                "{label}: retry predicate received a different error"
            );
            let (want_attempts, want_predicates) = if mode == "no_retry" { (1, 0) } else { (2, 1) };
            assert_eq!(
                (attempts.load(SeqCst), predicate_calls.load(SeqCst)),
                (want_attempts, want_predicates),
                "{label}: error identity changed retry behavior"
            );
            if mode == "recovered" {
                assert!(
                    errs.is_empty() && completed,
                    "{label}: identity-based retry did not recover: {errs:?}"
                );
            } else {
                let got = errs
                    .last()
                    .unwrap_or_else(|| panic!("{label}: no final error"));
                assert!(
                    case.same(got),
                    "{label}: final error identity changed: {got:?}"
                );
            }
        }
    }
}

/// Go `opaqueInventoryNode`: forwards Run to a wrapped node, hiding it from the executor.
struct OpaqueInventoryNode {
    base: BaseNode,
    node: Arc<dyn Node>,
}

impl Node for OpaqueInventoryNode {
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
    fn run(&self, ctx: Context, input: Value) -> EventStream {
        self.node.run(ctx, input)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[tokio::test]
async fn test_runner_workflow_opaque_wrapper_retry() {
    for placement in ["static", "dynamic"] {
        const FAILURE: &str = "scripted wrapped body failure";
        let charge_calls = Arc::new(AtomicI32::new(0));
        let body_calls = Arc::new(AtomicI32::new(0));
        let cc = charge_calls.clone();
        let charge = function_node::<Value, String, _, _>(
            "charge",
            move |_ctx: Context, _in: Value| {
                cc.fetch_add(1, SeqCst);
                async { Ok("charged".to_string()) }
            },
            NodeConfig::default(),
        );
        let bc = body_calls.clone();
        let body = dynamic_node::<Value, Value, _, _>(
            "body",
            move |ctx: Context, input: Value, _emit: Emit| {
                let (charge, bc) = (charge.clone(), bc.clone());
                async move {
                    let out = run_node(
                        &ctx,
                        charge,
                        input,
                        RunNodeOptions {
                            run_id: Some("stable".into()),
                            ..Default::default()
                        },
                    )
                    .await?;
                    if bc.fetch_add(1, SeqCst) + 1 == 1 {
                        return Err(AgentError::other(FAILURE));
                    }
                    Ok(out)
                }
            },
            NodeConfig::default(),
        );
        let cfg = if placement == "dynamic" {
            NodeConfig::default()
        } else {
            NodeConfig {
                retry_config: Some(retry(
                    2,
                    Some(Arc::new(|err: &AgentError| err.to_string() == FAILURE)),
                )),
                ..Default::default()
            }
        };
        let mut root: Arc<dyn Node> = Arc::new(OpaqueInventoryNode {
            base: BaseNode::new("body", "", cfg),
            node: body,
        });
        if placement == "dynamic" {
            let wrapped = root.clone();
            root = dynamic_node::<Value, Value, _, _>(
                "parent",
                move |ctx: Context, input: Value, _emit: Emit| {
                    let wrapped = wrapped.clone();
                    async move {
                        run_node(
                            &ctx,
                            wrapped,
                            input,
                            RunNodeOptions {
                                run_id: Some("stable".into()),
                                ..Default::default()
                            },
                        )
                        .await
                    }
                },
                NodeConfig {
                    retry_config: Some(retry(2, None)),
                    ..Default::default()
                },
            );
        }
        let r = new_join_resume_runner(vec![edge(&start(), &root)]);
        let (events, errs) = run_all(run_start(&r)).await;
        assert!(
            errs.is_empty(),
            "{placement}: custom wrapper did not recover: {errs:?}"
        );
        let completed = events.iter().any(|e| e.output == Some(json!("charged")));
        // Direct forwarding is opaque to the executor: unknown inventory invalidates the whole wrapper.
        assert!(
            completed && charge_calls.load(SeqCst) == 2 && body_calls.load(SeqCst) == 2,
            "{placement}: opaque wrapper did not conservatively retry (completed={completed}, charge={}, body={})",
            charge_calls.load(SeqCst),
            body_calls.load(SeqCst)
        );
    }
}

/// Go `filteringInventoryNode`: forwards Run to a wrapped node, dropping its failed-outcome events.
struct FilteringInventoryNode {
    base: BaseNode,
    node: Arc<dyn Node>,
}

impl Node for FilteringInventoryNode {
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
    fn run(&self, ctx: Context, input: Value) -> EventStream {
        self.node
            .run(ctx, input)
            .filter(|item| {
                futures::future::ready(
                    !matches!(item, Ok(ev) if ev.response.custom_metadata.get("adk.workflow.node_outcome") == Some(&json!("failed"))),
                )
            })
            .boxed()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[tokio::test]
async fn test_runner_workflow_opaque_wrapper_partial_sibling() {
    let first_calls = Arc::new(AtomicI32::new(0));
    let second_calls = Arc::new(AtomicI32::new(0));
    let fc = first_calls.clone();
    let first = function_node::<Value, String, _, _>(
        "first",
        move |_ctx: Context, _in: Value| {
            let call = fc.fetch_add(1, SeqCst) + 1;
            async move {
                if call == 1 {
                    return Err(AgentError::other("scripted first failure"));
                }
                Ok("recovered-first".to_string())
            }
        },
        NodeConfig::default(),
    );
    let sc = second_calls.clone();
    let second = emitting_function_node::<Value, String, _, _>(
        "second",
        move |ctx: Context, _in: Value, emit: Emit| {
            let call = sc.fetch_add(1, SeqCst) + 1;
            async move {
                if call == 1 {
                    let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
                    ev.output = Some(json!("incomplete-second"));
                    emit(ev).await?;
                    return Err(AgentError::other("scripted second failure"));
                }
                Ok("recovered-second".to_string())
            }
        },
        NodeConfig::default(),
    );
    let body = dynamic_node::<Value, Value, _, _>(
        "body",
        move |ctx: Context, input: Value, _emit: Emit| {
            let (first, second) = (first.clone(), second.clone());
            async move {
                let stable = || RunNodeOptions {
                    run_id: Some("stable".into()),
                    ..Default::default()
                };
                let first_res = run_node(&ctx, first, input.clone(), stable()).await;
                let second_res = run_node(&ctx, second, input, stable()).await;
                first_res?;
                second_res
            }
        },
        NodeConfig::default(),
    );
    let wrapper: Arc<dyn Node> = Arc::new(FilteringInventoryNode {
        base: BaseNode::new(
            "body",
            "",
            NodeConfig {
                retry_config: Some(retry(2, None)),
                ..Default::default()
            },
        ),
        node: body,
    });
    let r = new_join_resume_runner(vec![edge(&start(), &wrapper)]);
    let (events, errs) = run_all(run_start(&r)).await;
    assert!(errs.is_empty(), "wrapper failed to retry: {errs:?}");
    let recovered = events
        .iter()
        .any(|e| e.output == Some(json!("recovered-second")));
    assert!(
        first_calls.load(SeqCst) == 2 && second_calls.load(SeqCst) == 2 && recovered,
        "opaque wrapper trusted incomplete sibling inventory (first={} second={} recovered={recovered})",
        first_calls.load(SeqCst),
        second_calls.load(SeqCst)
    );
}
