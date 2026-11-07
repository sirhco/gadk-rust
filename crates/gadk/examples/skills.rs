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

//! An agent that discovers and follows skills through the `SkillToolset` (port of Go
//! examples/skills/main.go). The skills (`grocery-prices` and `weather`) are Go's, copied under
//! `examples/skills/`; they are read through `DirFs` from the crate directory, so the example
//! runs from any directory. Go runs it through the launcher (sub-project 6); this is a stdin loop.
//!
//! `GOOGLE_API_KEY=... cargo run -p gadk --example skills`, then ask
//! "What countries do you know current prices for? List the countries. Then find the prices for
//! each. Then re-organize these prices into a table: products on one side - countries on
//! another."

use std::io::{BufRead, Write};
use std::sync::Arc;

use futures::StreamExt;
use gadk::agent::{RunConfig, StreamingMode};
use gadk::engine::llm::LlmAgentConfig;
use gadk::engine::runner::{RunOptions, Runner};
use gadk::gemini::{ClientConfig, GeminiModel};
use gadk::genai::{Content, ROLE_USER};
use gadk::tools::skills::skill::{self, DirFs, FileSystemSource};
use gadk::tools::skills::{SkillToolset, SkillToolsetConfig};

/// Go's `./skills` directory, resolved at compile time.
const SKILLS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/skills");

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let model = GeminiModel::new(
        "gemini-flash-latest",
        ClientConfig {
            api_key: std::env::var("GOOGLE_API_KEY").ok(),
            ..Default::default()
        },
    )
    .await?;

    let source: Arc<dyn skill::Source> =
        Arc::new(FileSystemSource::new(Arc::new(DirFs::new(SKILLS_DIR))));
    let (source, _reload) = skill::with_complete_preload(source).await?;

    let skill_toolset = SkillToolset::new(SkillToolsetConfig {
        source,
        name: None,
        system_instruction: None,
    });

    let agent = gadk::engine::llm::agent::new(LlmAgentConfig {
        name: "skills_agent".to_string(),
        model: Some(model),
        description: "Agent to demonstrate using skills.".to_string(),
        instruction: "You are a helpful assistant.".to_string(),
        toolsets: vec![skill_toolset],
        ..Default::default()
    })?;

    let runner = Runner::in_memory("skills_example", agent)?;
    repl(&runner, "user", "session-1").await?;
    runner.close().await?;
    Ok(())
}

/// Go's stdin loop: one line per turn, printing the streamed (partial) model text.
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
