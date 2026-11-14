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

//! Part of the port of ADK-Go `plugin/agentanalytics/trace_manager_test.go`: the
//! 30 Go tests from `TestTraceManager_ConcurrentSiblingToolsRace` to
//! `TestTraceManager_ConcurrentInvocations` in the Go file's order that need no runner (the exact
//! list is in the 7b-3a plan's contracts, Appendix A).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![allow(clippy::type_complexity)] // Go's table-driven test cases as tuples.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use gadk_agent::{Agent, Context, InvocationContext, Plugin, Tool, ToolError};
use gadk_engine::llm::LlmAgentConfig;
use gadk_engine::llm_agent;
use gadk_engine::telemetry::d6::SpanIds;
use gadk_engine::testutil::MockModel;
use gadk_genai::{Content, GenerateContentResponseUsageMetadata, Part};
use gadk_model::{LlmRequest, LlmResponse, ModelError};
use gadk_session::{Event, NodeInfo};
use serde_json::{Map, Value, json};

use super::direct_support::{
    MockRun, SpanCounts, fake_tool, first_token_timed, open_spans, text_content as text,
    tracked_invocations,
};
use crate::batch_processor::BatchProcessor;
use crate::event_types::{
    EVENT_AGENT_COMPLETED, EVENT_AGENT_RESPONSE, EVENT_AGENT_STARTING, EVENT_INVOCATION_COMPLETED,
    EVENT_INVOCATION_ERROR, EVENT_INVOCATION_STARTING, EVENT_LLM_ERROR, EVENT_LLM_REQUEST,
    EVENT_LLM_RESPONSE, EVENT_TOOL_COMPLETED, EVENT_TOOL_ERROR, EVENT_TOOL_STARTING,
    EVENT_USER_MESSAGE_RECEIVED,
};
use crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::check_json_at;
use crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::{
    MockCtx, TracePlace, check_json, default_config, event_types, json_column, json_object_column,
    lookup, must_latency, new_clocked_plugin, new_test_plugin_with_config, only_row_of_type,
    place_of, queue_processor, row_latency, rows_of_type, setup_test_plugin, str_col, take_rows,
    until_event_type,
};
use crate::plugin::AnalyticsPlugin;
use crate::plugin::rows::agent_starting_data;
use crate::redaction::COLUMN_LATENCY_MS;
use crate::row::{Row, RowValue};
use crate::trace::testkit::ManualClock;
use crate::trace::{
    CallbackCtx, Identity, Latency, RowSpan, SWEEP_INTERVAL, TraceManager, tool_key,
};

// ---------------------------------------------------------------------------------------------
// File-local helpers
// ---------------------------------------------------------------------------------------------

/// Go `spanIDPattern`: 16 lowercase hex digits.
fn is_span_id(s: &str) -> bool {
    s.len() == 16 && s.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'))
}

/// Go `eventIDPattern`: 32 lowercase hex digits.
fn is_trace_id(s: &str) -> bool {
    s.len() == 32 && s.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'))
}

/// Go `identity{invocationID: inv, agentName: agent, branch: branch}`.
fn id_of(inv: &str, agent: &str, branch: &str) -> Identity {
    Identity {
        invocation_id: inv.into(),
        agent_name: agent.into(),
        branch: branch.into(),
        ..Identity::default()
    }
}

/// Go `checkJSON(t, name, got, want)`.
fn assert_json(name: &str, got: &Value, want: &str) {
    if let Err(e) = check_json(name, got, want) {
        panic!("{e}");
    }
}

/// Go `map[string]any{...}` tool arguments.
fn args(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        other => panic!("{other} is not an object"),
    }
}

/// Go's plugin callbacks (`p.BeforeAgentCallback()(ctx)`, …) over a gadk [`Plugin`]; each one
/// fails the test on a callback error (Go `callbackErrors.check`).
struct Cb<'a>(&'a Plugin);

impl Cb<'_> {
    async fn user_message(&self, ctx: InvocationContext, msg: Content) {
        self.0.on_user_message_callback().unwrap()(ctx, msg)
            .await
            .unwrap();
    }
    async fn before_run(&self, ctx: InvocationContext) {
        self.0.before_run_callback().unwrap()(ctx).await.unwrap();
    }
    async fn after_run(&self, ctx: InvocationContext) {
        self.0.after_run_callback().unwrap()(ctx).await;
    }
    async fn on_event(&self, ctx: InvocationContext, ev: Event) {
        self.0.on_event_callback().unwrap()(ctx, ev).await.unwrap();
    }
    async fn before_agent(&self, ctx: Context) {
        self.0.before_agent_callback().unwrap()(ctx).await.unwrap();
    }
    async fn after_agent(&self, ctx: Context) {
        self.0.after_agent_callback().unwrap()(ctx).await.unwrap();
    }
    async fn before_model(&self, ctx: Context, req: LlmRequest) {
        self.0.before_model_callback().unwrap()(ctx, req)
            .await
            .1
            .unwrap();
    }
    async fn after_model(&self, ctx: Context, res: LlmResponse) {
        self.0.after_model_callback().unwrap()(ctx, res, None)
            .await
            .1
            .unwrap();
    }
    async fn on_model_error(&self, ctx: Context, req: LlmRequest, err: &str) {
        let err = Arc::new(ModelError::Other(err.into()));
        self.0.on_model_error_callback().unwrap()(ctx, req, err)
            .await
            .unwrap();
    }
    async fn before_tool(&self, ctx: Context, t: &Arc<dyn Tool>, a: Map<String, Value>) {
        self.0.before_tool_callback().unwrap()(ctx, t.clone(), a)
            .await
            .unwrap();
    }
    async fn after_tool(
        &self,
        ctx: Context,
        t: &Arc<dyn Tool>,
        a: Map<String, Value>,
        result: Map<String, Value>,
        err: Option<&str>,
    ) {
        let err = err.map(|e| Arc::new(ToolError::Execution(e.into())));
        self.0.after_tool_callback().unwrap()(ctx, t.clone(), a, result, err)
            .await
            .unwrap();
    }
    async fn on_tool_error(
        &self,
        ctx: Context,
        t: &Arc<dyn Tool>,
        a: Map<String, Value>,
        err: &str,
    ) {
        let err = Arc::new(ToolError::Execution(err.into()));
        self.0.on_tool_error_callback().unwrap()(ctx, t.clone(), a, err)
            .await
            .unwrap();
    }
}

fn gemini_test_request() -> LlmRequest {
    LlmRequest {
        model: "gemini-test".into(),
        ..LlmRequest::default()
    }
}

fn response(content: Content) -> LlmResponse {
    LlmResponse {
        content: Some(content),
        ..LlmResponse::default()
    }
}

fn partial(content: Content) -> LlmResponse {
    LlmResponse {
        content: Some(content),
        partial: true,
        ..LlmResponse::default()
    }
}

fn final_response(content: Content) -> LlmResponse {
    LlmResponse {
        content: Some(content),
        turn_complete: true,
        ..LlmResponse::default()
    }
}

/// An analytics plugin with the default config whose rows wait in a processor that never
/// starts, for tests that hand its synchronous callbacks a [`CallbackCtx`] of their own.
async fn queued_plugin() -> (Arc<AnalyticsPlugin>, Arc<BatchProcessor>) {
    let config = default_config();
    let processor = queue_processor(&config).await;
    let ap = Arc::new(AnalyticsPlugin {
        config,
        processor: Some(Arc::clone(&processor)),
        traces: Some(Arc::new(TraceManager::new())),
        ..AnalyticsPlugin::default()
    });
    (ap, processor)
}

/// `ends` and `parents` checks shared by the span-tree tests.
fn check_parents(parents: &[(&str, &RowSpan, &RowSpan)]) {
    for (name, child, parent) in parents {
        assert_eq!(
            child.parent_span_id, parent.span_id,
            "{name}: parent span {:?}, want {:?}",
            child.parent_span_id, parent.span_id
        );
    }
}

