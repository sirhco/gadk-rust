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

//! The plugin callbacks of ADK-Go `plugin/agentanalytics/bigquery_agent_analytics_plugin.go`
//! (`onUserMessage`, `beforeRun`, `afterRun`, `logAgentErrors`, `logPurgedAgents`, `onEvent`,
//! `nodeCompletion`, `hasTaskCompletedResponse`, `functionResponseCalls`, `beforeAgent`,
//! `afterAgent`, `beforeModel`, `afterModel`, `hasGeneratedContent`, `onModelError`,
//! `beforeTool`, `isToolConfirmationOutcome`, `afterTool`, `onToolError`, `logToolError`,
//! `toolName`, `toolContent`) and their wiring into a `gadk_agent::PluginConfig`.
//!
//! Each gadk callback (async) clones the `Arc<AnalyticsPlugin>`, builds a [`CallbackCtx`], runs
//! the synchronous body below — all trace-manager work and row building, under panic recovery
//! — before any `.await`, and returns "no override" (`Ok(None)`; before-model `(req,
//! Ok(None))`; after-model `(resp, Ok(None))`; after-run `()`). After-run then awaits the
//! processor's `flush_run_end` when `Config::flush_on_run_end()`.

#![allow(dead_code, unused_variables)] // Task 9 implements the bodies.

use std::sync::Arc;

use gadk_agent::{CloseFn, PluginConfig, Tool, ToolError};
use gadk_genai::Content;
use gadk_model::{LlmRequest, LlmResponse, ModelError};
use gadk_session::Event;
use serde_json::{Map, Value};

use super::{AnalyticsPlugin, PluginState};
use crate::trace::{CallbackCtx, Identity, RowSpan, TraceError, UnclosedAgentSpan};

/// Go `finishTaskToolName`.
pub(crate) const FINISH_TASK_TOOL_NAME: &str = "finish_task";

/// The `PluginConfig` of an enabled plugin (Go's `baseplugin.Config` literal): the 12
/// callbacks over `plugin`, `close`, and `state` as the typed state slot.
pub(crate) fn plugin_config(
    plugin: &Arc<AnalyticsPlugin>,
    state: Arc<PluginState>,
    close: CloseFn,
) -> PluginConfig {
    todo!("Task 9")
}

impl AnalyticsPlugin {
    /// Go `onUserMessage(ctx, msg)`.
    pub(crate) fn on_user_message(&self, ctx: &CallbackCtx, msg: &Content) {
        todo!("Task 9")
    }

    /// Go `beforeRun(ctx)`.
    pub(crate) fn before_run(&self, ctx: &CallbackCtx) {
        todo!("Task 9")
    }

    /// Go `afterRun(ctx)` without its final `flushRunEnd` (the callback awaits it).
    pub(crate) fn after_run(&self, ctx: &CallbackCtx) {
        todo!("Task 9")
    }

    /// Go `logAgentErrors(ctx, id, unclosed, err)`.
    pub(crate) fn log_agent_errors(
        &self,
        ctx: &CallbackCtx,
        id: &Identity,
        unclosed: &[UnclosedAgentSpan],
        err: Option<&TraceError>,
    ) {
        todo!("Task 9")
    }

    /// Go `logPurgedAgents(ctx, id, purged)`.
    pub(crate) fn log_purged_agents(
        &self,
        ctx: &CallbackCtx,
        id: &Identity,
        purged: &[UnclosedAgentSpan],
    ) {
        todo!("Task 9")
    }

    /// Go `onEvent(ctx, ev)`.
    pub(crate) fn on_event(&self, ctx: &CallbackCtx, ev: &Event) {
        todo!("Task 9")
    }

    /// Go `beforeAgent(ctx)`.
    pub(crate) fn before_agent(&self, ctx: &CallbackCtx) {
        todo!("Task 9")
    }

    /// Go `afterAgent(ctx)`.
    pub(crate) fn after_agent(&self, ctx: &CallbackCtx) {
        todo!("Task 9")
    }

