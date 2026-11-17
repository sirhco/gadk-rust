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

//! The plugin callbacks (`on_user_message`, `before_run`, `after_run`, `log_agent_errors`,
//! `log_purged_agents`, `on_event`, `node_completion`, `has_task_completed_response`,
//! `function_response_calls`, `before_agent`, `after_agent`, `before_model`, `after_model`,
//! `has_generated_content`, `on_model_error`, `before_tool`, `is_tool_confirmation_outcome`,
//! `after_tool`, `on_tool_error`, `log_tool_error`, `tool_name`, `tool_content`) and their wiring
//! into a `gadk_agent::PluginConfig`.
//!
//! Each gadk callback (async) clones the `Arc<AnalyticsPlugin>`, builds a [`CallbackCtx`], runs
//! the synchronous body below — all trace-manager work and row building, under panic recovery
//! — before any `.await`, and returns "no override" (`Ok(None)`; before-model `(req,
//! Ok(None))`; after-model `(resp, Ok(None))`; after-run `()`). After-run then awaits the
//! processor's `flush_run_end` when `Config::flush_on_run_end()`.

use std::error::Error as StdError;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use futures::FutureExt;
use gadk_agent::{
    AfterAgentCallback, AfterModelCallback, AfterRunCallback, AfterToolCallback, AgentError,
    BeforeAgentCallback, BeforeModelCallback, BeforeRunCallback, BeforeToolCallback, CloseFn,
    Context, InvocationContext, OnEventCallback, OnModelErrorCallback, OnToolErrorCallback,
    OnUserMessageCallback, PluginConfig, Tool, ToolError,
};
use gadk_genai::Content;
use gadk_model::{LlmRequest, LlmResponse, ModelError};
use gadk_session::Event;
use serde_json::{Map, Value};

use super::rows::{
    agent_starting_data, identity_of, llm_request_attributes, llm_response_attributes,
    runner_agent_of,
};
use super::{AnalyticsPlugin, EventData, PLUGIN_NAME, Payload, PluginState, USER_AUTHOR};
use crate::context::ContextError;
use crate::error::Error;
use crate::event_types::{
    EVENT_AGENT_COMPLETED, EVENT_AGENT_ERROR, EVENT_AGENT_RESPONSE, EVENT_AGENT_STARTING,
    EVENT_INVOCATION_COMPLETED, EVENT_INVOCATION_ERROR, EVENT_INVOCATION_STARTING, EVENT_LLM_ERROR,
    EVENT_LLM_REQUEST, EVENT_LLM_RESPONSE, EVENT_TOOL_COMPLETED, EVENT_TOOL_ERROR,
    EVENT_TOOL_STARTING, EVENT_USER_MESSAGE_RECEIVED,
};
use crate::events::TOOL_ORIGIN_UNKNOWN;
use crate::trace::{
    CallbackCtx, Identity, RowSpan, TraceError, TraceManager, UnclosedAgentSpan,
    WORKFLOW_BASE_PATH_PREFIX, WORKFLOW_NODE_CANCEL_OUTCOME, WORKFLOW_NODE_COMPLETED_KEY,
    WORKFLOW_NODE_FAILED_OUTCOME, WORKFLOW_NODE_OUTCOME_KEY, llm_key, tool_key,
};

/// The name of the tool that finishes a task.
pub(crate) const FINISH_TASK_TOOL_NAME: &str = "finish_task";

/// The event type `on_event`'s recovery reports.
const LEGACY_EVENT_TYPE: &str = super::LEGACY_EVENT_TYPE;

/// The `PluginConfig` of an enabled plugin: the 12 callbacks over `plugin`, `close`, and `state`
/// as the typed state slot.
pub(crate) fn plugin_config(
    plugin: &Arc<AnalyticsPlugin>,
    state: Arc<PluginState>,
    close: CloseFn,
) -> PluginConfig {
    PluginConfig {
        close: Some(close),
        state: Some(state),
        ..callbacks_config(PLUGIN_NAME, plugin)
    }
}

