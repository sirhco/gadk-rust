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

//! Port of Go internal/llminternal/functions.go and the tool-execution half of base_flow.go
//! (lines 1205-1735): parallel function-call dispatch, tool callbacks, response merging and the
//! request-confirmation event.

use std::{
    any::Any,
    collections::{BTreeMap, HashMap, HashSet},
    future::Future,
    sync::Arc,
};

use futures::{StreamExt, future::BoxFuture};
use gadk_agent::fiber::{self, Fiber};
use gadk_agent::{
    AfterToolCallback, AgentError, BeforeToolCallback, CancelGuard, Context, ContextDelta,
    InvocationContext, LiveRequest, LiveSession, OnToolErrorCallback,
    REQUEST_CONFIRMATION_FUNCTION_CALL_NAME, SharedToolError, Tool, ToolError,
};
use gadk_genai::{Content, FunctionCall, FunctionResponse, Part};
use gadk_model::LlmResponse;
use gadk_session::{Event, EventActions, ToolConfirmation};
use serde_json::{Map, Value};
use tracing::Instrument;

use crate::{
    llm::{flow::Flow, live::session::LiveSessionImpl},
    telemetry::{
        attrs::{
            SpanError, start_execute_tool_span, trace_merged_tool_calls_result, trace_tool_result,
        },
        merged_tool_calls_span,
    },
    tool::utils::{function_calls, generate_function_call_id},
    workflow::route::go_quote,
};

/// Tool callbacks cloned out of the Flow so each spawned tool task is `'static`.
#[derive(Clone, Default)]
pub(crate) struct ToolCallbacks {
    pub(crate) before: Vec<BeforeToolCallback>,
    pub(crate) after: Vec<AfterToolCallback>,
    pub(crate) on_error: Vec<OnToolErrorCallback>,
}

impl Flow {
    /// The flow's tool callbacks, cloned (cheap `Arc` clones).
    pub(crate) fn tool_callbacks(&self) -> ToolCallbacks {
        ToolCallbacks {
            before: self.before_tool_callbacks.clone(),
            after: self.after_tool_callbacks.clone(),
            on_error: self.on_tool_error_callbacks.clone(),
        }
    }
}

/// Appendix C entry point (Go `f.handleFunctionCalls(ctx, tools, resp, nil, nil)`).
pub(crate) async fn handle_function_calls(
    ctx: &InvocationContext,
    flow: &Flow,
    tools: &HashMap<String, Arc<dyn Tool>>,
    function_call_event: &Event,
    filters: Option<&HashSet<String>>,
) -> Result<Option<Event>, AgentError> {
    handle_function_calls_with(ctx, flow, tools, function_call_event, filters, None).await
}

/// Port of base_flow.go:1330 `(*Flow).handleFunctionCalls`, minus live (`liveSess` is always nil
/// in 1b). Go takes the `*model.LLMResponse`; Rust takes the event carrying it (C5 D5 signature).
/// `filters` (adk-python's call-ID filter; Go has a TODO) keeps only calls whose ID is in the set.
///
/// Every call runs as one task on the platform's task runner (Go `platform.RunTasks`); dropping
/// the returned future lets the tool tasks still running finish (see [`FinishOnDrop`]).
pub(crate) async fn handle_function_calls_with(
    ctx: &InvocationContext,
    flow: &Flow,
    tools: &HashMap<String, Arc<dyn Tool>>,
    function_call_event: &Event,
    filters: Option<&HashSet<String>>,
    confirmations: Option<&HashMap<String, ToolConfirmation>>,
) -> Result<Option<Event>, AgentError> {
    handle_function_calls_inner(
        ctx,
        flow,
        tools,
        function_call_event,
        filters,
        confirmations,
        None,
    )
    .await
}

/// Port of Go handleFunctionCalls with its `liveSess` argument set (base_flow.go:1330): the live
/// streaming-tool and `stop_streaming` branches use `live`. No filters, no confirmations (Go's live
/// flow passes nil).
pub(crate) async fn handle_function_calls_live(
    ctx: &InvocationContext,
    flow: &Flow,
    tools: &HashMap<String, Arc<dyn Tool>>,
    function_call_event: &Event,
    live: Arc<dyn LiveSession>,
) -> Result<Option<Event>, AgentError> {
    handle_function_calls_inner(
        ctx,
        flow,
        tools,
        function_call_event,
        None,
        None,
        Some(live),
    )
    .await
}

/// Port of base_flow.go:1330 `(*Flow).handleFunctionCalls`. Go takes the `*model.LLMResponse`;
/// Rust takes the event carrying it (C5 D5 signature). `filters` (adk-python's call-ID filter; Go
/// has a TODO) keeps only calls whose ID is in the set. `live` is Go's `liveSess` argument (`None`
/// outside live mode).
///
/// Every call runs as one task on the platform's task runner (Go `platform.RunTasks`); dropping
/// the returned future cancels the calls' scope plainly and lets the tool tasks still running
/// finish (see [`FinishOnDrop`]; a live streaming tool's spawned task is not one of them: it
/// ends on its cancellation token).
async fn handle_function_calls_inner(
    ctx: &InvocationContext,
    flow: &Flow,
    tools: &HashMap<String, Arc<dyn Tool>>,
    function_call_event: &Event,
    filters: Option<&HashSet<String>>,
    confirmations: Option<&HashMap<String, ToolConfirmation>>,
    live: Option<Arc<dyn LiveSession>>,
) -> Result<Option<Event>, AgentError> {
    let calls: Vec<FunctionCall> = function_calls(function_call_event.response.content.as_ref())
        .into_iter()
        .filter(|fc| filters.is_none_or(|f| f.contains(&fc.id)))
        .cloned()
        .collect();
    // Go: map-key order (random); sorted here for determinism.
    let mut names: Vec<String> = tools.keys().cloned().collect();
    names.sort();
    let names = Arc::new(names);
    // Go: a merged span only for more than one call (otel.name "execute_tool (merged)").
    let merged_span = (calls.len() > 1).then(merged_tool_calls_span);
    let slots: Arc<parking_lot::Mutex<Vec<Option<Event>>>> =
        Arc::new(parking_lot::Mutex::new(vec![None; calls.len()]));
    let cbs = flow.tool_callbacks();
    // The calls run under a scope the dispatch owns: a drop mid-await (Go: the caller's cancel)
    // cancels it plainly before the calls finish as a teardown; a completed dispatch gives it up
    // uncancelled (see [`FinishOnDrop`]).
    let (calls_ctx, calls_scope) = ctx.clone().into_context().with_agent_cancel();
    let calls_ctx = calls_ctx.as_invocation().clone();
    let mut tasks: Vec<BoxFuture<'static, ()>> = Vec::with_capacity(calls.len());
    for (i, fc) in calls.into_iter().enumerate() {
        let span = match &merged_span {
            Some(m) => m.in_scope(|| start_execute_tool_span(&fc.name, &fc.args)),
            None => start_execute_tool_span(&fc.name, &fc.args),
        };
        let (ctx, cbs, slots, names) =
            (calls_ctx.clone(), cbs.clone(), slots.clone(), names.clone());
        let live = live.clone();
        let tool = tools.get(&fc.name).cloned();
        let confirmation = confirmations.and_then(|m| m.get(&fc.id).cloned());
        let call_span = span.clone();
        tasks.push(Box::pin(
            async move {
                let ev =
                    run_one_call(ctx, cbs, tool, fc, confirmation, names, live, call_span).await;
                slots.lock()[i] = ev;
            }
            .instrument(span),
        ));
    }
    let platform = ctx.platform().clone();
    let run = async move { platform.run_tasks(tasks).await };
    let run: BoxFuture<'static, ()> = match &merged_span {
        Some(s) => Box::pin(run.instrument(s.clone())),
        None => Box::pin(run),
    };
    FinishOnDrop::new(ctx, run, calls_scope).await;
    let events = std::mem::take(&mut *slots.lock());
    let merged = merge_parallel_function_response_events(events);
    if let Some(s) = &merged_span {
        // Go's deferred TraceMergedToolCallsResult(mergedToolCallSpan, mergedEvent, err).
        match &merged {
            Ok(ev) => trace_merged_tool_calls_result(s, ev.as_ref(), None),
            Err(e) => trace_merged_tool_calls_result(s, None, Some(&SpanError::new(e))),
        }
    }
    merged
}

