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

//! Responses stream translation (port of ADK-Go `internal/responses/stream.go`).

#![allow(dead_code, unused_variables, clippy::todo)] // D1: Task 7 fills the bodies.

use std::collections::HashMap;

use gadk_genai::GenerateContentResponse;

use crate::error::OpenAiError;
use crate::responses::wire::ResponseStreamEvent;

/// Go `streamTranslator`: turns stream events into partial genai responses, buffering a
/// function call's argument deltas until its `response.function_call_arguments.done`. Task 7
/// may add private fields.
#[derive(Debug, Default)]
pub(crate) struct StreamTranslator {
    /// Go `functionArgs`: argument deltas by item id (`""` → `"default"`).
    function_args: HashMap<String, String>,
    /// Go `itemToCallID`: item id → call id, from `response.output_item.added`.
    item_to_call_id: HashMap<String, String>,
    /// Go `itemToName`: item id → function name, from `response.output_item.added`.
    item_to_name: HashMap<String, String>,
}

impl StreamTranslator {
    /// Go `newStreamTranslator`.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Go `process`: the partial `event` contributes (text, refusal and reasoning deltas; a
    /// completed function call), `Ok(None)` for an event with none. `response.failed` is
    /// [`failed_response_error`](crate::responses::response::failed_response_error); an
    /// `error` event `openai stream error: "<clipped message>"` (or `openai stream error`);
    /// undecodable streamed arguments `openai: parse streamed function args: <err>` (plain).
    pub(crate) fn process(
        &mut self,
        event: &ResponseStreamEvent,
    ) -> Result<Option<GenerateContentResponse>, OpenAiError> {
        todo!("Task 7")
    }
}
