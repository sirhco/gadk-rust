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

//! Ports of ADK-Go `internal/telemetry/genaimessages_test.go`: the GenAI message content
//! (`gen_ai.input.messages`, `gen_ai.output.messages`, `gen_ai.system_instructions`) checked
//! against the OpenTelemetry GenAI message schemas, the finish-reason mapping, and the content on
//! real `generate_content` / `execute_tool` spans.
//!
//! Go's `attrJSON` reads an attribute as JSON whatever its encoding: here a STRING attribute is
//! its text and a structured one the JSON of `from_log_value`. On a captured span, a structured
//! attribute is the JSON text the capture reads from `record_bytes`. Every test installs its
//! telemetry environment with `test_env` (Go `t.Setenv` + `ApplyEnv`).

use std::collections::BTreeMap;
use std::sync::Arc;

use gadk_genai::{
    Blob, Content, FileData, FinishReason, FunctionCall, FunctionResponse, GenerateContentConfig,
    Part, ToolResponse,
};
use gadk_model::{LlmRequest, LlmResponse};
use gadk_session::Event;
use parking_lot::Mutex;
use pretty_assertions::assert_eq;
use rstest::rstest;
use serde_json::{Map, Value, json};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id};
use tracing_subscriber::layer::{Context as LayerContext, Layer, SubscriberExt};

use crate::llm::flow::span_capture::{CapturedSpan, capture_spans_in_env};
use crate::telemetry::attrs::{
    GEN_AI_REQUEST_MODEL, GEN_AI_TOOL_CALL_ARGUMENTS, GEN_AI_TOOL_NAME, GenerateContentParams,
    GenerateContentResult, SpanError, start_execute_tool_span, start_generate_content_span,
    trace_generate_content_result, trace_tool_result,
};
use crate::telemetry::converters::{AttrValue, from_log_value};
use crate::telemetry::env::{
    CAPTURE_MESSAGE_CONTENT_ENV_VAR, OTEL_SEMCONV_1_36, SCHEMA_VERSION_OPT_IN_ENV_VAR,
    capture_content_on_spans, capture_message_content, test_env,
};
use crate::telemetry::genai_messages::{
    ChatMessage, ContentValue, GEN_AI_INPUT_MESSAGES, GEN_AI_OUTPUT_MESSAGES,
    GEN_AI_SYSTEM_INSTRUCTIONS, MAX_INLINE_DATA_BYTES, PartValue, event_content_attributes,
    modality_of, request_content, response_content, schema_finish_reason, schema_role,
    span_content_attributes, tool_payload,
};
use crate::telemetry::semconv::OperationDetails;
use crate::telemetry::span_digest::pin_tracing_interest;

/// Go `cloudTraceAttributeValueLimit`: the largest attribute value Cloud Trace accepts, stated as
/// the external limit rather than derived from `MAX_CONTENT_ATTRIBUTE_BYTES`.
const CLOUD_TRACE_ATTRIBUTE_VALUE_LIMIT: usize = 64 * 1024;

/// Go `captureContent`: spans have to be asked for by name (a truthy value means log records
/// only). `extra` adds further variables (Go's later `t.Setenv`).
fn capture_content(extra: &[(&str, &str)]) -> crate::telemetry::env::TestEnv {
    let mut pairs = vec![(CAPTURE_MESSAGE_CONTENT_ENV_VAR, "SPAN_AND_EVENT")];
    pairs.extend_from_slice(extra);
    test_env(&pairs)
}

/// Go `attrJSON` over computed attributes.
fn attr_json(attrs: &[(&'static str, AttrValue)], key: &str) -> Option<String> {
    attrs.iter().find(|(k, _)| *k == key).map(|(_, v)| match v {
        AttrValue::String(s) => s.clone(),
        other => from_log_value(other).to_string(),
    })
}

/// Go `attrJSON` over a captured span (a structured attribute is captured as its JSON text).
fn span_attr_json(s: &CapturedSpan, key: &str) -> Option<String> {
    s.field(key)
}

/// Go `jsonDiff`: both sides parsed, compared as values.
fn assert_json_eq(want: &str, got: &str, what: &str) {
    let w: Value = serde_json::from_str(want).expect("want is not JSON");
    let g: Value = serde_json::from_str(got).expect("got is not JSON");
    assert_eq!(w, g, "{what} mismatch");
}

/// Go `requiredPartFields`: the `required` list of each part definition in the GenAI input and
/// output message schemas (a type absent here is a GenericPart, requiring only `type`).
fn required_part_fields(typ: &str) -> &'static [&'static str] {
    match typ {
        "text" | "reasoning" => &["type", "content"],
        "tool_call" => &["type", "name"],
        "tool_call_response" => &["type", "response"],
        "server_tool_call" => &["type", "name", "server_tool_call"],
        "server_tool_call_response" => &["type", "server_tool_call_response"],
        "blob" => &["type", "modality", "content"],
        "file" => &["type", "modality", "file_id"],
        "uri" => &["type", "modality", "uri"],
        "compaction" => &["type"],
        _ => &[],
    }
}

/// Go `schemaRoles`.
const SCHEMA_ROLES: &[&str] = &["system", "user", "assistant", "tool"];
/// Go `schemaFinishReasons`.
const SCHEMA_FINISH_REASONS: &[&str] = &[
    "stop",
    "length",
    "content_filter",
    "tool_call",
    "compaction",
    "error",
];
/// Go `schemaModalities`.
const SCHEMA_MODALITIES: &[&str] = &["image", "video", "audio", "document"];
/// Go `enumFields`: fields derived from genai enums, which must never carry a protobuf wire name.
const ENUM_FIELDS: &[&str] = &[
    "type",
    "role",
    "name",
    "modality",
    "outcome",
    "language",
    "finish_reason",
];

/// Go `validateMessages`: checks a messages value against the schema; `want_finish_reason` asks
/// for the output-message rules. Stricter than the schema where Go is: role and finish_reason
/// must be in the enum.
fn validate_messages(encoded: &str, want_finish_reason: bool) -> Vec<Map<String, Value>> {
    let msgs: Vec<Map<String, Value>> = serde_json::from_str(encoded)
        .unwrap_or_else(|e| panic!("messages are not a JSON array of objects: {e}\n{encoded}"));
    for (i, m) in msgs.iter().enumerate() {
        match m.get("role").and_then(Value::as_str) {
            None => panic!("message {i}: missing string role: {m:?}"),
            Some(role) => assert!(
                SCHEMA_ROLES.contains(&role),
                "message {i}: role {role:?} is not in the schema enum"
            ),
        }
        let parts = m
            .get("parts")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("message {i}: missing parts array: {m:?}"));
        if want_finish_reason {
            match m.get("finish_reason").and_then(Value::as_str) {
                None | Some("") => {
                    panic!("message {i}: finish_reason is required on output messages: {m:?}")
                }
                Some(fr) => assert!(
                    SCHEMA_FINISH_REASONS.contains(&fr),
                    "message {i}: finish_reason {fr:?} is not in the schema enum"
                ),
            }
        } else {
            assert!(
                !m.contains_key("finish_reason"),
                "message {i}: finish_reason must not appear on input messages"
            );
        }
        for (j, p) in parts.iter().enumerate() {
            validate_part(&format!("message {i} part {j}"), p);
        }
        check_enum_fields(&format!("message {i}"), m);
    }
    msgs
}

