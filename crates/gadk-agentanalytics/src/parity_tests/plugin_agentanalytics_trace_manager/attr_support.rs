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

//! Helpers shared by the goroutine-attribution ports of ADK-Go
//! `plugin/agentanalytics/trace_manager_test.go` (the `attr_*` files), over
//! [`runner_support`](super::runner_support).
//!
//! Go → gadk:
//! - goroutines: a test body's goroutine = [`on_test_goroutine`] (a `#[tokio::test]` body runs on
//!   no fiber, id 0, where Go's test goroutine has an id); `currentGoroutineID()` in a test body or
//!   a clock hook = [`goroutine_id`]; `go func() { … }()` = [`go`] (a tokio task: a distinct
//!   fallback fiber with a zero origin, as Go's test func literal is never a creator the trace
//!   manager matches); `go func() { for … range sub.Run(ctx) {…} }()` = [`go_run`];
//! - a `chan struct{}` that is closed once (`close(ch)` / `once.Do(func() { close(ch) })` /
//!   `<-ch`) = [`Gate`];
//! - the test models used by more than one group: `blockingLLM` = [`blocking_llm`],
//!   `flakyOnFirstPassLLM` = [`flaky_on_first_pass_llm`], `contextCanceledOnDoneLLM` =
//!   [`context_canceled_on_done_llm`], `streamThenWaitLLM` = [`stream_then_wait_llm`],
//!   `selfCancelingStreamLLM` = [`self_canceling_stream_llm`], `waitThenStreamPartialLLM` =
//!   [`wait_then_stream_partial_llm`], `waitDoneStreamLLM` = [`wait_done_stream_llm`],
//!   `gatedFlakyLLM` = [`gated_flaky_llm`], `finishTaskLLM` = [`finish_task_llm`],
//!   `partialThenWaitLLM` = [`partial_then_wait_llm`], `gatedLLM` = [`gated_llm`];
//! - builders: `newParallelAgent` = [`new_parallel_agent`], `workflowagent.New` =
//!   [`workflow_agent`], `workflow.NewAgentNode(a, workflow.NodeConfig{})` = [`agent_node`],
//!   `workflow.Start` = [`start`], `workflow.Chain` = `gadk_engine::workflow::chain`,
//!   `workflow.Edge{From, To}` = [`edge`]; Go's cached child (a custom agent whose
//!   BeforeAgentCallback closes a channel and answers) = [`cached_child`] /
//!   [`closing_before_agent`]; `runUnjoinedLateChild` = [`run_unjoined_late_child`];
//! - small Go idioms: `select { case <-ch: …; case <-time.After(10 * time.Second): … }` =
//!   [`within`] (real time; never fires in a passing run), `defer close(ch)` = [`CloseOnDrop`],
//!   `ctx.Err()` = [`ctx_err`], `map[string]any{key: value}` tool results = [`tool_result`], an
//!   event with model text on the context's branch = [`content_event`], `errSiblingFailed` =
//!   [`ERR_SIBLING_FAILED`].
//!
//! Runner-level helpers several groups share with the 7b-3a ports (`newCacheHitAgent`,
//! `requireCompletedWithHandledAgentError`, `requireChildStartedAfterPartialStop`, SSE run
//! config) are in [`runner_support`](super::runner_support); the late-state helpers
//! (`lateReviewTrace` / `lateReviewStack` / `lateReviewAgent` / `checkLateRecordDropped`) in
//! [`direct_support`](super::direct_support). Each group's file keeps its own Go-iterator helper
//! (`seq` / `go_iter` / `generator` / `yielding`): they differ in whether a `yield` may come from
//! another task and in what a stopped reader does to the body, which the file's tests rely on.
//!
//! Go's models read their request's `ctx`; gadk's `Llm::generate_content` has none. A Go model
//! that waits with `<-ctx.Done()` (then `yield(nil, ctx.Err())` or returns) is a stream that stays
//! pending after its earlier items ([`then_wait_for_cancel`]): when the agent's context ends,
//! the LLM flow ends the call itself and, as Go's flow does for the model's `ctx.Err()`, runs the
//! on-model-error callbacks with `context canceled` (`context deadline exceeded` on a deadline),
//! so the plugin writes Go's `LLM_ERROR` row; a model item that is already ready is still
//! yielded first.
//! Go's `requireADK*` capability probes are dropped: gadk-engine has those behaviours.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures::StreamExt;
use futures::stream::BoxStream;
use gadk_agent::fiber::{self, Fiber};
use gadk_agent::{
    AfterAgentCallback, Agent, AgentConfig, AgentError, BeforeAgentCallback, CancelCause, Context,
    InvocationContext, RunConfig,
};
use gadk_engine::workflow::{AgentNode, Edge, Node, NodeConfig, START};
use gadk_genai::{Content, FunctionCall, Part};
use gadk_model::{LlmResponse, ModelError};
use gadk_session::Event;
use serde_json::{Map, Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::runner_support::{RowCursor, final_text, fn_llm, new_event, partial_text};
use crate::event_types::EVENT_AGENT_ERROR;
use crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::{
    agent_error_counts, new_test_runner, require_invocation_completed_ok, rows_of_type, run_turn,
    setup_test_plugin, text_content, until_invocation_end,
};

/// Runs `fut` as Go runs a test body: on a goroutine of its own (a new root fiber), so
/// [`goroutine_id`] and the trace manager's `current_goroutine_id()` are non-zero in it.
pub(crate) async fn on_test_goroutine<F: Future>(fut: F) -> F::Output {
    Fiber::root().scope(fut).await
}

/// Go `currentGoroutineID()` called by a test (a test body, a clock hook): the id of the fiber
/// the caller runs on (0 outside any fiber and task).
pub(crate) fn goroutine_id() -> u64 {
    fiber::current().id
}

/// Go `go func() { … }()`: runs `fut` on a tokio task of its own (a distinct fallback fiber,
/// zero origin).
pub(crate) fn go<F>(fut: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    tokio::spawn(fut)
}

/// Go `go func() { for ev, err := range sub.Run(ctx) { … } }()`: runs `sub` in `ctx` on a task
/// of its own (the stream is created and polled there) and returns its items.
pub(crate) fn go_run(
    sub: Arc<dyn Agent>,
    ctx: InvocationContext,
) -> tokio::task::JoinHandle<Vec<Result<Event, AgentError>>> {
    go(async move { sub.run(ctx).collect().await })
}

/// Go's `chan struct{}` that is closed once and read by any number of waiters.
#[derive(Clone, Default, Debug)]
pub(crate) struct Gate(CancellationToken);

impl Gate {
    /// Go `make(chan struct{})`.
    pub(crate) fn new() -> Gate {
        Gate::default()
    }

    /// Go `close(ch)` (here idempotent, as Go's `once.Do(func() { close(ch) })`).
    pub(crate) fn close(&self) {
        self.0.cancel();
    }

    /// Whether the gate was closed (Go `select { case <-ch: … default: … }`).
    pub(crate) fn is_closed(&self) -> bool {
        self.0.is_cancelled()
    }

    /// Go `<-ch`: waits until the gate is closed.
    pub(crate) async fn wait(&self) {
        self.0.cancelled().await;
    }
}

/// `items`, then pending until dropped: Go's `<-ctx.Done()` after the model's earlier yields
/// (the LLM flow drops the call when the agent's context is cancelled; see the module docs).
pub(crate) fn then_wait_for_cancel(
    items: Vec<Result<LlmResponse, ModelError>>,
) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
    futures::stream::iter(items)
        .chain(futures::stream::pending())
        .boxed()
}

