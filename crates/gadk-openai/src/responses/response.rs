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

//! Responses response conversion (port of ADK-Go `internal/responses/response.go`).

#![allow(dead_code, unused_variables, clippy::todo)] // D1: Task 7 fills the bodies.

use gadk_genai::{
    FinishReason, GenerateContentResponse, GenerateContentResponseUsageMetadata, LogprobsResult,
    Part,
};

use crate::error::OpenAiError;
use crate::responses::wire::{Response, ResponseOutputItem, ResponseUsage};

/// Go `convertResponse`: `None` is the bare
/// [`ErrorKind::EmptyResponse`](crate::error::ErrorKind::EmptyResponse); a response that
/// [`reports_failure`] is [`failed_response_error`]; else one candidate from the output items.
pub(crate) fn convert_response(
    resp: Option<&Response>,
) -> Result<GenerateContentResponse, OpenAiError> {
    todo!("Task 7")
}

/// Go `reportsFailure`: status `failed`; or no status and a non-blank (clipped) error message
/// or code.
pub(crate) fn reports_failure(resp: &Response) -> bool {
    todo!("Task 7")
}

/// Go `failedResponseError`: [`ErrorKind::ResponseFailed`](crate::error::ErrorKind::ResponseFailed)
/// with the clipped, quoted id, code and message (`openai: response failed (id "…", code
/// "…"): "…"` and its shorter forms).
pub(crate) fn failed_response_error(resp: &Response) -> OpenAiError {
    todo!("Task 7")
}

/// Go `convertOutputItems`: message text and refusals, function calls, and reasoning content
/// and summaries as thought parts. No items is
/// [`ErrorKind::NoOutputItems`](crate::error::ErrorKind::NoOutputItems); no parts
/// [`ErrorKind::NoTextOrToolContent`](crate::error::ErrorKind::NoTextOrToolContent); an
/// unknown content or item type its sentinel with the quoted type.
pub(crate) fn convert_output_items(items: &[ResponseOutputItem]) -> Result<Vec<Part>, OpenAiError> {
    todo!("Task 7")
}

/// Go `convertFunctionCall`: the function-call part for an output item; undecodable
/// arguments are `ErrFunctionCallArgs: <err> (name "…", call_id "…")`.
pub(crate) fn convert_function_call(item: &ResponseOutputItem) -> Result<Part, OpenAiError> {
    todo!("Task 7")
}

/// Go `finishReason`: from `incomplete_details.reason`, else [`truncated`]; `None` is
/// FINISH_REASON_UNSPECIFIED.
pub(crate) fn finish_reason(resp: Option<&Response>, incomplete_event: bool) -> FinishReason {
    todo!("Task 7")
}

/// Go `truncated`: whether a turn naming no incomplete reason still ended early.
pub(crate) fn truncated(resp: &Response, incomplete_event: bool) -> bool {
    todo!("Task 7")
}

/// Go `finishMessage`: the provider's own account of why a turn ended (`""` for none).
pub(crate) fn finish_message(resp: Option<&Response>, incomplete_event: bool) -> String {
    todo!("Task 7")
}

/// Go `convertUsage`.
pub(crate) fn convert_usage(usage: &ResponseUsage) -> GenerateContentResponseUsageMetadata {
    todo!("Task 7")
}

/// Go `logprobsFor`: the response's logprobs only when its message text equals `text`;
/// `None` response is `None`.
pub(crate) fn logprobs_for(resp: Option<&Response>, text: &str) -> Option<LogprobsResult> {
    todo!("Task 7")
}

/// Go `outputText`: the text of the message items (output text and refusals).
pub(crate) fn output_text(items: &[ResponseOutputItem]) -> String {
    todo!("Task 7")
}

/// Go `convertLogprobs`: the `output_text` logprobs of the message items; `None` when there
/// are none.
pub(crate) fn convert_logprobs(items: &[ResponseOutputItem]) -> Option<LogprobsResult> {
    todo!("Task 7")
}
