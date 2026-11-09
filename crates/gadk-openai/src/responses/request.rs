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

//! Responses request building (port of ADK-Go `internal/responses/request.go`: `buildParams`,
//! `convertContents`, `bufferedContent`, `newMessage`, `newMediaContent`, `newOutputMessage`,
//! `normalizeRole`, `newFunctionCall`, `newFunctionResponse`; the config half is
//! [`crate::responses::generation_config`], the schema half [`crate::responses::schema`]).

#![allow(dead_code)] // D1: first used by Task 9 (`responses::model`).

use gadk_genai::{Content, FunctionCall, FunctionResponse};
use gadk_model::LlmRequest;
use gadk_platform::go_quote;

use crate::error::{ErrorKind, OpenAiError};
use crate::responses::generation_config::apply_generation_config;
use crate::responses::tools::{convert_tool_choice, convert_tools};
use crate::responses::wire::{
    EasyInputMessageContent, EasyInputMessageParam, EasyInputMessageRole, EasyInputMessageType,
    FunctionCallOutputTag, FunctionCallTag, InputTextTag, OutputTextTag,
    ResponseFunctionCallOutput, ResponseFunctionToolCallParam, ResponseInput, ResponseInputContent,
    ResponseInputFileParam, ResponseInputImageDetail, ResponseInputImageParam, ResponseInputItem,
    ResponseInputItemFunctionCallOutputParam, ResponseInputTextParam, ResponseNewParams,
    ResponseOutputMessageContent, ResponseOutputMessageParam, ResponseOutputMessageStatus,
    ResponseOutputTextParam,
};
use crate::shared::calls::{CallTracker, marshal_function_args, marshal_function_response};
use crate::shared::contents::{PartField, replayed_reasoning, unsupported_payload};
use crate::shared::media::{Media, MediaKind, MediaSource, classify_media};

/// Go `imageInputDetail`: the detail every image input carries. openai-go drops an unset
/// detail from the request (the field is `omitzero`), so `auto` is sent explicitly.
const IMAGE_INPUT_DETAIL: ResponseInputImageDetail = ResponseInputImageDetail::Auto;

/// Go `buildParams`: the Responses params for `req`, sent as `model_name` unless `req.model`
/// names a model. Contents that convert to no item are the bare
/// [`ErrorKind::NoContents`](crate::error::ErrorKind::NoContents), or
/// `ErrNoContents: every part was dropped as replayed reasoning` when a dropped thought
/// emptied them; then the generation config
/// ([`apply_generation_config`](crate::responses::generation_config::apply_generation_config)
/// with `req.config` and `req.http_options`), tools, and the tool choice. Go's nil-request
/// check has no counterpart.
pub(crate) fn build_params(
    model_name: &str,
    req: &LlmRequest,
) -> Result<ResponseNewParams, OpenAiError> {
    let model = if req.model.is_empty() {
        model_name
    } else {
        req.model.as_str()
    };
    let mut params = ResponseNewParams {
        model: model.to_string(),
        ..ResponseNewParams::default()
    };

    let (input, dropped_reasoning) = convert_contents(&req.contents)?;
    if input.is_empty() {
        if dropped_reasoning {
            // The drop is what emptied the request; don't report it as a caller who sent
            // nothing. A request that was empty on arrival keeps the bare sentinel.
            return Err(OpenAiError::new(
                ErrorKind::NoContents,
                "every part was dropped as replayed reasoning",
            ));
        }
        return Err(OpenAiError::sentinel(ErrorKind::NoContents));
    }
    params.input = Some(ResponseInput::InputItemList(input));

    let cfg = req.config.as_ref();
    apply_generation_config(&mut params, cfg, req.http_options.as_ref())?;
    params.tools = convert_tools(cfg)?;
    if let Some(tool_cfg) = cfg.and_then(|cfg| cfg.tool_config.as_ref())
        && let Some(choice) = convert_tool_choice(Some(tool_cfg))?
    {
        params.tool_choice = Some(choice);
    }
    Ok(params)
}

/// Go `bufferedContent`: one piece of a message, held in the order it arrived (text and media
/// interleave inside a single message).
#[derive(Debug)]
enum Buffered {
    Text(String),
    Media(Media),
}

