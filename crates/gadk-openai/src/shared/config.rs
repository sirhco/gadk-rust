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

//! The generation-config rules both APIs share (port of ADK-Go `internal/shared/config.go`).
//!
//! Go keeps `HTTPOptions` inside `GenerateContentConfig`; gadk carries it beside the config in
//! `LlmRequest::http_options`, so the functions that read it take it as a second argument
//! (Go's `cfg == nil` is `cfg: None` with `http_options: None`). Go's reflection over struct
//! fields becomes exhaustive destructuring ([`config_fields`], [`thinking_config_fields`],
//! [`http_option_fields`]) listing `(Go field name, is set)` in genai's declaration order, so a
//! field genai adds fails to compile here.

use std::time::Duration;

use gadk_genai::{GenerateContentConfig, ThinkingConfig, ThinkingLevel};
use gadk_model::HttpOptions;
use gadk_platform::go_quote;

use crate::error::{ErrorKind, OpenAiError};
use crate::oaishared::ReasoningEffort;

/// Go `IgnoredHTTPOptionFields`: the `HTTPOptions` fields neither API translates nor rejects.
pub(crate) const IGNORED_HTTP_OPTION_FIELDS: &[&str] = &["Headers"];

/// Go `SupportedHTTPOptionFields`: the `HTTPOptions` fields both APIs support.
pub(crate) const SUPPORTED_HTTP_OPTION_FIELDS: &[&str] = &["Timeout"];

/// Go `DynamicThinkingBudget`: genai's "let the model size its own thinking".
pub(crate) const DYNAMIC_THINKING_BUDGET: i32 = -1;

/// Go `SupportedConfigFields`: the `GenerateContentConfig` fields both APIs support (Go field
/// names). Each API adds the ones only it supports.
pub(crate) const SUPPORTED_CONFIG_FIELDS: &[&str] = &[
    "HTTPOptions",
    "SystemInstruction",
    "Temperature",
    "TopP",
    "MaxOutputTokens",
    "ResponseLogprobs",
    "Logprobs",
    "ResponseMIMEType",
    "ResponseSchema",
    "ResponseJsonSchema",
    "Tools",
    "ToolConfig",
    "ThinkingConfig",
    "ServiceTier",
    "CandidateCount",
    "AutomaticContinuation",
];

/// Go `SupportedThinkingConfigFields`.
pub(crate) const SUPPORTED_THINKING_CONFIG_FIELDS: &[&str] =
    &["IncludeThoughts", "ThinkingBudget", "ThinkingLevel"];

/// Go `RequestTimeout`: the caller's `http_options.timeout`, or `None` for none (a zero
/// duration is none, as Go's non-positive one is).
pub(crate) fn request_timeout(http_options: Option<&HttpOptions>) -> Option<Duration> {
    http_options
        .and_then(|options| options.timeout)
        .filter(|timeout| !timeout.is_zero())
}

/// Go `ReasoningEfforts[level]`: every genai thinking level's reasoning effort
/// (`THINKING_LEVEL_UNSPECIFIED` → medium, `MINIMAL` → minimal, `LOW` → low, `MEDIUM` → medium,
/// `HIGH` → high); `None` for a level the map does not know (`ThinkingLevel::Unknown`).
pub(crate) fn reasoning_effort_of_level(level: &ThinkingLevel) -> Option<ReasoningEffort> {
    match level {
        ThinkingLevel::Unspecified => Some(ReasoningEffort::Medium),
        ThinkingLevel::Minimal => Some(ReasoningEffort::Minimal),
        ThinkingLevel::Low => Some(ReasoningEffort::Low),
        ThinkingLevel::Medium => Some(ReasoningEffort::Medium),
        ThinkingLevel::High => Some(ReasoningEffort::High),
        ThinkingLevel::Unknown(_) => None,
    }
}

