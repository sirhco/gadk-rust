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

//! The registry's errors. Each variant's `Display` text starts with `agentregistry:`. Where a
//! variant wraps the failure of a library (reqwest, serde_json, google-cloud-auth), the
//! message carries that library's own text.

use gadk_platform::go_quote;

/// The registry API answered a non-2xx status.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("agentregistry: API request failed with status {status_code}: {body}")]
pub struct ApiError {
    /// The HTTP status code of the response.
    pub status_code: u16,
    /// The raw response body (decoded as lossy UTF-8).
    pub body: String,
}

/// A registry error.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// [`Client::new`](crate::Client::new): the project ID or the location is empty.
    #[error("agentregistry: project_id and location must be set")]
    Config,
    /// [`Client::new`](crate::Client::new): ADC could not be loaded (the
    /// cause is gadk-auth's text).
    #[error("agentregistry: loading Application Default Credentials: {0}")]
    Credentials(String),
    /// [`Client::new`](crate::Client::new): the default HTTP client could not be built.
    #[error("agentregistry: creating authenticated HTTP client: {0}")]
    HttpClient(String),
    /// A request URL did not parse.
    #[error("agentregistry: building request: {0}")]
    BuildRequest(String),
    /// A request failed in the transport; `path` is the resource path.
    #[error("agentregistry: GET {path}: {message}")]
    Http {
        /// The resource path as the caller passed it.
        path: String,
        /// The transport error's text.
        message: String,
    },
    /// The response body could not be read.
    #[error("agentregistry: reading response body: {0}")]
    ReadBody(String),
    /// A non-2xx answer; its `Display` is the [`ApiError`]'s.
    #[error(transparent)]
    Api(#[from] ApiError),
    /// The 2xx response body did not decode.
    #[error("agentregistry: decoding response: {0}")]
    Decode(String),
    /// `Client::remote_agent`: the embedded card did not decode as an A2A `AgentCard`.
    #[error("agentregistry: decoding embedded agent card for {}: {message}", go_quote(.resource))]
    EmbeddedCardDecode {
        /// The agent resource name.
        resource: String,
        /// serde_json's text.
        message: String,
    },
    /// `Client::remote_agent`: the embedded card has no supported interfaces.
    #[error("agentregistry: embedded agent card for {} has no supported interfaces", go_quote(.0))]
    EmbeddedCardNoInterfaces(String),
    /// `Client::remote_agent`: the embedded card's name cleans to nothing.
    #[error("agentregistry: embedded agent card for {} has an empty name", go_quote(.0))]
    EmbeddedCardEmptyName(String),
    /// `Client::remote_agent`: no A2A connection URI among the agent's protocols.
    #[error("agentregistry: A2A connection URI not found for agent {}", go_quote(.0))]
    NoConnectionUri(String),
    /// `Client::remote_agent`: neither the display name nor the resource name cleans to a non-empty
    /// name.
    #[error("agentregistry: cannot derive a non-empty agent name for {}", go_quote(.0))]
    EmptyName(String),
    /// `Client::mcp_toolset`: no JSONRPC or HTTP_JSON endpoint URI.
    #[error("agentregistry: MCP server endpoint URI not found for {}", go_quote(.0))]
    NoEndpoint(String),
    /// `Client::remote_agent`: building the A2A agent failed; the `Display` is the source's.
    #[cfg(feature = "a2a")]
    #[error(transparent)]
    A2a(gadk_a2a::Error),
    /// `Client::mcp_toolset`: building the MCP toolset failed; the `Display` is the
    /// source's.
    #[cfg(feature = "mcp")]
    #[error(transparent)]
    Mcp(gadk_mcp::Error),
}
