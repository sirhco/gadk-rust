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
//! Rust-only runner tests (spec § Testing): cancellation on drop, on_event-before-append, the
//! root-mode check, and TestAgentRunner.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::StreamExt;
use gadk_agent::{
    AgentConfig, Context, EventStream, InvocationContext, Mode, Plugin, RunConfig, StreamingMode,
    ToolError,
};
use gadk_engine::llm::{LlmAgentConfig, agent as llm_agent};
use gadk_engine::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use gadk_engine::testutil::{MockModel, TestAgentRunner};
use gadk_engine::tool::{FunctionTool, FunctionToolConfig};
use gadk_genai::{Content, FinishReason, Part, ROLE_MODEL, ROLE_USER};
use gadk_model::{Llm, LlmResponse};
use gadk_platform::Platform;
use gadk_session::{Event, GetRequest, InMemorySessionService, SessionService};
use parking_lot::Mutex;
use rstest::rstest;
use serde::Deserialize;
use serde_json::{Map, Value, json};

#[derive(Deserialize, schemars::JsonSchema)]
struct NoArgs {}

struct SetOnDrop(Arc<AtomicBool>);

impl Drop for SetOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

fn text(t: &str) -> Result<LlmResponse, gadk_model::ModelError> {
    Ok(LlmResponse {
        content: Some(Content::from_text(t, ROLE_MODEL)),
        finish_reason: Some(FinishReason::Stop),
        ..Default::default()
    })
}

fn first_text(ev: &Event) -> String {
    ev.response
        .content
        .as_ref()
        .and_then(|c| c.parts.first())
        .map(|p| p.text.clone())
        .unwrap_or_default()
}

fn run_fn<F>(f: F) -> gadk_agent::AgentRunFn
where
    F: Fn(InvocationContext) -> Vec<Event> + Send + Sync + 'static,
{
    Arc::new(move |ctx: InvocationContext| -> EventStream {
        Box::pin(futures::stream::iter(f(ctx).into_iter().map(Ok)))
    })
}

