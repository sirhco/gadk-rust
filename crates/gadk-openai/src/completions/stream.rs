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

//! Chat Completions stream translation (port of ADK-Go `internal/completions/stream.go`, with
//! the parts of openai-go v3.82.0's `ChatCompletionAccumulator` it relies on).

#![allow(dead_code, unused_variables, clippy::todo)] // D1: Task 6 fills the bodies.

use gadk_genai::GenerateContentResponse;

use crate::completions::wire::{ChatCompletion, ChatCompletionChunk, CompletionUsage};
use crate::error::OpenAiError;

/// Go `errToolCallChunkRejected`'s text.
pub(crate) const TOOL_CALL_CHUNK_REJECTED: &str =
    "openai: streamed tool call chunk could not be accumulated";

/// Go `streamTranslator`: turns chunks into partial genai responses while accumulating the
/// whole turn (openai-go `ChatCompletionAccumulator.AddChunk`). Task 6 may add private fields.
#[derive(Debug, Default)]
pub(crate) struct StreamTranslator {
    /// The accumulated turn (Go `acc.ChatCompletion`).
    completion: ChatCompletion,
    /// The first non-empty chunk id of the stream.
    id: String,
    /// The latest usage a chunk reported (`None` if none did).
    usage: Option<CompletionUsage>,
}

impl StreamTranslator {
    /// Go `newStreamTranslator`.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Go `process`: folds `chunk` into the turn and returns the partial it contributes
    /// (text or refusal delta, via [`single_part_response`](crate::shared::response::single_part_response)),
    /// `Ok(None)` for a chunk a caller sees nothing of. A tool-call delta the accumulator
    /// refuses is [`TOOL_CALL_CHUNK_REJECTED`] (a plain error).
    pub(crate) fn process(
        &mut self,
        chunk: ChatCompletionChunk,
    ) -> Result<Option<GenerateContentResponse>, OpenAiError> {
        todo!("Task 6")
    }

    /// Go `completion`: the whole turn as the blocking path would have received it.
    pub(crate) fn completion(&self) -> &ChatCompletion {
        &self.completion
    }

    /// Go `t.usage`: the latest usage a chunk reported.
    pub(crate) fn usage(&self) -> Option<&CompletionUsage> {
        self.usage.as_ref()
    }
}
