// Copyright 2026 Chris Olson
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

//! The crate error ([`Error`]).

/// An error loading an OpenAPI document, building the toolset, or sending a tool's request.
///
/// Loading and building return it directly. A tool call returns it as the source of
/// [`ToolError::Execution`](gadk_agent::ToolError::Execution) (`Request`, `Timeout` and
/// `Cancelled`).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The text is neither a JSON nor a YAML object, or a file could not be read.
    #[error("cannot parse the OpenAPI document: {message}")]
    Parse {
        /// What went wrong.
        message: String,
    },
    /// The document's `openapi` field is missing or is not `3.0.x` or `3.1.x`.
    #[error("unsupported OpenAPI version {found}: only 3.0.x and 3.1.x are supported")]
    UnsupportedVersion {
        /// The version found (`swagger 2.0` for a Swagger 2.0 document, `no openapi field` when
        /// there is none).
        found: String,
    },
    /// A `$ref` is not local (`#/…`) or points at nothing.
    #[error("cannot resolve $ref {reference:?} in {at}")]
    Ref {
        /// The reference.
        reference: String,
        /// The operation it was found in, as `<METHOD> <path>`.
        at: String,
    },
    /// Two operations have the same tool name, or two properties of one tool have the same name
    /// (`<tool name>.<property>`).
    #[error("duplicate name {name:?}")]
    DuplicateName {
        /// The duplicated name.
        name: String,
    },
    /// A prefixed tool name is longer than 64 bytes.
    #[error("tool name {name:?} is longer than 64 bytes")]
    NameTooLong {
        /// The prefixed name.
        name: String,
    },
    /// There is no absolute base URL: the server URL is relative, the document was not fetched,
    /// and `OpenApiConfig::base_url` is not set (or is not an absolute URL).
    #[error(
        "no absolute base URL: set OpenApiConfig::base_url or give the document an absolute servers[0].url"
    )]
    NoBaseUrl,
    /// Downloading the document failed.
    #[error("cannot fetch the OpenAPI document from {url}: {message}")]
    Fetch {
        /// The URL.
        url: String,
        /// What went wrong.
        message: String,
    },
    /// The HTTP client for the tools' requests could not be built (for example, the TLS backend
    /// failed to initialize).
    #[error("cannot build the HTTP client: {source}")]
    HttpClient {
        /// The underlying error.
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// Sending a tool's request or reading its response failed.
    #[error("tool {tool}: request failed: {source}")]
    Request {
        /// The exposed tool name.
        tool: String,
        /// The underlying error (connection, DNS, the credential provider, or the body read).
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// The tool's request did not finish within `OpenApiConfig::timeout`.
    #[error("tool {tool}: request timed out")]
    Timeout {
        /// The exposed tool name.
        tool: String,
    },
    /// The invocation was cancelled while the tool's request was in flight.
    #[error("tool {tool}: request cancelled")]
    Cancelled {
        /// The exposed tool name.
        tool: String,
    },
}
