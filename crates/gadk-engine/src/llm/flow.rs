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

//! Port of Go internal/llminternal/base_flow.go: the `Flow` struct and processor types, and the
//! step loop (`Run`, `runOneStep`, `callLLM`, `generateContent`).

use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    sync::Arc,
};

use futures::{StreamExt, future::BoxFuture, stream::BoxStream};
use gadk_agent::{
    AfterModelCallback, AfterToolCallback, Agent, AgentError, BeforeModelCallback,
    BeforeToolCallback, Context, ContextDelta, EventStream, InvocationContext,
    OnModelErrorCallback, OnToolErrorCallback, StreamingMode, Tool, tool_from_request,
};
use gadk_genai::Content;
use gadk_model::{Llm, LlmRequest, LlmResponse, ModelError};
use gadk_session::{Event, EventActions};
use serde_json::{Map, Value};

use crate::{
    llm::{
        functions::{generate_request_confirmation_event, handle_function_calls},
        live::reconnect::LiveReconnectPolicy,
        processors::{
            output_schema::{
                create_final_model_response_event, retrieve_structured_model_response,
            },
            tools::{tool_preprocess, toolset_preprocess},
        },
        transfer::transfer_target_agents,
    },
    telemetry::generate_content_span,
    tool::utils::{function_calls, populate_client_function_call_id},
    workflow::route::go_quote,
};

/// Port of Go `func(ctx agent.InvocationContext, req *model.LLMRequest, f *Flow) iter.Seq2[*session.Event, error]`.
/// Returns the events the processor emits; the flow yields them in order.
pub(crate) type RequestProcessor = for<'a> fn(
    &'a InvocationContext,
    &'a mut LlmRequest,
    &'a Flow,
) -> BoxFuture<'a, Result<Vec<Event>, AgentError>>;

/// Port of Go `func(ctx agent.InvocationContext, req *model.LLMRequest, resp *model.LLMResponse) error`.
pub(crate) type ResponseProcessor = for<'a> fn(
    &'a InvocationContext,
    &'a LlmRequest,
    &'a mut LlmResponse,
) -> Result<(), AgentError>;

/// Port of Go internal/llminternal/base_flow.go `Flow`.
#[derive(Default)]
pub(crate) struct Flow {
    pub(crate) model: Option<Arc<dyn Llm>>,
    /// Go `Flow.Tools`. The tools processor (slot 2) rewrites it on every step
    /// (`tools_processor.go:49`), and the request-confirmation processor and Task 11's
    /// `tool_preprocess` read it. Interior mutability is needed because processors get
    /// `&Flow` (C5 D5). Never hold the guard across an `.await`. There is no `toolsets`
    /// field: the processors read toolsets through `reveal(ctx.agent())` exactly as Go does.
    pub(crate) tools: parking_lot::RwLock<Vec<Arc<dyn Tool>>>,
    pub(crate) request_processors: Vec<RequestProcessor>,
    pub(crate) response_processors: Vec<ResponseProcessor>,
    pub(crate) before_model_callbacks: Vec<BeforeModelCallback>,
    pub(crate) after_model_callbacks: Vec<AfterModelCallback>,
    pub(crate) on_model_error_callbacks: Vec<OnModelErrorCallback>,
    pub(crate) before_tool_callbacks: Vec<BeforeToolCallback>,
    pub(crate) after_tool_callbacks: Vec<AfterToolCallback>,
    pub(crate) on_tool_error_callbacks: Vec<OnToolErrorCallback>,
    /// Go `Flow.reconnect`: bounds `run_live`'s reconnect attempts. `None` means the defaults;
    /// only tests set it, to shrink the delays.
    pub(crate) live_reconnect: Option<LiveReconnectPolicy>,
}

/// Port of base_flow.go maxConsecutiveThoughtOnlyTurns: a safety net against a model that only
/// ever "thinks"; deliberately not configurable.
pub(crate) const MAX_CONSECUTIVE_THOUGHT_ONLY_TURNS: usize = 10;

/// Go `ErrModelNotConfigured` text.
pub(crate) const ERR_MODEL_NOT_CONFIGURED: &str =
    "model not configured; ensure Model is set in llmagent.Config";

/// The step's callback actions: shared by the model callbacks and stamped on every model-response
/// event of the step (Go's `stateDelta`/`artifactDelta` maps).
type SharedActions = Arc<parking_lot::Mutex<EventActions>>;

