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

//! Port of ADK-Go `internal/llminternal/googlellm/variant.go`.

use std::sync::LazyLock;

use regex::Regex;

use crate::llm::{Backend, Llm};

/// `^gemini-(\d+(\.\d+)?)`; `None` only if the literal failed to compile (impossible),
/// in which case nothing matches.
static GEMINI_MODEL_VERSION_REGEX: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"^gemini-(\d+(\.\d+)?)").ok());

/// The Google LLM variant of `llm` (Gemini API or Vertex AI), or `None` when unspecified.
pub fn google_variant(llm: &dyn Llm) -> Option<Backend> {
    llm.google_variant()
}

/// Whether `model` names a Gemini model.
pub fn is_gemini_model(model: &str) -> bool {
    extract_model_name(model).starts_with("gemini-")
}

/// Whether `model` names a Gemini 1.x model.
pub fn is_gemini1_model(model: &str) -> bool {
    extract_gemini_version(model).is_some_and(|v| v < 2.0)
}

/// Whether `model` names a Gemini 2.5 or lower model.
pub fn is_gemini25_or_lower(model: &str) -> bool {
    extract_gemini_version(model).is_some_and(|v| v <= 2.5)
}

/// Whether `model` names a Gemini 2.0 or above model.
pub fn is_gemini2_or_above(model: &str) -> bool {
    extract_gemini_version(model).is_some_and(|v| v >= 2.0)
}

/// Whether `llm` supports an output schema together with tools natively: only Vertex AI
/// for Gemini 2.0+.
pub fn can_use_output_schema_with_tools(llm: Option<&dyn Llm>) -> bool {
    let Some(llm) = llm else {
        return false;
    };
    is_gemini_model(llm.name())
        && google_variant(llm) == Some(Backend::VertexAi)
        && is_gemini2_or_above(llm.name())
}

/// Whether `llm` is served by the Gemini API (not Vertex AI).
pub fn is_gemini_api_variant(llm: &dyn Llm) -> bool {
    google_variant(llm) == Some(Backend::GeminiApi)
}

/// Whether `llm` is a Gemini model that needs the `set_model_response` tool workaround
/// because it does not support output schema with tools natively.
pub fn needs_output_schema_processor(llm: Option<&dyn Llm>) -> bool {
    let Some(inner) = llm else {
        return false;
    };
    is_gemini_model(inner.name()) && !can_use_output_schema_with_tools(llm)
}

/// Go `isGemini3xLive`: whether `model` names a Gemini 3.x Live model other than Live
/// Translate (adk-python `_is_gemini_3_x_live`).
pub fn is_gemini_3x_live(model: &str) -> bool {
    let name = extract_model_name(model);
    name.starts_with("gemini-3.") && name.contains("-live") && !is_gemini_35_live_translate(model)
}

/// Go `isGemini35LiveTranslate`: whether `model` names a Gemini 3.5 Live Translate model.
pub fn is_gemini_35_live_translate(model: &str) -> bool {
    extract_model_name(model).starts_with("gemini-3.5-live-translate")
}

fn extract_gemini_version(model: &str) -> Option<f64> {
    let name = extract_model_name(model);
    let re = GEMINI_MODEL_VERSION_REGEX.as_ref()?;
    let caps = re.captures(&name)?;
    caps.get(1)?.as_str().parse::<f64>().ok()
}

fn extract_model_name(model: &str) -> String {
    model.rsplit('/').next().unwrap_or(model).to_lowercase()
}
