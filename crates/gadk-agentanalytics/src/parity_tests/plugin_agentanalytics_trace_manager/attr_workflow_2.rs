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
//! group D, second half (Go line order): workflow scheduler nodes, ParallelWorker items and
//! function nodes, reported node outcomes, `finish_task`, unjoined sub-agent calls and the
//! ParallelWorker guardrail.
//! Shared helpers: [`attr_support`](super::attr_support).
//!
//! Go → gadk (this file):
//! - a Go iterator body (`func(yield func(*session.Event, error) bool) { … }`) = [`generator`]: the
//!   body runs inside the stream's polls (on the fiber polling the run, as Go's body runs on the
//!   goroutine ranging over it), and `y.send(item).await` returns once the reader asks for the
//!   next item (`true`) or dropped the stream (`false`), as Go's `yield`;
//! - `ctx.Value(concurrentFailRunKey{})` = [`is_fail_run`]: a task-local set around the failing
//!   activation's goroutine (gadk contexts carry no values; the model and the callbacks of that
//!   run are polled on that task);
//! - `ctx.Err()` = [`ctx_err`](super::attr_support::ctx_err); `contextPath(ctx)` =
//!   `Context::context_path`;
//! - `select { case <-ch: case <-time.After(10 * time.Second): }` =
//!   [`within`](super::attr_support::within).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::task::Poll;
use std::time::Duration;

use futures::future::BoxFuture;
use futures::{FutureExt, StreamExt};
use gadk_agent::{
    Agent, AgentConfig, AgentError, BeforeAgentCallback, Context, ContextDelta, EventStream,
    InvocationContext, RunConfig, Tool, ToolError,
};
use gadk_engine::RunOptions;
use gadk_engine::llm::{LlmAgentConfig, Mode, run_llm_agent_as_node};
use gadk_engine::workflow::{
    AgentNode, DEFAULT, Edge, Emit, JoinNode, Node, NodeConfig, ParallelWorker, RetryConfig, chain,
    emitting_function_node, function_node,
};
use gadk_genai::Content;
use gadk_model::{LlmRequest, LlmResponse, ModelError};
use gadk_session::{Event, NodeInfo};
use serde_json::{Map, Value, json};
use tokio::sync::{mpsc, oneshot};

use super::attr_support::{
    CloseOnDrop, Gate, agent_node, blocking_llm, ctx_err, edge, finish_task_llm,
    flaky_on_first_pass_llm, go, new_parallel_agent, start, within, workflow_agent,
};
use super::runner_support::*;
use crate::event_types::*;
use crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::*;
use crate::plugin::callbacks::has_task_completed_response;
use crate::trace::testkit::sequential;
use crate::trace::{
    CallbackCtx, Identity, TraceManager, WORKFLOW_BASE_PATH_PREFIX, WORKFLOW_NODE_FAILED_OUTCOME,
    WORKFLOW_NODE_OUTCOME_KEY,
};

type Item = Result<Event, AgentError>;

/// Go's `yield` of an iterator body run by [`generator`].
struct Yield(mpsc::UnboundedSender<(Item, oneshot::Sender<()>)>);

impl Yield {
    /// Go `yield(ev, err)`: hands `item` to the reader and returns once the reader asks for the
    /// next item (`true`) or dropped the stream (`false`).
    async fn send(&self, item: Item) -> bool {
        let (ack, acked) = oneshot::channel();
        if self.0.send((item, ack)).is_err() {
            return false;
        }
        acked.await.is_ok()
    }
}

/// A Go iterator body as an event stream: `body` is polled inside the stream's polls, so it
/// runs on the fiber reading the stream, and each [`Yield::send`] resumes at the next poll.
fn generator<F, Fut>(body: F) -> EventStream
where
    F: FnOnce(Yield) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let (tx, mut rx) = mpsc::unbounded_channel::<(Item, oneshot::Sender<()>)>();
    let mut body: Option<Pin<Box<dyn Future<Output = ()> + Send>>> =
        Some(Box::pin(body(Yield(tx))));
    let mut ack: Option<oneshot::Sender<()>> = None;
    futures::stream::poll_fn(move |cx| {
        if let Some(a) = ack.take() {
            let _ = a.send(());
        }
        loop {
            if let Poll::Ready(Some((item, a))) = rx.poll_recv(cx) {
                ack = Some(a);
                return Poll::Ready(Some(item));
            }
            let Some(b) = body.as_mut() else {
                return Poll::Ready(None);
            };
            match b.as_mut().poll(cx) {
                Poll::Ready(()) => body = None,
                Poll::Pending => {
                    if let Poll::Ready(Some((item, a))) = rx.poll_recv(cx) {
                        ack = Some(a);
                        return Poll::Ready(Some(item));
                    }
                    return Poll::Pending;
                }
            }
        }
    })
    .boxed()
}

/// A before- or after-agent callback (the two Go types share one signature).
fn agent_cb<F, Fut>(f: F) -> BeforeAgentCallback
where
    F: Fn(Context) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Option<Content>, AgentError>> + Send + 'static,
{
    Arc::new(move |ctx| f(ctx).boxed())
}

/// Go `agent.New(agent.Config{Name, SubAgents, BeforeAgentCallbacks, AfterAgentCallbacks, Run})`.
fn agent_with(
    name: &str,
    sub_agents: Vec<Arc<dyn Agent>>,
    before: Vec<BeforeAgentCallback>,
    after: Vec<BeforeAgentCallback>,
    run: impl Fn(InvocationContext) -> EventStream + Send + Sync + 'static,
) -> Arc<dyn Agent> {
    gadk_agent::new_agent(AgentConfig {
        name: name.into(),
        sub_agents,
        before_agent_callbacks: before,
        after_agent_callbacks: after,
        run: Some(Arc::new(run)),
        ..Default::default()
    })
    .expect("custom agent")
}

/// Go `ev := session.NewEvent(ctx, ctx.InvocationID()); ev.Author = author; ev.Branch =
/// ctx.Branch(); ev.Content = textContent("model", text)`.
fn answer(ctx: &InvocationContext, author: &str, text: &str) -> Event {
    let mut ev = new_event(ctx);
    ev.author = author.into();
    ev.branch = ctx.branch();
    ev.response.content = Some(text_content("model", text));
    ev
}

/// The metadata of the completion event of a failed node run (Go `nodeCompletion`'s keys) on an
/// event of `author` at the context's node path: `NodeInfo.Path = contextPath(ctx)`,
/// `workflowBasePathPrefix + "root"` = the path up to its last `/`, and the failed outcome.
fn report_failed_node(ctx: &InvocationContext, ev: &mut Event) {
    let path = ctx.clone().into_context().context_path();
    let base = path.rfind('/').map_or("", |i| &path[..i]).to_owned();
    ev.node_info = Some(NodeInfo {
        path,
        ..Default::default()
    });
    ev.response.custom_metadata.insert(
        format!("{WORKFLOW_BASE_PATH_PREFIX}root"),
        Value::String(base),
    );
    ev.response.custom_metadata.insert(
        WORKFLOW_NODE_OUTCOME_KEY.into(),
        Value::String(WORKFLOW_NODE_FAILED_OUTCOME.into()),
    );
}

/// Go `workflow.NewFunctionNode(name, func(agent.Context, any) (string, error) { return name,
/// nil }, workflow.NodeConfig{})` (the tests' `quick`).
fn quick(name: &str) -> Arc<dyn Node> {
    let out = name.to_owned();
    function_node(
        name,
        move |_: Context, _: Value| {
            let out = out.clone();
            async move { Ok::<_, AgentError>(out) }
        },
        NodeConfig::default(),
    )
}

/// The node `c` of the tests that route two join nodes to one agent node: returns "c" once
/// `ready` closed, else fails with `never` after 10 s.
fn wait_node(ready: Gate, never: &'static str) -> Arc<dyn Node> {
    function_node(
        "c",
        move |_: Context, _: Value| {
            let ready = ready.clone();
            async move {
                if within(&ready).await {
                    Ok("c".to_owned())
                } else {
                    Err(AgentError::other(never))
                }
            }
        },
        NodeConfig::default(),
    )
}

/// The workflow `root` in which two join nodes route (`workflow.Default`) to the agent node `x`:
/// `j1` joins `a` and `b`, `j2` joins `a` and `c`, so the scheduler starts `x` a second time
/// once `c` returns.
fn two_joins_root(x: Arc<dyn Node>, c: Arc<dyn Node>) -> Arc<dyn Agent> {
    let (a, b) = (quick("a"), quick("b"));
    let j1: Arc<dyn Node> = JoinNode::new("j1");
    let j2: Arc<dyn Node> = JoinNode::new("j2");
    let st = start();
    let routed = |from: &Arc<dyn Node>| Edge {
        from: from.clone(),
        to: x.clone(),
        route: Some(DEFAULT.clone()),
    };
    workflow_agent(
        "root",
        vec![
            edge(&st, &a),
            edge(&st, &b),
            edge(&st, &c),
            edge(&a, &j1),
            edge(&b, &j1),
            edge(&a, &j2),
            edge(&c, &j2),
            routed(&j1),
            routed(&j2),
        ],
    )
}

