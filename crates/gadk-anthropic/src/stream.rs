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

use std::collections::{BTreeMap, VecDeque};

use futures::StreamExt;
use futures::stream::BoxStream;
use gadk_genai::{Content, Part, ROLE_MODEL};
use gadk_model::sse::{SseEvent, sse_events};
use gadk_model::{BodyStream, LlmResponse};

use crate::error::{AnthropicError, api_error_from_response};
use crate::response::convert_message_to_llm_response;
use crate::wire::{ContentBlock, Message, StreamEvent};

/// The SSE event names anthropic-sdk-go v1.81.0's `ssestream.Stream.Next` decodes as a stream
/// event (`completion` and the names of its second `case`); `ping` and any other name, or no
/// name, are skipped, and `error` ends the stream.
const DECODED_EVENT_NAMES: &[&str] = &[
    "completion",
    "message_start",
    "message_delta",
    "message_stop",
    "content_block_start",
    "content_block_delta",
    "content_block_stop",
    "message",
    "user.message",
    "user.interrupt",
    "user.tool_confirmation",
    "user.custom_tool_result",
    "user.tool_result",
    "agent.message",
    "agent.thinking",
    "agent.tool_use",
    "agent.tool_result",
    "agent.mcp_tool_use",
    "agent.mcp_tool_result",
    "agent.custom_tool_use",
    "agent.thread_context_compacted",
    "session.status_running",
    "session.status_idle",
    "session.status_rescheduled",
    "session.status_terminated",
    "session.error",
    "session.deleted",
    "session.updated",
    "span.model_request_start",
    "span.model_request_end",
    "span.outcome_evaluation_start",
    "span.outcome_evaluation_ongoing",
    "span.outcome_evaluation_end",
    "user.define_outcome",
    "agent.thread_message_received",
    "agent.thread_message_sent",
    "agent.session_thread_message_received",
    "agent.session_thread_message_sent",
    "session.thread_created",
    "session.thread_status_created",
    "session.thread_status_running",
    "session.thread_status_idle",
    "session.thread_status_rescheduled",
    "session.thread_status_terminated",
    "event_start",
    "event_delta",
    "system.message",
    "workflow_run.created",
    "workflow_run.status_running",
    "workflow_run.status_idle",
    "workflow_run.status_ended",
    "workflow_run.error",
    "workflow_run.phase_started",
    "workflow_run.phase_ended",
];

/// Accumulates one streamed message: fed each [`SseEvent`], it returns the partial responses to
/// yield; [`finish`](Self::finish) returns the aggregated final response.
///
/// Events are selected by their SSE `event:` name as the SDK's `ssestream` does (`ping` and
/// unknown names are skipped); an `error` event becomes the [`ApiError`](crate::ApiError) of
/// [`crate::error::api_error_from_response`] with this response's status and `request-id`.
pub(crate) struct StreamAccumulator {
    /// Go `unwrapOutput`.
    unwrap_output: bool,
    /// The HTTP status of the streaming response (the SDK's `APIError.StatusCode` for an SSE
    /// `error` event).
    status: u16,
    /// The response's `request-id` header (`""` when absent).
    request_id: String,
    /// The message built so far (SDK `Message.Accumulate`'s receiver).
    acc: Message,
    /// Go `toolInputBuffers`: the `partial_json` of each `tool_use` block, by block index.
    tool_input_buffers: BTreeMap<i64, String>,
}

impl StreamAccumulator {
    /// A new accumulator for a response with HTTP `status` and `request-id` header
    /// `request_id` (`""` when absent); `unwrap_output` suppresses text partials and unwraps the
    /// final text (Go `unwrapOutput`).
    pub(crate) fn new(unwrap_output: bool, status: u16, request_id: &str) -> Self {
        Self {
            unwrap_output,
            status,
            request_id: request_id.to_string(),
            acc: Message::default(),
            tool_input_buffers: BTreeMap::new(),
        }
    }

    /// Feeds one event (Go: `acc.Accumulate(event)` then the partial yields): the partial
    /// responses to yield, or the error that ends the stream.
    ///
    /// Errors: an SSE `error` event → the [`ApiError`](crate::ApiError); data that does not
    /// decode, and `Accumulate`'s index errors → `anthropicmodel: request failed: <err>` (Go
    /// `classifyError` of a plain error).
    pub(crate) fn push(&mut self, event: SseEvent) -> Result<Vec<LlmResponse>, AnthropicError> {
        match event.event.as_deref() {
            Some("error") => {
                return Err(AnthropicError::api(api_error_from_response(
                    self.status,
                    &self.request_id,
                    event.data.as_bytes(),
                )));
            }
            Some(name) if DECODED_EVENT_NAMES.contains(&name) => {}
            _ => return Ok(Vec::new()),
        }
        let ev: StreamEvent = serde_json::from_str(&event.data)
            .map_err(|err| AnthropicError::request_failed(err.to_string()))?;
        accumulate(&mut self.acc, &ev).map_err(AnthropicError::request_failed)?;
        Ok(self.partials(&ev))
    }

