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

//! An agent run through ADK-Go's launcher (port of Go `examples/quickstart/main.go`, which hands
//! its agent to `full.NewLauncher()`): the command line picks the console or the web server and
//! its sub-launchers, exactly as with Go.
//!
//! `GOOGLE_API_KEY=... cargo run -p gadk --features launcher --example launcher` (the console),
//! `… --example launcher -- web api webui` (the REST API and the web UI on
//! <http://localhost:8080/ui/>), or `… --example launcher -- -h` for every command line.

use std::sync::Arc;

use gadk::agent::{SingleLoader, Tool};
use gadk::engine::llm::LlmAgentConfig;
use gadk::gemini::{ClientConfig, GeminiModel};
use gadk::launcher::golog::Logger;
use gadk::launcher::{CancellationToken, Config, Launcher, full};
use gadk::tools::gemini::GoogleSearch;

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

    let google_search: Arc<dyn Tool> = Arc::new(GoogleSearch);
    let agent = gadk::engine::llm::agent::new(LlmAgentConfig {
        name: "weather_time_agent".to_string(),
        model: Some(model),
        description: "Agent to answer questions about the time and weather in a city.".to_string(),
        instruction: "Your SOLE purpose is to answer questions about the current time and weather in a specific city. You MUST refuse to answer any questions unrelated to time or weather.".to_string(),
        tools: vec![google_search],
        ..Default::default()
    })?;

    let config = Config::new(Arc::new(SingleLoader::new(agent)));

    // Go passes `context.Background()`: a token nothing cancels. The console stops at end of
    // input or Ctrl-C; the web server runs until the process is stopped.
    let mut l = full::new();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = l.execute(CancellationToken::new(), config, args).await {
        // Go `log.Fatalf("Run failed: %v\n\n%s", err, l.CommandLineSyntax())`.
        Logger::stderr().print(&format!("Run failed: {e}\n\n{}", l.command_line_syntax()));
        std::process::exit(1);
    }
    Ok(())
}