/// Go `validateParts`: a `gen_ai.system_instructions` value (TextParts or GenericParts).
fn validate_parts(encoded: &str) -> Vec<Value> {
    let parts: Vec<Value> = serde_json::from_str(encoded)
        .unwrap_or_else(|e| panic!("system instructions are not a JSON array: {e}\n{encoded}"));
    for (i, p) in parts.iter().enumerate() {
        validate_part(&format!("instruction part {i}"), p);
    }
    parts
}

/// Go `validatePart`.
fn validate_part(at: &str, p: &Value) {
    let obj = p
        .as_object()
        .unwrap_or_else(|| panic!("{at}: part is not an object: {p}"));
    let typ = match obj.get("type").and_then(Value::as_str) {
        Some(t) if !t.is_empty() => t,
        _ => panic!("{at}: every part requires a non-empty string type: {obj:?}"),
    };
    for field in required_part_fields(typ) {
        assert!(
            obj.contains_key(*field),
            "{at}: part type {typ:?} requires field {field:?}: {obj:?}"
        );
    }
    if let Some(m) = obj.get("modality") {
        assert!(
            m.as_str().is_some_and(|s| SCHEMA_MODALITIES.contains(&s)),
            "{at}: modality {m} is not in the schema enum"
        );
    }
    // Nested server tool bodies are themselves objects requiring a type.
    for key in ["server_tool_call", "server_tool_call_response"] {
        if let Some(nested) = obj.get(key) {
            let n = nested
                .as_object()
                .unwrap_or_else(|| panic!("{at}: {key} is not an object: {nested}"));
            assert!(
                n.get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty()),
                "{at}: {key} requires a non-empty string type: {n:?}"
            );
            check_enum_fields(&format!("{at} {key}"), n);
        }
    }
    check_enum_fields(at, obj);
}

/// Go `checkEnumFields`: an enum-derived field must not carry a SCREAMING_SNAKE_CASE wire name.
fn check_enum_fields(at: &str, obj: &Map<String, Value>) {
    for (k, v) in obj {
        if !ENUM_FIELDS.contains(&k.as_str()) {
            continue;
        }
        let Some(s) = v.as_str().filter(|s| !s.is_empty()) else {
            continue;
        };
        assert!(
            !(s == s.to_uppercase() && s.chars().any(|c| c.is_ascii_uppercase())),
            "{at}: field {k:?} leaks the protobuf wire name {s:?} into telemetry"
        );
    }
}

fn text_part(text: &str) -> Part {
    Part::from_text(text)
}

fn content(role: &str, parts: Vec<Part>) -> Content {
    Content {
        role: role.into(),
        parts,
    }
}