/// Go `newOneEventAgent`'s `afterYield`.
type AfterYield = Arc<dyn Fn(InvocationContext, i32) -> BoxFuture<'static, ()> + Send + Sync>;

/// Go `newOneEventAgent(t, name, afterYield)`: a custom agent that answers "<name> answer" and
/// then, if the reader asks for more, runs `after_yield` with its context and the number of the
/// run (from 1).
fn new_one_event_agent(name: &str, after_yield: Option<AfterYield>) -> Arc<dyn Agent> {
    let runs = Arc::new(AtomicI32::new(0));
    let author = name.to_owned();
    agent_with(name, vec![], vec![], vec![], move |ctx| {
        let (runs, author, after_yield) = (runs.clone(), author.clone(), after_yield.clone());
        generator(move |y| async move {
            let run = runs.fetch_add(1, Ordering::SeqCst) + 1;
            let ev = answer(&ctx, &author, &format!("{author} answer"));
            if y.send(Ok(ev)).await
                && let Some(f) = after_yield
            {
                f(ctx, run).await;
            }
        })
    })
}

/// Go `functionNodeEdges(t, fn, underWorker)`: the edges of a workflow that runs `f` once,
/// right after its start or as the node that a `workflow.ParallelWorker` runs for its only item.
fn function_node_edges(f: Arc<dyn Node>, under_worker: bool) -> Vec<Edge> {
    if !under_worker {
        return chain(&[start(), f]);
    }
    let items = function_node(
        "items",
        |_: Context, _: Value| async { Ok::<_, AgentError>(vec!["a".to_owned()]) },
        NodeConfig::default(),
    );
    let per_item: Arc<dyn Node> =
        ParallelWorker::new("per_item", f, 0, NodeConfig::default()).expect("parallel worker");
    chain(&[start(), items, per_item])
}

/// Drains `stream`; the first error, if any (Go `for _, err := range s { if err != nil {
/// return err } }`).
async fn first_error(mut stream: EventStream) -> Option<AgentError> {
    while let Some(item) = stream.next().await {
        if let Err(e) = item {
            return Some(e);
        }
    }
    None
}

/// Go `newTestRunner(t, root, p).Run(t.Context(), "u1", "s1", textContent("user", text),
/// agent.RunConfig{})` read to its end: the errors it yielded.
async fn run_errors(root: Arc<dyn Agent>, tp: &TestPlugin, text: &str) -> Vec<AgentError> {
    let r = new_test_runner(root, vec![tp.plugin.clone()]);
    run_turn(&r, "u1", "s1", text, RunConfig::default()).await.1
}

/// The AGENT_STARTING rows of `agent`.
fn starts_of(rows: &[crate::row::Row], agent: &str) -> usize {
    agent_rows_of_type(rows, EVENT_AGENT_STARTING, agent).len()
}

#[tokio::test]
async fn test_runner_workflow_root_contentless_partial_stopped_after_sibling_node_starts_emits_completed()
 {
    let tp = setup_test_plugin().await;
    let (launcher_ready, partial_delivered, waiter_started) =
        (Gate::new(), Gate::new(), Gate::new());

    let ready = launcher_ready.clone();
    let emit_node = emitting_function_node(
        "emitter",
        move |ctx: Context, _: Value, emit: Emit| {
            let ready = ready.clone();
            async move {
                let token = ctx.cancellation_token().clone();
                tokio::select! {
                    () = ready.wait() => {}
                    () = token.cancelled() => return Err::<Value, _>(ctx_err(&ctx)),
                }
                let mut partial = new_event(&ctx);
                partial.author = "wf_root".into();
                partial.response.partial = true;
                let _ = emit(partial).await;
                token.cancelled().await;
                Err(ctx_err(&ctx))
            }
        },
        NodeConfig::default(),
    );
    let started = waiter_started.clone();
    let waiter = agent_with("waiter", vec![], vec![], vec![], move |ctx| {
        let started = started.clone();
        generator(move |y| async move {
            started.close();
            ctx.cancellation_token().cancelled().await;
            y.send(Err(ctx_err(&ctx))).await;
        })
    });
    let (ready, delivered, sub) = (
        launcher_ready.clone(),
        partial_delivered.clone(),
        waiter.clone(),
    );
    let launcher = agent_with("launcher", vec![waiter], vec![], vec![], move |ctx| {
        let (ready, delivered, sub) = (ready.clone(), delivered.clone(), sub.clone());
        generator(move |y| async move {
            ready.close();
            let token = ctx.cancellation_token().clone();
            tokio::select! {
                () = delivered.wait() => {}
                () = token.cancelled() => {
                    y.send(Err(ctx_err(&ctx))).await;
                    return;
                }
            }
            let mut events = sub.run(ctx);
            while let Some(item) = events.next().await {
                let failed = item.is_err();
                if !y.send(item).await || failed {
                    return;
                }
            }
        })
    });
    let st = start();
    let wf_root = workflow_agent(
        "wf_root",
        vec![edge(&st, &emit_node), edge(&st, &agent_node(launcher))],
    );

    let r = new_test_runner(wf_root, vec![tp.plugin.clone()]);
    let mut events = r.run(
        "u1",
        "s1",
        Some(text_content("user", "go")),
        RunConfig::default(),
        RunOptions::default(),
    );
    let mut saw_partial = false;
    while let Some(item) = events.next().await {
        let ev = item.unwrap_or_else(|e| panic!("runner.Run error: {e}"));
        if ev.response.partial {
            partial_delivered.close();
            assert!(
                within(&waiter_started).await,
                "waiter did not start before consumer stop"
            );
            saw_partial = true;
            break;
        }
    }
    drop(events);
    assert!(saw_partial, "runner.Run yielded no partial event");

    let rows = RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await;
    require_invocation_completed_ok(&rows);
    assert_eq!(
        starts_of(&rows, "waiter"),
        1,
        "AGENT_STARTING rows for waiter; event types: {:?}",
        event_types(&rows)
    );
}

tokio::task_local! {
    /// Go `ctx.Value(concurrentFailRunKey{}) != nil`.
    static FAIL_RUN: ();
}

/// Whether the caller runs in the failing activation (Go `ctx.Value(concurrentFailRunKey{})
/// != nil`).
fn is_fail_run() -> bool {
    FAIL_RUN.try_with(|()| ()).is_ok()
}

/// Go `&concurrentBarrierLLM{name, fn}`: every request is answered by `f(request)`. `f` is
/// called when the call starts, so it reads [`is_fail_run`] of the caller.
fn concurrent_barrier_llm<F>(name: &str, f: F) -> Arc<dyn gadk_model::Llm>
where
    F: Fn(LlmRequest) -> BoxFuture<'static, Result<LlmResponse, ModelError>>
        + Send
        + Sync
        + 'static,
{
    fn_llm(name, move |req, _| futures::stream::once(f(req)).boxed())
}

