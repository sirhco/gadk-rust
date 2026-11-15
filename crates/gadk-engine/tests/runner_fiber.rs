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

//! Rust-only: the runner's consumer fiber (Go: the goroutine that ranges over `Run` / `RunLive`'s
//! iterator runs the before-run, root-agent and deferred after-run callbacks) and the live
//! session task's fiber (Go `internal/llminternal/base_flow.go` `RunLive`'s goroutine).

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use gadk_agent::fiber::{self, LIVE_SESSION_CREATOR};
use gadk_agent::{
    AgentConfig, AgentError, Context, EventStream, InvocationContext, LiveRunConfig, Plugin,
    RunConfig, ToolError,
};
use gadk_engine::llm::{LlmAgentConfig, agent as llm_agent};
use gadk_engine::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use gadk_engine::testutil::MockModel;
use gadk_engine::testutil::live::{go_live_test_llm, model_turn, tool_call};
use gadk_engine::tool::{FunctionTool, FunctionToolConfig};
use gadk_genai::{Content, FinishReason, ROLE_MODEL, ROLE_USER};
use gadk_model::{Llm, LlmResponse};
use gadk_platform::Platform;
use gadk_session::{Event, InMemorySessionService};
use parking_lot::Mutex;
use rstest::rstest;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

/// `(callback, fiber id)` in call order.
type Log = Arc<Mutex<Vec<(&'static str, u64)>>>;

fn record(log: &Log, what: &'static str) {
    log.lock().push((what, fiber::current().id));
}

/// A plugin recording the fiber of every runner-level callback; `after_run` also reports the
/// invocation token and whether it was cancelled when the callback ran.
fn probe(
    log: &Log,
    after_run: tokio::sync::mpsc::UnboundedSender<(bool, CancellationToken)>,
) -> Plugin {
    let (l1, l2, l3) = (log.clone(), log.clone(), log.clone());
    Plugin::new(gadk_agent::PluginConfig {
        name: "probe".into(),
        before_run_callback: Some(Arc::new(move |_ctx: InvocationContext| {
            record(&l1, "before_run");
            Box::pin(async { Ok::<Option<Content>, AgentError>(None) })
        })),
        on_event_callback: Some(Arc::new(move |_ctx: InvocationContext, _ev: Event| {
            record(&l2, "on_event");
            Box::pin(async { Ok::<Option<Event>, AgentError>(None) })
        })),
        after_run_callback: Some(Arc::new(move |ctx: InvocationContext| {
            record(&l3, "after_run");
            let token = ctx.cancellation_token().clone();
            let _ = after_run.send((token.is_cancelled(), token));
            Box::pin(async {})
        })),
        ..Default::default()
    })
}

fn runner_on(agent: Arc<dyn gadk_agent::Agent>, plugin: Plugin, platform: Platform) -> Runner {
    Runner::new(RunnerConfig {
        app_name: "app".into(),
        agent,
        session_service: Arc::new(InMemorySessionService::new()),
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig {
            plugins: vec![plugin],
            close_timeout: Duration::from_secs(1),
        },
        auto_create_session: true,
        platform,
        compaction: None,
    })
    .unwrap()
}

fn runner(agent: Arc<dyn gadk_agent::Agent>, plugin: Plugin) -> Runner {
    runner_on(agent, plugin, Platform::default())
}

/// A custom (non-workflow) root with recording before/after-agent callbacks whose run yields
/// one event, then ends (`pend == false`) or never ends (`pend == true`).
fn custom_root(log: &Log, pend: bool) -> Arc<dyn gadk_agent::Agent> {
    let (l1, l2, l3) = (log.clone(), log.clone(), log.clone());
    gadk_agent::new_agent(AgentConfig {
        name: "custom_root".into(),
        before_agent_callbacks: vec![Arc::new(move |_ctx: Context| {
            record(&l1, "before_agent");
            Box::pin(async { Ok::<Option<Content>, AgentError>(None) })
        })],
        after_agent_callbacks: vec![Arc::new(move |_ctx: Context| {
            record(&l2, "after_agent");
            Box::pin(async { Ok::<Option<Content>, AgentError>(None) })
        })],
        run: Some(Arc::new(move |ctx: InvocationContext| -> EventStream {
            let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
            ev.response.content = Some(Content::from_text("one", ROLE_MODEL));
            let log = l3.clone();
            Box::pin(async_stream::stream! {
                record(&log, "run");
                yield Ok(ev);
                if pend {
                    std::future::pending::<()>().await;
                }
            })
        })),
        ..Default::default()
    })
    .unwrap()
}

fn llm_root() -> Arc<dyn gadk_agent::Agent> {
    let model: Arc<dyn Llm> = MockModel::new(vec![Ok(LlmResponse {
        content: Some(Content::from_text("one", ROLE_MODEL)),
        finish_reason: Some(FinishReason::Stop),
        ..Default::default()
    })]);
    llm_agent::new(LlmAgentConfig {
        name: "llm_root".into(),
        model: Some(model),
        ..Default::default()
    })
    .unwrap()
}

/// Asserts every recorded callback ran on one fiber, which is non-zero (the `#[tokio::test]`
/// body has none: `fiber::current().id == 0`), and returns it.
fn one_fiber(log: &Log, want: &[&str]) -> u64 {
    let log = log.lock().clone();
    let names: Vec<&str> = log.iter().map(|(n, _)| *n).collect();
    assert_eq!(names, want, "callbacks in order");
    let id = log[0].1;
    assert_ne!(id, 0, "consumer-side callbacks run on a fiber: {log:?}");
    assert!(log.iter().all(|(_, f)| *f == id), "one fiber: {log:?}");
    id
}

/// R4: the consumer-side callbacks of a run (before-run, the root agent's own callbacks for a
/// non-workflow custom root, on-event, after-run) see one non-zero fiber.
#[rstest]
#[case::custom_root("custom", &["before_run", "before_agent", "run", "on_event", "after_agent", "after_run"])]
#[case::llm_root("llm", &["before_run", "on_event", "after_run"])]
#[tokio::test]
async fn consumer_side_callbacks_share_one_fiber(#[case] kind: &str, #[case] want: &[&str]) {
    assert_eq!(fiber::current().id, 0, "a #[tokio::test] body has no fiber");
    let log: Log = Arc::default();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let root = if kind == "custom" {
        custom_root(&log, false)
    } else {
        llm_root()
    };
    let runner = runner(root, probe(&log, tx));
    let mut stream = runner.run(
        "u",
        "s",
        Some(Content::from_text("hi", ROLE_USER)),
        RunConfig::default(),
        RunOptions::default(),
    );
    while let Some(item) = stream.next().await {
        item.unwrap();
    }
    rx.recv().await.expect("after-run ran inline");
    one_fiber(&log, want);
}

/// R4: a consumer that stops at a yield (Go `break`) runs the deferred after-run callbacks on its
/// own goroutine: the spawned after-run re-enters the consumer fiber.
#[rstest]
#[case::agent_path("custom", &["before_run", "before_agent", "run", "on_event", "after_run"])]
#[case::node_path("llm", &["before_run", "on_event", "after_run"])]
#[tokio::test]
async fn early_stop_after_run_sees_the_consumer_fiber(#[case] kind: &str, #[case] want: &[&str]) {
    let log: Log = Arc::default();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let root = if kind == "custom" {
        custom_root(&log, true)
    } else {
        llm_root()
    };
    let runner = runner(root, probe(&log, tx));
    let mut stream = runner.run(
        "u",
        "s",
        Some(Content::from_text("hi", ROLE_USER)),
        RunConfig::default(),
        RunOptions::default(),
    );
    stream.next().await.expect("one event").unwrap();
    drop(stream);
    let (cancelled, _) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("after-run callbacks ran")
        .expect("after-run callbacks ran");
    assert!(
        !cancelled,
        "a stop at a yield leaves the token live in after-run"
    );
    one_fiber(&log, want);
}

/// R5 + R4: a live consumer that drops the stream at a yield runs the after-run callbacks
/// before the invocation token is cancelled (Go `RunLive`'s `defer RunAfterRunCallback` inside
/// the iterator, after the consumer's `break`), on the consumer fiber; `on_event` runs on it too.
/// Go's `RunLive` itself (session lookup, before-run, the flow's goroutine start) runs on the
/// caller, outside the iterator, so before-run is not on the consumer fiber.
#[tokio::test]
async fn live_drop_at_a_yield_runs_after_run_before_cancel() {
    let log: Log = Arc::default();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let model = go_live_test_llm(vec![model_turn("one", false)]);
    let root = llm_agent::new(LlmAgentConfig {
        name: "live_root".into(),
        model: Some(model as Arc<dyn Llm>),
        ..Default::default()
    })
    .unwrap();
    let runner = runner(root, probe(&log, tx));
    let (_sess, mut events) = runner
        .run_live("u", "s", LiveRunConfig::default(), RunOptions::default())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), events.next())
        .await
        .expect("the live run yields an event")
        .expect("the live run yields an event")
        .unwrap();
    drop(events);
    let (cancelled, token) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("after-run callbacks ran")
        .expect("after-run callbacks ran");
    assert!(
        !cancelled,
        "after-run runs before the invocation token is cancelled"
    );
    tokio::time::timeout(Duration::from_secs(5), token.cancelled())
        .await
        .expect("the invocation token is cancelled once the after-run callbacks return");

    let log = log.lock().clone();
    let (names, ids): (Vec<&str>, Vec<u64>) = log.iter().copied().unzip();
    assert_eq!(names[0], "before_run");
    assert_eq!(
        ids[0], 0,
        "before-run runs in run_live's own future (the caller)"
    );
    assert_eq!(&names[1..], &["on_event", "after_run"]);
    assert_ne!(ids[1], 0, "the stream is polled on a consumer fiber");
    assert_eq!(ids[1], ids[2], "after-run re-enters the consumer fiber");
}

