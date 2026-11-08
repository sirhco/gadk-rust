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

//! Streaming (port of ADK-Go `stream.go` `generateStreaming` and anthropic-sdk-go v1.81.0
//! `Message.Accumulate` and `ssestream.Stream.Next`).
//!
//! Placeholder signatures written by Task 1; Task 4 implements them.

use futures::stream::BoxStream;
use gadk_model::sse::SseEvent;
use gadk_model::{BodyStream, LlmResponse};

use crate::error::AnthropicError;

/// Accumulates one streamed message: fed each [`SseEvent`], it returns the partial responses to
/// yield; [`finish`](Self::finish) returns the aggregated final response.
///
/// Events are selected by their SSE `event:` name as the SDK's `ssestream` does (`ping` and
/// unknown names are skipped); an `error` event becomes the [`ApiError`](crate::ApiError) of
/// [`crate::error::api_error_from_response`] with this response's status and `request-id`.
pub(crate) struct StreamAccumulator {}

#[allow(unused_variables, clippy::todo)]
impl StreamAccumulator {
    /// A new accumulator for a response with HTTP `status` and `request-id` header
    /// `request_id` (`""` when absent); `unwrap_output` suppresses text partials and unwraps the
    /// final text (Go `unwrapOutput`).
    pub(crate) fn new(unwrap_output: bool, status: u16, request_id: &str) -> Self {
        todo!("Task 4")
    }

    /// Feeds one event (Go: `acc.Accumulate(event)` then the partial yields): the partial
    /// responses to yield, or the error that ends the stream.
    pub(crate) fn push(&mut self, event: SseEvent) -> Result<Vec<LlmResponse>, AnthropicError> {
        todo!("Task 4")
    }

    /// After the body ended: applies the buffered tool inputs (unless the turn hit the token
    /// limit) and converts the accumulated message (Go's tail of `generateStreaming`).
    pub(crate) fn finish(self) -> Result<LlmResponse, AnthropicError> {
        todo!("Task 4")
    }
}

/// Drives [`gadk_model::sse::sse_events`] over `body` through a [`StreamAccumulator`]: the
/// partial responses, then the final one; the first error (a body error through
/// [`AnthropicError::from_model_error`](crate::error::AnthropicError), or the accumulator's)
/// ends the stream.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn stream_responses(
    body: BodyStream,
    unwrap_output: bool,
    status: u16,
    request_id: String,
) -> BoxStream<'static, Result<LlmResponse, AnthropicError>> {
    todo!("Task 4")
}