fn function_call_part(name: &str) -> Part {
    Part {
        function_call: Some(FunctionCall {
            name: name.into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn function_response_part(name: &str, response: Map<String, Value>) -> Part {
    Part {
        function_response: Some(FunctionResponse {
            name: name.into(),
            response,
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn obj(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}

fn result_of(resp: LlmResponse) -> GenerateContentResult {
    GenerateContentResult {
        response: Some(Arc::new(resp)),
        ..Default::default()
    }
}

// --- Part mapping ------------------------------------------------------

#[test]
fn test_request_content_attributes_text_part_shape() {
    let _env = capture_content(&[]);
    let req = LlmRequest {
        contents: vec![
            content("user", vec![text_part("Weather in Paris?")]),
            content("model", vec![text_part("Rainy.")]),
        ],
        ..Default::default()
    };
    let got = attr_json(
        &span_content_attributes(&request_content(&req)),
        GEN_AI_INPUT_MESSAGES,
    )
    .expect("gen_ai.input.messages was not set");
    let want = r#"[{"role":"user","parts":[{"type":"text","content":"Weather in Paris?"}]},{"role":"assistant","parts":[{"type":"text","content":"Rainy."}]}]"#;
    assert_json_eq(want, &got, "gen_ai.input.messages");
}

#[rstest]
#[case::user(content("user", vec![]), "user")]
#[case::model_becomes_assistant(content("model", vec![]), "assistant")]
#[case::empty_defaults_to_user(content("", vec![]), "user")]
#[case::function_response_turn_is_a_tool_message(
    content("user", vec![function_response_part("t", Map::new())]),
    "tool"
)]
#[case::server_tool_response_turn_is_a_tool_message(
    content("model", vec![Part {
        tool_response: Some(ToolResponse { id: "1".into(), ..Default::default() }),
        ..Default::default()
    }]),
    "tool"
)]
#[case::mixed_turn_with_a_tool_result_is_a_tool_message(
    content("user", vec![text_part("here you go"), function_response_part("t", Map::new())]),
    "tool"
)]
#[case::unknown_role_passes_through(content("narrator", vec![]), "narrator")]
fn test_schema_role(#[case] c: Content, #[case] want: &str) {
    assert_eq!(schema_role(Some(&c)), want);
}

/// The role a full tool round trip produces, since genai labels a tool result as "user".
#[test]
fn test_request_content_attributes_tool_turn_role() {
    let _env = capture_content(&[]);
    let req = LlmRequest {
        contents: vec![
            content("user", vec![text_part("weather?")]),
            content("model", vec![function_call_part("get_weather")]),
            content(
                "user",
                vec![function_response_part(
                    "get_weather",
                    obj(json!({"temp": 12})),
                )],
            ),
        ],
        ..Default::default()
    };
    let encoded = attr_json(
        &span_content_attributes(&request_content(&req)),
        GEN_AI_INPUT_MESSAGES,
    )
    .expect("gen_ai.input.messages was not set");
    let msgs = validate_messages(&encoded, false);
    let roles: Vec<&str> = msgs
        .iter()
        .map(|m| m["role"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(roles, ["user", "assistant", "tool"]);
}

#[test]
fn test_request_content_attributes_system_instructions() {
    let _env = capture_content(&[]);
    let req = LlmRequest {
        config: Some(GenerateContentConfig {
            system_instruction: Some(content(
                "",
                vec![
                    text_part("You are a translator."),
                    text_part("Translate English to French."),
                ],
            )),
            ..Default::default()
        }),
        ..Default::default()
    };
    let attrs = span_content_attributes(&request_content(&req));
    let got = attr_json(&attrs, GEN_AI_SYSTEM_INSTRUCTIONS)
        .unwrap_or_else(|| panic!("gen_ai.system_instructions was not set; got {attrs:?}"));
    validate_parts(&got);
    let want = r#"[{"type":"text","content":"You are a translator."},{"type":"text","content":"Translate English to French."}]"#;
    assert_json_eq(want, &got, "gen_ai.system_instructions");
    assert!(
        attr_json(&attrs, GEN_AI_INPUT_MESSAGES).is_none(),
        "gen_ai.input.messages must not be set when there are no contents"
    );
}

// --- Output messages ---------------------------------------------------

#[test]
fn test_response_content_attributes() {
    let _env = capture_content(&[]);
    let resp = LlmResponse {
        content: Some(content("model", vec![text_part("It is rainy.")])),
        finish_reason: Some(FinishReason::Stop),
        ..Default::default()
    };
    let got = attr_json(
        &span_content_attributes(&response_content(&result_of(resp))),
        GEN_AI_OUTPUT_MESSAGES,
    )
    .expect("gen_ai.output.messages was not set");
    validate_messages(&got, true);
    let want = r#"[{"role":"assistant","parts":[{"type":"text","content":"It is rainy."}],"finish_reason":"stop"}]"#;
    assert_json_eq(want, &got, "gen_ai.output.messages");
}

/// A partial chunk records nothing under the legacy schema, which records the final response
/// alone.
#[test]
fn test_response_content_attributes_legacy_skips_streaming_chunks() {
    let _env = capture_content(&[(SCHEMA_VERSION_OPT_IN_ENV_VAR, OTEL_SEMCONV_1_36)]);
    let partial = LlmResponse {
        content: Some(content("model", vec![text_part("It is ra")])),
        partial: true,
        ..Default::default()
    };
    let attrs = span_content_attributes(&response_content(&result_of(partial)));
    assert!(
        attrs.is_empty(),
        "attributes for a partial chunk = {attrs:?}, want none"
    );
    let settled = LlmResponse {
        content: Some(content("model", vec![text_part("It is rainy.")])),
        finish_reason: Some(FinishReason::Stop),
        ..Default::default()
    };
    let got = attr_json(
        &span_content_attributes(&response_content(&result_of(settled))),
        GEN_AI_OUTPUT_MESSAGES,
    )
    .expect("gen_ai.output.messages was not set for the settled response");
    assert!(
        got.contains("It is rainy."),
        "settled response was not recorded: {got}"
    );
}

/// A streamed turn is every chunk, each finish_reason as the model reported it.
#[test]
fn test_response_content_attributes_one_message_per_response() {
    let _env = capture_content(&[]);
    let partial = Arc::new(LlmResponse {
        content: Some(content("model", vec![text_part("It is ")])),
        partial: true,
        ..Default::default()
    });
    let last = Arc::new(LlmResponse {
        content: Some(content("model", vec![text_part("rainy.")])),
        finish_reason: Some(FinishReason::Stop),
        partial: true,
        ..Default::default()
    });
    let result = GenerateContentResult {
        response: Some(last.clone()),
        responses: vec![partial, last],
        ..Default::default()
    };
    let got = attr_json(
        &span_content_attributes(&response_content(&result)),
        GEN_AI_OUTPUT_MESSAGES,
    )
    .expect("gen_ai.output.messages was not set");
    let want = r#"[{"role":"assistant","parts":[{"type":"text","content":"It is "}],"finish_reason":""},{"role":"assistant","parts":[{"type":"text","content":"rainy."}],"finish_reason":"stop"}]"#;
    assert_json_eq(want, &got, "gen_ai.output.messages");
}

/// A response blocked before any content was produced: finish_reason is required, so a message
/// is still emitted.
#[test]
fn test_response_content_attributes_empty_content() {
    let _env = capture_content(&[]);
    let resp = LlmResponse {
        finish_reason: Some(FinishReason::Safety),
        ..Default::default()
    };
    let got = attr_json(
        &span_content_attributes(&response_content(&result_of(resp))),
        GEN_AI_OUTPUT_MESSAGES,
    )
    .expect("gen_ai.output.messages was not set");
    validate_messages(&got, true);
    let want = r#"[{"role":"assistant","parts":[],"finish_reason":"content_filter"}]"#;
    assert_json_eq(want, &got, "gen_ai.output.messages");
}

fn finish(fr: FinishReason) -> LlmResponse {
    LlmResponse {
        finish_reason: Some(fr),
        ..Default::default()
    }
}

fn with_call(fr: Option<FinishReason>) -> LlmResponse {
    LlmResponse {
        content: Some(content("", vec![function_call_part("")])),
        finish_reason: fr,
        ..Default::default()
    }
}

// parity: adapted: Go passes the call's error; schema_finish_reason takes whether the call failed
#[rstest]
#[case::stop(finish(FinishReason::Stop), false, "stop")]
#[case::stop_with_a_tool_call(with_call(Some(FinishReason::Stop)), false, "tool_call")]
#[case::unset(LlmResponse::default(), false, "stop")]
#[case::unset_with_a_tool_call(with_call(None), false, "tool_call")]
#[case::unspecified(finish(FinishReason::Unspecified), false, "stop")]
#[case::max_tokens(finish(FinishReason::MaxTokens), false, "length")]
#[case::safety(finish(FinishReason::Safety), false, "content_filter")]
#[case::recitation(finish(FinishReason::Recitation), false, "content_filter")]
#[case::blocklist(finish(FinishReason::Blocklist), false, "content_filter")]
#[case::spii(finish(FinishReason::Spii), false, "content_filter")]
#[case::malformed_function_call(finish(FinishReason::MalformedFunctionCall), false, "error")]
#[case::too_many_tool_calls(finish(FinishReason::TooManyToolCalls), false, "error")]
#[case::other(finish(FinishReason::Other), false, "error")]
#[case::error_code_beats_a_successful_finish_reason(
    LlmResponse { error_code: "429".into(), ..finish(FinishReason::Stop) },
    false,
    "error"
)]
#[case::interruption_beats_a_successful_finish_reason(
    LlmResponse { interrupted: true, ..finish(FinishReason::Stop) },
    false,
    "error"
)]
#[case::error_code_beats_a_tool_call(
    LlmResponse { error_code: "500".into(), ..with_call(None) },
    false,
    "error"
)]
#[case::error_beats_a_successful_finish_reason(finish(FinishReason::Stop), true, "error")]
fn test_schema_finish_reason(#[case] resp: LlmResponse, #[case] err: bool, #[case] want: &str) {
    assert_eq!(schema_finish_reason(Some(&resp), err), want);
}

