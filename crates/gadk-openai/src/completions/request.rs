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

//! Chat Completions request building: [`build_params`] and its content, media, role, tool-call
//! and generation-config conversions. Only [`build_params`] is shared; [`build_params_with`]
//! and [`supported_config_fields`] are the seam for tests that swap the supported-field list.

use gadk_genai::{Content, FunctionCall, FunctionResponse, GenerateContentConfig, ServiceTier};
use gadk_model::{HttpOptions, LlmRequest};
use gadk_platform::go_quote;

use crate::completions::tools::{convert_tool_choice, convert_tools};
use crate::completions::wire::{
    ChatCompletionAssistantContent, ChatCompletionAssistantMessageParam,
    ChatCompletionContentPartFileFileParam, ChatCompletionContentPartImageImageUrlParam,
    ChatCompletionContentPartParam, ChatCompletionMessageFunctionToolCallFunctionParam,
    ChatCompletionMessageFunctionToolCallParam, ChatCompletionMessageParam,
    ChatCompletionMessageToolCallParam, ChatCompletionNewParams, ChatCompletionResponseFormat,
    ChatCompletionServiceTier, ChatCompletionStop, FunctionTag,
};
use crate::error::{ErrorKind, OpenAiError};
use crate::oaishared::{
    ResponseFormatJsonObjectParam, ResponseFormatJsonSchemaJsonSchemaParam,
    ResponseFormatJsonSchemaParam,
};
use crate::shared::calls::{CallTracker, marshal_function_args, marshal_function_response};
use crate::shared::config::{
    SUPPORTED_CONFIG_FIELDS, reasoning_effort_for, reject_unsupported_config_fields,
    reject_untranslatable_values,
};
use crate::shared::contents::{
    PartField, code_execution_text, flatten_content_text, replayed_reasoning, unsupported_payload,
};
use crate::shared::media::{Media, MediaKind, MediaSource, classify_media};
use crate::shared::schema::{
    enforce_strict_openai_schema, normalize_schema, response_format_name, schema_to_map,
};

/// The message roles Chat Completions accepts from a caller (user, assistant,
/// system, developer). The tool role is absent on purpose: it comes from a
/// function response, never from a content role.
const ROLE_USER: &str = "user";
const ROLE_ASSISTANT: &str = "assistant";
const ROLE_SYSTEM: &str = "system";
const ROLE_DEVELOPER: &str = "developer";

/// The detail level every image input carries, named rather than left
/// to the server's default.
const IMAGE_INPUT_DETAIL: &str = "auto";

/// The Chat Completions params for `req`, sent as `model_name` unless
/// `req.model` names a model. Order: contents ([`ErrorKind::NoContents`](crate::error::ErrorKind::NoContents)
/// when they convert to no message), then the generation config (`req.config` with
/// `req.http_options`), then tools, then the tool choice (sent only alongside tools). There is
/// no nil-request check: an `LlmRequest` is never nil.
pub(crate) fn build_params(
    model_name: &str,
    req: &LlmRequest,
) -> Result<ChatCompletionNewParams, OpenAiError> {
    build_params_with(model_name, req, &supported_config_fields())
}

/// The fields both endpoints support
/// ([`SUPPORTED_CONFIG_FIELDS`]) plus the ones only Chat Completions can express.
pub(crate) fn supported_config_fields() -> Vec<&'static str> {
    let mut fields = SUPPORTED_CONFIG_FIELDS.to_vec();
    fields.extend([
        "StopSequences",
        "PresencePenalty",
        "FrequencyPenalty",
        "Seed",
    ]);
    fields
}

