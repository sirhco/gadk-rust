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
//! group E: ParallelAgent branches (errgroup origin and cancel cause).
//! Shared helpers: [`attr_support`](super::attr_support).
//!
//! Go's custom agents are `iter.Seq2` bodies that run on the goroutine reading them and block in
//! `yield` until the reader asks for the next item; [`seq`] runs such a body inside the reader's
//! poll (so on the reader's fiber), [`Yield::item`] returns once the reader polls again, and a
//! reader that stops drops the body (Go's `yield` returning false; the body's drop guards stand
//! for its `defer`s). Go's `<-ctx.Done(); yield(nil, ctx.Err())` waits on the context's token and
//! yields [`ctx_err`](super::attr_support::ctx_err).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::Poll;
use std::time::Duration;

use futures::StreamExt;
use futures::future::BoxFuture;
use gadk_agent::{
    AfterAgentCallback, Agent, AgentConfig, AgentError, BeforeAgentCallback, BeforeModelCallback,
    CancelCause, ContextDelta, EventStream, InvocationContext, RunConfig, ToolError,
};
use gadk_engine::llm::LlmAgentConfig;
use gadk_genai::Content;
use gadk_model::{LlmRequest, LlmResponse};
use gadk_session::Event;
use parking_lot::Mutex;
use serde_json::{Map, Value, json};

use super::attr_support::{
    ERR_SIBLING_FAILED, Gate, context_canceled_on_done_llm, ctx_err, go, new_parallel_agent,
    run_unjoined_late_child, stream_then_wait_llm, then_wait_for_cancel,
    wait_then_stream_partial_llm, within,
};
use super::runner_support::*;
use crate::event_types::*;
use crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::*;
use crate::row::Row;
use crate::trace::testkit::sequential;

type Item = Result<Event, AgentError>;

/// A builder of a test agent (Go's `func(t *testing.T) agent.Agent` table fields).
type NewAgent = fn() -> Arc<dyn Agent>;

/// Go's `yield` of an `iter.Seq2[*session.Event, error]` body run by [`seq`].
#[derive(Clone, Default)]
struct Yield(Arc<Mutex<Option<Item>>>);

impl Yield {
    /// Go `yield(ev, err)`: hands `item` to the reader and returns once the reader asks for the
    /// next item (Go's `true`); a reader that stops drops the body instead (Go's `false`).
    async fn item(&self, item: Item) {
        *self.0.lock() = Some(item);
        let mut handed = false;
        std::future::poll_fn(|_| {
            if std::mem::replace(&mut handed, true) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await;
    }
}

/// Go `func(yield func(*session.Event, error) bool) { body }`: the body runs inside the reader's
/// poll, so on the reader's fiber, as Go's iterator body runs on the reader's goroutine.
fn seq<F, Fut>(body: F) -> EventStream
where
    F: FnOnce(Yield) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let y = Yield::default();
    let mut fut: Option<Pin<Box<dyn Future<Output = ()> + Send>>> = Some(Box::pin(body(y.clone())));
    futures::stream::poll_fn(move |cx| {
        let Some(f) = fut.as_mut() else {
            return Poll::Ready(None);
        };
        let done = f.as_mut().poll(cx).is_ready();
        if done {
            fut = None;
        }
        match y.0.lock().take() {
            Some(item) => Poll::Ready(Some(item)),
            None if done => Poll::Ready(None),
            None => Poll::Pending,
        }
    })
    .boxed()
}

/// Go `agent.New(agent.Config{Name, SubAgents, Run})` whose run is the iterator body `body`.
fn go_agent<F, Fut>(name: &str, sub_agents: Vec<Arc<dyn Agent>>, body: F) -> Arc<dyn Agent>
where
    F: Fn(InvocationContext, Yield) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    custom_agent(name, sub_agents, move |ctx| seq(|y| body(ctx, y)))
}

/// [`go_agent`] with agent callbacks (Go `agent.New(agent.Config{…, BeforeAgentCallbacks,
/// AfterAgentCallbacks})`).
fn go_agent_with<F, Fut>(
    name: &str,
    sub_agents: Vec<Arc<dyn Agent>>,
    before: Vec<BeforeAgentCallback>,
    after: Vec<AfterAgentCallback>,
    body: F,
) -> Arc<dyn Agent>
where
    F: Fn(InvocationContext, Yield) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    gadk_agent::new_agent(AgentConfig {
        name: name.into(),
        sub_agents,
        before_agent_callbacks: before,
        after_agent_callbacks: after,
        run: Some(Arc::new(move |ctx: InvocationContext| {
            seq(|y| body(ctx, y))
        })),
        ..Default::default()
    })
    .expect("custom agent")
}

/// Go `<-ctx.Done()`.
async fn done(ctx: &InvocationContext) {
    ctx.cancellation_token().cancelled().await;
}

/// Go `session.NewEvent(ctx, ctx.InvocationID())` with `Author` and `Branch` set.
fn event_of(ctx: &InvocationContext, author: &str) -> Event {
    let mut ev = new_event(ctx);
    ev.author = author.into();
    ev.branch = ctx.branch();
    ev
}

/// [`event_of`] with `Content: textContent("model", text)`.
fn text_event(ctx: &InvocationContext, author: &str, text: &str) -> Event {
    let mut ev = event_of(ctx, author);
    ev.response.content = Some(text_content("model", text));
    ev
}

/// [`text_event`] with `TurnComplete: true`.
fn final_event(ctx: &InvocationContext, author: &str, text: &str) -> Event {
    let mut ev = text_event(ctx, author, text);
    ev.response.turn_complete = true;
    ev
}

/// [`event_of`] with `Partial: true` and, if any, `Content: textContent("model", text)`.
fn partial_event(ctx: &InvocationContext, author: &str, text: Option<&str>) -> Event {
    let mut ev = event_of(ctx, author);
    ev.response.partial = true;
    ev.response.content = text.map(|t| text_content("model", t));
    ev
}

/// [`event_of`] with `Actions.StateDelta: delta`.
fn state_event(ctx: &InvocationContext, author: &str, delta: Value) -> Event {
    let mut ev = event_of(ctx, author);
    ev.actions.state_delta = delta.as_object().cloned();
    ev
}

/// Go `for ev, err := range sub.Run(ctx) { if !yield(ev, err) || err != nil { return } }`.
async fn forward(y: &Yield, sub: &Arc<dyn Agent>, ctx: InvocationContext) {
    let mut events = sub.run(ctx);
    while let Some(item) = events.next().await {
        let failed = item.is_err();
        y.item(item).await;
        if failed {
            return;
        }
    }
}

/// Runs `f` when dropped (Go `defer f()`).
struct Defer<F: FnOnce()>(Option<F>);

impl<F: FnOnce()> Drop for Defer<F> {
    fn drop(&mut self) {
        if let Some(f) = self.0.take() {
            f();
        }
    }
}

/// An after-agent callback running `f` (Go `func(agent.Context) (*genai.Content, error)`).
fn after_agent(f: impl Fn() -> Option<Content> + Send + Sync + 'static) -> AfterAgentCallback {
    Arc::new(
        move |_ctx: gadk_agent::Context| -> BoxFuture<'static, Result<Option<Content>, AgentError>> {
            let c = f();
            Box::pin(async move { Ok(c) })
        },
    )
}

/// Go `baseplugin.New(baseplugin.Config{Name: "observer", OnEventCallback: …})` whose callback
/// runs `f` on each event and returns `(nil, nil)`.
fn observer<F, Fut>(name: &str, f: F) -> gadk_agent::Plugin
where
    F: Fn(InvocationContext, Event) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let f = Arc::new(f);
    plugin_named(
        name,
        gadk_agent::PluginConfig {
            on_event_callback: Some(Arc::new(move |ictx, ev: Event| {
                let f = f.clone();
                Box::pin(async move {
                    f(ictx, ev).await;
                    Ok(None)
                })
            })),
            ..Default::default()
        },
    )
}

/// Go `for _, err := range r.Run(…, "go", cfg) { if err != nil { gotErr = err; if stopAtError {
/// break } } }`: the last error the caller received.
async fn last_error(
    r: &gadk_engine::Runner,
    cfg: RunConfig,
    stop_at_error: bool,
) -> Option<AgentError> {
    let (_, errs) = run_until(r, "u1", "s1", "go", cfg, |item| {
        stop_at_error && item.is_err()
    })
    .await;
    errs.into_iter().last()
}

/// Whether `ev`'s first part is the text `text` (Go `ev.Content != nil && len(ev.Content.Parts) >
/// 0 && ev.Content.Parts[0].Text == text`).
fn first_text_is(ev: &Event, text: &str) -> bool {
    ev.response
        .content
        .as_ref()
        .and_then(|c| c.parts.first())
        .is_some_and(|p| p.text == text)
}

