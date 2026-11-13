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

//! Port of ADK-Go `plugin/agentanalytics/json_formatter.go`'s content summaries:
//! `summaryPiece`, `messageSummary` (`newMessageSummary`, `newResponseMessageSummary`,
//! `render`), `contentSummaryBuilder` (`omitTextFromSummary`, `retainInlineSummary`,
//! `hasFailedUploadTruncated`, `buildContent`, `applyToRow`), `contentSummary` and
//! `formatResponseContent`.

use gadk_genai::Content;
use serde_json::{Map, Value};

use super::parts::{language_text, or_unknown, outcome_text, sanitize_and_truncate_text};
use super::sanitize::truncate_string;
use super::{ContentSummaryBuilder, MessageSummary, SummaryPiece, UNSUPPORTED_OBJ_PLACEHOLDER};
use crate::redaction::COLUMN_CONTENT;

/// Go `contentSummary(content, maxLength, redact)`: what adk-python writes to
/// `content.text_summary` — the text parts, each truncated, and descriptions of function
/// responses and code parts, joined by `" | "` and truncated again. Function calls and inline
/// or file data are left out.
pub(crate) fn content_summary(
    content: Option<&Content>,
    max_length: i64,
    redact: bool,
) -> (String, bool) {
    MessageSummary::new(content, 0, max_length, redact).render(max_length)
}

/// Go `formatResponseContent(content, maxLength, redact)`: model output the way adk-python
/// writes `content.response` — one entry per part joined by `" | "`, or `"None"` if there are
/// no parts.
pub(crate) fn format_response_content(
    content: Option<&Content>,
    max_length: i64,
    redact: bool,
) -> (String, bool) {
    MessageSummary::new_response(content, 0, max_length, redact).render(max_length)
}

impl MessageSummary {
    /// Go `newMessageSummary(content, contentOrd, maxLength, redact)`.
    pub(crate) fn new(
        content: Option<&Content>,
        content_ord: usize,
        max_length: i64,
        redact: bool,
    ) -> MessageSummary {
        let mut m = MessageSummary {
            content_ord,
            ..MessageSummary::default()
        };
        let Some(content) = content else {
            return m;
        };
        for (i, part) in content.parts.iter().enumerate() {
            if part.file_data.is_some()
                || part.inline_data.is_some()
                || part.function_call.is_some()
            {
                continue;
            }
            let piece = |text: String, truncated: bool, is_text: bool| SummaryPiece {
                part_idx: i,
                is_text,
                text,
                truncated,
                omitted: false,
            };
            if !part.text.is_empty() {
                let (shown, t) = sanitize_and_truncate_text(&part.text, max_length, redact);
                m.pieces.push(piece(shown, t, true));
            } else if let Some(resp) = &part.function_response {
                let (name, t) =
                    sanitize_and_truncate_text(or_unknown(&resp.name), max_length, redact);
                m.pieces
                    .push(piece(format!("Function response: {name}"), t, false));
            } else if let Some(code) = &part.executable_code {
                let (text, t) = sanitize_and_truncate_text(&code.code, max_length, redact);
                let lang = language_text(code);
                m.pieces.push(piece(
                    format!("Executable code ({}): {text}", or_unknown(&lang)),
                    t,
                    false,
                ));
            } else if let Some(result) = &part.code_execution_result {
                let (output, t) = sanitize_and_truncate_text(&result.output, max_length, redact);
                let outcome = outcome_text(result);
                m.pieces.push(piece(
                    format!("Code execution result ({}): {output}", or_unknown(&outcome)),
                    t,
                    false,
                ));
            }
        }
        m
    }

    /// Go `newResponseMessageSummary(content, contentOrd, maxLength, redact)`.
    pub(crate) fn new_response(
        content: Option<&Content>,
        content_ord: usize,
        max_length: i64,
        redact: bool,
    ) -> MessageSummary {
        let mut m = MessageSummary {
            content_ord,
            is_response: true,
            ..MessageSummary::default()
        };
        let Some(content) = content else {
            return m;
        };
        for (i, part) in content.parts.iter().enumerate() {
            let mut t = false;
            let mut is_text = false;
            let piece = if !part.text.is_empty() {
                let text;
                (text, t) = sanitize_and_truncate_text(&part.text, max_length, redact);
                is_text = true;
                format!("text: '{text}'")
            } else if let Some(call) = &part.function_call {
                format!("call: {}", call.name)
            } else if let Some(resp) = &part.function_response {
                format!("resp: {}", resp.name)
            } else if let Some(code) = &part.executable_code {
                let text;
                (text, t) = sanitize_and_truncate_text(&code.code, max_length, redact);
                format!(
                    "Executable code ({}): {text}",
                    or_unknown(&language_text(code))
                )
            } else if let Some(result) = &part.code_execution_result {
                let output;
                (output, t) = sanitize_and_truncate_text(&result.output, max_length, redact);
                format!(
                    "Code execution result ({}): {output}",
                    or_unknown(&outcome_text(result))
                )
            } else {
                "other".to_owned()
            };
            m.pieces.push(SummaryPiece {
                part_idx: i,
                is_text,
                text: piece,
                truncated: t,
                omitted: false,
            });
        }
        m
    }

