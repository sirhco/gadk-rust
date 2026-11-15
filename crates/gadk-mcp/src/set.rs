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

use gadk_agent::tool::{ConfirmationProvider, Tool, ToolPredicate, Toolset};
use gadk_agent::{ReadonlyContext, ToolError};

use crate::client::{CallContext, McpClient, SessionClient};
use crate::config::{Config, MetadataProvider};
use crate::error::Error;
use crate::tool::McpTool;

/// Go `set`: the tools of one MCP server as a [`Toolset`] named `mcp_tool_set`. Build it with
/// [`new`](crate::new); close it with [`close`](McpToolset::close).
pub struct McpToolset {
    client: Arc<dyn SessionClient>,
    prefix: String,
    tool_filter: Option<ToolPredicate>,
    require_confirmation: bool,
    require_confirmation_provider: Option<ConfirmationProvider>,
    metadata_provider: Option<MetadataProvider>,
}

impl McpToolset {
    /// The toolset over `client`, with `cfg`'s filter, confirmation and metadata settings.
    pub(crate) fn from_parts(client: Arc<dyn SessionClient>, cfg: &Config) -> Self {
        Self {
            client,
            prefix: cfg.prefix.clone(),
            tool_filter: cfg.tool_filter.clone(),
            require_confirmation: cfg.require_confirmation,
            require_confirmation_provider: cfg.require_confirmation_provider.clone(),
            metadata_provider: cfg.metadata_provider.clone(),
        }
    }

    /// Go `Description()`: `Connects to a MCP Server, retrieves MCP Tools into ADK Tools.`
    pub fn description(&self) -> &str {
        "Connects to a MCP Server, retrieves MCP Tools into ADK Tools."
    }

    /// Go `Close`: waits for connection setup in progress, closes every MCP session, and makes
    /// later operations fail with [`Error::ConnectionClosed`]. Idempotent and safe to call
    /// concurrently; every call returns the first call's result.
    pub async fn close(&self) -> Result<(), Arc<Error>> {
        self.client.close().await
    }

    /// The credential session pool; panics for a toolset built without `auth` (as Go's type
    /// assertion in export_test.go does).
    #[cfg(test)]
    fn credential_sessions(&self) -> &crate::sessions::CredentialSessions {
        match self.client.as_credential_sessions() {
            Some(pool) => pool,
            None => panic!("the toolset was not built with Config.auth"),
        }
    }

    /// Go `SetSessionBoundsForTest` (export_test.go) for a toolset built with `auth`.
    #[cfg(test)]
    pub(crate) fn set_session_bounds_for_test(&self, idle: Duration, max: usize) {
        self.credential_sessions().set_bounds(idle, max);
    }

    /// Go `SessionCountForTest` (export_test.go) for a toolset built with `auth`.
    #[cfg(test)]
    pub(crate) fn session_count_for_test(&self) -> usize {
        self.credential_sessions().len()
    }
}

#[async_trait::async_trait]
impl Toolset for McpToolset {
    /// `mcp_tool_set`.
    fn name(&self) -> &str {
        "mcp_tool_set"
    }

    /// Go `Tools`: every server tool (all pages, server order) as an [`McpTool`](crate::McpTool),
    /// filtered by `tool_filter`, then renamed `<prefix>_<name>` when `prefix` is set. Listing stops with [`Error::Cancelled`] when `ctx`'s
    /// invocation is cancelled; a listing error reaches the caller as
    /// [`ToolError::Execution`] with the crate [`Error`] as its source.
    ///
    /// Go's `convertTool` can fail (`failed to convert MCP tool %q to adk tool: …`, which
    /// [`Error::ConvertTool`] spells), but only for a nil tool; rmcp's listing never yields one,
    /// so conversion here is infallible and `ConvertTool` is never produced.
    async fn tools(&self, ctx: &ReadonlyContext) -> Result<Vec<Arc<dyn Tool>>, ToolError> {
        let mcp_tools = self
            .client
            .list_tools(&CallContext::from(ctx))
            .await
            .map_err(|err| ToolError::Execution(Box::new(err)))?;
        let client: Arc<dyn McpClient> = self.client.clone();
        let mut adk_tools: Vec<Arc<dyn Tool>> = Vec::with_capacity(mcp_tools.len());
        for mcp_tool in mcp_tools {
            let mut tool = McpTool::new(
                mcp_tool,
                client.clone(),
                self.require_confirmation,
                self.require_confirmation_provider.clone(),
                self.metadata_provider.clone(),
            );
            // Go filters the converted tool, so the predicate sees the ADK tool, under its
            // server name (the prefix is applied after the filter).
            if let Some(filter) = &self.tool_filter
                && !filter(ctx, &tool)
            {
                continue;
            }
            if !self.prefix.is_empty() {
                tool.expose_as(format!("{}_{}", self.prefix, tool.original_name()));
            }
            adk_tools.push(Arc::new(tool));
        }
        Ok(adk_tools)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
#[path = "set_tests.rs"]
mod tests;
