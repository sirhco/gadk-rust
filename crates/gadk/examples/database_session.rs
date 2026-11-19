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

//! An `LlmAgent` whose sessions persist in sqlite through `gadk-session-database` (sessions are
//! stored in the tables of ADK's database session service). It reads one line per turn from
//! stdin in the session `session_1`; run it twice and the second run continues the first run's
//! conversation.
//!
//! `GOOGLE_API_KEY=... cargo run -p gadk --example database_session --features database`
//!
//! `GADK_DATABASE_URL` overrides the database (default `sqlite://gadk_sessions.db`).

use std::io::{BufRead, Write};
use std::sync::Arc;

use futures::StreamExt;
use gadk::agent::RunConfig;
use gadk::engine::llm::LlmAgentConfig;
use gadk::engine::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use gadk::gemini::{ClientConfig, GeminiModel};
use gadk::genai::{Content, ROLE_USER};
use gadk::platform::Platform;
use gadk::session_database::DatabaseSessionService;

const APP_NAME: &str = "database_session_example";
const USER_ID: &str = "example_user";
const SESSION_ID: &str = "session_1";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let url = std::env::var("GADK_DATABASE_URL")
        .unwrap_or_else(|_| "sqlite://gadk_sessions.db".to_string());
    let sessions = DatabaseSessionService::connect(&url).await?;
    // The service never creates tables itself; migrate on every startup.
    sessions.auto_migrate().await?;

    let model = GeminiModel::new(
        "gemini-3.8-flash",
        ClientConfig {
            api_key: std::env::var("GOOGLE_API_KEY").ok(),
            ..Default::default()
        },
    )
    .await?;
    let agent = gadk::engine::llm::agent::new(LlmAgentConfig {
        name: "remembering_assistant".to_string(),
        model: Some(model),
        description: "A helpful assistant whose conversation survives restarts.".to_string(),
        instruction: "You are a helpful assistant. Use the conversation so far when you answer."
            .to_string(),
        ..Default::default()
    })?;
    let runner = Runner::new(RunnerConfig {
        app_name: APP_NAME.to_string(),
        agent,
        session_service: Arc::new(sessions),
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig::default(),
        auto_create_session: true,
        platform: Platform::default(),
        compaction: None,
    })?;

    println!("Sessions are stored in {url}; type a message (end input to quit).");
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
            SESSION_ID,
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
    Ok(())
}