/// Go `&blockingLLM{entered, release}`: the model and the test's ends of its two channels.
pub(crate) struct BlockingLlm {
    pub(crate) model: Arc<dyn gadk_model::Llm>,
    /// Go `<-m.entered`: one item per call that entered `GenerateContent`.
    pub(crate) entered: mpsc::Receiver<()>,
    /// Go `close(m.release)`: lets every waiting and later call answer.
    pub(crate) release: Gate,
}

/// Go `&blockingLLM{entered: make(chan struct{}, capacity), release: make(chan struct{})}`
/// (model name `blocking-model`): each call sends on `entered`, waits for `release`, then
/// answers "worker done" (turn complete). Go's unbuffered `entered` (capacity 0) is a channel of
/// capacity 1 here: the call's send completes before the test receives it.
pub(crate) fn blocking_llm(capacity: usize) -> BlockingLlm {
    let (tx, entered) = mpsc::channel(capacity.max(1));
    let release = Gate::new();
    let gate = release.clone();
    let model = fn_llm("blocking-model", move |_, _| {
        let (tx, gate) = (tx.clone(), gate.clone());
        futures::stream::once(async move {
            let _ = tx.send(()).await;
            gate.wait().await;
            final_text("worker done")
        })
        .boxed()
    });
    BlockingLlm {
        model,
        entered,
        release,
    }
}

