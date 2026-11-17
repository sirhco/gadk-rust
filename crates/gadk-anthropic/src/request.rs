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

//! genai → Messages content conversion, except building the message params, which `model.rs`
//! assembles.
//!
//! Text-handling notes:
//! - [`go_to_lower`] lowercases char by char with Unicode's simple mappings (Rust's
//!   `str::to_lowercase` maps `İ` to two characters and handles a final sigma). Trimming
//!   uses `str::trim`.
//! - [`go_string`] keeps invalid UTF-8 as text with one U+FFFD per invalid byte.
//! - [`is_valid_https_url`] implements the URL syntax checks for an `https` URL itself, not
//!   the WHATWG rules of the `url` crate.

use std::net::Ipv6Addr;
use std::sync::LazyLock;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use gadk_genai::{
    Blob, Content, FileData, FunctionResponse, FunctionResponsePart, GenerateContentConfig,
    Language, Part, ROLE_MODEL,
};
use gadk_platform::go_to_lower;
use regex::Regex;
use serde_json::{Map, Value};

use crate::config::Backend;
use crate::error::{AnthropicError, ErrorKind};
use crate::tool_id::ToolUseIdSanitizer;
use crate::wire::{
    ContentBlockParam, DocumentBlockParam, ImageBlockParam, MessageParam,
    RedactedThinkingBlockParam, Role, TextBlockParam, ThinkingBlockParam, ToolResultBlockParam,
    ToolResultContent, ToolResultTag, ToolUseBlockParam, to_go_json,
};

/// What the conversion functions read from the model: the backend (file data is sent
/// only on [`Backend::Anthropic`]) and the request's model ID (a field of every warning).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ConvertCtx<'a> {
    /// The model's backend.
    pub(crate) backend: Backend,
    /// The resolved model ID of this request.
    pub(crate) model_id: &'a str,
}

/// The result of [`convert_content_parts`]: blocks, dropped reasons, and whether an empty
/// text part was seen.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ConvertedParts {
    /// The converted blocks, in part order.
    pub(crate) blocks: Vec<ContentBlockParam>,
    /// Why parts were dropped (e.g. `inline_data (mime_type=audio/wav)`).
    pub(crate) dropped_reasons: Vec<String>,
    /// Whether a part with no content (an empty text part) was seen.
    pub(crate) had_empty_text: bool,
}

/// The system instruction's non-empty text parts joined with `"\n\n"`
/// as one block, or none.
pub(crate) fn build_system_blocks(cfg: Option<&GenerateContentConfig>) -> Vec<TextBlockParam> {
    let Some(system) = cfg.and_then(|c| c.system_instruction.as_ref()) else {
        return Vec::new();
    };
    let texts: Vec<&str> = system
        .parts
        .iter()
        .filter(|p| !p.text.is_empty())
        .map(|p| p.text.as_str())
        .collect();
    if texts.is_empty() {
        return Vec::new();
    }
    vec![TextBlockParam::new(texts.join("\n\n"))]
}

/// `model` or `assistant` (trimmed, case-insensitive) is the assistant;
/// anything else is the user.
pub(crate) fn to_claude_role(role: &str) -> Role {
    match go_to_lower(role.trim()).as_str() {
        ROLE_MODEL | "assistant" => Role::Assistant,
        _ => Role::User,
    }
}

/// Converts every content, skips empty and fully dropped earlier
/// contents, coalesces adjacent same-role messages, and fails with `ErrorKind::NoContent` texts
/// (`contents must not be empty`, `newest message in contents is empty`, `newest message has
/// no sendable content[ after skipping unsupported parts (<reasons>)]`, `no sendable messages in
/// request`). One [`ToolUseIdSanitizer`] serves the whole request.
pub(crate) fn build_messages(
    cx: ConvertCtx<'_>,
    contents: &[Content],
) -> Result<Vec<MessageParam>, AnthropicError> {
    let Some(newest) = contents.last() else {
        return Err(AnthropicError::new(
            ErrorKind::NoContent,
            "contents must not be empty",
        ));
    };
    if newest.parts.is_empty() {
        return Err(AnthropicError::new(
            ErrorKind::NoContent,
            "newest message in contents is empty",
        ));
    }
    let newest_idx = contents.len() - 1;

    let mut sanitizer = ToolUseIdSanitizer::new();
    let mut messages: Vec<MessageParam> = Vec::new();

    for (i, content) in contents.iter().enumerate() {
        if content.parts.is_empty() {
            continue;
        }
        let role = to_claude_role(&content.role);
        let ConvertedParts {
            mut blocks,
            dropped_reasons,
            had_empty_text,
        } = convert_content_parts(cx, role, &content.parts, &mut sanitizer);

        if blocks.is_empty() && had_empty_text && dropped_reasons.is_empty() && role == Role::User {
            blocks.push(ContentBlockParam::text("."));
        }

        if blocks.is_empty() {
            if i == newest_idx {
                if !dropped_reasons.is_empty() {
                    return Err(AnthropicError::wrap2(
                        ErrorKind::NoContent,
                        ErrorKind::UnsupportedContent,
                        format!(
                            "newest message has no sendable content after skipping unsupported parts ({})",
                            dropped_reasons.join(", ")
                        ),
                    ));
                }
                return Err(AnthropicError::new(
                    ErrorKind::NoContent,
                    "newest message has no sendable content",
                ));
            }
            continue;
        }

        if role == Role::Assistant {
            blocks = order_assistant_blocks(blocks);
        }

        // Coalesce adjacent messages with the same role so conversation turns strictly
        // alternate between user and assistant.
        match messages.last_mut() {
            Some(last) if last.role == role => {
                last.content.append(&mut blocks);
                if role == Role::Assistant {
                    last.content = order_assistant_blocks(std::mem::take(&mut last.content));
                }
            }
            _ => messages.push(MessageParam {
                content: blocks,
                role,
            }),
        }
    }

    if messages.is_empty() {
        return Err(AnthropicError::new(
            ErrorKind::NoContent,
            "no sendable messages in request",
        ));
    }
    Ok(messages)
}

/// Orders thinking and redacted-thinking blocks first, others after, each
/// group in its original order.
pub(crate) fn order_assistant_blocks(blocks: Vec<ContentBlockParam>) -> Vec<ContentBlockParam> {
    let is_thinking = |b: &ContentBlockParam| {
        matches!(
            b,
            ContentBlockParam::Thinking(_) | ContentBlockParam::RedactedThinking(_)
        )
    };
    if !blocks.iter().any(is_thinking) || blocks.iter().all(is_thinking) {
        return blocks;
    }
    let (mut thinking, others): (Vec<_>, Vec<_>) = blocks.into_iter().partition(is_thinking);
    thinking.extend(others);
    thinking
}

