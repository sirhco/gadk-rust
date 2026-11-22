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

//! An end-to-end run of a prompt rollout: an agent whose instruction is the `production` label
//! of a versioned prompt, with version 2 rolled out to 50% of sessions, recorded in BigQuery by
//! the Agent Analytics plugin.
//!
//! It creates (if absent) versions 1 and 2 of the prompt `rollout-assistant` in a file prompt
//! store, points `production` at version 1 and starts a 50% rollout of version 2 on it; then runs
//! one user turn in each of N sessions (distinct session ids) and prints the version each
//! session was served (`session <id> -> v<N>`), a count per version and the store directory.
//! Closing the runner drains the queued rows. The store directory is left in place, so
//! `gadk-console --store file://<dir>` shows the rollout and per-version metrics for both
//! versions.
//!
//! `GOOGLE_CLOUD_PROJECT=<project> GADK_ANALYTICS_DATASET=<dataset> GADK_ROLLOUT_PROMPTS=<dir> \
//!   cargo run -p gadk --features gemini,agentanalytics,prompts --example rollout_e2e`
//!
//! Uses Application Default Credentials for Gemini on Vertex AI (location `us`) and BigQuery.
//! `GADK_ANALYTICS_TABLE` overrides the table (default `agent_events`) and
//! `GADK_ROLLOUT_SESSIONS` the number of sessions (default 8). Every row carries the app name
//! `gadk-rollout-e2e`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use futures::StreamExt;
use gadk::agent::RunConfig;
use gadk::agentanalytics::{Config, new_bigquery_agent_analytics_plugin_with_config};
use gadk::engine::llm::LlmAgentConfig;
use gadk::engine::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use gadk::gemini::{ClientConfig, GeminiModel};
use gadk::genai::{Content, ROLE_USER};
use gadk::model::Backend;
use gadk::platform::Platform;
use gadk::prompts::{FileStore, Label, NewVersion, PromptRegistry, PromptStore, Rollout, Selector};
use gadk::session::InMemorySessionService;

const APP_NAME: &str = "gadk-rollout-e2e";
const PROMPT: &str = "rollout-assistant";
const USER: &str = "rollout-user";
const TEMPLATES: [&str; 2] = [
    "You are a concise assistant. Answer in one or two sentences.",
    "You are a concise assistant. Answer in one sentence, then add one related fact.",
];

/// Creates versions 1 and 2 of `PROMPT` unless they exist, points `production` at version 1 and
/// rolls version 2 out to 50% of sessions.
async fn ensure_rollout(store: &dyn PromptStore) -> anyhow::Result<()> {
    let latest = store.load_all().await?.latest_version(PROMPT).unwrap_or(0);
    for (i, template) in TEMPLATES.iter().enumerate().skip(latest as usize) {
        store
            .create_version(NewVersion {
                name: PROMPT.to_string(),
                template: (*template).to_string(),
                config: None,
                author: "rollout_e2e".to_string(),
                message: format!("rollout check prompt, version {}", i + 1),
            })
            .await?;
    }
    // Moving the label clears its rollout, so start the rollout after it.
    store
        .set_label(PROMPT, &Label::production(), 1, None)
        .await?;
    store
        .set_rollout(PROMPT, &Label::production(), Some(Rollout::new(2, 50)?))
        .await?;
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let project =
        std::env::var("GOOGLE_CLOUD_PROJECT").context("GOOGLE_CLOUD_PROJECT is not set")?;
    let dataset =
        std::env::var("GADK_ANALYTICS_DATASET").context("GADK_ANALYTICS_DATASET is not set")?;
    let table = std::env::var("GADK_ANALYTICS_TABLE").unwrap_or_else(|_| "agent_events".into());
    let prompts_dir =
        std::env::var("GADK_ROLLOUT_PROMPTS").context("GADK_ROLLOUT_PROMPTS is not set")?;
    let sessions: usize = match std::env::var("GADK_ROLLOUT_SESSIONS") {
        Ok(n) => n.parse().context("GADK_ROLLOUT_SESSIONS is not a number")?,
        Err(_) => 8,
    };

    let store: Arc<dyn PromptStore> = Arc::new(FileStore::new(prompts_dir.clone()));
    ensure_rollout(store.as_ref()).await?;
    let registry = PromptRegistry::builder(store).load().await?;
    let selector = Selector::Label(Label::production());
    let instruction = registry
        .instruction(PROMPT, selector.clone())
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
        name: "rollout_assistant".to_string(),
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

    let run = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let mut served: BTreeMap<u32, usize> = BTreeMap::new();
    for i in 0..sessions {
        let session = format!("rollout-{run}-{i}");
        let mut events = runner.run(
            USER,
            &session,
            Some(Content::from_text(
                "Name one large city in Europe.",
                ROLE_USER,
            )),
            RunConfig::default(),
            RunOptions::default(),
        );
        while let Some(event) = events.next().await {
            event?;
        }
        // The instruction is rendered with the session id as the bucket key; `get_for` applies
        // the same rule to the same loaded snapshot and rollouts, so it names the served version.
        let version = registry.get_for(PROMPT, &selector, &session)?.version;
        println!("session {session} -> v{version}");
        *served.entry(version).or_default() += 1;
    }

    // Closing the runner closes the plugin, which drains the queued rows.
    runner.close().await?;
    registry.close();
    let counts: Vec<String> = served.iter().map(|(v, n)| format!("v{v}={n}")).collect();
    println!("served: {}", counts.join(" "));
    println!("store {prompts_dir}");
    Ok(())
}
