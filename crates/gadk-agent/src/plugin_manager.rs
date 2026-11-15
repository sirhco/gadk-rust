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

//! The plugin manager (port of Go `internal/plugininternal/plugin_manager.go`).
//!
//! Every `run_*` method visits the plugins in registration order. The first callback error is
//! returned unchanged (no plugin name), and the first `Some` result short-circuits, as in Go.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use gadk_genai::Content;
use gadk_model::{LlmRequest, LlmResponse, ModelError};
use gadk_session::Event;
use serde_json::{Map, Value};

use crate::agent::Agent;
use crate::context::{Context, InvocationContext};
use crate::error::{AgentError, ToolError};
use crate::go_fmt::go_quote;
use crate::plugin::Plugin;
use crate::tool::Tool;

/// Go `plugininternal.PluginManager`.
#[derive(Debug)]
pub struct PluginManager {
    plugins: Vec<Plugin>,
    close_timeout: Duration,
}

impl PluginManager {
    /// Go `NewPluginManager`/`registerPlugin`: fails only on a duplicate plugin name.
    pub fn new(plugins: Vec<Plugin>, close_timeout: Duration) -> Result<Self, AgentError> {
        let mut out: Vec<Plugin> = Vec::with_capacity(plugins.len());
        for p in plugins {
            if out.iter().any(|q| q.name() == p.name()) {
                return Err(AgentError::Config(format!(
                    "plugin with name '{}' already registered",
                    p.name()
                )));
            }
            out.push(p);
        }
        Ok(Self {
            plugins: out,
            close_timeout,
        })
    }

    /// Go `HasPlugins`: whether any plugin is registered. Go's nil manager is `None` at the call
    /// site (`pm.is_some_and(|m| m.has_plugins())`).
    pub fn has_plugins(&self) -> bool {
        !self.plugins.is_empty()
    }

    /// Go `RunOnUserMessageCallback`.
    pub async fn run_on_user_message(
        &self,
        ctx: InvocationContext,
        msg: Content,
    ) -> Result<Option<Content>, AgentError> {
        for p in &self.plugins {
            if let Some(cb) = p.on_user_message_callback()
                && let Some(c) = cb(ctx.clone(), msg.clone()).await?
            {
                return Ok(Some(c));
            }
        }
        Ok(None)
    }

    /// Go `RunBeforeRunCallback`.
    pub async fn run_before_run(
        &self,
        ctx: InvocationContext,
    ) -> Result<Option<Content>, AgentError> {
        for p in &self.plugins {
            if let Some(cb) = p.before_run_callback()
                && let Some(c) = cb(ctx.clone()).await?
            {
                return Ok(Some(c));
            }
        }
        Ok(None)
    }

    /// Go `RunAfterRunCallback`: every plugin's callback, in order.
    pub async fn run_after_run(&self, ctx: InvocationContext) {
        for p in &self.plugins {
            if let Some(cb) = p.after_run_callback() {
                cb(ctx.clone()).await;
            }
        }
    }

    /// Go `RunOnEventCallback`.
    pub async fn run_on_event(
        &self,
        ctx: InvocationContext,
        event: Event,
    ) -> Result<Option<Event>, AgentError> {
        for p in &self.plugins {
            if let Some(cb) = p.on_event_callback()
                && let Some(e) = cb(ctx.clone(), event.clone()).await?
            {
                return Ok(Some(e));
            }
        }
        Ok(None)
    }

    /// Go `RunBeforeAgentCallback`.
    pub async fn run_before_agent(&self, ctx: Context) -> Result<Option<Content>, AgentError> {
        for p in &self.plugins {
            if let Some(cb) = p.before_agent_callback()
                && let Some(c) = cb(ctx.clone()).await?
            {
                return Ok(Some(c));
            }
        }
        Ok(None)
    }

    /// Go `RunAfterAgentCallback`.
    pub async fn run_after_agent(&self, ctx: Context) -> Result<Option<Content>, AgentError> {
        for p in &self.plugins {
            if let Some(cb) = p.after_agent_callback()
                && let Some(c) = cb(ctx.clone()).await?
            {
                return Ok(Some(c));
            }
        }
        Ok(None)
    }

    /// Go `RunOnAgentErrorCallback`: notifies every plugin, in registration order, that `err`
    /// escaped `agent`'s run. Best-effort notification: no plugin can suppress the error. A panic
    /// from one callback is caught (and recorded on the current span as `plugin "<name>"
    /// OnAgentErrorCallback panicked: <panic>`), so the remaining callbacks still run.
    pub fn run_on_agent_error(&self, ctx: Context, agent: Arc<dyn Agent>, err: &AgentError) {
        for p in &self.plugins {
            let Some(cb) = p.on_agent_error_callback() else {
                continue;
            };
            let res = std::panic::catch_unwind(AssertUnwindSafe(|| {
                cb(ctx.clone(), agent.clone(), err);
            }));
            if let Err(payload) = res {
                let msg = format!(
                    "plugin {} OnAgentErrorCallback panicked: {}",
                    go_quote(p.name()),
                    panic_text(&*payload)
                );
                // Go `trace.SpanFromContext(cctx).RecordError(...)`: an `exception` event.
                tracing::error!(exception.message = msg.as_str(), "exception");
            }
        }
    }