/// Go `assertTerminalErrorRowsContain(t, rows, wantSubstr)`.
fn assert_terminal_error_rows_contain(rows: &[Row], want: &str) {
    let inv = only_row_of_type(rows, EVENT_INVOCATION_ERROR);
    let msg = str_col(&inv, "error_message");
    assert!(
        msg.contains(want),
        "INVOCATION_ERROR error_message = {msg:?}, want substring {want:?}"
    );
    for r in rows_of_type(rows, EVENT_AGENT_ERROR) {
        let msg = str_col(&r, "error_message");
        assert!(
            msg.contains(want),
            "AGENT_ERROR({:?}) error_message = {msg:?}, want substring {want:?}",
            agent_of(&r)
        );
    }
}

/// Asserts that `rows` hold no INVOCATION_COMPLETED row.
fn assert_no_invocation_completed(rows: &[Row]) {
    let n = rows_of_type(rows, EVENT_INVOCATION_COMPLETED).len();
    assert_eq!(
        n,
        0,
        "got {n} INVOCATION_COMPLETED rows, want 0; event types: {:?}",
        event_types(rows)
    );
}

/// `map[string]int{…}` of AGENT_ERROR counts.
fn counts(pairs: &[(&str, usize)]) -> BTreeMap<String, usize> {
    pairs.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect()
}

/// A scripted response calling `transfer_to_agent` with ID `call-transfer` (Go's coordinator
/// model).
fn transfer_call(target: &str) -> Result<LlmResponse, gadk_model::ModelError> {
    calls(vec![function_call_part_id(
        "call-transfer",
        "transfer_to_agent",
        json!({"agent_name": target}),
    )])
}

/// Go's `coord` LLM root that transfers to `par`.
fn transfer_root(par: Arc<dyn Agent>) -> Arc<dyn Agent> {
    llm_agent(LlmAgentConfig {
        name: "coord".into(),
        model: Some(scripted_llm(vec![transfer_call("par")])),
        sub_agents: vec![par],
        ..Default::default()
    })
}

/// Go `newStreamingAuthor(t, child, withContent)`.
fn new_streaming_author(child: Arc<dyn Agent>, with_content: bool) -> Arc<dyn Agent> {
    let run_child = child.clone();
    go_agent("author", vec![child], move |ctx, y| {
        let child = run_child.clone();
        async move {
            let text = with_content.then_some("progress...");
            y.item(Ok(partial_event(&ctx, "author", text))).await;
            forward(&y, &child, ctx).await;
        }
    })
}

/// Go `newGuardedAgent(t, seen)`.
fn new_guarded_agent(seen: Gate) -> Arc<dyn Agent> {
    let cb: BeforeModelCallback = Arc::new(move |_ctx, req: LlmRequest| {
        let seen = seen.clone();
        Box::pin(async move {
            seen.wait().await;
            (req, Err(AgentError::other(ERR_SIBLING_FAILED)))
        })
    });
    llm_agent(LlmAgentConfig {
        name: "failer".into(),
        model: Some(scripted_llm(vec![final_text("unused")])),
        before_model_callbacks: vec![cb],
        ..Default::default()
    })
}

/// Go `newCustomFailer(t, seen)`.
fn new_custom_failer(seen: Gate) -> Arc<dyn Agent> {
    go_agent("failer", vec![], move |_ctx, y| {
        let seen = seen.clone();
        async move {
            seen.wait().await;
            y.item(Err(AgentError::other(ERR_SIBLING_FAILED))).await;
        }
    })
}

/// Go `newStreamingLLMAgent(t, acked)`.
fn new_streaming_llm_agent(acked: Option<Gate>) -> Arc<dyn Agent> {
    llm_agent(LlmAgentConfig {
        name: "streamer".into(),
        model: Some(stream_then_wait_llm(acked)),
        ..Default::default()
    })
}

/// Go `newCustomStreamingAgent(t, acked)`.
fn new_custom_streaming_agent(acked: Option<Gate>) -> Arc<dyn Agent> {
    go_agent("streamer", vec![], move |ctx, y| {
        let acked = acked.clone();
        async move {
            y.item(Ok(partial_event(&ctx, "streamer", Some("streaming..."))))
                .await;
            if let Some(a) = acked {
                a.close();
            }
            done(&ctx).await;
            y.item(Err(ctx_err(&ctx))).await;
        }
    })
}

/// Go `&beforeAckStreamLLM{exited}` (model name `before-ack-stream-llm`): a partial
/// "streaming...", then waits for its context; `exited` closes when the call ends (Go `defer
/// close(m.exited)`), here when the flow drops the call's stream.
fn before_ack_stream_llm(exited: Gate) -> Arc<dyn gadk_model::Llm> {
    fn_llm("before-ack-stream-llm", move |_, _| {
        let exited = exited.clone();
        let guard = Defer(Some(move || exited.close()));
        then_wait_for_cancel(vec![partial_text("streaming...")])
            .map(move |r| {
                let _ = &guard;
                r
            })
            .boxed()
    })
}

/// Go `llmagent.New(llmagent.Config{Name: "streamer", Model: &beforeAckStreamLLM{exited}})`.
fn before_ack_streamer(exited: Gate) -> Arc<dyn Agent> {
    llm_agent(LlmAgentConfig {
        name: "streamer".into(),
        model: Some(before_ack_stream_llm(exited)),
        ..Default::default()
    })
}

/// Go `&funcStreamLLM{fn}` (model name `func-stream-llm`) of the `llm_streamer` recovery case:
/// waits for `final_seen`, streams a partial "streaming...", closes `acked` once the partial's
/// yield returned, then waits for its context.
fn final_seen_stream_llm(final_seen: Gate, acked: Gate) -> Arc<dyn gadk_model::Llm> {
    fn_llm("func-stream-llm", move |_, _| {
        let (final_seen, acked) = (final_seen.clone(), acked.clone());
        futures::stream::once(async move {
            final_seen.wait().await;
            partial_text("streaming...")
        })
        .chain(futures::stream::once(async move { acked.close() }).filter_map(|()| async { None }))
        .chain(futures::stream::pending())
        .boxed()
    })
}

/// Go `runUntilBranchFails(t, newRoot, cfg, stopAtError)`: the rows of the run and the last error
/// the caller received.
async fn run_until_branch_fails(
    new_root: impl FnOnce(Gate, Gate) -> Arc<dyn Agent>,
    cfg: RunConfig,
    stop_at_error: bool,
) -> (Vec<Row>, Option<AgentError>) {
    let tp = setup_test_plugin().await;
    let (seen, acked, ready) = (Gate::new(), Gate::new(), Gate::new());
    let bridge = go({
        let (seen, acked, ready) = (seen.clone(), acked.clone(), ready.clone());
        async move {
            seen.wait().await;
            acked.wait().await;
            ready.close();
        }
    });
    let obs = observer("observer", move |_, ev| {
        if ev.response.partial {
            seen.close();
        }
        async {}
    });
    let r = new_test_runner(new_root(ready, acked), vec![tp.plugin.clone(), obs]);
    let got = last_error(&r, cfg, stop_at_error).await;
    bridge.abort();
    assert!(
        got.as_ref()
            .is_some_and(|e| e.to_string().contains(ERR_SIBLING_FAILED)),
        "runner.Run error = {got:?}, want {ERR_SIBLING_FAILED}"
    );
    let rows = RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await;
    (rows, got)
}

/// One of Go's `branchFailureCases`.
struct BranchFailureCase {
    name: &'static str,
    sse: bool,
    want_error_agent: &'static str,
    want_any_error_agent: &'static [&'static str],
    no_llm_error: bool,
    new_root: fn(Gate, Gate) -> Arc<dyn Agent>,
}

