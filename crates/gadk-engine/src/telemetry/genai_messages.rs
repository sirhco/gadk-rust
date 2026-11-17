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

//! The opt-in message content of the GenAI semantic conventions (`gen_ai.input.messages`,
//! `gen_ai.output.messages`, `gen_ai.system_instructions`), recorded as a JSON string in the legacy
//! schema and as a structured value otherwise, and the schema's finish reasons.

use std::borrow::Cow;

use gadk_genai::{Blob, Content, FinishReason, Part};
use gadk_model::{LlmRequest, LlmResponse};
use serde::Serialize;
use serde_json::{Map, Value};

use super::attrs::GenerateContentResult;
use super::converters::{AttrValue, to_log_value};
use super::env::use_legacy_schema;

/// Attribute carrying the input messages.
pub(crate) const GEN_AI_INPUT_MESSAGES: &str = "gen_ai.input.messages";
/// Attribute carrying the output messages.
pub(crate) const GEN_AI_OUTPUT_MESSAGES: &str = "gen_ai.output.messages";
/// Attribute carrying the system instructions.
pub(crate) const GEN_AI_SYSTEM_INSTRUCTIONS: &str = "gen_ai.system_instructions";

const PART_TYPE_TEXT: &str = "text";
const PART_TYPE_REASONING: &str = "reasoning";
const PART_TYPE_TOOL_CALL: &str = "tool_call";
const PART_TYPE_TOOL_CALL_RESPONSE: &str = "tool_call_response";
const PART_TYPE_BLOB: &str = "blob";
const PART_TYPE_URI: &str = "uri";

const ROLE_USER: &str = "user";
const ROLE_ASSISTANT: &str = "assistant";
const ROLE_TOOL: &str = "tool";

/// Finish reason: the model stopped normally.
pub(crate) const FINISH_STOP: &str = "stop";
/// Finish reason: the output hit the token limit.
pub(crate) const FINISH_LENGTH: &str = "length";
/// Finish reason: the output was filtered.
pub(crate) const FINISH_CONTENT_FILTER: &str = "content_filter";
/// Finish reason: the model stopped to call a tool.
pub(crate) const FINISH_TOOL_CALL: &str = "tool_call";
/// Finish reason: the call failed.
pub(crate) const FINISH_ERROR: &str = "error";

/// The most bytes of JSON one content attribute may hold: stays under Cloud Trace's 64 KiB
/// attribute limit.
pub(crate) const MAX_CONTENT_ATTRIBUTE_BYTES: usize = 60 << 10;

/// The most bytes of one inline payload recorded (before base64).
pub(crate) const MAX_INLINE_DATA_BYTES: usize = 16 << 10;

/// A content attribute's value: a message list or a part list.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub(crate) enum ContentValue {
    /// A list of chat messages.
    Messages(Vec<ChatMessage>),
    /// A list of parts.
    Parts(Vec<PartValue>),
    /// An encoded tool payload ([`tool_payload`]).
    Raw(Value),
}

impl ContentValue {
    /// The value as an [`AttrValue`].
    pub(crate) fn value(&self) -> AttrValue {
        match self {
            Self::Messages(ms) => AttrValue::Slice(ms.iter().map(ChatMessage::value).collect()),
            Self::Parts(ps) => parts_value(ps),
            Self::Raw(v) => to_log_value(v),
        }
    }
}

/// A content attribute: its key and value.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ContentAttr {
    /// The attribute key.
    pub(crate) key: &'static str,
    /// The content.
    pub(crate) value: ContentValue,
}

/// One chat message (a turn); `finish_reason` only on an output message.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ChatMessage {
    /// The message role.
    pub(crate) role: String,
    /// The message parts.
    pub(crate) parts: Vec<PartValue>,
    /// The finish reason; omitted when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) finish_reason: Option<String>,
}

impl ChatMessage {
    fn value(&self) -> AttrValue {
        let mut kvs = vec![
            str_kv("role", &self.role),
            ("parts".to_owned(), parts_value(&self.parts)),
        ];
        if let Some(fr) = &self.finish_reason {
            kvs.push(str_kv("finish_reason", fr));
        }
        AttrValue::Map(kvs)
    }
}

fn parts_value(ps: &[PartValue]) -> AttrValue {
    AttrValue::Slice(ps.iter().map(PartValue::value).collect())
}

