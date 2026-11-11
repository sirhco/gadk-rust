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

//! Go `agentregistry/factory.go`, MCP part: `MCPToolset` and `mcpEndpointURI`.

use std::collections::BTreeMap;

use http::{HeaderMap, HeaderName, HeaderValue};

use super::{Egress, EgressClient, EgressConfig, add_headers};
use crate::config::Client;
use crate::error::Error;
use crate::resolve::{TRANSPORT_HTTP_JSON, TRANSPORT_JSONRPC, connection_uri};
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
    let mut ec = EgressConfig::default();
    for opt in opts {
        match opt {
            McpToolsetOption::HttpClient(c) => ec.http_client = Some(c.clone()),
            McpToolsetOption::Headers(h) => add_headers(&mut ec, h),
        }
    }
    ec
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
        let server = self.get_mcp_server(name).await?;
        let Some(uri) = mcp_endpoint_uri(&server) else {
            return Err(Error::NoEndpoint(name.to_owned()));
        };
        let ec = apply_mcp_toolset_options(opts);
        let egress = self.egress(&uri, &ec);
        gadk_mcp::new(self.mcp_config(uri, egress)?).map_err(Error::Mcp)
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
        let (client, auth) = match egress.client {
            EgressClient::Option(c) => (c, None),
            EgressClient::Registry => (self.requester.http.clone(), self.requester.auth.clone()),
            EgressClient::Default => (super::default_http_client()?, None),
        };
        let mut headers = HeaderMap::new();
        for (name, value) in &egress.headers {
            // Go's `Header.Set` would send any value; one `http` cannot represent is skipped.
            let (Ok(name), Ok(value)) = (
                HeaderName::from_bytes(name.as_bytes()),
                HeaderValue::from_str(value),
            ) else {
                continue;
            };
            headers.insert(name, value);
        }
        let mut cfg = gadk_mcp::Config::new(gadk_mcp::McpTransport::StreamableHttp {
            url: uri,
            client,
            headers,
        });
        cfg.auth = auth;
        Ok(cfg)
    }
}

/// Go `mcpEndpointURI`: the server's connection URI preferring the JSONRPC binding, then
/// HTTP_JSON (over its protocols and top-level interfaces).
pub(crate) fn mcp_endpoint_uri(server: &McpServer) -> Option<String> {
    [TRANSPORT_JSONRPC, TRANSPORT_HTTP_JSON]
        .into_iter()
        .find_map(|transport| connection_uri(&server.protocols, &server.interfaces, "", transport))
        .map(|c| c.uri)
}
