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

//! An `LlmAgent` on an OpenAI (or OpenAI-compatible) model with a function tool (port of
//! ADK-Go `examples/openai/responses/main.go` and `examples/openai/completions/main.go`, which
//! differ only in `ClientConfig.API`). Go hands the agent to its console launcher; this example
//! reads turns from stdin (one line per turn, until end of input) in one session, like the other
//! gadk examples.
//!
//! OpenAI:            `OPENAI_API_KEY=... cargo run -p gadk --example openai --features openai`
//! Compatible server: `OPENAI_BASE_URL=http://localhost:11434/v1/ cargo run -p gadk --example openai --features openai`
//!
//! `OPENAI_API` (`responses`, the default, or `chat_completions`) selects the endpoint;
//! `OPENAI_MODEL` overrides the model.

use std::io::{BufRead, Write};

use futures::StreamExt;
use gadk::agent::{AgentError, Context, RunConfig, ToolError};
use gadk::engine::llm::LlmAgentConfig;
use gadk::engine::runner::{RunOptions, Runner};
use gadk::engine::tool::{FunctionTool, FunctionToolConfig};
use gadk::genai::{Content, ROLE_USER};
use gadk::model::ModelError;
use gadk::openai::{Api, ClientConfig, OpenAiError, new_model};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Go `defaultModel`: used when `OPENAI_MODEL` is unset; cheap, and serves both APIs.
const DEFAULT_MODEL: &str = "gpt-4o-mini";

const APP_NAME: &str = "openai_example";
const USER_ID: &str = "example_user";
const SESSION_ID: &str = "session_1";

// Go `weatherInput` (plain comments: a doc comment would become the schema's description).
#[derive(Deserialize, JsonSchema)]
struct WeatherInput {
    city: String,
}

// Go `weatherOutput`.
#[derive(Serialize, JsonSchema)]
struct WeatherOutput {
    report: String,
}

/// Go `getWeather`: a stand-in for a real weather API, so the sample needs nothing beyond the
/// model call.
async fn get_weather(_ctx: Context, input: WeatherInput) -> Result<WeatherOutput, ToolError> {
    Ok(WeatherOutput {
        report: format!("It is currently 22°C and sunny in {}.", input.city),
    })
}

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("{}", explain(&err));
        std::process::exit(1);
    }
}

async fn run() -> anyhow::Result<()> {
    let var = |k: &str| {
        std::env::var(k)
            .map(|v| v.trim().to_string())
            .unwrap_or_default()
    };
    // Point at api.openai.com with a key, or at any endpoint that implements the OpenAI API
    // (recent Ollama, LM Studio, vLLM) via OPENAI_BASE_URL.
    let api_key = var("OPENAI_API_KEY");
    let base_url = var("OPENAI_BASE_URL");
    if api_key.is_empty() && base_url.is_empty() {
        anyhow::bail!(
            "set OPENAI_API_KEY (for OpenAI) or OPENAI_BASE_URL (for an OpenAI-compatible endpoint)"
        );
    }
    let api: Api = var("OPENAI_API").parse()?;
    let model_name = Some(var("OPENAI_MODEL"))
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| DEFAULT_MODEL.to_string());

    let llm = new_model(
        &model_name,
        ClientConfig {
            api_key,
            base_url,
            api,
            ..ClientConfig::default()
        },
    )
    .map_err(|e| anyhow::Error::new(e).context("create model"))?;

    let weather_tool = FunctionTool::new(
        FunctionToolConfig::new(
            "get_weather",
            "Returns the current weather for a given city.",
        ),
        get_weather,
    )
    .map_err(|e| anyhow::Error::new(e).context("create tool"))?;

    let agent = gadk::engine::llm::agent::new(LlmAgentConfig {
        name: "openai_weather_agent".into(),
        model: Some(llm),
        description: "Answers weather questions using an OpenAI model.".into(),
        instruction: "You are a helpful assistant. When asked about the weather in a city, call the get_weather tool and report the result.".into(),
        tools: vec![weather_tool],
        ..Default::default()
    })
    .map_err(|e| anyhow::Error::new(e).context("create agent"))?;

    // The in-memory runner creates the session on its first run.
    let runner = Runner::in_memory(APP_NAME, agent)
        .map_err(|e| anyhow::Error::new(e).context("create runner"))?;

    println!("API: {api} ({model_name}); ask about the weather in a city.");
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
        run_turn(&runner, Content::from_text(line, ROLE_USER)).await?;
    }
    runner.close().await?;
    Ok(())
}

/// Prints tool calls, tool results and the agent's (non-thought) text.
async fn run_turn(runner: &Runner, msg: Content) -> anyhow::Result<()> {
    let mut events = runner.run(
        USER_ID,
        SESSION_ID,
        Some(msg),
        RunConfig::default(),
        RunOptions::default(),
    );
    while let Some(event) = events.next().await {
        let event = event?;
        let Some(content) = event.response.content else {
            continue;
        };
        for part in content.parts {
            if let Some(fc) = part.function_call {
                println!(
                    "Tool Call -> {}({})",
                    fc.name,
                    serde_json::Value::Object(fc.args)
                );
            } else if let Some(fr) = part.function_response {
                println!(
                    "Tool Result -> {}: {}",
                    fr.name,
                    serde_json::Value::Object(fr.response)
                );
            } else if !part.text.is_empty() && !part.thought {
                println!("Agent -> {}", part.text.trim());
            }
        }
    }
    Ok(())
}

/// An OpenAI API error as its status and body; anything else as its text. A model error
/// reaches the runner's stream as `AgentError::Model(ModelError::Other(_))`.
fn explain(err: &anyhow::Error) -> String {
    let openai = err.chain().find_map(|e| {
        if let Some(o) = e.downcast_ref::<OpenAiError>() {
            return Some(o);
        }
        let model = match e.downcast_ref::<AgentError>() {
            Some(AgentError::Model(m)) => m,
            _ => e.downcast_ref::<ModelError>()?,
        };
        match model {
            ModelError::Other(inner) => inner.downcast_ref::<OpenAiError>(),
            _ => None,
        }
    });
    match openai.and_then(OpenAiError::api_error) {
        Some(api) => format!("{api}: {}", String::from_utf8_lossy(&api.body)),
        None => format!("{err:#}"),
    }
}
