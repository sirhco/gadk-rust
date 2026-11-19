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

//! The quickstart for an Agent Engine deployment:
//! an agent with a `random` tool and a `search_past_conversations` memory tool, a plugin that
//! records the session's last update time before each run and adds the session to memory after
//! it, served by `agentengine::new_launcher` — the reasoning-engine API (`/api/reasoning_engine`,
//! `/api/stream_reasoning_engine`) Vertex AI Agent Engine calls.
//!
//! Agent Engine provides `GOOGLE_CLOUD_PROJECT`, `GOOGLE_CLOUD_AGENT_ENGINE_LOCATION` and
//! `GOOGLE_CLOUD_AGENT_ENGINE_ID` to the container; to try it locally, set them to your project:
//!
//! `cargo run -p gadk --features agentengine,vertexai --example agentengine -- web agentengine`
//!
//! With the `vertexai` feature the sessions and memories live in the Agent Engine (reached with
//! Application Default Credentials); without it the launcher's in-memory services are used.
//! `… --example agentengine -- -h` prints every command line.

use std::hash::{BuildHasher, RandomState};
use std::sync::Arc;

use futures::FutureExt;
use gadk::agent::{Context, Plugin, PluginConfig, SingleLoader, Tool, ToolError};
use gadk::engine::llm::LlmAgentConfig;
use gadk::engine::runner::PluginConfig as RunnerPluginConfig;
use gadk::engine::tool::{FunctionTool, FunctionToolConfig};
use gadk::gemini::{ClientConfig, GeminiModel};
use gadk::launcher::golog::Logger;
use gadk::launcher::{CancellationToken, Config, Launcher};
use gadk::model::Backend;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The state key the plugin records the session's last update time under, from which the Memory
/// Bank takes the new events.
const STATE_KEY_SESSION_LAST_UPDATE_TIME: &str = "sessionLastUpdateTime";

/// The input of the memory search tool.
#[derive(Deserialize, JsonSchema)]
struct Args {
    /// The query to search for in the memory.
    query: String,
}

/// The output of the memory search tool.
#[derive(Serialize, JsonSchema)]
struct SearchResult {
    results: Vec<String>,
}

/// The texts of the user's memories relevant to the query.
async fn memory_search(ctx: Context, args: Args) -> Result<SearchResult, ToolError> {
    let found = ctx.search_memory(&args.query).await.map_err(|e| {
        Logger::stderr().print(&format!("Error searching memory: {e}"));
        ToolError::Execution(format!("failed memory search: {e}").into())
    })?;
    let results = found
        .memories
        .into_iter()
        .filter_map(|m| m.content)
        .flat_map(|c| c.parts)
        .map(|p| p.text)
        .collect();
    Ok(SearchResult { results })
}

/// The input of the `random` tool.
#[derive(Deserialize, JsonSchema)]
struct Input {
    min: i64,
    max: i64,
}

/// The output of the `random` tool.
#[derive(Serialize, JsonSchema)]
struct Output {
    result: i64,
}

/// A number between `min` and `max`, both included.
async fn random(_ctx: Context, input: Input) -> Result<Output, ToolError> {
    let span = input.max.saturating_sub(input.min).saturating_add(1);
    if span <= 0 {
        return Err(ToolError::InvalidArgument(
            "invalid argument to IntN".to_string(),
        ));
    }
    let r = RandomState::new().hash_one(std::time::SystemTime::now());
    let offset = i64::try_from(r % span.unsigned_abs()).unwrap_or_default();
    Ok(Output {
        result: input.min + offset,
    })
}

/// The plugin that records the session's last update time before each run and adds the session
/// to memory after it.
fn memory_plugin() -> Plugin {
    Plugin::new(PluginConfig {
        name: "Memory generator".to_string(),
        before_run_callback: Some(Arc::new(|ic| {
            async move {
                let session = ic.session();
                let last_update_time = serde_json::to_value(session.last_update_time())
                    .map_err(|e| gadk::agent::AgentError::Other(Box::new(e)))?;
                if let Err(e) = session
                    .state()
                    .set(STATE_KEY_SESSION_LAST_UPDATE_TIME, last_update_time)
                {
                    Logger::stderr().print(&format!("failed to set session state: {e}\n"));
                    return Err(gadk::agent::AgentError::Other(Box::new(e)));
                }
                Ok(None)
            }
            .boxed()
        })),
        after_run_callback: Some(Arc::new(|ic| {
            async move {
                let Some(memory) = ic.memory() else {
                    Logger::stderr().print("no memory service is configured\n");
                    return;
                };
                if let Err(e) = memory.add_session_to_memory(ic.session()).await {
                    Logger::stderr().print(&format!("failed to add session to memory: {e}\n"));
                }
            }
            .boxed()
        })),
        ..Default::default()
    })
}

