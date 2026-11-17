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

//! Aggregation of streamed model responses: [`StreamingResponseAggregator`].

use bytes::Bytes;
use gadk_genai::{
    CitationMetadata, Content, FinishReason, FunctionCall, GenerateContentResponse,
    GenerateContentResponseUsageMetadata, GroundingMetadata, Part, PartialArg, ROLE_MODEL,
};
use serde_json::{Map, Value};

use crate::convert::llm_response_from_genai;
use crate::error::ModelError;
use crate::jsonpath::{PathSegment, parse_json_path, set_value_by_path, value_by_path};
use crate::llm::LlmResponse;

/// Aggregates partial streaming responses: it yields an [`LlmResponse`] for every (partial)
/// model response, and builds one aggregated response from their content, returned by
/// [`close`](Self::close) once the stream ends. Text is buffered and merged, function calls
/// whose arguments are streamed as JSON-path-addressed partial arguments are assembled, and
/// thought signatures are propagated to the function call they belong to.
#[derive(Debug, Default)]
pub struct StreamingResponseAggregator {
    usage_metadata: Option<GenerateContentResponseUsageMetadata>,
    grounding_metadata: Option<GroundingMetadata>,
    citation_metadata: Option<CitationMetadata>,
    response: Option<LlmResponse>,

    current_thought_signature: Option<Bytes>,

    sequence: Vec<Part>,
    current_text_buffer: String,
    current_text_is_thought: bool,
    /// The first thought signature streamed on a chunk of `current_text_buffer`, for the part
    /// the buffer is flushed into. Unlike adk-python, the same signature is also still carried
    /// forward to the next function call that has none (`current_thought_signature`), which
    /// the turn history needs on Vertex AI.
    current_text_signature: Option<Bytes>,
    finish_reason: Option<FinishReason>,

    current_function_name: String,
    current_function_id: String,
    current_function_args: Map<String, Value>,
    current_function_thought_signature: Option<Bytes>,
}

impl StreamingResponseAggregator {
    /// Creates an empty aggregator.
    pub fn new() -> Self {
        Self::default()
    }

    /// Converts `resp` into an [`LlmResponse`] (marked partial), aggregates its content, and
    /// returns the items to yield, in order. The returned items never contain an error today;
    /// the `Result` leaves room for a future error.
    pub fn process_response(
        &mut self,
        resp: GenerateContentResponse,
    ) -> Vec<Result<LlmResponse, ModelError>> {
        let mut llm_resp = llm_response_from_genai(&resp);
        if let Some(candidate) = resp.candidates.first() {
            llm_resp.turn_complete = candidate.finish_reason.is_some();
        }
        let mut out = Vec::with_capacity(2);
        // Aggregate the response and check if an intermediate event to yield was created.
        if let Some(aggr_resp) = self.aggregate_response(&mut llm_resp) {
            out.push(Ok(aggr_resp));
        }
        // Yield the processed response.
        out.push(Ok(llm_resp));
        out
    }

    /// Folds `llm_response` into the aggregate state and marks it partial. It never produces an
    /// intermediate response.
    fn aggregate_response(&mut self, llm_response: &mut LlmResponse) -> Option<LlmResponse> {
        if llm_response.usage_metadata.is_some() {
            self.usage_metadata.clone_from(&llm_response.usage_metadata);
        }
        if llm_response.grounding_metadata.is_some() {
            self.grounding_metadata
                .clone_from(&llm_response.grounding_metadata);
        }
        if llm_response.citation_metadata.is_some() {
            self.citation_metadata
                .clone_from(&llm_response.citation_metadata);
        }
        if llm_response.finish_reason.is_some() {
            self.finish_reason.clone_from(&llm_response.finish_reason);
        }
        llm_response.partial = true;

        if let Some(content) = llm_response.content.as_mut() {
            for part in &mut content.parts {
                // Gemini 3 in streaming returns a last response with an empty part; filter it.
                if *part == Part::default() {
                    continue;
                }
                if !part.thought_signature.is_empty() {
                    self.current_thought_signature = Some(part.thought_signature.clone());
                }
                if !part.text.is_empty() {
                    if !self.current_text_buffer.is_empty()
                        && part.thought != self.current_text_is_thought
                    {
                        self.flush_text_buffer_to_sequence();
                    }
                    if self.current_text_buffer.is_empty() {
                        self.current_text_is_thought = part.thought;
                    }
                    self.current_text_buffer.push_str(&part.text);
                    if !part.thought_signature.is_empty() && self.current_text_signature.is_none() {
                        self.current_text_signature = Some(part.thought_signature.clone());
                    }
                } else if part.function_call.is_some() {
                    // Handles both streaming (partial) and non-streaming args.
                    self.process_function_call_part(part);
                } else {
                    // Other non-text parts (bytes, etc.): flush buffered text first.
                    self.flush_text_buffer_to_sequence();
                    self.sequence.push(part.clone());
                }
            }
        }
        // The response is stored before the loop above mutates its parts; storing the finished
        // response here observes the same final state.
        self.response = Some(llm_response.clone());
        None
    }