    /// Go `render(maxLength)`.
    pub(crate) fn render(&self, max_length: i64) -> (String, bool) {
        if self.pieces.is_empty() {
            let empty = if self.is_response { "None" } else { "" };
            return (empty.to_owned(), false);
        }
        let mut active = Vec::with_capacity(self.pieces.len());
        let mut truncated = false;
        for p in self.pieces.iter().filter(|p| !p.omitted) {
            active.push(p.text.as_str());
            truncated = truncated || p.truncated;
        }
        let joined = active.join(" | ");
        if self.is_response {
            return (joined, truncated);
        }
        let (summary, t) = truncate_string(&joined, max_length);
        (summary, truncated || t)
    }
}

impl ContentSummaryBuilder {
    /// Go `omitTextFromSummary(contentOrd, partIdx)`: a text part whose upload succeeded leaves
    /// the summary.
    pub(crate) fn omit_text_from_summary(&mut self, content_ord: usize, part_idx: usize) {
        let mut rebuild = false;
        for m in self
            .messages
            .iter_mut()
            .filter(|m| m.content_ord == content_ord)
        {
            for p in m
                .pieces
                .iter_mut()
                .filter(|p| p.part_idx == part_idx && p.is_text)
            {
                if !p.omitted || p.truncated {
                    p.omitted = true;
                    p.truncated = false;
                    rebuild = true;
                }
            }
        }
        self.needs_rebuild = self.needs_rebuild || rebuild;
    }

    /// Go `retainInlineSummary(contentOrd, partIdx, fallbackText, fallbackTruncated)`: a text
    /// part whose upload failed keeps its inline fallback in the summary.
    pub(crate) fn retain_inline_summary(
        &mut self,
        content_ord: usize,
        part_idx: usize,
        fallback_text: &str,
        fallback_truncated: bool,
    ) {
        let mut rebuild = false;
        for m in self
            .messages
            .iter_mut()
            .filter(|m| m.content_ord == content_ord)
        {
            let is_response = m.is_response;
            for p in m
                .pieces
                .iter_mut()
                .filter(|p| p.part_idx == part_idx && p.is_text)
            {
                rebuild = true;
                p.omitted = false;
                p.text = if is_response {
                    format!("text: '{fallback_text}'")
                } else {
                    fallback_text.to_owned()
                };
                p.truncated = fallback_truncated;
            }
        }
        self.needs_rebuild = self.needs_rebuild || rebuild;
    }

    /// Go `hasFailedUploadTruncated()`.
    pub(crate) fn has_failed_upload_truncated(&self) -> bool {
        self.messages
            .iter()
            .flat_map(|m| &m.pieces)
            .any(|p| !p.omitted && p.truncated)
    }

    /// Go `buildContent()`.
    pub(crate) fn build_content(&self) -> (Map<String, Value>, bool) {
        let mut content = Map::new();
        if self.is_llm_response {
            let mut truncated = false;
            if let Some(m) = self.messages.first() {
                let response;
                (response, truncated) = m.render(self.max_len);
                content.insert("response".to_owned(), Value::from(response));
            }
            if let Some(usage) = &self.usage {
                content.insert("usage".to_owned(), Value::Object(usage.clone()));
            }
            return (content, truncated);
        }
        if !self.is_llm_request {
            let (summary, t) = self
                .messages
                .first()
                .map_or((String::new(), false), |m| m.render(self.max_len));
            content.insert("text_summary".to_owned(), Value::from(summary));
            return (content, t);
        }
        let mut truncated = self.role_truncated;
        let mut prompt = Vec::new();
        for m in &self.messages {
            let (summary, t) = m.render(self.max_len);
            truncated = truncated || t;
            if m.is_system {
                content.insert("system_prompt".to_owned(), Value::from(summary));
            } else if m.has_role {
                let mut entry = Map::new();
                entry.insert("role".to_owned(), Value::from(m.role.as_str()));
                entry.insert("content".to_owned(), Value::from(summary));
                prompt.push(Value::Object(entry));
            }
        }
        if !prompt.is_empty() {
            content.insert("prompt".to_owned(), Value::Array(prompt));
        }
        (content, truncated)
    }

    /// Go `applyToRow(row)`: rewrites `content` (Go `json.Marshal` text) and `is_truncated` of
    /// `row` when the summary needs a rebuild.
    pub(crate) fn apply_to_row(&self, row: &mut Map<String, Value>) {
        if !self.needs_rebuild {
            return;
        }
        let mut summary_truncated = false;
        let mut json_truncated = false;
        if !self.content_denied {
            let (content, st) = self.build_content();
            summary_truncated = st;
            let encoded = gadk_model::to_go_json(&Value::Object(content))
                .ok()
                .and_then(|b| String::from_utf8(b).ok());
            match encoded {
                Some(text) => {
                    row.insert(COLUMN_CONTENT.to_owned(), Value::from(text));
                }
                None => {
                    row.insert(
                        COLUMN_CONTENT.to_owned(),
                        Value::from(format!("\"{UNSUPPORTED_OBJ_PLACEHOLDER}\"")),
                    );
                    json_truncated = true;
                }
            }
        }
        row.insert(
            "is_truncated".to_owned(),
            Value::Bool(
                self.other_truncated
                    || summary_truncated
                    || json_truncated
                    || self.has_failed_upload_truncated(),
            ),
        );
    }
}
