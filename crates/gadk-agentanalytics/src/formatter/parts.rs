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

//! Port of ADK-Go `plugin/agentanalytics/json_formatter.go`'s `content_parts` builder:
//! `FormatContentParts`, `formatContentParts`, `partsCollector` (`reserve`, `hasPendingText`,
//! `addContent`, `addPart`, `queue`, `addText`, `addInlineData`, `addFileData`,
//! `addStructured`), `normalizeFunctionResponse`, `formatPartAttributes`,
//! `sanitizeAndTruncateText` and `orUnknown`.

use std::collections::BTreeMap;

use gadk_genai::{Blob, Content, FileData, FunctionResponse, Part};
use serde::Serialize;
use serde::ser::{SerializeMap, Serializer};
use serde_json::{Map, Value};

use super::sanitize::{smart_truncate_bytes, truncate_string};
use super::{
    DEFAULT_BINARY_MIME_TYPE, GoAny, INLINE_DATA_TEXT, PartsCollector, PartsOptions, PendingUpload,
    STORAGE_MODE_EXTERNAL_URI, STORAGE_MODE_INLINE, UNSUPPORTED_OBJ_PLACEHOLDER,
    UPLOAD_FAILED_TEXT, object_ref,
};
use crate::redaction::{sanitize_raw_text, sanitize_uri};

/// Go `FormatContentParts(content, maxLength)`: the `content_parts` of `content` with redaction
/// on (`max_length` < 0: no limit).
pub fn format_content_parts(content: Option<&Content>, max_length: i64) -> Vec<Map<String, Value>> {
    format_content_parts_with(content, max_length, true).0
}

/// Go `formatContentParts(content, maxLength, redact)`: also reports whether any part lost
/// content to truncation or URI redaction.
pub(crate) fn format_content_parts_with(
    content: Option<&Content>,
    max_length: i64,
    redact: bool,
) -> (Vec<Map<String, Value>>, bool) {
    let mut c = PartsCollector::new(PartsOptions {
        max_len: max_length,
        redact,
        ..PartsOptions::default()
    });
    c.add_content(content, 0);
    (c.parts, c.truncated)
}

/// Go `formatPartAttributes(v, maxLength, redact)`: [`super::smart_truncate_with`] as a
/// string (Go's `"\"[UNSUPPORTED_OBJECT]\""` on a marshal error cannot happen here).
pub(crate) fn format_part_attributes<T: Serialize + ?Sized>(
    v: &T,
    max_length: i64,
    redact: bool,
) -> (String, bool) {
    let (b, truncated) = smart_truncate_bytes(v, max_length, redact);
    match String::from_utf8(b) {
        Ok(s) => (s, truncated),
        Err(_) => (format!("\"{UNSUPPORTED_OBJ_PLACEHOLDER}\""), true),
    }
}

/// Go `sanitizeAndTruncateText(raw, maxLength, redact)`.
pub(crate) fn sanitize_and_truncate_text(
    raw: &str,
    max_length: i64,
    redact: bool,
) -> (String, bool) {
    let mut content_lost = false;
    let mut text = raw.to_owned();
    if redact {
        (text, content_lost) = sanitize_raw_text(raw);
    }
    let (shown, length_truncated) = truncate_string(&text, max_length);
    (shown, content_lost || length_truncated)
}

/// Go `orUnknown(s)`: `"unknown"` for an empty string.
pub(crate) fn or_unknown(s: &str) -> &str {
    if s.is_empty() { "unknown" } else { s }
}

/// A Go `map[string]any` literal of one or two entries (Go walks a map, not a struct).
struct Entries<'a, A: ?Sized, B: ?Sized> {
    first: (&'static str, &'a A),
    second: Option<(&'static str, &'a B)>,
}

impl<A: Serialize + ?Sized, B: Serialize + ?Sized> Serialize for Entries<'_, A, B> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(1 + usize::from(self.second.is_some())))?;
        m.serialize_entry(self.first.0, self.first.1)?;
        if let Some((k, v)) = self.second {
            m.serialize_entry(k, v)?;
        }
        m.end()
    }
}

/// `map[string]any{key: value}`.
fn one_entry<'a, T: Serialize + ?Sized>(key: &'static str, value: &'a T) -> Entries<'a, T, ()> {
    Entries {
        first: (key, value),
        second: None,
    }
}