/// Go `branchFailureCases`.
fn branch_failure_cases() -> Vec<BranchFailureCase> {
    vec![
        BranchFailureCase {
            name: "parallel root, model callback error",
            sse: true,
            want_error_agent: "failer",
            want_any_error_agent: &[],
            no_llm_error: false,
            new_root: |ready, acked| {
                new_parallel_agent(
                    "par_root",
                    vec![
                        new_streaming_llm_agent(Some(acked)),
                        new_guarded_agent(ready),
                    ],
                )
            },
        },
        BranchFailureCase {
            name: "parallel root, custom agent error",
            sse: true,
            want_error_agent: "failer",
            want_any_error_agent: &[],
            no_llm_error: false,
            new_root: |ready, acked| {
                new_parallel_agent(
                    "par_root",
                    vec![
                        new_streaming_llm_agent(Some(acked)),
                        new_custom_failer(ready),
                    ],
                )
            },
        },
        BranchFailureCase {
            name: "parallel agent under a sequential root",
            sse: true,
            want_error_agent: "failer",
            want_any_error_agent: &[],
            no_llm_error: false,
            new_root: |ready, acked| {
                let par = new_parallel_agent(
                    "par",
                    vec![
                        new_streaming_llm_agent(Some(acked)),
                        new_guarded_agent(ready),
                    ],
                );
                sequential("seq_root", vec![par])
            },
        },
        BranchFailureCase {
            name: "progress event without content, no streaming",
            sse: false,
            want_error_agent: "",
            want_any_error_agent: &["failer", "worker"],
            no_llm_error: false,
            new_root: |ready, acked| {
                // Go passes acked as the model's entered channel.
                let (model, entered) = context_canceled_on_done_llm();
                drop(go(async move {
                    entered.wait().await;
                    acked.close();
                }));
                let worker = llm_agent(LlmAgentConfig {
                    name: "worker".into(),
                    model: Some(model),
                    ..Default::default()
                });
                new_parallel_agent(
                    "par_root",
                    vec![
                        new_streaming_author(worker, false),
                        new_custom_failer(ready),
                    ],
                )
            },
        },
        BranchFailureCase {
            name: "parallel agent that an LLM root transfers to",
            sse: true,
            want_error_agent: "failer",
            want_any_error_agent: &[],
            no_llm_error: false,
            new_root: |ready, acked| {
                transfer_root(new_parallel_agent(
                    "par",
                    vec![
                        new_streaming_llm_agent(Some(acked)),
                        new_guarded_agent(ready),
                    ],
                ))
            },
        },
        BranchFailureCase {
            name: "parallel root, custom streaming sibling",
            sse: false,
            want_error_agent: "failer",
            want_any_error_agent: &[],
            no_llm_error: true,
            new_root: |ready, acked| {
                new_parallel_agent(
                    "par_root",
                    vec![
                        new_custom_streaming_agent(Some(acked)),
                        new_custom_failer(ready),
                    ],
                )
            },
        },
        BranchFailureCase {
            name: "LLM root with custom streaming sibling",
            sse: false,
            want_error_agent: "failer",
            want_any_error_agent: &[],
            no_llm_error: true,
            new_root: |ready, acked| {
                transfer_root(new_parallel_agent(
                    "par",
                    vec![
                        new_custom_streaming_agent(Some(acked)),
                        new_guarded_agent(ready),
                    ],
                ))
            },
        },
    ]
}

/// The body shared by Go's two `branchFailureCases` tests.
async fn check_branch_failure_case(tc: &BranchFailureCase, stop_at_error: bool) {
    let name = tc.name;
    let cfg = if tc.sse { sse() } else { RunConfig::default() };
    let (rows, got_err) = run_until_branch_fails(tc.new_root, cfg, stop_at_error).await;
    assert!(
        rows_of_type(&rows, EVENT_INVOCATION_COMPLETED).is_empty(),
        "{name}: got INVOCATION_COMPLETED although the caller received {got_err:?}; event types: {:?}",
        event_types(&rows)
    );
    assert_terminal_error_rows_contain(&rows, ERR_SIBLING_FAILED);
    let err_counts = agent_error_counts(&rows);
    let count = |n: &str| err_counts.get(n).copied().unwrap_or(0);
    if !tc.want_any_error_agent.is_empty() {
        let total: usize = tc.want_any_error_agent.iter().map(|n| count(n)).sum();
        assert_eq!(
            total,
            1,
            "{name}: AGENT_ERROR counts = {err_counts:?}, want total 1 across {:?}; event types: {:?}",
            tc.want_any_error_agent,
            event_types(&rows)
        );
    } else {
        assert_eq!(
            count(tc.want_error_agent),
            1,
            "{name}: AGENT_ERROR counts = {err_counts:?}, want 1 for {:?}; event types: {:?}",
            tc.want_error_agent,
            event_types(&rows)
        );
    }
    if tc.want_error_agent == "failer" {
        assert_eq!(
            count("streamer"),
            0,
            "{name}: AGENT_ERROR counts = {err_counts:?}, want 0 for streamer"
        );
    }
    if tc.no_llm_error {
        let n = rows_of_type(&rows, EVENT_LLM_ERROR).len();
        assert_eq!(
            n,
            0,
            "{name}: got {n} LLM_ERROR rows, want 0; event types: {:?}",
            event_types(&rows)
        );
    }
}

/// A before-agent callback that runs `f` and answers `text`.
fn answering_after(f: impl Fn() + Send + Sync + 'static, text: &str) -> BeforeAgentCallback {
    let content = text_content("model", text);
    Arc::new(
        move |_ctx: gadk_agent::Context| -> BoxFuture<'static, Result<Option<Content>, AgentError>> {
            f();
            let c = content.clone();
            Box::pin(async move { Ok(Some(c)) })
        },
    )
}

/// Go `newSignalingCacheHitAgent(t, started)`.
fn new_signaling_cache_hit_agent(started: Gate) -> Arc<dyn Agent> {
    go_agent_with(
        "cached_child",
        vec![],
        vec![answering_after(move || started.close(), "cache hit")],
        vec![],
        |_ctx, _y| async {},
    )
}

/// Go `newBranchA(t, childStarted, done, withEvent)`.
fn new_branch_a(child_started: Gate, done_gate: Gate, with_event: bool) -> Arc<dyn Agent> {
    go_agent_with(
        "branch_a",
        vec![],
        vec![],
        vec![after_agent(move || {
            done_gate.close();
            None
        })],
        move |ctx, y| {
            let child_started = child_started.clone();
            async move {
                if !within(&child_started).await {
                    y.item(Err(AgentError::other("cached_child did not start")))
                        .await;
                    return;
                }
                if with_event {
                    y.item(Ok(state_event(&ctx, "branch_a", json!({"seen": true}))))
                        .await;
                }
            }
        },
    )
}

/// Go's `wait` tool: closes `tool_started`, then answers `{"ok": true}` once `parent_done` is
/// closed.
fn wait_tool(tool_started: Gate, parent_done: Gate) -> Arc<dyn gadk_agent::Tool> {
    function_tool(
        "wait",
        "Waits for parent to end",
        move |_ctx, _args: Map<String, Value>| {
            let (tool_started, parent_done) = (tool_started.clone(), parent_done.clone());
            async move {
                tool_started.close();
                if within(&parent_done).await {
                    Ok(json!({"ok": true}).as_object().cloned().unwrap())
                } else {
                    Err(ToolError::Execution("parent did not end".into()))
                }
            }
        },
    )
}

/// Go's `waiter` LLM agent calling `tool`, then answering "done".
fn tool_waiter(name: &str, tool: Arc<dyn gadk_agent::Tool>) -> Arc<dyn Agent> {
    let call = tool.name().to_owned();
    llm_agent(LlmAgentConfig {
        name: name.into(),
        model: Some(scripted_llm(vec![
            calls(vec![function_call_part(&call, json!({}))]),
            final_text("done"),
        ])),
        tools: vec![tool],
        ..Default::default()
    })
}

/// Runs `r` until `author`'s "done" answer (Go's loop that breaks there; errors are fatal).
async fn run_until_done_of(r: &gadk_engine::Runner, author: &str) {
    let mut saw_done = false;
    let (_, errs) = run_until(r, "u1", "s1", "go", RunConfig::default(), |item| {
        saw_done = matches!(item, Ok(ev) if ev.author == author && first_text_is(ev, "done"));
        saw_done
    })
    .await;
    assert!(errs.is_empty(), "runner.Run error: {errs:?}");
    assert!(saw_done, "the run ended without the {author}'s answer");
}

#[tokio::test]
async fn test_runner_parallel_root_stopped_mid_stream_sibling_model_returns_context_canceled_emits_completed_terminal_rows()
 {
    let tp = setup_test_plugin().await;
    let (slow_model, slow_entered) = context_canceled_on_done_llm();
    let slow = llm_agent(LlmAgentConfig {
        name: "slow".into(),
        model: Some(slow_model),
        ..Default::default()
    });
    let bot = llm_agent(LlmAgentConfig {
        name: "bot".into(),
        model: Some(wait_then_stream_partial_llm(
            slow_entered,
            "hello ",
            "hello world",
        )),
        ..Default::default()
    });
    let par = new_parallel_agent("par_root", vec![bot, slow]);
    let r = new_test_runner(par, vec![tp.plugin.clone()]);
    let mut saw_partial = false;
    let (_, errs) = run_until(&r, "u1", "s1", "stream please", sse(), |item| {
        saw_partial = matches!(item, Ok(ev) if ev.response.partial);
        saw_partial
    })
    .await;
    assert!(errs.is_empty(), "r.Run error: {errs:?}");
    assert!(
        saw_partial,
        "did not observe a partial event in StreamingModeSSE"
    );

    let rows = RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await;
    require_invocation_completed_ok(&rows);
    let n = rows_of_type(&rows, EVENT_LLM_ERROR).len();
    assert_eq!(
        n, 1,
        "got {n} LLM_ERROR rows, want 1 (from slow branch context.Canceled)"
    );
}