fn get(user: &str, session: &str, app: &str) -> GetRequest {
    GetRequest {
        app_name: app.into(),
        user_id: user.into(),
        session_id: session.into(),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tool_cancel_on_drop() {
    let started = Arc::new(tokio::sync::Notify::new());
    let dropped = Arc::new(AtomicBool::new(false));
    let (s2, d2) = (started.clone(), dropped.clone());
    let tool = FunctionTool::new(
        FunctionToolConfig::new("block", "blocks until cancelled"),
        move |ctx: Context, _a: NoArgs| {
            let (s, d) = (s2.clone(), d2.clone());
            async move {
                let _guard = SetOnDrop(d);
                s.notify_one();
                // Dropping the stream cancels the invocation; the tool task is not aborted
                // (Go `platform.RunTasks` waits for it), so it ends by observing the cancellation.
                ctx.cancellation_token().cancelled().await;
                Ok::<String, ToolError>(String::new())
            }
        },
    )
    .unwrap();
    let call = Ok(LlmResponse {
        content: Some(Content::from_parts(
            vec![Part::from_function_call("block", Map::new())],
            ROLE_MODEL,
        )),
        finish_reason: Some(FinishReason::Stop),
        ..Default::default()
    });
    let model: Arc<dyn Llm> = MockModel::new(vec![call]);
    let agent = llm_agent::new(LlmAgentConfig {
        name: "root".into(),
        model: Some(model),
        tools: vec![tool],
        ..Default::default()
    })
    .unwrap();
    let runner = Runner::in_memory("app", agent).unwrap();
    let mut stream = runner.run(
        "u",
        "s",
        Some(Content::from_text("go", ROLE_USER)),
        RunConfig::default(),
        RunOptions::default(),
    );
    let drive = async { while stream.next().await.is_some() {} };
    tokio::select! {
        _ = started.notified() => {}
        _ = drive => panic!("stream ended before the tool started"),
    }
    drop(stream);
    tokio::time::timeout(Duration::from_secs(5), async {
        while !dropped.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("dropping the Runner::run stream did not cancel the running tool");
}

#[tokio::test]
async fn runner_on_event_before_append() {
    let custom = gadk_agent::new_agent(AgentConfig {
        name: "a".into(),
        run: Some(run_fn(|ctx: InvocationContext| {
            let ev = |t: &str, partial: bool| {
                let mut e = Event::new(ctx.platform(), ctx.invocation_id());
                e.response.content = Some(Content::from_text(t, ROLE_MODEL));
                e.response.partial = partial;
                e
            };
            vec![ev("one", false), ev("two", false), ev("three", true)]
        })),
        ..Default::default()
    })
    .unwrap();
    let model: Arc<dyn Llm> = MockModel::new(vec![text("one")]);
    let llm = llm_agent::new(LlmAgentConfig {
        name: "a".into(),
        model: Some(model),
        ..Default::default()
    })
    .unwrap();

    for (kind, root) in [("custom", custom), ("llm", llm)] {
        let seen: Arc<Mutex<Vec<(usize, String)>>> = Arc::default();
        let probe = {
            let seen = seen.clone();
            Plugin::new(gadk_agent::PluginConfig {
                name: "probe".into(),
                on_event_callback: Some(Arc::new(move |ctx: InvocationContext, ev: Event| {
                    seen.lock()
                        .push((ctx.session().events().len(), first_text(&ev)));
                    let mut ev = ev;
                    ev.response
                        .custom_metadata
                        .insert("seen_by_plugin".into(), Value::Bool(true));
                    Box::pin(async move { Ok(Some(ev)) })
                })),
                ..Default::default()
            })
        };
        let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
        let runner = Runner::new(RunnerConfig {
            app_name: "app".into(),
            agent: root,
            session_service: svc.clone(),
            artifact_service: None,
            memory_service: None,
            plugin_config: PluginConfig {
                plugins: vec![probe],
                close_timeout: Duration::from_secs(1),
            },
            auto_create_session: true,
            platform: Platform::default(),
            compaction: None,
        })
        .unwrap();
        let items: Vec<_> = runner
            .run(
                "u",
                "s",
                Some(Content::from_text("hi", ROLE_USER)),
                RunConfig::default(),
                RunOptions::default(),
            )
            .collect()
            .await;
        let yielded: Vec<Event> = items.into_iter().map(|i| i.unwrap()).collect();
        assert!(
            yielded
                .iter()
                .all(|e| e.response.custom_metadata.get("seen_by_plugin") == Some(&json!(true))),
            "{kind}: every yielded event carries the plugin's result"
        );
        let stored = svc.get(get("u", "s", "app")).await.unwrap().events();
        let seen = seen.lock().clone();
        if kind == "custom" {
            assert_eq!(
                seen,
                vec![(1, "one".into()), (2, "two".into()), (3, "three".into())],
                "{kind}: on_event runs before the append"
            );
            assert_eq!(stored.len(), 3, "{kind}: the partial event is not stored");
            for ev in &stored[1..] {
                assert_eq!(
                    ev.response.custom_metadata.get("seen_by_plugin"),
                    Some(&json!(true)),
                    "{kind}: the plugin's event is what is persisted"
                );
            }
        } else {
            assert_eq!(
                seen.first(),
                Some(&(1, "one".to_string())),
                "{kind}: {seen:?}"
            );
            assert_eq!(
                stored
                    .last()
                    .unwrap()
                    .response
                    .custom_metadata
                    .get("seen_by_plugin"),
                Some(&json!(true)),
                "{kind}: the plugin's event is what is persisted"
            );
        }
    }
}

#[tokio::test]
async fn test_agent_runner_with_state_and_config() {
    let reader = || {
        gadk_agent::new_agent(AgentConfig {
            name: "reader".into(),
            run: Some(run_fn(|ctx: InvocationContext| {
                let k = ctx
                    .readonly_state()
                    .get("k")
                    .map(|v| v.to_string())
                    .unwrap_or_default();
                let mut e = Event::new(ctx.platform(), ctx.invocation_id());
                e.author = "reader".into();
                e.response.content = Some(Content::from_text(
                    format!("{k} {:?}", ctx.run_config().streaming_mode),
                    ROLE_MODEL,
                ));
                vec![e]
            })),
            ..Default::default()
        })
        .unwrap()
    };
    let Value::Object(state) = json!({"k": "v"}) else {
        unreachable!()
    };
    let r = TestAgentRunner::with_state(reader(), state).await;
    let events: Vec<_> = r
        .run_with_config(
            Content::from_text("go", ROLE_USER),
            RunConfig {
                streaming_mode: StreamingMode::Sse,
                ..Default::default()
            },
        )
        .collect()
        .await;
    assert_eq!(events.len(), 1);
    assert_eq!(first_text(events[0].as_ref().unwrap()), "\"v\" Sse");

    // Go's nil message: no user event is appended.
    let r = TestAgentRunner::new(reader()).await;
    let events: Vec<_> = r.run("").collect().await;
    assert!(events.iter().all(|e| e.is_ok()));
    let stored = r
        .session_service()
        .get(get("test_user", r.session_id(), "test_app"))
        .await
        .unwrap()
        .events();
    assert_eq!(stored.len(), 1, "only the agent's event is stored");
    assert_eq!(stored[0].author, "reader");
}

#[rstest]
#[case(
    Mode::Task,
    Some("root agent root must be a chat LlmAgent, but has mode task")
)]
#[case(
    Mode::SingleTurn,
    Some("root agent root must be a chat LlmAgent, but has mode single_turn")
)]
#[case(Mode::Chat, None)]
#[case(Mode::Unset, None)]
#[tokio::test]
async fn runner_rejects_non_chat_root(#[case] mode: Mode, #[case] want: Option<&str>) {
    let model: Arc<dyn Llm> = MockModel::new(vec![text("hi")]);
    let root = llm_agent::new(LlmAgentConfig {
        name: "root".into(),
        model: Some(model),
        mode,
        ..Default::default()
    })
    .unwrap();
    let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let runner = Runner::new(RunnerConfig {
        app_name: "app".into(),
        agent: root,
        session_service: svc.clone(),
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig::default(),
        auto_create_session: true,
        platform: Platform::default(),
        compaction: None,
    })
    .unwrap();
    let items: Vec<_> = runner
        .run(
            "u",
            "s",
            Some(Content::from_text("hi", ROLE_USER)),
            RunConfig::default(),
            RunOptions::default(),
        )
        .collect()
        .await;
    let stored = svc.get(get("u", "s", "app")).await.unwrap();
    match want {
        Some(msg) => {
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].as_ref().unwrap_err().to_string(), msg);
            assert!(
                stored.events().is_empty(),
                "the root check runs before the user message is appended"
            );
        }
        None => assert!(items.iter().all(|i| i.is_ok())),
    }
}