/// The 12 callbacks over `plugin` under `name`, without close or state (test plugins register
/// the same callbacks).
///
/// Each callback builds its [`CallbackCtx`] and runs the synchronous body right away, before
/// the returned future is awaited, so all trace-manager work and row building is done (and
/// every row appended) before any `.await`.
pub(crate) fn callbacks_config(name: &str, plugin: &Arc<AnalyticsPlugin>) -> PluginConfig {
    let p = Arc::clone(plugin);
    let on_user_message: OnUserMessageCallback =
        Arc::new(move |ctx: InvocationContext, msg: Content| {
            let cctx = CallbackCtx::from_invocation(&ctx);
            p.guard(EVENT_USER_MESSAGE_RECEIVED, || {
                p.on_user_message(&cctx, &msg)
            });
            async { Ok(None) }.boxed()
        });
    let p = Arc::clone(plugin);
    let before_run: BeforeRunCallback = Arc::new(move |ctx: InvocationContext| {
        let cctx = CallbackCtx::from_invocation(&ctx);
        p.guard(EVENT_INVOCATION_STARTING, || p.before_run(&cctx));
        async { Ok(None) }.boxed()
    });
    let p = Arc::clone(plugin);
    let after_run: AfterRunCallback = Arc::new(move |ctx: InvocationContext| {
        let cctx = CallbackCtx::from_invocation(&ctx);
        p.after_run(&cctx);
        // After-run ends with the flush of the run's rows, inside its recovery.
        let processor = p.processor.clone().filter(|_| p.config.flush_on_run_end());
        async move {
            if let Some(processor) = processor {
                processor.flush_run_end().await;
            }
        }
        .boxed()
    });
    let p = Arc::clone(plugin);
    let on_event: OnEventCallback = Arc::new(move |ctx: InvocationContext, ev: Event| {
        let cctx = CallbackCtx::from_invocation(&ctx);
        p.on_event(&cctx, &ev);
        async { Ok(None) }.boxed()
    });
    let p = Arc::clone(plugin);
    let before_agent: BeforeAgentCallback = Arc::new(move |ctx: Context| {
        let cctx = CallbackCtx::from_context(&ctx);
        p.before_agent(&cctx);
        async { Ok(None) }.boxed()
    });
    let p = Arc::clone(plugin);
    let after_agent: AfterAgentCallback = Arc::new(move |ctx: Context| {
        let cctx = CallbackCtx::from_context(&ctx);
        p.after_agent(&cctx);
        async { Ok(None) }.boxed()
    });
    let p = Arc::clone(plugin);
    let before_model: BeforeModelCallback = Arc::new(move |ctx: Context, req: LlmRequest| {
        let cctx = CallbackCtx::from_context(&ctx);
        p.guard(EVENT_LLM_REQUEST, || p.before_model(&cctx, &req));
        async move { (req, Ok(None)) }.boxed()
    });
    let p = Arc::clone(plugin);
    let after_model: AfterModelCallback = Arc::new(
        move |ctx: Context, res: LlmResponse, err: Option<Arc<ModelError>>| {
            let cctx = CallbackCtx::from_context(&ctx);
            p.after_model(&cctx, &res, err.as_deref());
            async move { (res, Ok(None)) }.boxed()
        },
    );
    let p = Arc::clone(plugin);
    let on_model_error: OnModelErrorCallback =
        Arc::new(move |ctx: Context, req: LlmRequest, err: Arc<ModelError>| {
            let cctx = CallbackCtx::from_context(&ctx);
            p.on_model_error(&cctx, &req, &err);
            async { Ok(None) }.boxed()
        });
    let p = Arc::clone(plugin);
    let before_tool: BeforeToolCallback = Arc::new(
        move |ctx: Context, t: Arc<dyn Tool>, args: Map<String, Value>| {
            let cctx = CallbackCtx::from_context(&ctx);
            p.before_tool(&cctx, &t, &args);
            async move { (args, Ok(None)) }.boxed()
        },
    );
    let p = Arc::clone(plugin);
    let after_tool: AfterToolCallback = Arc::new(
        move |ctx: Context,
              t: Arc<dyn Tool>,
              args: Map<String, Value>,
              result: Map<String, Value>,
              err: Option<Arc<ToolError>>| {
            let cctx = CallbackCtx::from_context(&ctx);
            p.after_tool(&cctx, &t, &args, &result, err.as_deref());
            async { Ok(None) }.boxed()
        },
    );
    let p = Arc::clone(plugin);
    let on_tool_error: OnToolErrorCallback = Arc::new(
        move |ctx: Context, t: Arc<dyn Tool>, args: Map<String, Value>, err: Arc<ToolError>| {
            let cctx = CallbackCtx::from_context(&ctx);
            p.on_tool_error(&cctx, &t, &args, &err);
            async { Ok(None) }.boxed()
        },
    );
    PluginConfig {
        name: name.to_owned(),
        on_user_message_callback: Some(on_user_message),
        on_event_callback: Some(on_event),
        before_run_callback: Some(before_run),
        after_run_callback: Some(after_run),
        before_agent_callback: Some(before_agent),
        after_agent_callback: Some(after_agent),
        // The plugin registers no on-agent-error callback.
        on_agent_error_callback: None,
        before_model_callback: Some(before_model),
        after_model_callback: Some(after_model),
        on_model_error_callback: Some(on_model_error),
        before_tool_callback: Some(before_tool),
        after_tool_callback: Some(after_tool),
        on_tool_error_callback: Some(on_tool_error),
        close: None,
        state: None,
    }
}

impl AnalyticsPlugin {
    /// Runs `f` under panic recovery for `event_type`.
    ///
    // The user-message, before-run and before-model callbacks have no recovery of their own
    // (only their rows do), so a panic outside a row there (e.g. in the user-message signal
    // rows) is recovered here too, as a dropped row of the callback's event type, so it never
    // reaches the agent run.
    pub(crate) fn guard(&self, event_type: &str, f: impl FnOnce()) {
        if let Err(panic) = catch_unwind(AssertUnwindSafe(f)) {
            self.recover_callback(event_type, panic.as_ref());
        }
    }

