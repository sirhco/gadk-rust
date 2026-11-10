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

//! Ports of ADK-Go `internal/telemetry/logger_test.go`. Log records are the D6 `tracing` events
//! a capture subscriber turns back into [`LogRecord`]s; Go's `toGoValue` view of a body or an
//! attribute is the record's JSON value. Go's `BackendUnspecified` is `None`.

use futures::StreamExt;
use gadk_genai::{Content, FinishReason, FunctionCall, GenerateContentConfig, Part};
use gadk_model::{Backend, LlmRequest, LlmResponse};
use pretty_assertions::assert_eq;
use rstest::rstest;
use serde_json::{Value, json};

use crate::telemetry::attrs::GenerateContentParams;
use crate::telemetry::env::{
    CAPTURE_MESSAGE_CONTENT_ENV_VAR, OTEL_SEMCONV_1_36, SCHEMA_VERSION_OPT_IN_ENV_VAR, TestEnv,
    test_env, use_legacy_schema,
};
use crate::telemetry::inference::GEN_AI_PROVIDER_NAME;
use crate::telemetry::instrumentation::instrument_generate_content;
use crate::telemetry::log_digest::LogRecord;
use crate::telemetry::logger::{log_request, log_response};
use crate::telemetry::semconv::INFERENCE_OPERATION_DETAILS_EVENT_NAME;
use crate::telemetry::span_digest::{SpanStub, capture};

const VERTEX: Option<Backend> = Some(Backend::VertexAi);
const GEMINI: Option<Backend> = Some(Backend::GeminiApi);
const UNSPECIFIED: Option<Backend> = None;

/// Go `setup`: the legacy schema, content capture on (`true`) or off (empty).
fn setup(capture_content: bool) -> TestEnv {
    test_env(&[
        (
            CAPTURE_MESSAGE_CONTENT_ENV_VAR,
            if capture_content { "true" } else { "" },
        ),
        (SCHEMA_VERSION_OPT_IN_ENV_VAR, OTEL_SEMCONV_1_36),
    ])
}

/// The log records `f` emits with no span current (Go's `t.Context()`).
async fn records(f: impl FnOnce(&tracing::Span)) -> Vec<LogRecord> {
    let ((), _, logs) = capture(async { f(&tracing::Span::none()) }).await;
    logs
}

fn text_part(text: &str) -> Part {
    Part {
        text: text.into(),
        ..Default::default()
    }
}

fn content(role: &str, texts: &[&str]) -> Content {
    Content {
        role: role.into(),
        parts: texts.iter().map(|t| text_part(t)).collect(),
    }
}

fn system_config(texts: &[&str]) -> Option<GenerateContentConfig> {
    Some(GenerateContentConfig {
        system_instruction: Some(content("system", texts)),
        ..Default::default()
    })
}

fn gen_ai_system(backend: Option<Backend>) -> Vec<(String, Value)> {
    match backend {
        Some(Backend::GeminiApi) => vec![("gen_ai.system".into(), json!("gcp.gemini"))],
        Some(Backend::VertexAi) => vec![("gen_ai.system".into(), json!("gcp.vertex_ai"))],
        None => vec![],
    }
}

/// A record as a test compares it: event name, body, attributes.
type Summary = (String, Value, Vec<(String, Value)>);

/// The [`Summary`] of each record.
fn summarize(logs: &[LogRecord]) -> Vec<Summary> {
    logs.iter()
        .map(|r| (r.event_name.clone(), r.body.clone(), r.attributes.clone()))
        .collect()
}

fn request_with_system_and_user_messages() -> LlmRequest {
    LlmRequest {
        config: system_config(&["System instruction part 1", "System instruction part 2"]),
        contents: vec![
            // Messages from previous turns.
            content(
                "user",
                &[
                    "Previous user message part 1",
                    "Previous user message part 2",
                ],
            ),
            content(
                "agent",
                &[
                    "Previous agent message part 1",
                    "Previous agent message part 2",
                ],
            ),
            // New message.
            content("user", &["User message part 1", "User message part 2"]),
        ],
        ..Default::default()
    }
}

fn empty_config_and_user_content_without_parts() -> LlmRequest {
    LlmRequest {
        // Config without system instruction.
        config: Some(GenerateContentConfig::default()),
        // Content without parts.
        contents: vec![Content {
            role: "user".into(),
            parts: vec![],
        }],
        ..Default::default()
    }
}

fn user_message(role: &str, texts: [&str; 2]) -> Value {
    json!({"content": {"role": role, "parts": [{"text": texts[0]}, {"text": texts[1]}]}})
}