#[derive(Deserialize, schemars::JsonSchema)]
struct NoArgs {}

#[derive(Serialize, schemars::JsonSchema)]
struct ToolOut {
    ok: bool,
}

/// Runs every task inline, in order, on the calling task (so on the caller's fiber).
struct InlineTasks;

#[async_trait::async_trait]
impl gadk_platform::TaskRunner for InlineTasks {
    async fn run(&self, tasks: Vec<futures::future::BoxFuture<'static, ()>>) {
        for t in tasks {
            t.await;
        }
    }
}

/// R3: the live flow's session task (Go `base_flow.go` `RunLive`'s `go func()`) runs on a fresh
/// fiber started by `LIVE_SESSION_CREATOR`. Its parent is whatever ran `Runner::run_live` (not
/// asserted, as the spec says). The tool calls it handles run on the platform's task runner (Go
/// `platform.RunTasks`, one goroutine per call under the default runner); an inline runner keeps
/// them on the session task, so the tool observes the session task's fiber.
#[tokio::test]
async fn live_session_task_runs_on_a_live_session_fiber() {
    let model = go_live_test_llm(vec![
        tool_call(&[("call-1", "probe_fiber")]),
        model_turn("done", true),
    ]);
    let seen: Arc<Mutex<Option<fiber::Fiber>>> = Arc::default();
    let seen_in_tool = seen.clone();
    let probe_fiber = FunctionTool::new(
        FunctionToolConfig::new("probe_fiber", "Records its fiber"),
        move |_ctx: Context, _a: NoArgs| {
            *seen_in_tool.lock() = Some(fiber::current());
            async move { Ok::<_, ToolError>(ToolOut { ok: true }) }
        },
    )
    .unwrap();
    let root = llm_agent::new(LlmAgentConfig {
        name: "live_root".into(),
        model: Some(model as Arc<dyn Llm>),
        tools: vec![probe_fiber],
        ..Default::default()
    })
    .unwrap();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let runner = runner_on(
        root,
        probe(&Log::default(), tx),
        Platform::default().with_task_runner(InlineTasks),
    );
    let (sess, mut events) = runner
        .run_live("u", "s", LiveRunConfig::default(), RunOptions::default())
        .await
        .unwrap();
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(5), events.next())
            .await
            .expect("timed out waiting for the model's final turn")
            .expect("stream ended before the model's final turn")
            .unwrap();
        if ev.response.turn_complete {
            break;
        }
    }
    sess.close().await.unwrap();
    while let Some(item) = tokio::time::timeout(Duration::from_secs(5), events.next())
        .await
        .expect("the live stream did not end")
    {
        item.unwrap();
    }

    let tool_fiber = seen.lock().clone().expect("the tool ran");
    assert_eq!(tool_fiber.origin.creator, LIVE_SESSION_CREATOR);
    assert!(
        tool_fiber.id != 0 && tool_fiber.id < (1 << 63),
        "a fresh fiber id: {tool_fiber:?}"
    );
}
