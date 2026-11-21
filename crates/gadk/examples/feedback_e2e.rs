// Copyright 2026 Chris Olson
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

//! An end-to-end run of the metrics stack: an agent whose instruction is a versioned prompt,
//! recorded in BigQuery by the Agent Analytics plugin, with an end-user score on each
//! invocation.
//!
//! It creates (if absent) the prompt `e2e-assistant` in a file prompt store, with version 1
//! labelled `production`; runs three user turns in one session with the analytics plugin; scores
//! the three invocations with `record_score` (a thumbs up, the number 4 and the category
//! `helpful`); then closes the runner, which drains the queued rows. It prints the session and
//! each invocation id. The rows' trace ids are made by the plugin; read them back from the table
//! by invocation id.
//!
//! `GOOGLE_CLOUD_PROJECT=<project> GADK_ANALYTICS_DATASET=<dataset> GADK_E2E_PROMPTS=<dir> \
//!   cargo run -p gadk --features gemini,agentanalytics,prompts --example feedback_e2e`
//!
//! Uses Application Default Credentials for Gemini on Vertex AI (location `us`) and BigQuery.
//! `GADK_ANALYTICS_TABLE` overrides the table (default `agent_events`). Every row carries the
//! app name `gadk-e2e`.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use gadk::agent::RunConfig;
use gadk::agentanalytics::{
    Config, Score, ScoreTarget, ScoreValue, as_bigquery_agent_analytics_plugin,
    new_bigquery_agent_analytics_plugin_with_config,
};
use gadk::engine::llm::LlmAgentConfig;
use gadk::engine::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use gadk::gemini::{ClientConfig, GeminiModel};
use gadk::genai::{Content, ROLE_USER};
use gadk::model::Backend;
use gadk::platform::Platform;
use gadk::prompts::{FileStore, Label, NewVersion, PromptRegistry, PromptStore, Selector};
use gadk::session::InMemorySessionService;

const APP_NAME: &str = "gadk-e2e";
const PROMPT: &str = "e2e-assistant";
const USER: &str = "e2e-user";

/// Creates prompt `PROMPT` (version 1) and its `production` label unless they exist.
async fn ensure_prompt(store: &dyn PromptStore) -> anyhow::Result<()> {
    let snap = store.load_all().await?;
    if !snap.prompts.contains_key(PROMPT) {
        store
            .create_version(NewVersion {
                name: PROMPT.to_string(),
                template: "You are a concise assistant. Answer in one or two sentences."
                    .to_string(),
                config: None,
                author: "feedback_e2e".to_string(),
                message: "end-to-end check prompt".to_string(),
            })
            .await?;
    }
    let snap = store.load_all().await?;
    let labelled = snap
        .prompts
        .get(PROMPT)
        .is_some_and(|p| p.labels.contains_key(&Label::production()));
    if !labelled {
        store
            .set_label(PROMPT, &Label::production(), 1, Some(None))
            .await?;
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let project = std::env::var("GOOGLE_CLOUD_PROJECT")?;
    let dataset = std::env::var("GADK_ANALYTICS_DATASET")?;
    let table = std::env::var("GADK_ANALYTICS_TABLE").unwrap_or_else(|_| "agent_events".into());
    let prompts_dir = std::env::var("GADK_E2E_PROMPTS")?;

    let store: Arc<dyn PromptStore> = Arc::new(FileStore::new(prompts_dir));
    ensure_prompt(store.as_ref()).await?;
    let registry = PromptRegistry::builder(store).load().await?;
    let instruction = registry
        .instruction(PROMPT, Selector::Label(Label::production()))
        .into_provider();

    let model = GeminiModel::new(
        "gemini-3.8-flash",
        ClientConfig {
            backend: Some(Backend::VertexAi),
            project: Some(project.clone()),
            location: Some("us".to_string()),
            ..Default::default()
        },
    )
    .await?;
    let agent = gadk::engine::llm::agent::new(LlmAgentConfig {
        name: "e2e_assistant".to_string(),
        model: Some(model),
        description: "Answers short questions.".to_string(),
        instruction_provider: Some(instruction),
        ..Default::default()
    })?;

    let plugin = new_bigquery_agent_analytics_plugin_with_config(
        &gadk::agentanalytics::Context::background(),
        Config {
            project_id: project,
            dataset_id: dataset,
            table_name: table,
            ..Config::default()
        },
    )
    .await?;
    let analytics = as_bigquery_agent_analytics_plugin(&plugin)
        .ok_or_else(|| anyhow::anyhow!("not an analytics plugin"))?;
    let runner = Runner::new(RunnerConfig {
        app_name: APP_NAME.to_string(),
        agent,
        session_service: Arc::new(InMemorySessionService::new()),
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig {
            plugins: vec![plugin],
            ..PluginConfig::default()
        },
        auto_create_session: true,
        platform: Platform::default(),
        compaction: None,
    })?;

    let session = format!(
        "e2e-{}",
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs()
    );
    println!("session {session}");
    let turns = [
        "What is the capital of France?",
        "And the capital of Japan?",
        "Name one river in each of those two countries.",
    ];
    let mut invocations = Vec::new();
    for turn in turns {
        println!("user -> {turn}");
        let mut events = runner.run(
            USER,
            &session,
            Some(Content::from_text(turn, ROLE_USER)),
            RunConfig::default(),
            RunOptions::default(),
        );
        let mut invocation: Option<String> = None;
        while let Some(event) = events.next().await {
            let event = event?;
            if invocation.is_none() && !event.invocation_id.is_empty() {
                invocation = Some(event.invocation_id.clone());
            }
            if let Some(content) = &event.response.content {
                for part in &content.parts {
                    if !part.text.is_empty() && !part.thought {
                        println!("agent -> {}", part.text.trim());
                    }
                }
            }
        }
        let id = invocation.ok_or_else(|| anyhow::anyhow!("no event for {turn:?}"))?;
        println!("invocation {id}");
        invocations.push(id);
    }

    // One end-user score per invocation, recorded before closing so the rows are drained too.
    let scores = [
        Score::new("thumbs", ScoreValue::Bool(true))?,
        Score::new("rating", ScoreValue::Number(4.0))?,
        Score::new("quality", ScoreValue::Category("helpful".to_string()))?,
    ];
    for (id, score) in invocations.iter().zip(&scores) {
        let target = ScoreTarget::invocation(APP_NAME, USER, session.as_str(), id.as_str());
        analytics.record_score(&target, score, USER);
        println!("scored invocation {id}: {}", score.name());
    }

    // Closing the runner closes the plugin, which drains the queued rows.
    runner.close().await?;
    registry.close();
    println!("invocations: {}", invocations.join(" "));
    Ok(())
}