/// Go `platform.RunTasks` blocks until every task completed (platform/exec.go:68), and a Go
/// consumer that stops reading early unwinds through it on its own goroutine, so the tool calls
/// in flight finish (after-tool and on-tool-error callbacks included) before the runner's
/// deferred after-run callbacks. Dropping a Rust future cannot wait, and a drop mid-await stands
/// for Go's caller cancellation: dropped before completion, this cancels the calls' scope plainly
/// (a tool that ignores cancellation still runs to completion; none is aborted) and hands the task
/// runner's still-running future to the invocation's teardown registry
/// ([`InvocationContext::spawn_teardown`]) on the fiber and span it was started in. Completed, it
/// gives the scope up uncancelled ([`CancelGuard::release`]): a tool context kept beyond the call
/// (a live streaming tool's task) stays live until the invocation ends.
struct FinishOnDrop {
    run: Option<BoxFuture<'static, ()>>,
    scope: Option<CancelGuard>,
    ctx: InvocationContext,
    fiber: Fiber,
    span: tracing::Span,
}

impl FinishOnDrop {
    fn new(ctx: &InvocationContext, run: BoxFuture<'static, ()>, scope: CancelGuard) -> Self {
        Self {
            run: Some(run),
            scope: Some(scope),
            ctx: ctx.clone(),
            fiber: fiber::current(),
            span: tracing::Span::current(),
        }
    }
}

impl Future for FinishOnDrop {
    type Output = ();

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<()> {
        let Some(run) = self.run.as_mut() else {
            return std::task::Poll::Ready(());
        };
        let polled = run.as_mut().poll(cx);
        if polled.is_ready() {
            self.run = None;
            if let Some(scope) = self.scope.take() {
                scope.release();
            }
        }
        polled
    }
}

impl Drop for FinishOnDrop {
    fn drop(&mut self) {
        if let Some(run) = self.run.take() {
            if let Some(scope) = self.scope.take() {
                scope.cancel();
            }
            let work = self.fiber.clone().scope(run).instrument(self.span.clone());
            self.ctx.spawn_teardown(work);
        }
    }
}