#[tokio::test]
async fn test_runner_parallel_stream_stop_canceled_work_after_partial_is_not_an_error() {
    let new_children: [(&str, NewAgent); 2] = [
        ("sub-agent that fails with the context's error", || {
            go_agent("child", vec![], |ctx, y| async move {
                done(&ctx).await;
                y.item(Err(ctx_err(&ctx))).await;
            })
        }),
        ("model call that fails with the context's error", || {
            llm_agent(LlmAgentConfig {
                name: "child".into(),
                model: Some(context_canceled_on_done_llm().0),
                ..Default::default()
            })
        }),
    ];
    for (_name, new_child) in new_children {
        require_child_started_after_partial_stop(|| async move {
            let tp = setup_test_plugin().await;
            let child = new_child();
            let run_child = child.clone();
            let streamer = go_agent("streamer", vec![child], move |ctx, y| {
                let child = run_child.clone();
                async move {
                    y.item(Ok(partial_event(
                        &ctx,
                        "streamer",
                        Some("progress update..."),
                    )))
                    .await;
                    forward(&y, &child, ctx).await;
                }
            });
            let par = new_parallel_agent("par_root", vec![streamer]);
            let r = new_test_runner(par, vec![tp.plugin.clone()]);
            let mut saw_partial = false;
            let (_, errs) = run_until(&r, "u1", "s1", "stream please", sse(), |item| {
                saw_partial = matches!(item, Ok(ev) if ev.response.partial);
                saw_partial
            })
            .await;
            assert!(errs.is_empty(), "r.Run error: {errs:?}");
            assert!(
                saw_partial,
                "did not observe a partial event in StreamingModeSSE"
            );
            RowCursor::default()
                .collect(&tp, until_invocation_end)
                .await
        })
        .await;
    }
}

#[tokio::test]
async fn test_runner_agent_canceled_sub_agent_context_after_partial_emits_error_terminal_rows() {
    for wrap in ["root", "sequential", "parallel"] {
        let tp = setup_test_plugin().await;
        let (model, entered) = context_canceled_on_done_llm();
        let sub_bot = llm_agent(LlmAgentConfig {
            name: "sub_bot".into(),
            model: Some(model),
            ..Default::default()
        });
        let run_sub = sub_bot.clone();
        let custom = go_agent("custom", vec![sub_bot], move |ctx, y| {
            let (sub_bot, entered) = (run_sub.clone(), entered.clone());
            async move {
                y.item(Ok(partial_event(
                    &ctx,
                    "custom",
                    Some("progress update..."),
                )))
                .await;
                // Go `subCtx, cancel := context.WithCancel(ctx)`, canceled by a goroutine once
                // the sub-agent's model call started.
                let (sub_ctx, cancel) = ctx.clone().into_context().with_agent_cancel();
                drop(go(async move {
                    entered.wait().await;
                    cancel.cancel();
                }));
                forward(&y, &sub_bot, sub_ctx.as_invocation().clone()).await;
            }
        });
        let root = match wrap {
            "sequential" => sequential("seq_root", vec![custom]),
            "parallel" => new_parallel_agent("par_root", vec![custom]),
            _ => custom,
        };

        let r = new_test_runner(root, vec![tp.plugin.clone()]);
        let (events, errs) = run_turn(&r, "u1", "s1", "go", RunConfig::default()).await;
        let saw_partial = events.iter().any(|ev| ev.response.partial);
        let got_err = errs.last().map(ToString::to_string);
        assert!(
            saw_partial
                && got_err
                    .as_deref()
                    .is_some_and(|e| e.ends_with("context canceled")),
            "{wrap}: runner.Run yielded partial event {saw_partial} and error {got_err:?}, want a partial event and context.Canceled"
        );

        let rows = RowCursor::default()
            .collect(&tp, until_invocation_end)
            .await;
        assert_no_invocation_completed(&rows);
        only_row_of_type(&rows, EVENT_INVOCATION_ERROR);
        let got = agent_error_counts(&rows);
        assert_eq!(
            got.get("sub_bot").copied(),
            Some(1),
            "{wrap}: AGENT_ERROR rows per agent = {got:?}, want one for sub_bot"
        );
    }
}

#[tokio::test]
async fn test_runner_parallel_branch_fails_while_sibling_streams_emits_error_terminal_rows() {
    for tc in branch_failure_cases() {
        check_branch_failure_case(&tc, false).await;
    }
}

#[tokio::test]
async fn test_runner_parallel_branch_fails_while_sibling_streams_stop_at_error_emits_error_terminal_rows()
 {
    for tc in branch_failure_cases() {
        check_branch_failure_case(&tc, true).await;
    }
}

#[tokio::test]
async fn test_runner_parallel_branch_fails_before_partial_ack_emits_error_terminal_rows() {
    type NewRoot = fn(Gate, Gate) -> Arc<dyn Agent>;
    let cases: [(&str, NewRoot); 3] = [
        ("parallel root, model callback error", |seen, exited| {
            new_parallel_agent(
                "par_root",
                vec![before_ack_streamer(exited), new_guarded_agent(seen)],
            )
        }),
        ("parallel root, custom agent error", |seen, exited| {
            new_parallel_agent(
                "par_root",
                vec![before_ack_streamer(exited), new_custom_failer(seen)],
            )
        }),
        ("parallel agent under a sequential root", |seen, exited| {
            let par = new_parallel_agent(
                "par",
                vec![before_ack_streamer(exited), new_guarded_agent(seen)],
            );
            sequential("seq_root", vec![par])
        }),
    ];

    for stop_at_error in [false, true] {
        let mode = if stop_at_error {
            "stop_at_error"
        } else {
            "drain"
        };
        for (name, new_root) in cases {
            let tp = setup_test_plugin().await;
            let (seen, exited) = (Gate::new(), Gate::new());
            let triggered = Arc::new(AtomicBool::new(false));
            let obs = observer("observer", {
                let (seen, exited) = (seen.clone(), exited.clone());
                move |_, ev| {
                    let first = ev.response.partial && !triggered.swap(true, Ordering::SeqCst);
                    let (seen, exited) = (seen.clone(), exited.clone());
                    async move {
                        if first {
                            seen.close();
                            assert!(
                                tokio::time::timeout(Duration::from_secs(5), exited.wait())
                                    .await
                                    .is_ok(),
                                "timed out waiting for streaming producer to exit before partial ack"
                            );
                        }
                    }
                }
            });
            let r = new_test_runner(new_root(seen, exited), vec![tp.plugin.clone(), obs]);
            let got = last_error(&r, sse(), stop_at_error).await;
            assert!(
                got.as_ref()
                    .is_some_and(|e| e.to_string().contains(ERR_SIBLING_FAILED)),
                "{mode}/{name}: runner.Run error = {got:?}, want {ERR_SIBLING_FAILED}"
            );

            let rows = RowCursor::default()
                .collect(&tp, until_invocation_end)
                .await;
            assert_no_invocation_completed(&rows);
            assert_terminal_error_rows_contain(&rows, ERR_SIBLING_FAILED);
            let n = rows_of_type(&rows, EVENT_LLM_ERROR).len();
            assert_eq!(
                n,
                0,
                "{mode}/{name}: got {n} LLM_ERROR rows, want 0 (canceled before partial ack); event types: {:?}",
                event_types(&rows)
            );
            let err_counts = agent_error_counts(&rows);
            assert!(
                err_counts.get("failer") == Some(&1) && !err_counts.contains_key("streamer"),
                "{mode}/{name}: AGENT_ERROR counts = {err_counts:?}, want failer=1 and streamer=0; event types: {:?}",
                event_types(&rows)
            );
        }
    }
}

#[tokio::test]
async fn test_runner_parallel_three_branches_and_streaming_branch_failure_records_caller_error() {
    for stop_at_error in [false, true] {
        let mode = if stop_at_error {
            "stop_at_error"
        } else {
            "drain"
        };

        // three_branches
        {
            let new_root = |ready: Gate, acked: Gate| {
                let idle = go_agent("idle", vec![], |ctx, y| async move {
                    done(&ctx).await;
                    y.item(Err(ctx_err(&ctx))).await;
                });
                new_parallel_agent(
                    "par_root",
                    vec![
                        new_streaming_llm_agent(Some(acked)),
                        new_custom_failer(ready),
                        idle,
                    ],
                )
            };
            let (rows, _) = run_until_branch_fails(new_root, sse(), stop_at_error).await;
            assert_no_invocation_completed(&rows);
            assert_terminal_error_rows_contain(&rows, ERR_SIBLING_FAILED);
            let err_counts = agent_error_counts(&rows);
            let count = |n: &str| err_counts.get(n).copied().unwrap_or(0);
            assert_eq!(
                count("streamer"),
                0,
                "{mode}: AGENT_ERROR counts = {err_counts:?}, want 0 for streamer"
            );
            assert_eq!(
                count("failer") + count("idle"),
                1,
                "{mode}: AGENT_ERROR counts = {err_counts:?}, want total 1 across [failer, idle]"
            );
        }

        // streaming_branch_fails
        {
            const ERR_STREAM_BACKEND: &str = "stream backend failed";
            let tp = setup_test_plugin().await;
            let (waiter_entered, seen) = (Gate::new(), Gate::new());
            let obs = observer("observer", {
                let seen = seen.clone();
                move |_, ev| {
                    if ev.response.partial {
                        seen.close();
                    }
                    async {}
                }
            });
            let streamfail = go_agent("streamfail", vec![], {
                let waiter_entered = waiter_entered.clone();
                move |ctx, y| {
                    let (waiter_entered, seen) = (waiter_entered.clone(), seen.clone());
                    async move {
                        waiter_entered.wait().await;
                        y.item(Ok(partial_event(&ctx, "streamfail", Some("streaming..."))))
                            .await;
                        seen.wait().await;
                        y.item(Err(AgentError::other(ERR_STREAM_BACKEND))).await;
                    }
                }
            });
            let waiter = go_agent("waiter", vec![], move |ctx, y| {
                let waiter_entered = waiter_entered.clone();
                async move {
                    waiter_entered.close();
                    done(&ctx).await;
                    y.item(Err(ctx_err(&ctx))).await;
                }
            });
            let par_root = new_parallel_agent("par_root", vec![streamfail, waiter]);
            let r = new_test_runner(par_root, vec![tp.plugin.clone(), obs]);
            let got = last_error(&r, RunConfig::default(), stop_at_error).await;
            assert!(
                got.as_ref()
                    .is_some_and(|e| e.to_string().contains(ERR_STREAM_BACKEND)),
                "{mode}: runner.Run error = {got:?}, want {ERR_STREAM_BACKEND}"
            );
            let rows = RowCursor::default()
                .collect(&tp, until_invocation_end)
                .await;
            assert_no_invocation_completed(&rows);
            assert_terminal_error_rows_contain(&rows, ERR_STREAM_BACKEND);
            let err_counts = agent_error_counts(&rows);
            assert!(
                err_counts.get("waiter") == Some(&1) && !err_counts.contains_key("streamfail"),
                "{mode}: AGENT_ERROR counts = {err_counts:?}, want waiter=1 and streamfail=0"
            );
        }
    }
}