impl Flow {
    /// Port of base_flow.go:129 `(*Flow).Run`: runs steps until a final response, a partial last
    /// event, or too many consecutive thought-only turns.
    pub(crate) fn run(self: Arc<Self>, ctx: InvocationContext) -> EventStream {
        Box::pin(async_stream::try_stream! {
            let mut thought_only_turns = 0usize;
            loop {
                let mut last_event: Option<Event> = None;
                let mut step = self.clone().run_one_step(ctx.clone());
                while let Some(item) = step.next().await {
                    let ev = item?;
                    last_event = Some(ev.clone());
                    // Forward the event first (Go order).
                    yield ev;
                }
                let Some(last) = last_event else { break };
                if last.is_final_response() {
                    // A thought-only turn reports as final but has no answer: call the model
                    // again, up to the bound.
                    if !is_thought_only_turn(Some(&last)) {
                        break;
                    }
                    thought_only_turns += 1;
                    if thought_only_turns >= MAX_CONSECUTIVE_THOUGHT_ONLY_TURNS {
                        let model = self.model.as_ref().map(|m| m.name().to_owned()).unwrap_or_default();
                        tracing::warn!(
                            target: "adk",
                            "adk: model {} produced {} consecutive thought-only turns without an answer (limit {}) for agent {} (invocation {}); giving up and returning the last thinking event",
                            go_quote(&model),
                            thought_only_turns,
                            MAX_CONSECUTIVE_THOUGHT_ONLY_TURNS,
                            go_quote(ctx.agent().name()),
                            go_quote(ctx.invocation_id())
                        );
                        break;
                    }
                } else {
                    thought_only_turns = 0;
                }
                if last.response.partial {
                    // A producer ended its stream without a terminal aggregate: the turn was
                    // truncated, so stop instead of looping again.
                    tracing::warn!(
                        target: "adk",
                        "adk: agent {} (invocation {}): step ended on a partial event from {}; the producer did not close its stream with an aggregated final event, so the turn will not appear in session history",
                        go_quote(ctx.agent().name()),
                        go_quote(ctx.invocation_id()),
                        go_quote(&last.author)
                    );
                    break;
                }
            }
        })
    }