    /// Go `generateStreaming`'s `switch event.Type` after `Accumulate`: text and thinking
    /// partials, and the tool input buffers.
    fn partials(&mut self, ev: &StreamEvent) -> Vec<LlmResponse> {
        let mut out = Vec::new();
        match ev.kind.as_str() {
            "content_block_start" => match ev.content_block.kind.as_str() {
                "text" if !self.unwrap_output && !ev.content_block.text.is_empty() => {
                    out.push(self.partial(Part::from_text(ev.content_block.text.clone())));
                }
                "thinking" if !ev.content_block.thinking.is_empty() => {
                    out.push(self.partial(thought(&ev.content_block.thinking)));
                }
                "tool_use" => {
                    self.tool_input_buffers.insert(ev.index, String::new());
                }
                _ => {}
            },
            "content_block_delta" => match ev.delta.kind.as_str() {
                "text_delta" if !self.unwrap_output && !ev.delta.text.is_empty() => {
                    out.push(self.partial(Part::from_text(ev.delta.text.clone())));
                }
                "thinking_delta" if !ev.delta.thinking.is_empty() => {
                    out.push(self.partial(thought(&ev.delta.thinking)));
                }
                "input_json_delta" if !ev.delta.partial_json.is_empty() => {
                    self.tool_input_buffers
                        .entry(ev.index)
                        .or_default()
                        .push_str(&ev.delta.partial_json);
                }
                _ => {}
            },
            _ => {}
        }
        out
    }

    /// Go `yieldPartialPart`.
    fn partial(&self, part: Part) -> LlmResponse {
        LlmResponse {
            content: Some(Content {
                parts: vec![part],
                role: ROLE_MODEL.to_string(),
            }),
            model_version: self.acc.model.clone(),
            partial: true,
            turn_complete: false,
            ..LlmResponse::default()
        }
    }

    /// After the body ended: applies the buffered tool inputs (unless the turn hit the token
    /// limit) and converts the accumulated message (Go's tail of `generateStreaming`).
    pub(crate) fn finish(mut self) -> Result<LlmResponse, AnthropicError> {
        if self.acc.stop_reason != "max_tokens"
            && self.acc.stop_reason != "model_context_window_exceeded"
        {
            for (idx, buf) in &self.tool_input_buffers {
                if let Ok(i) = usize::try_from(*idx)
                    && let Some(block) = self.acc.content.get_mut(i)
                    && !buf.is_empty()
                {
                    block.input = Some(buf.clone());
                }
            }
        }
        convert_message_to_llm_response(&self.acc, self.unwrap_output)
    }
}

/// A thought part (Go `&genai.Part{Thought: true, Text: …}`).
fn thought(text: &str) -> Part {
    Part {
        thought: true,
        text: text.to_string(),
        ..Part::default()
    }
}

/// Port of anthropic-sdk-go v1.81.0 `(*Message).Accumulate` over the fields gadk reads; the
/// error is the SDK's text. Citations (`citations_delta`) and the SDK's raw-JSON bookkeeping
/// are not modelled.
fn accumulate(acc: &mut Message, event: &StreamEvent) -> Result<(), String> {
    match event.kind.as_str() {
        "message_start" => *acc = event.message.clone(),
        "message_delta" => {
            acc.stop_reason = event.delta.stop_reason.clone();
            acc.stop_sequence = event.delta.stop_sequence.clone();
            // Every usage count here is a cumulative whole-message total, so it overwrites;
            // absent ones keep message_start's value.
            acc.usage.output_tokens = event.usage.output_tokens;
            if let Some(v) = event.usage.input_tokens {
                acc.usage.input_tokens = v;
            }
            if let Some(v) = event.usage.cache_creation_input_tokens {
                acc.usage.cache_creation_input_tokens = v;
            }
            if let Some(v) = event.usage.cache_read_input_tokens {
                acc.usage.cache_read_input_tokens = v;
            }
            if let Some(details) = &event.usage.output_tokens_details {
                acc.usage.output_tokens_details = Some(details.clone());
            }
        }
        "content_block_start" => {
            // Content blocks start in index order with no gaps.
            let len = acc.content.len();
            if usize::try_from(event.index).ok() != Some(len) {
                return Err(format!(
                    "received event of type {} for content block at index {}, expected index {}",
                    event.kind, event.index, len
                ));
            }
            acc.content.push(event.content_block.clone());
        }
        "content_block_delta" => {
            let cb = content_block_at(acc, &event.kind, event.index)?;
            match event.delta.kind.as_str() {
                "text_delta" => cb.text.push_str(&event.delta.text),
                "input_json_delta" => {
                    if !event.delta.partial_json.is_empty() {
                        match &mut cb.input {
                            Some(input) if input != "{}" => {
                                input.push_str(&event.delta.partial_json);
                            }
                            input => *input = Some(event.delta.partial_json.clone()),
                        }
                    }
                }
                "thinking_delta" => cb.thinking.push_str(&event.delta.thinking),
                "signature_delta" => cb.signature.push_str(&event.delta.signature),
                _ => {}
            }
        }
        "message_stop" => {
            // A block whose stop event never arrived has not been refreshed yet.
            for cb in &mut acc.content {
                refresh_content_block(cb);
            }
        }
        "content_block_stop" => {
            let cb = content_block_at(acc, &event.kind, event.index)?;
            refresh_content_block(cb);
        }
        _ => {}
    }
    Ok(())
}