    /// Go `RunBeforeModelCallback`: the request is threaded through every callback and always
    /// handed back.
    pub async fn run_before_model(
        &self,
        ctx: Context,
        mut req: LlmRequest,
    ) -> (LlmRequest, Result<Option<LlmResponse>, AgentError>) {
        for p in &self.plugins {
            if let Some(cb) = p.before_model_callback() {
                let (r, res) = cb(ctx.clone(), req).await;
                req = r;
                match res {
                    Err(e) => return (req, Err(e)),
                    Ok(Some(resp)) => return (req, Ok(Some(resp))),
                    Ok(None) => {}
                }
            }
        }
        (req, Ok(None))
    }

    /// Go `RunAfterModelCallback`: the response is threaded through every callback (Go passes
    /// one `*model.LLMResponse` that callbacks may edit in place) and always handed back.
    pub async fn run_after_model(
        &self,
        ctx: Context,
        mut resp: LlmResponse,
        err: Option<Arc<ModelError>>,
    ) -> (LlmResponse, Result<Option<LlmResponse>, AgentError>) {
        for p in &self.plugins {
            if let Some(cb) = p.after_model_callback() {
                let (r, res) = cb(ctx.clone(), resp, err.clone()).await;
                resp = r;
                match res {
                    Err(e) => return (resp, Err(e)),
                    Ok(Some(new_resp)) => return (resp, Ok(Some(new_resp))),
                    Ok(None) => {}
                }
            }
        }
        (resp, Ok(None))
    }

    /// Go `RunOnModelErrorCallback`.
    pub async fn run_on_model_error(
        &self,
        ctx: Context,
        req: LlmRequest,
        err: Arc<ModelError>,
    ) -> Result<Option<LlmResponse>, AgentError> {
        for p in &self.plugins {
            if let Some(cb) = p.on_model_error_callback()
                && let Some(r) = cb(ctx.clone(), req.clone(), err.clone()).await?
            {
                return Ok(Some(r));
            }
        }
        Ok(None)
    }

    /// Go `RunBeforeToolCallback`.
    pub async fn run_before_tool(
        &self,
        ctx: Context,
        tool: Arc<dyn Tool>,
        args: Map<String, Value>,
    ) -> Result<Option<Map<String, Value>>, AgentError> {
        for p in &self.plugins {
            if let Some(cb) = p.before_tool_callback()
                && let Some(r) = cb(ctx.clone(), tool.clone(), args.clone()).await?
            {
                return Ok(Some(r));
            }
        }
        Ok(None)
    }

    /// Go `RunAfterToolCallback`.
    pub async fn run_after_tool(
        &self,
        ctx: Context,
        tool: Arc<dyn Tool>,
        args: Map<String, Value>,
        result: Map<String, Value>,
        err: Option<Arc<ToolError>>,
    ) -> Result<Option<Map<String, Value>>, AgentError> {
        for p in &self.plugins {
            if let Some(cb) = p.after_tool_callback()
                && let Some(r) = cb(
                    ctx.clone(),
                    tool.clone(),
                    args.clone(),
                    result.clone(),
                    err.clone(),
                )
                .await?
            {
                return Ok(Some(r));
            }
        }
        Ok(None)
    }

    /// Go `RunOnToolErrorCallback`.
    pub async fn run_on_tool_error(
        &self,
        ctx: Context,
        tool: Arc<dyn Tool>,
        args: Map<String, Value>,
        err: Arc<ToolError>,
    ) -> Result<Option<Map<String, Value>>, AgentError> {
        for p in &self.plugins {
            if let Some(cb) = p.on_tool_error_callback()
                && let Some(r) = cb(ctx.clone(), tool.clone(), args.clone(), err.clone()).await?
            {
                return Ok(Some(r));
            }
        }
        Ok(None)
    }

    /// Go `Close`: closes every plugin, collecting `error closing plugin '<name>': <err>`, and
    /// returns `failed to close plugins: [e1 e2 …]` (Go `%v` of `[]error`). A `close_timeout` above
    /// zero bounds each close function (an elapsed one records `close timed out after
    /// {close_timeout:?}`); zero means no bound (Go never applies one).
    pub async fn close(&self) -> Result<(), AgentError> {
        let mut errors: Vec<String> = Vec::new();
        for p in &self.plugins {
            let Some(close) = p.close() else { continue };
            let res = if self.close_timeout.is_zero() {
                close().await
            } else {
                match tokio::time::timeout(self.close_timeout, close()).await {
                    Ok(r) => r,
                    Err(_) => Err(AgentError::other(format!(
                        "close timed out after {:?}",
                        self.close_timeout
                    ))),
                }
            };
            if let Err(e) = res {
                errors.push(format!("error closing plugin '{}': {e}", p.name()));
            }
        }
        if errors.is_empty() {
            return Ok(());
        }
        Err(AgentError::other(format!(
            "failed to close plugins: [{}]",
            errors.join(" ")
        )))
    }
}

/// Go `%v` of a recovered panic value: the `&str` / `String` payload, else a placeholder.
fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        return (*s).to_owned();
    }
    if let Some(s) = payload.downcast_ref::<String>() {
        return s.clone();
    }
    "<non-string panic>".to_owned()
}