    /// The trace manager; a plugin without one (a test plugin, a panic recovered by the
    /// callback) writes nothing.
    fn tm(&self) -> Option<Arc<TraceManager>> {
        self.traces.clone()
    }

    /// The user-message callback.
    pub(crate) fn on_user_message(&self, ctx: &CallbackCtx, msg: &Content) {
        let Some(tm) = self.tm() else { return };
        let id = identity_of(ctx);
        let runner = runner_agent_of(ctx);
        let mut span = RowSpan::default();
        self.log_event(
            ctx,
            &id,
            EVENT_USER_MESSAGE_RECEIVED,
            Some(&mut || {
                span = tm.invocation_span(ctx, &id, runner.as_ref());
                span.clone()
            }),
            Some(&mut |d: &mut EventData| {
                d.content = Some(Payload::Content(msg.clone()));
                d.author = USER_AUTHOR.to_owned();
            }),
        );
        self.emit_user_message_signal_events(ctx, &id, msg, &span);
    }

    /// The before-run callback.
    pub(crate) fn before_run(&self, ctx: &CallbackCtx) {
        let Some(tm) = self.tm() else { return };
        let id = identity_of(ctx);
        let runner = runner_agent_of(ctx);
        self.log_event(
            ctx,
            &id,
            EVENT_INVOCATION_STARTING,
            Some(&mut || tm.invocation_span(ctx, &id, runner.as_ref())),
            None,
        );
    }

    /// The after-run callback without its final flush (the callback awaits it).
    pub(crate) fn after_run(&self, ctx: &CallbackCtx) {
        self.guard(EVENT_INVOCATION_COMPLETED, || {
            let Some(tm) = self.tm() else { return };
            let id = identity_of(ctx);
            let (unclosed, mut unrec_err) = tm.pop_unclosed_agent_spans(ctx, &id);
            if unrec_err.is_none() && ctx.adk.is_some() {
                // A canceled invocation failed even when no span is left open, for example
                // when canceled before any agent started. The context's error is `Canceled`
                // whatever the cause.
                unrec_err = ctx
                    .err()
                    .map(|_| Arc::new(Error::Context(ContextError::Canceled)));
            }
            self.log_agent_errors(ctx, &id, &unclosed, unrec_err.as_ref());

            if unrec_err.is_some() || !unclosed.is_empty() {
                self.log_event(
                    ctx,
                    &id,
                    EVENT_INVOCATION_ERROR,
                    Some(&mut || tm.end_invocation(ctx, &id)),
                    Some(&mut |d: &mut EventData| {
                        d.failed = true;
                        if let Some(err) = &unrec_err {
                            d.err_msg = err.to_string();
                            d.content = Some(error_traceback(err));
                        }
                    }),
                );
            } else {
                self.log_event(
                    ctx,
                    &id,
                    EVENT_INVOCATION_COMPLETED,
                    Some(&mut || tm.end_invocation(ctx, &id)),
                    None,
                );
            }
        });
    }

    /// Logs the `AGENT_ERROR` rows of the unclosed agent spans of the run's end, with the
    /// invocation's terminal error `err`.
    pub(crate) fn log_agent_errors(
        &self,
        ctx: &CallbackCtx,
        id: &Identity,
        unclosed: &[UnclosedAgentSpan],
        err: Option<&TraceError>,
    ) {
        for u in unclosed {
            let mut agent_id = id.clone();
            if !u.agent_name.is_empty() {
                agent_id.agent_name = u.agent_name.clone();
            }
            agent_id.branch = u.branch.clone();
            agent_id.path = u.path.clone();
            self.log_event(
                ctx,
                &agent_id,
                EVENT_AGENT_ERROR,
                Some(&mut || u.span.clone()),
                Some(&mut |d: &mut EventData| {
                    d.failed = true;
                    d.timestamp = u.timestamp;
                    if let Some(agent_err) = u.err.as_ref().or(err) {
                        d.err_msg = agent_err.to_string();
                        d.content = Some(error_traceback(agent_err));
                    }
                    d.author = u.agent_name.clone();
                }),
            );
        }
    }

    /// Logs the `AGENT_ERROR` rows of the purged agent spans.
    pub(crate) fn log_purged_agents(
        &self,
        ctx: &CallbackCtx,
        id: &Identity,
        purged: &[UnclosedAgentSpan],
    ) {
        self.log_agent_errors(ctx, id, purged, None);
    }