/// A required setting of the example, read from the environment.
fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_default()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let log = Logger::stderr();
    // Those values are provided by Agent Engine, visible after the deployment to the
    // container; for testing, simply set them to your GCP project.
    let project_id = env("GOOGLE_CLOUD_PROJECT");
    let location = env("GOOGLE_CLOUD_AGENT_ENGINE_LOCATION");
    let agent_engine_id = env("GOOGLE_CLOUD_AGENT_ENGINE_ID");

    let model = GeminiModel::new(
        "gemini-3.8-flash",
        ClientConfig {
            backend: Some(Backend::VertexAi),
            project: Some(project_id.clone()),
            // Newer Gemini models are served from the `us` multi-region (or `global`), not from
            // the Agent Engine's single region.
            location: Some("us".to_string()),
            ..Default::default()
        },
    )
    .await
    .inspect_err(|e| log.print(&format!("Failed to create model: {e}")))?;

    let random_tool: Arc<dyn Tool> = FunctionTool::new(
        FunctionToolConfig::new("random", "Returns a random number between min and max"),
        random,
    )
    .inspect_err(|e| log.print(&format!("Failed to create tool: {e}")))?;
    let memory_search_tool: Arc<dyn Tool> = FunctionTool::new(
        FunctionToolConfig::new(
            "search_past_conversations",
            "Searches past conversations for relevant information.",
        ),
        memory_search,
    )
    .inspect_err(|e| log.print(&format!("Failed to create tool: {e}")))?;

    let agent = gadk::engine::llm::agent::new(LlmAgentConfig {
        name: "ae_agent".to_string(),
        model: Some(model),
        description: "General helpful agent".to_string(),
        instruction: "You are a helpful agent, you should answer any questions you are given. Use 'random' tool to provide random numbers. Use search_past_conversations tool to get the facts about the user".to_string(),
        tools: vec![random_tool, memory_search_tool],
        ..Default::default()
    })
    .inspect_err(|e| log.print(&format!("Failed to create agent: {e}")))?;

    let mut config = Config::new(Arc::new(SingleLoader::new(agent)));
    config.plugin_config = RunnerPluginConfig {
        plugins: vec![memory_plugin()],
        ..Default::default()
    };
    #[cfg(feature = "vertexai")]
    {
        use gadk::vertexai::AgentEngineData;
        use gadk::vertexai::memory::{VertexAiMemoryService, VertexAiMemoryServiceConfig};
        use gadk::vertexai::session::{VertexAiSessionService, VertexAiSessionServiceConfig};

        let sessions = VertexAiSessionService::new(VertexAiSessionServiceConfig {
            project_id: project_id.clone(),
            location: location.clone(),
            reasoning_engine: Some(agent_engine_id.clone()),
            ..Default::default()
        })
        .inspect_err(|e| log.print(&format!("Failed to create session service: {e}")))?;
        let memory = VertexAiMemoryService::new(VertexAiMemoryServiceConfig {
            engine: AgentEngineData {
                project_id,
                location,
                reasoning_engine: agent_engine_id.clone(),
            },
            state_key_session_last_update_time: Some(
                STATE_KEY_SESSION_LAST_UPDATE_TIME.to_string(),
            ),
            ..Default::default()
        })
        .inspect_err(|e| log.print(&format!("Failed to create memory service: {e}")))?;
        config.session_service = Some(Arc::new(sessions));
        config.memory_service = Some(Arc::new(memory));
    }

    let mut l = gadk::agentengine::new_launcher(&agent_engine_id);
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = l.execute(CancellationToken::new(), config, args).await {
        // Print the error and the command-line syntax, then exit.
        log.print(&format!("Run failed: {e}\n\n{}", l.command_line_syntax()));
        std::process::exit(1);
    }
    Ok(())
}