/// The body of Go's per-call task closure (base_flow.go:1350-1525).
#[allow(clippy::too_many_arguments)]
async fn run_one_call(
    ctx: InvocationContext,
    cbs: ToolCallbacks,
    tool: Option<Arc<dyn Tool>>,
    fc: FunctionCall,
    confirmation: Option<ToolConfirmation>,
    names: Arc<Vec<String>>,
    live: Option<Arc<dyn LiveSession>>,
    span: tracing::Span,
) -> Option<Event> {
    // Go: agent.NewToolContext(toolCallCtx, fnCall.ID, &session.EventActions{StateDelta: {}}, confirmation).
    let tool_ctx = ctx.clone().into_context().with_delta(ContextDelta {
        function_call_id: Some(fc.id.clone()),
        actions: Some(Arc::new(parking_lot::Mutex::new(EventActions {
            state_delta: Some(Map::new()),
            ..Default::default()
        }))),
        tool_confirmation: Some(confirmation),
        ..Default::default()
    });

    let result: Option<Map<String, Value>> = if fc.name == "stop_streaming" {
        // Go reaches this branch whenever the name matches, before the tool lookup. Without a
        // live session (or with one that is not a LiveSessionImpl) nothing can be stopped.
        // Go: funcToStop, _ := fnCall.Args["function_name"].(string)
        let func_to_stop = fc
            .args
            .get("function_name")
            .and_then(Value::as_str)
            .unwrap_or("");
        let stopped = live
            .as_ref()
            .and_then(|l| l.as_any().downcast_ref::<LiveSessionImpl>())
            .is_some_and(|s| s.cancel_all_streaming_tools(func_to_stop));
        let status = if stopped {
            format!("Successfully stopped all running instances of {func_to_stop}")
        } else {
            format!("No active streaming function named {func_to_stop} found")
        };
        Some(Map::from_iter([(
            "status".to_owned(),
            Value::String(status),
        )]))
    } else {
        match &tool {
            None => Some(not_found(&cbs, &tool_ctx, &fc, &names).await),
            // Go checks StreamingFunctionTool before FunctionTool. Callbacks do not run here.
            Some(t) if t.as_streaming().is_some() => match &live {
                Some(live) => Some(start_live_streaming(
                    t.clone(),
                    &tool_ctx,
                    &fc,
                    live.clone(),
                )),
                None => Some(run_streaming(t.as_ref(), tool_ctx.clone(), &fc.args).await),
            },
            Some(t) if t.as_runnable().is_none() => {
                Some(not_found(&cbs, &tool_ctx, &fc, &names).await)
            }
            Some(t) => call_tool(&cbs, tool_ctx.clone(), t.clone(), fc.args.clone()).await,
        }
    };

    // Go: `if result == nil` and the tool defers its response or is long-running, no response.
    // Deviation (C5 D5): `RunnableTool::run` cannot return Go's nil map, so an empty result map
    // stands in for nil; a deferring/long-running tool that returns a deliberately empty (Go
    // non-nil) map is also skipped.
    if result.as_ref().is_none_or(Map::is_empty)
        && tool
            .as_ref()
            .is_some_and(|t| t.defers_response() || t.is_long_running())
    {
        return None;
    }
    let result = result.unwrap_or_default();

    let mut parts = vec![Part {
        function_response: Some(FunctionResponse {
            id: fc.id.clone(),
            name: fc.name.clone(),
            response: result.clone(),
            ..Default::default()
        }),
        ..Default::default()
    }];
    // SkipSummarization ends the agent loop without the LLM seeing the response; tools that opt in
    // (agenttool) also get the result as a visible text part.
    if tool_ctx.actions().skip_summarization
        && tool
            .as_ref()
            .is_some_and(|t| t.display_result_on_skip_summarization())
        && let Some(text) = displayable_tool_result_text(&result)
    {
        parts.push(Part {
            text,
            ..Default::default()
        });
    }

    let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
    ev.response = LlmResponse {
        content: Some(Content {
            role: "user".into(),
            parts,
        }),
        ..Default::default()
    };
    ev.author = ctx.agent().name().to_owned();
    ev.branch = ctx.branch();
    ev.actions = tool_ctx.actions();
    // Compaction is the framework's to write (Go session.EventActions.Compaction).
    ev.actions.compaction = None;

    // Go telemetry.TraceToolResult: only a string (or error) "error" value marks the span failed;
    // a call without a tool (stop_streaming or an unknown name) is Go's fakeTool.
    let description = match &tool {
        Some(t) if fc.name != "stop_streaming" => t.description(),
        _ => "Tool not found",
    };
    let tool_err = match result.get("error") {
        Some(Value::String(err)) => Some(SpanError::text(err.as_str())),
        _ => None,
    };
    trace_tool_result(&span, description, Some(&ev), tool_err.as_ref());
    Some(ev)
}

/// Go streaming (non-live) branch: concatenate the chunks; the first error ends the call.
async fn run_streaming(
    tool: &dyn Tool,
    tool_ctx: Context,
    args: &Map<String, Value>,
) -> Map<String, Value> {
    let Some(st) = tool.as_streaming() else {
        return Map::new();
    };
    let mut stream = st.run_stream(tool_ctx, Value::Object(args.clone()));
    let mut out = String::new();
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(c) => out.push_str(&c),
            Err(e) => return error_map(&wrap_confirmation_error(tool.name(), e).to_string()),
        }
    }
    Map::from_iter([("result".to_owned(), Value::String(out))])
}

/// Go streaming branch with a live session (base_flow.go:1383-1426): answers at once with the
/// pending status and streams in a spawned task that outlives the call. Each chunk goes back to the
/// model as user content through `live.send`. The task stops on cancellation (`stop_streaming`,
/// session teardown, or the invocation's token) and unregisters before cancelling its scope.
fn start_live_streaming(
    tool: Arc<dyn Tool>,
    tool_ctx: &Context,
    fc: &FunctionCall,
    live: Arc<dyn LiveSession>,
) -> Map<String, Value> {
    let Some(st) = tool.as_streaming() else {
        return Map::new();
    };
    // Go: cancelCtx, cancel := toolCtx.WithAgentCancel()
    let (cancel_ctx, cancel_guard) = tool_ctx.with_agent_cancel();
    let token = cancel_ctx.cancellation_token().clone();
    let name = tool.name().to_owned();
    let call_id = fc.id.clone();
    let session = live.as_any().downcast_ref::<LiveSessionImpl>();
    if let Some(s) = session {
        s.register_streaming_tool(&name, &call_id, token.clone());
        // R9: a teardown that already drained the registry (dropping the event stream cancels
        // `done` while this tool task may still be mid-poll on another worker) would never
        // cancel this entry. Teardown cancels `done` before taking the registry lock to drain, so
        // seeing `done` cancelled here means the drain may have missed us: cancel ourselves.
        if s.done().is_cancelled() {
            token.cancel();
        }
    }
    let task_guard = session.map(|s| s.probe().task_guard());
    let mut stream = st.run_stream(cancel_ctx, Value::Object(fc.args.clone()));
    tokio::spawn(async move {
        let _task_guard = task_guard;
        loop {
            let next = tokio::select! {
                biased;
                _ = token.cancelled() => break,
                next = stream.next() => next,
            };
            let Some(item) = next else { break };
            if token.is_cancelled() {
                break;
            }
            let chunk = match item {
                Ok(chunk) => chunk,
                Err(e) => {
                    tracing::warn!(tool = %name, error = %e, "error in streaming tool");
                    break;
                }
            };
            let content = Content {
                role: "user".into(),
                parts: vec![Part::from_text(format!(
                    "Function {name} returned: {chunk}"
                ))],
            };
            let req = LiveRequest {
                content: Some(content),
                realtime_input: None,
            };
            let sent = tokio::select! {
                biased;
                _ = token.cancelled() => break,
                sent = live.send(req) => sent,
            };
            if let Err(e) = sent {
                tracing::warn!(tool = %name, error = %e, "failed to send content from streaming tool");
                break;
            }
        }
        drop(stream);
        // Go defer order: UnregisterStreamingTool, then cancel().
        if let Some(s) = live.as_any().downcast_ref::<LiveSessionImpl>() {
            s.unregister_streaming_tool(&name, &call_id);
        }
        drop(cancel_guard);
    });
    Map::from_iter([(
        "status".to_owned(),
        Value::String(
            "The function is running asynchronously and the results are pending.".to_owned(),
        ),
    )])
}

