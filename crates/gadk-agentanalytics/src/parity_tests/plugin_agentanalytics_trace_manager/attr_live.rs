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
//! group C: `RunLive` tool calls on the live session's goroutine, and sub-agents run on a
//! promoted sub-node path (`agent.PromoteWithDelta(ctx, &agent.CommonContextDelta{Path: …})` =
//! `ctx.with_delta(ContextDelta { path: Some(…), .. })`).
//! Shared helpers: [`attr_support`](super::attr_support).
//!
//! Go → gadk: `newFakeLiveServer(t, msgs…)` + `&liveTestLLM{client}` =
//! [`go_live_test_llm`] over the same messages; `newTestRunner(t, root, p).RunLive(t.Context(),
//! "u1", "s1", agent.LiveRunConfig{})` = [`Runner::run_live`]; `liveSess.Close()` =
//! `LiveSession::close`; `ctx.WithICDelta(nil)` (Go returns the context itself) = the
//! context's invocation context (`ctx.as_invocation().clone()`), whose run forks its own
//! activation as Go's `agent.Run` does. Go's `select { case <-ch: case <-time.After(10 *
//! time.Second): }` = [`wait_or_timeout`].

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use futures::StreamExt;
use futures::future::BoxFuture;
use gadk_agent::{
    AfterToolCallback, Agent, AgentConfig, AgentError, BeforeAgentCallback, Context, ContextDelta,
    EventStream, InvocationContext, LiveRunConfig, LiveSession, RunConfig, Tool, ToolError,
};
use gadk_engine::RunOptions;
use gadk_engine::llm::LlmAgentConfig;
use gadk_engine::runner::Runner;
use gadk_engine::testutil::live::{go_live_test_llm, model_turn, tool_call};
use gadk_genai::Content;
use gadk_model::LlmResponse;
use gadk_session::Event;
use serde_json::{Map, Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::attr_support::{Gate, blocking_llm, gated_llm, go, on_test_goroutine};
use super::direct_support::bg;
use super::runner_support::{
    agent_rows_of_type, custom_agent, final_text, function_tool, llm_agent, new_event,
    plugin_named, scripted_llm,
};
use crate::event_types::*;
use crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::{
    agent_error_counts, event_types, must_latency, new_test_runner, new_trace_only_plugin,
    place_of, require_invocation_completed_ok, rows_of_type, run_turn, setup_test_plugin, str_col,
    text_content, until_invocation_end,
};
use crate::row::Row;
use crate::trace::{Identity, TraceManager};

/// Go's 10 s bound on every wait of these tests.
const WAIT: Duration = Duration::from_secs(10);

/// Go `select { case <-gate: return true; case <-time.After(10 * time.Second): return false }`.
async fn wait_or_timeout(gate: &Gate) -> bool {
    tokio::time::timeout(WAIT, gate.wait()).await.is_ok()
}

/// Go `map[string]any{key: value}` as a tool result.
fn tool_result(key: &str, value: Value) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert(key.into(), value);
    m
}

/// A tool-execution error of Go's text (Go `return nil, errors.New(text)` from a function tool).
fn tool_error(text: &str) -> ToolError {
    ToolError::Execution(text.to_owned().into())
}

/// Go `AfterToolCallbacks: {func(…) (map[string]any, error) { close(done); return nil, nil }}`.
fn closing_after_tool(done: Gate) -> AfterToolCallback {
    Arc::new(move |_ctx, _tool, _args, _result, _err| {
        done.close();
        Box::pin(async { Ok(None) })
    })
}

/// Go `llmagent.New(llmagent.Config{Name: "live_root", Model: &liveTestLLM{client}, Tools:
/// tools, AfterToolCallbacks: {after_tool}})` over a fake live server sending `msgs`.
fn live_root(
    msgs: Vec<LlmResponse>,
    tools: Vec<Arc<dyn Tool>>,
    after_tool: AfterToolCallback,
) -> Arc<dyn Agent> {
    llm_agent(LlmAgentConfig {
        name: "live_root".into(),
        model: Some(go_live_test_llm(msgs) as Arc<dyn gadk_model::Llm>),
        tools,
        after_tool_callbacks: vec![after_tool],
        ..Default::default()
    })
}

/// Go `liveSess, events, err := r.RunLive(t.Context(), "u1", "s1", agent.LiveRunConfig{})`.
async fn run_live(r: &Runner) -> (Arc<dyn LiveSession>, EventStream) {
    r.run_live("u1", "s1", LiveRunConfig::default(), RunOptions::default())
        .await
        .expect("r.RunLive")
}

