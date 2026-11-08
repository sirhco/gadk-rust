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

//! Message → `LlmResponse` (port of ADK-Go `response.go`), and the decoding of a
//! non-streaming response body.
//!
//! Placeholder signatures written by Task 1; Task 4 implements them.

use gadk_genai::{FinishReason, GenerateContentResponseUsageMetadata, Part};
use gadk_model::LlmResponse;

use crate::error::AnthropicError;
use crate::wire::{ContentBlock, Message, Usage};

/// `LlmResponse.custom_metadata` key holding the raw Claude `stop_reason` when it does not map
/// one to one to a finish reason (Go `StopReasonMetadataKey`).
pub const STOP_REASON_METADATA_KEY: &str = "anthropic_stop_reason";

/// `LlmResponse.custom_metadata` key holding the prompt-cache creation token count when > 0
/// (Go `CacheCreationInputTokensMetadataKey`).
pub const CACHE_CREATION_INPUT_TOKENS_METADATA_KEY: &str = "anthropic_cache_creation_input_tokens";

/// Decodes a 2xx non-streaming body as anthropic-sdk-go's `decode` does: a non-JSON
/// `content_type` fails with the SDK's `expected destination type of 'string' or '[]byte' for
/// responses with content-type '<ct>' that is not 'application/json'`, a JSON decode error with
/// `error parsing response json: <err>`, both wrapped by
/// [`AnthropicError::request_failed`](crate::error::AnthropicError) as Go `classifyError` does.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn decode_message(content_type: &str, body: &[u8]) -> Result<Message, AnthropicError> {
    todo!("Task 4")
}

/// Go `convertMessageToLLMResponse`: role `model`, the converted blocks (tool use dropped when
/// the turn hit the token limit), finish reason, usage, `model_version`, `turn_complete`, and
/// the custom metadata keys above.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn convert_message_to_llm_response(
    msg: &Message,
    unwrap_output: bool,
) -> Result<LlmResponse, AnthropicError> {
    todo!("Task 4")
}

/// Go `convertContentBlock`: `None` for block types Go skips; Go's
/// `anthropicmodel: failed to decode tool_use input for "<name>"` (no sentinel) when the input
/// is not a JSON object.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn convert_content_block(
    block: &ContentBlock,
    unwrap_output: bool,
) -> Result<Option<Part>, AnthropicError> {
    todo!("Task 4")
}

/// Go `unwrapOutputSchemaText`: the `result` member's JSON text when `text` is an object with
/// exactly that member, else `text`.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn unwrap_output_schema_text(text: &str) -> String {
    todo!("Task 4")
}

/// Go `mapStopReason`: the finish reason and the raw stop reason to record (`""` when it maps
/// one to one).
#[allow(unused_variables, clippy::todo)]
pub(crate) fn map_stop_reason(stop_reason: &str) -> (FinishReason, String) {
    todo!("Task 4")
}

/// Go `extractUsageMetadata`: the usage metadata and the cache creation token count.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn extract_usage_metadata(usage: &Usage) -> (GenerateContentResponseUsageMetadata, i64) {
    todo!("Task 4")
}