/// Go: `newToolNotFoundError` routed through the on-tool-error callbacks with a `fakeTool`.
async fn not_found(
    cbs: &ToolCallbacks,
    tool_ctx: &Context,
    fc: &FunctionCall,
    names: &[String],
) -> Map<String, Value> {
    let err = new_tool_not_found_error(&fc.name, names);
    let fake: Arc<dyn Tool> = Arc::new(FakeTool {
        name: fc.name.clone(),
    });
    let (result, err) =
        run_on_tool_error_callbacks(cbs, tool_ctx, fake, &fc.args, Arc::new(err)).await;
    match err {
        Some(e) => error_map(&e.to_string()),
        None => result.unwrap_or_default(),
    }
}

fn error_map(msg: &str) -> Map<String, Value> {
    Map::from_iter([("error".to_owned(), Value::String(msg.to_owned()))])
}

/// Port of base_flow.go:1218 fakeTool.
struct FakeTool {
    name: String,
}

impl Tool for FakeTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        "Tool not found"
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Port of base_flow.go:1229 newToolNotFoundError (text verbatim, Python format).
fn new_tool_not_found_error(tool_name: &str, available_tools: &[String]) -> ToolError {
    let joined = available_tools.join(", ");
    ToolError::Execution(
        format!(
            "tool '{tool_name}' not found.
Available tools: {joined}

Possible causes:
  1. LLM hallucinated the function name - review agent instruction clarity
  2. Tool not registered - verify agent.tools list
  3. Name mismatch - check for typos

Suggested fixes:
  - Review agent instruction to ensure tool usage is clear
  - Verify tool is included in agent.tools list
  - Check for typos in function name"
        )
        .into(),
    )
}

/// Port of base_flow.go:1533 runOnToolErrorCallbacks: plugins first, then the agent callbacks.
async fn run_on_tool_error_callbacks(
    cbs: &ToolCallbacks,
    tool_ctx: &Context,
    tool: Arc<dyn Tool>,
    args: &Map<String, Value>,
    err: Arc<ToolError>,
) -> (Option<Map<String, Value>>, Option<Arc<ToolError>>) {
    if let Some(pm) = tool_ctx.plugin_manager() {
        match pm
            .run_on_tool_error(tool_ctx.clone(), tool.clone(), args.clone(), err.clone())
            .await
        {
            Ok(Some(r)) => return (Some(r), None),
            Err(e) => return (None, Some(to_tool_error(e))),
            Ok(None) => {}
        }
    }
    invoke_on_tool_error_callbacks(cbs, tool_ctx, &tool, args, err).await
}

/// Port of base_flow.go:1544 `(*Flow).callTool`. Callback errors (`AgentError`) become
/// `ToolError` so a single "current error" flows through the on-error and after callbacks, as
/// Go's untyped `error` does.
pub(crate) async fn call_tool(
    cbs: &ToolCallbacks,
    tool_ctx: Context,
    tool: Arc<dyn Tool>,
    args: Map<String, Value>,
) -> Option<Map<String, Value>> {
    let pm = tool_ctx.plugin_manager();
    let mut response: Option<Map<String, Value>> = None;
    let mut err: Option<Arc<ToolError>> = None;
    if let Some(pm) = &pm {
        match pm
            .run_before_tool(tool_ctx.clone(), tool.clone(), args.clone())
            .await
        {
            Ok(r) => response = r,
            Err(e) => err = Some(to_tool_error(e)),
        }
    }
    if response.is_none() && err.is_none() {
        (response, err) = invoke_before_tool_callbacks(cbs, &tool_ctx, &tool, &args).await;
    }
    if response.is_none()
        && err.is_none()
        && let Some(r) = tool.as_runnable()
    {
        match r.run(tool_ctx.clone(), Value::Object(args.clone())).await {
            Ok(m) => response = Some(m),
            // Go wraps the sentinel inside the tool, so every callback sees the wrapped error.
            Err(e) => err = Some(Arc::new(wrap_confirmation_error(tool.name(), e))),
        }
    }

    if let Some(e) = err.clone() {
        let (mut er, mut ce): (Option<Map<String, Value>>, Option<Arc<ToolError>>) = (None, None);
        if let Some(pm) = &pm {
            match pm
                .run_on_tool_error(tool_ctx.clone(), tool.clone(), args.clone(), e.clone())
                .await
            {
                Ok(r) => er = r,
                Err(x) => ce = Some(to_tool_error(x)),
            }
        }
        if er.is_none() && ce.is_none() {
            (er, ce) = invoke_on_tool_error_callbacks(cbs, &tool_ctx, &tool, &args, e).await;
        }
        if er.is_some() || ce.is_some() {
            response = er;
            err = ce;
        }
    }

    let (mut ar, mut ae): (Option<Map<String, Value>>, Option<Arc<ToolError>>) = (None, None);
    if let Some(pm) = &pm {
        match pm
            .run_after_tool(
                tool_ctx.clone(),
                tool.clone(),
                args.clone(),
                response.clone().unwrap_or_default(),
                err.clone(),
            )
            .await
        {
            Ok(r) => ar = r,
            Err(x) => ae = Some(to_tool_error(x)),
        }
    }
    if ar.is_none() && ae.is_none() {
        (ar, ae) = invoke_after_tool_callbacks(
            cbs,
            &tool_ctx,
            &tool,
            &args,
            response.clone(),
            err.clone(),
        )
        .await;
    }
    if ar.is_some() || ae.is_some() {
        response = ar;
        err = ae;
    }

    match err {
        Some(e) => Some(error_map(&e.to_string())),
        None => response,
    }
}

/// Go `fmt.Errorf("error tool %q %w", name, tool.ErrConfirmation{Required,Rejected})`
/// (functiontool function.go:205/224, streaming_function.go:161/180, tool.go confirmCall): Go
/// tools wrap the confirmation sentinels with the tool name, and that text reaches the model and
/// the session as `{"error": ...}`. Rust tools return the bare sentinels, so the flow applies the
/// wrap where it receives the tool's error. Other errors pass through unchanged.
fn wrap_confirmation_error(tool_name: &str, e: ToolError) -> ToolError {
    match e {
        ToolError::ConfirmationRequired | ToolError::ConfirmationRejected => {
            ToolError::Execution(Box::new(NamedToolError {
                name: tool_name.to_owned(),
                source: e,
            }))
        }
        other => other,
    }
}

/// Display `error tool "<name>" <sentinel>`; `source()` is the sentinel (Go `%w`), so a
/// source-chain walk still finds `ToolError::ConfirmationRequired`/`ConfirmationRejected`.
#[derive(Debug)]
struct NamedToolError {
    name: String,
    source: ToolError,
}

