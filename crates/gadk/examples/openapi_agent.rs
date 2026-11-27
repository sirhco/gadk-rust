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

//! An agent that answers weather questions with the OpenAPI toolset over a small Open-Meteo
//! forecast spec (one read-only `GET`, no API key).
//!
//! `GOOGLE_CLOUD_PROJECT=<project> cargo run -p gadk --example openapi_agent \
//!   --features gemini,openapi`
//!
//! Uses Application Default Credentials for Gemini on Vertex AI (location `us`).

use std::sync::Arc;

use futures::StreamExt;
use gadk::agent::RunConfig;
use gadk::engine::llm::LlmAgentConfig;
use gadk::engine::runner::{RunOptions, Runner};
use gadk::gemini::{ClientConfig, GeminiModel};
use gadk::genai::{Content, ROLE_USER};
use gadk::model::Backend;
use gadk::openapi::{OpenApiConfig, OpenApiSpec, OpenApiToolset};
use serde_json::Value;

/// The part of the Open-Meteo forecast API this example uses.
const OPEN_METEO: &str = r#"
openapi: 3.0.3
info:
  title: Open-Meteo forecast
  version: "1.0"
servers:
  - url: https://api.open-meteo.com
paths:
  /v1/forecast:
    get:
      operationId: getForecast
      summary: Weather forecast for one coordinate
      description: Current conditions and a daily forecast for a latitude and longitude.
      parameters:
        - name: latitude
          in: query
          required: true
          description: Latitude in degrees (WGS84).
          schema:
            type: number
        - name: longitude
          in: query
          required: true
          description: Longitude in degrees (WGS84).
          schema:
            type: number
        - name: current
          in: query
          description: >-
            Comma-separated current variables, for example
            temperature_2m,wind_speed_10m,weather_code.
          schema:
            type: string
        - name: daily
          in: query
          description: >-
            Comma-separated daily variables, for example
            temperature_2m_max,temperature_2m_min,precipitation_sum.
          schema:
            type: string
        - name: timezone
          in: query
          description: Time zone of the daily values; "auto" uses the coordinate's own.
          schema:
            type: string
        - name: forecast_days
          in: query
          description: Number of forecast days, 1 to 16.
          schema:
            type: integer
      responses:
        200:
          description: The forecast.
"#;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let project = std::env::var("GOOGLE_CLOUD_PROJECT")?;
    let model = GeminiModel::new(
        "gemini-3.8-flash",
        ClientConfig {
            backend: Some(Backend::VertexAi),
            project: Some(project),
            location: Some("us".to_string()),
            ..Default::default()
        },
    )
    .await?;

    let spec: OpenApiSpec = OPEN_METEO.parse()?;
    let weather = Arc::new(OpenApiToolset::new(&spec, OpenApiConfig::new())?);
    let agent = gadk::engine::llm::agent::new(LlmAgentConfig {
        name: "openapi_agent".to_string(),
        model: Some(model),
        description: "Answers weather questions with the Open-Meteo API.".to_string(),
        instruction: "Answer weather questions. Work out the latitude and longitude of the place \
                      asked about, then call get_forecast with timezone auto and the current or \
                      daily variables you need. Give temperatures in degrees Celsius."
            .to_string(),
        toolsets: vec![weather],
        ..Default::default()
    })?;

    let runner = Runner::in_memory("openapi_agent_example", agent)?;
    let msg = Content::from_text("Will it rain in Lisbon in the next three days?", ROLE_USER);
    let mut events = runner.run(
        "user",
        "session-1",
        Some(msg),
        RunConfig::default(),
        RunOptions::default(),
    );
    while let Some(event) = events.next().await {
        let event = event?;
        let parts = event.response.content.iter().flat_map(|c| c.parts.iter());
        for part in parts {
            if let Some(call) = &part.function_call {
                let args = Value::Object(call.args.clone());
                println!("[{}] tool call: {}({args})", event.author, call.name);
            } else if let Some(resp) = &part.function_response {
                let result = Value::Object(resp.response.clone());
                println!("[{}] tool result: {} -> {result}", event.author, resp.name);
            } else if !part.thought && !part.text.is_empty() {
                println!("[{}] {}", event.author, part.text);
            }
        }
    }
    runner.close().await?;
    Ok(())
}
