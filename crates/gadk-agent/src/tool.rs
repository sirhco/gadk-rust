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

//! Tool traits and the optional tool capabilities.

use std::any::Any;
use std::sync::Arc;

use futures::stream::BoxStream;
use gadk_genai::FunctionDeclaration;
use gadk_model::LlmRequest;
use serde_json::{Map, Value};

use crate::agent::Agent;
use crate::context::{Context, ReadonlyContext};
use crate::error::ToolError;

/// A tool, plus the optional capabilities as defaulted methods.
#[async_trait::async_trait]
pub trait Tool: Send + Sync + 'static {
    /// The name of the tool.
    fn name(&self) -> &str;
    /// A description of the tool.
    fn description(&self) -> &str;
    /// Whether the tool is a long-running operation, which typically returns a resource id first
    /// and finishes the operation later.
    fn is_long_running(&self) -> bool {
        false
    }
    /// The function declaration sent to the model.
    fn declaration(&self) -> Option<FunctionDeclaration> {
        None
    }
    /// Extra request edits only. The flow packs a tool that has
    /// a [`declaration`](Tool::declaration) with `pack_tool` before this runs; a tool without one
    /// is never packed. Async because implementations may do I/O (artifact listing, memory search).
    async fn process_request(
        &self,
        _ctx: &Context,
        _req: &mut LlmRequest,
    ) -> Result<(), ToolError> {
        Ok(())
    }
    /// The runnable capability.
    fn as_runnable(&self) -> Option<&dyn RunnableTool> {
        None
    }
    /// The streaming capability.
    fn as_streaming(&self) -> Option<&dyn StreamingTool> {
        None
    }
    /// Whether the tool defers its response: an empty result is then not sent to the model.
    fn defers_response(&self) -> bool {
        false
    }
    /// Whether the result is also shown as a text part when the call ends the agent loop with
    /// `skip_summarization`.
    fn display_result_on_skip_summarization(&self) -> bool {
        false
    }
    /// The `finish_task` wrapper key.
    fn wrapper_key(&self) -> Option<&str> {
        None
    }
    /// The tool this one wraps, for a confirmation or delegating wrapper (the agent-analytics
    /// plugin reaches the wrapped tool through it). `None` for a tool that wraps nothing.
    fn unwrap(&self) -> Option<Arc<dyn Tool>> {
        None
    }
    /// The kind of tool this is, for analytics (gadk tools declare it). A wrapper
    /// keeps the default and is classified through [`unwrap`](Tool::unwrap).
    fn origin_kind(&self) -> ToolOriginKind {
        ToolOriginKind::Unknown
    }
    /// The agent a [`ToolOriginKind::SubAgent`] tool runs (this tells a sub-agent from a remote
    /// A2A agent).
    fn origin_agent(&self) -> Option<Arc<dyn Agent>> {
        None
    }
    /// Every agent the tool can hand a call to, in order, for analytics
    /// (e.g. a transfer tool's supported agents). Defaults to
    /// [`origin_agent`](Tool::origin_agent) as a list of at most one.
    fn origin_agents(&self) -> Vec<Arc<dyn Agent>> {
        self.origin_agent().into_iter().collect()
    }
    /// Downcasting support.
    fn as_any(&self) -> &dyn Any;
}

/// What kind of tool a [`Tool`] is, as the agent-analytics plugin reports it. The `TRANSFER_A2A`
/// and `A2A` origins are not kinds: the plugin derives them from [`ToolOriginKind::TransferAgent`]
/// and [`ToolOriginKind::SubAgent`] by looking at the target agent.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
#[non_exhaustive]
pub enum ToolOriginKind {
    /// `"UNKNOWN"`: the tool does not say (the default).
    #[default]
    Unknown,
    /// `"LOCAL"`: a function tool or the load-memory tool.
    Local,
    /// `"MCP"`: a tool of an MCP toolset.
    Mcp,
    /// `"TRANSFER_AGENT"` (`"TRANSFER_A2A"` when the target is a remote
    /// A2A agent): the transfer-to-agent tool.
    TransferAgent,
    /// `"SUB_AGENT"` (`"A2A"` when [`Tool::origin_agent`] is a remote A2A
    /// agent): an agent tool, task-agent tool or single-turn tool.
    SubAgent,
    /// `"SKILL"`: a skill tool.
    Skill,
    /// `"TOOLBOX"`: a toolbox tool.
    Toolbox,
}

/// A tool that runs to completion.
#[async_trait::async_trait]
pub trait RunnableTool: Send + Sync {
    /// Runs the tool with `args`.
    async fn run(&self, ctx: Context, args: Value) -> Result<Map<String, Value>, ToolError>;
}

/// A tool that streams partial results.
pub trait StreamingTool: Send + Sync {
    /// Runs the tool with `args`, streaming its output.
    fn run_stream(
        &self,
        ctx: Context,
        args: Value,
    ) -> BoxStream<'static, Result<String, ToolError>>;
}

/// A collection of tools provided to an agent.
#[async_trait::async_trait]
pub trait Toolset: Send + Sync {
    /// The name of the toolset.
    fn name(&self) -> &str;
    /// The tools in the toolset; `ctx` can be used to decide which tools to return.
    async fn tools(&self, ctx: &ReadonlyContext) -> Result<Vec<Arc<dyn Tool>>, ToolError>;
    /// Extra request edits for the toolset.
    async fn process_request(
        &self,
        _ctx: &Context,
        _req: &mut LlmRequest,
    ) -> Result<(), ToolError> {
        Ok(())
    }
    /// Downcasting support.
    fn as_any(&self) -> &dyn Any;
}

/// Decides whether a tool is exposed to the LLM.
pub type ToolPredicate = Arc<dyn Fn(&ReadonlyContext, &dyn Tool) -> bool + Send + Sync>;

/// Decides whether a call to the named tool with the given arguments requires confirmation.
pub type ConfirmationProvider = Arc<dyn Fn(&str, &Value) -> bool + Send + Sync>;

pub use crate::tool_util::{pack_tool, tool_from_request};