    /// The event callback.
    pub(crate) fn on_event(&self, ctx: &CallbackCtx, ev: &Event) {
        self.guard(LEGACY_EVENT_TYPE, || {
            let Some(tm) = self.tm() else { return };
            let id = identity_of(ctx);
            let mut ev_id = id.clone();
            if let Some(ni) = &ev.node_info
                && !ni.path.is_empty()
            {
                ev_id.path = ni.path.clone();
            }
            let content = ev.response.content.as_ref();
            let responds = content.is_some() || ev.output.is_some();
            if ev.response.partial {
                tm.record_streamed(ctx, &ev_id, &ev.author, &ev.branch, responds);
            } else if responds {
                tm.complete_response(
                    ctx,
                    &ev_id,
                    &ev.author,
                    &ev.branch,
                    &function_response_calls(content),
                );
            } else {
                tm.clear_last_partial(ctx, &ev_id);
            }
            if let Some((workflow_name, failed)) = node_completion(ev) {
                let path = ev.node_info.as_ref().map_or("", |ni| ni.path.as_str());
                tm.record_node_completion(ctx, &ev_id, &workflow_name, path, &ev.branch, failed);
            }
            self.emit_session_event_signals(ctx, ev);
            if !ev.response.partial
                && !ev.author.is_empty()
                && ev.author != USER_AUTHOR
                && has_task_completed_response(content)
            {
                let mut task_id = ev_id.clone();
                task_id.agent_name = ev.author.clone();
                task_id.branch = ev.branch.clone();
                if let Some(span) =
                    tm.pop_task_agent_span(ctx, &task_id, &function_response_calls(content))
                {
                    self.log_event(
                        ctx,
                        &task_id,
                        EVENT_AGENT_COMPLETED,
                        Some(&mut || span.clone()),
                        None,
                    );
                }
            }
        });
    }

    /// The before-agent callback.
    pub(crate) fn before_agent(&self, ctx: &CallbackCtx) {
        self.guard(EVENT_AGENT_STARTING, || {
            let Some(tm) = self.tm() else { return };
            let id = identity_of(ctx);
            if let Some(span) = tm.pop_delegating_agent_span(ctx, &id) {
                self.log_event(
                    ctx,
                    &id,
                    EVENT_AGENT_COMPLETED,
                    Some(&mut || span.clone()),
                    None,
                );
            }
            // push_agent_span runs before the AGENT_STARTING row so that log_purged_agents can
            // write the AGENT_ERROR rows of the previous attempt before the new attempt's row,
            // with their own timestamps.
            let (span, purged) = tm.push_agent_span(ctx, &id);
            self.log_purged_agents(ctx, &id, &purged);
            self.log_event(
                ctx,
                &id,
                EVENT_AGENT_STARTING,
                Some(&mut || span.clone()),
                Some(&mut |d: &mut EventData| {
                    // The agent is looked up by name in the tree of the agent that the runner
                    // runs (the callback context has no agent of its own).
                    let (content, attrs) = match tm.agent_named(ctx, &id) {
                        Some(a) => agent_starting_data(a.as_ref()),
                        None => (None, Map::new()),
                    };
                    d.content = content.map(Payload::Json);
                    d.attributes = attrs;
                }),
            );
        });
    }

    /// The after-agent callback.
    pub(crate) fn after_agent(&self, ctx: &CallbackCtx) {
        self.guard(EVENT_AGENT_COMPLETED, || {
            let Some(tm) = self.tm() else { return };
            let id = identity_of(ctx);
            // A workflow agent ends only after every node run that its scheduler started, so
            // the failed attempts of nodes that it started again are purged here, before its
            // AGENT_COMPLETED row.
            let purged = tm.end_restarted_node_runs(ctx, &id);
            self.log_purged_agents(ctx, &id, &purged);
            self.log_event(
                ctx,
                &id,
                EVENT_AGENT_COMPLETED,
                Some(&mut || tm.pop_agent_span(ctx, &id)),
                None,
            );
        });
    }

    /// The before-model callback.
    pub(crate) fn before_model(&self, ctx: &CallbackCtx, req: &LlmRequest) {
        let Some(tm) = self.tm() else { return };
        let id = identity_of(ctx);
        self.log_event(
            ctx,
            &id,
            EVENT_LLM_REQUEST,
            Some(&mut || tm.push_llm_span(ctx, &id)),
            Some(&mut |d: &mut EventData| {
                d.content = Some(Payload::LlmRequest(Box::new(req.clone())));
                d.attributes = llm_request_attributes(req);
            }),
        );
    }

    /// The after-model callback.
    pub(crate) fn after_model(
        &self,
        ctx: &CallbackCtx,
        res: &LlmResponse,
        err: Option<&ModelError>,
    ) {
        self.guard(EVENT_LLM_RESPONSE, || {
            let Some(tm) = self.tm() else { return };
            let id = identity_of(ctx);
            if res.partial {
                // A streamed chunk times the first token when it carries generated content;
                // the final response alone is written, so that token usage is counted once.
                if has_generated_content(res) {
                    tm.record_first_token(ctx, &id);
                }
                return;
            }
            let key = llm_key(&id);
            match err {
                Some(err) => tm.record_error(ctx, &id, &key, trace_error_of_model(err)),
                None => tm.clear_error(ctx, &id, &key),
            }
            self.log_event(
                ctx,
                &id,
                EVENT_LLM_RESPONSE,
                Some(&mut || tm.pop_llm_span(ctx, &id, true)),
                Some(&mut |d: &mut EventData| {
                    let err_text = err.map(ToString::to_string).unwrap_or_default();
                    if err.is_some() {
                        d.failed = true;
                    }
                    if res.content.is_some() || res.usage_metadata.is_some() {
                        d.content = Some(Payload::LlmResponse(Box::new(res.clone())));
                    }
                    d.attributes = llm_response_attributes(res);
                    d.custom_metadata =
                        (!res.custom_metadata.is_empty()).then(|| res.custom_metadata.clone());
                    d.err_msg = if err_text.is_empty() {
                        res.error_message.clone()
                    } else {
                        err_text
                    };
                }),
            );
        });
    }