    /// Port of base_flow.go:785 `(*Flow).runOneStep`: preprocess, the model call (with its
    /// callbacks), function calls, the confirmation and structured-output events, and the
    /// `transfer_to_agent` dispatch.
    pub(crate) fn run_one_step(self: Arc<Self>, ctx: InvocationContext) -> EventStream {
        Box::pin(async_stream::try_stream! {
            'step: {
                let model = self.model.clone().ok_or_else(|| {
                    AgentError::Config(format!(
                        "agent {}: {}",
                        go_quote(ctx.agent().name()),
                        ERR_MODEL_NOT_CONFIGURED
                    ))
                })?;

                let mut req = LlmRequest {
                    model: model.name().to_owned(),
                    ..Default::default()
                };
                {
                    // Go yields each processor's events as they are produced, so they are in the
                    // session before a later processor (contents) reads it; regression:
                    // TestDelegation_06_ChatToTaskHITLResume (the resumed tool's response comes
                    // from the confirmation processor and must reach the same step's request).
                    let mut pre = self.preprocess(&ctx, &mut req);
                    while let Some(ev) = pre.next().await {
                        yield ev?;
                    }
                }
                if ctx.ended() {
                    break 'step;
                }

                let actions: SharedActions = Arc::new(parking_lot::Mutex::new(EventActions {
                    state_delta: Some(Map::new()),
                    artifact_delta: Some(BTreeMap::new()),
                    ..Default::default()
                }));

                // Go callLLM lines 990-1009: before-model callbacks, plugins first. `Some`
                // response short-circuits the model call (and the after-model callbacks).
                let mut short_circuit: Option<LlmResponse> = None;
                if let Some(pm) = ctx.plugin_manager() {
                    let (r, res) = pm
                        .run_before_model(callback_ctx(&ctx, &actions), std::mem::take(&mut req))
                        .await;
                    req = r;
                    match res {
                        Ok(r) => short_circuit = r,
                        Err(e) => {
                            // Go newResponseWithEventID consumes a UUID before the error.
                            let _ = ctx.platform().new_uuid();
                            Err::<(), AgentError>(e)?;
                        }
                    }
                }
                if short_circuit.is_none() {
                    for cb in &self.before_model_callbacks {
                        let (r, res) = cb(callback_ctx(&ctx, &actions), std::mem::take(&mut req)).await;
                        req = r;
                        match res {
                            Ok(Some(resp)) => {
                                short_circuit = Some(resp);
                                break;
                            }
                            Ok(None) => {}
                            Err(e) => {
                                let _ = ctx.platform().new_uuid();
                                Err::<(), AgentError>(e)?;
                            }
                        }
                    }
                }
                let mut responses: BoxStream<'static, Result<(LlmResponse, String), AgentError>> =
                    match short_circuit {
                        Some(resp) => {
                            let id = ctx.platform().new_uuid();
                            futures::stream::once(futures::future::ready(Ok((resp, id)))).boxed()
                        }
                        // The CURRENT request (with callback edits) is what the model sees.
                        None => self
                            .clone()
                            .call_model(ctx.clone(), model.clone(), req.clone(), actions.clone()),
                    };

                while let Some(item) = responses.next().await {
                    let (mut resp, event_id) = item?;
                    self.postprocess(&ctx, &req, &mut resp)?;
                    // Skip a response with no content, no error code and no interruption (the code
                    // executor relies on this to trigger another loop).
                    if resp.content.is_none() && resp.error_code.is_empty() && !resp.interrupted {
                        continue;
                    }
                    let tools = tools_from_request(&req)?;
                    let model_ev = finalize_model_response_event(&ctx, resp, event_id, &tools, &actions);
                    yield model_ev.clone();
                    if model_ev.response.partial {
                        continue;
                    }
                    let Some(fr_ev) = handle_function_calls(&ctx, &self, &tools, &model_ev, None).await? else {
                        continue;
                    };
                    let confirmation_ev = generate_request_confirmation_event(&ctx, &model_ev, &fr_ev);
                    // Function responses before confirmation requests, so consumers that pause for
                    // approval still persist completed tool results.
                    yield fr_ev.clone();
                    if let Some(c) = confirmation_ev {
                        yield c;
                    }
                    let structured = retrieve_structured_model_response(Some(&fr_ev))?;
                    if !structured.is_empty() {
                        yield create_final_model_response_event(&ctx, &structured);
                    }
                    if fr_ev.actions.transfer_to_agent.is_empty() {
                        break 'step;
                    }
                    let next = self
                        .agent_to_run(&ctx, &fr_ev.actions.transfer_to_agent)
                        .ok_or_else(|| {
                            AgentError::other(format!(
                                "failed to find agent: {}",
                                fr_ev.actions.transfer_to_agent
                            ))
                        })?;
                    // Go base_flow.go:904-913: an LlmAgent target (Go's nodeRunner interface,
                    // implemented only by llmAgent → RunLLMAgentAsNode) runs through the node
                    // wrapper with a promoted context and a nil node input; any other agent runs
                    // directly.
                    let mut s = if crate::llm::state::reveal(&*next).is_some() {
                        crate::llm::node_wrapper::run_llm_agent_as_node(
                            ctx.clone().into_context(),
                            next,
                            Value::Null,
                        )
                    } else {
                        next.run(ctx.clone())
                    };
                    // Forward, stopping on the first error.
                    while let Some(item) = s.next().await {
                        yield item?;
                    }
                }
            }
        })
    }

    /// Port of base_flow.go:922 `(*Flow).preprocess`: the request processors, then
    /// `toolPreprocess` over `Flow.tools` and `toolsetPreprocess`. Each processor's events are
    /// yielded before the next processor runs (Go base_flow.go:925-934), so the caller can append
    /// them to the session first. Go yields a failing processor's events before its error; Rust
    /// drops the events of the failing processor (those of earlier processors were yielded).
    pub(crate) fn preprocess<'a>(
        &'a self,
        ctx: &'a InvocationContext,
        req: &'a mut LlmRequest,
    ) -> BoxStream<'a, Result<Event, AgentError>> {
        Box::pin(async_stream::try_stream! {
            for p in &self.request_processors {
                for ev in p(ctx, &mut *req, self).await? {
                    yield ev;
                }
            }
            // Never hold the guard across an .await.
            let tools = self.tools.read().clone();
            tool_preprocess(ctx, &mut *req, &tools)?;
            toolset_preprocess(ctx, &mut *req).await?;
        })
    }

    /// Port of the model half of base_flow.go:986 `(*Flow).callLLM` (lines 1011-1081): the
    /// streaming flag, the LLM-call limit, the model call, the on-model-error and after-model
    /// callbacks. Each item carries the event ID reserved for its response.
    pub(crate) fn call_model(
        self: Arc<Self>,
        ctx: InvocationContext,
        model: Arc<dyn Llm>,
        req: LlmRequest,
        actions: SharedActions,
    ) -> BoxStream<'static, Result<(LlmResponse, String), AgentError>> {
        Box::pin(async_stream::try_stream! {
            // Streaming mode comes from the invocation context's RunConfig (Go issue #586).
            let use_stream = ctx.run_config().streaming_mode == StreamingMode::Sse;
            // Count this call before making it; no limiter means no limit.
            if let Some(l) = ctx.llm_call_limiter() {
                l.increment_and_enforce()?;
            }
            let token = ctx.cancellation_token().clone();
            if token.is_cancelled() {
                Err::<(), AgentError>(AgentError::Cancelled)?;
            }
            let mut inner = generate_content(&ctx, model, req.clone(), use_stream);
            loop {
                let next = tokio::select! {
                    biased;
                    _ = token.cancelled() => None,
                    n = inner.next() => Some(n),
                };
                let Some(next) = next else {
                    Err::<(), AgentError>(AgentError::Cancelled)?;
                    break;
                };
                let Some((item, event_id)) = next else { break };
                let mut resp = match item {
                    Ok(r) => r,
                    Err(model_err) => {
                        let err = Arc::new(model_err);
                        match self.run_on_model_error_callbacks(&ctx, &req, &actions, err.clone()).await {
                            Err(cb_err) => {
                                Err::<(), AgentError>(cb_err)?;
                                break;
                            }
                            Ok(None) => {
                                Err::<(), AgentError>(model_error_into_agent(err))?;
                                break;
                            }
                            Ok(Some(r)) => r,
                        }
                    }
                };
                // Function call IDs are optional in genai; set them for the after-model callbacks.
                populate_client_function_call_id(ctx.platform(), resp.content.as_mut());
                // Go passes the (now nil) model error, so the error argument is always None.
                match self.run_after_model_callbacks(&ctx, resp.clone(), &actions).await? {
                    Some(r) => yield (r, event_id),
                    None => yield (resp, event_id),
                }
            }
        })
    }

    /// Port of base_flow.go:1110 runAfterModelCallbacks: plugins first; the first `Some`/`Err`
    /// wins.
    async fn run_after_model_callbacks(
        &self,
        ctx: &InvocationContext,
        resp: LlmResponse,
        actions: &SharedActions,
    ) -> Result<Option<LlmResponse>, AgentError> {
        if let Some(pm) = ctx.plugin_manager()
            && let Some(r) = pm
                .run_after_model(callback_ctx(ctx, actions), resp.clone(), None)
                .await?
        {
            return Ok(Some(r));
        }
        for cb in &self.after_model_callbacks {
            if let Some(r) = cb(callback_ctx(ctx, actions), resp.clone(), None).await? {
                return Ok(Some(r));
            }
        }
        Ok(None)
    }

    /// Port of base_flow.go:1132 runOnModelErrorCallbacks: plugins first; the first `Some`/`Err`
    /// wins.
    async fn run_on_model_error_callbacks(
        &self,
        ctx: &InvocationContext,
        req: &LlmRequest,
        actions: &SharedActions,
        err: Arc<ModelError>,
    ) -> Result<Option<LlmResponse>, AgentError> {
        if let Some(pm) = ctx.plugin_manager()
            && let Some(r) = pm
                .run_on_model_error(callback_ctx(ctx, actions), req.clone(), err.clone())
                .await?
        {
            return Ok(Some(r));
        }
        for cb in &self.on_model_error_callbacks {
            if let Some(r) = cb(callback_ctx(ctx, actions), req.clone(), err.clone()).await? {
                return Ok(Some(r));
            }
        }
        Ok(None)
    }

    /// Port of base_flow.go:1159 postprocess: the response processors in order.
    fn postprocess(
        &self,
        ctx: &InvocationContext,
        req: &LlmRequest,
        resp: &mut LlmResponse,
    ) -> Result<(), AgentError> {
        for p in &self.response_processors {
            p(ctx, req, resp)?;
        }
        Ok(())
    }

    /// Port of base_flow.go:1169 agentToRun: the transfer target named `name`, searched among the
    /// current agent's transfer targets (not the whole tree, unlike adk-python).
    fn agent_to_run(&self, ctx: &InvocationContext, name: &str) -> Option<Arc<dyn Agent>> {
        transfer_target_agents(ctx, &ctx.agent())
            .into_iter()
            .find(|a| a.name() == name)
    }
}