/// How a consumer stops reading a run before it ends.
#[derive(Clone, Copy, Debug)]
enum Stop {
    /// Drops the stream right after an event (Go: `break` at a yield).
    AtEvent,
    /// Drops the stream right after an error (Go: `break` at a yield).
    AtError,
    /// Drops the stream while the agent is suspended in an await (Go: the caller cancels).
    MidAwait,
}

/// Go runner.go `defer pluginManager.RunAfterRunCallback(ctx)` runs on the caller's context: a
/// consumer that stops at a yield leaves it live, so the after-run callbacks see an uncancelled
/// invocation token (cancelled only once they return); a drop inside an await is Go's caller
/// cancelling, which the callbacks see.
#[rstest]
#[case::agent_path_event("custom", Stop::AtEvent, false)]
#[case::agent_path_error("custom", Stop::AtError, false)]
#[case::agent_path_mid_await("custom", Stop::MidAwait, true)]
#[case::node_path_event("llm", Stop::AtEvent, false)]
#[case::node_path_error("llm", Stop::AtError, false)]
#[tokio::test]
async fn after_run_on_consumer_drop_sees_go_context(
    #[case] kind: &str,
    #[case] stop: Stop,
    #[case] want_cancelled: bool,
) {
    let root = if kind == "custom" {
        gadk_agent::new_agent(AgentConfig {
            name: "a".into(),
            run: Some(Arc::new(move |ctx: InvocationContext| -> EventStream {
                let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
                ev.response.content = Some(Content::from_text("one", ROLE_MODEL));
                Box::pin(async_stream::stream! {
                    match stop {
                        Stop::AtError => yield Err(gadk_agent::AgentError::other("boom")),
                        _ => yield Ok(ev),
                    }
                    std::future::pending::<()>().await;
                })
            })),
            ..Default::default()
        })
        .unwrap()
    } else {
        let response = match stop {
            Stop::AtError => Err(gadk_model::ModelError::Other("boom".into())),
            _ => text("one"),
        };
        let model: Arc<dyn Llm> = MockModel::new(vec![response]);
        llm_agent::new(LlmAgentConfig {
            name: "a".into(),
            model: Some(model),
            ..Default::default()
        })
        .unwrap()
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let probe = Plugin::new(gadk_agent::PluginConfig {
        name: "probe".into(),
        after_run_callback: Some(Arc::new(move |ctx: InvocationContext| {
            let token = ctx.cancellation_token().clone();
            let _ = tx.send((token.is_cancelled(), token));
            Box::pin(async {})
        })),
        ..Default::default()
    });
    let runner = Runner::new(RunnerConfig {
        app_name: "app".into(),
        agent: root,
        session_service: Arc::new(InMemorySessionService::new()),
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig {
            plugins: vec![probe],
            close_timeout: Duration::from_secs(1),
        },
        auto_create_session: true,
        platform: Platform::default(),
        compaction: None,
    })
    .unwrap();
    let mut stream = runner.run(
        "u",
        "s",
        Some(Content::from_text("hi", ROLE_USER)),
        RunConfig::default(),
        RunOptions::default(),
    );
    let first = stream.next().await.expect("the run yields an item");
    assert_eq!(first.is_err(), matches!(stop, Stop::AtError), "{first:?}");
    if matches!(stop, Stop::MidAwait) {
        // Resume the stream into the agent's pending await, then abandon it there.
        let pending = tokio::time::timeout(Duration::from_millis(50), stream.next()).await;
        assert!(pending.is_err(), "the agent never ends: {pending:?}");
    }
    drop(stream);
    let (cancelled, token) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("after-run callbacks ran")
        .expect("after-run callbacks ran");
    assert_eq!(cancelled, want_cancelled, "token cancelled in after-run");
    tokio::time::timeout(Duration::from_secs(5), token.cancelled())
        .await
        .expect("the invocation token is cancelled once the after-run callbacks return");
}