    /// The model-error callback.
    pub(crate) fn on_model_error(&self, ctx: &CallbackCtx, req: &LlmRequest, err: &ModelError) {
        self.guard(EVENT_LLM_ERROR, || {
            let Some(tm) = self.tm() else { return };
            let id = identity_of(ctx);
            tm.record_error(ctx, &id, &llm_key(&id), trace_error_of_model(err));
            self.log_event(
                ctx,
                &id,
                EVENT_LLM_ERROR,
                Some(&mut || tm.pop_llm_span(ctx, &id, false)),
                Some(&mut |d: &mut EventData| {
                    d.failed = true;
                    d.err_msg = err.to_string();
                    if !req.model.is_empty() {
                        d.attributes
                            .insert("model".to_owned(), Value::from(req.model.clone()));
                    }
                }),
            );
        });
    }

    /// The before-tool callback.
    pub(crate) fn before_tool(
        &self,
        ctx: &CallbackCtx,
        t: &Arc<dyn Tool>,
        args: &Map<String, Value>,
    ) {
        self.guard(EVENT_TOOL_STARTING, || {
            let Some(tm) = self.tm() else { return };
            let id = identity_of(ctx);
            let call_id = function_call_id_of(ctx);
            let name = tool_name(t);
            self.log_event(
                ctx,
                &id,
                EVENT_TOOL_STARTING,
                Some(&mut || tm.push_tool_span(ctx, &id, &tool_key(&id, &call_id, &name))),
                Some(&mut |d: &mut EventData| {
                    let origin = self.resolve_tool_origin(ctx, &id, t, args);
                    d.content = Some(Payload::Json(tool_content(&name, "args", args, origin)));
                    d.attributes.insert(
                        "tool_description".to_owned(),
                        Value::from(t.description().to_owned()),
                    );
                    d.attributes.insert(
                        "is_long_running".to_owned(),
                        Value::from(t.is_long_running()),
                    );
                    d.function_call_id = call_id.clone();
                }),
            );
        });
    }

    /// The after-tool callback.
    pub(crate) fn after_tool(
        &self,
        ctx: &CallbackCtx,
        t: &Arc<dyn Tool>,
        args: &Map<String, Value>,
        result: &Map<String, Value>,
        err: Option<&ToolError>,
    ) {
        self.guard(EVENT_TOOL_COMPLETED, || {
            let Some(tm) = self.tm() else { return };
            let id = identity_of(ctx);
            let call_id = function_call_id_of(ctx);
            let name = tool_name(t);
            let (span, purged, closed_by_error) =
                tm.pop_tool_span(ctx, &id, &tool_key(&id, &call_id, &name), false);
            // An agent that the call ran as a tool and that failed ends with it.
            self.log_purged_agents(ctx, &id, &purged);
            // A tool that waits for or is rejected by the user's confirmation completes with
            // the error that ADK sends the model as its result (as adk-python).
            let confirmation = err.is_some_and(is_tool_confirmation_outcome);
            if let Some(err) = err
                && !confirmation
            {
                // OnToolErrorCallback normally owns the TOOL_ERROR row; when an earlier
                // plugin short-circuited it, the failed call still gets its terminal row.
                if !closed_by_error {
                    self.log_tool_error(
                        ctx,
                        &id,
                        t,
                        &mut || span.clone(),
                        &name,
                        &call_id,
                        args,
                        err,
                    );
                }
                return;
            }
            let result = match err {
                Some(err) if confirmation => {
                    let mut m = Map::new();
                    m.insert("error".to_owned(), Value::from(err.to_string()));
                    m
                }
                _ => {
                    if name == FINISH_TASK_TOOL_NAME {
                        // The finish_task response event that follows reaches the event
                        // callback without the context of the activation that made the call.
                        tm.note_task_finished(ctx, &id, &call_id);
                    }
                    result.clone()
                }
            };
            self.log_event(
                ctx,
                &id,
                EVENT_TOOL_COMPLETED,
                Some(&mut || span.clone()),
                Some(&mut |d: &mut EventData| {
                    let origin = self.resolve_tool_origin(ctx, &id, t, args);
                    let mut content = tool_content(&name, "result", &result, origin);
                    // A tool's empty result map (e.g. `transfer_to_agent` and `exit_loop`,
                    // which return no result) is recorded as `"result": null`, as ADK does.
                    if result.is_empty()
                        && let Value::Object(m) = &mut content
                    {
                        m.insert("result".to_owned(), Value::Null);
                    }
                    d.content = Some(Payload::Json(content));
                    d.function_call_id = call_id.clone();
                }),
            );
            if confirmation {
                // The tool has not run: AGENT_RESPONSE is written when the confirmed call runs.
                return;
            }
            if self.final_response_tools.contains(&name) {
                self.log_event(
                    ctx,
                    &id,
                    EVENT_AGENT_RESPONSE,
                    Some(&mut || tm.event_span(ctx, &id, &id.agent_name, &id.branch)),
                    Some(&mut |d: &mut EventData| {
                        let mut content = Map::new();
                        content.insert("response".to_owned(), Value::Object(args.clone()));
                        d.content = Some(Payload::Json(Value::Object(content)));
                        d.attributes
                            .insert("source_tool".to_owned(), Value::from(name.clone()));
                        d.function_call_id = call_id.clone();
                    }),
                );
            }
        });
    }