#[tokio::test]
async fn test_runner_parallel_responded_branch_fails_while_sibling_streams_consistent_across_drain_and_stop()
 {
    for root_kind in ["parallel_root", "llm_root_transfer"] {
        for stop_at_error in [false, true] {
            let mode = if stop_at_error {
                "stop_at_error"
            } else {
                "drain"
            };
            let tp = setup_test_plugin().await;
            let (final_seen, partial_seen, acked, ready) =
                (Gate::new(), Gate::new(), Gate::new(), Gate::new());
            let bridge = go({
                let (partial_seen, acked, ready) =
                    (partial_seen.clone(), acked.clone(), ready.clone());
                async move {
                    partial_seen.wait().await;
                    acked.wait().await;
                    ready.close();
                }
            });
            let obs = observer("observer", {
                let final_seen = final_seen.clone();
                move |_, ev| {
                    if !ev.response.partial && ev.author == "failer" {
                        final_seen.close();
                    }
                    if ev.response.partial && ev.author == "streamer" {
                        partial_seen.close();
                    }
                    async {}
                }
            });
            let streamer = go_agent("streamer", vec![], move |ctx, y| {
                let (final_seen, acked) = (final_seen.clone(), acked.clone());
                async move {
                    final_seen.wait().await;
                    y.item(Ok(partial_event(&ctx, "streamer", Some("streaming..."))))
                        .await;
                    acked.close();
                    done(&ctx).await;
                    y.item(Err(ctx_err(&ctx))).await;
                }
            });
            let failer = go_agent("failer", vec![], move |ctx, y| {
                let ready = ready.clone();
                async move {
                    y.item(Ok(final_event(&ctx, "failer", "initial answer")))
                        .await;
                    ready.wait().await;
                    y.item(Err(AgentError::other(ERR_SIBLING_FAILED))).await;
                }
            });
            let root = if root_kind == "parallel_root" {
                new_parallel_agent("par_root", vec![streamer, failer])
            } else {
                transfer_root(new_parallel_agent("par", vec![streamer, failer]))
            };

            let r = new_test_runner(root, vec![tp.plugin.clone(), obs]);
            let got = last_error(&r, RunConfig::default(), stop_at_error).await;
            bridge.abort();
            assert!(
                got.as_ref()
                    .is_some_and(|e| e.to_string().contains(ERR_SIBLING_FAILED)),
                "{root_kind}/{mode}: runner.Run error = {got:?}, want {ERR_SIBLING_FAILED}"
            );

            let rows = RowCursor::default()
                .collect(&tp, until_invocation_end)
                .await;
            assert_no_invocation_completed(&rows);
            assert_terminal_error_rows_contain(&rows, ERR_SIBLING_FAILED);
            let n = rows_of_type(&rows, EVENT_AGENT_ERROR).len();
            assert_eq!(
                n,
                0,
                "{root_kind}/{mode}: got {n} AGENT_ERROR rows, want 0 (all branches responded); counts: {:?}",
                agent_error_counts(&rows)
            );
        }
    }
}

/// One of Go's `TestRunner_CustomAgentHandlesParallelFailure_RecoveryModes` table cases.
struct RecoveryCase {
    name: &'static str,
    failer_responds: bool,
    llm_streamer: bool,
    read_par_to_end: bool,
    recovery: &'static str,
    want_completed: bool,
    want_agent_errors: &'static [(&'static str, usize)],
}