    /// Go `beforeModel(ctx, req)`.
    pub(crate) fn before_model(&self, ctx: &CallbackCtx, req: &LlmRequest) {
        todo!("Task 9")
    }

    /// Go `afterModel(ctx, res, err)`.
    pub(crate) fn after_model(
        &self,
        ctx: &CallbackCtx,
        res: &LlmResponse,
        err: Option<&ModelError>,
    ) {
        todo!("Task 9")
    }

    /// Go `onModelError(ctx, req, err)`.
    pub(crate) fn on_model_error(&self, ctx: &CallbackCtx, req: &LlmRequest, err: &ModelError) {
        todo!("Task 9")
    }

    /// Go `beforeTool(ctx, t, args)`.
    pub(crate) fn before_tool(
        &self,
        ctx: &CallbackCtx,
        t: &Arc<dyn Tool>,
        args: &Map<String, Value>,
    ) {
        todo!("Task 9")
    }

    /// Go `afterTool(ctx, t, args, result, err)`.
    pub(crate) fn after_tool(
        &self,
        ctx: &CallbackCtx,
        t: &Arc<dyn Tool>,
        args: &Map<String, Value>,
        result: &Map<String, Value>,
        err: Option<&ToolError>,
    ) {
        todo!("Task 9")
    }

    /// Go `onToolError(ctx, t, args, err)`.
    pub(crate) fn on_tool_error(
        &self,
        ctx: &CallbackCtx,
        t: &Arc<dyn Tool>,
        args: &Map<String, Value>,
        err: &ToolError,
    ) {
        todo!("Task 9")
    }

    /// Go `logToolError(ctx, id, t, spanFn, name, callID, args, err)`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn log_tool_error(
        &self,
        ctx: &CallbackCtx,
        id: &Identity,
        t: &Arc<dyn Tool>,
        span_fn: &mut dyn FnMut() -> RowSpan,
        name: &str,
        call_id: &str,
        args: &Map<String, Value>,
        err: &ToolError,
    ) {
        todo!("Task 9")
    }
}

/// Go `nodeCompletion(ev)`: `Some((workflow_name, failed))` for a workflow node-completion
/// event.
pub(crate) fn node_completion(ev: &Event) -> Option<(String, bool)> {
    todo!("Task 9")
}

/// Go `hasTaskCompletedResponse(c)`.
pub(crate) fn has_task_completed_response(c: Option<&Content>) -> bool {
    todo!("Task 9")
}

/// Go `functionResponseCalls(c)`: the function call ID, or else the name, of each function
/// response.
pub(crate) fn function_response_calls(c: Option<&Content>) -> Vec<String> {
    todo!("Task 9")
}

/// Go `hasGeneratedContent(res)`.
pub(crate) fn has_generated_content(res: &LlmResponse) -> bool {
    todo!("Task 9")
}

/// Go `isToolConfirmationOutcome(err)`: `ToolError::ConfirmationRequired` /
/// `ConfirmationRejected`.
pub(crate) fn is_tool_confirmation_outcome(err: &ToolError) -> bool {
    todo!("Task 9")
}

/// Go `toolName(t)`.
pub(crate) fn tool_name(t: &Arc<dyn Tool>) -> String {
    todo!("Task 9")
}

/// Go `toolContent(name, payloadKey, payload, origin)`.
pub(crate) fn tool_content(
    name: &str,
    payload_key: &str,
    payload: &Map<String, Value>,
    origin: &str,
) -> Value {
    todo!("Task 9")
}

/// Go's `error` of a model / tool callback as the trace manager records it (Go
/// `context.Canceled` stays recognizable by `trace::causes::is_canceled`).
pub(crate) fn trace_error_of_model(err: &ModelError) -> TraceError {
    todo!("Task 9")
}

/// As [`trace_error_of_model`] for a tool error.
pub(crate) fn trace_error_of_tool(err: &ToolError) -> TraceError {
    todo!("Task 9")
}
