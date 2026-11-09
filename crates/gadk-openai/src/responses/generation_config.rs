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

//! Responses generation-config translation (port of the config half of ADK-Go
//! `internal/responses/request.go`: `supportedConfigFields`, `applyGenerationConfig`,
//! `serviceTiers`, `applyThinkingConfig`).

#![allow(dead_code, unused_variables, clippy::todo)] // D1: Task 5 fills the bodies.

use gadk_genai::{GenerateContentConfig, ServiceTier, ThinkingConfig};
use gadk_model::HttpOptions;

use crate::error::OpenAiError;
use crate::responses::wire::{ResponseNewParams, ResponseServiceTier};

/// Go `supportedConfigFields`: [`SUPPORTED_CONFIG_FIELDS`](crate::shared::config::SUPPORTED_CONFIG_FIELDS)
/// plus `StopSequences` (accepted only when empty).
pub(crate) fn supported_config_fields() -> Vec<&'static str> {
    todo!("Task 5")
}

/// Go `applyGenerationConfig`: translates `cfg` (with `http_options`, Go's
/// `cfg.HTTPOptions`) onto `params`, rejecting what the Responses API cannot express in Go's
/// check order; `(None, None)` is Go's nil config (nothing to do).
pub(crate) fn apply_generation_config(
    params: &mut ResponseNewParams,
    cfg: Option<&GenerateContentConfig>,
    http_options: Option<&HttpOptions>,
) -> Result<(), OpenAiError> {
    todo!("Task 5")
}

/// Go `serviceTiers[tier]`: unspecified and standard → `default`, flex → `flex`, priority →
/// `priority`; `None` for a tier the map does not know.
pub(crate) fn service_tier_for(tier: &ServiceTier) -> Option<ResponseServiceTier> {
    todo!("Task 5")
}

/// Go `applyThinkingConfig`: `reasoning.effort` from
/// [`reasoning_effort_for`](crate::shared::config::reasoning_effort_for), `reasoning.summary`
/// `auto` when `include_thoughts`; `None` leaves `params` alone.
pub(crate) fn apply_thinking_config(
    params: &mut ResponseNewParams,
    cfg: Option<&ThinkingConfig>,
) -> Result<(), OpenAiError> {
    todo!("Task 5")
}
