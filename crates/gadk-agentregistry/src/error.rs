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

//! The registry's errors, each `Display`ing Go's text (`fmt.Errorf` wrappers of
//! `registry.go`, `requester.go` and `factory.go`). A wrapped Go `%w` cause from a
//! Rust library (reqwest, serde_json, google-cloud-auth) keeps that library's text (ledgered).

use gadk_platform::go_quote;

/// Go `APIError`: the registry API answered a non-2xx status.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("agentregistry: API request failed with status {status_code}: {body}")]
pub struct ApiError {
    /// Go `StatusCode`: the HTTP status code of the response.
    pub status_code: u16,
    /// Go `Body`: the raw response body (lossy UTF-8, as Go's `string(body)`).
    pub body: String,
}

/// A registry error.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// `New`: `agentregistry: ProjectID and Location must be set`.
    #[error("agentregistry: ProjectID and Location must be set")]
    Config,
    /// `New`: ADC could not be loaded (the cause is gadk-auth's text).
    #[error("agentregistry: loading Application Default Credentials: {0}")]
    Credentials(String),
    /// `New`: the default HTTP client could not be built (Go
    /// `creating authenticated HTTP client: %w`).
    #[error("agentregistry: creating authenticated HTTP client: {0}")]
    HttpClient(String),
    /// `restRequester.Get`: the request URL did not parse (Go `building request: %w`).
    #[error("agentregistry: building request: {0}")]
    BuildRequest(String),
    /// `restRequester.Get`: the request failed (Go `GET %s: %w`, `path` is the resource path).
    #[error("agentregistry: GET {path}: {message}")]
    Http {
        /// The resource path as the caller passed it.
        path: String,
        /// The transport error's text.
        message: String,
    },
    /// `restRequester.Get`: the body could not be read.
    #[error("agentregistry: reading response body: {0}")]
    ReadBody(String),
    /// `restRequester.Get`: a non-2xx answer (Go returns the `*APIError` unwrapped).
    #[error(transparent)]
    Api(#[from] ApiError),
    /// `restRequester.Get`: the 2xx body did not decode.
    #[error("agentregistry: decoding response: {0}")]
    Decode(String),
    /// `agentCard`: the embedded card did not decode as an A2A `AgentCard`.
    #[error("agentregistry: decoding embedded agent card for {}: {message}", go_quote(.resource))]
    EmbeddedCardDecode {
        /// The agent resource name.
        resource: String,
        /// serde_json's text.
        message: String,
    },
    /// `agentCard`: the embedded card has no supported interfaces.
    #[error("agentregistry: embedded agent card for {} has no supported interfaces", go_quote(.0))]
    EmbeddedCardNoInterfaces(String),
    /// `agentCard`: the embedded card's name cleans to nothing.
    #[error("agentregistry: embedded agent card for {} has an empty name", go_quote(.0))]
    EmbeddedCardEmptyName(String),
    /// `agentCard`: no A2A connection URI among the agent's protocols.
    #[error("agentregistry: A2A connection URI not found for agent {}", go_quote(.0))]
    NoConnectionUri(String),
    /// `agentCard`: neither the display name nor the resource name cleans to a non-empty name.
    #[error("agentregistry: cannot derive a non-empty agent name for {}", go_quote(.0))]
    EmptyName(String),
    /// `MCPToolset`: no JSONRPC or HTTP_JSON endpoint URI.
    #[error("agentregistry: MCP server endpoint URI not found for {}", go_quote(.0))]
    NoEndpoint(String),
    /// `RemoteAgent`: `remoteagent.NewA2A` failed (Go returns its error unwrapped).
    #[cfg(feature = "a2a")]
    #[error(transparent)]
    A2a(gadk_a2a::Error),
    /// `MCPToolset`: `mcptoolset.New` failed (Go returns its error unwrapped).
    #[cfg(feature = "mcp")]
    #[error(transparent)]
    Mcp(gadk_mcp::Error),
}