/// Go `ReasoningEffortFor`: the reasoning effort for a thinking config; `Ok(None)` is Go's
/// `""` (send none). A budget below [`DYNAMIC_THINKING_BUDGET`] is
/// `ErrUnsupportedConfigField: ThinkingConfig.ThinkingBudget <n>`; an unknown level
/// `ErrUnsupportedConfigField: ThinkingConfig.ThinkingLevel "<level>"`.
pub(crate) fn reasoning_effort_for(
    cfg: Option<&ThinkingConfig>,
) -> Result<Option<ReasoningEffort>, OpenAiError> {
    let Some(cfg) = cfg else {
        return Ok(None);
    };
    if let Some(budget) = cfg.thinking_budget
        && budget < DYNAMIC_THINKING_BUDGET
    {
        // Rejected before the level is read: a level set alongside it wins and would
        // otherwise carry the nonsense value through unmentioned.
        return Err(OpenAiError::new(
            ErrorKind::UnsupportedConfigField,
            format!("ThinkingConfig.ThinkingBudget {budget}"),
        ));
    }
    // A level outranks a budget only when it names one: UNSPECIFIED with a budget set defers
    // to the budget. Go's empty level is `None` (or an empty unknown value).
    let level = cfg
        .thinking_level
        .as_ref()
        .filter(|level| !level.as_str().is_empty())
        .filter(|level| !(**level == ThinkingLevel::Unspecified && cfg.thinking_budget.is_some()));
    if let Some(level) = level {
        return match reasoning_effort_of_level(level) {
            Some(effort) => Ok(Some(effort)),
            None => Err(OpenAiError::new(
                ErrorKind::UnsupportedConfigField,
                format!("ThinkingConfig.ThinkingLevel {}", go_quote(level.as_str())),
            )),
        };
    }
    Ok(match cfg.thinking_budget {
        // "Do not think" is the none effort, not minimal (the least thinking).
        Some(0) => Some(ReasoningEffort::None),
        // The model's own choice: send no effort.
        Some(DYNAMIC_THINKING_BUDGET) | None => None,
        Some(_) => Some(ReasoningEffort::Medium),
    })
}

/// Go `RejectUntranslatableValues`: `Logprobs without ResponseLogprobs`, `negative
/// MaxOutputTokens`, `negative CandidateCount`, then an `HTTPOptions` field outside
/// [`SUPPORTED_HTTP_OPTION_FIELDS`] + [`IGNORED_HTTP_OPTION_FIELDS`]
/// (`HTTPOptions.<Field>`), then a non-positive timeout (unrepresentable in a `Duration`
/// except zero: `non-positive HTTPOptions.Timeout 0s`), each wrapping
/// `ErrUnsupportedConfigField`.
pub(crate) fn reject_untranslatable_values(
    cfg: &GenerateContentConfig,
    http_options: Option<&HttpOptions>,
) -> Result<(), OpenAiError> {
    if cfg.logprobs.is_some() && !cfg.response_logprobs {
        return Err(OpenAiError::new(
            ErrorKind::UnsupportedConfigField,
            "Logprobs without ResponseLogprobs",
        ));
    }
    if cfg.max_output_tokens < 0 {
        return Err(OpenAiError::new(
            ErrorKind::UnsupportedConfigField,
            "negative MaxOutputTokens",
        ));
    }
    if cfg.candidate_count < 0 {
        return Err(OpenAiError::new(
            ErrorKind::UnsupportedConfigField,
            "negative CandidateCount",
        ));
    }
    if let Some(options) = http_options {
        let allowed: Vec<&str> = SUPPORTED_HTTP_OPTION_FIELDS
            .iter()
            .chain(IGNORED_HTTP_OPTION_FIELDS)
            .copied()
            .collect();
        reject_unsupported_fields("HTTPOptions.", &http_option_fields(options), &allowed)?;
        // A zero timeout is "no deadline" to the transport, so forwarding it would lift the
        // caller's bound rather than apply it. A `Duration` cannot be negative.
        if options.timeout.is_some_and(|timeout| timeout.is_zero()) {
            return Err(OpenAiError::new(
                ErrorKind::UnsupportedConfigField,
                "non-positive HTTPOptions.Timeout 0s",
            ));
        }
    }
    Ok(())
}

/// Go `RejectUnsupportedConfigFields`: the first field the caller set ([`config_fields`]) not
/// in `supported`, then the first set `ThinkingConfig` field not in
/// [`SUPPORTED_THINKING_CONFIG_FIELDS`], each `ErrUnsupportedConfigField: <prefix><Field>`.
pub(crate) fn reject_unsupported_config_fields(
    cfg: &GenerateContentConfig,
    http_options: Option<&HttpOptions>,
    supported: &[&str],
) -> Result<(), OpenAiError> {
    reject_unsupported_fields("", &config_fields(cfg, http_options), supported)?;
    match &cfg.thinking_config {
        Some(thinking) => reject_unsupported_fields(
            "ThinkingConfig.",
            &thinking_config_fields(thinking),
            SUPPORTED_THINKING_CONFIG_FIELDS,
        ),
        None => Ok(()),
    }
}

