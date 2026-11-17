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

//! Live (bidi streaming) quickstart: one LLM agent driven through `Runner::run_live`, with text
//! typed on stdin as the user's turns and the model's audio transcribed to stdout.
//!
//! Gemini API: `GOOGLE_API_KEY=... cargo run -p gadk --example bidi`
//!
//! The model defaults to `gemini-3.1-flash-live-preview`; override it with `GADK_MODEL`.

use std::io::BufRead;

use futures::StreamExt;
use gadk::agent::{Context, LiveRequest, LiveRunConfig, ToolError};
use gadk::engine::llm::LlmAgentConfig;
use gadk::engine::runner::{RunOptions, Runner};
use gadk::engine::tool::{FunctionTool, FunctionToolConfig};
use gadk::gemini::{ClientConfig, GeminiModel};
use gadk::genai::{AudioTranscriptionConfig, Content, Modality, ROLE_USER};
use gadk::session::Event;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Arguments of `camera_toggle` (none).
#[derive(Deserialize, JsonSchema)]
struct EmptyArgs {}

/// Result of `camera_toggle`.
#[derive(Serialize, JsonSchema)]
struct MessageResult {
    message: String,
}

async fn camera_toggle(_ctx: Context, _args: EmptyArgs) -> Result<MessageResult, ToolError> {
    println!("Camera tool was called!");
    Ok(MessageResult {
        message: "Camera tool called successfully!".to_string(),
    })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let model_name =
        std::env::var("GADK_MODEL").unwrap_or_else(|_| "gemini-3.1-flash-live-preview".to_string());
    let model = GeminiModel::new(&model_name, ClientConfig::default()).await?;

    let camera = FunctionTool::new(
        FunctionToolConfig::new("camera_toggle", "Turns the camera on or off."),
        camera_toggle,
    )?;
    // No Google Search tool is configured: gadk has no Google Search tool yet.
    let agent = gadk::engine::llm::agent::new(LlmAgentConfig {
        name: "bidi-demo".to_string(),
        description: "Agent optimized for real-time bidirectional streaming.".to_string(),
        model: Some(model),
        instruction: "You are a real-time voice assistant.".to_string(),
        tools: vec![camera],
        ..Default::default()
    })?;

    let runner = Runner::in_memory("bidi-demo", agent)?;
    drive(&runner).await
}

/// Runs one live session: stdin lines become user turns; events are printed until the session
/// ends.
async fn drive(runner: &Runner) -> anyhow::Result<()> {
    let cfg = LiveRunConfig {
        response_modalities: vec![Modality::Audio],
        output_audio_transcription: Some(AudioTranscriptionConfig::default()),
        ..Default::default()
    };
    let (session, mut events) = runner
        .run_live("user", "bidi-session", cfg, RunOptions::default())
        .await?;

    let (tx, mut lines) = tokio::sync::mpsc::channel::<String>(8);
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if tx.blocking_send(line).is_err() {
                break;
            }
        }
    });

    let mut stdin_open = true;
    loop {
        tokio::select! {
            line = lines.recv(), if stdin_open => match line {
                Some(line) => {
                    session
                        .send(LiveRequest {
                            content: Some(Content::from_text(line, ROLE_USER)),
                            ..Default::default()
                        })
                        .await?;
                }
                None => {
                    stdin_open = false;
                    session.close().await?;
                }
            },
            event = events.next() => match event {
                Some(Ok(event)) => print_event(&event),
                Some(Err(e)) => eprintln!("error: {e}"),
                None => break,
            },
        }
    }
    Ok(())
}

fn print_event(event: &Event) {
    if let Some(t) = &event.response.output_transcription {
        println!("[{}] (transcript) {}", event.author, t.text);
    }
    for part in event.response.content.iter().flat_map(|c| &c.parts) {
        if !part.text.is_empty() {
            println!("[{}] {}", event.author, part.text);
        }
        if let Some(fc) = &part.function_call {
            println!(
                "[{}] call {}({})",
                event.author,
                fc.name,
                serde_json::Value::Object(fc.args.clone())
            );
        }
    }
}