/// [`build_params`] against `supported` in place of [`supported_config_fields`] (the list is
/// passed in, so tests can swap it).
pub(crate) fn build_params_with(
    model_name: &str,
    req: &LlmRequest,
    supported: &[&str],
) -> Result<ChatCompletionNewParams, OpenAiError> {
    let mut params = ChatCompletionNewParams {
        model: if req.model.is_empty() {
            model_name.to_string()
        } else {
            req.model.clone()
        },
        ..ChatCompletionNewParams::default()
    };

    let messages = convert_contents(&req.contents)?;
    if messages.is_empty() {
        return Err(OpenAiError::sentinel(ErrorKind::NoContents));
    }
    params.messages = messages;

    // Runs after the messages exist, because the system instruction joins them as the first
    // one rather than as a field of its own.
    apply_generation_config(
        &mut params,
        req.config.as_ref(),
        req.http_options.as_ref(),
        supported,
    )?;

    params.tools = convert_tools(req.config.as_ref())?;

    if let Some(tool_config) = req.config.as_ref().and_then(|cfg| cfg.tool_config.as_ref()) {
        let choice = convert_tool_choice(Some(tool_config))?;
        // Only alongside tools: the API rejects a tool_choice sent without them, "none"
        // included, so a turn whose toolset came up empty would fail.
        if choice.is_some() && !params.tools.is_empty() {
            params.tool_choice = choice;
        }
    }

    Ok(params)
}

/// The contents as a Chat Completions message list. One content can
/// produce several messages, because a tool result is a message of its own. A content without
/// parts is skipped.
fn convert_contents(contents: &[Content]) -> Result<Vec<ChatCompletionMessageParam>, OpenAiError> {
    let mut messages = Vec::new();
    let mut tracker = CallTracker::default();
    for content in contents {
        if content.parts.is_empty() {
            continue;
        }
        messages.extend(convert_content(content, &mut tracker)?);
    }
    Ok(messages)
}

/// One piece of a message in arrival order, either text or a classified
/// media value.
enum ContentPiece {
    Text(String),
    Media(Media),
}

/// A tool message per function response, then at most one message for
/// the rest.
fn convert_content(
    content: &Content,
    tracker: &mut CallTracker,
) -> Result<Vec<ChatCompletionMessageParam>, OpenAiError> {
    let role = normalize_role(&content.role)?;

    let mut messages = Vec::new();
    let mut texts: Vec<String> = Vec::new();
    let mut calls = Vec::new();
    // The turn's text and media in arrival order, because a user message that carries media
    // is a list of parts and "describe this:" has to stay ahead of the image it introduces.
    let mut pieces = Vec::new();
    let mut has_media = false;
    for part in &content.parts {
        // Checked first, so a field this API cannot send is named even when text or a call on
        // the same part would otherwise carry it out unnoticed.
        if let Some(field) =
            unsupported_payload(part, &[PartField::Media, PartField::CodeExecution])
        {
            return Err(OpenAiError::plain(format!(
                "openai: unsupported content part: {field}"
            )));
        }
        // Code execution has no wire field here. It goes out as the same fenced text the
        // Responses path sends, so a Gemini turn replayed into this conversation is not
        // dropped.
        let code_text = code_execution_text(part);
        // Classified before any of the part is emitted, so media that cannot be labeled fails
        // the request instead of leaving with the text beside it.
        let media = classify_media(part)?;
        if let Some(first) = media.first()
            && part.function_response.is_some()
        {
            // A tool message carries text only, so the media would be dropped without a word.
            return Err(OpenAiError::plain(format!(
                "openai: media on a function response part is not supported: {}",
                first.mime_type
            )));
        }
        // Text is read apart from a call or a response because one part can carry both.
        // Reasoning and signatures are dropped: Chat Completions has no field for either.
        let send_text = !part.text.is_empty() && !part.thought;
        if send_text {
            texts.push(part.text.clone());
            pieces.push(ContentPiece::Text(part.text.clone()));
        }
        if !code_text.is_empty() {
            texts.push(code_text.clone());
            pieces.push(ContentPiece::Text(code_text.clone()));
        }
        let carries_media = !media.is_empty();
        for m in media {
            pieces.push(ContentPiece::Media(m));
            has_media = true;
        }
        if let Some(fc) = &part.function_call {
            calls.push(new_tool_call(tracker, fc)?);
        } else if let Some(fr) = &part.function_response {
            messages.push(new_tool_message(tracker, fr)?);
        } else if !send_text && code_text.is_empty() && !carries_media && !replayed_reasoning(part)
        {
            return Err(OpenAiError::plain(
                "openai: unsupported content part: carries nothing to send",
            ));
        }
    }
    if !calls.is_empty() && role != ROLE_ASSISTANT {
        // Only an assistant message carries tool calls, and dropping them would leave their
        // results answering calls the request never made.
        return Err(OpenAiError::plain(format!(
            "openai: function call in a {role} turn: only model turns can carry calls"
        )));
    }
    if has_media {
        messages.push(new_media_message(role, pieces)?);
        return Ok(messages);
    }

    let text = join_text(&texts);
    if role == ROLE_ASSISTANT {
        if !text.is_empty() || !calls.is_empty() {
            messages.push(ChatCompletionMessageParam::Assistant(
                ChatCompletionAssistantMessageParam {
                    content: (!text.is_empty())
                        .then_some(ChatCompletionAssistantContent::String(text)),
                    tool_calls: calls,
                    ..ChatCompletionAssistantMessageParam::default()
                },
            ));
        }
    } else if text.is_empty() {
    } else if role == ROLE_SYSTEM {
        messages.push(ChatCompletionMessageParam::system(text));
    } else if role == ROLE_DEVELOPER {
        messages.push(ChatCompletionMessageParam::developer(text));
    } else {
        messages.push(ChatCompletionMessageParam::user(text));
    }
    Ok(messages)
}

