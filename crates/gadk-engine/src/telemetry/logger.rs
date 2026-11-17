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

//! The OTel log records of the legacy (1.36) schema: `gen_ai.system.message`, `gen_ai.user.message`
//! and `gen_ai.choice`, emitted as `tracing` events through
//! [`emit_log_record`](super::emit_log_record) under the `generate_content` span, at `INFO` level.
//! The environment rules are in [`env`](super::env), the default schema's inference-details record
//! in [`semconv`](super::semconv).

use gadk_genai::Content;
use gadk_model::{Backend, LlmRequest, LlmResponse};
use serde_json::{Map, Value, json};

use super::env::{capture_message_content, use_legacy_schema};
use super::{emit_log_record, gen_ai_system};

/// What a record carries in place of message content it may not capture.
pub(crate) const ELIDED_CONTENT: &str = "<elided>";

/// The semconv v1.36 `gen_ai.system` attribute key.
pub(crate) const GEN_AI_SYSTEM: &str = "gen_ai.system";

/// Logs the request: the system message, then one `gen_ai.user.message` record per request content
/// (whatever its role), under `span`. Legacy schema only.
pub(crate) fn log_request(span: &tracing::Span, req: &LlmRequest, backend: Option<Backend>) {
    if !use_legacy_schema() {
        return;
    }
    let attrs = system_attributes(backend);
    log_system_message(span, req, &attrs);
    for content in &req.contents {
        log_user_message(span, content, &attrs);
    }
}

/// Logs the response: the `gen_ai.choice` record of the inference result, under `span`. Body
/// `index` (always 0: ADK keeps one candidate), `content`, and `finish_reason` when the response
/// reports one. Legacy schema only.
///
/// Unlike semconv, `content` sits directly in the body (not under `message`) and `tool_calls` is
/// omitted, for consistency with ADK Python.
pub(crate) fn log_response(
    span: &tracing::Span,
    resp: Option<&LlmResponse>,
    backend: Option<Backend>,
) {
    if !use_legacy_schema() {
        return;
    }
    let finish_reason = resp
        .and_then(|r| r.finish_reason.as_ref())
        .map_or("", |f| f.as_str());
    let content = resp.and_then(|r| r.content.as_ref());
    let mut body = Map::new();
    body.insert("index".to_owned(), json!(0));
    body.insert("content".to_owned(), content_to_json_like_value(content));
    if !finish_reason.is_empty() {
        body.insert("finish_reason".to_owned(), json!(finish_reason));
    }
    emit_log_record(
        span,
        tracing::Level::INFO,
        "gen_ai.choice",
        &Value::Object(body),
        &system_attributes(backend),
    );
}

/// Logs the system message: body `content` only (semconv's `role` is omitted, as in ADK Python).
fn log_system_message(span: &tracing::Span, req: &LlmRequest, attrs: &[(String, Value)]) {
    emit_log_record(
        span,
        tracing::Level::INFO,
        "gen_ai.system.message",
        &json!({ "content": extract_system_message(req) }),
        attrs,
    );
}

/// Logs one user message: body `content`, the whole content (its role, when set, is inside it).
fn log_user_message(span: &tracing::Span, content: &Content, attrs: &[(String, Value)]) {
    emit_log_record(
        span,
        tracing::Level::INFO,
        "gen_ai.user.message",
        &json!({ "content": content_to_json_like_value(Some(content)) }),
        attrs,
    );
}

/// The record's attributes: `gen_ai.system` for a backend that can be named, none otherwise.
fn system_attributes(backend: Option<Backend>) -> Vec<(String, Value)> {
    gen_ai_system(backend)
        .map(|s| vec![(GEN_AI_SYSTEM.to_owned(), json!(s))])
        .unwrap_or_default()
}

/// The system message value: [`ELIDED_CONTENT`] when content may not be captured, `null` without a
/// system instruction, else its non-empty text parts joined by newlines.
pub(crate) fn extract_system_message(req: &LlmRequest) -> Value {
    if !capture_message_content() {
        return json!(ELIDED_CONTENT);
    }
    let Some(si) = req
        .config
        .as_ref()
        .and_then(|c| c.system_instruction.as_ref())
    else {
        return Value::Null;
    };
    let text: Vec<&str> = si
        .parts
        .iter()
        .filter(|p| !p.text.is_empty())
        .map(|p| p.text.as_str())
        .collect();
    Value::String(text.join("\n"))
}

/// The log value of a content: [`ELIDED_CONTENT`] when content may not be captured, `null` for no
/// content, else the content's JSON form (gadk-genai's serde for `Content`; `<not_serializable>` if
/// that fails).
pub(crate) fn content_to_json_like_value(c: Option<&Content>) -> Value {
    if !capture_message_content() {
        return json!(ELIDED_CONTENT);
    }
    let Some(c) = c else { return Value::Null };
    serde_json::to_value(c).unwrap_or_else(|_| json!("<not_serializable>"))
}
