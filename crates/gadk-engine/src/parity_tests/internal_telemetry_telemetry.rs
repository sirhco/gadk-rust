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

//! Ports of ADK-Go `internal/telemetry/telemetry_test.go`.
//!
//! Spans are captured with the 1b span capture (`capture_spans_in_env`), whose string fields are
//! Go's `attributesToMap` (`Value.Emit`: a structured attribute is its JSON text); the span
//! status is the `otel.status_code` field and its description the `error.message` field. Every
//! test installs its telemetry environment with `test_env` (Go `setEnvForTesting`; an empty one
//! where Go reads the default).

use std::collections::BTreeMap;
use std::sync::Arc;

use futures::StreamExt;
use gadk_agent::{AgentConfig, AgentError, InvocationContext, agent};
use gadk_genai::{
    Content, FinishReason, FunctionCall, FunctionResponse, GenerateContentResponseUsageMetadata,
    Part,
};
use gadk_model::{LlmRequest, LlmResponse, ModelError};
use gadk_platform::GoWrapped;
use gadk_session::{CreateRequest, Event, InMemorySessionService, SessionService};
use pretty_assertions::assert_eq;
use rstest::rstest;
use serde_json::{Map, Value};

use crate::llm::flow::span_capture::{CapturedSpan, capture_spans_in_env};
use crate::telemetry::attrs::{
    GenerateContentParams, GenerateContentResult, MERGE_TOOL_NAME, SCHEMA_URL_1_36,
    SCHEMA_URL_1_44, SpanError, error_type, schema_url, start_execute_tool_span,
    start_generate_content_span, trace_generate_content_result, trace_merged_tool_calls_result,
    trace_tool_result,
};
use crate::telemetry::env::{
    CAPTURE_MESSAGE_CONTENT_ENV_VAR, OTEL_SEMCONV_1_36, SCHEMA_VERSION_OPT_IN_ENV_VAR, test_env,
};
use crate::telemetry::{merged_tool_calls_span, workflow_span};

const ERR_TEST: &str = "test error";

/// Go `attributesToMap`: the span's OTel attributes as `Value.Emit` strings (the capture's
/// `otel.*` fields, the status description and the error event's message are not attributes).
fn attributes_to_map(s: &CapturedSpan) -> BTreeMap<String, String> {
    s.fields
        .iter()
        .filter(|(k, _)| !k.starts_with("otel.") && *k != "error.message" && *k != "message")
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// The one span named `otel_name`.
fn only<'a>(spans: &'a [CapturedSpan], otel_name: &str) -> &'a CapturedSpan {
    let found: Vec<_> = spans.iter().filter(|s| s.otel_name == otel_name).collect();
    assert_eq!(found.len(), 1, "spans named {otel_name}: {spans:#?}");
    found[0]
}

/// Go's span status: `ERROR` when the status field says so, else `UNSET`.
fn status(s: &CapturedSpan) -> &str {
    match s.field("otel.status_code").as_deref() {
        Some("ERROR") => "ERROR",
        _ => "UNSET",
    }
}

