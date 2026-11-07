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

//! In-process MCP servers and connectors for tests (feature `testutil`; always built for this
//! crate's own unit tests). They stand in for go-sdk's `mcp.NewInMemoryTransports`, the fake
//! transports of ADK-Go's `tool/mcptoolset` tests, and `httptest` servers:
//! - [`ToolServer`] and [`weather_server`]: an rmcp server with fixed tools (Go `mcp.NewServer` +
//!   `mcp.AddTool`);
//! - [`in_memory`] and [`ScriptedFactory`]: [`TransportFactory`](crate::TransportFactory)s that
//!   serve a fresh server over a `tokio::io::duplex` per connect, the latter observable and
//!   steerable (connect counts, a gate, failures, killing the last connection);
//! - [`HttpTestServer`]: a streamable-HTTP MCP server on `127.0.0.1` that records requests and can
//!   bind sessions to bearer-token users (Go `userBoundServer`).

mod factory;
mod http;
mod server;

pub use factory::{ScriptedFactory, in_memory};
pub use http::{FrontHook, HttpTestServer, InitializeHold, RecordedRequest};
pub use server::{ToolFuture, ToolHandler, ToolServer, weather_server};
