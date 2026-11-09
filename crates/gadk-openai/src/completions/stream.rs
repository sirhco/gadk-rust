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

#![allow(dead_code)] // D1: first used by Task 8 (`completions::model`).

use gadk_genai::{GenerateContentResponse, Part};

use crate::completions::wire::{
    ChatCompletion, ChatCompletionChunk, ChatCompletionChunkChoice, CompletionUsage,
};
use crate::error::OpenAiError;
use crate::shared::response::single_part_response;

/// Go `errToolCallChunkRejected`'s text.
pub(crate) const TOOL_CALL_CHUNK_REJECTED: &str =
    "openai: streamed tool call chunk could not be accumulated";

/// openai-go `maxStreamAccumulatorChoiceIndex`: choice indices 0 through 127 are accepted.
const MAX_CHOICE_INDEX: i64 = 127;

/// openai-go `maxStreamAccumulatorToolCallGrowth`: one tool-call index may grow its choice by
/// at most this many positions.
const MAX_TOOL_CALL_GROWTH: usize = 128;

/// Go `streamTranslator`: turns chunks into partial genai responses while accumulating the
/// whole turn (openai-go `ChatCompletionAccumulator.AddChunk`). Task 6 may add private fields.
///
/// Tool calls are not emitted as they stream: nothing marks one complete (arguments arrive as
/// fragments until the stream ends), so they reach the caller on the final response, built from
/// [`completion`](Self::completion) by the converter the blocking path uses.
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
        mut chunk: ChatCompletionChunk,
    ) -> Result<Option<GenerateContentResponse>, OpenAiError> {
        // One stream is one completion, so a provider varying the id per chunk must not have
        // the accumulator refuse every chunk after the first.
        if self.id.is_empty() {
            self.id.clone_from(&chunk.id);
        }
        if !self.id.is_empty() {
            chunk.id.clone_from(&self.id);
        }
        // A chunk still refused (a choice index beyond the accumulator's bound, or a tool-call
        // index growing it too far) leaves the snapshot untouched. Its text is still yielded
        // below, but a call exists only in the snapshot.
        if !add_chunk(&mut self.completion, &chunk) && carries_tool_call(&chunk) {
            return Err(OpenAiError::plain(TOOL_CALL_CHUNK_REJECTED));
        }
        // Kept apart from the accumulator, which sums every report: right only for a provider
        // sending usage once, while several resend the running total on every chunk. The
        // latest report is the turn's total either way.
        if let Some(usage) = chunk.usage.take() {
            self.usage = Some(usage);
        }

        let Some(choice) = chunk.choices.into_iter().next() else {
            // The usage-only chunk that closes a stream requesting usage.
            return Ok(None);
        };
        let delta = choice.delta;
        let text = if !delta.content.is_empty() {
            delta.content
        } else if !delta.refusal.is_empty() {
            // Blocking reports a refusal as text, so streaming does the same.
            delta.refusal
        } else {
            return Ok(None);
        };
        Ok(Some(single_part_response(Part {
            text,
            ..Part::default()
        })))
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

/// Go `carriesToolCall`: whether any choice in `chunk` carries a tool-call delta.
fn carries_tool_call(chunk: &ChatCompletionChunk) -> bool {
    chunk
        .choices
        .iter()
        .any(|choice| !choice.delta.tool_calls.is_empty())
}

/// openai-go `ChatCompletionAccumulator.AddChunk`, the parts ADK-Go's translator observes:
/// `false`, leaving `acc` untouched, for a chunk whose id differs from the accumulated one, a
/// choice index outside `0..=127`, or a tool-call index that is negative (other than `-1`,
/// read as `0`) or grows its choice by more than 128 positions (or the chunk's shared growth
/// budget). openai-go's representable-`int` checks on text and logprob sizes cannot fail on a
/// 64-bit target and are not reproduced, nor are its `JustFinished*` events, which ADK-Go does
/// not read.
///
/// The snapshot's `usage` stays `None`: openai-go sums each chunk's usage into
/// `ChatCompletion.Usage` but never sets `JSON.Usage`, so ADK-Go's `convertCompletion` reads
/// the accumulated turn as carrying no usage. The translator keeps the latest report instead.
fn add_chunk(acc: &mut ChatCompletion, chunk: &ChatCompletionChunk) -> bool {
    if !acc.id.is_empty() && acc.id != chunk.id {
        return false;
    }
    if !valid_chunk_indices(acc, chunk) {
        return false;
    }
    accumulate_delta(acc, chunk);
    true
}

/// openai-go `validChatCompletionChunkIndices`: every choice index is in range and the
/// tool-call indices stay within their growth bounds.
fn valid_chunk_indices(acc: &ChatCompletion, chunk: &ChatCompletionChunk) -> bool {
    // One position per received entry plus 127 of sparse slack, shared by all choices (and
    // repeated choice entries) in this chunk.
    let mut remaining_growth = MAX_TOOL_CALL_GROWTH - 1
        + chunk
            .choices
            .iter()
            .map(|choice| choice.delta.tool_calls.len())
            .sum::<usize>();
    let mut tool_call_counts: [Option<usize>; MAX_CHOICE_INDEX as usize + 1] =
        [None; MAX_CHOICE_INDEX as usize + 1];
    for choice in &chunk.choices {
        let Some(choice_index) = checked_choice_index(choice.index) else {
            return false;
        };
        let count = tool_call_counts[choice_index].get_or_insert_with(|| {
            acc.choices
                .get(choice_index)
                .map_or(0, |existing| existing.message.tool_calls.len())
        });
        for tool_call in &choice.delta.tool_calls {
            let Some(tool_index) = checked_tool_call_index(tool_call.index, *count) else {
                return false;
            };
            if tool_index >= *count {
                let growth = tool_index + 1 - *count;
                if growth > remaining_growth {
                    return false;
                }
                remaining_growth -= growth;
                *count = tool_index + 1;
            }
        }
    }
    true
}

/// openai-go `checkedStreamAccumulatorChoiceIndex`.
fn checked_choice_index(index: i64) -> Option<usize> {
    if (0..=MAX_CHOICE_INDEX).contains(&index) {
        usize::try_from(index).ok()
    } else {
        None
    }
}

/// openai-go `checkedToolCallIndex`: `-1` (AWS Bedrock's index for a single tool call) is `0`;
/// a negative index, or one more than 127 positions past `count`, is refused.
fn checked_tool_call_index(index: i64, count: usize) -> Option<usize> {
    let index = if index == -1 { 0 } else { index };
    if index < 0 || index == i64::MAX {
        return None;
    }
    let index = usize::try_from(index).ok()?;
    if index >= count && index - count >= MAX_TOOL_CALL_GROWTH {
        return None;
    }
    Some(index)
}

/// openai-go `accumulateDelta` on a chunk [`valid_chunk_indices`] accepted.
fn accumulate_delta(acc: &mut ChatCompletion, chunk: &ChatCompletionChunk) {
    if acc.id.is_empty() {
        acc.id.clone_from(&chunk.id);
    }
    for delta in &chunk.choices {
        accumulate_choice(acc, delta);
    }
    // openai-go assigns the metadata from every chunk, an empty value included.
    acc.model.clone_from(&chunk.model);
    acc.created = chunk.created;
    acc.system_fingerprint.clone_from(&chunk.system_fingerprint);
    acc.service_tier.clone_from(&chunk.service_tier);
    if chunk.object == "chat.completion.chunk" {
        acc.object = "chat.completion".to_string();
    }
}

/// One choice of [`accumulate_delta`].
fn accumulate_choice(acc: &mut ChatCompletion, delta: &ChatCompletionChunkChoice) {
    let Some(choice_index) = checked_choice_index(delta.index) else {
        return;
    };
    if acc.choices.len() <= choice_index {
        acc.choices.resize_with(choice_index + 1, Default::default);
    }
    let choice = &mut acc.choices[choice_index];
    choice.index = delta.index;
    // Assigned from every delta, so a later chunk without one clears it (openai-go).
    choice.finish_reason.clone_from(&delta.finish_reason);
    let message = &mut choice.message;
    if !delta.delta.role.is_empty() {
        message.role.clone_from(&delta.delta.role);
    }
    message.content.push_str(&delta.delta.content);
    message.refusal.push_str(&delta.delta.refusal);

    for delta_tool in &delta.delta.tool_calls {
        let Some(tool_index) = checked_tool_call_index(delta_tool.index, message.tool_calls.len())
        else {
            continue;
        };
        if message.tool_calls.len() <= tool_index {
            // A skipped index is padded with an empty call, which `convert_completion` drops.
            message
                .tool_calls
                .resize_with(tool_index + 1, Default::default);
        }
        let tool = &mut message.tool_calls[tool_index];
        if !delta_tool.id.is_empty() {
            tool.id.clone_from(&delta_tool.id);
        }
        if !delta_tool.kind.is_empty() {
            tool.kind.clone_from(&delta_tool.kind);
        }
        tool.function.name.push_str(&delta_tool.function.name);
        tool.function
            .arguments
            .push_str(&delta_tool.function.arguments);
    }

    choice
        .logprobs
        .content
        .extend(delta.logprobs.content.iter().cloned());
    choice
        .logprobs
        .refusal
        .extend(delta.logprobs.refusal.iter().cloned());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completions::response::convert_completion;
    use crate::error::ErrorKind;
    use gadk_genai::{FinishReason, FunctionCall};
    use pretty_assertions::assert_eq;
    use serde_json::json;

    /// A chunk decoded from the JSON a provider streams, so the tests exercise the wire
    /// types' own decoding.
    fn chunk(raw: serde_json::Value) -> ChatCompletionChunk {
        match serde_json::from_value(raw) {
            Ok(chunk) => chunk,
            Err(err) => panic!("decode chunk: {err}"),
        }
    }

    /// The text of a partial (its single part's text).
    fn partial_text(resp: Option<GenerateContentResponse>) -> Option<String> {
        let resp = resp?;
        assert_eq!(resp.candidates.len(), 1);
        let content = resp.candidates[0].content.as_ref()?;
        assert_eq!(content.role, "model");
        assert_eq!(content.parts.len(), 1);
        Some(content.parts[0].text.clone())
    }

    fn feed(t: &mut StreamTranslator, raw: serde_json::Value) -> Option<String> {
        match t.process(chunk(raw)) {
            Ok(resp) => partial_text(resp),
            Err(err) => panic!("process: {err}"),
        }
    }

    fn tool_delta(index: i64, id: &str, name: &str, args: &str) -> serde_json::Value {
        json!({"id": "c", "choices": [{"index": 0, "delta": {"tool_calls": [
            {"index": index, "id": id, "type": if id.is_empty() { "" } else { "function" },
             "function": {"name": name, "arguments": args}}]}}]})
    }

    #[test]
    fn text_deltas_stream_and_accumulate() {
        let mut t = StreamTranslator::new();
        assert_eq!(
            feed(
                &mut t,
                json!({"id": "chatcmpl-1", "object": "chat.completion.chunk", "model": "gpt-4o-mini",
                       "created": 7, "choices": [{"index": 0, "delta": {"role": "assistant", "content": ""}}]})
            ),
            None
        );
        assert_eq!(
            feed(
                &mut t,
                json!({"id": "chatcmpl-1", "object": "chat.completion.chunk", "model": "gpt-4o-mini",
                       "choices": [{"index": 0, "delta": {"content": "hail "}}]})
            ),
            Some("hail ".to_string())
        );
        assert_eq!(
            feed(
                &mut t,
                json!({"id": "chatcmpl-1", "object": "chat.completion.chunk", "model": "gpt-4o-mini",
                       "choices": [{"index": 0, "delta": {"content": "at -7 C"}, "finish_reason": "stop"}]})
            ),
            Some("at -7 C".to_string())
        );
        let completion = t.completion();
        assert_eq!(completion.id, "chatcmpl-1");
        assert_eq!(completion.model, "gpt-4o-mini");
        assert_eq!(completion.object, "chat.completion");
        assert_eq!(completion.choices.len(), 1);
        let choice = &completion.choices[0];
        assert_eq!(choice.message.role, "assistant");
        assert_eq!(choice.message.content, "hail at -7 C");
        assert_eq!(choice.finish_reason, "stop");

        let resp = match convert_completion(Some(completion)) {
            Ok(resp) => resp,
            Err(err) => panic!("convert: {err}"),
        };
        assert_eq!(resp.response_id, "chatcmpl-1");
        assert_eq!(resp.candidates[0].finish_reason, Some(FinishReason::Stop));
        assert_eq!(resp.usage_metadata, None);
    }

    #[test]
    fn refusal_deltas_stream_as_text() {
        let mut t = StreamTranslator::new();
        assert_eq!(
            feed(
                &mut t,
                json!({"id": "c", "choices": [{"index": 0, "delta": {"refusal": "I cannot"}}]})
            ),
            Some("I cannot".to_string())
        );
        assert_eq!(
            feed(
                &mut t,
                json!({"id": "c", "choices": [{"index": 0, "delta": {"refusal": " help"}}]})
            ),
            Some(" help".to_string())
        );
        assert_eq!(t.completion().choices[0].message.refusal, "I cannot help");
    }

    #[test]
    fn tool_call_argument_deltas_accumulate_without_partials() {
        let mut t = StreamTranslator::new();
        for raw in [
            tool_delta(0, "call_1", "get_weather", ""),
            tool_delta(0, "", "", "{\"ci"),
            tool_delta(0, "", "", "ty\":\"Lisbon\"}"),
            tool_delta(1, "call_2", "get_time", "{}"),
            json!({"id": "c", "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]}),
        ] {
            // Tool calls reach the caller only on the final response.
            assert_eq!(feed(&mut t, raw), None);
        }
        let calls = &t.completion().choices[0].message.tool_calls;
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].kind, "function");
        assert_eq!(calls[0].function.name, "get_weather");
        assert_eq!(calls[0].function.arguments, r#"{"city":"Lisbon"}"#);
        assert_eq!(calls[1].function.name, "get_time");

        let resp = match convert_completion(Some(t.completion())) {
            Ok(resp) => resp,
            Err(err) => panic!("convert: {err}"),
        };
        let parts = match &resp.candidates[0].content {
            Some(content) => &content.parts,
            None => panic!("no content"),
        };
        let calls: Vec<&FunctionCall> = parts
            .iter()
            .filter_map(|part| part.function_call.as_ref())
            .collect();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].args.get("city"), Some(&json!("Lisbon")));
        assert!(calls[1].args.is_empty());
        assert_eq!(resp.candidates[0].finish_reason, Some(FinishReason::Stop));
    }

    #[test]
    fn a_skipped_tool_call_index_is_padded_and_dropped() {
        let mut t = StreamTranslator::new();
        assert_eq!(feed(&mut t, tool_delta(2, "call_3", "f", "{}")), None);
        let calls = &t.completion().choices[0].message.tool_calls;
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0], Default::default());
        assert_eq!(calls[1], Default::default());
        let resp = match convert_completion(Some(t.completion())) {
            Ok(resp) => resp,
            Err(err) => panic!("convert: {err}"),
        };
        let parts = match &resp.candidates[0].content {
            Some(content) => &content.parts,
            None => panic!("no content"),
        };
        assert_eq!(parts.len(), 1);
        assert_eq!(
            parts[0].function_call.as_ref().map(|c| c.id.as_str()),
            Some("call_3")
        );
    }

    #[test]
    fn a_minus_one_tool_call_index_is_index_zero() {
        let mut t = StreamTranslator::new();
        feed(&mut t, tool_delta(-1, "call_1", "f", "{\"a\":"));
        feed(&mut t, tool_delta(-1, "", "", "1}"));
        let calls = &t.completion().choices[0].message.tool_calls;
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.arguments, r#"{"a":1}"#);
    }

    #[test]
    fn rejected_tool_call_chunks_fail_the_stream() {
        for (name, raw) in [
            ("negative index", tool_delta(-2, "call_1", "f", "{}")),
            (
                "growth past 128 positions",
                tool_delta(128, "call_1", "f", "{}"),
            ),
            (
                "choice index past 127",
                json!({"id": "c", "choices": [{"index": 128, "delta": {"tool_calls": [
                    {"index": 0, "id": "call_1", "type": "function", "function": {"name": "f"}}]}}]}),
            ),
        ] {
            let mut t = StreamTranslator::new();
            let err = match t.process(chunk(raw)) {
                Err(err) => err,
                Ok(resp) => panic!("{name}: want an error, got {resp:?}"),
            };
            assert_eq!(err.to_string(), TOOL_CALL_CHUNK_REJECTED, "{name}");
            assert!(err.kinds().is_empty(), "{name}");
            assert_eq!(t.completion(), &ChatCompletion::default(), "{name}");
        }
        // 127 positions of slack plus one per received entry is accepted.
        let mut t = StreamTranslator::new();
        assert_eq!(feed(&mut t, tool_delta(127, "call_1", "f", "{}")), None);
        assert_eq!(t.completion().choices[0].message.tool_calls.len(), 128);
    }

    #[test]
    fn a_rejected_chunk_without_tool_calls_still_yields_its_text() {
        let mut t = StreamTranslator::new();
        assert_eq!(
            feed(
                &mut t,
                json!({"id": "c", "choices": [{"index": 128, "delta": {"content": "hi"}}]})
            ),
            Some("hi".to_string())
        );
        // The snapshot is untouched.
        assert_eq!(t.completion(), &ChatCompletion::default());
    }

    #[test]
    fn chunk_ids_follow_the_first_non_empty_one() {
        let mut t = StreamTranslator::new();
        feed(
            &mut t,
            json!({"id": "", "choices": [{"index": 0, "delta": {"content": "a"}}]}),
        );
        feed(
            &mut t,
            json!({"id": "first", "choices": [{"index": 0, "delta": {"content": "b"}}]}),
        );
        // A provider varying the id per chunk does not have later chunks refused.
        feed(
            &mut t,
            json!({"id": "other", "choices": [{"index": 0, "delta": {"content": "c"}}]}),
        );
        let mut tool = tool_delta(0, "call_1", "f", "{}");
        tool["id"] = json!("third");
        feed(&mut t, tool);
        let completion = t.completion();
        assert_eq!(completion.id, "first");
        assert_eq!(completion.choices[0].message.content, "abc");
        assert_eq!(completion.choices[0].message.tool_calls.len(), 1);
    }

    #[test]
    fn usage_is_the_latest_report_not_the_sum() {
        let mut t = StreamTranslator::new();
        assert_eq!(t.usage(), None);
        feed(
            &mut t,
            json!({"id": "c", "choices": [{"index": 0, "delta": {"content": "a"}}],
                   "usage": {"prompt_tokens": 10, "completion_tokens": 1, "total_tokens": 11}}),
        );
        feed(
            &mut t,
            json!({"id": "c", "choices": [{"index": 0, "delta": {"content": "b"}}], "usage": null}),
        );
        // The usage-only chunk that closes a stream requesting usage yields nothing.
        assert_eq!(
            feed(
                &mut t,
                json!({"id": "c", "choices": [],
                       "usage": {"prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12,
                                 "completion_tokens_details": {"reasoning_tokens": 1}}})
            ),
            None
        );
        let usage = match t.usage() {
            Some(usage) => usage,
            None => panic!("no usage"),
        };
        assert_eq!(
            (
                usage.prompt_tokens,
                usage.completion_tokens,
                usage.total_tokens
            ),
            (10, 2, 12)
        );
        assert_eq!(usage.completion_tokens_details.reasoning_tokens, 1);
        // openai-go never marks the accumulated usage present, so the snapshot carries none.
        assert_eq!(t.completion().usage, None);
    }

    #[test]
    fn finish_reason_is_the_latest_choice_delta() {
        let mut t = StreamTranslator::new();
        feed(
            &mut t,
            json!({"id": "c", "choices": [{"index": 0, "delta": {"content": "a"}, "finish_reason": "length"}]}),
        );
        assert_eq!(t.completion().choices[0].finish_reason, "length");
        // openai-go assigns it from every delta of the choice, so a later one without it
        // clears it; a usage-only chunk carries no choice and leaves it alone.
        feed(
            &mut t,
            json!({"id": "c", "choices": [{"index": 0, "delta": {"content": "b"}}]}),
        );
        assert_eq!(t.completion().choices[0].finish_reason, "");
        feed(
            &mut t,
            json!({"id": "c", "choices": [{"index": 0, "delta": {}, "finish_reason": "content_filter"}]}),
        );
        feed(&mut t, json!({"id": "c", "choices": [], "usage": {}}));
        let resp = match convert_completion(Some(t.completion())) {
            Ok(resp) => resp,
            Err(err) => panic!("convert: {err}"),
        };
        assert_eq!(resp.candidates[0].finish_reason, Some(FinishReason::Safety));
    }

    #[test]
    fn later_choices_and_logprobs_accumulate() {
        let mut t = StreamTranslator::new();
        // Only the first choice of a chunk becomes a partial.
        assert_eq!(
            feed(
                &mut t,
                json!({"id": "c", "choices": [
                    {"index": 1, "delta": {"content": "second"}},
                    {"index": 0, "delta": {"content": "first"},
                     "logprobs": {"content": [{"token": "first", "logprob": -0.5, "top_logprobs": []}]}}]})
            ),
            Some("second".to_string())
        );
        feed(
            &mut t,
            json!({"id": "c", "choices": [{"index": 0, "delta": {"content": "!"},
                   "logprobs": {"content": [{"token": "!", "logprob": -1.5, "top_logprobs": []}]}}]}),
        );
        let completion = t.completion();
        assert_eq!(completion.choices.len(), 2);
        assert_eq!(completion.choices[0].message.content, "first!");
        assert_eq!(completion.choices[1].message.content, "second");
        assert_eq!(completion.choices[1].index, 1);
        let tokens: Vec<&str> = completion.choices[0]
            .logprobs
            .content
            .iter()
            .map(|lp| lp.token.as_str())
            .collect();
        assert_eq!(tokens, ["first", "!"]);
    }

    #[test]
    fn an_empty_stream_converts_to_no_choices() {
        let t = StreamTranslator::new();
        let err = match convert_completion(Some(t.completion())) {
            Err(err) => err,
            Ok(resp) => panic!("want an error, got {resp:?}"),
        };
        assert!(err.is(ErrorKind::NoChoices));
    }
}
