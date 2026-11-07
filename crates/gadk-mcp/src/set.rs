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

//! The MCP toolset (port of ADK-Go `tool/mcptoolset/set.go` `set`).

use std::any::Any;
use std::sync::Arc;
#[cfg(test)]
use std::time::Duration;

use gadk_agent::tool::{Tool, Toolset};
use gadk_agent::{ReadonlyContext, ToolError};

use crate::client::SessionClient;
use crate::config::Config;
use crate::error::Error;

/// Go `set`: the tools of one MCP server as a [`Toolset`] named `mcp_tool_set`. Build it with
/// [`new`](crate::new); close it with [`close`](McpToolset::close).
pub struct McpToolset {}

#[allow(unused_variables, dead_code, clippy::todo)]
impl McpToolset {
    /// The toolset over `client`, with `cfg`'s filter, confirmation and metadata settings.
    pub(crate) fn from_parts(client: Arc<dyn SessionClient>, cfg: &Config) -> Self {
        todo!("Task 9")
    }

    /// Go `Description()`: `Connects to a MCP Server, retrieves MCP Tools into ADK Tools.`
    pub fn description(&self) -> &str {
        todo!("Task 9")
    }

    /// Go `Close`: waits for connection setup in progress, closes every MCP session, and makes
    /// later operations fail with [`Error::ConnectionClosed`]. Idempotent and safe to call
    /// concurrently; every call returns the first call's result.
    pub async fn close(&self) -> Result<(), Arc<Error>> {
        todo!("Task 9")
    }

    /// Go `SetSessionBoundsForTest` (export_test.go) for a toolset built with `auth`.
    #[cfg(test)]
    pub(crate) fn set_session_bounds_for_test(&self, idle: Duration, max: usize) {
        todo!("Task 9")
    }

    /// Go `SessionCountForTest` (export_test.go) for a toolset built with `auth`.
    #[cfg(test)]
    pub(crate) fn session_count_for_test(&self) -> usize {
        todo!("Task 9")
    }
}

#[async_trait::async_trait]
#[allow(unused_variables, clippy::todo)]
impl Toolset for McpToolset {
    /// `mcp_tool_set`.
    fn name(&self) -> &str {
        todo!("Task 9")
    }

    /// Go `Tools`: every server tool (all pages, server order) as an [`McpTool`](crate::McpTool),
    /// filtered by `tool_filter`.
    async fn tools(&self, ctx: &ReadonlyContext) -> Result<Vec<Arc<dyn Tool>>, ToolError> {
        todo!("Task 9")
    }

    fn as_any(&self) -> &dyn Any {
        todo!("Task 9")
    }
}

#[cfg(test)]
#[path = "set_tests.rs"]
mod tests;
