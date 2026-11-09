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

//! gadk: Agent Development Kit for Rust (port of ADK-Go).
//!
//! The member crates are re-exported as modules; [`prelude`] gathers the common types for
//! building and running agents.

pub use gadk_agent as agent;
/// Claude models on the Anthropic Messages API and on Vertex AI (feature `anthropic`, not on
/// by default; re-export of `gadk-anthropic`).
#[cfg(feature = "anthropic")]
pub use gadk_anthropic as anthropic;
pub use gadk_artifact as artifact;
/// Authentication for outbound HTTP calls: credentials, credential providers, the credential
/// store, the refresh-and-retry transport and (feature `gcp`) per-user Google Cloud credentials
/// (re-export of `gadk-auth`).
pub use gadk_auth as auth;
pub use gadk_engine as engine;
#[cfg(feature = "gemini")]
pub use gadk_gemini as gemini;
pub use gadk_genai as genai;
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
pub use gadk_session as session;
/// Sessions in a relational database: sqlite, or PostgreSQL with the crate's `postgres`
/// feature (feature `database`, not on by default; re-export of `gadk-session-database`).
#[cfg(feature = "database")]
pub use gadk_session_database as session_database;
/// Built-in tools that need no authentication: the Gemini built-ins, artifact and memory tools,
/// the example tool, tool search, the skills toolset and the `toolauth` wire model (re-export of
/// `gadk-tools`).
pub use gadk_tools as tools;

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
