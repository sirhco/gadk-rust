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

//! gadk: Agent Development Kit for Rust.
//!
//! The member crates are re-exported as modules; [`prelude`] gathers the common types for
//! building and running agents.

/// The Agent2Agent (A2A) protocol: serve a gadk agent over A2A (`a2a::server`) and use a remote A2A
/// agent as a gadk agent (`a2a::remoteagent`), wire-compatible with ADK's A2A agents (feature
/// `a2a`, not on by default; re-export of `gadk-a2a`).
#[cfg(feature = "a2a")]
pub use gadk_a2a as a2a;
pub use gadk_agent as agent;
/// The BigQuery Agent Analytics plugin: the plugin
/// (`new_bigquery_agent_analytics_plugin*`, a [`agent::Plugin`] for a runner's plugin list,
/// `as_bigquery_agent_analytics_plugin` for its drop stats and view sync), its configuration,
/// the events table's BigQuery and Arrow schemas, table creation and additive schema upgrade, the
/// analytics views, the BigQuery Storage Write client, the batch processor and the GCS content
/// offloader (feature `agentanalytics`, not on by default; re-export of `gadk-agentanalytics`).
#[cfg(feature = "agentanalytics")]
pub use gadk_agentanalytics as agentanalytics;
/// The Agent Engine server and launcher: serve an agent with the Vertex AI Agent Engine
/// reasoning-engine API (`/reasoning_engine`, `/stream_reasoning_engine`), wire-compatible with
/// ADK's Agent Engine server and launcher (feature `agentengine`, not on by default).
#[cfg(feature = "agentengine")]
pub mod agentengine {
    pub use gadk_launcher::agentengine::new_launcher;
    pub use gadk_launcher::web::agentengine::{AgentEngineLauncher, new};
    pub use gadk_server::agentengine::{
        AgentEngineConfig, AgentEngineLog, Error, list_class_methods, new_handler,
    };
}
/// The Agent Registry client: list and resolve registered agents, MCP servers and endpoints,
/// and use them as a remote A2A agent (`remote_agent`, feature `a2a` of the crate) or an MCP
/// toolset (`mcp_toolset`, feature `mcp`). Feature `agentregistry`, not on by default; re-export
/// of `gadk-agentregistry`.
#[cfg(feature = "agentregistry")]
pub use gadk_agentregistry as agentregistry;
/// Claude models on the Anthropic Messages API and on Vertex AI (feature `anthropic`, not on
/// by default; re-export of `gadk-anthropic`).
#[cfg(feature = "anthropic")]
pub use gadk_anthropic as anthropic;
pub use gadk_artifact as artifact;
/// Artifacts in Google Cloud Storage (feature `gcs`, not on by default; re-export of
/// `gadk-artifact-gcs`).
#[cfg(feature = "gcs")]
pub use gadk_artifact_gcs as artifact_gcs;
/// Authentication for outbound HTTP calls: credentials, credential providers, the credential
/// store, the refresh-and-retry transport and (feature `gcp`) per-user Google Cloud credentials
/// (re-export of `gadk-auth`).
pub use gadk_auth as auth;
pub use gadk_engine as engine;
/// Offline agent evaluation: YAML eval sets, deterministic, tool-trajectory and LLM-judge
/// scorers, and per-variant reports (feature `eval`, not on by default; re-export of `gadk-eval`).
#[cfg(feature = "eval")]
pub use gadk_eval as eval;
#[cfg(feature = "gemini")]
pub use gadk_gemini as gemini;
pub use gadk_genai as genai;
/// The launcher: run an agent from the command line in console mode or behind a web server with the
/// REST API, A2A, the ADK web UI and the Pub/Sub and Eventarc triggers, with the command lines,
/// flags and output of ADK's launcher (feature `launcher`, not on by default; re-export of
/// `gadk-launcher`).
#[cfg(feature = "launcher")]
pub use gadk_launcher as launcher;
/// Model Context Protocol tools: the tools of an MCP server as a toolset (feature `mcp`, on by
/// default; re-export of `gadk-mcp`).
#[cfg(feature = "mcp")]
pub use gadk_mcp as mcp;
pub use gadk_memory as memory;
pub use gadk_model as model;
/// OpenAI models on the Responses and Chat Completions APIs (feature `openai`, not on by
/// default; re-export of `gadk-openai`).
#[cfg(feature = "openai")]
pub use gadk_openai as openai;
pub use gadk_platform as platform;
/// The prompt registry: versioned, labelled prompts and instruction providers (feature
/// `prompts`, not on by default; re-export of `gadk-prompts`). The Firestore store is
/// `prompts_firestore`.
///
/// ```no_run
/// # #[cfg(feature = "prompts-firestore")]
/// # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
/// use std::sync::Arc;
/// let store = gadk::prompts_firestore::FirestoreStore::new(gadk::prompts_firestore::FirestoreConfig {
///     project: "my-project".into(),
///     ..Default::default()
/// })?;
/// let registry = gadk::prompts::PromptRegistry::builder(Arc::new(store)).load().await?;
/// # let _ = registry; Ok(()) }
/// ```
#[cfg(feature = "prompts")]
pub use gadk_prompts as prompts;
/// The prompt registry's store in Google Cloud Firestore (feature `prompts-firestore`, not on by
/// default; re-export of `gadk-prompts-firestore`).
#[cfg(feature = "prompts-firestore")]
pub use gadk_prompts_firestore as prompts_firestore;
/// The ADK REST API server (`server::rest`) with inbound authentication (`server::authn`) and
/// authorization (`server::authz`), wire-compatible with ADK REST clients such as the ADK web UI
/// (feature `server`, not on by default; re-export of `gadk-server`).
#[cfg(feature = "server")]
pub use gadk_server as server;
pub use gadk_session as session;
/// Sessions in a relational database: sqlite, or PostgreSQL with the crate's `postgres`
/// feature (feature `database`, not on by default; re-export of `gadk-session-database`).
#[cfg(feature = "database")]
pub use gadk_session_database as session_database;
/// OpenTelemetry export: tracer, logger and meter providers
/// from options and the environment (OTLP/HTTP, export to `telemetry.googleapis.com`, the GCP
/// resource detector) and the `tracing` layer that exports gadk's ADK spans and log records
/// through them (feature `telemetry`, not on by default, enabled by `launcher`; re-export of
/// `gadk-telemetry`).
#[cfg(feature = "telemetry")]
pub use gadk_telemetry as telemetry;
/// Built-in tools that need no authentication: the Gemini built-ins, artifact and memory tools,
/// the example tool, tool search, the skills toolset and the `toolauth` wire model (re-export of
/// `gadk-tools`).
pub use gadk_tools as tools;
/// Sessions and memory in a Vertex AI Agent Engine and an example provider backed by a Vertex AI
/// Example Store (feature `vertexai`, not on by default; re-export of `gadk-vertexai`).
#[cfg(feature = "vertexai")]
pub use gadk_vertexai as vertexai;

