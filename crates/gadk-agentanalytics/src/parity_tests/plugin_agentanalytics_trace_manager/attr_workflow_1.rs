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

//! Goroutine-attribution ports (7b-3b) of ADK-Go `plugin/agentanalytics/trace_manager_test.go`:
//! group D, first half (Go line order): workflow scheduler nodes, ParallelWorker items and
//! function nodes.
//! Shared helpers: [`attr_support`](super::attr_support).
//!
//! Go's custom-agent iterators (`func(yield func(*session.Event, error) bool)`) are [`go_iter`]
//! streams: the body runs inside the stream's own poll (no task of its own), and each
//! [`Yielder::send`] returns only when the consumer polls the stream again, as Go's `yield`
//! returns after the consumer's loop body; a consumer that stops drops the body (Go's `yield`
//! returning false, followed by a `return`).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, HashMap};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::{Context as TaskContext, Poll};
use std::time::Duration;

use futures::future::BoxFuture;
use futures::{FutureExt, Stream, StreamExt};
use gadk_agent::{
    AfterAgentCallback, Agent, AgentConfig, AgentError, BeforeAgentCallback, InvocationContext,
    RunConfig, StreamingMode,
};
use gadk_engine::llm::LlmAgentConfig;
use gadk_engine::workflow::{
    Emit, Node, NodeConfig, ParallelWorker, RetryConfig, RunNodeOptions, Workflow, WorkflowOptions,
    chain, dynamic_node, function_node, run_node,
};
use gadk_engine::workflow_agent::{self, WorkflowAgentConfig};
use gadk_engine::{parallel_agent, sequential_agent};
use gadk_model::LlmResponse;
use gadk_session::Event;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use super::attr_support::{
    CloseOnDrop, ERR_SIBLING_FAILED, Gate, agent_node, blocking_llm, ctx_err,
    flaky_on_first_pass_llm, gated_flaky_llm, go, new_parallel_agent, start, then_wait_for_cancel,
    wait_then_stream_partial_llm, workflow_agent,
};
use super::runner_support::*;
use crate::event_types::*;
use crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::*;
use crate::row::{Row, RowExt};
use crate::trace::testkit::sequential;

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

type Item = Result<Event, AgentError>;

/// The `yield` of a [`go_iter`] body.
#[derive(Clone)]
struct Yielder(Arc<Mutex<Option<Item>>>);

impl Yielder {
    /// Go `yield(ev, err)`: hands `item` to the consumer and returns once the consumer polls
    /// the stream again (a consumer that stops drops the body here).
    async fn send(&self, item: Item) {
        *self.0.lock() = Some(item);
        let slot = self.0.clone();
        let mut first = true;
        futures::future::poll_fn(move |_| {
            if first {
                first = false;
                return Poll::Pending;
            }
            if slot.lock().is_some() {
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        })
        .await;
    }
}

/// A stream whose items are what `body` yields (Go's iterator function).
struct GoIter {
    slot: Arc<Mutex<Option<Item>>>,
    body: Option<BoxFuture<'static, ()>>,
}

impl Stream for GoIter {
    type Item = Item;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Item>> {
        let Some(body) = self.body.as_mut() else {
            return Poll::Ready(None);
        };
        match body.as_mut().poll(cx) {
            Poll::Ready(()) => {
                self.body = None;
                Poll::Ready(self.slot.lock().take())
            }
            Poll::Pending => match self.slot.lock().take() {
                Some(item) => Poll::Ready(Some(item)),
                None => Poll::Pending,
            },
        }
    }
}

/// Go `func(yield func(*session.Event, error) bool) { … }` as an event stream.
fn go_iter<F, Fut>(body: F) -> gadk_agent::EventStream
where
    F: FnOnce(Yielder) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let slot = Arc::new(Mutex::new(None));
    GoIter {
        body: Some(body(Yielder(slot.clone())).boxed()),
        slot,
    }
    .boxed()
}

/// Go `errors.Is(err, target)` for a target known by its text: `err` or an error it wraps reads
/// `want`. `AgentError::Other` displays its inner error without reporting it as its `source`,
/// so the walk steps into it (Go's `%w` chain).
fn is_error(err: &AgentError, want: &str) -> bool {
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(e) = cur {
        if e.to_string() == want {
            return true;
        }
        cur = match e.downcast_ref::<AgentError>() {
            Some(AgentError::Other(inner)) => Some(&**inner),
            _ => e.source(),
        };
    }
    false
}

/// A model-authored event of `ctx` by `author` on the context's branch (Go
/// `session.NewEvent(ctx, ctx.InvocationID())` with `Author`, `Branch` and `LLMResponse` set).
fn event_of(ctx: &InvocationContext, author: &str, response: LlmResponse) -> Event {
    let mut ev = new_event(ctx);
    ev.author = author.into();
    ev.branch = ctx.branch();
    ev.response = response;
    ev
}

/// Go `model.LLMResponse{Content: textContent("model", text)}` with `partial` and
/// `turn_complete`.
fn text_response(text: &str, partial: bool, turn_complete: bool) -> LlmResponse {
    LlmResponse {
        content: Some(text_content("model", text)),
        partial,
        turn_complete,
        ..Default::default()
    }
}

/// Go `&model.LLMResponse{Content: textContent("model", text)}`.
fn model_text(text: &str) -> Result<LlmResponse, gadk_model::ModelError> {
    Ok(text_response(text, false, false))
}

/// `agent.New(agent.Config{Name, SubAgents, BeforeAgentCallbacks, AfterAgentCallbacks, Run})`.
fn custom_agent_with(
    name: &str,
    before: Vec<BeforeAgentCallback>,
    after: Vec<AfterAgentCallback>,
    run: impl Fn(InvocationContext) -> gadk_agent::EventStream + Send + Sync + 'static,
) -> Arc<dyn Agent> {
    gadk_agent::new_agent(AgentConfig {
        name: name.into(),
        before_agent_callbacks: before,
        after_agent_callbacks: after,
        run: Some(Arc::new(run)),
        ..Default::default()
    })
    .expect("custom agent")
}

/// A callback answering `text` (Go `func(agent.Context) (*genai.Content, error) { return
/// textContent("model", text), nil }`).
fn answering(text: &str) -> BeforeAgentCallback {
    answering_before_agent(text_content("model", text))
}

/// `workflow.NewAgentNode(a, cfg)`.
fn agent_node_with(a: Arc<dyn Agent>, cfg: NodeConfig) -> Arc<dyn Node> {
    gadk_engine::workflow::AgentNode::new(a, cfg).expect("agent node")
}