fn want(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

fn assert_has(got: &BTreeMap<String, String>, wanted: &[(&str, &str)]) {
    for (k, v) in wanted {
        assert_eq!(got.get(*k).map(String::as_str), Some(*v), "attribute {k}");
    }
}

// parity: skipped TestWrapYield: Go's WrapYield wraps an iter.Seq2 yield function; Rust streams have none, and the agent span's end-of-stream status is CustomAgent's (gadk-agent) RunState drop
// parity: skipped TestWrapYield_MultipleCalls: as TestWrapYield (Go iter.Seq2 yield wrapper)

/// Go `fakeAgent` (an LLM agent) yielding `err`, if any.
fn fake_agent(err: Option<&'static str>) -> Arc<dyn gadk_agent::Agent> {
    agent::new(AgentConfig {
        name: "test-agent".into(),
        description: "test-agent-description".into(),
        run: Some(Arc::new(move |ctx: InvocationContext| {
            Box::pin(futures::stream::once(async move {
                match err {
                    Some(e) => Err(AgentError::Config(e.to_owned())),
                    None => Ok(Event::new(ctx.platform(), ctx.invocation_id())),
                }
            }))
        })),
        ..Default::default()
    })
    .unwrap()
}

// parity: adapted: Go drives StartNodeSpan + TraceAgentResult on a fake agent; gadk's invoke_agent span is CustomAgent::run's (gadk-agent), so the test runs a custom agent whose stream yields the result
#[rstest]
#[case::success(None)]
#[case::error(Some(ERR_TEST))]
#[tokio::test]
async fn test_invoke_agent(#[case] err: Option<&'static str>) {
    let _env = test_env(&[]);
    let svc = InMemorySessionService::new();
    let sess = svc
        .create(CreateRequest {
            app_name: "test_app".into(),
            user_id: "test_user".into(),
            session_id: Some("test-session".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let a = fake_agent(err);
    let ctx = InvocationContext::builder()
        .agent(a.clone())
        .session(sess)
        .invocation_id("test-invocation-id")
        .build_invocation();
    let ((), spans) = capture_spans_in_env(async {
        let mut s = a.run(ctx);
        while s.next().await.is_some() {}
    })
    .await;
    let span = only(&spans, "invoke_agent test-agent");
    assert_eq!(status(span), if err.is_some() { "ERROR" } else { "UNSET" });
    if let Some(e) = err {
        assert_eq!(span.field("error.message").as_deref(), Some(e));
    }
    assert_has(
        &attributes_to_map(span),
        &[
            ("gen_ai.operation.name", "invoke_agent"),
            ("gen_ai.agent.name", "test-agent"),
            ("gen_ai.agent.description", "test-agent-description"),
            ("gen_ai.conversation.id", "test-session"),
            ("gcp.vertex.agent.invocation_id", "test-invocation-id"),
        ],
    );
}

fn usage(
    prompt: i32,
    tool_use_prompt: i32,
    candidates: i32,
    cached: i32,
    thoughts: i32,
) -> Option<GenerateContentResponseUsageMetadata> {
    Some(GenerateContentResponseUsageMetadata {
        prompt_token_count: prompt,
        tool_use_prompt_token_count: tool_use_prompt,
        candidates_token_count: candidates,
        cached_content_token_count: cached,
        thoughts_token_count: thoughts,
        ..Default::default()
    })
}

fn result_with(resp: Option<LlmResponse>, err: Option<&str>) -> GenerateContentResult {
    GenerateContentResult {
        response: resp.map(Arc::new),
        error: err.map(SpanError::text),
        ..Default::default()
    }
}

/// Go `startGenerateContentSpan` + `traceGenerateContentResult` + `End` on model `model`.
async fn generate_content_spans(model: &str, result: GenerateContentResult) -> Vec<CapturedSpan> {
    let params = GenerateContentParams {
        model_name: model.into(),
        invocation_id: "test-invocation-id".into(),
        ..Default::default()
    };
    let ((), spans) = capture_spans_in_env(async {
        let span = start_generate_content_span(&params, &LlmRequest::default());
        trace_generate_content_result(&span, &result);
    })
    .await;
    spans
}

#[rstest]
#[case::success(
    result_with(Some(LlmResponse { usage_metadata: usage(10, 0, 20, 5, 15), finish_reason: Some(FinishReason::Stop), ..Default::default() }), None),
    None,
    &[
        ("gen_ai.operation.name", "generate_content"),
        ("gen_ai.request.model", "test-model"),
        ("gen_ai.usage.input_tokens", "10"),
        ("gen_ai.usage.output_tokens", "35"),
        ("gen_ai.usage.cache_read.input_tokens", "5"),
        ("gen_ai.usage.reasoning.output_tokens", "15"),
        ("gen_ai.response.finish_reasons", "[\"stop\"]"),
        ("gcp.vertex.agent.invocation_id", "test-invocation-id"),
    ][..],
)]
#[case::tool_use_prompt_tokens_count_as_input(
    result_with(Some(LlmResponse { usage_metadata: usage(10, 7, 20, 0, 15), finish_reason: Some(FinishReason::Stop), ..Default::default() }), None),
    None,
    &[("gen_ai.usage.input_tokens", "17"), ("gen_ai.usage.output_tokens", "35")][..],
)]
#[case::error(
    result_with(None, Some(ERR_TEST)),
    Some(ERR_TEST),
    &[
        ("gen_ai.operation.name", "generate_content"),
        ("gen_ai.request.model", "test-model"),
        ("gen_ai.response.finish_reasons", "[\"error\"]"),
        ("gcp.vertex.agent.invocation_id", "test-invocation-id"),
    ][..],
)]
#[tokio::test]
async fn test_generate_content(
    #[case] result: GenerateContentResult,
    #[case] want_err: Option<&str>,
    #[case] want_attrs: &[(&str, &str)],
) {
    let _env = test_env(&[]);
    let spans = generate_content_spans("test-model", result).await;
    let span = only(&spans, "generate_content test-model");
    assert_eq!(span.name, "generate_content");
    assert_eq!(
        status(span),
        if want_err.is_some() { "ERROR" } else { "UNSET" }
    );
    if let Some(e) = want_err {
        assert_eq!(span.field("error.message").as_deref(), Some(e));
    }
    assert_has(&attributes_to_map(span), want_attrs);
}

fn content_with(part: Part) -> Option<Content> {
    Some(Content {
        role: "model".into(),
        parts: vec![part],
    })
}

