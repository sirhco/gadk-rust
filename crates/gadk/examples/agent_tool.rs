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

//! An LLM agent with one function tool, run through `Runner::in_memory`.
//!
//! Gemini API: `GOOGLE_API_KEY=... cargo run -p gadk --example agent_tool`
//! Vertex AI:  `GOOGLE_GENAI_USE_VERTEXAI=true GOOGLE_CLOUD_PROJECT=... GOOGLE_CLOUD_LOCATION=us cargo run -p gadk --example agent_tool`
//!
//! The model defaults to `gemini-3.8-flash`; override it with `GADK_MODEL`.

use futures::StreamExt;
use gadk::agent::{Context, RunConfig, ToolError};
use gadk::engine::llm::LlmAgentConfig;
use gadk::engine::runner::{RunOptions, Runner};
use gadk::engine::tool::{FunctionTool, FunctionToolConfig};
use gadk::gemini::{ClientConfig, GeminiModel};
use gadk::genai::{Content, ROLE_USER};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Arguments of the `get_weather` tool.
#[derive(Deserialize, JsonSchema)]
struct WeatherArgs {
    /// The city to report the weather for.
    city: String,
}

/// Result of the `get_weather` tool.
#[derive(Serialize, JsonSchema)]
struct WeatherReport {
    city: String,
    forecast: String,
    temperature_celsius: i32,
}

/// A canned weather service, so the example needs no second API.
async fn get_weather(_ctx: Context, args: WeatherArgs) -> Result<WeatherReport, ToolError> {
    Ok(WeatherReport {
        city: args.city,
        forecast: "sunny".to_string(),
        temperature_celsius: 21,
    })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let model_name = std::env::var("GADK_MODEL").unwrap_or_else(|_| "gemini-3.8-flash".to_string());
    let model = GeminiModel::new(&model_name, ClientConfig::default()).await?;

    let weather = FunctionTool::new(
        FunctionToolConfig::new("get_weather", "Returns the current weather for a city."),
        get_weather,
    )?;
    let agent = gadk::engine::llm::agent::new(LlmAgentConfig {
        name: "weather_agent".to_string(),
        description: "Answers questions about the weather.".to_string(),
        model: Some(model),
        instruction: "Answer weather questions. Always call get_weather for the city asked about."
            .to_string(),
        tools: vec![weather],
        ..Default::default()
    })?;

    let runner = Runner::in_memory("agent_tool_example", agent)?;
    let msg = Content::from_text("What is the weather in Paris right now?", ROLE_USER);
    let mut events = runner.run(
        "user",
        "session-1",
        Some(msg),
        RunConfig::default(),
        RunOptions::default(),
    );
    while let Some(event) = events.next().await {
        let event = event?;
        let parts = event.response.content.iter().flat_map(|c| c.parts.iter());
        for part in parts {
            if let Some(call) = &part.function_call {
                let args = Value::Object(call.args.clone());
                println!("[{}] tool call: {}({args})", event.author, call.name);
            } else if let Some(resp) = &part.function_response {
                let result = Value::Object(resp.response.clone());
                println!("[{}] tool result: {} -> {result}", event.author, resp.name);
            } else if !part.thought && !part.text.is_empty() {
                println!("[{}] {}", event.author, part.text);
            }
        }
    }
    runner.close().await?;
    Ok(())
}
