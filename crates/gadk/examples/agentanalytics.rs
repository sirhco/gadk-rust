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

//! An agent with a function tool whose runs are recorded in BigQuery by the Agent Analytics
//! plugin. It runs a few scripted conversations, each in its own session, then closes the
//! runner, which drains the queued rows. The plugin creates (or upgrades) the events table and
//! its per-event-type views; the dataset must exist.
//!
//! `GOOGLE_CLOUD_PROJECT=<project> GADK_ANALYTICS_DATASET=<dataset> \
//!   cargo run -p gadk --features gemini,agentanalytics --example agentanalytics`
//!
//! Uses Application Default Credentials for Gemini on Vertex AI (location `us`) and BigQuery.
//! `GADK_ANALYTICS_TABLE` overrides the table (default `agent_events`).

use std::sync::Arc;

use futures::StreamExt;
use gadk::agent::{Context, RunConfig, Tool, ToolError};
use gadk::agentanalytics::{
    ClientOptions, Config, as_bigquery_agent_analytics_plugin,
    new_bigquery_agent_analytics_plugin_with_config,
};
use gadk::engine::llm::LlmAgentConfig;
use gadk::engine::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use gadk::engine::tool::{FunctionTool, FunctionToolConfig};
use gadk::gemini::{ClientConfig, GeminiModel};
use gadk::genai::{Content, ROLE_USER};
use gadk::model::Backend;
use gadk::platform::Platform;
use gadk::session::InMemorySessionService;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

const APP_NAME: &str = "agentanalytics_example";

// The input of the `order_status` tool.
#[derive(Deserialize, JsonSchema)]
struct OrderStatusInput {
    // The order ID, like O000123.
    order_id: String,
}

// The output of the `order_status` tool.
#[derive(Serialize, JsonSchema)]
struct OrderStatusOutput {
    order_id: String,
    status: String,
    eta_days: u32,
}

/// The `order_status` tool: a canned status derived from the order number.
async fn order_status(
    _ctx: Context,
    input: OrderStatusInput,
) -> Result<OrderStatusOutput, ToolError> {
    let n: u32 = input
        .order_id
        .trim_start_matches(|c: char| !c.is_ascii_digit())
        .parse()
        .unwrap_or(0);
    let (status, eta_days) = match n % 3 {
        0 => ("delivered", 0),
        1 => ("shipped", 2),
        _ => ("processing", 5),
    };
    Ok(OrderStatusOutput {
        order_id: input.order_id,
        status: status.to_string(),
        eta_days,
    })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let project = std::env::var("GOOGLE_CLOUD_PROJECT")?;
    let dataset = std::env::var("GADK_ANALYTICS_DATASET")?;
    let table = std::env::var("GADK_ANALYTICS_TABLE").unwrap_or_else(|_| "agent_events".into());

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
    let tool: Arc<dyn Tool> = FunctionTool::new(
        FunctionToolConfig::new("order_status", "Looks up the status of an order by its ID."),
        order_status,
    )?;
    let agent = gadk::engine::llm::agent::new(LlmAgentConfig {
        name: "support_agent".to_string(),
        model: Some(model),
        description: "Answers customer questions about orders.".to_string(),
        instruction: "You are a concise customer-support agent. Use order_status for questions \
                      about a specific order; answer other questions briefly."
            .to_string(),
        tools: vec![tool],
        ..Default::default()
    })?;

    // Application Default Credentials with the `cloud-platform` scope, which also covers
    // BigQuery: user credentials from `gcloud auth application-default login` are granted only
    // that scope and fail a refresh that asks for the narrower BigQuery scopes.
    let credentials = gadk::model::gcp_auth::adc_credentials(
        std::env::var("GOOGLE_APPLICATION_CREDENTIALS")
            .ok()
            .as_deref(),
        &["https://www.googleapis.com/auth/cloud-platform"],
    )?;
    let plugin = new_bigquery_agent_analytics_plugin_with_config(
        &gadk::agentanalytics::Context::background(),
        Config {
            project_id: project,
            dataset_id: dataset,
            table_name: table,
            client_options: ClientOptions {
                credentials: Some(credentials),
                ..ClientOptions::default()
            },
            ..Config::default()
        },
    )
    .await?;
    // The plugin starts the per-event-type views in the background; a run this short closes the
    // plugin before they all exist, so create them now.
    if let Some(analytics) = as_bigquery_agent_analytics_plugin(&plugin) {
        analytics
            .sync_views(&gadk::agentanalytics::Context::background())
            .await?;
    }
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

    let conversations: [(&str, &str, &[&str]); 4] = [
        (
            "user_ava",
            "session_1",
            &["Where is my order O000123?", "Thanks! And O000124?"],
        ),
        (
            "user_liam",
            "session_2",
            &["What's your return policy, in one sentence?"],
        ),
        ("user_mia", "session_3", &["Has order O000500 shipped yet?"]),
        (
            "user_omar",
            "session_4",
            &["Hi!", "Can you check O001001 and O001002 for me?"],
        ),
    ];
    for (user, session, turns) in conversations {
        for turn in turns {
            println!("[{session}] {user} -> {turn}");
            let mut events = runner.run(
                user,
                session,
                Some(Content::from_text(*turn, ROLE_USER)),
                RunConfig::default(),
                RunOptions::default(),
            );
            while let Some(event) = events.next().await {
                let Some(content) = event?.response.content else {
                    continue;
                };
                for part in content.parts {
                    if !part.text.is_empty() && !part.thought {
                        println!("[{session}] agent -> {}", part.text.trim());
                    }
                }
            }
        }
    }
    // Closing the runner closes the plugin, which drains the queued rows.
    runner.close().await?;
    Ok(())
}
