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

//! `responses/request_test.go`: the generation_config tests (Task 5; see `mod.rs`).
//!
//! Go compares `ResponseNewParams` structs; these compare the encoded params (Go's probe of the
//! same calls gave the bytes), so an omitted or misplaced field fails as it would on the wire.

use std::time::Duration;

use bytes::Bytes;
use gadk_genai::{
    AudioTranscriptionConfig, Content, GenerateContentConfig, GenerationConfigRoutingConfig,
    HarmCategory, ImageConfig, MediaResolution, ModelArmorConfig, ModelSelectionConfig,
    SafetySetting, Schema, ServiceTier, SpeechConfig, ThinkingConfig, ThinkingLevel, Type,
};
use gadk_model::HttpOptions;
use http::{HeaderMap, HeaderName, HeaderValue};
use pretty_assertions::assert_eq;

use crate::error::{ErrorKind, OpenAiError};
use crate::oaishared::ReasoningEffort;
use crate::parity_tests::model_openaimodel_internal_responses_tools::wire;
use crate::responses::generation_config::{
    apply_generation_config, service_tier_for, supported_config_fields,
};
use crate::responses::wire::{ResponseNewParams, ResponseServiceTier};
use crate::shared::config::{
    DYNAMIC_THINKING_BUDGET, IGNORED_HTTP_OPTION_FIELDS, SUPPORTED_THINKING_CONFIG_FIELDS,
    config_fields, http_option_fields, reasoning_effort_of_level, reject_unsupported_fields,
    request_timeout, thinking_config_fields,
};

fn cfg() -> GenerateContentConfig {
    GenerateContentConfig::default()
}

/// Go `applyGenerationConfig(params, cfg)` on fresh params, with no `HTTPOptions`.
fn apply(cfg: &GenerateContentConfig) -> (ResponseNewParams, Result<(), OpenAiError>) {
    apply_with(cfg, None)
}

/// [`apply`] with `cfg.HTTPOptions` (gadk carries it beside the config).
fn apply_with(
    cfg: &GenerateContentConfig,
    http_options: Option<&HttpOptions>,
) -> (ResponseNewParams, Result<(), OpenAiError>) {
    let mut params = ResponseNewParams::default();
    let result = apply_generation_config(&mut params, Some(cfg), http_options);
    (params, result)
}

fn thinking_cfg(thinking: ThinkingConfig) -> GenerateContentConfig {
    GenerateContentConfig {
        thinking_config: Some(thinking),
        ..cfg()
    }
}

fn level(level: ThinkingLevel) -> ThinkingConfig {
    ThinkingConfig {
        thinking_level: Some(level),
        ..ThinkingConfig::default()
    }
}

fn budget(budget: i32) -> ThinkingConfig {
    ThinkingConfig {
        thinking_budget: Some(budget),
        ..ThinkingConfig::default()
    }
}

fn timeout(d: Duration) -> HttpOptions {
    HttpOptions {
        timeout: Some(d),
        ..HttpOptions::default()
    }
}

/// Go `errors.Is(err, want)`: `want` `None` is Go's nil (no error).
fn assert_is(name: &str, result: &Result<(), OpenAiError>, want: Option<ErrorKind>) {
    match (result, want) {
        (Ok(()), None) => {}
        (Err(err), Some(kind)) if err.is(kind) => {}
        _ => panic!("{name}: applyGenerationConfig() error = {result:?}, wantErr {want:?}"),
    }
}

/// The shared assertion of Go's "rejected and named" tables.
fn assert_names(name: &str, result: &Result<(), OpenAiError>, field: &str) {
    let err = match result {
        Err(err) if err.is(ErrorKind::UnsupportedConfigField) => err,
        _ => panic!(
            "{name}: applyGenerationConfig() error = {result:?}, want ErrUnsupportedConfigField"
        ),
    };
    assert!(
        err.to_string().contains(field),
        "{name}: applyGenerationConfig() error = {err}, want it to name {field:?}"
    );
}

/// A `TestApplyGenerationConfig` case: name, config (`None` is Go's nil config, no HTTP
/// options either), the error kind wanted, and the encoded params wanted.
type ConfigCase = (
    &'static str,
    Option<GenerateContentConfig>,
    Option<ErrorKind>,
    Option<&'static str>,
);