#[rstest]
#[case::stop(Some(LlmResponse { finish_reason: Some(FinishReason::Stop), ..Default::default() }), None, Some("stop"))]
#[case::tool_call(Some(LlmResponse {
    content: content_with(Part { function_call: Some(FunctionCall { name: "lookup".into(), ..Default::default() }), ..Default::default() }),
    finish_reason: Some(FinishReason::Stop),
    ..Default::default()
}), None, Some("tool_call"))]
#[case::length(Some(LlmResponse { finish_reason: Some(FinishReason::MaxTokens), ..Default::default() }), None, Some("length"))]
#[case::unreported(Some(LlmResponse { content: content_with(Part { text: "mid-stream".into(), ..Default::default() }), ..Default::default() }), None, None)]
#[case::unspecified(Some(LlmResponse { finish_reason: Some(FinishReason::Unspecified), ..Default::default() }), None, None)]
#[case::safety_error_code(Some(LlmResponse { finish_reason: Some(FinishReason::Safety), error_code: "SAFETY".into(), ..Default::default() }), None, Some("error"))]
#[case::stream_error(Some(LlmResponse {
    content: content_with(Part { text: "partial".into(), ..Default::default() }),
    partial: true,
    ..Default::default()
}), Some(ERR_TEST), Some("error"))]
#[case::stream_error_without_response(None, Some(ERR_TEST), Some("error"))]
#[case::without_response_or_error(None, None, None)]
#[tokio::test]
async fn test_trace_generate_content_result_maps_finish_reason(
    #[case] response: Option<LlmResponse>,
    #[case] err: Option<&str>,
    #[case] want: Option<&str>,
) {
    let _env = test_env(&[]);
    let spans = generate_content_spans("test-model", result_with(response, err)).await;
    let attrs = attributes_to_map(only(&spans, "generate_content test-model"));
    let got = attrs.get("gen_ai.response.finish_reasons").cloned();
    assert_eq!(got, want.map(|w| format!("[\"{w}\"]")));
}

#[tokio::test]
async fn test_trace_generate_content_result_nil_response_nil_error_leaves_span_successful() {
    let _env = test_env(&[]);
    let spans = generate_content_spans("test-model", GenerateContentResult::default()).await;
    let span = only(&spans, "generate_content test-model");
    assert_eq!(status(span), "UNSET");
    assert!(!attributes_to_map(span).contains_key("gen_ai.response.finish_reasons"));
}

fn args(pairs: &[(&str, &str)]) -> Map<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), Value::String((*v).to_owned())))
        .collect()
}

#[rstest]
#[case::success(args(&[("arg", "val")]), Some(Event { id: "test-event".into(), ..Default::default() }), None)]
#[case::error(Map::new(), None, Some(ERR_TEST))]
#[tokio::test]
async fn test_execute_tool(
    #[case] tool_args: Map<String, Value>,
    #[case] response_event: Option<Event>,
    #[case] err: Option<&str>,
) {
    let _env = test_env(&[]);
    let span_err = err.map(SpanError::text);
    let ((), spans) = capture_spans_in_env(async {
        let span = start_execute_tool_span("test-tool", &tool_args);
        trace_tool_result(
            &span,
            "tool-description",
            response_event.as_ref(),
            span_err.as_ref(),
        );
    })
    .await;
    let span = only(&spans, "execute_tool test-tool");
    assert_eq!(span.name, "execute_tool");
    assert_eq!(status(span), if err.is_some() { "ERROR" } else { "UNSET" });
    if let Some(e) = err {
        assert_eq!(span.field("error.message").as_deref(), Some(e));
    } else {
        assert_has(
            &attributes_to_map(span),
            &[
                ("gen_ai.operation.name", "execute_tool"),
                ("gen_ai.tool.name", "test-tool"),
                ("gen_ai.tool.description", "tool-description"),
            ],
        );
    }
}

#[rstest]
#[case::otel_semconv_1_36(OTEL_SEMCONV_1_36, &[
    ("gen_ai.operation.name", "execute_tool"),
    ("gen_ai.tool.name", MERGE_TOOL_NAME),
    ("gen_ai.tool.description", MERGE_TOOL_NAME),
    ("gcp.vertex.agent.tool_call_args", "N/A"),
    ("gcp.vertex.agent.tool_response", "null"),
][..])]
#[case::otel_semconv_1_44("otel_semconv_1_44", &[
    ("gen_ai.operation.name", "execute_tool"),
    ("gen_ai.tool.name", MERGE_TOOL_NAME),
    ("gen_ai.tool.description", MERGE_TOOL_NAME),
][..])]
#[tokio::test]
async fn test_trace_merged_tool_calls_result(
    #[case] schema_version: &str,
    #[case] wanted: &[(&str, &str)],
) {
    let _env = test_env(&[(SCHEMA_VERSION_OPT_IN_ENV_VAR, schema_version)]);
    let ((), spans) = capture_spans_in_env(async {
        let span = merged_tool_calls_span();
        trace_merged_tool_calls_result(&span, None, None);
    })
    .await;
    assert_eq!(
        attributes_to_map(only(&spans, "execute_tool (merged)")),
        want(wanted)
    );
}