    /// The tool-error callback.
    pub(crate) fn on_tool_error(
        &self,
        ctx: &CallbackCtx,
        t: &Arc<dyn Tool>,
        args: &Map<String, Value>,
        err: &ToolError,
    ) {
        self.guard(EVENT_TOOL_ERROR, || {
            if is_tool_confirmation_outcome(err) {
                // The call waits for or was rejected by the user's confirmation and has not
                // failed. Its span stays open for the TOOL_COMPLETED row of after_tool.
                return;
            }
            let Some(tm) = self.tm() else { return };
            let id = identity_of(ctx);
            let call_id = function_call_id_of(ctx);
            let name = tool_name(t);
            let (span, purged, _) =
                tm.pop_tool_span(ctx, &id, &tool_key(&id, &call_id, &name), true);
            // An agent that the call ran as a tool and that failed ends with it.
            self.log_purged_agents(ctx, &id, &purged);
            self.log_tool_error(
                ctx,
                &id,
                t,
                &mut || span.clone(),
                &name,
                &call_id,
                args,
                err,
            );
        });
    }

    /// Logs the `TOOL_ERROR` row of tool `t`'s call `call_id` failing with `err`.
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
        self.log_event(
            ctx,
            id,
            EVENT_TOOL_ERROR,
            Some(span_fn),
            Some(&mut |d: &mut EventData| {
                let origin = self.resolve_tool_origin(ctx, id, t, args);
                d.content = Some(Payload::Json(tool_content(name, "args", args, origin)));
                d.failed = true;
                d.err_msg = err.to_string();
                d.function_call_id = call_id.to_owned();
            }),
        );
    }
}

/// The content `{"error_traceback": <err text>}`.
fn error_traceback(err: &TraceError) -> Payload {
    let mut m = Map::new();
    m.insert("error_traceback".to_owned(), Value::from(err.to_string()));
    Payload::Json(Value::Object(m))
}

/// The tool call ID of an ADK context, or "".
fn function_call_id_of(ctx: &CallbackCtx) -> String {
    ctx.adk
        .as_ref()
        .map(gadk_agent::Context::function_call_id)
        .unwrap_or_default()
}

/// `Some((workflow_name, failed))` for a workflow node-completion event.
pub(crate) fn node_completion(ev: &Event) -> Option<(String, bool)> {
    if ev.response.partial {
        return None;
    }
    let path = ev.node_info.as_ref().map(|ni| ni.path.as_str())?;
    if path.is_empty() {
        return None;
    }
    let meta = &ev.response.custom_metadata;
    let outcome = meta.get(WORKFLOW_NODE_OUTCOME_KEY).and_then(Value::as_str);
    let completed = meta
        .get(WORKFLOW_NODE_COMPLETED_KEY)
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if outcome.is_none() && !completed {
        return None;
    }
    let parent = path.rfind('/').map_or("", |slash| &path[..slash]);
    let mut workflow_name: Option<&str> = None;
    for (key, value) in meta {
        let Some(name) = key.strip_prefix(WORKFLOW_BASE_PATH_PREFIX) else {
            continue;
        };
        if value.as_str() != Some(parent) {
            continue;
        }
        if workflow_name.is_some() {
            return None;
        }
        workflow_name = Some(name);
    }
    let workflow_name = workflow_name.filter(|n| !n.is_empty())?;
    let failed = matches!(
        outcome,
        Some(WORKFLOW_NODE_FAILED_OUTCOME | WORKFLOW_NODE_CANCEL_OUTCOME)
    );
    Some((workflow_name.to_owned(), failed))
}

/// Whether the first `finish_task` function response says `"result": "Task completed."`.
pub(crate) fn has_task_completed_response(c: Option<&Content>) -> bool {
    let Some(c) = c else { return false };
    for part in &c.parts {
        let Some(fr) = &part.function_response else {
            continue;
        };
        if fr.name == FINISH_TASK_TOOL_NAME {
            return fr.response.get("result").and_then(Value::as_str) == Some("Task completed.");
        }
    }
    false
}

