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

//! The generation-config rules both APIs share.
//!
//! `HttpOptions` is carried beside the config in `LlmRequest::http_options`, so the functions
//! that read it take it as a second argument (an absent config is `cfg: None` with
//! `http_options: None`). The config's fields are listed by exhaustive destructuring
//! ([`config_fields`], [`thinking_config_fields`], [`http_option_fields`]) as
//! `(field name, is set)` in the declaration order of the genai types, using the CamelCase
//! field names that appear in error messages, so a field added to those types fails to compile
//! here.

use std::time::Duration;

use gadk_genai::{GenerateContentConfig, ThinkingConfig, ThinkingLevel};
use gadk_model::HttpOptions;
use gadk_platform::go_quote;

use crate::error::{ErrorKind, OpenAiError};
use crate::oaishared::ReasoningEffort;

/// The `HTTPOptions` fields neither API translates nor rejects.
pub(crate) const IGNORED_HTTP_OPTION_FIELDS: &[&str] = &["headers"];

/// The `HTTPOptions` fields both APIs support.
pub(crate) const SUPPORTED_HTTP_OPTION_FIELDS: &[&str] = &["timeout"];

/// The dynamic thinking budget: "let the model size its own thinking".
pub(crate) const DYNAMIC_THINKING_BUDGET: i32 = -1;

/// The `GenerateContentConfig` fields both APIs support (CamelCase field
/// names). Each API adds the ones only it supports.
pub(crate) const SUPPORTED_CONFIG_FIELDS: &[&str] = &[
    "http_options",
    "system_instruction",
    "temperature",
    "top_p",
    "max_output_tokens",
    "response_logprobs",
    "logprobs",
    "response_mime_type",
    "response_schema",
    "response_json_schema",
    "tools",
    "tool_config",
    "thinking_config",
    "service_tier",
    "candidate_count",
    "automatic_continuation",
];

/// The `ThinkingConfig` fields both APIs support.
pub(crate) const SUPPORTED_THINKING_CONFIG_FIELDS: &[&str] =
    &["include_thoughts", "thinking_budget", "thinking_level"];

/// The caller's `http_options.timeout`, or `None` for none (a zero
/// duration is none).
pub(crate) fn request_timeout(http_options: Option<&HttpOptions>) -> Option<Duration> {
    http_options
        .and_then(|options| options.timeout)
        .filter(|timeout| !timeout.is_zero())
}

/// The reasoning effort of every thinking level
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

/// The reasoning effort for a thinking config; `Ok(None)` means send none. A budget below
/// [`DYNAMIC_THINKING_BUDGET`] is
/// `UnsupportedConfigField: ThinkingConfig.ThinkingBudget <n>`; an unknown level
/// `UnsupportedConfigField: ThinkingConfig.ThinkingLevel "<level>"`.
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
            format!("thinking_config.thinking_budget {budget}"),
        ));
    }
    // A level outranks a budget only when it names one: UNSPECIFIED with a budget set defers
    // to the budget. An empty level is `None` (or an empty unknown value).
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
                format!(
                    "thinking_config.thinking_level {}",
                    go_quote(level.as_str())
                ),
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