// The model's `<-ctx.Done()` arms are dropped (the flow drops a cancelled call).
// parity: adapted: Go's ctx.Value(concurrentFailRunKey{}) is a task-local around the failing activation (gadk contexts carry no values)
#[tokio::test]
async fn test_trace_manager_retired_peer_trailer_does_not_clear_open_failing_peer_error() {
    const MODEL_ERR: &str = "quota exceeded on failing activation";
    for prior_tool_turn in [false, true] {
        let name = if prior_tool_turn {
            "after_prior_tool_turn"
        } else {
            "single_turn"
        };
        let tp = setup_test_plugin().await;
        let (started, in_model, fail_call_count) = (
            Arc::new(AtomicI32::new(0)),
            Arc::new(AtomicI32::new(0)),
            Arc::new(AtomicI32::new(0)),
        );
        let (first_started, both_in_model, peer_retired, fail_ready, trailer_delivered) = (
            Gate::new(),
            Gate::new(),
            Gate::new(),
            Gate::new(),
            Gate::new(),
        );

        let (both, ready) = (both_in_model.clone(), fail_ready.clone());
        let mdl = concurrent_barrier_llm("test-model", move |_| {
            let n = in_model.fetch_add(1, Ordering::SeqCst) + 1;
            let is_fail = is_fail_run();
            let (both, ready, fail_calls) = (both.clone(), ready.clone(), fail_call_count.clone());
            async move {
                if n == 2 {
                    both.close();
                } else if !within(&both).await {
                    return Err(ModelError::Other(
                        "timed out waiting for peer model call".into(),
                    ));
                }
                if prior_tool_turn && is_fail {
                    if fail_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                        return calls(vec![function_call_part("probe", json!({}))]);
                    }
                    return Err(ModelError::Other(MODEL_ERR.into()));
                }
                if is_fail {
                    return Err(ModelError::Other(MODEL_ERR.into()));
                }
                // Succeeding peer: wait for the failing peer to be ready, then return success
                // so it can retire while the failing peer is still open.
                if !within(&ready).await {
                    return Err(ModelError::Other(
                        "timed out waiting for failing activation".into(),
                    ));
                }
                final_text("peer ok")
            }
            .boxed()
        });
        let first = first_started.clone();
        let retired = peer_retired.clone();
        let mut worker_cfg = LlmAgentConfig {
            name: "worker".into(),
            model: Some(mdl),
            before_agent_callbacks: vec![agent_cb(move |_| {
                if started.fetch_add(1, Ordering::SeqCst) == 0 {
                    first.close();
                }
                async { Ok(None) }
            })],
            after_agent_callbacks: vec![agent_cb(move |_| {
                let retired = retired.clone();
                let fail = is_fail_run();
                async move {
                    if fail {
                        return Ok(None);
                    }
                    // Signal that the successful peer retired; its trailer event arrives after
                    // retirement.
                    retired.close();
                    Ok(Some(text_content("model", "peer trailer")))
                }
            })],
            ..Default::default()
        };
        if prior_tool_turn {
            worker_cfg.tools = vec![function_tool(
                "probe",
                "Probes before failing",
                |_, _| async { Ok(json!({"ok": true}).as_object().cloned().unwrap()) },
            )];
        }
        let worker = llm_agent(worker_cfg);
        let (sub, ready, delivered) = (
            worker.clone(),
            fail_ready.clone(),
            trailer_delivered.clone(),
        );
        let parent = agent_with("parent", vec![worker], vec![], vec![], move |ctx| {
            let (sub, ready, delivered) = (sub.clone(), ready.clone(), delivered.clone());
            generator(move |y| async move {
                type Sent = (Item, Option<oneshot::Sender<bool>>);
                let (ch, mut items) = mpsc::channel::<Sent>(8);
                let done = Gate::new();
                let _close_done = CloseOnDrop(done.clone());
                let err1 = Arc::new(parking_lot::Mutex::new(None::<AgentError>));
                // Failing activation.
                let (fail_sub, fail_ctx, fail_err, fail_ch) =
                    (sub.clone(), ctx.clone(), err1.clone(), ch.clone());
                go(FAIL_RUN.scope((), async move {
                    let _ready = CloseOnDrop(ready);
                    let _wg = fail_ch;
                    let mut events = fail_sub.run(fail_ctx);
                    while let Some(item) = events.next().await {
                        if let Err(e) = item {
                            let mut first = fail_err.lock();
                            if first.is_none() {
                                *first = Some(e);
                                break;
                            }
                        }
                    }
                }));
                // Succeeding activation.
                let (ok_sub, ok_ctx, ok_done) = (sub.clone(), ctx.clone(), done.clone());
                go(async move {
                    let mut events = ok_sub.run(ok_ctx);
                    while let Some(item) = events.next().await {
                        if item.is_ok() {
                            let (ack, acked) = oneshot::channel();
                            if ch.send((item, Some(ack))).await.is_err() {
                                return;
                            }
                            tokio::select! {
                                cont = acked => if !cont.unwrap_or(false) { return },
                                () = ok_done.wait() => return,
                            }
                            continue;
                        }
                        if ch.send((item, None)).await.is_err() {
                            return;
                        }
                    }
                });
                while let Some((item, ack)) = items.recv().await {
                    let trailer = matches!(&item, Ok(ev) if ev
                        .response
                        .content
                        .as_ref()
                        .and_then(|c| c.parts.first())
                        .is_some_and(|p| p.text == "peer trailer"));
                    let cont = y.send(item).await;
                    if let Some(ack) = ack {
                        let _ = ack.send(cont);
                    }
                    if trailer {
                        delivered.close();
                    }
                    if !cont {
                        return;
                    }
                }
                let first = err1.lock().take();
                if let Some(e) = first {
                    y.send(Err(e)).await;
                }
            })
        });

        let r = new_test_runner(parent, vec![tp.plugin.clone()]);
        let (_, errs) = run_until_error(&r, "go").await;
        let got = errs.first().map(ToString::to_string);
        assert_eq!(got.as_deref(), Some(MODEL_ERR), "{name}: runner.Run error");
        let rows = RowCursor::default()
            .collect(&tp, until_invocation_end)
            .await;
        let inv_err = only_row_of_type(&rows, EVENT_INVOCATION_ERROR);
        assert_eq!(
            str_col(&inv_err, "error_message"),
            MODEL_ERR,
            "{name}: INVOCATION_ERROR error_message"
        );
        assert_eq!(
            rows_of_type(&rows, EVENT_LLM_ERROR).len(),
            1,
            "{name}: LLM_ERROR rows; event types: {:?}",
            event_types(&rows)
        );
        // The retired peer's trailer must not have cleared the failing peer's error: the run
        // must end with INVOCATION_ERROR, not INVOCATION_COMPLETED.
        assert_eq!(
            rows_of_type(&rows, EVENT_INVOCATION_COMPLETED).len(),
            0,
            "{name}: INVOCATION_COMPLETED rows; the retired peer's trailer cleared the failing peer's error"
        );
        // Verify the trailer was delivered.
        assert!(
            within(&trailer_delivered).await,
            "{name}: peer trailer was never delivered"
        );
    }
}

/// The child of the two `FunctionNodeConcurrentChild*` tests: its first run's
/// BeforeAgentCallback closes `entered` and answers "cache hit"; the second one's closes
/// `second_entered`; a run answers "child answer".
fn cached_then_running_child(entered: &Gate, second_entered: &Gate) -> Arc<dyn Agent> {
    let calls = Arc::new(AtomicI32::new(0));
    let (entered, second_entered) = (entered.clone(), second_entered.clone());
    agent_with(
        "child",
        vec![],
        vec![agent_cb(move |_| {
            let answer = if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                entered.close();
                Some(text_content("model", "cache hit"))
            } else {
                second_entered.close();
                None
            };
            async move { Ok(answer) }
        })],
        vec![],
        |ctx| futures::stream::iter([Ok(answer(&ctx, "child", "child answer"))]).boxed(),
    )
}

/// The function node `fan` of the two `FunctionNodeConcurrentChild*` tests: runs `child` twice
/// on goroutines of its own, the second run once `entered` closed, the first one waiting after
/// each event until `second_entered` closed, and joins both. `own_scope` runs each child under
/// a `ctx.WithAgentCancel` context of its own.
fn fan_out_twice(
    child: Arc<dyn Agent>,
    entered: Gate,
    second_entered: Gate,
    own_scope: bool,
) -> Arc<dyn Node> {
    function_node(
        "fan",
        move |ctx: Context, _: Value| {
            let (child, entered, second_entered) =
                (child.clone(), entered.clone(), second_entered.clone());
            async move {
                // runChild runs child and, after each event, waits for wait unless it is None.
                let run_child = move |child: Arc<dyn Agent>, ctx: Context, wait: Option<Gate>| async move {
                    let (child_ctx, _cancel) = if own_scope {
                        let (c, g) = ctx.with_agent_cancel();
                        (c, Some(g))
                    } else {
                        (ctx, None)
                    };
                    let mut events = child.run(child_ctx.as_invocation().clone());
                    while let Some(item) = events.next().await {
                        item?;
                        let Some(w) = &wait else { continue };
                        if !within(w).await {
                            return Err(AgentError::other(
                                "the second run never entered its BeforeAgentCallback",
                            ));
                        }
                    }
                    Ok(())
                };
                let first = go(run_child(child.clone(), ctx.clone(), Some(second_entered)));
                let second = go(async move {
                    if !within(&entered).await {
                        return Err(AgentError::other(
                            "first run never entered its BeforeAgentCallback",
                        ));
                    }
                    run_child(child, ctx, None).await
                });
                let errs: Vec<String> = [first.await.unwrap(), second.await.unwrap()]
                    .into_iter()
                    .filter_map(|r| r.err().map(|e| e.to_string()))
                    .collect();
                if errs.is_empty() {
                    Ok("fanned out".to_owned())
                } else {
                    Err(AgentError::other(errs.join("\n")))
                }
            }
        },
        NodeConfig::default(),
    )
}

