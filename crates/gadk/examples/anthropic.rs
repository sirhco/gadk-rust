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

//! An `LlmAgent` on Claude with a function tool. The first turn sends an inline PNG image and the
//! agent calls `get_palette_info` while answering; afterwards the example reads follow-up turns
//! from stdin (one line per turn, until end of input) in the same session, like the other gadk
//! examples.
//!
//! Anthropic API: `ANTHROPIC_API_KEY=... cargo run -p gadk --example anthropic --features anthropic`
//! Vertex AI:     `GOOGLE_CLOUD_PROJECT=... GOOGLE_CLOUD_LOCATION=us-east5 cargo run -p gadk --example anthropic --features anthropic`
//!
//! `ANTHROPIC_BACKEND` (`vertex` or `anthropic`) forces a backend; `ANTHROPIC_MODEL` overrides
//! the model.

use std::io::{BufRead, Write};

use futures::StreamExt;
use gadk::agent::{AgentError, Context, RunConfig, ToolError};
use gadk::anthropic::{AnthropicError, AnthropicModel, Backend, ClientConfig};
use gadk::engine::llm::LlmAgentConfig;
use gadk::engine::runner::{RunOptions, Runner};
use gadk::engine::tool::{FunctionTool, FunctionToolConfig};
use gadk::genai::{Content, Part, ROLE_USER};
use gadk::model::ModelError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A versioned Claude model ID served on both Vertex AI and the first-party
/// Anthropic API. Vertex AI does not serve `-latest` aliases.
const DEFAULT_MODEL: &str = "claude-sonnet-4-5-20250929";

/// A 1x1 solid blue PNG image embedded directly so the sample runs without
/// external asset files.
const SAMPLE_BLUE_PNG: [u8; 73] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, //
    0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, //
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, //
    0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53, //
    0xde, 0x00, 0x00, 0x00, 0x10, 0x49, 0x44, 0x41, //
    0x54, 0x78, 0x9c, 0x62, 0x62, 0x60, 0xf8, 0x0f, //
    0x08, 0x00, 0x00, 0xff, 0xff, 0x01, 0x0b, 0x01, //
    0x02, 0xd6, 0x95, 0x43, 0xa8, 0x00, 0x00, 0x00, //
    0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, //
    0x82,
];

const APP_NAME: &str = "anthropic_example";
const USER_ID: &str = "example_user";
const SESSION_ID: &str = "session_1";

// The input of `get_palette_info` (a plain comment: a doc comment would become the schema's
// description).
#[derive(Deserialize, JsonSchema)]
struct PaletteInput {
    /// The dominant color name in lowercase, such as blue, red, or green.
    color: String,
}

// The output of `get_palette_info`.
#[derive(Serialize, JsonSchema)]
struct PaletteOutput {
    color: String,
    hex_code: String,
    family: String,
}

