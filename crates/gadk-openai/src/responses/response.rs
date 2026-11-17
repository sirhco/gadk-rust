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

//! Responses response conversion.

use gadk_genai::{
    Candidate, Content, FinishReason, FunctionCall, GenerateContentResponse,
    GenerateContentResponseUsageMetadata, LogprobsResult, LogprobsResultCandidate,
    LogprobsResultTopCandidates, MediaModality, ModalityTokenCount, Part, ROLE_MODEL,
};
use gadk_platform::go_quote;
use serde_json::{Map, Value};

use crate::error::{ErrorKind, OpenAiError};
use crate::responses::wire::{Response, ResponseOutputItem, ResponseUsage, status};
use crate::shared::calls::function_call_args;
use crate::shared::response::safe_int32;
use crate::shared::servertext::clip_server_text;

/// Converts a response: `None` is the bare
/// [`ErrorKind::EmptyResponse`](crate::error::ErrorKind::EmptyResponse); a response that
/// [`reports_failure`] is [`failed_response_error`]; else one candidate from the output items.
pub(crate) fn convert_response(
    resp: Option<&Response>,
) -> Result<GenerateContentResponse, OpenAiError> {
    let Some(resp) = resp else {
        return Err(OpenAiError::sentinel(ErrorKind::EmptyResponse));
    };
    if reports_failure(resp) {
        return Err(failed_response_error(resp));
    }
    let candidate = build_candidate(resp)?;
    Ok(GenerateContentResponse {
        candidates: vec![candidate],
        model_version: resp.model_str().to_string(),
        response_id: resp.id_str().to_string(),
        usage_metadata: Some(convert_usage(&resp.usage)),
        ..GenerateContentResponse::default()
    })
}

/// Whether the response reports a failure: status `failed`; or no status and a non-blank
/// (clipped) error message
/// or code.
pub(crate) fn reports_failure(resp: &Response) -> bool {
    match resp.status_str() {
        status::FAILED => true,
        // Clipped, not raw: the renderer reports what clipping leaves, so a predicate reading
        // the raw value would send a body whose error is nothing but whitespace down the
        // failure path and then name nothing.
        "" => {
            !clip_server_text(resp.error_message()).is_empty()
                || !clip_server_text(resp.error_code()).is_empty()
        }
        _ => false,
    }
}

/// Builds the [`ErrorKind::ResponseFailed`](crate::error::ErrorKind::ResponseFailed) error
/// with the clipped, quoted id, code and message (`openai: response failed (id "…", code
/// "…"): "…"` and its shorter forms).
pub(crate) fn failed_response_error(resp: &Response) -> OpenAiError {
    // Clipped, so a value of nothing but spaces contributes nothing rather than a label or
    // separator with nothing after it.
    let msg = clip_server_text(resp.error_message());
    let mut details = Vec::new();
    let id = clip_server_text(resp.id_str());
    if !id.is_empty() {
        details.push(format!("id {}", go_quote(&id)));
    }
    let code = clip_server_text(resp.error_code());
    if !code.is_empty() {
        details.push(format!("code {}", go_quote(&code)));
    }
    let joined = details.join(", ");
    let err = OpenAiError::sentinel(ErrorKind::ResponseFailed);
    match (joined.is_empty(), msg.is_empty()) {
        (false, false) => err.suffixed(format!(" ({joined}): {}", go_quote(&msg))),
        (false, true) => err.suffixed(format!(" ({joined})")),
        (true, false) => err.suffixed(format!(": {}", go_quote(&msg))),
        // The server said "failed" and nothing more.
        (true, true) => err,
    }
}

/// Builds the candidate from the output items.
fn build_candidate(resp: &Response) -> Result<Candidate, OpenAiError> {
    let parts = convert_output_items(resp.output_items())?;
    Ok(Candidate {
        content: Some(Content {
            role: ROLE_MODEL.to_string(),
            parts,
        }),
        // No event announced this response, so its own payload is all there is to read the
        // finish reason from.
        finish_reason: Some(finish_reason(Some(resp), false)),
        logprobs_result: convert_logprobs(resp.output_items()),
        ..Candidate::default()
    })
}

/// A text part.
fn text_part(text: &str, thought: bool) -> Part {
    Part {
        text: text.to_string(),
        thought,
        ..Part::default()
    }
}