#[tokio::test]
async fn test_concurrency_race_function_node_concurrent_cached_child_is_not_retry() {
    let tp = setup_test_plugin().await;
    let (entered, second_entered) = (Gate::new(), Gate::new());
    let child = cached_then_running_child(&entered, &second_entered);
    let fan = fan_out_twice(child, entered, second_entered, false);
    let root = workflow_agent("root", chain(&[start(), fan]));

    let errs = run_errors(root, &tp, "go").await;
    assert!(errs.is_empty(), "runner.Run error: {errs:?}");
    let rows = RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await;
    require_invocation_completed_ok(&rows);
    assert_eq!(
        rows_of_type(&rows, EVENT_AGENT_ERROR).len(),
        0,
        "AGENT_ERROR rows; counts: {:?}",
        agent_error_counts(&rows)
    );
}

#[tokio::test]
async fn test_concurrency_race_function_node_concurrent_child_distinct_cancel_scopes() {
    let tp = setup_test_plugin().await;
    let (entered, second_entered) = (Gate::new(), Gate::new());
    let child = cached_then_running_child(&entered, &second_entered);
    let fan = fan_out_twice(child, entered, second_entered, true);
    let root = workflow_agent("root", chain(&[start(), fan]));

    let errs = run_errors(root, &tp, "go").await;
    assert!(errs.is_empty(), "runner.Run error: {errs:?}");
    let rows = RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await;
    require_invocation_completed_ok(&rows);
    assert_eq!(
        rows_of_type(&rows, EVENT_AGENT_ERROR).len(),
        0,
        "AGENT_ERROR rows; counts: {:?}",
        agent_error_counts(&rows)
    );
}

#[tokio::test]
async fn test_concurrency_race_function_node_returns_before_unjoined_cached_child() {
    let tp = setup_test_plugin().await;
    let (entered, root_done, child_done) = (Gate::new(), Gate::new(), Gate::new());
    let e = entered.clone();
    let child = agent_with(
        "cached_child",
        vec![],
        vec![agent_cb(move |_| {
            e.close();
            async { Ok(Some(text_content("model", "cached"))) }
        })],
        vec![],
        |_| futures::stream::empty().boxed(),
    );
    let (e, rd, cd) = (entered.clone(), root_done.clone(), child_done.clone());
    let fan = function_node(
        "fan",
        move |ctx: Context, _: Value| {
            let (child, entered, root_done, child_done) =
                (child.clone(), e.clone(), rd.clone(), cd.clone());
            async move {
                go(async move {
                    let _done = CloseOnDrop(child_done);
                    let mut events = child.run(ctx.as_invocation().clone());
                    while events.next().await.is_some() {
                        within(&root_done).await;
                    }
                });
                if !within(&entered).await {
                    return Err(AgentError::other(
                        "child did not enter its BeforeAgentCallback",
                    ));
                }
                Ok("started".to_owned())
            }
        },
        NodeConfig::default(),
    );
    let rd = root_done.clone();
    let root = gadk_engine::workflow_agent::new(gadk_engine::workflow_agent::WorkflowAgentConfig {
        name: "root".into(),
        edges: chain(&[start(), fan]),
        after_agent_callbacks: vec![agent_cb(move |_| {
            rd.close();
            async { Ok(None) }
        })],
        ..Default::default()
    })
    .expect("workflow agent");

    let errs = run_errors(root, &tp, "go").await;
    assert!(errs.is_empty(), "runner.Run error: {errs:?}");
    assert!(within(&child_done).await, "child did not exit");
    let rows = RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await;
    require_invocation_completed_ok(&rows);
    assert_eq!(
        rows_of_type(&rows, EVENT_AGENT_ERROR).len(),
        0,
        "AGENT_ERROR rows; counts: {:?}",
        agent_error_counts(&rows)
    );
}

#[tokio::test]
async fn test_concurrency_race_routed_node_started_again_while_running() {
    let tp = setup_test_plugin().await;
    let (first_started, second_entered) = (Gate::new(), Gate::new());
    // The scheduler keeps the runs of a node by the node's name, so it stops taking events once
    // either run of child completes, and the other run would then wait forever for its event to
    // be processed. yielded[i] is closed once the event of run i+1 was processed; neither run
    // returns before both were.
    let yielded = [Gate::new(), Gate::new()];
    let (calls, runs) = (Arc::new(AtomicI32::new(0)), Arc::new(AtomicI32::new(0)));
    let (c, se) = (calls.clone(), second_entered.clone());
    let (fs, se2) = (first_started.clone(), second_entered.clone());
    let child = agent_with(
        "child",
        vec![],
        vec![agent_cb(move |_| {
            if c.fetch_add(1, Ordering::SeqCst) == 1 {
                se.close();
            }
            async { Ok(None) }
        })],
        vec![],
        move |ctx| {
            let (runs, fs, se, yielded) = (runs.clone(), fs.clone(), se2.clone(), yielded.clone());
            generator(move |y| async move {
                let n = runs.fetch_add(1, Ordering::SeqCst) + 1;
                if n == 1 {
                    fs.close();
                    ctx.end_invocation();
                    within(&se).await;
                }
                if !y.send(Ok(answer(&ctx, "child", "answer"))).await || n > 2 {
                    return;
                }
                let n = usize::try_from(n).unwrap();
                yielded[n - 1].close();
                within(&yielded[2 - n]).await;
            })
        },
    );
    let c = wait_node(first_started, "the first run of child never started");
    let root = two_joins_root(agent_node(child), c);

    // Go logs the run's errors.
    let _ = run_errors(root, &tp, "go").await;
    assert_eq!(calls.load(Ordering::SeqCst), 2, "child started");
    let rows = RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await;
    assert_eq!(
        rows_of_type(&rows, EVENT_AGENT_ERROR).len(),
        0,
        "AGENT_ERROR rows; counts: {:?}",
        agent_error_counts(&rows)
    );
}

#[tokio::test]
async fn test_runner_function_node_nested_child_runs_emit_no_agent_error() {
    for (name, under_worker) in [
        ("function_node", false),
        ("parallel_worker_function_node", true),
    ] {
        let tp = setup_test_plugin().await;
        let child = new_one_event_agent("child", None);
        let nested_runs = Arc::new(AtomicI32::new(0));
        let nr = nested_runs.clone();
        let f = function_node(
            "fn",
            move |ctx: Context, _: Value| {
                let (child, nested_runs) = (child.clone(), nr.clone());
                async move {
                    let mut events = child.run(ctx.as_invocation().clone());
                    while let Some(item) = events.next().await {
                        item?;
                        if let Some(e) = first_error(child.run(ctx.as_invocation().clone())).await {
                            return Err(e);
                        }
                        nested_runs.fetch_add(1, Ordering::SeqCst);
                    }
                    Ok("nested".to_owned())
                }
            },
            NodeConfig::default(),
        );
        let root = workflow_agent("root", function_node_edges(f, under_worker));
        let errs = run_errors(root, &tp, "go").await;
        assert!(errs.is_empty(), "{name}: runner.Run error: {errs:?}");
        assert_eq!(
            nested_runs.load(Ordering::SeqCst),
            1,
            "{name}: nested runs of child"
        );
        let rows = RowCursor::default()
            .collect(&tp, until_invocation_end)
            .await;
        assert_eq!(
            starts_of(&rows, "child"),
            2,
            "{name}: AGENT_STARTING rows for child; event types: {:?}",
            event_types(&rows)
        );
        require_invocation_completed_ok(&rows);
        assert_eq!(
            rows_of_type(&rows, EVENT_AGENT_ERROR).len(),
            0,
            "{name}: AGENT_ERROR rows; counts: {:?}",
            agent_error_counts(&rows)
        );
    }
}

#[tokio::test]
async fn test_concurrency_race_function_node_resumes_child_after_nested_run() {
    let tp = setup_test_plugin().await;
    let (nested_done, other_ended) = (Gate::new(), Gate::new());
    let oe = other_ended.clone();
    let child = new_one_event_agent(
        "child",
        Some(Arc::new(move |ctx: InvocationContext, run: i32| {
            let oe = oe.clone();
            async move {
                if run != 1 {
                    return;
                }
                ctx.end_invocation();
                within(&oe).await;
            }
            .boxed()
        })),
    );
    let nd = nested_done.clone();
    let f = emitting_function_node(
        "fn",
        move |ctx: Context, _: Value, emit: Emit| {
            let (child, nested_done) = (child.clone(), nd.clone());
            async move {
                let mut nested = false;
                let mut events = child.run(ctx.as_invocation().clone());
                while let Some(item) = events.next().await {
                    emit(item?).await?;
                    if nested {
                        continue;
                    }
                    nested = true;
                    if let Some(e) = first_error(child.run(ctx.as_invocation().clone())).await {
                        return Err(e);
                    }
                    nested_done.close();
                }
                Ok(Value::Null)
            }
        },
        NodeConfig::default(),
    );
    let (nd, oe) = (nested_done.clone(), other_ended.clone());
    let other = agent_with(
        "other",
        vec![],
        vec![],
        // The agent's own AfterAgentCallbacks run after those of the plugins.
        vec![agent_cb(move |_| {
            oe.close();
            async { Ok(None) }
        })],
        move |ctx| {
            let nd = nd.clone();
            generator(move |y| async move {
                if !within(&nd).await {
                    y.send(Err(AgentError::other(
                        "the nested run of child never ended",
                    )))
                    .await;
                    return;
                }
                y.send(Ok(answer(&ctx, "other", "other answer"))).await;
            })
        },
    );
    let st = start();
    let root = workflow_agent("root", vec![edge(&st, &f), edge(&st, &agent_node(other))]);
    let errs = run_errors(root, &tp, "go").await;
    assert!(errs.is_empty(), "runner.Run error: {errs:?}");
    let rows = RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await;
    require_invocation_completed_ok(&rows);
    assert_eq!(
        rows_of_type(&rows, EVENT_AGENT_ERROR).len(),
        0,
        "AGENT_ERROR rows; counts: {:?}",
        agent_error_counts(&rows)
    );
}