/// No known genai finish reason reaches telemetry as a protobuf wire name, and the result is
/// always a value the schema defines.
#[test]
fn test_schema_finish_reason_every_genai_value() {
    use FinishReason as F;
    for fr in [
        F::Unspecified,
        F::Stop,
        F::MaxTokens,
        F::Safety,
        F::Recitation,
        F::Language,
        F::Other,
        F::Blocklist,
        F::ProhibitedContent,
        F::Spii,
        F::MalformedFunctionCall,
        F::ImageSafety,
        F::UnexpectedToolCall,
        F::TooManyToolCalls,
        F::ImageProhibitedContent,
        F::NoImage,
        F::ImageRecitation,
        F::ImageOther,
    ] {
        let got = schema_finish_reason(Some(&finish(fr.clone())), false);
        assert!(
            SCHEMA_FINISH_REASONS.contains(&got.as_ref()),
            "genai {fr} maps to {got:?}, which the schema does not define"
        );
    }
    // An unknown future value is lowercased rather than passed through.
    assert_eq!(
        schema_finish_reason(Some(&finish(F::Unknown("BRAND_NEW_REASON".into()))), false),
        "brand_new_reason"
    );
}

// --- End to end through the span ---------------------------------------

/// The one span named `otel_name`.
fn only<'a>(spans: &'a [CapturedSpan], otel_name: &str) -> &'a CapturedSpan {
    let found: Vec<_> = spans.iter().filter(|s| s.otel_name == otel_name).collect();
    assert_eq!(found.len(), 1, "spans named {otel_name}: {spans:#?}");
    found[0]
}