/// Go `icontext.NewCallbackContextWithDelta`: a callback context recording into `actions`.
fn callback_ctx(ctx: &InvocationContext, actions: &SharedActions) -> Context {
    ctx.clone().into_context().with_delta(ContextDelta {
        actions: Some(actions.clone()),
        ..Default::default()
    })
}

/// A model error still shared by a callback that kept a clone of its `Arc`.
#[derive(Debug)]
struct SharedModelError(Arc<ModelError>);

impl fmt::Display for SharedModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&*self.0, f)
    }
}

impl std::error::Error for SharedModelError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&*self.0)
    }
}

/// The model error as an `AgentError::Model`, or `AgentError::Other` (same text) when a callback
/// still holds the `Arc`.
fn model_error_into_agent(err: Arc<ModelError>) -> AgentError {
    match Arc::try_unwrap(err) {
        Ok(e) => AgentError::Model(e),
        Err(shared) => AgentError::Other(Box::new(SharedModelError(shared))),
    }
}

/// Go runOneStep's tools map: every packed request tool as an `Arc<dyn Tool>`.
fn tools_from_request(req: &LlmRequest) -> Result<HashMap<String, Arc<dyn Tool>>, AgentError> {
    let mut tools = HashMap::with_capacity(req.tools.len());
    for k in req.tools.keys() {
        let Some(t) = tool_from_request(req, k) else {
            return Err(AgentError::other(format!(
                "unexpected tool type for tool {k}"
            )));
        };
        tools.insert(k.clone(), t);
    }
    Ok(tools)
}

/// Port of base_flow.go:1183 finalizeModelResponseEvent.
fn finalize_model_response_event(
    ctx: &InvocationContext,
    mut resp: LlmResponse,
    event_id: String,
    tools: &HashMap<String, Arc<dyn Tool>>,
    actions: &SharedActions,
) -> Event {
    // Matching function calls and responses needs non-empty call IDs.
    populate_client_function_call_id(ctx.platform(), resp.content.as_mut());
    // Go NewEvent consumes a UUID even though the ID is then replaced.
    let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
    ev.id = event_id;
    ev.author = ctx.agent().name().to_owned();
    ev.branch = ctx.branch();
    {
        let a = actions.lock();
        ev.actions.state_delta = a.state_delta.clone();
        ev.actions.artifact_delta = a.artifact_delta.clone();
    }
    ev.long_running_tool_ids = find_long_running_function_call_ids(resp.content.as_ref(), tools);
    ev.response = resp;
    ev
}

/// Port of base_flow.go:1204 findLongRunningFunctionCallIDs: IDs of calls (non-empty ID) to
/// long-running tools, deduplicated. Go collects a map (random order); call order here.
pub(crate) fn find_long_running_function_call_ids(
    c: Option<&Content>,
    tools: &HashMap<String, Arc<dyn Tool>>,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for fc in function_calls(c) {
        if !fc.id.is_empty()
            && tools.get(&fc.name).is_some_and(|t| t.is_long_running())
            && !out.contains(&fc.id)
        {
            out.push(fc.id.clone());
        }
    }
    out
}