#[tokio::test]
async fn test_runner_function_node_reruns_child_without_retry_config_keeps_failure() {
    for (name, node, under_worker) in [
        ("function_node", "fn", false),
        ("function_node_named_like_child", "child", false),
        ("parallel_worker_function_node", "fn", true),
    ] {
        let tp = setup_test_plugin().await;
        let child = llm_agent(LlmAgentConfig {
            name: "child".into(),
            model: Some(flaky_on_first_pass_llm(
                "child model failure 503",
                "child answer",
            )),
            mode: Mode::SingleTurn,
            ..Default::default()
        });
        let recovered = Arc::new(AtomicBool::new(false));
        let rec = recovered.clone();
        let f = function_node(
            node,
            move |ctx: Context, _: Value| {
                let (child, recovered) = (child.clone(), rec.clone());
                async move {
                    let mut events = child.run(ctx.as_invocation().clone());
                    while let Some(item) = events.next().await {
                        if item.is_err() {
                            break;
                        }
                    }
                    drop(events);
                    if let Some(e) = first_error(child.run(ctx.as_invocation().clone())).await {
                        return Err(e);
                    }
                    recovered.store(true, Ordering::SeqCst);
                    Ok("the function node handled the child error".to_owned())
                }
            },
            NodeConfig::default(),
        );
        let root = workflow_agent("root", function_node_edges(f, under_worker));
        let errs = run_errors(root, &tp, "go").await;
        assert!(errs.is_empty(), "{name}: runner.Run error: {errs:?}");
        assert!(
            recovered.load(Ordering::SeqCst),
            "{name}: the function node did not recover from the child error"
        );
        let rows = RowCursor::default()
            .collect(&tp, until_invocation_end)
            .await;
        assert_eq!(
            rows_of_type(&rows, EVENT_INVOCATION_COMPLETED).len(),
            0,
            "{name}: INVOCATION_COMPLETED rows; event types: {:?}",
            event_types(&rows)
        );
        only_row_of_type(&rows, EVENT_INVOCATION_ERROR);
        assert_eq!(
            agent_error_counts(&rows).get("child").copied().unwrap_or(0),
            1,
            "{name}: AGENT_ERROR rows for child; counts: {:?}",
            agent_error_counts(&rows)
        );
    }
}

#[tokio::test]
async fn test_concurrency_race_reported_node_outcome_does_not_end_running_run() {
    let tp = setup_test_plugin().await;
    let (reported, second_answered) = (Gate::new(), Gate::new());
    let (calls, runs) = (Arc::new(AtomicI32::new(0)), Arc::new(AtomicI32::new(0)));
    let c = calls.clone();
    let (rp, sa) = (reported.clone(), second_answered.clone());
    let child = agent_with(
        "child",
        vec![],
        vec![agent_cb(move |_| {
            c.fetch_add(1, Ordering::SeqCst);
            async { Ok(None) }
        })],
        vec![],
        move |ctx| {
            let (runs, rp, sa) = (runs.clone(), rp.clone(), sa.clone());
            generator(move |y| async move {
                let n = runs.fetch_add(1, Ordering::SeqCst) + 1;
                let mut ev = answer(&ctx, "child", "answer");
                if n == 1 {
                    report_failed_node(&ctx, &mut ev);
                }
                if !y.send(Ok(ev)).await {
                    return;
                }
                match n {
                    1 => {
                        rp.close();
                        ctx.end_invocation();
                        within(&sa).await;
                    }
                    2 => sa.close(),
                    _ => {}
                }
            })
        },
    );
    let c = wait_node(reported, "the first run of child never answered");
    let root = two_joins_root(agent_node(child), c);

    // Go logs the run's errors.
    let _ = run_errors(root, &tp, "go").await;
    assert_eq!(calls.load(Ordering::SeqCst), 2, "child started");
    let rows = RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await;
    assert_eq!(
        rows_of_type(&rows, EVENT_AGENT_ERROR).len(),
        0,
        "AGENT_ERROR rows; counts: {:?}; event types: {:?}",
        agent_error_counts(&rows),
        event_types(&rows)
    );
}

#[tokio::test]
async fn test_runner_retried_parallel_node_with_silent_branch_failure_emits_branch_agent_error() {
    let tp = setup_test_plugin().await;
    let attempts = Arc::new(AtomicI32::new(0));
    let silent = agent_with("silent", vec![], vec![], vec![], move |ctx| {
        let item = if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(AgentError::other("silent failed before any event"))
        } else {
            Ok(answer(&ctx, "silent", "silent answered on retry"))
        };
        futures::stream::iter([item]).boxed()
    });
    let node: Arc<dyn Node> = AgentNode::new(
        new_parallel_agent("par", vec![silent]),
        NodeConfig {
            retry_config: Some(RetryConfig {
                max_attempts: 2,
                initial_delay: Duration::from_millis(1),
                max_delay: Duration::from_millis(1),
                backoff_factor: 1.0,
                ..Default::default()
            }),
            ..Default::default()
        },
    )
    .expect("agent node");
    let wf_root = workflow_agent("wf_root", vec![edge(&start(), &node)]);

    let errs = run_errors(wf_root, &tp, "go").await;
    assert!(errs.is_empty(), "runner.Run error: {errs:?}");
    let rows = RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await;
    assert_eq!(
        rows_of_type(&rows, EVENT_INVOCATION_ERROR).len(),
        0,
        "got INVOCATION_ERROR, want INVOCATION_COMPLETED; event types: {:?}",
        event_types(&rows)
    );
    only_row_of_type(&rows, EVENT_INVOCATION_COMPLETED);
    assert_eq!(
        agent_error_counts(&rows),
        BTreeMap::from([("par".to_owned(), 1), ("silent".to_owned(), 1)]),
        "AGENT_ERROR rows per agent; event types: {:?}",
        event_types(&rows)
    );
}

#[tokio::test]
async fn test_concurrency_race_finish_task_ends_the_run_that_made_the_call() {
    let tp = setup_test_plugin().await;
    let (model, model_called) = finish_task_llm();
    let finish_called = Gate::new();
    let starts = Arc::new(AtomicI32::new(0));
    let (st, fc) = (starts.clone(), finish_called.clone());
    let fc2 = finish_called.clone();
    let task = llm_agent(LlmAgentConfig {
        name: "task".into(),
        mode: Mode::Task,
        model: Some(model),
        before_agent_callbacks: vec![agent_cb(move |_| {
            let second = st.fetch_add(1, Ordering::SeqCst) == 1;
            let fc = fc.clone();
            async move {
                if !second {
                    return Ok(None);
                }
                within(&fc).await;
                Ok(Some(text_content("model", "cached")))
            }
        })],
        after_tool_callbacks: vec![Arc::new(
            move |_ctx: Context, t: Arc<dyn Tool>, _args, _res, _err| {
                if t.name() == "finish_task" {
                    fc2.close();
                }
                async { Ok(None) }.boxed()
            },
        )],
        ..Default::default()
    });
    let root = agent_with("root", vec![], vec![], vec![], move |ctx| {
        let (task, model_called) = (task.clone(), model_called.clone());
        generator(move |y| async move {
            let node_ctx = ctx.into_context();
            let (results, mut rx) = mpsc::channel::<Item>(1);
            let run_task = {
                let (task, node_ctx) = (task.clone(), node_ctx.clone());
                move |results: mpsc::Sender<Item>| async move {
                    let mut events = run_llm_agent_as_node(node_ctx, task, json!("do the task"));
                    while let Some(item) = events.next().await {
                        if results.send(item).await.is_err() {
                            return;
                        }
                    }
                }
            };
            go(run_task.clone()(results.clone()));
            go(async move {
                within(&model_called).await;
                run_task(results).await;
            });
            let mut reading = true;
            while let Some(item) = rx.recv().await {
                if reading {
                    reading = y.send(item).await;
                }
            }
        })
    });

    // Go logs the run's errors.
    let _ = run_errors(root, &tp, "go").await;
    assert_eq!(starts.load(Ordering::SeqCst), 2, "task started");
    let rows = RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await;
    let requests = agent_rows_of_type(&rows, EVENT_LLM_REQUEST, "task");
    assert_eq!(
        requests.len(),
        1,
        "task LLM_REQUEST rows (only the first run calls its model); event types: {:?}",
        event_types(&rows)
    );
    let first = place_of(&requests[0]).parent;
    assert!(
        agent_rows_of_type(&rows, EVENT_AGENT_COMPLETED, "task")
            .iter()
            .any(|row| place_of(row).span == first),
        "no AGENT_COMPLETED row ends span {first:?} of the run that made the finish_task call; event types: {:?}",
        event_types(&rows)
    );
}