fn params(model: &str, invocation_id: &str) -> GenerateContentParams {
    GenerateContentParams {
        model_name: model.into(),
        invocation_id: invocation_id.into(),
        ..Default::default()
    }
}

/// The real span helpers put exactly this JSON on the span.
#[tokio::test]
async fn test_generate_content_span_content_attributes() {
    let _env = capture_content(&[]);
    let req = LlmRequest {
        contents: vec![content("user", vec![text_part("hi")])],
        config: Some(GenerateContentConfig {
            system_instruction: Some(content("", vec![text_part("be brief")])),
            ..Default::default()
        }),
        ..Default::default()
    };
    let ((), spans) = capture_spans_in_env(async {
        let span = start_generate_content_span(&params("test-model", "inv-1"), &req);
        OperationDetails::new(span.clone()).note_llm_request(&req);
        trace_generate_content_result(
            &span,
            &result_of(LlmResponse {
                content: Some(content("model", vec![text_part("ok")])),
                finish_reason: Some(FinishReason::Stop),
                ..Default::default()
            }),
        );
    })
    .await;
    assert_eq!(spans.len(), 1, "{spans:#?}");
    let s = only(&spans, "generate_content test-model");
    for (k, v) in [
        (
            GEN_AI_SYSTEM_INSTRUCTIONS,
            r#"[{"type":"text","content":"be brief"}]"#,
        ),
        (
            GEN_AI_INPUT_MESSAGES,
            r#"[{"role":"user","parts":[{"type":"text","content":"hi"}]}]"#,
        ),
        (
            GEN_AI_OUTPUT_MESSAGES,
            r#"[{"role":"assistant","parts":[{"type":"text","content":"ok"}],"finish_reason":"stop"}]"#,
        ),
    ] {
        let got = span_attr_json(s, k).unwrap_or_else(|| panic!("attribute {k} was not set"));
        assert_json_eq(v, &got, k);
    }
}

/// The input is on the span even when the call fails, which is when it is most wanted.
#[tokio::test]
async fn test_generate_content_span_error_still_carries_the_prompt() {
    let _env = capture_content(&[]);
    let req = LlmRequest {
        contents: vec![content("user", vec![text_part("hi")])],
        ..Default::default()
    };
    let ((), spans) = capture_spans_in_env(async {
        let span = start_generate_content_span(&params("test-model", ""), &req);
        OperationDetails::new(span.clone()).note_llm_request(&req);
        trace_generate_content_result(
            &span,
            &GenerateContentResult {
                error: Some(SpanError::text("test error")),
                ..Default::default()
            },
        );
    })
    .await;
    assert_eq!(spans.len(), 1, "{spans:#?}");
    assert!(
        spans[0].field(GEN_AI_INPUT_MESSAGES).is_some(),
        "gen_ai.input.messages is missing from a failed generate_content span"
    );
    assert!(
        spans[0].field(GEN_AI_OUTPUT_MESSAGES).is_none(),
        "gen_ai.output.messages was set for a call that produced no response"
    );
}

// parity: adapted: start_generate_content_span takes the request by reference, so Go's nil request is the empty request
#[tokio::test]
async fn test_generate_content_span_nil_request() {
    let _env = capture_content(&[]);
    let ((), spans) = capture_spans_in_env(async {
        let _span = start_generate_content_span(&params("test-model", ""), &LlmRequest::default());
    })
    .await;
    assert_eq!(spans.len(), 1, "{spans:#?}");
    assert!(
        spans[0].field(GEN_AI_INPUT_MESSAGES).is_none(),
        "gen_ai.input.messages was set for a nil request"
    );
}

/// Go `startAttrRecorder`: the fields a span carries at creation (`on_new_span`, where a
/// subscriber decides whether it wants the span), before anything is recorded on it later.
#[derive(Clone, Default)]
struct StartAttrRecorder(Arc<Mutex<BTreeMap<String, String>>>);

struct StartFields<'a>(&'a mut BTreeMap<String, String>);

impl Visit for StartFields<'_> {
    fn record_str(&mut self, f: &Field, v: &str) {
        self.0.insert(f.name().to_owned(), v.to_owned());
    }
    fn record_bytes(&mut self, f: &Field, v: &[u8]) {
        self.0
            .insert(f.name().to_owned(), String::from_utf8_lossy(v).into_owned());
    }
    fn record_debug(&mut self, f: &Field, v: &dyn std::fmt::Debug) {
        self.0.insert(f.name().to_owned(), format!("{v:?}"));
    }
}

impl<S: tracing::Subscriber> Layer<S> for StartAttrRecorder {
    fn on_new_span(&self, attrs: &Attributes<'_>, _id: &Id, _ctx: LayerContext<'_, S>) {
        let mut at_start = BTreeMap::new();
        attrs.record(&mut StartFields(&mut at_start));
        *self.0.lock() = at_start;
    }
}

/// Runs `f` under a subscriber holding only a [`StartAttrRecorder`] and returns what the last
/// span carried at creation.
fn record_start_attrs(f: impl FnOnce()) -> BTreeMap<String, String> {
    pin_tracing_interest();
    let rec = StartAttrRecorder::default();
    let sub = tracing_subscriber::registry().with(rec.clone());
    tracing::subscriber::with_default(sub, f);
    rec.0.lock().clone()
}

