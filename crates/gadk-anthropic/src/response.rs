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

//! Message → `LlmResponse` (port of ADK-Go `response.go`), and the decoding of a
//! non-streaming response body.

use std::collections::HashMap;

use bytes::Bytes;
use gadk_genai::{
    Content, FinishReason, FunctionCall, GenerateContentResponseUsageMetadata, Part, ROLE_MODEL,
};
use gadk_model::LlmResponse;
use serde_json::value::RawValue;
use serde_json::{Map, Value};

use crate::error::{AnthropicError, go_quote};
use crate::thinking::WRAPPED_OUTPUT_SCHEMA_KEY;
use crate::wire::{ContentBlock, Message, Usage};

/// `LlmResponse.custom_metadata` key holding the raw Claude `stop_reason` when it does not map
/// one to one to a finish reason (Go `StopReasonMetadataKey`).
pub const STOP_REASON_METADATA_KEY: &str = "anthropic_stop_reason";

/// `LlmResponse.custom_metadata` key holding the prompt-cache creation token count when > 0
/// (Go `CacheCreationInputTokensMetadataKey`).
pub const CACHE_CREATION_INPUT_TOKENS_METADATA_KEY: &str = "anthropic_cache_creation_input_tokens";

/// Decodes a 2xx non-streaming body as anthropic-sdk-go's `decode` does: a non-JSON
/// `content_type` fails with the SDK's `expected destination type of 'string' or '[]byte' for
/// responses with content-type '<ct>' that is not 'application/json'`, a JSON decode error with
/// `error parsing response json: <err>`, both wrapped by
/// [`AnthropicError::request_failed`](crate::error::AnthropicError) as Go `classifyError` does.
///
/// As Go's `json.Decoder.Decode`, only the first JSON value of the body is decoded (trailing
/// bytes are ignored) and an empty body fails with `EOF`. Other decode error texts are
/// serde_json's.
pub(crate) fn decode_message(content_type: &str, body: &[u8]) -> Result<Message, AnthropicError> {
    let media = media_type(content_type);
    let is_json = media.contains("application/json") || media.ends_with("+json");
    if !is_json {
        return Err(AnthropicError::request_failed(format!(
            "expected destination type of 'string' or '[]byte' for responses with content-type '{content_type}' that is not 'application/json'"
        )));
    }
    let mut values = serde_json::Deserializer::from_slice(body).into_iter::<Message>();
    match values.next() {
        Some(Ok(msg)) => Ok(msg),
        Some(Err(err)) => Err(AnthropicError::request_failed(format!(
            "error parsing response json: {err}"
        ))),
        None => Err(AnthropicError::request_failed(
            "error parsing response json: EOF",
        )),
    }
}

/// The media type of Go `mime.ParseMediaType` as the SDK reads it (the error is ignored):
/// lowercased, parameters dropped, or `""` when it is not `type[/subtype]` of MIME token
/// characters or when a parameter name repeats with a different value (Go
/// `errDuplicateParamName`). A malformed parameter ends parsing and keeps the media type (Go
/// `ErrInvalidMediaParameter`); parameter values are otherwise unused.
fn media_type(content_type: &str) -> String {
    let (base, mut rest) = match content_type.find(';') {
        Some(i) => content_type.split_at(i),
        None => (content_type, ""),
    };
    let media = base.to_lowercase().trim().to_string();
    let valid = match media.split_once('/') {
        Some((kind, sub)) => is_mime_token(kind) && is_mime_token(sub),
        None => is_mime_token(&media),
    };
    if !valid {
        return String::new();
    }
    let mut params: HashMap<String, Vec<u8>> = HashMap::new();
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        let Some((key, value, after)) = consume_media_param(rest) else {
            // A trailing `;` is not an error; anything else is ErrInvalidMediaParameter, which
            // keeps the media type.
            break;
        };
        match params.get(&key) {
            Some(prev) if *prev != value => return String::new(),
            _ => {}
        }
        params.insert(key, value);
        rest = after;
    }
    media
}

/// Go `mime` `consumeToken`: the leading run of token bytes and the rest.
fn consume_token(v: &str) -> (&str, &str) {
    let end = v
        .bytes()
        .position(|b| !is_mime_token_byte(b))
        .unwrap_or(v.len());
    v.split_at(end)
}

