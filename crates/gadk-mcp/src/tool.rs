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

//! One MCP server tool as a gadk tool (port of ADK-Go `tool/mcptoolset/tool.go` `convertTool`,
//! `mcpTool`).

use std::any::Any;
use std::sync::Arc;

use gadk_agent::Context;
use gadk_agent::ToolError;
use gadk_agent::tool::{ConfirmationProvider, RunnableTool, Tool};
use gadk_genai::FunctionDeclaration;
use serde_json::{Map, Value};

use crate::client::McpClient;
use crate::config::MetadataProvider;

/// Go `mcpTool`: an MCP server tool. Its declaration carries the server's input and output
/// schemas unchanged; running it calls `tools/call` on the toolset's session.
pub struct McpTool {}

#[allow(unused_variables, dead_code, clippy::todo)]
impl McpTool {
    /// Go `convertTool`.
    pub(crate) fn new(
        tool: rmcp::model::Tool,
        client: Arc<dyn McpClient>,
        require_confirmation: bool,
        provider: Option<ConfirmationProvider>,
        metadata: Option<MetadataProvider>,
    ) -> Self {
        todo!("Task 5")
    }
}

#[async_trait::async_trait]
#[allow(unused_variables, clippy::todo)]
impl Tool for McpTool {
    fn name(&self) -> &str {
        todo!("Task 5")
    }

    fn description(&self) -> &str {
        todo!("Task 5")
    }

    /// The declaration: name, description, `parameters_json_schema` = the MCP `inputSchema`,
    /// `response_json_schema` = the MCP `outputSchema` when present.
    fn declaration(&self) -> Option<FunctionDeclaration> {
        todo!("Task 5")
    }

    fn as_runnable(&self) -> Option<&dyn RunnableTool> {
        todo!("Task 5")
    }

    fn as_any(&self) -> &dyn Any {
        todo!("Task 5")
    }
}

#[async_trait::async_trait]
#[allow(unused_variables, clippy::todo)]
impl RunnableTool for McpTool {
    /// Go `mcpTool.Run`: confirmation, metadata, `tools/call`, result shaping.
    async fn run(&self, ctx: Context, args: Value) -> Result<Map<String, Value>, ToolError> {
        todo!("Task 5")
    }
}

#[cfg(test)]
#[path = "tool_tests.rs"]
mod tests;
