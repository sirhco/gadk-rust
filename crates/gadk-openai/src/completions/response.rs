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

//! Chat Completions response conversion (port of ADK-Go `internal/completions/response.go`).

#![allow(dead_code, unused_variables, clippy::todo)] // D1: Task 6 fills the bodies.

use gadk_genai::{
    FinishReason, GenerateContentResponse, GenerateContentResponseUsageMetadata, LogprobsResult,
};

use crate::completions::wire::{ChatCompletion, ChatCompletionChoiceLogprobs, CompletionUsage};
use crate::error::OpenAiError;

/// Go `convertCompletion`: the genai response for a Chat Completions response (blocking, and
/// the accumulated snapshot of a stream). `None` is the bare
/// [`ErrorKind::EmptyResponse`](crate::error::ErrorKind::EmptyResponse); no choices
/// [`ErrorKind::NoChoices`](crate::error::ErrorKind::NoChoices); a first choice with nothing to
/// read [`ErrorKind::NoTextOrToolContent`](crate::error::ErrorKind::NoTextOrToolContent).
/// Usage is set only when the response carried it (`resp.usage.is_some()`).
pub(crate) fn convert_completion(
    resp: Option<&ChatCompletion>,
) -> Result<GenerateContentResponse, OpenAiError> {
    todo!("Task 6")
}

/// Go `finishReason`: `stop`/`tool_calls`/`function_call` → STOP, `length` → MAX_TOKENS,
/// `content_filter` → SAFETY, `""` → FINISH_REASON_UNSPECIFIED, anything else → OTHER.
pub(crate) fn finish_reason(reason: &str) -> FinishReason {
    todo!("Task 6")
}

/// Go `convertUsage`.
pub(crate) fn convert_usage(usage: &CompletionUsage) -> GenerateContentResponseUsageMetadata {
    todo!("Task 6")
}

/// Go `convertLogprobs`: the choice's content logprobs (refusal logprobs left out); `None`
/// when there are none.
pub(crate) fn convert_logprobs(logprobs: &ChatCompletionChoiceLogprobs) -> Option<LogprobsResult> {
    todo!("Task 6")
}