/// The user message for a turn that carries media. Only a user message
/// can, so media in an assistant turn is
/// [`ErrorKind::MediaOnAssistantTurn`](crate::error::ErrorKind::MediaOnAssistantTurn) and in a
/// system or developer turn `openai: media in a <role> message is not supported: <mime>`.
/// Whitespace-only text pieces are dropped.
fn new_media_message(
    role: &str,
    pieces: Vec<ContentPiece>,
) -> Result<ChatCompletionMessageParam, OpenAiError> {
    let mut parts = Vec::with_capacity(pieces.len());
    for piece in pieces {
        let media = match piece {
            ContentPiece::Text(text) => {
                if !text.trim().is_empty() {
                    parts.push(ChatCompletionContentPartParam::text(text));
                }
                continue;
            }
            ContentPiece::Media(media) => media,
        };
        match role {
            ROLE_ASSISTANT => {
                return Err(OpenAiError::new(
                    ErrorKind::MediaOnAssistantTurn,
                    &media.mime_type,
                ));
            }
            ROLE_SYSTEM | ROLE_DEVELOPER => {
                return Err(OpenAiError::plain(format!(
                    "openai: media in a {role} message is not supported: {}",
                    media.mime_type
                )));
            }
            _ => {}
        }
        parts.push(new_media_part(&media)?);
    }
    Ok(ChatCompletionMessageParam::user_parts(parts))
}

/// The content part for one classified media value. The kind picks the
/// wire field and the source picks how that field names the bytes. Errors name the kind and
/// the source, never the URI (it can be a presigned URL).
fn new_media_part(m: &Media) -> Result<ChatCompletionContentPartParam, OpenAiError> {
    match m.kind {
        MediaKind::Image => {
            let url = match m.source {
                MediaSource::Data => m.data_url(),
                MediaSource::Url => m.url.clone(),
                // image_url has no field for an uploaded file's id.
                MediaSource::FileId => {
                    return Err(OpenAiError::plain(format!(
                        "openai: unsupported media source for an image on Chat Completions: {}",
                        m.source.as_str()
                    )));
                }
            };
            Ok(ChatCompletionContentPartParam::image(
                ChatCompletionContentPartImageImageUrlParam {
                    url,
                    detail: IMAGE_INPUT_DETAIL.to_string(),
                },
            ))
        }
        MediaKind::File => {
            let file = match m.source {
                MediaSource::Data => ChatCompletionContentPartFileFileParam {
                    file_data: Some(m.data_url()),
                    filename: (!m.filename.is_empty()).then(|| m.filename.clone()),
                    ..ChatCompletionContentPartFileFileParam::default()
                },
                MediaSource::FileId => ChatCompletionContentPartFileFileParam {
                    file_id: Some(m.file_id.clone()),
                    ..ChatCompletionContentPartFileFileParam::default()
                },
                // The file part has file_data and file_id and no location to fetch.
                MediaSource::Url => {
                    return Err(OpenAiError::plain(format!(
                        "openai: unsupported media source for a file on Chat Completions: {}",
                        m.source.as_str()
                    )));
                }
            };
            Ok(ChatCompletionContentPartParam::file(file))
        }
        // Audio and video are labeled by the classifier and refused here, as on the
        // Responses endpoint.
        MediaKind::Audio | MediaKind::Video => Err(OpenAiError::new(
            ErrorKind::UnsupportedMimeType,
            &m.mime_type,
        )),
    }
}

