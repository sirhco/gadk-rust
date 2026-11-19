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

//! An agent that loads and describes artifacts through the `load_artifacts` tool. The image
//! is compiled in, so the example runs from any directory.
//!
//! `GOOGLE_API_KEY=... cargo run -p gadk --example load_artifacts`, then ask
//! "What artifacts do you have? Describe them.".

use std::io::{BufRead, Write};
use std::sync::Arc;

use futures::StreamExt;
use gadk::agent::{RunConfig, StreamingMode};
use gadk::artifact::{ArtifactService, InMemoryArtifactService, SaveRequest};
use gadk::engine::llm::LlmAgentConfig;
use gadk::engine::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use gadk::gemini::{ClientConfig, GeminiModel};
use gadk::genai::{Content, Part, ROLE_USER};
use gadk::platform::Platform;
use gadk::session::{CreateRequest, InMemorySessionService, SessionService};

const USER_ID: &str = "test_user";
const APP_NAME: &str = "test_app";

/// The sample image the agent loads, compiled in.
const ANIMAL_PICTURE: &[u8] = include_bytes!("assets/animal_picture.png");

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let model = GeminiModel::new(
        "gemini-2.5-flash",
        ClientConfig {
            api_key: std::env::var("GOOGLE_API_KEY").ok(),
            ..Default::default()
        },
    )
    .await?;

    let agent = gadk::engine::llm::agent::new(LlmAgentConfig {
        name: "artifact_describer".to_string(),
        model: Some(model),
        description: "Agent to answer questions about artifacts.".to_string(),
        instruction: "When user asks about the artifact, load them and describe them.".to_string(),
        tools: vec![gadk::tools::load_artifacts::new()],
        ..Default::default()
    })?;

    let session_service = Arc::new(InMemorySessionService::new());
    let session = session_service
        .create(CreateRequest {
            app_name: APP_NAME.to_string(),
            user_id: USER_ID.to_string(),
            ..Default::default()
        })
        .await?;

    // Populate artifacts that can be described later.
    let artifact_service = Arc::new(InMemoryArtifactService::new());
    artifact_service
        .save(SaveRequest {
            app_name: APP_NAME.to_string(),
            user_id: USER_ID.to_string(),
            session_id: session.id().to_string(),
            file_name: "animal_picture.png".to_string(),
            part: Some(Part::from_bytes(ANIMAL_PICTURE.into(), "image/png")),
            ..Default::default()
        })
        .await?;
    artifact_service
        .save(SaveRequest {
            app_name: APP_NAME.to_string(),
            user_id: USER_ID.to_string(),
            session_id: session.id().to_string(),
            file_name: "haiku.txt".to_string(),
            part: Some(Part::from_text(
                "An old silent pond...A frog jumps into the pond,splash! Silence again.",
            )),
            ..Default::default()
        })
        .await?;

    let runner = Runner::new(RunnerConfig {
        app_name: APP_NAME.to_string(),
        agent,
        session_service,
        artifact_service: Some(artifact_service),
        memory_service: None,
        plugin_config: PluginConfig::default(),
        auto_create_session: false,
        platform: Platform::default(),
        compaction: None,
    })?;

    repl(&runner, USER_ID, session.id()).await?;
    runner.close().await?;
    Ok(())
}

/// Reads stdin, one line per turn, printing the streamed (partial) model text.
async fn repl(runner: &Runner, user_id: &str, session_id: &str) -> anyhow::Result<()> {
    let stdin = std::io::stdin();
    loop {
        print!("\nUser -> ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            return Ok(());
        }
        print!("\nAgent -> ");
        std::io::stdout().flush()?;
        let cfg = RunConfig {
            streaming_mode: StreamingMode::Sse,
            ..Default::default()
        };
        let msg = Content::from_text(line, ROLE_USER);
        let mut events = runner.run(user_id, session_id, Some(msg), cfg, RunOptions::default());
        while let Some(event) = events.next().await {
            let event = match event {
                Ok(event) => event,
                Err(e) => {
                    println!("\nAGENT_ERROR: {e}");
                    continue;
                }
            };
            // In SSE mode only the partial responses are printed: the final response repeats
            // their text.
            if !event.response.partial {
                continue;
            }
            for part in event.response.content.iter().flat_map(|c| c.parts.iter()) {
                print!("{}", part.text);
            }
            std::io::stdout().flush()?;
        }
    }
}
