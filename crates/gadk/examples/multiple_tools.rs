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

//! Using Google Search together with a function tool (port of Go
//! examples/tools/multipletools/main.go). Gemini does not accept a built-in tool and function
//! tools in one request, so each kind lives in its own agent, and a root agent calls both
//! through agent tools. Go runs it through the launcher (sub-project 6); this is a stdin loop.
//!
//! `GOOGLE_API_KEY=... cargo run -p gadk --example multiple_tools`, then ask
//! "What is the weather in Paris?" or "Write a 3-line poem.".

use std::io::{BufRead, Write};
use std::sync::Arc;

use futures::StreamExt;
use gadk::agent::{Context, RunConfig, StreamingMode, Tool, ToolError};
use gadk::engine::llm::LlmAgentConfig;
use gadk::engine::runner::{RunOptions, Runner};
use gadk::engine::tool::{AgentTool, FunctionTool, FunctionToolConfig};
use gadk::gemini::{ClientConfig, GeminiModel};
use gadk::genai::{Content, ROLE_USER};
use gadk::tools::gemini::GoogleSearch;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Go `Input`.
#[derive(Deserialize, JsonSchema)]
struct PoemInput {
    #[serde(rename = "lineCount")]
    line_count: i64,
}

/// Go `Output`.
#[derive(Serialize, JsonSchema)]
struct PoemOutput {
    poem: String,
}

/// Go's `poem` handler.
async fn poem(_ctx: Context, input: PoemInput) -> Result<PoemOutput, ToolError> {
    let lines = usize::try_from(input.line_count).unwrap_or(0);
    Ok(PoemOutput {
        poem: "A line of a poem,".repeat(lines) + "\n",
    })
}

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
    let search_agent = gadk::engine::llm::agent::new(LlmAgentConfig {
        name: "search_agent".to_string(),
        model: Some(model.clone()),
        description: "Does google search.".to_string(),
        instruction: "You're a specialist in Google Search.".to_string(),
        tools: vec![google_search],
        ..Default::default()
    })?;

    let poem_tool = FunctionTool::new(FunctionToolConfig::new("poem", "Returns poem"), poem)?;
    let poem_agent = gadk::engine::llm::agent::new(LlmAgentConfig {
        name: "poem_agent".to_string(),
        model: Some(model.clone()),
        description: "returns poem".to_string(),
        instruction: "You return poems.".to_string(),
        tools: vec![poem_tool],
        ..Default::default()
    })?;

    let root_agent = gadk::engine::llm::agent::new(LlmAgentConfig {
        name: "root_agent".to_string(),
        model: Some(model),
        description: "You can do a google search and generate poems.".to_string(),
        instruction: "Answer questions about weather based on google search unless asked for a poem, for a poem generate it with a tool.".to_string(),
        tools: vec![AgentTool::new(search_agent, None), AgentTool::new(poem_agent, None)],
        ..Default::default()
    })?;

    // Go also sets OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT=true for its launcher's
    // telemetry; setting the environment is `unsafe` in Rust 2024 and the workspace forbids it.
    let runner = Runner::in_memory("multiple_tools_example", root_agent)?;
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