#[tokio::test]
async fn test_runner_custom_agent_handles_parallel_failure_recovery_modes() {
    let tests = [
        RecoveryCase {
            name: "reads_par_to_end_returns_without_event",
            failer_responds: true,
            llm_streamer: false,
            read_par_to_end: true,
            recovery: "none",
            want_completed: false,
            want_agent_errors: &[],
        },
        RecoveryCase {
            name: "reads_par_to_end_fallback_subagent",
            failer_responds: true,
            llm_streamer: false,
            read_par_to_end: true,
            recovery: "fallback",
            want_completed: false,
            want_agent_errors: &[],
        },
        RecoveryCase {
            name: "reads_par_to_end_guard_own_final_event",
            failer_responds: true,
            llm_streamer: false,
            read_par_to_end: true,
            recovery: "own_final",
            want_completed: true,
            want_agent_errors: &[],
        },
        RecoveryCase {
            name: "breaks_out_of_par_fallback_subagent",
            failer_responds: true,
            llm_streamer: false,
            read_par_to_end: false,
            recovery: "fallback",
            want_completed: false,
            want_agent_errors: &[],
        },
        RecoveryCase {
            name: "breaks_out_of_par_guard_own_final_event",
            failer_responds: true,
            llm_streamer: false,
            read_par_to_end: false,
            recovery: "own_final",
            want_completed: false,
            want_agent_errors: &[],
        },
        RecoveryCase {
            name: "unresponded_failer_reads_par_to_end_guard_own_final_event",
            failer_responds: false,
            llm_streamer: false,
            read_par_to_end: true,
            recovery: "own_final",
            want_completed: false,
            want_agent_errors: &[("failer", 1)],
        },
        RecoveryCase {
            name: "llm_streamer_collateral_error_reads_par_to_end_guard_own_final_event",
            failer_responds: true,
            llm_streamer: true,
            read_par_to_end: true,
            recovery: "own_final",
            want_completed: false,
            want_agent_errors: &[],
        },
    ];

    // reads_seq_with_silent_child_guard_own_final_event
    {
        let tp = setup_test_plugin().await;
        let w = go_agent("w", vec![], |_ctx, y| async move {
            y.item(Err(AgentError::other(ERR_SIBLING_FAILED))).await;
        });
        let seq = sequential("seq", vec![w]);
        let run_seq = seq.clone();
        let guard = go_agent("guard", vec![seq], move |ctx, y| {
            let seq = run_seq.clone();
            async move {
                let mut events = seq.run(ctx.clone());
                while let Some(item) = events.next().await {
                    if let Ok(ev) = item {
                        y.item(Ok(ev)).await;
                    }
                }
                y.item(Ok(final_event(&ctx, "guard", "guard recovered")))
                    .await;
            }
        });
        let (_, errs) = run_turn(
            &new_test_runner(guard, vec![tp.plugin.clone()]),
            "u1",
            "s1",
            "go",
            RunConfig::default(),
        )
        .await;
        assert!(
            errs.is_empty(),
            "runner.Run yielded unexpected error: {errs:?}"
        );
        let rows = RowCursor::default()
            .collect(&tp, until_invocation_end)
            .await;
        assert_no_invocation_completed(&rows);
        assert_terminal_error_rows_contain(
            &rows,
            r#"agent "w" execution failed before completion"#,
        );
        let got = agent_error_counts(&rows);
        assert_eq!(
            got,
            counts(&[("w", 1)]),
            "AGENT_ERROR counts = {got:?}, want map[w:1]"
        );
    }

    for tc in tests {
        let name = tc.name;
        let tp = setup_test_plugin().await;
        let final_seen = Gate::new();
        if !tc.failer_responds {
            final_seen.close();
        }
        let (partial_seen, acked, ready) = (Gate::new(), Gate::new(), Gate::new());
        let bridge = go({
            let (partial_seen, acked, ready) = (partial_seen.clone(), acked.clone(), ready.clone());
            async move {
                partial_seen.wait().await;
                acked.wait().await;
                ready.close();
            }
        });
        let failer_responds = tc.failer_responds;
        let obs = observer("observer", {
            let final_seen = final_seen.clone();
            move |_, ev| {
                if failer_responds && !ev.response.partial && ev.author == "failer" {
                    final_seen.close();
                }
                if ev.response.partial && ev.author == "streamer" {
                    partial_seen.close();
                }
                async {}
            }
        });

        let streamer = if tc.llm_streamer {
            llm_agent(LlmAgentConfig {
                name: "streamer".into(),
                model: Some(final_seen_stream_llm(final_seen, acked)),
                ..Default::default()
            })
        } else {
            go_agent("streamer", vec![], move |ctx, y| {
                let (final_seen, acked) = (final_seen.clone(), acked.clone());
                async move {
                    tokio::select! {
                        () = final_seen.wait() => {}
                        () = done(&ctx) => return,
                    }
                    y.item(Ok(partial_event(&ctx, "streamer", Some("streaming..."))))
                        .await;
                    acked.close();
                    done(&ctx).await;
                    y.item(Err(ctx_err(&ctx))).await;
                }
            })
        };
        let failer = go_agent("failer", vec![], move |ctx, y| {
            let ready = ready.clone();
            async move {
                if failer_responds {
                    y.item(Ok(final_event(&ctx, "failer", "initial answer")))
                        .await;
                }
                tokio::select! {
                    () = ready.wait() => {}
                    () = done(&ctx) => return,
                }
                y.item(Err(AgentError::other(ERR_SIBLING_FAILED))).await;
            }
        });
        let par = new_parallel_agent("par", vec![streamer, failer]);
        let fallback = go_agent("fallback", vec![], |ctx, y| async move {
            y.item(Ok(final_event(&ctx, "fallback", "fallback answer")))
                .await;
        });

        let (read_par_to_end, recovery) = (tc.read_par_to_end, tc.recovery);
        let (run_par, run_fallback) = (par.clone(), fallback.clone());
        let guard = go_agent("guard", vec![par, fallback], move |ctx, y| {
            let (par, fallback) = (run_par.clone(), run_fallback.clone());
            async move {
                let mut events = par.run(ctx.clone());
                while let Some(item) = events.next().await {
                    match item {
                        Err(_) if !read_par_to_end => break,
                        Err(_) => continue,
                        Ok(ev) => y.item(Ok(ev)).await,
                    }
                }
                drop(events);
                match recovery {
                    "fallback" => {
                        let mut events = fallback.run(ctx.clone());
                        while let Some(item) = events.next().await {
                            y.item(item).await;
                        }
                    }
                    "own_final" => {
                        y.item(Ok(final_event(&ctx, "guard", "guard recovered")))
                            .await;
                    }
                    _ => {}
                }
            }
        });

        let run_cfg = if tc.llm_streamer {
            sse()
        } else {
            RunConfig::default()
        };
        let (_, errs) = run_turn(
            &new_test_runner(guard, vec![tp.plugin.clone(), obs]),
            "u1",
            "s1",
            "go",
            run_cfg,
        )
        .await;
        bridge.abort();
        assert!(
            errs.is_empty(),
            "{name}: runner.Run yielded unexpected error: {errs:?}"
        );

        let rows = RowCursor::default()
            .collect(&tp, until_invocation_end)
            .await;
        if tc.want_completed {
            let n = rows_of_type(&rows, EVENT_INVOCATION_ERROR).len();
            assert_eq!(
                n,
                0,
                "{name}: got {n} INVOCATION_ERROR rows, want 0; event types: {:?}",
                event_types(&rows)
            );
            only_row_of_type(&rows, EVENT_INVOCATION_COMPLETED);
        } else {
            assert_no_invocation_completed(&rows);
            assert_terminal_error_rows_contain(&rows, ERR_SIBLING_FAILED);
        }
        let got = agent_error_counts(&rows);
        assert_eq!(
            got,
            counts(tc.want_agent_errors),
            "{name}: AGENT_ERROR counts = {got:?}, want {:?}",
            tc.want_agent_errors
        );
        if tc.llm_streamer {
            let n = rows_of_type(&rows, EVENT_LLM_ERROR).len();
            assert_eq!(
                n,
                1,
                "{name}: got {n} LLM_ERROR rows, want 1; event types: {:?}",
                event_types(&rows)
            );
        }
    }
}

#[tokio::test]
async fn test_runner_stop_reading_on_partial_event_without_content_is_not_an_error() {
    let new_pinger = || {
        go_agent("pinger", vec![], |ctx, y| async move {
            let mut ping = new_event(&ctx);
            ping.branch = ctx.branch();
            ping.response.partial = true;
            y.item(Ok(ping)).await;
            let mut done_ev = new_event(&ctx);
            done_ev.branch = ctx.branch();
            done_ev.response.content = Some(text_content("model", "done"));
            y.item(Ok(done_ev)).await;
        })
    };
    let new_parallel = || {
        // waiter runs until the parallel agent stops it and then fails with the context's
        // error, without any event.
        let waiter = go_agent("waiter", vec![], |ctx, y| async move {
            done(&ctx).await;
            y.item(Err(ctx_err(&ctx))).await;
        });
        new_parallel_agent("par_root", vec![new_pinger(), waiter])
    };
    for (name, root) in [
        ("custom root", new_pinger()),
        ("sub-agent of a parallel root", new_parallel()),
    ] {
        let tp = setup_test_plugin().await;
        let r = new_test_runner(root, vec![tp.plugin.clone()]);
        let mut saw_partial = false;
        let (_, errs) = run_until(&r, "u1", "s1", "go", RunConfig::default(), |item| {
            saw_partial = matches!(item, Ok(ev) if ev.response.partial);
            saw_partial
        })
        .await;
        assert!(errs.is_empty(), "{name}: r.Run error: {errs:?}");
        assert!(saw_partial, "{name}: runner.Run yielded no partial event");

        require_invocation_completed_ok(
            &RowCursor::default()
                .collect(&tp, until_invocation_end)
                .await,
        );
    }
}

#[tokio::test]
async fn test_trace_manager_cached_child_stop_before_parent_retires() {
    let cases: [(&str, NewAgent, bool); 3] = [
        ("llm sub-agent", new_cache_hit_agent, false),
        (
            "custom sub-agent",
            || {
                go_agent_with(
                    "cached",
                    vec![],
                    vec![answering_before_agent(text_content("model", "cache hit"))],
                    vec![],
                    |_ctx, _y| async {},
                )
            },
            false,
        ),
        (
            "unanswered custom sub-agent",
            || {
                go_agent("cached", vec![], |_ctx, y| async move {
                    y.item(Err(AgentError::other("silent child error"))).await;
                })
            },
            true,
        ),
    ];
    for (name, new_cached, want_err) in cases {
        let tp = setup_test_plugin().await;
        let (tool_started, parent_done) = (Gate::new(), Gate::new());
        let cached = new_cached();
        let run_cached = cached.clone();
        let parent = go_agent_with(
            "parent",
            vec![cached],
            vec![],
            vec![after_agent({
                let parent_done = parent_done.clone();
                move || {
                    parent_done.close();
                    None
                }
            })],
            {
                let tool_started = tool_started.clone();
                move |ctx, y| {
                    let (cached, tool_started) = (run_cached.clone(), tool_started.clone());
                    async move {
                        y.item(Ok(text_event(&ctx, "parent", "parent answer")))
                            .await;
                        // Wait until the sibling branch's tool call has started so call.seq <
                        // cached.seq holds and keeps cached on agentStack when parent retires.
                        if !within(&tool_started).await {
                            y.item(Err(AgentError::other("the wait tool did not start")))
                                .await;
                            return;
                        }
                        // The parent drops the sub-agent's event (or swallows its error in the
                        // unanswered subtest).
                        let mut events = cached.run(ctx);
                        while let Some(item) = events.next().await {
                            if item.is_err() {
                                break;
                            }
                        }
                    }
                }
            },
        );
        let waiter = tool_waiter("waiter", wait_tool(tool_started, parent_done));
        let par = new_parallel_agent("par", vec![parent, waiter]);

        // Stop at the waiter's answer, after popToolSpan settled cached and before the
        // parallel agent retires cached.
        run_until_done_of(&new_test_runner(par, vec![tp.plugin.clone()]), "waiter").await;
        let rows = RowCursor::default()
            .collect(&tp, until_invocation_end)
            .await;
        if want_err {
            only_row_of_type(&rows, EVENT_INVOCATION_ERROR);
            let got = agent_error_counts(&rows);
            assert_eq!(
                got,
                counts(&[("cached", 1)]),
                "{name}: AGENT_ERROR counts = {got:?}, want map[cached:1]"
            );
            continue;
        }
        require_invocation_completed_ok(&rows);
    }
}

