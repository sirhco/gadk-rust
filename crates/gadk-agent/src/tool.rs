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

//! Tool traits (Go `tool/tool.go` interfaces and the optional `internal/toolinternal/tool.go`
//! capabilities).

use std::any::Any;
use std::sync::Arc;

use futures::stream::BoxStream;
use gadk_genai::FunctionDeclaration;
use gadk_model::LlmRequest;
use serde_json::{Map, Value};

use crate::agent::Agent;
use crate::context::{Context, ReadonlyContext};
use crate::error::ToolError;

/// Go `tool.Tool`, plus the optional `toolinternal` capabilities as defaulted methods.
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
    /// The function declaration sent to the model (`toolinternal.FunctionTool.Declaration`).
    fn declaration(&self) -> Option<FunctionDeclaration> {
        None
    }
    /// Extra request edits only (`toolinternal.RequestProcessor`). The flow packs a tool that has
    /// a [`declaration`](Tool::declaration) with `pack_tool` before this runs; a tool without one
    /// is never packed (Go's flow packs nothing, and Go tools without a declaration never pack
    /// themselves). Async because implementations may do I/O (artifact listing, memory search).
    async fn process_request(
        &self,
        _ctx: &Context,
        _req: &mut LlmRequest,
    ) -> Result<(), ToolError> {
        Ok(())
    }
    /// The runnable capability (`toolinternal.FunctionTool.Run`).
    fn as_runnable(&self) -> Option<&dyn RunnableTool> {
        None
    }
    /// The streaming capability (`toolinternal.StreamingFunctionTool.RunStream`).
    fn as_streaming(&self) -> Option<&dyn StreamingTool> {
        None
    }
    /// `toolinternal.ResponseDeferrer`.
    fn defers_response(&self) -> bool {
        false
    }
    /// `toolinternal.SkipSummarizationResultDisplayer`.
    fn display_result_on_skip_summarization(&self) -> bool {
        false
    }
    /// `finish_task`'s `WrapperKey`.
    fn wrapper_key(&self) -> Option<&str> {
        None
    }
    /// The tool this one wraps, for a confirmation or delegating wrapper (Go's analytics plugin
    /// reaches it through an `Unwrap() tool.Tool` method or the wrapper's inner field,
    /// `plugin/agentanalytics/events.go` `unwrapToolValue`). `None` for a tool that wraps nothing.
    fn unwrap(&self) -> Option<Arc<dyn Tool>> {
        None
    }
    /// The kind of tool this is, for analytics (Go classifies a tool by its package and type
    /// name, `events.go` `toolOriginOf`/`classifyADKToolType`; gadk tools declare it). A wrapper
    /// keeps the default and is classified through [`unwrap`](Tool::unwrap).
    fn origin_kind(&self) -> ToolOriginKind {
        ToolOriginKind::Unknown
    }
    /// The agent a [`ToolOriginKind::SubAgent`] tool runs (Go reads the tool's `agent` field,
    /// `events.go` `subAgentOrA2A`, to tell a sub-agent from a remote A2A agent).
    fn origin_agent(&self) -> Option<Arc<dyn Agent>> {
        None
    }
    /// Downcasting support.
    fn as_any(&self) -> &dyn Any;
}

/// What kind of tool a [`Tool`] is (Go's `toolOrigin*` constants, `plugin/agentanalytics/events.go`
/// :39-49, as `classifyADKToolType` :152-187 assigns them). Go's `TRANSFER_A2A` and `A2A` are not
/// kinds: the plugin derives them from [`ToolOriginKind::TransferAgent`] and
/// [`ToolOriginKind::SubAgent`] by looking at the target agent.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum ToolOriginKind {
    /// `toolOriginUnknown` (`"UNKNOWN"`): the tool does not say (the default).
    #[default]
    Unknown,
    /// `toolOriginLocal` (`"LOCAL"`): a function tool or the load-memory tool.
    Local,
    /// `toolOriginMCP` (`"MCP"`): a tool of an MCP toolset.
    Mcp,
    /// `toolOriginTransferAgent` (`"TRANSFER_AGENT"`; `"TRANSFER_A2A"` when the target is a remote
    /// A2A agent): the transfer-to-agent tool.
    TransferAgent,
    /// `toolOriginSubAgent` (`"SUB_AGENT"`; `"A2A"` when [`Tool::origin_agent`] is a remote A2A
    /// agent, Go `subAgentOrA2A`): an agent tool, task-agent tool or single-turn tool.
    SubAgent,
    /// `toolOriginSkill` (`"SKILL"`): a skill tool.
    Skill,
    /// `toolOriginToolbox` (`"TOOLBOX"`): a toolbox tool.
    Toolbox,
}

/// A tool that runs to completion (Go `toolinternal.FunctionTool.Run`).
#[async_trait::async_trait]
pub trait RunnableTool: Send + Sync {
    /// Runs the tool with `args`.
    async fn run(&self, ctx: Context, args: Value) -> Result<Map<String, Value>, ToolError>;
}

/// A tool that streams partial results (Go `RunStream`: `iter.Seq2[string, error]`).
pub trait StreamingTool: Send + Sync {
    /// Runs the tool with `args`, streaming its output.
    fn run_stream(
        &self,
        ctx: Context,
        args: Value,
    ) -> BoxStream<'static, Result<String, ToolError>>;
}

/// Go `tool.Toolset`: a collection of tools provided to an agent.
#[async_trait::async_trait]
pub trait Toolset: Send + Sync {
    /// The name of the toolset.
    fn name(&self) -> &str;
    /// The tools in the toolset; `ctx` can be used to decide which tools to return.
    async fn tools(&self, ctx: &ReadonlyContext) -> Result<Vec<Arc<dyn Tool>>, ToolError>;
    /// Extra request edits (`toolinternal.RequestProcessor` on toolsets).
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

/// Go `tool.Predicate`: decides whether a tool is exposed to the LLM.
pub type ToolPredicate = Arc<dyn Fn(&ReadonlyContext, &dyn Tool) -> bool + Send + Sync>;

/// Decides whether a call to the named tool with the given arguments requires confirmation.
pub type ConfirmationProvider = Arc<dyn Fn(&str, &Value) -> bool + Send + Sync>;

pub use crate::tool_util::{pack_tool, tool_from_request};