/// Go `normalizeFunctionResponse(r)`: the SDK-owned fields of `r` and its media parts under
/// adk-python's snake_case names; caller-defined keys inside `response` are untouched, and
/// inline data stays bytes ([`GoAny::Bytes`]) so the sanitizer redacts and truncates UTF-8 data
/// as text and replaces binary data with a placeholder.
pub(crate) fn normalize_function_response(r: &FunctionResponse) -> BTreeMap<String, GoAny> {
    let mut out = BTreeMap::new();
    let text = |s: &str| GoAny::Json(Value::from(s));
    if !r.id.is_empty() {
        out.insert("id".to_owned(), text(&r.id));
    }
    if !r.name.is_empty() {
        out.insert("name".to_owned(), text(&r.name));
    }
    if !r.response.is_empty() {
        out.insert(
            "response".to_owned(),
            GoAny::Json(Value::Object(r.response.clone())),
        );
    }
    if let Some(will_continue) = r.will_continue {
        out.insert(
            "will_continue".to_owned(),
            GoAny::Json(Value::Bool(will_continue)),
        );
    }
    if let Some(scheduling) = r.scheduling.as_ref().filter(|s| !s.as_str().is_empty()) {
        out.insert("scheduling".to_owned(), text(scheduling.as_str()));
    }
    if !r.parts.is_empty() {
        let mut parts = Vec::with_capacity(r.parts.len());
        for p in &r.parts {
            let mut part_map = BTreeMap::new();
            if let Some(b) = &p.inline_data {
                let mut blob_map = BTreeMap::new();
                if !b.mime_type.is_empty() {
                    blob_map.insert("mime_type".to_owned(), text(&b.mime_type));
                }
                if !b.data.is_empty() {
                    // Not base64: that would hide the data from the sanitizer.
                    blob_map.insert("data".to_owned(), GoAny::Bytes(b.data.clone()));
                }
                if !b.display_name.is_empty() {
                    blob_map.insert("display_name".to_owned(), text(&b.display_name));
                }
                part_map.insert("inline_data".to_owned(), GoAny::Map(blob_map));
            }
            if let Some(f) = &p.file_data {
                let mut file_map = BTreeMap::new();
                if !f.file_uri.is_empty() {
                    file_map.insert("file_uri".to_owned(), text(&f.file_uri));
                }
                if !f.mime_type.is_empty() {
                    file_map.insert("mime_type".to_owned(), text(&f.mime_type));
                }
                if !f.display_name.is_empty() {
                    file_map.insert("display_name".to_owned(), text(&f.display_name));
                }
                part_map.insert("file_data".to_owned(), GoAny::Map(file_map));
            }
            parts.push(GoAny::Map(part_map));
        }
        out.insert("parts".to_owned(), GoAny::List(parts));
    }
    out
}