/// Converts the output items: message text and refusals, function calls, and reasoning content
/// and summaries as thought parts. No items is
/// [`ErrorKind::NoOutputItems`](crate::error::ErrorKind::NoOutputItems); no parts
/// [`ErrorKind::NoTextOrToolContent`](crate::error::ErrorKind::NoTextOrToolContent); an
/// unknown content or item type its sentinel with the quoted type.
pub(crate) fn convert_output_items(items: &[ResponseOutputItem]) -> Result<Vec<Part>, OpenAiError> {
    if items.is_empty() {
        return Err(OpenAiError::sentinel(ErrorKind::NoOutputItems));
    }
    let mut parts = Vec::new();
    for item in items {
        match item.kind.as_str() {
            "message" => {
                for content in &item.content {
                    match content.kind.as_str() {
                        "output_text" => {
                            if !content.text.is_empty() {
                                parts.push(text_part(&content.text, false));
                            }
                        }
                        "refusal" => parts.push(text_part(&content.refusal, false)),
                        other => {
                            return Err(OpenAiError::new(
                                ErrorKind::UnsupportedMessageContentType,
                                go_quote(other),
                            ));
                        }
                    }
                }
            }
            "function_call" => parts.push(convert_function_call(item)?),
            "reasoning" => {
                for chunk in &item.content {
                    if !chunk.text.is_empty() {
                        parts.push(text_part(&chunk.text, true));
                    }
                }
                for summary in &item.summary {
                    if !summary.text.is_empty() {
                        parts.push(text_part(&summary.text, true));
                    }
                }
            }
            other => {
                return Err(OpenAiError::new(
                    ErrorKind::UnsupportedOutputItemType,
                    go_quote(other),
                ));
            }
        }
    }
    if parts.is_empty() {
        return Err(OpenAiError::sentinel(ErrorKind::NoTextOrToolContent));
    }
    Ok(parts)
}

/// The function-call part for an output item; undecodable
/// arguments are `ErrorKind::FunctionCallArgs: <err> (name "…", call_id "…")`.
pub(crate) fn convert_function_call(item: &ResponseOutputItem) -> Result<Part, OpenAiError> {
    // Name the offending call: `convert_output_items` aborts the entire response on the first
    // error, so nothing else identifies it.
    let args = function_call_item_args(item).map_err(|err| {
        err.suffixed(format!(
            " (name {}, call_id {})",
            go_quote(&item.name),
            go_quote(&item.call_id)
        ))
    })?;
    Ok(Part {
        function_call: Some(FunctionCall {
            name: item.name.clone(),
            id: item.call_id.clone(),
            args,
            ..FunctionCall::default()
        }),
        ..Part::default()
    })
}

/// The arguments of a function-call output item. A JSON string is its
/// value; any other value is its original bytes, so a duplicate key resolves last-one-wins;
/// absent or `null` is no arguments.
fn function_call_item_args(item: &ResponseOutputItem) -> Result<Map<String, Value>, OpenAiError> {
    let raw = match item.arguments.as_deref().map(|raw| raw.get()) {
        None => String::new(),
        Some(text) if text.starts_with('"') => {
            // A JSON string, unquoted.
            serde_json::from_str::<String>(text).unwrap_or_default()
        }
        Some(text) => text.to_string(),
    };
    function_call_args(&raw)
}

/// The finish reason: from `incomplete_details.reason`, else [`truncated`]; `None` is
/// FINISH_REASON_UNSPECIFIED.
pub(crate) fn finish_reason(resp: Option<&Response>, incomplete_event: bool) -> FinishReason {
    let Some(resp) = resp else {
        return FinishReason::Unspecified;
    };
    match resp.incomplete_reason() {
        "max_output_tokens" => FinishReason::MaxTokens,
        "content_filter" => FinishReason::Safety,
        // No reason given, so the status and what the event was called are all that is left
        // to go on.
        "" if truncated(resp, incomplete_event) => FinishReason::Other,
        "" => FinishReason::Stop,
        _ => FinishReason::Other,
    }
}