/// Converts the parts of one content, by part kind in this order (thought, content-free
/// signature, function call, function response, inline data, file data, executable code, code
/// execution result, unsupported server-side/audio/video parts, text, empty).
pub(crate) fn convert_content_parts(
    cx: ConvertCtx<'_>,
    role: Role,
    parts: &[Part],
    sanitizer: &mut ToolUseIdSanitizer,
) -> ConvertedParts {
    let mut out = ConvertedParts::default();
    for part in parts {
        // 1. Thought parts (thinking and redacted_thinking).
        if part.thought {
            if role != Role::Assistant {
                // Claude only accepts thinking blocks on assistant turns.
                continue;
            }
            if !part.text.is_empty() {
                if part.thought_signature.is_empty() {
                    // Strip unsigned thinking blocks when replaying history, as Claude
                    // rejects them.
                    continue;
                }
                out.blocks
                    .push(ContentBlockParam::Thinking(ThinkingBlockParam::new(
                        go_string(&part.thought_signature),
                        part.text.clone(),
                    )));
                continue;
            }
            if !part.thought_signature.is_empty() {
                out.blocks.push(ContentBlockParam::RedactedThinking(
                    RedactedThinkingBlockParam::new(go_string(&part.thought_signature)),
                ));
            }
            continue;
        }

        // 2. Content-free foreign thought signature (e.g. from a Gemini turn in history).
        if is_content_free_signature(part) {
            warn_skipped_part(cx, role, "thought_signature", "", "");
            continue;
        }

        // 3. FunctionCall -> tool_use.
        if let Some(fc) = &part.function_call {
            let tool_id = sanitizer.sanitize(&fc.id, &fc.name, true);
            out.blocks
                .push(ContentBlockParam::ToolUse(ToolUseBlockParam::new(
                    tool_id,
                    fc.args.clone(),
                    fc.name.clone(),
                )));
            continue;
        }

        // 4. FunctionResponse -> tool_result.
        if let Some(fr) = &part.function_response {
            out.blocks
                .push(convert_function_response_part(cx, role, fr, sanitizer));
            continue;
        }

        // 5. InlineData (images, PDF, plain text).
        if let Some(blob) = &part.inline_data {
            match convert_inline_data_part(cx, role, blob) {
                Ok(block) => out.blocks.push(block),
                Err(reason) => out.dropped_reasons.push(reason),
            }
            continue;
        }

        // 6. FileData (https:// URLs for images and PDFs on Backend::Anthropic).
        if let Some(fd) = &part.file_data {
            match convert_file_data_part(cx, role, fd) {
                Ok(block) => out.blocks.push(block),
                Err(reason) => out.dropped_reasons.push(reason),
            }
            continue;
        }

        // 7. ExecutableCode and CodeExecutionResult (text blocks, like adk-python).
        if let Some(code) = &part.executable_code {
            let raw_lang = code.language.as_ref().map_or("", Language::as_str);
            let mut lang = go_to_lower(raw_lang.trim());
            if lang == go_to_lower(Language::Unspecified.as_str()) {
                lang.clear();
            }
            out.blocks.push(ContentBlockParam::text(format!(
                "Code:\n```{lang}\n{}\n```",
                code.code
            )));
            continue;
        }
        if let Some(result) = &part.code_execution_result {
            out.blocks.push(ContentBlockParam::text(format!(
                "Execution Result:\n```code_output\n{}\n```",
                result.output
            )));
            continue;
        }

        // 8. Unsupported server-side tool / audio / video metadata parts.
        let unsupported = if part.tool_call.is_some() {
            Some("tool_call")
        } else if part.tool_response.is_some() {
            Some("tool_response")
        } else if part.audio_transcription.is_some() {
            Some("audio_transcription")
        } else if part.video_metadata.is_some() && part.text.is_empty() {
            Some("video_metadata")
        } else {
            None
        };
        if let Some(kind) = unsupported {
            warn_skipped_part(cx, role, kind, "", "");
            out.dropped_reasons.push(kind.to_string());
            continue;
        }

        // 9. Plain text.
        if !part.text.is_empty() {
            out.blocks.push(ContentBlockParam::text(part.text.clone()));
            continue;
        }

        out.had_empty_text = true;
    }
    out
}

/// Whether the part carries only a thought signature and no content.
pub(crate) fn is_content_free_signature(part: &Part) -> bool {
    if part.thought_signature.is_empty() || part.thought || !part.text.is_empty() {
        return false;
    }
    part.function_call.is_none()
        && part.function_response.is_none()
        && part.inline_data.is_none()
        && part.file_data.is_none()
        && part.executable_code.is_none()
        && part.code_execution_result.is_none()
        && part.tool_call.is_none()
        && part.tool_response.is_none()
        && part.audio_transcription.is_none()
}

/// The value for a warning field: `unspecified` for an empty value.
fn label(value: &str) -> &str {
    if value.is_empty() {
        "unspecified"
    } else {
        value
    }
}

/// Converts an inline-data part: `Err(reason)` when the part is dropped (after the warning).
pub(crate) fn convert_inline_data_part(
    cx: ConvertCtx<'_>,
    role: Role,
    blob: &Blob,
) -> Result<ContentBlockParam, String> {
    let mime_type = normalize_mime_type(&blob.mime_type);
    let label_mime = label(&mime_type);
    if role != Role::User {
        warn_skipped_part(cx, role, "inline_data", &mime_type, "");
        return Err(format!(
            "inline_data (mime_type={label_mime} on {} turn)",
            role.as_str()
        ));
    }
    if blob.data.is_empty() {
        warn_skipped_part(cx, role, "inline_data", &mime_type, "");
        return Err(format!("empty inline_data (mime_type={label_mime})"));
    }

    if is_supported_image_mime(&mime_type) {
        let encoded = STANDARD.encode(&blob.data);
        return Ok(ContentBlockParam::Image(ImageBlockParam::base64(
            mime_type.as_str(),
            encoded,
        )));
    }
    match mime_type.as_str() {
        "application/pdf" => Ok(ContentBlockParam::Document(DocumentBlockParam::base64_pdf(
            STANDARD.encode(&blob.data),
        ))),
        "text/plain" => Ok(ContentBlockParam::Document(DocumentBlockParam::plain_text(
            go_string(&blob.data),
        ))),
        _ => {
            warn_skipped_part(cx, role, "inline_data", &mime_type, "");
            Err(format!("inline_data (mime_type={label_mime})"))
        }
    }
}

/// Converts a file-data part: `Err(reason)` when the part is dropped (after the warning).
pub(crate) fn convert_file_data_part(
    cx: ConvertCtx<'_>,
    role: Role,
    fd: &FileData,
) -> Result<ContentBlockParam, String> {
    let mime_type = normalize_mime_type(&fd.mime_type);
    let raw_uri = fd.file_uri.trim();
    let scheme = extract_uri_scheme(raw_uri);
    let reason = || {
        format!(
            "file_data (uri_scheme={}, mime_type={})",
            label(&scheme),
            label(&mime_type)
        )
    };

    if role != Role::User
        || cx.backend != Backend::Anthropic
        || scheme != "https"
        || !is_valid_https_url(raw_uri)
    {
        warn_skipped_part(cx, role, "file_data", &mime_type, &scheme);
        return Err(reason());
    }

    if is_supported_image_mime(&mime_type) {
        return Ok(ContentBlockParam::Image(ImageBlockParam::url(raw_uri)));
    }
    if mime_type == "application/pdf" {
        return Ok(ContentBlockParam::Document(DocumentBlockParam::url_pdf(
            raw_uri,
        )));
    }
    warn_skipped_part(cx, role, "file_data", &mime_type, &scheme);
    Err(reason())
}

/// Converts a function response part: a `tool_result` with `is_error: false`, the serialized
/// response as a text block (if non-empty) and the media blocks.
pub(crate) fn convert_function_response_part(
    cx: ConvertCtx<'_>,
    role: Role,
    fr: &FunctionResponse,
    sanitizer: &mut ToolUseIdSanitizer,
) -> ContentBlockParam {
    let tool_use_id = sanitizer.sanitize(&fr.id, &fr.name, false);
    let text_content = serialize_function_response_data(&fr.response);
    let media_blocks = convert_function_response_media(cx, role, &fr.parts);

    let mut content = Vec::with_capacity(media_blocks.len() + 1);
    if !text_content.is_empty() {
        content.push(ToolResultContent::Text(TextBlockParam::new(text_content)));
    }
    content.extend(media_blocks);

    ContentBlockParam::ToolResult(ToolResultBlockParam {
        tool_use_id,
        is_error: Some(false),
        cache_control: None,
        content,
        kind: ToolResultTag,
    })
}

/// Serializes a function response's data. JSON is written with [`crate::wire::to_go_json`]
/// (compact, HTML-escaped).
///
/// The response map holds JSON values, so every [`Value::Array`] is treated as a list.
pub(crate) fn serialize_function_response_data(resp: &Map<String, Value>) -> String {
    if resp.is_empty() {
        return String::new();
    }
    match resp.get("content") {
        Some(Value::Array(items)) if !items.is_empty() => {
            return items
                .iter()
                .map(|item| match item {
                    Value::Object(m)
                        if m.get("type").and_then(Value::as_str) == Some("text")
                            && m.get("text").is_some_and(Value::is_string) =>
                    {
                        m.get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string()
                    }
                    other => format_arbitrary_value(other),
                })
                .collect::<Vec<_>>()
                .join("\n");
        }
        Some(Value::String(s)) if !s.is_empty() => return s.clone(),
        _ => {}
    }

    if resp.len() == 1
        && let Some(result) = resp.get("result").filter(|v| !v.is_null())
    {
        return format_arbitrary_value(result);
    }

    to_go_json(resp)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_default()
}

/// Formats a value as text: a JSON string as itself, `null` as `""`, anything else as JSON
/// ([`crate::wire::to_go_json`]).
pub(crate) fn format_arbitrary_value(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => to_go_json(other)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .unwrap_or_else(|| other.to_string()),
    }
}