/// Go `convertContents`: the input items for `contents`, and whether any part was dropped as
/// replayed reasoning.
pub(crate) fn convert_contents(
    contents: &[Content],
) -> Result<(Vec<ResponseInputItem>, bool), OpenAiError> {
    let mut items = Vec::new();
    let mut tracker = CallTracker::default();
    let mut buffered = Vec::new();
    let mut dropped_reasoning = false;

    for content in contents {
        if content.parts.is_empty() {
            continue;
        }
        let role = content.role.as_str();
        for part in &content.parts {
            // Reported before anything is emitted, so a field this API cannot send is named
            // even when text or a call rides on the same part. Media is emitted below.
            if let Some(field) = unsupported_payload(part, &[PartField::Media]) {
                return Err(OpenAiError::plain(format!(
                    "openai: unsupported content part: {field}"
                )));
            }
            // Classified before any of it is emitted, for the same reason.
            let media = classify_media(part)?;
            // Text is read independently of a call or a response because one part can carry
            // both.
            let send_text = !part.text.is_empty() && !part.thought;
            if send_text {
                buffered.push(Buffered::Text(part.text.clone()));
            } else if !part.text.is_empty() || replayed_reasoning(part) {
                // Dropping reasoning must not hide a bad role. The drop counts toward the
                // emptied-request report only when it suppressed non-blank text.
                if !part.text.trim().is_empty() {
                    dropped_reasoning = true;
                }
                normalize_role(role)?;
            }
            // After the part's own text and before the call below, whose flush carries the
            // media out with it.
            let has_media = !media.is_empty();
            buffered.extend(media.into_iter().map(Buffered::Media));
            if let Some(fc) = &part.function_call {
                // Flush first so buffered text keeps its place ahead of the call.
                flush(role, &mut buffered, &mut items)?;
                items.push(ResponseInputItem::FunctionCall(new_function_call(
                    &mut tracker,
                    fc,
                )?));
            } else if let Some(fr) = &part.function_response {
                flush(role, &mut buffered, &mut items)?;
                items.push(ResponseInputItem::FunctionCallOutput(
                    new_function_response(&mut tracker, fr)?,
                ));
            } else if !send_text && !has_media && !replayed_reasoning(part) {
                // Nothing in the part reaches the request.
                return Err(OpenAiError::plain(
                    "openai: unsupported content part: carries nothing to send",
                ));
            }
        }
        // After all parts of a content, flush what remains.
        flush(role, &mut buffered, &mut items)?;
    }
    Ok((items, dropped_reasoning))
}

/// Go `convertContents`' `flushText`: the buffered content as one message of `role` (an
/// output message for the assistant, whose turns the API rejects as `input_text`).
fn flush(
    role: &str,
    buffered: &mut Vec<Buffered>,
    items: &mut Vec<ResponseInputItem>,
) -> Result<(), OpenAiError> {
    if buffered.is_empty() {
        return Ok(());
    }
    let msg_role = normalize_role(role)?;
    if msg_role == EasyInputMessageRole::Assistant {
        if let Some(msg) = new_output_message(buffered)? {
            items.push(ResponseInputItem::OutputMessage(msg));
        }
    } else if let Some(msg) = new_message(msg_role, buffered)? {
        items.push(ResponseInputItem::Message(msg));
    }
    buffered.clear();
    Ok(())
}

/// Go `newMessage`: an easy input message for an already-normalized role; `None` when nothing
/// but blank text was buffered.
fn new_message(
    msg_role: EasyInputMessageRole,
    buffered: &[Buffered],
) -> Result<Option<EasyInputMessageParam>, OpenAiError> {
    let mut content_list = Vec::with_capacity(buffered.len());
    for b in buffered {
        match b {
            Buffered::Media(media) => {
                // System and developer messages take input_text only.
                if matches!(
                    msg_role,
                    EasyInputMessageRole::System | EasyInputMessageRole::Developer
                ) {
                    return Err(OpenAiError::plain(format!(
                        "openai: media in a {} message is not supported: {}",
                        msg_role.as_str(),
                        media.mime_type
                    )));
                }
                content_list.push(new_media_content(media)?);
            }
            Buffered::Text(text) => {
                if text.trim().is_empty() {
                    continue;
                }
                content_list.push(ResponseInputContent::Text(ResponseInputTextParam {
                    text: text.clone(),
                    kind: InputTextTag,
                }));
            }
        }
    }
    if content_list.is_empty() {
        return Ok(None);
    }
    Ok(Some(EasyInputMessageParam {
        content: EasyInputMessageContent::InputItemContentList(content_list),
        role: msg_role,
        kind: Some(EasyInputMessageType::Message),
    }))
}