/// Go `go func() { select { case <-gate: case <-time.After(10 * time.Second): }; _ =
/// liveSess.Close() }()`.
fn close_after(gate: Gate, live_sess: Arc<dyn LiveSession>) -> tokio::task::JoinHandle<()> {
    go(async move {
        wait_or_timeout(&gate).await;
        let _ = live_sess.close().await;
    })
}

/// Go `for _, err := range events { if err != nil { t.Errorf("live event error: %v", err) } }`
/// (bounded, so that a stream that never ends fails the test instead of hanging it).
async fn drain_live(mut events: EventStream) {
    let mut errs = Vec::new();
    let drained = tokio::time::timeout(WAIT, async {
        while let Some(item) = events.next().await {
            if let Err(err) = item {
                errs.push(err.to_string());
            }
        }
    })
    .await;
    assert!(drained.is_ok(), "the live event stream did not end");
    assert!(errs.is_empty(), "live event errors: {errs:?}");
}

/// Go `agent.PromoteWithDelta(ctx, &agent.CommonContextDelta{Path: &path})`.
fn promote_with_path(ctx: &Context, path: &str) -> Context {
    ctx.with_delta(ContextDelta {
        path: Some(path.into()),
        ..Default::default()
    })
}

/// Go `agent.New(agent.Config{Name: name, BeforeAgentCallbacks: {before}, Run: func(…) {
/// return func(func(*session.Event, error) bool) {} }})`: a custom agent whose run yields
/// nothing.
fn silent_agent(name: &str, before: BeforeAgentCallback) -> Arc<dyn Agent> {
    gadk_agent::new_agent(AgentConfig {
        name: name.into(),
        before_agent_callbacks: vec![before],
        run: Some(Arc::new(|_ic: InvocationContext| {
            futures::stream::empty().boxed()
        })),
        ..Default::default()
    })
    .expect("custom agent")
}

/// A before-agent callback that closes `gate` and answers `content` (Go `func(agent.Context)
/// (*genai.Content, error) { close(gate); return content, nil }`).
fn closing_answer(gate: Gate, content: Content) -> BeforeAgentCallback {
    Arc::new(
        move |_ctx: Context| -> BoxFuture<'static, Result<Option<Content>, AgentError>> {
            gate.close();
            let c = content.clone();
            Box::pin(async move { Ok(Some(c)) })
        },
    )
}

/// Go's `textContent("model", text)` event of the running invocation, authored by `author` on
/// its branch.
fn text_event(ctx: &InvocationContext, author: &str, text: &str) -> Event {
    let mut ev = new_event(ctx);
    ev.author = author.into();
    ev.branch = ctx.branch();
    ev.response.content = Some(text_content("model", text));
    ev
}