/// SDK `checkContentBlockIndex`, returning the addressed block.
fn content_block_at<'a>(
    acc: &'a mut Message,
    event_type: &str,
    index: i64,
) -> Result<&'a mut ContentBlock, String> {
    let len = acc.content.len();
    usize::try_from(index)
        .ok()
        .and_then(|i| acc.content.get_mut(i))
        .ok_or_else(|| {
            format!(
                "received event of type {event_type} for content block at index {index} but there are only {len} content blocks"
            )
        })
}

/// The part of SDK `refreshContentBlockRaw` that changes a field gadk reads: a cut-off tool
/// call left non-JSON input, which is emptied to `{}`.
fn refresh_content_block(cb: &mut ContentBlock) {
    if let Some(input) = &cb.input
        && !input.is_empty()
        && serde_json::from_str::<serde::de::IgnoredAny>(input).is_err()
    {
        cb.input = Some("{}".to_string());
    }
}

/// Drives [`gadk_model::sse::sse_events`] over `body` through a [`StreamAccumulator`]: the
/// partial responses, then the final one; the first error (a body error through
/// [`AnthropicError::from_body_error`](crate::error::AnthropicError), or the accumulator's)
/// ends the stream.
pub(crate) fn stream_responses(
    body: BodyStream,
    unwrap_output: bool,
    status: u16,
    request_id: String,
) -> BoxStream<'static, Result<LlmResponse, AnthropicError>> {
    struct State {
        events: BoxStream<'static, Result<SseEvent, gadk_model::ModelError>>,
        acc: Option<StreamAccumulator>,
        pending: VecDeque<LlmResponse>,
    }
    impl State {
        /// Ends the stream after an error and drops the body now, as Go's deferred
        /// `stream.Close()` does when `generateStreaming` returns, not when the caller drops
        /// the stream.
        fn release(&mut self) {
            self.acc = None;
            self.events = futures::stream::empty().boxed();
        }
    }
    let state = State {
        events: sse_events(body),
        acc: Some(StreamAccumulator::new(unwrap_output, status, &request_id)),
        pending: VecDeque::new(),
    };
    futures::stream::unfold(state, |mut st| async move {
        loop {
            if let Some(resp) = st.pending.pop_front() {
                return Some((Ok(resp), st));
            }
            let acc = st.acc.as_mut()?;
            match st.events.next().await {
                Some(Ok(event)) => match acc.push(event) {
                    Ok(partials) => st.pending.extend(partials),
                    Err(err) => {
                        st.release();
                        return Some((Err(err), st));
                    }
                },
                Some(Err(err)) => {
                    st.release();
                    return Some((Err(AnthropicError::from_body_error(err, "")), st));
                }
                None => {
                    let acc = st.acc.take()?;
                    return Some((acc.finish(), st));
                }
            }
        }
    })
    .boxed()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::error::ErrorKind;
    use crate::response::STOP_REASON_METADATA_KEY;
    use bytes::Bytes;
    use gadk_genai::{FinishReason, GenerateContentResponseUsageMetadata};
    use gadk_model::ModelError;
    use pretty_assertions::assert_eq;
    use serde_json::{Value, json};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// An SSE body of `chunks` (each one body chunk).
    fn body(chunks: &[&str]) -> BodyStream {
        let chunks: Vec<Result<Bytes, ModelError>> = chunks
            .iter()
            .map(|c| Ok(Bytes::from(c.to_string())))
            .collect();
        futures::stream::iter(chunks).boxed()
    }

    /// `lines` joined with `"\n"`, as Go's tests build their SSE bodies.
    fn sse(lines: &[&str]) -> String {
        lines.join("\n")
    }

    async fn run(
        text: &str,
        unwrap_output: bool,
        request_id: &str,
    ) -> Vec<Result<LlmResponse, AnthropicError>> {
        stream_responses(body(&[text]), unwrap_output, 200, request_id.to_string())
            .collect()
            .await
    }

    async fn run_ok(text: &str, unwrap_output: bool) -> Vec<LlmResponse> {
        run(text, unwrap_output, "")
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect()
    }

    fn named(event: &str, data: &str) -> SseEvent {
        SseEvent {
            event: Some(event.to_string()),
            data: data.to_string(),
        }
    }

    fn part_texts(resp: &LlmResponse) -> Vec<String> {
        resp.content
            .as_ref()
            .unwrap()
            .parts
            .iter()
            .map(|p| p.text.clone())
            .collect()
    }

    /// The streaming exchange of `TestGenerate_Anthropic.httprr`.
    const RECORDED_STREAM: &str = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_stream_01\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-sonnet-4-5-20250929\",\"content\":[],\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{\"input_tokens\":18,\"output_tokens\":1}}}\n\nevent: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Par\"}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"is\"}}\n\nevent: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":2}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";

    #[tokio::test]
    async fn recorded_stream_yields_partials_then_final() {
        let got = run_ok(RECORDED_STREAM, false).await;
        let partial = |text: &str| LlmResponse {
            content: Some(Content {
                parts: vec![Part::from_text(text)],
                role: "model".to_string(),
            }),
            model_version: "claude-sonnet-4-5-20250929".to_string(),
            partial: true,
            ..LlmResponse::default()
        };
        let final_resp = LlmResponse {
            content: Some(Content {
                parts: vec![Part::from_text("Paris")],
                role: "model".to_string(),
            }),
            usage_metadata: Some(GenerateContentResponseUsageMetadata {
                prompt_token_count: 18,
                candidates_token_count: 2,
                total_token_count: 20,
                ..GenerateContentResponseUsageMetadata::default()
            }),
            finish_reason: Some(FinishReason::Stop),
            model_version: "claude-sonnet-4-5-20250929".to_string(),
            turn_complete: true,
            ..LlmResponse::default()
        };
        assert_eq!(got, vec![partial("Par"), partial("is"), final_resp]);
    }

    /// The recorded stream split into small body chunks gives the same responses.
    #[tokio::test]
    async fn recorded_stream_in_small_chunks() {
        let chunks: Vec<String> = RECORDED_STREAM
            .as_bytes()
            .chunks(7)
            .map(|c| String::from_utf8(c.to_vec()).unwrap())
            .collect();
        let refs: Vec<&str> = chunks.iter().map(String::as_str).collect();
        let got: Vec<_> = stream_responses(body(&refs), false, 200, String::new())
            .map(|r| r.unwrap())
            .collect()
            .await;
        assert_eq!(got, run_ok(RECORDED_STREAM, false).await);
    }

    /// Go `TestTruncatedStreamedToolCall/streaming_truncated_tool_call_returns_max_tokens_without_error_or_function_call`.
    #[tokio::test]
    async fn truncated_tool_call_returns_max_tokens_without_function_call() {
        let text = sse(&[
            r#"event: message_start"#,
            r#"data: {"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"claude-sonnet-4-5-20250929","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":1}}}"#,
            r#""#,
            r#"event: content_block_start"#,
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"get_weather","input":{}}}"#,
            r#""#,
            r#"event: content_block_delta"#,
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"city\": \"Warsa"}}"#,
            r#""#,
            r#"event: content_block_stop"#,
            r#"data: {"type":"content_block_stop","index":0}"#,
            r#""#,
            r#"event: message_delta"#,
            r#"data: {"type":"message_delta","delta":{"stop_reason":"max_tokens","stop_sequence":null},"usage":{"output_tokens":20}}"#,
            r#""#,
            r#"event: message_stop"#,
            r#"data: {"type":"message_stop"}"#,
            r#""#,
        ]);
        let got = run_ok(&text, false).await;
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].finish_reason, Some(FinishReason::MaxTokens));
        assert!(
            got[0]
                .content
                .as_ref()
                .unwrap()
                .parts
                .iter()
                .all(|p| p.function_call.is_none())
        );
    }

    /// Go `TestTruncatedStreamedToolCall/model_context_window_exceeded_…`, streaming half.
    #[tokio::test]
    async fn context_window_exceeded_omits_streamed_tool_use() {
        let text = sse(&[
            r#"event: message_start"#,
            r#"data: {"type":"message_start","message":{"id":"msg_ctx_s","type":"message","role":"assistant","model":"claude-sonnet-4-5-20250929","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":1}}}"#,
            r#""#,
            r#"event: content_block_start"#,
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":"Checking weather."}}"#,
            r#""#,
            r#"event: content_block_stop"#,
            r#"data: {"type":"content_block_stop","index":0}"#,
            r#""#,
            r#"event: content_block_start"#,
            r#"data: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_ok","name":"get_weather","input":{}}}"#,
            r#""#,
            r#"event: content_block_delta"#,
            r#"data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"city\":\"Warsaw\"}"}}"#,
            r#""#,
            r#"event: content_block_stop"#,
            r#"data: {"type":"content_block_stop","index":1}"#,
            r#""#,
            r#"event: content_block_start"#,
            r#"data: {"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_trunc","name":"get_time","input":{}}}"#,
            r#""#,
            r#"event: content_block_delta"#,
            r#"data: {"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"tz\":\"Eu"}}"#,
            r#""#,
            r#"event: content_block_stop"#,
            r#"data: {"type":"content_block_stop","index":2}"#,
            r#""#,
            r#"event: message_delta"#,
            r#"data: {"type":"message_delta","delta":{"stop_reason":"model_context_window_exceeded","stop_sequence":null},"usage":{"output_tokens":25}}"#,
            r#""#,
            r#"event: message_stop"#,
            r#"data: {"type":"message_stop"}"#,
            r#""#,
        ]);
        let got = run_ok(&text, false).await;
        let partials: Vec<_> = got.iter().filter(|r| r.partial).collect();
        assert_eq!(partials.len(), 1);
        assert_eq!(
            part_texts(partials[0]),
            vec!["Checking weather.".to_string()]
        );
        let final_resp = got.last().unwrap();
        assert!(!final_resp.partial);
        assert_eq!(final_resp.finish_reason, Some(FinishReason::MaxTokens));
        assert_eq!(
            final_resp.custom_metadata.get(STOP_REASON_METADATA_KEY),
            Some(&json!("model_context_window_exceeded"))
        );
        assert_eq!(
            part_texts(final_resp),
            vec!["Checking weather.".to_string()]
        );
    }

    fn tool_stream(partials: &[&str], stop_reason: &str) -> String {
        let mut lines = vec![
            "event: message_start".to_string(),
            r#"data: {"type":"message_start","message":{"id":"msg_t","type":"message","role":"assistant","model":"m","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":3,"output_tokens":1}}}"#.to_string(),
            String::new(),
            "event: content_block_start".to_string(),
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"get_weather","input":{}}}"#.to_string(),
            String::new(),
        ];
        for p in partials {
            lines.push("event: content_block_delta".to_string());
            lines.push(format!(
                r#"data: {{"type":"content_block_delta","index":0,"delta":{{"type":"input_json_delta","partial_json":{}}}}}"#,
                serde_json::to_string(p).unwrap()
            ));
            lines.push(String::new());
        }
        lines.extend([
            "event: content_block_stop".to_string(),
            r#"data: {"type":"content_block_stop","index":0}"#.to_string(),
            String::new(),
            "event: message_delta".to_string(),
            format!(
                r#"data: {{"type":"message_delta","delta":{{"stop_reason":"{stop_reason}","stop_sequence":null}},"usage":{{"output_tokens":9}}}}"#
            ),
            String::new(),
            "event: message_stop".to_string(),
            r#"data: {"type":"message_stop"}"#.to_string(),
            String::new(),
        ]);
        lines.join("\n")
    }

    #[tokio::test]
    async fn streamed_tool_call_input_is_the_joined_deltas() {
        let got = run_ok(
            &tool_stream(&["{\"city\":", " \"Warsaw\"}"], "tool_use"),
            false,
        )
        .await;
        assert_eq!(got.len(), 1);
        let fc = got[0].content.as_ref().unwrap().parts[0]
            .function_call
            .clone()
            .unwrap();
        assert_eq!(fc.id, "toolu_1");
        assert_eq!(fc.name, "get_weather");
        assert_eq!(Value::Object(fc.args), json!({"city": "Warsaw"}));
        assert_eq!(got[0].finish_reason, Some(FinishReason::Stop));
    }

    #[tokio::test]
    async fn streamed_tool_call_without_deltas_has_empty_args() {
        let got = run_ok(&tool_stream(&[], "tool_use"), false).await;
        let fc = got[0].content.as_ref().unwrap().parts[0]
            .function_call
            .clone()
            .unwrap();
        assert!(fc.args.is_empty());
    }

    /// A cut-off tool call whose turn did not hit the token limit: Go applies the buffered
    /// input and fails to decode it.
    #[tokio::test]
    async fn cut_off_tool_call_without_token_limit_is_decode_error() {
        let got = run(&tool_stream(&["{\"city\": \"Warsa"], "tool_use"), false, "").await;
        assert_eq!(got.len(), 1);
        let err = got.into_iter().next().unwrap().unwrap_err();
        assert_eq!(
            err.to_string(),
            r#"anthropicmodel: failed to decode tool_use input for "get_weather""#
        );
    }

    /// Go `TestOutputSchemaAndStructuredOutput/output_schema_processor_request_shape`'s stream:
    /// an object root schema streams text partials as usual.
    #[tokio::test]
    async fn object_schema_streams_partials() {
        let text = sse(&[
            r#"event: message_start"#,
            r#"data: {"type":"message_start","message":{"id":"msg_os_s","type":"message","role":"assistant","model":"claude-sonnet-4-5-20250929","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":1}}}"#,
            r#""#,
            r#"event: content_block_start"#,
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":"{\"city\":"}}"#,
            r#""#,
            r#"event: content_block_delta"#,
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"\"Warsaw\",\"temp_c\":18}"}}"#,
            r#""#,
            r#"event: content_block_stop"#,
            r#"data: {"type":"content_block_stop","index":0}"#,
            r#""#,
            r#"event: message_delta"#,
            r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":10}}"#,
            r#""#,
            r#"event: message_stop"#,
            r#"data: {"type":"message_stop"}"#,
            r#""#,
        ]);
        let got = run_ok(&text, false).await;
        let partials: Vec<String> = got
            .iter()
            .filter(|r| r.partial)
            .flat_map(part_texts)
            .collect();
        assert_eq!(
            partials,
            vec![
                r#"{"city":"#.to_string(),
                r#""Warsaw","temp_c":18}"#.to_string()
            ]
        );
        assert_eq!(
            part_texts(got.last().unwrap()),
            vec![r#"{"city":"Warsaw","temp_c":18}"#.to_string()]
        );
    }

    /// Go `TestOutputSchemaAndStructuredOutput/non_object_root_schema_wrapped_and_unwrapped`'s
    /// stream: a wrapped schema yields no partials and the unwrapped final text.
    #[tokio::test]
    async fn wrapped_schema_yields_no_partials_and_unwraps() {
        let text = sse(&[
            r#"event: message_start"#,
            r#"data: {"type":"message_start","message":{"id":"msg_arr_s","type":"message","role":"assistant","model":"claude-sonnet-4-5-20250929","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":6,"output_tokens":1}}}"#,
            r#""#,
            r#"event: content_block_start"#,
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":"{\"result\":"}}"#,
            r#""#,
            r#"event: content_block_delta"#,
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"[\"Warsaw\",\"Krakow\"]}"}}"#,
            r#""#,
            r#"event: content_block_stop"#,
            r#"data: {"type":"content_block_stop","index":0}"#,
            r#""#,
            r#"event: message_delta"#,
            r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":8}}"#,
            r#""#,
            r#"event: message_stop"#,
            r#"data: {"type":"message_stop"}"#,
            r#""#,
        ]);
        let got = run_ok(&text, true).await;
        assert_eq!(got.len(), 1);
        assert!(!got[0].partial);
        assert_eq!(
            part_texts(&got[0]),
            vec![r#"["Warsaw","Krakow"]"#.to_string()]
        );
    }

    #[tokio::test]
    async fn thinking_partials_and_signature() {
        let text = sse(&[
            "event: message_start",
            r#"data: {"type":"message_start","message":{"model":"m","content":[],"usage":{"input_tokens":1,"output_tokens":1}}}"#,
            "",
            "event: content_block_start",
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"Let ","signature":""}}"#,
            "",
            "event: content_block_delta",
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"me think"}}"#,
            "",
            "event: content_block_delta",
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig1"}}"#,
            "",
            "event: content_block_delta",
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig2"}}"#,
            "",
            "event: content_block_stop",
            r#"data: {"type":"content_block_stop","index":0}"#,
            "",
            "event: content_block_start",
            r#"data: {"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
            "",
            "event: content_block_delta",
            r#"data: {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Answer"}}"#,
            "",
            "event: message_delta",
            r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":12,"output_tokens_details":{"thinking_tokens":5}}}"#,
            "",
        ]);
        let got = run_ok(&text, true).await;
        let thought_partial = |t: &str| LlmResponse {
            content: Some(Content {
                parts: vec![Part {
                    thought: true,
                    text: t.to_string(),
                    ..Part::default()
                }],
                role: "model".to_string(),
            }),
            model_version: "m".to_string(),
            partial: true,
            ..LlmResponse::default()
        };
        // unwrap_output suppresses text partials, never thinking partials.
        assert_eq!(
            got[..2],
            [thought_partial("Let "), thought_partial("me think")]
        );
        assert_eq!(got.len(), 3);
        let final_parts = &got[2].content.as_ref().unwrap().parts;
        assert_eq!(
            final_parts[0],
            Part {
                thought: true,
                text: "Let me think".to_string(),
                thought_signature: Bytes::from_static(b"sig1sig2"),
                ..Part::default()
            }
        );
        assert_eq!(final_parts[1], Part::from_text("Answer"));
        let usage = got[2].usage_metadata.as_ref().unwrap();
        assert_eq!(usage.thoughts_token_count, 5);
        assert_eq!(usage.candidates_token_count, 7);
    }

    /// Go `TestFinishReasonAndErrorClassification/http_status_and_midstream_sse_errors`'s SSE
    /// cases: an `error` event is an `ApiError` with the stream's status (200) mapped from the
    /// error type, and the body's request ID when the response has no `request-id` header.
    #[tokio::test]
    async fn midstream_error_events() {
        let cases = [
            (
                "midstream_sse_overloaded",
                "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-sonnet-4-5-20250929\",\"content\":[],\"usage\":{\"input_tokens\":5,\"output_tokens\":1}}}\n\nevent: error\ndata: {\"type\":\"error\",\"request_id\":\"req_sse_529\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded mid-stream\"}}\n\n",
                ErrorKind::Overloaded,
                529,
                "overloaded_error",
                "anthropicmodel: api error (status 529) [type=overloaded_error] [request_id=req_sse_529]: Overloaded mid-stream",
            ),
            (
                "midstream_sse_rate_limit",
                "event: error\ndata: {\"type\":\"error\",\"request_id\":\"req_sse_429\",\"error\":{\"type\":\"rate_limit_error\",\"message\":\"Rate limited mid-stream\"}}\n\n",
                ErrorKind::RateLimited,
                429,
                "rate_limit_error",
                "anthropicmodel: api error (status 429) [type=rate_limit_error] [request_id=req_sse_429]: Rate limited mid-stream",
            ),
            (
                "midstream_sse_api_error_maps_to_500_not_200",
                "event: error\ndata: {\"type\":\"error\",\"request_id\":\"req_sse_500\",\"error\":{\"type\":\"api_error\",\"message\":\"Internal server error mid-stream\"}}\n\n",
                ErrorKind::ApiStatus,
                500,
                "api_error",
                "anthropicmodel: api error (status 500) [type=api_error] [request_id=req_sse_500]: Internal server error mid-stream",
            ),
        ];
        for (name, text, kind, code, error_type, display) in cases {
            let got = run(text, false, "").await;
            assert_eq!(got.len(), 1, "{name}");
            let err = got.into_iter().next().unwrap().unwrap_err();
            assert!(err.is(kind), "{name}: {err}");
            let api = err.api_error().expect("ApiError");
            assert_eq!(api.status_code, code, "{name}");
            assert_eq!(api.error_type, error_type, "{name}");
            assert_eq!(err.to_string(), display, "{name}");
        }
    }

    #[tokio::test]
    async fn midstream_error_prefers_request_id_header() {
        let text = "event: error\ndata: {\"type\":\"error\",\"request_id\":\"req_body\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"x\"}}\n\n";
        let err = run(text, false, "req_hdr")
            .await
            .into_iter()
            .next()
            .unwrap()
            .unwrap_err();
        assert_eq!(err.api_error().unwrap().request_id, "req_hdr");
    }

    #[tokio::test]
    async fn partials_before_an_error_are_yielded_and_nothing_after() {
        let text = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"m\"}}\n\nevent: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"Hel\"}}\n\nevent: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"api_error\",\"message\":\"boom\"}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"lo\"}}\n\n";
        let got = run(text, false, "").await;
        assert_eq!(got.len(), 2);
        assert_eq!(
            part_texts(got[0].as_ref().unwrap()),
            vec!["Hel".to_string()]
        );
        assert!(got[1].as_ref().unwrap_err().is(ErrorKind::ApiStatus));
    }

    #[test]
    fn invalid_error_event_data_is_api_status_500() {
        let mut acc = StreamAccumulator::new(false, 200, "req_1");
        let err = acc.push(named("error", "not json")).unwrap_err();
        assert!(err.is(ErrorKind::ApiStatus));
        assert_eq!(
            err.to_string(),
            "anthropicmodel: api error (status 500) [request_id=req_1]"
        );
    }

    #[test]
    fn ping_unknown_and_unnamed_events_are_skipped() {
        let mut acc = StreamAccumulator::new(false, 200, "");
        let start = r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":"x"}}"#;
        for ev in [
            named("ping", r#"{"type":"ping"}"#),
            named("something_new", start),
            SseEvent {
                event: None,
                data: start.to_string(),
            },
            // Skipped before decoding, so invalid data does not matter.
            named("ping", "not json"),
        ] {
            assert_eq!(acc.push(ev).unwrap(), Vec::<LlmResponse>::new());
        }
        assert!(acc.acc.content.is_empty());
        // A decoded name selects decoding; the data's own `type` drives accumulation.
        let got = acc.push(named("message", start)).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(acc.acc.content.len(), 1);
    }

    #[test]
    fn undecodable_event_is_request_failed() {
        let mut acc = StreamAccumulator::new(false, 200, "");
        let err = acc.push(named("message_start", "{not json")).unwrap_err();
        assert!(
            err.to_string()
                .starts_with("anthropicmodel: request failed: "),
            "{err}"
        );
        assert!(err.kinds().is_empty());
        assert!(err.api_error().is_none());
    }

    #[test]
    fn content_block_index_errors() {
        let mut acc = StreamAccumulator::new(false, 200, "");
        let err = acc
            .push(named(
                "content_block_start",
                r#"{"type":"content_block_start","index":1,"content_block":{"type":"text"}}"#,
            ))
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "anthropicmodel: request failed: received event of type content_block_start for content block at index 1, expected index 0"
        );

        let mut acc = StreamAccumulator::new(false, 200, "");
        let err = acc
            .push(named(
                "content_block_delta",
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"x"}}"#,
            ))
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "anthropicmodel: request failed: received event of type content_block_delta for content block at index 0 but there are only 0 content blocks"
        );

        let mut acc = StreamAccumulator::new(false, 200, "");
        let err = acc
            .push(named(
                "content_block_stop",
                r#"{"type":"content_block_stop","index":-1}"#,
            ))
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "anthropicmodel: request failed: received event of type content_block_stop for content block at index -1 but there are only 0 content blocks"
        );
    }

    #[test]
    fn message_delta_usage_overwrites_only_present_counts() {
        let mut acc = StreamAccumulator::new(false, 200, "");
        acc.push(named(
            "message_start",
            r#"{"type":"message_start","message":{"model":"m","usage":{"input_tokens":10,"cache_read_input_tokens":4,"cache_creation_input_tokens":3,"output_tokens":1}}}"#,
        ))
        .unwrap();
        acc.push(named(
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"stop_sequence","stop_sequence":"END"},"usage":{"output_tokens":7,"cache_read_input_tokens":null,"cache_creation_input_tokens":5}}"#,
        ))
        .unwrap();
        assert_eq!(acc.acc.stop_reason, "stop_sequence");
        assert_eq!(acc.acc.stop_sequence, "END");
        assert_eq!(acc.acc.usage.input_tokens, 10);
        assert_eq!(acc.acc.usage.cache_read_input_tokens, 4);
        assert_eq!(acc.acc.usage.cache_creation_input_tokens, 5);
        assert_eq!(acc.acc.usage.output_tokens, 7);
        let resp = acc.finish().unwrap();
        assert_eq!(
            resp.custom_metadata
                .get(crate::response::CACHE_CREATION_INPUT_TOKENS_METADATA_KEY),
            Some(&json!(5))
        );
        assert_eq!(resp.usage_metadata.unwrap().prompt_token_count, 19);
    }

    #[test]
    fn input_json_delta_replaces_empty_object_and_appends_otherwise() {
        let mut acc = StreamAccumulator::new(false, 200, "");
        acc.push(named(
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"a","name":"f","input":{}}}"#,
        ))
        .unwrap();
        acc.push(named(
            "content_block_start",
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"b","name":"g","input":{"x":1}}}"#,
        ))
        .unwrap();
        for idx in [0, 1] {
            acc.push(named(
                "content_block_delta",
                &format!(
                    r#"{{"type":"content_block_delta","index":{idx},"delta":{{"type":"input_json_delta","partial_json":"{{\"y\":2}}"}}}}"#
                ),
            ))
            .unwrap();
        }
        assert_eq!(acc.acc.content[0].input.as_deref(), Some(r#"{"y":2}"#));
        assert_eq!(
            acc.acc.content[1].input.as_deref(),
            Some(r#"{"x":1}{"y":2}"#)
        );
        // content_block_stop empties the invalid input, as the SDK's refresh does.
        acc.push(named(
            "content_block_stop",
            r#"{"type":"content_block_stop","index":1}"#,
        ))
        .unwrap();
        assert_eq!(acc.acc.content[1].input.as_deref(), Some("{}"));
        assert_eq!(
            acc.tool_input_buffers.get(&0).map(String::as_str),
            Some(r#"{"y":2}"#)
        );
        assert_eq!(
            acc.tool_input_buffers.get(&1).map(String::as_str),
            Some(r#"{"y":2}"#)
        );
    }

    #[test]
    fn message_stop_refreshes_every_block() {
        let mut acc = StreamAccumulator::new(false, 200, "");
        acc.push(named(
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"a","name":"f","input":{}}}"#,
        ))
        .unwrap();
        acc.push(named(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"a\":"}}"#,
        ))
        .unwrap();
        assert_eq!(acc.acc.content[0].input.as_deref(), Some(r#"{"a":"#));
        acc.push(named("message_stop", r#"{"type":"message_stop"}"#))
            .unwrap();
        assert_eq!(acc.acc.content[0].input.as_deref(), Some("{}"));
    }

    #[tokio::test]
    async fn body_error_is_request_failed_after_partials() {
        let chunks: Vec<Result<Bytes, ModelError>> = vec![
            Ok(Bytes::from_static(
                b"event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"Hi\"}}\n\n",
            )),
            Err(ModelError::Transport(Box::new(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "connection reset",
            )))),
        ];
        let got: Vec<_> = stream_responses(
            futures::stream::iter(chunks).boxed(),
            false,
            200,
            String::new(),
        )
        .collect()
        .await;
        assert_eq!(got.len(), 2);
        assert_eq!(part_texts(got[0].as_ref().unwrap()), vec!["Hi".to_string()]);
        // Go: the SDK's SSE decoder returns the scanner's read error (not a *url.Error), so
        // classifyError gives `request failed`, with no `error reading response body:` prefix.
        let err = got[1].as_ref().unwrap_err();
        assert!(!err.is_transport());
        assert_eq!(
            err.to_string(),
            "anthropicmodel: request failed: connection reset"
        );
    }

    /// Go closes the body as soon as `generateStreaming` returns: the stream drops the body on
    /// its first error, while the caller still holds the stream.
    #[tokio::test]
    async fn error_releases_the_body_immediately() {
        struct DropFlag(Arc<AtomicBool>);
        impl Drop for DropFlag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let cases: [(&str, Result<Bytes, ModelError>); 3] = [
            (
                "error_event",
                Ok(Bytes::from_static(
                    b"event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"api_error\",\"message\":\"x\"}}\n\n",
                )),
            ),
            (
                "undecodable_event",
                Ok(Bytes::from_static(b"event: message_start\ndata: {\n\n")),
            ),
            (
                "body_error",
                Err(ModelError::Transport(Box::new(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "connection reset",
                )))),
            ),
        ];
        for (name, chunk) in cases {
            let dropped = Arc::new(AtomicBool::new(false));
            let guard = DropFlag(Arc::clone(&dropped));
            // The body never ends after the first chunk, like a live connection.
            let body = futures::stream::iter([chunk])
                .chain(futures::stream::pending())
                .map(move |item| {
                    let _ = &guard;
                    item
                })
                .boxed();
            let mut stream = stream_responses(body, false, 200, String::new());
            assert!(stream.next().await.unwrap().is_err(), "{name}");
            assert!(dropped.load(Ordering::SeqCst), "{name}: body still held");
            assert!(stream.next().await.is_none(), "{name}");
        }
    }

    #[tokio::test]
    async fn deadline_from_the_body_is_kept() {
        let chunks: Vec<Result<Bytes, ModelError>> =
            vec![Err(AnthropicError::deadline_exceeded().into())];
        let got: Vec<_> = stream_responses(
            futures::stream::iter(chunks).boxed(),
            false,
            200,
            String::new(),
        )
        .collect()
        .await;
        assert_eq!(got.len(), 1);
        assert!(got[0].as_ref().unwrap_err().is_deadline_exceeded());
    }

    #[tokio::test]
    async fn empty_body_yields_an_empty_final_response() {
        let got = run_ok("", false).await;
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].finish_reason, Some(FinishReason::Unspecified));
        assert!(got[0].turn_complete);
        assert_eq!(got[0].content.as_ref().unwrap().parts, Vec::<Part>::new());
    }
}