/// Port of base_flow.go:183 isThoughtOnlyTurn: a completed (non-partial) turn whose parts are all
/// thoughts.
pub(crate) fn is_thought_only_turn(ev: Option<&Event>) -> bool {
    let Some(ev) = ev.filter(|e| !e.response.partial) else {
        return false;
    };
    let Some(c) = ev.response.content.as_ref().filter(|c| !c.parts.is_empty()) else {
        return false;
    };
    c.parts.iter().all(|p| p.thought)
}

/// Port of base_flow.go:1090 generateContent + telemetry.InstrumentGenerateContent (non-legacy
/// schema). The `generate_content` span covers only the model call (callbacks and plugins stay
/// outside it). It ends on the first error, the first response that reports a finish reason, the
/// first response when `!stream`, or the end of the stream, recording the last response's usage.
/// Each item reserves one event ID (Go newResponseWithEventID).
pub(crate) fn generate_content(
    ctx: &InvocationContext,
    model: Arc<dyn Llm>,
    req: LlmRequest,
    stream: bool,
) -> BoxStream<'static, (Result<LlmResponse, ModelError>, String)> {
    let span = generate_content_span(model.name());
    let platform = ctx.platform().clone();
    let mut inner = span.in_scope(|| model.generate_content(req, stream));
    Box::pin(async_stream::stream! {
        let mut span = Some(span);
        let mut last: Option<LlmResponse> = None;
        while let Some(item) = inner.next().await {
            let event_id = platform.new_uuid();
            if let Some(s) = span.as_ref() {
                match &item {
                    Err(e) => {
                        record_generate_end(s, last.as_ref(), Some(e));
                        // Dropping the last handle closes the span.
                        span = None;
                    }
                    Ok(r) => {
                        last = Some(r.clone());
                        if r.finish_reason.is_some() || !stream {
                            record_generate_end(s, last.as_ref(), None);
                            span = None;
                        }
                    }
                }
            }
            yield (item, event_id);
        }
        if let Some(s) = span.take() {
            record_generate_end(&s, last.as_ref(), None);
        }
    })
}

