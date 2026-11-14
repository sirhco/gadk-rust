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

//! Helpers of the runner-level trace-manager test ports, over the shared
//! [`support`](crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support):
//! Go's `scriptedLLM` / `failingLLM` = [`scripted_llm`] / [`failing_llm`] (gadk-engine
//! `MockModel` renamed), `textContent` = [`text_content`], the transfer / function-call responses,
//! `requireInvocationCompletedOK`, `checkJSONAt`, the ID patterns, and [`RowCursor`]: Go's
//! `collectRows` reads a channel, so each call returns only the rows received since the previous
//! one; the cursor keeps that position over the cumulative [`TestPlugin::rows`].

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use gadk_agent::{Agent, AgentError, BeforeAgentCallback, Context, Tool, ToolError};
use gadk_engine::llm::LlmAgentConfig;
use gadk_engine::llm_agent;
use gadk_engine::testutil::MockModel;
use gadk_engine::tool::function::{FunctionTool, FunctionToolConfig};
use gadk_genai::{Content, FunctionCall, Part};
use gadk_model::{LlmRequest, LlmResponse, ModelError};
use regex::Regex;
use serde_json::{Map, Value};

pub(crate) use crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::{
    check_json_at, text_content,
};
use crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::{
    TestPlugin, TracePlace, event_types, json_object_column, place_of, rows_of_type, str_col,
};
use crate::row::Row;

/// Go `eventIDPattern` (bigquery_agent_analytics_plugin_test.go).
pub(crate) fn event_id_pattern() -> Regex {
    Regex::new("^[0-9a-f]{32}$").unwrap()
}

/// Go `spanIDPattern`.
pub(crate) fn span_id_pattern() -> Regex {
    Regex::new("^[0-9a-f]{16}$").unwrap()
}

/// Go `&model.LLMResponse{Content: textContent("model", text), TurnComplete: true}`.
pub(crate) fn final_text(text: &str) -> Result<LlmResponse, ModelError> {
    Ok(LlmResponse {
        content: Some(text_content("model", text)),
        turn_complete: true,
        ..Default::default()
    })
}