/// Converts the media parts of a function response.
pub(crate) fn convert_function_response_media(
    cx: ConvertCtx<'_>,
    role: Role,
    parts: &[FunctionResponsePart],
) -> Vec<ToolResultContent> {
    let mut out = Vec::new();
    for p in parts {
        if let Some(fd) = &p.file_data {
            let mime_type = normalize_mime_type(&fd.mime_type);
            let raw_uri = fd.file_uri.trim();
            let scheme = extract_uri_scheme(raw_uri);
            if role != Role::User
                || cx.backend != Backend::Anthropic
                || scheme != "https"
                || !is_valid_https_url(raw_uri)
            {
                warn_skipped_part(cx, role, "function_response_file_data", &mime_type, &scheme);
                continue;
            }
            if is_supported_image_mime(&mime_type) {
                out.push(ToolResultContent::Image(ImageBlockParam::url(raw_uri)));
            } else if mime_type == "application/pdf" {
                out.push(ToolResultContent::Document(DocumentBlockParam::url_pdf(
                    raw_uri,
                )));
            } else {
                warn_skipped_part(cx, role, "function_response_file_data", &mime_type, &scheme);
            }
            continue;
        }
        let Some(blob) = &p.inline_data else {
            continue;
        };
        let mime_type = normalize_mime_type(&blob.mime_type);
        if blob.data.is_empty() {
            warn_skipped_part(cx, role, "function_response_inline_data", &mime_type, "");
            continue;
        }
        if is_supported_image_mime(&mime_type) {
            let encoded = STANDARD.encode(&blob.data);
            out.push(ToolResultContent::Image(ImageBlockParam::base64(
                mime_type.as_str(),
                encoded,
            )));
        } else if mime_type == "application/pdf" {
            out.push(ToolResultContent::Document(DocumentBlockParam::base64_pdf(
                STANDARD.encode(&blob.data),
            )));
        } else if mime_type == "text/plain" {
            out.push(ToolResultContent::Document(DocumentBlockParam::plain_text(
                go_string(&blob.data),
            )));
        } else {
            warn_skipped_part(cx, role, "function_response_inline_data", &mime_type, "");
        }
    }
    out
}

/// Logs a skipped part: `tracing::warn!` with the message
/// `skipping unsupported content part for Claude request` and the fields `backend`, `model`,
/// `role`, `part_kind`, `mime_type`, `uri_scheme`.
pub(crate) fn warn_skipped_part(
    cx: ConvertCtx<'_>,
    role: Role,
    part_kind: &str,
    mime_type: &str,
    uri_scheme: &str,
) {
    tracing::warn!(
        backend = cx.backend.as_str(),
        model = cx.model_id,
        role = role.as_str(),
        part_kind,
        mime_type,
        uri_scheme,
        "skipping unsupported content part for Claude request"
    );
}

#[allow(clippy::expect_used)] // constant pattern, cannot fail
static VALID_MIME_TYPE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(application|audio|example|font|haptics|image|message|model|multipart|text|video|x-[a-z0-9._+-]{1,32})/[a-z0-9][a-z0-9._+-]{0,63}$",
    )
    .expect("valid MIME type regex")
});
#[allow(clippy::expect_used)] // constant pattern, cannot fail
static VALID_URI_SCHEME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9+.-]{0,15}$").expect("valid URI scheme regex"));

/// Normalizes a MIME type: the lowercased type before any `;`, `""` when blank, `invalid` when
/// it does not match the MIME type pattern.
pub(crate) fn normalize_mime_type(raw: &str) -> String {
    let base = raw.split_once(';').map_or(raw, |(base, _)| base);
    let cleaned = go_to_lower(base.trim());
    if cleaned.is_empty() {
        return String::new();
    }
    if !VALID_MIME_TYPE.is_match(&cleaned) {
        return "invalid".to_string();
    }
    cleaned
}

/// Whether the MIME type is supported for images: `image/jpeg`, `image/png`, `image/gif`,
/// `image/webp`.
pub(crate) fn is_supported_image_mime(mime_type: &str) -> bool {
    matches!(
        mime_type,
        "image/jpeg" | "image/png" | "image/gif" | "image/webp"
    )
}

/// The lowercased URI scheme, `""` for a blank URI, `unknown` when it is
/// malformed or not followed by `//` (except `data`, `blob`, `urn`, `mailto`).
pub(crate) fn extract_uri_scheme(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let Some((scheme_raw, rest)) = trimmed.split_once(':') else {
        return "unknown".to_string();
    };
    let scheme = go_to_lower(scheme_raw);
    if !VALID_URI_SCHEME.is_match(&scheme) {
        return "unknown".to_string();
    }
    if !rest.starts_with("//") && !matches!(scheme.as_str(), "data" | "blob" | "urn" | "mailto") {
        return "unknown".to_string();
    }
    scheme
}

/// Whether `raw` parses as a URL with scheme `https` (any case) and a non-empty host.
///
/// Checks, in order: no ASCII control byte before
/// the `#`; a scheme of letters, digits, `+`, `-`, `.` not starting with a digit or sign; the
/// query (not validated) cut at the first `?`; an authority after `//` up to the next
/// `/` with a valid userinfo, an IP literal that is an IPv6 address, an optional all-digit
/// port (one colon only) and only host-safe bytes; and
/// well-formed `%` escapes in userinfo, host, path and fragment.
pub(crate) fn is_valid_https_url(raw: &str) -> bool {
    let (url, fragment) = raw.split_once('#').unwrap_or((raw, ""));
    if url.bytes().any(|b| b < b' ' || b == 0x7f) {
        return false;
    }
    let Some((scheme, rest)) = go_split_scheme(url) else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("https") {
        return false;
    }
    let rest = if rest.ends_with('?') && rest.matches('?').count() == 1 {
        &rest[..rest.len() - 1]
    } else {
        rest.split_once('?').map_or(rest, |(before, _)| before)
    };
    let Some(after_slashes) = rest.strip_prefix("//") else {
        // Opaque or host-less: the host is empty.
        return false;
    };
    let (authority, path) = match after_slashes.find('/') {
        Some(i) => after_slashes.split_at(i),
        None => (after_slashes, ""),
    };
    let host = match authority.rfind('@') {
        Some(i) => {
            let userinfo = &authority[..i];
            if !go_valid_userinfo(userinfo) || !go_unescape_ok(userinfo, Escape::Other) {
                return false;
            }
            &authority[i + 1..]
        }
        None => authority,
    };
    go_parse_host_ok(host)
        && go_unescape_ok(path, Escape::Other)
        && go_unescape_ok(fragment, Escape::Other)
        && !host.is_empty()
}

/// Splits an absolute URL into scheme and rest: `None` when there is no scheme.
fn go_split_scheme(url: &str) -> Option<(&str, &str)> {
    for (i, b) in url.bytes().enumerate() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' => {}
            b'0'..=b'9' | b'+' | b'-' | b'.' if i > 0 => {}
            b':' if i > 0 => return Some((&url[..i], &url[i + 1..])),
            _ => return None,
        }
    }
    None
}

/// Whether every byte of the userinfo is allowed.
fn go_valid_userinfo(s: &str) -> bool {
    s.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"-._:~!$&'()*+,;=%@".contains(&b))
}

/// The `net/url` encoding modes whose `unescape` checks differ.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Escape {
    /// `encodeHost`.
    Host,
    /// `encodeZone`.
    Zone,
    /// Path, fragment, user/password: only `%` escapes are checked.
    Other,
}

/// Whether `b` is an ASCII byte allowed unescaped in a host.
fn go_host_byte_ok(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!$&'()*+,;=:[]<>\"-_.~".contains(&b)
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Unescapes `%XX` sequences: the unescaped bytes, or `None` for a malformed escape.
fn go_unescape(s: &str, mode: Escape) -> Option<Vec<u8>> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'%' {
            let hi = hex_val(*bytes.get(i + 1)?)?;
            let lo = hex_val(*bytes.get(i + 2)?)?;
            let is_pct25 = &bytes[i..i + 3] == b"%25";
            // In a host, `%` may only escape non-ASCII bytes (or be `%25`).
            if mode == Escape::Host && hi < 8 && !is_pct25 {
                return None;
            }
            let v = hi << 4 | lo;
            // In a zone, `%` may only escape bytes a host may hold unescaped, or a space
            // (`shouldEscape(v, encodeHost)` is true for every byte >= 0x80).
            if mode == Escape::Zone && !is_pct25 && v != b' ' && (v >= 0x80 || !go_host_byte_ok(v))
            {
                return None;
            }
            out.push(v);
            i += 3;
            continue;
        }
        if matches!(mode, Escape::Host | Escape::Zone) && b < 0x80 && !go_host_byte_ok(b) {
            return None;
        }
        out.push(b);
        i += 1;
    }
    Some(out)
}

fn go_unescape_ok(s: &str, mode: Escape) -> bool {
    go_unescape(s, mode).is_some()
}

/// Whether the port is empty or `:` followed by digits.
fn go_valid_optional_port(port: &str) -> bool {
    port.is_empty()
        || port
            .strip_prefix(':')
            .is_some_and(|digits| digits.bytes().all(|b| b.is_ascii_digit()))
}

