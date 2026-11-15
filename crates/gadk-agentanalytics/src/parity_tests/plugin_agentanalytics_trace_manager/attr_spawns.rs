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
//! group G: goroutines that test agents and tools start themselves (custom fan-outs,
//! unjoined children).
//! Shared helpers: [`attr_support`](super::attr_support).
//!
//! Go → gadk, beyond [`attr_support`](super::attr_support):
//! - a custom agent's iterator body that calls `yield` from goroutines of its own = [`yielding`]:
//!   the body runs when the agent's stream is polled (on the consumer's fiber, as Go's body runs
//!   on the consumer's goroutine), and [`Yield::call`] hands an item to the consumer from any
//!   task and returns once the consumer asks for the next item (`true`) or dropped the stream
//!   (`false`), as Go's `yield` returns;
//! - `time.After(10 * time.Second)` fail-safes = [`within`](super::attr_support::within) (real
//!   time: the tests write rows to a fake BigQuery server);
//! - `ctx.WithICDelta(&agent.InvocationContextDelta{Branch: &b})` = [`on_branch`];
//!   `agent.PromoteWithDelta(ctx, &agent.CommonContextDelta{Path: &p}).WithICDelta(nil)` = the
//!   tool context with the path delta, as an `InvocationContext` ([`promoted`]).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::time::Duration;

use chrono::Utc;
use futures::StreamExt;
use futures::future::BoxFuture;
use gadk_agent::{
    AfterAgentCallback, AfterToolCallback, Agent, AgentConfig, AgentError, CancelCause, Context,
    ContextDelta, EventStream, InvocationContext, RunConfig, StreamingMode, ToolError,
};
use gadk_engine::llm::LlmAgentConfig;
use gadk_genai::{Content, FunctionResponse, Part};
use gadk_model::ModelError;
use gadk_session::Event;
use serde_json::json;
use tokio::sync::{mpsc, oneshot};

use super::attr_support::{
    CloseOnDrop, Gate, blocking_llm, cached_child, content_event, gated_llm, go, goroutine_id,
    partial_then_wait_llm, run_unjoined_late_child, tool_result, within,
};
use super::runner_support::*;
use crate::event_types::*;
use crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::*;
use crate::row::Row;
use crate::trace::TraceManager;

/// An item of an agent's event stream (Go's `(ev, err)` pair).
type Item = Result<Event, AgentError>;

/// Go's `yield` of a custom agent's iterator, callable from any task.
#[derive(Clone)]
struct Yield(mpsc::UnboundedSender<(Item, oneshot::Sender<()>)>);

impl Yield {
    /// Go `yield(ev, err)`: hands `item` to the consumer; `true` once the consumer asks for the
    /// next item, `false` if it dropped the stream (Go's `yield` returning false).
    async fn call(&self, item: Item) -> bool {
        let (ack, acked) = oneshot::channel();
        self.0.send((item, ack)).is_ok() && acked.await.is_ok()
    }
}

/// Go `func(yield func(*session.Event, error) bool) { body }` whose body (or goroutines it
/// starts and joins) may call `yield`: the stream polls `body` and yields every item sent through
/// a [`Yield`]; it ends once `body` returned and every [`Yield`] clone is dropped.
fn yielding<F, Fut>(body: F) -> EventStream
where
    F: FnOnce(Yield) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    struct State {
        rx: mpsc::UnboundedReceiver<(Item, oneshot::Sender<()>)>,
        body: Option<BoxFuture<'static, ()>>,
        ack: Option<oneshot::Sender<()>>,
    }
    enum Step {
        Item(Option<Box<(Item, oneshot::Sender<()>)>>),
        BodyDone,
    }
    let (tx, rx) = mpsc::unbounded_channel();
    let state = State {
        rx,
        body: Some(Box::pin(body(Yield(tx)))),
        ack: None,
    };
    futures::stream::unfold(state, |mut st| async move {
        if let Some(ack) = st.ack.take() {
            let _ = ack.send(());
        }
        loop {
            let step = {
                let body = async {
                    match st.body.as_mut() {
                        Some(b) => b.await,
                        None => std::future::pending().await,
                    }
                };
                tokio::select! {
                    biased;
                    item = st.rx.recv() => Step::Item(item.map(Box::new)),
                    () = body => Step::BodyDone,
                }
            };
            match step {
                Step::BodyDone => st.body = None,
                Step::Item(None) => {
                    // The body dropped its `Yield` without returning: it still runs to its end.
                    if let Some(body) = st.body.take() {
                        body.await;
                    }
                    return None;
                }
                Step::Item(Some(sent)) => {
                    let (item, ack) = *sent;
                    st.ack = Some(ack);
                    return Some((item, st));
                }
            }
        }
    })
    .boxed()
}

/// Go `for range events {}`: reads `events` to the end.
async fn drain(mut events: EventStream) {
    while events.next().await.is_some() {}
}

/// `agent.New(cfg)`.
fn new_agent(cfg: AgentConfig) -> Arc<dyn Agent> {
    gadk_agent::new_agent(cfg).expect("agent")
}

/// Go `ctx.WithICDelta(&agent.InvocationContextDelta{Branch: &branch})`.
fn on_branch(ctx: &InvocationContext, branch: &str) -> InvocationContext {
    ctx.with_delta(ContextDelta {
        branch: Some(branch.into()),
        ..Default::default()
    })
}

/// Go `agent.PromoteWithDelta(ctx, &agent.CommonContextDelta{Path: &path}).WithICDelta(nil)`.
fn promoted(ctx: &Context, path: &str) -> InvocationContext {
    ctx.with_delta(ContextDelta {
        path: Some(path.into()),
        ..Default::default()
    })
    .as_invocation()
    .clone()
}

/// Go `path := leaf; if parentPath := ctx.Path(); parentPath != "" { path = parentPath + "/" +
/// leaf }`.
fn sub_path(ctx: &Context, leaf: &str) -> String {
    match ctx.path() {
        p if p.is_empty() => leaf.to_owned(),
        p => format!("{p}/{leaf}"),
    }
}

