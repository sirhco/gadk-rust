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
//! `toolsearch_catalog.rs`, shared with the example's test. Go runs it through the launcher
//! (sub-project 6); this is a stdin loop.
//!
//! `GOOGLE_API_KEY=... cargo run -p gadk --example toolsearch`, then ask
//! "What time is it?", "Multiply 6 by 7." or "Convert 21 degrees Celsius to Fahrenheit.".

#[path = "toolsearch_catalog.rs"]
mod toolsearch_catalog;

use std::io::{BufRead, Write};

use futures::StreamExt;
use gadk::agent::{RunConfig, StreamingMode};
use gadk::engine::runner::{RunOptions, Runner};
use gadk::gemini::{ClientConfig, GeminiModel};
use gadk::genai::{Content, ROLE_USER};

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
    let runner = Runner::in_memory("toolsearch_example", agent)?;
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
