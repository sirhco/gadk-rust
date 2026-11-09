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

#![allow(dead_code, unused_variables, clippy::todo)] // D1: Task 3 fills the bodies.

use std::time::Duration;

use gadk_genai::{GenerateContentConfig, ThinkingConfig, ThinkingLevel};
use gadk_model::HttpOptions;

use crate::error::OpenAiError;
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
    todo!("Task 3")
}

/// Go `ReasoningEfforts[level]`: every genai thinking level's reasoning effort
/// (`THINKING_LEVEL_UNSPECIFIED` → medium, `MINIMAL` → minimal, `LOW` → low, `MEDIUM` → medium,
/// `HIGH` → high); `None` for a level the map does not know (`ThinkingLevel::Unknown`).
pub(crate) fn reasoning_effort_of_level(level: &ThinkingLevel) -> Option<ReasoningEffort> {
    todo!("Task 3")
}

/// Go `ReasoningEffortFor`: the reasoning effort for a thinking config; `Ok(None)` is Go's
/// `""` (send none). A budget below [`DYNAMIC_THINKING_BUDGET`] is
/// `ErrUnsupportedConfigField: ThinkingConfig.ThinkingBudget <n>`; an unknown level
/// `ErrUnsupportedConfigField: ThinkingConfig.ThinkingLevel "<level>"`.
pub(crate) fn reasoning_effort_for(
    cfg: Option<&ThinkingConfig>,
) -> Result<Option<ReasoningEffort>, OpenAiError> {
    todo!("Task 3")
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
    todo!("Task 3")
}

/// Go `RejectUnsupportedConfigFields`: the first field the caller set ([`config_fields`]) not
/// in `supported`, then the first set `ThinkingConfig` field not in
/// [`SUPPORTED_THINKING_CONFIG_FIELDS`], each `ErrUnsupportedConfigField: <prefix><Field>`.
pub(crate) fn reject_unsupported_config_fields(
    cfg: &GenerateContentConfig,
    http_options: Option<&HttpOptions>,
    supported: &[&str],
) -> Result<(), OpenAiError> {
    todo!("Task 3")
}

/// Go `rejectUnsupportedFields` over a field list: the first `(name, true)` whose name is not
/// in `supported` is `ErrUnsupportedConfigField: <prefix><name>`.
pub(crate) fn reject_unsupported_fields(
    prefix: &str,
    fields: &[(&'static str, bool)],
    supported: &[&str],
) -> Result<(), OpenAiError> {
    todo!("Task 3")
}

/// Every `GenerateContentConfig` field as `(Go field name, set)`, in genai's declaration order
/// (`HTTPOptions` first: `http_options.is_some()`), by exhaustive destructuring. "Set" is Go's
/// `!IsZero()`: a non-`None` option, a non-empty string, slice or map, a non-zero number, or
/// `true`.
pub(crate) fn config_fields(
    cfg: &GenerateContentConfig,
    http_options: Option<&HttpOptions>,
) -> Vec<(&'static str, bool)> {
    todo!("Task 3")
}

/// Every `ThinkingConfig` field as `(Go field name, set)`, in genai's declaration order.
pub(crate) fn thinking_config_fields(cfg: &ThinkingConfig) -> Vec<(&'static str, bool)> {
    todo!("Task 3")
}

/// Every `HttpOptions` field as `(Go field name, set)`, in genai's declaration order (gadk's
/// fields mapped to Go's names: `BaseURL`, `APIVersion`, `Headers`, `Timeout`,
/// `ExtrasRequestProvider`).
pub(crate) fn http_option_fields(http_options: &HttpOptions) -> Vec<(&'static str, bool)> {
    todo!("Task 3")
}