/// Go `newMediaContent`: one classified media value in the content field the Responses API
/// has for it. The kind picks the field (`input_image` / `input_file`), the source which of
/// its references carries the bytes; audio and video have no field
/// (`ErrUnsupportedMIMEType: <mime>`).
fn new_media_content(m: &Media) -> Result<ResponseInputContent, OpenAiError> {
    match m.kind {
        MediaKind::Image => {
            let mut image = ResponseInputImageParam {
                detail: Some(IMAGE_INPUT_DETAIL),
                ..ResponseInputImageParam::default()
            };
            match m.source {
                MediaSource::Data => image.image_url = Some(m.data_url()),
                MediaSource::FileId => image.file_id = Some(m.file_id.clone()),
                MediaSource::Url => image.image_url = Some(m.url.clone()),
            }
            Ok(ResponseInputContent::Image(image))
        }
        MediaKind::File => {
            let mut file = ResponseInputFileParam::default();
            match m.source {
                MediaSource::Data => file.file_data = Some(m.data_url()),
                MediaSource::FileId => file.file_id = Some(m.file_id.clone()),
                MediaSource::Url => file.file_url = Some(m.url.clone()),
            }
            // The filename travels only with inline data: a file id or a URL already names
            // the file.
            if m.source == MediaSource::Data && !m.filename.is_empty() {
                file.filename = Some(m.filename.clone());
            }
            Ok(ResponseInputContent::File(file))
        }
        MediaKind::Audio | MediaKind::Video => Err(OpenAiError::new(
            ErrorKind::UnsupportedMimeType,
            &m.mime_type,
        )),
    }
}

/// Go `newOutputMessage`: an assistant output message of `output_text` items, as replaying a
/// prior assistant turn requires; media has no field there
/// (`ErrMediaOnAssistantTurn: <mime>`). `None` when nothing but blank text was buffered.
fn new_output_message(
    buffered: &[Buffered],
) -> Result<Option<ResponseOutputMessageParam>, OpenAiError> {
    let mut content_list = Vec::with_capacity(buffered.len());
    for b in buffered {
        match b {
            Buffered::Media(media) => {
                return Err(OpenAiError::new(
                    ErrorKind::MediaOnAssistantTurn,
                    &media.mime_type,
                ));
            }
            Buffered::Text(text) => {
                if text.trim().is_empty() {
                    continue;
                }
                content_list.push(ResponseOutputMessageContent::OutputText(
                    ResponseOutputTextParam {
                        text: text.clone(),
                        kind: OutputTextTag,
                    },
                ));
            }
        }
    }
    if content_list.is_empty() {
        return Ok(None);
    }
    Ok(Some(ResponseOutputMessageParam {
        content: content_list,
        status: Some(ResponseOutputMessageStatus::Completed),
        ..ResponseOutputMessageParam::default()
    }))
}

/// Go `normalizeRole`: `""`/`user` → user, `model` → assistant, `system`, `developer`; else
/// `openai: unsupported role "<role>"` (plain).
pub(crate) fn normalize_role(role: &str) -> Result<EasyInputMessageRole, OpenAiError> {
    match role {
        "" | "user" => Ok(EasyInputMessageRole::User),
        "model" => Ok(EasyInputMessageRole::Assistant),
        "system" => Ok(EasyInputMessageRole::System),
        "developer" => Ok(EasyInputMessageRole::Developer),
        other => Err(OpenAiError::plain(format!(
            "openai: unsupported role {}",
            go_quote(other)
        ))),
    }
}

/// Go `newFunctionCall`: the `function_call` item for `fc` (id from
/// [`CallTracker::take_call_id`], arguments as Go JSON); a call without a name is the bare
/// [`ErrorKind::FunctionCallMissingName`].
fn new_function_call(
    tracker: &mut CallTracker,
    fc: &FunctionCall,
) -> Result<ResponseFunctionToolCallParam, OpenAiError> {
    if fc.name.is_empty() {
        return Err(OpenAiError::sentinel(ErrorKind::FunctionCallMissingName));
    }
    let call_id = tracker.take_call_id(fc);
    let arguments = marshal_function_args(&fc.args)?;
    Ok(ResponseFunctionToolCallParam {
        arguments,
        call_id,
        name: fc.name.clone(),
        kind: FunctionCallTag,
    })
}

/// Go `newFunctionResponse`: the `function_call_output` item answering `fr`'s call
/// ([`CallTracker::resolve_response_id`]; payload
/// [`marshal_function_response`](crate::shared::calls::marshal_function_response)).
pub(crate) fn new_function_response(
    tracker: &mut CallTracker,
    fr: &FunctionResponse,
) -> Result<ResponseInputItemFunctionCallOutputParam, OpenAiError> {
    let call_id = tracker.resolve_response_id(fr)?;
    let payload = marshal_function_response(&fr.response)?;
    Ok(ResponseInputItemFunctionCallOutputParam {
        output: ResponseFunctionCallOutput::String(payload),
        call_id: Some(call_id),
        kind: FunctionCallOutputTag,
    })
}