fn check_ends(ends: &[(&str, &RowSpan, &RowSpan)], trace_id: Option<&str>) {
    for (name, start, end) in ends {
        assert!(
            end.span_id == start.span_id && end.parent_span_id == start.parent_span_id,
            "{name} ended at span {:?} under {:?}, want {:?} under {:?}",
            end.span_id,
            end.parent_span_id,
            start.span_id,
            start.parent_span_id
        );
        assert!(end.latency.is_some(), "{name} ended without latency");
        if let Some(trace) = trace_id {
            assert_eq!(end.trace_id, trace, "{name}: trace");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

/// Go `TestTraceManager_ConcurrentSiblingToolsRace`: 20 tool calls of an agent start before any
/// of them ends, each on a task of its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_trace_manager_concurrent_sibling_tools_race() {
    const CALLS: usize = 20;
    let tp = setup_test_plugin().await;
    let run = MockRun::new(&tp.ctx);
    let cb = Cb(&tp.plugin);
    let lookup_tool = fake_tool("lookup");

    cb.before_agent(run.ctx()).await;
    let mut handles = Vec::new();
    for i in 0..CALLS {
        let f = tp.plugin.before_tool_callback().unwrap();
        let (ctx, t) = (run.tool(&format!("call-{i}")), lookup_tool.clone());
        let call = format!("call-{i}");
        handles.push(tokio::spawn(async move {
            f(ctx, t, args(json!({"call": call}))).await.unwrap();
        }));
    }
    for h in handles.drain(..) {
        h.await.unwrap();
    }
    for i in 0..CALLS {
        let f = tp.plugin.after_tool_callback().unwrap();
        let (ctx, t) = (run.tool(&format!("call-{i}")), lookup_tool.clone());
        let call = format!("call-{i}");
        handles.push(tokio::spawn(async move {
            f(ctx, t, Map::new(), args(json!({"call": call})), None)
                .await
                .unwrap();
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    cb.after_agent(run.ctx()).await;
    cb.after_run(run.inv()).await;

    let rows = tp
        .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;
    let agent_start = place_of(&only_row_of_type(&rows, EVENT_AGENT_STARTING));
    let mut starts: HashMap<String, TracePlace> = HashMap::new();
    for row in rows_of_type(&rows, EVENT_TOOL_STARTING) {
        starts.insert(function_call_id(&row), place_of(&row));
    }
    assert_eq!(starts.len(), CALLS, "TOOL_STARTING rows for calls");
    let mut spans = HashSet::new();
    for (id, start) in &starts {
        assert_eq!(
            start.parent, agent_start.span,
            "TOOL_STARTING of {id}: parent_span_id"
        );
        assert!(is_span_id(&start.span), "TOOL_STARTING of {id}: span_id");
        spans.insert(start.span.clone());
    }
    assert_eq!(spans.len(), CALLS, "distinct spans of the calls");
    let completes = rows_of_type(&rows, EVENT_TOOL_COMPLETED);
    assert_eq!(completes.len(), CALLS, "TOOL_COMPLETED rows");
    for row in &completes {
        let id = function_call_id(row);
        assert_eq!(place_of(row), starts[&id], "TOOL_COMPLETED of {id}");
        must_latency(row);
    }
    let agent_end = only_row_of_type(&rows, EVENT_AGENT_COMPLETED);
    assert_eq!(place_of(&agent_end), agent_start, "AGENT_COMPLETED place");
    must_latency(&agent_end);
}

/// Go `functionCallIDOf(t, row)`.
fn function_call_id(row: &Row) -> String {
    crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::function_call_id_of(row)
}

/// Go `TestTraceManager_OnToolErrorThenAfterTool_NoDoublePop`.
#[tokio::test]
async fn test_trace_manager_on_tool_error_then_after_tool_no_double_pop() {
    let tp = setup_test_plugin().await;
    let run = MockRun::new(&tp.ctx);
    let cb = Cb(&tp.plugin);
    let lookup_tool = fake_tool("lookup");
    let a = args(json!({"query": "weather"}));
    cb.before_agent(run.ctx()).await;
    cb.before_tool(run.tool("call-1"), &lookup_tool, a.clone())
        .await;
    cb.on_tool_error(run.tool("call-1"), &lookup_tool, a.clone(), "lookup failed")
        .await;
    cb.after_tool(
        run.tool("call-1"),
        &lookup_tool,
        a,
        Map::new(),
        Some("lookup failed"),
    )
    .await;
    cb.after_agent(run.ctx()).await;
    cb.after_run(run.inv()).await;

    let rows = tp
        .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;
    assert_eq!(
        event_types(&rows),
        [
            EVENT_AGENT_STARTING,
            EVENT_TOOL_STARTING,
            EVENT_TOOL_ERROR,
            EVENT_AGENT_COMPLETED,
            EVENT_INVOCATION_COMPLETED
        ]
    );
    let p: Vec<TracePlace> = rows.iter().map(place_of).collect();
    let (agent_start, tool_start, tool_error, agent_end, invocation_end) =
        (&p[0], &p[1], &p[2], &p[3], &p[4]);
    assert_eq!(tool_start.parent, agent_start.span, "TOOL_STARTING parent");
    assert_eq!(tool_error, tool_start, "TOOL_ERROR place");
    must_latency(&rows[2]);
    assert_eq!(agent_end, agent_start, "AGENT_COMPLETED place");
    must_latency(&rows[3]);
    assert_eq!(
        invocation_end.span, agent_start.parent,
        "INVOCATION_COMPLETED span_id"
    );
    must_latency(&rows[4]);
}

/// Go `TestTraceManager_AmbientOTelTraceIDInherited`.
// parity: adapted: the ambient OpenTelemetry span is set on the callbacks' CallbackCtx (Go `trace.ContextWithSpanContext`); gadk callbacks read it from the tracing span and the crate has no subscriber in tests, so the synchronous callbacks of a queued-rows plugin are called directly
#[tokio::test]
async fn test_trace_manager_ambient_o_tel_trace_id_inherited() {
    const TRACE_HEX: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
    const SPAN_HEX: &str = "00f067aa0ba902b7";
    let mut trace_id = [0u8; 16];
    let mut span_id = [0u8; 8];
    for (i, b) in trace_id.iter_mut().enumerate() {
        *b = u8::from_str_radix(&TRACE_HEX[2 * i..2 * i + 2], 16).unwrap();
    }
    for (i, b) in span_id.iter_mut().enumerate() {
        *b = u8::from_str_radix(&SPAN_HEX[2 * i..2 * i + 2], 16).unwrap();
    }
    let sc = SpanIds { trace_id, span_id };
    let (p, processor) = queued_plugin().await;
    let run = MockRun::new(&MockCtx::new());
    let ctx = run.cb().with_span(sc);
    let tool_ctx = run.tool_cb("call-1").with_span(sc);
    let lookup_tool = fake_tool("lookup");

    p.on_user_message(&ctx, &text("user", "hello"));
    p.before_run(&ctx);
    p.before_agent(&ctx);
    p.before_model(&ctx, &gemini_test_request());
    p.after_model(&ctx, &response(text("model", "calling lookup")), None);
    p.before_tool(&tool_ctx, &lookup_tool, &Map::new());
    p.after_tool(
        &tool_ctx,
        &lookup_tool,
        &Map::new(),
        &args(json!({"answer": "sunny"})),
        None,
    );
    p.on_event(
        &ctx,
        &Event {
            id: "event-1".into(),
            author: "test-agent".into(),
            response: response(text("model", "It is sunny.")),
            ..Event::default()
        },
    );
    p.after_agent(&ctx);
    p.after_run(&ctx);

    let rows = take_rows(Some(&processor));
    let invocation = place_of(&only_row_of_type(&rows, EVENT_INVOCATION_STARTING));
    let agent_span = place_of(&only_row_of_type(&rows, EVENT_AGENT_STARTING));
    let llm_span = place_of(&only_row_of_type(&rows, EVENT_LLM_REQUEST));
    let tool_span = place_of(&only_row_of_type(&rows, EVENT_TOOL_STARTING));
    let mut spans = HashSet::new();
    for s in [
        &invocation.span,
        &agent_span.span,
        &llm_span.span,
        &tool_span.span,
    ] {
        assert!(
            is_span_id(s) && s != SPAN_HEX,
            "span_id {s:?}: want 16 hex digits of a span of the invocation"
        );
        spans.insert(s.clone());
    }
    assert_eq!(spans.len(), 4, "distinct spans");

    let place = |span: &str, parent: &str| TracePlace {
        trace: TRACE_HEX.into(),
        span: span.into(),
        parent: parent.into(),
    };
    let in_invocation = place(&invocation.span, SPAN_HEX);
    let in_agent = place(&agent_span.span, &invocation.span);
    let want_places: HashMap<&str, TracePlace> = HashMap::from([
        (EVENT_USER_MESSAGE_RECEIVED, in_invocation.clone()),
        (EVENT_INVOCATION_STARTING, in_invocation.clone()),
        (EVENT_INVOCATION_COMPLETED, in_invocation.clone()),
        (EVENT_AGENT_STARTING, in_agent.clone()),
        (EVENT_AGENT_COMPLETED, in_agent.clone()),
        (EVENT_LLM_REQUEST, place(&llm_span.span, &agent_span.span)),
        (EVENT_LLM_RESPONSE, place(&llm_span.span, &agent_span.span)),
        (
            EVENT_TOOL_STARTING,
            place(&tool_span.span, &agent_span.span),
        ),
        (
            EVENT_TOOL_COMPLETED,
            place(&tool_span.span, &agent_span.span),
        ),
    ]);
    let ends_span: HashSet<&str> = HashSet::from([
        EVENT_INVOCATION_COMPLETED,
        EVENT_AGENT_COMPLETED,
        EVENT_LLM_RESPONSE,
        EVENT_TOOL_COMPLETED,
    ]);
    let mut saw_event = false;
    for row in &rows {
        let event_type = str_col(row, "event_type");
        let attrs = json_object_column(row, "attributes");
        let mut want = want_places.get(event_type.as_str()).cloned();
        // Rows that describe the session event belong to the agent that wrote it.
        if lookup(&attrs, &["adk", "source_event_id"]) == Some(&json!("event-1")) {
            want = Some(in_agent.clone());
            saw_event = true;
        }
        let Some(want) = want else {
            panic!("unexpected {event_type} row");
        };
        assert_eq!(place_of(row), want, "{event_type} row place");
        assert_eq!(
            row_latency(row).is_some(),
            ends_span.contains(event_type.as_str()),
            "{event_type} row has latency_ms"
        );
        check_json_at(&attrs, r#""test-agent""#, &["root_agent_name"]);
    }
    assert!(
        saw_event,
        "no row describes the session event; event types: {:?}",
        event_types(&rows)
    );
}

/// Go `TestTraceManager_StreamingTTFTAndSingleMarshaledLatency`.
#[tokio::test]
async fn test_trace_manager_streaming_ttft_and_single_marshaled_latency() {
    const GAP: Duration = Duration::from_millis(20);
    let tp = setup_test_plugin().await;
    let run = MockRun::new(&tp.ctx);
    let cb = Cb(&tp.plugin);
    cb.before_model(run.ctx(), gemini_test_request()).await;
    tokio::time::sleep(GAP).await;
    cb.after_model(run.ctx(), partial(text("model", "Hel")))
        .await;
    tokio::time::sleep(GAP).await;
    cb.after_model(run.ctx(), partial(text("model", "lo")))
        .await;
    cb.after_model(run.ctx(), final_response(text("model", "Hello")))
        .await;
    cb.after_run(run.inv()).await;

    let rows = tp
        .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;
    assert_eq!(
        event_types(&rows),
        [
            EVENT_LLM_REQUEST,
            EVENT_LLM_RESPONSE,
            EVENT_INVOCATION_COMPLETED
        ]
    );
    let (request, resp) = (&rows[0], &rows[1]);
    assert_eq!(place_of(resp), place_of(request), "LLM_RESPONSE place");
    let raw = match resp.get(COLUMN_LATENCY_MS) {
        Some(RowValue::Str(s)) => s.clone(),
        other => panic!("LLM_RESPONSE latency_ms = {other:?}, want JSON text"),
    };
    let decoded: Value = serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("latency_ms {raw:?} is not JSON: {e}"));
    assert!(
        decoded.is_object(),
        "latency_ms = {raw}, want a JSON object, not JSON text encoded twice"
    );
    let latency = must_latency(resp);
    assert_eq!(
        latency.keys().cloned().collect::<Vec<_>>(),
        ["time_to_first_token_ms", "total_ms"]
    );
    let (ttft, total) = (latency["time_to_first_token_ms"], latency["total_ms"]);
    let want_min = i64::try_from(GAP.as_millis()).unwrap();
    assert!(
        ttft >= want_min && total - ttft >= want_min,
        "latency_ms = {raw}, want time_to_first_token_ms >= {want_min} and total_ms >= time_to_first_token_ms + {want_min}"
    );
    check_json_at(
        &json_object_column(resp, "attributes"),
        "false",
        &["partial"],
    );
    assert_json(
        "content",
        &json_object_column(resp, "content"),
        r#"{"response":"text: 'Hello'"}"#,
    );
}

/// Go `TestAfterModel_FirstTokenIsFirstGeneratedContent`.
#[tokio::test]
async fn test_after_model_first_token_is_first_generated_content() {
    let usage = GenerateContentResponseUsageMetadata {
        prompt_token_count: 10,
        ..Default::default()
    };
    let role_only = |parts: Vec<Part>| LlmResponse {
        partial: true,
        content: Some(Content {
            role: "model".into(),
            parts,
        }),
        ..LlmResponse::default()
    };
    let tests: Vec<(&str, LlmResponse)> = vec![
        (
            "usage only",
            LlmResponse {
                partial: true,
                usage_metadata: Some(usage.clone()),
                ..LlmResponse::default()
            },
        ),
        ("role only", role_only(vec![])),
        // Go's nil and empty Parts slices are the same empty Vec in gadk.
        ("empty parts", role_only(vec![])),
        ("empty part", role_only(vec![Part::default()])),
    ];
    for (name, leading) in tests {
        let p = new_clocked_plugin().await;
        let run = MockRun::new(&MockCtx::named("agent", "inv-first-token"));
        let ctx = run.cb();
        let start = p.clock.now();
        let at = |ms: i64| {
            let delta = (start + chrono::Duration::milliseconds(ms)) - p.clock.now();
            p.clock.advance_ms(delta.num_milliseconds());
        };
        p.plugin.before_model(&ctx, &gemini_test_request());
        at(10);
        p.plugin.after_model(&ctx, &leading, None);
        assert!(
            !first_token_timed(&p, &ctx),
            "{name}: a chunk without generated content timed the first token"
        );
        at(800);
        p.plugin
            .after_model(&ctx, &partial(text("model", "Hel")), None);
        assert!(
            first_token_timed(&p, &ctx),
            "{name}: the first chunk with generated content did not time the first token"
        );
        at(900);
        p.plugin
            .after_model(&ctx, &partial(text("model", "lo")), None);
        at(1000);
        p.plugin
            .after_model(&ctx, &final_response(text("model", "Hello")), None);

        let resp = only_row_of_type(&p.take_rows(), EVENT_LLM_RESPONSE);
        let want = BTreeMap::from([
            ("time_to_first_token_ms".to_string(), 800),
            ("total_ms".to_string(), 1000),
        ]);
        assert_eq!(must_latency(&resp), want, "{name}: LLM_RESPONSE latency_ms");
    }
}

/// Go `TestAfterModel_MetadataOnlyChunksLeaveFirstTokenToFinalResponse`.
#[tokio::test]
async fn test_after_model_metadata_only_chunks_leave_first_token_to_final_response() {
    let p = new_clocked_plugin().await;
    let run = MockRun::new(&MockCtx::named("agent", "inv-metadata-only"));
    let ctx = run.cb();
    p.plugin.before_model(&ctx, &gemini_test_request());
    p.clock.advance_ms(10);
    p.plugin.after_model(
        &ctx,
        &LlmResponse {
            partial: true,
            usage_metadata: Some(GenerateContentResponseUsageMetadata {
                prompt_token_count: 10,
                ..Default::default()
            }),
            ..LlmResponse::default()
        },
        None,
    );
    p.clock.advance_ms(990);
    p.plugin
        .after_model(&ctx, &final_response(text("model", "Hello")), None);

    let resp = only_row_of_type(&p.take_rows(), EVENT_LLM_RESPONSE);
    let want = BTreeMap::from([
        ("time_to_first_token_ms".to_string(), 1000),
        ("total_ms".to_string(), 1000),
    ]);
    assert_eq!(must_latency(&resp), want, "LLM_RESPONSE latency_ms");
}

/// Go `TestTraceManager_UnknownToolErrorKeepsAgentSpan`.
#[tokio::test]
async fn test_trace_manager_unknown_tool_error_keeps_agent_span() {
    let tp = setup_test_plugin().await;
    let run = MockRun::new(&tp.ctx);
    let cb = Cb(&tp.plugin);
    cb.before_agent(run.ctx()).await;
    cb.on_tool_error(
        run.tool("call-1"),
        &fake_tool("missing"),
        Map::new(),
        "tool not found",
    )
    .await;
    cb.after_agent(run.ctx()).await;
    cb.after_run(run.inv()).await;

    let rows = tp
        .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;
    assert_eq!(
        event_types(&rows),
        [
            EVENT_AGENT_STARTING,
            EVENT_TOOL_ERROR,
            EVENT_AGENT_COMPLETED,
            EVENT_INVOCATION_COMPLETED
        ]
    );
    let (agent_start, tool_error, agent_end) =
        (place_of(&rows[0]), place_of(&rows[1]), place_of(&rows[2]));
    assert!(
        tool_error.parent == agent_start.span
            && tool_error.span != agent_start.span
            && is_span_id(&tool_error.span),
        "TOOL_ERROR is at {tool_error:?}, want a span of its own under the agent span {:?}",
        agent_start.span
    );
    assert!(
        row_latency(&rows[1]).is_none(),
        "TOOL_ERROR of a call that never started has latency_ms"
    );
    assert_eq!(agent_end, agent_start, "AGENT_COMPLETED place");
    must_latency(&rows[2]);
}

/// Go `TestTraceManager_ModelCallSpans`.
#[tokio::test]
async fn test_trace_manager_model_call_spans() {
    let request = gemini_test_request;
    let resp = || response(text("model", "hi"));

    // response without streaming
    {
        let tp = setup_test_plugin().await;
        let run = MockRun::new(&tp.ctx);
        let cb = Cb(&tp.plugin);
        cb.before_model(run.ctx(), request()).await;
        tokio::time::sleep(Duration::from_millis(5)).await;
        cb.after_model(run.ctx(), resp()).await;
        cb.after_run(run.inv()).await;
        let rows = tp
            .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
            .await;
        let latency = must_latency(&only_row_of_type(&rows, EVENT_LLM_RESPONSE));
        // As in adk-python, the whole response arrives with the first token.
        assert_eq!(
            latency.get("time_to_first_token_ms"),
            latency.get("total_ms"),
            "latency_ms = {latency:?}, want time_to_first_token_ms equal to total_ms"
        );
    }

    // model error
    {
        let tp = setup_test_plugin().await;
        let run = MockRun::new(&tp.ctx);
        let cb = Cb(&tp.plugin);
        cb.before_model(run.ctx(), request()).await;
        cb.on_model_error(run.ctx(), request(), "model unavailable")
            .await;
        cb.after_run(run.inv()).await;
        let rows = tp
            .collect_rows(until_event_type(EVENT_INVOCATION_ERROR))
            .await;
        let llm_error = only_row_of_type(&rows, EVENT_LLM_ERROR);
        assert_eq!(
            place_of(&llm_error),
            place_of(&only_row_of_type(&rows, EVENT_LLM_REQUEST)),
            "LLM_ERROR place"
        );
        let latency = must_latency(&llm_error);
        assert_eq!(
            latency.keys().cloned().collect::<Vec<_>>(),
            ["total_ms"],
            "LLM_ERROR latency_ms = {latency:?}, want total_ms only"
        );
    }

    // response after a model error: when a plugin turns a model error into a response, ADK
    // runs AfterModelCallback for a call that OnModelErrorCallback ended.
    {
        let tp = setup_test_plugin().await;
        let run = MockRun::new(&tp.ctx);
        let cb = Cb(&tp.plugin);
        cb.before_model(run.ctx(), request()).await;
        cb.on_model_error(run.ctx(), request(), "model unavailable")
            .await;
        cb.after_model(run.ctx(), resp()).await;
        cb.after_run(run.inv()).await;
        let rows = tp
            .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
            .await;
        let req = only_row_of_type(&rows, EVENT_LLM_REQUEST);
        let llm_err = only_row_of_type(&rows, EVENT_LLM_ERROR);
        let res = only_row_of_type(&rows, EVENT_LLM_RESPONSE);
        assert_eq!(place_of(&res), place_of(&req), "LLM_RESPONSE place");
        assert_eq!(place_of(&llm_err), place_of(&req), "LLM_ERROR place");
        must_latency(&llm_err);
        assert!(
            row_latency(&res).is_none(),
            "LLM_RESPONSE of an ended call has latency_ms"
        );
    }

    // request that gets no response: a BeforeModelCallback that answers a request ends the
    // call without AfterModelCallback. The next request replaces it.
    {
        let tp = setup_test_plugin().await;
        let run = MockRun::new(&tp.ctx);
        let cb = Cb(&tp.plugin);
        cb.before_model(run.ctx(), request()).await;
        cb.before_model(run.ctx(), request()).await;
        cb.after_model(run.ctx(), resp()).await;
        cb.after_run(run.inv()).await;
        let rows = tp
            .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
            .await;
        let reqs = rows_of_type(&rows, EVENT_LLM_REQUEST);
        assert_eq!(reqs.len(), 2, "LLM_REQUEST rows");
        assert_eq!(
            place_of(&only_row_of_type(&rows, EVENT_LLM_RESPONSE)),
            place_of(&reqs[1]),
            "LLM_RESPONSE is at the place of the second LLM_REQUEST"
        );
    }
}

/// Go `TestTraceManager_AfterAgentWithoutBeforeAgent`.
#[tokio::test]
async fn test_trace_manager_after_agent_without_before_agent() {
    let tp = setup_test_plugin().await;
    let run = MockRun::new(&tp.ctx);
    let cb = Cb(&tp.plugin);
    cb.user_message(run.inv(), text("user", "hello")).await;
    cb.after_agent(run.ctx()).await;
    cb.after_run(run.inv()).await;

    let rows = tp
        .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;
    assert_eq!(
        event_types(&rows),
        [
            EVENT_USER_MESSAGE_RECEIVED,
            EVENT_AGENT_COMPLETED,
            EVENT_INVOCATION_COMPLETED
        ]
    );
    let (invocation, agent_end, invocation_end) =
        (place_of(&rows[0]), place_of(&rows[1]), place_of(&rows[2]));
    assert!(
        agent_end.parent == invocation.span && agent_end.span != invocation.span,
        "AGENT_COMPLETED is at {agent_end:?}, want a span of its own under the invocation span {:?}",
        invocation.span
    );
    assert!(
        row_latency(&rows[1]).is_none(),
        "AGENT_COMPLETED of an agent that never started has latency_ms"
    );
    assert_eq!(invocation_end, invocation, "INVOCATION_COMPLETED place");
    must_latency(&rows[2]);
}

/// Go `TestTraceManager_InvocationsHaveTracesOfTheirOwn`.
#[tokio::test]
async fn test_trace_manager_invocations_have_traces_of_their_own() {
    let tp = setup_test_plugin().await;
    let run = MockRun::new(&tp.ctx);
    let other = MockRun::new(&MockCtx {
        invocation_id: "inv-2".into(),
        ..tp.ctx.clone()
    });
    let cb = Cb(&tp.plugin);
    cb.user_message(run.inv(), text("user", "hello")).await;
    cb.user_message(other.inv(), text("user", "hi")).await;
    cb.after_run(run.inv()).await;
    cb.after_run(other.inv()).await;

    let rows = tp
        .collect_rows(|rows: &[Row]| rows_of_type(rows, EVENT_INVOCATION_COMPLETED).len() == 2)
        .await;
    let mut traces: HashMap<String, String> = HashMap::new();
    for row in &rows {
        let invocation_id = str_col(row, "invocation_id");
        let got = place_of(row);
        assert!(
            is_trace_id(&got.trace) && is_span_id(&got.span) && got.parent.is_empty(),
            "{} row of {invocation_id} is at {got:?}, want a 32-digit trace and a 16-digit span without parent",
            str_col(row, "event_type")
        );
        if let Some(seen) = traces.get(&invocation_id) {
            assert_eq!(seen, &got.trace, "rows of {invocation_id}: one trace");
        }
        traces.insert(invocation_id, got.trace);
    }
    assert!(
        traces.len() == 2 && traces["inv-id"] != traces["inv-2"],
        "trace_id by invocation = {traces:?}, want a trace for each of the two invocations"
    );
}

/// Go `TestTraceManager_LatencyColumnDenied`.
#[tokio::test]
async fn test_trace_manager_latency_column_denied() {
    let tp = new_test_plugin_with_config(|c| {
        c.payload_column_denylist = vec![COLUMN_LATENCY_MS.to_string()];
    })
    .await;
    let run = MockRun::new(&tp.ctx);
    let cb = Cb(&tp.plugin);
    cb.before_agent(run.ctx()).await;
    cb.after_agent(run.ctx()).await;
    cb.after_run(run.inv()).await;

    let rows = tp
        .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;
    for row in &rows {
        assert!(
            json_column(row, COLUMN_LATENCY_MS).is_none(),
            "{} row has latency_ms, want none",
            str_col(row, "event_type")
        );
    }
    let got = place_of(&only_row_of_type(&rows, EVENT_AGENT_COMPLETED));
    let want = place_of(&only_row_of_type(&rows, EVENT_AGENT_STARTING));
    assert!(
        got == want && is_span_id(&got.span),
        "AGENT_COMPLETED is at {got:?}, want the agent span {want:?}"
    );
}

/// Go `TestTraceManager_ParallelBranches`: the sub-agents of a parallel agent run concurrently
/// on branches named after it and end in any order.
#[test]
fn test_trace_manager_parallel_branches() {
    let ctx = CallbackCtx::background();
    let tm = TraceManager::new();
    let id = |a: &str, b: &str| id_of("inv", a, b);
    let invocation = tm.invocation_span(&ctx, &id("par", ""), None);
    let par = tm.push_agent_span(&ctx, &id("par", "")).0;
    let a = tm.push_agent_span(&ctx, &id("a", "par.a")).0;
    // ab shares the string prefix "par.a" with branch "par.a", but is a sibling branch under
    // par rather than a sub-branch of a.
    let ab = tm.push_agent_span(&ctx, &id("ab", "par.ab")).0;
    let b = tm.push_agent_span(&ctx, &id("b", "par.b")).0;
    // a1 is a sub-agent of a, such as a step of a sequential agent, which keeps a's branch.
    let a1 = tm.push_agent_span(&ctx, &id("a1", "par.a")).0;
    let b_model = tm.push_llm_span(&ctx, &id("b", "par.b"));
    let a1_tool = tm.push_tool_span(&ctx, &id("a1", "par.a"), "call-1");
    let a1_tool_end = tm
        .pop_tool_span(&ctx, &id("a1", "par.a"), "call-1", false)
        .0;
    let a1_end = tm.pop_agent_span(&ctx, &id("a1", "par.a"));
    // a ends while b, which started after it, is still running.
    let a_end = tm.pop_agent_span(&ctx, &id("a", "par.a"));
    let ab_end = tm.pop_agent_span(&ctx, &id("ab", "par.ab"));
    let b_model_end = tm.pop_llm_span(&ctx, &id("b", "par.b"), true);
    let b_tool = tm.push_tool_span(&ctx, &id("b", "par.b"), "call-2");
    let b_end = tm.pop_agent_span(&ctx, &id("b", "par.b"));
    let par_end = tm.pop_agent_span(&ctx, &id("par", ""));
    let invocation_end = tm.end_invocation(&ctx, &id("par", ""));

    check_parents(&[
        ("par", &par, &invocation),
        ("a", &a, &par),
        ("ab", &ab, &par),
        ("b", &b, &par),
        ("a1", &a1, &a),
        ("model call of b", &b_model, &b),
        ("tool call of a1", &a1_tool, &a1),
        ("tool call of b", &b_tool, &b),
    ]);
    check_ends(
        &[
            ("tool call of a1", &a1_tool, &a1_tool_end),
            ("a1", &a1, &a1_end),
            ("a", &a, &a_end),
            ("ab", &ab, &ab_end),
            ("model call of b", &b_model, &b_model_end),
            ("b", &b, &b_end),
            ("par", &par, &par_end),
            ("invocation", &invocation, &invocation_end),
        ],
        Some(&invocation.trace_id),
    );
}

/// Go `TestTraceManager_AgentNesting`.
#[test]
fn test_trace_manager_agent_nesting() {
    const INNER_BRANCH: &str = "fan.inner";
    const X_BRANCH: &str = "fan.inner.inner.x";
    const W1_BRANCH: &str = "fan.inner.inner.w1";
    const W2_BRANCH: &str = "fan.inner.inner.w2";
    let ctx = CallbackCtx::background();
    let tm = TraceManager::new();
    let id = |a: &str, b: &str| id_of("inv", a, b);
    let lookup_key = tool_key(&id("worker", W1_BRANCH), "", "lookup");
    let invocation = tm.invocation_span(&ctx, &id("coordinator", ""), None);
    let coordinator = tm.push_agent_span(&ctx, &id("coordinator", "")).0;
    let fan = tm.push_agent_span(&ctx, &id("fan", "")).0;
    let inner = tm.push_agent_span(&ctx, &id("inner", INNER_BRANCH)).0;
    let x = tm.push_agent_span(&ctx, &id("x", X_BRANCH)).0;
    let w1 = tm.push_agent_span(&ctx, &id("worker", W1_BRANCH)).0;
    let w2 = tm.push_agent_span(&ctx, &id("worker", W2_BRANCH)).0;
    let w1_model = tm.push_llm_span(&ctx, &id("worker", W1_BRANCH));
    let w2_model = tm.push_llm_span(&ctx, &id("worker", W2_BRANCH));
    let w1_tool = tm.push_tool_span(&ctx, &id("worker", W1_BRANCH), &lookup_key);

    let events: [(&str, &str, &str, &RowSpan); 5] = [
        ("event of worker on w1", "worker", W1_BRANCH, &w1),
        ("event of worker on w2", "worker", W2_BRANCH, &w2),
        // An author that does not run on the event's branch is found by name: its most
        // recent run.
        (
            "event of worker on another branch",
            "worker",
            "fan.elsewhere",
            &w2,
        ),
        ("event of x", "x", X_BRANCH, &x),
        ("event of the user", "user", "", &invocation),
    ];
    for (name, author, branch, want) in events {
        // Event callbacks get the agent that the runner runs.
        let got = tm.event_span(&ctx, &id("coordinator", ""), author, branch);
        assert!(
            got.span_id == want.span_id && got.parent_span_id == want.parent_span_id,
            "{name}: span {:?} under {:?}, want {:?} under {:?}",
            got.span_id,
            got.parent_span_id,
            want.span_id,
            want.parent_span_id
        );
        assert!(got.latency.is_none(), "{name}: latency {:?}", got.latency);
    }

    let w1_model_end = tm.pop_llm_span(&ctx, &id("worker", W1_BRANCH), true);
    let w1_tool_end = tm
        .pop_tool_span(&ctx, &id("worker", W1_BRANCH), &lookup_key, false)
        .0;
    let w1_end = tm.pop_agent_span(&ctx, &id("worker", W1_BRANCH));
    let w2_model_end = tm.pop_llm_span(&ctx, &id("worker", W2_BRANCH), true);
    let x_end = tm.pop_agent_span(&ctx, &id("x", X_BRANCH));
    let w2_end = tm.pop_agent_span(&ctx, &id("worker", W2_BRANCH));
    let inner_end = tm.pop_agent_span(&ctx, &id("inner", INNER_BRANCH));
    let fan_end = tm.pop_agent_span(&ctx, &id("fan", ""));
    let summary = tm.push_agent_span(&ctx, &id("summary", "")).0;
    // A model call of the coordinator while summary runs, on the same branch, belongs to the
    // coordinator.
    let coordinator_model = tm.push_llm_span(&ctx, &id("coordinator", ""));
    let coordinator_model_end = tm.pop_llm_span(&ctx, &id("coordinator", ""), true);
    let summary_end = tm.pop_agent_span(&ctx, &id("summary", ""));
    let coordinator_end = tm.pop_agent_span(&ctx, &id("coordinator", ""));
    assert_eq!(
        open_spans(&tm, "inv"),
        SpanCounts::default(),
        "spans still open after all of them ended"
    );
    let invocation_end = tm.end_invocation(&ctx, &id("coordinator", ""));

    check_parents(&[
        ("coordinator", &coordinator, &invocation),
        ("fan", &fan, &coordinator),
        ("inner", &inner, &fan),
        ("x", &x, &inner),
        // worker on w1 starts after x, on a sibling branch of x.
        ("worker on w1", &w1, &inner),
        ("worker on w2", &w2, &inner),
        ("model call of worker on w1", &w1_model, &w1),
        ("model call of worker on w2", &w2_model, &w2),
        ("tool call of worker on w1", &w1_tool, &w1),
        ("summary", &summary, &coordinator),
        (
            "model call of the coordinator",
            &coordinator_model,
            &coordinator,
        ),
    ]);
    check_ends(
        &[
            ("model call of worker on w1", &w1_model, &w1_model_end),
            ("tool call of worker on w1", &w1_tool, &w1_tool_end),
            ("worker on w1", &w1, &w1_end),
            ("model call of worker on w2", &w2_model, &w2_model_end),
            ("x", &x, &x_end),
            ("worker on w2", &w2, &w2_end),
            ("inner", &inner, &inner_end),
            ("fan", &fan, &fan_end),
            (
                "model call of the coordinator",
                &coordinator_model,
                &coordinator_model_end,
            ),
            ("summary", &summary, &summary_end),
            ("coordinator", &coordinator, &coordinator_end),
            ("invocation", &invocation, &invocation_end),
        ],
        None,
    );
}

/// Go `TestTraceManager_FailedToolCallBookkeeping`.
#[test]
fn test_trace_manager_failed_tool_call_bookkeeping() {
    let ctx = CallbackCtx::background();
    let tm = TraceManager::new();
    let id = id_of("inv", "agent", "");
    tm.invocation_span(&ctx, &id, None);
    let agent_span = tm.push_agent_span(&ctx, &id).0;
    // fail runs OnToolErrorCallback and then AfterToolCallback for the call key, and returns
    // the place of the TOOL_ERROR row.
    let fail = |key: &str| -> RowSpan {
        let (error_end, _, closed_by_error) = tm.pop_tool_span(&ctx, &id, key, true);
        assert!(
            !closed_by_error,
            "{key}: OnToolErrorCallback found the call ended by an earlier error"
        );
        let (after_end, _, closed_by_error) = tm.pop_tool_span(&ctx, &id, key, false);
        assert!(
            closed_by_error,
            "{key}: AfterToolCallback did not find the call ended by OnToolErrorCallback"
        );
        assert!(
            after_end.span_id == error_end.span_id
                && after_end.parent_span_id == error_end.parent_span_id,
            "{key}: AfterToolCallback at span {:?} under {:?}, want the span of the error {:?} under {:?}",
            after_end.span_id,
            after_end.parent_span_id,
            error_end.span_id,
            error_end.parent_span_id
        );
        assert!(
            after_end.latency.is_none(),
            "{key}: AfterToolCallback has latency {:?}",
            after_end.latency
        );
        error_end
    };

    // The span of a call that fails ends once, with the error.
    let start = tm.push_tool_span(&ctx, &id, "call-1");
    let got = fail("call-1");
    assert!(
        got.span_id == start.span_id
            && got.parent_span_id == agent_span.span_id
            && got.latency.is_some(),
        "error of a started call at {got:?}, want the span of the call {:?} under {:?} with latency",
        start.span_id,
        agent_span.span_id
    );
    // A call whose start was not seen gets a span of its own.
    let got = fail("call-2");
    assert!(
        got.span_id != agent_span.span_id
            && got.parent_span_id == agent_span.span_id
            && got.latency.is_none(),
        "error of a call that never started at {got:?}, want a span of its own under {:?} without latency",
        agent_span.span_id
    );
    // Without function call IDs, the calls of a tool by an agent share a key. Consecutive
    // OnToolErrorCallback-only calls each allocate a fresh span ID, and the next started call
    // drops the stale record.
    let key = tool_key(&id, "", "lookup");
    let (first_err, _, first_closed) = tm.pop_tool_span(&ctx, &id, &key, true);
    let (second_err, _, second_closed) = tm.pop_tool_span(&ctx, &id, &key, true);
    assert!(
        !first_closed && !second_closed && first_err.span_id != second_err.span_id,
        "consecutive OnToolErrorCallback-only spans = ({:?}/{first_closed}, {:?}/{second_closed}), want distinct spans with closed_by_error=false",
        first_err.span_id,
        second_err.span_id
    );
    let next = tm.push_tool_span(&ctx, &id, &key);
    let (got, _, closed_by_error) = tm.pop_tool_span(&ctx, &id, &key, false);
    assert!(
        !closed_by_error && got.span_id == next.span_id && got.latency.is_some(),
        "end of the next call at {got:?} (closed by error: {closed_by_error}), want the span of the call {:?} with latency",
        next.span_id
    );
    assert_eq!(
        open_spans(&tm, "inv"),
        SpanCounts {
            agents: 1,
            ..SpanCounts::default()
        },
        "open spans"
    );
}

/// Go `TestTraceManager_RecoveredToolErrorEmitsCompletedAndAgentResponse`.
#[tokio::test]
async fn test_trace_manager_recovered_tool_error_emits_completed_and_agent_response() {
    let tp = new_test_plugin_with_config(|c| {
        c.final_response_tool_names = vec!["final_answer".into()];
    })
    .await;
    let run = MockRun::new(&tp.ctx);
    let cb = Cb(&tp.plugin);
    let final_tool = fake_tool("final_answer");
    let a = args(json!({"answer": "draft"}));
    let recovered = args(json!({"answer": "fallback-42"}));
    cb.before_agent(run.ctx()).await;
    cb.before_tool(run.tool("call-recovered"), &final_tool, a.clone())
        .await;
    cb.on_tool_error(
        run.tool("call-recovered"),
        &final_tool,
        a.clone(),
        "transient error",
    )
    .await;
    cb.after_tool(run.tool("call-recovered"), &final_tool, a, recovered, None)
        .await;
    cb.after_agent(run.ctx()).await;
    cb.after_run(run.inv()).await;

    let rows = tp
        .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;
    assert_eq!(
        event_types(&rows),
        [
            EVENT_AGENT_STARTING,
            EVENT_TOOL_STARTING,
            EVENT_TOOL_ERROR,
            EVENT_TOOL_COMPLETED,
            EVENT_AGENT_RESPONSE,
            EVENT_AGENT_COMPLETED,
            EVENT_INVOCATION_COMPLETED,
        ]
    );
    let agent_start = place_of(&rows[0]);
    let tool_start = place_of(&rows[1]);
    assert_eq!(tool_start.parent, agent_start.span, "TOOL_STARTING parent");
    for ev in [EVENT_TOOL_ERROR, EVENT_TOOL_COMPLETED] {
        assert_eq!(
            place_of(&only_row_of_type(&rows, ev)),
            tool_start,
            "{ev} place"
        );
    }
    assert_eq!(
        place_of(&only_row_of_type(&rows, EVENT_AGENT_RESPONSE)),
        agent_start,
        "AGENT_RESPONSE place"
    );
    must_latency(&only_row_of_type(&rows, EVENT_TOOL_ERROR));
    for ev in [EVENT_TOOL_COMPLETED, EVENT_AGENT_RESPONSE] {
        assert!(
            row_latency(&only_row_of_type(&rows, ev)).is_none(),
            "{ev} after OnToolErrorCallback has latency_ms"
        );
    }
    assert_json(
        "TOOL_COMPLETED content",
        &json_object_column(&only_row_of_type(&rows, EVENT_TOOL_COMPLETED), "content"),
        r#"{"tool":"final_answer","result":{"answer":"fallback-42"},"tool_origin":"UNKNOWN"}"#,
    );
    assert_eq!(
        place_of(&only_row_of_type(&rows, EVENT_AGENT_COMPLETED)),
        agent_start,
        "AGENT_COMPLETED place"
    );
}

/// Go `TestTraceManager_FinalResponseToolRow`.
#[tokio::test]
async fn test_trace_manager_final_response_tool_row() {
    let tp = new_test_plugin_with_config(|c| {
        c.final_response_tool_names = vec!["final_answer".into()];
    })
    .await;
    let run = MockRun::new(&tp.ctx);
    let cb = Cb(&tp.plugin);
    let final_tool = fake_tool("final_answer");
    let answer = args(json!({"answer": "42"}));
    // Go sets mCtx.functionCallID for every callback; the agent callbacks ignore it.
    cb.before_agent(run.tool("call-7")).await;
    cb.before_tool(run.tool("call-7"), &final_tool, answer.clone())
        .await;
    cb.after_tool(run.tool("call-7"), &final_tool, answer, Map::new(), None)
        .await;
    cb.after_agent(run.tool("call-7")).await;
    cb.after_run(run.tool("call-7").as_invocation().clone())
        .await;

    let rows = tp
        .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;
    let agent_start = place_of(&only_row_of_type(&rows, EVENT_AGENT_STARTING));
    let tool_start = place_of(&only_row_of_type(&rows, EVENT_TOOL_STARTING));
    let completed = only_row_of_type(&rows, EVENT_TOOL_COMPLETED);
    let resp = only_row_of_type(&rows, EVENT_AGENT_RESPONSE);
    assert_eq!(place_of(&completed), tool_start, "TOOL_COMPLETED place");
    assert_eq!(place_of(&resp), agent_start, "AGENT_RESPONSE place");
    must_latency(&completed);
    assert!(
        row_latency(&resp).is_none(),
        "AGENT_RESPONSE has latency_ms, want none"
    );
}

/// Go `TestTraceManager_RootAgentName`.
#[test]
fn test_trace_manager_root_agent_name() {
    let ctx = CallbackCtx::background();
    let tm = TraceManager::new();
    // The runner's callbacks get the agent that the runner runs.
    tm.invocation_span(&ctx, &id_of("inv-1", "root", ""), None);
    let sub1 = tm.push_agent_span(&ctx, &id_of("inv-1", "sub", "")).0;
    assert_eq!(
        sub1.root_agent_name, "root",
        "root_agent_name after the runner callbacks"
    );
    // Without them, the first agent to start is the root.
    let sub2 = tm.push_agent_span(&ctx, &id_of("inv-2", "sub", "")).0;
    assert_eq!(
        sub2.root_agent_name, "sub",
        "root_agent_name of the first agent"
    );
    assert_eq!(
        tm.event_span(&ctx, &id_of("inv-2", "root", ""), "sub", "")
            .root_agent_name,
        "sub",
        "root_agent_name after another agent"
    );
    // When the first callback carries a dotted branch, the top-level branch prefix names the
    // root agent.
    let sub3 = tm
        .push_agent_span(&ctx, &id_of("inv-3", "sub", "root.sub"))
        .0;
    assert_eq!(
        sub3.root_agent_name, "root",
        "root_agent_name from dotted branch"
    );
}

/// Sets `clock` to `start + ms` milliseconds (Go `now = start.Add(...)`).
fn set_clock(clock: &ManualClock, start: chrono::DateTime<chrono::Utc>, ms: i64) {
    let delta = (start + chrono::Duration::milliseconds(ms)) - clock.now();
    clock.advance_ms(delta.num_milliseconds());
}

/// Go `TestTraceManager_EvictsLeastRecentlyActiveInvocations`.
#[test]
fn test_trace_manager_evicts_least_recently_active_invocations() {
    let ctx = CallbackCtx::background();
    let clock = ManualClock::new();
    let mut tm = TraceManager::new();
    tm.max_tracked = 3;
    tm.now = clock.clock();
    let touch = |tm: &TraceManager, inv: &str| {
        clock.advance_ms(1000);
        tm.invocation_span(&ctx, &id_of(inv, "", ""), None);
    };
    touch(&tm, "inv-1");
    touch(&tm, "inv-2");
    touch(&tm, "inv-3");
    touch(&tm, "inv-1"); // inv-2 is now the least recently active.
    touch(&tm, "inv-4");
    assert_eq!(tracked_invocations(&tm), ["inv-1", "inv-3", "inv-4"]);
    // The invocation being started is never evicted.
    tm.max_tracked = 1;
    touch(&tm, "inv-5");
    assert_eq!(tracked_invocations(&tm), ["inv-5"]);
    // Not even when the clock went back, which makes it the least recently active invocation.
    clock.advance_ms(-3_600_000);
    touch(&tm, "inv-6");
    assert_eq!(
        tracked_invocations(&tm),
        ["inv-6"],
        "tracked invocations after the clock went back"
    );
}

/// Go `TestTraceManager_ExpiresIdleInvocations`.
#[test]
fn test_trace_manager_expires_idle_invocations() {
    let ctx = CallbackCtx::background();
    let clock = ManualClock::new();
    let start = clock.now();
    let mut tm = TraceManager::new();
    tm.now = clock.clock();
    let start1 = |inv: &str| {
        tm.invocation_span(&ctx, &id_of(inv, "", ""), None);
    };
    start1("idle");
    start1("active");
    set_clock(&clock, start, 90 * 60 * 1000);
    start1("active"); // A callback keeps the invocation active.
    set_clock(&clock, start, 2 * 3600 * 1000);
    // Expired invocations are dropped every SWEEP_INTERVAL insertions.
    for i in 3..SWEEP_INTERVAL {
        start1(&format!("filler-{i}"));
    }
    assert!(
        tracked_invocations(&tm).contains(&"idle".to_string()),
        "the idle invocation was dropped before a sweep"
    );
    start1("last-filler");
    let tracked = tracked_invocations(&tm);
    assert!(
        !tracked.contains(&"idle".to_string()),
        "the idle invocation is still tracked after a sweep"
    );
    assert!(
        tracked.contains(&"active".to_string()),
        "the active invocation was dropped"
    );
    assert_eq!(tracked.len(), SWEEP_INTERVAL - 1, "tracked invocations");
}

/// Go `TestTraceManager_UntrackedInvocations`.
#[test]
fn test_trace_manager_untracked_invocations() {
    let ctx = CallbackCtx::background();
    let tm = TraceManager::new();
    // Callbacks without an invocation ID still get IDs.
    let no_id = id_of("", "solo", "");
    let inv_span = tm.invocation_span(&ctx, &no_id, None);
    let solo_span = tm.push_agent_span(&ctx, &no_id).0;
    let mut spans = vec![inv_span, solo_span, tm.end_invocation(&ctx, &no_id)];
    // So does the end of an invocation that is not tracked, for example because it was
    // evicted.
    spans.push(tm.end_invocation(&ctx, &id_of("never-started", "", "")));
    for (i, s) in spans.iter().enumerate() {
        assert!(
            is_trace_id(&s.trace_id) && is_span_id(&s.span_id),
            "span {i}: trace {:?}, span {:?}, want 32 and 16 hex digits",
            s.trace_id,
            s.span_id
        );
    }
    assert!(
        spans[2].latency.is_none(),
        "end of an invocation without ID has latency {:?}",
        spans[2].latency
    );
    assert!(
        spans[3].latency.is_none(),
        "end of an untracked invocation has latency {:?}",
        spans[3].latency
    );
    // Starting and ending a tracked invocation removes it from the tracked invocations.
    let tracked_id = id_of("tracked", "solo", "");
    let started = tm.invocation_span(&ctx, &tracked_id, None);
    assert_eq!(
        tracked_invocations(&tm),
        ["tracked"],
        "tracked invocations while running"
    );
    let ended = tm.end_invocation(&ctx, &tracked_id);
    assert!(
        ended.trace_id == started.trace_id
            && ended.span_id == started.span_id
            && ended.latency.is_some(),
        "end_invocation(tracked) = {ended:?}, want trace {:?} span {:?} with latency",
        started.trace_id,
        started.span_id
    );
    assert!(
        tracked_invocations(&tm).is_empty(),
        "tracked invocations = {:?}, want none",
        tracked_invocations(&tm)
    );
}

/// An LLM agent of `cfg` (Go `llmagent.New`).
fn llm_agent_of(cfg: LlmAgentConfig) -> Arc<dyn Agent> {
    llm_agent::new(cfg).unwrap()
}

/// Go `&scriptedLLM{}`: a model named `scripted-model`.
fn scripted_model() -> Arc<dyn gadk_model::Llm> {
    MockModel::new(vec![]).name_as("scripted-model")
}

/// Go `TestAgentStartingData`.
// parity: adapted: Go's "wrapper with zero outer Config" case reads an embedded agent's config by reflection, which gadk does not do (ledgered divergence in plugin/rows.rs agent_starting_data), and Go's "no agent" case passes a nil agent, which agent_starting_data's &dyn Agent cannot be; both cases are left out
#[test]
fn test_agent_starting_data() {
    let helper = llm_agent_of(LlmAgentConfig {
        name: "helper".into(),
        description: "Helps".into(),
        ..Default::default()
    });
    let planner = llm_agent_of(LlmAgentConfig {
        name: "planner".into(),
        description: "Plans trips".into(),
        model: Some(scripted_model()),
        instruction: "Plan the trip.".into(),
        sub_agents: vec![helper.clone()],
        ..Default::default()
    });
    let provided = llm_agent_of(LlmAgentConfig {
        name: "provided".into(),
        model: Some(scripted_model()),
        instruction: "Overridden by the provider.".into(),
        instruction_provider: Some(Arc::new(|_| {
            Box::pin(async { Ok("Built at run time.".to_string()) })
        })),
        ..Default::default()
    });
    let custom = gadk_agent::new_agent(gadk_agent::AgentConfig {
        name: "custom".into(),
        description: "Runs code".into(),
        ..Default::default()
    })
    .unwrap();

    let tests: Vec<(&str, Arc<dyn Agent>, Option<Value>, Value)> = vec![
        (
            "LLM agent",
            planner,
            Some(json!("Plan the trip.")),
            json!({"agent_description": "Plans trips", "model": "scripted-model", "sub_agents": ["helper"]}),
        ),
        (
            "instruction provider",
            provided,
            None,
            json!({"model": "scripted-model", "instruction_source": "provider"}),
        ),
        (
            "LLM agent that inherits its model",
            helper,
            None,
            json!({"agent_description": "Helps"}),
        ),
        (
            "custom agent",
            custom,
            None,
            json!({"agent_description": "Runs code"}),
        ),
    ];
    for (name, a, want_content, want_attrs) in tests {
        let (content, attrs) = agent_starting_data(&*a);
        assert_eq!(content, want_content, "{name}: content");
        assert_eq!(Value::Object(attrs), want_attrs, "{name}: attributes");
    }
}

/// Go `TestTraceManager_AgentNamed`.
#[test]
fn test_trace_manager_agent_named() {
    let helper = llm_agent_of(LlmAgentConfig {
        name: "helper".into(),
        ..Default::default()
    });
    let planner = llm_agent_of(LlmAgentConfig {
        name: "planner".into(),
        sub_agents: vec![helper.clone()],
        ..Default::default()
    });
    let other = llm_agent_of(LlmAgentConfig {
        name: "other".into(),
        ..Default::default()
    });
    let ctx = CallbackCtx::background();
    let tm = TraceManager::new();
    let id = |a: &str| id_of("inv", a, "");
    if let Some(got) = tm.agent_named(&ctx, &id("planner")) {
        panic!(
            "agent_named(planner) before the runner's agent is known = {:?}, want None",
            got.name()
        );
    }
    tm.invocation_span(&ctx, &id("planner"), Some(&planner));
    tm.invocation_span(&ctx, &id("other"), Some(&other));
    let tests: [(&str, Option<&Arc<dyn Agent>>); 4] = [
        ("planner", Some(&planner)),
        ("helper", Some(&helper)),
        // The first agent recorded stays the runner's agent.
        ("other", None),
        ("", None),
    ];
    for (name, want) in tests {
        let got = tm.agent_named(&ctx, &id(name));
        match (got, want) {
            (None, None) => {}
            (Some(g), Some(w)) if Arc::ptr_eq(&g, w) => {}
            (g, w) => panic!(
                "agent_named({name:?}) = {:?}, want {:?}",
                g.map(|a| a.name().to_string()),
                w.map(|a| a.name().to_string())
            ),
        }
    }
}

/// Go `TestAgentStarting_LooksUpAgentInRunnerTree`: the agent callbacks find the agents by
/// name in the runner's tree, without asking their context.
// parity: adapted: Go's agentContext fails the test when an agent callback asks its context for the agent; a gadk context always has an agent, so the agent callbacks get a custom agent of the same name, whose AGENT_STARTING row would lack the runner tree's description and instruction if the plugin read it
#[tokio::test]
async fn test_agent_starting_looks_up_agent_in_runner_tree() {
    let tp = setup_test_plugin().await;
    let helper = llm_agent_of(LlmAgentConfig {
        name: "helper".into(),
        description: "Helps".into(),
        ..Default::default()
    });
    let planner = llm_agent_of(LlmAgentConfig {
        name: "planner".into(),
        description: "Plans trips".into(),
        instruction: "Plan the trip.".into(),
        sub_agents: vec![helper],
        ..Default::default()
    });
    let context_of = |name: &str, a: Option<Arc<dyn Agent>>| {
        MockRun::new(&MockCtx {
            agent_name: name.into(),
            agent: a,
            ..tp.ctx.clone()
        })
    };
    let cb = Cb(&tp.plugin);
    let runner_ctx = context_of("planner", Some(planner));
    cb.before_run(runner_ctx.inv()).await;
    for name in ["planner", "helper", "stranger"] {
        let c = context_of(name, None);
        cb.before_agent(c.ctx()).await;
        cb.after_agent(c.ctx()).await;
    }
    cb.after_run(runner_ctx.inv()).await;

    let rows = tp
        .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;
    let starts = rows_of_type(&rows, EVENT_AGENT_STARTING);
    assert_eq!(
        starts.len(),
        3,
        "AGENT_STARTING rows; event types: {:?}",
        event_types(&rows)
    );
    let tests: [(&str, &str, Vec<(&str, &str)>); 3] = [
        (
            "planner",
            r#""Plan the trip.""#,
            vec![
                ("agent_description", r#""Plans trips""#),
                ("sub_agents", r#"["helper"]"#),
            ],
        ),
        ("helper", "", vec![("agent_description", r#""Helps""#)]),
        ("stranger", "", vec![]),
    ];
    for (i, (agent_name, want_content, want_attrs)) in tests.iter().enumerate() {
        let row = &starts[i];
        assert_eq!(
            str_col(row, "agent"),
            *agent_name,
            "AGENT_STARTING row {i}: agent"
        );
        let content = json_column(row, "content");
        assert_eq!(
            content.is_some(),
            !want_content.is_empty(),
            "{agent_name} AGENT_STARTING: has content ({content:?})"
        );
        if let Some(content) = &content {
            assert_json(
                &format!("{agent_name} AGENT_STARTING content"),
                content,
                want_content,
            );
        }
        let attrs = json_object_column(row, "attributes");
        let want_attrs: HashMap<&str, &str> = want_attrs.iter().copied().collect();
        for key in [
            "agent_description",
            "sub_agents",
            "model",
            "instruction_source",
        ] {
            let want = want_attrs.get(key);
            assert_eq!(
                attrs.get(key).is_some(),
                want.is_some(),
                "{agent_name} AGENT_STARTING: attributes.{key} set"
            );
            if let Some(want) = want {
                check_json_at(&attrs, want, &[key]);
            }
        }
    }
}

/// Go `TestTraceManager_SameFunctionCallIDAcrossConcurrentSubAgents`: two concurrent
/// sub-agents whose model responses assign the same function call ID do not collide.
#[tokio::test]
async fn test_trace_manager_same_function_call_id_across_concurrent_sub_agents() {
    let tp = setup_test_plugin().await;
    let ctx_of = |name: &str, branch: &str| {
        MockRun::new(&MockCtx {
            agent_name: name.into(),
            branch: branch.into(),
            ..tp.ctx.clone()
        })
    };
    let par = ctx_of("par", "");
    let a = ctx_of("a", "par.a");
    let b = ctx_of("b", "par.b");
    let lookup_tool = fake_tool("lookup");
    let cb = Cb(&tp.plugin);

    cb.before_run(par.inv()).await;
    cb.before_agent(par.ctx()).await;
    cb.before_agent(a.ctx()).await;
    cb.before_agent(b.ctx()).await;
    cb.before_tool(a.tool("call_0"), &lookup_tool, args(json!({"q": "a"})))
        .await;
    cb.before_tool(b.tool("call_0"), &lookup_tool, args(json!({"q": "b"})))
        .await;
    cb.after_tool(
        a.tool("call_0"),
        &lookup_tool,
        args(json!({"q": "a"})),
        args(json!({"r": "a"})),
        None,
    )
    .await;
    cb.after_tool(
        b.tool("call_0"),
        &lookup_tool,
        args(json!({"q": "b"})),
        args(json!({"r": "b"})),
        None,
    )
    .await;
    cb.after_agent(a.ctx()).await;
    cb.after_agent(b.ctx()).await;
    cb.after_agent(par.ctx()).await;
    cb.after_run(par.inv()).await;

    let rows = tp
        .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;
    let mut agent_span_by_agent: HashMap<String, String> = HashMap::new();
    for row in rows_of_type(&rows, EVENT_AGENT_STARTING) {
        agent_span_by_agent.insert(str_col(&row, "agent"), place_of(&row).span);
    }
    let mut tool_starts: HashMap<String, TracePlace> = HashMap::new();
    for row in rows_of_type(&rows, EVENT_TOOL_STARTING) {
        let agent = str_col(&row, "agent");
        let place = place_of(&row);
        assert_eq!(
            place.parent, agent_span_by_agent[&agent],
            "TOOL_STARTING of agent {agent:?}: parent_span_id"
        );
        tool_starts.insert(agent, place);
    }
    assert_ne!(
        tool_starts["a"].span, tool_starts["b"].span,
        "concurrent sub-agents with the same function call ID share a tool span"
    );
    for row in rows_of_type(&rows, EVENT_TOOL_COMPLETED) {
        let agent = str_col(&row, "agent");
        assert_eq!(
            place_of(&row),
            tool_starts[&agent],
            "TOOL_COMPLETED of agent {agent:?}"
        );
        must_latency(&row);
    }
}

/// Go `TestTraceManager_SameAgentConcurrentNodePaths`: two concurrent activations of one agent
/// on one branch, told apart by their node paths.
#[tokio::test]
async fn test_trace_manager_same_agent_concurrent_node_paths() {
    let tp = setup_test_plugin().await;
    let ctx_of = |name: &str, path: &str| {
        MockRun::new(&MockCtx {
            agent_name: name.into(),
            path: path.into(),
            ..tp.ctx.clone()
        })
    };
    let root = ctx_of("wf", "wf@0");
    let w1 = ctx_of("worker", "wf@0/worker@1");
    let w2 = ctx_of("worker", "wf@0/worker@2");
    let lookup_tool = fake_tool("lookup");
    let req = || LlmRequest {
        model: "gemini-2.5-pro".into(),
        contents: vec![text("user", "hi")],
        ..LlmRequest::default()
    };
    let res = || response(text("model", "ok"));
    let event = |path: &str, body: &str, id: &str| Event {
        id: id.into(),
        timestamp: chrono::Utc::now(),
        invocation_id: tp.ctx.invocation_id.clone(),
        author: "worker".into(),
        node_info: Some(NodeInfo {
            path: path.into(),
            ..NodeInfo::default()
        }),
        response: response(text("model", body)),
        ..Event::default()
    };
    let ev1 = event("wf@0/worker@1", "event-1", "ev-1");
    let ev2 = event("wf@0/worker@2", "event-2", "ev-2");
    let cb = Cb(&tp.plugin);

    cb.before_run(root.inv()).await;
    cb.before_agent(root.ctx()).await;
    cb.before_agent(w1.ctx()).await;
    cb.before_agent(w2.ctx()).await;
    cb.before_model(w1.ctx(), req()).await;
    cb.before_model(w2.ctx(), req()).await;
    cb.after_model(w1.ctx(), res()).await;
    cb.after_model(w2.ctx(), res()).await;
    cb.before_tool(w1.tool("call_0"), &lookup_tool, args(json!({"id": 1})))
        .await;
    cb.before_tool(w2.tool("call_0"), &lookup_tool, args(json!({"id": 2})))
        .await;
    cb.after_tool(
        w1.tool("call_0"),
        &lookup_tool,
        args(json!({"id": 1})),
        args(json!({"ok": true})),
        None,
    )
    .await;
    cb.after_tool(
        w2.tool("call_0"),
        &lookup_tool,
        args(json!({"id": 2})),
        args(json!({"ok": true})),
        None,
    )
    .await;
    cb.on_event(root.inv(), ev1).await;
    cb.on_event(root.inv(), ev2).await;
    // Activation 1 finishes before activation 2 (FIFO order on the same branch).
    cb.after_agent(w1.ctx()).await;
    cb.after_agent(w2.ctx()).await;
    cb.after_agent(root.ctx()).await;
    cb.after_run(root.inv()).await;

    let rows = tp
        .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;
    let agent_starts = rows_of_type(&rows, EVENT_AGENT_STARTING);
    assert_eq!(agent_starts.len(), 3, "AGENT_STARTING rows");
    let wf_span = place_of(&agent_starts[0]);
    let w1_span = place_of(&agent_starts[1]);
    let w2_span = place_of(&agent_starts[2]);
    assert!(
        w1_span.parent == wf_span.span
            && w2_span.parent == wf_span.span
            && w1_span.span != w2_span.span,
        "worker spans = ({w1_span:?}, {w2_span:?}), want two distinct spans parented to wf ({:?})",
        wf_span.span
    );
    let llm_reqs = rows_of_type(&rows, EVENT_LLM_REQUEST);
    let llm_resps = rows_of_type(&rows, EVENT_LLM_RESPONSE);
    assert_eq!(
        place_of(&llm_reqs[0]).parent,
        w1_span.span,
        "LLM_REQUEST 1 parent"
    );
    assert_eq!(
        place_of(&llm_reqs[1]).parent,
        w2_span.span,
        "LLM_REQUEST 2 parent"
    );
    assert_eq!(
        place_of(&llm_resps[0]),
        place_of(&llm_reqs[0]),
        "LLM_RESPONSE 1"
    );
    assert_eq!(
        place_of(&llm_resps[1]),
        place_of(&llm_reqs[1]),
        "LLM_RESPONSE 2"
    );
    let tool_starts = rows_of_type(&rows, EVENT_TOOL_STARTING);
    let tool_ends = rows_of_type(&rows, EVENT_TOOL_COMPLETED);
    assert!(
        place_of(&tool_starts[0]).parent == w1_span.span
            && place_of(&tool_starts[1]).parent == w2_span.span
            && place_of(&tool_starts[0]).span != place_of(&tool_starts[1]).span,
        "TOOL_STARTING places = ({:?}, {:?}), want distinct spans under ({:?}, {:?})",
        place_of(&tool_starts[0]),
        place_of(&tool_starts[1]),
        w1_span.span,
        w2_span.span
    );
    assert_eq!(
        place_of(&tool_ends[0]),
        place_of(&tool_starts[0]),
        "TOOL_COMPLETED 1"
    );
    assert_eq!(
        place_of(&tool_ends[1]),
        place_of(&tool_starts[1]),
        "TOOL_COMPLETED 2"
    );
    let resp_rows = rows_of_type(&rows, EVENT_AGENT_RESPONSE);
    assert_eq!(resp_rows.len(), 2, "AGENT_RESPONSE rows");
    assert_eq!(place_of(&resp_rows[0]), w1_span, "AGENT_RESPONSE 1 place");
    assert_eq!(place_of(&resp_rows[1]), w2_span, "AGENT_RESPONSE 2 place");
    let agent_ends = rows_of_type(&rows, EVENT_AGENT_COMPLETED);
    assert_eq!(place_of(&agent_ends[0]), w1_span, "AGENT_COMPLETED 1");
    assert_eq!(place_of(&agent_ends[1]), w2_span, "AGENT_COMPLETED 2");
    assert_eq!(place_of(&agent_ends[2]), wf_span, "AGENT_COMPLETED 3");
}

/// Go `TestTraceManager_ExactMillisecondLatency`.
#[test]
fn test_trace_manager_exact_millisecond_latency() {
    let ctx = CallbackCtx::background();
    let clock = ManualClock::new();
    let mut tm = TraceManager::new();
    tm.now = clock.clock();
    let id = id_of("inv-clock", "agent", "");
    tm.invocation_span(&ctx, &id, None);
    tm.push_agent_span(&ctx, &id);
    tm.push_llm_span(&ctx, &id);
    clock.advance_ms(150);
    tm.record_first_token(&ctx, &id);
    clock.advance_ms(850);
    let llm_end = tm.pop_llm_span(&ctx, &id, true);
    assert_eq!(
        llm_end.latency,
        Some(Latency {
            total_ms: 1000,
            time_to_first_token_ms: Some(150),
        }),
        "LLM latency"
    );
}

/// Go `TestToolFailure_ShortCircuitedOnToolErrorStillLogsError`.
#[tokio::test]
async fn test_tool_failure_short_circuited_on_tool_error_still_logs_error() {
    let tp = setup_test_plugin().await;
    let run = MockRun::new(&tp.ctx);
    let cb = Cb(&tp.plugin);
    let lookup_tool = fake_tool("lookup");
    let a = args(json!({"city": "Paris"}));
    cb.before_agent(run.ctx()).await;
    cb.before_tool(run.tool("call-short-circuit"), &lookup_tool, a.clone())
        .await;
    // OnToolErrorCallback is skipped because an earlier plugin returned a result/error.
    cb.after_tool(
        run.tool("call-short-circuit"),
        &lookup_tool,
        a,
        Map::new(),
        Some("tool boom"),
    )
    .await;
    cb.after_agent(run.ctx()).await;
    cb.after_run(run.inv()).await;

    let rows = tp
        .collect_rows(until_event_type(EVENT_INVOCATION_COMPLETED))
        .await;
    assert_eq!(
        event_types(&rows),
        [
            EVENT_AGENT_STARTING,
            EVENT_TOOL_STARTING,
            EVENT_TOOL_ERROR,
            EVENT_AGENT_COMPLETED,
            EVENT_INVOCATION_COMPLETED,
        ]
    );
    let tool_start = place_of(&only_row_of_type(&rows, EVENT_TOOL_STARTING));
    let tool_err = only_row_of_type(&rows, EVENT_TOOL_ERROR);
    assert_eq!(place_of(&tool_err), tool_start, "TOOL_ERROR place");
    must_latency(&tool_err);
    assert_eq!(str_col(&tool_err, "status"), "ERROR", "TOOL_ERROR status");
}

/// Go `TestTraceManager_InFlightProtectedFromTTLSweepAndPreservesTraceOnCapEviction`.
#[test]
fn test_trace_manager_in_flight_protected_from_ttl_sweep_and_preserves_trace_on_cap_eviction() {
    let ctx = CallbackCtx::background();
    let clock = ManualClock::new();
    let start = clock.now();
    let warnings: Arc<parking_lot::Mutex<Vec<String>>> = Arc::default();
    let mut tm = TraceManager::new();
    tm.now = clock.clock();
    let w = Arc::clone(&warnings);
    tm.logf = Some(Arc::new(move |line: &str| w.lock().push(line.to_owned())));

    let in_flight = id_of("in-flight", "root", "");
    let idle = id_of("idle", "root", "");
    let orig_span = tm.invocation_span(&ctx, &in_flight, None);
    tm.push_agent_span(&ctx, &in_flight);
    tm.push_tool_span(&ctx, &in_flight, "long-tool");
    tm.invocation_span(&ctx, &idle, None);

    // Advance past the TTL and trigger a sweep: idle is dropped, but in-flight (which still
    // has an open agent and tool span) is retained.
    set_clock(&clock, start, 2 * 3600 * 1000);
    for i in 3..=SWEEP_INTERVAL {
        let id = id_of(&format!("filler-{i}"), "", "");
        tm.invocation_span(&ctx, &id, None);
        tm.end_invocation(&ctx, &id);
    }
    let tracked = tracked_invocations(&tm);
    assert!(
        !tracked.contains(&"idle".to_string()),
        "idle invocation is still tracked after TTL sweep"
    );
    assert!(
        tracked.contains(&"in-flight".to_string()),
        "in-flight invocation with open spans was dropped by TTL sweep"
    );

    // Force capacity eviction of in-flight.
    tm.max_tracked = 1;
    clock.advance_ms(60_000);
    tm.invocation_span(&ctx, &id_of("other", "other-root", ""), None);
    {
        let warnings = warnings.lock();
        assert!(
            warnings.len() == 1 && warnings[0].contains("in-flight"),
            "capacity eviction warnings = {warnings:?}, want 1 warning naming in-flight"
        );
    }

    // Later callbacks for in-flight re-create its tracking state from its remembered trace ID
    // and root agent name, and omit a bogus latency on INVOCATION_COMPLETED.
    clock.advance_ms(60_000);
    let sub = id_of("in-flight", "sub", "");
    let tool_end = tm.pop_tool_span(&ctx, &sub, "long-tool", false).0;
    assert!(
        tool_end.trace_id == orig_span.trace_id && tool_end.root_agent_name == "root",
        "post-eviction pop_tool_span = {tool_end:?}, want trace {:?} and root agent \"root\"",
        orig_span.trace_id
    );
    let inv_end = tm.end_invocation(&ctx, &sub);
    assert!(
        inv_end.trace_id == orig_span.trace_id
            && inv_end.span_id == orig_span.span_id
            && inv_end.root_agent_name == "root",
        "post-eviction end_invocation = {inv_end:?}, want trace {:?}, span {:?}, root agent \"root\"",
        orig_span.trace_id,
        orig_span.span_id
    );
    assert!(
        inv_end.latency.is_none(),
        "post-eviction end_invocation has latency {:?}",
        inv_end.latency
    );
}

/// Go `TestTraceManager_ConcurrentSubAgentsAndModelCalls`: 32 threads run sub-agent spans,
/// model calls, tool calls and event lookups on one invocation.
#[test]
fn test_trace_manager_concurrent_sub_agents_and_model_calls() {
    const WORKERS: usize = 32;
    let ctx = CallbackCtx::background();
    let tm = TraceManager::new();
    let root_id = id_of("inv-concurrent", "root", "");
    let inv_span = tm.invocation_span(&ctx, &root_id, None);
    let root_span = tm.push_agent_span(&ctx, &root_id).0;

    std::thread::scope(|s| {
        for i in 0..WORKERS {
            let (tm, ctx, root_id, inv_span, root_span) =
                (&tm, &ctx, &root_id, &inv_span, &root_span);
            s.spawn(move || {
                let sub = Identity {
                    invocation_id: "inv-concurrent".into(),
                    agent_name: format!("worker-{i}"),
                    branch: format!("root.worker-{i}"),
                    path: format!("root@0/worker-{i}@{}", i + 1),
                    ..Identity::default()
                };
                let agent_span = tm.push_agent_span(ctx, &sub).0;
                assert!(
                    agent_span.trace_id == inv_span.trace_id
                        && agent_span.parent_span_id == root_span.span_id,
                    "worker {i} agent span = {agent_span:?}"
                );
                let llm_start = tm.push_llm_span(ctx, &sub);
                tm.record_first_token(ctx, &sub);
                let llm_end = tm.pop_llm_span(ctx, &sub, true);
                assert!(
                    llm_end.span_id == llm_start.span_id
                        && llm_end.parent_span_id == agent_span.span_id,
                    "worker {i} llm end = {llm_end:?}"
                );
                let key = tool_key(&sub, "call_0", "lookup");
                let tool_start = tm.push_tool_span(ctx, &sub, &key);
                let ev = tm.event_span(ctx, root_id, &sub.agent_name, &sub.branch);
                assert_eq!(ev.span_id, agent_span.span_id, "worker {i} event span");
                let (tool_end, _, closed) = tm.pop_tool_span(ctx, &sub, &key, false);
                assert!(
                    !closed
                        && tool_end.span_id == tool_start.span_id
                        && tool_end.parent_span_id == agent_span.span_id,
                    "worker {i} tool end = {tool_end:?} (closed={closed})"
                );
                let agent_end = tm.pop_agent_span(ctx, &sub);
                assert_eq!(
                    agent_end.span_id, agent_span.span_id,
                    "worker {i} agent end"
                );
            });
        }
    });

    tm.pop_agent_span(&ctx, &root_id);
    assert_eq!(
        open_spans(&tm, "inv-concurrent"),
        SpanCounts::default(),
        "open spans after concurrent workers"
    );
    tm.end_invocation(&ctx, &root_id);
    assert!(tracked_invocations(&tm).is_empty(), "tracked invocations");
}

/// Go `TestTraceManager_ConcurrentInvocations`: 32 threads start, run and end independent
/// invocations.
#[test]
fn test_trace_manager_concurrent_invocations() {
    const WORKERS: usize = 32;
    let ctx = CallbackCtx::background();
    let tm = TraceManager::new();
    std::thread::scope(|s| {
        for i in 0..WORKERS {
            let (tm, ctx) = (&tm, &ctx);
            s.spawn(move || {
                let id = id_of(&format!("inv-{i}"), "planner", "");
                let inv = tm.invocation_span(ctx, &id, None);
                let ag = tm.push_agent_span(ctx, &id).0;
                let llm = tm.push_llm_span(ctx, &id);
                tm.record_first_token(ctx, &id);
                let llm_end = tm.pop_llm_span(ctx, &id, true);
                let ag_end = tm.pop_agent_span(ctx, &id);
                let inv_end = tm.end_invocation(ctx, &id);
                assert!(
                    ag.parent_span_id == inv.span_id
                        && llm.parent_span_id == ag.span_id
                        && llm_end.span_id == llm.span_id
                        && ag_end.span_id == ag.span_id
                        && inv_end.span_id == inv.span_id,
                    "invocation {i} spans inconsistent: inv={inv:?} ag={ag:?} llm={llm:?}"
                );
            });
        }
    });
    assert!(
        tracked_invocations(&tm).is_empty(),
        "tracked invocations after all concurrent invocations ended = {:?}",
        tracked_invocations(&tm)
    );
}