/// Go `mime` `consumeValue`: a token or a quoted string (with Go's MSIE-tolerant escapes);
/// `None` when no value can be read (Go's `value == "" && rest2 == rest`).
fn consume_value(v: &str) -> Option<(Vec<u8>, &str)> {
    if !v.starts_with('"') {
        let (token, rest) = consume_token(v);
        return (!token.is_empty()).then(|| (token.as_bytes().to_vec(), rest));
    }
    let bytes = v.as_bytes();
    let mut value = Vec::new();
    let mut i = 1;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'"' {
            return Some((value, &v[i + 1..]));
        }
        if b == b'\\' && i + 1 < bytes.len() && is_tspecial(bytes[i + 1]) {
            value.push(bytes[i + 1]);
            i += 2;
            continue;
        }
        if b == b'\r' || b == b'\n' {
            return None;
        }
        value.push(b);
        i += 1;
    }
    None
}

/// Go `mime` `consumeMediaParam`: `; name=value` with the name lowercased, or `None`.
fn consume_media_param(v: &str) -> Option<(String, Vec<u8>, &str)> {
    let rest = v.trim_start().strip_prefix(';')?.trim_start();
    let (name, rest) = consume_token(rest);
    if name.is_empty() {
        return None;
    }
    let rest = rest.trim_start().strip_prefix('=')?.trim_start();
    let (value, rest) = consume_value(rest)?;
    Some((name.to_ascii_lowercase(), value, rest))
}

/// Go `mime` `isTSpecial`.
fn is_tspecial(b: u8) -> bool {
    b"()<>@,;:\\\"/[]?=".contains(&b)
}

/// Go `mime` `isTokenChar`.
fn is_mime_token_byte(b: u8) -> bool {
    b > 0x20 && b < 0x7f && !is_tspecial(b)
}

/// Go `mime` `isToken`: non-empty, every byte a token character.
fn is_mime_token(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(is_mime_token_byte)
}

/// Go `convertMessageToLLMResponse`: role `model`, the converted blocks (tool use dropped when
/// the turn hit the token limit), finish reason, usage, `model_version`, `turn_complete`, and
/// the custom metadata keys above.
pub(crate) fn convert_message_to_llm_response(
    msg: &Message,
    unwrap_output: bool,
) -> Result<LlmResponse, AnthropicError> {
    let truncated_by_token_limit =
        msg.stop_reason == "max_tokens" || msg.stop_reason == "model_context_window_exceeded";

    let mut parts = Vec::with_capacity(msg.content.len());
    for block in &msg.content {
        if truncated_by_token_limit && block.kind == "tool_use" {
            // Do not emit a FunctionCall part from a turn cut off by the token limit, as its
            // arguments may be incomplete or empty.
            continue;
        }
        if let Some(part) = convert_content_block(block, unwrap_output)? {
            parts.push(part);
        }
    }

    let (finish_reason, raw_stop_reason) = map_stop_reason(&msg.stop_reason);
    let (usage_metadata, cache_creation_tokens) = extract_usage_metadata(&msg.usage);

    let mut custom_metadata = Map::new();
    if !raw_stop_reason.is_empty() {
        custom_metadata.insert(
            STOP_REASON_METADATA_KEY.to_string(),
            Value::String(raw_stop_reason),
        );
    }
    if cache_creation_tokens > 0 {
        custom_metadata.insert(
            CACHE_CREATION_INPUT_TOKENS_METADATA_KEY.to_string(),
            Value::from(cache_creation_tokens),
        );
    }

    Ok(LlmResponse {
        content: Some(Content {
            parts,
            role: ROLE_MODEL.to_string(),
        }),
        usage_metadata: Some(usage_metadata),
        finish_reason: Some(finish_reason),
        model_version: msg.model.clone(),
        partial: false,
        turn_complete: true,
        custom_metadata,
        ..LlmResponse::default()
    })
}