/// Go's `sibling`: answers "sibling answer", then (if `then` is some) closes the first gate and
/// waits for the second (else yields the error `missing`).
fn answering_sibling(then: Option<(Gate, Gate, &'static str)>) -> Arc<dyn Agent> {
    new_agent(AgentConfig {
        name: "sibling".into(),
        run: Some(Arc::new(move |ctx: InvocationContext| {
            let then = then.clone();
            yielding(move |y| async move {
                if !y
                    .call(Ok(content_event(&ctx, "sibling", "sibling answer")))
                    .await
                {
                    return;
                }
                if let Some((answered, wait, missing)) = then {
                    answered.close();
                    if !within(&wait).await {
                        y.call(Err(AgentError::other(missing))).await;
                    }
                }
            })
        })),
        ..Default::default()
    })
}

/// Go `go func() { defer wg.Done(); [wait]; for _, runErr := range child.Run(ctx) { if runErr !=
/// nil && cachedErr == nil { cachedErr = runErr } } }()`: the first error of the run (or
/// `missing` when `wait` did not close), dropping the run's events.
fn go_cached(
    child: Arc<dyn Agent>,
    ctx: InvocationContext,
    wait: Option<(Gate, &'static str)>,
) -> tokio::task::JoinHandle<Option<AgentError>> {
    go(async move {
        if let Some((gate, missing)) = wait
            && !within(&gate).await
        {
            return Some(AgentError::other(missing));
        }
        let mut first = None;
        let mut events = child.run(ctx);
        while let Some(item) = events.next().await {
            if let Err(e) = item
                && first.is_none()
            {
                first = Some(e);
            }
        }
        first
    })
}

/// Go `go func() { defer wg.Done(); [wait]; for ev, runErr := range sibling.Run(ctx) { if
/// !yield(ev, runErr) { stoppedOut = true; return } } }()`: whether the consumer stopped.
fn go_forward(
    sibling: Arc<dyn Agent>,
    ctx: InvocationContext,
    y: Yield,
    wait: Option<(Gate, &'static str)>,
) -> tokio::task::JoinHandle<bool> {
    go(async move {
        if let Some((gate, missing)) = wait
            && !within(&gate).await
        {
            y.call(Err(AgentError::other(missing))).await;
            return false;
        }
        let mut events = sibling.run(ctx);
        while let Some(item) = events.next().await {
            if !y.call(item).await {
                return true;
            }
        }
        false
    })
}

/// The parent of the cached-child races: answers "parent answer", runs `cached` (on
/// `cached_branch`, after `cached_wait`) and the forwarded `sibling` (on `sibling_branch`, after
/// `sibling_wait`) on goroutines of its own, joins both and yields the cached child's error
/// unless the consumer stopped.
#[derive(Clone)]
struct CachedRace {
    cached: Arc<dyn Agent>,
    sibling: Arc<dyn Agent>,
    cached_branch: Option<&'static str>,
    sibling_branch: Option<&'static str>,
    cached_wait: Option<(Gate, &'static str)>,
    sibling_wait: Option<(Gate, &'static str)>,
}

impl CachedRace {
    fn parent(self, declare_sub_agents: bool) -> Arc<dyn Agent> {
        let sub_agents = if declare_sub_agents {
            vec![self.cached.clone(), self.sibling.clone()]
        } else {
            vec![]
        };
        new_agent(AgentConfig {
            name: "parent".into(),
            sub_agents,
            run: Some(Arc::new(move |ctx: InvocationContext| {
                let race = self.clone();
                yielding(move |y| async move {
                    if !y
                        .call(Ok(content_event(&ctx, "parent", "parent answer")))
                        .await
                    {
                        return;
                    }
                    let branch =
                        |b: Option<&str>| b.map_or_else(|| ctx.clone(), |b| on_branch(&ctx, b));
                    let cached =
                        go_cached(race.cached, branch(race.cached_branch), race.cached_wait);
                    let sibling = go_forward(
                        race.sibling,
                        branch(race.sibling_branch),
                        y.clone(),
                        race.sibling_wait,
                    );
                    let cached_err = cached.await.unwrap();
                    let stopped_out = sibling.await.unwrap();
                    if !stopped_out && let Some(e) = cached_err {
                        y.call(Err(e)).await;
                    }
                })
            })),
            ..Default::default()
        })
    }
}

/// Go `for _, err := range newTestRunner(t, root, p).Run(t.Context(), "u1", "s1",
/// textContent("user", "go"), agent.RunConfig{}) { if err != nil { t.Fatalf(…) } }` followed by
/// `requireInvocationCompletedOK(t, collectRows(t, requests, untilInvocationEnd))`; returns the
/// rows.
async fn run_completed_ok(tp: &TestPlugin, root: Arc<dyn Agent>, what: &str) -> Vec<Row> {
    let r = new_test_runner(root, vec![tp.plugin.clone()]);
    let (_, errs) = run_turn(&r, "u1", "s1", "go", RunConfig::default()).await;
    assert!(errs.is_empty(), "{what}: runner.Run error: {errs:?}");
    let rows = RowCursor::default().collect(tp, until_invocation_end).await;
    require_invocation_completed_ok(&rows);
    rows
}

/// Go `for _, err := range r.Run(…) { if err != nil { gotErr = err; break } }`: the first error.
async fn first_run_error(tp: &TestPlugin, root: Arc<dyn Agent>) -> Option<AgentError> {
    let r = new_test_runner(root, vec![tp.plugin.clone()]);
    let (_, mut errs) = run_until(&r, "u1", "s1", "go", RunConfig::default(), Result::is_err).await;
    (!errs.is_empty()).then(|| errs.remove(0))
}

#[tokio::test]
async fn test_trace_manager_concurrent_children_same_branch_cached_end_invocation_race() {
    for declare_sub_agents in [true, false] {
        for sibling_ends in [false, true] {
            let label = format!(
                "{}/{}",
                if declare_sub_agents {
                    "with SubAgents declared"
                } else {
                    "without SubAgents declared"
                },
                if sibling_ends {
                    "sibling ends while cached answers"
                } else {
                    "sibling starts while cached answers"
                }
            );
            let tp = setup_test_plugin().await;
            let child1_answered = Gate::new();
            let sibling_started = Gate::new();
            let race = CachedRace {
                cached: cached_child("cached_child", "cache hit", child1_answered.clone()),
                sibling: answering_sibling(sibling_ends.then(|| {
                    (
                        sibling_started.clone(),
                        child1_answered.clone(),
                        "cached_child did not answer",
                    )
                })),
                cached_branch: None,
                sibling_branch: None,
                // Drop cached_child's event so only contextEnded settled after parent joins can
                // mark it responded.
                cached_wait: sibling_ends
                    .then(|| (sibling_started.clone(), "sibling did not start")),
                sibling_wait: (!sibling_ends)
                    .then(|| (child1_answered.clone(), "cached_child did not answer")),
            };
            run_completed_ok(&tp, race.parent(declare_sub_agents), &label).await;
        }
    }
}

#[tokio::test]
async fn test_concurrency_race_cached_child_on_descendant_branch_under_undeclared_sibling() {
    for declare_sub_agents in [false, true] {
        let tp = setup_test_plugin().await;
        let sibling_answered = Gate::new();
        let cached_answering = Gate::new();
        let race = CachedRace {
            cached: cached_child("cached_child", "cache hit", cached_answering.clone()),
            // End while cached_child answers on its nested branch.
            sibling: answering_sibling(Some((
                sibling_answered.clone(),
                cached_answering.clone(),
                "cached_child did not answer",
            ))),
            cached_branch: Some("nested"),
            sibling_branch: None,
            cached_wait: Some((sibling_answered.clone(), "sibling did not answer")),
            sibling_wait: None,
        };
        run_completed_ok(
            &tp,
            race.parent(declare_sub_agents),
            &format!("declared {declare_sub_agents}"),
        )
        .await;
    }
}

#[tokio::test]
async fn test_concurrency_race_same_override_branch_sibling_and_cached_child() {
    for declare_sub_agents in [false, true] {
        let tp = setup_test_plugin().await;
        let sibling_answered = Gate::new();
        let cached_answering = Gate::new();
        let race = CachedRace {
            cached: cached_child("cached_child", "cache hit", cached_answering.clone()),
            sibling: answering_sibling(Some((
                sibling_answered.clone(),
                cached_answering.clone(),
                "cached_child did not answer",
            ))),
            cached_branch: Some("nested"),
            sibling_branch: Some("nested"),
            cached_wait: Some((sibling_answered.clone(), "sibling did not answer")),
            sibling_wait: None,
        };
        run_completed_ok(
            &tp,
            race.parent(declare_sub_agents),
            &format!("declared {declare_sub_agents}"),
        )
        .await;
    }
}

#[tokio::test]
async fn test_concurrency_race_nested_descendant_branch_cached_child_under_override_sibling() {
    let tp = setup_test_plugin().await;
    let sibling_answered = Gate::new();
    let cached_entering = Gate::new();
    let race = CachedRace {
        cached: cached_child("cached_child", "cache hit", cached_entering.clone()),
        sibling: answering_sibling(Some((
            sibling_answered.clone(),
            cached_entering.clone(),
            "cached_child did not enter callback",
        ))),
        cached_branch: Some("b1.nested"),
        sibling_branch: Some("b1"),
        cached_wait: Some((sibling_answered.clone(), "sibling did not answer")),
        sibling_wait: None,
    };
    run_completed_ok(&tp, race.parent(false), "undeclared").await;
}

/// Go's `item{ev, err, ack}` of the parents that read their sub-agents' goroutines through a
/// channel.
type Acked = (Item, Option<oneshot::Sender<bool>>);

/// Go `ch <- item{ev, err, ack}; select { case cont := <-ack: …; case <-done: return }`: sends
/// `item` with an ack and waits for it; whether to go on.
async fn send_acked(ch: &mpsc::Sender<Acked>, item: Item, done: &Gate) -> bool {
    let (ack, acked) = oneshot::channel();
    if ch.send((item, Some(ack))).await.is_err() {
        return false;
    }
    tokio::select! {
        cont = acked => cont.unwrap_or(false),
        () = done.wait() => false,
    }
}

#[tokio::test]
async fn test_concurrency_race_concurrent_same_custom_agent_failure_not_masked_by_peer_response() {
    for (name, buffer_until_retired) in [
        ("response delivered before peer retires", false),
        ("buffered response delivered after peer retires", true),
    ] {
        let tp = setup_test_plugin().await;
        let entered = Arc::new(AtomicI32::new(0));
        let both_open = Gate::new();
        let run1_done = Gate::new();
        let run1_delivered = Gate::new();
        let run2_err = "second child activation failed before responding";

        let child = new_agent(AgentConfig {
            name: "child".into(),
            run: Some(Arc::new({
                let (entered, both_open, run1_delivered) =
                    (entered.clone(), both_open.clone(), run1_delivered.clone());
                move |ctx: InvocationContext| {
                    let (entered, both_open, run1_delivered) =
                        (entered.clone(), both_open.clone(), run1_delivered.clone());
                    yielding(move |y| async move {
                        let my_idx = entered.fetch_add(1, Ordering::SeqCst) + 1;
                        if my_idx == 2 {
                            both_open.close();
                        } else if !within(&both_open).await {
                            y.call(Err(AgentError::other("second run never started")))
                                .await;
                            return;
                        }
                        if my_idx == 1 {
                            y.call(Ok(content_event(&ctx, "child", "run 1 answer")))
                                .await;
                            return;
                        }
                        if !within(&run1_delivered).await {
                            y.call(Err(AgentError::other("run 1 barrier timed out")))
                                .await;
                            return;
                        }
                        y.call(Err(AgentError::other(run2_err))).await;
                    })
                }
            })),
            ..Default::default()
        });

        let run_child = child.clone();
        let parent = new_agent(AgentConfig {
            name: "parent".into(),
            sub_agents: vec![child],
            run: Some(Arc::new(move |ctx: InvocationContext| {
                let (child, run1_done, run1_delivered) =
                    (run_child.clone(), run1_done.clone(), run1_delivered.clone());
                yielding(move |y| async move {
                    let (ch, mut items) = mpsc::channel::<Acked>(8);
                    let done = Gate::new();
                    let _close_done = CloseOnDrop(done.clone());
                    for _ in 0..2 {
                        let (ch, done, run1_done, child, ctx) = (
                            ch.clone(),
                            done.clone(),
                            run1_done.clone(),
                            child.clone(),
                            ctx.clone(),
                        );
                        go(async move {
                            let mut saw_ev = false;
                            let mut events = child.run(ctx);
                            while let Some(item) = events.next().await {
                                if item.is_ok() {
                                    saw_ev = true;
                                    if !buffer_until_retired {
                                        if !send_acked(&ch, item, &done).await {
                                            return;
                                        }
                                        continue;
                                    }
                                }
                                let failed = item.is_err();
                                let _ = ch.send((item, None)).await;
                                if failed {
                                    break;
                                }
                            }
                            if saw_ev {
                                run1_done.close();
                            }
                        });
                    }
                    // Go `go func() { wg.Wait(); close(ch) }()`: the channel closes once
                    // both goroutines dropped their senders.
                    drop(ch);
                    let mut notified = false;
                    while let Some((item, ack)) = items.recv().await {
                        let is_ev = item.is_ok();
                        if buffer_until_retired && is_ev && !within(&run1_done).await {
                            y.call(Err(AgentError::other("peer retirement timed out")))
                                .await;
                            return;
                        }
                        let cont = y.call(item).await;
                        if let Some(ack) = ack {
                            let _ = ack.send(cont);
                        }
                        if !cont {
                            return;
                        }
                        if is_ev && !notified {
                            notified = true;
                            run1_delivered.close();
                        }
                    }
                })
            })),
            ..Default::default()
        });

        let got = first_run_error(&tp, parent).await;
        assert_eq!(
            got.as_ref().map(ToString::to_string).as_deref(),
            Some(run2_err),
            "{name}: runner.Run error"
        );
        let rows = RowCursor::default()
            .collect(&tp, until_invocation_end)
            .await;
        only_row_of_type(&rows, EVENT_INVOCATION_ERROR);
        assert_eq!(
            rows_of_type(&rows, EVENT_INVOCATION_COMPLETED).len(),
            0,
            "{name}: INVOCATION_COMPLETED rows; event types: {:?}",
            event_types(&rows)
        );
        let counts = agent_error_counts(&rows);
        assert_eq!(
            counts.get("child").copied().unwrap_or(0),
            1,
            "{name}: AGENT_ERROR count for child; counts={counts:?}, event types={:?}",
            event_types(&rows)
        );
        assert_eq!(
            counts.get("parent").copied().unwrap_or(0),
            1,
            "{name}: AGENT_ERROR count for parent; counts={counts:?}, event types={:?}",
            event_types(&rows)
        );
    }
}

#[tokio::test]
async fn test_concurrency_race_concurrent_silent_custom_agent_runs_emits_invocation_completed() {
    let tp = setup_test_plugin().await;
    let entered = Arc::new(AtomicI32::new(0));
    let both_open = Gate::new();
    let silent_child = new_agent(AgentConfig {
        name: "silent_child".into(),
        run: Some(Arc::new(move |_ctx: InvocationContext| {
            let (entered, both_open) = (entered.clone(), both_open.clone());
            yielding(move |y| async move {
                if entered.fetch_add(1, Ordering::SeqCst) + 1 == 2 {
                    both_open.close();
                } else if !within(&both_open).await {
                    y.call(Err(AgentError::other("second silent run never started")))
                        .await;
                }
            })
        })),
        ..Default::default()
    });
    let run_child = silent_child.clone();
    let parent = new_agent(AgentConfig {
        name: "parent".into(),
        sub_agents: vec![silent_child],
        run: Some(Arc::new(move |ctx: InvocationContext| {
            let child = run_child.clone();
            yielding(move |y| async move {
                let runs: Vec<_> = (0..2)
                    .map(|_| go_cached(child.clone(), ctx.clone(), None))
                    .collect();
                for run in runs {
                    if let Some(e) = run.await.unwrap()
                        && !y.call(Err(e)).await
                    {
                        return;
                    }
                }
            })
        })),
        ..Default::default()
    });
    let rows = run_completed_ok(&tp, parent, "silent runs").await;
    assert_eq!(
        rows_of_type(&rows, EVENT_AGENT_COMPLETED).len(),
        3,
        "AGENT_COMPLETED rows; event types: {:?}",
        event_types(&rows)
    );
    assert_eq!(
        rows_of_type(&rows, EVENT_AGENT_ERROR).len(),
        0,
        "AGENT_ERROR rows; counts: {:?}",
        agent_error_counts(&rows)
    );
}

#[derive(Clone, Copy)]
struct SameLlmMode {
    name: &'static str,
    break_on_err: bool,
    with_trailer: bool,
    hold_until_trailer: bool,
    prior_tool_turn: bool,
}

// parity: adapted: Go marks the failing activation's context with a value
// (`context.WithValue(ctx, concurrentFailRunKey{}, true)`) that its model and AfterAgentCallback
// read; gadk's model API and contexts carry no such values, so the failing goroutine records its
// fiber id and the model and the callback compare the fiber they run on (both run on the
// goroutine that reads that activation's events, in Go as here).
#[tokio::test]
async fn test_concurrency_race_concurrent_same_llm_agent_model_error_not_cleared_by_peer_success() {
    let mode =
        |name, break_on_err, with_trailer, hold_until_trailer, prior_tool_turn| SameLlmMode {
            name,
            break_on_err,
            with_trailer,
            hold_until_trailer,
            prior_tool_turn,
        };
    let modes = [
        mode("drain failing activation", false, false, false, false),
        mode(
            "break on failing activation error",
            true,
            false,
            false,
            false,
        ),
        mode(
            "drain failing activation/with_peer_after_agent_trailer",
            false,
            true,
            false,
            false,
        ),
        mode(
            "hold failing activation open until trailer/with_peer_after_agent_trailer",
            false,
            true,
            true,
            false,
        ),
        mode(
            "break on failing activation error/with_peer_after_agent_trailer",
            true,
            true,
            false,
            false,
        ),
        mode(
            "drain failing activation/prior_tool_turn",
            false,
            false,
            false,
            true,
        ),
        mode(
            "break on failing activation error/prior_tool_turn",
            true,
            false,
            false,
            true,
        ),
        mode(
            "drain failing activation/with_peer_after_agent_trailer/prior_tool_turn",
            false,
            true,
            false,
            true,
        ),
        mode(
            "hold failing activation open until trailer/with_peer_after_agent_trailer/prior_tool_turn",
            false,
            true,
            true,
            true,
        ),
        mode(
            "break on failing activation error/with_peer_after_agent_trailer/prior_tool_turn",
            true,
            true,
            false,
            true,
        ),
    ];
    for mode in modes {
        for failing_first in [true, false] {
            let order = if failing_first {
                "failing run starts first"
            } else {
                "succeeding run starts first"
            };
            same_llm_agent_case(mode, failing_first, &format!("{}/{order}", mode.name)).await;
        }
    }
}

async fn same_llm_agent_case(mode: SameLlmMode, failing_first: bool, name: &str) {
    let tp = setup_test_plugin().await;
    let started = Arc::new(AtomicI32::new(0));
    let in_model = Arc::new(AtomicI32::new(0));
    let first_started = Gate::new();
    let both_in_model = Gate::new();
    let fail_ready = Gate::new();
    let trailer_delivered = Gate::new();
    let model_err = "quota exceeded on failing activation";
    // Go `ctx.Value(concurrentFailRunKey{}) != nil`: the fiber of the failing goroutine.
    let fail_fiber = Arc::new(AtomicU64::new(0));
    let is_fail = {
        let fail_fiber = fail_fiber.clone();
        move || goroutine_id() == fail_fiber.load(Ordering::SeqCst)
    };
    // failCallCount tracks the number of model calls made by the failing activation, so a
    // prior tool turn's function call is returned on the first call and the error on the
    // second.
    let fail_call_count = Arc::new(AtomicI32::new(0));

    let mdl = fn_llm("test-model", {
        let (in_model, both_in_model, fail_ready, is_fail) = (
            in_model.clone(),
            both_in_model.clone(),
            fail_ready.clone(),
            is_fail.clone(),
        );
        move |_, _| {
            let fail = is_fail();
            let (in_model, both_in_model, fail_ready, fail_call_count) = (
                in_model.clone(),
                both_in_model.clone(),
                fail_ready.clone(),
                fail_call_count.clone(),
            );
            futures::stream::once(async move {
                if in_model.fetch_add(1, Ordering::SeqCst) + 1 == 2 {
                    both_in_model.close();
                } else if !within(&both_in_model).await {
                    return Err(ModelError::Other(
                        "timed out waiting for peer model call".into(),
                    ));
                }
                if mode.prior_tool_turn && fail {
                    if fail_call_count.fetch_add(1, Ordering::SeqCst) + 1 == 1 {
                        // First call of the failing activation: return a function call so the
                        // tool turn runs.
                        return calls(vec![function_call_part("probe", json!({}))]);
                    }
                    // Second call of the failing activation fails.
                    return Err(ModelError::Other(model_err.into()));
                }
                if fail {
                    return Err(ModelError::Other(model_err.into()));
                }
                if !within(&fail_ready).await {
                    return Err(ModelError::Other(
                        "timed out waiting for failing activation".into(),
                    ));
                }
                final_text("peer ok")
            })
            .boxed()
        }
    });
    let mut worker_cfg = LlmAgentConfig {
        name: "worker".into(),
        model: Some(mdl),
        before_agent_callbacks: vec![Arc::new({
            let (started, first_started) = (started.clone(), first_started.clone());
            move |_ctx: Context| {
                if started.fetch_add(1, Ordering::SeqCst) + 1 == 1 {
                    first_started.close();
                }
                Box::pin(async { Ok(None) })
            }
        })],
        ..Default::default()
    };
    if mode.prior_tool_turn {
        worker_cfg.tools = vec![function_tool(
            "probe",
            "Probes before failing",
            |_ctx, _args| async move { Ok(json!({"ok": true}).as_object().cloned().unwrap()) },
        )];
    }
    if mode.with_trailer {
        let is_fail = is_fail.clone();
        let trailer: AfterAgentCallback = Arc::new(move |_ctx: Context| {
            let answer = (!is_fail()).then(|| text_content("model", "peer trailer"));
            Box::pin(async move { Ok(answer) })
        });
        worker_cfg.after_agent_callbacks = vec![trailer];
    }
    let worker = llm_agent(worker_cfg);

    let run_worker = worker.clone();
    let parent = new_agent(AgentConfig {
        name: "parent".into(),
        sub_agents: vec![worker],
        run: Some(Arc::new(move |ctx: InvocationContext| {
            let worker = run_worker.clone();
            let (first_started, fail_ready, trailer_delivered, fail_fiber) = (
                first_started.clone(),
                fail_ready.clone(),
                trailer_delivered.clone(),
                fail_fiber.clone(),
            );
            yielding(move |y| async move {
                let (ch, mut items) = mpsc::channel::<Acked>(8);
                let done = Gate::new();
                let _close_done = CloseOnDrop(done.clone());
                let failing = go({
                    let (ch, worker, ctx, done, first_started, fail_ready, trailer_delivered) = (
                        ch.clone(),
                        worker.clone(),
                        ctx.clone(),
                        done.clone(),
                        first_started.clone(),
                        fail_ready.clone(),
                        trailer_delivered.clone(),
                    );
                    async move {
                        // Holds the channel open until this goroutine returns (Go's wg).
                        let _ch = ch;
                        let _fail_ready = CloseOnDrop(fail_ready.clone());
                        if !failing_first && !within(&first_started).await {
                            return Some(AgentError::other("succeeding run never started"));
                        }
                        fail_fiber.store(goroutine_id(), Ordering::SeqCst);
                        let mut err1 = None;
                        let mut events = worker.run(ctx);
                        while let Some(item) = events.next().await {
                            let Err(e) = item else { continue };
                            if err1.is_some() {
                                continue;
                            }
                            err1 = Some(e);
                            if mode.break_on_err {
                                break;
                            }
                            if mode.hold_until_trailer {
                                fail_ready.close();
                                tokio::select! {
                                    () = trailer_delivered.wait() => {}
                                    () = done.wait() => return err1,
                                    () = tokio::time::sleep(Duration::from_secs(10)) => return err1,
                                }
                            }
                        }
                        err1
                    }
                });
                go({
                    let (ch, worker, ctx, done, first_started) = (
                        ch.clone(),
                        worker.clone(),
                        ctx.clone(),
                        done.clone(),
                        first_started.clone(),
                    );
                    async move {
                        if failing_first && !within(&first_started).await {
                            let _ = ch
                                .send((Err(AgentError::other("failing run never started")), None))
                                .await;
                            return;
                        }
                        let mut events = worker.run(ctx);
                        while let Some(item) = events.next().await {
                            if item.is_ok() {
                                if !send_acked(&ch, item, &done).await {
                                    return;
                                }
                                continue;
                            }
                            let _ = ch.send((item, None)).await;
                        }
                    }
                });
                drop(ch);
                while let Some((item, ack)) = items.recv().await {
                    let trailer = matches!(&item, Ok(ev) if ev
                        .response
                        .content
                        .as_ref()
                        .and_then(|c| c.parts.first())
                        .is_some_and(|p| p.text == "peer trailer"));
                    let cont = y.call(item).await;
                    if let Some(ack) = ack {
                        let _ = ack.send(cont);
                    }
                    if trailer {
                        trailer_delivered.close();
                    }
                    if !cont {
                        return;
                    }
                }
                if let Some(e) = failing.await.unwrap() {
                    y.call(Err(e)).await;
                }
            })
        })),
        ..Default::default()
    });

    let got = first_run_error(&tp, parent).await;
    assert_eq!(
        got.as_ref().map(ToString::to_string).as_deref(),
        Some(model_err),
        "{name}: runner.Run error"
    );
    let rows = RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await;
    let inv_err = only_row_of_type(&rows, EVENT_INVOCATION_ERROR);
    assert_eq!(
        str_col(&inv_err, "error_message"),
        model_err,
        "{name}: INVOCATION_ERROR error_message"
    );
    let llm_reqs = rows_of_type(&rows, EVENT_LLM_REQUEST);
    let llm_resps = rows_of_type(&rows, EVENT_LLM_RESPONSE);
    let llm_errs = rows_of_type(&rows, EVENT_LLM_ERROR);
    // The failing activation makes two model calls with a prior tool turn (one returning a
    // function call, one failing), so 3 LLM_REQUEST, 2 LLM_RESPONSE (one for the function call,
    // one for the peer) and 1 LLM_ERROR.
    let want = if mode.prior_tool_turn {
        (3, 2, 1)
    } else {
        (2, 1, 1)
    };
    assert_eq!(
        (llm_reqs.len(), llm_resps.len(), llm_errs.len()),
        want,
        "{name}: LLM rows req/resp/err; event types: {:?}",
        event_types(&rows)
    );
    assert_ne!(
        str_col(&llm_resps[0], "span_id"),
        str_col(&llm_errs[0], "span_id"),
        "{name}: LLM_RESPONSE and LLM_ERROR shared a span_id, want distinct model spans"
    );
    let want_worker_errs = usize::from(mode.break_on_err);
    let counts = agent_error_counts(&rows);
    assert_eq!(
        counts.get("worker").copied().unwrap_or(0),
        want_worker_errs,
        "{name}: AGENT_ERROR count for worker; counts={counts:?}"
    );
    assert_eq!(
        counts.get("parent").copied().unwrap_or(0),
        want_worker_errs,
        "{name}: AGENT_ERROR count for parent; counts={counts:?}"
    );
}

#[tokio::test]
async fn test_concurrency_race_parallel_looking_branch_override_under_custom_parent() {
    let tp = setup_test_plugin().await;
    let sibling_ready = Gate::new();
    let cached_answering = Gate::new();
    let cached = cached_child("cached_child", "cached shortcut", cached_answering.clone());
    let sibling = answering_sibling(Some((
        sibling_ready.clone(),
        cached_answering,
        "cached_child did not answer",
    )));
    let parent = new_agent(AgentConfig {
        name: "parent".into(),
        run: Some(Arc::new(move |ctx: InvocationContext| {
            let (cached, sibling, sibling_ready) =
                (cached.clone(), sibling.clone(), sibling_ready.clone());
            yielding(move |y| async move {
                if !y
                    .call(Ok(content_event(&ctx, "parent", "parent answer")))
                    .await
                {
                    return;
                }
                let (ch, mut items) = mpsc::channel::<Acked>(4);
                let done = Gate::new();
                let _close_done = CloseOnDrop(done.clone());
                go({
                    let (ch, done, sib_ctx) =
                        (ch.clone(), done.clone(), on_branch(&ctx, "parent.sibling"));
                    async move {
                        let mut events = sibling.run(sib_ctx);
                        while let Some(item) = events.next().await {
                            if item.is_ok() {
                                if !send_acked(&ch, item, &done).await {
                                    return;
                                }
                                continue;
                            }
                            let _ = ch.send((item, None)).await;
                        }
                    }
                });
                go({
                    let (ch, nested_ctx) = (ch.clone(), on_branch(&ctx, "parent.sibling.nested"));
                    async move {
                        if !within(&sibling_ready).await {
                            let _ = ch
                                .send((Err(AgentError::other("sibling did not start")), None))
                                .await;
                            return;
                        }
                        // Drop cached_child's event so settlement relies on contextEnded.
                        let mut events = cached.run(nested_ctx);
                        while let Some(item) = events.next().await {
                            if let Err(e) = item {
                                let _ = ch.send((Err(e), None)).await;
                                return;
                            }
                        }
                    }
                });
                drop(ch);
                while let Some((item, ack)) = items.recv().await {
                    let cont = y.call(item).await;
                    if let Some(ack) = ack {
                        let _ = ack.send(cont);
                    }
                    if !cont {
                        return;
                    }
                }
            })
        })),
        ..Default::default()
    });
    run_completed_ok(&tp, parent, "parallel-looking branch").await;
}

#[tokio::test]
async fn test_concurrency_race_custom_parent_returns_before_unjoined_child_ends_invocation() {
    for declare_sub_agents in [true, false] {
        let name = if declare_sub_agents {
            "declared_subagent"
        } else {
            "undeclared_subagent"
        };

        // (a) unjoined child answered by BeforeAgentCallback.
        {
            let tp = setup_test_plugin().await;
            let child_in_before = Gate::new();
            let child_done = Gate::new();
            let child = cached_child("unjoined_child", "shortcut", child_in_before.clone());
            let run_child = child.clone();
            let parent = new_agent(AgentConfig {
                name: "parent".into(),
                sub_agents: if declare_sub_agents {
                    vec![child]
                } else {
                    vec![]
                },
                run: Some(Arc::new({
                    let child_done = child_done.clone();
                    move |ctx: InvocationContext| {
                        let (child, child_in_before, child_done) = (
                            run_child.clone(),
                            child_in_before.clone(),
                            child_done.clone(),
                        );
                        yielding(move |y| async move {
                            if !y
                                .call(Ok(content_event(&ctx, "parent", "parent answer")))
                                .await
                            {
                                return;
                            }
                            go(async move {
                                let _done = CloseOnDrop(child_done);
                                drain(child.run(ctx)).await;
                            });
                            if !within(&child_in_before).await {
                                y.call(Err(AgentError::other(
                                    "unjoined_child never entered BeforeAgentCallback",
                                )))
                                .await;
                            }
                            // Return immediately without waiting for childDone so parent's
                            // AfterAgentCallback overlaps child's EndInvocation().
                        })
                    }
                })),
                ..Default::default()
            });
            run_completed_ok(&tp, parent, &format!("{name}/unjoined_cached_child")).await;
            assert!(
                within(&child_done).await,
                "{name}/unjoined_cached_child: background child goroutine did not finish"
            );
        }

        // (b) completed_sibling_then_unjoined_cached_child: a parent that yields its own
        // answer, then runs a sibling child to completion (forwarding its events), then starts
        // a cached child (answered by BeforeAgentCallback) without joining it and returns.
        {
            let tp = setup_test_plugin().await;
            let cached_in_before = Gate::new();
            let cached_done = Gate::new();
            let cached = cached_child("cached_child", "cache hit", cached_in_before.clone());
            let sibling = new_agent(AgentConfig {
                name: "sibling".into(),
                run: Some(Arc::new(|ctx: InvocationContext| {
                    futures::stream::iter([Ok(content_event(&ctx, "sibling", "sibling answer"))])
                        .boxed()
                })),
                ..Default::default()
            });
            let (run_cached, run_sibling) = (cached.clone(), sibling.clone());
            let parent = new_agent(AgentConfig {
                name: "parent".into(),
                sub_agents: if declare_sub_agents {
                    vec![sibling, cached]
                } else {
                    vec![]
                },
                run: Some(Arc::new({
                    let cached_done = cached_done.clone();
                    move |ctx: InvocationContext| {
                        let (cached, sibling, cached_in_before, cached_done) = (
                            run_cached.clone(),
                            run_sibling.clone(),
                            cached_in_before.clone(),
                            cached_done.clone(),
                        );
                        yielding(move |y| async move {
                            if !y
                                .call(Ok(content_event(&ctx, "parent", "parent answer")))
                                .await
                            {
                                return;
                            }
                            // Run the sibling to completion, forwarding its events.
                            let mut events = sibling.run(ctx.clone());
                            while let Some(item) = events.next().await {
                                if !y.call(item).await {
                                    return;
                                }
                            }
                            // Start the cached child without joining it.
                            go(async move {
                                let _done = CloseOnDrop(cached_done);
                                drain(cached.run(ctx)).await;
                            });
                            if !within(&cached_in_before).await {
                                y.call(Err(AgentError::other(
                                    "cached_child never entered BeforeAgentCallback",
                                )))
                                .await;
                            }
                            // Return without waiting for cachedDone so the parent's
                            // AfterAgentCallback overlaps the cached child's EndInvocation().
                        })
                    }
                })),
                ..Default::default()
            });
            run_completed_ok(
                &tp,
                parent,
                &format!("{name}/completed_sibling_then_unjoined_cached_child"),
            )
            .await;
            assert!(
                within(&cached_done).await,
                "{name}/completed_sibling_then_unjoined_cached_child: background cached child goroutine did not finish"
            );
        }

        // (c) joined_cached_child_without_forwarding: a parent that yields its own answer, then
        // starts a cached child (answered by BeforeAgentCallback) on a goroutine, joins it
        // (waits for it to finish), but does NOT forward its event.
        {
            let tp = setup_test_plugin().await;
            let cached = cached_child("cached_child", "cache hit", Gate::new());
            let run_cached = cached.clone();
            let parent = new_agent(AgentConfig {
                name: "parent".into(),
                sub_agents: if declare_sub_agents {
                    vec![cached]
                } else {
                    vec![]
                },
                run: Some(Arc::new(move |ctx: InvocationContext| {
                    let cached = run_cached.clone();
                    yielding(move |y| async move {
                        if !y
                            .call(Ok(content_event(&ctx, "parent", "parent answer")))
                            .await
                        {
                            return;
                        }
                        // Start the cached child on a goroutine, drop its event, and wait for it
                        // to finish (join).
                        let cached_done = Gate::new();
                        go({
                            let cached_done = cached_done.clone();
                            async move {
                                let _done = CloseOnDrop(cached_done);
                                drain(cached.run(ctx)).await;
                            }
                        });
                        if !within(&cached_done).await {
                            y.call(Err(AgentError::other("cached_child did not finish")))
                                .await;
                        }
                    })
                })),
                ..Default::default()
            });
            run_completed_ok(
                &tp,
                parent,
                &format!("{name}/joined_cached_child_without_forwarding"),
            )
            .await;
        }
    }
}

#[tokio::test]
async fn test_concurrency_race_sequential_agent_unjoined_late_child() {
    run_unjoined_late_child(|sub, after| {
        gadk_engine::sequential_agent::new(gadk_engine::sequential_agent::SequentialAgentConfig {
            agent: AgentConfig {
                name: "pipeline".into(),
                sub_agents: vec![sub],
                after_agent_callbacks: vec![after],
                ..Default::default()
            },
        })
        .expect("sequential agent")
    })
    .await;
}

/// Go's `&scriptedLLM{…}` of the tool tests: a call of `tool` (ID `id`, no arguments), then
/// "done" (turn complete).
fn calls_tool_then_done(id: &str, tool: &str) -> Arc<dyn gadk_model::Llm> {
    scripted_llm(vec![
        calls(vec![function_call_part_id(id, tool, json!({}))]),
        final_text("done"),
    ])
}

/// A Go `AfterToolCallback` running `f` and returning `(nil, nil)`.
fn after_tool(
    f: impl Fn() -> BoxFuture<'static, Result<(), AgentError>> + Send + Sync + 'static,
) -> AfterToolCallback {
    Arc::new(move |_ctx, _tool, _args, _result, _err| {
        let run = f();
        Box::pin(async move {
            run.await?;
            Ok(None)
        })
    })
}

#[tokio::test]
async fn test_concurrency_race_function_tool_returns_before_unjoined_cached_child() {
    let tp = setup_test_plugin().await;
    let entered = Gate::new();
    let tool_done = Gate::new();
    let child_done = Gate::new();
    let child = cached_child("cached_child", "cached", entered.clone());
    let launch = function_tool("launch", "Starts cached_child without waiting for it", {
        let (tool_done, child_done) = (tool_done.clone(), child_done.clone());
        move |ctx: Context, _args| {
            let child_ctx = promoted(&ctx, &sub_path(&ctx, "cached_child@1"));
            let (child, entered, tool_done, child_done) = (
                child.clone(),
                entered.clone(),
                tool_done.clone(),
                child_done.clone(),
            );
            go(async move {
                let _done = CloseOnDrop(child_done);
                let mut events = child.run(child_ctx);
                while events.next().await.is_some() {
                    let _ = within(&tool_done).await;
                }
            });
            async move {
                if !within(&entered).await {
                    return Err(ToolError::Execution(
                        "cached_child never entered its BeforeAgentCallback".into(),
                    ));
                }
                Ok(tool_result("started", json!(true)))
            }
        }
    });
    let root = llm_agent(LlmAgentConfig {
        name: "root".into(),
        tools: vec![launch],
        model: Some(calls_tool_then_done("call-1", "launch")),
        after_tool_callbacks: vec![after_tool(move || {
            tool_done.close();
            Box::pin(async { Ok(()) })
        })],
        ..Default::default()
    });
    let r = new_test_runner(root, vec![tp.plugin.clone()]);
    let (_, errs) = run_turn(&r, "u1", "s1", "go", RunConfig::default()).await;
    assert!(errs.is_empty(), "runner.Run error: {errs:?}");
    let rows = RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await;
    assert!(within(&child_done).await, "cached_child run never finished");
    require_invocation_completed_ok(&rows);
    assert_eq!(
        rows_of_type(&rows, EVENT_AGENT_ERROR).len(),
        0,
        "AGENT_ERROR rows; counts: {:?}",
        agent_error_counts(&rows)
    );
}

#[tokio::test]
async fn test_runner_tool_run_custom_parent_with_unjoined_cached_child_emits_no_agent_error() {
    for declared in [true, false] {
        let name = if declared {
            "declared_child"
        } else {
            "undeclared_child"
        };
        let tp = setup_test_plugin().await;
        let entered = Gate::new();
        let child_done = Gate::new();
        let cached = cached_child("cached_child", "cache hit", entered.clone());
        let run_cached = cached.clone();
        let parent = new_agent(AgentConfig {
            name: "parent".into(),
            sub_agents: if declared { vec![cached] } else { vec![] },
            run: Some(Arc::new({
                let child_done = child_done.clone();
                move |ctx: InvocationContext| {
                    let (cached, entered, child_done) =
                        (run_cached.clone(), entered.clone(), child_done.clone());
                    yielding(move |y| async move {
                        go({
                            let ctx = ctx.clone();
                            async move {
                                let _done = CloseOnDrop(child_done);
                                drain(cached.run(ctx)).await;
                            }
                        });
                        if !within(&entered).await {
                            y.call(Err(AgentError::other(
                                "cached_child never entered its BeforeAgentCallback",
                            )))
                            .await;
                            return;
                        }
                        if !y
                            .call(Ok(content_event(&ctx, "parent", "parent answer")))
                            .await
                        {
                            return;
                        }
                        ctx.end_invocation();
                    })
                }
            })),
            ..Default::default()
        });
        let run_parent = function_tool("run_parent", "Runs parent", move |ctx: Context, _args| {
            let parent = parent.clone();
            async move {
                let mut events = parent.run(promoted(&ctx, "parent@1"));
                while let Some(item) = events.next().await {
                    if let Err(e) = item {
                        return Err(ToolError::Execution(Box::new(e)));
                    }
                }
                Ok(tool_result("ok", json!(true)))
            }
        });
        let coordinator = llm_agent(LlmAgentConfig {
            name: "coordinator".into(),
            tools: vec![run_parent],
            model: Some(calls_tool_then_done("call-parent-1", "run_parent")),
            ..Default::default()
        });
        let r = new_test_runner(coordinator, vec![tp.plugin.clone()]);
        let (_, errs) = run_turn(&r, "u1", "s1", "go", RunConfig::default()).await;
        assert!(errs.is_empty(), "{name}: runner.Run error: {errs:?}");
        let rows = RowCursor::default()
            .collect(&tp, until_invocation_end)
            .await;
        assert!(
            within(&child_done).await,
            "{name}: cached_child run never finished"
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

/// Go `blockingLLM.entered` shared by the code that waits for it.
type Entered = Arc<tokio::sync::Mutex<mpsc::Receiver<()>>>;

#[tokio::test]
async fn test_runner_tool_returns_before_helper_model_call_ends_keeps_helper_open() {
    let tp = setup_test_plugin().await;
    let gate = blocking_llm(1);
    let entered: Entered = Arc::new(tokio::sync::Mutex::new(gate.entered));
    let release = gate.release;
    let helper = llm_agent(LlmAgentConfig {
        name: "helper".into(),
        model: Some(gate.model),
        ..Default::default()
    });
    let helper_done = Gate::new();
    let launch = function_tool("launch", "Starts helper without waiting for it", {
        let helper_done = helper_done.clone();
        move |ctx: Context, _args| {
            let helper_ctx = promoted(&ctx, &sub_path(&ctx, "helper@1"));
            let (helper, helper_done, entered) =
                (helper.clone(), helper_done.clone(), entered.clone());
            go(async move {
                let _done = CloseOnDrop(helper_done);
                drain(helper.run(helper_ctx)).await;
            });
            async move {
                let started = tokio::time::timeout(Duration::from_secs(10), async {
                    entered.lock().await.recv().await
                })
                .await;
                if !matches!(started, Ok(Some(()))) {
                    return Err(ToolError::Execution(
                        "helper never started its model call".into(),
                    ));
                }
                Ok(tool_result("started", json!(true)))
            }
        }
    });
    let root = llm_agent(LlmAgentConfig {
        name: "root".into(),
        tools: vec![launch],
        model: Some(calls_tool_then_done("call-1", "launch")),
        after_tool_callbacks: vec![after_tool({
            let release = release.clone();
            move || {
                release.close();
                let helper_done = helper_done.clone();
                Box::pin(async move {
                    if !within(&helper_done).await {
                        return Err(AgentError::other("helper never returned"));
                    }
                    Ok(())
                })
            }
        })],
        ..Default::default()
    });
    let _release = CloseOnDrop(release);
    let r = new_test_runner(root, vec![tp.plugin.clone()]);
    let (_, errs) = run_turn(&r, "u1", "s1", "go", RunConfig::default()).await;
    assert!(errs.is_empty(), "runner.Run error: {errs:?}");
    let rows = RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await;
    require_invocation_completed_ok(&rows);
    assert_eq!(
        rows_of_type(&rows, EVENT_AGENT_ERROR).len(),
        0,
        "AGENT_ERROR rows; event types: {:?}",
        event_types(&rows)
    );
    for (start, end) in [
        (EVENT_LLM_REQUEST, EVENT_LLM_RESPONSE),
        (EVENT_AGENT_STARTING, EVENT_AGENT_COMPLETED),
    ] {
        let starts = agent_rows_of_type(&rows, start, "helper");
        let ends = agent_rows_of_type(&rows, end, "helper");
        assert!(
            starts.len() == 1 && ends.len() == 1,
            "helper rows: {} {start} and {} {end}, want one of each; event types: {:?}",
            starts.len(),
            ends.len(),
            event_types(&rows)
        );
        assert_eq!(
            place_of(&ends[0]),
            place_of(&starts[0]),
            "helper {end} is not at the place of its {start}"
        );
        must_latency(&ends[0]);
    }
}

#[tokio::test]
async fn test_concurrency_race_forwarded_finish_task_event_does_not_end_running_child() {
    let tp = setup_test_plugin().await;
    let forwarded = Gate::new();
    let child = new_agent(AgentConfig {
        name: "child".into(),
        run: Some(Arc::new({
            let forwarded = forwarded.clone();
            move |ctx: InvocationContext| {
                let forwarded = forwarded.clone();
                yielding(move |y| async move {
                    let mut ev = new_event(&ctx);
                    ev.author = "child".into();
                    ev.branch = ctx.branch();
                    ev.response.content = Some(Content {
                        role: "user".into(),
                        parts: vec![Part {
                            function_response: Some(FunctionResponse {
                                id: "call-finish".into(),
                                name: "finish_task".into(),
                                response: tool_result("result", json!("Task completed.")),
                                ..Default::default()
                            }),
                            ..Default::default()
                        }],
                    });
                    if !y.call(Ok(ev)).await {
                        return;
                    }
                    ctx.end_invocation();
                    let _ = within(&forwarded).await;
                })
            }
        })),
        ..Default::default()
    });
    let run_child = child.clone();
    let parent = new_agent(AgentConfig {
        name: "parent".into(),
        sub_agents: vec![child],
        run: Some(Arc::new(move |ctx: InvocationContext| {
            let (child, forwarded) = (run_child.clone(), forwarded.clone());
            yielding(move |y| async move {
                // Go's unbuffered `results` channel.
                let (results, mut received) = mpsc::channel::<Item>(1);
                go(async move {
                    let mut events = child.run(ctx);
                    while let Some(item) = events.next().await {
                        if results.send(item).await.is_err() {
                            return;
                        }
                    }
                });
                let mut reading = true;
                while let Some(item) = received.recv().await {
                    if reading {
                        reading = y.call(item).await;
                    }
                    forwarded.close();
                }
            })
        })),
        ..Default::default()
    });
    let r = new_test_runner(parent, vec![tp.plugin.clone()]);
    let (_, errs) = run_turn(&r, "u1", "s1", "go", RunConfig::default()).await;
    assert!(errs.is_empty(), "runner.Run error: {errs:?}");
    let rows = RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await;
    require_invocation_completed_ok(&rows);
    assert_eq!(
        rows_of_type(&rows, EVENT_AGENT_ERROR).len(),
        0,
        "AGENT_ERROR rows; event types: {:?}",
        event_types(&rows)
    );
    // The sub-agent does not run in task mode, so its finish_task response does not end its span
    // (see TestRunner_NonTaskModeFinishTaskTool), and ADK runs no AfterAgentCallback for an
    // agent that ended its invocation: its span retires with the custom agent's, without a
    // closing row.
    let starts = agent_rows_of_type(&rows, EVENT_AGENT_STARTING, "child");
    let ends = agent_rows_of_type(&rows, EVENT_AGENT_COMPLETED, "child");
    assert!(
        starts.len() == 1 && ends.is_empty(),
        "child rows: {} AGENT_STARTING and {} AGENT_COMPLETED, want 1 and 0; event types: {:?}",
        starts.len(),
        ends.len(),
        event_types(&rows)
    );
}

#[tokio::test]
async fn test_runner_content_event_keeps_in_flight_sub_agent_model_call() {
    let tp = setup_test_plugin().await;
    let gate = blocking_llm(0);
    let entered: Entered = Arc::new(tokio::sync::Mutex::new(gate.entered));
    let release = gate.release;
    let sub = llm_agent(LlmAgentConfig {
        name: "sub".into(),
        model: Some(gate.model),
        ..Default::default()
    });
    let run_sub = sub.clone();
    let root = new_agent(AgentConfig {
        name: "root".into(),
        sub_agents: vec![sub],
        run: Some(Arc::new(move |ctx: InvocationContext| {
            let (sub, entered, release) = (run_sub.clone(), entered.clone(), release.clone());
            yielding(move |y| async move {
                // sub runs on a goroutine of its own, and each of its events is acknowledged
                // once yielded, as a parallel agent does.
                let (results, mut received) = mpsc::channel::<(Item, oneshot::Sender<()>)>(1);
                go({
                    let ctx = ctx.clone();
                    async move {
                        let mut events = sub.run(ctx);
                        while let Some(item) = events.next().await {
                            let (ack, acked) = oneshot::channel();
                            if results.send((item, ack)).await.is_err() {
                                return;
                            }
                            let _ = acked.await;
                        }
                    }
                });
                entered.lock().await.recv().await;
                let mut progress = new_event(&ctx);
                progress.author = "root".into();
                progress.branch = ctx.branch();
                progress.response.content = Some(text_content("model", "working on it"));
                let mut reading = y.call(Ok(progress)).await;
                release.close();
                while let Some((item, ack)) = received.recv().await {
                    if reading {
                        reading = y.call(item).await;
                    }
                    let _ = ack.send(());
                }
            })
        })),
        ..Default::default()
    });
    let r = new_test_runner(root, vec![tp.plugin.clone()]);
    let (_, errs) = run_turn(&r, "u1", "s1", "go", RunConfig::default()).await;
    assert!(errs.is_empty(), "r.Run error: {errs:?}");
    let rows = RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await;
    require_invocation_completed_ok(&rows);
    let request = place_of(&only_row_of_type(&rows, EVENT_LLM_REQUEST));
    let response = only_row_of_type(&rows, EVENT_LLM_RESPONSE);
    assert_eq!(
        place_of(&response),
        request,
        "LLM_RESPONSE place, want LLM_REQUEST place"
    );
    must_latency(&response);
}

#[tokio::test]
async fn test_repro_unjoined_call_started_after_launcher_returned_moves_to_late() {
    let tm = Arc::new(TraceManager::new());
    let p = new_trace_only_plugin(tm.clone());
    let s_started = Gate::new();
    let c_returned = Gate::new();
    let (s_model, s_model_entered, s_model_release) = gated_llm();
    let s = llm_agent(LlmAgentConfig {
        name: "s".into(),
        model: Some(s_model),
        before_agent_callbacks: vec![Arc::new({
            let (s_started, c_returned) = (s_started.clone(), c_returned.clone());
            move |_ctx: Context| {
                let (s_started, c_returned) = (s_started.clone(), c_returned.clone());
                Box::pin(async move {
                    s_started.close();
                    c_returned.wait().await;
                    Ok(None)
                })
            }
        })],
        ..Default::default()
    });
    let s_done = Gate::new();
    let run_s = s.clone();
    let c = new_agent(AgentConfig {
        name: "c".into(),
        sub_agents: vec![s],
        run: Some(Arc::new({
            let s_done = s_done.clone();
            move |ctx: InvocationContext| {
                let (s, s_started, s_done) = (run_s.clone(), s_started.clone(), s_done.clone());
                yielding(move |_y| async move {
                    go(async move {
                        let _done = CloseOnDrop(s_done);
                        drain(s.run(ctx)).await;
                    });
                    s_started.wait().await;
                })
            }
        })),
        ..Default::default()
    });
    // Go `&waitLLM{wait: sModel.entered}` (model name `wait`).
    let b_model = fn_llm("wait", move |_, _| {
        let wait = s_model_entered.clone();
        futures::stream::once(async move {
            wait.wait().await;
            final_text("b")
        })
        .boxed()
    });
    let b = llm_agent(LlmAgentConfig {
        name: "b".into(),
        model: Some(b_model),
        ..Default::default()
    });
    let pipeline = crate::trace::testkit::sequential("pipeline", vec![c, b]);
    let signal = plugin_named(
        "signal",
        gadk_agent::PluginConfig {
            after_agent_callback: Some(Arc::new(move |ctx: Context| {
                if ctx.agent_name() == "c" {
                    c_returned.close();
                }
                Box::pin(async { Ok(None) })
            })),
            ..Default::default()
        },
    );
    let r = new_test_runner(pipeline, vec![p, signal]);
    let (events, errs) = run_turn(&r, "u1", "s1", "hi", RunConfig::default()).await;
    assert!(errs.is_empty(), "run: {errs:?}");
    let inv_id = events
        .first()
        .map(|ev| ev.invocation_id.clone())
        .unwrap_or_default();
    let moved = {
        let st = tm.mu.lock();
        st.completed
            .get(&inv_id)
            .and_then(|c| c.late.clone())
            .map_or(0, |late| late.mu.lock().llm_spans.len())
    };
    s_model_release.close();
    assert!(within(&s_done).await, "s did not return");
    assert_eq!(
        moved, 1,
        "s's model call, still running at AfterRun, moved to late state"
    );
}

/// Go `clockReadBy(name)`: whether the trace-manager method that reads the clock in the caller
/// (the innermost `TraceManager` method on the stack) is `name`.
///
/// Go's `runtime.Callers` always names its frames; a `std::backtrace::Backtrace` names them only
/// when the test binary keeps its symbols (the default `test` profile; not with
/// `strip = "symbols"`), which [`require_symbolized_backtraces`] checks up front so such a build
/// fails with that reason instead of a clock hook that never matches. An optimized test build
/// may also inline `push_agent_span` into its caller; its frame is then named only from debug
/// info (keep `debug` on when testing with `--release`).
fn clock_read_by(name: &str) -> bool {
    let trace = std::backtrace::Backtrace::force_capture().to_string();
    trace
        .lines()
        .find_map(|line| line.split_once("TraceManager>::").map(|(_, m)| m))
        .is_some_and(|method| method.split("::").next() == Some(name))
}

/// Fails the test unless `std::backtrace` names this binary's frames (what [`clock_read_by`]
/// relies on).
#[inline(never)]
fn require_symbolized_backtraces() {
    let trace = std::backtrace::Backtrace::force_capture().to_string();
    assert!(
        trace.contains("require_symbolized_backtraces"),
        "clockReadBy's port reads function names from std::backtrace::Backtrace: build the tests \
         with symbols (no `strip = \"symbols\"`)"
    );
}

// parity: adapted: Go's clock hook blocks the child's goroutine; here the hook blocks the
// worker thread of the child's task (a multi-threaded runtime keeps the run going), and
// `clockReadBy`'s `runtime.Callers` is a `std::backtrace::Backtrace` of the hook's caller.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_runner_agent_starting_across_after_run_starts_in_late_state() {
    require_symbolized_backtraces();
    let mut tm = TraceManager::new();
    let child_goroutine = Arc::new(AtomicU64::new(0));
    let held = Gate::new();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let release_tx = Arc::new(parking_lot::Mutex::new(Some(release_tx)));
    let unblock = {
        let release_tx = release_tx.clone();
        move || drop(release_tx.lock().take())
    };
    let release_rx = parking_lot::Mutex::new(release_rx);
    let hold_once = AtomicBool::new(false);
    tm.now = Arc::new({
        let (child_goroutine, held) = (child_goroutine.clone(), held.clone());
        move || {
            // pushAgentSpan reads the clock once it looked the invocation up.
            let g = child_goroutine.load(Ordering::SeqCst);
            if g != 0
                && g == goroutine_id()
                && clock_read_by("push_agent_span")
                && !hold_once.swap(true, Ordering::SeqCst)
            {
                held.close();
                let _ = release_rx.lock().recv_timeout(Duration::from_secs(10));
            }
            Utc::now()
        }
    });
    let tm = Arc::new(tm);
    let (p, processor) = new_queued_rows_plugin(tm.clone()).await;
    let child = llm_agent(LlmAgentConfig {
        name: "child".into(),
        model: Some(scripted_llm(vec![final_text("late")])),
        ..Default::default()
    });
    let child_done = Gate::new();
    let run_child = child.clone();
    let root = new_agent(AgentConfig {
        name: "root".into(),
        sub_agents: vec![child],
        run: Some(Arc::new({
            let child_done = child_done.clone();
            move |ctx: InvocationContext| {
                let (child, child_goroutine, held, child_done) = (
                    run_child.clone(),
                    child_goroutine.clone(),
                    held.clone(),
                    child_done.clone(),
                );
                yielding(move |y| async move {
                    go({
                        let ctx = ctx.clone();
                        async move {
                            let _done = CloseOnDrop(child_done);
                            child_goroutine.store(goroutine_id(), Ordering::SeqCst);
                            drain(child.run(ctx)).await;
                        }
                    });
                    if !within(&held).await {
                        y.call(Err(AgentError::other(
                            "the child's start never looked the invocation up",
                        )))
                        .await;
                        return;
                    }
                    let mut ev = new_event(&ctx);
                    ev.author = "root".into();
                    ev.response.content = Some(text_content("model", "started"));
                    y.call(Ok(ev)).await;
                })
            }
        })),
        ..Default::default()
    });
    let r = new_test_runner(root, vec![p]);
    let (_, errs) = run_turn(&r, "u1", "s1", "go", RunConfig::default()).await;
    assert!(errs.is_empty(), "runner.Run error: {errs:?}");
    unblock();
    assert!(within(&child_done).await, "the child never returned");
    let rows = take_rows(Some(&processor));
    assert_eq!(
        rows_of_type(&rows, EVENT_INVOCATION_COMPLETED).len(),
        1,
        "INVOCATION_COMPLETED rows; event types: {:?}",
        event_types(&rows)
    );
    let starts = agent_rows_of_type(&rows, EVENT_AGENT_STARTING, "child");
    let ends = agent_rows_of_type(&rows, EVENT_AGENT_COMPLETED, "child");
    assert!(
        starts.len() == 1 && ends.len() == 1,
        "child rows: {} AGENT_STARTING and {} AGENT_COMPLETED, want one of each; event types: {:?}",
        starts.len(),
        ends.len(),
        event_types(&rows)
    );
    assert_eq!(
        place_of(&ends[0]),
        place_of(&starts[0]),
        "child AGENT_COMPLETED is not at the place of its AGENT_STARTING"
    );
    must_latency(&ends[0]);
    for row in agent_rows_of_type(&rows, EVENT_LLM_REQUEST, "child") {
        assert_eq!(
            place_of(&row).parent,
            place_of(&starts[0]).span,
            "child LLM_REQUEST parent, want the child's span"
        );
    }
}

#[tokio::test]
async fn test_runner_unjoined_stopped_stream_leaves_no_late_state() {
    let tm = Arc::new(TraceManager::new());
    let p = new_trace_only_plugin(tm.clone());
    let sub = llm_agent(LlmAgentConfig {
        name: "sub".into(),
        model: Some(partial_then_wait_llm("hello ")),
        ..Default::default()
    });
    let run_sub = sub.clone();
    let root = new_agent(AgentConfig {
        name: "root".into(),
        sub_agents: vec![sub],
        run: Some(Arc::new(move |ctx: InvocationContext| {
            let sub = run_sub.clone();
            yielding(move |y| async move {
                let (partial, mut received) = mpsc::channel::<Event>(1);
                go(async move {
                    let mut events = sub.run(ctx);
                    while let Some(item) = events.next().await {
                        if let Ok(ev) = item
                            && ev.response.partial
                        {
                            let _ = partial.send(ev).await;
                            return;
                        }
                    }
                });
                if let Some(ev) = received.recv().await {
                    y.call(Ok(ev)).await;
                }
            })
        })),
        ..Default::default()
    });
    let r = new_test_runner(root, vec![p]);
    let cfg = RunConfig {
        streaming_mode: StreamingMode::Sse,
        ..Default::default()
    };
    let (events, errs) = run_turn(&r, "u1", "s1", "hi", cfg).await;
    assert!(errs.is_empty(), "runner.Run error: {errs:?}");
    assert!(
        events.iter().any(|ev| ev.response.partial),
        "the run yielded no partial event"
    );
    let inv_id = events.last().map(|ev| ev.invocation_id.clone()).unwrap();
    let st = tm.mu.lock();
    let c = st
        .completed
        .get(&inv_id)
        .unwrap_or_else(|| panic!("run {inv_id:?} is not completed"));
    if let Some(late) = &c.late {
        panic!(
            "completed run kept late state (open calls {}) for a stream that its reader stopped reading",
            late.open_calls.load(Ordering::SeqCst)
        );
    }
}

#[tokio::test]
async fn test_custom_fan_out_handles_canceled_child_with_second_sub_agent() {
    let failer = new_agent(AgentConfig {
        name: "failer".into(),
        run: Some(Arc::new(|ic: InvocationContext| {
            let ev = content_event(&ic, "failer", "partial answer");
            futures::stream::iter([Ok(ev), Err(AgentError::other("errFailer"))]).boxed()
        })),
        ..Default::default()
    });
    let succeeder = new_agent(AgentConfig {
        name: "succeeder".into(),
        run: Some(Arc::new(|ic: InvocationContext| {
            let mut ev = content_event(&ic, "succeeder", "success answer");
            ev.response.turn_complete = true;
            futures::stream::iter([Ok(ev)]).boxed()
        })),
        ..Default::default()
    });
    let (run_failer, run_succeeder) = (failer.clone(), succeeder.clone());
    let root = new_agent(AgentConfig {
        name: "fanout".into(),
        sub_agents: vec![failer, succeeder],
        run: Some(Arc::new(move |ic: InvocationContext| {
            let (failer, succeeder) = (run_failer.clone(), run_succeeder.clone());
            yielding(move |y| async move {
                // Go `gctx, cancel := context.WithCancelCause(ic); defer cancel(nil)`.
                let (gctx, _cancel) = ic.clone().into_context().with_agent_cancel();
                let mut events = failer.run(gctx.as_invocation().clone());
                while let Some(item) = events.next().await {
                    match item {
                        Err(e) => {
                            // as errgroup.WithContext does on the first error
                            gctx.cancel_with_cause(CancelCause::Error(Arc::new(e)));
                            break;
                        }
                        Ok(ev) => {
                            if !y.call(Ok(ev)).await {
                                return;
                            }
                        }
                    }
                }
                drop(events);
                let mut events = succeeder.run(ic.clone());
                while let Some(item) = events.next().await {
                    let ev = item.unwrap_or_else(|e| panic!("succeeder.Run yielded error: {e}"));
                    if !y.call(Ok(ev)).await {
                        return;
                    }
                }
                y.call(Ok(content_event(&ic, "fanout", "fallback answer")))
                    .await;
            })
        })),
        ..Default::default()
    });
    let tp = setup_test_plugin().await;
    let r = new_test_runner(root, vec![tp.plugin.clone()]);
    let (_, errs) = run_turn(&r, "u1", "s1", "hello", RunConfig::default()).await;
    assert!(errs.is_empty(), "runner.Run yielded an error: {errs:?}");
    let rows = RowCursor::default()
        .collect(&tp, until_invocation_end)
        .await;
    assert_eq!(
        rows_of_type(&rows, EVENT_INVOCATION_ERROR).len(),
        0,
        "INVOCATION_ERROR rows"
    );
}