#[tokio::test]
async fn test_trace_manager_cached_child_retired_while_other_branch_tool_runs() {
    let tp = setup_test_plugin().await;
    let (tool_started, parent_done) = (Gate::new(), Gate::new());
    let cached = new_cache_hit_agent();
    let run_cached = cached.clone();
    let parent = go_agent_with(
        "parent",
        vec![cached],
        vec![],
        vec![after_agent({
            let parent_done = parent_done.clone();
            move || {
                parent_done.close();
                None
            }
        })],
        {
            let tool_started = tool_started.clone();
            move |ctx, y| {
                let (cached, tool_started) = (run_cached.clone(), tool_started.clone());
                async move {
                    // The parent drops the sub-agent's event.
                    let mut events = cached.run(ctx);
                    while let Some(item) = events.next().await {
                        if let Err(e) = item {
                            y.item(Err(e)).await;
                            return;
                        }
                    }
                    // The parent ends only once the other branch's tool call is open.
                    if !within(&tool_started).await {
                        y.item(Err(AgentError::other("the wait tool did not start")))
                            .await;
                    }
                }
            }
        },
    );
    let waiter = tool_waiter("waiter", wait_tool(tool_started, parent_done));
    let par = new_parallel_agent("par", vec![parent, waiter]);

    // Stop at the waiter's answer, before the parallel agent ends.
    run_until_done_of(&new_test_runner(par, vec![tp.plugin.clone()]), "waiter").await;
    require_invocation_completed_ok(
        &RowCursor::default()
            .collect(&tp, until_invocation_end)
            .await,
    );
}

#[tokio::test]
async fn test_trace_manager_cross_branch_tool_runs_cached_child() {
    let tp = setup_test_plugin().await;
    let (child_started, branch_a_done) = (Gate::new(), Gate::new());
    let cached = new_signaling_cache_hit_agent(child_started.clone());
    let run_child = function_tool("run_child", "Runs cached_child on branch_a", {
        let branch_a_done = branch_a_done.clone();
        move |ctx: gadk_agent::Context, _args: Map<String, Value>| {
            let (cached, branch_a_done) = (cached.clone(), branch_a_done.clone());
            async move {
                // The tool context's invocation context runs the sub-agent on branch_a. The
                // sub-agent's event is dropped.
                let sub_ctx = ctx.as_invocation().with_delta(ContextDelta {
                    branch: Some("par.branch_a".into()),
                    ..Default::default()
                });
                let mut events = cached.run(sub_ctx);
                while let Some(item) = events.next().await {
                    if let Err(e) = item {
                        return Err(ToolError::Execution(Box::new(e)));
                    }
                }
                if within(&branch_a_done).await {
                    Ok(json!({"ok": true}).as_object().cloned().unwrap())
                } else {
                    Err(ToolError::Execution("branch_a did not end".into()))
                }
            }
        }
    });
    let branch_b = tool_waiter("branch_b", run_child);
    let par = new_parallel_agent(
        "par",
        vec![new_branch_a(child_started, branch_a_done, true), branch_b],
    );
    let (_, errs) = run_turn(
        &new_test_runner(par, vec![tp.plugin.clone()]),
        "u1",
        "s1",
        "go",
        RunConfig::default(),
    )
    .await;
    assert!(errs.is_empty(), "runner.Run error: {errs:?}");
    require_invocation_completed_ok(
        &RowCursor::default()
            .collect(&tp, until_invocation_end)
            .await,
    );
}

// parity: adapted: Go cancels the run's context (runCtx); gadk's Runner::run takes none, so the hold plugin cancels the invocation context it is handed (the root of the run's cancel scope)
#[tokio::test]
async fn test_trace_manager_contentless_event_cancel_and_sibling_failure_end_invocation_race() {
    for (name, cancel_run, want_terminal, want_agent_errs) in [
        (
            "run canceled while contentless event held",
            true,
            EVENT_INVOCATION_ERROR,
            counts(&[]),
        ),
        (
            "sibling fails while contentless event held",
            false,
            EVENT_INVOCATION_ERROR,
            counts(&[("failer", 1)]),
        ),
    ] {
        let tp = setup_test_plugin().await;
        let (resumed, fail_now) = (Gate::new(), Gate::new());

        let ender = go_agent("ender", vec![], {
            let resumed = resumed.clone();
            move |ctx, y| {
                let resumed = resumed.clone();
                async move {
                    let _defer = Defer(Some({
                        let ctx = ctx.clone();
                        move || {
                            resumed.close();
                            ctx.end_invocation();
                        }
                    }));
                    y.item(Ok(state_event(&ctx, "ender", json!({"step": 1}))))
                        .await;
                }
            }
        });
        let mut sub_agents = vec![ender];
        if !cancel_run {
            let fail_now = fail_now.clone();
            sub_agents.push(go_agent("failer", vec![], move |ctx, y| {
                let fail_now = fail_now.clone();
                async move {
                    tokio::select! {
                        () = fail_now.wait() => y.item(Err(AgentError::other("boom"))).await,
                        () = done(&ctx) => {}
                    }
                }
            }));
        }

        let held_too_long = Arc::new(AtomicBool::new(false));
        let triggered = Arc::new(AtomicBool::new(false));
        let hold = observer("hold_contentless_event", {
            let held_too_long = held_too_long.clone();
            move |ictx: InvocationContext, ev| {
                let first = ev.author == "ender"
                    && ev.response.content.is_none()
                    && !ev.response.partial
                    && !triggered.swap(true, Ordering::SeqCst);
                let (resumed, fail_now, held_too_long) =
                    (resumed.clone(), fail_now.clone(), held_too_long.clone());
                async move {
                    if !first {
                        return;
                    }
                    if cancel_run {
                        ictx.cancel_with_cause(CancelCause::Canceled);
                    } else {
                        fail_now.close();
                    }
                    if !within(&resumed).await {
                        held_too_long.store(true, Ordering::SeqCst);
                    }
                }
            }
        });

        let par = new_parallel_agent("par", sub_agents);
        run_turn(
            &new_test_runner(par, vec![hold, tp.plugin.clone()]),
            "u1",
            "s1",
            "go",
            RunConfig::default(),
        )
        .await;
        assert!(
            !held_too_long.load(Ordering::SeqCst),
            "{name}: ender did not unblock while its contentless event was held"
        );
        let rows = RowCursor::default()
            .collect(&tp, until_invocation_end)
            .await;
        only_row_of_type(&rows, want_terminal);
        let got = agent_error_counts(&rows);
        assert_eq!(
            got, want_agent_errs,
            "{name}: AGENT_ERROR counts = {got:?}, want {want_agent_errs:?}"
        );
    }
}

/// Go's `answering` and `failing` branches of the recovered-parallel tests: `answering` answers
/// and closes `answered`; `failing` answers, waits for `answered`, then fails.
fn answering_and_failing() -> (Arc<dyn Agent>, Arc<dyn Agent>) {
    let answered = Gate::new();
    let answering = go_agent("answering", vec![], {
        let answered = answered.clone();
        move |ctx, y| {
            let answered = answered.clone();
            async move {
                let _defer = Defer(Some(move || answered.close()));
                y.item(Ok(text_event(&ctx, "answering", "answering branch answer")))
                    .await;
            }
        }
    });
    let failing = go_agent("failing", vec![], move |ctx, y| {
        let answered = answered.clone();
        async move {
            y.item(Ok(text_event(&ctx, "failing", "failing branch answer")))
                .await;
            within(&answered).await;
            y.item(Err(AgentError::other("branch failed after answering")))
                .await;
        }
    });
    (answering, failing)
}