/// Go traceGenerateContentResult: usage and status of the ended `generate_content` span.
fn record_generate_end(span: &tracing::Span, resp: Option<&LlmResponse>, err: Option<&ModelError>) {
    let usage = resp.and_then(|r| r.usage_metadata.as_ref());
    let input = usage.map(|u| u.prompt_token_count);
    let output = usage.map(|u| u.candidates_token_count);
    match err {
        Some(e) => {
            // Also set the span's declared Empty status fields, not just the event's.
            span.record("otel.status_code", "ERROR");
            span.record("error.message", tracing::field::display(e));
            tracing::error!(
                parent: span,
                otel.status_code = "ERROR",
                otel.status_description = %e,
                gen_ai.usage.input_tokens = input,
                gen_ai.usage.output_tokens = output,
                "generate_content failed"
            );
        }
        None => tracing::info!(
            parent: span,
            gen_ai.usage.input_tokens = input,
            gen_ai.usage.output_tokens = output,
            "generate_content finished"
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::{any::Any, sync::Arc};

    use async_trait::async_trait;
    use futures::StreamExt;
    use gadk_agent::{
        AgentConfig, AgentError, BeforeModelCallback, Context, InvocationContext,
        OnModelErrorCallback, ParentMap, RunnableTool, Tool, ToolError, agent,
        run_limits::LlmCallLimiter,
    };
    use gadk_genai::{Content, FunctionCall, Part};
    use gadk_model::{LlmResponse, ModelError};
    use gadk_session::Event;
    use pretty_assertions::assert_eq;
    use serde_json::{Map, Value, json};

    use super::*;
    use crate::{
        llm::{
            config::LlmAgentConfig,
            processors::{default_request_processors, test_llm_agent},
        },
        testutil::MockModel,
    };

    fn named_agent(name: &str) -> Arc<dyn Agent> {
        agent::new(AgentConfig {
            name: name.into(),
            ..Default::default()
        })
        .unwrap()
    }

    fn ctx_for(name: &str) -> InvocationContext {
        InvocationContext::builder()
            .agent(named_agent(name))
            .build_invocation()
    }

    fn text_resp(t: &str) -> LlmResponse {
        LlmResponse {
            content: Some(Content {
                role: "model".into(),
                parts: vec![Part {
                    text: t.into(),
                    ..Default::default()
                }],
            }),
            ..Default::default()
        }
    }

    fn call_resp(id: &str, name: &str, args: Value) -> LlmResponse {
        LlmResponse {
            content: Some(Content {
                role: "model".into(),
                parts: vec![Part {
                    function_call: Some(FunctionCall {
                        id: id.into(),
                        name: name.into(),
                        args: args.as_object().cloned().unwrap_or_default(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
            }),
            ..Default::default()
        }
    }

    fn texts(events: &[Event]) -> Vec<String> {
        events
            .iter()
            .filter_map(|e| e.response.content.as_ref())
            .flat_map(|c| {
                c.parts
                    .iter()
                    .filter(|p| !p.text.is_empty())
                    .map(|p| p.text.clone())
            })
            .collect()
    }

    async fn run_all(flow: Flow, ctx: InvocationContext) -> Vec<Result<Event, AgentError>> {
        Arc::new(flow).run(ctx).collect().await
    }

    /// A no-op tool, optionally long-running.
    struct Noop {
        name: &'static str,
        long_running: bool,
    }

    impl Tool for Noop {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            "noop"
        }
        fn is_long_running(&self) -> bool {
            self.long_running
        }
        fn as_runnable(&self) -> Option<&dyn RunnableTool> {
            Some(self)
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    #[async_trait]
    impl RunnableTool for Noop {
        async fn run(&self, _: Context, _: Value) -> Result<Map<String, Value>, ToolError> {
            Ok(Map::new())
        }
    }

    #[tokio::test]
    async fn model_not_configured_error_text() {
        let items = run_all(Flow::default(), ctx_for("a")).await;
        assert_eq!(items.len(), 1);
        let err = items.into_iter().next().unwrap().unwrap_err();
        assert_eq!(
            err.to_string(),
            "agent \"a\": model not configured; ensure Model is set in llmagent.Config"
        );
    }

    #[tokio::test]
    async fn before_model_callback_short_circuits() {
        let model = MockModel::new(vec![Ok(text_resp("model"))]);
        let cb: BeforeModelCallback =
            Arc::new(|_, req| Box::pin(async move { (req, Ok(Some(text_resp("cb")))) }));
        let flow = Flow {
            model: Some(model.clone()),
            before_model_callbacks: vec![cb],
            ..Default::default()
        };
        let events: Vec<Event> = run_all(flow, ctx_for("a"))
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect();
        assert_eq!(texts(&events), ["cb"]);
        assert_eq!(events.len(), 1);
        assert!(model.requests().is_empty(), "model must not be called");
    }

    #[tokio::test]
    async fn before_model_callback_request_edits_reach_model() {
        let model = MockModel::new(vec![Ok(text_resp("ok"))]);
        let cb: BeforeModelCallback = Arc::new(|_, mut req| {
            Box::pin(async move {
                req.model = "x".into();
                (req, Ok(None))
            })
        });
        let flow = Flow {
            model: Some(model.clone()),
            before_model_callbacks: vec![cb],
            ..Default::default()
        };
        let items = run_all(flow, ctx_for("a")).await;
        assert!(items.iter().all(Result::is_ok));
        let reqs = model.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].model, "x");
    }

    #[tokio::test]
    async fn on_model_error_callback_recovers() {
        let model = MockModel::new(vec![Err(ModelError::Other("boom".into()))]);
        let cb: OnModelErrorCallback = Arc::new(|_, _, err| {
            assert_eq!(err.to_string(), "boom");
            Box::pin(async move { Ok(Some(text_resp("recovered"))) })
        });
        let flow = Flow {
            model: Some(model),
            on_model_error_callbacks: vec![cb],
            ..Default::default()
        };
        let events: Vec<Event> = run_all(flow, ctx_for("a"))
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect();
        assert_eq!(texts(&events), ["recovered"]);
    }

    #[tokio::test]
    async fn model_error_without_callback_is_returned() {
        let model = MockModel::new(vec![Err(ModelError::Other("boom".into()))]);
        let flow = Flow {
            model: Some(model),
            ..Default::default()
        };
        let items = run_all(flow, ctx_for("a")).await;
        assert_eq!(items.len(), 1);
        let err = items.into_iter().next().unwrap().unwrap_err();
        assert!(matches!(err, AgentError::Model(_)), "{err:?}");
        assert_eq!(err.to_string(), "boom");
    }

    #[tokio::test]
    async fn long_running_tool_without_result_emits_no_response() {
        let lr: Arc<dyn Tool> = Arc::new(Noop {
            name: "lr",
            long_running: true,
        });
        let flow = Flow {
            model: Some(MockModel::new(vec![Ok(call_resp("fc1", "lr", json!({})))])),
            tools: parking_lot::RwLock::new(vec![lr.clone()]),
            ..Default::default()
        };
        let events: Vec<Event> = run_all(flow, ctx_for("a"))
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect();
        // Only the model event: the long-running call has no response, and the event is final.
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].long_running_tool_ids, ["fc1"]);

        let tools = HashMap::from([("lr".to_owned(), lr)]);
        let got = handle_function_calls(&ctx_for("a"), &Flow::default(), &tools, &events[0], None)
            .await
            .unwrap();
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn transfer_runs_target_agent() {
        let c = agent::new(AgentConfig {
            name: "c".into(),
            description: "child".into(),
            run: Some(Arc::new(|ctx: InvocationContext| {
                Box::pin(futures::stream::once(async move {
                    let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
                    ev.author = "c".into();
                    ev.response = text_resp("from c");
                    Ok(ev)
                }))
            })),
            ..Default::default()
        })
        .unwrap();
        let model = MockModel::new(vec![Ok(call_resp(
            "fc1",
            "transfer_to_agent",
            json!({"agent_name": "c"}),
        ))]);
        let root = test_llm_agent(LlmAgentConfig {
            name: "root".into(),
            model: Some(model.clone()),
            sub_agents: vec![c],
            ..Default::default()
        });
        let pm = Arc::new(ParentMap::new(&root).unwrap());
        let ctx = InvocationContext::builder()
            .agent(root)
            .parent_map(pm)
            .build_invocation();
        let flow = Flow {
            model: Some(model),
            request_processors: default_request_processors(),
            ..Default::default()
        };
        let events: Vec<Event> = run_all(flow, ctx)
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect();
        assert_eq!(events.len(), 3, "{events:#?}");
        let call = &events[0].response.content.as_ref().unwrap().parts[0];
        assert_eq!(
            call.function_call.as_ref().unwrap().name,
            "transfer_to_agent"
        );
        let resp = &events[1].response.content.as_ref().unwrap().parts[0];
        assert_eq!(resp.function_response.as_ref().unwrap().id, "fc1");
        assert_eq!(events[1].actions.transfer_to_agent, "c");
        assert_eq!(events[2].author, "c");
        assert_eq!(texts(&events[2..]), ["from c"]);
    }

    #[tokio::test]
    async fn llm_call_limit_exceeded() {
        let noop: Arc<dyn Tool> = Arc::new(Noop {
            name: "noop",
            long_running: false,
        });
        let model = MockModel::new(vec![
            Ok(call_resp("", "noop", json!({}))),
            Ok(call_resp("", "noop", json!({}))),
        ]);
        let flow = Flow {
            model: Some(model.clone()),
            tools: parking_lot::RwLock::new(vec![noop]),
            ..Default::default()
        };
        let ctx = InvocationContext::builder()
            .agent(named_agent("a"))
            .llm_call_limiter(Arc::new(LlmCallLimiter::new(1)))
            .build_invocation();
        let items = run_all(flow, ctx).await;
        // Step 1: the call and its response; step 2 fails before calling the model.
        assert_eq!(items.len(), 3);
        assert!(items[..2].iter().all(Result::is_ok));
        let err = items.into_iter().nth(2).unwrap().unwrap_err();
        assert!(
            matches!(err, AgentError::LlmCallsLimitExceeded { limit: 1 }),
            "{err:?}"
        );
        assert_eq!(
            err.to_string(),
            "max number of llm calls exceeded: limit is 1"
        );
        assert_eq!(model.requests().len(), 1);
    }

    #[derive(serde::Deserialize, schemars::JsonSchema)]
    struct NoArgs {}

    #[tokio::test]
    async fn confirmation_required_error_carries_go_tool_prefix() {
        use gadk_agent::AfterToolCallback;

        use crate::tool::{FunctionTool, FunctionToolConfig};

        let secure = FunctionTool::new(
            FunctionToolConfig {
                require_confirmation: true,
                ..FunctionToolConfig::new("secure_action", "needs approval")
            },
            |_ctx: Context, _: NoArgs| async move { Ok::<_, ToolError>(json!({"done": true})) },
        )
        .unwrap();
        let seen: Arc<parking_lot::Mutex<Option<(String, bool)>>> = Arc::default();
        let seen_cb = seen.clone();
        let after: AfterToolCallback = Arc::new(move |_, _, _, _, err| {
            let err = err.expect("after callback sees the tool error");
            // The sentinel stays reachable through source() (Go errors.Is).
            let mut src: Option<&(dyn std::error::Error + 'static)> = Some(&*err);
            let mut is_required = false;
            while let Some(e) = src {
                is_required |= matches!(
                    e.downcast_ref::<ToolError>(),
                    Some(ToolError::ConfirmationRequired)
                );
                src = e.source();
            }
            *seen_cb.lock() = Some((err.to_string(), is_required));
            Box::pin(async { Ok(None) })
        });
        let flow = Flow {
            model: Some(MockModel::new(vec![Ok(call_resp(
                "fc1",
                "secure_action",
                json!({}),
            ))])),
            tools: parking_lot::RwLock::new(vec![secure]),
            after_tool_callbacks: vec![after],
            ..Default::default()
        };
        let events: Vec<Event> = run_all(flow, ctx_for("a"))
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect();
        let want = "error tool \"secure_action\" requires confirmation, please approve or reject";
        let fr = events[1].response.content.as_ref().unwrap().parts[0]
            .function_response
            .clone()
            .unwrap();
        assert_eq!(
            fr.response,
            json!({"error": want}).as_object().unwrap().clone()
        );
        assert_eq!(*seen.lock(), Some((want.to_owned(), true)));
        // The confirmation request follows the response event.
        let req = events[2].response.content.as_ref().unwrap().parts[0]
            .function_call
            .clone()
            .unwrap();
        assert_eq!(
            req.name,
            gadk_agent::REQUEST_CONFIRMATION_FUNCTION_CALL_NAME
        );
    }

    #[tokio::test]
    async fn cancelled_token_stops_model_call() {
        let model = MockModel::new(vec![Ok(text_resp("never"))]);
        let token = tokio_util::sync::CancellationToken::new();
        token.cancel();
        let ctx = InvocationContext::builder()
            .agent(named_agent("a"))
            .cancellation_token(token)
            .build_invocation();
        let flow = Flow {
            model: Some(model.clone()),
            ..Default::default()
        };
        let items = run_all(flow, ctx).await;
        assert_eq!(items.len(), 1);
        assert!(matches!(items[0], Err(AgentError::Cancelled)));
        assert!(model.requests().is_empty());
    }
}

/// Test-only `tracing` span recorder for the span parity tests.
#[cfg(test)]
pub(crate) mod span_capture {
    use std::{
        cell::RefCell, collections::BTreeMap, collections::HashMap, future::Future, sync::Arc,
    };

    use tracing::{
        field::{Field, Visit},
        span::{Attributes, Id, Record},
    };
    use tracing_subscriber::{
        Layer,
        layer::{Context as LayerContext, SubscriberExt},
        registry::LookupSpan,
    };

    /// One recorded span: its name, `otel.name`, the fields recorded on it or on events inside it,
    /// whether it has closed, and its parent's name.
    #[derive(Clone, Debug, Default)]
    pub(crate) struct CapturedSpan {
        pub(crate) name: String,
        pub(crate) otel_name: String,
        pub(crate) fields: BTreeMap<String, String>,
        pub(crate) closed: bool,
        /// The static name of the span's parent, `None` for a root span.
        pub(crate) parent: Option<String>,
    }

    impl CapturedSpan {
        /// The rendered value of field `k`, if recorded.
        pub(crate) fn field(&self, k: &str) -> Option<String> {
            self.fields.get(k).cloned()
        }
    }

    thread_local! {
        static CLOSED: RefCell<HashMap<String, usize>> = RefCell::new(HashMap::new());
    }

    /// How many spans named `name` have closed on this thread since `capture_spans` started.
    pub(crate) fn closed_count(name: &str) -> usize {
        CLOSED.with(|c| c.borrow().get(name).copied().unwrap_or(0))
    }

    /// Installs (once per test binary) a no-op global `Registry` so the global max-level hint and
    /// callsite interest never collapse to "off" while thread-default capture subscribers come and
    /// go on parallel test threads. Without it, a concurrent interest rebuild on another thread
    /// (which does not yet see this thread's new default) can disable every span macro and a
    /// capture records nothing (flaky `execute_tool_spans_for_parallel_calls`).
    pub(crate) fn pin_tracing_interest() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let _ = tracing::subscriber::set_global_default(tracing_subscriber::registry());
        });
    }

    /// Runs `fut` with a recording subscriber as the thread default; use only on a
    /// current-thread runtime.
    pub(crate) async fn capture_spans<F: Future>(fut: F) -> (F::Output, Vec<CapturedSpan>) {
        pin_tracing_interest();
        CLOSED.with(|c| c.borrow_mut().clear());
        let spans: Arc<parking_lot::Mutex<Vec<CapturedSpan>>> = Arc::default();
        let sub = tracing_subscriber::registry().with(CaptureLayer(spans.clone()));
        let out = {
            let _g = tracing::subscriber::set_default(sub);
            fut.await
        };
        let spans = spans.lock().clone();
        (out, spans)
    }

    struct CaptureLayer(Arc<parking_lot::Mutex<Vec<CapturedSpan>>>);

    /// The index of a span in the captured list (stored in the span's extensions).
    struct Idx(usize);

    struct Fields<'a>(&'a mut BTreeMap<String, String>);

    impl Visit for Fields<'_> {
        fn record_str(&mut self, f: &Field, v: &str) {
            self.0.insert(f.name().to_owned(), v.to_owned());
        }
        fn record_debug(&mut self, f: &Field, v: &dyn std::fmt::Debug) {
            self.0.insert(f.name().to_owned(), format!("{v:?}"));
        }
    }

    impl CaptureLayer {
        fn with_span<S>(
            &self,
            id: &Id,
            ctx: &LayerContext<'_, S>,
            f: impl FnOnce(&mut CapturedSpan),
        ) where
            S: tracing::Subscriber + for<'a> LookupSpan<'a>,
        {
            let Some(span) = ctx.span(id) else { return };
            let Some(i) = span.extensions().get::<Idx>().map(|i| i.0) else {
                return;
            };
            if let Some(s) = self.0.lock().get_mut(i) {
                f(s);
            }
        }
    }

    impl<S> Layer<S> for CaptureLayer
    where
        S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    {
        fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: LayerContext<'_, S>) {
            let mut fields = BTreeMap::new();
            attrs.record(&mut Fields(&mut fields));
            let parent = ctx
                .span(id)
                .and_then(|s| s.parent())
                .map(|p| p.name().to_owned());
            let captured = CapturedSpan {
                name: attrs.metadata().name().to_owned(),
                otel_name: fields.get("otel.name").cloned().unwrap_or_default(),
                fields,
                closed: false,
                parent,
            };
            let i = {
                let mut v = self.0.lock();
                v.push(captured);
                v.len() - 1
            };
            if let Some(span) = ctx.span(id) {
                span.extensions_mut().insert(Idx(i));
            }
        }

        fn on_record(&self, id: &Id, values: &Record<'_>, ctx: LayerContext<'_, S>) {
            self.with_span(id, &ctx, |s| values.record(&mut Fields(&mut s.fields)));
        }

        fn on_event(&self, event: &tracing::Event<'_>, ctx: LayerContext<'_, S>) {
            let Some(parent) = ctx.event_span(event) else {
                return;
            };
            let id = parent.id();
            self.with_span(&id, &ctx, |s| event.record(&mut Fields(&mut s.fields)));
        }

        fn on_close(&self, id: Id, ctx: LayerContext<'_, S>) {
            let mut name = None;
            self.with_span(&id, &ctx, |s| {
                s.closed = true;
                name = Some(s.name.clone());
            });
            if let Some(n) = name {
                CLOSED.with(|c| *c.borrow_mut().entry(n).or_insert(0) += 1);
            }
        }
    }
}
