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

//! Ports of TestFinishReasonAndErrorClassification, TestGenerateContentConfigValidation,
//! TestConcurrencyAndNonMutation, TestContextCancellation, TestTruncatedStreamedToolCall,
//! TestMaxRetriesAndInFlightNonStreamingCancellation and TestGenaiConfigReflectionCompleteness
//!.
//!
//! Every Go subtest and table case is a named case here; a failing case names itself in the
//! panic message. Error `Display` texts are asserted whole, byte for byte with Go's `Error()`
//! (Go's tests check `strings.Contains` on a field name; the whole text is a stronger form of
//! the same check).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use gadk_anthropic::{
    ClientConfig, ErrorKind, PromptCacheConfig, PromptCacheTtl, STATUS_OVERLOADED,
    STOP_REASON_METADATA_KEY,
};
use gadk_genai::{
    AudioTranscriptionConfig, ComputerUse, Content, EnterpriseWebSearch, FileSearch, FinishReason,
    FunctionCall, FunctionCallingConfig, FunctionDeclaration, FunctionResponse,
    GenerateContentConfig, GenerationConfigRoutingConfig, GoogleMaps, GoogleSearch,
    GoogleSearchRetrieval, ImageConfig, MCPServer, MediaResolution, ModelArmorConfig,
    ModelSelectionConfig, Part, ROLE_MODEL, ROLE_USER, Retrieval, RetrievalConfig, SafetySetting,
    Schema, ServiceTier, SpeechConfig, ThinkingConfig, ThinkingLevel, Tool, ToolCodeExecution,
    ToolConfig, ToolExaAISearch, ToolParallelAISearch, Type, URLContext,
};
use gadk_model::{HttpOptions, Llm, LlmRequest, LlmResponse};
use serde_json::json;

use crate::harness::{
    FakeResponse, FakeServer, MODEL, SAMPLE_PNG_1X1, anthropic_config, anthropic_error, generate,
    generate_err_in, generate_last_in, new_model, sse_event, sse_lines, text_request,
};

/// Go `ErrInvalidConfig.Error()`.
const INVALID_CONFIG: &str = "anthropicmodel: invalid configuration";

/// `GenerateContent(ctx, req, stream)` must yield exactly one item, an error; returns it.
async fn only_error(
    m: &dyn Llm,
    req: LlmRequest,
    stream: bool,
    case: &str,
) -> gadk_model::ModelError {
    let mut items = generate(m, req, stream).await;
    assert_eq!(
        items.len(),
        1,
        "{case}: want exactly one yielded item, got {items:?}"
    );
    match items.pop().unwrap() {
        Err(e) => e,
        Ok(resp) => panic!("{case}: want an error, got response {resp:?}"),
    }
}

// --------------------------------------------------------------------------------------------
// TestFinishReasonAndErrorClassification

struct StopReasonCase {
    stop_reason: &'static str,
    want_finish: FinishReason,
    want_meta: &'static str,
}

/// Go subtest `stop_reason_mapping_without_error_code`.
async fn stop_reason_mapping_without_error_code() {
    let cases = [
        StopReasonCase {
            stop_reason: "end_turn",
            want_finish: FinishReason::Stop,
            want_meta: "",
        },
        StopReasonCase {
            stop_reason: "stop_sequence",
            want_finish: FinishReason::Stop,
            want_meta: "",
        },
        StopReasonCase {
            stop_reason: "tool_use",
            want_finish: FinishReason::Stop,
            want_meta: "",
        },
        StopReasonCase {
            stop_reason: "max_tokens",
            want_finish: FinishReason::MaxTokens,
            want_meta: "",
        },
        StopReasonCase {
            stop_reason: "model_context_window_exceeded",
            want_finish: FinishReason::MaxTokens,
            want_meta: "model_context_window_exceeded",
        },
        StopReasonCase {
            stop_reason: "refusal",
            want_finish: FinishReason::Safety,
            want_meta: "refusal",
        },
        StopReasonCase {
            stop_reason: "pause_turn",
            want_finish: FinishReason::Stop,
            want_meta: "pause_turn",
        },
        StopReasonCase {
            stop_reason: "future_unknown_reason",
            want_finish: FinishReason::Other,
            want_meta: "future_unknown_reason",
        },
    ];

    for tc in cases {
        let case = format!("stop_reason_mapping_without_error_code/{}", tc.stop_reason);
        let srv = FakeServer::json(format!(
            r#"{{"id":"msg_1","type":"message","role":"assistant","model":"claude-sonnet-4-5-20250929","stop_reason":"{}","content":[{{"type":"text","text":"content present"}}],"usage":{{"input_tokens":5,"output_tokens":2}}}}"#,
            tc.stop_reason
        ));
        let m = new_model(MODEL, anthropic_config(&srv.url())).await;

        let got = generate_last_in(&case, m.as_ref(), text_request("hi"), false).await;
        assert_eq!(
            got.finish_reason,
            Some(tc.want_finish),
            "{case}: FinishReason"
        );
        assert!(
            got.error_code.is_empty() && got.error_message.is_empty(),
            "{case}: ErrorCode={:?} ErrorMessage={:?} must not be set on content-bearing response",
            got.error_code,
            got.error_message
        );
        let got_meta = got
            .custom_metadata
            .get(STOP_REASON_METADATA_KEY)
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(
            got_meta, tc.want_meta,
            "{case}: CustomMetadata[{STOP_REASON_METADATA_KEY:?}]"
        );
    }
}

struct ErrorCase {
    name: &'static str,
    status: u16,
    sse: bool,
    body: String,
    want_sentinel: ErrorKind,
    want_code: u16,
    want_type: &'static str,
    /// Go `APIError.Error()` (not asserted by the Go test; the whole Go text).
    want_text: String,
}

