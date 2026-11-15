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

//! Rust-only: Go-faithful teardown on early stop. A Go consumer that stops reading unwinds
//! synchronously, so the work already started finishes before the runner's deferred after-run
//! callbacks: ParallelAgent acks the event the reader stopped on, cancels its branches and waits
//! for their teardown (agent/workflowagents/parallelagent/agent.go:118-142), `platform.RunTasks`
//! waits for every tool task (platform/exec.go:68), and `RunLive`'s session goroutine finishes
//! its in-flight tool call after `Close` (internal/llminternal/base_flow.go:424).

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use gadk_agent::{
    AfterToolCallback, Agent, AgentConfig, AgentError, Context, EventStream, InvocationContext,
    LiveRunConfig, Plugin, RunConfig, ToolError,
};
use gadk_engine::llm::{LlmAgentConfig, agent as llm_agent};
use gadk_engine::parallel_agent::{self, ParallelAgentConfig};
use gadk_engine::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use gadk_engine::testutil::MockModel;
use gadk_engine::testutil::live::{go_live_test_llm, model_turn, tool_call};
use gadk_engine::tool::{FunctionTool, FunctionToolConfig};
use gadk_genai::{Content, FinishReason, Part, ROLE_MODEL, ROLE_USER};
use gadk_model::{Llm, LlmResponse};
use gadk_platform::Platform;
use gadk_session::{Event, InMemorySessionService};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Map;
use tokio::sync::Notify;

type Log = Arc<Mutex<Vec<String>>>;

fn record(log: &Log, what: impl Into<String>) {
    log.lock().push(what.into());
}

/// A plugin whose after-run callback records `after_run` (and whether the invocation token was
/// cancelled then) and signals `done`.
fn after_run_probe(log: &Log, done: tokio::sync::mpsc::UnboundedSender<bool>) -> Plugin {
    let log = log.clone();
    Plugin::new(gadk_agent::PluginConfig {
        name: "probe".into(),
        after_run_callback: Some(Arc::new(move |ctx: InvocationContext| {
            record(&log, "after_run");
            let _ = done.send(ctx.cancellation_token().is_cancelled());
            Box::pin(async {})
        })),
        ..Default::default()
    })
}

fn runner(agent: Arc<dyn Agent>, plugin: Plugin) -> Runner {
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
        platform: Platform::default(),
        compaction: None,
    })
    .unwrap()
}

fn text_event(ctx: &InvocationContext, text: &str) -> Event {
    let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
    ev.author = ctx.agent().name().to_string();
    ev.branch = ctx.branch();
    ev.response.content = Some(Content::from_text(text, ROLE_MODEL));
    ev
}

/// Yields one event; once the consumer acknowledges it, records `yielder acked`, tears down for
/// a while and records `yielder done`.
fn yielder(log: &Log) -> Arc<dyn Agent> {
    let log = log.clone();
    gadk_agent::new_agent(AgentConfig {
        name: "yielder".into(),
        run: Some(Arc::new(move |ctx: InvocationContext| -> EventStream {
            let log = log.clone();
            Box::pin(async_stream::stream! {
                yield Ok(text_event(&ctx, "one"));
                record(&log, "yielder acked");
                tokio::time::sleep(Duration::from_millis(20)).await;
                record(&log, "yielder done");
            })
        })),
        ..Default::default()
    })
    .unwrap()
}

/// Waits for the group's cancellation, tears down for a while and records `waiter done`.
fn waiter(log: &Log) -> Arc<dyn Agent> {
    let log = log.clone();
    gadk_agent::new_agent(AgentConfig {
        name: "waiter".into(),
        run: Some(Arc::new(move |ctx: InvocationContext| -> EventStream {
            let log = log.clone();
            Box::pin(async_stream::stream! {
                ctx.cancellation_token().cancelled().await;
                tokio::time::sleep(Duration::from_millis(20)).await;
                record(&log, "waiter done");
                if false {
                    yield Err(AgentError::Cancelled);
                }
            })
        })),
        ..Default::default()
    })
    .unwrap()
}

