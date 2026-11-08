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

//! Per-request configuration validation (port of ADK-Go `config.go`
//! `validateGenerateContentConfig`).

use gadk_genai::GenerateContentConfig;
use gadk_model::HttpOptions;

use crate::config::Effort;
use crate::error::{AnthropicError, ErrorKind};
use crate::thinking::{
    is_thinking_enabled, validate_output_format_config, validate_thinking_config,
};
use crate::tools::{ReqTools, validate_tool_config, validate_tools};

/// Go `fmt.Errorf("%w: <detail>", ErrInvalidConfig)`.
fn invalid_config(detail: impl std::fmt::Display) -> AnthropicError {
    AnthropicError::new(ErrorKind::InvalidConfig, detail)
}

/// Go's `HTTPOptions` block of `validateGenerateContentConfig`, on gadk's
/// `LlmRequest.http_options`. The destructuring has no `..`, so a new `HttpOptions` field
/// fails to compile until it is classified here.
///
/// | Field | Disposition |
/// |---|---|
/// | `base_url` | rejected when non-empty (use `ClientConfig::base_url`) |
/// | `api_version` | rejected when non-empty |
/// | `headers` | ignored by design (backend-specific headers are never forwarded) |
/// | `timeout` | honoured when > 0 (Go's negative check is unrepresentable) |
/// | `extras_request_provider` | rejected |
///
/// Go's `BaseURLResourceScope`, `ExtraBody` and `RetryOptions` have no gadk field.
fn validate_http_options(http_options: Option<&HttpOptions>) -> Result<(), AnthropicError> {
    let Some(HttpOptions {
        base_url,
        api_version,
        headers: _,
        timeout: _,
        extras_request_provider,
    }) = http_options
    else {
        return Ok(());
    };
    if base_url.as_deref().is_some_and(|s| !s.is_empty()) {
        return Err(invalid_config(
            "HTTPOptions.BaseURL is not supported; set ClientConfig.BaseURL instead",
        ));
    }
    if api_version.as_deref().is_some_and(|s| !s.is_empty()) {
        return Err(invalid_config("HTTPOptions.APIVersion is not supported"));
    }
    if extras_request_provider.is_some() {
        return Err(invalid_config(
            "HTTPOptions.ExtrasRequestProvider is not supported",
        ));
    }
    Ok(())
}