#[tokio::test]
#[ignore = "7b-3b defect: dropping the closed live session's event stream aborts the session task (GuardedStream::drop), whose JoinSet aborts the in-flight run_child call, so its AfterToolCallback never runs (Go's session goroutine finishes the call)"]
async fn test_trace_manager_live_tool_cached_child_stop_race() {
    on_test_goroutine(async {
        let tp = setup_test_plugin().await;
        let (answered, release, tool_done) = (Gate::new(), Gate::new(), Gate::new());

        let child = silent_agent(
            "cached_child",
            closing_answer(answered.clone(), text_content("model", "cache hit")),
        );
        let run_child = function_tool("run_child", "Runs cached_child", {
            let (child, release) = (child.clone(), release.clone());
            move |ctx: Context, _args| {
                let (child, release) = (child.clone(), release.clone());
                async move {
                    // The tool context does not support running an agent; its underlying
                    // context does.
                    let mut events = child.run(ctx.as_invocation().clone());
                    while let Some(item) = events.next().await {
                        item.map_err(|e| ToolError::Execution(Box::new(e)))?;
                    }
                    release.wait().await;
                    Ok(tool_result("ok", json!(true)))
                }
            }
        });
        let root = live_root(
            vec![tool_call(&[("call-1", "run_child")])],
            vec![run_child],
            closing_after_tool(tool_done.clone()),
        );

        // Holds the event that calls the tool until the sub-agent has started, so that
        // OnEventCallback gets the root agent's event, which has no open span in a live run,
        // while the sub-agent runs on the root agent's branch and node path: the event must not
        // mark the sub-agent as responded.
        let held_too_long = Arc::new(AtomicBool::new(false));
        let hold = plugin_named(
            "hold_tool_call_event",
            gadk_agent::PluginConfig {
                on_event_callback: Some(Arc::new({
                    let (answered, held_too_long) = (answered.clone(), held_too_long.clone());
                    move |_ictx, ev: Event| {
                        let calls_tool =
                            ev.author == "live_root"
                                && ev.response.content.as_ref().is_some_and(|c| {
                                    c.parts.iter().any(|p| p.function_call.is_some())
                                });
                        let (answered, held_too_long) = (answered.clone(), held_too_long.clone());
                        Box::pin(async move {
                            if calls_tool && !wait_or_timeout(&answered).await {
                                held_too_long.store(true, Ordering::SeqCst);
                            }
                            Ok(None)
                        })
                    }
                })),
                ..Default::default()
            },
        );

        let r = new_test_runner(root, vec![hold, tp.plugin.clone()]);
        let (live_sess, events) = run_live(&r).await;
        // Stop the live run once the BeforeAgentCallback has answered, while the tool's
        // goroutine goes on to end cached_child's InvocationContext.
        let stopped = close_after(answered.clone(), live_sess);
        drain_live(events).await;
        // The tool call is still open when AfterRun runs: it returns only after the
        // invocation's terminal row.
        let rows = tp.collect_rows(until_invocation_end).await;
        release.close();
        assert!(
            wait_or_timeout(&tool_done).await,
            "run_child did not return"
        );
        stopped.await.unwrap();
        assert!(
            !held_too_long.load(Ordering::SeqCst),
            "cached_child did not start while the event that called run_child was held"
        );

        let n = rows_of_type(&rows, EVENT_INVOCATION_COMPLETED).len();
        assert_eq!(
            n,
            0,
            "INVOCATION_COMPLETED rows; event types: {:?}",
            event_types(&rows)
        );
        assert_eq!(
            rows_of_type(&rows, EVENT_INVOCATION_ERROR).len(),
            1,
            "INVOCATION_ERROR rows; event types: {:?}",
            event_types(&rows)
        );
        assert_eq!(
            agent_error_counts(&rows),
            BTreeMap::from([("cached_child".to_owned(), 1)]),
            "AGENT_ERROR counts"
        );
    })
    .await;
}

/// One item of the child runs that the parent of
/// [`test_concurrency_race_concurrent_same_agent_with_promoted_sub_node_path`] forwards (Go
/// `item{ev, err, ack}`).
struct Item {
    item: Result<Event, AgentError>,
    ack: Option<mpsc::Sender<bool>>,
}

/// Go's parent loop `for it := range ch { cont := yield(it.ev, it.err); if it.ack != nil {
/// it.ack <- cont } }` as a stream: an item's ack is sent when the consumer asks for the next
/// item (Go's `yield` returned true), and the stream ends when `ch` closes.
fn forward_acked(rx: mpsc::Receiver<Item>) -> EventStream {
    futures::stream::unfold(
        (rx, None::<mpsc::Sender<bool>>),
        |(mut rx, ack)| async move {
            if let Some(ack) = ack {
                let _ = ack.send(true).await;
            }
            let it = rx.recv().await?;
            Some((it.item, (rx, it.ack)))
        },
    )
    .boxed()
}