/// Whether `host` is valid for an `https` URL.
fn go_parse_host_ok(host: &str) -> bool {
    match host.rfind('[') {
        Some(i) if i > 0 => false,
        Some(_) => {
            let Some(close) = host.rfind(']') else {
                return false;
            };
            let colon_port = &host[close + 1..];
            if !go_valid_optional_port(colon_port) || !go_unescape_ok(colon_port, Escape::Host) {
                return false;
            }
            let hostname = &host[1..close];
            let unescaped = match hostname.find("%25") {
                Some(zone) => {
                    let (Some(mut addr), Some(zone)) = (
                        go_unescape(&hostname[..zone], Escape::Host),
                        go_unescape(&hostname[zone..], Escape::Zone),
                    ) else {
                        return false;
                    };
                    addr.extend(zone);
                    addr
                }
                None => match go_unescape(hostname, Escape::Host) {
                    Some(addr) => addr,
                    None => return false,
                },
            };
            go_is_ipv6_literal(&unescaped)
        }
        None => {
            if let Some(i) = host.find(':') {
                // An https host has at most one colon, so the
                // port runs from the first one.
                if !go_valid_optional_port(&host[i..]) {
                    return false;
                }
            }
            go_unescape_ok(host, Escape::Host)
        }
    }
}

/// Whether `s` is an IPv6 address (not IPv4) with an optional
/// non-empty `%zone`.
fn go_is_ipv6_literal(s: &[u8]) -> bool {
    let (addr, zone) = match s.iter().position(|&b| b == b'%') {
        Some(i) => (&s[..i], Some(&s[i + 1..])),
        None => (s, None),
    };
    if zone.is_some_and(<[u8]>::is_empty) {
        return false;
    }
    std::str::from_utf8(addr).is_ok_and(|a| a.parse::<Ipv6Addr>().is_ok())
}

