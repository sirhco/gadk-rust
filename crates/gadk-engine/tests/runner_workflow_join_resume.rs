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
//! Port of ADK-Go runner/workflow_join_resume_test.go.

use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering::SeqCst};
use std::time::Duration;

use futures::StreamExt;
use gadk_agent::{
    Agent, AgentConfig, AgentError, AgentRunFn, Context, EventStream, InvocationContext, RunConfig,
};
use gadk_engine::agents::workflow_agent::{self, WorkflowAgentConfig};
use gadk_engine::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use gadk_engine::workflow::{
    AgentNode, Edge, Emit, JoinNode, Node, NodeConfig, RunNodeOptions, START,
    WORKFLOW_INPUT_FUNCTION_CALL_NAME, dynamic_node, function_node, resume_or_request_input,
    run_node,
};
use gadk_engine::{WorkflowErrorKind, workflow_error};
use gadk_genai::{Content, FunctionResponse};
use gadk_platform::Platform;
use gadk_session::{Event, InMemorySessionService, RequestInput};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

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

/// Port of Go `resumeContent`: one FunctionResponse with the payload under "payload".
fn resume_content(call_id: &str, call_name: &str, payload: Value) -> Content {
    function_response_content(call_id, call_name, json!({ "payload": payload }))
}

/// Go `for ev, err := range r.Run(...) { if err != nil { t.Fatal(err) } }`: every event.
async fn drain(s: EventStream) -> Vec<Event> {
    futures::TryStreamExt::try_collect(s)
        .await
        .expect("runner yielded error")
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

/// Go `joinResumeAgent`: a custom agent that either pauses via ResumeOrRequestInput
/// (interrupt id = its name) and then answers `output`, or answers `output` at once.
fn join_resume_agent(name: &str, output: &str, interrupt: bool) -> Arc<dyn Agent> {
    let (agent_name, output) = (name.to_string(), output.to_string());
    let run_fn: AgentRunFn = Arc::new(move |ctx: InvocationContext| -> EventStream {
        let (name, output) = (agent_name.clone(), output.clone());
        Box::pin(async_stream::stream! {
            if interrupt {
                // Go's emit yields straight to the consumer; ResumeOrRequestInput emits at most one
                // event and then returns, so buffering it until the call returns keeps the order.
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
                let emit: Emit = Arc::new(move |ev: Event| {
                    let tx = tx.clone();
                    Box::pin(async move { tx.send(ev).map_err(|_| AgentError::other("consumer stopped")) })
                });
                let res = resume_or_request_input(
                    &ctx.clone().into_context(),
                    &emit,
                    RequestInput { interrupt_id: name.clone(), message: "approve".into(), ..Default::default() },
                )
                .await;
                drop(emit);
                while let Ok(ev) = rx.try_recv() {
                    yield Ok::<Event, AgentError>(ev);
                }
                if res.is_err() {
                    return;
                }
                // Go yields a bare &session.Event{LLMResponse: …}; the agent wrapper fills author/ids.
                let mut ev = Event::default();
                ev.response.content = Some(Content::from_text(output.clone(), "model"));
                yield Ok(ev);
                return;
            }
            let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
            ev.response.content = Some(Content::from_text(output.clone(), "model"));
            yield Ok(ev);
        })
    });
    gadk_agent::agent::new(AgentConfig {
        name: name.into(),
        run: Some(run_fn),
        ..Default::default()
    })
    .expect("agent.New")
}

/// Go `joinResumeAgentNode`.
fn agent_node(a: &Arc<dyn Agent>) -> Arc<dyn Node> {
    AgentNode::new(a.clone(), NodeConfig::default()).expect("workflow.NewAgentNode()")
}

#[tokio::test]
async fn test_runner_workflow_hitl_dynamic_output_ownership() {
    for use_join in [false, true] {
        for delegate in [false, true] {
            let label = format!(
                "{}/{}",
                if use_join { "join" } else { "terminal" },
                if delegate { "delegated" } else { "discarded" }
            );
            let (b, d) = (
                join_resume_agent("b", "B", true),
                join_resume_agent("d", "D", false),
            );
            let (bn, dn) = (agent_node(&b), agent_node(&d));
            let q = function_node::<Value, String, _, _>(
                "q",
                |_ctx: Context, _in: Value| async { Ok("q-out".to_string()) },
                NodeConfig::default(),
            );
            let p = dynamic_node::<Value, Value, _, _>(
                "p",
                move |ctx: Context, input: Value, _emit: Emit| {
                    let q = q.clone();
                    async move {
                        let opts = RunNodeOptions {
                            use_as_output: delegate,
                            ..Default::default()
                        };
                        run_node(&ctx, q, input, opts).await?;
                        Ok(Value::Null) // Go returns (nil, err)
                    }
                },
                NodeConfig::default(),
            );
            let mut edges = vec![edge(&start(), &p), edge(&start(), &bn)];
            if use_join {
                let join: Arc<dyn Node> = JoinNode::new("join");
                edges.extend([edge(&p, &join), edge(&bn, &join), edge(&join, &dn)]);
            } else {
                edges.push(edge(&bn, &dn));
            }
            let r = new_join_resume_runner(edges, vec![b, d]);
            drain(run(&r, user_text("start"))).await;

            let (events, errs) = run_all(run(
                &r,
                resume_content("b", WORKFLOW_INPUT_FUNCTION_CALL_NAME, json!("approved")),
            ))
            .await;
            let saw_d = events
                .iter()
                .any(|e| e.author == "d" && e.response.content.is_some());
            let mut saw_join = false;
            for ev in &events {
                if let Some(out @ Value::Object(_)) = &ev.output {
                    saw_join = true;
                    let want_p = if delegate {
                        json!("q-out")
                    } else {
                        Value::Null
                    };
                    assert_eq!(
                        out,
                        &json!({ "p": want_p, "b": "approved" }),
                        "{label}: join did not preserve the orchestrator's output ownership"
                    );
                }
            }
            assert!(
                (!use_join || saw_join) && saw_d,
                "{label}: resume did not reach the join and downstream node"
            );
            if !use_join && delegate {
                assert!(
                    errs.last()
                        .is_some_and(|e| is_kind(e, WorkflowErrorKind::MultipleTerminalOutputs)),
                    "{label}: delegated terminal output was not counted: {errs:?}"
                );
            } else {
                assert!(errs.is_empty(), "{label}: {errs:?}");
            }
        }
    }
}

// An output emitted before failure must not open the barrier on a later turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_runner_workflow_hitl_failed_sibling_retries() {
    for child in [false, true] {
        run_failed_sibling_retry(child).await;
    }
}