/// Go subtest `http_status_and_midstream_sse_errors`.
async fn http_status_and_midstream_sse_errors() {
    let message_start = r#"{"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"claude-sonnet-4-5-20250929","content":[],"usage":{"input_tokens":5,"output_tokens":1}}}"#;
    let long_message = format!("a{}", "é".repeat(200));
    let cases = vec![
        ErrorCase {
            name: "http_429_rate_limit",
            status: 429,
            sse: false,
            body: r#"{"type":"error","request_id":"req_429","error":{"type":"rate_limit_error","message":"Rate limit exceeded"}}"#.to_string(),
            want_sentinel: ErrorKind::RateLimited,
            want_code: 429,
            want_type: "rate_limit_error",
            want_text: "anthropicmodel: api error (status 429) [type=rate_limit_error] [request_id=req_hdr]: Rate limit exceeded".to_string(),
        },
        ErrorCase {
            name: "http_529_overloaded",
            status: STATUS_OVERLOADED,
            sse: false,
            body: r#"{"type":"error","request_id":"req_529","error":{"type":"overloaded_error","message":"Overloaded"}}"#.to_string(),
            want_sentinel: ErrorKind::Overloaded,
            want_code: STATUS_OVERLOADED,
            want_type: "overloaded_error",
            want_text: "anthropicmodel: api error (status 529) [type=overloaded_error] [request_id=req_hdr]: Overloaded".to_string(),
        },
        ErrorCase {
            name: "http_401_auth",
            status: 401,
            sse: false,
            body: r#"{"type":"error","request_id":"req_401","error":{"type":"authentication_error","message":"Invalid key sk-ant-secret"}}"#.to_string(),
            want_sentinel: ErrorKind::Authentication,
            want_code: 401,
            want_type: "authentication_error",
            want_text: "anthropicmodel: api error (status 401) [type=authentication_error] [request_id=req_hdr]: api request rejected".to_string(),
        },
        ErrorCase {
            name: "http_400_bad_request",
            status: 400,
            sse: false,
            body: r#"{"type":"error","request_id":"req_400","error":{"type":"invalid_request_error","message":"thinking.budget_tokens: Input should be >= 1024"}}"#.to_string(),
            want_sentinel: ErrorKind::BadRequest,
            want_code: 400,
            want_type: "invalid_request_error",
            want_text: "anthropicmodel: api error (status 400) [type=invalid_request_error] [request_id=req_hdr]: thinking.budget_tokens: Input should be >= 1024".to_string(),
        },
        ErrorCase {
            name: "midstream_sse_overloaded",
            status: 200,
            sse: true,
            body: format!(
                "{}{}",
                sse_event("message_start", message_start),
                sse_event(
                    "error",
                    r#"{"type":"error","request_id":"req_sse_529","error":{"type":"overloaded_error","message":"Overloaded mid-stream"}}"#
                )
            ),
            want_sentinel: ErrorKind::Overloaded,
            want_code: STATUS_OVERLOADED,
            want_type: "overloaded_error",
            want_text: "anthropicmodel: api error (status 529) [type=overloaded_error] [request_id=req_sse_529]: Overloaded mid-stream".to_string(),
        },
        ErrorCase {
            name: "midstream_sse_rate_limit",
            status: 200,
            sse: true,
            body: sse_event(
                "error",
                r#"{"type":"error","request_id":"req_sse_429","error":{"type":"rate_limit_error","message":"Rate limited mid-stream"}}"#,
            ),
            want_sentinel: ErrorKind::RateLimited,
            want_code: 429,
            want_type: "rate_limit_error",
            want_text: "anthropicmodel: api error (status 429) [type=rate_limit_error] [request_id=req_sse_429]: Rate limited mid-stream".to_string(),
        },
        ErrorCase {
            name: "midstream_sse_api_error_maps_to_500_not_200",
            status: 200,
            sse: true,
            body: sse_event(
                "error",
                r#"{"type":"error","request_id":"req_sse_500","error":{"type":"api_error","message":"Internal server error mid-stream"}}"#,
            ),
            want_sentinel: ErrorKind::ApiStatus,
            want_code: 500,
            want_type: "api_error",
            want_text: "anthropicmodel: api error (status 500) [type=api_error] [request_id=req_sse_500]: Internal server error mid-stream".to_string(),
        },
        ErrorCase {
            name: "multibyte_utf8_message_truncation_preserves_valid_utf8",
            status: 500,
            sse: false,
            body: format!(
                r#"{{"type":"error","request_id":"req_utf8","error":{{"type":"api_error","message":{}}}}}"#,
                serde_json::to_string(&long_message).unwrap()
            ),
            want_sentinel: ErrorKind::ApiStatus,
            want_code: 500,
            want_type: "api_error",
            // Go `strings.ToValidUTF8(msg[:256], "")`: 1 + 127·2 = 255 bytes, the split 128th
            // "é" is dropped.
            want_text: format!(
                "anthropicmodel: api error (status 500) [type=api_error] [request_id=req_hdr]: a{}",
                "é".repeat(127)
            ),
        },
    ];

    for tc in cases {
        let case = format!("http_status_and_midstream_sse_errors/{}", tc.name);
        let resp = if tc.sse {
            FakeResponse::new(tc.status)
                .header("Content-Type", "text/event-stream")
                .chunk(tc.body.clone())
        } else {
            FakeResponse::new(tc.status)
                .header("Content-Type", "application/json")
                .header("Request-Id", "req_hdr")
                .chunk(tc.body.clone())
        };
        let srv = FakeServer::always(resp);
        let m = new_model(
            MODEL,
            ClientConfig {
                api_key: Some("sk-ant-secret".to_string()),
                max_retries: Some(0),
                ..anthropic_config(&srv.url())
            },
        )
        .await;

        let got_err = generate_err_in(
            &case,
            m.as_ref(),
            text_request("SECRET_PROMPT_TEXT"),
            tc.sse,
        )
        .await;
        let err = anthropic_error(&got_err);
        assert!(
            err.is(tc.want_sentinel),
            "{case}: GenerateContent() error = {got_err}, want errors.Is({})",
            tc.want_sentinel
        );
        let api_err = err
            .api_error()
            .unwrap_or_else(|| panic!("{case}: expected errors.As(*APIError), got {got_err:?}"));
        assert_eq!(api_err.status_code, tc.want_code, "{case}: StatusCode");
        assert_eq!(api_err.error_type, tc.want_type, "{case}: Type");
        assert_eq!(api_err.kind, tc.want_sentinel, "{case}: APIError.Err");
        // Go's `utf8.ValidString` checks hold by construction: Rust `String`s are valid UTF-8.
        let err_str = got_err.to_string();
        assert_eq!(err_str, tc.want_text, "{case}: Error()");
        assert_eq!(
            api_err.to_string(),
            tc.want_text,
            "{case}: APIError.Error()"
        );
        assert!(
            !err_str.contains("SECRET_PROMPT_TEXT")
                && !err_str.contains("sk-ant-secret")
                && !err_str.contains(&srv.url()),
            "{case}: error string {err_str:?} leaked prompt, key, or URL"
        );
    }
}

#[tokio::test]
async fn test_finish_reason_and_error_classification() {
    stop_reason_mapping_without_error_code().await;
    http_status_and_midstream_sse_errors().await;
}

// --------------------------------------------------------------------------------------------
// TestGenerateContentConfigValidation

struct RejectedCase {
    name: &'static str,
    /// The request; `None` when gadk cannot express the Go case (see `adapted`).
    req: Option<LlmRequest>,
    /// The whole Go error text (Go asserts `ErrInvalidConfig` and a substring naming the field).
    want_text: String,
    /// Why the case is not run (gadk has no such field), or `""`.
    adapted: &'static str,
}

fn cfg_request(cfg: GenerateContentConfig) -> LlmRequest {
    LlmRequest {
        config: Some(cfg),
        ..text_request("hi")
    }
}

/// Go `GenerateContentConfig{HTTPOptions: opts}`: gadk carries HTTP options on the request.
fn http_options_request(opts: HttpOptions) -> LlmRequest {
    LlmRequest {
        config: Some(GenerateContentConfig::default()),
        http_options: Some(opts),
        ..text_request("hi")
    }
}

fn rejected(name: &'static str, req: LlmRequest, detail: &str) -> RejectedCase {
    RejectedCase {
        name,
        req: Some(req),
        want_text: format!("{INVALID_CONFIG}: {detail}"),
        adapted: "",
    }
}

fn adapted(name: &'static str, detail: &str, why: &'static str) -> RejectedCase {
    RejectedCase {
        name,
        req: None,
        want_text: format!("{INVALID_CONFIG}: {detail}"),
        adapted: why,
    }
}