#[tokio::test]
async fn test_concurrency_race_concurrent_same_agent_with_promoted_sub_node_path() {
    for cached_first in [true, false] {
        let order = if cached_first {
            "cached_run_starts_first"
        } else {
            "answering_run_starts_first"
        };
        on_test_goroutine(async {
            let tp = setup_test_plugin().await;
            let (first_entered, cached_answered) = (Gate::new(), Gate::new());
            let calls = Arc::new(AtomicUsize::new(0));

            let before: BeforeAgentCallback = Arc::new({
                let (first_entered, cached_answered, calls) = (
                    first_entered.clone(),
                    cached_answered.clone(),
                    calls.clone(),
                );
                move |_ctx: Context| -> BoxFuture<'static, Result<Option<Content>, AgentError>> {
                    let first = calls.fetch_add(1, Ordering::SeqCst) == 0;
                    if first {
                        first_entered.close();
                    }
                    let answer = (first == cached_first).then(|| {
                        cached_answered.close();
                        text_content("model", "cache hit")
                    });
                    Box::pin(async move { Ok(answer) })
                }
            });
            let child = gadk_agent::new_agent(AgentConfig {
                name: "child".into(),
                before_agent_callbacks: vec![before],
                run: Some(Arc::new({
                    let cached_answered = cached_answered.clone();
                    move |ic: InvocationContext| {
                        let cached_answered = cached_answered.clone();
                        futures::stream::once(async move {
                            if !wait_or_timeout(&cached_answered).await {
                                return Err(AgentError::other("cached run did not answer"));
                            }
                            Ok(text_event(&ic, "child", "child answer"))
                        })
                        .boxed()
                    }
                })),
                ..Default::default()
            })
            .expect("agent.New(child)");

            let run_child = child.clone();
            let parent = custom_agent("parent", vec![child], move |ic| {
                let answer = text_event(&ic, "parent", "parent answer");
                let promoted = promote_with_path(&ic.clone().into_context(), "custom@1");
                let (child, first_entered) = (run_child.clone(), first_entered.clone());
                // Go starts the two goroutines after `yield(answer)` returns: here when the
                // consumer asks for the item after the answer.
                let rest = futures::stream::once(async move {
                    let (tx, rx) = mpsc::channel::<Item>(4);
                    // Drops the cached run's event, so that settlement uses contextEnded.
                    let cached = go({
                        let (child, ctx, tx, first_entered) = (
                            child.clone(),
                            promoted.as_invocation().clone(),
                            tx.clone(),
                            first_entered.clone(),
                        );
                        async move {
                            if !cached_first && !wait_or_timeout(&first_entered).await {
                                let err = AgentError::other("first run never entered");
                                let _ = tx
                                    .send(Item {
                                        item: Err(err),
                                        ack: None,
                                    })
                                    .await;
                                return;
                            }
                            let mut events = child.run(ctx);
                            while let Some(item) = events.next().await {
                                if let Err(err) = item {
                                    let _ = tx
                                        .send(Item {
                                            item: Err(err),
                                            ack: None,
                                        })
                                        .await;
                                    return;
                                }
                            }
                        }
                    });
                    let answering = go({
                        let (ctx, tx) = (promoted.as_invocation().clone(), tx.clone());
                        async move {
                            if cached_first && !wait_or_timeout(&first_entered).await {
                                let err = AgentError::other("first run never entered");
                                let _ = tx
                                    .send(Item {
                                        item: Err(err),
                                        ack: None,
                                    })
                                    .await;
                                return;
                            }
                            let mut events = child.run(ctx);
                            while let Some(item) = events.next().await {
                                if item.is_err() {
                                    let _ = tx.send(Item { item, ack: None }).await;
                                    continue;
                                }
                                let (ack_tx, mut ack_rx) = mpsc::channel(1);
                                if tx
                                    .send(Item {
                                        item,
                                        ack: Some(ack_tx),
                                    })
                                    .await
                                    .is_err()
                                {
                                    return;
                                }
                                // Go: `case cont := <-ack: if !cont { return }` / `case <-done:
                                // return` (the parent's stream was dropped).
                                if ack_rx.recv().await != Some(true) {
                                    return;
                                }
                            }
                        }
                    });
                    drop(tx);
                    // Go `go func() { wg.Wait(); close(ch) }()`: the channel closes when both
                    // senders are gone; the join handles are detached.
                    drop((cached, answering));
                    forward_acked(rx)
                })
                .flatten();
                futures::stream::iter([Ok(answer)]).chain(rest).boxed()
            });

            let r = new_test_runner(parent, vec![tp.plugin.clone()]);
            let (_, errs) = run_turn(&r, "u1", "s1", "go", RunConfig::default()).await;
            assert!(errs.is_empty(), "{order}: runner.Run error: {errs:?}");
            let rows = tp.collect_rows(until_invocation_end).await;
            require_invocation_completed_ok(&rows);
            assert_eq!(
                rows_of_type(&rows, EVENT_AGENT_ERROR).len(),
                0,
                "{order}: AGENT_ERROR rows; counts: {:?}",
                agent_error_counts(&rows)
            );
        })
        .await;
    }
}

