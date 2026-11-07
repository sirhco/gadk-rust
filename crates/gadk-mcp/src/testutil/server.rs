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

//! [`ToolServer`]: an rmcp server with fixed tools, and set_test.go's weather server.

use std::future::Future;
use std::sync::Arc;

use futures::future::BoxFuture;
use rmcp::model::{CallToolRequestParams, CallToolResult, Tool};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler};

/// What a [`ToolHandler`] returns.
pub type ToolFuture = BoxFuture<'static, Result<CallToolResult, ErrorData>>;

/// A tool's handler: the call's parameters and its request context (`context.meta` holds the
/// request's `_meta`; over HTTP, `context.extensions` holds the request's
/// `http::request::Parts`).
pub type ToolHandler =
    Arc<dyn Fn(CallToolRequestParams, RequestContext<RoleServer>) -> ToolFuture + Send + Sync>;

/// Go `mcp.NewServer(&mcp.Implementation{Name, Version}, nil)` plus `mcp.AddTool`: a server whose
/// `tools/list` returns the added tools in order (one page) and whose `tools/call` runs the
/// matching handler. Cheap to clone; clones share the tools.
#[derive(Clone)]
pub struct ToolServer {}

#[allow(unused_variables, clippy::todo)]
impl ToolServer {
    /// A server named `name` at `version`, without tools.
    pub fn new(name: &str, version: &str) -> Self {
        todo!("Task 4")
    }

    /// Go `mcp.AddTool`: adds `tool`, answered by `handler`.
    pub fn tool<H, Fut>(self, tool: Tool, handler: H) -> Self
    where
        H: Fn(CallToolRequestParams, RequestContext<RoleServer>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<CallToolResult, ErrorData>> + Send + 'static,
    {
        todo!("Task 4")
    }

    /// Adds set_test.go's `weatherFunc` as tool `name` (an empty `description` is omitted, as
    /// Go's zero `Description` is): go-sdk's exact input schema for `Input{City}` and output
    /// schema for `Output{WeatherSummary}`, answering `{"weather_summary": "Today in \"<city>\"
    /// is sunny"}` as structured content with its JSON text as the content, as go-sdk does.
    pub fn weather(self, name: &str, description: &str) -> Self {
        todo!("Task 4")
    }
}

/// Task 4 overrides `get_info` (tools capability, the server's name and version), `list_tools`
/// and `call_tool`.
impl ServerHandler for ToolServer {}

/// set_test.go's server: `weather_server` `v1.0.0` with
/// `.weather("get_weather", "returns weather in the given city")`.
#[allow(clippy::todo)]
pub fn weather_server() -> ToolServer {
    todo!("Task 4")
}