/// Go `convertContentBlock`: `None` for block types Go skips; Go's
/// `anthropicmodel: failed to decode tool_use input for "<name>"` (no sentinel) when the input
/// is not a JSON object.
pub(crate) fn convert_content_block(
    block: &ContentBlock,
    unwrap_output: bool,
) -> Result<Option<Part>, AnthropicError> {
    match block.kind.as_str() {
        "thinking" => {
            let mut part = Part {
                thought: true,
                text: block.thinking.clone(),
                ..Part::default()
            };
            if !block.signature.is_empty() {
                part.thought_signature = Bytes::from(block.signature.clone().into_bytes());
            }
            Ok(Some(part))
        }
        "redacted_thinking" => Ok(Some(Part {
            thought: true,
            thought_signature: Bytes::from(block.data.clone().into_bytes()),
            ..Part::default()
        })),
        "text" => {
            let text = if unwrap_output {
                unwrap_output_schema_text(&block.text)
            } else {
                block.text.clone()
            };
            Ok(Some(Part::from_text(text)))
        }
        "tool_use" => {
            let mut args = Map::new();
            let trimmed = block.input.as_deref().unwrap_or_default().trim();
            if !trimmed.is_empty() && trimmed != "null" {
                args = serde_json::from_str::<Map<String, Value>>(trimmed).map_err(|_| {
                    AnthropicError::plain(format!(
                        "anthropicmodel: failed to decode tool_use input for {}",
                        go_quote(&block.name)
                    ))
                })?;
            }
            Ok(Some(Part {
                function_call: Some(FunctionCall {
                    id: block.id.clone(),
                    name: block.name.clone(),
                    args,
                    ..FunctionCall::default()
                }),
                ..Part::default()
            }))
        }
        _ => Ok(None),
    }
}

/// Go `unwrapOutputSchemaText`: the `result` member's JSON text when `text` is an object with
/// exactly that member, else `text`.
pub(crate) fn unwrap_output_schema_text(text: &str) -> String {
    let Ok(wrapper) = serde_json::from_str::<HashMap<String, Box<RawValue>>>(text) else {
        return text.to_string();
    };
    match wrapper.get(WRAPPED_OUTPUT_SCHEMA_KEY) {
        Some(raw) if wrapper.len() == 1 => raw.get().trim().to_string(),
        _ => text.to_string(),
    }
}

/// Go `mapStopReason`: the finish reason and the raw stop reason to record (`""` when it maps
/// one to one).
pub(crate) fn map_stop_reason(stop_reason: &str) -> (FinishReason, String) {
    match stop_reason {
        "" => (FinishReason::Unspecified, String::new()),
        "end_turn" | "stop_sequence" | "tool_use" => (FinishReason::Stop, String::new()),
        "max_tokens" => (FinishReason::MaxTokens, String::new()),
        "model_context_window_exceeded" => (FinishReason::MaxTokens, stop_reason.to_string()),
        "refusal" => (FinishReason::Safety, stop_reason.to_string()),
        "pause_turn" => (FinishReason::Stop, stop_reason.to_string()),
        _ => (FinishReason::Other, stop_reason.to_string()),
    }
}

