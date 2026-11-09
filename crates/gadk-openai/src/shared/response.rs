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

//! Response helpers both APIs share (port of ADK-Go `internal/shared/response.go`).

#![allow(dead_code, unused_variables, clippy::todo)] // D1: Task 3 fills the bodies.

use futures::stream::BoxStream;
use gadk_genai::{Content, FunctionCall, GenerateContentResponse, Part};
use gadk_model::{LlmResponse, ModelError};

use crate::error::OpenAiError;

/// Go `SafeInt32`: a token count narrowed to `i32`, saturating at `i32::MAX`.
pub(crate) fn safe_int32(v: i64) -> i32 {
    todo!("Task 3")
}

/// Go `SingleErrorSequence`: a response stream that yields `err` (as
/// [`ModelError::Other`]) and ends.
pub(crate) fn single_error_sequence(
    err: OpenAiError,
) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
    todo!("Task 3")
}

/// Go `IsEmptyOutput`: whether `err` is [`ErrorKind::NoOutputItems`](crate::error::ErrorKind::NoOutputItems),
/// [`ErrorKind::NoTextOrToolContent`](crate::error::ErrorKind::NoTextOrToolContent) or
/// [`ErrorKind::NoChoices`](crate::error::ErrorKind::NoChoices).
pub(crate) fn is_empty_output(err: &OpenAiError) -> bool {
    todo!("Task 3")
}

/// Go `CarriesContent`: whether the response has content with at least one part.
pub(crate) fn carries_content(resp: Option<&LlmResponse>) -> bool {
    todo!("Task 3")
}

/// Go `PartsWithoutCalls`: the parts that are not function calls, the calls (both in stream
/// order), and the index among the kept parts where the first call sat (the number of kept
/// parts when there is none). `None` is `(vec![], vec![], 0)`.
pub(crate) fn parts_without_calls(
    content: Option<&Content>,
) -> (Vec<Part>, Vec<FunctionCall>, usize) {
    todo!("Task 3")
}

/// Go `CompletedContentSupersedes`: whether a terminal snapshot can replace content assembled
/// from deltas (it carries visible text or a call, its text contains the aggregate's, and it
/// holds every aggregated call). `completed: None` is `false`; `aggregate: None` has no parts.
pub(crate) fn completed_content_supersedes(
    aggregate: Option<&Content>,
    completed: Option<&Content>,
) -> bool {
    todo!("Task 3")
}

/// Go `AnswerText`: the content's non-thought text, concatenated.
pub(crate) fn answer_text(content: Option<&Content>) -> String {
    todo!("Task 3")
}

/// Go `SinglePartResponse`: one streamed part as a genai response (one candidate, role
/// `model`, no finish reason).
pub(crate) fn single_part_response(part: Part) -> GenerateContentResponse {
    todo!("Task 3")
}