impl std::fmt::Display for NamedToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "error tool {} {}", go_quote(&self.name), self.source)
    }
}

impl std::error::Error for NamedToolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// `AgentError` → the flow's current `Arc<ToolError>`: an `AgentError::Other` holding a
/// [`SharedToolError`] gives back its `Arc` unchanged (Go passes the same `err` value onward);
/// `AgentError::Tool` is unwrapped; anything else is boxed (Display is transparent).
fn to_tool_error(e: AgentError) -> Arc<ToolError> {
    match e {
        AgentError::Tool { source, .. } => Arc::new(source),
        AgentError::Other(b) => match b.downcast::<SharedToolError>() {
            Ok(shared) => shared.0,
            Err(b) => Arc::new(ToolError::Execution(Box::new(AgentError::Other(b)))),
        },
        other => Arc::new(ToolError::Execution(Box::new(other))),
    }
}

/// Port of base_flow.go:1591 invokeBeforeToolCallbacks.
async fn invoke_before_tool_callbacks(
    cbs: &ToolCallbacks,
    tool_ctx: &Context,
    tool: &Arc<dyn Tool>,
    args: &Map<String, Value>,
) -> (Option<Map<String, Value>>, Option<Arc<ToolError>>) {
    for cb in &cbs.before {
        match cb(tool_ctx.clone(), tool.clone(), args.clone()).await {
            Err(e) => return (None, Some(to_tool_error(e))),
            Ok(Some(r)) => return (Some(r), None),
            Ok(None) => {}
        }
    }
    (None, None)
}

/// Port of base_flow.go:1606 invokeAfterToolCallbacks: with no callback answering, the inputs
/// are returned unchanged.
async fn invoke_after_tool_callbacks(
    cbs: &ToolCallbacks,
    tool_ctx: &Context,
    tool: &Arc<dyn Tool>,
    args: &Map<String, Value>,
    result: Option<Map<String, Value>>,
    err: Option<Arc<ToolError>>,
) -> (Option<Map<String, Value>>, Option<Arc<ToolError>>) {
    for cb in &cbs.after {
        match cb(
            tool_ctx.clone(),
            tool.clone(),
            args.clone(),
            result.clone().unwrap_or_default(),
            err.clone(),
        )
        .await
        {
            Err(e) => return (None, Some(to_tool_error(e))),
            Ok(Some(r)) => return (Some(r), None),
            Ok(None) => {}
        }
    }
    (result, err)
}

/// Port of base_flow.go:1622 invokeOnToolErrorCallbacks: with no callback answering, the
/// original error is returned.
async fn invoke_on_tool_error_callbacks(
    cbs: &ToolCallbacks,
    tool_ctx: &Context,
    tool: &Arc<dyn Tool>,
    args: &Map<String, Value>,
    err: Arc<ToolError>,
) -> (Option<Map<String, Value>>, Option<Arc<ToolError>>) {
    for cb in &cbs.on_error {
        match cb(tool_ctx.clone(), tool.clone(), args.clone(), err.clone()).await {
            Err(e) => return (None, Some(to_tool_error(e))),
            Ok(Some(r)) => return (Some(r), None),
            Ok(None) => {}
        }
    }
    (None, Some(err))
}

/// Port of base_flow.go:1638 mergeParallelFunctionResponseEvents. The first non-`None` event
/// becomes the merged result: its parts are those of every event in slot order and its actions
/// the fold of every event's actions.
pub(crate) fn merge_parallel_function_response_events(
    events: Vec<Option<Event>>,
) -> Result<Option<Event>, AgentError> {
    match events.len() {
        0 => return Ok(None),
        1 => return Ok(events.into_iter().next().flatten()),
        _ => {}
    }
    let mut parts = Vec::new();
    let mut actions: Option<EventActions> = None;
    let mut result: Option<Event> = None;
    for ev in events.into_iter().flatten() {
        let Some(content) = ev.response.content.as_ref() else {
            continue;
        };
        parts.extend(content.parts.iter().cloned());
        actions = merge_event_actions(actions, Some(ev.actions.clone()));
        if result.is_none() {
            result = Some(ev);
        }
    }
    // All entries were None (e.g. every call was long-running/deferred).
    let Some(mut result) = result else {
        return Ok(None);
    };
    result.response = LlmResponse {
        content: Some(Content {
            role: "user".into(),
            parts,
        }),
        ..Default::default()
    };
    result.actions = actions.unwrap_or_default();
    Ok(Some(result))
}

/// Port of base_flow.go:1672 mergeEventActions (adk-python merge_parallel_function_response_events).
pub(crate) fn merge_event_actions(
    base: Option<EventActions>,
    other: Option<EventActions>,
) -> Option<EventActions> {
    let Some(other) = other else { return base };
    let Some(mut base) = base else {
        return Some(other);
    };
    if other.skip_summarization {
        base.skip_summarization = true;
    }
    if !other.transfer_to_agent.is_empty() {
        base.transfer_to_agent = other.transfer_to_agent;
    }
    if other.escalate {
        base.escalate = true;
    }
    if let Some(sd) = other.state_delta {
        base.state_delta = Some(deep_merge_map(
            base.state_delta.take().unwrap_or_default(),
            sd,
        ));
    }
    if let Some(ad) = other.artifact_delta {
        base.artifact_delta = Some(merge_artifact_deltas(base.artifact_delta.take(), ad));
    }
    base.requested_tool_confirmations
        .extend(other.requested_tool_confirmations);
    Some(base)
}

/// Port of base_flow.go:1706 mergeArtifactDeltas: prefers higher versions (a deliberate
/// divergence from adk-python's last-write-wins).
fn merge_artifact_deltas(
    dst: Option<BTreeMap<String, i64>>,
    src: BTreeMap<String, i64>,
) -> BTreeMap<String, i64> {
    let Some(mut dst) = dst else { return src };
    for (k, v) in src {
        dst.entry(k).and_modify(|d| *d = (*d).max(v)).or_insert(v);
    }
    dst
}

/// Port of base_flow.go:1720 deepMergeMap.
fn deep_merge_map(mut dst: Map<String, Value>, src: Map<String, Value>) -> Map<String, Value> {
    for (k, v) in src {
        match (dst.get_mut(&k), v) {
            (Some(Value::Object(d)), Value::Object(s)) => {
                let merged = deep_merge_map(std::mem::take(d), s);
                *d = merged;
            }
            (_, v) => {
                dst.insert(k, v);
            }
        }
    }
    dst
}