async fn run_failed_sibling_retry(child: bool) {
    const FAILURE: &str = "scripted node failure";
    let label = if child { "dynamic_child" } else { "direct" };
    let (b, d) = (
        join_resume_agent("b", "B", true),
        join_resume_agent("d", "D", false),
    );
    let (bn, dn) = (agent_node(&b), agent_node(&d));
    let paused = CancellationToken::new(); // Go `paused` channel; cancel() == close()
    let attempts = Arc::new(AtomicI32::new(0));
    let make_fn = || {
        let (paused, attempts) = (paused.clone(), attempts.clone());
        move |ctx: Context, _in: Value, emit: Emit| {
            let (paused, attempts) = (paused.clone(), attempts.clone());
            async move {
                if attempts.fetch_add(1, SeqCst) + 1 != 1 {
                    return Ok(json!("C-retry"));
                }
                let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
                ev.output = Some(json!("C"));
                emit(ev).await?;
                tokio::select! {
                    _ = paused.cancelled() => Err(AgentError::other(FAILURE)),
                    _ = ctx.cancellation_token().cancelled() => Err(AgentError::Cancelled),
                }
            }
        }
    };
    let mut c = dynamic_node::<Value, Value, _, _>("c", make_fn(), NodeConfig::default());
    if child {
        let q = dynamic_node::<Value, Value, _, _>("q", make_fn(), NodeConfig::default());
        c = dynamic_node::<Value, Value, _, _>(
            "c",
            move |ctx: Context, input: Value, emit: Emit| {
                let q = q.clone();
                async move {
                    resume_or_request_input(
                        &ctx,
                        &emit,
                        RequestInput {
                            interrupt_id: "c".into(),
                            message: "approve".into(),
                            ..Default::default()
                        },
                    )
                    .await?;
                    run_node(&ctx, q, input, RunNodeOptions::default()).await
                }
            },
            NodeConfig::default(),
        );
    }
    let join: Arc<dyn Node> = JoinNode::new("join");
    let r = new_join_resume_runner(
        vec![
            edge(&start(), &bn),
            edge(&start(), &c),
            edge(&bn, &join),
            edge(&c, &join),
            edge(&join, &dn),
        ],
        vec![b, d],
    );
    let (mut saw_failure, mut saw_pause) = (false, false);
    let mut s = run(&r, user_text("start"));
    // The failing node waits on `paused`; bound the drain so a missed pause fails, not hangs.
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(item) = s.next().await {
            match item {
                Err(e) => {
                    saw_failure = chain_has(&e, FAILURE)
                        || (child && is_kind(&e, WorkflowErrorKind::NodeFailed));
                }
                Ok(ev) => {
                    if ev.requested_input.is_some() && !saw_pause {
                        saw_pause = true;
                        paused.cancel();
                    }
                }
            }
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!("{label}: first turn did not finish within 10s (pause never released the node?)")
    });
    drop(s);
    if child {
        let (_, errs) = run_all(run(
            &r,
            resume_content("c", WORKFLOW_INPUT_FUNCTION_CALL_NAME, json!("approved C")),
        ))
        .await;
        if let Some(e) = errs.last() {
            saw_failure = is_kind(e, WorkflowErrorKind::NodeFailed);
        }
    }
    assert!(
        saw_failure && saw_pause,
        "{label}: fixture did not produce both the pause and sibling failure"
    );

    let events = drain(run(
        &r,
        resume_content("b", WORKFLOW_INPUT_FUNCTION_CALL_NAME, json!("approved")),
    ))
    .await;
    let mut saw_join = false;
    for ev in &events {
        if let Some(out @ Value::Object(_)) = &ev.output {
            saw_join = true;
            assert_eq!(
                out,
                &json!({ "b": "approved", "c": "C-retry" }),
                "{label}: join consumed output from the failed attempt"
            );
        }
    }
    let saw_d = events
        .iter()
        .any(|e| e.author == "d" && e.response.content.is_some());
    assert!(
        attempts.load(SeqCst) == 2 && saw_join && saw_d,
        "{label}: resume did not retry the failed predecessor before advancing (attempts={}, join={saw_join}, d={saw_d})",
        attempts.load(SeqCst)
    );
}

