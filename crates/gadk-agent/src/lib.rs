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

//! gadk-agent: the agent API — agents, contexts, tools, callbacks and plugins.
//!
//! Besides the API itself it carries the execution-identity pieces the engine and the analytics
//! plugin rely on: [`fiber`] (fiber ids, spawn origins and attempt sites the engine sets where it
//! starts a concurrent task; read by the analytics plugin's fiber attribution), cancel causes
//! ([`CancelCause`], [`InvocationContext::cancel_with_cause`] / [`InvocationContext::cause`],
//! with `DeadlineExceeded` for agent timeouts), [`Context::context_path`] and the per-invocation
//! teardown registry [`Teardowns`], through which work a component was dropped with finishes
//! before the runner's after-run callbacks, even when a consumer stops reading early.

pub mod agent;
pub mod callback;
pub mod context;
pub mod error;
pub mod fiber;
mod go_fmt;
pub mod live;
pub mod loader;
pub mod mode;
pub mod parent_map;
pub mod plugin;
pub mod plugin_manager;
pub mod run_config;
pub mod scheduler;
pub mod services;
pub mod state;
pub mod teardown;
pub mod tool;
pub mod tool_util;
pub mod toolset;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod parity_tests;

pub use agent::{
    Agent, AgentConfig, AgentRunFn, LiveAgent, new as new_agent, new_with_state, self_arc,
};
pub use callback::*;
pub use context::{
    ActivationId, CancelCause, CancelGuard, Context, ContextBuilder, ContextDelta, Identity,
    InvocationContext, PromptUse, ReadonlyContext, SharedCauseError,
};
pub use error::{AgentError, EventStream, SharedToolError, ToolError};
#[doc(hidden)]
pub use go_fmt::go_quote;
pub use live::{LiveRequest, LiveRunConfig, LiveSession};
pub use loader::{Loader, LoaderError, MultiLoader, SingleLoader};
pub use mode::{BoundModes, Mode};
pub use parent_map::ParentMap;
pub use plugin::{Plugin, PluginConfig};
pub use plugin_manager::PluginManager;
pub use run_config::{RunConfig, StreamingMode, run_limits};
pub use scheduler::DynamicSubScheduler;
pub use services::{ArtifactBacking, Artifacts, Memory};
pub use state::{AgentState, AgentType};
pub use teardown::Teardowns;
pub use tool::{
    ConfirmationProvider, RunnableTool, StreamingTool, Tool, ToolOriginKind, ToolPredicate, Toolset,
};
pub use tool_util::{
    REQUEST_CONFIRMATION_FUNCTION_CALL_NAME, append_instructions, original_call_from, pack_tool,
    tool_from_request,
};
pub use toolset::{allowed_tools_predicate, filter_toolset, string_predicate, with_confirmation};