/// Rejects values that cannot be translated: `Logprobs without ResponseLogprobs`, `negative
/// MaxOutputTokens`, `negative CandidateCount`, then an `HTTPOptions` field outside
/// [`SUPPORTED_HTTP_OPTION_FIELDS`] + [`IGNORED_HTTP_OPTION_FIELDS`]
/// (`HTTPOptions.<Field>`), then a non-positive timeout (unrepresentable in a `Duration`
/// except zero: `non-positive HTTPOptions.Timeout 0s`), each wrapping
/// `ErrorKind::UnsupportedConfigField`.
pub(crate) fn reject_untranslatable_values(
    cfg: &GenerateContentConfig,
    http_options: Option<&HttpOptions>,
) -> Result<(), OpenAiError> {
    if cfg.logprobs.is_some() && !cfg.response_logprobs {
        return Err(OpenAiError::new(
            ErrorKind::UnsupportedConfigField,
            "logprobs without response_logprobs",
        ));
    }
    if cfg.max_output_tokens < 0 {
        return Err(OpenAiError::new(
            ErrorKind::UnsupportedConfigField,
            "negative max_output_tokens",
        ));
    }
    if cfg.candidate_count < 0 {
        return Err(OpenAiError::new(
            ErrorKind::UnsupportedConfigField,
            "negative candidate_count",
        ));
    }
    if let Some(options) = http_options {
        let allowed: Vec<&str> = SUPPORTED_HTTP_OPTION_FIELDS
            .iter()
            .chain(IGNORED_HTTP_OPTION_FIELDS)
            .copied()
            .collect();
        reject_unsupported_fields("http_options.", &http_option_fields(options), &allowed)?;
        // A zero timeout is "no deadline" to the transport, so forwarding it would lift the
        // caller's bound rather than apply it. A `Duration` cannot be negative.
        if options.timeout.is_some_and(|timeout| timeout.is_zero()) {
            return Err(OpenAiError::new(
                ErrorKind::UnsupportedConfigField,
                "non-positive http_options.timeout 0s",
            ));
        }
    }
    Ok(())
}

/// Rejects the first field the caller set ([`config_fields`]) not
/// in `supported`, then the first set `ThinkingConfig` field not in
/// [`SUPPORTED_THINKING_CONFIG_FIELDS`], each `UnsupportedConfigField: <prefix><Field>`.
pub(crate) fn reject_unsupported_config_fields(
    cfg: &GenerateContentConfig,
    http_options: Option<&HttpOptions>,
    supported: &[&str],
) -> Result<(), OpenAiError> {
    reject_unsupported_fields("", &config_fields(cfg, http_options), supported)?;
    match &cfg.thinking_config {
        Some(thinking) => reject_unsupported_fields(
            "thinking_config.",
            &thinking_config_fields(thinking),
            SUPPORTED_THINKING_CONFIG_FIELDS,
        ),
        None => Ok(()),
    }
}

/// Rejects the first `(name, true)` whose name is not
/// in `supported`: `UnsupportedConfigField: <prefix><name>`.
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

/// Every `GenerateContentConfig` field as `(field name, set)`, in declaration order
/// (`HTTPOptions` first: `http_options.is_some()`), by exhaustive destructuring. "Set" means
/// non-zero: a non-`None` option, a non-empty string, slice or map, a non-zero number, or
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
        ("http_options", http_options.is_some()),
        ("system_instruction", system_instruction.is_some()),
        ("temperature", temperature.is_some()),
        ("top_p", top_p.is_some()),
        ("top_k", top_k.is_some()),
        ("candidate_count", *candidate_count != 0),
        ("max_output_tokens", *max_output_tokens != 0),
        ("stop_sequences", !stop_sequences.is_empty()),
        ("response_logprobs", *response_logprobs),
        ("logprobs", logprobs.is_some()),
        ("presence_penalty", presence_penalty.is_some()),
        ("frequency_penalty", frequency_penalty.is_some()),
        ("seed", seed.is_some()),
        ("response_mime_type", !response_mime_type.is_empty()),
        ("response_schema", response_schema.is_some()),
        ("response_json_schema", response_json_schema.is_some()),
        ("routing_config", routing_config.is_some()),
        ("model_selection_config", model_selection_config.is_some()),
        ("safety_settings", !safety_settings.is_empty()),
        ("tools", !tools.is_empty()),
        ("tool_config", tool_config.is_some()),
        ("labels", !labels.is_empty()),
        ("cached_content", !cached_content.is_empty()),
        ("response_modalities", !response_modalities.is_empty()),
        (
            "media_resolution",
            media_resolution
                .as_ref()
                .is_some_and(|v| !v.as_str().is_empty()),
        ),
        ("speech_config", speech_config.is_some()),
        ("audio_timestamp", *audio_timestamp),
        ("thinking_config", thinking_config.is_some()),
        ("image_config", image_config.is_some()),
        (
            "enable_enhanced_civic_answers",
            enable_enhanced_civic_answers.is_some(),
        ),
        ("model_armor_config", model_armor_config.is_some()),
        (
            "service_tier",
            service_tier
                .as_ref()
                .is_some_and(|v| !v.as_str().is_empty()),
        ),
        (
            "audio_transcription_config",
            audio_transcription_config.is_some(),
        ),
        ("continuation_token", continuation_token.is_some()),
        ("automatic_continuation", automatic_continuation.is_some()),
        (
            "generation_output_config",
            generation_output_config.is_some(),
        ),
        ("store", store.is_some()),
    ]
}

