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

//! An agent that discovers and follows skills through the `SkillToolset`. The skills
//! (`grocery-prices` and `weather`) live under `examples/skills/`; they are read through
//! `DirFs` from the crate directory, so the example runs from any directory. It runs through
//! the launcher.
//!
//! `GOOGLE_API_KEY=... cargo run -p gadk --features launcher --example skills [console | web
//! webui api]`, then ask
//! "What countries do you know current prices for? List the countries. Then find the prices for
//! each. Then re-organize these prices into a table: products on one side - countries on
//! another."

use std::sync::Arc;

use gadk::agent::SingleLoader;
use gadk::engine::llm::LlmAgentConfig;
use gadk::gemini::{ClientConfig, GeminiModel};
use gadk::launcher::golog::Logger;
use gadk::launcher::{CancellationToken, Config, Launcher, full};
use gadk::tools::skills::skill::{self, DirFs, FileSystemSource};
use gadk::tools::skills::{SkillToolset, SkillToolsetConfig};

/// The `examples/skills` directory, resolved at compile time.
const SKILLS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/skills");

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

    let config = Config::new(Arc::new(SingleLoader::new(agent)));
    let mut l = full::new();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = l.execute(CancellationToken::new(), config, args).await {
        // Print the error and the command-line syntax, then exit.
        Logger::stderr().print(&format!("Run failed: {e}\n\n{}", l.command_line_syntax()));
        std::process::exit(1);
    }
    Ok(())
}