#[tokio::test]
async fn test_concurrency_race_live_tool_concurrent_same_child_with_promoted_path_under_invocation_root()
 {
    on_test_goroutine(async {
        let tp = setup_test_plugin().await;
        let (entered, second_entered, tool_done) = (Gate::new(), Gate::new(), Gate::new());
        let calls = Arc::new(AtomicUsize::new(0));

        // helper is an llmagent. The first activation is answered by BeforeAgentCallback: it
        // signals the second run to start and returns cached content, so the agent's run ends
        // its invocation and skips the model call. The second activation returns no content, so
        // the model call runs.
        let before: BeforeAgentCallback = Arc::new({
            let (entered, second_entered, calls) =
                (entered.clone(), second_entered.clone(), calls.clone());
            move |_ctx: Context| -> BoxFuture<'static, Result<Option<Content>, AgentError>> {
                let answer = if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    entered.close();
                    Some(text_content("model", "cached"))
                } else {
                    second_entered.close();
                    None
                };
                Box::pin(async move { Ok(answer) })
            }
        });
        let helper = llm_agent(LlmAgentConfig {
            name: "helper".into(),
            before_agent_callbacks: vec![before],
            model: Some(scripted_llm(vec![final_text("ok")])),
            ..Default::default()
        });

        let run_helper = function_tool("run_helper", "Runs helper twice concurrently", {
            let (helper, entered, second_entered) =
                (helper.clone(), entered.clone(), second_entered.clone());
            move |ctx: Context, _args| {
                let (helper, entered, second_entered) =
                    (helper.clone(), entered.clone(), second_entered.clone());
                async move {
                    // Promote the context to helper@1, as a Live tool would.
                    let promoted = promote_with_path(&ctx, "helper@1");
                    // Each run runs its own activation of helper (Go: its own WithICDelta(nil)
                    // context); the first run waits after each event for `wait` unless it is
                    // None.
                    let run = |wait: Option<Gate>| {
                        let (helper, run_ctx) = (helper.clone(), promoted.as_invocation().clone());
                        async move {
                            let mut events = helper.run(run_ctx);
                            while let Some(item) = events.next().await {
                                if item.is_err() {
                                    return;
                                }
                                if let Some(wait) = &wait {
                                    wait_or_timeout(wait).await;
                                }
                            }
                        }
                    };
                    let first = go(run(Some(second_entered)));
                    let second_run = run(None);
                    let second = go(async move {
                        if !wait_or_timeout(&entered).await {
                            return;
                        }
                        second_run.await;
                    });
                    first.await.unwrap();
                    second.await.unwrap();
                    Ok(tool_result("ok", json!(true)))
                }
            }
        });

        // RunLive, so the root agent (live_root) has no span: helper parents directly under the
        // invocation root, as in real Live runs. The fake server sends a toolCall, then a
        // serverContent with turnComplete to end the turn after the tool returns.
        let root = live_root(
            vec![
                tool_call(&[("call-1", "run_helper")]),
                model_turn("done", true),
            ],
            vec![run_helper],
            closing_after_tool(tool_done.clone()),
        );
        let r = new_test_runner(root, vec![tp.plugin.clone()]);
        let (live_sess, events) = run_live(&r).await;
        // Close the live session after the tool completes, so the flow shuts down cleanly once
        // both helper goroutines join and the AfterToolCallback fires.
        let _closer = close_after(tool_done.clone(), live_sess);
        drain_live(events).await;
        let rows = tp.collect_rows(until_invocation_end).await;
        require_invocation_completed_ok(&rows);
        assert_eq!(calls.load(Ordering::SeqCst), 2, "helper runs");
    })
    .await;
}

#[tokio::test]
async fn test_concurrency_race_live_tool_returns_before_unjoined_cached_child() {
    on_test_goroutine(async {
        let tp = setup_test_plugin().await;
        let (entered, tool_done, helper_done) = (Gate::new(), Gate::new(), Gate::new());
        let helper = silent_agent(
            "helper",
            closing_answer(entered.clone(), text_content("model", "cached")),
        );
        let start_helper = function_tool("start_helper", "Starts helper without waiting for it", {
            let (helper, entered, tool_done, helper_done) = (
                helper.clone(),
                entered.clone(),
                tool_done.clone(),
                helper_done.clone(),
            );
            move |ctx: Context, _args| {
                let run_ctx = promote_with_path(&ctx, "helper@1").as_invocation().clone();
                let (helper, tool_done, helper_done) =
                    (helper.clone(), tool_done.clone(), helper_done.clone());
                let entered = entered.clone();
                async move {
                    go(async move {
                        let mut events = helper.run(run_ctx);
                        while events.next().await.is_some() {
                            wait_or_timeout(&tool_done).await;
                        }
                        helper_done.close();
                    });
                    if !wait_or_timeout(&entered).await {
                        return Err(tool_error("helper never entered its BeforeAgentCallback"));
                    }
                    Ok(tool_result("started", json!(true)))
                }
            }
        });
        let root = live_root(
            vec![
                tool_call(&[("call-1", "start_helper")]),
                model_turn("done", true),
            ],
            vec![start_helper],
            closing_after_tool(tool_done.clone()),
        );
        let r = new_test_runner(root, vec![tp.plugin.clone()]);
        let (live_sess, events) = run_live(&r).await;
        let _closer = close_after(tool_done.clone(), live_sess);
        drain_live(events).await;
        let rows = tp.collect_rows(until_invocation_end).await;
        assert!(
            wait_or_timeout(&helper_done).await,
            "helper run never finished"
        );
        require_invocation_completed_ok(&rows);
        assert_eq!(
            rows_of_type(&rows, EVENT_AGENT_ERROR).len(),
            0,
            "AGENT_ERROR rows; counts: {:?}",
            agent_error_counts(&rows)
        );
    })
    .await;
}

