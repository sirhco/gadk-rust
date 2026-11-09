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

#![allow(dead_code)] // D1: first used by Task 8 (`completions::model`).

use gadk_genai::{
    Candidate, Content, FinishReason, FunctionCall, GenerateContentResponse,
    GenerateContentResponseUsageMetadata, LogprobsResult, LogprobsResultCandidate,
    LogprobsResultTopCandidates, MediaModality, ModalityTokenCount, Part, ROLE_MODEL,
};
use gadk_platform::go_quote;

use crate::completions::wire::{
    ChatCompletion, ChatCompletionChoiceLogprobs, ChatCompletionMessage, CompletionUsage,
};
use crate::error::{ErrorKind, OpenAiError};
use crate::shared::calls::function_call_args;
use crate::shared::response::safe_int32;

/// Go `convertCompletion`: the genai response for a Chat Completions response (blocking, and
/// the accumulated snapshot of a stream). `None` is the bare
/// [`ErrorKind::EmptyResponse`](crate::error::ErrorKind::EmptyResponse); no choices
/// [`ErrorKind::NoChoices`](crate::error::ErrorKind::NoChoices); a first choice with nothing to
/// read [`ErrorKind::NoTextOrToolContent`](crate::error::ErrorKind::NoTextOrToolContent).
/// Usage is set only when the response carried it (`resp.usage.is_some()`).
pub(crate) fn convert_completion(
    resp: Option<&ChatCompletion>,
) -> Result<GenerateContentResponse, OpenAiError> {
    let Some(resp) = resp else {
        return Err(OpenAiError::sentinel(ErrorKind::EmptyResponse));
    };
    let Some(choice) = resp.choices.first() else {
        return Err(OpenAiError::sentinel(ErrorKind::NoChoices));
    };
    let parts = convert_message(&choice.message)?;
    if parts.is_empty() {
        return Err(OpenAiError::sentinel(ErrorKind::NoTextOrToolContent));
    }
    Ok(GenerateContentResponse {
        candidates: vec![Candidate {
            content: Some(Content {
                role: ROLE_MODEL.to_string(),
                parts,
            }),
            finish_reason: Some(finish_reason(&choice.finish_reason)),
            logprobs_result: convert_logprobs(&choice.logprobs),
            ..Candidate::default()
        }],
        model_version: resp.model.clone(),
        response_id: resp.id.clone(),
        // A provider that omits usage has not said the turn cost nothing, which zeros would;
        // streaming leaves it unset in the same case.
        usage_metadata: resp.usage.as_ref().map(convert_usage),
        ..GenerateContentResponse::default()
    })
}

/// Go `convertMessage`: an assistant message as genai parts. A refusal becomes text, as the
/// Responses path reports a refusal content block, so one model reports a refusal the same way
/// on either endpoint.
fn convert_message(msg: &ChatCompletionMessage) -> Result<Vec<Part>, OpenAiError> {
    let mut parts = Vec::new();
    if !msg.content.is_empty() {
        parts.push(text_part(&msg.content));
    }
    if !msg.refusal.is_empty() {
        parts.push(text_part(&msg.refusal));
    }
    for call in &msg.tool_calls {
        if !call.kind.is_empty() && call.kind != "function" {
            // Only function tools are ever declared, so nothing can answer it; the turn
            // fails, as the Responses path does for an output item it cannot convert.
            return Err(OpenAiError::new(
                ErrorKind::UnsupportedOutputItemType,
                format!("tool call {}", go_quote(&call.kind)),
            ));
        }
        let function = &call.function;
        if function.name.is_empty() {
            if call.id.is_empty() && function.arguments.is_empty() {
                // The accumulator pads a tool-call index a stream skipped, and no call was
                // made there.
                continue;
            }
            // Stored in the session, a nameless call would fail every later request, so the
            // turn fails here instead.
            return Err(OpenAiError::sentinel(ErrorKind::FunctionCallMissingName)
                .suffixed(format!(" (call_id {})", go_quote(&call.id))));
        }
        // Name the offending call: conversion aborts on the first error, so nothing else
        // identifies it.
        let args = function_call_args(&function.arguments).map_err(|err| {
            err.suffixed(format!(
                " (name {}, call_id {})",
                go_quote(&function.name),
                go_quote(&call.id)
            ))
        })?;
        parts.push(Part {
            function_call: Some(FunctionCall {
                name: function.name.clone(),
                id: call.id.clone(),
                args,
                ..FunctionCall::default()
            }),
            ..Part::default()
        });
    }
    Ok(parts)
}

