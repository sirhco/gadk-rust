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
//! Port of ADK-Go runner/workflow_review_regressions_test.go.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering::SeqCst};
use std::time::Duration;

use futures::StreamExt;
use gadk_agent::{Agent, AgentError, Context, EventStream, RunConfig};
use gadk_engine::agents::workflow_agent::{self, WorkflowAgentConfig};
use gadk_engine::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use gadk_engine::workflow::{
    Edge, Emit, JoinNode, Node, NodeConfig, RetryConfig, RunNodeOptions, START,
    WORKFLOW_INPUT_FUNCTION_CALL_NAME, WorkflowNode, dynamic_node, function_node,
    resume_or_request_input, run_node,
};
use gadk_engine::{WorkflowErrorKind, workflow_error};
use gadk_genai::{Content, FunctionResponse};
use gadk_platform::Platform;
use gadk_session::{Event, InMemorySessionService, RequestInput};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

mod workflow_support;
use workflow_support::{agent_node, join_resume_agent, resume_content};

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

fn user_text(text: &str) -> Content {
    Content::from_text(text, "user")
}

fn function_response_content(id: &str, name: &str, response: Value) -> Content {
    Content::from_function_response(
        FunctionResponse {
            id: id.into(),
            name: name.into(),
            response: response.as_object().cloned().unwrap_or_default(),
            ..Default::default()
        },
        "user",
    )
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

/// Go `errors.Is(err, workflow.ErrX)`.
fn is_kind(err: &AgentError, kind: WorkflowErrorKind) -> bool {
    workflow_error(err).is_some_and(|w| w.is(kind))
}

fn new_runner(agent: Arc<dyn Agent>) -> Runner {
    Runner::new(RunnerConfig {
        app_name: "repro".into(),
        agent,
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

/// Go `newJoinResumeRunner`: root workflow agent "root", app "repro", auto-created session.
fn new_join_resume_runner(edges: Vec<Edge>, agents: Vec<Arc<dyn Agent>>) -> Runner {
    let root = workflow_agent::new(WorkflowAgentConfig {
        name: "root".into(),
        sub_agents: agents,
        edges,
        ..Default::default()
    })
    .expect("workflowagent.New()");
    new_runner(root)
}

/// Go `r.Run(ctx, "u", "s", msg, RunConfig{})`.
fn run(r: &Runner, msg: Content) -> EventStream {
    r.run(
        "u",
        "s",
        Some(msg),
        RunConfig::default(),
        RunOptions::default(),
    )
}

/// Go `errors.Is(err, sentinel)` for a sentinel created with `errors.New(text)`: some error in
/// the source chain (or a workflow error's cause chain) displays exactly `text`.
fn chain_has(err: &AgentError, text: &str) -> bool {
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(e) = cur {
        if e.to_string() == text {
            return true;
        }
        cur = e.source();
    }
    workflow_error(err)
        .and_then(|w| w.cause())
        .is_some_and(|c| chain_has(c, text))
}

/// Retry config with Go's zero-valued delays (`&workflow.RetryConfig{MaxAttempts: n}`).
fn retry(max_attempts: i32) -> RetryConfig {
    RetryConfig {
        max_attempts,
        initial_delay: Duration::ZERO,
        max_delay: Duration::ZERO,
        backoff_factor: 0.0,
        jitter: 0.0,
        should_retry: None,
    }
}

/// `RequestInput{InterruptID: id, Message: message}`.
fn request(id: &str, message: &str) -> RequestInput {
    RequestInput {
        interrupt_id: id.into(),
        message: message.into(),
        ..Default::default()
    }
}

/// Go `r.Run(ctx, "u", "review-delegation", msg, RunConfig{})`.
fn run_delegation(r: &Runner, msg: Content) -> EventStream {
    r.run(
        "u",
        "review-delegation",
        Some(msg),
        RunConfig::default(),
        RunOptions::default(),
    )
}

/// The completion check of the retry tests: an output "charged" stamped at path "root@1/p@1".
fn completed_at_p(events: &[Event]) -> bool {
    events.iter().any(|e| {
        e.output == Some(json!("charged"))
            && e.node_info.as_ref().is_some_and(|n| n.path == "root@1/p@1")
    })
}

// parity: adapted: serde_json keeps integer JSON numbers as integers
#[tokio::test]
async fn test_runner_workflow_hitl_normalized_reentry_response() {
    let cases: [(&str, Value, Option<Value>, Value); 4] = [
        (
            "result",
            json!({"result": "approved"}),
            None,
            json!("approved"),
        ),
        (
            "result_with_schema",
            json!({"result": "approved"}),
            Some(json!({"type": "string"})),
            json!("approved"),
        ),
        ("json_payload", json!({"payload": "42"}), None, json!(42)), // Go decodes to float64 42; serde_json keeps the integer
        (
            "business_map",
            json!({"result": "approved", "other": true}),
            None,
            json!({"result": "approved", "other": true}),
        ),
    ];
    for (name, response, schema, want) in cases {
        let calls = Arc::new(AtomicI32::new(0));
        let wrong_shape = Arc::new(AtomicBool::new(false));
        let (c2, w2, want2) = (calls.clone(), wrong_shape.clone(), want.clone());
        let ask = dynamic_node::<Value, Value, _, _>(
            "ask",
            move |ctx: Context, _in: Value, emit: Emit| {
                let (calls, wrong, want, schema) =
                    (c2.clone(), w2.clone(), want2.clone(), schema.clone());
                async move {
                    calls.fetch_add(1, SeqCst);
                    let req = RequestInput {
                        response_schema: schema,
                        ..request("approval", "approve")
                    };
                    let answer = resume_or_request_input(&ctx, &emit, req)
                        .await
                        .map(Option::unwrap_or_default);
                    if let Ok(a) = &answer
                        && a != &want
                    {
                        wrong.store(true, SeqCst);
                    }
                    answer
                }
            },
            NodeConfig::default(),
        );
        let r = new_join_resume_runner(vec![edge(&start(), &ask)], vec![]);
        let (_, errs) = run_all(run(&r, user_text("start"))).await;
        assert!(errs.is_empty(), "{name}: initial run failed: {errs:?}");
        let msg =
            function_response_content("approval", WORKFLOW_INPUT_FUNCTION_CALL_NAME, response);
        let (_, errs) = run_all(run(&r, msg)).await;
        assert!(
            errs.is_empty(),
            "{name}: normalized response failed to resume: {errs:?}"
        );
        assert!(
            !wrong_shape.load(SeqCst),
            "{name}: re-entry received a response with the wrong shape"
        );
        assert_eq!(calls.load(SeqCst), 2, "{name}: asker did not re-enter");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_runner_workflow_hitl_delegated_output_after_interrupt() {
    let calls = Arc::new(AtomicI32::new(0));
    let downstream_calls = Arc::new(AtomicI32::new(0));
    let raised = CancellationToken::new(); // Go `raised` channel; cancel() == close()
    let compute = function_node::<Value, String, _, _>(
        "compute",
        |_ctx: Context, _in: Value| async { Ok("computed".to_string()) },
        NodeConfig::default(),
    );
    let ask = dynamic_node::<Value, Value, _, _>(
        "ask",
        |ctx: Context, _in: Value, emit: Emit| async move {
            resume_or_request_input(&ctx, &emit, request("approval", "approve"))
                .await
                .map(Option::unwrap_or_default)
        },
        NodeConfig::default(),
    );
    let (c2, r2) = (calls.clone(), raised.clone());
    let p = dynamic_node::<Value, Value, _, _>(
        "p",
        move |ctx: Context, input: Value, _emit: Emit| {
            let (calls, raised, ask, compute) =
                (c2.clone(), r2.clone(), ask.clone(), compute.clone());
            async move {
                calls.fetch_add(1, SeqCst);
                // Go: `go func() { _, err := workflow.RunNode[any](ctx, ask, in); askDone <- err }()`.
                // Rust drives the ask future concurrently in this task with select!/join! instead
                // of spawning it: run_node stays on the node's task, which owns its ctx and emit.
                let (ask_ctx, ask_in) = (ctx.clone(), input.clone());
                let ask_fut = async move {
                    run_node(&ask_ctx, ask, ask_in, RunNodeOptions::default())
                        .await
                        .map(|_| ())
                };
                tokio::pin!(ask_fut);
                let mut ask_res: Option<Result<(), AgentError>> = None;
                loop {
                    tokio::select! {
                        r = &mut ask_fut, if ask_res.is_none() => ask_res = Some(r),
                        _ = raised.cancelled() => break,
                        _ = ctx.cancellation_token().cancelled() => return Err(AgentError::Cancelled),
                    }
                }
                let compute_fut = run_node(
                    &ctx,
                    compute,
                    input,
                    RunNodeOptions {
                        use_as_output: true,
                        ..Default::default()
                    },
                );
                let (computed, asked) = match ask_res {
                    Some(r) => (compute_fut.await, r),
                    None => tokio::join!(compute_fut, &mut ask_fut),
                };
                computed?;
                asked?;
                Ok(Value::Null)
            }
        },
        NodeConfig::default(),
    );
    let dc = downstream_calls.clone();
    let d = function_node::<Value, String, _, _>(
        "d",
        move |_ctx: Context, _in: Value| {
            dc.fetch_add(1, SeqCst);
            async { Ok("done".to_string()) }
        },
        NodeConfig::default(),
    );
    let r = new_join_resume_runner(vec![edge(&start(), &p), edge(&p, &d)], vec![]);

    let mut s = run(&r, user_text("start"));
    // The parent waits on `raised`; bound the drain so a missed interrupt fails, not hangs.
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(item) = s.next().await {
            let ev = item.expect("initial run failed");
            if ev.requested_input.is_some() {
                raised.cancel();
            }
        }
    })
    .await
    .expect("first turn did not finish within 10s (interrupt never raised?)");
    drop(s);
    let msg = resume_content(
        "approval",
        WORKFLOW_INPUT_FUNCTION_CALL_NAME,
        json!("approved"),
    );
    let (_, errs) = run_all(run(&r, msg.clone())).await;
    assert!(
        errs.is_empty(),
        "delegated output prevented re-entry: {errs:?}"
    );
    assert!(
        calls.load(SeqCst) == 2 && downstream_calls.load(SeqCst) == 1,
        "resume did not execute the parent and its successor (parent={}, downstream={})",
        calls.load(SeqCst),
        downstream_calls.load(SeqCst)
    );

    let (_, errs) = run_all(run(&r, msg)).await;
    assert!(
        errs.iter()
            .all(|e| is_kind(e, WorkflowErrorKind::NothingToResume)),
        "duplicate resume returned an unexpected error: {errs:?}"
    );
    assert!(
        calls.load(SeqCst) == 2 && downstream_calls.load(SeqCst) == 1,
        "duplicate answer repeated completed work (parent={}, downstream={})",
        calls.load(SeqCst),
        downstream_calls.load(SeqCst)
    );
}

/// Go `p1`/`p2` body shared by the cached-delegation tests: compute delegated as the output,
/// then two sequential approvals, returning nil.
fn delegating_approver(name: &str) -> Arc<dyn Node> {
    let compute = function_node::<Value, String, _, _>(
        "compute",
        |_ctx: Context, _in: Value| async { Ok("computed".to_string()) },
        NodeConfig::default(),
    );
    dynamic_node::<Value, Value, _, _>(
        name,
        move |ctx: Context, input: Value, emit: Emit| {
            let compute = compute.clone();
            async move {
                run_node(
                    &ctx,
                    compute,
                    input,
                    RunNodeOptions {
                        use_as_output: true,
                        ..Default::default()
                    },
                )
                .await?;
                resume_or_request_input(&ctx, &emit, request("p1", "approve")).await?;
                resume_or_request_input(&ctx, &emit, request("p2", "approve 2")).await?;
                Ok(Value::Null)
            }
        },
        NodeConfig::default(),
    )
}

/// The four turns of the cached-delegation tests; `p` is the node whose output joins with b's.
async fn run_cached_delegation(p: Arc<dyn Node>, what: &str) {
    let b = join_resume_agent("b", "B", true);
    let bn = agent_node(&b);
    let join: Arc<dyn Node> = JoinNode::new("join");
    let r = new_join_resume_runner(
        vec![
            edge(&start(), &p),
            edge(&start(), &bn),
            edge(&p, &join),
            edge(&bn, &join),
        ],
        vec![b],
    );
    for msg in [
        user_text("start"),
        resume_content("p1", WORKFLOW_INPUT_FUNCTION_CALL_NAME, json!("approved P")),
        resume_content(
            "p2",
            WORKFLOW_INPUT_FUNCTION_CALL_NAME,
            json!("approved P2"),
        ),
    ] {
        let (_, errs) = run_all(run_delegation(&r, msg)).await;
        assert!(errs.is_empty(), "workflow execution failed: {errs:?}");
    }
    let (events, errs) = run_all(run_delegation(
        &r,
        resume_content("b", WORKFLOW_INPUT_FUNCTION_CALL_NAME, json!("approved B")),
    ))
    .await;
    assert!(errs.is_empty(), "workflow execution failed: {errs:?}");
    let mut saw_join = false;
    for ev in &events {
        if let Some(out @ Value::Object(_)) = &ev.output {
            saw_join = true;
            assert_eq!(
                out,
                &json!({"p": "computed", "b": "approved B"}),
                "join lost the {what} delegated result"
            );
        }
    }
    assert!(saw_join, "join not emitted");
}

#[tokio::test]
async fn test_runner_workflow_hitl_cached_delegation_across_resume() {
    run_cached_delegation(delegating_approver("p"), "cached").await;
}

#[tokio::test]
async fn test_runner_workflow_hitl_nested_cached_delegation_across_resume() {
    let mid = delegating_approver("mid");
    let p = dynamic_node::<Value, Value, _, _>(
        "p",
        move |ctx: Context, input: Value, _emit: Emit| {
            let mid = mid.clone();
            async move {
                run_node(
                    &ctx,
                    mid,
                    input,
                    RunNodeOptions {
                        use_as_output: true,
                        ..Default::default()
                    },
                )
                .await
            }
        },
        NodeConfig::default(),
    );
    run_cached_delegation(p, "nested").await;
}

#[tokio::test]
async fn test_runner_workflow_retry_preserves_completed_child() {
    const FAILURE: &str = "scripted body failure";
    // (name, nested, child_failure, workflow_node)
    for (name, nested, child_failure, workflow_node) in [
        ("direct", false, false, false),
        ("nested", true, false, false),
        ("nested_body_failure", true, true, false),
        ("workflow_node_body_failure", true, true, true),
    ] {
        let charge_calls = Arc::new(AtomicI32::new(0));
        let parent_calls = Arc::new(AtomicI32::new(0));
        let child_calls = Arc::new(AtomicI32::new(0));
        let cc = charge_calls.clone();
        let mut charge = function_node::<Value, String, _, _>(
            "charge",
            move |_ctx: Context, _in: Value| {
                cc.fetch_add(1, SeqCst);
                async { Ok("charged".to_string()) }
            },
            NodeConfig::default(),
        );
        if workflow_node {
            let compute = charge.clone();
            charge = dynamic_node::<Value, Value, _, _>(
                "charge",
                move |ctx: Context, input: Value, _emit: Emit| {
                    let compute = compute.clone();
                    async move {
                        run_node(
                            &ctx,
                            compute,
                            input,
                            RunNodeOptions {
                                use_as_output: true,
                                ..Default::default()
                            },
                        )
                        .await
                    }
                },
                NodeConfig::default(),
            );
        }
        let mut child = charge.clone();
        if nested {
            let (charge, ch) = (charge.clone(), child_calls.clone());
            child = dynamic_node::<Value, String, _, _>(
                "mid",
                move |ctx: Context, input: Value, _emit: Emit| {
                    let (charge, ch) = (charge.clone(), ch.clone());
                    async move {
                        let out = run_node(
                            &ctx,
                            charge,
                            input,
                            RunNodeOptions {
                                run_id: Some("charge".into()),
                                ..Default::default()
                            },
                        )
                        .await?;
                        if ch.fetch_add(1, SeqCst) + 1 == 1 && child_failure {
                            return Err(AgentError::other(FAILURE));
                        }
                        Ok(out.as_str().unwrap_or_default().to_string())
                    }
                },
                NodeConfig::default(),
            );
        }
        if workflow_node {
            child = WorkflowNode::new("inner", vec![edge(&start(), &child)])
                .expect("nested workflow construction failed");
        }
        let pc = parent_calls.clone();
        let p = dynamic_node::<Value, String, _, _>(
            "p",
            move |ctx: Context, input: Value, _emit: Emit| {
                let (child, pc) = (child.clone(), pc.clone());
                async move {
                    pc.fetch_add(1, SeqCst);
                    let out = run_node(
                        &ctx,
                        child,
                        input,
                        RunNodeOptions {
                            run_id: Some("charge".into()),
                            ..Default::default()
                        },
                    )
                    .await?;
                    if pc.load(SeqCst) == 1 && !child_failure {
                        return Err(AgentError::other(FAILURE));
                    }
                    Ok(out.as_str().unwrap_or_default().to_string())
                }
            },
            NodeConfig {
                retry_config: Some(retry(2)),
                ..Default::default()
            },
        );
        let r = new_join_resume_runner(vec![edge(&start(), &p)], vec![]);
        let (events, errs) = run_all(run(&r, user_text("start"))).await;
        assert!(
            errs.is_empty(),
            "{name}: runner did not recover the parent failure: {errs:?}"
        );
        let completed = completed_at_p(&events);
        assert!(
            completed && parent_calls.load(SeqCst) == 2 && charge_calls.load(SeqCst) == 1,
            "{name}: parent retry repeated a completed child or lost its result (completed={completed}, parent={}, charge={}, child={})",
            parent_calls.load(SeqCst),
            charge_calls.load(SeqCst),
            child_calls.load(SeqCst)
        );
    }
}

#[tokio::test]
async fn test_runner_workflow_retry_replaces_recovered_child_failures() {
    const LEAF_FAILURE: &str = "scripted leaf failure";
    let leaf_calls = Arc::new(AtomicI32::new(0));
    let mid_calls = Arc::new(AtomicI32::new(0));
    let parent_calls = Arc::new(AtomicI32::new(0));
    let lc = leaf_calls.clone();
    let leaf = function_node::<Value, String, _, _>(
        "leaf",
        move |_ctx: Context, _in: Value| {
            let call = lc.fetch_add(1, SeqCst) + 1;
            async move {
                if call == 1 {
                    return Err(AgentError::other(LEAF_FAILURE));
                }
                Ok("charged".to_string())
            }
        },
        NodeConfig::default(),
    );
    let mc = mid_calls.clone();
    let mid = dynamic_node::<Value, String, _, _>(
        "mid",
        move |ctx: Context, input: Value, _emit: Emit| {
            let (leaf, call) = (leaf.clone(), mc.fetch_add(1, SeqCst) + 1);
            async move {
                let out = run_node(
                    &ctx,
                    leaf,
                    input,
                    RunNodeOptions {
                        run_id: Some("leaf".into()),
                        ..Default::default()
                    },
                )
                .await?;
                if call == 2 {
                    return Err(AgentError::other("scripted middle body failure"));
                }
                Ok(out.as_str().unwrap_or_default().to_string())
            }
        },
        NodeConfig::default(),
    );
    let pc = parent_calls.clone();
    let p = dynamic_node::<Value, Value, _, _>(
        "p",
        move |ctx: Context, input: Value, _emit: Emit| {
            let mid = mid.clone();
            pc.fetch_add(1, SeqCst);
            async move {
                let middle = || RunNodeOptions {
                    run_id: Some("middle".into()),
                    ..Default::default()
                };
                let mut res = run_node(&ctx, mid.clone(), input.clone(), middle()).await;
                if res.as_ref().is_err_and(|e| chain_has(e, LEAF_FAILURE)) {
                    res = run_node(&ctx, mid, input, middle()).await;
                }
                res
            }
        },
        NodeConfig {
            retry_config: Some(retry(2)),
            ..Default::default()
        },
    );
    let r = new_join_resume_runner(vec![edge(&start(), &p)], vec![]);
    let (events, errs) = run_all(run(&r, user_text("start"))).await;
    assert!(
        errs.is_empty(),
        "runner did not recover the nested failures: {errs:?}"
    );
    assert!(
        completed_at_p(&events)
            && leaf_calls.load(SeqCst) == 2
            && mid_calls.load(SeqCst) == 3
            && parent_calls.load(SeqCst) == 2,
        "new child inventory retained a recovered leaf's stale failure (leaf={}, mid={}, parent={})",
        leaf_calls.load(SeqCst),
        mid_calls.load(SeqCst),
        parent_calls.load(SeqCst)
    );
}