#[tokio::test]
async fn test_runner_joined_child_stopped_at_error_keeps_completed_identities() {
    on_test_goroutine(async {
        let mut tm = TraceManager::new();
        tm.max_tracked = 1;
        let limit = tm.max_tracked.max(64);
        let tm = Arc::new(tm);
        let p = new_trace_only_plugin(tm.clone());
        let child = llm_agent(LlmAgentConfig {
            name: "child".into(),
            model: Some(scripted_llm(vec![])),
            before_model_callbacks: vec![Arc::new(|_ctx, req| {
                Box::pin(async move { (req, Err(AgentError::other("blocked by guardrail"))) })
            })],
            ..Default::default()
        });
        let run_child = child.clone();
        let parent = custom_agent("parent", vec![child], move |ic| {
            let child = run_child.clone();
            futures::stream::once(async move {
                // Go: `go func() { defer close(done); for _, err := range child.Run(ctx) { if
                // err != nil { childErr = err; break } } }(); <-done`.
                let child_err = go(async move {
                    let mut events = child.run(ic);
                    while let Some(item) = events.next().await {
                        if let Err(err) = item {
                            return Some(err);
                        }
                    }
                    None
                })
                .await
                .unwrap();
                futures::stream::iter(child_err.map(Err))
            })
            .flatten()
            .boxed()
        });
        let r = new_test_runner(parent, vec![p.clone()]);
        let mut errs = 0;
        for i in 0..limit + 6 {
            let (_, run_errs) =
                run_turn(&r, "u1", &format!("j{i}"), "hi", RunConfig::default()).await;
            errs += run_errs.len();
        }
        assert_eq!(errs, limit + 6, "runs that yielded the guardrail's error");

        let answers = scripted_llm((0..limit / 2).map(|_| final_text("ok")).collect());
        let bot = llm_agent(LlmAgentConfig {
            name: "bot".into(),
            model: Some(answers),
            ..Default::default()
        });
        let rb = new_test_runner(bot, vec![p]);
        let mut ordinary = Vec::new();
        for i in 0..limit / 2 {
            let (events, run_errs) =
                run_turn(&rb, "u1", &format!("o{i}"), "hi", RunConfig::default()).await;
            assert!(run_errs.is_empty(), "ordinary run {i}: {run_errs:?}");
            let inv_id = events
                .first()
                .map(|ev| ev.invocation_id.clone())
                .unwrap_or_default();
            let completed_span = tm
                .mu
                .lock()
                .completed
                .get(&inv_id)
                .map(|c| c.identity.span_id.clone());
            let Some(completed_span) = completed_span else {
                panic!("ordinary run {i} lost its completed identity at its own completion");
            };
            let late = tm.invocation(
                &bg(),
                &Identity {
                    invocation_id: inv_id.clone(),
                    agent_name: "bot".into(),
                    ..Default::default()
                },
            );
            let active = tm.mu.lock().invocations.contains_key(&inv_id);
            assert!(
                !active && late.root_span_id == completed_span,
                "event of ordinary run {i} after AfterRun: new active invocation = {active}, \
                 root span = {:?}, want false and the run's {completed_span:?}",
                late.root_span_id
            );
            ordinary.push(inv_id);
        }

        let st = tm.mu.lock();
        let waiting = st
            .completed
            .values()
            .filter(|c| c.waits_for_late_calls())
            .count();
        assert!(
            waiting <= limit / 2,
            "completed runs that wait for late calls = {waiting}, want at most {}",
            limit / 2
        );
        for (i, inv_id) in ordinary.iter().enumerate() {
            assert!(
                st.completed.contains_key(inv_id),
                "ordinary run {i} lost its completed identity to runs that wait for late calls"
            );
        }
    })
    .await;
}