/// A genai content role as a Chat Completions message role (`""` and
/// `user` → user, `model` → assistant; system and developer survive); anything else is
/// `openai: unsupported role "<role>"`.
fn normalize_role(role: &str) -> Result<&'static str, OpenAiError> {
    match role {
        "" | "user" => Ok(ROLE_USER),
        "model" => Ok(ROLE_ASSISTANT),
        ROLE_SYSTEM => Ok(ROLE_SYSTEM),
        ROLE_DEVELOPER => Ok(ROLE_DEVELOPER),
        other => Err(OpenAiError::plain(format!(
            "openai: unsupported role {}",
            go_quote(other)
        ))),
    }
}

/// A turn's text parts joined with `\n`, whitespace-only parts dropped.
fn join_text(texts: &[String]) -> String {
    let mut out = String::new();
    for text in texts {
        if text.trim().is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(text);
    }
    out
}

/// A function call as the tool call an assistant message carries; an empty
/// name is [`ErrorKind::FunctionCallMissingName`](crate::error::ErrorKind::FunctionCallMissingName).
fn new_tool_call(
    tracker: &mut CallTracker,
    fc: &FunctionCall,
) -> Result<ChatCompletionMessageToolCallParam, OpenAiError> {
    if fc.name.is_empty() {
        return Err(OpenAiError::sentinel(ErrorKind::FunctionCallMissingName));
    }
    let id = tracker.take_call_id(fc);
    let arguments = marshal_function_args(&fc.args)?;
    Ok(ChatCompletionMessageToolCallParam::Function(
        ChatCompletionMessageFunctionToolCallParam {
            id,
            function: ChatCompletionMessageFunctionToolCallFunctionParam {
                arguments,
                name: fc.name.clone(),
            },
            kind: FunctionTag,
        },
    ))
}

/// A function response as the tool-role message answering its call.
fn new_tool_message(
    tracker: &mut CallTracker,
    fr: &FunctionResponse,
) -> Result<ChatCompletionMessageParam, OpenAiError> {
    let call_id = tracker.resolve_response_id(fr)?;
    let payload = marshal_function_response(&fr.response)?;
    Ok(ChatCompletionMessageParam::tool(payload, call_id))
}

/// genai's processing tiers as Chat Completions tiers (unspecified reads as
/// default, as on the Responses path); `None` for a tier the map does not know.
fn service_tier(tier: &ServiceTier) -> Option<ChatCompletionServiceTier> {
    match tier {
        ServiceTier::Unspecified | ServiceTier::Standard => {
            Some(ChatCompletionServiceTier::Default)
        }
        ServiceTier::Flex => Some(ChatCompletionServiceTier::Flex),
        ServiceTier::Priority => Some(ChatCompletionServiceTier::Priority),
        ServiceTier::Unknown(_) => None,
    }
}