/// Go `genai.NewPartFromFunctionCall(name, args)` (no ID: ADK assigns one).
pub(crate) fn function_call_part(name: &str, args: Value) -> Part {
    Part {
        function_call: Some(FunctionCall {
            name: name.into(),
            args: args.as_object().cloned().unwrap_or_default(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// A model response calling the given functions (Go
/// `genai.NewContentFromParts(parts, genai.RoleModel)`).
pub(crate) fn calls(parts: Vec<Part>) -> Result<LlmResponse, ModelError> {
    Ok(LlmResponse {
        content: Some(Content {
            role: "model".into(),
            parts,
        }),
        ..Default::default()
    })
}

/// Go `transferTo(target)`: a `transfer_to_agent` call.
pub(crate) fn transfer_to(target: &str) -> Result<LlmResponse, ModelError> {
    calls(vec![function_call_part(
        "transfer_to_agent",
        serde_json::json!({"agent_name": target}),
    )])
}

/// Go `&scriptedLLM{responses: …}`: answers each request with the next response (model name
/// `scripted-model`).
pub(crate) fn scripted_llm(
    responses: Vec<Result<LlmResponse, ModelError>>,
) -> Arc<dyn gadk_model::Llm> {
    MockModel::new(responses).name_as("scripted-model")
}

/// A model whose every request is answered by `f(request, stream)` (Go's test-local `model.LLM`
/// types).
pub(crate) struct FnLlm<F> {
    name: String,
    f: F,
}

impl<F> gadk_model::Llm for FnLlm<F>
where
    F: Fn(LlmRequest, bool) -> BoxStream<'static, Result<LlmResponse, ModelError>>
        + Send
        + Sync
        + 'static,
{
    fn name(&self) -> &str {
        &self.name
    }

    fn generate_content(
        &self,
        req: LlmRequest,
        stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        (self.f)(req, stream)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// A model named `name` answering every request with `f(request, stream)`.
pub(crate) fn fn_llm<F>(name: &str, f: F) -> Arc<dyn gadk_model::Llm>
where
    F: Fn(LlmRequest, bool) -> BoxStream<'static, Result<LlmResponse, ModelError>>
        + Send
        + Sync
        + 'static,
{
    Arc::new(FnLlm {
        name: name.into(),
        f,
    })
}

/// Go `&failingLLM{err: err}`: every request fails with `err` (model name `failing-model`).
pub(crate) fn failing_llm(err: &str) -> Arc<dyn gadk_model::Llm> {
    let err = err.to_owned();
    fn_llm("failing-model", move |_, _| {
        futures::stream::iter([Err(ModelError::Other(err.clone().into()))]).boxed()
    })
}

/// Go `functiontool.New(functiontool.Config{Name, Description}, f)` over JSON-object arguments.
pub(crate) fn function_tool<F, Fut>(name: &str, description: &str, f: F) -> Arc<dyn Tool>
where
    F: Fn(Context, Map<String, Value>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Map<String, Value>, ToolError>> + Send + 'static,
{
    FunctionTool::new(FunctionToolConfig::new(name, description), f).expect("function tool")
}

/// `llmagent.New(cfg)`.
pub(crate) fn llm_agent(cfg: LlmAgentConfig) -> Arc<dyn Agent> {
    llm_agent::new(cfg).expect("llm agent")
}

/// A before-agent callback answering `content` (Go `func(agent.Context) (*genai.Content, error)
/// { return content, nil }`).
pub(crate) fn answering_before_agent(content: Content) -> BeforeAgentCallback {
    Arc::new(
        move |_ctx: Context| -> BoxFuture<'static, Result<Option<Content>, AgentError>> {
            let c = content.clone();
            Box::pin(async move { Ok(Some(c)) })
        },
    )
}

/// `checkJSONAt(t, jsonObjectColumn(t, row, columnAttributes), want, path...)`.
pub(crate) fn check_attr(row: &Row, want: &str, path: &[&str]) {
    check_json_at(&json_object_column(row, "attributes"), want, path);
}

/// `row["agent"]`.
pub(crate) fn agent_of(row: &Row) -> String {
    str_col(row, "agent")
}

/// The rows of `event_type` written for `agent`.
pub(crate) fn agent_rows_of_type(rows: &[Row], event_type: &str, agent: &str) -> Vec<Row> {
    rows_of_type(rows, event_type)
        .into_iter()
        .filter(|r| agent_of(r) == agent)
        .collect()
}

/// Go's per-call `collectRows`: the position in the server's rows up to which a previous
/// collection read.
#[derive(Default)]
pub(crate) struct RowCursor {
    seen: usize,
}

impl RowCursor {
    /// Go `collectRows(t, requests, done)`: waits (up to 5 s) until `done` holds for the rows
    /// received after the previous collection, and returns those rows.
    pub(crate) async fn collect(
        &mut self,
        tp: &TestPlugin,
        done: impl Fn(&[Row]) -> bool,
    ) -> Vec<Row> {
        let from = self.seen;
        let wait = async {
            loop {
                let n = tp.server.append_requests().len();
                let rows = tp.rows();
                let fresh = rows.get(from..).unwrap_or_default().to_vec();
                if done(&fresh) {
                    return (rows.len(), fresh);
                }
                tp.server.wait_for_append_requests(n + 1).await;
            }
        };
        let (total, fresh) = tokio::time::timeout(Duration::from_secs(5), wait)
            .await
            .unwrap_or_else(|_| {
                let rows = tp.rows();
                panic!(
                    "timed out collecting rows; got {:?}",
                    event_types(rows.get(from..).unwrap_or_default())
                )
            });
        self.seen = total;
        fresh
    }
}

/// Go `runner.New(runner.Config{AppName: app, Agent: root, SessionService: sessions,
/// AutoCreateSession: auto_create, PluginConfig: {Plugins: plugins}})`.
pub(crate) fn runner_with(
    app: &str,
    root: Arc<dyn Agent>,
    sessions: Arc<dyn gadk_session::SessionService>,
    auto_create: bool,
    plugins: Vec<gadk_agent::Plugin>,
) -> gadk_engine::Runner {
    gadk_engine::Runner::new(gadk_engine::RunnerConfig {
        app_name: app.into(),
        agent: root,
        session_service: sessions,
        artifact_service: None,
        memory_service: None,
        plugin_config: gadk_engine::runner::PluginConfig {
            plugins,
            ..Default::default()
        },
        auto_create_session: auto_create,
        platform: gadk_platform::Platform::default(),
        compaction: None,
    })
    .expect("runner")
}

/// [`runner_with`] over a new in-memory session service with auto-created sessions.
pub(crate) fn app_runner(
    app: &str,
    root: Arc<dyn Agent>,
    plugins: Vec<gadk_agent::Plugin>,
) -> gadk_engine::Runner {
    runner_with(
        app,
        root,
        Arc::new(gadk_session::InMemorySessionService::new()),
        true,
        plugins,
    )
}

/// The AGENT_STARTING place of each agent (the last row of an agent wins).
pub(crate) fn start_by_agent(rows: &[Row]) -> std::collections::HashMap<String, TracePlace> {
    rows_of_type(rows, crate::event_types::EVENT_AGENT_STARTING)
        .iter()
        .map(|r| (agent_of(r), place_of(r)))
        .collect()
}

/// Go `for event, err := range r.Run(…) { …; if stop { break } }`: iterates the run and drops
/// the stream (Go's `break`) after the first item for which `stop` holds; returns the events
/// and the errors read, in order.
pub(crate) async fn run_until(
    runner: &gadk_engine::Runner,
    user_id: &str,
    session_id: &str,
    text: &str,
    cfg: gadk_agent::RunConfig,
    mut stop: impl FnMut(&Result<gadk_session::Event, AgentError>) -> bool,
) -> (Vec<gadk_session::Event>, Vec<AgentError>) {
    let mut events = Vec::new();
    let mut errs = Vec::new();
    let mut stream = runner.run(
        user_id,
        session_id,
        Some(text_content("user", text)),
        cfg,
        gadk_engine::RunOptions::default(),
    );
    while let Some(item) = stream.next().await {
        let done = stop(&item);
        match item {
            Ok(ev) => events.push(ev),
            Err(err) => errs.push(err),
        }
        if done {
            break;
        }
    }
    (events, errs)
}

/// [`run_until`] stopping at the first error (Go `if err != nil { runErr = err; break }`).
pub(crate) async fn run_until_error(
    runner: &gadk_engine::Runner,
    text: &str,
) -> (Vec<gadk_session::Event>, Vec<AgentError>) {
    run_until(
        runner,
        "u1",
        "s1",
        text,
        gadk_agent::RunConfig::default(),
        Result::is_err,
    )
    .await
}

/// A custom agent named `name` running `run` (Go `agent.New(agent.Config{Name, Run})`).
pub(crate) fn custom_agent(
    name: &str,
    sub_agents: Vec<Arc<dyn Agent>>,
    run: impl Fn(gadk_agent::InvocationContext) -> gadk_agent::EventStream + Send + Sync + 'static,
) -> Arc<dyn Agent> {
    gadk_agent::new_agent(gadk_agent::AgentConfig {
        name: name.into(),
        sub_agents,
        run: Some(Arc::new(run)),
        ..Default::default()
    })
    .expect("custom agent")
}

/// A new event of the running invocation (Go `session.NewEvent(ctx, ctx.InvocationID())`).
pub(crate) fn new_event(ctx: &gadk_agent::InvocationContext) -> gadk_session::Event {
    gadk_session::Event::new(ctx.platform(), ctx.invocation_id())
}

/// A plugin named `name` with `config`'s callbacks (Go `baseplugin.New(baseplugin.Config{…})`).
pub(crate) fn plugin_named(name: &str, config: gadk_agent::PluginConfig) -> gadk_agent::Plugin {
    gadk_agent::Plugin::new(gadk_agent::PluginConfig {
        name: name.into(),
        ..config
    })
}

/// Asserts `row`'s `status` and `error_message` columns.
pub(crate) fn assert_status(row: &Row, status: &str, error_message: Option<&str>) {
    let what = format!("{} of {:?}", str_col(row, "event_type"), agent_of(row));
    assert_eq!(str_col(row, "status"), status, "{what} status");
    if let Some(msg) = error_message {
        assert_eq!(str_col(row, "error_message"), msg, "{what} error_message");
    }
}

/// A model stream of `steps`: each item is yielded after its pause (Go's `time.Sleep` before a
/// `yield`), lazily, so a stream dropped by its reader stops there.
pub(crate) fn timed_stream(
    steps: Vec<(Duration, Result<LlmResponse, ModelError>)>,
) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
    futures::stream::iter(steps)
        .then(|(pause, item)| async move {
            if !pause.is_zero() {
                tokio::time::sleep(pause).await;
            }
            item
        })
        .boxed()
}

/// A partial text chunk (Go `&model.LLMResponse{Content: textContent("model", text), Partial:
/// true}`).
pub(crate) fn partial_text(text: &str) -> Result<LlmResponse, ModelError> {
    Ok(LlmResponse {
        content: Some(text_content("model", text)),
        partial: true,
        ..Default::default()
    })
}

/// Go `streamingLLM{leading, chunks, pause, err}` (model name `streaming-model`): when asked to
/// stream, yields `leading`, then each chunk as a partial response after `pause`; after a last
/// `pause`, `err` or the aggregated final response.
pub(crate) fn streaming_llm(
    leading: Vec<LlmResponse>,
    chunks: Vec<&str>,
    pause: Duration,
    err: Option<&str>,
) -> Arc<dyn gadk_model::Llm> {
    let chunks: Vec<String> = chunks.into_iter().map(str::to_owned).collect();
    let err = err.map(str::to_owned);
    fn_llm("streaming-model", move |_, stream| {
        let last = match &err {
            Some(e) => Err(ModelError::Other(e.clone().into())),
            None => final_text(&chunks.concat()),
        };
        if !stream {
            // Go sleeps before each chunk and the last response, yielding only the last.
            let total = pause * u32::try_from(chunks.len() + 1).unwrap_or(u32::MAX);
            return timed_stream(vec![(total, last)]);
        }
        let mut steps: Vec<_> = leading
            .iter()
            .map(|res| (Duration::ZERO, Ok(res.clone())))
            .collect();
        steps.extend(chunks.iter().map(|chunk| (pause, partial_text(chunk))));
        steps.push((pause, last));
        timed_stream(steps)
    })
}

/// A custom agent's event stream computed by `f` when first polled (Go's iterator body).
pub(crate) fn async_stream_of<F>(f: F) -> gadk_agent::EventStream
where
    F: Future<Output = Vec<Result<gadk_session::Event, AgentError>>> + Send + 'static,
{
    futures::stream::once(f)
        .flat_map(futures::stream::iter)
        .boxed()
}

/// A model named `name` answering its `n`th request (from 0) with `f(n)`.
pub(crate) fn counted_llm<F>(name: &str, f: F) -> Arc<dyn gadk_model::Llm>
where
    F: Fn(usize) -> Vec<Result<LlmResponse, ModelError>> + Send + Sync + 'static,
{
    let calls = std::sync::atomic::AtomicUsize::new(0);
    fn_llm(name, move |_, _| {
        let n = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        futures::stream::iter(f(n)).boxed()
    })
}

/// A function call with an ID (Go `&genai.FunctionCall{ID, Name, Args}`).
pub(crate) fn function_call_part_id(id: &str, name: &str, args: Value) -> Part {
    let mut part = function_call_part(name, args);
    if let Some(fc) = part.function_call.as_mut() {
        fc.id = id.into();
    }
    part
}

/// Go `assertAgentSpansClosed(t, rows)`.
pub(crate) fn assert_agent_spans_closed(rows: &[Row]) {
    use crate::event_types::{EVENT_AGENT_COMPLETED, EVENT_AGENT_STARTING};
    let mut starts: std::collections::HashMap<String, Row> = std::collections::HashMap::new();
    let mut ends: std::collections::HashSet<String> = std::collections::HashSet::new();
    for row in rows_of_type(rows, EVENT_AGENT_STARTING) {
        let id = str_col(&row, "span_id");
        assert!(!starts.contains_key(&id), "agent span started twice");
        starts.insert(id, row);
    }
    assert!(!starts.is_empty(), "no agent spans started");
    for row in rows_of_type(rows, EVENT_AGENT_COMPLETED) {
        let id = str_col(&row, "span_id");
        let Some(opened) = starts.get(&id) else {
            panic!(
                "AGENT_COMPLETED of {:?} closes no started span",
                agent_of(&row)
            );
        };
        assert!(ends.insert(id), "agent span completed twice");
        assert!(
            agent_of(&row) == agent_of(opened)
                && str_col(&row, "parent_span_id") == str_col(opened, "parent_span_id"),
            "agent completion changed its owner or parent"
        );
        assert!(
            crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::row_latency(&row).is_some(),
            "AGENT_COMPLETED of {:?} has no latency_ms",
            agent_of(&row)
        );
    }
    assert_eq!(
        ends.len(),
        starts.len(),
        "closed {} of {} agent spans",
        ends.len(),
        starts.len()
    );
}

/// A lazy stream over `inner` whose items `map` turns into the items to yield; `map` returns
/// `true` to end the stream after them (Go's `return` inside a `range` over a sub-agent's
/// events).
pub(crate) fn forward_stream<M>(inner: gadk_agent::EventStream, map: M) -> gadk_agent::EventStream
where
    M: FnMut(
            Result<gadk_session::Event, AgentError>,
        ) -> (Vec<Result<gadk_session::Event, AgentError>>, bool)
        + Send
        + 'static,
{
    let state = (
        Some(inner),
        std::collections::VecDeque::<Result<gadk_session::Event, AgentError>>::new(),
        map,
    );
    futures::stream::unfold(state, |(mut inner, mut pending, mut map)| async move {
        loop {
            if let Some(item) = pending.pop_front() {
                return Some((item, (inner, pending, map)));
            }
            let stream = inner.as_mut()?;
            match stream.next().await {
                None => return None,
                Some(item) => {
                    let (out, stop) = map(item);
                    pending.extend(out);
                    if stop {
                        inner = None;
                    }
                }
            }
        }
    })
    .boxed()
}

/// Go `&scriptedLLM{responses: …}` whose remaining responses the test inspects: the
/// [`MockModel`] itself (its `requests()` count the consumed responses).
pub(crate) fn scripted_mock(responses: Vec<Result<LlmResponse, ModelError>>) -> Arc<MockModel> {
    MockModel::new(responses).name_as("scripted-model")
}

/// An empty stream that runs `f` when polled (Go code after the last `yield` of an iterator).
pub(crate) fn run_then<F>(f: F) -> gadk_agent::EventStream
where
    F: FnOnce() + Send + 'static,
{
    futures::stream::once(async move {
        f();
    })
    .filter_map(|()| async { None::<Result<gadk_session::Event, AgentError>> })
    .boxed()
}

/// Go `for _, err := range sub.Run(ctx) { if err != nil { yield(nil, err); return } }` followed by
/// `after`'s events: drains `sub` without forwarding its events.
pub(crate) fn drain_then(
    sub: Arc<dyn Agent>,
    ctx: gadk_agent::InvocationContext,
    after: impl FnOnce(&gadk_agent::InvocationContext) -> Vec<Result<gadk_session::Event, AgentError>>
    + Send
    + 'static,
) -> gadk_agent::EventStream {
    async_stream_of(async move {
        let mut events = sub.run(ctx.clone());
        while let Some(item) = events.next().await {
            if let Err(e) = item {
                return vec![Err(e)];
            }
        }
        after(&ctx)
    })
}