/// One converted part: text, tool call, blob, URI or tool response. Fields serialize in declaration
/// order, and empty optional fields are omitted.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub(crate) enum PartValue {
    /// A text part (`text` or `reasoning`).
    Text {
        /// The part type.
        #[serde(rename = "type")]
        kind: &'static str,
        /// The text.
        content: String,
    },
    /// A tool call.
    ToolCall {
        /// The part type.
        #[serde(rename = "type")]
        kind: &'static str,
        /// The call ID.
        #[serde(skip_serializing_if = "String::is_empty")]
        id: String,
        /// The tool name.
        name: String,
        /// The call arguments ([`tool_payload`]).
        arguments: Value,
    },
    /// An inline blob.
    Blob {
        /// The part type.
        #[serde(rename = "type")]
        kind: &'static str,
        /// The MIME type.
        #[serde(skip_serializing_if = "String::is_empty")]
        mime_type: String,
        /// The modality (`image`, `video`, `audio` or `document`).
        modality: &'static str,
        /// The payload, base64-encoded.
        content: String,
        /// Whether the payload was cut to the inline-data limit.
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        truncated: bool,
    },
    /// A file referenced by URI.
    Uri {
        /// The part type.
        #[serde(rename = "type")]
        kind: &'static str,
        /// The MIME type.
        #[serde(skip_serializing_if = "String::is_empty")]
        mime_type: String,
        /// The modality (`image`, `video`, `audio` or `document`).
        modality: &'static str,
        /// The URI.
        uri: String,
    },
    /// A tool response.
    ToolResponse {
        /// The part type.
        #[serde(rename = "type")]
        kind: &'static str,
        /// The call ID.
        #[serde(skip_serializing_if = "String::is_empty")]
        id: String,
        /// The tool name.
        #[serde(skip_serializing_if = "String::is_empty")]
        name: String,
        /// The tool response ([`tool_payload`]).
        response: Value,
    },
}

impl PartValue {
    /// The part as a map [`AttrValue`], with the same fields and omissions as its serialization.
    fn value(&self) -> AttrValue {
        let mut kvs = Vec::new();
        match self {
            Self::Text { kind, content } => {
                kvs.push(str_kv("type", kind));
                kvs.push(str_kv("content", content));
            }
            Self::ToolCall {
                kind,
                id,
                name,
                arguments,
            } => {
                kvs.push(str_kv("type", kind));
                if !id.is_empty() {
                    kvs.push(str_kv("id", id));
                }
                kvs.push(str_kv("name", name));
                kvs.push(("arguments".to_owned(), to_log_value(arguments)));
            }
            Self::Blob {
                kind,
                mime_type,
                modality,
                content,
                truncated,
            } => {
                kvs.push(str_kv("type", kind));
                if !mime_type.is_empty() {
                    kvs.push(str_kv("mime_type", mime_type));
                }
                kvs.push(str_kv("modality", modality));
                kvs.push(str_kv("content", content));
                if *truncated {
                    kvs.push(("truncated".to_owned(), AttrValue::Bool(true)));
                }
            }
            Self::Uri {
                kind,
                mime_type,
                modality,
                uri,
            } => {
                kvs.push(str_kv("type", kind));
                if !mime_type.is_empty() {
                    kvs.push(str_kv("mime_type", mime_type));
                }
                kvs.push(str_kv("modality", modality));
                kvs.push(str_kv("uri", uri));
            }
            Self::ToolResponse {
                kind,
                id,
                name,
                response,
            } => {
                kvs.push(str_kv("type", kind));
                if !id.is_empty() {
                    kvs.push(str_kv("id", id));
                }
                if !name.is_empty() {
                    kvs.push(str_kv("name", name));
                }
                kvs.push(("response".to_owned(), to_log_value(response)));
            }
        }
        AttrValue::Map(kvs)
    }
}

/// A string key-value pair. A Rust string is always valid UTF-8, so nothing needs replacing.
fn str_kv(k: &str, v: &str) -> (String, AttrValue) {
    (k.to_owned(), AttrValue::string(v))
}

/// Request content: the system instruction and the conversation, in the order sent. User content,
/// so callers gate it on content capture.
pub(crate) fn request_content(req: &LlmRequest) -> Vec<ContentAttr> {
    let mut content = Vec::new();
    if let Some(si) = req
        .config
        .as_ref()
        .and_then(|c| c.system_instruction.as_ref())
    {
        let parts = semconv_parts(&si.parts);
        if !parts.is_empty() {
            content.push(ContentAttr {
                key: GEN_AI_SYSTEM_INSTRUCTIONS,
                value: ContentValue::Parts(parts),
            });
        }
    }
    if !req.contents.is_empty() {
        let msgs = req
            .contents
            .iter()
            .map(|c| ChatMessage {
                role: schema_role(Some(c)).to_owned(),
                parts: semconv_parts(&c.parts),
                finish_reason: None,
            })
            .collect();
        content.push(ContentAttr {
            key: GEN_AI_INPUT_MESSAGES,
            value: ContentValue::Messages(msgs),
        });
    }
    content
}