#[tokio::test]
async fn test_runner_reported_node_outcome_before_delayed_response_emits_no_agent_error() {
    let tp = setup_test_plugin().await;
    let (reported, second_started, first_answered) = (Gate::new(), Gate::new(), Gate::new());
    let (calls, runs) = (Arc::new(AtomicI32::new(0)), Arc::new(AtomicI32::new(0)));
    let (c, ss) = (calls.clone(), second_started.clone());
    let (rp, ss2, fa) = (
        reported.clone(),
        second_started.clone(),
        first_answered.clone(),
    );
    let child = agent_with(
        "child",
        vec![],
        vec![agent_cb(move |_| {
            if c.fetch_add(1, Ordering::SeqCst) == 1 {
                ss.close();
            }
            async { Ok(None) }
        })],
        vec![],
        move |ctx| {
            let (runs, rp, ss, fa) = (runs.clone(), rp.clone(), ss2.clone(), fa.clone());
            generator(move |y| async move {
                if runs.fetch_add(1, Ordering::SeqCst) != 0 {
                    within(&fa).await;
                    return;
                }
                let _answered = CloseOnDrop(fa);
                let mut report = new_event(&ctx);
                report.author = "child".into();
                report.branch = ctx.branch();
                report_failed_node(&ctx, &mut report);
                if !y.send(Ok(report)).await {
                    return;
                }
                rp.close();
                within(&ss).await;
                y.send(Ok(answer(&ctx, "child", "answer"))).await;
            })
        },
    );
    let c = wait_node(reported, "the first run of child never reported");
    let root = two_joins_root(agent_node(child), c);

    let errs = run_errors(root, &tp, "go").await;
    assert!(errs.is_empty(), "runner.Run error: {errs:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 2, "child started");
    let rows = RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await;
    require_invocation_completed_ok(&rows);
    assert_eq!(
        rows_of_type(&rows, EVENT_AGENT_ERROR).len(),
        0,
        "AGENT_ERROR rows for two eventually successful runs; counts: {:?}; event types: {:?}",
        agent_error_counts(&rows),
        event_types(&rows)
    );
}

/// Go `newTraceOnlyToolPlugin(t, tm)`: [`new_trace_only_plugin`], which registers the tool
/// callbacks too.
fn new_trace_only_tool_plugin(tm: Arc<TraceManager>) -> gadk_agent::Plugin {
    new_trace_only_plugin(tm)
}

/// Go `&toolThenTextLLM{toolName}` (model name `tool-then-text-model`): calls `tool` until the
/// request ends with a function response, then answers "done".
fn tool_then_text_llm(tool: &str) -> Arc<dyn gadk_model::Llm> {
    let tool = tool.to_owned();
    fn_llm("tool-then-text-model", move |req, _| {
        let responded = req
            .contents
            .last()
            .is_some_and(|c| c.parts.iter().any(|p| p.function_response.is_some()));
        let item = if responded {
            final_text("done")
        } else {
            calls(vec![function_call_part(&tool, json!({}))])
        };
        futures::stream::iter([item]).boxed()
    })
}

/// Go `&transferToLLM{target}` (model name `transfer-to-model`).
fn transfer_to_llm(target: &str) -> Arc<dyn gadk_model::Llm> {
    let target = target.to_owned();
    fn_llm("transfer-to-model", move |_, _| {
        futures::stream::iter([transfer_to(&target)]).boxed()
    })
}

/// Go `runOrdinaryBot(t, p)`: runs an agent whose model answers at once through a runner with
/// `p`; the run's invocation ID.
async fn run_ordinary_bot(p: &gadk_agent::Plugin) -> String {
    let bot = llm_agent(LlmAgentConfig {
        name: "bot".into(),
        model: Some(scripted_llm(vec![final_text("ok")])),
        ..Default::default()
    });
    let r = new_test_runner(bot, vec![p.clone()]);
    let (events, errs) = run_turn(&r, "u1", "ordinary", "hi", RunConfig::default()).await;
    assert!(errs.is_empty(), "ordinary run: {errs:?}");
    events
        .first()
        .map(|ev| ev.invocation_id.clone())
        .unwrap_or_default()
}

/// Go `checkLateLookupFindsCompletedRun(t, tm, invID, author)`.
fn check_late_lookup_finds_completed_run(tm: &TraceManager, inv_id: &str, author: &str) {
    let want = tm
        .mu
        .lock()
        .completed
        .get(inv_id)
        .map(|c| (c.identity.trace_id.clone(), c.identity.span_id.clone()));
    let Some((want_trace, want_span)) = want.filter(|_| !inv_id.is_empty()) else {
        panic!("completed run {inv_id:?} lost its identity at its own completion");
    };
    let span = tm.event_span(
        &CallbackCtx::background(),
        &Identity {
            invocation_id: inv_id.into(),
            agent_name: author.into(),
            ..Default::default()
        },
        author,
        "",
    );
    let active = tm.mu.lock().invocations.contains_key(inv_id);
    assert!(
        !active && span.trace_id == want_trace && span.span_id == want_span,
        "late event place = (trace {:?}, span {:?}), active invocation started = {active}; want the completed run's (trace {want_trace:?}, span {want_span:?}) and none started",
        span.trace_id,
        span.span_id
    );
}

/// The root agents of `TestRunner_CallsWithoutCallbacksHoldNoCompletedRuns`.
#[derive(Clone, Copy)]
enum GuardedRoot {
    Guarded,
    Sequential,
    Parallel,
    Workflow,
    CustomInLine,
    CustomGoroutine,
    Transfer,
    Plain,
    Cached,
    Budgeted,
    Tooled,
}

/// Go's `refuse`: a BeforeModelCallback failing with "blocked by guardrail".
fn refuse() -> gadk_agent::BeforeModelCallback {
    Arc::new(|_ctx, req| {
        async move { (req, Err(AgentError::other("blocked by guardrail"))) }.boxed()
    })
}

/// Go's `lookup` tool.
fn lookup_tool() -> Arc<dyn Tool> {
    function_tool("lookup", "Looks something up", |_, _| async {
        Ok(Map::new())
    })
}

/// Go's `newGuarded`.
fn new_guarded() -> Arc<dyn Agent> {
    llm_agent(LlmAgentConfig {
        name: "guarded".into(),
        model: Some(tool_then_text_llm("lookup")),
        before_model_callbacks: vec![refuse()],
        ..Default::default()
    })
}

impl GuardedRoot {
    fn build(self) -> Arc<dyn Agent> {
        match self {
            GuardedRoot::Guarded => new_guarded(),
            GuardedRoot::Sequential => sequential("pipeline", vec![new_guarded()]),
            GuardedRoot::Parallel => new_parallel_agent("fanout", vec![new_guarded()]),
            GuardedRoot::Workflow => {
                workflow_agent("graph", vec![edge(&start(), &agent_node(new_guarded()))])
            }
            GuardedRoot::CustomInLine => {
                let guarded = new_guarded();
                let sub = guarded.clone();
                custom_agent("custom", vec![guarded], move |ctx| {
                    let sub = sub.clone();
                    generator(move |y| async move {
                        let mut events = sub.run(ctx);
                        while let Some(item) = events.next().await {
                            if !y.send(item).await {
                                return;
                            }
                        }
                    })
                })
            }
            GuardedRoot::CustomGoroutine => {
                let guarded = new_guarded();
                let sub = guarded.clone();
                custom_agent("custom", vec![guarded], move |ctx| {
                    let sub = sub.clone();
                    generator(move |y| async move {
                        let (results, mut rx) = mpsc::channel::<Item>(1);
                        go(async move {
                            let mut events = sub.run(ctx);
                            while let Some(item) = events.next().await {
                                if results.send(item).await.is_err() {
                                    return;
                                }
                            }
                        });
                        let mut reading = true;
                        while let Some(item) = rx.recv().await {
                            if reading {
                                reading = y.send(item).await;
                            }
                        }
                    })
                })
            }
            GuardedRoot::Transfer => llm_agent(LlmAgentConfig {
                name: "coordinator".into(),
                model: Some(transfer_to_llm("guarded")),
                sub_agents: vec![new_guarded()],
                ..Default::default()
            }),
            GuardedRoot::Plain => llm_agent(LlmAgentConfig {
                name: "guarded".into(),
                model: Some(tool_then_text_llm("lookup")),
                ..Default::default()
            }),
            GuardedRoot::Cached => llm_agent(LlmAgentConfig {
                name: "cached".into(),
                model: Some(tool_then_text_llm("lookup")),
                ..Default::default()
            }),
            GuardedRoot::Budgeted => llm_agent(LlmAgentConfig {
                name: "budgeted".into(),
                model: Some(tool_then_text_llm("lookup")),
                tools: vec![lookup_tool()],
                ..Default::default()
            }),
            GuardedRoot::Tooled => llm_agent(LlmAgentConfig {
                name: "tooled".into(),
                model: Some(tool_then_text_llm("lookup")),
                tools: vec![lookup_tool()],
                ..Default::default()
            }),
        }
    }
}

#[tokio::test]
async fn test_runner_calls_without_callbacks_hold_no_completed_runs() {
    let guardrail = || {
        plugin_named(
            "guardrail",
            gadk_agent::PluginConfig {
                before_model_callback: Some(refuse()),
                ..Default::default()
            },
        )
    };
    let cache = || {
        plugin_named(
            "cache",
            gadk_agent::PluginConfig {
                before_model_callback: Some(Arc::new(|_ctx, req| {
                    async move {
                        let res = LlmResponse {
                            content: Some(text_content("model", "cached")),
                            turn_complete: true,
                            ..Default::default()
                        };
                        (req, Ok(Some(res)))
                    }
                    .boxed()
                })),
                ..Default::default()
            },
        )
    };
    let tool_guardrail = || {
        plugin_named(
            "tool-guardrail",
            gadk_agent::PluginConfig {
                before_tool_callback: Some(Arc::new(|_ctx, _tool, _args| {
                    async { Err(AgentError::other("tool blocked")) }.boxed()
                })),
                ..Default::default()
            },
        )
    };
    type Later = Option<fn() -> gadk_agent::Plugin>;
    let cases: Vec<(&str, GuardedRoot, Later, i32)> = vec![
        (
            "agent's BeforeModelCallback returns an error",
            GuardedRoot::Guarded,
            None,
            0,
        ),
        (
            "agent's BeforeModelCallback in a sub-agent of a sequential agent returns an error",
            GuardedRoot::Sequential,
            None,
            0,
        ),
        (
            "agent's BeforeModelCallback in a branch of a parallel agent returns an error",
            GuardedRoot::Parallel,
            None,
            0,
        ),
        (
            "agent's BeforeModelCallback in a node of a workflow agent returns an error",
            GuardedRoot::Workflow,
            None,
            0,
        ),
        (
            "agent's BeforeModelCallback in a sub-agent that a custom agent runs in line returns an error",
            GuardedRoot::CustomInLine,
            None,
            0,
        ),
        (
            "agent's BeforeModelCallback in a sub-agent that a custom agent runs on a goroutine and waits for returns an error",
            GuardedRoot::CustomGoroutine,
            None,
            0,
        ),
        (
            "agent's BeforeModelCallback in an agent that the root agent transfers to returns an error",
            GuardedRoot::Transfer,
            None,
            0,
        ),
        (
            "later plugin's BeforeModelCallback returns an error",
            GuardedRoot::Plain,
            Some(guardrail),
            0,
        ),
        (
            "later plugin's BeforeModelCallback returns a response",
            GuardedRoot::Cached,
            Some(cache),
            0,
        ),
        ("model-call budget runs out", GuardedRoot::Budgeted, None, 1),
        (
            "later plugin's BeforeToolCallback returns an error",
            GuardedRoot::Tooled,
            Some(tool_guardrail),
            0,
        ),
    ];
    for (case, root, later, max_llm_calls) in cases {
        for (mode, stop_at_error) in [
            ("caller reads to the end", false),
            ("caller stops at the first error", true),
        ] {
            let name = format!("{case}/{mode}");
            let cfg = RunConfig {
                max_llm_calls,
                ..Default::default()
            };
            let mut tm = TraceManager::new();
            tm.max_tracked = 1;
            let limit = tm.max_tracked.max(64);
            let tm = Arc::new(tm);
            let p = new_trace_only_tool_plugin(tm.clone());
            let mut plugins = vec![p.clone()];
            if let Some(later) = later {
                plugins.push(later());
            }
            let r = new_test_runner(root.build(), plugins);
            for i in 0..limit + 6 {
                run_until(&r, "u1", &format!("s{i}"), "hi", cfg.clone(), |item| {
                    stop_at_error && item.is_err()
                })
                .await;
            }
            let waiting = tm
                .mu
                .lock()
                .completed
                .values()
                .filter(|c| c.has_pending_or_open_late_spans())
                .count();
            assert_eq!(waiting, 0, "{name}: completed runs waiting for late calls");
            let inv_id = run_ordinary_bot(&p).await;
            check_late_lookup_finds_completed_run(&tm, &inv_id, "bot");
        }
    }
}

/// The roots of `TestRunner_UnjoinedSubAgentCallOutlivesAfterRun`: the agent that the runner
/// runs, which calls `start_sub` and then returns once `wait` returns, without waiting for the
/// run that `start_sub` begins.
#[derive(Clone, Copy)]
enum UnjoinedRoot {
    Custom,
    FunctionNode,
}

#[tokio::test]
async fn test_runner_unjoined_sub_agent_call_outlives_after_run() {
    for (name, kind) in [
        ("custom agent", UnjoinedRoot::Custom),
        (
            "function node of a workflow agent",
            UnjoinedRoot::FunctionNode,
        ),
    ] {
        let tm = Arc::new(TraceManager::new());
        let p = new_trace_only_plugin(tm.clone());
        let gate = blocking_llm(1);
        let sub = llm_agent(LlmAgentConfig {
            name: "sub".into(),
            model: Some(gate.model.clone()),
            ..Default::default()
        });
        let done = Gate::new();
        let release = gate.release.clone();
        let entered = Arc::new(tokio::sync::Mutex::new(gate.entered));
        let start_sub = {
            let (sub, done) = (sub.clone(), done.clone());
            move |ctx: InvocationContext| {
                let (sub, done) = (sub.clone(), done.clone());
                go(async move {
                    let _done = CloseOnDrop(done);
                    let mut events = sub.run(ctx);
                    while events.next().await.is_some() {}
                });
            }
        };
        // Return once the model call started, without waiting for it.
        let wait = move || {
            let entered = entered.clone();
            async move {
                entered.lock().await.recv().await;
            }
        };
        let root = match kind {
            UnjoinedRoot::Custom => custom_agent("root", vec![sub], move |ctx| {
                let (start_sub, wait) = (start_sub.clone(), wait.clone());
                async_stream_of(async move {
                    start_sub(ctx);
                    wait().await;
                    Vec::new()
                })
            }),
            UnjoinedRoot::FunctionNode => {
                let fan = function_node(
                    "fan",
                    move |ctx: Context, _: Value| {
                        let (start_sub, wait) = (start_sub.clone(), wait.clone());
                        async move {
                            start_sub(ctx.as_invocation().clone());
                            wait().await;
                            Ok::<_, AgentError>("started".to_owned())
                        }
                    },
                    NodeConfig::default(),
                );
                workflow_agent("root", chain(&[start(), fan]))
            }
        };
        let r = new_test_runner(root, vec![p]);
        let (events, _) = run_turn(&r, "u1", "s1", "hi", RunConfig::default()).await;
        let mut inv_id = events
            .first()
            .map(|ev| ev.invocation_id.clone())
            .unwrap_or_default();
        let late = {
            let st = tm.mu.lock();
            if inv_id.is_empty()
                && let Some((back, _)) = st.completion_order.back()
            {
                inv_id = back.clone();
            }
            st.completed.get(&inv_id).and_then(|c| c.late.clone())
        };
        let Some(late) = late.filter(|l| l.has_open_calls()) else {
            panic!("{name}: completed run {inv_id:?} handed no open model call to the late state");
        };

        release.close();
        assert!(
            within(&done).await,
            "{name}: the unjoined sub-agent never finished"
        );
        let active = tm.mu.lock().invocations.contains_key(&inv_id);
        assert!(
            !late.has_open_calls() && !active,
            "{name}: after the unjoined model call ended: call still open in the late state = {}, active invocation started = {active}; want false, false",
            late.has_open_calls()
        );
    }
}

#[tokio::test]
async fn test_repro_parallel_worker_guardrail_pins_nothing() {
    let mut tm = TraceManager::new();
    tm.max_tracked = 1;
    let limit = tm.max_tracked.max(64);
    let tm = Arc::new(tm);
    let p = new_trace_only_plugin(tm.clone());
    let guarded = llm_agent(LlmAgentConfig {
        name: "guarded".into(),
        model: Some(scripted_llm(vec![])),
        before_model_callbacks: vec![refuse()],
        ..Default::default()
    });
    let items = function_node(
        "items",
        |_: Context, _: Value| async { Ok::<_, AgentError>(vec!["a".to_owned(), "b".to_owned()]) },
        NodeConfig::default(),
    );
    let per_item: Arc<dyn Node> =
        ParallelWorker::new("per_item", agent_node(guarded), 0, NodeConfig::default())
            .expect("parallel worker");
    let root = workflow_agent("root", chain(&[start(), items, per_item]));
    let r = new_test_runner(root, vec![p.clone()]);
    let mut errs = 0;
    for i in 0..limit + 6 {
        let (_, run_errs) = run_turn(&r, "u1", &format!("s{i}"), "hi", RunConfig::default()).await;
        errs += run_errs.len();
    }
    let pinned = tm
        .mu
        .lock()
        .completed
        .values()
        .filter(|c| c.late.as_ref().is_some_and(|l| l.has_open_calls()))
        .count();
    assert!(errs > 0, "no run yielded an error: the guardrail never ran");
    let inv_id = run_ordinary_bot(&p).await;
    let kept = tm.mu.lock().completed.contains_key(&inv_id);
    assert!(
        pinned == 0 && kept,
        "runs pinned by calls with no callback = {pinned}, want 0; ordinary identity kept = {kept}, want true"
    );
}

#[tokio::test]
async fn test_runner_reported_node_outcome_before_delayed_response_call_in_flight_emits_no_agent_error()
 {
    for (name, tool_call) in [("model call", false), ("tool call", true)] {
        let tp = setup_test_plugin().await;
        let (reported, first_done) = (Gate::new(), Gate::new());
        let (calls, runs) = (Arc::new(AtomicI32::new(0)), Arc::new(AtomicI32::new(0)));
        // Go `&blockingLLM{entered: make(chan struct{}, 1), release: secondStarted}`.
        let blocking = blocking_llm(1);
        let second_started = blocking.release.clone();
        let mut helper_cfg = LlmAgentConfig {
            name: "helper".into(),
            model: Some(blocking.model.clone()),
            ..Default::default()
        };
        if tool_call {
            let ss = second_started.clone();
            helper_cfg.tools = vec![function_tool(
                "wait",
                "Waits for the second run of child",
                move |_, _| {
                    let ss = ss.clone();
                    async move {
                        if within(&ss).await {
                            Ok(json!({"waited": true}).as_object().cloned().unwrap())
                        } else {
                            Err(ToolError::Execution(
                                "the second run of child never started".into(),
                            ))
                        }
                    }
                },
            )];
            helper_cfg.model = Some(scripted_llm(vec![
                super::runner_support::calls(vec![function_call_part_id(
                    "call-wait",
                    "wait",
                    json!({}),
                )]),
                final_text("answer"),
            ]));
        }
        let helper = llm_agent(helper_cfg);
        let (c, ss) = (calls.clone(), second_started.clone());
        let (rp, fd, sub) = (reported.clone(), first_done.clone(), helper.clone());
        let child = agent_with(
            "child",
            vec![helper],
            vec![agent_cb(move |_| {
                if c.fetch_add(1, Ordering::SeqCst) == 1 {
                    ss.close();
                }
                async { Ok(None) }
            })],
            vec![],
            move |ctx| {
                let (runs, rp, fd, sub) = (runs.clone(), rp.clone(), fd.clone(), sub.clone());
                generator(move |y| async move {
                    if runs.fetch_add(1, Ordering::SeqCst) != 0 {
                        within(&fd).await;
                        return;
                    }
                    let _done = CloseOnDrop(fd);
                    let mut report = new_event(&ctx);
                    report.author = "child".into();
                    report.branch = ctx.branch();
                    report_failed_node(&ctx, &mut report);
                    if !y.send(Ok(report)).await {
                        return;
                    }
                    rp.close();
                    let mut events = sub.run(ctx);
                    while let Some(item) = events.next().await {
                        if !y.send(item).await {
                            return;
                        }
                    }
                })
            },
        );
        let c = wait_node(reported, "the first run of child never reported");
        let root = two_joins_root(agent_node(child), c);

        let errs = run_errors(root, &tp, "go").await;
        assert!(errs.is_empty(), "{name}: runner.Run error: {errs:?}");
        assert_eq!(calls.load(Ordering::SeqCst), 2, "{name}: child started");
        let rows = RowCursor::default()
            .collect(&tp, until_invocation_end)
            .await;
        require_invocation_completed_ok(&rows);
        assert_eq!(
            rows_of_type(&rows, EVENT_AGENT_ERROR).len(),
            0,
            "{name}: AGENT_ERROR rows for eventually successful runs; counts: {:?}; event types: {:?}",
            agent_error_counts(&rows),
            event_types(&rows)
        );
        let mut pairs = vec![
            (EVENT_AGENT_STARTING, EVENT_AGENT_COMPLETED),
            (EVENT_LLM_REQUEST, EVENT_LLM_RESPONSE),
        ];
        if tool_call {
            pairs.push((EVENT_TOOL_STARTING, EVENT_TOOL_COMPLETED));
        }
        for (start_type, end_type) in pairs {
            let starts = agent_rows_of_type(&rows, start_type, "helper");
            let ends = agent_rows_of_type(&rows, end_type, "helper");
            assert!(
                !starts.is_empty() && starts.len() == ends.len(),
                "{name}: helper rows: {} {start_type} and {} {end_type}, want as many of each, at least one; event types: {:?}",
                starts.len(),
                ends.len(),
                event_types(&rows)
            );
            assert_eq!(
                place_of(&ends[0]),
                place_of(&starts[0]),
                "{name}: helper {end_type} is not at the place of its {start_type}"
            );
        }
    }
}

#[tokio::test]
async fn test_runner_finish_task_with_reused_call_id_ends_the_run_on_the_event_path() {
    let tp = setup_test_plugin().await;
    let (model, _called) = finish_task_llm();
    let task = llm_agent(LlmAgentConfig {
        name: "task".into(),
        mode: Mode::Task,
        model: Some(model),
        ..Default::default()
    });
    const PATHS: [&str; 2] = ["left/task@1", "right/task@1"];
    let root = agent_with("root", vec![], vec![], vec![], move |ctx| {
        let task = task.clone();
        generator(move |y| async move {
            let mut held = Vec::new();
            for path in PATHS {
                let node_ctx = ctx
                    .with_delta(ContextDelta {
                        path: Some(path.into()),
                        ..Default::default()
                    })
                    .into_context();
                let mut events =
                    run_llm_agent_as_node(node_ctx, task.clone(), json!("do the task"));
                while let Some(item) = events.next().await {
                    let mut ev = match item {
                        Ok(ev) => ev,
                        Err(e) => {
                            y.send(Err(e)).await;
                            return;
                        }
                    };
                    ev.node_info.get_or_insert_with(NodeInfo::default).path = path.into();
                    if has_task_completed_response(ev.response.content.as_ref()) {
                        held.push(ev);
                        continue;
                    }
                    if !y.send(Ok(ev)).await {
                        return;
                    }
                }
            }
            for ev in held {
                if !y.send(Ok(ev)).await {
                    return;
                }
            }
        })
    });

    let errs = run_errors(root, &tp, "go").await;
    assert!(errs.is_empty(), "runner.Run error: {errs:?}");
    let rows = RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await;
    require_invocation_completed_ok(&rows);
    let starts = agent_rows_of_type(&rows, EVENT_AGENT_STARTING, "task");
    let ends = agent_rows_of_type(&rows, EVENT_AGENT_COMPLETED, "task");
    assert!(
        starts.len() == PATHS.len() && ends.len() == PATHS.len(),
        "task rows: {} AGENT_STARTING and {} AGENT_COMPLETED, want {} of each; event types: {:?}",
        starts.len(),
        ends.len(),
        PATHS.len(),
        event_types(&rows)
    );
    for (i, path) in PATHS.iter().enumerate() {
        assert_eq!(
            place_of(&ends[i]).span,
            place_of(&starts[i]).span,
            "AGENT_COMPLETED row {i}, for the finish_task response on {path}, does not end the span of the run on that path"
        );
    }
}