    /// Takes `part` mutably because a propagated thought signature and a streamed
    /// call's id are written into the part itself, which is also the part of the response
    /// being yielded.
    fn process_function_call_part(&mut self, part: &mut Part) {
        let Some(fc) = part.function_call.as_mut() else {
            return;
        };
        if !fc.partial_args.is_empty() || fc.will_continue == Some(true) {
            if !part.thought_signature.is_empty()
                && self.current_function_thought_signature.is_none()
            {
                self.current_function_thought_signature = Some(part.thought_signature.clone());
            }
            self.process_streaming_function_call_part(fc);
        } else if fc.name.is_empty() {
            // A nameless chunk continues the call being streamed, for example by only setting
            // will_continue to false, so it carries that call's id.
            if fc.id.is_empty() {
                fc.id.clone_from(&self.current_function_id);
            }
        } else {
            self.flush_text_buffer_to_sequence();
            if part.thought_signature.is_empty()
                && let Some(sig) = self.current_thought_signature.as_ref()
            {
                part.thought_signature = sig.clone();
            }
            self.current_thought_signature = None;
            self.sequence.push(part.clone());
        }
    }

    /// Processes a streaming function call carrying partial arguments. A chunk without an id
    /// is given the streamed call's id, in the yielded part too.
    fn process_streaming_function_call_part(&mut self, fc: &mut FunctionCall) {
        if !fc.name.is_empty() {
            self.current_function_name.clone_from(&fc.name);
        }
        if fc.id.is_empty() {
            fc.id.clone_from(&self.current_function_id);
        } else {
            self.current_function_id.clone_from(&fc.id);
        }
        for arg in &fc.partial_args {
            let Some(segments) = parse_json_path(&arg.json_path) else {
                continue;
            };
            let Some(value) = self.get_value_from_partial_arg(arg, &segments) else {
                continue;
            };
            set_value_by_path(&mut self.current_function_args, &segments, value);
        }
        if fc.will_continue == Some(true) {
            return;
        }
        self.flush_text_buffer_to_sequence();
        self.flush_function_call_to_sequence();
    }

    /// The value a partial argument carries, or `None` when it carries none. A string is
    /// streamed in chunks that all carry the same path, so a string chunk is appended to
    /// whatever of it has already been assembled.
    fn get_value_from_partial_arg(
        &self,
        partial_arg: &PartialArg,
        segments: &[PathSegment],
    ) -> Option<Value> {
        if !partial_arg.string_value.is_empty() {
            let chunk = &partial_arg.string_value;
            let value = match value_by_path(&self.current_function_args, segments) {
                Some(Value::String(existing)) => format!("{existing}{chunk}"),
                _ => chunk.clone(),
            };
            return Some(Value::String(value));
        }
        if let Some(n) = partial_arg.number_value {
            // Non-finite numbers have no JSON representation, so they become JSON `null`.
            return Some(serde_json::Number::from_f64(n).map_or(Value::Null, Value::Number));
        }
        if let Some(b) = partial_arg.bool_value {
            return Some(Value::Bool(b));
        }
        if !partial_arg.null_value.is_empty() {
            return Some(Value::Null);
        }
        None
    }

    /// Moves the buffered text, if any, into the sequence as one text part.
    fn flush_text_buffer_to_sequence(&mut self) {
        if !self.current_text_buffer.is_empty() {
            self.sequence.push(Part {
                text: std::mem::take(&mut self.current_text_buffer),
                thought: self.current_text_is_thought,
                thought_signature: self.current_text_signature.take().unwrap_or_default(),
                ..Default::default()
            });
            self.current_text_is_thought = false;
        }
    }

    /// Moves the pending function call, if any, into the sequence as one function-call part.
    fn flush_function_call_to_sequence(&mut self) {
        if !self.current_function_name.is_empty() {
            let fc = FunctionCall {
                name: std::mem::take(&mut self.current_function_name),
                args: std::mem::take(&mut self.current_function_args),
                id: std::mem::take(&mut self.current_function_id),
                ..Default::default()
            };
            self.sequence.push(Part {
                function_call: Some(fc),
                thought_signature: self
                    .current_function_thought_signature
                    .take()
                    .unwrap_or_default(),
                ..Default::default()
            });
        }
    }

    /// Generates the aggregated response at the end of the stream, if any response was
    /// processed. Call it after all the model responses are processed.
    pub fn close(&mut self) -> Option<LlmResponse> {
        let response = self.response.as_ref()?;
        let (error_code, error_message) = if self.finish_reason == Some(FinishReason::Stop) {
            (String::new(), String::new())
        } else {
            (response.error_code.clone(), response.error_message.clone())
        };
        let model_version = response.model_version.clone();
        self.flush_text_buffer_to_sequence();
        self.flush_function_call_to_sequence();
        Some(LlmResponse {
            content: Some(Content {
                parts: self.sequence.clone(),
                role: ROLE_MODEL.to_owned(),
            }),
            usage_metadata: self.usage_metadata.clone(),
            grounding_metadata: self.grounding_metadata.clone(),
            citation_metadata: self.citation_metadata.clone(),
            error_code,
            error_message,
            finish_reason: self.finish_reason.clone(),
            model_version,
            ..Default::default()
        })
    }
}