#[test]
fn test_apply_generation_config() {
    let tests: Vec<ConfigCase> = vec![
        ("nil config", None, None, None),
        (
            "TopK not supported",
            Some(GenerateContentConfig {
                top_k: Some(5.0),
                ..cfg()
            }),
            Some(ErrorKind::TopKNotSupported),
            None,
        ),
        (
            "StopSequences not supported",
            Some(GenerateContentConfig {
                stop_sequences: vec!["stop".to_string()],
                ..cfg()
            }),
            Some(ErrorKind::StopSequencesNotSupported),
            None,
        ),
        (
            "Multiple candidates not supported",
            Some(GenerateContentConfig {
                candidate_count: 2,
                ..cfg()
            }),
            Some(ErrorKind::MultipleCandidatesNotSupported),
            None,
        ),
        (
            "Penalties not supported",
            Some(GenerateContentConfig {
                frequency_penalty: Some(0.5),
                ..cfg()
            }),
            Some(ErrorKind::PenaltiesNotSupported),
            None,
        ),
        (
            "Labels not supported",
            Some(GenerateContentConfig {
                labels: [("a".to_string(), "b".to_string())].into(),
                ..cfg()
            }),
            Some(ErrorKind::LabelsNotSupported),
            None,
        ),
        (
            "Safety settings not supported",
            Some(GenerateContentConfig {
                safety_settings: vec![SafetySetting::default()],
                ..cfg()
            }),
            Some(ErrorKind::SafetySettingsNotSupported),
            None,
        ),
        (
            "Unsupported MIME type",
            Some(GenerateContentConfig {
                response_mime_type: "image/png".to_string(),
                ..cfg()
            }),
            Some(ErrorKind::UnsupportedMimeType),
            None,
        ),
        (
            "success fully configured",
            Some(GenerateContentConfig {
                temperature: Some(0.8),
                top_p: Some(0.9),
                max_output_tokens: 100,
                response_logprobs: true,
                logprobs: Some(2),
                system_instruction: Some(Content::from_text("sys", "system")),
                response_mime_type: "application/json".to_string(),
                response_schema: Some(Schema {
                    r#type: Some(Type::Object),
                    ..Schema::default()
                }),
                ..cfg()
            }),
            None,
            // float64(float32(0.8)) and float64(float32(0.9)), as Go writes them.
            Some(
                r#"{"instructions":"sys","max_output_tokens":100,"temperature":0.800000011920929,"top_logprobs":2,"top_p":0.8999999761581421,"include":["message.output_text.logprobs"],"text":{"format":{"name":"adk_response","schema":{"additionalProperties":false,"properties":{},"required":[],"type":"object"},"strict":true,"type":"json_schema"}}}"#,
            ),
        ),
        (
            "success application/json without schema falls back to json_object",
            Some(GenerateContentConfig {
                response_mime_type: "application/json".to_string(),
                ..cfg()
            }),
            None,
            Some(r#"{"text":{"format":{"type":"json_object"}}}"#),
        ),
        (
            "success logprobs only",
            Some(GenerateContentConfig {
                response_logprobs: true,
                ..cfg()
            }),
            None,
            Some(r#"{"top_logprobs":1,"include":["message.output_text.logprobs"]}"#),
        ),
    ];
    for (name, config, want_err, want_params) in tests {
        let mut params = ResponseNewParams::default();
        let result = apply_generation_config(&mut params, config.as_ref(), None);
        assert_is(name, &result, want_err);
        if let Some(want) = want_params {
            assert_eq!(
                wire(&params),
                want,
                "{name}: applyGenerationConfig() params"
            );
        }
    }
}

// Each case sets exactly one previously-dropped field and expects the request to fail naming
// it.
#[test]
fn test_apply_generation_config_rejects_unsupported_fields() {
    let tests: Vec<(&str, GenerateContentConfig)> = vec![
        (
            "Seed",
            GenerateContentConfig {
                seed: Some(7),
                ..cfg()
            },
        ),
        (
            "RoutingConfig",
            GenerateContentConfig {
                routing_config: Some(GenerationConfigRoutingConfig::default()),
                ..cfg()
            },
        ),
        (
            "ModelSelectionConfig",
            GenerateContentConfig {
                model_selection_config: Some(ModelSelectionConfig::default()),
                ..cfg()
            },
        ),
        (
            "CachedContent",
            GenerateContentConfig {
                cached_content: "cached".to_string(),
                ..cfg()
            },
        ),
        (
            "ResponseModalities",
            GenerateContentConfig {
                response_modalities: vec!["AUDIO".to_string()],
                ..cfg()
            },
        ),
        (
            "MediaResolution",
            GenerateContentConfig {
                media_resolution: Some(MediaResolution::Low),
                ..cfg()
            },
        ),
        (
            "SpeechConfig",
            GenerateContentConfig {
                speech_config: Some(SpeechConfig::default()),
                ..cfg()
            },
        ),
        (
            "AudioTimestamp",
            GenerateContentConfig {
                audio_timestamp: true,
                ..cfg()
            },
        ),
        (
            "ImageConfig",
            GenerateContentConfig {
                image_config: Some(ImageConfig::default()),
                ..cfg()
            },
        ),
        (
            "EnableEnhancedCivicAnswers",
            GenerateContentConfig {
                enable_enhanced_civic_answers: Some(true),
                ..cfg()
            },
        ),
        (
            "ModelArmorConfig",
            GenerateContentConfig {
                model_armor_config: Some(ModelArmorConfig::default()),
                ..cfg()
            },
        ),
        (
            "AudioTranscriptionConfig",
            GenerateContentConfig {
                audio_transcription_config: Some(AudioTranscriptionConfig::default()),
                ..cfg()
            },
        ),
        (
            "ContinuationToken",
            GenerateContentConfig {
                continuation_token: Some(Bytes::from_static(b"token")),
                ..cfg()
            },
        ),
    ];
    for (field, config) in tests {
        let (_, result) = apply(&config);
        assert_names(field, &result, field);
    }
}

// A set field outside the supported lists is refused by name, whatever genai adds. Dropping a
// name from a list stands in for a field genai adds later.
// parity: adapted: Go swaps the package variables supportedConfigFields, SupportedHTTPOptionFields and SupportedThinkingConfigFields; gadk's lists are consts, so each narrowed list runs through the check apply_generation_config ends with (reject_unsupported_fields over the destructured field lists), and the real lists are shown to accept the same three configs
#[test]
fn test_apply_generation_config_refuses_fields_outside_the_supported_lists() {
    let top: Vec<&str> = supported_config_fields()
        .into_iter()
        .filter(|name| *name != "Temperature")
        .collect();
    let http_allowed: Vec<&str> = IGNORED_HTTP_OPTION_FIELDS.to_vec(); // SupportedHTTPOptionFields = nil
    let thinking = ["IncludeThoughts", "ThinkingBudget"];

    let temperature = GenerateContentConfig {
        temperature: Some(0.5),
        ..cfg()
    };
    let second = timeout(Duration::from_secs(1));
    let low = thinking_cfg(level(ThinkingLevel::Low));
    let tests: Vec<(&str, Result<(), OpenAiError>)> = vec![
        (
            "Temperature",
            reject_unsupported_fields("", &config_fields(&temperature, None), &top),
        ),
        (
            "HTTPOptions.Timeout",
            reject_unsupported_fields("HTTPOptions.", &http_option_fields(&second), &http_allowed),
        ),
        (
            "ThinkingConfig.ThinkingLevel",
            reject_unsupported_fields(
                "ThinkingConfig.",
                &thinking_config_fields(&level(ThinkingLevel::Low)),
                &thinking,
            ),
        ),
    ];
    for (field, result) in tests {
        let want = format!(
            "{}: {field}",
            ErrorKind::UnsupportedConfigField.sentinel_text()
        );
        match &result {
            Err(err) if err.is(ErrorKind::UnsupportedConfigField) && err.to_string() == want => {}
            _ => panic!("{field}: error = {result:?}, want {want:?}"),
        }
    }
    // With the real lists the same configs pass.
    assert!(apply(&temperature).1.is_ok());
    assert!(apply_with(&cfg(), Some(&second)).1.is_ok());
    assert!(apply(&low).1.is_ok());
    assert!(SUPPORTED_THINKING_CONFIG_FIELDS.contains(&"ThinkingLevel"));
}

// AutomaticContinuation has no effect on these APIs, so a config shared with a Gemini model
// that sets it must be accepted.
#[test]
fn test_apply_generation_config_accepts_automatic_continuation() {
    for on in [false, true] {
        let (_, result) = apply(&GenerateContentConfig {
            automatic_continuation: Some(on),
            ..cfg()
        });
        assert!(
            result.is_ok(),
            "{on}: applyGenerationConfig() error = {result:?}, want nil"
        );
    }
}

// ThinkingConfig maps onto effort-based reasoning; summary rides on IncludeThoughts alone.
#[test]
fn test_apply_generation_config_thinking_config() {
    let with_thoughts = |thinking: ThinkingConfig| ThinkingConfig {
        include_thoughts: true,
        ..thinking
    };
    let tests: Vec<(&str, ThinkingConfig, &str)> = vec![
        (
            "minimal level",
            level(ThinkingLevel::Minimal),
            r#"{"effort":"minimal"}"#,
        ),
        (
            "low level",
            level(ThinkingLevel::Low),
            r#"{"effort":"low"}"#,
        ),
        (
            "medium level",
            level(ThinkingLevel::Medium),
            r#"{"effort":"medium"}"#,
        ),
        (
            "high level",
            level(ThinkingLevel::High),
            r#"{"effort":"high"}"#,
        ),
        // Explicitly unspecified is distinct from unset, and resolves to medium.
        (
            "unspecified level",
            level(ThinkingLevel::Unspecified),
            r#"{"effort":"medium"}"#,
        ),
        // Zero is none rather than minimal.
        ("zero budget", budget(0), r#"{"effort":"none"}"#),
        ("positive budget", budget(2048), r#"{"effort":"medium"}"#),
        // -1 is genai's "you decide": send no effort at all.
        ("dynamic budget", budget(DYNAMIC_THINKING_BUDGET), "{}"),
        (
            "dynamic budget with thoughts",
            with_thoughts(budget(DYNAMIC_THINKING_BUDGET)),
            r#"{"summary":"auto"}"#,
        ),
        // A level wins over a budget.
        (
            "level wins over budget",
            ThinkingConfig {
                thinking_budget: Some(0),
                ..level(ThinkingLevel::Minimal)
            },
            r#"{"effort":"minimal"}"#,
        ),
        // IncludeThoughts is what asks for summaries, and only it.
        (
            "thoughts with level",
            with_thoughts(level(ThinkingLevel::High)),
            r#"{"effort":"high","summary":"auto"}"#,
        ),
        (
            "thoughts with budget",
            with_thoughts(budget(2048)),
            r#"{"effort":"medium","summary":"auto"}"#,
        ),
        // Thoughts alone leave the effort to the model.
        (
            "thoughts alone",
            with_thoughts(ThinkingConfig::default()),
            r#"{"summary":"auto"}"#,
        ),
    ];
    for (name, thinking, want) in tests {
        let (params, result) = apply(&thinking_cfg(thinking));
        assert!(
            result.is_ok(),
            "{name}: applyGenerationConfig() error = {result:?}, want nil"
        );
        assert_eq!(wire(&params.reasoning), want, "{name}: params.Reasoning");
        // A zero reasoning block is omitted from the request.
        let body = wire(&params);
        if want == "{}" {
            assert_eq!(body, "{}", "{name}");
        } else {
            assert_eq!(body, format!(r#"{{"reasoning":{want}}}"#), "{name}");
        }
    }
}

// A summary costs a verified organization, so no caller gets one without asking.
#[test]
fn test_apply_generation_config_omits_reasoning_summary_unless_asked() {
    let tests: Vec<(&str, ThinkingConfig)> = vec![
        ("high level", level(ThinkingLevel::High)),
        ("zero budget", budget(0)),
        ("positive budget", budget(4096)),
        ("dynamic budget", budget(DYNAMIC_THINKING_BUDGET)),
        (
            "level with thoughts off",
            ThinkingConfig {
                include_thoughts: false,
                ..level(ThinkingLevel::Low)
            },
        ),
    ];
    for (name, thinking) in tests {
        let (params, result) = apply(&thinking_cfg(thinking));
        assert!(
            result.is_ok(),
            "{name}: applyGenerationConfig() error = {result:?}, want nil"
        );
        assert_eq!(params.reasoning.summary, None, "{name}: Summary set");
        assert!(
            !wire(&params).contains("summary"),
            "{name}: summary on the wire"
        );
    }
}

// A budget below the dynamic sentinel means nothing in genai, so it is named.
#[test]
fn test_apply_generation_config_rejects_negative_thinking_budget() {
    let tests: Vec<(&str, ThinkingConfig)> = vec![
        ("budget alone", budget(-2)),
        // A level wins over a budget, and must not carry a nonsense one past unmentioned.
        (
            "budget behind a winning level",
            ThinkingConfig {
                thinking_budget: Some(-2),
                ..level(ThinkingLevel::Low)
            },
        ),
        (
            "budget behind thoughts",
            ThinkingConfig {
                include_thoughts: true,
                ..budget(-100)
            },
        ),
    ];
    for (name, thinking) in tests {
        let (_, result) = apply(&thinking_cfg(thinking));
        assert_names(name, &result, "ThinkingBudget");
    }
}

// THINKING_LEVEL_UNSPECIFIED stands in for medium only when no budget says something more
// specific.
#[test]
fn test_apply_generation_config_unspecified_level_yields_to_a_budget() {
    let with_budget = |l: ThinkingLevel, b: i32| ThinkingConfig {
        thinking_budget: Some(b),
        ..level(l)
    };
    let tests: Vec<(&str, ThinkingConfig, Option<ReasoningEffort>)> = vec![
        (
            "unspecified alone",
            level(ThinkingLevel::Unspecified),
            Some(ReasoningEffort::Medium),
        ),
        (
            "unspecified yields to zero budget",
            with_budget(ThinkingLevel::Unspecified, 0),
            Some(ReasoningEffort::None),
        ),
        (
            "unspecified yields to dynamic budget",
            with_budget(ThinkingLevel::Unspecified, DYNAMIC_THINKING_BUDGET),
            None,
        ),
        // A named level is a choice, so it keeps winning.
        (
            "named level still wins over zero budget",
            with_budget(ThinkingLevel::High, 0),
            Some(ReasoningEffort::High),
        ),
    ];
    for (name, thinking, want) in tests {
        let (params, result) = apply(&thinking_cfg(thinking));
        assert!(
            result.is_ok(),
            "{name}: applyGenerationConfig() error = {result:?}, want nil"
        );
        assert_eq!(params.reasoning.effort, want, "{name}: params.Reasoning");
        assert_eq!(params.reasoning.summary, None, "{name}: params.Reasoning");
    }
}

// A thinking level genai grows later must be named in an error.
#[test]
fn test_apply_generation_config_rejects_unknown_thinking_level() {
    let unknown = "EXHAUSTIVE".parse::<ThinkingLevel>().unwrap();
    let (_, result) = apply(&thinking_cfg(level(unknown)));
    assert_names("EXHAUSTIVE", &result, "EXHAUSTIVE");
    assert_eq!(
        result.unwrap_err().to_string(),
        r#"openai: unsupported generation config field: ThinkingConfig.ThinkingLevel "EXHAUSTIVE""#
    );
}

// Every level genai declares maps to an effort, reachable end to end.
// parity: adapted: Go's map length check becomes "an unknown level maps to nothing" (the gadk mapping is a function over the enum, whose match is exhaustive)
#[test]
fn test_reasoning_efforts_cover_every_thinking_level() {
    let levels = [
        ThinkingLevel::Unspecified,
        ThinkingLevel::Minimal,
        ThinkingLevel::Low,
        ThinkingLevel::Medium,
        ThinkingLevel::High,
    ];
    for level_value in &levels {
        assert!(
            reasoning_effort_of_level(level_value).is_some(),
            "ReasoningEfforts is missing genai.ThinkingLevel {level_value}"
        );
    }
    assert_eq!(
        reasoning_effort_of_level(&"EXHAUSTIVE".parse::<ThinkingLevel>().unwrap()),
        None,
        "ReasoningEfforts gained a level this test does not list"
    );
    // The mapping is reachable end to end, not just present.
    for level_value in levels {
        let want = reasoning_effort_of_level(&level_value);
        let (params, result) = apply(&thinking_cfg(level(level_value.clone())));
        assert!(
            result.is_ok(),
            "ThinkingLevel {level_value}: error = {result:?}, want nil"
        );
        assert_eq!(
            params.reasoning.effort, want,
            "ThinkingLevel {level_value} produced effort"
        );
    }
}

// A ThinkingConfig with nothing set asks for nothing, so nothing is sent and nothing is
// refused.
#[test]
fn test_apply_generation_config_accepts_empty_thinking_config() {
    let tests: Vec<(&str, ThinkingConfig)> = vec![
        ("zero value", ThinkingConfig::default()),
        (
            "thoughts explicitly off",
            ThinkingConfig {
                include_thoughts: false,
                ..ThinkingConfig::default()
            },
        ),
    ];
    for (name, thinking) in tests {
        let (params, result) = apply(&thinking_cfg(thinking));
        assert!(
            result.is_ok(),
            "{name}: applyGenerationConfig() error = {result:?}, want nil"
        );
        assert!(
            params.reasoning.is_zero(),
            "{name}: params.Reasoning = {:?}, want zero",
            params.reasoning
        );
        assert_eq!(wire(&params), "{}", "{name}");
    }
}

// Reasoning stays unset when the caller asks for nothing, so non-reasoning models are not sent
// a reasoning block they would reject.
#[test]
fn test_apply_generation_config_omits_reasoning_by_default() {
    let (params, result) = apply(&cfg());
    assert!(
        result.is_ok(),
        "applyGenerationConfig() error = {result:?}, want nil"
    );
    assert!(
        params.reasoning.is_zero(),
        "params.Reasoning = {:?}, want zero",
        params.reasoning
    );
    assert_eq!(wire(&params), "{}");
}

// Values the presence check would let through, on fields the package supports.
#[test]
fn test_apply_generation_config_rejects_untranslatable_values() {
    let tests: Vec<(&str, GenerateContentConfig, &str)> = vec![
        // Logprobs only sizes the list ResponseLogprobs asks for.
        (
            "orphan Logprobs",
            GenerateContentConfig {
                logprobs: Some(5),
                ..cfg()
            },
            "Logprobs",
        ),
        (
            "negative MaxOutputTokens",
            GenerateContentConfig {
                max_output_tokens: -1,
                ..cfg()
            },
            "MaxOutputTokens",
        ),
        (
            "negative CandidateCount",
            GenerateContentConfig {
                candidate_count: -1,
                ..cfg()
            },
            "CandidateCount",
        ),
    ];
    for (name, config, names) in tests {
        let (_, result) = apply(&config);
        assert_names(name, &result, names);
    }
}

// The values either side of the rejected ones still work.
// parity: adapted: Go's "empty StopSequences" is a non-nil empty slice; a gadk Vec has one empty state, the same as unset
#[test]
fn test_apply_generation_config_accepts_boundary_values() {
    let tests: Vec<(&str, GenerateContentConfig)> = vec![
        ("unset MaxOutputTokens", cfg()),
        (
            "positive MaxOutputTokens",
            GenerateContentConfig {
                max_output_tokens: 1,
                ..cfg()
            },
        ),
        ("unset CandidateCount", cfg()),
        // Zero and one both mean the single candidate Responses returns.
        (
            "single CandidateCount",
            GenerateContentConfig {
                candidate_count: 1,
                ..cfg()
            },
        ),
        // An empty list asks for no stop sequences.
        (
            "empty StopSequences",
            GenerateContentConfig {
                stop_sequences: Vec::new(),
                ..cfg()
            },
        ),
        (
            "Logprobs alongside ResponseLogprobs",
            GenerateContentConfig {
                response_logprobs: true,
                logprobs: Some(5),
                ..cfg()
            },
        ),
    ];
    for (name, config) in tests {
        let (_, result) = apply(&config);
        assert!(
            result.is_ok(),
            "{name}: applyGenerationConfig() error = {result:?}, want nil"
        );
    }
}

// Presence, not value: an off value still means the caller expected the knob to be wired up.
#[test]
fn test_apply_generation_config_rejects_explicit_off() {
    let (_, result) = apply(&GenerateContentConfig {
        enable_enhanced_civic_answers: Some(false),
        ..cfg()
    });
    assert_names("explicit off", &result, "EnableEnhancedCivicAnswers");
}

// Every tier genai names has a Responses equivalent; a tier genai adds later is named.
#[test]
fn test_apply_generation_config_service_tier() {
    let tests = [
        (ServiceTier::Flex, "flex"),
        (ServiceTier::Priority, "priority"),
        // genai's "standard" is what OpenAI calls "default".
        (ServiceTier::Standard, "default"),
        // genai calls this one "Default service tier, which is standard", so it lands on
        // default rather than auto.
        (ServiceTier::Unspecified, "default"),
    ];
    for (tier, want) in tests {
        let (params, result) = apply(&GenerateContentConfig {
            service_tier: Some(tier.clone()),
            ..cfg()
        });
        assert!(
            result.is_ok(),
            "{tier}: applyGenerationConfig() error = {result:?}, want nil"
        );
        assert_eq!(
            wire(&params),
            format!(r#"{{"service_tier":"{want}"}}"#),
            "{tier}: params.ServiceTier"
        );
    }

    let (_, result) = apply(&GenerateContentConfig {
        service_tier: Some("platinum".parse::<ServiceTier>().unwrap()),
        ..cfg()
    });
    assert_names("unknown tier", &result, "platinum");
    assert_eq!(
        result.unwrap_err().to_string(),
        r#"openai: unsupported generation config field: ServiceTier "platinum""#
    );
}

// serviceTiers must cover every tier genai declares.
// parity: adapted: Go's map length check becomes "an unknown tier maps to nothing" (the gadk mapping is a function over the enum, whose match is exhaustive)
#[test]
fn test_service_tiers_cover_every_genai_tier() {
    let tiers = [
        ServiceTier::Unspecified,
        ServiceTier::Standard,
        ServiceTier::Flex,
        ServiceTier::Priority,
    ];
    for tier in &tiers {
        assert!(
            service_tier_for(tier).is_some(),
            "serviceTiers is missing genai.ServiceTier {tier}"
        );
    }
    assert_eq!(
        service_tier_for(&"platinum".parse::<ServiceTier>().unwrap()),
        None,
        "serviceTiers gained a tier this test does not list"
    );
    assert_eq!(
        service_tier_for(&ServiceTier::Standard),
        Some(ResponseServiceTier::Default)
    );
}

// A positive Timeout is the one part of HTTPOptions that crosses to Responses.
#[test]
fn test_request_timeout_honors_a_positive_timeout() {
    let options = timeout(Duration::from_secs(45));
    let (_, result) = apply_with(&cfg(), Some(&options));
    assert!(
        result.is_ok(),
        "applyGenerationConfig() error = {result:?}, want nil: a timeout is honored"
    );
    assert_eq!(
        request_timeout(Some(&options)),
        Some(Duration::from_secs(45))
    );
}

// openai-go reads a zero timeout as "no deadline", so forwarding one would lift the caller's
// bound instead of applying it.
// parity: adapted: a `Duration` cannot be negative, so Go's `-time.Second` case has no counterpart; the zero case is ported
#[test]
fn test_apply_generation_config_rejects_non_positive_timeout() {
    let options = timeout(Duration::ZERO);
    let (_, result) = apply_with(&cfg(), Some(&options));
    assert_names("timeout 0s", &result, "Timeout");
    assert_eq!(
        result.unwrap_err().to_string(),
        "openai: unsupported generation config field: non-positive HTTPOptions.Timeout 0s"
    );
}

// Headers are ignored rather than refused, and never forwarded.
// parity: adapted: an `http::HeaderMap` normalizes names to lowercase, so Go's "Authorization" and "authorization" cases are the same map; both are still run
#[test]
fn test_headers_affect_neither_validation_nor_timeout() {
    let headers = [
        ("Authorization", "Bearer caller"),
        ("authorization", "Bearer lowercase"),
        ("X-Goog-Api-Key", "gemini-key"),
        ("X-Trace-Id", "harmless"),
    ];
    // Names, never values: a header value is exactly the thing not to log.
    for (name, value) in headers {
        let mut map = HeaderMap::new();
        map.insert(
            HeaderName::from_bytes(name.as_bytes()).unwrap(),
            HeaderValue::from_static(value),
        );
        let options = HttpOptions {
            headers: map,
            ..HttpOptions::default()
        };
        let (_, result) = apply_with(&cfg(), Some(&options));
        assert!(
            result.is_ok(),
            "headers [{name}]: error = {result:?}, want nil: headers are ignored, not refused"
        );
        assert_eq!(
            request_timeout(Some(&options)),
            None,
            "headers [{name}]: produced a timeout"
        );
    }
}

// parity: skipped TestApplyGenerationConfigRejectsEmptyResponseModalities: Go tells an explicit empty ResponseModalities slice from nil; gadk's Vec<String> has one empty state, which is unset (the non-empty case is in test_apply_generation_config_rejects_unsupported_fields)

// Every named error is checked before the sentinel.
#[test]
fn test_apply_generation_config_keeps_named_error_precedence() {
    let tests: Vec<(&str, GenerateContentConfig, ErrorKind)> = vec![
        (
            "TopK over Seed",
            GenerateContentConfig {
                top_k: Some(5.0),
                seed: Some(7),
                ..cfg()
            },
            ErrorKind::TopKNotSupported,
        ),
        (
            "Labels over orphan Logprobs",
            GenerateContentConfig {
                logprobs: Some(5),
                labels: [("team".to_string(), "search".to_string())].into(),
                ..cfg()
            },
            ErrorKind::LabelsNotSupported,
        ),
        (
            "SafetySettings over orphan Logprobs",
            GenerateContentConfig {
                logprobs: Some(5),
                safety_settings: vec![SafetySetting {
                    category: Some(HarmCategory::Harassment),
                    ..SafetySetting::default()
                }],
                ..cfg()
            },
            ErrorKind::SafetySettingsNotSupported,
        ),
        (
            "MIME type over orphan Logprobs",
            GenerateContentConfig {
                logprobs: Some(5),
                response_mime_type: "text/csv".to_string(),
                ..cfg()
            },
            ErrorKind::UnsupportedMimeType,
        ),
        // The budget has to be one applyThinkingConfig rejects, or there is no competing error.
        (
            "StopSequences over ThinkingConfig",
            GenerateContentConfig {
                stop_sequences: vec!["STOP".to_string()],
                thinking_config: Some(budget(-2)),
                ..cfg()
            },
            ErrorKind::StopSequencesNotSupported,
        ),
    ];
    for (name, config, want) in tests {
        let (_, result) = apply(&config);
        assert_is(name, &result, Some(want));
    }
}

// HTTPOptions fields both endpoints refuse, with realistic values.
// parity: adapted: gadk's HttpOptions has BaseURL, APIVersion, Headers, Timeout and ExtrasRequestProvider; Go's BaseURLResourceScope, ExtraBody and RetryOptions cases have no gadk field and are left out
#[test]
fn test_apply_generation_config_rejects_gemini_shaped_http_options() {
    let tests: Vec<(&str, HttpOptions)> = vec![
        (
            "BaseURL",
            HttpOptions {
                base_url: Some("https://example.test".to_string()),
                ..HttpOptions::default()
            },
        ),
        (
            "APIVersion",
            HttpOptions {
                api_version: Some("v1beta".to_string()),
                ..HttpOptions::default()
            },
        ),
        (
            "ExtrasRequestProvider",
            HttpOptions {
                extras_request_provider: Some(std::sync::Arc::new(|m| m)),
                ..HttpOptions::default()
            },
        ),
    ];
    for (field, options) in tests {
        let (_, result) = apply_with(&cfg(), Some(&options));
        assert_names(field, &result, field);
    }
}

// A name in SupportedConfigFields is accepted, so every translated field has to reach the
// params.
#[test]
fn test_translated_fields_are_not_rejected() {
    let tests: Vec<(&str, GenerateContentConfig, Option<HttpOptions>)> = vec![
        (
            "Temperature",
            GenerateContentConfig {
                temperature: Some(0.5),
                ..cfg()
            },
            None,
        ),
        (
            "TopP",
            GenerateContentConfig {
                top_p: Some(0.9),
                ..cfg()
            },
            None,
        ),
        (
            "MaxOutputTokens",
            GenerateContentConfig {
                max_output_tokens: 128,
                ..cfg()
            },
            None,
        ),
        (
            "SystemInstruction",
            GenerateContentConfig {
                system_instruction: Some(Content::from_text("be terse", "user")),
                ..cfg()
            },
            None,
        ),
        (
            "ResponseMIMEType",
            GenerateContentConfig {
                response_mime_type: "application/json".to_string(),
                ..cfg()
            },
            None,
        ),
        (
            "ResponseLogprobs",
            GenerateContentConfig {
                response_logprobs: true,
                ..cfg()
            },
            None,
        ),
        (
            "Logprobs",
            GenerateContentConfig {
                response_logprobs: true,
                logprobs: Some(3),
                ..cfg()
            },
            None,
        ),
        (
            "ThinkingConfig",
            thinking_cfg(level(ThinkingLevel::Low)),
            None,
        ),
        (
            "ServiceTier",
            GenerateContentConfig {
                service_tier: Some(ServiceTier::Flex),
                ..cfg()
            },
            None,
        ),
        (
            "HTTPOptions.Timeout",
            cfg(),
            Some(timeout(Duration::from_secs(30))),
        ),
    ];
    for (field, config, options) in tests {
        let (params, result) = apply_with(&config, options.as_ref());
        assert!(
            result.is_ok(),
            "{field}: applyGenerationConfig() error = {result:?}, want nil: {field} is listed as translated"
        );
        // HTTPOptions.Timeout lands on the call, not the params.
        if field == "HTTPOptions.Timeout" {
            assert!(
                request_timeout(options.as_ref()).is_some(),
                "RequestTimeout() = 0, want the configured bound"
            );
            continue;
        }
        assert_ne!(
            wire(&params),
            "{}",
            "{field} left the params untouched, so it is not translated"
        );
    }
}