/// Go `&flakyOnFirstPassLLM{err: errors.New(err), reply}` (model name
/// `flaky-on-first-pass-llm`): call 1 fails with `err`, later calls answer `reply` (turn
/// complete).
pub(crate) fn flaky_on_first_pass_llm(err: &str, reply: &str) -> Arc<dyn gadk_model::Llm> {
    let (err, reply) = (err.to_owned(), reply.to_owned());
    let calls = AtomicUsize::new(0);
    fn_llm("flaky-on-first-pass-llm", move |_, _| {
        let item = if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(ModelError::Other(err.clone().into()))
        } else {
            final_text(&reply)
        };
        futures::stream::iter([item]).boxed()
    })
}

/// Go `&contextCanceledOnDoneLLM{entered: make(chan struct{})}` (model name
/// `context-canceled-on-done-llm`): closes `entered` once, then waits for its context (see the
/// module docs). Returns the model and `entered`.
pub(crate) fn context_canceled_on_done_llm() -> (Arc<dyn gadk_model::Llm>, Gate) {
    let entered = Gate::new();
    let gate = entered.clone();
    let model = fn_llm("context-canceled-on-done-llm", move |_, _| {
        gate.close();
        then_wait_for_cancel(vec![])
    });
    (model, entered)
}

/// Go `streamThenWaitLLM{acked}` (model name `stream-then-wait-llm`): a partial "streaming...",
/// then closes `acked` (if any) and waits for its context.
pub(crate) fn stream_then_wait_llm(acked: Option<Gate>) -> Arc<dyn gadk_model::Llm> {
    fn_llm("stream-then-wait-llm", move |_, _| {
        let acked = acked.clone();
        futures::stream::iter([partial_text("streaming...")])
            .chain(
                futures::stream::once(async move {
                    if let Some(a) = acked {
                        a.close();
                    }
                })
                .filter_map(|()| async { None }),
            )
            .chain(futures::stream::pending())
            .boxed()
    })
}

/// Go `&selfCancelingStreamLLM{}` (model name `self-canceling-stream-llm`): a partial "hello ",
/// then the error of a request context it canceled itself (Go `context.Canceled` = gadk
/// `AgentError::Cancelled`, as `runner_2`'s port of
/// `TestRunner_ModelCanceledOwnRequestAfterPartial_EmitsErrorTerminalRows`).
pub(crate) fn self_canceling_stream_llm() -> Arc<dyn gadk_model::Llm> {
    fn_llm("self-canceling-stream-llm", |_, _| {
        futures::stream::iter([
            partial_text("hello "),
            Err(ModelError::Other(Box::new(AgentError::Cancelled))),
        ])
        .boxed()
    })
}

