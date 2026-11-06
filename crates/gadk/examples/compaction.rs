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

// parity: adapted: Go example runs through the launcher (sub-project 6)

//! An LLM agent with sliding-window context compaction, chatting over stdin through
//! `Runner::run` (port of Go examples/compaction/main.go).
//!
//! Gemini API: `GOOGLE_API_KEY=... cargo run -p gadk --example compaction`
//! Vertex AI:  `GOOGLE_GENAI_USE_VERTEXAI=true GOOGLE_CLOUD_PROJECT=... GOOGLE_CLOUD_LOCATION=us-central1 cargo run -p gadk --example compaction`

use std::io::{BufRead, Write};
use std::sync::Arc;

use futures::StreamExt;
use gadk::agent::RunConfig;
use gadk::compaction::Config;
use gadk::engine::llm::LlmAgentConfig;
use gadk::engine::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use gadk::gemini::{ClientConfig, GeminiModel};
use gadk::genai::{Content, ROLE_USER};
use gadk::session::InMemorySessionService;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let model = GeminiModel::new("gemini-flash-latest", ClientConfig::default()).await?;
    let agent = gadk::engine::llm::agent::new(LlmAgentConfig {
        name: "assistant".to_string(),
        model: Some(model),
        description: "A general purpose assistant with a long memory.".to_string(),
        instruction: "You are a helpful assistant. Keep your answers to a sentence or two."
            .to_string(),
        ..Default::default()
    })?;

    let runner = Runner::new(RunnerConfig {
        app_name: "compaction_example".to_string(),
        agent,
        session_service: Arc::new(InMemorySessionService::new()),
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig::default(),
        auto_create_session: true,
        platform: Default::default(),
        // No summarizer, so the runner summarizes with the root agent's own model.
        compaction: Some(Config {
            // Sliding window: after every 2 completed turns, summarize the turns since the last
            // compaction, carrying 1 earlier turn forward so consecutive summaries overlap.
            compaction_interval: 2,
            overlap_size: 1,
            // Tail retention bounds prompt size instead; it fires mid-turn once a prompt reaches
            // `token_threshold`. At an interval of 2 it never fires, so to try it replace the two
            // settings above with, for example:
            //   token_threshold: 32_000,
            //   event_retention_size: 10,
            ..Default::default()
        }),
    })?;

    let stdin = std::io::stdin();
    loop {
        print!("you> ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut events = runner.run(
            "user",
            "session-1",
            Some(Content::from_text(line, ROLE_USER)),
            RunConfig::default(),
            RunOptions::default(),
        );
        while let Some(event) = events.next().await {
            let event = event?;
            for part in event.response.content.iter().flat_map(|c| c.parts.iter()) {
                if !part.thought && !part.text.is_empty() {
                    println!("[{}] {}", event.author, part.text);
                }
            }
        }
    }
    runner.close().await?;
    Ok(())
}