#[rstest]
#[case::request_with_system_and_user_messages(
    true,
    UNSPECIFIED,
    request_with_system_and_user_messages(),
    vec![
        ("gen_ai.system.message", json!({"content": "System instruction part 1\nSystem instruction part 2"})),
        ("gen_ai.user.message", user_message("user", ["Previous user message part 1", "Previous user message part 2"])),
        ("gen_ai.user.message", user_message("agent", ["Previous agent message part 1", "Previous agent message part 2"])),
        ("gen_ai.user.message", user_message("user", ["User message part 1", "User message part 2"])),
    ]
)]
#[case::request_with_nil_config_and_contents(
    true,
    UNSPECIFIED,
    LlmRequest::default(),
    vec![("gen_ai.system.message", json!({"content": null}))]
)]
#[case::request_with_nil_contents_gemini_backend(
    true,
    GEMINI,
    LlmRequest::default(),
    vec![("gen_ai.system.message", json!({"content": null}))]
)]
#[case::request_with_nil_contents_vertex_backend(
    true,
    VERTEX,
    LlmRequest::default(),
    vec![("gen_ai.system.message", json!({"content": null}))]
)]
#[case::request_with_empty_config_and_user_content_without_parts(
    true,
    UNSPECIFIED,
    empty_config_and_user_content_without_parts(),
    vec![
        ("gen_ai.system.message", json!({"content": null})),
        ("gen_ai.user.message", json!({"content": {"role": "user"}})),
    ]
)]
#[case::elided_request(
    false,
    UNSPECIFIED,
    LlmRequest {
        config: system_config(&["System instruction"]),
        contents: vec![content("user", &["Hello"])],
        ..Default::default()
    },
    vec![
        ("gen_ai.system.message", json!({"content": "<elided>"})),
        ("gen_ai.user.message", json!({"content": "<elided>"})),
    ]
)]
#[case::elided_request_with_nil_config_and_contents(
    false,
    UNSPECIFIED,
    LlmRequest::default(),
    vec![("gen_ai.system.message", json!({"content": "<elided>"}))]
)]
#[case::elided_request_with_empty_config_and_user_content_without_parts(
    false,
    UNSPECIFIED,
    empty_config_and_user_content_without_parts(),
    vec![
        ("gen_ai.system.message", json!({"content": "<elided>"})),
        ("gen_ai.user.message", json!({"content": "<elided>"})),
    ]
)]
#[tokio::test]
async fn test_log_request(
    #[case] capture_message_content: bool,
    #[case] backend: Option<Backend>,
    #[case] req: LlmRequest,
    #[case] want_events: Vec<(&str, Value)>,
) {
    let _env = setup(capture_message_content);
    let logs = records(|span| log_request(span, &req, backend)).await;
    let want: Vec<_> = want_events
        .into_iter()
        .map(|(name, body)| (name.to_owned(), body, gen_ai_system(backend)))
        .collect();
    assert_eq!(summarize(&logs), want);
}

fn model_response(parts: Vec<Part>) -> Option<LlmResponse> {
    Some(LlmResponse {
        finish_reason: Some(FinishReason::Stop),
        content: Some(Content {
            role: "model".into(),
            parts,
        }),
        ..Default::default()
    })
}

fn texts(texts: &[&str]) -> Vec<Part> {
    texts.iter().map(|t| text_part(t)).collect()
}

fn function_call_part(name: &str, id: &str, arg: (&str, &str)) -> Part {
    Part {
        function_call: Some(FunctionCall {
            name: name.into(),
            id: id.into(),
            args: json!({arg.0: arg.1})
                .as_object()
                .cloned()
                .unwrap_or_default(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn choice(text_parts: &[&str]) -> Value {
    let parts: Vec<Value> = text_parts.iter().map(|t| json!({"text": t})).collect();
    json!({"index": 0, "finish_reason": "STOP", "content": {"role": "model", "parts": parts}})
}

#[rstest]
#[case::response(
    true,
    model_response(texts(&["Text part 1", "Text part 2"])),
    UNSPECIFIED,
    choice(&["Text part 1", "Text part 2"])
)]
#[case::response_gemini_backend(
    true,
    model_response(texts(&["Text"])),
    GEMINI,
    choice(&["Text"])
)]
#[case::response_vertex_backend(
    true,
    model_response(texts(&["Text part 1", "Text part 2"])),
    VERTEX,
    choice(&["Text part 1", "Text part 2"])
)]
#[case::response_with_function_call(
    true,
    model_response(vec![
        Part { thought: true, text: "Call tools".into(), ..Default::default() },
        function_call_part("myTool1", "id1", ("arg1", "val1")),
        function_call_part("myTool2", "id2", ("arg2", "val2")),
    ]),
    UNSPECIFIED,
    json!({
        "index": 0,
        "finish_reason": "STOP",
        "content": {"role": "model", "parts": [
            {"text": "Call tools", "thought": true},
            {"functionCall": {"name": "myTool1", "id": "id1", "args": {"arg1": "val1"}}},
            {"functionCall": {"name": "myTool2", "id": "id2", "args": {"arg2": "val2"}}},
        ]},
    })
)]
#[case::nil_response(true, None, UNSPECIFIED, json!({"index": 0, "content": null}))]
#[case::elided_response(
    false,
    model_response(texts(&["Response part 1", "Response part 2"])),
    UNSPECIFIED,
    json!({"index": 0, "finish_reason": "STOP", "content": "<elided>"})
)]
#[case::elided_nil_response(false, None, UNSPECIFIED, json!({"index": 0, "content": "<elided>"}))]
#[tokio::test]
async fn test_log_response(
    #[case] capture_message_content: bool,
    #[case] resp: Option<LlmResponse>,
    #[case] backend: Option<Backend>,
    #[case] want_body: Value,
) {
    let _env = setup(capture_message_content);
    let logs = records(|span| log_response(span, resp.as_ref(), backend)).await;
    assert_eq!(
        summarize(&logs),
        vec![(
            "gen_ai.choice".to_owned(),
            want_body,
            gen_ai_system(backend)
        )]
    );
}