/// Go `&workflow.RetryConfig{MaxAttempts: 2, InitialDelay: delay, MaxDelay: delay,
/// BackoffFactor: 1.0}`.
fn retry_twice(delay: Duration) -> NodeConfig {
    NodeConfig {
        retry_config: Some(RetryConfig {
            max_attempts: 2,
            initial_delay: delay,
            max_delay: delay,
            backoff_factor: 1.0,
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// `workflowagent.New(workflowagent.Config{Name: name, Edges: [{Start, node}…]})`: the nodes
/// all start from `workflow.Start`.
fn fan_out_workflow(name: &str, nodes: Vec<Arc<dyn Node>>) -> Arc<dyn Agent> {
    workflow_agent(
        name,
        nodes
            .into_iter()
            .map(|to| gadk_engine::workflow::Edge {
                from: start(),
                to,
                route: None,
            })
            .collect(),
    )
}

/// Go `streamingPartialThenBlockLLM{chunkText, finalText}` (model name
/// `streaming-partial-llm`): a partial `chunk`, then the final `final_answer` (turn complete).
fn streaming_partial_then_block_llm(chunk: &str, final_answer: &str) -> Arc<dyn gadk_model::Llm> {
    let (chunk, final_answer) = (chunk.to_owned(), final_answer.to_owned());
    fn_llm("streaming-partial-llm", move |_, _| {
        futures::stream::iter([partial_text(&chunk), final_text(&final_answer)]).boxed()
    })
}

/// Go `&blockingUntilContextDoneLLM{entered}` (model name `blocking-until-context-done-llm`):
/// closes `entered` once and waits for its context (see `attr_support`'s module docs).
fn blocking_until_context_done_llm(entered: Gate) -> Arc<dyn gadk_model::Llm> {
    fn_llm("blocking-until-context-done-llm", move |_, _| {
        entered.close();
        then_wait_for_cancel(vec![])
    })
}

/// Go `newShortcutSequenceGraph(t, next, registerSequence)`.
fn new_shortcut_sequence_graph(next: Arc<dyn Agent>, register_sequence: bool) -> Arc<dyn Agent> {
    let shortcut = llm_agent(LlmAgentConfig {
        name: "shortcut".into(),
        model: Some(never_sent()),
        before_agent_callbacks: vec![answering("answered by callback")],
        ..Default::default()
    });
    let seq = sequential("sequence", vec![shortcut, next]);
    let node = agent_node(seq.clone());
    workflow_agent::new(WorkflowAgentConfig {
        name: "root".into(),
        edges: chain(&[start(), node]),
        sub_agents: if register_sequence { vec![seq] } else { vec![] },
        ..Default::default()
    })
    .expect("workflow agent")
}

/// Go `newStreamingAuthor(t, child, withContent)`.
fn new_streaming_author(child: Arc<dyn Agent>, with_content: bool) -> Arc<dyn Agent> {
    let run_child = child.clone();
    custom_agent("author", vec![child], move |ctx| {
        let child = run_child.clone();
        go_iter(move |y| async move {
            let mut partial = event_of(
                &ctx,
                "author",
                LlmResponse {
                    partial: true,
                    ..Default::default()
                },
            );
            if with_content {
                partial.response.content = Some(text_content("model", "progress..."));
            }
            y.send(Ok(partial)).await;
            let mut events = child.run(ctx);
            while let Some(item) = events.next().await {
                let failed = item.is_err();
                y.send(item).await;
                if failed {
                    return;
                }
            }
        })
    })
}

/// Go `newSignalingCacheHitAgent(t, started)`.
fn new_signaling_cache_hit_agent(started: Gate) -> Arc<dyn Agent> {
    let cb: BeforeAgentCallback = Arc::new(move |_ctx| {
        started.close();
        async { Ok(Some(text_content("model", "cache hit"))) }.boxed()
    });
    custom_agent_with("cached_child", vec![cb], vec![], |_| {
        futures::stream::empty().boxed()
    })
}

/// Go `newBranchA(t, childStarted, done, withEvent)`.
fn new_branch_a(child_started: Gate, done: Gate, with_event: bool) -> Arc<dyn Agent> {
    let after: AfterAgentCallback = Arc::new(move |_ctx| {
        done.close();
        async { Ok(None) }.boxed()
    });
    custom_agent_with("branch_a", vec![], vec![after], move |ctx| {
        let child_started = child_started.clone();
        go_iter(move |y| async move {
            if tokio::time::timeout(Duration::from_secs(10), child_started.wait())
                .await
                .is_err()
            {
                y.send(Err(AgentError::other("cached_child did not start")))
                    .await;
                return;
            }
            if with_event {
                let mut state = event_of(&ctx, "branch_a", LlmResponse::default());
                state.actions.state_delta =
                    Some(json!({"seen": true}).as_object().cloned().unwrap());
                y.send(Ok(state)).await;
            }
        })
    })
}

// ---------------------------------------------------------------------------------------------
// Ports
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn test_trace_manager_runner_concurrent_workflow_run_node_same_agent() {
    let tp = setup_test_plugin().await;
    let b_llm = blocking_llm(2);
    let (mut entered, release) = (b_llm.entered, b_llm.release);
    go(async move {
        entered.recv().await;
        entered.recv().await;
        release.close();
    });

    let mut worker_nodes = Vec::new();
    let mut primary_worker = None;
    for _ in 0..2 {
        let w = llm_agent(LlmAgentConfig {
            name: "worker".into(),
            description: "Concurrent worker".into(),
            model: Some(b_llm.model.clone()),
            instruction: "Do the work.".into(),
            ..Default::default()
        });
        primary_worker.get_or_insert_with(|| w.clone());
        worker_nodes.push(agent_node(w));
    }

    let orch = dynamic_node::<Value, String, _, _>(
        "orch",
        move |ctx, _input: Value, _emit: Emit| {
            let worker_nodes = worker_nodes.clone();
            async move {
                // Go: one goroutine per RunNode call, joined with a WaitGroup.
                let runs: Vec<_> = worker_nodes
                    .into_iter()
                    .enumerate()
                    .map(|(idx, node)| {
                        let ctx = ctx.clone();
                        go(async move {
                            run_node(
                                &ctx,
                                node,
                                json!(format!("task-{idx}")),
                                RunNodeOptions::default(),
                            )
                            .await
                        })
                    })
                    .collect();
                for run in futures::future::join_all(runs).await {
                    run.expect("RunNode goroutine")?;
                }
                Ok("all workers done".to_string())
            }
        },
        NodeConfig::default(),
    );
    let wf = Workflow::new("wf", chain(&[start(), orch]), WorkflowOptions::default())
        .expect("workflow.New(wf)");
    let root = gadk_agent::new_agent(AgentConfig {
        name: "root".into(),
        sub_agents: vec![primary_worker.unwrap()],
        run: Some(Arc::new(move |ic| wf.run(ic))),
        ..Default::default()
    })
    .unwrap();

    let r = new_test_runner(root, vec![tp.plugin.clone()]);
    let (_, errs) = run_until(
        &r,
        "user-1",
        "session-1",
        "fan out",
        RunConfig::default(),
        |_| false,
    )
    .await;
    assert!(errs.is_empty(), "r.Run: {errs:?}");

    let rows = tp
        .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;
    let agent_starts = rows_of_type(&rows, EVENT_AGENT_STARTING);
    assert_eq!(
        agent_starts.len(),
        3,
        "AGENT_STARTING rows, want 3 (root + 2 workers); event types: {:?}",
        event_types(&rows)
    );
    let root_span = place_of(&agent_starts[0]);
    let w1_span = place_of(&agent_starts[1]);
    let w2_span = place_of(&agent_starts[2]);
    assert!(
        w1_span.parent == root_span.span
            && w2_span.parent == root_span.span
            && w1_span.span != w2_span.span,
        "worker AGENT_STARTING places = ({w1_span:?}, {w2_span:?}), want two distinct spans both parented to root span {:?}",
        root_span.span
    );
    let worker_spans = [w1_span.span.clone(), w2_span.span.clone()];

    let llm_reqs = rows_of_type(&rows, EVENT_LLM_REQUEST);
    let llm_resps = rows_of_type(&rows, EVENT_LLM_RESPONSE);
    assert!(
        llm_reqs.len() == 2 && llm_resps.len() == 2,
        "got {} LLM_REQUEST and {} LLM_RESPONSE rows, want 2 of each",
        llm_reqs.len(),
        llm_resps.len()
    );
    let mut req_by_span = HashMap::new();
    let mut req_parents = std::collections::HashSet::new();
    for req in &llm_reqs {
        let pl = place_of(req);
        assert!(
            worker_spans.contains(&pl.parent),
            "LLM_REQUEST place {pl:?} is not parented to either worker span {worker_spans:?}"
        );
        req_parents.insert(pl.parent.clone());
        req_by_span.insert(pl.span.clone(), pl);
    }
    assert!(
        req_by_span.len() == 2 && req_parents.len() == 2,
        "LLM_REQUEST spans = {req_by_span:?} (parents {req_parents:?}), want 2 distinct spans across both worker activations"
    );
    for resp in &llm_resps {
        let pl = place_of(resp);
        assert_eq!(
            req_by_span.get(&pl.span),
            Some(&pl),
            "LLM_RESPONSE place = {pl:?}, want matching LLM_REQUEST in {req_by_span:?}"
        );
        must_latency(resp);
    }

    let agent_ends = rows_of_type(&rows, EVENT_AGENT_COMPLETED);
    assert_eq!(agent_ends.len(), 3, "AGENT_COMPLETED rows, want 3");
    let mut completed_spans = HashMap::new();
    for row in &agent_ends {
        let pl = place_of(row);
        completed_spans.insert(pl.span.clone(), pl);
        must_latency(row);
    }
    for want in [root_span, w1_span, w2_span] {
        assert_eq!(
            completed_spans.get(&want.span),
            Some(&want),
            "missing or mismatched AGENT_COMPLETED for {want:?}; got {completed_spans:?}"
        );
    }
}

#[tokio::test]
async fn test_runner_graph_sequential_sibling_after_shortcut() {
    for (name, register_sequence) in [
        ("sequence only as a graph node", false),
        ("sequence also in the SubAgents of root", true),
    ] {
        let tp = setup_test_plugin().await;
        let next = llm_agent(LlmAgentConfig {
            name: "next".into(),
            model: Some(scripted_llm(vec![model_text("next answer")])),
            ..Default::default()
        });
        let r = new_test_runner(
            new_shortcut_sequence_graph(next, register_sequence),
            vec![tp.plugin.clone()],
        );
        let (_, errs) = run_until(&r, "u1", "s1", "hi", RunConfig::default(), |_| false).await;
        assert!(errs.is_empty(), "{name}: r.Run error: {errs:?}");

        let rows = tp.collect_rows(until_invocation_end).await;
        let starts = rows_of_type(&rows, EVENT_AGENT_STARTING);
        assert_eq!(
            starts.len(),
            4,
            "{name}: AGENT_STARTING rows, want 4; event types: {:?}",
            event_types(&rows)
        );
        let places: BTreeMap<String, TracePlace> =
            starts.iter().map(|r| (agent_of(r), place_of(r))).collect();
        assert_eq!(
            places.keys().map(String::as_str).collect::<Vec<_>>(),
            ["next", "root", "sequence", "shortcut"],
            "{name}: AGENT_STARTING agents"
        );
        assert_eq!(
            places["sequence"].parent, places["root"].span,
            "{name}: sequence parent_span_id, want the span of root"
        );
        assert_eq!(
            places["shortcut"].parent, places["sequence"].span,
            "{name}: shortcut parent_span_id, want the span of sequence"
        );
        assert_eq!(
            places["next"].parent, places["sequence"].span,
            "{name}: next parent_span_id, want the span of sequence, not that of shortcut {:?}",
            places["shortcut"].span
        );
        require_invocation_completed_ok(&rows);
    }
}

#[tokio::test]
async fn test_runner_graph_sequential_failure_does_not_blame_shortcut() {
    let tp = setup_test_plugin().await;
    let model_err = "model failure 500";
    let next = llm_agent(LlmAgentConfig {
        name: "next".into(),
        model: Some(failing_llm(model_err)),
        ..Default::default()
    });
    let r = new_test_runner(
        new_shortcut_sequence_graph(next, false),
        vec![tp.plugin.clone()],
    );
    let (_, errs) = run_until_error(&r, "hi").await;
    let run_err = errs.first().expect("r.Run error");
    assert!(
        is_error(run_err, model_err),
        "r.Run error = {run_err}, want {model_err}"
    );

    let rows = tp.collect_rows(until_invocation_end).await;
    assert!(
        rows_of_type(&rows, EVENT_INVOCATION_COMPLETED).is_empty(),
        "got INVOCATION_COMPLETED after a model failure, want INVOCATION_ERROR; event types: {:?}",
        event_types(&rows)
    );
    assert_eq!(
        agent_error_counts(&rows),
        BTreeMap::from([
            ("next".to_string(), 1),
            ("root".to_string(), 1),
            ("sequence".to_string(), 1)
        ]),
        "AGENT_ERROR rows per agent"
    );
    let inv_row = only_row_of_type(&rows, EVENT_INVOCATION_ERROR);
    assert_eq!(
        str_col(&inv_row, "error_message"),
        model_err,
        "INVOCATION_ERROR error_message"
    );
}

/// Runs `root` in SSE mode until the first partial event and returns the rows.
async fn stop_at_first_partial(tp: &TestPlugin, root: Arc<dyn Agent>) -> Vec<Row> {
    let r = new_test_runner(root, vec![tp.plugin.clone()]);
    let mut saw_partial = false;
    let (_, errs) = run_until(
        &r,
        "u1",
        "s1",
        "stream please",
        RunConfig {
            streaming_mode: StreamingMode::Sse,
            ..Default::default()
        },
        |item| {
            saw_partial = matches!(item, Ok(ev) if ev.response.partial);
            item.is_err() || saw_partial
        },
    )
    .await;
    assert!(errs.is_empty(), "r.Run error: {errs:?}");
    assert!(
        saw_partial,
        "did not observe a partial event in StreamingModeSSE"
    );
    tp.collect_rows(until_invocation_end).await
}

/// Go `newStreamingBot(t, name)`.
fn new_streaming_bot(name: &str) -> Arc<dyn Agent> {
    llm_agent(LlmAgentConfig {
        name: name.into(),
        model: Some(streaming_partial_then_block_llm("hello ", "hello world")),
        ..Default::default()
    })
}

/// The `bot` of Go's parallel and workflow subtests: waits for `ready`, then streams.
fn wait_then_stream_bot(ready: Gate) -> Arc<dyn Agent> {
    llm_agent(LlmAgentConfig {
        name: "bot".into(),
        model: Some(wait_then_stream_partial_llm(ready, "hello ", "hello world")),
        ..Default::default()
    })
}

#[tokio::test]
async fn test_runner_stop_reading_mid_stream_is_not_an_error() {
    // single streaming agent
    let tp = setup_test_plugin().await;
    let rows = stop_at_first_partial(&tp, new_streaming_bot("bot")).await;
    require_invocation_completed_ok(&rows);

    // sequential root
    let tp = setup_test_plugin().await;
    let seq = sequential(
        "seq_root",
        vec![new_streaming_bot("bot"), new_streaming_bot("step2")],
    );
    let rows = stop_at_first_partial(&tp, seq).await;
    require_invocation_completed_ok(&rows);

    // loop root
    let tp = setup_test_plugin().await;
    let loop_root = gadk_engine::loop_agent::new(gadk_engine::loop_agent::LoopAgentConfig {
        agent: AgentConfig {
            name: "loop_root".into(),
            sub_agents: vec![new_streaming_bot("bot")],
            ..Default::default()
        },
        max_iterations: 2,
    })
    .unwrap();
    let rows = stop_at_first_partial(&tp, loop_root).await;
    require_invocation_completed_ok(&rows);

    // parallel root with sibling model call in flight
    let tp = setup_test_plugin().await;
    let slow_entered = Gate::new();
    let slow = llm_agent(LlmAgentConfig {
        name: "slow".into(),
        model: Some(blocking_until_context_done_llm(slow_entered.clone())),
        ..Default::default()
    });
    let par = new_parallel_agent("par_root", vec![wait_then_stream_bot(slow_entered), slow]);
    let rows = stop_at_first_partial(&tp, par).await;
    require_invocation_completed_ok(&rows);

    // parallel root with sibling model call returning ctx.Err on cancel
    let tp = setup_test_plugin().await;
    let (slow_model, slow_entered) = super::attr_support::context_canceled_on_done_llm();
    let slow = llm_agent(LlmAgentConfig {
        name: "slow".into(),
        model: Some(slow_model),
        ..Default::default()
    });
    let par = new_parallel_agent("par_root", vec![wait_then_stream_bot(slow_entered), slow]);
    let rows = stop_at_first_partial(&tp, par).await;
    require_invocation_completed_ok(&rows);
    assert_eq!(
        rows_of_type(&rows, EVENT_LLM_ERROR).len(),
        1,
        "LLM_ERROR rows, want 1 (from slow branch context.Canceled)"
    );

    // workflow root with sibling node in flight
    let tp = setup_test_plugin().await;
    let slow_entered = Gate::new();
    let slow = llm_agent(LlmAgentConfig {
        name: "slow".into(),
        model: Some(blocking_until_context_done_llm(slow_entered.clone())),
        ..Default::default()
    });
    let wf = fan_out_workflow(
        "wf_root",
        vec![
            agent_node(wait_then_stream_bot(slow_entered)),
            agent_node(slow),
        ],
    );
    let rows = stop_at_first_partial(&tp, wf).await;
    require_invocation_completed_ok(&rows);
}

#[tokio::test]
async fn test_runner_concurrent_graph_parents_same_child_names() {
    let tp = setup_test_plugin().await;
    let gates_ready = Gate::new();
    let gate_arrivals = Arc::new(AtomicUsize::new(0));

    let new_flow = |flow_name: &str| -> Arc<dyn Agent> {
        let (ready, arrivals, shortcut) = (
            gates_ready.clone(),
            gate_arrivals.clone(),
            format!("{flow_name} gate shortcut"),
        );
        let wait: BeforeAgentCallback = Arc::new(move |ctx: gadk_agent::Context| {
            let (ready, shortcut) = (ready.clone(), shortcut.clone());
            // Go gateArrivals.Done(); a goroutine closes gatesReady after both arrivals.
            if arrivals.fetch_add(1, Ordering::SeqCst) + 1 == 2 {
                ready.close();
            }
            async move {
                tokio::select! {
                    () = ready.wait() => {}
                    () = ctx.cancellation_token().cancelled() => return Err(ctx_err(&ctx)),
                }
                Ok(Some(text_content("model", &shortcut)))
            }
            .boxed()
        });
        let gate = llm_agent(LlmAgentConfig {
            name: "gate".into(),
            model: Some(scripted_llm(vec![model_text("unused")])),
            before_agent_callbacks: vec![wait],
            ..Default::default()
        });
        let step = llm_agent(LlmAgentConfig {
            name: "step".into(),
            model: Some(scripted_llm(vec![final_text(&format!(
                "{flow_name} step done"
            ))])),
            ..Default::default()
        });
        sequential(flow_name, vec![gate, step])
    };

    let root = fan_out_workflow(
        "root",
        vec![
            agent_node(new_flow("flow_a")),
            agent_node(new_flow("flow_b")),
        ],
    );
    let r = new_test_runner(root, vec![tp.plugin.clone()]);
    let (_, errs) = run_until(&r, "u1", "s1", "go", RunConfig::default(), |_| false).await;
    assert!(errs.is_empty(), "r.Run error: {errs:?}");

    let rows = tp.collect_rows(until_invocation_end).await;
    require_invocation_completed_ok(&rows);

    let (mut root_start, mut flow_a_start, mut flow_b_start) = Default::default();
    let (mut gate_parents, mut step_start_parents) = (Vec::new(), Vec::new());
    let mut step_start_by_span = HashMap::new();
    for row in rows_of_type(&rows, EVENT_AGENT_STARTING) {
        let pl = place_of(&row);
        match agent_of(&row).as_str() {
            "root" => root_start = pl,
            "flow_a" => flow_a_start = pl,
            "flow_b" => flow_b_start = pl,
            "gate" => gate_parents.push(pl.parent),
            "step" => {
                step_start_parents.push(pl.parent.clone());
                step_start_by_span.insert(pl.span.clone(), pl);
            }
            _ => {}
        }
    }
    let (root_start, flow_a_start, flow_b_start): (TracePlace, TracePlace, TracePlace) =
        (root_start, flow_a_start, flow_b_start);
    assert!(
        !root_start.span.is_empty()
            && !flow_a_start.span.is_empty()
            && !flow_b_start.span.is_empty(),
        "missing spans: root={:?} flow_a={:?} flow_b={:?}",
        root_start.span,
        flow_a_start.span,
        flow_b_start.span
    );
    let mut want_flow_spans = vec![flow_a_start.span.clone(), flow_b_start.span.clone()];
    want_flow_spans.sort();
    gate_parents.sort();
    step_start_parents.sort();
    assert_eq!(
        gate_parents, want_flow_spans,
        "gate AGENT_STARTING parent_span_id"
    );
    assert_eq!(
        step_start_parents, want_flow_spans,
        "step AGENT_STARTING parent_span_id"
    );

    let mut step_complete_parents = Vec::new();
    let (mut flow_a_complete, mut flow_b_complete) = (TracePlace::default(), TracePlace::default());
    for row in rows_of_type(&rows, EVENT_AGENT_COMPLETED) {
        let pl = place_of(&row);
        match agent_of(&row).as_str() {
            "flow_a" => {
                flow_a_complete = pl;
                must_latency(&row);
            }
            "flow_b" => {
                flow_b_complete = pl;
                must_latency(&row);
            }
            "step" => {
                must_latency(&row);
                let start_pl = step_start_by_span.get(&pl.span).unwrap_or_else(|| {
                    panic!(
                        "step AGENT_COMPLETED span_id {:?} does not match any step AGENT_STARTING span",
                        pl.span
                    )
                });
                assert_eq!(&pl, start_pl, "step AGENT_COMPLETED place");
                step_complete_parents.push(pl.parent);
            }
            _ => {}
        }
    }
    assert!(
        flow_a_complete == flow_a_start && flow_a_start.parent == root_start.span,
        "flow_a AGENT_COMPLETED = {flow_a_complete:?}, start = {flow_a_start:?} (want parent {:?})",
        root_start.span
    );
    assert!(
        flow_b_complete == flow_b_start && flow_b_start.parent == root_start.span,
        "flow_b AGENT_COMPLETED = {flow_b_complete:?}, start = {flow_b_start:?} (want parent {:?})",
        root_start.span
    );
    step_complete_parents.sort();
    assert_eq!(
        step_complete_parents, want_flow_spans,
        "step AGENT_COMPLETED parent_span_id"
    );
}

#[tokio::test]
async fn test_runner_internally_recovered_errors_single_turn_tool_and_workflow_retry_emits_completed_terminal_rows()
 {
    // single_turn sub-agent tool fails and parent recovers
    let tp = setup_test_plugin().await;
    let helper = llm_agent(LlmAgentConfig {
        name: "helper".into(),
        description: "Runs a single-turn helper task.".into(),
        mode: gadk_agent::Mode::SingleTurn,
        model: Some(failing_llm("helper model unavailable")),
        ..Default::default()
    });
    let coordinator = llm_agent(LlmAgentConfig {
        name: "coordinator".into(),
        mode: gadk_agent::Mode::Chat,
        sub_agents: vec![helper],
        model: Some(scripted_llm(vec![
            calls(vec![function_call_part_id(
                "call-helper-1",
                "helper",
                json!({"request": "try helper"}),
            )]),
            final_text("answered directly after helper failed"),
        ])),
        ..Default::default()
    });
    let r = new_test_runner(coordinator, vec![tp.plugin.clone()]);
    let (_, errs) = run_until(&r, "u1", "s1", "help me", RunConfig::default(), |_| false).await;
    assert!(errs.is_empty(), "r.Run yielded unexpected error: {errs:?}");
    let rows = tp.collect_rows(until_invocation_end).await;
    require_completed_with_handled_agent_error(&rows, "helper", "helper model unavailable");
    assert_eq!(
        rows_of_type(&rows, EVENT_LLM_ERROR).len(),
        1,
        "LLM_ERROR rows"
    );
    assert_eq!(
        rows_of_type(&rows, EVENT_TOOL_ERROR).len(),
        1,
        "TOOL_ERROR rows"
    );
    let agent_err_idx = rows
        .iter()
        .position(|r| str_col(r, "event_type") == EVENT_AGENT_ERROR && agent_of(r) == "helper");
    let tool_err_idx = rows
        .iter()
        .position(|r| str_col(r, "event_type") == EVENT_TOOL_ERROR);
    assert!(
        matches!((agent_err_idx, tool_err_idx), (Some(a), Some(t)) if a < t),
        "AGENT_ERROR(helper) index = {agent_err_idx:?}, TOOL_ERROR index = {tool_err_idx:?}; want AGENT_ERROR < TOOL_ERROR in {:?}",
        event_types(&rows)
    );

    // workflow node RetryConfig recovers on retry attempt
    let tp = setup_test_plugin().await;
    let flaky = llm_agent(LlmAgentConfig {
        name: "flaky".into(),
        mode: gadk_agent::Mode::SingleTurn,
        model: Some(flaky_on_first_pass_llm(
            "transient 503",
            "succeeded on attempt 2",
        )),
        ..Default::default()
    });
    let node = agent_node_with(flaky, retry_twice(Duration::from_millis(1)));
    let r = new_test_runner(
        fan_out_workflow("wf_root", vec![node]),
        vec![tp.plugin.clone()],
    );
    let (_, errs) = run_until(&r, "u1", "s1", "run workflow", RunConfig::default(), |_| {
        false
    })
    .await;
    assert!(errs.is_empty(), "r.Run yielded unexpected error: {errs:?}");
    let rows = tp.collect_rows(until_invocation_end).await;
    require_completed_with_handled_agent_error(&rows, "flaky", "transient 503");
    assert_eq!(
        rows_of_type(&rows, EVENT_LLM_ERROR).len(),
        1,
        "LLM_ERROR rows, want 1 (from failed attempt 1)"
    );
    assert_eq!(
        rows_of_type(&rows, EVENT_LLM_RESPONSE).len(),
        1,
        "LLM_RESPONSE rows, want 1 (from successful retry)"
    );
}

#[tokio::test]
async fn test_runner_retried_composite_node_skips_cached_child_agent_error() {
    let tp = setup_test_plugin().await;
    let cached = llm_agent(LlmAgentConfig {
        name: "cached".into(),
        model: Some(scripted_llm(vec![])),
        before_agent_callbacks: vec![answering("cached answer")],
        ..Default::default()
    });
    let flaky = llm_agent(LlmAgentConfig {
        name: "flaky".into(),
        model: Some(flaky_on_first_pass_llm(
            "flaky backend unavailable",
            "succeeded on attempt 2",
        )),
        ..Default::default()
    });
    let pipeline = sequential("pipeline", vec![cached, flaky]);
    let node = agent_node_with(pipeline, retry_twice(Duration::from_millis(1)));
    let r = new_test_runner(
        fan_out_workflow("wf_root", vec![node]),
        vec![tp.plugin.clone()],
    );
    let (_, errs) = run_until(
        &r,
        "u1",
        "s1",
        "run the pipeline",
        RunConfig::default(),
        |_| false,
    )
    .await;
    assert!(errs.is_empty(), "r.Run yielded unexpected error: {errs:?}");

    let rows = tp.collect_rows(until_invocation_end).await;
    assert!(
        rows_of_type(&rows, EVENT_INVOCATION_ERROR).is_empty(),
        "got INVOCATION_ERROR, want INVOCATION_COMPLETED; event types: {:?}",
        event_types(&rows)
    );
    only_row_of_type(&rows, EVENT_INVOCATION_COMPLETED);
    assert_eq!(
        agent_error_counts(&rows),
        BTreeMap::from([("flaky".to_string(), 1), ("pipeline".to_string(), 1)]),
        "AGENT_ERROR rows per agent; event types: {:?}",
        event_types(&rows)
    );
    for row in rows_of_type(&rows, EVENT_AGENT_ERROR) {
        let msg = str_col(&row, "error_message");
        assert!(
            msg.contains("flaky backend unavailable"),
            "AGENT_ERROR of {:?} error_message = {msg:?}, want it to contain the failure of flaky",
            agent_of(&row)
        );
    }
}

#[tokio::test]
async fn test_runner_purged_retry_attempt_timestamp_precedes_next_attempt() {
    let tp = setup_test_plugin().await;
    let flaky = llm_agent(LlmAgentConfig {
        name: "flaky".into(),
        model: Some(flaky_on_first_pass_llm(
            "flaky backend unavailable",
            "succeeded on attempt 2",
        )),
        ..Default::default()
    });
    let delay = Duration::from_millis(25);
    let node = agent_node_with(flaky, retry_twice(delay));
    let r = new_test_runner(
        fan_out_workflow("wf_root", vec![node]),
        vec![tp.plugin.clone()],
    );
    let (_, errs) = run_until(&r, "u1", "s1", "run", RunConfig::default(), |_| false).await;
    assert!(errs.is_empty(), "r.Run yielded unexpected error: {errs:?}");
    let rows = tp.collect_rows(until_invocation_end).await;
    assert!(
        rows_of_type(&rows, EVENT_INVOCATION_ERROR).is_empty(),
        "got INVOCATION_ERROR, want COMPLETED; event types: {:?}",
        event_types(&rows)
    );
    only_row_of_type(&rows, EVENT_INVOCATION_COMPLETED);

    let (mut err_row, mut start1, mut start2, mut llm_err_row) = (None, None, None, None);
    let (mut err_idx, mut start2_idx) = (None, None);
    let mut start_count = 0;
    for (i, row) in rows.iter().enumerate() {
        if agent_of(row) != "flaky" {
            continue;
        }
        match str_col(row, "event_type").as_str() {
            EVENT_AGENT_ERROR if err_row.is_none() => {
                err_row = Some(row);
                err_idx = Some(i);
            }
            EVENT_AGENT_STARTING => {
                start_count += 1;
                match start_count {
                    1 => start1 = Some(row),
                    2 => {
                        start2 = Some(row);
                        start2_idx = Some(i);
                    }
                    _ => {}
                }
            }
            EVENT_LLM_ERROR if llm_err_row.is_none() => llm_err_row = Some(row),
            _ => {}
        }
    }
    let err_row = err_row.expect("no AGENT_ERROR(flaky) row found");
    let (Some(start1), Some(start2)) = (start1, start2) else {
        panic!(
            "missing AGENT_STARTING(flaky) rows: start1={} start2={}",
            start1.is_some(),
            start2.is_some()
        );
    };
    let ts = |row: &Row| row.timestamp("timestamp").expect("timestamp");
    let (err_ts, start1_ts, start2_ts) = (ts(err_row), ts(start1), ts(start2));
    assert!(
        err_ts < start2_ts,
        "AGENT_ERROR(flaky) timestamp {err_ts} is not before the second AGENT_STARTING(flaky) timestamp {start2_ts}"
    );
    assert!(
        matches!((err_idx, start2_idx), (Some(e), Some(s)) if e < s),
        "AGENT_ERROR(flaky) row index {err_idx:?} is not before second AGENT_STARTING(flaky) index {start2_idx:?}"
    );
    if let Some(llm_err_row) = llm_err_row {
        let llm_err_ts = ts(llm_err_row);
        assert!(
            err_ts >= llm_err_ts,
            "AGENT_ERROR(flaky) timestamp {err_ts} is before LLM_ERROR(flaky) timestamp {llm_err_ts}"
        );
    }
    let gap = start2_ts - err_ts;
    assert!(
        gap >= chrono::TimeDelta::milliseconds(20),
        "start2TS - errTS = {gap}, want >= 20ms (retry delay is {delay:?})"
    );
    let lat = must_latency(err_row);
    let want_total_ms = (err_ts - start1_ts).num_milliseconds();
    let diff = lat["total_ms"] - want_total_ms;
    assert!(
        (-1..=1).contains(&diff),
        "AGENT_ERROR(flaky) latency total_ms = {}, want {want_total_ms} ±1ms (errTS {err_ts} - start1TS {start1_ts})",
        lat["total_ms"]
    );
}

#[tokio::test]
async fn test_runner_stop_on_partial_event_of_async_author_is_not_an_error() {
    let new_child = || {
        custom_agent("child", vec![], |ctx| {
            let answer = event_of(&ctx, "child", text_response("child answer", false, false));
            futures::stream::iter([Ok(answer)]).boxed()
        })
    };
    let new_parallel = |with_content: bool| {
        let waiter = custom_agent("waiter", vec![], |ctx| {
            go_iter(move |y| async move {
                ctx.cancellation_token().cancelled().await;
                y.send(Err(ctx_err(&ctx))).await;
            })
        });
        new_parallel_agent(
            "par_root",
            vec![new_streaming_author(new_child(), with_content), waiter],
        )
    };
    type NewRoot<'a> = Box<dyn Fn() -> Arc<dyn Agent> + 'a>;
    let cases: Vec<(&str, NewRoot<'_>)> = vec![
        (
            "parallel agent, partial event with content",
            Box::new(|| new_parallel(true)),
        ),
        (
            "parallel agent, partial event without content",
            Box::new(|| new_parallel(false)),
        ),
        (
            "workflow node",
            Box::new(|| {
                fan_out_workflow(
                    "wf_root",
                    vec![agent_node(new_streaming_author(new_child(), true))],
                )
            }),
        ),
    ];
    for (name, new_root) in cases {
        require_child_started_after_partial_stop(|| async {
            let tp = setup_test_plugin().await;
            let r = new_test_runner(new_root(), vec![tp.plugin.clone()]);
            let mut saw_partial = false;
            let (_, errs) = run_until(&r, "u1", "s1", "go", RunConfig::default(), |item| {
                saw_partial = matches!(item, Ok(ev) if ev.response.partial);
                item.is_err() || saw_partial
            })
            .await;
            assert!(errs.is_empty(), "{name}: r.Run error: {errs:?}");
            assert!(saw_partial, "{name}: runner.Run yielded no partial event");
            tp.collect_rows(until_invocation_end).await
        })
        .await;
    }
}

#[tokio::test]
async fn test_runner_workflow_node_timeout_after_before_agent_shortcut_consumer_stop_completes() {
    for caller_mode in ["stop_on_partial", "stop_on_final", "drain"] {
        let tp = setup_test_plugin().await;
        let cached_ctx: Arc<Mutex<Option<gadk_agent::Context>>> = Arc::default();
        let keep = cached_ctx.clone();
        let capture: BeforeAgentCallback = Arc::new(move |cb_ctx| {
            *keep.lock() = Some(cb_ctx);
            async { Ok(Some(text_content("model", "cached answer"))) }.boxed()
        });
        let cached = llm_agent(LlmAgentConfig {
            name: "cached".into(),
            model: Some(scripted_llm(vec![])),
            before_agent_callbacks: vec![capture],
            ..Default::default()
        });
        let cached_node = agent_node_with(
            cached,
            NodeConfig {
                timeout: Some(Duration::from_millis(300)),
                ..Default::default()
            },
        );
        // Go `&waitDoneStreamLLM{waitCtx: func() context.Context { return cachedCtx }}`.
        let wait_ctx = cached_ctx.clone();
        let writer = llm_agent(LlmAgentConfig {
            name: "writer".into(),
            model: Some(fn_llm("wait-done-stream-llm", move |_, _| {
                let done = wait_ctx
                    .lock()
                    .as_ref()
                    .expect("cachedCtx")
                    .cancellation_token()
                    .clone();
                futures::stream::once(async move { done.cancelled().await })
                    .flat_map(|()| {
                        futures::stream::iter([
                            partial_text("streaming..."),
                            final_text("final answer"),
                        ])
                    })
                    .boxed()
            })),
            ..Default::default()
        });
        let writer_node = agent_node(writer);
        let wf_root = workflow_agent(
            "wf_root",
            vec![
                gadk_engine::workflow::Edge {
                    from: start(),
                    to: cached_node.clone(),
                    route: None,
                },
                gadk_engine::workflow::Edge {
                    from: cached_node,
                    to: writer_node,
                    route: None,
                },
            ],
        );

        let r = new_test_runner(wf_root, vec![tp.plugin.clone()]);
        let (_, errs) = run_until(
            &r,
            "u1",
            "s1",
            "go",
            RunConfig {
                streaming_mode: StreamingMode::Sse,
                ..Default::default()
            },
            |item| match item {
                Err(_) => true,
                Ok(ev) if ev.author != "writer" => false,
                Ok(ev) => {
                    (caller_mode == "stop_on_partial" && ev.response.partial)
                        || (caller_mode == "stop_on_final" && !ev.response.partial)
                }
            },
        )
        .await;
        assert!(
            errs.is_empty(),
            "{caller_mode}: r.Run yielded unexpected error: {errs:?}"
        );

        let rows = tp.collect_rows(until_invocation_end).await;
        assert!(
            rows_of_type(&rows, EVENT_INVOCATION_ERROR).is_empty(),
            "{caller_mode}: INVOCATION_ERROR rows, want 0; event types: {:?}",
            event_types(&rows)
        );
        only_row_of_type(&rows, EVENT_INVOCATION_COMPLETED);
        assert!(
            rows_of_type(&rows, EVENT_AGENT_ERROR).is_empty(),
            "{caller_mode}: AGENT_ERROR rows, want 0; event types: {:?}",
            event_types(&rows)
        );
    }
}

#[tokio::test]
async fn test_runner_workflow_node_failure_while_sibling_streams_emits_invocation_error() {
    let make_streamer = || {
        custom_agent("streamer", vec![], |ic| {
            go_iter(move |y| async move {
                let partial = event_of(&ic, "streamer", text_response("streaming...", true, false));
                y.send(Ok(partial)).await;
                // Keep running until the caller stops or the workflow cancels us.
                ic.cancellation_token().cancelled().await;
            })
        })
    };
    let make_failer = |ready: Gate| {
        custom_agent("failer", vec![], move |ic| {
            let ready = ready.clone();
            go_iter(move |y| async move {
                // Wait for streamer to have emitted its partial event.
                tokio::select! {
                    () = ready.wait() => {}
                    () = ic.cancellation_token().cancelled() => return,
                }
                y.send(Err(AgentError::other("node boom"))).await;
            })
        })
    };
    let run = |tp: &TestPlugin, wf: Arc<dyn Agent>, ready: Gate| {
        let r = new_test_runner(wf, vec![tp.plugin.clone()]);
        async move {
            let mut saw_partial = false;
            run_until(
                &r,
                "u1",
                "s1",
                "go",
                RunConfig::default(),
                |item| match item {
                    Err(_) => true,
                    Ok(ev) => {
                        if ev.response.partial && ev.author == "streamer" {
                            saw_partial = true;
                            ready.close();
                        }
                        false
                    }
                },
            )
            .await;
            assert!(saw_partial, "did not observe a partial event from streamer");
        }
    };

    // 2-node workflow
    let tp = setup_test_plugin().await;
    let ready = Gate::new();
    let wf = fan_out_workflow(
        "wf_root",
        vec![
            agent_node(make_streamer()),
            agent_node(make_failer(ready.clone())),
        ],
    );
    run(&tp, wf, ready).await;
    let rows = tp.collect_rows(until_invocation_end).await;
    assert_eq!(
        rows_of_type(&rows, EVENT_INVOCATION_ERROR).len(),
        1,
        "2-node workflow: INVOCATION_ERROR rows; event types: {:?}",
        event_types(&rows)
    );
    assert_eq!(
        agent_error_counts(&rows).get("failer"),
        Some(&1),
        "2-node workflow: AGENT_ERROR rows for failer; event types: {:?}",
        event_types(&rows)
    );

    // with cached shortcut node
    let tp = setup_test_plugin().await;
    let ready = Gate::new();
    let cached = llm_agent(LlmAgentConfig {
        name: "cached".into(),
        model: Some(scripted_llm(vec![])),
        before_agent_callbacks: vec![answering("cached answer")],
        ..Default::default()
    });
    let wf = fan_out_workflow(
        "wf_root",
        vec![
            agent_node(make_streamer()),
            agent_node(make_failer(ready.clone())),
            agent_node(cached),
        ],
    );
    run(&tp, wf, ready).await;
    let rows = tp.collect_rows(until_invocation_end).await;
    assert_eq!(
        rows_of_type(&rows, EVENT_INVOCATION_ERROR).len(),
        1,
        "with cached shortcut node: INVOCATION_ERROR rows; event types: {:?}",
        event_types(&rows)
    );
    let counts = agent_error_counts(&rows);
    assert_eq!(
        counts.get("failer"),
        Some(&1),
        "with cached shortcut node: AGENT_ERROR rows for failer; event types: {:?}",
        event_types(&rows)
    );
    // The cached shortcut node should NOT be attributed AGENT_ERROR.
    assert_eq!(
        counts.get("cached"),
        None,
        "with cached shortcut node: AGENT_ERROR rows for cached; event types: {:?}",
        event_types(&rows)
    );
}

#[tokio::test]
async fn test_runner_workflow_retry_parallel_responded_branches_purges_failed_attempt_and_completes()
 {
    for llm_streamer in [false, true] {
        let name = if llm_streamer {
            "llm_streamer_collateral_error"
        } else {
            "custom_streamer"
        };
        let tp = setup_test_plugin().await;
        let (final_seen, partial_seen, acked, ready) =
            (Gate::new(), Gate::new(), Gate::new(), Gate::new());
        go({
            let (partial_seen, acked, ready) = (partial_seen.clone(), acked.clone(), ready.clone());
            async move {
                partial_seen.wait().await;
                acked.wait().await;
                ready.close();
            }
        });
        let observer = plugin_named(
            "observer",
            gadk_agent::PluginConfig {
                on_event_callback: Some(Arc::new({
                    let (final_seen, partial_seen) = (final_seen.clone(), partial_seen.clone());
                    move |_ic, ev: Event| {
                        if !ev.response.partial && ev.author == "failer" {
                            final_seen.close();
                        }
                        if ev.response.partial && ev.author == "streamer" {
                            partial_seen.close();
                        }
                        async { Ok(None) }.boxed()
                    }
                })),
                ..Default::default()
            },
        );

        let streamer_attempts = Arc::new(AtomicUsize::new(0));
        let failer_attempts = Arc::new(AtomicUsize::new(0));
        let streamer = if llm_streamer {
            let (final_seen, acked) = (final_seen.clone(), acked.clone());
            llm_agent(LlmAgentConfig {
                name: "streamer".into(),
                // Go `&funcStreamLLM{fn}` (model name `func-stream-llm`).
                model: Some(fn_llm("func-stream-llm", move |_, _| {
                    if streamer_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                        let (final_seen, acked) = (final_seen.clone(), acked.clone());
                        return futures::stream::once(async move {
                            final_seen.wait().await;
                            partial_text("streaming...")
                        })
                        .chain(
                            futures::stream::once(async move { acked.close() })
                                .filter_map(|()| async { None }),
                        )
                        .chain(futures::stream::pending())
                        .boxed();
                    }
                    futures::stream::iter([final_text("streamer succeeded on retry")]).boxed()
                })),
                ..Default::default()
            })
        } else {
            let (final_seen, acked) = (final_seen.clone(), acked.clone());
            custom_agent("streamer", vec![], move |ctx| {
                let first = streamer_attempts.fetch_add(1, Ordering::SeqCst) == 0;
                let (final_seen, acked) = (final_seen.clone(), acked.clone());
                go_iter(move |y| async move {
                    if first {
                        tokio::select! {
                            () = final_seen.wait() => {}
                            () = ctx.cancellation_token().cancelled() => return,
                        }
                        let partial =
                            event_of(&ctx, "streamer", text_response("streaming...", true, false));
                        y.send(Ok(partial)).await;
                        acked.close();
                        ctx.cancellation_token().cancelled().await;
                        y.send(Err(ctx_err(&ctx))).await;
                        return;
                    }
                    let ev = event_of(
                        &ctx,
                        "streamer",
                        text_response("streamer succeeded on retry", false, true),
                    );
                    y.send(Ok(ev)).await;
                })
            })
        };
        let failer = {
            let ready = ready.clone();
            custom_agent("failer", vec![], move |ctx| {
                let first = failer_attempts.fetch_add(1, Ordering::SeqCst) == 0;
                let ready = ready.clone();
                go_iter(move |y| async move {
                    if first {
                        let final_ev =
                            event_of(&ctx, "failer", text_response("initial answer", false, true));
                        y.send(Ok(final_ev)).await;
                        tokio::select! {
                            () = ready.wait() => {}
                            () = ctx.cancellation_token().cancelled() => return,
                        }
                        y.send(Err(AgentError::other(ERR_SIBLING_FAILED))).await;
                        return;
                    }
                    let ev = event_of(
                        &ctx,
                        "failer",
                        text_response("failer succeeded on retry", false, true),
                    );
                    y.send(Ok(ev)).await;
                })
            })
        };

        let par = new_parallel_agent("par", vec![streamer, failer]);
        let node = agent_node_with(par, retry_twice(Duration::from_millis(1)));
        let wf_root = fan_out_workflow("wf_root", vec![node]);
        let run_cfg = RunConfig {
            streaming_mode: if llm_streamer {
                StreamingMode::Sse
            } else {
                StreamingMode::None
            },
            ..Default::default()
        };
        let r = new_test_runner(wf_root, vec![tp.plugin.clone(), observer]);
        let (_, errs) = run_until(&r, "u1", "s1", "go", run_cfg, |_| false).await;
        assert!(
            errs.is_empty(),
            "{name}: runner.Run yielded unexpected error: {errs:?}"
        );

        let rows = tp.collect_rows(until_invocation_end).await;
        assert!(
            rows_of_type(&rows, EVENT_INVOCATION_ERROR).is_empty(),
            "{name}: INVOCATION_ERROR rows, want 0; event types: {:?}",
            event_types(&rows)
        );
        only_row_of_type(&rows, EVENT_INVOCATION_COMPLETED);
        assert_eq!(
            agent_error_counts(&rows),
            BTreeMap::from([("par".to_string(), 1)]),
            "{name}: AGENT_ERROR rows per agent; event types: {:?}",
            event_types(&rows)
        );
        let err_row = only_row_of_type(&rows, EVENT_AGENT_ERROR);
        let msg = str_col(&err_row, "error_message");
        assert!(
            msg.contains(ERR_SIBLING_FAILED),
            "{name}: AGENT_ERROR(par) error_message = {msg:?}, want it to contain {ERR_SIBLING_FAILED:?}"
        );
        if llm_streamer {
            assert_eq!(
                rows_of_type(&rows, EVENT_LLM_ERROR).len(),
                1,
                "{name}: LLM_ERROR rows; event types: {:?}",
                event_types(&rows)
            );
        }
    }
}

// Go's ContentFormatter sleeps on the plugin's writer goroutine; a multi-threaded runtime keeps
// the agents running while the formatter sleeps.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_runner_concurrent_workflow_retries_orders_purged_agent_error_before_next_starting() {
    for recorded in [true, false] {
        let name = if recorded {
            "recorded failure"
        } else {
            "silent failure"
        };
        let first_agent_error = Arc::new(AtomicBool::new(false));
        let tp = new_test_plugin_with_config(|cfg| {
            cfg.content_formatter = Some(Arc::new(move |payload: &Value, event_type: &str| {
                if event_type == EVENT_AGENT_ERROR
                    && first_agent_error
                        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                        .is_ok()
                {
                    std::thread::sleep(Duration::from_millis(25));
                }
                Ok(payload.clone())
            }));
        })
        .await;

        let err_transient = "transient attempt-1 failure";
        let (attempt1_ready, attempt2_ready) = (Gate::new(), Gate::new());
        let attempt1_entered = Arc::new(AtomicUsize::new(0));
        let attempt2_entered = Arc::new(AtomicUsize::new(0));
        let seen_by_agent: Arc<Mutex<HashMap<String, usize>>> = Arc::default();

        let gate = plugin_named(
            "retry_gate",
            gadk_agent::PluginConfig {
                before_agent_callback: Some(Arc::new({
                    let attempt2_ready = attempt2_ready.clone();
                    move |cb_ctx: gadk_agent::Context| {
                        let name = cb_ctx.agent_name();
                        if name != "agent_a" && name != "agent_b" {
                            return async { Ok(None) }.boxed();
                        }
                        let n = {
                            let mut seen = seen_by_agent.lock();
                            let n = seen.entry(name).or_insert(0);
                            *n += 1;
                            *n
                        };
                        if n != 2 {
                            return async { Ok(None) }.boxed();
                        }
                        if attempt2_entered.fetch_add(1, Ordering::SeqCst) + 1 == 2 {
                            attempt2_ready.close();
                        }
                        let attempt2_ready = attempt2_ready.clone();
                        async move {
                            tokio::select! {
                                () = attempt2_ready.wait() => {}
                                () = cb_ctx.cancellation_token().cancelled() => {}
                            }
                            Ok(None)
                        }
                        .boxed()
                    }
                })),
                ..Default::default()
            },
        );

        let make_retry_node = |name: &str| -> Arc<dyn Node> {
            if recorded {
                let (entered, ready) = (attempt1_entered.clone(), attempt1_ready.clone());
                let a = llm_agent(LlmAgentConfig {
                    name: name.into(),
                    model: Some(gated_flaky_llm(
                        move || {
                            if entered.fetch_add(1, Ordering::SeqCst) + 1 == 2 {
                                ready.close();
                            }
                        },
                        attempt1_ready.clone(),
                        err_transient,
                        &format!("{name} ok"),
                    )),
                    ..Default::default()
                });
                return agent_node_with(a, retry_twice(Duration::from_millis(1)));
            }
            let attempts = Arc::new(AtomicUsize::new(0));
            let (entered, ready, author) = (
                attempt1_entered.clone(),
                attempt1_ready.clone(),
                name.to_owned(),
            );
            let a = custom_agent(name, vec![], move |ctx| {
                let first = attempts.fetch_add(1, Ordering::SeqCst) == 0;
                let (entered, ready, author) = (entered.clone(), ready.clone(), author.clone());
                go_iter(move |y| async move {
                    if first {
                        if entered.fetch_add(1, Ordering::SeqCst) + 1 == 2 {
                            ready.close();
                        }
                        tokio::select! {
                            () = ready.wait() => {}
                            () = ctx.cancellation_token().cancelled() => return,
                        }
                        y.send(Err(AgentError::other(err_transient))).await;
                        return;
                    }
                    let ev = event_of(
                        &ctx,
                        &author,
                        text_response(&format!("{author} ok"), false, true),
                    );
                    y.send(Ok(ev)).await;
                })
            });
            agent_node_with(a, retry_twice(Duration::from_millis(1)))
        };

        let wf_a = fan_out_workflow("wf_a", vec![make_retry_node("agent_a")]);
        let wf_b = fan_out_workflow("wf_b", vec![make_retry_node("agent_b")]);
        let par_root = new_parallel_agent("par_root", vec![wf_a, wf_b]);

        let r = new_test_runner(par_root, vec![gate, tp.plugin.clone()]);
        let (_, errs) = run_until(&r, "u1", "s1", "go", RunConfig::default(), |_| false).await;
        assert!(
            errs.is_empty(),
            "{name}: runner.Run yielded unexpected error: {errs:?}"
        );

        let rows = tp.collect_rows(until_invocation_end).await;
        only_row_of_type(&rows, EVENT_INVOCATION_COMPLETED);

        for agent in ["agent_a", "agent_b"] {
            let agent_rows: Vec<&Row> = rows
                .iter()
                .filter(|r| {
                    agent_of(r) == agent
                        && [
                            EVENT_AGENT_STARTING,
                            EVENT_AGENT_ERROR,
                            EVENT_AGENT_COMPLETED,
                        ]
                        .contains(&str_col(r, "event_type").as_str())
                })
                .collect();
            let got_types: Vec<String> = agent_rows
                .iter()
                .map(|r| str_col(r, "event_type"))
                .collect();
            // A recorded failure purges the first attempt when the second one starts; a
            // silent one only when the workflow agent ends.
            let (want_types, first, second) = if recorded {
                (
                    [
                        EVENT_AGENT_STARTING,
                        EVENT_AGENT_ERROR,
                        EVENT_AGENT_STARTING,
                        EVENT_AGENT_COMPLETED,
                    ],
                    [0, 1],
                    [2, 3],
                )
            } else {
                (
                    [
                        EVENT_AGENT_STARTING,
                        EVENT_AGENT_STARTING,
                        EVENT_AGENT_COMPLETED,
                        EVENT_AGENT_ERROR,
                    ],
                    [0, 3],
                    [1, 2],
                )
            };
            assert_eq!(got_types, want_types, "{name}: {agent} lifecycle row order");
            let span = |i: usize| str_col(agent_rows[i], "span_id");
            let (span1_start, span1_err) = (span(first[0]), span(first[1]));
            let (span2_start, span2_done) = (span(second[0]), span(second[1]));
            assert!(
                !span1_start.is_empty() && span1_start == span1_err,
                "{name}: {agent} attempt-1 span_id mismatch: start={span1_start:?} error={span1_err:?}"
            );
            assert!(
                !span2_start.is_empty() && span2_start == span2_done,
                "{name}: {agent} attempt-2 span_id mismatch: start={span2_start:?} completed={span2_done:?}"
            );
            assert_ne!(
                span1_start, span2_start,
                "{name}: {agent} reused attempt-1 span_id on attempt 2"
            );
        }
    }
}

#[tokio::test]
// parity: adapted: Go's check of the std log for "… is not supported for callback context" is dropped
// (Go agent/callback_context_wrapper.go log.Print; gadk has no such log).
async fn test_runner_parallel_worker_pipeline_with_cached_step_emits_invocation_completed() {
    let per_item_root = |pipeline: Arc<dyn Agent>, cfg: NodeConfig| {
        let items = function_node::<Value, Vec<String>, _, _>(
            "items",
            |_ctx, _in: Value| async { Ok(vec!["a".to_string(), "b".to_string()]) },
            NodeConfig::default(),
        );
        let per_item: Arc<dyn Node> = ParallelWorker::new("per_item", agent_node(pipeline), 0, cfg)
            .expect("workflow.NewParallelWorker");
        workflow_agent("root", chain(&[start(), items, per_item]))
    };

    // pipeline_succeeds_on_first_attempt
    let tp = setup_test_plugin().await;
    let summarize = llm_agent(LlmAgentConfig {
        name: "summarize".into(),
        model: Some(scripted_llm(vec![
            model_text("summary"),
            model_text("summary"),
        ])),
        ..Default::default()
    });
    let pipeline = sequential("pipeline", vec![new_cache_hit_agent(), summarize]);
    let r = new_test_runner(
        per_item_root(pipeline, NodeConfig::default()),
        vec![tp.plugin.clone()],
    );
    let (_, errs) = run_until(&r, "u1", "s1", "hi", RunConfig::default(), |_| false).await;
    assert!(errs.is_empty(), "r.Run error: {errs:?}");
    require_invocation_completed_ok(&tp.collect_rows(until_invocation_end).await);

    // retry_on_parallel_worker_pipeline_with_cached_and_flaky_steps
    let tp = setup_test_plugin().await;
    let flaky = llm_agent(LlmAgentConfig {
        name: "flaky".into(),
        model: Some(flaky_on_first_pass_llm("attempt 1 flaky 503", "summary ok")),
        ..Default::default()
    });
    let pipeline = sequential("pipeline", vec![new_cache_hit_agent(), flaky]);
    let r = new_test_runner(
        per_item_root(pipeline, retry_twice(Duration::from_millis(1))),
        vec![tp.plugin.clone()],
    );
    let (_, errs) = run_until(&r, "u1", "s1", "hi", RunConfig::default(), |_| false).await;
    assert!(errs.is_empty(), "r.Run error: {errs:?}");
    let rows = tp.collect_rows(until_invocation_end).await;
    assert!(
        rows_of_type(&rows, EVENT_INVOCATION_ERROR).is_empty(),
        "INVOCATION_ERROR rows, want 0; event types: {:?}",
        event_types(&rows)
    );
    only_row_of_type(&rows, EVENT_INVOCATION_COMPLETED);
    assert_eq!(
        agent_error_counts(&rows),
        BTreeMap::from([("flaky".to_string(), 1), ("pipeline".to_string(), 1)]),
        "AGENT_ERROR counts (cached step must not get AGENT_ERROR)"
    );
}

#[tokio::test]
async fn test_runner_workflow_agent_run_again_after_forwarded_node_error_emits_invocation_error() {
    for (name, stop_at_error) in [
        ("guard reads wf to its end", false),
        ("guard stops reading wf at the node error", true),
    ] {
        let tp = setup_test_plugin().await;
        let model_err = "pass 1 model failure 503";
        let x = llm_agent(LlmAgentConfig {
            name: "x".into(),
            model: Some(flaky_on_first_pass_llm(model_err, "ok on pass 2")),
            ..Default::default()
        });
        let wf = fan_out_workflow("wf", vec![agent_node(x)]);
        // guard forwards the events and errors of wf, stops at the node error if
        // stop_at_error, and then runs wf once more.
        let run_wf = wf.clone();
        let guard = custom_agent("guard", vec![wf], move |ctx| {
            let wf = run_wf.clone();
            go_iter(move |y| async move {
                for _ in 0..2 {
                    let mut events = wf.run(ctx.clone());
                    while let Some(item) = events.next().await {
                        let failed = item.is_err();
                        y.send(item).await;
                        if failed && stop_at_error {
                            break;
                        }
                    }
                }
            })
        });
        let r = new_test_runner(guard, vec![tp.plugin.clone()]);
        let (_, errs) = run_until(&r, "u1", "s1", "go", RunConfig::default(), |_| false).await;
        assert!(
            errs.len() == 1 && is_error(&errs[0], model_err),
            "{name}: r.Run errors = {errs:?}, want [{model_err}]"
        );
        let rows = tp.collect_rows(until_invocation_end).await;
        assert!(
            rows_of_type(&rows, EVENT_INVOCATION_COMPLETED).is_empty(),
            "{name}: got INVOCATION_COMPLETED although the caller received {model_err}; event types: {:?}",
            event_types(&rows)
        );
        assert_eq!(
            str_col(
                &only_row_of_type(&rows, EVENT_INVOCATION_ERROR),
                "error_message"
            ),
            model_err,
            "{name}: INVOCATION_ERROR error_message"
        );
    }
}

#[tokio::test]
async fn test_trace_manager_cross_branch_dynamic_node_runs_cached_child() {
    let tp = setup_test_plugin().await;
    let (child_started, branch_a_done) = (Gate::new(), Gate::new());
    let cached_node = agent_node(new_signaling_cache_hit_agent(child_started.clone()));
    let dyn_node = dynamic_node::<Value, String, _, _>(
        "dyn",
        move |ctx, _input: Value, _emit: Emit| {
            let cached_node = cached_node.clone();
            async move {
                run_node(
                    &ctx,
                    cached_node,
                    json!("go"),
                    RunNodeOptions {
                        override_branch: Some("par.branch_a".into()),
                        ..Default::default()
                    },
                )
                .await?;
                Ok("ok".to_string())
            }
        },
        NodeConfig::default(),
    );
    let wf = Workflow::new(
        "wf",
        chain(&[start(), dyn_node]),
        WorkflowOptions::default(),
    )
    .expect("workflow.New(wf)");
    let branch_b = custom_agent("branch_b", vec![], move |ic| wf.run(ic));
    let held_too_long = Arc::new(AtomicBool::new(false));
    let hold = plugin_named(
        "hold_cached_child_event",
        gadk_agent::PluginConfig {
            on_event_callback: Some(Arc::new({
                let (branch_a_done, held_too_long) = (branch_a_done.clone(), held_too_long.clone());
                move |_ic, ev: Event| {
                    let (branch_a_done, held_too_long) =
                        (branch_a_done.clone(), held_too_long.clone());
                    async move {
                        if ev.author == "cached_child"
                            && tokio::time::timeout(Duration::from_secs(10), branch_a_done.wait())
                                .await
                                .is_err()
                        {
                            held_too_long.store(true, Ordering::SeqCst);
                        }
                        Ok(None)
                    }
                    .boxed()
                }
            })),
            ..Default::default()
        },
    );
    let par = parallel_agent::new(parallel_agent::ParallelAgentConfig {
        agent: AgentConfig {
            name: "par".into(),
            sub_agents: vec![new_branch_a(child_started, branch_a_done, false), branch_b],
            ..Default::default()
        },
    })
    .unwrap();
    let r = new_test_runner(par, vec![hold, tp.plugin.clone()]);
    let (_, errs) = run_until(&r, "u1", "s1", "go", RunConfig::default(), |_| false).await;
    assert!(errs.is_empty(), "runner.Run error: {errs:?}");
    assert!(
        !held_too_long.load(Ordering::SeqCst),
        "branch_a did not end while the event of cached_child was held"
    );
    require_invocation_completed_ok(&tp.collect_rows(until_invocation_end).await);
}

#[tokio::test]
async fn test_concurrency_race_concurrent_same_agent_on_same_branch_and_path() {
    for as_node in [false, true] {
        let topology = if as_node {
            "custom parent as workflow node"
        } else {
            "custom parent as root"
        };
        for cached_first in [true, false] {
            let order = if cached_first {
                "cached run starts first"
            } else {
                "answering run starts first"
            };
            let name = format!("{topology}/{order}");
            let tp = setup_test_plugin().await;
            let (first_entered, cached_answered) = (Gate::new(), Gate::new());
            let calls = Arc::new(AtomicUsize::new(0));

            let before: BeforeAgentCallback = Arc::new({
                let (first_entered, cached_answered) =
                    (first_entered.clone(), cached_answered.clone());
                move |_ctx| {
                    let first = calls.fetch_add(1, Ordering::SeqCst) == 0;
                    if first {
                        first_entered.close();
                    }
                    let answer = (first == cached_first).then(|| {
                        cached_answered.close();
                        text_content("model", "cache hit")
                    });
                    async move { Ok(answer) }.boxed()
                }
            });
            let child = custom_agent_with("child", vec![before], vec![], {
                let cached_answered = cached_answered.clone();
                move |ctx| {
                    let cached_answered = cached_answered.clone();
                    go_iter(move |y| async move {
                        // Answer only once the cached run answered, so that this run ends
                        // while the cached activation of the same agent is open.
                        if tokio::time::timeout(Duration::from_secs(10), cached_answered.wait())
                            .await
                            .is_err()
                        {
                            y.send(Err(AgentError::other("cached run did not answer")))
                                .await;
                            return;
                        }
                        let ev =
                            event_of(&ctx, "child", text_response("child answer", false, false));
                        y.send(Ok(ev)).await;
                    })
                }
            });

            let parent = custom_agent("parent", vec![], move |ctx| {
                let (child, first_entered) = (child.clone(), first_entered.clone());
                go_iter(move |y| async move {
                    let answer =
                        event_of(&ctx, "parent", text_response("parent answer", false, false));
                    y.send(Ok(answer)).await;

                    // Only the answering run's events are forwarded: the cached run's event is
                    // dropped so that only its ended state, settled after parent joins it,
                    // marks it responded. Go's goroutines yield to the parent's consumer; here
                    // they hand each item over and wait for the parent's yield to return.
                    let (tx, mut rx) = mpsc::channel::<(Item, oneshot::Sender<()>)>(1);
                    let run = |i: usize, forward: bool| {
                        let (child, ctx, first_entered, tx) = (
                            child.clone(),
                            ctx.clone(),
                            first_entered.clone(),
                            tx.clone(),
                        );
                        go(async move {
                            let mut run_err: Option<String> = None;
                            if i == 1
                                && tokio::time::timeout(
                                    Duration::from_secs(10),
                                    first_entered.wait(),
                                )
                                .await
                                .is_err()
                            {
                                return (Some("first child run did not start".to_string()), false);
                            }
                            let mut events = child.run(ctx);
                            while let Some(item) = events.next().await {
                                if !forward {
                                    if let Err(e) = &item {
                                        run_err.get_or_insert_with(|| e.to_string());
                                    }
                                    continue;
                                }
                                let (ack_tx, ack_rx) = oneshot::channel();
                                if tx.send((item, ack_tx)).await.is_err() || ack_rx.await.is_err() {
                                    return (run_err, true);
                                }
                            }
                            (run_err, false)
                        })
                    };
                    let runs = [run(0, !cached_first), run(1, cached_first)];
                    drop(tx);
                    while let Some((item, ack)) = rx.recv().await {
                        y.send(item).await;
                        let _ = ack.send(());
                    }
                    let mut errs = Vec::new();
                    let mut stopped_out = false;
                    for run in runs {
                        let (err, stopped) = run.await.expect("child run goroutine");
                        stopped_out |= stopped;
                        errs.extend(err);
                    }
                    if !stopped_out && !errs.is_empty() {
                        y.send(Err(AgentError::other(errs.join("\n")))).await;
                    }
                })
            });
            let root = if as_node {
                workflow_agent("root", chain(&[start(), agent_node(parent)]))
            } else {
                parent
            };

            let r = new_test_runner(root, vec![tp.plugin.clone()]);
            let (_, errs) = run_until(&r, "u1", "s1", "go", RunConfig::default(), |_| false).await;
            assert!(errs.is_empty(), "{name}: runner.Run error: {errs:?}");
            let rows = tp.collect_rows(until_invocation_end).await;
            require_invocation_completed_ok(&rows);

            let started = agent_rows_of_type(&rows, EVENT_AGENT_STARTING, "child");
            let completed = agent_rows_of_type(&rows, EVENT_AGENT_COMPLETED, "child");
            assert_eq!(
                started.len(),
                2,
                "{name}: AGENT_STARTING rows of child; event types: {:?}",
                event_types(&rows)
            );
            assert_ne!(
                str_col(&started[0], "span_id"),
                str_col(&started[1], "span_id"),
                "{name}: both runs of child started on one span, want distinct spans"
            );
            // The answering run starts second when the cached run starts first, and it is the
            // only run of child that gets an AfterAgentCallback.
            let answering = if cached_first {
                &started[1]
            } else {
                &started[0]
            };
            assert_eq!(
                completed.len(),
                1,
                "{name}: AGENT_COMPLETED rows of child; event types: {:?}",
                event_types(&rows)
            );
            assert_eq!(
                str_col(&completed[0], "span_id"),
                str_col(answering, "span_id"),
                "{name}: AGENT_COMPLETED row of child, want the answering run's span"
            );
            assert_eq!(
                str_col(&completed[0], "status"),
                "OK",
                "{name}: AGENT_COMPLETED status of child"
            );
        }
    }
}

#[tokio::test]
async fn test_runner_workflow_agent_with_after_agent_callback_drained_after_branch_error_emits_invocation_error()
 {
    for wrap_in_seq in [false, true] {
        let name = if wrap_in_seq {
            "sequential root with AfterAgentCallback wrapping parallel"
        } else {
            "parallel root with AfterAgentCallback"
        };
        let tp = setup_test_plugin().await;
        let answered = Gate::new();
        let branch_err = "branch failed after answering";

        let answering = custom_agent("answering", vec![], {
            let answered = answered.clone();
            move |ctx| {
                let answered = answered.clone();
                go_iter(move |y| async move {
                    let _close = CloseOnDrop(answered);
                    let ev = event_of(
                        &ctx,
                        "answering",
                        text_response("answering branch answer", false, false),
                    );
                    y.send(Ok(ev)).await;
                })
            }
        });
        let failing = custom_agent("failing", vec![], move |ctx| {
            let answered = answered.clone();
            go_iter(move |y| async move {
                let ev = event_of(
                    &ctx,
                    "failing",
                    text_response("failing branch answer", false, false),
                );
                y.send(Ok(ev)).await;
                let _ = tokio::time::timeout(Duration::from_secs(10), answered.wait()).await;
                y.send(Err(AgentError::other(branch_err))).await;
            })
        });

        let trailer = |text: &'static str| -> AfterAgentCallback {
            Arc::new(move |_ctx| async move { Ok(Some(text_content("model", text))) }.boxed())
        };
        let par = parallel_agent::new(parallel_agent::ParallelAgentConfig {
            agent: AgentConfig {
                name: "par".into(),
                sub_agents: vec![answering, failing],
                after_agent_callbacks: if wrap_in_seq {
                    vec![]
                } else {
                    vec![trailer("par trailer")]
                },
                ..Default::default()
            },
        })
        .unwrap();
        let root = if wrap_in_seq {
            sequential_agent::new(sequential_agent::SequentialAgentConfig {
                agent: AgentConfig {
                    name: "seq_root".into(),
                    sub_agents: vec![par],
                    after_agent_callbacks: vec![trailer("seq trailer")],
                    ..Default::default()
                },
            })
            .unwrap()
        } else {
            par
        };

        let r = new_test_runner(root, vec![tp.plugin.clone()]);
        let (_, errs) = run_until(&r, "u1", "s1", "go", RunConfig::default(), |_| false).await;
        let saw_err = errs.first();
        assert!(
            saw_err.is_some_and(|e| is_error(e, branch_err)),
            "{name}: runner.Run error = {saw_err:?}, want {branch_err}"
        );
        let rows = tp.collect_rows(until_invocation_end).await;
        assert!(
            rows_of_type(&rows, EVENT_INVOCATION_COMPLETED).is_empty(),
            "{name}: INVOCATION_COMPLETED rows, want 0; event types: {:?}",
            event_types(&rows)
        );
        let inv_err = only_row_of_type(&rows, EVENT_INVOCATION_ERROR);
        assert_eq!(
            str_col(&inv_err, "status"),
            "ERROR",
            "{name}: INVOCATION_ERROR status"
        );
    }
}