/// A text part.
fn text_part(text: &str) -> Part {
    Part {
        text: text.to_string(),
        ..Part::default()
    }
}

/// Go `finishReason`: `stop`/`tool_calls`/`function_call` → STOP, `length` → MAX_TOKENS,
/// `content_filter` → SAFETY, `""` → FINISH_REASON_UNSPECIFIED, anything else → OTHER.
pub(crate) fn finish_reason(reason: &str) -> FinishReason {
    match reason {
        // A turn ended by calling a tool stopped cleanly, as in adk-python's LiteLLM path.
        "stop" | "tool_calls" | "function_call" => FinishReason::Stop,
        "length" => FinishReason::MaxTokens,
        "content_filter" => FinishReason::Safety,
        // Silence is not a clean stop: a caller that retries on anything but STOP must not
        // accept a partial answer as final.
        "" => FinishReason::Unspecified,
        _ => FinishReason::Other,
    }
}

/// Go `convertUsage`.
pub(crate) fn convert_usage(usage: &CompletionUsage) -> GenerateContentResponseUsageMetadata {
    GenerateContentResponseUsageMetadata {
        prompt_token_count: safe_int32(usage.prompt_tokens),
        candidates_token_count: safe_int32(usage.completion_tokens),
        total_token_count: safe_int32(usage.total_tokens),
        cached_content_token_count: safe_int32(usage.prompt_tokens_details.cached_tokens),
        prompt_tokens_details: vec![text_tokens(usage.prompt_tokens)],
        candidates_tokens_details: vec![text_tokens(usage.completion_tokens)],
        thoughts_token_count: safe_int32(usage.completion_tokens_details.reasoning_tokens),
        ..GenerateContentResponseUsageMetadata::default()
    }
}

/// A `TEXT` modality count.
fn text_tokens(count: i64) -> ModalityTokenCount {
    ModalityTokenCount {
        modality: Some(MediaModality::Text),
        token_count: safe_int32(count),
    }
}