/// Whether a turn naming no incomplete reason still ended early.
pub(crate) fn truncated(resp: &Response, incomplete_event: bool) -> bool {
    if incomplete_event {
        // The event stands in for "response.completed", so its name is the provider's
        // verdict, and it outranks a payload that says otherwise.
        return true;
    }
    match resp.status_str() {
        status::COMPLETED => false,
        // A finished turn carries incomplete_details as null.
        "" => resp.incomplete_details.is_some(),
        // failed, cancelled, incomplete, in_progress and queued all describe an unfinished
        // turn. Enumerating the finished states instead keeps a status added later from
        // defaulting to a clean stop.
        _ => true,
    }
}

/// The provider's own account of why a turn ended (`""` for none).
pub(crate) fn finish_message(resp: Option<&Response>, incomplete_event: bool) -> String {
    let Some(resp) = resp else {
        return String::new();
    };
    let msg = resp.error_message();
    if !msg.is_empty() {
        return msg.to_string();
    }
    let reason = resp.incomplete_reason();
    if !reason.is_empty() {
        return reason.to_string();
    }
    // The contradiction `truncated` resolves, resolved the same way: the event's name
    // outranks a payload calling the turn completed. Every other status is still the
    // provider's own wording for why.
    let status = resp.status_str();
    if !status.is_empty() && (!incomplete_event || status != status::COMPLETED) {
        return status.to_string();
    }
    if truncated(resp, incomplete_event) {
        // No usable reason or status, yet the turn did not finish: the event's name, or a
        // bare incomplete_details, is all the provider said.
        return status::INCOMPLETE.to_string();
    }
    String::new()
}

/// Converts the usage.
pub(crate) fn convert_usage(usage: &ResponseUsage) -> GenerateContentResponseUsageMetadata {
    GenerateContentResponseUsageMetadata {
        prompt_token_count: safe_int32(usage.input_tokens),
        candidates_token_count: safe_int32(usage.output_tokens),
        total_token_count: safe_int32(usage.total_tokens),
        cached_content_token_count: safe_int32(usage.input_tokens_details.cached_tokens),
        prompt_tokens_details: vec![ModalityTokenCount {
            modality: Some(MediaModality::Text),
            token_count: safe_int32(usage.input_tokens),
        }],
        candidates_tokens_details: vec![ModalityTokenCount {
            modality: Some(MediaModality::Text),
            token_count: safe_int32(usage.output_tokens),
        }],
        thoughts_token_count: safe_int32(usage.output_tokens_details.reasoning_tokens),
        ..GenerateContentResponseUsageMetadata::default()
    }
}

/// The response's logprobs only when its message text equals `text`;
/// `None` response is `None`.
pub(crate) fn logprobs_for(resp: Option<&Response>, text: &str) -> Option<LogprobsResult> {
    let resp = resp?;
    if output_text(resp.output_items()) != text {
        return None;
    }
    convert_logprobs(resp.output_items())
}

/// The text of the message items (output text and refusals).
pub(crate) fn output_text(items: &[ResponseOutputItem]) -> String {
    let mut text = String::new();
    for item in items.iter().filter(|item| item.kind == "message") {
        for content in &item.content {
            match content.kind.as_str() {
                "output_text" => text.push_str(&content.text),
                "refusal" => text.push_str(&content.refusal),
                _ => {}
            }
        }
    }
    text
}

/// The `output_text` logprobs of the message items; `None` when there
/// are none.
pub(crate) fn convert_logprobs(items: &[ResponseOutputItem]) -> Option<LogprobsResult> {
    let mut res: Option<LogprobsResult> = None;
    for item in items.iter().filter(|item| item.kind == "message") {
        for content in &item.content {
            if content.kind != "output_text" || content.logprobs.is_empty() {
                continue;
            }
            let res = res.get_or_insert_with(LogprobsResult::default);
            for lp in &content.logprobs {
                res.chosen_candidates.push(LogprobsResultCandidate {
                    token: lp.token.clone(),
                    log_probability: lp.logprob as f32,
                    ..LogprobsResultCandidate::default()
                });
                let candidates = lp
                    .top_logprobs
                    .iter()
                    .map(|tlp| LogprobsResultCandidate {
                        token: tlp.token.clone(),
                        log_probability: tlp.logprob as f32,
                        ..LogprobsResultCandidate::default()
                    })
                    .collect();
                res.top_candidates
                    .push(LogprobsResultTopCandidates { candidates });
            }
        }
    }
    res
}
