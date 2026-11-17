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

//! gadk-agentregistry: a client for the Google Cloud Agent Registry
//! (`agentregistry.googleapis.com`), a governed catalog of A2A agents, MCP servers, and model
//! endpoints.
//!
//! This crate provides the client foundation: configuration ([`Config`]), an authenticated REST
//! transport to the third-party (public) endpoint using Application Default Credentials with mTLS
//! endpoint selection, typed errors ([`ApiError`]), and the wire types returned by the service.
//! Discovery methods ([`Client::list_agents`], [`Client::all_mcp_servers`], …) and the
//! [`Client::remote_agent`] (feature `a2a`) / [`Client::mcp_toolset`] (feature `mcp`) factory
//! helpers build on this foundation.

mod client;
mod config;
mod error;
#[cfg(any(feature = "a2a", feature = "mcp"))]
mod factory;
mod requester;
mod resolve;
mod types;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod parity_tests;

pub use client::{ListOption, with_filter, with_page_size, with_page_token};
pub use config::{Client, Config};
pub use error::{ApiError, Error};
#[cfg(feature = "mcp")]
pub use factory::{McpToolsetOption, with_mcp_headers, with_mcp_http_client};
#[cfg(feature = "a2a")]
pub use factory::{RemoteAgentOption, with_a2a_headers, with_a2a_http_client};
pub use types::{
    Agent, Annotations, Card, Endpoint, Interface, ListAgentsResponse, ListEndpointsResponse,
    ListMcpServersResponse, McpServer, Protocol, Skill, Tool,
};