/// Go `convertLogprobs`: the choice's content logprobs (refusal logprobs left out); `None`
/// when there are none.
pub(crate) fn convert_logprobs(logprobs: &ChatCompletionChoiceLogprobs) -> Option<LogprobsResult> {
    if logprobs.content.is_empty() {
        return None;
    }
    let mut res = LogprobsResult::default();
    for lp in &logprobs.content {
        res.chosen_candidates.push(LogprobsResultCandidate {
            token: lp.token.clone(),
            log_probability: lp.logprob as f32,
            ..LogprobsResultCandidate::default()
        });
        let candidates = lp
            .top_logprobs
            .iter()
            .map(|top| LogprobsResultCandidate {
                token: top.token.clone(),
                log_probability: top.logprob as f32,
                ..LogprobsResultCandidate::default()
            })
            .collect();
        res.top_candidates
            .push(LogprobsResultTopCandidates { candidates });
    }
    Some(res)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completions::wire::{
        ChatCompletionChoice, ChatCompletionMessageToolCall, ChatCompletionTokenLogprob,
        ChatCompletionTokenLogprobTopLogprob,
    };
    use pretty_assertions::assert_eq;

    fn with_call(call: ChatCompletionMessageToolCall) -> ChatCompletion {
        ChatCompletion {
            choices: vec![ChatCompletionChoice {
                message: ChatCompletionMessage {
                    tool_calls: vec![call],
                    ..ChatCompletionMessage::default()
                },
                ..ChatCompletionChoice::default()
            }],
            ..ChatCompletion::default()
        }
    }

    fn call(kind: &str, id: &str, name: &str, arguments: &str) -> ChatCompletionMessageToolCall {
        let mut call = ChatCompletionMessageToolCall {
            id: id.to_string(),
            kind: kind.to_string(),
            ..ChatCompletionMessageToolCall::default()
        };
        call.function.name = name.to_string();
        call.function.arguments = arguments.to_string();
        call
    }

    fn err_text(resp: &ChatCompletion) -> String {
        match convert_completion(Some(resp)) {
            Err(err) => err.to_string(),
            Ok(got) => panic!("want an error, got {got:?}"),
        }
    }

    /// The texts Go's `fmt.Errorf` wraps build (`%q` via `go_quote`).
    #[test]
    fn unusable_calls_render_go_text() {
        assert_eq!(
            err_text(&with_call(call("custom", "call_9", "", ""))),
            r#"openai: unsupported output item type: tool call "custom""#
        );
        assert_eq!(
            err_text(&with_call(call("function", "call_9", "", "{}"))),
            r#"openai: function call missing name (call_id "call_9")"#
        );
        assert_eq!(
            err_text(&with_call(call("function", "call_9", "get_weather", "{"))),
            r#"openai: parse function call args: unexpected end of JSON input (name "get_weather", call_id "call_9")"#
        );
        assert_eq!(
            err_text(&with_call(call("", "call_9", "f", "[1]"))),
            "openai: parse function call args: json: cannot unmarshal array into Go value of type map[string]interface {} (name \"f\", call_id \"call_9\")"
        );
    }

    #[test]
    fn content_refusal_and_calls_keep_their_order() {
        let mut resp = with_call(call("", "a", "f", r#"{"n":3}"#));
        resp.choices[0].message.content = "text".to_string();
        resp.choices[0].message.refusal = "no".to_string();
        let got = match convert_completion(Some(&resp)) {
            Ok(got) => got,
            Err(err) => panic!("{err}"),
        };
        let parts = match &got.candidates[0].content {
            Some(content) => &content.parts,
            None => panic!("no content"),
        };
        assert_eq!(parts.len(), 3);
        assert_eq!(
            (parts[0].text.as_str(), parts[1].text.as_str()),
            ("text", "no")
        );
        let call = parts[2].function_call.as_ref();
        assert_eq!(
            call.map(|c| c.args.get("n")),
            Some(Some(&serde_json::json!(3)))
        );
        // No usage on the response: none is reported.
        assert_eq!(got.usage_metadata, None);
        assert_eq!(
            got.candidates[0].finish_reason,
            Some(FinishReason::Unspecified)
        );
        assert_eq!(got.candidates[0].logprobs_result, None);
    }

    #[test]
    fn usage_maps_every_count() {
        let mut usage = CompletionUsage {
            prompt_tokens: 100,
            completion_tokens: 20,
            total_tokens: i64::from(i32::MAX) + 1,
            ..CompletionUsage::default()
        };
        usage.prompt_tokens_details.cached_tokens = 40;
        usage.completion_tokens_details.reasoning_tokens = 7;
        let text = |n| ModalityTokenCount {
            modality: Some(MediaModality::Text),
            token_count: n,
        };
        assert_eq!(
            convert_usage(&usage),
            GenerateContentResponseUsageMetadata {
                prompt_token_count: 100,
                candidates_token_count: 20,
                total_token_count: i32::MAX,
                cached_content_token_count: 40,
                prompt_tokens_details: vec![text(100)],
                candidates_tokens_details: vec![text(20)],
                thoughts_token_count: 7,
                ..GenerateContentResponseUsageMetadata::default()
            }
        );
    }

    #[test]
    fn logprobs_keep_content_and_leave_refusal_out() {
        let lp = |token: &str, logprob: f64| ChatCompletionTokenLogprob {
            token: token.to_string(),
            logprob,
            top_logprobs: vec![ChatCompletionTokenLogprobTopLogprob {
                token: format!("{token}?"),
                logprob: logprob - 1.0,
                ..ChatCompletionTokenLogprobTopLogprob::default()
            }],
            ..ChatCompletionTokenLogprob::default()
        };
        let only_refusal = ChatCompletionChoiceLogprobs {
            refusal: vec![lp("no", -0.1)],
            ..ChatCompletionChoiceLogprobs::default()
        };
        assert_eq!(convert_logprobs(&only_refusal), None);
        let got = convert_logprobs(&ChatCompletionChoiceLogprobs {
            content: vec![lp("a", -0.5), lp("b", -0.25)],
            refusal: vec![lp("no", -0.1)],
        });
        let candidate = |token: &str, log_probability: f32| LogprobsResultCandidate {
            token: token.to_string(),
            log_probability,
            ..LogprobsResultCandidate::default()
        };
        assert_eq!(
            got,
            Some(LogprobsResult {
                chosen_candidates: vec![candidate("a", -0.5), candidate("b", -0.25)],
                top_candidates: vec![
                    LogprobsResultTopCandidates {
                        candidates: vec![candidate("a?", -1.5)],
                    },
                    LogprobsResultTopCandidates {
                        candidates: vec![candidate("b?", -1.25)],
                    },
                ],
                log_probability_sum: None,
            })
        );
    }
}
