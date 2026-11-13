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

//! Go `internal/configurable/configurable_utils.go` `init()`: the built-in tool factories
//! (`exit_loop`, `google_search`, `url_context`, `google_maps_grounding`, `AgentTool`,
//! `LongRunningFunctionTool`, `ExampleTool`) and the `McpToolset` toolset factory. Go's
//! anonymous factory closures are named after their registered names.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::sync::Arc;

use gadk_agent::{Tool, Toolset};

use crate::{ConfigError, GoMap, Registry, ToolCtx, ToolEntry};

/// The built-in factories in Go's `init()` registration order, for [`Registry::new`].
pub(crate) fn builtin_tool_factories() -> Vec<(&'static str, ToolEntry)> {
    // Go: configurable_utils.go init (tool and toolset factories)
    todo!("Task 4")
}

/// Go `exit_loop`: `exitlooptool.New()`.
pub(crate) async fn exit_loop(
    registry: Registry,
    ctx: ToolCtx,
    args: Option<GoMap>,
) -> Result<Arc<dyn Tool>, ConfigError> {
    // Go: configurable_utils.go init (exit_loop)
    todo!("Task 4")
}

/// Go `google_search`: `geminitool.GoogleSearch{}`.
pub(crate) async fn google_search(
    registry: Registry,
    ctx: ToolCtx,
    args: Option<GoMap>,
) -> Result<Arc<dyn Tool>, ConfigError> {
    // Go: configurable_utils.go init (google_search)
    todo!("Task 4")
}

/// Go `url_context`: `geminitool.New("url_context", "url context", &genai.Tool{URLContext:
/// &genai.URLContext{}})`.
pub(crate) async fn url_context(
    registry: Registry,
    ctx: ToolCtx,
    args: Option<GoMap>,
) -> Result<Arc<dyn Tool>, ConfigError> {
    // Go: configurable_utils.go init (url_context)
    todo!("Task 4")
}

/// Go `google_maps_grounding`: `geminitool.New("google_maps_grounding", "google maps
/// grounding", &genai.Tool{GoogleMaps: &genai.GoogleMaps{}})`.
pub(crate) async fn google_maps_grounding(
    registry: Registry,
    ctx: ToolCtx,
    args: Option<GoMap>,
) -> Result<Arc<dyn Tool>, ConfigError> {
    // Go: configurable_utils.go init (google_maps_grounding)
    todo!("Task 4")
}

/// Go `AgentTool`: `args is nil`, `agent not found in args`, `parentPath not found in context`,
/// `config_path not found in args`; resolves the agent relative to the context's parent path
/// and wraps it in `agenttool.New` with `skip_summarization`.
pub(crate) async fn agent_tool(
    registry: Registry,
    ctx: ToolCtx,
    args: Option<GoMap>,
) -> Result<Arc<dyn Tool>, ConfigError> {
    // Go: configurable_utils.go init (AgentTool)
    todo!("Task 4")
}

/// Go `LongRunningFunctionTool`: `args is nil`, `func not found in args`, resolves `func` with
/// the same args, `tool '%s' not found` when it names a toolset.
pub(crate) async fn long_running_function_tool(
    registry: Registry,
    ctx: ToolCtx,
    args: Option<GoMap>,
) -> Result<Arc<dyn Tool>, ConfigError> {
    // Go: configurable_utils.go init (LongRunningFunctionTool)
    todo!("Task 4")
}

/// Go `ExampleTool`: `args is nil`, `examples not found in args`, `examples is not a list`;
/// wraps a non-list `output` in a list, round-trips through JSON
/// (`failed to decode normalized examples: %w`) into `exampletool.Example`s.
pub(crate) async fn example_tool(
    registry: Registry,
    ctx: ToolCtx,
    args: Option<GoMap>,
) -> Result<Arc<dyn Tool>, ConfigError> {
    // Go: configurable_utils.go init (ExampleTool)
    todo!("Task 4")
}

/// Go `McpToolset`: `stdio_connection_params not found in args`,
/// `server_params not found in stdio_connection_params`, `command not found in server_params`,
/// `args not found in server_params`, `tool_filter not found in args`,
/// `server_params.args[%d]: expected string, got %T (%v)`,
/// `tool_filter[%d]: expected string, got %T (%v)`; a command transport with a string
/// predicate filter (`failed to create mcp toolset: %v`).
pub(crate) async fn mcp_toolset(
    registry: Registry,
    ctx: ToolCtx,
    args: Option<GoMap>,
) -> Result<Arc<dyn Toolset>, ConfigError> {
    // Go: configurable_utils.go init (McpToolset)
    todo!("Task 4")
}