/// Port of base_flow.go:1288 displayableToolResultText: the tool result as text for a
/// skip-summarization event, or `None` for results with nothing worth showing (errors, empty or
/// null results). A single plain-text `"result"` is shown as-is; anything else as JSON (sorted
/// keys, not HTML-escaped: Go `SetEscapeHTML(false)`).
pub(crate) fn displayable_tool_result_text(result: &Map<String, Value>) -> Option<String> {
    if result.is_empty() || result.contains_key("error") {
        return None;
    }
    if result.len() == 1
        && let Some(v) = result.get("result")
    {
        match v {
            Value::Null => return None,
            Value::String(s) => return (!s.is_empty()).then(|| s.clone()),
            _ => {}
        }
    }
    serde_json::to_string(result).ok()
}

/// Port of functions.go:32 generateRequestConfirmationEvent: one `adk_request_confirmation` call
/// per function call (in parts order) whose ID has a requested confirmation.
pub(crate) fn generate_request_confirmation_event(
    ctx: &InvocationContext,
    function_call_event: &Event,
    function_response_event: &Event,
) -> Option<Event> {
    let confirmations = &function_response_event.actions.requested_tool_confirmations;
    if confirmations.is_empty() {
        return None;
    }
    let content = function_call_event.response.content.as_ref()?;
    let mut parts = Vec::new();
    let mut long_running_tool_ids = Vec::new();
    for original in &content.parts {
        let Some(fc) = &original.function_call else {
            continue;
        };
        let Some(confirmation) = confirmations.get(&fc.id) else {
            continue;
        };
        let args = Map::from_iter([
            (
                "originalFunctionCall".to_owned(),
                serde_json::to_value(fc).unwrap_or(Value::Null),
            ),
            (
                "toolConfirmation".to_owned(),
                serde_json::to_value(confirmation).unwrap_or(Value::Null),
            ),
        ]);
        let id = generate_function_call_id(ctx.platform());
        long_running_tool_ids.push(id.clone());
        parts.push(Part {
            function_call: Some(FunctionCall {
                id,
                name: REQUEST_CONFIRMATION_FUNCTION_CALL_NAME.to_owned(),
                args,
                ..Default::default()
            }),
            thought_signature: original.thought_signature.clone(),
            ..Default::default()
        });
    }
    if parts.is_empty() {
        return None;
    }
    let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
    ev.author = ctx.agent().name().to_owned();
    ev.branch = ctx.branch();
    ev.response = LlmResponse {
        content: Some(Content {
            role: "model".into(),
            parts,
        }),
        ..Default::default()
    };
    ev.long_running_tool_ids = long_running_tool_ids;
    Some(ev)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use futures::future::BoxFuture;
    use gadk_agent::{AgentConfig, CancelCause, RunnableTool, agent};
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use tokio::sync::oneshot;

    use super::*;
    use crate::llm::flow::span_capture::capture_spans;

    type RunFn = Arc<dyn Fn(Context) -> BoxFuture<'static, Map<String, Value>> + Send + Sync>;

    struct FnTool {
        name: &'static str,
        run: RunFn,
    }

    impl FnTool {
        fn arc(name: &'static str, run: RunFn) -> Arc<dyn Tool> {
            Arc::new(Self { name, run })
        }
    }

    impl Tool for FnTool {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            "fn tool"
        }
        fn as_runnable(&self) -> Option<&dyn RunnableTool> {
            Some(self)
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    #[async_trait]
    impl RunnableTool for FnTool {
        async fn run(&self, ctx: Context, _: Value) -> Result<Map<String, Value>, ToolError> {
            Ok((self.run)(ctx).await)
        }
    }

    /// A tool writing `key = value` to state and returning `{"ok": key}`.
    fn state_tool(name: &'static str, key: &'static str, value: i64) -> Arc<dyn Tool> {
        FnTool::arc(
            name,
            Arc::new(move |ctx: Context| {
                Box::pin(async move {
                    ctx.update_actions(|a| {
                        a.state_delta
                            .get_or_insert_with(Map::new)
                            .insert(key.into(), json!(value));
                    });
                    Map::from_iter([("ok".to_owned(), json!(key))])
                })
            }),
        )
    }

    fn ctx() -> InvocationContext {
        InvocationContext::builder()
            .agent(
                agent::new(AgentConfig {
                    name: "a".into(),
                    ..Default::default()
                })
                .unwrap(),
            )
            .build_invocation()
    }

    fn calls_event(names: &[&str]) -> Event {
        Event {
            response: LlmResponse {
                content: Some(Content {
                    role: "model".into(),
                    parts: names
                        .iter()
                        .enumerate()
                        .map(|(i, n)| Part {
                            function_call: Some(FunctionCall {
                                id: format!("fc{}", i + 1),
                                name: (*n).into(),
                                ..Default::default()
                            }),
                            ..Default::default()
                        })
                        .collect(),
                }),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn tool_map(tools: Vec<Arc<dyn Tool>>) -> HashMap<String, Arc<dyn Tool>> {
        tools
            .into_iter()
            .map(|t| (t.name().to_owned(), t))
            .collect()
    }

    #[tokio::test]
    async fn parallel_calls_merge_in_call_order_with_state_delta() {
        let tools = tool_map(vec![state_tool("t1", "a", 1), state_tool("t2", "b", 2)]);
        let merged = handle_function_calls(
            &ctx(),
            &Flow::default(),
            &tools,
            &calls_event(&["t1", "t2"]),
            None,
        )
        .await
        .unwrap()
        .unwrap();
        let content = merged.response.content.as_ref().unwrap();
        assert_eq!(content.role, "user");
        let ids: Vec<(&str, &str)> = content
            .parts
            .iter()
            .map(|p| {
                let fr = p.function_response.as_ref().unwrap();
                (fr.id.as_str(), fr.name.as_str())
            })
            .collect();
        assert_eq!(ids, [("fc1", "t1"), ("fc2", "t2")]);
        assert_eq!(merged.author, "a");
        assert_eq!(
            merged.actions.state_delta,
            json!({"a": 1, "b": 2}).as_object().cloned()
        );
    }

    #[tokio::test]
    async fn filters_keep_only_listed_calls() {
        let tools = tool_map(vec![state_tool("t1", "a", 1), state_tool("t2", "b", 2)]);
        let keep = HashSet::from(["fc2".to_owned()]);
        let ev = handle_function_calls(
            &ctx(),
            &Flow::default(),
            &tools,
            &calls_event(&["t1", "t2"]),
            Some(&keep),
        )
        .await
        .unwrap()
        .unwrap();
        let parts = &ev.response.content.as_ref().unwrap().parts;
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].function_response.as_ref().unwrap().id, "fc2");
    }

    #[tokio::test]
    async fn unknown_tool_reports_go_not_found_text() {
        let tools = tool_map(vec![state_tool("t2", "b", 2), state_tool("t1", "a", 1)]);
        let ev = handle_function_calls(
            &ctx(),
            &Flow::default(),
            &tools,
            &calls_event(&["nope"]),
            None,
        )
        .await
        .unwrap()
        .unwrap();
        let fr = ev.response.content.as_ref().unwrap().parts[0]
            .function_response
            .clone()
            .unwrap();
        assert_eq!(
            fr.response.get("error").and_then(Value::as_str),
            Some(
                "tool 'nope' not found.\nAvailable tools: t1, t2\n\nPossible causes:\n  1. LLM hallucinated the function name - review agent instruction clarity\n  2. Tool not registered - verify agent.tools list\n  3. Name mismatch - check for typos\n\nSuggested fixes:\n  - Review agent instruction to ensure tool usage is clear\n  - Verify tool is included in agent.tools list\n  - Check for typos in function name"
            )
        );
    }

    #[tokio::test]
    async fn execute_tool_spans_for_parallel_calls() {
        let tools = tool_map(vec![state_tool("t1", "a", 1), state_tool("t2", "b", 2)]);
        let ctx = ctx();
        let (res, spans) = capture_spans(handle_function_calls(
            &ctx,
            &Flow::default(),
            &tools,
            &calls_event(&["t1", "t2"]),
            None,
        ))
        .await;
        assert!(res.unwrap().is_some());
        let mut otel: Vec<&str> = spans
            .iter()
            .filter(|s| s.name == "execute_tool")
            .map(|s| s.otel_name.as_str())
            .collect();
        otel.sort();
        assert_eq!(
            otel,
            [
                "execute_tool (merged)",
                "execute_tool t1",
                "execute_tool t2"
            ]
        );
        assert!(spans.iter().all(|s| s.closed), "{spans:#?}");
        // Go TraceMergedToolCallsResult on the merged span; TraceToolResult on each call's.
        let merged = spans
            .iter()
            .find(|s| s.otel_name == "execute_tool (merged)")
            .unwrap();
        assert_eq!(
            merged.field("gen_ai.tool.name").as_deref(),
            Some("(merged tools)")
        );
        assert_eq!(
            merged.field("gen_ai.tool.description").as_deref(),
            Some("(merged tools)")
        );
        assert!(merged.field("gcp.vertex.agent.event_id").is_some());
        assert!(merged.field("gcp.vertex.agent.tool_response").is_none());
        let t1 = spans
            .iter()
            .find(|s| s.otel_name == "execute_tool t1")
            .unwrap();
        assert_eq!(
            t1.field("gen_ai.operation.name").as_deref(),
            Some("execute_tool")
        );
        assert!(t1.field("gcp.vertex.agent.event_id").is_some());
        assert!(t1.field("gen_ai.tool.description").is_some());
    }

    #[tokio::test]
    async fn tool_error_marks_execute_tool_span() {
        let failing = FnTool::arc("bad", Arc::new(|_| Box::pin(async { error_map("boom") })));
        let tools = tool_map(vec![failing]);
        let ctx = ctx();
        let (_, spans) = capture_spans(handle_function_calls(
            &ctx,
            &Flow::default(),
            &tools,
            &calls_event(&["bad"]),
            None,
        ))
        .await;
        let span = spans
            .iter()
            .find(|s| s.otel_name == "execute_tool bad")
            .unwrap();
        assert_eq!(span.field("otel.status_code").as_deref(), Some("ERROR"));
        assert_eq!(
            span.field("otel.status_description").as_deref(),
            Some("boom")
        );
        assert_eq!(span.field("error.message").as_deref(), Some("boom"));
        assert_eq!(
            span.field("error.type").as_deref(),
            Some("*errors.errorString")
        );
    }

    #[derive(serde::Deserialize, schemars::JsonSchema)]
    struct NoArgs {}

    #[tokio::test]
    async fn streaming_confirmation_errors_carry_go_tool_prefix() {
        use crate::tool::{FunctionToolConfig, StreamingFunctionTool};

        let stream_tool = StreamingFunctionTool::new(
            FunctionToolConfig {
                require_confirmation: true,
                ..FunctionToolConfig::new("secure_stream", "needs approval")
            },
            |_ctx: Context, _: NoArgs| futures::stream::iter([Ok("chunk".to_owned())]),
        )
        .unwrap();
        let tools = tool_map(vec![stream_tool]);
        let error_of = |ev: Event| {
            ev.response.content.unwrap().parts[0]
                .function_response
                .clone()
                .unwrap()
                .response
        };
        // No confirmation yet: required.
        let ev = handle_function_calls(
            &ctx(),
            &Flow::default(),
            &tools,
            &calls_event(&["secure_stream"]),
            None,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            error_of(ev),
            json!({"error": "error tool \"secure_stream\" requires confirmation, please approve or reject"})
                .as_object()
                .unwrap()
                .clone()
        );
        // Rejected confirmation.
        let confs = HashMap::from([("fc1".to_owned(), ToolConfirmation::default())]);
        let ev = handle_function_calls_with(
            &ctx(),
            &Flow::default(),
            &tools,
            &calls_event(&["secure_stream"]),
            None,
            Some(&confs),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            error_of(ev),
            json!({"error": "error tool \"secure_stream\" call is rejected"})
                .as_object()
                .unwrap()
                .clone()
        );
    }

    /// Go `platform.RunTasks` waits for every tool task (exec.go:68): dropping the future does
    /// not abort a running tool (this one ignores cancellation); its task finishes, after-tool
    /// callbacks included, as a teardown the invocation's registry awaits.
    #[tokio::test]
    async fn dropping_the_future_lets_tool_tasks_finish_as_a_teardown() {
        let (started_tx, started_rx) = oneshot::channel::<()>();
        let release = Arc::new(tokio::sync::Notify::new());
        let slots = Arc::new(parking_lot::Mutex::new(Some(started_tx)));
        let gate = release.clone();
        let blocking = FnTool::arc(
            "block",
            Arc::new(move |_| {
                let started = slots.lock().take();
                let gate = gate.clone();
                Box::pin(async move {
                    if let Some(started) = started {
                        let _ = started.send(());
                    }
                    gate.notified().await;
                    Map::new()
                })
            }),
        );
        let after_ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let after: AfterToolCallback = {
            let after_ran = after_ran.clone();
            Arc::new(move |_, _, _, _, _| {
                after_ran.store(true, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async { Ok(None) })
            })
        };
        let tools = tool_map(vec![blocking]);
        let reg = Arc::new(gadk_agent::Teardowns::new());
        let ctx = InvocationContext::builder()
            .agent(
                agent::new(AgentConfig {
                    name: "a".into(),
                    ..Default::default()
                })
                .unwrap(),
            )
            .teardowns(reg.clone())
            .build_invocation();
        let flow = Flow {
            after_tool_callbacks: vec![after],
            ..Default::default()
        };
        let call = calls_event(&["block"]);
        let mut fut = Box::pin(handle_function_calls(&ctx, &flow, &tools, &call, None));
        tokio::select! {
            _ = &mut fut => panic!("handle_function_calls must not complete"),
            r = started_rx => r.unwrap(),
        }
        drop(fut);
        assert!(
            !reg.is_empty(),
            "the running tool call is handed to the registry"
        );
        assert!(!after_ran.load(std::sync::atomic::Ordering::SeqCst));
        release.notify_one();
        reg.wait().await;
        assert!(
            after_ran.load(std::sync::atomic::Ordering::SeqCst),
            "the dropped call's after-tool callbacks ran before the teardown finished"
        );
    }

    /// A Rust drop mid-await is a Go caller cancellation: the dropped dispatch cancels its tool
    /// calls' scope plainly before handing them to the registry, so a tool that waits on its
    /// context ends and the registry wait (the runner's after-run) does not hang. The
    /// invocation's own token stays live.
    #[tokio::test]
    async fn dropping_the_future_cancels_the_tool_calls_scope_before_teardown() {
        let (started_tx, started_rx) = oneshot::channel::<()>();
        let slots = Arc::new(parking_lot::Mutex::new(Some(started_tx)));
        let cause: Arc<parking_lot::Mutex<Option<CancelCause>>> = Arc::default();
        let seen = cause.clone();
        let waiting = FnTool::arc(
            "wait",
            Arc::new(move |ctx: Context| {
                let started = slots.lock().take();
                let seen = seen.clone();
                Box::pin(async move {
                    if let Some(started) = started {
                        let _ = started.send(());
                    }
                    ctx.cancellation_token().cancelled().await;
                    *seen.lock() = ctx.cause();
                    Map::new()
                })
            }),
        );
        let tools = tool_map(vec![waiting]);
        let reg = Arc::new(gadk_agent::Teardowns::new());
        let ctx = InvocationContext::builder()
            .agent(
                agent::new(AgentConfig {
                    name: "a".into(),
                    ..Default::default()
                })
                .unwrap(),
            )
            .teardowns(reg.clone())
            .build_invocation();
        let flow = Flow::default();
        let call = calls_event(&["wait"]);
        let mut fut = Box::pin(handle_function_calls(&ctx, &flow, &tools, &call, None));
        tokio::select! {
            _ = &mut fut => panic!("handle_function_calls must not complete"),
            r = started_rx => r.unwrap(),
        }
        drop(fut);
        tokio::time::timeout(std::time::Duration::from_secs(5), reg.wait())
            .await
            .expect("the registry wait ends: the dropped dispatch cancelled its tool calls");
        assert!(matches!(*cause.lock(), Some(CancelCause::Canceled)));
        assert!(
            !ctx.cancellation_token().is_cancelled(),
            "only the dispatch's own scope is cancelled, not the invocation"
        );
    }

    /// A dispatch that completes gives its scope up without cancelling it: a tool context kept
    /// beyond the call (a live streaming tool's task) stays live until the invocation ends.
    #[tokio::test]
    async fn completed_dispatch_leaves_the_tool_context_live() {
        let kept: Arc<parking_lot::Mutex<Option<Context>>> = Arc::default();
        let keep = kept.clone();
        let tool = FnTool::arc(
            "keep",
            Arc::new(move |ctx: Context| {
                *keep.lock() = Some(ctx);
                Box::pin(async { Map::new() })
            }),
        );
        let ctx = ctx();
        let tools = tool_map(vec![tool]);
        handle_function_calls(
            &ctx,
            &Flow::default(),
            &tools,
            &calls_event(&["keep"]),
            None,
        )
        .await
        .unwrap();
        let kept = kept.lock().take().unwrap();
        assert!(!kept.cancellation_token().is_cancelled());
        ctx.cancel_with_cause(CancelCause::Canceled);
        assert!(kept.cancellation_token().is_cancelled());
    }

    /// Go passes a callback's `err` value onward unchanged: a callback error carrying a
    /// [`SharedToolError`] reaches the later after-tool callbacks as the same `Arc`.
    #[tokio::test]
    async fn shared_tool_error_from_callback_keeps_arc_identity() {
        use std::sync::Mutex;

        let original = Arc::new(ToolError::InvalidArgument("original".into()));
        let seen: Arc<Mutex<Option<Arc<ToolError>>>> = Arc::new(Mutex::new(None));
        let before: BeforeToolCallback = {
            let original = original.clone();
            Arc::new(move |_, _, _| {
                let original = original.clone();
                Box::pin(async move { Err(AgentError::Other(Box::new(SharedToolError(original)))) })
            })
        };
        let after: AfterToolCallback = {
            let seen = seen.clone();
            Arc::new(move |_, _, _, _, err| {
                *seen.lock().unwrap() = err;
                Box::pin(async { Ok(None) })
            })
        };
        let cbs = ToolCallbacks {
            before: vec![before],
            after: vec![after],
            on_error: vec![],
        };
        let tool = state_tool("t", "a", 1);
        let resp = call_tool(&cbs, ctx().into_context(), tool, Map::new()).await;
        assert_eq!(resp, Some(error_map("original")));
        let seen = seen.lock().unwrap().clone().unwrap();
        assert!(
            Arc::ptr_eq(&seen, &original),
            "after-tool got a new Arc: {seen:?}"
        );
    }
}