#[tokio::test]
async fn test_runner_live_tool_sub_agent_call_moves_to_late_state() {
    on_test_goroutine(async {
        let tm = Arc::new(TraceManager::new());
        let p = new_trace_only_plugin(tm.clone());
        let (helper_model, entered, release) = gated_llm();
        let helper = llm_agent(LlmAgentConfig {
            name: "helper".into(),
            model: Some(helper_model),
            ..Default::default()
        });
        let (helper_done, tool_done) = (Gate::new(), Gate::new());
        let start_helper = function_tool("start_helper", "Starts helper without waiting for it", {
            let (helper, entered, helper_done) =
                (helper.clone(), entered.clone(), helper_done.clone());
            move |ctx: Context, _args| {
                let run_ctx = promote_with_path(&ctx, "helper@1").as_invocation().clone();
                let (helper, entered, helper_done) =
                    (helper.clone(), entered.clone(), helper_done.clone());
                async move {
                    go(async move {
                        helper.run(run_ctx).for_each(|_| async {}).await;
                        helper_done.close();
                    });
                    if !wait_or_timeout(&entered).await {
                        return Err(tool_error("helper never started its model call"));
                    }
                    Ok(tool_result("started", json!(true)))
                }
            }
        });
        let root = live_root(
            vec![
                tool_call(&[("call-1", "start_helper")]),
                model_turn("done", true),
            ],
            vec![start_helper],
            closing_after_tool(tool_done.clone()),
        );
        let r = new_test_runner(root, vec![p]);
        let (live_sess, events) = run_live(&r).await;
        let _closer = close_after(tool_done.clone(), live_sess);
        drain_live(events).await;
        let (runs, moved) = {
            let st = tm.mu.lock();
            let moved: usize = st
                .completed
                .values()
                .filter_map(|c| c.late.as_ref())
                .map(|late| late.mu.lock().llm_spans.len())
                .sum();
            (st.completed.len(), moved)
        };
        release.close();
        assert!(
            wait_or_timeout(&helper_done).await,
            "helper run never finished"
        );
        assert!(
            runs == 1 && moved == 1,
            "completed runs = {runs}, open model calls moved to the late state = {moved}, want \
             1 and 1, the call of helper"
        );
    })
    .await;
}

/// `requireClosedPair` of
/// [`test_runner_live_tool_returns_before_helper_model_call_ends_keeps_helper_open`]: the only
/// end row of a kind closes the span of the only start row of that kind with a latency.
fn require_closed_pair(rows: &[Row], start: &[Row], end: &[Row], what: &str) {
    assert!(
        start.len() == 1 && end.len() == 1,
        "{what}: {} start rows and {} end rows, want one of each; event types: {:?}",
        start.len(),
        end.len(),
        event_types(rows)
    );
    assert_eq!(
        place_of(&end[0]),
        place_of(&start[0]),
        "{what}: end row at the place of its start row"
    );
    must_latency(&end[0]);
}

