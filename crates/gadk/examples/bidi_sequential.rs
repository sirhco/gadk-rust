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

//! Live sequential agents: an idea generator hands over to a story teller when it calls the
//! `task_completed` tool that `SequentialAgent::run_live` injects.
//!
//! Gemini API: `GOOGLE_API_KEY=... cargo run -p gadk --example bidi_sequential`
//!
//! The model defaults to `gemini-2.5-flash-native-audio-preview-12-2025`; override it with
//! `GADK_MODEL`.

use std::io::BufRead;

use futures::StreamExt;
use gadk::agent::{AgentConfig, LiveRequest, LiveRunConfig};
use gadk::engine::llm::LlmAgentConfig;
use gadk::engine::runner::{RunOptions, Runner};
use gadk::engine::sequential_agent::{self, SequentialAgentConfig};
use gadk::gemini::{ClientConfig, GeminiModel};
use gadk::genai::{AudioTranscriptionConfig, Content, Modality, ROLE_USER};
use gadk::session::Event;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let model_name = std::env::var("GADK_MODEL")
        .unwrap_or_else(|_| "gemini-2.5-flash-native-audio-preview-12-2025".to_string());
    let model = GeminiModel::new(&model_name, ClientConfig::default()).await?;

    // No Google Search tool is configured: gadk has no Google Search tool yet.
    let idea_generator = gadk::engine::llm::agent::new(LlmAgentConfig {
        name: "idea_generator".to_string(),
        description: "Brainstorms creative story ideas with the user.".to_string(),
        model: Some(model.clone()),
        instruction: "You are the Idea Generator. Ask the user for a topic they are interested in, and brainstorm 3 creative story ideas for them. Discuss the options and help them choose their favorite idea. Once the user confirms their choice, call the task_completed function so the Story Teller agent can take over to narrate the story.".to_string(),
        ..Default::default()
    })?;
    let story_teller = gadk::engine::llm::agent::new(LlmAgentConfig {
        name: "story_teller".to_string(),
        description: "Narrates an engaging story based on the chosen idea.".to_string(),
        model: Some(model),
        instruction: "You are the Story Teller. The previous agent has just finalized a story idea with the user. Greet the user, ask them to remind you of the chosen idea, and then narrate an exciting, highly engaging short story about it using your voice.".to_string(),
        ..Default::default()
    })?;
    let seq_agent = sequential_agent::new(SequentialAgentConfig {
        agent: AgentConfig {
            name: "bidi-demo".to_string(),
            sub_agents: vec![idea_generator, story_teller],
            ..Default::default()
        },
    })?;

    let runner = Runner::in_memory("bidi-demo", seq_agent)?;
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