/// Lossy text of bytes: each byte that does not start a valid
/// UTF-8 sequence becomes one U+FFFD.
fn go_string(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for chunk in bytes.utf8_chunks() {
        out.push_str(chunk.valid());
        for _ in chunk.invalid() {
            out.push('\u{FFFD}');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::fmt;
    use std::sync::{Arc, Mutex};

    use bytes::Bytes;
    use gadk_genai::{
        CodeExecutionResult, ExecutableCode, FunctionCall, FunctionResponseBlob,
        FunctionResponseFileData, Outcome, ROLE_USER, ToolCall, ToolResponse, Transcription,
        VideoMetadata,
    };
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use tracing::field::{Field, Visit};
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::{Context, SubscriberExt};

    use super::*;

    const MODEL: &str = "claude-sonnet-4-5-20250929";
    const WARN_MSG: &str = "skipping unsupported content part for Claude request";

    fn anthropic() -> ConvertCtx<'static> {
        ConvertCtx {
            backend: Backend::Anthropic,
            model_id: MODEL,
        }
    }

    fn vertex() -> ConvertCtx<'static> {
        ConvertCtx {
            backend: Backend::Vertex,
            model_id: MODEL,
        }
    }

    /// One captured `tracing` event: level, message and the other fields in order.
    #[derive(Debug, Clone, PartialEq)]
    struct Record {
        level: tracing::Level,
        message: String,
        fields: Vec<(String, String)>,
    }

    impl Record {
        fn field(&self, name: &str) -> &str {
            self.fields
                .iter()
                .find(|(k, _)| k == name)
                .map_or("", |(_, v)| v.as_str())
        }
    }

    struct Visitor<'a>(&'a mut Record);

    impl Visitor<'_> {
        fn push(&mut self, field: &Field, value: String) {
            if field.name() == "message" {
                self.0.message = value;
            } else {
                self.0.fields.push((field.name().to_string(), value));
            }
        }
    }

    impl Visit for Visitor<'_> {
        fn record_str(&mut self, field: &Field, value: &str) {
            self.push(field, value.to_string());
        }

        fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
            self.push(field, format!("{value:?}"));
        }
    }

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<Record>>>);

    impl<S: tracing::Subscriber> Layer<S> for Capture {
        fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
            let mut record = Record {
                level: *event.metadata().level(),
                message: String::new(),
                fields: Vec::new(),
            };
            event.record(&mut Visitor(&mut record));
            self.0.lock().unwrap().push(record);
        }
    }

    /// Runs `f` with a thread-local subscriber and returns its result and the events.
    fn capture<R>(f: impl FnOnce() -> R) -> (R, Vec<Record>) {
        let cap = Capture::default();
        let subscriber = tracing_subscriber::registry().with(cap.clone());
        let out = tracing::subscriber::with_default(subscriber, f);
        let records = cap.0.lock().unwrap().clone();
        (out, records)
    }

    fn json_of<T: serde::Serialize>(value: &T) -> String {
        String::from_utf8(to_go_json(value).unwrap()).unwrap()
    }

    fn user(parts: Vec<Part>) -> Content {
        Content::from_parts(parts, ROLE_USER)
    }

    fn model(parts: Vec<Part>) -> Content {
        Content::from_parts(parts, ROLE_MODEL)
    }

    fn text(t: &str) -> Part {
        Part::from_text(t)
    }

    fn bytes_part(data: &'static [u8], mime_type: &str) -> Part {
        Part::from_bytes(Bytes::from_static(data), mime_type)
    }

    fn call(id: &str, name: &str, args: Value) -> Part {
        Part {
            function_call: Some(FunctionCall {
                id: id.to_string(),
                name: name.to_string(),
                args: args.as_object().cloned().unwrap_or_default(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn response_part(id: &str, name: &str, response: Value) -> Part {
        Part {
            function_response: Some(FunctionResponse {
                id: id.to_string(),
                name: name.to_string(),
                response: response.as_object().cloned().unwrap_or_default(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn thought(text: &str, signature: &'static [u8]) -> Part {
        Part {
            thought: true,
            text: text.to_string(),
            thought_signature: Bytes::from_static(signature),
            ..Default::default()
        }
    }

    fn convert(cx: ConvertCtx<'_>, role: Role, parts: &[Part]) -> (ConvertedParts, Vec<Record>) {
        capture(|| convert_content_parts(cx, role, parts, &mut ToolUseIdSanitizer::new()))
    }

    #[test]
    fn build_system_blocks_joins_non_empty_texts() {
        assert_eq!(build_system_blocks(None), Vec::new());
        assert_eq!(
            build_system_blocks(Some(&GenerateContentConfig::default())),
            Vec::new()
        );
        let cfg = GenerateContentConfig {
            system_instruction: Some(user(vec![text("Be terse."), text(""), text("Use SI.")])),
            ..Default::default()
        };
        assert_eq!(
            build_system_blocks(Some(&cfg)),
            vec![TextBlockParam::new("Be terse.\n\nUse SI.")]
        );
        let empty = GenerateContentConfig {
            system_instruction: Some(user(vec![text("")])),
            ..Default::default()
        };
        assert_eq!(build_system_blocks(Some(&empty)), Vec::new());
    }

    #[test]
    fn to_claude_role_like_go() {
        for role in [
            "model",
            " MODEL ",
            "assistant",
            "Assistant",
            "ASS\u{130}STANT",
        ] {
            assert_eq!(to_claude_role(role), Role::Assistant, "{role:?}");
        }
        for role in ["user", "", "system", "tool", "models"] {
            assert_eq!(to_claude_role(role), Role::User, "{role:?}");
        }
    }

    #[test]
    fn build_messages_empty_contents_errors_are_gos() {
        let err = build_messages(anthropic(), &[]).unwrap_err();
        assert!(err.is(ErrorKind::NoContent));
        assert_eq!(
            err.to_string(),
            "anthropicmodel: request contains no sendable content: contents must not be empty"
        );

        let err = build_messages(anthropic(), &[user(vec![text("hi")]), user(vec![])]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "anthropicmodel: request contains no sendable content: newest message in contents is empty"
        );

        // An empty-text assistant turn has nothing to send (no "." fallback off user turns).
        let err = build_messages(
            anthropic(),
            &[user(vec![text("hi")]), model(vec![text("")])],
        )
        .unwrap_err();
        assert_eq!(err.kinds(), &[ErrorKind::NoContent]);
        assert_eq!(
            err.to_string(),
            "anthropicmodel: request contains no sendable content: newest message has no sendable content"
        );
    }

    #[test]
    fn build_messages_empty_text_user_turn_sends_a_dot() {
        let msgs = build_messages(anthropic(), &[user(vec![text("")])]).unwrap();
        assert_eq!(
            msgs,
            vec![MessageParam {
                content: vec![ContentBlockParam::text(".")],
                role: Role::User,
            }]
        );
    }

    // Go TestMultimodalSupport/newest_message_empty_after_skipping_unsupported_fails_without_network_call.
    #[test]
    fn build_messages_newest_message_empty_after_skipping_unsupported() {
        let contents = [
            Content::from_text("Earlier valid message", ROLE_USER),
            Content::from_text("Earlier assistant reply", ROLE_MODEL),
            user(vec![bytes_part(b"audio", "audio/wav")]),
        ];
        let (result, records) = capture(|| build_messages(anthropic(), &contents));
        let err = result.unwrap_err();
        assert!(err.is(ErrorKind::NoContent) && err.is(ErrorKind::UnsupportedContent));
        assert_eq!(
            err.to_string(),
            "anthropicmodel: request contains no sendable content: anthropicmodel: unsupported content: \
             newest message has no sendable content after skipping unsupported parts (inline_data (mime_type=audio/wav))"
        );
        assert_eq!(records.len(), 1);
    }

    // Go TestMultimodalSupport/earlier_unsupported_message_omitted_and_session_continues.
    #[test]
    fn build_messages_earlier_unsupported_message_is_omitted() {
        let contents = [
            user(vec![bytes_part(b"audio", "audio/wav")]),
            Content::from_text("Valid follow-up question", ROLE_USER),
        ];
        let (result, records) = capture(|| build_messages(anthropic(), &contents));
        assert_eq!(
            result.unwrap(),
            vec![MessageParam {
                content: vec![ContentBlockParam::text("Valid follow-up question")],
                role: Role::User,
            }]
        );
        assert_eq!(records.len(), 1);
    }

    #[test]
    fn build_messages_skips_empty_earlier_contents_and_coalesces_same_roles() {
        let contents = [
            user(vec![]),
            user(vec![text("one")]),
            user(vec![text("two")]),
            model(vec![text("answer")]),
            model(vec![text("more"), thought("why", b"sig")]),
            user(vec![text("next")]),
        ];
        let msgs = build_messages(anthropic(), &contents).unwrap();
        assert_eq!(
            json_of(&msgs),
            r#"[{"content":[{"text":"one","type":"text"},{"text":"two","type":"text"}],"role":"user"},{"content":[{"signature":"sig","thinking":"why","type":"thinking"},{"text":"answer","type":"text"},{"text":"more","type":"text"}],"role":"assistant"},{"content":[{"text":"next","type":"text"}],"role":"user"}]"#
        );
    }

    // Go TestExtendedThinkingAndEffort/streaming_thinking_redacted_and_tool_loop_roundtrip (replay half).
    #[test]
    fn replayed_assistant_turn_orders_thinking_first_and_strips_unsigned_thoughts() {
        let contents = [
            Content::from_text("Compute 6*7", ROLE_USER),
            model(vec![
                text("I will calculate that."),
                Part {
                    thought: true,
                    text: "unsigned thought that must be stripped".to_string(),
                    ..Default::default()
                },
                Part {
                    thought_signature: Bytes::from_static(
                        b"foreign_gemini_sig_without_thought_flag",
                    ),
                    ..Default::default()
                },
                thought("Let me compute.", b"sig_stream_123"),
                thought("", b"enc_blob_456"),
                call("toolu_calc", "calc", json!({"expr": "6*7"})),
            ]),
            user(vec![response_part(
                "toolu_calc",
                "calc",
                json!({"result": 42}),
            )]),
        ];
        let (msgs, records) = capture(|| build_messages(anthropic(), &contents));
        let msgs = msgs.unwrap();
        assert_eq!(
            json_of(&msgs[1]),
            r#"{"content":[{"signature":"sig_stream_123","thinking":"Let me compute.","type":"thinking"},{"data":"enc_blob_456","type":"redacted_thinking"},{"text":"I will calculate that.","type":"text"},{"id":"toolu_calc","input":{"expr":"6*7"},"name":"calc","type":"tool_use"}],"role":"assistant"}"#
        );
        assert_eq!(
            json_of(&msgs[2]),
            r#"{"content":[{"tool_use_id":"toolu_calc","is_error":false,"content":[{"text":"42","type":"text"}],"type":"tool_result"}],"role":"user"}"#
        );
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].field("part_kind"), "thought_signature");
    }

    #[test]
    fn thought_parts_off_assistant_turns_or_without_content_are_dropped_silently() {
        let parts = [thought("t", b"sig"), thought("", b"sig"), thought("", b"")];
        let (out, records) = convert(anthropic(), Role::User, &parts);
        assert_eq!(out, ConvertedParts::default());
        assert!(records.is_empty());

        let (out, records) = convert(anthropic(), Role::Assistant, &[thought("", b"")]);
        assert_eq!(out, ConvertedParts::default());
        assert!(records.is_empty());
    }

    #[test]
    fn thought_signature_bytes_become_text_like_go_string() {
        let part = Part {
            thought: true,
            text: "t".to_string(),
            thought_signature: Bytes::from_static(b"\xe2\x82a\xff"),
            ..Default::default()
        };
        let (out, _) = convert(anthropic(), Role::Assistant, &[part]);
        assert_eq!(
            out.blocks,
            vec![ContentBlockParam::Thinking(ThinkingBlockParam::new(
                "\u{FFFD}\u{FFFD}a\u{FFFD}",
                "t"
            ))]
        );
    }

    #[test]
    fn order_assistant_blocks_moves_thinking_first_and_keeps_group_order() {
        let t1 = ContentBlockParam::Thinking(ThinkingBlockParam::new("s1", "a"));
        let r1 = ContentBlockParam::RedactedThinking(RedactedThinkingBlockParam::new("r"));
        let x1 = ContentBlockParam::text("x1");
        let x2 = ContentBlockParam::text("x2");
        assert_eq!(
            order_assistant_blocks(vec![x1.clone(), t1.clone(), x2.clone(), r1.clone()]),
            vec![t1.clone(), r1.clone(), x1.clone(), x2.clone()]
        );
        assert_eq!(
            order_assistant_blocks(vec![x2.clone(), x1.clone()]),
            vec![x2.clone(), x1.clone()]
        );
        assert_eq!(
            order_assistant_blocks(vec![r1.clone(), t1.clone()]),
            vec![r1, t1]
        );
        assert_eq!(order_assistant_blocks(Vec::new()), Vec::new());
    }

    #[test]
    fn is_content_free_signature_like_go() {
        let sig = |p: Part| Part {
            thought_signature: Bytes::from_static(b"s"),
            ..p
        };
        assert!(is_content_free_signature(&sig(Part::default())));
        assert!(!is_content_free_signature(&Part::default()));
        assert!(!is_content_free_signature(&sig(text("x"))));
        assert!(!is_content_free_signature(&sig(Part {
            thought: true,
            ..Default::default()
        })));
        assert!(!is_content_free_signature(&sig(call("a", "f", json!({})))));
        assert!(!is_content_free_signature(&sig(bytes_part(
            b"x",
            "image/png"
        ))));
        assert!(!is_content_free_signature(&sig(Part {
            tool_call: Some(ToolCall::default()),
            ..Default::default()
        })));
        // Go does not list VideoMetadata, so a signature with it alone is content-free.
        assert!(is_content_free_signature(&sig(Part {
            video_metadata: Some(VideoMetadata::default()),
            ..Default::default()
        })));
    }

    /// Go TestMultimodalSupport's table: `(name, part, anthropic sent, vertex sent)`.
    fn multimodal_cases() -> Vec<(&'static str, Part, bool, bool)> {
        vec![
            (
                "inline_image_png",
                bytes_part(b"\x89PNG", "image/png; charset=binary"),
                true,
                true,
            ),
            (
                "inline_image_jpeg",
                bytes_part(b"jpeg-bytes", "image/jpeg"),
                true,
                true,
            ),
            (
                "inline_image_gif",
                bytes_part(b"gif-bytes", "image/gif"),
                true,
                true,
            ),
            (
                "inline_image_webp",
                bytes_part(b"webp-bytes", "image/webp"),
                true,
                true,
            ),
            (
                "inline_pdf",
                bytes_part(b"%PDF-1.4", "application/pdf"),
                true,
                true,
            ),
            (
                "inline_plain_text",
                bytes_part(b"plain document text", "text/plain; charset=utf-8"),
                true,
                true,
            ),
            (
                "https_url_image",
                Part::from_uri("https://example.com/secret-image.png", "image/png"),
                true,
                false,
            ),
            (
                "https_url_pdf",
                Part::from_uri("https://example.com/secret-doc.pdf", "application/pdf"),
                true,
                false,
            ),
            (
                "https_url_plain_text_unsupported",
                Part::from_uri("https://example.com/notes.txt", "text/plain"),
                false,
                false,
            ),
            (
                "gs_uri_unsupported",
                Part::from_uri("gs://my-secret-bucket/doc.pdf", "application/pdf"),
                false,
                false,
            ),
            (
                "inline_audio_unsupported",
                bytes_part(b"SECRET_AUDIO_BYTES", "audio/mp3"),
                false,
                false,
            ),
            (
                "inline_video_unsupported",
                bytes_part(b"SECRET_VIDEO_BYTES", "video/mp4"),
                false,
                false,
            ),
            (
                "inline_octet_stream_unsupported",
                bytes_part(b"SECRET_BINARY", "application/octet-stream"),
                false,
                false,
            ),
        ]
    }

    #[test]
    fn multimodal_parts_per_backend() {
        for (name, part, anthropic_sent, vertex_sent) in multimodal_cases() {
            for (cx, want_sent) in [(anthropic(), anthropic_sent), (vertex(), vertex_sent)] {
                let case = format!("{name}_{}", cx.backend.as_str());
                let contents = [user(vec![part.clone(), text("Describe this.")])];
                let (msgs, records) = capture(|| build_messages(cx, &contents));
                let msgs = msgs.unwrap();
                let blocks = &msgs[0].content;
                if want_sent {
                    assert_eq!(blocks.len(), 2, "{case}");
                    assert!(records.is_empty(), "{case}: {records:?}");
                } else {
                    assert_eq!(blocks.len(), 1, "{case}");
                    assert_eq!(records.len(), 1, "{case}");
                    assert_eq!(records[0].level, tracing::Level::WARN, "{case}");
                    for (k, v) in &records[0].fields {
                        assert!(
                            !v.contains("example.com")
                                && !v.contains("my-secret-bucket")
                                && !v.contains("SECRET_"),
                            "{case}: field {k}={v:?} leaked sensitive data"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn supported_media_blocks_serialize_like_the_sdk() {
        let parts = [
            bytes_part(b"\x89PNG", "IMAGE/PNG; charset=binary"),
            bytes_part(b"%PDF-1.4", "application/pdf"),
            bytes_part(b"plain <doc>", "text/plain; charset=utf-8"),
            Part::from_uri(" https://example.com/a.png ", "image/png"),
            Part::from_uri("https://example.com/d.pdf", "application/pdf"),
        ];
        let (out, records) = convert(anthropic(), Role::User, &parts);
        assert!(records.is_empty());
        assert_eq!(
            json_of(&out.blocks),
            concat!(
                r#"[{"source":{"data":"iVBORw==","media_type":"image/png","type":"base64"},"type":"image"},"#,
                r#"{"source":{"data":"JVBERi0xLjQ=","media_type":"application/pdf","type":"base64"},"type":"document"},"#,
                r#"{"source":{"data":"plain \u003cdoc\u003e","media_type":"text/plain","type":"text"},"type":"document"},"#,
                r#"{"source":{"url":"https://example.com/a.png","type":"url"},"type":"image"},"#,
                r#"{"source":{"url":"https://example.com/d.pdf","type":"url"},"type":"document"}]"#
            )
        );
    }

    #[test]
    fn inline_and_file_data_drop_reasons_are_gos() {
        let cases = [
            (
                Role::Assistant,
                bytes_part(b"x", "image/png"),
                "inline_data (mime_type=image/png on assistant turn)",
                "image/png",
                "",
            ),
            (
                Role::Assistant,
                bytes_part(b"x", ""),
                "inline_data (mime_type=unspecified on assistant turn)",
                "",
                "",
            ),
            (
                Role::User,
                bytes_part(b"", "image/png"),
                "empty inline_data (mime_type=image/png)",
                "image/png",
                "",
            ),
            (
                Role::User,
                bytes_part(b"", ""),
                "empty inline_data (mime_type=unspecified)",
                "",
                "",
            ),
            (
                Role::User,
                bytes_part(b"x", "audio/wav"),
                "inline_data (mime_type=audio/wav)",
                "audio/wav",
                "",
            ),
            (
                Role::User,
                Part::from_uri("gs://bucket/doc.pdf", "application/pdf"),
                "file_data (uri_scheme=gs, mime_type=application/pdf)",
                "application/pdf",
                "gs",
            ),
            (
                Role::User,
                Part::from_uri("", ""),
                "file_data (uri_scheme=unspecified, mime_type=unspecified)",
                "",
                "",
            ),
            (
                Role::Assistant,
                Part::from_uri("https://example.com/a.png", "image/png"),
                "file_data (uri_scheme=https, mime_type=image/png)",
                "image/png",
                "https",
            ),
            (
                Role::User,
                Part::from_uri("https://", "image/png"),
                "file_data (uri_scheme=https, mime_type=image/png)",
                "image/png",
                "https",
            ),
        ];
        for (role, part, reason, mime_type, scheme) in cases {
            let (out, records) = convert(anthropic(), role, std::slice::from_ref(&part));
            assert!(out.blocks.is_empty(), "{reason}");
            assert_eq!(out.dropped_reasons, vec![reason.to_string()]);
            assert_eq!(records.len(), 1, "{reason}");
            let kind = if part.inline_data.is_some() {
                "inline_data"
            } else {
                "file_data"
            };
            assert_eq!(
                records[0].fields,
                vec![
                    ("backend".to_string(), "anthropic".to_string()),
                    ("model".to_string(), MODEL.to_string()),
                    ("role".to_string(), role.as_str().to_string()),
                    ("part_kind".to_string(), kind.to_string()),
                    ("mime_type".to_string(), mime_type.to_string()),
                    ("uri_scheme".to_string(), scheme.to_string()),
                ],
                "{reason}"
            );
        }
    }

    #[test]
    fn warn_skipped_part_logs_gos_message_and_fields() {
        let ((), records) = capture(|| {
            warn_skipped_part(vertex(), Role::User, "file_data", "application/pdf", "gs");
        });
        assert_eq!(
            records,
            vec![Record {
                level: tracing::Level::WARN,
                message: WARN_MSG.to_string(),
                fields: vec![
                    ("backend".to_string(), "vertex".to_string()),
                    ("model".to_string(), MODEL.to_string()),
                    ("role".to_string(), "user".to_string()),
                    ("part_kind".to_string(), "file_data".to_string()),
                    ("mime_type".to_string(), "application/pdf".to_string()),
                    ("uri_scheme".to_string(), "gs".to_string()),
                ],
            }]
        );
    }

    #[test]
    fn unsupported_server_side_and_media_metadata_parts_are_dropped() {
        let parts = [
            Part {
                tool_call: Some(ToolCall::default()),
                ..Default::default()
            },
            Part {
                tool_response: Some(ToolResponse::default()),
                ..Default::default()
            },
            Part {
                audio_transcription: Some(Transcription::default()),
                ..Default::default()
            },
            Part {
                video_metadata: Some(VideoMetadata::default()),
                ..Default::default()
            },
            Part {
                video_metadata: Some(VideoMetadata::default()),
                text: "caption".to_string(),
                ..Default::default()
            },
            Part {
                thought_signature: Bytes::from_static(b"foreign"),
                ..Default::default()
            },
            text(""),
        ];
        let (out, records) = convert(anthropic(), Role::User, &parts);
        assert_eq!(out.blocks, vec![ContentBlockParam::text("caption")]);
        assert_eq!(
            out.dropped_reasons,
            vec![
                "tool_call",
                "tool_response",
                "audio_transcription",
                "video_metadata"
            ]
        );
        assert!(out.had_empty_text);
        let kinds: Vec<&str> = records.iter().map(|r| r.field("part_kind")).collect();
        assert_eq!(
            kinds,
            vec![
                "tool_call",
                "tool_response",
                "audio_transcription",
                "video_metadata",
                "thought_signature"
            ]
        );
    }

    // Go TestExecutableCodeAndURISchemePrivacy (first request).
    #[test]
    fn executable_code_and_results_become_fenced_text() {
        let parts = [
            Part::from_uri(
                "reports/q3-acquisition-plan.pdf?next=https://x.example/y",
                "application/pdf",
            ),
            Part {
                executable_code: Some(ExecutableCode {
                    language: Some(Language::Python),
                    code: "print(1)".to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            Part {
                executable_code: Some(ExecutableCode {
                    language: Some(Language::Unspecified),
                    code: "echo hi".to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            Part {
                executable_code: Some(ExecutableCode {
                    language: None,
                    code: "x".to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            Part {
                executable_code: Some(ExecutableCode {
                    language: Some(Language::Unknown(" Rust ".to_string())),
                    code: "fn main() {}".to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            Part {
                code_execution_result: Some(CodeExecutionResult {
                    outcome: Some(Outcome::Ok),
                    output: "1".to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ];
        let (out, records) = convert(anthropic(), Role::User, &parts);
        assert_eq!(
            out.blocks,
            vec![
                ContentBlockParam::text("Code:\n```python\nprint(1)\n```"),
                ContentBlockParam::text("Code:\n```\necho hi\n```"),
                ContentBlockParam::text("Code:\n```\nx\n```"),
                ContentBlockParam::text("Code:\n```rust\nfn main() {}\n```"),
                ContentBlockParam::text("Execution Result:\n```code_output\n1\n```"),
            ]
        );
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].field("uri_scheme"), "unknown");
        for (k, v) in &records[0].fields {
            assert!(
                !v.contains("reports") && !v.contains("q3-acquisition"),
                "field {k}={v:?} leaked the file path"
            );
        }
    }

    // Go TestExecutableCodeAndURISchemePrivacy (malicious MIME types and URIs).
    #[test]
    fn malformed_mime_types_and_uris_are_sanitized_in_warnings_and_errors() {
        let contents = [user(vec![
            bytes_part(b"payload", "https://evil.example/x?token=s3cret\ninjected"),
            Part::from_uri(
                "my-secret-host.internal:8443/path?tok=s3cret",
                "application/pdf",
            ),
            Part::from_uri(
                "my-secret-host.internal:8443/path?tok=s3cret\ninjected",
                "application/pdf",
            ),
            Part::from_uri("db-host:5432/x?tok=s3cret", "application/pdf"),
        ])];
        let (result, records) = capture(|| build_messages(anthropic(), &contents));
        let err = result.unwrap_err();
        assert!(err.is(ErrorKind::NoContent) && err.is(ErrorKind::UnsupportedContent));
        let text = err.to_string();
        for leak in ["evil.example", "s3cret", "my-secret-host", "db-host", "\n"] {
            assert!(!text.contains(leak), "error {text:?} leaked {leak:?}");
        }
        assert_eq!(
            text,
            "anthropicmodel: request contains no sendable content: anthropicmodel: unsupported content: \
             newest message has no sendable content after skipping unsupported parts (\
             inline_data (mime_type=invalid), \
             file_data (uri_scheme=unknown, mime_type=application/pdf), \
             file_data (uri_scheme=unknown, mime_type=application/pdf), \
             file_data (uri_scheme=unknown, mime_type=application/pdf))"
        );
        assert_eq!(records.len(), 4);
        assert_eq!(records[0].field("mime_type"), "invalid");
        for r in &records[1..] {
            assert_eq!(r.field("uri_scheme"), "unknown");
        }
    }

    // Go TestToolsAndToolConfig/schema_normalization_and_parallel_tool_calls_and_id_sanitization (messages half).
    #[test]
    fn function_call_and_response_ids_are_sanitized_and_paired() {
        let contents = [
            Content::from_text("Weather and time?", ROLE_USER),
            model(vec![
                call("bad id#1", "get_weather", json!({"city": "Paris"})),
                call("", "get_time", json!({"tz": "UTC"})),
            ]),
            user(vec![
                response_part("bad id#1", "get_weather", json!({"result": "Sunny 22C"})),
                response_part(
                    "",
                    "get_time",
                    json!({"content": [{"type": "text", "text": "12:00 UTC"}]}),
                ),
            ]),
        ];
        let msgs = build_messages(anthropic(), &contents).unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(
            json_of(&msgs[1].content),
            r#"[{"id":"toolu_fallback_0","input":{"city":"Paris"},"name":"get_weather","type":"tool_use"},{"id":"toolu_fallback_1","input":{"tz":"UTC"},"name":"get_time","type":"tool_use"}]"#
        );
        assert_eq!(
            json_of(&msgs[2].content),
            r#"[{"tool_use_id":"toolu_fallback_0","is_error":false,"content":[{"text":"Sunny 22C","type":"text"}],"type":"tool_result"},{"tool_use_id":"toolu_fallback_1","is_error":false,"content":[{"text":"12:00 UTC","type":"text"}],"type":"tool_result"}]"#
        );
    }

    #[test]
    fn function_call_without_args_sends_an_empty_input_object() {
        let (out, _) = convert(
            anthropic(),
            Role::Assistant,
            &[call("toolu_a", "noop", json!({}))],
        );
        assert_eq!(
            json_of(&out.blocks),
            r#"[{"id":"toolu_a","input":{},"name":"noop","type":"tool_use"}]"#
        );
    }

    // Go TestFunctionResponseStructSerializationAndEmptyResult.
    #[test]
    fn function_response_struct_serialization_and_empty_result() {
        let contents = [
            Content::from_text("List items and ping", ROLE_USER),
            model(vec![
                call("toolu_items", "list_items", json!({})),
                call("toolu_empty", "noop", json!({})),
            ]),
            user(vec![
                response_part(
                    "toolu_items",
                    "list_items",
                    json!({"result": [{"name": "apple", "price": 3}, {"name": "pear", "price": 4}]}),
                ),
                response_part("toolu_empty", "noop", json!({})),
            ]),
        ];
        let msgs = build_messages(anthropic(), &contents).unwrap();
        let ContentBlockParam::ToolResult(items) = &msgs[2].content[0] else {
            panic!("want a tool_result, got {:?}", msgs[2].content[0]);
        };
        assert_eq!(
            items.content,
            vec![ToolResultContent::Text(TextBlockParam::new(
                r#"[{"name":"apple","price":3},{"name":"pear","price":4}]"#
            ))]
        );
        assert_eq!(
            json_of(&msgs[2].content[1]),
            r#"{"tool_use_id":"toolu_empty","is_error":false,"type":"tool_result"}"#
        );
    }

    // Go TestMultimodalSupport/function_response_media_parts.
    #[test]
    fn function_response_media_parts() {
        let fr = FunctionResponse {
            id: "toolu_chart".to_string(),
            name: "render_chart".to_string(),
            response: json!({"status": "done"}).as_object().cloned().unwrap(),
            parts: vec![
                FunctionResponsePart {
                    inline_data: Some(FunctionResponseBlob {
                        mime_type: "image/png".to_string(),
                        data: Bytes::from_static(b"\x89PNG"),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                FunctionResponsePart {
                    inline_data: Some(FunctionResponseBlob {
                        mime_type: "application/pdf".to_string(),
                        data: Bytes::from_static(b"%PDF-1.4"),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                FunctionResponsePart {
                    file_data: Some(FunctionResponseFileData {
                        mime_type: "image/png".to_string(),
                        file_uri: "https://example.com/chart.png".to_string(),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                FunctionResponsePart {
                    file_data: Some(FunctionResponseFileData {
                        mime_type: "application/pdf".to_string(),
                        file_uri: "gs://bucket/chart.pdf".to_string(),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                FunctionResponsePart {
                    inline_data: Some(FunctionResponseBlob {
                        mime_type: "audio/ogg".to_string(),
                        data: Bytes::from_static(b"audio"),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let contents = [
            Content::from_text("Render chart", ROLE_USER),
            model(vec![call("toolu_chart", "render_chart", json!({}))]),
            user(vec![Part {
                function_response: Some(fr),
                ..Default::default()
            }]),
        ];
        let (msgs, records) = capture(|| build_messages(anthropic(), &contents));
        let msgs = msgs.unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].field("part_kind"), "function_response_file_data");
        assert_eq!(records[0].field("uri_scheme"), "gs");
        assert_eq!(
            records[1].field("part_kind"),
            "function_response_inline_data"
        );
        assert_eq!(records[1].field("mime_type"), "audio/ogg");
        assert_eq!(
            json_of(&msgs[2].content[0]),
            concat!(
                r#"{"tool_use_id":"toolu_chart","is_error":false,"content":["#,
                r#"{"text":"{\"status\":\"done\"}","type":"text"},"#,
                r#"{"source":{"data":"iVBORw==","media_type":"image/png","type":"base64"},"type":"image"},"#,
                r#"{"source":{"data":"JVBERi0xLjQ=","media_type":"application/pdf","type":"base64"},"type":"document"},"#,
                r#"{"source":{"url":"https://example.com/chart.png","type":"url"},"type":"image"}"#,
                r#"],"type":"tool_result"}"#
            )
        );
    }

    #[test]
    fn function_response_media_rules() {
        let inline = |mime: &str, data: &'static [u8]| FunctionResponsePart {
            inline_data: Some(FunctionResponseBlob {
                mime_type: mime.to_string(),
                data: Bytes::from_static(data),
                ..Default::default()
            }),
            ..Default::default()
        };
        let file = |mime: &str, uri: &str| FunctionResponsePart {
            file_data: Some(FunctionResponseFileData {
                mime_type: mime.to_string(),
                file_uri: uri.to_string(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let parts = [
            FunctionResponsePart::default(),
            inline("text/plain", b"notes"),
            inline("image/png", b""),
            file("application/pdf", " https://example.com/r.pdf "),
            file("text/plain", "https://example.com/r.txt"),
        ];
        let (out, records) =
            capture(|| convert_function_response_media(anthropic(), Role::User, &parts));
        assert_eq!(
            json_of(&out),
            r#"[{"source":{"data":"notes","media_type":"text/plain","type":"text"},"type":"document"},{"source":{"url":"https://example.com/r.pdf","type":"url"},"type":"document"}]"#
        );
        let kinds: Vec<(&str, &str)> = records
            .iter()
            .map(|r| (r.field("part_kind"), r.field("mime_type")))
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("function_response_inline_data", "image/png"),
                ("function_response_file_data", "text/plain"),
            ]
        );

        // File data needs a user turn on the Anthropic backend.
        let urls = [file("image/png", "https://example.com/a.png")];
        let (out, records) =
            capture(|| convert_function_response_media(vertex(), Role::User, &urls));
        assert!(out.is_empty());
        assert_eq!(records.len(), 1);
        let (out, records) =
            capture(|| convert_function_response_media(anthropic(), Role::Assistant, &urls));
        assert!(out.is_empty());
        assert_eq!(records[0].field("role"), "assistant");
    }

    #[test]
    fn serialize_function_response_data_like_go() {
        let obj = |v: Value| v.as_object().cloned().unwrap();
        let cases: Vec<(Value, &str)> = vec![
            (json!({}), ""),
            (
                json!({"content": [{"type": "text", "text": "a"}, {"type": "image", "url": "u"}, 5, null, "s", {"type": "text", "text": 1}]}),
                "a\n{\"type\":\"image\",\"url\":\"u\"}\n5\n\ns\n{\"text\":1,\"type\":\"text\"}",
            ),
            (json!({"content": "plain", "result": "ignored"}), "plain"),
            (json!({"content": ""}), r#"{"content":""}"#),
            (json!({"content": []}), r#"{"content":[]}"#),
            (json!({"content": null}), r#"{"content":null}"#),
            (json!({"content": 7}), r#"{"content":7}"#),
            (json!({"result": "Sunny 22C"}), "Sunny 22C"),
            (json!({"result": 42}), "42"),
            (json!({"result": 3.0}), "3"),
            (
                json!({"result": {"b": 1, "a": "<x>"}}),
                r#"{"a":"\u003cx\u003e","b":1}"#,
            ),
            (json!({"result": null}), r#"{"result":null}"#),
            (
                json!({"result": "x", "other": 1}),
                r#"{"other":1,"result":"x"}"#,
            ),
            (json!({"status": "done"}), r#"{"status":"done"}"#),
        ];
        for (resp, want) in cases {
            assert_eq!(
                serialize_function_response_data(&obj(resp.clone())),
                want,
                "{resp}"
            );
        }
    }

    #[test]
    fn format_arbitrary_value_like_go() {
        assert_eq!(format_arbitrary_value(&Value::Null), "");
        assert_eq!(format_arbitrary_value(&json!("s & <t>")), "s & <t>");
        assert_eq!(format_arbitrary_value(&json!(true)), "true");
        assert_eq!(format_arbitrary_value(&json!(1e21)), "1e+21");
        assert_eq!(format_arbitrary_value(&json!(1.5)), "1.5");
        assert_eq!(
            format_arbitrary_value(&json!([1.5, null, true])),
            "[1.5,null,true]"
        );
        assert_eq!(
            format_arbitrary_value(&json!({"b": 1, "a": "<x>"})),
            r#"{"a":"\u003cx\u003e","b":1}"#
        );
    }

    #[test]
    fn normalize_mime_type_matches_go() {
        // Expected values printed by Go 1.27 running request.go's normalizeMIMEType.
        let long_ok = format!("image/{}", "a".repeat(64));
        let long_bad = format!("image/{}", "a".repeat(65));
        let cases: Vec<(&str, &str)> = vec![
            ("", ""),
            ("  ", ""),
            ("image/png", "image/png"),
            ("IMAGE/PNG", "image/png"),
            ("image/png; charset=binary", "image/png"),
            (" text/plain ; charset=utf-8", "text/plain"),
            ("application/octet-stream", "application/octet-stream"),
            ("x-custom/thing", "x-custom/thing"),
            ("x-/a", "invalid"),
            ("foo/bar", "invalid"),
            ("image/", "invalid"),
            ("image/-png", "invalid"),
            ("image/p\u{130}ng", "image/ping"),
            ("image/png\n", "image/png"),
            ("https://evil.example/x?token=s3cret\ninjected", "invalid"),
            ("\u{a0}image/gif\u{a0}", "image/gif"),
            ("\u{2028}image/gif", "image/gif"),
            ("image/svg+xml", "image/svg+xml"),
            ("Image/WebP;q=1", "image/webp"),
            (&long_ok, &long_ok),
            (&long_bad, "invalid"),
        ];
        for (raw, want) in cases {
            assert_eq!(normalize_mime_type(raw), want, "{raw:?}");
        }
    }

    #[test]
    fn is_supported_image_mime_lists_gos_four() {
        for m in ["image/jpeg", "image/png", "image/gif", "image/webp"] {
            assert!(is_supported_image_mime(m), "{m}");
        }
        for m in [
            "image/svg+xml",
            "IMAGE/PNG",
            "image/jpg",
            "",
            "application/pdf",
        ] {
            assert!(!is_supported_image_mime(m), "{m}");
        }
    }

    #[test]
    fn extract_uri_scheme_matches_go() {
        // Expected values printed by Go 1.27 running request.go's extractURIScheme.
        let cases = [
            ("", ""),
            ("   ", ""),
            ("https://example.com/a.png", "https"),
            ("HTTPS://Example.com", "https"),
            ("gs://bucket/x", "gs"),
            (
                "reports/q3-acquisition-plan.pdf?next=https://x.example/y",
                "unknown",
            ),
            ("my-secret-host.internal:8443/path?tok=s3cret", "unknown"),
            ("db-host:5432/x?tok=s3cret", "unknown"),
            ("data:image/png;base64,xx", "data"),
            ("blob:https://a", "blob"),
            ("urn:isbn:1", "urn"),
            ("mailto:a@b", "mailto"),
            ("file:/etc/passwd", "unknown"),
            ("file:///etc/passwd", "file"),
            ("1http://x", "unknown"),
            (":foo", "unknown"),
            ("a:", "unknown"),
            ("h\u{130}tps://x", "hitps"),
            ("abcdefghijklmnop://x", "abcdefghijklmnop"),
            ("abcdefghijklmnopq://x", "unknown"),
            (" https://x.com ", "https"),
            ("s3://b/k", "s3"),
        ];
        for (raw, want) in cases {
            assert_eq!(extract_uri_scheme(raw), want, "{raw:?}");
        }
    }

    #[test]
    fn is_valid_https_url_matches_go_url_parse() {
        // Expected values printed by Go 1.27 (go.mod `go 1.26`) running request.go's
        // isValidHTTPSURL.
        let cases = [
            ("https://example.com/a.png", true),
            ("HTTPS://example.com", true),
            ("https://", false),
            ("https:///path", false),
            ("https://:443", true),
            ("https://user@", false),
            ("https://user:pw@example.com/x", true),
            ("https://us er@example.com", false),
            ("https://example.com:abc/", false),
            ("https://example.com:443:1/", false),
            ("https://exa mple.com/", false),
            ("https://exa{mple}.com", false),
            ("https://example.com%41/", false),
            ("https://example.com%C3%A4/", true),
            ("https://example.com/a b", true),
            ("https://example.com/%zz", false),
            ("https://example.com/?%zz", true),
            ("https://example.com/#%zz", false),
            ("https://example.com/#\n", true),
            ("https://example.com/\n", false),
            ("https://[::1]/", true),
            ("https://[::1]:8080/x", true),
            ("https://[::1", false),
            ("https://[1.2.3.4]/", false),
            ("https://[fe80::1%25en0]/", true),
            ("https://[fe80::1%25]/", false),
            ("https://x[::1]/", false),
            ("https://[::1]x/", false),
            ("https://ex\u{e4}mple.com/", true),
            ("https://a.com?", true),
            ("https://a.com?x=1?y", true),
            ("http://example.com", false),
            ("https:example.com", false),
            ("https://a_b.com", true),
            ("https://a<b>.com", true),
            ("https://a%25b.com", true),
            ("https://[::ffff:1.2.3.4]/", true),
            ("https://[v1.x]/", false),
            ("https://a.com@b.com", true),
            ("https://a@b@c.com", true),
            ("https://a%zz@b.com", false),
            ("https://[::1]:/", true),
            ("https://example.com:/", true),
            ("https://example.com:", true),
            ("https://a.com\u{7f}", false),
        ];
        for (raw, want) in cases {
            assert_eq!(is_valid_https_url(raw), want, "{raw:?}");
        }
    }

    #[test]
    fn go_string_replaces_each_invalid_byte() {
        assert_eq!(go_string(b"ok"), "ok");
        assert_eq!(go_string(b"\xe2\x82a\xff"), "\u{FFFD}\u{FFFD}a\u{FFFD}");
        assert_eq!(go_string("\u{e4}".as_bytes()), "\u{e4}");
    }
}