/// The generation config onto Chat Completions params. Stop
/// sequences, the penalties and seed are translated here (the Responses path rejects them).
/// An absent config is `(None, None)`; `http_options` alone stands for a config carrying only
/// `HTTPOptions`.
fn apply_generation_config(
    params: &mut ChatCompletionNewParams,
    cfg: Option<&GenerateContentConfig>,
    http_options: Option<&HttpOptions>,
    supported: &[&str],
) -> Result<(), OpenAiError> {
    let default_cfg;
    let cfg = match (cfg, http_options) {
        (Some(cfg), _) => cfg,
        (None, None) => return Ok(()),
        (None, Some(_)) => {
            default_cfg = GenerateContentConfig::default();
            &default_cfg
        }
    };
    if let Some(temperature) = cfg.temperature {
        params.temperature = Some(f64::from(temperature));
    }
    if let Some(top_p) = cfg.top_p {
        params.top_p = Some(f64::from(top_p));
    }
    if cfg.top_k.is_some() {
        // Chat Completions has no top_k either, and forwarding it would trade a clear error
        // for a field the provider ignores.
        return Err(OpenAiError::sentinel(ErrorKind::TopKNotSupported));
    }
    if cfg.max_output_tokens > 0 {
        params.max_completion_tokens = Some(i64::from(cfg.max_output_tokens));
    }
    if !cfg.stop_sequences.is_empty() {
        // Sent whatever its length: OpenAI documents a limit of four, but compatible
        // providers differ, and a 400 naming the field diagnoses it better than a guess.
        params.stop = Some(ChatCompletionStop::StringArray(cfg.stop_sequences.clone()));
    }
    if cfg.candidate_count > 1 {
        // The API has "n", but a genai response carries one candidate.
        return Err(OpenAiError::sentinel(
            ErrorKind::MultipleCandidatesNotSupported,
        ));
    }
    if let Some(penalty) = cfg.frequency_penalty {
        params.frequency_penalty = Some(f64::from(penalty));
    }
    if let Some(penalty) = cfg.presence_penalty {
        params.presence_penalty = Some(f64::from(penalty));
    }
    if let Some(seed) = cfg.seed {
        params.seed = Some(i64::from(seed));
    }
    if cfg.response_logprobs {
        // top_logprobs is refused without this, so it is set whenever logprobs are asked
        // for at all.
        params.logprobs = Some(true);
        if let Some(logprobs) = cfg.logprobs {
            params.top_logprobs = Some(i64::from(logprobs));
        }
    }
    if let Some(instruction) = &cfg.system_instruction {
        let inst = flatten_content_text(Some(instruction))
            .map_err(|err| err.prefixed("openai: system instruction: "))?;
        if !inst.is_empty() {
            // There is no instructions field here, so the instruction leads the conversation
            // as a system message.
            params
                .messages
                .insert(0, ChatCompletionMessageParam::system(inst));
        }
    }
    let mime = cfg.response_mime_type.as_str();
    if !mime.is_empty() && mime != "text/plain" && mime != "application/json" {
        return Err(OpenAiError::new(ErrorKind::UnsupportedMimeType, mime));
    }
    if mime == "application/json"
        || cfg.response_schema.is_some()
        || cfg.response_json_schema.is_some()
    {
        params.response_format = Some(new_response_format(cfg)?);
    }
    if !cfg.labels.is_empty() {
        return Err(OpenAiError::sentinel(ErrorKind::LabelsNotSupported));
    }
    if !cfg.safety_settings.is_empty() {
        return Err(OpenAiError::sentinel(ErrorKind::SafetySettingsNotSupported));
    }
    // IncludeThoughts is accepted and ignored: this endpoint has no reasoning summary to ask
    // for, and refusing it would break a config that works against Responses.
    params.reasoning_effort = reasoning_effort_for(cfg.thinking_config.as_ref())?;
    if let Some(tier) = cfg
        .service_tier
        .as_ref()
        .filter(|tier| !tier.as_str().is_empty())
    {
        params.service_tier = Some(service_tier(tier).ok_or_else(|| {
            OpenAiError::new(
                ErrorKind::UnsupportedConfigField,
                format!("ServiceTier {}", go_quote(tier.as_str())),
            )
        })?);
    }
    reject_untranslatable_values(cfg, http_options)?;
    // Last, so the named errors above win when a caller sets both.
    reject_unsupported_config_fields(cfg, http_options, supported)
}