/// Looks up the hex code and color family of a color name.
async fn get_palette_info(_ctx: Context, input: PaletteInput) -> Result<PaletteOutput, ToolError> {
    let color = input.color.trim().to_lowercase();
    let (hex_code, family) = match color.as_str() {
        "blue" => ("#0055FF", "cool primary"),
        "red" => ("#FF2200", "warm primary"),
        _ => ("#336699", "standard palette"),
    };
    Ok(PaletteOutput {
        color,
        hex_code: hex_code.into(),
        family: family.into(),
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
    let cfg = resolve_client_config()?;
    let backend = cfg.backend;

    let model_name = std::env::var("ANTHROPIC_MODEL")
        .ok()
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| DEFAULT_MODEL.to_string());
    let llm = AnthropicModel::new(&model_name, cfg)
        .await
        .map_err(|e| anyhow::Error::new(e).context("create Claude model"))?;

    let palette_tool = FunctionTool::new(
        FunctionToolConfig::new(
            "get_palette_info",
            "Looks up the canonical design-system hex code and color family for a color name.",
        ),
        get_palette_info,
    )
    .map_err(|e| anyhow::Error::new(e).context("create tool"))?;

    let agent = gadk::engine::llm::agent::new(LlmAgentConfig {
        name: "claude_vision_agent".into(),
        model: Some(llm),
        description: "Inspects an inline image and looks up its design-system palette metadata."
            .into(),
        instruction: "Identify the dominant color in the attached image, call get_palette_info with that color name, and report the hex code and color family.".into(),
        tools: vec![palette_tool],
        ..Default::default()
    })
    .map_err(|e| anyhow::Error::new(e).context("create agent"))?;

    // The in-memory runner creates the session on its first run.
    let runner = Runner::in_memory(APP_NAME, agent)
        .map_err(|e| anyhow::Error::new(e).context("create runner"))?;

    let prompt = "What color is the attached image? Use get_palette_info to report its hex code and color family.";
    let user_msg = Content {
        role: ROLE_USER.into(),
        parts: vec![
            Part::from_bytes(SAMPLE_BLUE_PNG.to_vec().into(), "image/png"),
            Part::from_text(prompt),
        ],
    };

    println!("Backend: {backend} ({model_name})");
    println!(
        "User -> [inline image/png, {} bytes] {prompt}",
        SAMPLE_BLUE_PNG.len()
    );
    run_turn(&runner, user_msg).await?;

    // Follow-up turns from stdin, in the same session.
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

/// Runs one turn: prints tool calls, tool results and the agent's (non-thought) text.
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
            // Tool args and results print as JSON.
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

/// Resolves the backend: from `ANTHROPIC_BACKEND`, else Vertex AI when a Google
/// Cloud project or location is set, else the Anthropic API when `ANTHROPIC_API_KEY` is set.
fn resolve_client_config() -> anyhow::Result<ClientConfig> {
    let var = |k: &str| {
        std::env::var(k)
            .map(|v| v.trim().to_string())
            .unwrap_or_default()
    };
    let backend_env = var("ANTHROPIC_BACKEND").to_lowercase();
    let project = var("GOOGLE_CLOUD_PROJECT");
    let location = var("GOOGLE_CLOUD_LOCATION");
    let api_key = var("ANTHROPIC_API_KEY");

    let vertex = |project: String, location: String| ClientConfig {
        backend: Backend::Vertex,
        project: Some(project),
        location: Some(location),
        ..Default::default()
    };
    let anthropic = |api_key: String| ClientConfig {
        backend: Backend::Anthropic,
        api_key: Some(api_key),
        ..Default::default()
    };

    match backend_env.as_str() {
        "vertex" => {
            if project.is_empty() {
                anyhow::bail!("GOOGLE_CLOUD_PROJECT must be set when ANTHROPIC_BACKEND=vertex");
            }
            if location.is_empty() {
                anyhow::bail!("GOOGLE_CLOUD_LOCATION must be set when ANTHROPIC_BACKEND=vertex");
            }
            Ok(vertex(project, location))
        }
        "anthropic" => {
            if api_key.is_empty() {
                anyhow::bail!("ANTHROPIC_API_KEY must be set when ANTHROPIC_BACKEND=anthropic");
            }
            Ok(anthropic(api_key))
        }
        "" => {
            if !project.is_empty() || !location.is_empty() {
                if project.is_empty() {
                    anyhow::bail!(
                        "GOOGLE_CLOUD_PROJECT must be set alongside GOOGLE_CLOUD_LOCATION for Vertex AI"
                    );
                }
                if location.is_empty() {
                    anyhow::bail!(
                        "GOOGLE_CLOUD_LOCATION must be set alongside GOOGLE_CLOUD_PROJECT for Vertex AI"
                    );
                }
                return Ok(vertex(project, location));
            }
            if !api_key.is_empty() {
                return Ok(anthropic(api_key));
            }
            anyhow::bail!(
                "set GOOGLE_CLOUD_PROJECT and GOOGLE_CLOUD_LOCATION (for Vertex AI) or ANTHROPIC_API_KEY (for the Anthropic API)"
            )
        }
        other => anyhow::bail!(
            "unsupported ANTHROPIC_BACKEND {other:?}; valid values are \"vertex\" or \"anthropic\""
        ),
    }
}

/// Describes an error: a Claude API error as its status, type and message; anything else as its
/// text. A model error reaches the runner's stream as `AgentError::Model(ModelError::Other(_))`.
fn explain(err: &anyhow::Error) -> String {
    let anthropic = err.chain().find_map(|e| {
        if let Some(a) = e.downcast_ref::<AnthropicError>() {
            return Some(a);
        }
        let model = match e.downcast_ref::<AgentError>() {
            Some(AgentError::Model(m)) => m,
            _ => e.downcast_ref::<ModelError>()?,
        };
        match model {
            ModelError::Other(inner) => inner.downcast_ref::<AnthropicError>(),
            _ => None,
        }
    });
    match anthropic.and_then(AnthropicError::api_error) {
        Some(api) => format!(
            "Claude API error (status {}, type {}): {}",
            api.status_code, api.error_type, api.message
        ),
        None => format!("{err:#}"),
    }
}