#[tokio::test]
async fn test_execute_tool_omits_what_is_unknown() {
    let _env = test_env(&[
        (SCHEMA_VERSION_OPT_IN_ENV_VAR, "otel_semconv_1_44"),
        (CAPTURE_MESSAGE_CONTENT_ENV_VAR, "SPAN_ONLY"),
    ]);
    let ev = Event {
        id: "e".into(),
        response: LlmResponse {
            content: Some(Content {
                role: String::new(),
                parts: vec![Part {
                    function_response: Some(FunctionResponse {
                        name: "t".into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    let ((), spans) = capture_spans_in_env(async {
        let span = start_execute_tool_span("t", &Map::new());
        trace_tool_result(&span, "d", Some(&ev), None);
    })
    .await;
    assert_eq!(
        attributes_to_map(only(&spans, "execute_tool t")),
        want(&[
            ("gen_ai.operation.name", "execute_tool"),
            ("gen_ai.tool.name", "t"),
            ("gen_ai.tool.description", "d"),
            ("gcp.vertex.agent.event_id", "e"),
        ])
    );
}

#[rstest]
#[case::otel_semconv_1_36(OTEL_SEMCONV_1_36, false)]
#[case::otel_semconv_1_44("otel_semconv_1_44", true)]
#[tokio::test]
async fn test_invoke_workflow_nested(#[case] schema_version: &str, #[case] want_nested: bool) {
    let _env = test_env(&[(SCHEMA_VERSION_OPT_IN_ENV_VAR, schema_version)]);
    let ((), spans) = capture_spans_in_env(async {
        let outer = workflow_span("outer", "s");
        let inner = outer.in_scope(|| workflow_span("inner", "s"));
        drop(inner);
        drop(outer);
    })
    .await;
    assert_eq!(spans.len(), 2);
    for s in &spans {
        let nested = attributes_to_map(s).contains_key("gen_ai.workflow.nested");
        let want = want_nested && s.otel_name == "invoke_workflow inner";
        assert_eq!(nested, want, "{}: nested", s.otel_name);
    }
}

// parity: adapted: Go names a non-API error's dynamic type (*errors.errorString for errors.New); a Rust error has no Go type, so every non-API error reads *errors.errorString; Go's %w wrapping is gadk_platform::GoWrapped
#[rstest]
#[case::api_error(Box::new(api_error()), "429")]
#[case::wrapped_api_error(Box::new(GoWrapped::new("model call", api_error())), "429")]
#[case::agent_api_error(Box::new(AgentError::Model(api_error())), "429")]
#[case::other_error(Box::new(AgentError::Config("boom".into())), "*errors.errorString")]
fn test_error_type(#[case] err: Box<dyn std::error::Error + Send + Sync>, #[case] want: &str) {
    assert_eq!(error_type(err.as_ref()), want);
}

fn api_error() -> ModelError {
    ModelError::Api {
        code: 429,
        status: String::new(),
        message: String::new(),
        details: Vec::new(),
    }
}

// parity: adapted: tracing spans have no instrumentation scope, so the schema URL Go puts on the tracer and logger is checked on schema_url() itself, with the generate_content span and its log record emitted under each setting
#[rstest]
#[case::otel_semconv_1_36("otel_semconv_1_36", SCHEMA_URL_1_36)]
#[case::otel_semconv_1_44("otel_semconv_1_44", SCHEMA_URL_1_44)]
#[case::default("", SCHEMA_URL_1_44)]
#[tokio::test]
async fn test_schema_url(#[case] schema: &str, #[case] want: &str) {
    let _env = test_env(&[(SCHEMA_VERSION_OPT_IN_ENV_VAR, schema)]);
    assert_eq!(schema_url(), want);
    let ((), spans, logs) = crate::telemetry::span_digest::capture(async {
        let params = GenerateContentParams {
            model_name: "m".into(),
            ..Default::default()
        };
        let mut s = crate::telemetry::instrumentation::instrument_generate_content(
            params,
            LlmRequest::default(),
            |_| {
                Box::pin(futures::stream::iter([(
                    Ok(LlmResponse::default()),
                    String::new(),
                )]))
            },
        );
        while s.next().await.is_some() {}
    })
    .await;
    assert_eq!(spans.len(), 1, "{spans:#?}");
    if want == SCHEMA_URL_1_44 {
        // The legacy schema's records are Go logger.go's (logRequest/logResponse).
        assert!(!logs.is_empty(), "no log records emitted");
    }
}
