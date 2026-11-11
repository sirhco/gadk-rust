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

//! Tools hidden behind `search_tools`: the model loads a tool's declaration only when it needs
//! that tool (port of Go examples/toolsearch/main.go). The catalog and agent are in
//! `toolsearch_catalog.rs`, shared with the example's test. It runs through the launcher, as
//! Go's does.
//!
//! `GOOGLE_API_KEY=... cargo run -p gadk --features launcher --example toolsearch [console | web
//! webui api]`, then ask
//! "What time is it?", "Multiply 6 by 7." or "Convert 21 degrees Celsius to Fahrenheit.".

#[path = "toolsearch_catalog.rs"]
mod toolsearch_catalog;

use std::sync::Arc;

use gadk::agent::SingleLoader;
use gadk::gemini::{ClientConfig, GeminiModel};
use gadk::launcher::golog::Logger;
use gadk::launcher::{CancellationToken, Config, Launcher, full};

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
    let agent = toolsearch_catalog::new_agent(model)?;
    let config = Config::new(Arc::new(SingleLoader::new(agent)));
    let mut l = full::new();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = l.execute(CancellationToken::new(), config, args).await {
        // Go `log.Fatalf("Run failed: %v\n\n%s", err, l.CommandLineSyntax())`.
        Logger::stderr().print(&format!("Run failed: {e}\n\n{}", l.command_line_syntax()));
        std::process::exit(1);
    }
    Ok(())
}