/// Context compaction (re-export of `gadk_session::compaction`).
///
/// ```
/// let cfg = gadk::compaction::Config { compaction_interval: 2, overlap_size: 1, ..Default::default() };
/// assert!(cfg.validate().is_ok());
/// ```
pub use gadk_session::compaction;

/// Common types for building and running agents: `use gadk::prelude::*;`.
pub mod prelude {
    pub use gadk_agent::{
        Agent, AgentConfig, AgentError, Context, EventStream, InvocationContext, LiveAgent,
        LiveRequest, LiveRunConfig, LiveSession, Mode, Plugin, ReadonlyContext, RunConfig,
        StreamingMode, Tool, ToolError, Toolset,
    };
    pub use gadk_artifact::{ArtifactService, InMemoryArtifactService};
    pub use gadk_engine::llm::{IncludeContents, InstructionProvider, LlmAgentConfig};
    pub use gadk_engine::runner::{RunOptions, Runner, RunnerConfig};
    pub use gadk_engine::tool::{
        AgentTool, AgentToolConfig, FunctionTool, FunctionToolConfig, StreamingFunctionTool,
        exit_loop,
    };
    pub use gadk_engine::workflow::{
        Edge, EdgeBuilder, Node, NodeConfig, START, Workflow, WorkflowOptions, function_node,
    };
    pub use gadk_engine::{
        llm_agent, loop_agent, parallel_agent, sequential_agent, workflow_agent,
    };
    pub use gadk_genai::{Content, Part};
    pub use gadk_memory::{InMemoryMemoryService, MemoryService};
    pub use gadk_model::{Llm, RealtimeInput};
    pub use gadk_session::{Event, InMemorySessionService, SessionService};
}
