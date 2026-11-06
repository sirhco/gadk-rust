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

//! Port of Go tool/agenttool/agent_tool_artifacts_test.go.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::StreamExt;
use gadk_agent::{
    Agent, AgentConfig, AgentError, ContextDelta, EventStream, InvocationContext, RunConfig,
};
use gadk_artifact::{ArtifactService, InMemoryArtifactService, LoadRequest, SaveRequest};
use gadk_genai::{Content, Part};
use gadk_platform::Platform;
use gadk_session::{Event, EventActions, InMemorySessionService};
use parking_lot::Mutex;
use serde_json::json;

use crate::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use crate::tool::AgentTool;

/// Go artifactParent: a custom agent that runs `child` through an AgentTool from a tool context
/// whose actions become its own event's actions.
fn artifact_parent(name: &str, child: Arc<dyn Agent>) -> Arc<dyn Agent> {
    let wrapped = AgentTool::new(child, None);
    gadk_agent::new_agent(AgentConfig {
        name: name.into(),
        run: Some(Arc::new(move |ctx: InvocationContext| -> EventStream {
            let wrapped = wrapped.clone();
            Box::pin(async_stream::stream! {
                let mut event = Event::new(ctx.platform(), ctx.invocation_id());
                let actions = Arc::new(Mutex::new(event.actions.clone()));
                // Go agent.NewToolContext(ctx, "child_call", &event.Actions, nil).
                let tool_ctx = ctx.clone().into_context().with_delta(ContextDelta {
                    function_call_id: Some("child_call".into()),
                    actions: Some(actions.clone()),
                    ..Default::default()
                });
                let runnable = wrapped.as_runnable().expect("runnable");
                if let Err(e) = runnable.run(tool_ctx, json!({"request": "run"})).await {
                    yield Err(AgentError::Tool { tool: "child".into(), source: e });
                    return;
                }
                event.actions = actions.lock().clone();
                event.response.content = Some(Content::from_text("done", "model"));
                yield Ok(event);
            })
        })),
        ..Default::default()
    })
    .expect("parent construction failed")
}

#[derive(Default)]
struct Flags {
    loaded: AtomicBool,
    listed: AtomicBool,
    saved: AtomicBool,
}

/// The Go child agent: loads and lists the parent's artifacts, then saves through a tracking
/// context.
fn artifact_child(flags: Arc<Flags>) -> Arc<dyn Agent> {
    gadk_agent::new_agent(AgentConfig {
        name: "child".into(),
        run: Some(Arc::new(move |ctx: InvocationContext| -> EventStream {
            let flags = flags.clone();
            Box::pin(async_stream::stream! {
                let arts = ctx.artifacts().expect("child has artifacts");
                let input = arts.load("input.txt").await;
                flags.loaded.store(
                    input.is_ok_and(|r| r.part.is_some_and(|p| p.text == "input")),
                    Ordering::SeqCst,
                );
                let files = arts.list().await;
                flags.listed.store(files.is_ok_and(|f| f.file_names.len() == 1), Ordering::SeqCst);
                let mut event = Event::new(ctx.platform(), ctx.invocation_id());
                let actions = Arc::new(Mutex::new(EventActions::default()));
                // Go agent.NewCallbackContextWithArtifactTracking(ctx, &event.Actions).
                let cb = ctx.clone().into_context().with_delta(ContextDelta {
                    actions: Some(actions.clone()),
                    ..Default::default()
                });
                let saved = cb
                    .artifacts()
                    .expect("tracked artifacts")
                    .save("output.txt", Part::from_text("output"))
                    .await;
                flags.saved.store(saved.is_ok(), Ordering::SeqCst);
                if let Err(e) = saved {
                    yield Err(e);
                    return;
                }
                event.actions = actions.lock().clone();
                event.response.content = Some(Content::from_text("done", "model"));
                yield Ok(event);
            })
        })),
        ..Default::default()
    })
    .expect("child construction failed")
}

#[tokio::test]
async fn test_agent_tool_run_artifacts() {
    // Go subtests "direct" and "nested".
    for nested in [false, true] {
        test_agent_tool_artifacts(nested).await;
    }
}

async fn test_agent_tool_artifacts(nested: bool) {
    let store = Arc::new(InMemoryArtifactService::new());
    store
        .save(SaveRequest {
            app_name: "app".into(),
            user_id: "user".into(),
            session_id: "session".into(),
            file_name: "input.txt".into(),
            part: Some(Part::from_text("input")),
            ..Default::default()
        })
        .await
        .expect("parent fixture save failed");

    let flags = Arc::new(Flags::default());
    let mut child = artifact_child(flags.clone());
    if nested {
        child = artifact_parent("middle", child);
    }
    let r = Runner::new(RunnerConfig {
        app_name: "app".into(),
        agent: artifact_parent("parent", child),
        session_service: Arc::new(InMemorySessionService::new()),
        artifact_service: Some(store.clone()),
        memory_service: None,
        plugin_config: PluginConfig::default(),
        auto_create_session: true,
        platform: Platform::default(),
        compaction: None,
    })
    .expect("runner construction failed");

    let mut parent_delta = false;
    let mut s = r.run(
        "user",
        "session",
        Some(Content::from_text("run", "user")),
        RunConfig::default(),
        RunOptions::default(),
    );
    while let Some(item) = s.next().await {
        let event = item.unwrap_or_else(|e| panic!("parent run failed (nested={nested}): {e}"));
        if event
            .actions
            .artifact_delta
            .as_ref()
            .and_then(|d| d.get("output.txt"))
            .is_some_and(|v| *v > 0)
        {
            parent_delta = true;
        }
    }
    assert!(
        flags.saved.load(Ordering::SeqCst),
        "child save was not exercised (nested={nested})"
    );
    assert!(
        flags.loaded.load(Ordering::SeqCst),
        "child cannot load parent's input artifact (nested={nested})"
    );
    assert!(
        flags.listed.load(Ordering::SeqCst),
        "child cannot list parent's input artifact (nested={nested})"
    );
    let output = store
        .load(LoadRequest {
            app_name: "app".into(),
            user_id: "user".into(),
            session_id: "session".into(),
            file_name: "output.txt".into(),
            ..Default::default()
        })
        .await;
    assert!(
        output.is_ok_and(|o| o.part.is_some_and(|p| p.text == "output")),
        "parent cannot load child's saved artifact (nested={nested})"
    );
    assert!(
        parent_delta,
        "parent event lacks child's artifact delta (nested={nested})"
    );
}