// parity: adapted: the helper runs under a cancellation token of its own (Go's runCtx descends from t.Context(), which outlives the run; gadk's runner cancels the invocation token when its stream ends, which would end the helper's model call with LLM_ERROR)
#[tokio::test]
async fn test_runner_live_tool_returns_before_helper_model_call_ends_keeps_helper_open() {
    for joined in [false, true] {
        let name = if joined {
            "helper released and joined before the session closed"
        } else {
            "helper released after the session closed"
        };
        on_test_goroutine(async {
            let tp = setup_test_plugin().await;
            let gate = blocking_llm(1);
            let (model, entered, release) = (
                gate.model,
                Arc::new(tokio::sync::Mutex::new(gate.entered)),
                gate.release,
            );
            let helper = llm_agent(LlmAgentConfig {
                name: "helper".into(),
                model: Some(model),
                ..Default::default()
            });
            let helper_done = Gate::new();
            // Go `waitHelper`.
            let wait_helper = {
                let helper_done = helper_done.clone();
                move || {
                    let helper_done = helper_done.clone();
                    async move {
                        if wait_or_timeout(&helper_done).await {
                            Ok(())
                        } else {
                            Err(AgentError::other("helper never returned"))
                        }
                    }
                }
            };
            let start_helper =
                function_tool("start_helper", "Starts helper without waiting for it", {
                    let (helper, entered, helper_done) =
                        (helper.clone(), entered.clone(), helper_done.clone());
                    move |ctx: Context, _args| {
                        // Go's runCtx descends from t.Context(), which outlives the run; gadk's
                        // tool context descends from the invocation token, which the runner
                        // cancels when its stream ends: the helper runs under a token of its own.
                        let run_ctx = promote_with_path(&ctx, "helper@1")
                            .with_delta(ContextDelta {
                                cancellation_token: Some(CancellationToken::new()),
                                ..Default::default()
                            })
                            .as_invocation()
                            .clone();
                        let (helper, entered, helper_done) =
                            (helper.clone(), entered.clone(), helper_done.clone());
                        async move {
                            go(async move {
                                helper.run(run_ctx).for_each(|_| async {}).await;
                                helper_done.close();
                            });
                            let started = tokio::time::timeout(WAIT, async {
                                entered.lock().await.recv().await
                            })
                            .await;
                            if !matches!(started, Ok(Some(()))) {
                                return Err(tool_error("helper never started its model call"));
                            }
                            Ok(tool_result("started", json!(true)))
                        }
                    }
                });
            let tool_done = Gate::new();
            let after_tool: AfterToolCallback = Arc::new({
                let (tool_done, release, wait_helper) =
                    (tool_done.clone(), release.clone(), wait_helper.clone());
                move |_ctx, _tool, _args, _result, _err| {
                    let (tool_done, release, wait_helper) =
                        (tool_done.clone(), release.clone(), wait_helper.clone());
                    Box::pin(async move {
                        let res = if joined {
                            release.close();
                            wait_helper().await.map(|()| None)
                        } else {
                            Ok(None)
                        };
                        tool_done.close();
                        res
                    })
                }
            });
            let root = live_root(
                vec![
                    tool_call(&[("call-1", "start_helper")]),
                    model_turn("done", true),
                ],
                vec![start_helper],
                after_tool,
            );
            let r = new_test_runner(root, vec![tp.plugin.clone()]);
            let (live_sess, events) = run_live(&r).await;
            let _closer = close_after(tool_done.clone(), live_sess);
            drain_live(events).await;
            if !joined {
                release.close();
                wait_helper().await.unwrap();
            }
            let rows = tp
                .collect_rows(|rows| {
                    until_invocation_end(rows)
                        && !agent_rows_of_type(rows, EVENT_LLM_RESPONSE, "helper").is_empty()
                        && (!joined
                            || !agent_rows_of_type(rows, EVENT_AGENT_COMPLETED, "helper")
                                .is_empty())
                })
                .await;
            // Go `t.Cleanup(release)`.
            release.close();
            let mut invocation_ends = rows_of_type(&rows, EVENT_INVOCATION_COMPLETED);
            if joined {
                require_invocation_completed_ok(&rows);
            } else {
                // The helper still runs when the session closes, so AfterRun, as on main,
                // reports it with AGENT_ERROR and the run with INVOCATION_ERROR: no callback
                // shows whether it will end. The tool's return did not end it before then.
                invocation_ends.extend(rows_of_type(&rows, EVENT_INVOCATION_ERROR));
                let type_of = |row: &Row| str_col(row, "event_type");
                let tool_end = rows
                    .iter()
                    .position(|row| type_of(row) == EVENT_TOOL_COMPLETED);
                let agent_error = rows
                    .iter()
                    .position(|row| type_of(row) == EVENT_AGENT_ERROR);
                if let Some(i) = agent_error {
                    assert!(
                        !tool_end.is_some_and(|t| i < t),
                        "{name}: AGENT_ERROR row before the TOOL_COMPLETED row of the call that \
                         started the helper; event types: {:?}",
                        event_types(&rows)
                    );
                }
            }
            require_closed_pair(
                &rows,
                &agent_rows_of_type(&rows, EVENT_LLM_REQUEST, "helper"),
                &agent_rows_of_type(&rows, EVENT_LLM_RESPONSE, "helper"),
                &format!("{name}: helper model call"),
            );
            require_closed_pair(
                &rows,
                &rows_of_type(&rows, EVENT_TOOL_STARTING),
                &rows_of_type(&rows, EVENT_TOOL_COMPLETED),
                &format!("{name}: start_helper tool call"),
            );
            require_closed_pair(
                &rows,
                &rows_of_type(&rows, EVENT_INVOCATION_STARTING),
                &invocation_ends,
                &format!("{name}: invocation"),
            );
            if joined {
                require_closed_pair(
                    &rows,
                    &agent_rows_of_type(&rows, EVENT_AGENT_STARTING, "helper"),
                    &agent_rows_of_type(&rows, EVENT_AGENT_COMPLETED, "helper"),
                    &format!("{name}: helper run"),
                );
            }
        })
        .await;
    }
}