/// Go `extractUsageMetadata`: the usage metadata and the cache creation token count.
///
/// Counts are clamped at 0 and summed with wrapping, as Go's `int64` sums; thinking tokens (when the response reports them) are capped at the
/// output tokens and excluded from the candidates count. The `int32` conversions wrap as Go's
/// do.
pub(crate) fn extract_usage_metadata(usage: &Usage) -> (GenerateContentResponseUsageMetadata, i64) {
    let input_tokens = usage.input_tokens.max(0);
    let cache_read = usage.cache_read_input_tokens.max(0);
    let cache_create = usage.cache_creation_input_tokens.max(0);
    let output_tokens = usage.output_tokens.max(0);

    // Go's int64 sums wrap; so do these (no panic on absurd server counts).
    let prompt_total = input_tokens
        .wrapping_add(cache_read)
        .wrapping_add(cache_create);

    let thinking_tokens = usage
        .output_tokens_details
        .as_ref()
        .and_then(|d| d.thinking_tokens)
        .map_or(0, |t| t.max(0).min(output_tokens));

    let candidates_tokens = output_tokens - thinking_tokens;

    (
        GenerateContentResponseUsageMetadata {
            prompt_token_count: prompt_total as i32,
            candidates_token_count: candidates_tokens as i32,
            total_token_count: prompt_total.wrapping_add(output_tokens) as i32,
            cached_content_token_count: cache_read as i32,
            thoughts_token_count: thinking_tokens as i32,
            ..GenerateContentResponseUsageMetadata::default()
        },
        cache_create,
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::wire::OutputTokensDetails;
    use bytes::Bytes;
    use gadk_genai::Content;
    use pretty_assertions::assert_eq;
    use serde_json::{Map, Value, json};

    fn msg(json: &str) -> Message {
        serde_json::from_str(json).unwrap()
    }

    fn texts(resp: &LlmResponse) -> Vec<String> {
        resp.content
            .as_ref()
            .unwrap()
            .parts
            .iter()
            .map(|p| p.text.clone())
            .collect()
    }

    /// Go `TestFinishReasonAndErrorClassification/stop_reason_mapping_without_error_code`'s
    /// table, through `convert_message_to_llm_response` over the body Go's server sends.
    #[test]
    fn stop_reason_mapping_without_error_code() {
        let cases = [
            ("end_turn", FinishReason::Stop, ""),
            ("stop_sequence", FinishReason::Stop, ""),
            ("tool_use", FinishReason::Stop, ""),
            ("max_tokens", FinishReason::MaxTokens, ""),
            (
                "model_context_window_exceeded",
                FinishReason::MaxTokens,
                "model_context_window_exceeded",
            ),
            ("refusal", FinishReason::Safety, "refusal"),
            ("pause_turn", FinishReason::Stop, "pause_turn"),
            (
                "future_unknown_reason",
                FinishReason::Other,
                "future_unknown_reason",
            ),
        ];
        for (stop_reason, want_finish, want_meta) in cases {
            let body = format!(
                r#"{{"id":"msg_1","type":"message","role":"assistant","model":"claude-sonnet-4-5-20250929","stop_reason":"{stop_reason}","content":[{{"type":"text","text":"content present"}}],"usage":{{"input_tokens":5,"output_tokens":2}}}}"#
            );
            let got = convert_message_to_llm_response(&msg(&body), false).unwrap();
            assert_eq!(got.finish_reason, Some(want_finish), "{stop_reason}");
            assert_eq!(got.error_code, "", "{stop_reason}");
            assert_eq!(got.error_message, "", "{stop_reason}");
            let got_meta = got
                .custom_metadata
                .get(STOP_REASON_METADATA_KEY)
                .and_then(Value::as_str)
                .unwrap_or_default();
            assert_eq!(got_meta, want_meta, "{stop_reason}");
            assert_eq!(texts(&got), vec!["content present".to_string()]);
        }
    }

    #[test]
    fn map_stop_reason_empty_is_unspecified() {
        assert_eq!(
            map_stop_reason(""),
            (FinishReason::Unspecified, String::new())
        );
    }

    /// The non-streaming exchange of `TestGenerate_Anthropic.httprr`.
    #[test]
    fn recorded_non_streaming_response() {
        let body = br#"{"id":"msg_anth_01","type":"message","role":"assistant","model":"claude-sonnet-4-5-20250929","stop_reason":"end_turn","content":[{"type":"text","text":"Paris"}],"usage":{"input_tokens":18,"output_tokens":2}}"#;
        let m = decode_message("application/json", body).unwrap();
        let got = convert_message_to_llm_response(&m, false).unwrap();
        let want = LlmResponse {
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
        assert_eq!(got, want);
    }

    /// Go `TestTruncatedStreamedToolCall/non_streaming_max_tokens_omits_incomplete_tool_use_and_keeps_text`.
    #[test]
    fn max_tokens_omits_tool_use_and_keeps_text() {
        let m = msg(
            r#"{"id":"msg_trunc","type":"message","role":"assistant","model":"claude-sonnet-4-5-20250929","stop_reason":"max_tokens","content":[{"type":"text","text":"Let me check the weather."},{"type":"tool_use","id":"toolu_trunc","name":"get_weather","input":{}}],"usage":{"input_tokens":10,"output_tokens":20}}"#,
        );
        let got = convert_message_to_llm_response(&m, false).unwrap();
        assert_eq!(got.finish_reason, Some(FinishReason::MaxTokens));
        assert_eq!(texts(&got), vec!["Let me check the weather.".to_string()]);
        assert!(got.custom_metadata.is_empty());
    }

    /// Go `TestTruncatedStreamedToolCall/model_context_window_exceeded_…`, non-streaming half.
    #[test]
    fn context_window_exceeded_omits_tool_use() {
        let m = msg(
            r#"{"id":"msg_ctx","type":"message","role":"assistant","model":"claude-sonnet-4-5-20250929","stop_reason":"model_context_window_exceeded","content":[{"type":"text","text":"Checking weather."},{"type":"tool_use","id":"toolu_ok","name":"get_weather","input":{"city":"Warsaw"}}],"usage":{"input_tokens":10,"output_tokens":25}}"#,
        );
        let got = convert_message_to_llm_response(&m, false).unwrap();
        assert_eq!(got.finish_reason, Some(FinishReason::MaxTokens));
        assert_eq!(
            got.custom_metadata.get(STOP_REASON_METADATA_KEY),
            Some(&json!("model_context_window_exceeded"))
        );
        assert_eq!(texts(&got), vec!["Checking weather.".to_string()]);
    }

    #[test]
    fn tool_use_block_becomes_function_call() {
        let m = msg(
            r#"{"model":"m","stop_reason":"tool_use","content":[{"type":"tool_use","id":"toolu_1","name":"get_weather","input":{"city":"Warsaw","n":1}}]}"#,
        );
        let got = convert_message_to_llm_response(&m, false).unwrap();
        let fc = got.content.unwrap().parts[0].function_call.clone().unwrap();
        assert_eq!(fc.id, "toolu_1");
        assert_eq!(fc.name, "get_weather");
        assert_eq!(Value::Object(fc.args), json!({"city": "Warsaw", "n": 1}));
    }

    #[test]
    fn tool_use_input_absent_null_or_blank_is_empty_args() {
        for input in [None, Some("null"), Some("  "), Some(" null "), Some("{}")] {
            let block = ContentBlock {
                kind: "tool_use".to_string(),
                id: "t".to_string(),
                name: "f".to_string(),
                input: input.map(str::to_string),
                ..ContentBlock::default()
            };
            let part = convert_content_block(&block, false).unwrap().unwrap();
            assert!(part.function_call.unwrap().args.is_empty(), "{input:?}");
        }
    }

    #[test]
    fn tool_use_invalid_input_is_go_error() {
        for input in [r#"{"city": "Warsa"#, "[1]", r#""x""#, "5"] {
            let block = ContentBlock {
                kind: "tool_use".to_string(),
                name: "get_weather".to_string(),
                input: Some(input.to_string()),
                ..ContentBlock::default()
            };
            let err = convert_content_block(&block, false).unwrap_err();
            assert_eq!(
                err.to_string(),
                r#"anthropicmodel: failed to decode tool_use input for "get_weather""#,
                "{input}"
            );
            assert!(err.kinds().is_empty());
            assert!(err.api_error().is_none());
        }
    }

    #[test]
    fn thinking_blocks() {
        let thinking = ContentBlock {
            kind: "thinking".to_string(),
            thinking: "let me think".to_string(),
            signature: "sig".to_string(),
            ..ContentBlock::default()
        };
        assert_eq!(
            convert_content_block(&thinking, false).unwrap(),
            Some(Part {
                thought: true,
                text: "let me think".to_string(),
                thought_signature: Bytes::from_static(b"sig"),
                ..Part::default()
            })
        );
        let unsigned = ContentBlock {
            kind: "thinking".to_string(),
            thinking: "t".to_string(),
            ..ContentBlock::default()
        };
        assert!(
            convert_content_block(&unsigned, false)
                .unwrap()
                .unwrap()
                .thought_signature
                .is_empty()
        );
        let redacted = ContentBlock {
            kind: "redacted_thinking".to_string(),
            data: "opaque".to_string(),
            ..ContentBlock::default()
        };
        assert_eq!(
            convert_content_block(&redacted, false).unwrap(),
            Some(Part {
                thought: true,
                thought_signature: Bytes::from_static(b"opaque"),
                ..Part::default()
            })
        );
    }

    #[test]
    fn unknown_block_types_are_skipped() {
        for kind in ["server_tool_use", "web_search_tool_result", ""] {
            let block = ContentBlock {
                kind: kind.to_string(),
                text: "x".to_string(),
                ..ContentBlock::default()
            };
            assert_eq!(
                convert_content_block(&block, false).unwrap(),
                None,
                "{kind}"
            );
        }
    }

    /// Go `TestOutputSchemaAndStructuredOutput/non_object_root_schema_wrapped_and_unwrapped`
    /// (non-streaming body), and the text it must leave alone.
    #[test]
    fn unwrap_output_schema() {
        assert_eq!(
            unwrap_output_schema_text(r#"{"result":["Warsaw","Krakow"]}"#),
            r#"["Warsaw","Krakow"]"#
        );
        assert_eq!(
            unwrap_output_schema_text("{ \"result\" :  [ 1, 2 ]  }"),
            "[ 1, 2 ]"
        );
        assert_eq!(unwrap_output_schema_text(r#"{"result":"x"}"#), r#""x""#);
        for keep in [
            r#"{"result":1,"other":2}"#,
            r#"{"city":"Warsaw"}"#,
            "not json",
            "null",
            "[1]",
            "",
        ] {
            assert_eq!(unwrap_output_schema_text(keep), keep);
        }

        let m = msg(
            r#"{"id":"msg_arr","type":"message","role":"assistant","model":"claude-sonnet-4-5-20250929","stop_reason":"end_turn","content":[{"type":"text","text":"{\"result\":[\"Warsaw\",\"Krakow\"]}"}],"usage":{"input_tokens":6,"output_tokens":8}}"#,
        );
        let got = convert_message_to_llm_response(&m, true).unwrap();
        assert_eq!(texts(&got), vec![r#"["Warsaw","Krakow"]"#.to_string()]);
        let kept = convert_message_to_llm_response(&m, false).unwrap();
        assert_eq!(
            texts(&kept),
            vec![r#"{"result":["Warsaw","Krakow"]}"#.to_string()]
        );
    }

    #[test]
    fn usage_with_cache_and_thinking() {
        let usage = Usage {
            input_tokens: 10,
            cache_read_input_tokens: 20,
            cache_creation_input_tokens: 30,
            output_tokens: 50,
            output_tokens_details: Some(OutputTokensDetails {
                thinking_tokens: Some(15),
            }),
        };
        let (meta, cache_create) = extract_usage_metadata(&usage);
        assert_eq!(cache_create, 30);
        assert_eq!(
            meta,
            GenerateContentResponseUsageMetadata {
                prompt_token_count: 60,
                candidates_token_count: 35,
                total_token_count: 110,
                cached_content_token_count: 20,
                thoughts_token_count: 15,
                ..GenerateContentResponseUsageMetadata::default()
            }
        );
    }

    #[test]
    fn usage_clamps_negative_and_excess_thinking() {
        let usage = Usage {
            input_tokens: -5,
            cache_read_input_tokens: -1,
            cache_creation_input_tokens: -1,
            output_tokens: 4,
            output_tokens_details: Some(OutputTokensDetails {
                thinking_tokens: Some(9),
            }),
        };
        let (meta, cache_create) = extract_usage_metadata(&usage);
        assert_eq!(cache_create, 0);
        assert_eq!(meta.prompt_token_count, 0);
        assert_eq!(meta.thoughts_token_count, 4);
        assert_eq!(meta.candidates_token_count, 0);
        assert_eq!(meta.total_token_count, 4);

        let negative_thinking = Usage {
            output_tokens: 4,
            output_tokens_details: Some(OutputTokensDetails {
                thinking_tokens: Some(-3),
            }),
            ..Usage::default()
        };
        assert_eq!(
            extract_usage_metadata(&negative_thinking)
                .0
                .thoughts_token_count,
            0
        );

        let no_thinking = Usage {
            output_tokens: 4,
            output_tokens_details: Some(OutputTokensDetails {
                thinking_tokens: None,
            }),
            ..Usage::default()
        };
        assert_eq!(
            extract_usage_metadata(&no_thinking)
                .0
                .candidates_token_count,
            4
        );
    }

    /// Go's `int64` sums wrap (no panic on absurd server counts), then `int32()` truncates.
    #[test]
    fn usage_sums_wrap_like_go() {
        let usage = Usage {
            input_tokens: i64::MAX,
            cache_read_input_tokens: 2,
            cache_creation_input_tokens: 3,
            output_tokens: 10,
            output_tokens_details: None,
        };
        let (meta, cache_create) = extract_usage_metadata(&usage);
        assert_eq!(cache_create, 3);
        // i64::MAX + 5 wraps to i64::MIN + 4, whose low 32 bits are 4.
        assert_eq!(meta.prompt_token_count, 4);
        assert_eq!(meta.total_token_count, 14);
        assert_eq!(meta.cached_content_token_count, 2);
        assert_eq!(meta.candidates_token_count, 10);

        let output_overflow = Usage {
            input_tokens: 1,
            output_tokens: i64::MAX,
            ..Usage::default()
        };
        let meta = extract_usage_metadata(&output_overflow).0;
        assert_eq!(meta.prompt_token_count, 1);
        assert_eq!(meta.total_token_count, 0);
        assert_eq!(meta.candidates_token_count, -1);
    }

    #[test]
    fn cache_creation_tokens_metadata() {
        let m = msg(
            r#"{"model":"m","stop_reason":"end_turn","content":[],"usage":{"input_tokens":1,"cache_creation_input_tokens":7,"output_tokens":1}}"#,
        );
        let got = convert_message_to_llm_response(&m, false).unwrap();
        let mut want = Map::new();
        want.insert(
            CACHE_CREATION_INPUT_TOKENS_METADATA_KEY.to_string(),
            json!(7),
        );
        assert_eq!(got.custom_metadata, want);
        assert_eq!(got.content.unwrap().parts, Vec::<Part>::new());
    }

    #[test]
    fn empty_message_is_unspecified_turn() {
        let got = convert_message_to_llm_response(&Message::default(), false).unwrap();
        assert_eq!(got.finish_reason, Some(FinishReason::Unspecified));
        assert!(got.turn_complete);
        assert!(!got.partial);
        assert!(got.custom_metadata.is_empty());
        assert_eq!(got.content.unwrap().role, "model");
    }

    #[test]
    fn decode_message_content_types() {
        let body = br#"{"model":"m","content":[{"type":"text","text":"hi"}]}"#;
        for ct in [
            "application/json",
            "application/json; charset=utf-8",
            "Application/JSON",
            "application/problem+json",
        ] {
            assert_eq!(decode_message(ct, body).unwrap().model, "m", "{ct}");
        }
        for ct in [
            "text/html",
            "",
            "text/plain; charset=utf-8",
            "application/json/x",
        ] {
            let err = decode_message(ct, body).unwrap_err();
            assert_eq!(
                err.to_string(),
                format!(
                    "anthropicmodel: request failed: expected destination type of 'string' or '[]byte' for responses with content-type '{ct}' that is not 'application/json'"
                ),
                "{ct}"
            );
        }
    }

    /// Go `mime.ParseMediaType` returns `""` for a repeated parameter name with a different
    /// value (names compare case-insensitively); equal repeats, a parse error before the repeat
    /// and a trailing `;` keep the media type.
    #[test]
    fn decode_message_duplicate_params_follow_go() {
        let body = br#"{"model":"m","content":[]}"#;
        for ct in [
            "application/json; charset=a; charset=a",
            "application/json; charset=\"a\"; charset=a",
            "application/json; charset=a; garbage; charset=b",
            "application/json; charset=a; charset*=b",
            "application/json;",
            "application/json; charset=utf-8;",
        ] {
            assert_eq!(decode_message(ct, body).unwrap().model, "m", "{ct}");
        }
        for ct in [
            "application/json; charset=a; charset=b",
            "application/json; Charset=a; CHARSET=b",
            "application/json;charset=a;x=1;charset=\"b\"",
        ] {
            let err = decode_message(ct, body).unwrap_err();
            assert_eq!(
                err.to_string(),
                format!(
                    "anthropicmodel: request failed: expected destination type of 'string' or '[]byte' for responses with content-type '{ct}' that is not 'application/json'"
                ),
                "{ct}"
            );
        }
    }

    #[test]
    fn decode_message_json_errors() {
        let err = decode_message("application/json", b"").unwrap_err();
        assert_eq!(
            err.to_string(),
            "anthropicmodel: request failed: error parsing response json: EOF"
        );
        let err = decode_message("application/json", b"{\"model\":").unwrap_err();
        assert!(
            err.to_string()
                .starts_with("anthropicmodel: request failed: error parsing response json: "),
            "{err}"
        );
        assert!(err.kinds().is_empty());
        // Go's json.Decoder reads only the first value.
        let m = decode_message("application/json", b"{\"model\":\"m\"} trailing").unwrap();
        assert_eq!(m.model, "m");
    }
}