/// Go #1186: a consumer that stops on a ParallelAgent event acknowledges it, cancels the
/// branches and waits for every branch's teardown before the after-run callbacks, which still
/// see a live invocation token.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_early_stop_acks_and_awaits_branches_before_after_run() {
    let log: Log = Arc::default();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let par = parallel_agent::new(ParallelAgentConfig {
        agent: AgentConfig {
            name: "par".into(),
            sub_agents: vec![yielder(&log), waiter(&log)],
            ..Default::default()
        },
    })
    .unwrap();
    let r = runner(par, after_run_probe(&log, tx));
    let mut stream = r.run(
        "u",
        "s",
        Some(Content::from_text("hi", ROLE_USER)),
        RunConfig::default(),
        RunOptions::default(),
    );
    let ev = stream.next().await.expect("one event").unwrap();
    assert_eq!(ev.author, "yielder");
    drop(stream);
    let cancelled = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("after-run callbacks ran")
        .unwrap();
    assert!(
        !cancelled,
        "a stop at a yield leaves the token live in after-run"
    );
    let log = log.lock().clone();
    assert_eq!(log.last().map(String::as_str), Some("after_run"), "{log:?}");
    for want in ["yielder acked", "yielder done", "waiter done"] {
        assert!(log.iter().any(|l| l == want), "missing {want}: {log:?}");
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
struct NoArgs {}

#[derive(Serialize, schemars::JsonSchema)]
struct ToolOut {
    ok: bool,
}

/// A tool that signals `started`, then (ignoring cancellation) waits for `release`.
fn gated_tool(started: Arc<Notify>, release: Arc<Notify>) -> Arc<dyn gadk_agent::Tool> {
    FunctionTool::new(
        FunctionToolConfig::new("gated", "Waits for its release"),
        move |_ctx: Context, _a: NoArgs| {
            let (started, release) = (started.clone(), release.clone());
            async move {
                started.notify_one();
                release.notified().await;
                Ok::<_, ToolError>(ToolOut { ok: true })
            }
        },
    )
    .unwrap()
}

fn after_tool_probe(log: &Log) -> AfterToolCallback {
    let log = log.clone();
    Arc::new(move |_ctx, _tool, _args, _result, _err| {
        record(&log, "after_tool");
        Box::pin(async { Ok(None) })
    })
}

/// Go `platform.RunTasks` waits for every tool task: a consumer that drops the run while a tool
/// call is in flight gets the after-run callbacks only once that call (after-tool callbacks
/// included) has finished.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tool_calls_finish_before_after_run_on_early_drop() {
    let log: Log = Arc::default();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (started, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    let model: Arc<dyn Llm> = MockModel::new(vec![Ok(LlmResponse {
        content: Some(Content::from_parts(
            vec![Part::from_function_call("gated", Map::new())],
            ROLE_MODEL,
        )),
        finish_reason: Some(FinishReason::Stop),
        ..Default::default()
    })]);
    let root = llm_agent::new(LlmAgentConfig {
        name: "root".into(),
        model: Some(model),
        tools: vec![gated_tool(started.clone(), release.clone())],
        after_tool_callbacks: vec![after_tool_probe(&log)],
        ..Default::default()
    })
    .unwrap();
    let r = runner(root, after_run_probe(&log, tx));
    let mut stream = r.run(
        "u",
        "s",
        Some(Content::from_text("go", ROLE_USER)),
        RunConfig::default(),
        RunOptions::default(),
    );
    let drive = async { while stream.next().await.is_some() {} };
    tokio::select! {
        () = started.notified() => {}
        () = drive => panic!("stream ended before the tool started"),
    }
    drop(stream);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        log.lock().is_empty(),
        "after-run waits for the running tool call: {:?}",
        log.lock()
    );
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("after-run callbacks ran")
        .unwrap();
    assert_eq!(*log.lock(), ["after_tool", "after_run"]);
}

/// Go `RunLive`: `Close` only closes the session; its goroutine finishes the in-flight tool call
/// (after-tool callbacks included) and returns. The live session task is not aborted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_close_lets_the_in_flight_tool_finish() {
    let log: Log = Arc::default();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (started, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    let model = go_live_test_llm(vec![
        tool_call(&[("call-1", "gated")]),
        model_turn("done", true),
    ]);
    let root = llm_agent::new(LlmAgentConfig {
        name: "live_root".into(),
        model: Some(model as Arc<dyn Llm>),
        tools: vec![gated_tool(started.clone(), release.clone())],
        after_tool_callbacks: vec![after_tool_probe(&log)],
        ..Default::default()
    })
    .unwrap();
    let r = runner(root, after_run_probe(&log, tx));
    let (sess, mut events) = r
        .run_live("u", "s", LiveRunConfig::default(), RunOptions::default())
        .await
        .unwrap();
    let drive = async { while events.next().await.is_some() {} };
    tokio::select! {
        () = started.notified() => {}
        () = drive => panic!("stream ended before the tool started"),
    }
    sess.close().await.unwrap();
    while let Some(item) = tokio::time::timeout(Duration::from_secs(5), events.next())
        .await
        .expect("the live stream did not end")
    {
        let _ = item;
    }
    drop(events);
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("after-run callbacks ran")
        .unwrap();
    // Go does not wait for the session goroutine before after-run: the tool is still running.
    // Give a would-be abort of the session task time to land before releasing the tool.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!log.lock().iter().any(|l| l == "after_tool"));
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !log.lock().iter().any(|l| l == "after_tool") {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the in-flight tool's after-tool callbacks ran after close");
}