/// `json_object` without a schema, else a strict `json_schema` format
/// carrying `response_json_schema` (preferred) or `response_schema`, strict-rewritten.
fn new_response_format(
    cfg: &GenerateContentConfig,
) -> Result<ChatCompletionResponseFormat, OpenAiError> {
    if cfg.response_schema.is_none() && cfg.response_json_schema.is_none() {
        return Ok(ChatCompletionResponseFormat::JsonObject(
            ResponseFormatJsonObjectParam::default(),
        ));
    }
    let mut schema = match &cfg.response_json_schema {
        Some(json_schema) => normalize_schema(Some(json_schema))?,
        None => schema_to_map(cfg.response_schema.as_ref())?,
    };
    if let Some(schema) = schema.as_mut() {
        enforce_strict_openai_schema(schema);
    }
    Ok(ChatCompletionResponseFormat::JsonSchema(
        ResponseFormatJsonSchemaParam {
            json_schema: ResponseFormatJsonSchemaJsonSchemaParam {
                name: response_format_name(cfg.response_schema.as_ref()),
                strict: Some(true),
                description: None,
                schema,
            },
            ..ResponseFormatJsonSchemaParam::default()
        },
    ))
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use gadk_genai::{
        Blob, Content, FunctionCallingConfig, FunctionCallingConfigMode, FunctionDeclaration,
        GenerateContentConfig, Part, Schema, ServiceTier, ThinkingConfig, ThinkingLevel, Tool,
        ToolConfig, Type,
    };
    use gadk_model::LlmRequest;
    use pretty_assertions::assert_eq;
    use serde_json::{Map, Value, json};

    use super::build_params;
    use crate::client::encode_body;

    fn content(role: &str, parts: Vec<Part>) -> Content {
        Content {
            role: role.to_string(),
            parts,
        }
    }

    fn text(text: &str) -> Part {
        Part {
            text: text.to_string(),
            ..Part::default()
        }
    }

    fn object(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            _ => Map::new(),
        }
    }

    fn city_schema(title: &str) -> Schema {
        Schema {
            title: title.to_string(),
            r#type: Some(Type::Object),
            properties: [(
                "city".to_string(),
                Schema {
                    r#type: Some(Type::String),
                    ..Schema::default()
                },
            )]
            .into(),
            ..Schema::default()
        }
    }

    fn body(req: &LlmRequest) -> String {
        let params = build_params("gpt-4o-mini", req).unwrap();
        String::from_utf8(encode_body(&params, false).unwrap().to_vec()).unwrap()
    }

    /// Every message kind, media part and generation field in one request, against the bytes
    /// ADK-Go's `buildParams` + `json.Marshal` produce for it (openai-go v3.91.1 declaration
    /// order, Go float formatting, HTML escaping).
    #[test]
    fn full_request_matches_go_bytes() {
        let call = gadk_genai::FunctionCall {
            id: "call_1".to_string(),
            name: "get_weather".to_string(),
            args: object(json!({"city": "Lisbon", "days": 2.5})),
            ..gadk_genai::FunctionCall::default()
        };
        let response = gadk_genai::FunctionResponse {
            id: "call_1".to_string(),
            name: "get_weather".to_string(),
            response: object(json!({"c": -7})),
            ..gadk_genai::FunctionResponse::default()
        };
        let req = LlmRequest {
            model: "gpt-5".to_string(),
            contents: vec![
                content("user", vec![text("weather in <Lisbon> & Porto?")]),
                content(
                    "model",
                    vec![
                        text("let me check"),
                        Part {
                            function_call: Some(call),
                            ..Part::default()
                        },
                    ],
                ),
                content(
                    "user",
                    vec![Part {
                        function_response: Some(response),
                        ..Part::default()
                    }],
                ),
                content(
                    "user",
                    vec![
                        text("and this?"),
                        Part {
                            inline_data: Some(Blob {
                                mime_type: "image/png".to_string(),
                                data: Bytes::from_static(&[0x89, b'P', b'N', b'G']),
                                ..Blob::default()
                            }),
                            ..Part::default()
                        },
                        Part {
                            inline_data: Some(Blob {
                                mime_type: "application/pdf".to_string(),
                                data: Bytes::from_static(b"%PDF"),
                                display_name: "r.pdf".to_string(),
                            }),
                            ..Part::default()
                        },
                    ],
                ),
                content("developer", vec![text("be nice")]),
            ],
            config: Some(GenerateContentConfig {
                system_instruction: Some(Content::from_text("be terse", "user")),
                temperature: Some(0.3),
                top_p: Some(0.9),
                max_output_tokens: 256,
                stop_sequences: vec!["STOP".to_string()],
                seed: Some(42),
                frequency_penalty: Some(0.5),
                presence_penalty: Some(-0.25),
                response_logprobs: true,
                logprobs: Some(3),
                response_schema: Some(city_schema("answer")),
                thinking_config: Some(ThinkingConfig {
                    thinking_level: Some(ThinkingLevel::Low),
                    ..ThinkingConfig::default()
                }),
                service_tier: Some(ServiceTier::Flex),
                tools: vec![Tool {
                    function_declarations: vec![
                        FunctionDeclaration {
                            name: "get_weather".to_string(),
                            description: "look up the weather".to_string(),
                            parameters: Some(Schema {
                                title: String::new(),
                                ..city_schema("")
                            }),
                            ..FunctionDeclaration::default()
                        },
                        FunctionDeclaration {
                            name: "ping".to_string(),
                            ..FunctionDeclaration::default()
                        },
                    ],
                    ..Tool::default()
                }],
                tool_config: Some(ToolConfig {
                    function_calling_config: Some(FunctionCallingConfig {
                        mode: Some(FunctionCallingConfigMode::Any),
                        allowed_function_names: vec!["get_weather".to_string()],
                        ..FunctionCallingConfig::default()
                    }),
                    ..ToolConfig::default()
                }),
                ..GenerateContentConfig::default()
            }),
            ..LlmRequest::default()
        };
        let want = r#"{"messages":[{"content":"be terse","role":"system"},{"content":"weather in \u003cLisbon\u003e \u0026 Porto?","role":"user"},{"content":"let me check","tool_calls":[{"id":"call_1","function":{"arguments":"{\"city\":\"Lisbon\",\"days\":2.5}","name":"get_weather"},"type":"function"}],"role":"assistant"},{"content":"{\"c\":-7}","tool_call_id":"call_1","role":"tool"},{"content":[{"text":"and this?","type":"text"},{"image_url":{"url":"data:image/png;base64,iVBORw==","detail":"auto"},"type":"image_url"},{"file":{"file_data":"data:application/pdf;base64,JVBERg==","filename":"r.pdf"},"type":"file"}],"role":"user"},{"content":"be nice","role":"developer"}],"model":"gpt-5","frequency_penalty":0.5,"logprobs":true,"max_completion_tokens":256,"presence_penalty":-0.25,"seed":42,"temperature":0.30000001192092896,"top_logprobs":3,"top_p":0.8999999761581421,"reasoning_effort":"low","service_tier":"flex","stop":["STOP"],"response_format":{"json_schema":{"name":"answer","strict":true,"schema":{"additionalProperties":false,"properties":{"city":{"type":"string"}},"required":["city"],"title":"answer","type":"object"}},"type":"json_schema"},"tool_choice":{"function":{"name":"get_weather"},"type":"function"},"tools":[{"function":{"name":"get_weather","description":"look up the weather","parameters":{"properties":{"city":{"type":"string"}},"type":"object"}},"type":"function"},{"function":{"name":"ping","parameters":{"properties":{},"type":"object"}},"type":"function"}]}"#;
        assert_eq!(body(&req), want);
    }

    /// `json_object`, an allowed-tools choice (blank names skipped, Go map key order) and the
    /// none effort, against ADK-Go's bytes.
    #[test]
    fn json_object_and_allowed_tools_match_go_bytes() {
        let req = LlmRequest {
            contents: vec![content("user", vec![text("hi")])],
            config: Some(GenerateContentConfig {
                response_mime_type: "application/json".to_string(),
                tools: vec![Tool {
                    function_declarations: vec![FunctionDeclaration {
                        name: "ping".to_string(),
                        parameters_json_schema: Some(json!({"type": "object", "properties": {}})),
                        ..FunctionDeclaration::default()
                    }],
                    ..Tool::default()
                }],
                tool_config: Some(ToolConfig {
                    function_calling_config: Some(FunctionCallingConfig {
                        mode: Some(FunctionCallingConfigMode::Auto),
                        allowed_function_names: vec![
                            "ping".to_string(),
                            String::new(),
                            "pong".to_string(),
                        ],
                        ..FunctionCallingConfig::default()
                    }),
                    ..ToolConfig::default()
                }),
                thinking_config: Some(ThinkingConfig {
                    thinking_budget: Some(0),
                    ..ThinkingConfig::default()
                }),
                ..GenerateContentConfig::default()
            }),
            ..LlmRequest::default()
        };
        let want = r#"{"messages":[{"content":"hi","role":"user"}],"model":"gpt-4o-mini","reasoning_effort":"none","response_format":{"type":"json_object"},"tool_choice":{"allowed_tools":{"mode":"auto","tools":[{"function":{"name":"ping"},"type":"function"},{"function":{"name":"pong"},"type":"function"}]},"type":"allowed_tools"},"tools":[{"function":{"name":"ping","parameters":{"properties":{},"type":"object"}},"type":"function"}]}"#;
        assert_eq!(body(&req), want);
    }
}