/// The function call ID, or else the name, of each function response.
pub(crate) fn function_response_calls(c: Option<&Content>) -> Vec<String> {
    let Some(c) = c else { return Vec::new() };
    c.parts
        .iter()
        .filter_map(|part| part.function_response.as_ref())
        .filter_map(|fr| {
            let call = if fr.id.is_empty() { &fr.name } else { &fr.id };
            (!call.is_empty()).then(|| call.clone())
        })
        .collect()
}

/// Whether the response has a part with text, inline or file data, a function call or
/// response, or executable code or its result.
pub(crate) fn has_generated_content(res: &LlmResponse) -> bool {
    let Some(content) = &res.content else {
        return false;
    };
    content.parts.iter().any(|part| {
        !part.text.is_empty()
            || part.inline_data.is_some()
            || part.function_call.is_some()
            || part.function_response.is_some()
            || part.file_data.is_some()
            || part.executable_code.is_some()
            || part.code_execution_result.is_some()
    })
}

/// Whether `err` is `ToolError::ConfirmationRequired` / `ConfirmationRejected` anywhere in its
/// chain (the flow wraps them as `error tool "<name>" <error>`).
pub(crate) fn is_tool_confirmation_outcome(err: &ToolError) -> bool {
    let mut link: Option<&(dyn StdError + 'static)> = Some(err);
    while let Some(e) = link {
        match e.downcast_ref::<ToolError>() {
            Some(ToolError::ConfirmationRequired | ToolError::ConfirmationRejected) => {
                return true;
            }
            // `#[error(transparent)]` hides the boxed error itself from `source()`.
            Some(ToolError::Execution(inner)) => {
                link = Some(inner.as_ref());
                continue;
            }
            _ => {}
        }
        link = e.source();
    }
    false
}

/// `t.name()`, or "" if it panics.
pub(crate) fn tool_name(t: &Arc<dyn Tool>) -> String {
    catch_unwind(AssertUnwindSafe(|| t.name().to_owned())).unwrap_or_default()
}

/// The content of a tool row: `{"tool": name, <payload_key>: payload, "tool_origin": origin}`
/// (an empty `origin` reads as the unknown origin).
pub(crate) fn tool_content(
    name: &str,
    payload_key: &str,
    payload: &Map<String, Value>,
    origin: &str,
) -> Value {
    let mut m = Map::new();
    m.insert("tool".to_owned(), Value::from(name));
    m.insert(payload_key.to_owned(), Value::Object(payload.clone()));
    let origin = if origin.is_empty() {
        TOOL_ORIGIN_UNKNOWN
    } else {
        origin
    };
    m.insert("tool_origin".to_owned(), Value::from(origin));
    Value::Object(m)
}

/// The error of a model / tool callback as the trace manager records it (a canceled context
/// stays recognizable by `trace::causes::is_canceled`).
pub(crate) fn trace_error_of_model(err: &ModelError) -> TraceError {
    trace_error_of(err)
}

/// As [`trace_error_of_model`] for a tool error (the trace-manager tests record tool errors
/// directly; the callbacks record none).
#[cfg(test)]
pub(crate) fn trace_error_of_tool(err: &ToolError) -> TraceError {
    trace_error_of(err)
}

/// The error's text as a [`TraceError`]; a cancellation or deadline in its chain (gadk's
/// `AgentError::Cancelled` / `DeadlineExceeded`, a [`ContextError`]) becomes
/// `ContextError::Canceled` / `DeadlineExceeded` in the error's tree. Also maps a context's
/// `CancelCause::Error` (`crate::trace::span::trace_error_of_cause`).
pub(crate) fn trace_error_of(err: &(dyn StdError + 'static)) -> TraceError {
    let text = err.to_string();
    let mut found = None;
    let mut link: Option<&(dyn StdError + 'static)> = Some(err);
    while let Some(e) = link {
        if let Some(c) = e.downcast_ref::<ContextError>() {
            found = Some(*c);
            break;
        }
        // Transparent / plain-field variants hide the boxed error from `source()`.
        let inner: Option<&(dyn StdError + 'static)> =
            if let Some(a) = e.downcast_ref::<AgentError>() {
                match a {
                    AgentError::Cancelled => {
                        found = Some(ContextError::Canceled);
                        break;
                    }
                    AgentError::DeadlineExceeded => {
                        found = Some(ContextError::DeadlineExceeded);
                        break;
                    }
                    AgentError::Other(b) | AgentError::Workflow(b) => Some(b.as_ref()),
                    _ => None,
                }
            } else if let Some(ToolError::Execution(b)) = e.downcast_ref::<ToolError>() {
                Some(b.as_ref())
            } else if let Some(ModelError::Other(b) | ModelError::Transport(b)) =
                e.downcast_ref::<ModelError>()
            {
                Some(b.as_ref())
            } else {
                None
            };
        link = inner.or_else(|| e.source());
    }
    Arc::new(match found {
        Some(c) if text == c.to_string() => Error::Context(c),
        Some(c) => Error::wrap_text(text, Error::Context(c)),
        None => Error::msg(text),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use gadk_genai::{FunctionResponse, Part};
    use gadk_session::NodeInfo;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;
    use crate::trace::causes::is_canceled;

    fn fr(id: &str, name: &str, response: Value) -> Part {
        Part {
            function_response: Some(FunctionResponse {
                id: id.into(),
                name: name.into(),
                response: response.as_object().cloned().unwrap_or_default(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn content(parts: Vec<Part>) -> Content {
        Content {
            parts,
            role: "user".into(),
        }
    }

    #[test]
    fn function_response_calls_prefer_the_call_id() {
        let c = content(vec![
            fr("call-1", "a", json!({})),
            fr("", "b", json!({})),
            fr("", "", json!({})),
            Part::from_text("x"),
        ]);
        assert_eq!(function_response_calls(Some(&c)), vec!["call-1", "b"]);
        assert!(function_response_calls(None).is_empty());
    }

    #[test]
    fn task_completed_response_reads_the_first_finish_task_response() {
        let done = content(vec![fr(
            "c",
            FINISH_TASK_TOOL_NAME,
            json!({"result": "Task completed."}),
        )]);
        assert!(has_task_completed_response(Some(&done)));
        let other = content(vec![
            fr("c", FINISH_TASK_TOOL_NAME, json!({"result": "nope"})),
            fr(
                "d",
                FINISH_TASK_TOOL_NAME,
                json!({"result": "Task completed."}),
            ),
        ]);
        assert!(!has_task_completed_response(Some(&other)));
        assert!(!has_task_completed_response(None));
    }

    #[test]
    fn node_completion_needs_a_unique_workflow_base_path() {
        let mut ev = Event {
            node_info: Some(NodeInfo {
                path: "wf@1/step@2".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(node_completion(&ev), None, "no outcome");
        let md = &mut ev.response.custom_metadata;
        md.insert(WORKFLOW_NODE_OUTCOME_KEY.into(), json!("failed"));
        md.insert(format!("{WORKFLOW_BASE_PATH_PREFIX}wf"), json!("wf@1"));
        assert_eq!(node_completion(&ev), Some(("wf".into(), true)));
        ev.response
            .custom_metadata
            .insert(WORKFLOW_NODE_OUTCOME_KEY.into(), json!("ok"));
        assert_eq!(node_completion(&ev), Some(("wf".into(), false)));
        ev.response
            .custom_metadata
            .insert(format!("{WORKFLOW_BASE_PATH_PREFIX}other"), json!("wf@1"));
        assert_eq!(node_completion(&ev), None, "ambiguous");
        ev.response.partial = true;
        assert_eq!(node_completion(&ev), None, "partial");
    }

    #[test]
    fn generated_content_excludes_metadata_only_chunks() {
        let mut res = LlmResponse::default();
        assert!(!has_generated_content(&res));
        res.content = Some(Content {
            parts: vec![Part::default()],
            role: "model".into(),
        });
        assert!(!has_generated_content(&res));
        res.content = Some(Content::from_text("hi", "model"));
        assert!(has_generated_content(&res));
    }

    #[derive(Debug)]
    struct Named(ToolError);
    impl std::fmt::Display for Named {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "error tool \"t\" {}", self.0)
        }
    }
    impl StdError for Named {
        fn source(&self) -> Option<&(dyn StdError + 'static)> {
            Some(&self.0)
        }
    }

    #[test]
    fn confirmation_outcomes_are_found_through_the_flow_wrap() {
        assert!(is_tool_confirmation_outcome(
            &ToolError::ConfirmationRequired
        ));
        assert!(is_tool_confirmation_outcome(
            &ToolError::ConfirmationRejected
        ));
        let wrapped = ToolError::Execution(Box::new(Named(ToolError::ConfirmationRequired)));
        assert!(is_tool_confirmation_outcome(&wrapped));
        assert!(!is_tool_confirmation_outcome(&ToolError::InvalidArgument(
            "x".into()
        )));
    }

    #[test]
    fn tool_content_defaults_the_origin_to_unknown() {
        let args = json!({"a": 1}).as_object().cloned().unwrap();
        assert_eq!(
            tool_content("t", "args", &args, ""),
            json!({"tool": "t", "args": {"a": 1}, "tool_origin": "UNKNOWN"})
        );
        assert_eq!(
            tool_content("t", "result", &args, "LOCAL"),
            json!({"tool": "t", "result": {"a": 1}, "tool_origin": "LOCAL"})
        );
    }

    #[test]
    fn trace_errors_keep_text_and_cancellation() {
        let e = trace_error_of_model(&ModelError::Other("model failure 404".into()));
        assert_eq!(e.to_string(), "model failure 404");
        assert!(!is_canceled(&e));
        let e = trace_error_of_tool(&ToolError::Execution(Box::new(AgentError::Cancelled)));
        assert_eq!(e.to_string(), "context canceled");
        assert!(is_canceled(&e));
        let e = trace_error_of_model(&ModelError::Other(Box::new(AgentError::Cancelled)));
        assert!(is_canceled(&e));
    }
}
