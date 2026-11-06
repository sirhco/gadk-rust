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
pub use gadk_artifact as artifact;
pub use gadk_engine as engine;
#[cfg(feature = "gemini")]
pub use gadk_gemini as gemini;
pub use gadk_genai as genai;
pub use gadk_memory as memory;
pub use gadk_model as model;
pub use gadk_platform as platform;
pub use gadk_session as session;

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