fn rejected_cases() -> Vec<RejectedCase> {
    let gcc = GenerateContentConfig::default;
    vec![
        rejected(
            "temperature_and_top_p_together",
            cfg_request(GenerateContentConfig {
                temperature: Some(0.5),
                top_p: Some(0.9),
                ..gcc()
            }),
            "Temperature and TopP cannot both be set on Claude models",
        ),
        rejected(
            "negative_top_k",
            cfg_request(GenerateContentConfig {
                top_k: Some(-1.0),
                ..gcc()
            }),
            "TopK must be non-negative",
        ),
        rejected(
            "candidate_count_gt_1",
            cfg_request(GenerateContentConfig {
                candidate_count: 2,
                ..gcc()
            }),
            "CandidateCount 2 is not supported; Claude only supports 1 candidate",
        ),
        rejected(
            "negative_max_output_tokens",
            cfg_request(GenerateContentConfig {
                max_output_tokens: -10,
                ..gcc()
            }),
            "MaxOutputTokens must be non-negative",
        ),
        rejected(
            "non_text_system_instruction",
            cfg_request(GenerateContentConfig {
                system_instruction: Some(Content {
                    parts: vec![Part::from_bytes(
                        Bytes::from_static(&SAMPLE_PNG_1X1),
                        "image/png",
                    )],
                    ..Default::default()
                }),
                ..gcc()
            }),
            "SystemInstruction only supports text parts",
        ),
        rejected(
            "non_text_response_modality",
            cfg_request(GenerateContentConfig {
                response_modalities: vec!["AUDIO".to_string()],
                ..gcc()
            }),
            "ResponseModalities only supports TEXT",
        ),
        rejected(
            "http_options_base_url",
            http_options_request(HttpOptions {
                base_url: Some("https://other.example.com".to_string()),
                ..Default::default()
            }),
            "HTTPOptions.BaseURL is not supported; set ClientConfig.BaseURL instead",
        ),
        // parity: adapted: gadk_model::HttpOptions has no base_url_resource_scope field.
        adapted(
            "http_options_base_url_resource_scope",
            "HTTPOptions.BaseURLResourceScope is not supported",
            "gadk_model::HttpOptions has no base_url_resource_scope field",
        ),
        rejected(
            "http_options_api_version",
            http_options_request(HttpOptions {
                api_version: Some("v1beta".to_string()),
                ..Default::default()
            }),
            "HTTPOptions.APIVersion is not supported",
        ),
        // parity: adapted: gadk_model::HttpOptions has no extra_body field.
        adapted(
            "http_options_extra_body",
            "HTTPOptions.ExtraBody is not supported",
            "gadk_model::HttpOptions has no extra_body field",
        ),
        rejected(
            "http_options_extras_request_provider",
            http_options_request(HttpOptions {
                extras_request_provider: Some(Arc::new(|b| b)),
                ..Default::default()
            }),
            "HTTPOptions.ExtrasRequestProvider is not supported",
        ),
        // parity: adapted: gadk_model::HttpOptions has no retry_options field (retries are
        // ClientConfig::max_retries).
        adapted(
            "http_options_retry_options",
            "HTTPOptions.RetryOptions is not supported; set ClientConfig.MaxRetries instead",
            "gadk_model::HttpOptions has no retry_options field",
        ),
        // parity: adapted: HttpOptions::timeout is a std::time::Duration, which cannot be
        // negative.
        adapted(
            "http_options_negative_timeout",
            "HTTPOptions.Timeout must be non-negative",
            "HttpOptions::timeout is an unsigned Duration",
        ),
        rejected(
            "response_mime_type_without_schema",
            cfg_request(GenerateContentConfig {
                response_mime_type: "application/json".to_string(),
                ..gcc()
            }),
            r#"ResponseMIMEType "application/json" requires ResponseSchema or ResponseJsonSchema"#,
        ),
        rejected(
            "response_mime_type_non_json_with_schema",
            cfg_request(GenerateContentConfig {
                response_mime_type: "text/plain".to_string(),
                response_schema: Some(Schema {
                    r#type: Some(Type::Object),
                    ..Default::default()
                }),
                ..gcc()
            }),
            r#"ResponseMIMEType "text/plain" cannot be combined with ResponseSchema or ResponseJsonSchema"#,
        ),
        rejected(
            "response_schema_and_response_json_schema_both_set",
            cfg_request(GenerateContentConfig {
                response_schema: Some(Schema {
                    r#type: Some(Type::Object),
                    ..Default::default()
                }),
                response_json_schema: Some(json!({"type": "object"})),
                ..gcc()
            }),
            "ResponseSchema and ResponseJsonSchema cannot both be set",
        ),
        rejected(
            "response_logprobs",
            cfg_request(GenerateContentConfig {
                response_logprobs: true,
                ..gcc()
            }),
            "ResponseLogprobs is not supported",
        ),
        rejected(
            "logprobs",
            cfg_request(GenerateContentConfig {
                logprobs: Some(1),
                ..gcc()
            }),
            "Logprobs is not supported",
        ),
        rejected(
            "presence_penalty",
            cfg_request(GenerateContentConfig {
                presence_penalty: Some(0.5),
                ..gcc()
            }),
            "PresencePenalty is not supported",
        ),
        rejected(
            "frequency_penalty",
            cfg_request(GenerateContentConfig {
                frequency_penalty: Some(0.5),
                ..gcc()
            }),
            "FrequencyPenalty is not supported",
        ),
        rejected(
            "seed",
            cfg_request(GenerateContentConfig {
                seed: Some(1),
                ..gcc()
            }),
            "Seed is not supported",
        ),
        rejected(
            "safety_settings",
            cfg_request(GenerateContentConfig {
                safety_settings: vec![SafetySetting::default()],
                ..gcc()
            }),
            "SafetySettings is not supported",
        ),
        rejected(
            "cached_content",
            cfg_request(GenerateContentConfig {
                cached_content: "caches/123".to_string(),
                ..gcc()
            }),
            "CachedContent is not supported; use ClientConfig.PromptCache instead",
        ),
        rejected(
            "labels",
            cfg_request(GenerateContentConfig {
                labels: BTreeMap::from([("env".to_string(), "prod".to_string())]),
                ..gcc()
            }),
            "Labels is not supported",
        ),
        rejected(
            "media_resolution",
            cfg_request(GenerateContentConfig {
                media_resolution: Some(MediaResolution::High),
                ..gcc()
            }),
            "MediaResolution is not supported",
        ),
        rejected(
            "speech_config",
            cfg_request(GenerateContentConfig {
                speech_config: Some(SpeechConfig::default()),
                ..gcc()
            }),
            "SpeechConfig is not supported",
        ),
        rejected(
            "audio_timestamp",
            cfg_request(GenerateContentConfig {
                audio_timestamp: true,
                ..gcc()
            }),
            "AudioTimestamp is not supported",
        ),
        rejected(
            "image_config",
            cfg_request(GenerateContentConfig {
                image_config: Some(ImageConfig::default()),
                ..gcc()
            }),
            "ImageConfig is not supported",
        ),
        rejected(
            "routing_config",
            cfg_request(GenerateContentConfig {
                routing_config: Some(GenerationConfigRoutingConfig::default()),
                ..gcc()
            }),
            "RoutingConfig is not supported",
        ),
        rejected(
            "model_selection_config",
            cfg_request(GenerateContentConfig {
                model_selection_config: Some(ModelSelectionConfig::default()),
                ..gcc()
            }),
            "ModelSelectionConfig is not supported",
        ),
        rejected(
            "model_armor_config",
            cfg_request(GenerateContentConfig {
                model_armor_config: Some(ModelArmorConfig::default()),
                ..gcc()
            }),
            "ModelArmorConfig is not supported",
        ),
        rejected(
            "enable_enhanced_civic_answers",
            cfg_request(GenerateContentConfig {
                enable_enhanced_civic_answers: Some(true),
                ..gcc()
            }),
            "EnableEnhancedCivicAnswers is not supported",
        ),
        rejected(
            "service_tier",
            cfg_request(GenerateContentConfig {
                service_tier: Some(ServiceTier::Flex),
                ..gcc()
            }),
            "ServiceTier is not supported",
        ),
        rejected(
            "audio_transcription_config",
            cfg_request(GenerateContentConfig {
                audio_transcription_config: Some(AudioTranscriptionConfig::default()),
                ..gcc()
            }),
            "AudioTranscriptionConfig is not supported",
        ),
        rejected(
            "continuation_token",
            cfg_request(GenerateContentConfig {
                continuation_token: Some(Bytes::from_static(b"tok")),
                ..gcc()
            }),
            "ContinuationToken is not supported",
        ),
    ]
}

// parity: adapted: gadk keeps HTTP options on LlmRequest.http_options (not inside the config) and has no BaseURLResourceScope, ExtraBody or RetryOptions field and no negative Timeout; those four cases are listed but not run
#[tokio::test]
async fn test_generate_content_config_validation() {
    let srv = FakeServer::json(
        r#"{"id":"msg_v","type":"message","role":"assistant","model":"claude-sonnet-4-5-20250929","stop_reason":"end_turn","content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":5,"output_tokens":1}}"#,
    );
    let m = new_model(MODEL, anthropic_config(&srv.url())).await;

    let cases = rejected_cases();
    assert_eq!(cases.len(), 35, "Go's rejectedCases has 35 entries");
    for tc in cases {
        let Some(req) = tc.req else {
            assert!(
                !tc.adapted.is_empty(),
                "{}: a case without a request needs a reason",
                tc.name
            );
            continue;
        };
        let err = only_error(m.as_ref(), req, false, tc.name).await;
        assert!(
            anthropic_error(&err).is(ErrorKind::InvalidConfig),
            "{}: error = {err}, want ErrInvalidConfig",
            tc.name
        );
        assert_eq!(err.to_string(), tc.want_text, "{}", tc.name);
    }
    assert_eq!(
        srv.hits(),
        0,
        "rejected configs must fail before any request is sent"
    );

    // Go subtest `ignored_fields_do_not_error_or_leak_headers`.
    let case = "ignored_fields_do_not_error_or_leak_headers";
    let mut headers = http::HeaderMap::new();
    headers.insert("X-Gemini-Only", "should-not-be-sent".parse().unwrap());
    let req = LlmRequest {
        config: Some(GenerateContentConfig {
            response_modalities: vec!["TEXT".to_string()],
            automatic_continuation: Some(true),
            ..Default::default()
        }),
        http_options: Some(HttpOptions {
            timeout: Some(Duration::from_secs(5)),
            headers,
            ..Default::default()
        }),
        ..text_request("hi")
    };
    for item in generate(m.as_ref(), req, false).await {
        if let Err(e) = item {
            panic!("{case}: GenerateContent() error = {e}");
        }
    }
    assert_eq!(srv.hits(), 1, "{case}: requests sent");
    let got_custom_header = srv.last_request().header("X-Gemini-Only");
    assert_eq!(
        got_custom_header, "",
        "{case}: HTTPOptions.Headers leaked X-Gemini-Only={got_custom_header:?} to server"
    );
}

