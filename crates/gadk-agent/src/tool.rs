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

use crate::context::{Context, ReadonlyContext};
use crate::error::ToolError;

/// Go `tool.Tool`, plus the optional `toolinternal` capabilities as defaulted methods.
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
    /// Extra request edits only (`toolinternal.RequestProcessor`); packing is done by the flow via
    /// `pack_tool` before this runs.
    fn process_request(&self, _ctx: &Context, _req: &mut LlmRequest) -> Result<(), ToolError> {
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
    /// Downcasting support.
    fn as_any(&self) -> &dyn Any;
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