// parity: adapted: tracing has no sampler; the subscriber's decision is taken when the span is created, so the fields a span carries at creation (on_new_span) stand in for OnStart's attributes
#[test]
fn test_start_generate_content_span_converts_after_the_sampling_decision() {
    let _env = capture_content(&[]);
    let req = LlmRequest {
        contents: vec![Content::from_text("a question worth capturing", "user")],
        ..Default::default()
    };
    let at_start = record_start_attrs(|| {
        let _span = start_generate_content_span(&params("mock", "inv-1"), &req);
    });
    assert!(
        !at_start.contains_key(GEN_AI_INPUT_MESSAGES),
        "content was converted before the sampler could decide"
    );
    // Sanity: the span-creation attributes are there, so the recorder works.
    assert!(
        at_start.contains_key(GEN_AI_REQUEST_MODEL),
        "recorder saw no creation attributes at all: {at_start:?}"
    );
}

// parity: adapted: as TestStartGenerateContentSpan_ConvertsAfterTheSamplingDecision (creation fields stand in for OnStart's attributes)
#[test]
fn test_start_execute_tool_span_arguments_after_the_sampling_decision() {
    let _env = capture_content(&[(SCHEMA_VERSION_OPT_IN_ENV_VAR, "otel_semconv_1_44")]);
    let at_start = record_start_attrs(|| {
        let _span = start_execute_tool_span("t", &obj(json!({"a": 1})));
    });
    assert!(
        !at_start.contains_key(GEN_AI_TOOL_CALL_ARGUMENTS),
        "tool arguments were converted before the sampler could decide"
    );
    assert!(
        at_start.contains_key(GEN_AI_TOOL_NAME),
        "recorder saw no creation attributes at all: {at_start:?}"
    );
}

/// Collects the messages of the events emitted while it is the default subscriber (Go's
/// `log.SetOutput`).
#[derive(Clone, Default)]
struct LogLines(Arc<Mutex<String>>);

impl<S: tracing::Subscriber> Layer<S> for LogLines {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: LayerContext<'_, S>) {
        let mut fields = BTreeMap::new();
        event.record(&mut StartFields(&mut fields));
        if let Some(m) = fields.get("message") {
            let mut out = self.0.lock();
            out.push_str(m);
            out.push('\n');
        }
    }
}

// parity: adapted: Go's warning goes to the standard log (otel.Handle); gadk's is a tracing WARN event, captured by a test subscriber
#[rstest]
#[case::otel_semconv_1_36(OTEL_SEMCONV_1_36)]
#[case::otel_semconv_1_44("otel_semconv_1_44")]
fn test_oversized_attribute_is_dropped(#[case] schema: &str) {
    let _env = capture_content(&[(SCHEMA_VERSION_OPT_IN_ENV_VAR, schema)]);
    let contents: Vec<Content> = (0..200)
        .map(|_| content("user", vec![text_part(&"x".repeat(1000))]))
        .collect();

    pin_tracing_interest();
    let logged = LogLines::default();
    let sub = tracing_subscriber::registry().with(logged.clone());
    let attrs = tracing::subscriber::with_default(sub, || {
        span_content_attributes(&request_content(&LlmRequest {
            contents,
            ..Default::default()
        }))
    });
    let logged = logged.0.lock().clone();
    if let Some(got) = attr_json(&attrs, GEN_AI_INPUT_MESSAGES) {
        panic!(
            "an attribute of {} bytes was recorded, want it dropped",
            got.len()
        );
    }
    assert!(
        logged.contains(&format!("leaving {GEN_AI_INPUT_MESSAGES} off the span")),
        "dropping the attribute logged {logged:?}, want a warning naming it"
    );
    assert!(!logged.contains("xxx"), "the warning holds message content");

    let attrs = span_content_attributes(&request_content(&LlmRequest {
        contents: vec![content("user", vec![text_part("hello")])],
        ..Default::default()
    }));
    let got =
        attr_json(&attrs, GEN_AI_INPUT_MESSAGES).expect("an ordinary conversation was dropped");
    assert!(
        got.len() <= CLOUD_TRACE_ATTRIBUTE_VALUE_LIMIT,
        "attribute is {} bytes, over the {CLOUD_TRACE_ATTRIBUTE_VALUE_LIMIT} the backend accepts",
        got.len()
    );
}

