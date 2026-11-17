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

//! Live streaming tool: `count_to` streams its results back to the model while the session
//! continues; the model can cancel it with `stop_streaming`.
//!
//! Gemini API: `GOOGLE_API_KEY=... cargo run -p gadk --example bidi_streaming_tool`
//!
//! The model defaults to `gemini-3.1-flash-live-preview`; override it with `GADK_MODEL`.

use std::io::BufRead;
use std::time::Duration;

use futures::StreamExt;
use gadk::agent::{Context, LiveRequest, LiveRunConfig, ToolError};
use gadk::engine::llm::LlmAgentConfig;
use gadk::engine::runner::{RunOptions, Runner};
use gadk::engine::tool::{FunctionTool, FunctionToolConfig, StreamingFunctionTool};
use gadk::gemini::{ClientConfig, GeminiModel};
use gadk::genai::{AudioTranscriptionConfig, Content, Modality, ROLE_USER};
use gadk::session::Event;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Arguments of `count_to`.
#[derive(Deserialize, JsonSchema)]
struct CountArgs {
    n: i64,
}

/// Arguments of `stop_streaming`.
#[derive(Deserialize, JsonSchema)]
struct StopArgs {
    function_name: String,
}

/// Result of `stop_streaming`: `{"status": …}`.
#[derive(Serialize, JsonSchema)]
struct StatusResult {
    status: String,
}

/// Arguments of `check_divisible`.
#[derive(Deserialize, JsonSchema)]
struct DivisibleArgs {
    number: i64,
    divisor: i64,
}

/// Result of `check_divisible`: `{"result": bool}`, or `{"result": false, "error": …}`.
#[derive(Serialize, JsonSchema)]
struct DivisibleResult {
    result: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// The flow answers `stop_streaming` itself in live mode; this body runs only outside it.
async fn stop_streaming(_ctx: Context, args: StopArgs) -> Result<StatusResult, ToolError> {
    Ok(StatusResult {
        status: format!("Requested to stop {}", args.function_name),
    })
}

async fn check_divisible(_ctx: Context, args: DivisibleArgs) -> Result<DivisibleResult, ToolError> {
    if args.divisor == 0 {
        return Ok(DivisibleResult {
            result: false,
            error: Some("cannot divide by zero".to_string()),
        });
    }
    println!("Dividing {} by {}", args.number, args.divisor);
    Ok(DivisibleResult {
        result: args.number % args.divisor == 0,
        error: None,
    })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let model_name =
        std::env::var("GADK_MODEL").unwrap_or_else(|_| "gemini-3.1-flash-live-preview".to_string());
    let model = GeminiModel::new(&model_name, ClientConfig::default()).await?;

    let counter = StreamingFunctionTool::new(
        FunctionToolConfig::new(
            "count_to",
            "Counts to a specified number, yielding each number with a delay.",
        ),
        |_ctx: Context, args: CountArgs| {
            let n = args.n;
            futures::stream::unfold(1i64, move |i| async move {
                if i > n {
                    return None;
                }
                tokio::time::sleep(Duration::from_millis(5000)).await;
                Some((Ok::<String, ToolError>(format!("Count: {i}")), i + 1))
            })
        },
    )?;
    let stop = FunctionTool::new(
        FunctionToolConfig::new("stop_streaming", "Stops a running streaming function."),
        stop_streaming,
    )?;
    let divisible = FunctionTool::new(
        FunctionToolConfig::new(
            "check_divisible",
            "Checks if a number is divisible by another number.",
        ),
        check_divisible,
    )?;
    let agent = gadk::engine::llm::agent::new(LlmAgentConfig {
        name: "bidi-demo".to_string(),
        model: Some(model),
        instruction: "You are a helpful assistant with a streaming tool 'count_to'. Always use it when asked to count. Wait for the tool results, and when you receive them you should say the number, if it is divisible by 3 you should not say the number and instead say Fizz and if it is divisible by 5 you should say Buzz, if it is divisible by both 3 and 5 you should say FizzBuzz. Always use the check_divisible tool".to_string(),
        tools: vec![counter, stop, divisible],
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
