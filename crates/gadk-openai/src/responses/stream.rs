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

#![allow(dead_code)] // D1: first used by Task 9 (`responses::model`).

use std::collections::HashMap;

use gadk_genai::{FunctionCall, GenerateContentResponse, Part};
use gadk_platform::go_quote;

use crate::error::OpenAiError;
use crate::responses::consts::{
    ERROR_EVENT, RESPONSE_FAILED, RESPONSE_FUNCTION_CALL_ARGUMENTS_DELTA,
    RESPONSE_FUNCTION_CALL_ARGUMENTS_DONE, RESPONSE_OUTPUT_ITEM_ADDED, RESPONSE_OUTPUT_TEXT_DELTA,
    RESPONSE_REASONING_SUMMARY_TEXT_DELTA, RESPONSE_REASONING_TEXT_DELTA, RESPONSE_REFUSAL_DELTA,
};
use crate::responses::response::failed_response_error;
use crate::responses::wire::ResponseStreamEvent;
use crate::shared::calls::unmarshal_function_args;
use crate::shared::response::single_part_response;
use crate::shared::servertext::clip_server_text;

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
        match event.kind.as_str() {
            // Text deltas become a response with a single text part; blocking responses
            // expose refusals as text, so streaming does the same.
            RESPONSE_OUTPUT_TEXT_DELTA | RESPONSE_REFUSAL_DELTA => {
                Ok(delta_response(&event.delta, false))
            }
            // Reasoning text and summary deltas are thought parts.
            RESPONSE_REASONING_TEXT_DELTA | RESPONSE_REASONING_SUMMARY_TEXT_DELTA => {
                Ok(delta_response(&event.delta, true))
            }
            RESPONSE_FUNCTION_CALL_ARGUMENTS_DELTA => {
                if !event.delta.is_empty() {
                    // Arguments are buffered as they stream in, by item id.
                    self.buffer(&event.item_id).push_str(&event.delta);
                }
                Ok(None)
            }
            RESPONSE_FUNCTION_CALL_ARGUMENTS_DONE => {
                // Complete arguments: emit the whole function call.
                let part = self.emit_function_call(event)?;
                Ok(Some(single_part_response(part)))
            }
            // Built by the same renderer the blocking path uses, so one server failure reads
            // the same however it arrived.
            RESPONSE_FAILED => Err(failed_response_error(&event.response)),
            ERROR_EVENT => {
                // Same treatment as a failed response body: the text is the server's, so it
                // is capped, and quoted rather than interpolated bare.
                let msg = clip_server_text(&event.message);
                if msg.is_empty() {
                    return Err(OpenAiError::plain("openai stream error"));
                }
                Err(OpenAiError::plain(format!(
                    "openai stream error: {}",
                    go_quote(&msg)
                )))
            }
            RESPONSE_OUTPUT_ITEM_ADDED => {
                let item = &event.item;
                if !item.id.is_empty() && !item.call_id.is_empty() {
                    self.item_to_call_id
                        .insert(item.id.clone(), item.call_id.clone());
                }
                if !item.id.is_empty() && !item.name.is_empty() {
                    self.item_to_name.insert(item.id.clone(), item.name.clone());
                }
                Ok(None)
            }
            // The `*.done`, `response.completed`, `response.incomplete`,
            // `response.in_progress` and `response.output_item.done` events are informational,
            // with no part of their own (the stream loop reads the finish reason off the
            // terminal ones directly); any other event type is ignored.
            _ => Ok(None),
        }
    }

    /// Go `buffer`: the argument buffer of item `id` (`""` is `"default"`).
    fn buffer(&mut self, id: &str) -> &mut String {
        let id = if id.is_empty() { "default" } else { id };
        self.function_args.entry(id.to_string()).or_default()
    }

    /// Go `emitFunctionCall`: the function call a `response.function_call_arguments.done`
    /// completes, from its own arguments or else the buffered deltas (looked up by the event's
    /// raw item id, as Go does), with the call id and name `response.output_item.added`
    /// recorded. The item's buffered state is dropped.
    fn emit_function_call(&mut self, done: &ResponseStreamEvent) -> Result<Part, OpenAiError> {
        let buffered = self.function_args.remove(&done.item_id);
        let mut payload = done.arguments.clone();
        if payload.is_empty() {
            payload = buffered.unwrap_or_default();
        }
        let call_id = self
            .item_to_call_id
            .remove(&done.item_id)
            .unwrap_or_else(|| done.item_id.clone());
        // The done event carries no name of its own: it arrived on the function_call item of
        // the earlier response.output_item.added event.
        let name = self.item_to_name.remove(&done.item_id).unwrap_or_default();
        if payload.is_empty() {
            payload = "{}".to_string();
        }
        let args = unmarshal_function_args(&payload)
            .map_err(|err| {
                OpenAiError::plain(format!("openai: parse streamed function args: {err}"))
            })?
            // Match the blocking path: JSON null means the call takes no arguments.
            .unwrap_or_default();
        Ok(Part {
            function_call: Some(FunctionCall {
                name,
                id: call_id,
                args,
                ..FunctionCall::default()
            }),
            ..Part::default()
        })
    }
}

/// A delta event's single-part response, or `None` for an empty delta.
fn delta_response(delta: &str, thought: bool) -> Option<GenerateContentResponse> {
    if delta.is_empty() {
        return None;
    }
    Some(single_part_response(Part {
        text: delta.to_string(),
        thought,
        ..Part::default()
    }))
}
