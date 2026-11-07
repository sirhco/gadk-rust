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

//! An agent that recalls a previous conversation through the `preload_memory` and `load_memory`
//! tools (port of Go examples/tools/loadmemory/main.go).
//!
//! `GOOGLE_API_KEY=... cargo run -p gadk --example load_memory`, then ask
//! "What do you remember about my trip?".

use std::io::{BufRead, Write};
use std::sync::Arc;

use futures::StreamExt;
use gadk::agent::{RunConfig, StreamingMode};
use gadk::engine::llm::LlmAgentConfig;
use gadk::engine::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use gadk::gemini::{ClientConfig, GeminiModel};
use gadk::genai::{Content, ROLE_USER};
use gadk::memory::{InMemoryMemoryService, MemoryService};
use gadk::platform::Platform;
use gadk::session::{
    CreateRequest, Event, GetRequest, InMemorySessionService, Session, SessionService,
};

const USER_ID: &str = "test_user";
const APP_NAME: &str = "memory_app";

/// Go `createPreviousSessionWithHistory`'s events: a conversation about a trip to Tokyo.
const HISTORY: [(&str, &str); 8] = [
    ("user", "I just got back from an amazing trip to Tokyo!"),
    (
        "model",
        "That sounds wonderful! Tokyo is such a vibrant city. What were the highlights of your trip?",
    ),
    (
        "user",
        "I visited the Senso-ji temple in Asakusa, it was beautiful. I also tried authentic ramen at a small shop in Shinjuku.",
    ),
    (
        "model",
        "Senso-ji is one of Tokyo's oldest and most significant temples. The ramen in Shinjuku is legendary! Did you try any other Japanese cuisine?",
    ),
    (
        "user",
        "Yes! I had sushi at Tsukiji outer market and tried takoyaki in Shibuya. The food was incredible.",
    ),
    (
        "model",
        "You really experienced the best of Tokyo's food scene! Tsukiji is famous for its fresh seafood. Did you get to see any other sights?",
    ),
    (
        "user",
        "I went to the top of Tokyo Skytree and saw Mount Fuji in the distance. The view was breathtaking.",
    ),
    (
        "model",
        "Seeing Mount Fuji from Tokyo Skytree is a special experience, especially on a clear day. It sounds like you had an unforgettable trip!",
    ),
];

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

    let agent = gadk::engine::llm::agent::new(LlmAgentConfig {
        name: "memory_assistant".to_string(),
        model: Some(model),
        description: "Agent that can recall information from memory.".to_string(),
        instruction: "You are a helpful assistant with access to memory. \
            Relevant memory may be preloaded automatically for each request. \
            If the preloaded context is not enough, use the load_memory tool \
            to search for additional relevant information. \
            If you find relevant memories, use them to provide informed responses."
            .to_string(),
        tools: vec![
            gadk::tools::preload_memory::new(),
            gadk::tools::load_memory::new(),
        ],
        ..Default::default()
    })?;

    let session_service = Arc::new(InMemorySessionService::new());
    let memory_service = Arc::new(InMemoryMemoryService::new());

    // A previous session with some conversation history, added to memory so it can be searched.
    let previous = create_previous_session_with_history(&session_service).await?;
    memory_service.add_session_to_memory(&*previous).await?;

    println!("Memory populated with previous conversation about a trip to Tokyo.");
    println!("Memories will be preloaded automatically for each request.");
    println!("Try asking: 'What do you remember about my trip?'");

    // A new session for the current conversation.
    let current = session_service
        .create(CreateRequest {
            app_name: APP_NAME.to_string(),
            user_id: USER_ID.to_string(),
            ..Default::default()
        })
        .await?;

    let runner = Runner::new(RunnerConfig {
        app_name: APP_NAME.to_string(),
        agent,
        session_service,
        artifact_service: None,
        memory_service: Some(memory_service),
        plugin_config: PluginConfig::default(),
        auto_create_session: false,
        platform: Platform::default(),
        compaction: None,
    })?;

    repl(&runner, USER_ID, current.id()).await?;
    runner.close().await?;
    Ok(())
}

/// Port of Go `createPreviousSessionWithHistory`. Returns the session as stored, events included.
async fn create_previous_session_with_history(
    session_service: &InMemorySessionService,
) -> anyhow::Result<Arc<dyn Session>> {
    let session = session_service
        .create(CreateRequest {
            app_name: APP_NAME.to_string(),
            user_id: USER_ID.to_string(),
            ..Default::default()
        })
        .await?;
    let platform = Platform::default();
    for (author, text) in HISTORY {
        let mut event = Event::new(&platform, "previous-session");
        event.author = author.to_string();
        event.response.content = Some(Content::from_text(text, author));
        session_service.append_event(&*session, &mut event).await?;
    }
    Ok(session_service
        .get(GetRequest {
            app_name: APP_NAME.to_string(),
            user_id: USER_ID.to_string(),
            session_id: session.id().to_string(),
            ..Default::default()
        })
        .await?)
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
