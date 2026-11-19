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

//! Tests for the `Llm` trait, requests and responses.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::sync::Arc;

use gadk_genai::*;
use gadk_model::*;
use pretty_assertions::assert_eq;

fn text_content(s: &str) -> Content {
    Content {
        parts: vec![Part {
            text: s.into(),
            ..Default::default()
        }],
        role: String::new(),
    }
}

fn lp(token: &str, log_probability: f32, token_id: i32) -> LogprobsResultCandidate {
    LogprobsResultCandidate {
        log_probability,
        token: token.into(),
        token_id,
    }
}

fn top(tokens: &[&str]) -> LogprobsResultTopCandidates {
    LogprobsResultTopCandidates {
        candidates: tokens.iter().map(|t| lp(t, 0.0, 0)).collect(),
    }
}

/// Port of ADK-Go `model/llm_test.go` `TestCreateResponse`.
#[test]
fn test_create_response() {
    let empty_logprobs = LogprobsResult::default();
    let concrete_logprobs = LogprobsResult {
        chosen_candidates: vec![
            lp("The", -0.1, 123),
            lp(" capital", -0.5, 456),
            lp(" of", -0.2, 789),
        ],
        top_candidates: vec![
            top(&["The", "A", "This"]),
            top(&[" capital", " city", " main"]),
        ],
        ..Default::default()
    };
    let partial_logprobs = LogprobsResult {
        chosen_candidates: vec![lp("Hello", -0.05, 111), lp(" world", -0.8, 222)],
        ..Default::default()
    };
    let citation_meta = CitationMetadata {
        citations: vec![Citation {
            start_index: 0,
            end_index: 10,
            uri: "https://example.com".into(),
            ..Default::default()
        }],
    };

    let one = |c: Candidate| GenerateContentResponse {
        candidates: vec![c],
        ..Default::default()
    };

    let cases: Vec<(&str, GenerateContentResponse, LlmResponse)> = vec![
        (
            "CreateWithLogprobs",
            one(Candidate {
                content: Some(text_content("Response text")),
                finish_reason: Some(FinishReason::Stop),
                avg_logprobs: -0.75,
                logprobs_result: Some(empty_logprobs.clone()),
                ..Default::default()
            }),
            LlmResponse {
                content: Some(text_content("Response text")),
                finish_reason: Some(FinishReason::Stop),
                avg_logprobs: -0.75,
                logprobs_result: Some(empty_logprobs.clone()),
                ..Default::default()
            },
        ),
        (
            "CreateWithoutLogprobs",
            one(Candidate {
                content: Some(text_content("Response text")),
                finish_reason: Some(FinishReason::Stop),
                ..Default::default()
            }),
            LlmResponse {
                content: Some(text_content("Response text")),
                finish_reason: Some(FinishReason::Stop),
                ..Default::default()
            },
        ),
        (
            "CreateErrorCaseWithLogprobs",
            one(Candidate {
                finish_reason: Some(FinishReason::Safety),
                finish_message: "Safety filter triggered".into(),
                avg_logprobs: -2.1,
                ..Default::default()
            }),
            LlmResponse {
                error_code: "SAFETY".into(),
                error_message: "Safety filter triggered".into(),
                avg_logprobs: -2.1,
                finish_reason: Some(FinishReason::Safety),
                ..Default::default()
            },
        ),
        (
            "CreateNoCandidates",
            GenerateContentResponse {
                candidates: vec![],
                prompt_feedback: Some(GenerateContentResponsePromptFeedback {
                    block_reason: Some(BlockedReason::Safety),
                    block_reason_message: "Prompt blocked for safety".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            LlmResponse {
                error_code: "SAFETY".into(),
                error_message: "Prompt blocked for safety".into(),
                ..Default::default()
            },
        ),
        (
            "CreateWithConcreteLogprobsResult",
            one(Candidate {
                content: Some(text_content("The capital of France is Paris.")),
                finish_reason: Some(FinishReason::Stop),
                avg_logprobs: -0.27,
                logprobs_result: Some(concrete_logprobs.clone()),
                ..Default::default()
            }),
            LlmResponse {
                content: Some(text_content("The capital of France is Paris.")),
                finish_reason: Some(FinishReason::Stop),
                avg_logprobs: -0.27,
                logprobs_result: Some(concrete_logprobs.clone()),
                ..Default::default()
            },
        ),
        (
            "CreateWithPartial*genai.LogprobsResult",
            one(Candidate {
                content: Some(text_content("Hello world")),
                finish_reason: Some(FinishReason::Stop),
                avg_logprobs: -0.425,
                logprobs_result: Some(partial_logprobs.clone()),
                ..Default::default()
            }),
            LlmResponse {
                content: Some(text_content("Hello world")),
                finish_reason: Some(FinishReason::Stop),
                avg_logprobs: -0.425,
                logprobs_result: Some(partial_logprobs.clone()),
                ..Default::default()
            },
        ),
        (
            "CreateWithCitationMetadata",
            one(Candidate {
                content: Some(text_content("Response text")),
                finish_reason: Some(FinishReason::Stop),
                citation_metadata: Some(citation_meta.clone()),
                ..Default::default()
            }),
            LlmResponse {
                content: Some(text_content("Response text")),
                finish_reason: Some(FinishReason::Stop),
                citation_metadata: Some(citation_meta.clone()),
                ..Default::default()
            },
        ),
        (
            "CreateWithoutCitationMetadata",
            one(Candidate {
                content: Some(text_content("Response text")),
                finish_reason: Some(FinishReason::Stop),
                ..Default::default()
            }),
            LlmResponse {
                content: Some(text_content("Response text")),
                finish_reason: Some(FinishReason::Stop),
                ..Default::default()
            },
        ),
        (
            "CreateErrorCaseWithCitationMetadata",
            one(Candidate {
                finish_reason: Some(FinishReason::Recitation),
                finish_message: "Response blocked due to recitation triggered".into(),
                citation_metadata: Some(citation_meta.clone()),
                ..Default::default()
            }),
            LlmResponse {
                error_code: "RECITATION".into(),
                error_message: "Response blocked due to recitation triggered".into(),
                citation_metadata: Some(citation_meta.clone()),
                finish_reason: Some(FinishReason::Recitation),
                ..Default::default()
            },
        ),
    ];

    for (name, input, want) in cases {
        let got = llm_response_from_genai(&input);
        assert_eq!(got.avg_logprobs, want.avg_logprobs, "{name}: avg_logprobs");
        assert_eq!(got.error_code, want.error_code, "{name}: error_code");
        assert_eq!(
            got.error_message, want.error_message,
            "{name}: error_message"
        );
        assert_eq!(
            got.finish_reason, want.finish_reason,
            "{name}: finish_reason"
        );
        assert_eq!(got.content, want.content, "{name}: content");
        assert_eq!(
            got.logprobs_result, want.logprobs_result,
            "{name}: logprobs_result"
        );
        assert_eq!(
            got.citation_metadata, want.citation_metadata,
            "{name}: citation_metadata"
        );
        assert_eq!(got, want, "{name}: whole response");
    }
}

#[test]
fn convert_prompt_feedback_sets_error() {
    let res = GenerateContentResponse {
        prompt_feedback: Some(GenerateContentResponsePromptFeedback {
            block_reason: Some(BlockedReason::Safety),
            block_reason_message: "blocked".into(),
            ..Default::default()
        }),
        model_version: "m".into(),
        ..Default::default()
    };
    let got = llm_response_from_genai(&res);
    assert_eq!(
        (
            got.error_code.as_str(),
            got.error_message.as_str(),
            got.model_version.as_str()
        ),
        ("SAFETY", "blocked", "m")
    );
}

#[test]
fn convert_no_candidates_no_feedback_gives_empty_model_content() {
    let got = llm_response_from_genai(&GenerateContentResponse::default());
    assert_eq!(
        got.content,
        Some(Content {
            parts: vec![],
            role: ROLE_MODEL.into()
        })
    );
}

#[test]
fn convert_content_branch_carries_usage_grounding_and_model_version() {
    let usage = GenerateContentResponseUsageMetadata {
        total_token_count: 7,
        ..Default::default()
    };
    let res = GenerateContentResponse {
        candidates: vec![Candidate {
            content: Some(text_content("hi")),
            grounding_metadata: Some(GroundingMetadata::default()),
            ..Default::default()
        }],
        usage_metadata: Some(usage.clone()),
        model_version: "v1".into(),
        ..Default::default()
    };
    let got = llm_response_from_genai(&res);
    assert_eq!(
        got,
        LlmResponse {
            content: Some(text_content("hi")),
            grounding_metadata: Some(GroundingMetadata::default()),
            usage_metadata: Some(usage),
            model_version: "v1".into(),
            ..Default::default()
        }
    );
}

#[test]
fn convert_stop_without_content_takes_content_branch() {
    let res = GenerateContentResponse {
        candidates: vec![Candidate {
            finish_reason: Some(FinishReason::Stop),
            finish_message: "ignored".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let got = llm_response_from_genai(&res);
    assert_eq!(
        got,
        LlmResponse {
            finish_reason: Some(FinishReason::Stop),
            ..Default::default()
        }
    );
}

#[test]
fn convert_empty_parts_non_stop_takes_error_branch() {
    let res = GenerateContentResponse {
        candidates: vec![Candidate {
            content: Some(Content {
                parts: vec![],
                role: ROLE_MODEL.into(),
            }),
            ..Default::default()
        }],
        prompt_feedback: Some(GenerateContentResponsePromptFeedback {
            block_reason: Some(BlockedReason::Safety),
            ..Default::default()
        }),
        ..Default::default()
    };
    let got = llm_response_from_genai(&res);
    // Candidate wins over prompt feedback; no finish reason means empty error code.
    assert_eq!(got, LlmResponse::default());
}

#[test]
fn llm_response_json_matches_go_tags() {
    let r = LlmResponse {
        partial: true,
        turn_complete: true,
        error_code: "429".into(),
        finish_reason: Some(FinishReason::Stop),
        ..Default::default()
    };
    assert_eq!(
        serde_json::to_value(&r).unwrap(),
        serde_json::json!({"partial":true,"turnComplete":true,"errorCode":"429","finishReason":"STOP"})
    );
}

#[test]
fn llm_response_json_field_names_and_order() {
    let mut custom = serde_json::Map::new();
    custom.insert("k".into(), serde_json::json!(1));
    let r = LlmResponse {
        content: Some(text_content("x")),
        citation_metadata: Some(CitationMetadata::default()),
        grounding_metadata: Some(GroundingMetadata::default()),
        usage_metadata: Some(GenerateContentResponseUsageMetadata::default()),
        custom_metadata: custom,
        logprobs_result: Some(LogprobsResult::default()),
        input_transcription: Some(Transcription::default()),
        output_transcription: Some(Transcription::default()),
        model_version: "mv".into(),
        partial: true,
        turn_complete: true,
        interrupted: true,
        session_resumption_handle: "h".into(),
        error_code: "c".into(),
        error_message: "m".into(),
        finish_reason: Some(FinishReason::Stop),
        avg_logprobs: -0.5,
    };
    let s = serde_json::to_string(&r).unwrap();
    let keys = [
        "content",
        "citationMetadata",
        "groundingMetadata",
        "usageMetadata",
        "customMetadata",
        "logprobsResult",
        "inputTranscription",
        "outputTranscription",
        "modelVersion",
        "partial",
        "turnComplete",
        "interrupted",
        "sessionResumptionHandle",
        "errorCode",
        "errorMessage",
        "finishReason",
        "avgLogprobs",
    ];
    let positions: Vec<usize> = keys
        .iter()
        .map(|k| {
            s.find(&format!("\"{k}\":"))
                .unwrap_or_else(|| panic!("missing {k} in {s}"))
        })
        .collect();
    let mut sorted = positions.clone();
    sorted.sort_unstable();
    assert_eq!(positions, sorted, "field order: {s}");
    let back: LlmResponse = serde_json::from_str(&s).unwrap();
    assert_eq!(back, r);
    assert_eq!(
        serde_json::to_value(LlmResponse::default()).unwrap(),
        serde_json::json!({})
    );
}

#[test]
fn llm_request_debug_prints_tool_keys_only() {
    let mut tools: HashMap<String, Arc<dyn std::any::Any + Send + Sync>> = HashMap::new();
    tools.insert("my_tool".into(), Arc::new(42_u32));
    let req = LlmRequest {
        model: "gemini-2.0-flash".into(),
        tools,
        http_options: Some(HttpOptions {
            extras_request_provider: Some(Arc::new(|m| m)),
            ..Default::default()
        }),
        ..Default::default()
    };
    let dbg = format!("{req:?}");
    assert!(dbg.contains("my_tool"), "{dbg}");
    assert!(dbg.contains("gemini-2.0-flash"), "{dbg}");
    assert!(dbg.contains("<fn>"), "{dbg}");
    let cloned = req.clone();
    assert_eq!(cloned.tools.len(), 1);
}

#[test]
fn http_options_debug_redacts_header_values() {
    let mut headers = http::HeaderMap::new();
    headers.insert(
        http::header::AUTHORIZATION,
        http::HeaderValue::from_static("Bearer secret-token"),
    );
    headers.insert("x-custom", http::HeaderValue::from_static("hunter2"));
    let opts = HttpOptions {
        headers,
        ..Default::default()
    };
    let dbg = format!("{opts:?}");
    assert!(!dbg.contains("secret-token"), "{dbg}");
    assert!(!dbg.contains("hunter2"), "{dbg}");
    assert!(dbg.contains("authorization"), "{dbg}");
    assert!(dbg.contains("x-custom"), "{dbg}");
    assert!(dbg.contains("<redacted>"), "{dbg}");

    // Reached through LlmRequest's Debug as well.
    let req = LlmRequest {
        http_options: Some(opts),
        ..Default::default()
    };
    let dbg = format!("{req:?}");
    assert!(!dbg.contains("secret-token"), "{dbg}");
    assert!(dbg.contains("authorization"), "{dbg}");
}
