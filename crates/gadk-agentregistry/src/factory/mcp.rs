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

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

//! Go `agentregistry/factory.go`, MCP part: `MCPToolset` and `mcpEndpointURI`.

use std::collections::BTreeMap;

use super::{Egress, EgressConfig};
use crate::config::Client;
use crate::error::Error;
use crate::types::McpServer;

/// Go `MCPToolsetOption`: customizes [`Client::mcp_toolset`]; applied in order.
#[derive(Debug, Clone)]
pub enum McpToolsetOption {
    /// Go `WithMCPHTTPClient`: the HTTP client used to reach the MCP server, overriding the
    /// default (the registry's authentication for `*.googleapis.com`, else a default client).
    HttpClient(reqwest::Client),
    /// Go `WithMCPHeaders`: headers added to (or overwriting) every MCP request; repeated
    /// options accumulate, a later value wins on a key conflict.
    Headers(BTreeMap<String, String>),
}

/// Go `WithMCPHTTPClient`.
pub fn with_mcp_http_client(client: reqwest::Client) -> McpToolsetOption {
    McpToolsetOption::HttpClient(client)
}

/// Go `WithMCPHeaders`.
pub fn with_mcp_headers<K, V>(headers: impl IntoIterator<Item = (K, V)>) -> McpToolsetOption
where
    K: Into<String>,
    V: Into<String>,
{
    McpToolsetOption::Headers(
        headers
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect(),
    )
}

/// Go `applyMCPToolsetOptions`.
pub(crate) fn apply_mcp_toolset_options(opts: &[McpToolsetOption]) -> EgressConfig {
    todo!("Task 5")
}

impl Client {
    /// Go `MCPToolset`: resolves the registered MCP server `name` (full resource name) into a
    /// streamable-HTTP MCP toolset: [`Client::get_mcp_server`], [`mcp_endpoint_uri`] (none →
    /// [`Error::NoEndpoint`]), [`Client::egress`], [`Client::mcp_config`], `gadk_mcp::new`.
    /// Close the toolset as `gadk_mcp::McpToolset::close` describes.
    pub async fn mcp_toolset(
        &self,
        name: &str,
        opts: &[McpToolsetOption],
    ) -> Result<gadk_mcp::McpToolset, Error> {
        todo!("Task 5")
    }

    /// The toolset config for `uri` over `egress`: `McpTransport::StreamableHttp { url, client,
    /// headers }` with the egress client (`Registry` → the requester's client) and headers, and
    /// `auth` = the registry's ADC credential provider when the client is `Registry` on the ADC
    /// path (`None` otherwise).
    pub(crate) fn mcp_config(
        &self,
        uri: String,
        egress: Egress,
    ) -> Result<gadk_mcp::Config, Error> {
        todo!("Task 5")
    }
}

/// Go `mcpEndpointURI`: the server's connection URI preferring the JSONRPC binding, then
/// HTTP_JSON (over its protocols and top-level interfaces).
pub(crate) fn mcp_endpoint_uri(server: &McpServer) -> Option<String> {
    todo!("Task 5")
}
