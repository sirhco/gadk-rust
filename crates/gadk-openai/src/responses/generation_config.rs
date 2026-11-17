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

//! Responses generation-config translation: the supported fields, the generation config, the
//! service tiers and the thinking config.

use gadk_genai::{GenerateContentConfig, ServiceTier, ThinkingConfig};
use gadk_model::HttpOptions;
use gadk_platform::go_quote;

use crate::error::{ErrorKind, OpenAiError};
use crate::oaishared::{ReasoningParam, ReasoningSummary, ResponseFormatJsonObjectParam};
use crate::responses::schema::new_json_schema_format;
use crate::responses::wire::{
    ResponseFormatTextConfig, ResponseIncludable, ResponseNewParams, ResponseServiceTier,
    ResponseTextConfigParam,
};
use crate::shared::config::{
    SUPPORTED_CONFIG_FIELDS, reasoning_effort_for, reject_unsupported_config_fields,
    reject_untranslatable_values,
};
use crate::shared::contents::flatten_content_text;

/// [`SUPPORTED_CONFIG_FIELDS`](crate::shared::config::SUPPORTED_CONFIG_FIELDS)
/// plus `StopSequences` (accepted only when empty).
pub(crate) fn supported_config_fields() -> Vec<&'static str> {
    let mut fields = SUPPORTED_CONFIG_FIELDS.to_vec();
    // The empty list asks for no stop sequences, which needs no wire field, while
    // apply_generation_config refuses a non-empty one by name.
    fields.push("StopSequences");
    fields
}

/// Translates `cfg` (with `http_options`) onto `params`, rejecting what the Responses API
/// cannot express in check order; `(None, None)` is an absent config (nothing to do).
pub(crate) fn apply_generation_config(
    params: &mut ResponseNewParams,
    cfg: Option<&GenerateContentConfig>,
    http_options: Option<&HttpOptions>,
) -> Result<(), OpenAiError> {
    // `HTTPOptions` is carried beside the config, so options without a config are a config
    // holding nothing else.
    let empty;
    let cfg = match (cfg, http_options) {
        (Some(cfg), _) => cfg,
        (None, Some(_)) => {
            empty = GenerateContentConfig::default();
            &empty
        }
        (None, None) => return Ok(()),
    };
    if let Some(temperature) = cfg.temperature {
        params.temperature = Some(f64::from(temperature));
    }
    if let Some(top_p) = cfg.top_p {
        params.top_p = Some(f64::from(top_p));
    }
    if cfg.top_k.is_some() {
        return Err(OpenAiError::sentinel(ErrorKind::TopKNotSupported));
    }
    if cfg.max_output_tokens > 0 {
        params.max_output_tokens = Some(i64::from(cfg.max_output_tokens));
    }
    if !cfg.stop_sequences.is_empty() {
        return Err(OpenAiError::sentinel(ErrorKind::StopSequencesNotSupported));
    }
    if cfg.candidate_count > 1 {
        return Err(OpenAiError::sentinel(
            ErrorKind::MultipleCandidatesNotSupported,
        ));
    }
    if cfg.frequency_penalty.is_some() || cfg.presence_penalty.is_some() {
        return Err(OpenAiError::sentinel(ErrorKind::PenaltiesNotSupported));
    }
    if cfg.response_logprobs {
        params.top_logprobs = Some(cfg.logprobs.map_or(1, i64::from));
        // Responses returns logprobs only when explicitly included.
        params
            .include
            .push(ResponseIncludable::MessageOutputTextLogprobs);
    }
    if let Some(instruction) = &cfg.system_instruction {
        let inst = flatten_content_text(Some(instruction))
            .map_err(|err| err.prefixed("openai: system instruction: "))?;
        if !inst.is_empty() {
            params.instructions = Some(inst);
        }
    }
    let mime = cfg.response_mime_type.as_str();
    if !mime.is_empty() && mime != "text/plain" && mime != "application/json" {
        return Err(OpenAiError::new(ErrorKind::UnsupportedMimeType, mime));
    }
    if mime == "application/json"
        || cfg.response_schema.is_some()
        || cfg.response_json_schema.is_some()
    {
        let format = if cfg.response_schema.is_none() && cfg.response_json_schema.is_none() {
            ResponseFormatTextConfig::JsonObject(ResponseFormatJsonObjectParam::default())
        } else {
            ResponseFormatTextConfig::JsonSchema(new_json_schema_format(cfg)?)
        };
        params.text = ResponseTextConfigParam {
            format: Some(format),
        };
    }
    if !cfg.labels.is_empty() {
        return Err(OpenAiError::sentinel(ErrorKind::LabelsNotSupported));
    }
    if !cfg.safety_settings.is_empty() {
        return Err(OpenAiError::sentinel(ErrorKind::SafetySettingsNotSupported));
    }
    apply_thinking_config(params, cfg.thinking_config.as_ref())?;
    if let Some(tier) = cfg.service_tier.as_ref().filter(|t| !t.as_str().is_empty()) {
        match service_tier_for(tier) {
            Some(tier) => params.service_tier = Some(tier),
            None => {
                return Err(OpenAiError::new(
                    ErrorKind::UnsupportedConfigField,
                    format!("ServiceTier {}", go_quote(tier.as_str())),
                ));
            }
        }
    }
    reject_untranslatable_values(cfg, http_options)?;
    // Last, so the named errors above win when a caller sets both.
    reject_unsupported_config_fields(cfg, http_options, &supported_config_fields())
}

/// The service tier: unspecified and standard → `default`, flex → `flex`, priority →
/// `priority`; `None` for a tier the map does not know.
pub(crate) fn service_tier_for(tier: &ServiceTier) -> Option<ResponseServiceTier> {
    match tier {
        // genai documents "unspecified" as "Default service tier, which is standard".
        ServiceTier::Unspecified | ServiceTier::Standard => Some(ResponseServiceTier::Default),
        ServiceTier::Flex => Some(ResponseServiceTier::Flex),
        ServiceTier::Priority => Some(ResponseServiceTier::Priority),
        ServiceTier::Unknown(_) => None,
    }
}

/// Applies the thinking config: `reasoning.effort` from
/// [`reasoning_effort_for`](crate::shared::config::reasoning_effort_for), `reasoning.summary`
/// `auto` when `include_thoughts`; `None` leaves `params` alone.
pub(crate) fn apply_thinking_config(
    params: &mut ResponseNewParams,
    cfg: Option<&ThinkingConfig>,
) -> Result<(), OpenAiError> {
    let Some(cfg) = cfg else {
        return Ok(());
    };
    let effort = reasoning_effort_for(Some(cfg))?;
    // A reasoning block holding neither is the zero value, which is dropped from the request.
    params.reasoning = ReasoningParam {
        effort,
        summary: None,
    };
    // Summaries need a verified organization, so they ride on IncludeThoughts alone.
    if cfg.include_thoughts {
        params.reasoning.summary = Some(ReasoningSummary::Auto);
    }
    Ok(())
}
