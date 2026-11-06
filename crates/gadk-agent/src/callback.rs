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

//! Callback types (Go `agent.BeforeAgentCallback`/`AfterAgentCallback` in `agent/agent.go`, the
//! model/tool callbacks in `agent/llmagent/llmagent.go`, and the plugin callbacks in
//! `plugin/plugin.go`).

use std::sync::Arc;

use futures::future::BoxFuture;
use gadk_genai::Content;
use gadk_model::{LlmRequest, LlmResponse, ModelError};
use gadk_session::Event;
use serde_json::{Map, Value};

use crate::context::{Context, InvocationContext};
use crate::error::{AgentError, ToolError};
use crate::tool::Tool;

/// Runs before the agent. `Some` content with no error is yielded as an event and skips the
/// remaining before-agent callbacks, the agent run, and the after-agent callbacks.
///
/// An error takes precedence over content, skips the remaining before-agent callbacks, and is
/// yielded as an `Err` item. If the caller keeps polling, the agent and after-agent callbacks can
/// still run.
pub type BeforeAgentCallback =
    Arc<dyn Fn(Context) -> BoxFuture<'static, Result<Option<Content>, AgentError>> + Send + Sync>;

/// Runs after the agent completes. It is skipped if the invocation has ended or the caller stops
/// polling before that point. `Some` content or an error skips the remaining after-agent callbacks.
/// Content is yielded as an event; an error takes precedence and is yielded as an `Err` item.
pub type AfterAgentCallback =
    Arc<dyn Fn(Context) -> BoxFuture<'static, Result<Option<Content>, AgentError>> + Send + Sync>;

/// Called before sending a request to the model. If it returns `Some` response or an error, the
/// actual model call is skipped and the returned response/error is used. The request (possibly
/// edited) is always handed back.
pub type BeforeModelCallback = Arc<
    dyn Fn(
            Context,
            LlmRequest,
        ) -> BoxFuture<'static, (LlmRequest, Result<Option<LlmResponse>, AgentError>)>
        + Send
        + Sync,
>;

/// Called after receiving a response from the model. If it returns `Some` response or an error, the
/// actual model response/error is replaced with the returned response/error.
pub type AfterModelCallback = Arc<
    dyn Fn(
            Context,
            LlmResponse,
            Option<Arc<ModelError>>,
        ) -> BoxFuture<'static, Result<Option<LlmResponse>, AgentError>>
        + Send
        + Sync,
>;

/// Called when the model returns an error. If it returns `Some` response or an error, the actual
/// model response/error is replaced with the returned response/error.
pub type OnModelErrorCallback = Arc<
    dyn Fn(
            Context,
            LlmRequest,
            Arc<ModelError>,
        ) -> BoxFuture<'static, Result<Option<LlmResponse>, AgentError>>
        + Send
        + Sync,
>;

/// Called before a tool runs. If it returns `Some` result or an error, the remaining callbacks and
/// the tool call are skipped and the returned result is used as the tool result.
pub type BeforeToolCallback = Arc<
    dyn Fn(
            Context,
            Arc<dyn Tool>,
            Map<String, Value>,
        ) -> BoxFuture<'static, Result<Option<Map<String, Value>>, AgentError>>
        + Send
        + Sync,
>;

/// Called after a tool runs, whether it returned a result or an error. If it returns `Some` result
/// or an error, the remaining callbacks are skipped and the returned result/error is the final
/// tool output.
pub type AfterToolCallback = Arc<
    dyn Fn(
            Context,
            Arc<dyn Tool>,
            Map<String, Value>,
            Map<String, Value>,
            Option<Arc<ToolError>>,
        ) -> BoxFuture<'static, Result<Option<Map<String, Value>>, AgentError>>
        + Send
        + Sync,
>;

/// Called when a tool returns an error. If it returns `Some` result or an error, the tool's
/// result/error is replaced with the returned one.
pub type OnToolErrorCallback = Arc<
    dyn Fn(
            Context,
            Arc<dyn Tool>,
            Map<String, Value>,
            Arc<ToolError>,
        ) -> BoxFuture<'static, Result<Option<Map<String, Value>>, AgentError>>
        + Send
        + Sync,
>;

/// Invoked when the user sends a message; may return a replacement message.
pub type OnUserMessageCallback = Arc<
    dyn Fn(InvocationContext, Content) -> BoxFuture<'static, Result<Option<Content>, AgentError>>
        + Send
        + Sync,
>;

/// Invoked for each event; may return a replacement event.
pub type OnEventCallback = Arc<
    dyn Fn(InvocationContext, Event) -> BoxFuture<'static, Result<Option<Event>, AgentError>>
        + Send
        + Sync,
>;

/// Invoked before a run starts; may return content that short-circuits the run.
pub type BeforeRunCallback = Arc<
    dyn Fn(InvocationContext) -> BoxFuture<'static, Result<Option<Content>, AgentError>>
        + Send
        + Sync,
>;

/// Invoked after a run completes.
pub type AfterRunCallback = Arc<dyn Fn(InvocationContext) -> BoxFuture<'static, ()> + Send + Sync>;

/// A plugin's close function (Go `plugin.Config.CloseFunc`).
pub type CloseFn = Arc<dyn Fn() -> BoxFuture<'static, Result<(), AgentError>> + Send + Sync>;
