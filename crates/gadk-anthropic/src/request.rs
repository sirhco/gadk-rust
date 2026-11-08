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

//! genai → Messages content conversion (port of ADK-Go `request.go`, except
//! `buildMessageParams`, which `model.rs` assembles).
//!
//! Placeholder signatures written by Task 1; Task 2 implements them.

use gadk_genai::{
    Blob, Content, FileData, FunctionResponse, FunctionResponsePart, GenerateContentConfig, Part,
};
use serde_json::{Map, Value};

use crate::config::Backend;
use crate::error::AnthropicError;
use crate::tool_id::ToolUseIdSanitizer;
use crate::wire::{ContentBlockParam, MessageParam, Role, TextBlockParam, ToolResultContent};

/// What Go's conversion methods read from `anthropicModel`: the backend (file data is sent
/// only on [`Backend::Anthropic`]) and the request's model ID (a field of every warning).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ConvertCtx<'a> {
    /// The model's backend.
    pub(crate) backend: Backend,
    /// The resolved model ID of this request.
    pub(crate) model_id: &'a str,
}

/// The result of [`convert_content_parts`] (Go's `blocks, droppedReasons, hadEmptyText`).
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ConvertedParts {
    /// The converted blocks, in part order.
    pub(crate) blocks: Vec<ContentBlockParam>,
    /// Why parts were dropped (Go's reason strings, e.g. `inline_data (mime_type=audio/wav)`).
    pub(crate) dropped_reasons: Vec<String>,
    /// Whether a part with no content (an empty text part) was seen.
    pub(crate) had_empty_text: bool,
}

/// Go `buildSystemBlocks`: the system instruction's non-empty text parts joined with `"\n\n"`
/// as one block, or none.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn build_system_blocks(cfg: Option<&GenerateContentConfig>) -> Vec<TextBlockParam> {
    todo!("Task 2")
}

/// Go `toClaudeRole`: `model` or `assistant` (trimmed, case-insensitive) is the assistant;
/// anything else is the user.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn to_claude_role(role: &str) -> Role {
    todo!("Task 2")
}

/// Go `buildMessages`: converts every content, skips empty and fully dropped earlier
/// contents, coalesces adjacent same-role messages, and fails with Go's `ErrNoContent` texts
/// (`contents must not be empty`, `newest message in contents is empty`, `newest message has
/// no sendable content[ after skipping unsupported parts (<reasons>)]`, `no sendable messages in
/// request`). One [`ToolUseIdSanitizer`] serves the whole request.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn build_messages(
    cx: ConvertCtx<'_>,
    contents: &[Content],
) -> Result<Vec<MessageParam>, AnthropicError> {
    todo!("Task 2")
}

/// Go `orderAssistantBlocks`: thinking and redacted-thinking blocks first, others after, each
/// group in its original order.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn order_assistant_blocks(blocks: Vec<ContentBlockParam>) -> Vec<ContentBlockParam> {
    todo!("Task 2")
}

/// Go `convertContentParts`, part kinds in Go's order (thought, content-free signature,
/// function call, function response, inline data, file data, executable code, code execution
/// result, unsupported server-side/audio/video parts, text, empty).
#[allow(unused_variables, clippy::todo)]
pub(crate) fn convert_content_parts(
    cx: ConvertCtx<'_>,
    role: Role,
    parts: &[Part],
    sanitizer: &mut ToolUseIdSanitizer,
) -> ConvertedParts {
    todo!("Task 2")
}

/// Go `isContentFreeSignature`.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn is_content_free_signature(part: &Part) -> bool {
    todo!("Task 2")
}

/// Go `convertInlineDataPart`: `Err(reason)` when the part is dropped (after the warning).
#[allow(unused_variables, clippy::todo)]
pub(crate) fn convert_inline_data_part(
    cx: ConvertCtx<'_>,
    role: Role,
    blob: &Blob,
) -> Result<ContentBlockParam, String> {
    todo!("Task 2")
}

/// Go `convertFileDataPart`: `Err(reason)` when the part is dropped (after the warning).
#[allow(unused_variables, clippy::todo)]
pub(crate) fn convert_file_data_part(
    cx: ConvertCtx<'_>,
    role: Role,
    fd: &FileData,
) -> Result<ContentBlockParam, String> {
    todo!("Task 2")
}

/// Go `convertFunctionResponsePart`: a `tool_result` with `is_error: false`, the serialized
/// response as a text block (if non-empty) and the media blocks.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn convert_function_response_part(
    cx: ConvertCtx<'_>,
    role: Role,
    fr: &FunctionResponse,
    sanitizer: &mut ToolUseIdSanitizer,
) -> ContentBlockParam {
    todo!("Task 2")
}

/// Go `serializeFunctionResponseData`. JSON is written with [`crate::wire::to_go_json`] (Go
/// `json.Marshal`).
#[allow(unused_variables, clippy::todo)]
pub(crate) fn serialize_function_response_data(resp: &Map<String, Value>) -> String {
    todo!("Task 2")
}

/// Go `formatArbitraryValue` (a JSON string as itself, `null` as `""`, anything else as Go
/// JSON).
#[allow(unused_variables, clippy::todo)]
pub(crate) fn format_arbitrary_value(value: &Value) -> String {
    todo!("Task 2")
}

/// Go `convertFunctionResponseMedia`.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn convert_function_response_media(
    cx: ConvertCtx<'_>,
    role: Role,
    parts: &[FunctionResponsePart],
) -> Vec<ToolResultContent> {
    todo!("Task 2")
}

/// Go `warnSkippedPart`: `tracing::warn!` with the message
/// `skipping unsupported content part for Claude request` and the fields `backend`, `model`,
/// `role`, `part_kind`, `mime_type`, `uri_scheme` (D10).
#[allow(unused_variables, clippy::todo)]
pub(crate) fn warn_skipped_part(
    cx: ConvertCtx<'_>,
    role: Role,
    part_kind: &str,
    mime_type: &str,
    uri_scheme: &str,
) {
    todo!("Task 2")
}

/// Go `normalizeMIMEType`: the lowercased type before any `;`, `""` when blank, `invalid` when
/// it does not match Go's `validMIMETypePattern`.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn normalize_mime_type(raw: &str) -> String {
    todo!("Task 2")
}

/// Go `isSupportedImageMIME`: `image/jpeg`, `image/png`, `image/gif`, `image/webp`.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn is_supported_image_mime(mime_type: &str) -> bool {
    todo!("Task 2")
}

/// Go `extractURIScheme`: the lowercased scheme, `""` for a blank URI, `unknown` when it is
/// malformed or not followed by `//` (except `data`, `blob`, `urn`, `mailto`).
#[allow(unused_variables, clippy::todo)]
pub(crate) fn extract_uri_scheme(raw: &str) -> String {
    todo!("Task 2")
}

/// Go `isValidHTTPSURL`: parses, scheme `https` (any case), non-empty host.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn is_valid_https_url(raw: &str) -> bool {
    todo!("Task 2")
}