/// Go `&waitThenStreamPartialLLM{ready, chunkText, finalText}` (model name
/// `wait-then-stream-partial-llm`): waits for `ready`, then a partial `chunk` and the final
/// `final_text` (turn complete).
pub(crate) fn wait_then_stream_partial_llm(
    ready: Gate,
    chunk: &str,
    final_answer: &str,
) -> Arc<dyn gadk_model::Llm> {
    let (chunk, final_answer) = (chunk.to_owned(), final_answer.to_owned());
    fn_llm("wait-then-stream-partial-llm", move |_, _| {
        let ready = ready.clone();
        let items = vec![partial_text(&chunk), final_text(&final_answer)];
        futures::stream::once(async move {
            ready.wait().await;
            futures::stream::iter(items)
        })
        .flatten()
        .boxed()
    })
}

/// Go `&waitDoneStreamLLM{waitCtx}` (model name `wait-done-stream-llm`): waits for `wait` (Go
/// `<-m.waitCtx().Done()`), then a partial "streaming..." and the final "final answer" (turn
/// complete).
pub(crate) fn wait_done_stream_llm(wait: Gate) -> Arc<dyn gadk_model::Llm> {
    fn_llm("wait-done-stream-llm", move |_, _| {
        let wait = wait.clone();
        futures::stream::once(async move { wait.wait().await })
            .flat_map(|()| {
                futures::stream::iter([partial_text("streaming..."), final_text("final answer")])
            })
            .boxed()
    })
}

/// Go `&gatedFlakyLLM{enter, ready, err: errors.New(err), reply}` (model name
/// `gated-flaky-model`): call 1 runs `enter`, waits for `ready`, then fails with `err`; later
/// calls answer `reply` (turn complete).
pub(crate) fn gated_flaky_llm(
    enter: impl Fn() + Send + Sync + 'static,
    ready: Gate,
    err: &str,
    reply: &str,
) -> Arc<dyn gadk_model::Llm> {
    let (err, reply) = (err.to_owned(), reply.to_owned());
    let calls = AtomicUsize::new(0);
    fn_llm("gated-flaky-model", move |_, _| {
        if calls.fetch_add(1, Ordering::SeqCst) != 0 {
            return futures::stream::iter([final_text(&reply)]).boxed();
        }
        enter();
        let (ready, err) = (ready.clone(), err.clone());
        futures::stream::once(async move {
            ready.wait().await;
            Err(ModelError::Other(err.into()))
        })
        .boxed()
    })
}