/// Response content: the legacy schema records the final response (none for a partial one);
/// otherwise every response of the call is one output message.
pub(crate) fn response_content(r: &GenerateContentResult) -> Vec<ContentAttr> {
    if use_legacy_schema() {
        let Some(resp) = r.response.as_deref().filter(|resp| !resp.partial) else {
            return Vec::new();
        };
        let fr = schema_finish_reason(Some(resp), r.error.is_some());
        return vec![ContentAttr {
            key: GEN_AI_OUTPUT_MESSAGES,
            value: ContentValue::Messages(vec![output_message(resp, &fr)]),
        }];
    }
    let msgs: Vec<ChatMessage> = r
        .responses()
        .iter()
        .map(|resp| output_message(resp, &reported_finish_reason(Some(resp), false)))
        .collect();
    if msgs.is_empty() {
        return Vec::new();
    }
    vec![ContentAttr {
        key: GEN_AI_OUTPUT_MESSAGES,
        value: ContentValue::Messages(msgs),
    }]
}

/// One output message for `resp`: a suppressed candidate keeps an empty parts list.
fn output_message(resp: &LlmResponse, finish_reason: &str) -> ChatMessage {
    ChatMessage {
        role: ROLE_ASSISTANT.to_owned(),
        parts: resp
            .content
            .as_ref()
            .map(|c| semconv_parts(&c.parts))
            .unwrap_or_default(),
        finish_reason: Some(finish_reason.to_owned()),
    }
}

/// Span content attributes: the legacy schema records each value's JSON text, the default one the
/// structured value; a value whose JSON is over [`MAX_CONTENT_ATTRIBUTE_BYTES`] is left off and
/// reported.
pub(crate) fn span_content_attributes(content: &[ContentAttr]) -> Vec<(&'static str, AttrValue)> {
    let legacy = use_legacy_schema();
    let mut attrs = Vec::new();
    for c in content {
        let Ok(encoded) = gadk_model::to_go_json(&c.value) else {
            // Unreachable.
            continue;
        };
        if encoded.len() > MAX_CONTENT_ATTRIBUTE_BYTES {
            tracing::warn!(
                target: "gadk_engine::telemetry",
                "adk: telemetry: leaving {} off the span: its JSON encoding is {} bytes, over the {}-byte limit",
                c.key,
                encoded.len(),
                MAX_CONTENT_ATTRIBUTE_BYTES
            );
            continue;
        }
        if legacy {
            attrs.push((
                c.key,
                AttrValue::String(String::from_utf8_lossy(&encoded).into_owned()),
            ));
        } else {
            attrs.push((c.key, c.value.value()));
        }
    }
    attrs
}

/// Log-record content attributes: always the structured value.
pub(crate) fn event_content_attributes(content: &[ContentAttr]) -> Vec<(&'static str, AttrValue)> {
    content.iter().map(|c| (c.key, c.value.value())).collect()
}

/// Converts parts; parts with no mapping are skipped.
pub(crate) fn semconv_parts(ps: &[Part]) -> Vec<PartValue> {
    ps.iter().filter_map(semconv_part).collect()
}

/// Converts one part: structured variants are tried before text; code execution and provider-side
/// tool parts are dropped.
pub(crate) fn semconv_part(p: &Part) -> Option<PartValue> {
    if let Some(fc) = &p.function_call {
        return Some(PartValue::ToolCall {
            kind: PART_TYPE_TOOL_CALL,
            id: fc.id.clone(),
            name: fc.name.clone(),
            arguments: tool_payload(&fc.args),
        });
    }
    if let Some(fr) = &p.function_response {
        return Some(PartValue::ToolResponse {
            kind: PART_TYPE_TOOL_CALL_RESPONSE,
            id: fr.id.clone(),
            name: fr.name.clone(),
            response: tool_payload(&fr.response),
        });
    }
    if let Some(blob) = &p.inline_data {
        let (content, truncated) = encode_inline_data(blob);
        return Some(PartValue::Blob {
            kind: PART_TYPE_BLOB,
            mime_type: blob.mime_type.clone(),
            modality: modality_of(&blob.mime_type),
            content,
            truncated,
        });
    }
    if let Some(fd) = &p.file_data {
        return Some(PartValue::Uri {
            kind: PART_TYPE_URI,
            mime_type: fd.mime_type.clone(),
            modality: modality_of(&fd.mime_type),
            uri: fd.file_uri.clone(),
        });
    }
    if !p.text.is_empty() {
        let kind = if p.thought {
            PART_TYPE_REASONING
        } else {
            PART_TYPE_TEXT
        };
        return Some(PartValue::Text {
            kind,
            content: p.text.clone(),
        });
    }
    None
}