/// Go `rejectUnsupportedFields` over a field list: the first `(name, true)` whose name is not
/// in `supported` is `ErrUnsupportedConfigField: <prefix><name>`.
pub(crate) fn reject_unsupported_fields(
    prefix: &str,
    fields: &[(&'static str, bool)],
    supported: &[&str],
) -> Result<(), OpenAiError> {
    match fields
        .iter()
        .find(|(name, set)| *set && !supported.contains(name))
    {
        Some((name, _)) => Err(OpenAiError::new(
            ErrorKind::UnsupportedConfigField,
            format!("{prefix}{name}"),
        )),
        None => Ok(()),
    }
}

/// Every `GenerateContentConfig` field as `(Go field name, set)`, in genai's declaration order
/// (`HTTPOptions` first: `http_options.is_some()`), by exhaustive destructuring. "Set" is Go's
/// `!IsZero()`: a non-`None` option, a non-empty string, slice or map, a non-zero number, or
/// `true`.
pub(crate) fn config_fields(
    cfg: &GenerateContentConfig,
    http_options: Option<&HttpOptions>,
) -> Vec<(&'static str, bool)> {
    let GenerateContentConfig {
        system_instruction,
        temperature,
        top_p,
        top_k,
        candidate_count,
        max_output_tokens,
        stop_sequences,
        response_logprobs,
        logprobs,
        presence_penalty,
        frequency_penalty,
        seed,
        response_mime_type,
        response_schema,
        response_json_schema,
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
        automatic_continuation,
        generation_output_config,
        store,
    } = cfg;
    vec![
        ("HTTPOptions", http_options.is_some()),
        ("SystemInstruction", system_instruction.is_some()),
        ("Temperature", temperature.is_some()),
        ("TopP", top_p.is_some()),
        ("TopK", top_k.is_some()),
        ("CandidateCount", *candidate_count != 0),
        ("MaxOutputTokens", *max_output_tokens != 0),
        ("StopSequences", !stop_sequences.is_empty()),
        ("ResponseLogprobs", *response_logprobs),
        ("Logprobs", logprobs.is_some()),
        ("PresencePenalty", presence_penalty.is_some()),
        ("FrequencyPenalty", frequency_penalty.is_some()),
        ("Seed", seed.is_some()),
        ("ResponseMIMEType", !response_mime_type.is_empty()),
        ("ResponseSchema", response_schema.is_some()),
        ("ResponseJsonSchema", response_json_schema.is_some()),
        ("RoutingConfig", routing_config.is_some()),
        ("ModelSelectionConfig", model_selection_config.is_some()),
        ("SafetySettings", !safety_settings.is_empty()),
        ("Tools", !tools.is_empty()),
        ("ToolConfig", tool_config.is_some()),
        ("Labels", !labels.is_empty()),
        ("CachedContent", !cached_content.is_empty()),
        ("ResponseModalities", !response_modalities.is_empty()),
        (
            "MediaResolution",
            media_resolution
                .as_ref()
                .is_some_and(|v| !v.as_str().is_empty()),
        ),
        ("SpeechConfig", speech_config.is_some()),
        ("AudioTimestamp", *audio_timestamp),
        ("ThinkingConfig", thinking_config.is_some()),
        ("ImageConfig", image_config.is_some()),
        (
            "EnableEnhancedCivicAnswers",
            enable_enhanced_civic_answers.is_some(),
        ),
        ("ModelArmorConfig", model_armor_config.is_some()),
        (
            "ServiceTier",
            service_tier
                .as_ref()
                .is_some_and(|v| !v.as_str().is_empty()),
        ),
        (
            "AudioTranscriptionConfig",
            audio_transcription_config.is_some(),
        ),
        ("ContinuationToken", continuation_token.is_some()),
        ("AutomaticContinuation", automatic_continuation.is_some()),
        ("GenerationOutputConfig", generation_output_config.is_some()),
        ("Store", store.is_some()),
    ]
}

/// Every `ThinkingConfig` field as `(Go field name, set)`, in genai's declaration order.
pub(crate) fn thinking_config_fields(cfg: &ThinkingConfig) -> Vec<(&'static str, bool)> {
    let ThinkingConfig {
        include_thoughts,
        thinking_budget,
        thinking_level,
    } = cfg;
    vec![
        ("IncludeThoughts", *include_thoughts),
        ("ThinkingBudget", thinking_budget.is_some()),
        (
            "ThinkingLevel",
            thinking_level
                .as_ref()
                .is_some_and(|v| !v.as_str().is_empty()),
        ),
    ]
}

/// Every `HttpOptions` field as `(Go field name, set)`, in genai's declaration order (gadk's
/// fields mapped to Go's names: `BaseURL`, `APIVersion`, `Headers`, `Timeout`,
/// `ExtrasRequestProvider`).
pub(crate) fn http_option_fields(http_options: &HttpOptions) -> Vec<(&'static str, bool)> {
    let HttpOptions {
        base_url,
        api_version,
        headers,
        timeout,
        extras_request_provider,
    } = http_options;
    vec![
        ("BaseURL", base_url.as_ref().is_some_and(|v| !v.is_empty())),
        (
            "APIVersion",
            api_version.as_ref().is_some_and(|v| !v.is_empty()),
        ),
        ("Headers", !headers.is_empty()),
        ("Timeout", timeout.is_some()),
        ("ExtrasRequestProvider", extras_request_provider.is_some()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use gadk_genai::ImageConfig;
    use pretty_assertions::assert_eq;

    fn thinking(budget: Option<i32>, level: Option<ThinkingLevel>) -> ThinkingConfig {
        ThinkingConfig {
            thinking_budget: budget,
            thinking_level: level,
            ..ThinkingConfig::default()
        }
    }

    fn effort(cfg: ThinkingConfig) -> Result<Option<ReasoningEffort>, String> {
        reasoning_effort_for(Some(&cfg)).map_err(|e| e.to_string())
    }

    #[test]
    fn reasoning_effort_follows_go() {
        assert_eq!(reasoning_effort_for(None).unwrap(), None);
        let cases = [
            (thinking(None, None), Ok(None)),
            (thinking(Some(0), None), Ok(Some(ReasoningEffort::None))),
            (thinking(Some(-1), None), Ok(None)),
            (thinking(Some(1024), None), Ok(Some(ReasoningEffort::Medium))),
            (
                thinking(None, Some(ThinkingLevel::Unspecified)),
                Ok(Some(ReasoningEffort::Medium)),
            ),
            // UNSPECIFIED defers to a budget the caller did set.
            (
                thinking(Some(0), Some(ThinkingLevel::Unspecified)),
                Ok(Some(ReasoningEffort::None)),
            ),
            // A named level outranks a budget.
            (
                thinking(Some(0), Some(ThinkingLevel::High)),
                Ok(Some(ReasoningEffort::High)),
            ),
            (
                thinking(None, Some(ThinkingLevel::Minimal)),
                Ok(Some(ReasoningEffort::Minimal)),
            ),
            (
                thinking(None, Some(ThinkingLevel::Low)),
                Ok(Some(ReasoningEffort::Low)),
            ),
            (
                thinking(None, Some(ThinkingLevel::Unknown(String::new()))),
                Ok(None),
            ),
            (
                thinking(Some(-5), Some(ThinkingLevel::High)),
                Err(
                    "openai: unsupported generation config field: ThinkingConfig.ThinkingBudget -5"
                        .to_string(),
                ),
            ),
            (
                thinking(None, Some(ThinkingLevel::Unknown("ULTRA".to_string()))),
                Err(
                    r#"openai: unsupported generation config field: ThinkingConfig.ThinkingLevel "ULTRA""#
                        .to_string(),
                ),
            ),
        ];
        for (cfg, want) in cases {
            assert_eq!(effort(cfg.clone()), want, "{cfg:?}");
        }
    }

    #[test]
    fn untranslatable_values_are_refused_in_go_order() {
        let msg = |cfg: &GenerateContentConfig, http: Option<&HttpOptions>| {
            reject_untranslatable_values(cfg, http)
                .err()
                .map(|e| e.to_string())
        };
        let prefix = "openai: unsupported generation config field: ";
        assert_eq!(msg(&GenerateContentConfig::default(), None), None);
        let logprobs = GenerateContentConfig {
            logprobs: Some(3),
            max_output_tokens: -1,
            ..GenerateContentConfig::default()
        };
        assert_eq!(
            msg(&logprobs, None),
            Some(format!("{prefix}Logprobs without ResponseLogprobs"))
        );
        let negative = GenerateContentConfig {
            max_output_tokens: -1,
            candidate_count: -1,
            ..GenerateContentConfig::default()
        };
        assert_eq!(
            msg(&negative, None),
            Some(format!("{prefix}negative MaxOutputTokens"))
        );
        let candidates = GenerateContentConfig {
            candidate_count: -1,
            ..GenerateContentConfig::default()
        };
        assert_eq!(
            msg(&candidates, None),
            Some(format!("{prefix}negative CandidateCount"))
        );

        let cfg = GenerateContentConfig::default();
        let mut headers = http::HeaderMap::new();
        headers.insert("x-ignored", http::HeaderValue::from_static("1"));
        let ignored = HttpOptions {
            headers,
            timeout: Some(Duration::from_secs(1)),
            ..HttpOptions::default()
        };
        assert_eq!(msg(&cfg, Some(&ignored)), None);
        let base_url = HttpOptions {
            base_url: Some("https://example.test".to_string()),
            timeout: Some(Duration::ZERO),
            ..HttpOptions::default()
        };
        assert_eq!(
            msg(&cfg, Some(&base_url)),
            Some(format!("{prefix}HTTPOptions.BaseURL"))
        );
        let zero = HttpOptions {
            timeout: Some(Duration::ZERO),
            ..HttpOptions::default()
        };
        assert_eq!(
            msg(&cfg, Some(&zero)),
            Some(format!("{prefix}non-positive HTTPOptions.Timeout 0s"))
        );
    }

    #[test]
    fn config_fields_list_every_field_in_genai_order() {
        let fields = config_fields(&GenerateContentConfig::default(), None);
        assert_eq!(fields.len(), 37);
        assert!(fields.iter().all(|(_, set)| !set));
        assert_eq!(fields[0].0, "HTTPOptions");
        assert_eq!(fields[34].0, "AutomaticContinuation");
        assert_eq!(fields[35].0, "GenerationOutputConfig");
        assert_eq!(fields[36].0, "Store");
        let set: Vec<&str> = config_fields(
            &GenerateContentConfig {
                top_k: Some(0.0),
                image_config: Some(ImageConfig::default()),
                ..GenerateContentConfig::default()
            },
            Some(&HttpOptions::default()),
        )
        .into_iter()
        .filter_map(|(name, set)| set.then_some(name))
        .collect();
        assert_eq!(set, ["HTTPOptions", "TopK", "ImageConfig"]);
        assert_eq!(
            http_option_fields(&HttpOptions {
                api_version: Some(String::new()),
                ..HttpOptions::default()
            }),
            [
                ("BaseURL", false),
                ("APIVersion", false),
                ("Headers", false),
                ("Timeout", false),
                ("ExtrasRequestProvider", false),
            ]
        );
    }

    #[test]
    fn unsupported_config_fields_name_the_first_in_genai_order() {
        let cfg = GenerateContentConfig {
            seed: Some(1),
            top_k: Some(1.0),
            ..GenerateContentConfig::default()
        };
        let err =
            reject_unsupported_config_fields(&cfg, None, SUPPORTED_CONFIG_FIELDS).unwrap_err();
        assert!(err.is(ErrorKind::UnsupportedConfigField));
        assert_eq!(
            err.to_string(),
            "openai: unsupported generation config field: TopK"
        );
    }

    /// genai v1.77.0 fields are refused by name like every other field not supported.
    #[test]
    fn genai_v1_77_config_fields_are_unsupported() {
        for (cfg, field) in [
            (
                GenerateContentConfig {
                    generation_output_config: Some(Default::default()),
                    ..GenerateContentConfig::default()
                },
                "GenerationOutputConfig",
            ),
            (
                GenerateContentConfig {
                    store: Some(false),
                    ..GenerateContentConfig::default()
                },
                "Store",
            ),
        ] {
            let err =
                reject_unsupported_config_fields(&cfg, None, SUPPORTED_CONFIG_FIELDS).unwrap_err();
            assert!(err.is(ErrorKind::UnsupportedConfigField));
            assert_eq!(
                err.to_string(),
                format!("openai: unsupported generation config field: {field}")
            );
        }
    }
}
