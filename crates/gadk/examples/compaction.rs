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

//! An LLM agent with sliding-window context compaction.
//!
//! Setting `compaction` on the launcher's [`Config`] enables compaction on every surface that
//! reads that config. The launcher used here, `full::new()`, serves the console, the web UI, A2A,
//! the Pub/Sub and Eventarc triggers, and the REST API.
//!
//! Run it and hold a conversation of several turns:
//!
//! Gemini API: `GOOGLE_API_KEY=... cargo run -p gadk --features launcher --example compaction console`
//! (or `… --example compaction web webui api`)
//! Vertex AI:  `GOOGLE_GENAI_USE_VERTEXAI=true GOOGLE_CLOUD_PROJECT=... GOOGLE_CLOUD_LOCATION=us-central1 cargo run -p gadk --features launcher --example compaction console`
//!
//! The web command needs at least one sub-launcher named after it, as above. Running it bare
//! exits with "no active sublaunchers found".
//!
//! After every two turns a compaction event is appended to the session, and the turns it covers
//! stop being sent to the model. The summary is bookkeeping rather than conversation, so it is
//! not streamed back with the agent's reply; look for it in the session's event list, for
//! example in the web UI.

use std::sync::Arc;

use gadk::agent::SingleLoader;
use gadk::compaction;
use gadk::engine::llm::LlmAgentConfig;
use gadk::gemini::{ClientConfig, GeminiModel};
use gadk::launcher::golog::Logger;
use gadk::launcher::{CancellationToken, Config, Launcher, full};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let model = GeminiModel::new("gemini-2.5-flash", ClientConfig::default()).await?;
    let agent = gadk::engine::llm::agent::new(LlmAgentConfig {
        name: "assistant".to_string(),
        model: Some(model),
        description: "A general purpose assistant with a long memory.".to_string(),
        instruction: "You are a helpful assistant. Keep your answers to a sentence or two."
            .to_string(),
        ..Default::default()
    })?;

    let mut config = Config::new(Arc::new(SingleLoader::new(agent)));
    // No summarizer, so the runner summarizes with the root agent's own model.
    config.compaction = Some(compaction::Config {
        // Sliding window: after every 2 completed turns, summarize the turns since the last
        // compaction, carrying 1 earlier turn forward so consecutive summaries overlap and context
        // is not lost at the seam. Runs once a turn has finished.
        compaction_interval: 2,
        overlap_size: 1,
        // Tail retention, left out on purpose: armed alongside the interval of 2 above it would do
        // nothing, since the sliding window summarizes every group of two turns, so the events it
        // looks at never reach `event_retention_size`. To try it, replace the two sliding-window
        // settings above with these. It runs during a turn, so it also catches a single long
        // tool-calling turn, and it is the trigger that bounds prompt size rather than just
        // reducing it. Running both is fine at a larger interval.
        //   token_threshold: 32_000,
        //   event_retention_size: 10,
        ..Default::default()
    });

    let mut l = full::new();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = l.execute(CancellationToken::new(), config, args).await {
        // Print the error and the command-line syntax, then exit.
        Logger::stderr().print(&format!("Run failed: {e}\n\n{}", l.command_line_syntax()));
        std::process::exit(1);
    }
    Ok(())
}