/// An application tool value as JSON; an empty map is `null`, as elsewhere in gadk. A `serde_json`
/// value always encodes, so no unserializable placeholder is needed.
pub(crate) fn tool_payload(v: &Map<String, Value>) -> Value {
    if v.is_empty() {
        return Value::Null;
    }
    Value::Object(v.clone())
}

/// Encodes standard base64 of at most [`MAX_INLINE_DATA_BYTES`] bytes of `blob.data`, and whether
/// the payload was cut (gadk-genai's `Blob` serde does the encoding).
pub(crate) fn encode_inline_data(blob: &Blob) -> (String, bool) {
    let truncated = blob.data.len() > MAX_INLINE_DATA_BYTES;
    let data = if truncated {
        blob.data.slice(..MAX_INLINE_DATA_BYTES)
    } else {
        blob.data.clone()
    };
    let probe = Blob {
        data,
        ..Default::default()
    };
    let encoded = serde_json::to_value(&probe)
        .ok()
        .and_then(|v| v.get("data").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default();
    (encoded, truncated)
}

/// Maps an IANA MIME type onto the schema's modality enum.
pub(crate) fn modality_of(mime_type: &str) -> &'static str {
    let t = gadk_platform::go_to_lower(mime_type);
    if t.starts_with("image/") {
        "image"
    } else if t.starts_with("video/") {
        "video"
    } else if t.starts_with("audio/") {
        "audio"
    } else {
        "document"
    }
}

/// Whether the content holds a function call.
pub(crate) fn has_function_call(c: Option<&Content>) -> bool {
    c.is_some_and(|c| c.parts.iter().any(|p| p.function_call.is_some()))
}

/// The schema role of a turn: a turn carrying a tool result is `tool`; `user`/`model` map to
/// `user`/`assistant`; any other role passes through.
pub(crate) fn schema_role(c: Option<&Content>) -> &str {
    let Some(c) = c else { return ROLE_USER };
    if c.parts
        .iter()
        .any(|p| p.function_response.is_some() || p.tool_response.is_some())
    {
        return ROLE_TOOL;
    }
    match c.role.as_str() {
        "" | gadk_genai::ROLE_USER => ROLE_USER,
        gadk_genai::ROLE_MODEL => ROLE_ASSISTANT,
        other => other,
    }
}

/// The finish reason to report: [`schema_finish_reason`], or `""` when `resp` reports no reason
/// (`FINISH_REASON_UNSPECIFIED` counts as none). `err` is whether the call failed.
pub(crate) fn reported_finish_reason(resp: Option<&LlmResponse>, err: bool) -> Cow<'static, str> {
    if !err
        && let Some(r) = resp
        && r.error_code.is_empty()
        && !r.interrupted
        && matches!(r.finish_reason, None | Some(FinishReason::Unspecified))
    {
        return Cow::Borrowed("");
    }
    schema_finish_reason(resp, err)
}

/// The schema's finish reason: a failed call, an error code or an interruption is `error`; a stop
/// carrying a tool call is `tool_call`; a value this mapping does not know is lowercased.
pub(crate) fn schema_finish_reason(resp: Option<&LlmResponse>, err: bool) -> Cow<'static, str> {
    let Some(resp) = resp else {
        return Cow::Borrowed(FINISH_ERROR);
    };
    if err || !resp.error_code.is_empty() || resp.interrupted {
        return Cow::Borrowed(FINISH_ERROR);
    }
    use FinishReason as F;
    Cow::Borrowed(match &resp.finish_reason {
        None | Some(F::Unspecified | F::Stop) => {
            if has_function_call(resp.content.as_ref()) {
                FINISH_TOOL_CALL
            } else {
                FINISH_STOP
            }
        }
        Some(F::MaxTokens) => FINISH_LENGTH,
        Some(
            F::Safety
            | F::Recitation
            | F::Language
            | F::Blocklist
            | F::ProhibitedContent
            | F::Spii
            | F::ImageSafety
            | F::ImageProhibitedContent
            | F::ImageRecitation,
        ) => FINISH_CONTENT_FILTER,
        Some(
            F::Other
            | F::MalformedFunctionCall
            | F::UnexpectedToolCall
            | F::TooManyToolCalls
            | F::NoImage
            | F::ImageOther,
        ) => FINISH_ERROR,
        Some(other) => return Cow::Owned(gadk_platform::go_to_lower(other.as_str())),
    })
}