/// Go `errors.Is(err, sentinel)` for a sentinel created with `errors.New(text)`: some error in
/// the source chain displays exactly `text`.
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

#[tokio::test]
async fn test_runner_workflow_hitl_previously_resumed_sibling() {
    let (b, d) = (
        join_resume_agent("b", "B", true),
        join_resume_agent("d", "D", false),
    );
    let (bn, dn) = (agent_node(&b), agent_node(&d));
    let attempts = Arc::new(AtomicI32::new(0));
    let a2 = attempts.clone();
    let c = dynamic_node::<Value, Value, _, _>(
        "c",
        move |ctx: Context, _in: Value, emit: Emit| {
            a2.fetch_add(1, SeqCst);
            async move {
                resume_or_request_input(
                    &ctx,
                    &emit,
                    RequestInput {
                        interrupt_id: "c".into(),
                        message: "approve".into(),
                        ..Default::default()
                    },
                )
                .await
                .map(Option::unwrap_or_default)
            }
        },
        NodeConfig::default(),
    );
    let join: Arc<dyn Node> = JoinNode::new("join");
    let r = new_join_resume_runner(
        vec![
            edge(&start(), &bn),
            edge(&start(), &c),
            edge(&bn, &join),
            edge(&c, &join),
            edge(&join, &dn),
        ],
        vec![b, d],
    );
    drain(run(&r, user_text("start"))).await;

    for ev in drain(run(
        &r,
        resume_content("c", WORKFLOW_INPUT_FUNCTION_CALL_NAME, json!("approved C")),
    ))
    .await
    {
        assert_ne!(ev.author, "d", "join advanced before both approvals");
    }

    let events = drain(run(
        &r,
        resume_content("b", WORKFLOW_INPUT_FUNCTION_CALL_NAME, json!("approved")),
    ))
    .await;
    let mut saw_join = false;
    for ev in &events {
        if let Some(out @ Value::Object(_)) = &ev.output {
            saw_join = true;
            assert_eq!(
                out,
                &json!({ "b": "approved", "c": "approved C" }),
                "join lost the result from the earlier re-entry turn"
            );
        }
    }
    let saw_d = events
        .iter()
        .any(|e| e.author == "d" && e.response.content.is_some());
    assert!(
        saw_join && saw_d && attempts.load(SeqCst) == 2,
        "completed re-entry sibling was re-run or blocked the join (join={saw_join}, d={saw_d}, attempts={})",
        attempts.load(SeqCst)
    );

    let (_, errs) = run_all(run(
        &r,
        resume_content("c", WORKFLOW_INPUT_FUNCTION_CALL_NAME, json!("approved C")),
    ))
    .await;
    for e in &errs {
        assert!(
            is_kind(e, WorkflowErrorKind::NothingToResume),
            "duplicate resume: {e}"
        );
    }
    assert_eq!(
        attempts.load(SeqCst),
        2,
        "duplicate answer re-ran a completed re-entry sibling"
    );
}

