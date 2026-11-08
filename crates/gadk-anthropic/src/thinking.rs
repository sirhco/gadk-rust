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

//! Thinking, effort and structured output (port of ADK-Go `thinking.go`).
//!
//! Placeholder signatures written by Task 1; Task 3 implements them.

use gadk_genai::{GenerateContentConfig, ThinkingConfig};
use serde_json::{Map, Value};

use crate::config::Effort;
use crate::error::AnthropicError;
use crate::wire::{OutputConfig, ThinkingConfigParam};

/// The property a non-object output schema is wrapped in (Go `wrappedOutputSchemaKey`).
pub(crate) const WRAPPED_OUTPUT_SCHEMA_KEY: &str = "result";

/// A normalized output schema (Go `buildOutputFormatSchema`'s `map[string]any, bool`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OutputSchema {
    /// The schema sent as `output_config.format.schema`.
    pub(crate) schema: Map<String, Value>,
    /// Whether the schema was wrapped in `{"result": …}` (a non-object root), so text
    /// responses must be unwrapped.
    pub(crate) unwrap: bool,
}

/// Go `validateThinkingConfig`: rejects `thinking_level` and a missing `thinking_budget`.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn validate_thinking_config(tc: Option<&ThinkingConfig>) -> Result<(), AnthropicError> {
    todo!("Task 3")
}

/// Go `isThinkingEnabled`: a budget is set and non-zero.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn is_thinking_enabled(tc: Option<&ThinkingConfig>) -> bool {
    todo!("Task 3")
}

/// Go `buildThinkingParam`: budget 0 → disabled, < 0 → adaptive (summarized), > 0 → enabled
/// with that budget; `None` without a budget.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn build_thinking_param(
    cfg: Option<&GenerateContentConfig>,
) -> Option<ThinkingConfigParam> {
    todo!("Task 3")
}

/// Go `validateOutputFormatConfig`.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn validate_output_format_config(
    cfg: Option<&GenerateContentConfig>,
) -> Result<(), AnthropicError> {
    todo!("Task 3")
}

/// Go `buildOutputFormatSchema`: `None` without a response schema (`response_json_schema`
/// wins over `response_schema`); errors name the field Go names.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn build_output_format_schema(
    cfg: Option<&GenerateContentConfig>,
) -> Result<Option<OutputSchema>, AnthropicError> {
    todo!("Task 3")
}

/// Go `rewriteWrappedSchemaRefs`: local `$ref`s point into `#/properties/result`, except
/// `$defs`/`definitions`, which are hoisted to the wrapper.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn rewrite_wrapped_schema_refs(node: &mut Value) {
    todo!("Task 3")
}

/// Go `normalizeOutputSchemaObjects`: every `type: object` gets `properties: {}` and
/// `additionalProperties: false` when missing.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn normalize_output_schema_objects(node: &mut Value) {
    todo!("Task 3")
}

/// Go `buildOutputConfigParam`: `output_config` (`None` when neither effort nor a schema is
/// set) and whether text responses must be unwrapped.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn build_output_config_param(
    effort: Option<Effort>,
    cfg: Option<&GenerateContentConfig>,
) -> Result<(Option<OutputConfig>, bool), AnthropicError> {
    todo!("Task 3")
}