/// Inline data and file references, which the reporter of adk-go #608 was getting in v0.4.0
/// and would otherwise lose.
#[test]
fn test_media_parts() {
    let _env = capture_content(&[]);
    let small = b"pretend this is a png";
    let req = LlmRequest {
        contents: vec![content(
            "user",
            vec![
                Part {
                    inline_data: Some(Blob {
                        mime_type: "image/png".into(),
                        data: small.to_vec().into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                Part {
                    file_data: Some(FileData {
                        mime_type: "VIDEO/MP4".into(),
                        file_uri: "gs://bucket/clip.mp4".into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            ],
        )],
        ..Default::default()
    };
    let got = attr_json(
        &span_content_attributes(&request_content(&req)),
        GEN_AI_INPUT_MESSAGES,
    )
    .expect("gen_ai.input.messages was not set");
    // base64.StdEncoding of `small`.
    let want = r#"[{"role":"user","parts":[{"type":"blob","mime_type":"image/png","modality":"image","content":"cHJldGVuZCB0aGlzIGlzIGEgcG5n"},{"type":"uri","mime_type":"VIDEO/MP4","modality":"video","uri":"gs://bucket/clip.mp4"}]}]"#;
    assert_json_eq(want, &got, "gen_ai.input.messages");
}

/// One image must not consume the whole attribute and leave the conversation around it
/// unrecorded.
#[test]
fn test_inline_data_is_capped_per_payload() {
    let _env = capture_content(&[]);
    let big: Vec<u8> = vec![0u8; MAX_INLINE_DATA_BYTES * 4];
    let req = LlmRequest {
        contents: vec![content(
            "user",
            vec![
                Part {
                    inline_data: Some(Blob {
                        mime_type: "audio/wav".into(),
                        data: big.clone().into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                text_part("what is in this recording?"),
            ],
        )],
        ..Default::default()
    };
    let got = attr_json(
        &span_content_attributes(&request_content(&req)),
        GEN_AI_INPUT_MESSAGES,
    )
    .expect("a turn with one large payload dropped the whole attribute");
    // The base64 of `big` is all `A`s; the recorded payload holds a quarter of them.
    let full = "A".repeat(big.len() / 3 * 4);
    assert!(!got.contains(&full), "the whole payload was recorded");
    assert!(
        got.contains(r#""truncated":true"#),
        "truncation not marked: {:.200}",
        got
    );
    // The text alongside it still has to survive; that is the point of the cap.
    assert!(
        got.contains("what is in this recording?"),
        "the rest of the turn was lost: {:.200}",
        got
    );
}

/// RFC 2045 section 5.1: a MIME type has no canonical case, and modality is required on blob and
/// uri parts.
#[test]
fn test_modality_is_case_insensitive() {
    for mt in ["image/png", "IMAGE/PNG", "Image/Png"] {
        assert_eq!(modality_of(mt), "image", "modality_of({mt:?})");
    }
    assert_eq!(modality_of("application/pdf"), "document");
}

/// Which signals each value of the environment variable turns on. A truthy value keeps meaning
/// log records only: it was a boolean before spans could carry content.
#[rstest]
#[case::empty("", false, false)]
#[case::false_("false", false, false)]
#[case::zero("0", false, false)]
#[case::nonsense("nonsense", false, false)]
#[case::true_("true", true, false)]
#[case::one("1", true, false)]
#[case::padded_upper_true(" TRUE ", true, false)]
#[case::event_only("EVENT_ONLY", true, false)]
#[case::span_only("SPAN_ONLY", false, true)]
#[case::span_and_event("SPAN_AND_EVENT", true, true)]
#[case::span_and_event_lower("span_and_event", true, true)]
fn test_content_capture_modes(#[case] env: &str, #[case] in_logs: bool, #[case] on_span: bool) {
    let _env = test_env(&[(CAPTURE_MESSAGE_CONTENT_ENV_VAR, env)]);
    assert_eq!(capture_message_content(), in_logs, "content in log records");
    assert_eq!(capture_content_on_spans(), on_span, "content on spans");
}

/// Every agent deployed through the CLI has this variable set to "true"; span capture behind
/// that value would ship full conversations to a tracing backend nobody opted into.
#[tokio::test]
async fn test_truthy_value_does_not_put_content_on_spans() {
    let _env = test_env(&[
        (CAPTURE_MESSAGE_CONTENT_ENV_VAR, "true"),
        (SCHEMA_VERSION_OPT_IN_ENV_VAR, "otel_semconv_1_44"),
    ]);
    let req = LlmRequest {
        contents: vec![content("user", vec![text_part("canary")])],
        ..Default::default()
    };
    let ((), spans) = capture_spans_in_env(async {
        let span = start_generate_content_span(&params("m", ""), &req);
        OperationDetails::new(span.clone()).note_llm_request(&req);
        trace_generate_content_result(
            &span,
            &result_of(LlmResponse {
                content: Some(content("", vec![text_part("canary")])),
                ..Default::default()
            }),
        );
        drop(span);
        let span = start_execute_tool_span("t", &obj(json!({"a": "canary"})));
        let mut ev = Event::default();
        ev.response.content = Some(content(
            "",
            vec![function_response_part("", obj(json!({"r": "canary"})))],
        ));
        trace_tool_result(&span, "", Some(&ev), None);
    })
    .await;
    assert_eq!(spans.len(), 2, "{spans:#?}");
    for s in &spans {
        for (k, v) in &s.fields {
            assert!(
                !v.contains("canary"),
                "a truthy value put {k} on span {:?}",
                s.otel_name
            );
        }
    }
}

/// Event content has no size cap, and tool argument numbers keep their type.
#[test]
fn test_event_content_attributes() {
    let long = "x".repeat(CLOUD_TRACE_ATTRIBUTE_VALUE_LIMIT);
    let req = LlmRequest {
        contents: vec![
            content("user", vec![text_part(&long)]),
            Content::from_function_call(
                FunctionCall {
                    name: "f".into(),
                    args: obj(json!({"n": 3, "f": 1.5})),
                    ..Default::default()
                },
                "model",
            ),
        ],
        ..Default::default()
    };
    let attrs = event_content_attributes(&request_content(&req));
    assert_eq!(attrs.len(), 1, "attributes = {attrs:?}");
    assert_eq!(attrs[0].0, GEN_AI_INPUT_MESSAGES);
    let AttrValue::Slice(msgs) = &attrs[0].1 else {
        panic!("gen_ai.input.messages is not a slice: {:?}", attrs[0].1);
    };
    let text = &from_log_value(&msgs[0])["parts"][0]["content"];
    assert!(
        text.as_str() == Some(long.as_str()),
        "the long message was not recorded in full"
    );
    // The argument values keep Go's INT64 / FLOAT64 types.
    let arguments = |v: &AttrValue| -> Option<Vec<(String, AttrValue)>> {
        let AttrValue::Map(msg) = v else { return None };
        let (_, AttrValue::Slice(parts)) = msg.iter().find(|(k, _)| k == "parts")? else {
            return None;
        };
        let AttrValue::Map(part) = parts.first()? else {
            return None;
        };
        let (_, AttrValue::Map(args)) = part.iter().find(|(k, _)| k == "arguments")? else {
            return None;
        };
        let mut args = args.clone();
        args.sort_by(|a, b| a.0.cmp(&b.0));
        Some(args)
    };
    assert_eq!(
        arguments(&msgs[1]),
        Some(vec![
            ("f".to_owned(), AttrValue::Float64(1.5)),
            ("n".to_owned(), AttrValue::Int64(3)),
        ]),
        "tool arguments mismatch"
    );
}

/// Go `filled[textPart]()`: every string field holds its Go field name.
fn filled_text() -> PartValue {
    PartValue::Text {
        kind: "Type",
        content: "Content".into(),
    }
}

/// Go `filled[chatMessage]()`.
fn filled_message() -> ChatMessage {
    ChatMessage {
        role: "Role".into(),
        parts: vec![filled_text()],
        finish_reason: Some("FinishReason".into()),
    }
}

fn parts(p: PartValue) -> ContentValue {
    ContentValue::Parts(vec![p])
}

fn msg(role: &str, parts: Vec<PartValue>, finish_reason: Option<&str>) -> ChatMessage {
    ChatMessage {
        role: role.into(),
        parts,
        finish_reason: finish_reason.map(str::to_owned),
    }
}

// parity: adapted: Go's contentValue interface includes the individual part types; gadk's ContentValue holds part and message lists, so a single part or message is checked as a one-element list. Go's reflective filled[T] is written out per type; the invalid UTF-8 case holds U+FFFD in place of the invalid bytes (a Rust String cannot hold invalid UTF-8; Go's str makes the same replacement); Go's "unserializable payload" case has no counterpart, as a serde_json value always encodes; a tool call "without arguments" carries Go's null payload, as toolPayload(nil) does
#[rstest]
#[case::text_filled(parts(filled_text()))]
#[case::tool_call_filled(parts(PartValue::ToolCall {
    kind: "Type", id: "ID".into(), name: "Name".into(), arguments: json!({"k": 1}),
}))]
#[case::blob_filled(parts(PartValue::Blob {
    kind: "Type", mime_type: "MIMEType".into(), modality: "Modality",
    content: "Content".into(), truncated: true,
}))]
#[case::uri_filled(parts(PartValue::Uri {
    kind: "Type", mime_type: "MIMEType".into(), modality: "Modality", uri: "URI".into(),
}))]
#[case::tool_response_filled(parts(PartValue::ToolResponse {
    kind: "Type", id: "ID".into(), name: "Name".into(), response: json!({"k": 1}),
}))]
#[case::message_filled(ContentValue::Messages(vec![filled_message()]))]
#[case::invalid_utf_8(parts(PartValue::Text {
    kind: "text",
    content: String::from_utf8_lossy(b"a\xff\xfeb\xe2\x82").into_owned(),
}))]
#[case::text(parts(PartValue::Text { kind: "text", content: "hi".into() }))]
#[case::tool_call(parts(PartValue::ToolCall {
    kind: "tool_call", id: "1".into(), name: "f".into(),
    arguments: tool_payload(&obj(json!({"n": 1, "x": 1.5, "s": ["a", true]}))),
}))]
#[case::tool_call_without_id_or_arguments(parts(PartValue::ToolCall {
    kind: "tool_call", id: String::new(), name: "f".into(), arguments: Value::Null,
}))]
#[case::tool_call_with_null_arguments(parts(PartValue::ToolCall {
    kind: "tool_call", id: String::new(), name: "f".into(), arguments: tool_payload(&Map::new()),
}))]
#[case::blob(parts(PartValue::Blob {
    kind: "blob", mime_type: "image/png".into(), modality: "image",
    content: "AA==".into(), truncated: true,
}))]
#[case::blob_without_mime_type(parts(PartValue::Blob {
    kind: "blob", mime_type: String::new(), modality: "image",
    content: String::new(), truncated: false,
}))]
#[case::uri(parts(PartValue::Uri {
    kind: "uri", mime_type: "video/mp4".into(), modality: "video", uri: "gs://b/o".into(),
}))]
#[case::uri_without_mime_type(parts(PartValue::Uri {
    kind: "uri", mime_type: String::new(), modality: "video", uri: "gs://b/o".into(),
}))]
#[case::tool_response(parts(PartValue::ToolResponse {
    kind: "tool_call_response", id: "1".into(), name: "f".into(),
    response: tool_payload(&obj(json!({"ok": true}))),
}))]
#[case::tool_response_without_id_or_name(parts(PartValue::ToolResponse {
    kind: "tool_call_response", id: String::new(), name: String::new(),
    response: tool_payload(&Map::new()),
}))]
#[case::messages(ContentValue::Messages(vec![
    msg("user", vec![PartValue::Text { kind: "text", content: "q".into() }], None),
    msg("assistant", vec![], Some("stop")),
    msg("assistant", vec![], Some("")),
]))]
fn test_content_value_matches_json(#[case] value: ContentValue) {
    let encoded = gadk_model::to_go_json(&value).expect("json encoding");
    let decoded: Value = serde_json::from_slice(&encoded).expect("valid JSON");
    let want = from_log_value(&ContentValue::Raw(decoded).value());
    assert_eq!(
        want,
        from_log_value(&value.value()),
        "value() differs from the JSON encoding (-json +value)"
    );
}
