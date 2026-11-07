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

//! gadk-mcp: the tools of a Model Context Protocol server as a gadk toolset (port of ADK-Go
//! `tool/mcptoolset`), built on [`rmcp`].
//!
//! [`new`] turns a [`Config`] into an [`McpToolset`]. The toolset lists the server's tools
//! (`tools/list`, every page) and exposes each one as an [`McpTool`] whose declaration carries the
//! server's input and output schemas unchanged; running a tool calls `tools/call` and shapes the
//! result as ADK-Go does (`{"output": …}`, `Tool execution failed.` on `isError`, and
//! [`format_mcp_content`] for text, resources, images and audio).
//!
//! The session connects lazily on first use. An operation that fails because the connection was
//! lost pings the session, reconnects once and retries (Go's connection refresher). A toolset
//! with [`Config::auth`] keeps one session per distinct credential, so a server that binds
//! sessions to the user who opened them serves every user; idle sessions close after 10 minutes
//! and at most 64 are kept open.
//!
//! Transports ([`McpTransport`]): streamable HTTP ([`McpTransport::Endpoint`],
//! [`McpTransport::StreamableHttp`]), a child process speaking stdio
//! ([`McpTransport::Command`]), or any rmcp client transport through a [`TransportFactory`]
//! ([`McpTransport::Custom`] with [`serve_client`]). Every HTTP request carries the ADK tracking
//! headers (`user-agent`, `x-goog-api-client`) and, with auth, the credential's headers.
//! Go's legacy SSE client transport has no rmcp counterpart and is not ported.
//!
//! Close a toolset with [`McpToolset::close`] when it is no longer used; dropping it cancels its
//! sessions without waiting.

mod client;
mod config;
mod error;
mod format;
mod go_mime;
mod http;
mod sessions;
mod set;
mod tool;
mod transport;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod parity_tests;
#[cfg(any(test, feature = "testutil"))]
pub mod testutil;

pub use client::{CallContext, McpClient};
pub use config::{
    CommandBuilder, Config, McpTransport, MetadataProvider, TransportFactory, new, serve_client,
};
pub use error::{BoxError, Error};
pub use format::format_mcp_content;
pub use rmcp;
pub use set::McpToolset;
pub use tool::McpTool;