/// Go `&finishTaskLLM{called: make(chan struct{})}` (model name `finish-task-model`): closes
/// `called` once and calls `finish_task` (ID `call-finish-1`, `{"result": "task finished"}`).
/// Returns the model and `called`.
pub(crate) fn finish_task_llm() -> (Arc<dyn gadk_model::Llm>, Gate) {
    let called = Gate::new();
    let gate = called.clone();
    let model = fn_llm("finish-task-model", move |_, _| {
        gate.close();
        futures::stream::iter([Ok(LlmResponse {
            content: Some(Content {
                role: "model".into(),
                parts: vec![Part {
                    function_call: Some(FunctionCall {
                        id: "call-finish-1".into(),
                        name: "finish_task".into(),
                        args: json!({"result": "task finished"})
                            .as_object()
                            .cloned()
                            .unwrap_or_default(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
            }),
            ..Default::default()
        })])
        .boxed()
    });
    (model, called)
}

/// Go `&partialThenWaitLLM{chunkText}` (model name `partial-then-wait-llm`): a partial `chunk`,
/// then waits for its context.
pub(crate) fn partial_then_wait_llm(chunk: &str) -> Arc<dyn gadk_model::Llm> {
    let chunk = chunk.to_owned();
    fn_llm("partial-then-wait-llm", move |_, _| {
        then_wait_for_cancel(vec![partial_text(&chunk)])
    })
}

/// Go `&gatedLLM{entered, release}` (model name `gated`): closes `entered`, waits for `release`
/// (at most 10 s), then answers "late" (turn complete). Returns the model, `entered` and
/// `release`.
pub(crate) fn gated_llm() -> (Arc<dyn gadk_model::Llm>, Gate, Gate) {
    let (entered, release) = (Gate::new(), Gate::new());
    let (e, r) = (entered.clone(), release.clone());
    let model = fn_llm("gated", move |_, _| {
        e.close();
        let r = r.clone();
        futures::stream::once(async move {
            let _ = tokio::time::timeout(Duration::from_secs(10), r.wait()).await;
            final_text("late")
        })
        .boxed()
    });
    (model, entered, release)
}

/// Go `newParallelAgent(t, name, branches...)`.
pub(crate) fn new_parallel_agent(name: &str, branches: Vec<Arc<dyn Agent>>) -> Arc<dyn Agent> {
    gadk_engine::parallel_agent::new(gadk_engine::parallel_agent::ParallelAgentConfig {
        agent: AgentConfig {
            name: name.into(),
            sub_agents: branches,
            ..Default::default()
        },
    })
    .expect("parallel agent")
}

/// Go `workflowagent.New(workflowagent.Config{Name: name, Edges: edges})`.
pub(crate) fn workflow_agent(name: &str, edges: Vec<Edge>) -> Arc<dyn Agent> {
    gadk_engine::workflow_agent::new(gadk_engine::workflow_agent::WorkflowAgentConfig {
        name: name.into(),
        edges,
        ..Default::default()
    })
    .expect("workflow agent")
}

/// Go `workflow.NewAgentNode(a, workflow.NodeConfig{})`.
pub(crate) fn agent_node(a: Arc<dyn Agent>) -> Arc<dyn Node> {
    AgentNode::new(a, NodeConfig::default()).expect("agent node")
}

/// Go `workflow.Start`.
pub(crate) fn start() -> Arc<dyn Node> {
    START.clone()
}

/// Go's `time.After(10 * time.Second)` bound on the tests' waits.
pub(crate) const TEN_SECONDS: Duration = Duration::from_secs(10);

/// Go `select { case <-ch: return true; case <-time.After(10 * time.Second): return false }`:
/// whether `gate` closed within 10 s.
pub(crate) async fn within(gate: &Gate) -> bool {
    tokio::time::timeout(TEN_SECONDS, gate.wait()).await.is_ok()
}

/// Closes its gate when dropped (Go `defer close(ch)` / `defer once.Do(func() { close(ch) })`).
pub(crate) struct CloseOnDrop(pub(crate) Gate);

impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        self.0.close();
    }
}

/// Go `ctx.Err()` of an ended agent context: `context.DeadlineExceeded` on a deadline, else
/// `context.Canceled` (an errgroup cause still reads `context canceled`).
pub(crate) fn ctx_err(ctx: &InvocationContext) -> AgentError {
    match ctx.cause() {
        Some(CancelCause::DeadlineExceeded) => AgentError::DeadlineExceeded,
        _ => AgentError::Cancelled,
    }
}

/// An unconditional edge (Go `workflow.Edge{From: from, To: to}`).
pub(crate) fn edge(from: &Arc<dyn Node>, to: &Arc<dyn Node>) -> Edge {
    Edge {
        from: from.clone(),
        to: to.clone(),
        route: None,
    }
}

/// Go `map[string]any{key: value}` as a tool result.
pub(crate) fn tool_result(key: &str, value: Value) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert(key.into(), value);
    m
}

/// Go `errSiblingFailed`.
pub(crate) const ERR_SIBLING_FAILED: &str = "guardrail rejected the request";

/// An event of `author` on `ctx`'s branch with model text `text` (Go
/// `ev := session.NewEvent(ctx, ctx.InvocationID()); ev.Author = author; ev.Branch =
/// ctx.Branch(); ev.Content = textContent("model", text)`).
pub(crate) fn content_event(ctx: &InvocationContext, author: &str, text: &str) -> Event {
    let mut ev = new_event(ctx);
    ev.author = author.into();
    ev.branch = ctx.branch();
    ev.response.content = Some(text_content("model", text));
    ev
}

/// A Go `BeforeAgentCallback` that closes `entered` and answers `text`.
pub(crate) fn closing_before_agent(entered: Gate, text: &'static str) -> BeforeAgentCallback {
    Arc::new(move |_ctx: Context| {
        let entered = entered.clone();
        Box::pin(async move {
            entered.close();
            Ok(Some(text_content("model", text)))
        })
    })
}

/// Go's cached child: `agent.New(agent.Config{Name: name, BeforeAgentCallbacks: {close(entered);
/// return textContent("model", text), nil}, Run: <yields nothing>})`.
pub(crate) fn cached_child(name: &str, text: &'static str, entered: Gate) -> Arc<dyn Agent> {
    gadk_agent::new_agent(AgentConfig {
        name: name.into(),
        before_agent_callbacks: vec![closing_before_agent(entered, text)],
        run: Some(Arc::new(|_| futures::stream::empty().boxed())),
        ..Default::default()
    })
    .expect("cached child")
}

/// Go `runUnjoinedLateChild(t, build)`: runs the composite agent that `build` makes over a
/// custom sub-agent, launcher, which starts cached_child on a goroutine of its own and returns
/// without waiting for it. cached_child starts only once launcher's span has ended, so it runs
/// right under the composite agent, on launcher's branch, and its BeforeAgentCallback answers
/// for it, so agent.Run ends its InvocationContext on that goroutine, which then waits until the
/// composite agent's AfterAgentCallback has run.
pub(crate) async fn run_unjoined_late_child(
    build: impl FnOnce(Arc<dyn Agent>, AfterAgentCallback) -> Arc<dyn Agent>,
) {
    let tp = setup_test_plugin().await;
    let entered = Gate::new();
    let launcher_done = Gate::new();
    let parent_done = Gate::new();
    let child_done = Gate::new();
    let child = cached_child("cached_child", "cached", entered.clone());
    let launcher = gadk_agent::new_agent(AgentConfig {
        name: "launcher".into(),
        run: Some(Arc::new({
            let (launcher_done, parent_done, child_done) = (
                launcher_done.clone(),
                parent_done.clone(),
                child_done.clone(),
            );
            move |ctx: InvocationContext| {
                let (child, launcher_done, parent_done, child_done) = (
                    child.clone(),
                    launcher_done.clone(),
                    parent_done.clone(),
                    child_done.clone(),
                );
                let child_ctx = ctx.clone();
                go(async move {
                    let _done = CloseOnDrop(child_done);
                    if !within(&launcher_done).await {
                        return;
                    }
                    let mut events = child.run(child_ctx);
                    while events.next().await.is_some() {
                        let _ = within(&parent_done).await;
                    }
                });
                futures::stream::iter([Ok(content_event(&ctx, "launcher", "launched"))]).boxed()
            }
        })),
        after_agent_callbacks: vec![Arc::new(move |_ctx: Context| {
            let (launcher_done, entered) = (launcher_done.clone(), entered.clone());
            Box::pin(async move {
                launcher_done.close();
                let _ = within(&entered).await;
                Ok(None)
            })
        })],
        ..Default::default()
    })
    .expect("launcher");
    let root = build(
        launcher,
        Arc::new(move |_ctx: Context| {
            let parent_done = parent_done.clone();
            Box::pin(async move {
                parent_done.close();
                Ok(None)
            })
        }),
    );
    let r = new_test_runner(root, vec![tp.plugin.clone()]);
    let (_, errs) = run_turn(&r, "u1", "s1", "go", RunConfig::default()).await;
    assert!(errs.is_empty(), "runner.Run error: {errs:?}");
    assert!(within(&child_done).await, "cached_child did not exit");
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

#[cfg(test)]
mod tests {
    use futures::FutureExt;
    use gadk_agent::BeforeAgentCallback;
    use gadk_agent::fiber::{AttemptSite, Origin, SCHEDULER_CREATOR};
    use gadk_model::LlmRequest;
    use parking_lot::Mutex;

    use super::super::runner_support::{app_runner, custom_agent, new_event, run_until_error};
    use super::*;

    async fn items(m: &Arc<dyn gadk_model::Llm>) -> Vec<Result<LlmResponse, ModelError>> {
        m.generate_content(LlmRequest::default(), false)
            .collect()
            .await
    }

    fn text_of(r: &Result<LlmResponse, ModelError>) -> (String, bool, bool) {
        let r = r.as_ref().expect("response");
        let text = r
            .content
            .as_ref()
            .and_then(|c| c.parts.first())
            .map(|p| p.text.clone())
            .unwrap_or_default();
        (text, r.partial, r.turn_complete)
    }

    #[tokio::test]
    async fn goroutines_are_distinct_fibers() {
        assert_eq!(goroutine_id(), 0);
        let (body, spawned) = on_test_goroutine(async {
            let body = goroutine_id();
            let spawned = go(async { (goroutine_id(), fiber::current().origin) })
                .await
                .unwrap();
            (body, spawned)
        })
        .await;
        assert_ne!(body, 0);
        assert_ne!(spawned.0, 0);
        assert_ne!(spawned.0, body);
        assert_eq!(spawned.1, Origin::default());
    }

    #[tokio::test]
    async fn gate_closes_once_for_every_waiter() {
        let g = Gate::new();
        assert!(!g.is_closed());
        let waiter = go({
            let g = g.clone();
            async move { g.wait().await }
        });
        g.close();
        g.close();
        waiter.await.unwrap();
        assert!(g.is_closed());
    }

    #[tokio::test]
    async fn models_answer_as_go() {
        let mut b = blocking_llm(0);
        let call = go({
            let m = b.model.clone();
            async move { items(&m).await }
        });
        b.entered.recv().await.unwrap();
        b.release.close();
        let got = call.await.unwrap();
        assert_eq!(b.model.name(), "blocking-model");
        assert_eq!(text_of(&got[0]), ("worker done".into(), false, true));

        let flaky = flaky_on_first_pass_llm("boom", "ok");
        assert_eq!(
            items(&flaky).await[0].as_ref().unwrap_err().to_string(),
            "boom"
        );
        assert_eq!(text_of(&items(&flaky).await[0]).0, "ok");

        let (m, entered) = context_canceled_on_done_llm();
        assert!(
            m.generate_content(LlmRequest::default(), true)
                .next()
                .now_or_never()
                .is_none()
        );
        assert!(entered.is_closed());

        let acked = Gate::new();
        let m = stream_then_wait_llm(Some(acked.clone()));
        let mut s = m.generate_content(LlmRequest::default(), true);
        assert_eq!(
            text_of(&s.next().await.unwrap()),
            ("streaming...".into(), true, false)
        );
        assert!(s.next().now_or_never().is_none());
        assert!(acked.is_closed());

        let got = items(&self_canceling_stream_llm()).await;
        assert_eq!(text_of(&got[0]), ("hello ".into(), true, false));
        assert_eq!(got[1].as_ref().unwrap_err().to_string(), "context canceled");

        let ready = Gate::new();
        ready.close();
        let got = items(&wait_then_stream_partial_llm(ready.clone(), "c", "f")).await;
        assert_eq!(text_of(&got[0]), ("c".into(), true, false));
        assert_eq!(text_of(&got[1]), ("f".into(), false, true));
        let got = items(&wait_done_stream_llm(ready.clone())).await;
        assert_eq!(text_of(&got[1]), ("final answer".into(), false, true));

        let entered = Arc::new(AtomicUsize::new(0));
        let m = gated_flaky_llm(
            {
                let entered = entered.clone();
                move || {
                    entered.fetch_add(1, Ordering::SeqCst);
                }
            },
            ready,
            "boom",
            "ok",
        );
        assert!(items(&m).await[0].is_err());
        assert_eq!(text_of(&items(&m).await[0]).0, "ok");
        assert_eq!(entered.load(Ordering::SeqCst), 1);

        let (m, called) = finish_task_llm();
        let got = items(&m).await;
        assert!(called.is_closed());
        let fc = got[0].as_ref().unwrap().content.as_ref().unwrap().parts[0]
            .function_call
            .clone()
            .unwrap();
        assert_eq!(
            (fc.id.as_str(), fc.name.as_str()),
            ("call-finish-1", "finish_task")
        );

        let m = partial_then_wait_llm("p");
        let mut s = m.generate_content(LlmRequest::default(), true);
        assert_eq!(text_of(&s.next().await.unwrap()).0, "p");
        assert!(s.next().now_or_never().is_none());

        let (m, entered, release) = gated_llm();
        release.close();
        assert_eq!(text_of(&items(&m).await[0]).0, "late");
        assert!(entered.is_closed());
    }

    /// A workflow node's agent runs on a scheduler-started fiber with the Node attempt; a
    /// ParallelAgent built by [`new_parallel_agent`] runs its branches; [`go_run`] drains an
    /// agent on a task of its own.
    #[tokio::test]
    async fn builders_run_real_agents() {
        type Seen = Arc<Mutex<Vec<(String, u64, Origin, Option<AttemptSite>)>>>;
        let seen = Seen::default();
        let probe = |name: &str| -> Arc<dyn Agent> {
            let seen = seen.clone();
            let cb: BeforeAgentCallback = Arc::new(move |ctx: gadk_agent::Context| {
                let f = fiber::current();
                seen.lock().push((
                    ctx.agent().name().to_string(),
                    f.id,
                    f.origin,
                    fiber::current_attempt(),
                ));
                async { Ok(None) }.boxed()
            });
            gadk_agent::new_agent(AgentConfig {
                name: name.into(),
                before_agent_callbacks: vec![cb],
                run: Some(Arc::new(|ic: InvocationContext| {
                    futures::stream::iter([Ok(new_event(&ic))]).boxed()
                })),
                ..Default::default()
            })
            .unwrap()
        };
        let wf = workflow_agent(
            "wf",
            gadk_engine::workflow::chain(&[start(), agent_node(probe("node_agent"))]),
        );
        let (_, errs) = run_until_error(&app_runner("app", wf, vec![]), "go").await;
        assert!(errs.is_empty(), "{errs:?}");
        {
            let got = seen.lock();
            let node = got.iter().find(|s| s.0 == "node_agent").unwrap();
            assert_eq!(node.2.creator, SCHEDULER_CREATOR);
            assert_eq!(node.3, Some(AttemptSite::Node));
        }

        let par = new_parallel_agent("par", vec![probe("a"), probe("b")]);
        let (_, errs) = run_until_error(&app_runner("app", par, vec![]), "go").await;
        assert!(errs.is_empty(), "{errs:?}");
        assert!(seen.lock().iter().any(|s| s.0 == "b"));

        let solo = custom_agent("solo", vec![], |ic| {
            futures::stream::iter([Ok(new_event(&ic))]).boxed()
        });
        let ctx = InvocationContext::builder()
            .agent(solo.clone())
            .build_invocation();
        let got = go_run(solo, ctx).await.unwrap();
        assert_eq!(got.len(), 1);
    }
}