// --------------------------------------------------------------------------------------------
// TestConcurrencyAndNonMutation

/// Go `json.Marshal(sharedReq)`, plus the request's `Debug` (which also lists HTTP options and
/// tool names, fields Go's JSON leaves out).
fn snapshot(req: &LlmRequest) -> String {
    let json = json!({
        "model": req.model,
        "contents": req.contents,
        "config": req.config,
    });
    format!("{json}\n{req:?}")
}

// parity: adapted: generate_content takes the LlmRequest by value, so each of the 16 concurrent workers sends a clone of one shared request (held in an Arc); the shared request must be unchanged afterwards and every worker's request body identical
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_concurrency_and_non_mutation() {
    let srv = FakeServer::json(
        r#"{"id":"msg_conc","type":"message","role":"assistant","model":"claude-sonnet-4-5-20250929","stop_reason":"end_turn","content":[{"type":"text","text":"concurrent ok"}],"usage":{"input_tokens":8,"output_tokens":2}}"#,
    );
    let m = new_model(
        MODEL,
        ClientConfig {
            prompt_cache: Some(PromptCacheConfig {
                ttl: PromptCacheTtl::FiveMinutes,
            }),
            ..anthropic_config(&srv.url())
        },
    )
    .await;

    let shared_req = Arc::new(LlmRequest {
        model: MODEL.to_string(),
        contents: vec![
            Content::from_text("Hello", ROLE_USER),
            Content {
                role: ROLE_MODEL.to_string(),
                parts: vec![Part {
                    function_call: Some(FunctionCall {
                        id: "invalid id".to_string(),
                        name: "lookup".to_string(),
                        args: json!({"k": "v"}).as_object().unwrap().clone(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
            },
            Content {
                role: ROLE_USER.to_string(),
                parts: vec![Part {
                    function_response: Some(FunctionResponse {
                        id: "invalid id".to_string(),
                        name: "lookup".to_string(),
                        response: json!({"result": "found"}).as_object().unwrap().clone(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
            },
        ],
        config: Some(GenerateContentConfig {
            system_instruction: Some(Content::from_text("Be helpful.", ROLE_USER)),
            temperature: Some(0.2),
            stop_sequences: vec!["STOP".to_string()],
            tools: vec![Tool {
                function_declarations: vec![FunctionDeclaration {
                    name: "lookup".to_string(),
                    parameters_json_schema: Some(json!({
                        "type": "OBJECT",
                        "properties": {
                            "k": {"type": "STRING"},
                        },
                    })),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }),
        ..Default::default()
    });

    let snapshot_before = snapshot(&shared_req);

    const WORKERS: usize = 16;
    let mut handles = Vec::with_capacity(WORKERS);
    for idx in 0..WORKERS {
        let (m, req) = (m.clone(), shared_req.clone());
        handles.push(tokio::spawn(async move {
            let mut errs = Vec::new();
            let mut stream = m.generate_content(LlmRequest::clone(&req), false);
            while let Some(item) = stream.next().await {
                if let Err(e) = item {
                    errs.push(format!("worker {idx}: {e}"));
                }
            }
            errs
        }));
    }
    let mut errs = Vec::new();
    for h in handles {
        errs.extend(h.await.unwrap());
    }
    assert!(
        errs.is_empty(),
        "concurrent GenerateContent failed: {errs:#?}"
    );

    let snapshot_after = snapshot(&shared_req);
    assert_eq!(
        snapshot_before, snapshot_after,
        "sharedReq was mutated during concurrent GenerateContent"
    );

    let requests = srv.requests();
    assert_eq!(requests.len(), WORKERS, "requests sent");
    for (i, r) in requests.iter().enumerate() {
        assert_eq!(
            r.body, requests[0].body,
            "request {i} body differs from request 0"
        );
    }
}

// --------------------------------------------------------------------------------------------
// TestContextCancellation

// parity: adapted: cancellation is by dropping the stream (spec Design → Cancellation and timeouts): the pre-cancelled context becomes "a stream dropped before its first poll sends no request"; the mid-stream cancel becomes "drop the stream after the first partial; the server observes the disconnect"
#[tokio::test]
async fn test_context_cancellation() {
    let srv = FakeServer::always(
        FakeResponse::sse_chunks([
            sse_event(
                "message_start",
                r#"{"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"claude-sonnet-4-5-20250929","content":[],"usage":{"input_tokens":5,"output_tokens":1}}}"#,
            ),
            sse_event(
                "content_block_start",
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":"first"}}"#,
            ),
        ])
        .hang(),
    );
    let m = new_model(MODEL, anthropic_config(&srv.url())).await;

    // 1. Pre-canceled context.
    let stream = m.generate_content(text_request("hi"), false);
    drop(stream);
    tokio::task::yield_now().await;
    assert_eq!(
        srv.hits(),
        0,
        "pre-canceled: a stream dropped before its first poll sent a request"
    );

    // 2. Mid-stream cancellation.
    let mut stream = m.generate_content(text_request("hi"), true);
    let first: LlmResponse = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match stream.next().await {
                Some(Ok(resp)) if resp.partial => break resp,
                Some(Ok(_)) => continue,
                Some(Err(e)) => panic!("mid-stream: error before the first partial: {e}"),
                None => panic!("mid-stream: stream ended before a partial response"),
            }
        }
    })
    .await
    .expect("mid-stream: no partial response within 10s");
    assert_eq!(
        first.content.as_ref().map(|c| c.parts[0].text.as_str()),
        Some("first"),
        "mid-stream: first partial"
    );
    drop(stream);
    srv.wait_for_drops(1).await;
    assert_eq!(
        srv.hits(),
        1,
        "requests sent (the pre-canceled stream sent none)"
    );
}

// --------------------------------------------------------------------------------------------
// TestTruncatedStreamedToolCall

/// Go subtest `streaming_truncated_tool_call_returns_max_tokens_without_error_or_function_call`.
async fn streaming_truncated_tool_call_returns_max_tokens_without_error_or_function_call() {
    let case = "streaming_truncated_tool_call_returns_max_tokens_without_error_or_function_call";
    let srv = FakeServer::always(FakeResponse::sse(sse_lines(&[
        "event: message_start",
        r#"data: {"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"claude-sonnet-4-5-20250929","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":1}}}"#,
        "",
        "event: content_block_start",
        r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"get_weather","input":{}}}"#,
        "",
        "event: content_block_delta",
        r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"city\": \"Warsa"}}"#,
        "",
        "event: content_block_stop",
        r#"data: {"type":"content_block_stop","index":0}"#,
        "",
        "event: message_delta",
        r#"data: {"type":"message_delta","delta":{"stop_reason":"max_tokens","stop_sequence":null},"usage":{"output_tokens":20}}"#,
        "",
        "event: message_stop",
        r#"data: {"type":"message_stop"}"#,
        "",
    ])));
    let m = new_model(MODEL, anthropic_config(&srv.url())).await;

    let mut resps = Vec::new();
    for item in generate(m.as_ref(), text_request("hi"), true).await {
        match item {
            Ok(resp) => resps.push(resp),
            Err(e) => panic!(
                "{case}: GenerateContent(stream=true) returned error {e}; want LLMResponse with FinishReasonMaxTokens"
            ),
        }
    }
    assert!(
        resps.len() == 1 && resps[0].finish_reason == Some(FinishReason::MaxTokens),
        "{case}: got {resps:?}, want 1 final response with FinishReasonMaxTokens"
    );
    if let Some(content) = &resps[0].content {
        for p in &content.parts {
            assert!(
                p.function_call.is_none(),
                "{case}: truncated tool_use block must not emit incomplete FunctionCall: {:?}",
                p.function_call
            );
        }
    }
}

/// Go subtest `non_streaming_max_tokens_omits_incomplete_tool_use_and_keeps_text`.
async fn non_streaming_max_tokens_omits_incomplete_tool_use_and_keeps_text() {
    let case = "non_streaming_max_tokens_omits_incomplete_tool_use_and_keeps_text";
    let srv = FakeServer::json(
        r#"{"id":"msg_trunc","type":"message","role":"assistant","model":"claude-sonnet-4-5-20250929","stop_reason":"max_tokens","content":[{"type":"text","text":"Let me check the weather."},{"type":"tool_use","id":"toolu_trunc","name":"get_weather","input":{}}],"usage":{"input_tokens":10,"output_tokens":20}}"#,
    );
    let m = new_model(MODEL, anthropic_config(&srv.url())).await;

    let got = generate_last_in(case, m.as_ref(), text_request("hi"), false).await;
    assert_eq!(
        got.finish_reason,
        Some(FinishReason::MaxTokens),
        "{case}: got = {got:?}"
    );
    let parts = got
        .content
        .as_ref()
        .map(|c| c.parts.as_slice())
        .unwrap_or_default();
    assert!(
        parts.len() == 1 && parts[0].text == "Let me check the weather.",
        "{case}: got Content.Parts = {:?}, want only the text part without truncated FunctionCall",
        got.content
    );
}

/// Go subtest `model_context_window_exceeded_omits_tool_use_in_streaming_and_non_streaming`.
async fn model_context_window_exceeded_omits_tool_use_in_streaming_and_non_streaming() {
    let case = "model_context_window_exceeded_omits_tool_use_in_streaming_and_non_streaming";
    let sse = sse_lines(&[
        "event: message_start",
        r#"data: {"type":"message_start","message":{"id":"msg_ctx_s","type":"message","role":"assistant","model":"claude-sonnet-4-5-20250929","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":1}}}"#,
        "",
        "event: content_block_start",
        r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":"Checking weather."}}"#,
        "",
        "event: content_block_stop",
        r#"data: {"type":"content_block_stop","index":0}"#,
        "",
        "event: content_block_start",
        r#"data: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_ok","name":"get_weather","input":{}}}"#,
        "",
        "event: content_block_delta",
        r#"data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"city\":\"Warsaw\"}"}}"#,
        "",
        "event: content_block_stop",
        r#"data: {"type":"content_block_stop","index":1}"#,
        "",
        "event: content_block_start",
        r#"data: {"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_trunc","name":"get_time","input":{}}}"#,
        "",
        "event: content_block_delta",
        r#"data: {"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"tz\":\"Eu"}}"#,
        "",
        "event: content_block_stop",
        r#"data: {"type":"content_block_stop","index":2}"#,
        "",
        "event: message_delta",
        r#"data: {"type":"message_delta","delta":{"stop_reason":"model_context_window_exceeded","stop_sequence":null},"usage":{"output_tokens":25}}"#,
        "",
        "event: message_stop",
        r#"data: {"type":"message_stop"}"#,
        "",
    ]);
    let srv = FakeServer::start(move |r| {
        if r.is_stream() {
            FakeResponse::sse(sse.clone())
        } else {
            FakeResponse::json(
                r#"{"id":"msg_ctx","type":"message","role":"assistant","model":"claude-sonnet-4-5-20250929","stop_reason":"model_context_window_exceeded","content":[{"type":"text","text":"Checking weather."},{"type":"tool_use","id":"toolu_ok","name":"get_weather","input":{"city":"Warsaw"}}],"usage":{"input_tokens":10,"output_tokens":25}}"#,
            )
        }
    });
    let m = new_model(MODEL, anthropic_config(&srv.url())).await;

    for stream_mode in [false, true] {
        let mut final_resp: Option<LlmResponse> = None;
        for item in generate(m.as_ref(), text_request("hi"), stream_mode).await {
            let resp = item.unwrap_or_else(|e| {
                panic!("{case}: GenerateContent(stream={stream_mode}) error = {e}")
            });
            if !resp.partial {
                final_resp = Some(resp);
            }
        }
        let final_resp = final_resp.unwrap_or_else(|| {
            panic!("{case}: stream={stream_mode} finalResp = None, want FinishReasonMaxTokens")
        });
        assert_eq!(
            final_resp.finish_reason,
            Some(FinishReason::MaxTokens),
            "{case}: stream={stream_mode} finalResp = {final_resp:?}"
        );
        assert_eq!(
            final_resp.custom_metadata.get(STOP_REASON_METADATA_KEY),
            Some(&json!("model_context_window_exceeded")),
            "{case}: stream={stream_mode} StopReasonMetadataKey"
        );
        let parts = final_resp
            .content
            .as_ref()
            .map(|c| c.parts.as_slice())
            .unwrap_or_default();
        assert!(
            parts.len() == 1 && parts[0].text == "Checking weather.",
            "{case}: stream={stream_mode} Content.Parts = {:?}, want only text part with all tool_use blocks omitted",
            final_resp.content
        );
    }
}

#[tokio::test]
async fn test_truncated_streamed_tool_call() {
    streaming_truncated_tool_call_returns_max_tokens_without_error_or_function_call().await;
    non_streaming_max_tokens_omits_incomplete_tool_use_and_keeps_text().await;
    model_context_window_exceeded_omits_tool_use_in_streaming_and_non_streaming().await;
}

// --------------------------------------------------------------------------------------------
// TestMaxRetriesAndInFlightNonStreamingCancellation

/// Go subtest `max_retries_honored`.
async fn max_retries_honored() {
    let cases: [(&str, Option<u32>, usize); 3] = [
        ("nil_default_2_retries", None, 3),
        ("zero_retries", Some(0), 1),
        ("one_retry", Some(1), 2),
    ];
    for (name, max_retries, want_calls) in cases {
        let case = format!("max_retries_honored/{name}");
        let srv = FakeServer::always(
            FakeResponse::json_status(
                STATUS_OVERLOADED,
                r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
            )
            .header("Retry-After-Ms", "1"),
        );
        let m = new_model(
            MODEL,
            ClientConfig {
                max_retries,
                ..anthropic_config(&srv.url())
            },
        )
        .await;
        let err = only_error(m.as_ref(), text_request("hi"), false, &case).await;
        assert!(
            anthropic_error(&err).is(ErrorKind::Overloaded),
            "{case}: GenerateContent() error = {err}, want ErrOverloaded"
        );
        assert_eq!(
            err.to_string(),
            "anthropicmodel: api error (status 529) [type=overloaded_error]: Overloaded",
            "{case}: Error()"
        );
        assert_eq!(srv.hits(), want_calls, "{case}: server received calls");
    }
}

/// Go subtest `in_flight_non_streaming_request_cancellation`.
async fn in_flight_non_streaming_request_cancellation() {
    let case = "in_flight_non_streaming_request_cancellation";
    let srv = FakeServer::always(FakeResponse::never());
    let m = new_model(MODEL, anthropic_config(&srv.url())).await;

    let mut stream = m.generate_content(text_request("hi"), false);
    tokio::select! {
        item = stream.next() => panic!("{case}: the request finished before the cancel: {item:?}"),
        () = srv.wait_for_hits(1) => {}
    }
    drop(stream);
    srv.wait_for_drops(1).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        srv.hits(),
        1,
        "{case}: nothing is sent after the drop (no retry)"
    );
}

// parity: adapted: in_flight_non_streaming_request_cancellation drops the stream once the server has the request (drop-cancellation, spec Design → Cancellation and timeouts) instead of cancelling a context, and checks the server sees the disconnect and no retry
#[tokio::test]
async fn test_max_retries_and_in_flight_non_streaming_cancellation() {
    max_retries_honored().await;
    in_flight_non_streaming_request_cancellation().await;
}

// --------------------------------------------------------------------------------------------
// TestGenaiConfigReflectionCompleteness

/// Go's per-field disposition (`doc.go` "GenerateContentConfig Support" and the validators):
/// honored (translated into the Messages request, possibly after validation), rejected with
/// `ErrInvalidConfig` naming the field, or ignored by design.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Disposition {
    Honored,
    Rejected,
    Ignored,
}

use Disposition::{Honored, Ignored, Rejected};

/// One field of a gadk type: (gadk field, Go field, disposition).
type FieldRow = (&'static str, &'static str, Disposition);

// Each `*_fields` function destructures its type without `..`: a field added to the gadk type
// breaks the build here until it is classified.

fn generate_content_config_fields(cfg: &GenerateContentConfig) -> Vec<FieldRow> {
    let GenerateContentConfig {
        system_instruction: _,
        temperature: _,
        top_p: _,
        top_k: _,
        candidate_count: _,
        max_output_tokens: _,
        stop_sequences: _,
        response_logprobs: _,
        logprobs: _,
        presence_penalty: _,
        frequency_penalty: _,
        seed: _,
        response_mime_type: _,
        response_schema: _,
        response_json_schema: _,
        routing_config: _,
        model_selection_config: _,
        safety_settings: _,
        tools: _,
        tool_config: _,
        labels: _,
        cached_content: _,
        response_modalities: _,
        media_resolution: _,
        speech_config: _,
        audio_timestamp: _,
        thinking_config: _,
        image_config: _,
        enable_enhanced_civic_answers: _,
        model_armor_config: _,
        service_tier: _,
        audio_transcription_config: _,
        continuation_token: _,
        automatic_continuation: _,
    } = cfg;
    vec![
        ("system_instruction", "SystemInstruction", Honored),
        ("temperature", "Temperature", Honored),
        ("top_p", "TopP", Honored),
        ("top_k", "TopK", Honored),
        ("candidate_count", "CandidateCount", Honored),
        ("max_output_tokens", "MaxOutputTokens", Honored),
        ("stop_sequences", "StopSequences", Honored),
        ("response_logprobs", "ResponseLogprobs", Rejected),
        ("logprobs", "Logprobs", Rejected),
        ("presence_penalty", "PresencePenalty", Rejected),
        ("frequency_penalty", "FrequencyPenalty", Rejected),
        ("seed", "Seed", Rejected),
        ("response_mime_type", "ResponseMIMEType", Honored),
        ("response_schema", "ResponseSchema", Honored),
        ("response_json_schema", "ResponseJsonSchema", Honored),
        ("routing_config", "RoutingConfig", Rejected),
        ("model_selection_config", "ModelSelectionConfig", Rejected),
        ("safety_settings", "SafetySettings", Rejected),
        ("tools", "Tools", Honored),
        ("tool_config", "ToolConfig", Honored),
        ("labels", "Labels", Rejected),
        ("cached_content", "CachedContent", Rejected),
        ("response_modalities", "ResponseModalities", Honored),
        ("media_resolution", "MediaResolution", Rejected),
        ("speech_config", "SpeechConfig", Rejected),
        ("audio_timestamp", "AudioTimestamp", Rejected),
        ("thinking_config", "ThinkingConfig", Honored),
        ("image_config", "ImageConfig", Rejected),
        (
            "enable_enhanced_civic_answers",
            "EnableEnhancedCivicAnswers",
            Rejected,
        ),
        ("model_armor_config", "ModelArmorConfig", Rejected),
        ("service_tier", "ServiceTier", Rejected),
        (
            "audio_transcription_config",
            "AudioTranscriptionConfig",
            Rejected,
        ),
        ("continuation_token", "ContinuationToken", Rejected),
        ("automatic_continuation", "AutomaticContinuation", Ignored),
    ]
}

fn http_options_fields(opts: &HttpOptions) -> Vec<FieldRow> {
    let HttpOptions {
        base_url: _,
        api_version: _,
        headers: _,
        timeout: _,
        extras_request_provider: _,
    } = opts;
    vec![
        ("base_url", "BaseURL", Rejected),
        ("api_version", "APIVersion", Rejected),
        ("headers", "Headers", Ignored),
        ("timeout", "Timeout", Honored),
        ("extras_request_provider", "ExtrasRequestProvider", Rejected),
    ]
}

fn thinking_config_fields(tc: &ThinkingConfig) -> Vec<FieldRow> {
    let ThinkingConfig {
        include_thoughts: _,
        thinking_budget: _,
        thinking_level: _,
    } = tc;
    vec![
        ("include_thoughts", "IncludeThoughts", Ignored),
        ("thinking_budget", "ThinkingBudget", Honored),
        ("thinking_level", "ThinkingLevel", Rejected),
    ]
}

fn tool_config_fields(tc: &ToolConfig) -> Vec<FieldRow> {
    let ToolConfig {
        retrieval_config: _,
        function_calling_config: _,
        include_server_side_tool_invocations: _,
    } = tc;
    vec![
        ("retrieval_config", "RetrievalConfig", Rejected),
        ("function_calling_config", "FunctionCallingConfig", Honored),
        (
            "include_server_side_tool_invocations",
            "IncludeServerSideToolInvocations",
            Rejected,
        ),
    ]
}

fn function_calling_config_fields(fcc: &FunctionCallingConfig) -> Vec<FieldRow> {
    let FunctionCallingConfig {
        allowed_function_names: _,
        mode: _,
        stream_function_call_arguments: _,
    } = fcc;
    vec![
        ("allowed_function_names", "AllowedFunctionNames", Honored),
        ("mode", "Mode", Honored),
        (
            "stream_function_call_arguments",
            "StreamFunctionCallArguments",
            Rejected,
        ),
    ]
}

fn tool_fields(tool: &Tool) -> Vec<FieldRow> {
    let Tool {
        retrieval: _,
        computer_use: _,
        file_search: _,
        google_search: _,
        google_maps: _,
        code_execution: _,
        enterprise_web_search: _,
        function_declarations: _,
        google_search_retrieval: _,
        parallel_ai_search: _,
        url_context: _,
        mcp_servers: _,
        exa_ai_search: _,
    } = tool;
    vec![
        ("retrieval", "Retrieval", Rejected),
        ("computer_use", "ComputerUse", Rejected),
        ("file_search", "FileSearch", Rejected),
        ("google_search", "GoogleSearch", Rejected),
        ("google_maps", "GoogleMaps", Rejected),
        ("code_execution", "CodeExecution", Rejected),
        ("enterprise_web_search", "EnterpriseWebSearch", Rejected),
        ("function_declarations", "FunctionDeclarations", Honored),
        ("google_search_retrieval", "GoogleSearchRetrieval", Rejected),
        ("parallel_ai_search", "ParallelAISearch", Rejected),
        ("url_context", "URLContext", Rejected),
        ("mcp_servers", "MCPServers", Rejected),
        ("exa_ai_search", "ExaAISearch", Rejected),
    ]
}

/// Go's allowlist for one type (every exported field), with its disposition.
type GoTable = &'static [(&'static str, Disposition)];

const GO_GENERATE_CONTENT_CONFIG: GoTable = &[
    ("SystemInstruction", Honored),
    ("Temperature", Honored),
    ("TopP", Honored),
    ("TopK", Honored),
    ("CandidateCount", Honored),
    ("MaxOutputTokens", Honored),
    ("StopSequences", Honored),
    ("ResponseLogprobs", Rejected),
    ("Logprobs", Rejected),
    ("PresencePenalty", Rejected),
    ("FrequencyPenalty", Rejected),
    ("Seed", Rejected),
    ("ResponseMIMEType", Honored),
    ("ResponseSchema", Honored),
    ("ResponseJsonSchema", Honored),
    ("RoutingConfig", Rejected),
    ("ModelSelectionConfig", Rejected),
    ("SafetySettings", Rejected),
    ("Tools", Honored),
    ("ToolConfig", Honored),
    ("Labels", Rejected),
    ("CachedContent", Rejected),
    ("ResponseModalities", Honored),
    ("MediaResolution", Rejected),
    ("SpeechConfig", Rejected),
    ("AudioTimestamp", Rejected),
    ("ThinkingConfig", Honored),
    ("ImageConfig", Rejected),
    ("HTTPOptions", Honored),
    ("ModelArmorConfig", Rejected),
    ("EnableEnhancedCivicAnswers", Rejected),
    ("ServiceTier", Rejected),
    ("AudioTranscriptionConfig", Rejected),
    ("AutomaticContinuation", Ignored),
    ("ContinuationToken", Rejected),
];

const GO_HTTP_OPTIONS: GoTable = &[
    ("BaseURL", Rejected),
    ("BaseURLResourceScope", Rejected),
    ("APIVersion", Rejected),
    ("Headers", Ignored),
    ("Timeout", Honored),
    ("ExtraBody", Rejected),
    ("ExtrasRequestProvider", Rejected),
    ("RetryOptions", Rejected),
];

const GO_THINKING_CONFIG: GoTable = &[
    ("IncludeThoughts", Ignored),
    ("ThinkingBudget", Honored),
    ("ThinkingLevel", Rejected),
];

const GO_TOOL_CONFIG: GoTable = &[
    ("FunctionCallingConfig", Honored),
    ("RetrievalConfig", Rejected),
    ("IncludeServerSideToolInvocations", Rejected),
];

const GO_FUNCTION_CALLING_CONFIG: GoTable = &[
    ("Mode", Honored),
    ("AllowedFunctionNames", Honored),
    ("StreamFunctionCallArguments", Rejected),
];

const GO_TOOL: GoTable = &[
    ("FunctionDeclarations", Honored),
    ("Retrieval", Rejected),
    ("GoogleSearch", Rejected),
    ("GoogleSearchRetrieval", Rejected),
    ("EnterpriseWebSearch", Rejected),
    ("GoogleMaps", Rejected),
    ("URLContext", Rejected),
    ("ComputerUse", Rejected),
    ("CodeExecution", Rejected),
    ("FileSearch", Rejected),
    ("ParallelAISearch", Rejected),
    ("MCPServers", Rejected),
    ("ExaAISearch", Rejected),
];

/// Go fields the gadk type does not have, with the reason.
const ABSENT_IN_GADK: &[(&str, &str, &str)] = &[
    (
        "GenerateContentConfig",
        "HTTPOptions",
        "gadk carries HTTP options on LlmRequest.http_options (classified in the HttpOptions table)",
    ),
    (
        "HttpOptions",
        "BaseURLResourceScope",
        "gadk_model::HttpOptions has no such field",
    ),
    (
        "HttpOptions",
        "ExtraBody",
        "gadk_model::HttpOptions has no such field",
    ),
    (
        "HttpOptions",
        "RetryOptions",
        "gadk_model::HttpOptions has no such field (retries are ClientConfig::max_retries)",
    ),
];

/// Go `assertFieldsMatch`, both ways, plus the disposition of every shared field. Returns the
/// problems found.
fn assert_fields_match(type_name: &str, gadk: &[FieldRow], go: GoTable) -> Vec<String> {
    let mut errs = Vec::new();
    let go_map: BTreeMap<&str, Disposition> = go.iter().copied().collect();
    let absent: BTreeSet<&str> = ABSENT_IN_GADK
        .iter()
        .filter(|(t, _, _)| *t == type_name)
        .map(|(_, f, _)| *f)
        .collect();
    let mut seen = BTreeSet::new();
    for &(rust_name, go_name, disposition) in gadk {
        if !seen.insert(go_name) {
            errs.push(format!("{type_name}: Go field {go_name:?} mapped twice"));
        }
        match go_map.get(go_name) {
            None => errs.push(format!(
                "{type_name} has unhandled field {rust_name:?} (Go {go_name:?}); classify it in config/tool validation and update this test"
            )),
            Some(&want) if want != disposition => errs.push(format!(
                "{type_name}.{rust_name}: disposition {disposition:?}, Go {want:?}"
            )),
            Some(_) => {}
        }
        if absent.contains(go_name) {
            errs.push(format!(
                "{type_name}: {go_name:?} is listed as absent but gadk has {rust_name:?}"
            ));
        }
    }
    for &(go_name, _) in go {
        if !seen.contains(go_name) && !absent.contains(go_name) {
            errs.push(format!(
                "{type_name} allowlist contains Go field {go_name:?} that gadk neither has nor lists as absent"
            ));
        }
    }
    for name in &absent {
        if !go_map.contains_key(name) {
            errs.push(format!(
                "{type_name}: absent field {name:?} is not a Go field"
            ));
        }
    }
    errs
}

/// A request that sets exactly one field whose Go disposition is "rejected", with the whole
/// error text Go returns for it.
fn rejected_field_samples() -> Vec<(String, LlmRequest, String)> {
    let gcc = GenerateContentConfig::default;
    let with_tool = |tool: Tool| {
        cfg_request(GenerateContentConfig {
            tools: vec![tool],
            ..gcc()
        })
    };
    let tool = Tool::default;
    let invalid = |detail: &str| format!("{INVALID_CONFIG}: {detail}");
    let unsupported_tool = |field: &str| {
        format!(
            "{INVALID_CONFIG}: anthropicmodel: unsupported tool type: Tools.{field} is not supported by Claude"
        )
    };
    let mut out = vec![];
    for (go_name, req) in [
        (
            "ResponseLogprobs",
            cfg_request(GenerateContentConfig {
                response_logprobs: true,
                ..gcc()
            }),
        ),
        (
            "Logprobs",
            cfg_request(GenerateContentConfig {
                logprobs: Some(1),
                ..gcc()
            }),
        ),
        (
            "PresencePenalty",
            cfg_request(GenerateContentConfig {
                presence_penalty: Some(0.5),
                ..gcc()
            }),
        ),
        (
            "FrequencyPenalty",
            cfg_request(GenerateContentConfig {
                frequency_penalty: Some(0.5),
                ..gcc()
            }),
        ),
        (
            "Seed",
            cfg_request(GenerateContentConfig {
                seed: Some(1),
                ..gcc()
            }),
        ),
        (
            "RoutingConfig",
            cfg_request(GenerateContentConfig {
                routing_config: Some(Default::default()),
                ..gcc()
            }),
        ),
        (
            "ModelSelectionConfig",
            cfg_request(GenerateContentConfig {
                model_selection_config: Some(Default::default()),
                ..gcc()
            }),
        ),
        (
            "SafetySettings",
            cfg_request(GenerateContentConfig {
                safety_settings: vec![Default::default()],
                ..gcc()
            }),
        ),
        (
            "Labels",
            cfg_request(GenerateContentConfig {
                labels: BTreeMap::from([("k".to_string(), "v".to_string())]),
                ..gcc()
            }),
        ),
        (
            "CachedContent",
            cfg_request(GenerateContentConfig {
                cached_content: "c".to_string(),
                ..gcc()
            }),
        ),
        (
            "MediaResolution",
            cfg_request(GenerateContentConfig {
                media_resolution: Some(MediaResolution::High),
                ..gcc()
            }),
        ),
        (
            "SpeechConfig",
            cfg_request(GenerateContentConfig {
                speech_config: Some(Default::default()),
                ..gcc()
            }),
        ),
        (
            "AudioTimestamp",
            cfg_request(GenerateContentConfig {
                audio_timestamp: true,
                ..gcc()
            }),
        ),
        (
            "ImageConfig",
            cfg_request(GenerateContentConfig {
                image_config: Some(Default::default()),
                ..gcc()
            }),
        ),
        (
            "ModelArmorConfig",
            cfg_request(GenerateContentConfig {
                model_armor_config: Some(Default::default()),
                ..gcc()
            }),
        ),
        (
            "EnableEnhancedCivicAnswers",
            cfg_request(GenerateContentConfig {
                enable_enhanced_civic_answers: Some(true),
                ..gcc()
            }),
        ),
        (
            "ServiceTier",
            cfg_request(GenerateContentConfig {
                service_tier: Some(ServiceTier::Flex),
                ..gcc()
            }),
        ),
        (
            "AudioTranscriptionConfig",
            cfg_request(GenerateContentConfig {
                audio_transcription_config: Some(Default::default()),
                ..gcc()
            }),
        ),
        (
            "ContinuationToken",
            cfg_request(GenerateContentConfig {
                continuation_token: Some(Bytes::from_static(b"t")),
                ..gcc()
            }),
        ),
    ] {
        let detail = match go_name {
            "CachedContent" => {
                "CachedContent is not supported; use ClientConfig.PromptCache instead".to_string()
            }
            _ => format!("{go_name} is not supported"),
        };
        out.push((go_name.to_string(), req, invalid(&detail)));
    }
    out.push((
        "HTTPOptions.BaseURL".to_string(),
        http_options_request(HttpOptions {
            base_url: Some("https://x.example".to_string()),
            ..Default::default()
        }),
        invalid("HTTPOptions.BaseURL is not supported; set ClientConfig.BaseURL instead"),
    ));
    out.push((
        "HTTPOptions.APIVersion".to_string(),
        http_options_request(HttpOptions {
            api_version: Some("v1".to_string()),
            ..Default::default()
        }),
        invalid("HTTPOptions.APIVersion is not supported"),
    ));
    out.push((
        "HTTPOptions.ExtrasRequestProvider".to_string(),
        http_options_request(HttpOptions {
            extras_request_provider: Some(Arc::new(|b| b)),
            ..Default::default()
        }),
        invalid("HTTPOptions.ExtrasRequestProvider is not supported"),
    ));
    out.push((
        "ThinkingConfig.ThinkingLevel".to_string(),
        cfg_request(GenerateContentConfig {
            thinking_config: Some(ThinkingConfig {
                thinking_budget: Some(0),
                thinking_level: Some(ThinkingLevel::High),
                ..Default::default()
            }),
            ..gcc()
        }),
        invalid("ThinkingConfig.ThinkingLevel is not supported on Claude; configure ClientConfig.Effort instead"),
    ));
    out.push((
        "ToolConfig.RetrievalConfig".to_string(),
        cfg_request(GenerateContentConfig {
            tool_config: Some(ToolConfig {
                retrieval_config: Some(RetrievalConfig::default()),
                ..Default::default()
            }),
            ..gcc()
        }),
        invalid("ToolConfig.RetrievalConfig is not supported"),
    ));
    out.push((
        "ToolConfig.IncludeServerSideToolInvocations".to_string(),
        cfg_request(GenerateContentConfig {
            tool_config: Some(ToolConfig {
                include_server_side_tool_invocations: Some(true),
                ..Default::default()
            }),
            ..gcc()
        }),
        invalid("ToolConfig.IncludeServerSideToolInvocations is not supported"),
    ));
    out.push((
        "ToolConfig.FunctionCallingConfig.StreamFunctionCallArguments".to_string(),
        cfg_request(GenerateContentConfig {
            tool_config: Some(ToolConfig {
                function_calling_config: Some(FunctionCallingConfig {
                    stream_function_call_arguments: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..gcc()
        }),
        invalid("ToolConfig.FunctionCallingConfig.StreamFunctionCallArguments is not supported"),
    ));
    for (field, t) in [
        (
            "Retrieval",
            Tool {
                retrieval: Some(Retrieval::default()),
                ..tool()
            },
        ),
        (
            "ComputerUse",
            Tool {
                computer_use: Some(ComputerUse::default()),
                ..tool()
            },
        ),
        (
            "FileSearch",
            Tool {
                file_search: Some(FileSearch::default()),
                ..tool()
            },
        ),
        (
            "GoogleSearch",
            Tool {
                google_search: Some(GoogleSearch::default()),
                ..tool()
            },
        ),
        (
            "GoogleMaps",
            Tool {
                google_maps: Some(GoogleMaps::default()),
                ..tool()
            },
        ),
        (
            "CodeExecution",
            Tool {
                code_execution: Some(ToolCodeExecution::default()),
                ..tool()
            },
        ),
        (
            "EnterpriseWebSearch",
            Tool {
                enterprise_web_search: Some(EnterpriseWebSearch::default()),
                ..tool()
            },
        ),
        (
            "GoogleSearchRetrieval",
            Tool {
                google_search_retrieval: Some(GoogleSearchRetrieval::default()),
                ..tool()
            },
        ),
        (
            "ParallelAISearch",
            Tool {
                parallel_ai_search: Some(ToolParallelAISearch::default()),
                ..tool()
            },
        ),
        (
            "URLContext",
            Tool {
                url_context: Some(URLContext::default()),
                ..tool()
            },
        ),
        (
            "MCPServers",
            Tool {
                mcp_servers: vec![MCPServer::default()],
                ..tool()
            },
        ),
        (
            "ExaAISearch",
            Tool {
                exa_ai_search: Some(ToolExaAISearch::default()),
                ..tool()
            },
        ),
    ] {
        out.push((
            format!("Tools.{field}"),
            with_tool(t),
            unsupported_tool(field),
        ));
    }
    out
}

/// A request that sets exactly one field whose Go disposition is "ignored".
fn ignored_field_samples() -> Vec<(&'static str, LlmRequest)> {
    let mut headers = http::HeaderMap::new();
    headers.insert("X-Gemini-Only", "x".parse().unwrap());
    vec![
        (
            "AutomaticContinuation",
            cfg_request(GenerateContentConfig {
                automatic_continuation: Some(true),
                ..Default::default()
            }),
        ),
        (
            "HTTPOptions.Headers",
            http_options_request(HttpOptions {
                headers,
                ..Default::default()
            }),
        ),
        (
            "ThinkingConfig.IncludeThoughts",
            cfg_request(GenerateContentConfig {
                thinking_config: Some(ThinkingConfig {
                    include_thoughts: true,
                    thinking_budget: Some(0),
                    ..Default::default()
                }),
                ..Default::default()
            }),
        ),
    ]
}

/// The Go field paths (as Go's error texts spell them) of every gadk field with disposition
/// `want` across the six tables.
fn field_paths(want: Disposition) -> BTreeSet<String> {
    let tables: [(&str, Vec<FieldRow>); 6] = [
        (
            "",
            generate_content_config_fields(&GenerateContentConfig::default()),
        ),
        ("HTTPOptions.", http_options_fields(&HttpOptions::default())),
        (
            "ThinkingConfig.",
            thinking_config_fields(&ThinkingConfig::default()),
        ),
        ("ToolConfig.", tool_config_fields(&ToolConfig::default())),
        (
            "ToolConfig.FunctionCallingConfig.",
            function_calling_config_fields(&FunctionCallingConfig::default()),
        ),
        ("Tools.", tool_fields(&Tool::default())),
    ];
    tables
        .iter()
        .flat_map(|(prefix, rows)| {
            rows.iter()
                .filter(move |(_, _, d)| *d == want)
                .map(move |(_, go, _)| format!("{prefix}{go}"))
        })
        .collect()
}

// parity: adapted: Go's reflect allowlists become exhaustive destructures without `..` (a new gadk field breaks the build) plus Go-disposition tables checked both ways; every rejected and ignored field gadk has is also exercised through the model
#[tokio::test]
async fn test_genai_config_reflection_completeness() {
    let mut errs = Vec::new();
    errs.extend(assert_fields_match(
        "GenerateContentConfig",
        &generate_content_config_fields(&GenerateContentConfig::default()),
        GO_GENERATE_CONTENT_CONFIG,
    ));
    errs.extend(assert_fields_match(
        "HttpOptions",
        &http_options_fields(&HttpOptions::default()),
        GO_HTTP_OPTIONS,
    ));
    errs.extend(assert_fields_match(
        "ThinkingConfig",
        &thinking_config_fields(&ThinkingConfig::default()),
        GO_THINKING_CONFIG,
    ));
    errs.extend(assert_fields_match(
        "ToolConfig",
        &tool_config_fields(&ToolConfig::default()),
        GO_TOOL_CONFIG,
    ));
    errs.extend(assert_fields_match(
        "FunctionCallingConfig",
        &function_calling_config_fields(&FunctionCallingConfig::default()),
        GO_FUNCTION_CALLING_CONFIG,
    ));
    errs.extend(assert_fields_match(
        "Tool",
        &tool_fields(&Tool::default()),
        GO_TOOL,
    ));
    assert!(
        errs.is_empty(),
        "field classification problems:\n{}",
        errs.join("\n")
    );

    // The rejected and ignored sets, exercised through the model.
    let srv = FakeServer::json(
        r#"{"id":"msg_r","type":"message","role":"assistant","model":"claude-sonnet-4-5-20250929","stop_reason":"end_turn","content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":1,"output_tokens":1}}"#,
    );
    let m = new_model(MODEL, anthropic_config(&srv.url())).await;

    let rejected = rejected_field_samples();
    let sampled: BTreeSet<String> = rejected.iter().map(|(p, _, _)| p.clone()).collect();
    assert_eq!(
        sampled,
        field_paths(Rejected),
        "rejected samples cover exactly the rejected fields"
    );
    for (path, req, want_text) in rejected {
        let err = only_error(m.as_ref(), req, false, &format!("rejected/{path}")).await;
        assert!(
            anthropic_error(&err).is(ErrorKind::InvalidConfig),
            "rejected/{path}: error = {err}, want ErrInvalidConfig"
        );
        assert_eq!(err.to_string(), want_text, "rejected/{path}");
    }
    assert_eq!(
        srv.hits(),
        0,
        "rejected fields must fail before any request is sent"
    );

    let ignored = ignored_field_samples();
    let sampled: BTreeSet<String> = ignored.iter().map(|(p, _)| p.to_string()).collect();
    assert_eq!(
        sampled,
        field_paths(Ignored),
        "ignored samples cover exactly the ignored fields"
    );
    for (path, req) in ignored {
        for item in generate(m.as_ref(), req, false).await {
            if let Err(e) = item {
                panic!("ignored/{path}: GenerateContent() error = {e}");
            }
        }
        assert_eq!(
            srv.last_request().header("X-Gemini-Only"),
            "",
            "ignored/{path}: header leaked"
        );
    }
    assert_eq!(srv.hits(), 3, "one request per ignored field");
}