#[tokio::test]
async fn test_failed_parallel_agent_recovered_by_root_after_agent_callback_reports_completed() {
    for under_root in [false, true] {
        let topology = if under_root {
            "guard under custom root"
        } else {
            "guard as root"
        };
        let mut agent_errors: [Option<BTreeMap<String, usize>>; 2] = [None, None];
        for (i, from_callback) in [false, true].into_iter().enumerate() {
            let mode = if from_callback {
                "recovered from AfterAgentCallback"
            } else {
                "recovered from Run"
            };
            let tp = setup_test_plugin().await;
            let (answering, failing) = answering_and_failing();
            let par = new_parallel_agent("par", vec![answering, failing]);

            let run_par = par.clone();
            let after = if from_callback {
                vec![after_agent(|| {
                    Some(text_content("model", "guard recovered"))
                })]
            } else {
                vec![]
            };
            let guard = go_agent_with("guard", vec![par], vec![], after, move |ctx, y| {
                let par = run_par.clone();
                async move {
                    let mut events = par.run(ctx.clone());
                    while let Some(item) = events.next().await {
                        // The guard swallows the branch failure.
                        if let Ok(ev) = item {
                            y.item(Ok(ev)).await;
                        }
                    }
                    drop(events);
                    if from_callback {
                        return;
                    }
                    y.item(Ok(text_event(&ctx, "guard", "guard recovered")))
                        .await;
                }
            });
            let root = if under_root {
                let run_guard = guard.clone();
                go_agent("root", vec![guard], move |ctx, y| {
                    let guard = run_guard.clone();
                    async move {
                        let mut events = guard.run(ctx);
                        while let Some(item) = events.next().await {
                            y.item(item).await;
                        }
                    }
                })
            } else {
                guard
            };

            let (events, errs) = run_turn(
                &new_test_runner(root, vec![tp.plugin.clone()]),
                "u1",
                "s1",
                "go",
                RunConfig::default(),
            )
            .await;
            assert!(
                errs.is_empty(),
                "{topology}/{mode}: runner.Run error: {errs:?}"
            );
            assert!(
                events
                    .iter()
                    .any(|ev| ev.author == "guard" && first_text_is(ev, "guard recovered")),
                "{topology}/{mode}: runner did not yield the guard's recovery content"
            );
            let rows = RowCursor::default()
                .collect(&tp, until_invocation_end)
                .await;
            let n = rows_of_type(&rows, EVENT_INVOCATION_ERROR).len();
            assert_eq!(
                n,
                0,
                "{topology}/{mode}: got {n} INVOCATION_ERROR rows, want 0; event types: {:?}",
                event_types(&rows)
            );
            assert_status(
                &only_row_of_type(&rows, EVENT_INVOCATION_COMPLETED),
                "OK",
                None,
            );
            let got = agent_error_counts(&rows);
            assert!(
                !got.contains_key("guard") && !got.contains_key("root"),
                "{topology}/{mode}: AGENT_ERROR counts = {got:?}, want none for guard or root, which recovered"
            );
            agent_errors[i] = Some(got);
        }
        assert_eq!(
            agent_errors[1], agent_errors[0],
            "{topology}: AGENT_ERROR counts when recovered from AfterAgentCallback, want those when recovered from Run"
        );
    }
}

/// An item handed to Go's fan-in root, with the acknowledgement its sender waits for.
type Sent = (Item, Option<tokio::sync::oneshot::Sender<bool>>);

/// Go's `send(ev, err)` of the fan-in root: an event waits for the root's acknowledgement (or
/// for the root to stop); an error is sent without one.
async fn send_acked(tx: &tokio::sync::mpsc::Sender<Sent>, stop: &Gate, item: Item) -> bool {
    if item.is_err() {
        let _ = tx.send((item, None)).await;
        return true;
    }
    let (ack, acked) = tokio::sync::oneshot::channel();
    if tx.send((item, Some(ack))).await.is_err() {
        return false;
    }
    tokio::select! {
        cont = acked => cont.unwrap_or(false),
        () = stop.wait() => false,
    }
}

#[tokio::test]
async fn test_failed_parallel_agent_recovered_by_after_agent_callback_with_concurrent_undeclared_sibling()
 {
    let tp = setup_test_plugin().await;
    let (guard_started, sibling_open, guard_recovered) = (Gate::new(), Gate::new(), Gate::new());
    let (answering, failing) = answering_and_failing();
    let par = new_parallel_agent("par", vec![answering, failing]);

    let run_par = par.clone();
    let guard = go_agent_with(
        "guard",
        vec![par],
        vec![],
        vec![after_agent(|| {
            Some(text_content("model", "guard recovered"))
        })],
        {
            let (guard_started, sibling_open) = (guard_started.clone(), sibling_open.clone());
            move |ctx, y| {
                let (par, guard_started, sibling_open) =
                    (run_par.clone(), guard_started.clone(), sibling_open.clone());
                async move {
                    guard_started.close();
                    if !within(&sibling_open).await {
                        y.item(Err(AgentError::other("sibling never opened"))).await;
                        return;
                    }
                    let mut events = par.run(ctx);
                    while let Some(item) = events.next().await {
                        if let Ok(ev) = item {
                            y.item(Ok(ev)).await;
                        }
                    }
                }
            }
        },
    );

    let sibling = go_agent("sibling", vec![], {
        let guard_recovered = guard_recovered.clone();
        move |ctx, y| {
            let (sibling_open, guard_recovered) = (sibling_open.clone(), guard_recovered.clone());
            async move {
                sibling_open.close();
                if !within(&guard_recovered).await {
                    y.item(Err(AgentError::other("guard never recovered")))
                        .await;
                    return;
                }
                y.item(Ok(text_event(&ctx, "sibling", "sibling ok"))).await;
            }
        }
    });

    // Go's root runs guard and sibling on goroutines of its own, which hand each event to the
    // root's iterator and wait for its acknowledgement.
    let root = go_agent("root", vec![], move |ctx, y| {
        let (guard, sibling, guard_started, guard_recovered) = (
            guard.clone(),
            sibling.clone(),
            guard_started.clone(),
            guard_recovered.clone(),
        );
        async move {
            let (tx, mut rx) = tokio::sync::mpsc::channel::<Sent>(16);
            let stop = Gate::new();
            let _close_done = Defer(Some({
                let stop = stop.clone();
                move || stop.close()
            }));
            let (guard_tx, guard_stop) = (tx.clone(), stop.clone());
            let guard_ctx = ctx.clone();
            let g = go(async move {
                let mut events = guard.run(guard_ctx);
                while let Some(item) = events.next().await {
                    if !send_acked(&guard_tx, &guard_stop, item).await {
                        return;
                    }
                }
            });
            let (sibling_tx, sibling_stop) = (tx.clone(), stop.clone());
            let sibling_ctx = ctx.clone();
            let failed = tx.clone();
            let s = go(async move {
                if !within(&guard_started).await {
                    let _ = failed
                        .send((Err(AgentError::other("guard never started")), None))
                        .await;
                    return;
                }
                let mut events = sibling.run(sibling_ctx);
                while let Some(item) = events.next().await {
                    if !send_acked(&sibling_tx, &sibling_stop, item).await {
                        return;
                    }
                }
            });
            drop(tx);
            let _abort = Defer(Some(move || {
                g.abort();
                s.abort();
            }));
            while let Some((item, ack)) = rx.recv().await {
                let is_guard = matches!(&item, Ok(ev) if ev.author == "guard");
                y.item(item).await;
                if let Some(ack) = ack {
                    let _ = ack.send(true);
                }
                if is_guard {
                    guard_recovered.close();
                }
            }
        }
    });

    let (_, errs) = run_turn(
        &new_test_runner(root, vec![tp.plugin.clone()]),
        "u1",
        "s1",
        "go",
        RunConfig::default(),
    )
    .await;
    assert!(errs.is_empty(), "runner.Run error: {errs:?}");
    let rows = RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await;
    let n = rows_of_type(&rows, EVENT_INVOCATION_ERROR).len();
    assert_eq!(
        n,
        0,
        "got {n} INVOCATION_ERROR rows, want 0; event types={:?}, agentErrorCounts={:?}",
        event_types(&rows),
        agent_error_counts(&rows)
    );
    assert_status(
        &only_row_of_type(&rows, EVENT_INVOCATION_COMPLETED),
        "OK",
        None,
    );
    let got = agent_error_counts(&rows);
    assert!(
        !got.contains_key("guard") && !got.contains_key("sibling") && !got.contains_key("root"),
        "AGENT_ERROR counts = {got:?}, want 0 for guard, sibling, and root"
    );
}

#[tokio::test]
async fn test_concurrency_race_parallel_agent_unjoined_late_child() {
    run_unjoined_late_child(|sub, after| {
        gadk_engine::parallel_agent::new(gadk_engine::parallel_agent::ParallelAgentConfig {
            agent: AgentConfig {
                name: "fanout".into(),
                sub_agents: vec![sub],
                after_agent_callbacks: vec![after],
                ..Default::default()
            },
        })
        .expect("parallel agent")
    })
    .await;
}

#[tokio::test]
async fn test_parallel_agent_branch_failure_after_content_emits_invocation_error() {
    let failer = go_agent("failer", vec![], |ctx, y| async move {
        y.item(Ok(text_event(&ctx, "failer", "failer content")))
            .await;
        y.item(Err(AgentError::other("errFailer"))).await;
    });
    let succeeder = go_agent("succeeder", vec![], |ctx, y| async move {
        y.item(Ok(final_event(&ctx, "succeeder", "success answer")))
            .await;
    });
    let par = new_parallel_agent("par_root", vec![failer, succeeder]);
    let tp = setup_test_plugin().await;
    let r = new_test_runner(par, vec![tp.plugin.clone()]);
    run_turn(&r, "u1", "s1", "hello", RunConfig::default()).await;

    let rows = RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await;
    let inv = only_row_of_type(&rows, EVENT_INVOCATION_ERROR);
    let msg = str_col(&inv, "error_message");
    assert!(
        msg.contains("errFailer"),
        "INVOCATION_ERROR error_message = {msg:?}, want it to contain \"errFailer\""
    );
    for row in rows_of_type(&rows, EVENT_AGENT_ERROR) {
        if agent_of(&row) == "failer" {
            let msg = str_col(&row, "error_message");
            assert!(
                msg.contains("errFailer"),
                "AGENT_ERROR(failer) error_message = {msg:?}, want it to contain \"errFailer\""
            );
        }
    }
}
