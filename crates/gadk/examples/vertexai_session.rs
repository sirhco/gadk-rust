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

//! An `LlmAgent` whose sessions live in a Vertex AI Agent Engine and whose memories live in
//! the engine's Memory Bank, through `gadk-vertexai`. It reads one line per turn from stdin in a
//! new engine session; at the end of input it generates memories from the conversation, so a
//! later run can recall it with `load_memory`.
//!
//! `GOOGLE_CLOUD_PROJECT=my-project GOOGLE_CLOUD_LOCATION=us-central1 VERTEX_ENGINE_ID=1234567890
//! GOOGLE_API_KEY=... cargo run -p gadk --example vertexai_session --features vertexai`
//!
//! The engine is reached with Application Default Credentials
//! (`gcloud auth application-default login`); the Gemini model reads its own configuration from
//! the environment (`GOOGLE_API_KEY`, or `GOOGLE_GENAI_USE_VERTEXAI=true` for Vertex AI).

use std::io::{BufRead, Write};
use std::sync::Arc;

use futures::StreamExt;
use gadk::agent::RunConfig;
use gadk::engine::llm::LlmAgentConfig;
use gadk::engine::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use gadk::gemini::{ClientConfig, GeminiModel};
use gadk::genai::{Content, ROLE_USER};
use gadk::memory::MemoryService;
use gadk::platform::Platform;
use gadk::session::{CreateRequest, GetRequest, SessionService};
use gadk::vertexai::AgentEngineData;
use gadk::vertexai::memory::{VertexAiMemoryService, VertexAiMemoryServiceConfig};
use gadk::vertexai::session::{VertexAiSessionService, VertexAiSessionServiceConfig};

const APP_NAME: &str = "vertexai_session_example";
const USER_ID: &str = "example_user";

/// A required environment variable; an error when it is unset or empty.
fn env(name: &str) -> anyhow::Result<String> {
    std::env::var(name)
        .ok()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Env var {name} is not set"))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let engine = AgentEngineData {
        project_id: env("GOOGLE_CLOUD_PROJECT")?,
        location: env("GOOGLE_CLOUD_LOCATION")?,
        reasoning_engine: env("VERTEX_ENGINE_ID")?,
    };
    let sessions = Arc::new(VertexAiSessionService::new(VertexAiSessionServiceConfig {
        project_id: engine.project_id.clone(),
        location: engine.location.clone(),
        reasoning_engine: Some(engine.reasoning_engine.clone()),
        ..Default::default()
    })?);
    let memory = Arc::new(VertexAiMemoryService::new(VertexAiMemoryServiceConfig {
        engine,
        wait_for_completion: true,
        ..Default::default()
    })?);

    let model = GeminiModel::new("gemini-2.5-flash", ClientConfig::default()).await?;
    let agent = gadk::engine::llm::agent::new(LlmAgentConfig {
        name: "remembering_assistant".to_string(),
        model: Some(model),
        description: "A helpful assistant that remembers earlier conversations.".to_string(),
        instruction: "You are a helpful assistant. When the user refers to an earlier \
            conversation, use the load_memory tool to look it up."
            .to_string(),
        tools: vec![gadk::tools::load_memory::new()],
        ..Default::default()
    })?;

    // The engine assigns the session ID.
    let session = sessions
        .create(CreateRequest {
            app_name: APP_NAME.to_string(),
            user_id: USER_ID.to_string(),
            ..Default::default()
        })
        .await?;
    let session_id = session.id().to_string();

    let runner = Runner::new(RunnerConfig {
        app_name: APP_NAME.to_string(),
        agent,
        session_service: sessions.clone(),
        artifact_service: None,
        memory_service: Some(memory.clone()),
        plugin_config: PluginConfig::default(),
        auto_create_session: false,
        platform: Platform::default(),
        compaction: None,
    })?;

    println!(
        "Session {session_id} is stored in the Agent Engine; type a message (end input to quit)."
    );
    let stdin = std::io::stdin();
    loop {
        print!("User -> ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            println!();
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut events = runner.run(
            USER_ID,
            &session_id,
            Some(Content::from_text(line, ROLE_USER)),
            RunConfig::default(),
            RunOptions::default(),
        );
        while let Some(event) = events.next().await {
            let Some(content) = event?.response.content else {
                continue;
            };
            for part in content.parts {
                if !part.text.is_empty() && !part.thought {
                    println!("Agent -> {}", part.text.trim());
                }
            }
        }
    }
    runner.close().await?;

    // Generate memories from the whole conversation as the engine stored it.
    let stored = sessions
        .get(GetRequest {
            app_name: APP_NAME.to_string(),
            user_id: USER_ID.to_string(),
            session_id,
            ..Default::default()
        })
        .await?;
    memory.add_session_to_memory(stored.as_ref()).await?;
    println!(
        "Generated memories from {} events of the conversation.",
        stored.events().len()
    );
    Ok(())
}
