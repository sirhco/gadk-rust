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
//! Port of Go agent/llmagent/model_callback_artifact_test.go.

use std::any::Any;
use std::sync::Arc;

use futures::StreamExt;
use futures::stream::BoxStream;
use gadk_agent::{AfterModelCallback, AgentError, BeforeModelCallback, Context, RunConfig};
use gadk_engine::llm::LlmAgentConfig;
use gadk_engine::llm::agent as llm_agent;
use gadk_engine::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use gadk_genai::{Content, Part, ROLE_MODEL, ROLE_USER};
use gadk_model::{Llm, LlmRequest, LlmResponse, ModelError};
use gadk_platform::Platform;
use gadk_session::{CreateRequest, Event, InMemorySessionService, SessionService};

/// Port of Go FakeLLM (state_agent_test.go) with no GenerateContentFunc: always answers
/// "fake model response".
struct FakeLlm;
impl Llm for FakeLlm {
    fn name(&self) -> &str {
        "fake-llm"
    }
    fn generate_content(
        &self,
        _req: LlmRequest,
        _stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        let out = LlmResponse {
            content: Some(Content::from_text("fake model response", ROLE_MODEL)),
            ..Default::default()
        };
        futures::stream::once(async move { Ok(out) }).boxed()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Go saveArtifact.
async fn save_artifact(ctx: &Context, name: &str) -> Result<(), AgentError> {
    let artifacts = ctx.artifacts().expect("artifacts");
    artifacts.save(name, Part::from_text(name)).await?;
    Ok(())
}

/// Checks that artifacts saved from before and after model callbacks show up in the model
/// response event's ArtifactDelta, the same way state set in those callbacks shows up in
/// StateDelta.
#[tokio::test]
async fn test_model_callback_artifact_save_recorded_on_event() {
    let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let before: BeforeModelCallback = Arc::new(|ctx: Context, req: LlmRequest| {
        Box::pin(async move {
            let res = save_artifact(&ctx, "before.txt").await.map(|()| None);
            (req, res)
        })
    });
    let after: AfterModelCallback = Arc::new(|ctx: Context, _resp, _err| {
        Box::pin(async move { save_artifact(&ctx, "after.txt").await.map(|()| None) })
    });
    let a = llm_agent::new(LlmAgentConfig {
        name: "root_agent".into(),
        model: Some(Arc::new(FakeLlm)),
        before_model_callbacks: vec![before],
        after_model_callbacks: vec![after],
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("llmagent.New() error = {e}"));
    let r = Runner::new(RunnerConfig {
        app_name: "test_app".into(),
        agent: a,
        session_service: svc.clone(),
        artifact_service: Some(Arc::new(gadk_artifact::InMemoryArtifactService::new())),
        memory_service: None,
        plugin_config: PluginConfig::default(),
        auto_create_session: false,
        platform: Platform::default(),
        compaction: None,
    })
    .unwrap_or_else(|e| panic!("runner.New() error = {e}"));
    let created = svc
        .create(CreateRequest {
            app_name: "test_app".into(),
            user_id: "u".into(),
            ..Default::default()
        })
        .await
        .unwrap_or_else(|e| panic!("Create() error = {e}"));

    let mut model_event: Option<Event> = None;
    let mut stream = r.run(
        "u",
        created.id(),
        Some(Content::from_text("hi", ROLE_USER)),
        RunConfig::default(),
        RunOptions::default(),
    );
    while let Some(ev) = stream.next().await {
        let ev = ev.unwrap_or_else(|e| panic!("Run() error = {e}"));
        if ev.author == "root_agent" && ev.response.content.is_some() {
            model_event = Some(ev);
        }
    }
    let model_event = model_event.expect("no model response event");
    let delta = model_event
        .actions
        .artifact_delta
        .clone()
        .unwrap_or_default();
    for name in ["before.txt", "after.txt"] {
        assert!(
            delta.contains_key(name),
            "model response event ArtifactDelta = {delta:?}, missing {name:?}"
        );
    }
}