#[tokio::test]
async fn test_runner_workflow_hitl_terminal_outputs_across_turns() {
    let (b, c) = (
        join_resume_agent("b", "B", true),
        join_resume_agent("c", "C", false),
    );
    let r = new_join_resume_runner(
        vec![
            edge(&start(), &agent_node(&b)),
            edge(&start(), &agent_node(&c)),
        ],
        vec![b, c],
    );
    drain(run(&r, user_text("start"))).await;
    let (_, errs) = run_all(run(
        &r,
        resume_content("b", WORKFLOW_INPUT_FUNCTION_CALL_NAME, json!("approved")),
    ))
    .await;
    assert!(
        errs.last()
            .is_some_and(|e| is_kind(e, WorkflowErrorKind::MultipleTerminalOutputs)),
        "resume did not enforce the single-terminal-output rule: {errs:?}"
    );
}

#[tokio::test]
async fn test_runner_workflow_hitl_agent_join_resume() {
    for second_wait in [false, true] {
        run_agent_join_resume(second_wait).await;
    }
}

async fn run_agent_join_resume(second_wait: bool) {
    let label = if second_wait {
        "two_waiting_siblings"
    } else {
        "completed_sibling"
    };
    let b = join_resume_agent("b", "B", true);
    let c = join_resume_agent("c", "C", second_wait);
    let d = join_resume_agent("d", "D", false);
    let (b_node, c_node, d_node) = (agent_node(&b), agent_node(&c), agent_node(&d));
    let join: Arc<dyn Node> = JoinNode::new("join");
    let root = workflow_agent::new(WorkflowAgentConfig {
        name: "root".into(),
        sub_agents: vec![b, c, d],
        edges: vec![
            edge(&start(), &b_node),
            edge(&start(), &c_node),
            edge(&b_node, &join),
            edge(&c_node, &join),
            edge(&join, &d_node),
        ],
        ..Default::default()
    })
    .expect("workflowagent.New()");
    let r = new_runner(root);

    for ev in drain(run(&r, user_text("start"))).await {
        assert_ne!(ev.author, "d", "{label}: D executed before approval");
    }
    let mut msg = function_response_content(
        "b",
        WORKFLOW_INPUT_FUNCTION_CALL_NAME,
        json!({"response": "approved"}),
    );
    let mut want_c = "C";
    if second_wait {
        for ev in drain(run(&r, msg)).await {
            assert!(
                ev.author != "d" && ev.output.is_none(),
                "{label}: join advanced while C still waited"
            );
        }
        msg = resume_content("c", WORKFLOW_INPUT_FUNCTION_CALL_NAME, json!("approved C"));
        want_c = "approved C";
    }
    let (mut saw_d, mut saw_join) = (false, false);
    for ev in drain(run(&r, msg)).await {
        assert_ne!(ev.author, "c", "{label}: completed C ran again");
        if let Some(got @ Value::Object(_)) = &ev.output {
            saw_join = true;
            assert_eq!(
                got,
                &json!({"b": "approved", "c": want_c}),
                "{label}: join output did not match the completed predecessors"
            );
        }
        if ev.author == "d" {
            saw_d = true;
        }
    }
    assert!(saw_join, "{label}: join did not emit predecessor outputs");
    assert!(
        saw_d,
        "{label}: D never executed after approval; completed predecessor must survive rehydration"
    );
}