/// The wire string of a genai enum field (`""` when unset), as Go's `string(...)` of it.
fn enum_text<T: Serialize>(v: Option<&T>) -> String {
    v.and_then(|v| serde_json::to_value(v).ok())
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

impl PartsCollector {
    /// Go `reserve(n)`: `(reserved, ok)`.
    pub(crate) fn reserve(&self, n: i64) -> (i64, bool) {
        match &self.opts.reserve_bytes {
            None => (0, true),
            Some(reserve) if reserve(n) => (n, true),
            Some(_) => (0, false),
        }
    }

    /// Go `hasPendingText()`: whether any queued upload is a text part.
    pub(crate) fn has_pending_text(&self) -> bool {
        self.pending.iter().any(|u| u.is_text)
    }

    /// Go `addContent(content, contentOrd)`: appends the parts of `content`. `part_index` counts
    /// the parts of one message; `content_ord`, the position of the message in its request,
    /// tells apart the parts of different messages in object names.
    pub(crate) fn add_content(&mut self, content: Option<&Content>, content_ord: usize) {
        let Some(content) = content else {
            return;
        };
        for (i, part) in content.parts.iter().enumerate() {
            let mut entry = Map::new();
            entry.insert("part_index".to_owned(), Value::from(i));
            entry.insert("storage_mode".to_owned(), Value::from(STORAGE_MODE_INLINE));
            self.add_part(&mut entry, part, content_ord, i);
            self.parts.push(entry);
        }
    }

    /// Go `addPart(entry, part, contentOrd, index)`.
    fn add_part(
        &mut self,
        entry: &mut Map<String, Value>,
        part: &Part,
        content_ord: usize,
        index: usize,
    ) {
        let (max_len, redact) = (self.opts.max_len, self.opts.redact);
        if !part.text.is_empty() {
            self.add_text(entry, &part.text, content_ord, index);
        } else if let Some(blob) = &part.inline_data {
            self.add_inline_data(entry, blob, content_ord, index);
        } else if let Some(file) = &part.file_data {
            self.add_file_data(entry, file);
        } else if let Some(call) = &part.function_call {
            let (name, name_trunc) =
                sanitize_and_truncate_text(or_unknown(&call.name), max_len, redact);
            let attrs_value = Entries {
                first: ("function_name", call.name.as_str()),
                second: Some(("function_call", call)),
            };
            let (attrs, attr_trunc) = format_part_attributes(&attrs_value, max_len, redact);
            entry.insert("mime_type".to_owned(), Value::from("application/json"));
            entry.insert("text".to_owned(), Value::from(format!("Function: {name}")));
            entry.insert("part_attributes".to_owned(), Value::from(attrs));
            self.truncated = self.truncated || name_trunc || attr_trunc;
            self.non_upload_truncated = self.non_upload_truncated || name_trunc || attr_trunc;
        } else if let Some(resp) = &part.function_response {
            let (name, name_trunc) =
                sanitize_and_truncate_text(or_unknown(&resp.name), max_len, redact);
            let normalized = normalize_function_response(resp);
            let (attrs, attr_trunc) = format_part_attributes(
                &one_entry("function_response", &normalized),
                max_len,
                redact,
            );
            entry.insert("mime_type".to_owned(), Value::from("application/json"));
            entry.insert(
                "text".to_owned(),
                Value::from(format!("Function response: {name}")),
            );
            entry.insert("part_attributes".to_owned(), Value::from(attrs));
            self.truncated = self.truncated || name_trunc || attr_trunc;
            self.non_upload_truncated = self.non_upload_truncated || name_trunc || attr_trunc;
        } else if let Some(code) = &part.executable_code {
            self.add_structured(entry, "text/plain", &code.code, "executable_code", code);
        } else if let Some(result) = &part.code_execution_result {
            self.add_structured(
                entry,
                "text/plain",
                &result.output,
                "code_execution_result",
                result,
            );
        } else {
            entry.insert("mime_type".to_owned(), Value::from("text/plain"));
            entry.insert("part_attributes".to_owned(), Value::from("{}"));
        }
    }

    /// Go `queue(u)`: records a payload for the entry about to be appended to `parts`.
    fn queue(&mut self, mut u: PendingUpload) {
        u.slot = self.parts.len();
        self.pending.push(u);
    }

    /// Go `addText(entry, text, contentOrd, index)`: a text part; if it is too long to keep
    /// inline and an upload is possible, the entry holds the truncated text until the upload
    /// replaces it with a preview and a reference.
    fn add_text(
        &mut self,
        entry: &mut Map<String, Value>,
        text: &str,
        content_ord: usize,
        index: usize,
    ) {
        let mut content_lost = false;
        let mut text = text.to_owned();
        if self.opts.redact {
            (text, content_lost) = sanitize_raw_text(&text);
        }
        let (shown, length_truncated) = truncate_string(&text, self.opts.max_len);
        let truncated = content_lost || length_truncated;
        entry.insert("mime_type".to_owned(), Value::from("text/plain"));
        entry.insert("text".to_owned(), Value::from(shown));
        let len = i64::try_from(text.len()).unwrap_or(i64::MAX);
        if self.opts.offload && (length_truncated || len > self.opts.inline_limit) {
            let (reserved, ok) = self.reserve(len);
            if ok {
                self.queue(PendingUpload {
                    content_ord,
                    part_idx: index,
                    mime_type: "text/plain".to_owned(),
                    is_text: true,
                    text,
                    fallback_truncated: truncated,
                    reserved,
                    ..PendingUpload::default()
                });
                self.truncated = self.truncated || content_lost;
                self.non_upload_truncated = self.non_upload_truncated || content_lost;
                return;
            }
        }
        self.truncated = self.truncated || truncated;
        self.non_upload_truncated = self.non_upload_truncated || truncated;
    }

    /// Go `addInlineData(entry, blob, contentOrd, index)`: a binary part. The bytes are kept
    /// if they are to be uploaded (Divergence (ledger 7b-1): a cheap handle sharing the
    /// caller's immutable `Bytes` buffer, where Go copies with `bytes.Clone`; nothing can change
    /// the bytes through the message, so the queued data is the same);
    /// otherwise a placeholder stands in for them. Offloading records
    /// [`DEFAULT_BINARY_MIME_TYPE`] for a blob without a MIME type.
    fn add_inline_data(
        &mut self,
        entry: &mut Map<String, Value>,
        blob: &Blob,
        content_ord: usize,
        index: usize,
    ) {
        entry.insert("mime_type".to_owned(), Value::from(blob.mime_type.as_str()));
        entry.insert("text".to_owned(), Value::from(INLINE_DATA_TEXT));
        if self.opts.offload {
            let mime_type = if blob.mime_type.is_empty() {
                DEFAULT_BINARY_MIME_TYPE
            } else {
                blob.mime_type.as_str()
            };
            entry.insert("mime_type".to_owned(), Value::from(mime_type));
            let len = i64::try_from(blob.data.len()).unwrap_or(i64::MAX);
            let (reserved, ok) = self.reserve(len);
            if ok {
                self.queue(PendingUpload {
                    content_ord,
                    part_idx: index,
                    mime_type: mime_type.to_owned(),
                    data: blob.data.clone(),
                    reserved,
                    ..PendingUpload::default()
                });
            } else {
                entry.insert("text".to_owned(), Value::from(UPLOAD_FAILED_TEXT));
            }
        }
    }

    /// Go `addFileData(entry, file)`: a part that already refers to external data; with
    /// offloading, a `gs://` file also gets an `object_ref` (details `"{}"`, the connection as
    /// authorizer), unless its URI had to be redacted.
    fn add_file_data(&mut self, entry: &mut Map<String, Value>, file: &FileData) {
        let mut uri = file.file_uri.clone();
        let mut changed = false;
        if self.opts.redact {
            (uri, changed) = sanitize_uri(&uri);
        }
        entry.insert("mime_type".to_owned(), Value::from(file.mime_type.as_str()));
        entry.insert("uri".to_owned(), Value::from(uri.as_str()));
        entry.insert(
            "storage_mode".to_owned(),
            Value::from(STORAGE_MODE_EXTERNAL_URI),
        );
        self.truncated = self.truncated || changed;
        self.non_upload_truncated = self.non_upload_truncated || changed;
        if self.opts.offload && !changed && uri.starts_with("gs://") {
            entry.insert(
                "object_ref".to_owned(),
                Value::Object(object_ref(&uri, "{}", &self.opts.connection_id)),
            );
        }
    }

    /// Go `addStructured(entry, mimeType, text, key, value)`: a short text, and the object
    /// itself under `key` in `part_attributes`.
    fn add_structured<T: Serialize>(
        &mut self,
        entry: &mut Map<String, Value>,
        mime_type: &str,
        text: &str,
        key: &'static str,
        value: &T,
    ) {
        let (max_len, redact) = (self.opts.max_len, self.opts.redact);
        let (shown, truncated) = sanitize_and_truncate_text(text, max_len, redact);
        entry.insert("mime_type".to_owned(), Value::from(mime_type));
        entry.insert("text".to_owned(), Value::from(shown));
        self.truncated = self.truncated || truncated;
        let (attrs, attrs_truncated) =
            format_part_attributes(&one_entry(key, value), max_len, redact);
        entry.insert("part_attributes".to_owned(), Value::from(attrs));
        self.truncated = self.truncated || attrs_truncated;
        self.non_upload_truncated = self.non_upload_truncated || truncated || attrs_truncated;
    }
}

/// The language of an executable-code part as Go's `string(Language)`.
pub(super) fn language_text(code: &gadk_genai::ExecutableCode) -> String {
    enum_text(code.language.as_ref())
}

/// The outcome of a code-execution-result part as Go's `string(Outcome)`.
pub(super) fn outcome_text(result: &gadk_genai::CodeExecutionResult) -> String {
    enum_text(result.outcome.as_ref())
}