/// Every `ThinkingConfig` field as `(field name, set)`, in declaration order.
pub(crate) fn thinking_config_fields(cfg: &ThinkingConfig) -> Vec<(&'static str, bool)> {
    let ThinkingConfig {
        include_thoughts,
        thinking_budget,
        thinking_level,
    } = cfg;
    vec![
        ("include_thoughts", *include_thoughts),
        ("thinking_budget", thinking_budget.is_some()),
        (
            "thinking_level",
            thinking_level
                .as_ref()
                .is_some_and(|v| !v.as_str().is_empty()),
        ),
    ]
}

/// Every `HttpOptions` field as `(field name, set)`, in declaration order (gadk's
/// fields mapped to the names `BaseURL`, `APIVersion`, `Headers`, `Timeout`,
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
        ("base_url", base_url.as_ref().is_some_and(|v| !v.is_empty())),
        (
            "api_version",
            api_version.as_ref().is_some_and(|v| !v.is_empty()),
        ),
        ("headers", !headers.is_empty()),
        ("timeout", timeout.is_some()),
        ("extras_request_provider", extras_request_provider.is_some()),
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
                    "openai: unsupported generation config field: thinking_config.thinking_budget -5"
                        .to_string(),
                ),
            ),
            (
                thinking(None, Some(ThinkingLevel::Unknown("ULTRA".to_string()))),
                Err(
                    r#"openai: unsupported generation config field: thinking_config.thinking_level "ULTRA""#
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
            Some(format!("{prefix}logprobs without response_logprobs"))
        );
        let negative = GenerateContentConfig {
            max_output_tokens: -1,
            candidate_count: -1,
            ..GenerateContentConfig::default()
        };
        assert_eq!(
            msg(&negative, None),
            Some(format!("{prefix}negative max_output_tokens"))
        );
        let candidates = GenerateContentConfig {
            candidate_count: -1,
            ..GenerateContentConfig::default()
        };
        assert_eq!(
            msg(&candidates, None),
            Some(format!("{prefix}negative candidate_count"))
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
            Some(format!("{prefix}http_options.base_url"))
        );
        let zero = HttpOptions {
            timeout: Some(Duration::ZERO),
            ..HttpOptions::default()
        };
        assert_eq!(
            msg(&cfg, Some(&zero)),
            Some(format!("{prefix}non-positive http_options.timeout 0s"))
        );
    }

    #[test]
    fn config_fields_list_every_field_in_genai_order() {
        let fields = config_fields(&GenerateContentConfig::default(), None);
        assert_eq!(fields.len(), 37);
        assert!(fields.iter().all(|(_, set)| !set));
        assert_eq!(fields[0].0, "http_options");
        assert_eq!(fields[34].0, "automatic_continuation");
        assert_eq!(fields[35].0, "generation_output_config");
        assert_eq!(fields[36].0, "store");
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
        assert_eq!(set, ["http_options", "top_k", "image_config"]);
        assert_eq!(
            http_option_fields(&HttpOptions {
                api_version: Some(String::new()),
                ..HttpOptions::default()
            }),
            [
                ("base_url", false),
                ("api_version", false),
                ("headers", false),
                ("timeout", false),
                ("extras_request_provider", false),
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
            "openai: unsupported generation config field: top_k"
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
                "generation_output_config",
            ),
            (
                GenerateContentConfig {
                    store: Some(false),
                    ..GenerateContentConfig::default()
                },
                "store",
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