/// Go `validateGenerateContentConfig`, with every rejection's Go text, in Go's order. Go's
/// `HTTPOptions` checks (between the tool-config and output-format checks) apply to
/// `LlmRequest.http_options`, which gadk keeps outside the config, and run even when `cfg` is
/// `None`: `base_url` → `HTTPOptions.BaseURL is not supported; set ClientConfig.BaseURL
/// instead`, `api_version` → `HTTPOptions.APIVersion is not supported`,
/// `extras_request_provider` → `HTTPOptions.ExtrasRequestProvider is not supported`; `headers`
/// are ignored and `timeout` is accepted.
///
/// The config is destructured without `..`: a new `GenerateContentConfig` field fails to
/// compile until it is classified as translated (checked here or by the request builder),
/// rejected or ignored, as Go's `doc.go` table classifies every field.
pub(crate) fn validate_generate_content_config(
    cfg: Option<&GenerateContentConfig>,
    req_tools: &ReqTools,
    effort: Option<Effort>,
    http_options: Option<&HttpOptions>,
) -> Result<(), AnthropicError> {
    let Some(cfg) = cfg else {
        return validate_http_options(http_options);
    };
    let GenerateContentConfig {
        system_instruction,
        temperature,
        top_p,
        top_k,
        candidate_count,
        max_output_tokens,
        stop_sequences: _, // translated (request builder)
        response_logprobs,
        logprobs,
        presence_penalty,
        frequency_penalty,
        seed,
        response_mime_type: _,   // validate_output_format_config
        response_schema: _,      // validate_output_format_config
        response_json_schema: _, // validate_output_format_config
        routing_config,
        model_selection_config,
        safety_settings,
        tools,
        tool_config,
        labels,
        cached_content,
        response_modalities,
        media_resolution,
        speech_config,
        audio_timestamp,
        thinking_config,
        image_config,
        enable_enhanced_civic_answers,
        model_armor_config,
        service_tier,
        audio_transcription_config,
        continuation_token,
        automatic_continuation: _, // ignored by design
    } = cfg;

    if let Some(si) = system_instruction {
        for part in &si.parts {
            if part.thought
                || part.inline_data.is_some()
                || part.file_data.is_some()
                || part.function_call.is_some()
                || part.function_response.is_some()
                || part.executable_code.is_some()
                || part.code_execution_result.is_some()
                || part.video_metadata.is_some()
            {
                return Err(invalid_config("SystemInstruction only supports text parts"));
            }
        }
    }

    if !is_thinking_enabled(thinking_config.as_ref())
        && effort.is_none()
        && temperature.is_some()
        && top_p.is_some()
    {
        return Err(invalid_config(
            "Temperature and TopP cannot both be set on Claude models",
        ));
    }
    if top_k.is_some_and(|k| k < 0.0) {
        return Err(invalid_config("TopK must be non-negative"));
    }
    if *candidate_count < 0 || *candidate_count > 1 {
        return Err(invalid_config(format!(
            "CandidateCount {candidate_count} is not supported; Claude only supports 1 candidate"
        )));
    }
    if *max_output_tokens < 0 {
        return Err(invalid_config("MaxOutputTokens must be non-negative"));
    }

    for modality in response_modalities {
        if !modality.trim().eq_ignore_ascii_case("TEXT") {
            return Err(invalid_config("ResponseModalities only supports TEXT"));
        }
    }

    validate_thinking_config(thinking_config.as_ref())?;

    let has_decls = validate_tools(tools)?;
    let has_any_tools = has_decls || !req_tools.is_empty();

    validate_tool_config(
        tool_config.as_ref(),
        has_any_tools,
        thinking_config.as_ref(),
    )?;

    validate_http_options(http_options)?;

    validate_output_format_config(Some(cfg))?;

    // Reject unsupported GenerateContentConfig fields with errors naming the field.
    if *response_logprobs {
        return Err(invalid_config("ResponseLogprobs is not supported"));
    }
    if logprobs.is_some() {
        return Err(invalid_config("Logprobs is not supported"));
    }
    if presence_penalty.is_some() {
        return Err(invalid_config("PresencePenalty is not supported"));
    }
    if frequency_penalty.is_some() {
        return Err(invalid_config("FrequencyPenalty is not supported"));
    }
    if seed.is_some() {
        return Err(invalid_config("Seed is not supported"));
    }
    if !safety_settings.is_empty() {
        return Err(invalid_config("SafetySettings is not supported"));
    }
    if !cached_content.is_empty() {
        return Err(invalid_config(
            "CachedContent is not supported; use ClientConfig.PromptCache instead",
        ));
    }
    if !labels.is_empty() {
        return Err(invalid_config("Labels is not supported"));
    }
    if media_resolution
        .as_ref()
        .is_some_and(|m| !m.as_str().is_empty())
    {
        return Err(invalid_config("MediaResolution is not supported"));
    }
    if speech_config.is_some() {
        return Err(invalid_config("SpeechConfig is not supported"));
    }
    if *audio_timestamp {
        return Err(invalid_config("AudioTimestamp is not supported"));
    }
    if image_config.is_some() {
        return Err(invalid_config("ImageConfig is not supported"));
    }
    if routing_config.is_some() {
        return Err(invalid_config("RoutingConfig is not supported"));
    }
    if model_selection_config.is_some() {
        return Err(invalid_config("ModelSelectionConfig is not supported"));
    }
    if model_armor_config.is_some() {
        return Err(invalid_config("ModelArmorConfig is not supported"));
    }
    if enable_enhanced_civic_answers.is_some() {
        return Err(invalid_config(
            "EnableEnhancedCivicAnswers is not supported",
        ));
    }
    if service_tier
        .as_ref()
        .is_some_and(|t| !t.as_str().is_empty())
    {
        return Err(invalid_config("ServiceTier is not supported"));
    }
    if audio_transcription_config.is_some() {
        return Err(invalid_config("AudioTranscriptionConfig is not supported"));
    }
    if continuation_token.as_ref().is_some_and(|t| !t.is_empty()) {
        return Err(invalid_config("ContinuationToken is not supported"));
    }
    // AutomaticContinuation is ignored by design.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;
    use bytes::Bytes;
    use gadk_genai::{
        AudioTranscriptionConfig, Blob, Content, FileData, FunctionCall, FunctionCallingConfig,
        FunctionCallingConfigMode, FunctionDeclaration, GenerationConfigRoutingConfig, ImageConfig,
        MediaResolution, ModelArmorConfig, ModelSelectionConfig, Part, SafetySetting, Schema,
        ServiceTier, SpeechConfig, ThinkingConfig, Tool, ToolConfig, Type, VideoMetadata,
    };
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use std::any::Any;
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::time::Duration;

    fn validate(cfg: &GenerateContentConfig) -> Result<(), AnthropicError> {
        validate_generate_content_config(Some(cfg), &ReqTools::new(), None, None)
    }

    fn system(part: Part) -> GenerateContentConfig {
        GenerateContentConfig {
            system_instruction: Some(Content {
                parts: vec![Part::from_text("You are helpful."), part],
                role: "user".into(),
            }),
            ..Default::default()
        }
    }

    fn http(opts: HttpOptions) -> Option<HttpOptions> {
        Some(opts)
    }

    #[test]
    fn rejected_fields_name_the_field() {
        // Go TestGenerateContentConfigValidation's rejectedCases, as whole texts. Go's
        // http_options_base_url_resource_scope, _extra_body, _retry_options and
        // _negative_timeout have no gadk field (unrepresentable).
        let cases: Vec<(&str, GenerateContentConfig, Option<HttpOptions>, &str)> = vec![
            (
                "temperature_and_top_p_together",
                GenerateContentConfig {
                    temperature: Some(0.5),
                    top_p: Some(0.9),
                    ..Default::default()
                },
                None,
                "Temperature and TopP cannot both be set on Claude models",
            ),
            (
                "negative_top_k",
                GenerateContentConfig {
                    top_k: Some(-1.0),
                    ..Default::default()
                },
                None,
                "TopK must be non-negative",
            ),
            (
                "candidate_count_gt_1",
                GenerateContentConfig {
                    candidate_count: 2,
                    ..Default::default()
                },
                None,
                "CandidateCount 2 is not supported; Claude only supports 1 candidate",
            ),
            (
                "candidate_count_negative",
                GenerateContentConfig {
                    candidate_count: -1,
                    ..Default::default()
                },
                None,
                "CandidateCount -1 is not supported; Claude only supports 1 candidate",
            ),
            (
                "negative_max_output_tokens",
                GenerateContentConfig {
                    max_output_tokens: -10,
                    ..Default::default()
                },
                None,
                "MaxOutputTokens must be non-negative",
            ),
            (
                "non_text_system_instruction",
                system(Part {
                    inline_data: Some(Blob {
                        data: Bytes::from_static(b"\x89PNG"),
                        mime_type: "image/png".into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                None,
                "SystemInstruction only supports text parts",
            ),
            (
                "non_text_response_modality",
                GenerateContentConfig {
                    response_modalities: vec!["AUDIO".into()],
                    ..Default::default()
                },
                None,
                "ResponseModalities only supports TEXT",
            ),
            (
                "http_options_base_url",
                GenerateContentConfig::default(),
                http(HttpOptions {
                    base_url: Some("https://other.example.com".into()),
                    ..Default::default()
                }),
                "HTTPOptions.BaseURL is not supported; set ClientConfig.BaseURL instead",
            ),
            (
                "http_options_api_version",
                GenerateContentConfig::default(),
                http(HttpOptions {
                    api_version: Some("v1beta".into()),
                    ..Default::default()
                }),
                "HTTPOptions.APIVersion is not supported",
            ),
            (
                "http_options_extras_request_provider",
                GenerateContentConfig::default(),
                http(HttpOptions {
                    extras_request_provider: Some(Arc::new(|b| b)),
                    ..Default::default()
                }),
                "HTTPOptions.ExtrasRequestProvider is not supported",
            ),
            (
                "response_mime_type_without_schema",
                GenerateContentConfig {
                    response_mime_type: "application/json".into(),
                    ..Default::default()
                },
                None,
                "ResponseMIMEType \"application/json\" requires ResponseSchema or ResponseJsonSchema",
            ),
            (
                "response_mime_type_non_json_with_schema",
                GenerateContentConfig {
                    response_mime_type: "text/plain".into(),
                    response_schema: Some(Schema {
                        r#type: Some(Type::Object),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                None,
                "ResponseMIMEType \"text/plain\" cannot be combined with ResponseSchema or ResponseJsonSchema",
            ),
            (
                "response_schema_and_response_json_schema_both_set",
                GenerateContentConfig {
                    response_schema: Some(Schema {
                        r#type: Some(Type::Object),
                        ..Default::default()
                    }),
                    response_json_schema: Some(json!({"type": "object"})),
                    ..Default::default()
                },
                None,
                "ResponseSchema and ResponseJsonSchema cannot both be set",
            ),
            (
                "response_logprobs",
                GenerateContentConfig {
                    response_logprobs: true,
                    ..Default::default()
                },
                None,
                "ResponseLogprobs is not supported",
            ),
            (
                "logprobs",
                GenerateContentConfig {
                    logprobs: Some(1),
                    ..Default::default()
                },
                None,
                "Logprobs is not supported",
            ),
            (
                "presence_penalty",
                GenerateContentConfig {
                    presence_penalty: Some(0.5),
                    ..Default::default()
                },
                None,
                "PresencePenalty is not supported",
            ),
            (
                "frequency_penalty",
                GenerateContentConfig {
                    frequency_penalty: Some(0.5),
                    ..Default::default()
                },
                None,
                "FrequencyPenalty is not supported",
            ),
            (
                "seed",
                GenerateContentConfig {
                    seed: Some(1),
                    ..Default::default()
                },
                None,
                "Seed is not supported",
            ),
            (
                "safety_settings",
                GenerateContentConfig {
                    safety_settings: vec![SafetySetting::default()],
                    ..Default::default()
                },
                None,
                "SafetySettings is not supported",
            ),
            (
                "cached_content",
                GenerateContentConfig {
                    cached_content: "caches/123".into(),
                    ..Default::default()
                },
                None,
                "CachedContent is not supported; use ClientConfig.PromptCache instead",
            ),
            (
                "labels",
                GenerateContentConfig {
                    labels: BTreeMap::from([("env".to_string(), "prod".to_string())]),
                    ..Default::default()
                },
                None,
                "Labels is not supported",
            ),
            (
                "media_resolution",
                GenerateContentConfig {
                    media_resolution: Some(MediaResolution::High),
                    ..Default::default()
                },
                None,
                "MediaResolution is not supported",
            ),
            (
                "speech_config",
                GenerateContentConfig {
                    speech_config: Some(SpeechConfig::default()),
                    ..Default::default()
                },
                None,
                "SpeechConfig is not supported",
            ),
            (
                "audio_timestamp",
                GenerateContentConfig {
                    audio_timestamp: true,
                    ..Default::default()
                },
                None,
                "AudioTimestamp is not supported",
            ),
            (
                "image_config",
                GenerateContentConfig {
                    image_config: Some(ImageConfig::default()),
                    ..Default::default()
                },
                None,
                "ImageConfig is not supported",
            ),
            (
                "routing_config",
                GenerateContentConfig {
                    routing_config: Some(GenerationConfigRoutingConfig::default()),
                    ..Default::default()
                },
                None,
                "RoutingConfig is not supported",
            ),
            (
                "model_selection_config",
                GenerateContentConfig {
                    model_selection_config: Some(ModelSelectionConfig::default()),
                    ..Default::default()
                },
                None,
                "ModelSelectionConfig is not supported",
            ),
            (
                "model_armor_config",
                GenerateContentConfig {
                    model_armor_config: Some(ModelArmorConfig::default()),
                    ..Default::default()
                },
                None,
                "ModelArmorConfig is not supported",
            ),
            (
                "enable_enhanced_civic_answers",
                GenerateContentConfig {
                    enable_enhanced_civic_answers: Some(true),
                    ..Default::default()
                },
                None,
                "EnableEnhancedCivicAnswers is not supported",
            ),
            (
                "service_tier",
                GenerateContentConfig {
                    service_tier: Some(ServiceTier::Flex),
                    ..Default::default()
                },
                None,
                "ServiceTier is not supported",
            ),
            (
                "audio_transcription_config",
                GenerateContentConfig {
                    audio_transcription_config: Some(AudioTranscriptionConfig::default()),
                    ..Default::default()
                },
                None,
                "AudioTranscriptionConfig is not supported",
            ),
            (
                "continuation_token",
                GenerateContentConfig {
                    continuation_token: Some(Bytes::from_static(b"tok")),
                    ..Default::default()
                },
                None,
                "ContinuationToken is not supported",
            ),
        ];
        for (name, cfg, opts, want) in cases {
            let err =
                validate_generate_content_config(Some(&cfg), &ReqTools::new(), None, opts.as_ref())
                    .expect_err(name);
            assert_eq!(
                err.to_string(),
                format!("anthropicmodel: invalid configuration: {want}"),
                "{name}"
            );
            assert_eq!(err.kinds(), &[ErrorKind::InvalidConfig], "{name}");
        }
    }

    #[test]
    fn every_non_text_system_instruction_part_is_rejected() {
        let parts = [
            Part {
                thought: true,
                text: "t".into(),
                ..Default::default()
            },
            Part {
                inline_data: Some(Blob::default()),
                ..Default::default()
            },
            Part {
                file_data: Some(FileData::default()),
                ..Default::default()
            },
            Part {
                function_call: Some(FunctionCall::default()),
                ..Default::default()
            },
            Part {
                function_response: Some(Default::default()),
                ..Default::default()
            },
            Part {
                executable_code: Some(Default::default()),
                ..Default::default()
            },
            Part {
                code_execution_result: Some(Default::default()),
                ..Default::default()
            },
            Part {
                video_metadata: Some(VideoMetadata::default()),
                ..Default::default()
            },
        ];
        for part in parts {
            let err = validate(&system(part.clone())).unwrap_err();
            assert_eq!(
                err.to_string(),
                "anthropicmodel: invalid configuration: SystemInstruction only supports text parts",
                "{part:?}"
            );
        }
    }

    #[test]
    fn ignored_fields_do_not_error() {
        // Go TestGenerateContentConfigValidation/ignored_fields_do_not_error_or_leak_headers
        // (the validation half; Task 12 checks that the header is not sent).
        let cfg = GenerateContentConfig {
            response_modalities: vec![" text ".into(), "TEXT".into()],
            automatic_continuation: Some(true),
            ..Default::default()
        };
        let mut headers = http::HeaderMap::new();
        headers.insert("x-gemini-only", "should-not-be-sent".parse().unwrap());
        let opts = HttpOptions {
            timeout: Some(Duration::from_secs(5)),
            headers,
            ..Default::default()
        };
        assert!(
            validate_generate_content_config(Some(&cfg), &ReqTools::new(), None, Some(&opts))
                .is_ok()
        );
    }

    #[test]
    fn empty_go_strings_are_unset() {
        let opts = HttpOptions {
            base_url: Some(String::new()),
            api_version: Some(String::new()),
            ..Default::default()
        };
        let cfg = GenerateContentConfig {
            media_resolution: Some(MediaResolution::Unknown(String::new())),
            service_tier: Some(ServiceTier::Unknown(String::new())),
            continuation_token: Some(Bytes::new()),
            ..Default::default()
        };
        assert!(
            validate_generate_content_config(Some(&cfg), &ReqTools::new(), None, Some(&opts))
                .is_ok()
        );
    }

    #[test]
    fn http_options_are_checked_without_a_config() {
        let opts = HttpOptions {
            base_url: Some("https://other.example.com".into()),
            ..Default::default()
        };
        let err = validate_generate_content_config(None, &ReqTools::new(), None, Some(&opts))
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "anthropicmodel: invalid configuration: HTTPOptions.BaseURL is not supported; set ClientConfig.BaseURL instead"
        );
        assert!(validate_generate_content_config(None, &ReqTools::new(), None, None).is_ok());
        assert!(
            validate_generate_content_config(
                None,
                &ReqTools::new(),
                None,
                Some(&HttpOptions::default())
            )
            .is_ok()
        );
    }

    #[test]
    fn temperature_and_top_p_allowed_with_thinking_or_effort() {
        // Go: the pair is rejected only when thinking and Effort are both off (the request
        // builder then omits them with a warning).
        let mut cfg = GenerateContentConfig {
            temperature: Some(0.5),
            top_p: Some(0.9),
            thinking_config: Some(ThinkingConfig {
                thinking_budget: Some(1024),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(validate(&cfg).is_ok());
        cfg.thinking_config = Some(ThinkingConfig {
            thinking_budget: Some(0),
            ..Default::default()
        });
        assert!(validate(&cfg).is_err());
        assert!(
            validate_generate_content_config(
                Some(&cfg),
                &ReqTools::new(),
                Some(Effort::High),
                None
            )
            .is_ok()
        );
    }

    #[test]
    fn accepted_numeric_boundaries() {
        for cfg in [
            GenerateContentConfig {
                top_k: Some(0.0),
                candidate_count: 1,
                max_output_tokens: 0,
                ..Default::default()
            },
            GenerateContentConfig {
                candidate_count: 0,
                temperature: Some(0.0),
                ..Default::default()
            },
        ] {
            assert!(validate(&cfg).is_ok(), "{cfg:?}");
        }
    }

    #[test]
    fn checks_run_in_go_order() {
        // SystemInstruction before Temperature/TopP.
        let mut cfg = system(Part {
            thought: true,
            ..Default::default()
        });
        cfg.temperature = Some(0.5);
        cfg.top_p = Some(0.5);
        assert!(
            validate(&cfg)
                .unwrap_err()
                .to_string()
                .contains("SystemInstruction")
        );

        // ThinkingConfig before Tools.
        let cfg = GenerateContentConfig {
            thinking_config: Some(ThinkingConfig::default()),
            tools: vec![Tool::default()],
            ..Default::default()
        };
        assert!(
            validate(&cfg)
                .unwrap_err()
                .to_string()
                .contains("ThinkingBudget")
        );

        // Tools before ToolConfig.
        let cfg = GenerateContentConfig {
            tools: vec![Tool::default()],
            tool_config: Some(ToolConfig {
                retrieval_config: Some(Default::default()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(
            validate(&cfg)
                .unwrap_err()
                .to_string()
                .contains("FunctionDeclarations")
        );

        // ToolConfig before HTTPOptions, HTTPOptions before the output format, the output
        // format before the unsupported fields.
        let cfg = GenerateContentConfig {
            tool_config: Some(ToolConfig {
                retrieval_config: Some(Default::default()),
                ..Default::default()
            }),
            response_mime_type: "application/json".into(),
            seed: Some(1),
            ..Default::default()
        };
        let opts = HttpOptions {
            api_version: Some("v1".into()),
            ..Default::default()
        };
        let run = |cfg: &GenerateContentConfig, opts: Option<&HttpOptions>| {
            validate_generate_content_config(Some(cfg), &ReqTools::new(), None, opts)
                .unwrap_err()
                .to_string()
        };
        assert!(run(&cfg, Some(&opts)).contains("RetrievalConfig"));
        let cfg = GenerateContentConfig {
            tool_config: None,
            ..cfg
        };
        assert!(run(&cfg, Some(&opts)).contains("APIVersion"));
        assert!(run(&cfg, None).contains("ResponseMIMEType"));
        let cfg = GenerateContentConfig {
            response_mime_type: String::new(),
            ..cfg
        };
        assert!(run(&cfg, None).contains("Seed"));
    }

    #[test]
    fn tool_validation_uses_request_tools() {
        // Mode ANY needs a declared function tool, or a request tool.
        let cfg = GenerateContentConfig {
            tool_config: Some(ToolConfig {
                function_calling_config: Some(FunctionCallingConfig {
                    mode: Some(FunctionCallingConfigMode::Any),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(
            validate(&cfg)
                .unwrap_err()
                .to_string()
                .contains("Mode ANY requires at least one declared function tool")
        );
        let mut req_tools = ReqTools::new();
        req_tools.insert("t".into(), Arc::new(()) as Arc<dyn Any + Send + Sync>);
        assert!(validate_generate_content_config(Some(&cfg), &req_tools, None, None).is_ok());

        let with_decl = GenerateContentConfig {
            tools: vec![Tool {
                function_declarations: vec![FunctionDeclaration {
                    name: "fn".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..cfg
        };
        assert!(validate(&with_decl).is_ok());
    }

    #[test]
    fn tool_type_errors_keep_both_sentinels() {
        let cfg = GenerateContentConfig {
            tools: vec![Tool {
                google_search: Some(Default::default()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let err = validate(&cfg).unwrap_err();
        assert!(err.is(ErrorKind::InvalidConfig) && err.is(ErrorKind::UnsupportedToolType));
    }

    #[test]
    fn no_config_is_valid() {
        assert!(validate(&GenerateContentConfig::default()).is_ok());
    }
}
