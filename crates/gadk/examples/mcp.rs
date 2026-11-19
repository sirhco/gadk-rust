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

//! The `mcp` example: an agent using the tools of an MCP server. It shows two ways to
//! reach a server; set `AGENT_MODE` to choose (`local` is the default):
//!
//! 1. An in-memory MCP server: `get_weather` is an rmcp tool (`#[tool_router]`), served over a
//!    `tokio::io::duplex` pipe by a [`TransportFactory`] given as [`McpTransport::Custom`].
//! 2. `AGENT_MODE=github`: GitHub's remote MCP server
//!    (<https://github.com/github/github-mcp-server>) over streamable HTTP, authenticated with a
//!    GitHub personal access token from `GITHUB_PAT` as a static bearer token.
//!
//! It runs through the launcher.
//!
//! `GOOGLE_API_KEY=... cargo run -p gadk --features launcher --example mcp [console | web webui
//! api]`, then ask "What is the weather in Paris?"; or `AGENT_MODE=github GITHUB_PAT=...
//! GOOGLE_API_KEY=... cargo run -p gadk --features launcher --example mcp` and ask about your
//! repositories.

use std::sync::Arc;

use gadk::agent::SingleLoader;
use gadk::auth::static_token;
use gadk::engine::llm::LlmAgentConfig;
use gadk::gemini::{ClientConfig, GeminiModel};
use gadk::launcher::golog::Logger;
use gadk::launcher::{CancellationToken, Config as LauncherConfig, Launcher, full};
use gadk::mcp::rmcp::handler::server::wrapper::{Json, Parameters};
use gadk::mcp::rmcp::model::ClientConfig as McpClientConfig;
use gadk::mcp::rmcp::service::RunningService;
use gadk::mcp::rmcp::{RoleClient, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use gadk::mcp::{BoxError, Config, McpToolset, McpTransport, TransportFactory, serve_client};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// The input of `get_weather` (a plain comment: a doc comment would become the schema's
// description).
#[derive(Deserialize, JsonSchema)]
struct Input {
    #[schemars(description = "city name")]
    city: String,
}

// The output of `get_weather`.
#[derive(Serialize, JsonSchema)]
struct Output {
    #[schemars(description = "weather summary in the given city")]
    weather_summary: String,
}

/// The in-memory `weather_server` (`v1.0.0`): one tool, `get_weather`.
#[derive(Clone)]
struct WeatherServer;

#[tool_router]
impl WeatherServer {
    /// Returns a canned weather summary for the city.
    #[tool(
        name = "get_weather",
        description = "returns weather in the given city"
    )]
    async fn get_weather(&self, Parameters(input): Parameters<Input>) -> Json<Output> {
        Json(Output {
            weather_summary: format!("Today in {} is sunny\n", gadk::agent::go_quote(&input.city)),
        })
    }
}

#[tool_handler(name = "weather_server", version = "v1.0.0")]
impl ServerHandler for WeatherServer {}

/// The transport of the in-memory server: every (re)connect serves a fresh [`WeatherServer`] on one
/// end of an in-memory pipe and opens the client session on the other.
struct LocalWeather;

#[async_trait::async_trait]
impl TransportFactory for LocalWeather {
    async fn connect(
        &self,
        client_info: McpClientConfig,
    ) -> Result<RunningService<RoleClient, McpClientConfig>, BoxError> {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        tokio::spawn(async move {
            match WeatherServer.serve(server_io).await {
                Ok(server) => {
                    let _ = server.waiting().await;
                }
                Err(e) => eprintln!("weather server: {e}"),
            }
        });
        serve_client(client_info, client_io).await
    }
}

/// The transport of GitHub's MCP endpoint with the `GITHUB_PAT` bearer token.
fn github_config() -> Config {
    let mut cfg = Config::new(McpTransport::Endpoint(
        "https://api.githubcopilot.com/mcp/".to_string(),
    ));
    cfg.auth = Some(static_token(
        std::env::var("GITHUB_PAT").unwrap_or_default(),
    ));
    cfg
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let model = GeminiModel::new(
        "gemini-2.5-flash",
        ClientConfig {
            api_key: std::env::var("GOOGLE_API_KEY").ok(),
            ..Default::default()
        },
    )
    .await?;

    let github = std::env::var("AGENT_MODE").is_ok_and(|mode| mode.to_lowercase() == "github");
    let cfg = if github {
        github_config()
    } else {
        Config::new(McpTransport::Custom(Arc::new(LocalWeather)))
    };
    let mcp_tool_set: Arc<McpToolset> = Arc::new(gadk::mcp::new(cfg)?);

    // The LLM agent with the MCP tool set.
    let agent = gadk::engine::llm::agent::new(LlmAgentConfig {
        name: "helper_agent".to_string(),
        model: Some(model),
        description: "Helper agent.".to_string(),
        instruction: "You are a helpful assistant that helps users with various tasks.".to_string(),
        toolsets: vec![mcp_tool_set.clone()],
        ..Default::default()
    })?;

    let config = LauncherConfig::new(Arc::new(SingleLoader::new(agent)));
    let mut l = full::new();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = l.execute(CancellationToken::new(), config, args).await;
    // Ends the MCP session (and, in local mode, the in-memory server).
    if let Err(e) = mcp_tool_set.close().await {
        eprintln!("closing the MCP tool set: {e}");
    }
    if let Err(e) = result {
        // Print the error and the command-line syntax, then exit.
        Logger::stderr().print(&format!("Run failed: {e}\n\n{}", l.command_line_syntax()));
        std::process::exit(1);
    }
    Ok(())
}