#[tokio::test]
async fn test_span_id_propagation() {
    let _env = setup(false);
    let req = LlmRequest {
        config: system_config(&["You are a helpful assistant."]),
        contents: vec![content("user", &["Hello"])],
        ..Default::default()
    };
    let ((), spans, logs) = capture(async {
        let span = tracing::info_span!("test");
        log_request(&span, &req, VERTEX);
        log_response(&span, Some(&LlmResponse::default()), VERTEX);
    })
    .await;
    assert_eq!(logs.len(), 3, "{logs:#?}");
    let want_span_id = spans[0].span_id;
    for r in &logs {
        assert_eq!(r.span_id, Some(want_span_id), "{}", r.event_name);
    }
}

/// The value of `key` among `attrs`.
fn attr<'a>(attrs: &'a [(String, Value)], key: &str) -> Option<&'a Value> {
    attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

// Not covered by goldens, whose mock model has no backend.
#[rstest]
#[case::otel_semconv_1_44_vertex_ai("otel_semconv_1_44", VERTEX, Some("gcp.vertex_ai"), 1)]
#[case::otel_semconv_1_44_gemini_api("otel_semconv_1_44", GEMINI, Some("gcp.gemini"), 1)]
#[case::otel_semconv_1_44_unspecified("otel_semconv_1_44", UNSPECIFIED, None, 1)]
#[case::otel_semconv_1_36_vertex_ai("otel_semconv_1_36", VERTEX, None, 0)]
#[tokio::test]
async fn test_generate_content_provider_name(
    #[case] schema: &str,
    #[case] backend: Option<Backend>,
    #[case] want: Option<&str>,
    #[case] want_events: usize,
) {
    let _env = test_env(&[(SCHEMA_VERSION_OPT_IN_ENV_VAR, schema)]);
    let params = GenerateContentParams {
        model_name: "m".into(),
        backend,
        ..Default::default()
    };
    let ((), spans, logs): ((), Vec<SpanStub>, Vec<LogRecord>) = capture(async {
        let mut s = instrument_generate_content(params, LlmRequest::default(), |_| {
            Box::pin(futures::stream::iter([(
                Ok(LlmResponse::default()),
                String::new(),
            )]))
        });
        while s.next().await.is_some() {}
    })
    .await;
    assert_eq!(spans.len(), 1, "{spans:#?}");
    let want = want.map(|w| json!(w));
    assert_eq!(
        attr(&spans[0].attributes, GEN_AI_PROVIDER_NAME),
        want.as_ref(),
        "span gen_ai.provider.name"
    );
    let events: Vec<_> = logs
        .iter()
        .filter(|r| r.event_name == INFERENCE_OPERATION_DETAILS_EVENT_NAME)
        .collect();
    for r in &events {
        assert_eq!(
            attr(&r.attributes, GEN_AI_PROVIDER_NAME),
            want.as_ref(),
            "event gen_ai.provider.name"
        );
    }
    assert_eq!(
        events.len(),
        want_events,
        "{INFERENCE_OPERATION_DETAILS_EVENT_NAME} events"
    );
}

#[rstest]
#[case::otel_semconv_1_36("otel_semconv_1_36", true)]
#[case::otel_semconv_1_36_mixed_case("OTel_Semconv_1_36", true)]
#[case::one_with_space(" 1\n", true)]
#[case::otel_semconv_1_44("otel_semconv_1_44", false)]
#[case::two("2", false)]
#[case::empty("", false)]
#[case::true_value("true", false)]
fn test_use_legacy_schema(#[case] value: &str, #[case] want: bool) {
    let _env = test_env(&[(SCHEMA_VERSION_OPT_IN_ENV_VAR, value)]);
    assert_eq!(
        use_legacy_schema(),
        want,
        "use_legacy_schema() for {value:?}"
    );
}
