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
//!
//! Placeholder signature written by Task 1; Task 3 implements it.

use gadk_genai::GenerateContentConfig;
use gadk_model::HttpOptions;

use crate::config::Effort;
use crate::error::AnthropicError;
use crate::tools::ReqTools;

/// Go `validateGenerateContentConfig`, with every rejection's Go text, in Go's order. Go's
/// `HTTPOptions` checks (between the tool-config and output-format checks) apply to
/// `LlmRequest.http_options`, which gadk keeps outside the config, and run even when `cfg` is
/// `None`: `base_url` → `HTTPOptions.BaseURL is not supported; set ClientConfig.BaseURL
/// instead`, `api_version` → `HTTPOptions.APIVersion is not supported`,
/// `extras_request_provider` → `HTTPOptions.ExtrasRequestProvider is not supported`; `headers`
/// are ignored and `timeout` is accepted.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn validate_generate_content_config(
    cfg: Option<&GenerateContentConfig>,
    req_tools: &ReqTools,
    effort: Option<Effort>,
    http_options: Option<&HttpOptions>,
) -> Result<(), AnthropicError> {
    todo!("Task 3")
}
