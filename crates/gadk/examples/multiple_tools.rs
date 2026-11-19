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

//! Using Google Search together with a function tool. Gemini does not accept a built-in tool and
//! function tools in one request, so each kind lives in its own agent, and a root agent calls both
//! through agent tools. It runs through the launcher.
//!
//! `GOOGLE_API_KEY=... cargo run -p gadk --features launcher --example multiple_tools [console |
//! web webui api]`, then ask
//! "What is the weather in Paris?" or "Write a 3-line poem.".

use std::sync::Arc;

use gadk::agent::{Context, SingleLoader, Tool, ToolError};
use gadk::engine::llm::LlmAgentConfig;
use gadk::engine::tool::{AgentTool, FunctionTool, FunctionToolConfig};
use gadk::gemini::{ClientConfig, GeminiModel};
use gadk::launcher::golog::Logger;
use gadk::launcher::{CancellationToken, Config, Launcher, full};
use gadk::tools::gemini::GoogleSearch;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// The input of the `poem` tool (a plain comment: a doc comment would become the schema's
// description).
#[derive(Deserialize, JsonSchema)]
struct PoemInput {
    #[serde(rename = "lineCount")]
    line_count: i64,
}

// The output of the `poem` tool.
#[derive(Serialize, JsonSchema)]
struct PoemOutput {
    poem: String,
}

/// The `poem` tool: writes a poem of the requested number of lines.
async fn poem(_ctx: Context, input: PoemInput) -> Result<PoemOutput, ToolError> {
    let lines = usize::try_from(input.line_count).unwrap_or(0);
    Ok(PoemOutput {
        poem: "A line of a poem,".repeat(lines) + "\n",
    })
}

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

    // Set OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT=true in the shell that runs this
    // example for the launcher's OpenTelemetry export to carry message content; setting the
    // environment from code is `unsafe` in Rust 2024, which the workspace forbids.
    let config = Config::new(Arc::new(SingleLoader::new(root_agent)));
    let mut l = full::new();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = l.execute(CancellationToken::new(), config, args).await {
        // Print the error and the command-line syntax, then exit.
        Logger::stderr().print(&format!("Run failed: {e}\n\n{}", l.command_line_syntax()));
        std::process::exit(1);
    }
    Ok(())
}
