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

//! An agent run through the launcher with telemetry: the resource names the service, and the
//! launcher starts telemetry from `Config::telemetry_options` before it serves.
//!
//! Spans and log records are exported over OTLP/HTTP when `OTEL_EXPORTER_OTLP_ENDPOINT` (or
//! `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` / `…_LOGS_ENDPOINT`) is set, and to
//! `telemetry.googleapis.com` with `-otel_to_cloud` (Application Default Credentials and a
//! project, e.g. `GOOGLE_CLOUD_PROJECT`):
//! `GOOGLE_API_KEY=... OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318 cargo run -p gadk --features launcher --example telemetry`
//! (the console), or `… --example telemetry -- web -otel_to_cloud api webui`.

use std::sync::Arc;

use gadk::agent::{SingleLoader, Tool};
use gadk::engine::llm::LlmAgentConfig;
use gadk::gemini::{ClientConfig, GeminiModel};
use gadk::launcher::golog::Logger;
use gadk::launcher::{CancellationToken, Config, Launcher, full};
use gadk::telemetry::with_resource;
use gadk::tools::gemini::GoogleSearch;
use opentelemetry::KeyValue;
use opentelemetry_sdk::Resource;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let model = GeminiModel::new(
        "gemini-3.8-flash",
        ClientConfig {
            api_key: std::env::var("GOOGLE_API_KEY").ok(),
            ..Default::default()
        },
    )
    .await
    .map_err(|e| anyhow::anyhow!("failed to create model: {e}"))?;

    let google_search: Arc<dyn Tool> = Arc::new(GoogleSearch);
    let agent = gadk::engine::llm::agent::new(LlmAgentConfig {
        name: "weather_time_agent".to_string(),
        model: Some(model),
        description: "Agent to answer questions about the time and weather in a city.".to_string(),
        instruction: "Your SOLE purpose is to answer questions about the current time and weather in a specific city. You MUST refuse to answer any questions unrelated to time or weather.".to_string(),
        tools: vec![google_search],
        ..Default::default()
    })
    .map_err(|e| anyhow::anyhow!("failed to create agent: {e}"))?;

    // The service name and version attributes; no schema URL, so the resource merges with the
    // default resource without a conflict.
    let resource = Resource::builder_empty()
        .with_attributes([
            KeyValue::new("service.name", "weather-time-agent"),
            KeyValue::new("service.version", "1.0.0"),
        ])
        .build();
    let mut config = Config::new(Arc::new(SingleLoader::new(agent)));
    config.telemetry_options = vec![
        with_resource(resource),
        // Other telemetry options can be added here.
    ];

    // The launcher starts the telemetry and shuts it down when it returns.
    let mut l = full::new();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = l.execute(CancellationToken::new(), config, args).await {
        // Print the error and the command-line syntax, then exit.
        Logger::stderr().print(&format!("run failed: {e}\n\n{}", l.command_line_syntax()));
        std::process::exit(1);
    }
    Ok(())
}
