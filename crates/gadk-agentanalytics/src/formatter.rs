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

//! [`format_content_parts`] and the parts collector, content summaries, function-response
//! normalization and [`smart_truncate`] (a sanitizing `serde::Serializer` that builds the truncated
//! value directly), plus the pending upload, object reference and upload text and content type the
//! GCS offloader shares.
//!
//! The bodies live in submodules, re-exported here so every item keeps its `crate::formatter::`
//! path: `sanitize` (`smart_truncate` and the sanitizing serializer), `parts` (`content_parts`,
//! function-response normalization) and `summary` (content summaries).

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;
use serde::{Serialize, Serializer};
use serde_json::{Map, Value};

#[cfg(test)]
mod golden_tests;
mod parts;
mod sanitize;
mod summary;

pub use parts::format_content_parts;
pub(crate) use parts::{format_content_parts_with, sanitize_and_truncate_text};
#[cfg(test)]
pub(crate) use parts::{format_part_attributes, normalize_function_response};
#[cfg(test)]
pub(crate) use sanitize::JSON_NUMBER_TOKEN;
pub use sanitize::smart_truncate;
pub(crate) use sanitize::{sanitize_value, smart_truncate_with, truncate_string};
#[cfg(test)]
pub(crate) use summary::content_summary;
pub(crate) use summary::format_response_content;

/// The suffix of truncated text.
pub(crate) const TRUNCATION_SUFFIX: &str = "...[TRUNCATED]";
/// The text of an inline binary part.
pub(crate) const INLINE_DATA_TEXT: &str = "[BINARY DATA]";
/// Non-UTF-8 bytes in a sanitized value.
pub(crate) const BINARY_DATA_PLACEHOLDER: &str = "[BINARY_DATA]";
/// The placeholder of a circular reference. Unused: a `Serialize` value cannot contain itself, so
/// there is no cycle detection.
pub(crate) const CIRCULAR_REF_PLACEHOLDER: &str = "[CIRCULAR_REFERENCE]";

/// The value of map keys that are written alike but disagree on their value, with redaction off.
pub(crate) const COLLIDING_KEY_PLACEHOLDER: &str = "[COLLIDING_KEY]";
/// The placeholder of a value nested past the depth limit.
pub(crate) const MAX_DEPTH_PLACEHOLDER: &str = "[MAX_DEPTH_EXCEEDED]";
/// The placeholder of a value reached after the node budget ran out.
pub(crate) const SANITIZE_BUDGET_PLACEHOLDER: &str = "[SANITIZE_BUDGET_EXCEEDED]";
/// The placeholder of a map key that is not a supported key type.
pub(crate) const UNSUPPORTED_KEY_PLACEHOLDER: &str = "[UNSUPPORTED_KEY]";
/// The placeholder of a value that cannot be sanitized.
pub(crate) const UNSUPPORTED_OBJ_PLACEHOLDER: &str = "[UNSUPPORTED_OBJECT]";

/// The depth limit of a sanitization (adk-python's).
pub(crate) const MAX_SANITIZE_DEPTH: usize = 50;
/// The node budget of a sanitization (adk-python's).
pub(crate) const MAX_SANITIZE_NODES: i64 = 100_000;
/// The most capacity preallocated for a decoded container.
pub(crate) const MAX_CONTAINER_ALLOC: usize = 1024;

/// The `storage_mode` of a part kept inline.
pub(crate) const STORAGE_MODE_INLINE: &str = "INLINE";
/// The `storage_mode` of a part that refers to an external URI.
pub(crate) const STORAGE_MODE_EXTERNAL_URI: &str = "EXTERNAL_URI";
/// The `storage_mode` of a part offloaded to Cloud Storage.
pub(crate) const STORAGE_MODE_GCS_REFERENCE: &str = "GCS_REFERENCE";

/// The MIME type of an offloaded blob that has none.
pub(crate) const DEFAULT_BINARY_MIME_TYPE: &str = "application/octet-stream";

/// The text of a binary part whose upload failed.
pub(crate) const UPLOAD_FAILED_TEXT: &str = "[UPLOAD FAILED]";
/// The content type of uploaded text objects.
pub(crate) const TEXT_UPLOAD_CONTENT_TYPE: &str = "text/plain; charset=utf-8";

/// The node budget of one sanitization, shared by the formatter's walk and redaction's JSON-text
/// recursion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NodeBudget {
    /// The nodes left.
    pub(crate) remaining: i64,
}

impl NodeBudget {
    /// A budget of [`MAX_SANITIZE_NODES`] nodes.
    pub(crate) fn new() -> NodeBudget {
        NodeBudget {
            remaining: MAX_SANITIZE_NODES,
        }
    }

    /// Uses one node; `false` once the budget is spent.
    pub(crate) fn take(&mut self) -> bool {
        if self.remaining <= 0 {
            return false;
        }
        self.remaining -= 1;
        true
    }
}

impl Default for NodeBudget {
    fn default() -> Self {
        NodeBudget::new()
    }
}

/// A value the formatter builds by hand: JSON values plus raw bytes, which serialize through
/// `serialize_bytes` so the sanitizer writes them as UTF-8 text, else `"[BINARY_DATA]"`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum GoAny {
    /// A JSON value.
    Json(Value),
    /// Raw bytes.
    Bytes(Bytes),
    /// A list.
    List(Vec<GoAny>),
    /// A map.
    Map(BTreeMap<String, GoAny>),
}

impl Serialize for GoAny {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            GoAny::Json(v) => v.serialize(s),
            GoAny::Bytes(b) => s.serialize_bytes(b),
            GoAny::List(items) => s.collect_seq(items),
            GoAny::Map(m) => s.collect_map(m),
        }
    }
}

/// A payload to upload to Cloud Storage when its row is written, and the `content_parts` entry that
/// then refers to it It owns its data.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct PendingUpload {
    /// The index of the entry in the row's `content_parts`.
    pub(crate) slot: usize,
    /// The position of the message in its request.
    pub(crate) content_ord: usize,
    /// The index of the part in its message.
    pub(crate) part_idx: usize,
    /// The MIME type of a binary part.
    pub(crate) mime_type: String,
    /// The payload of a binary part.
    pub(crate) data: Bytes,
    /// The payload of a text part.
    pub(crate) text: String,
    /// Whether the payload is text.
    pub(crate) is_text: bool,
    /// The inline fallback text was truncated by `max_content_len`.
    pub(crate) fallback_truncated: bool,
    /// Bytes reserved in the batch processor's pending-upload budget.
    pub(crate) reserved: i64,
}

impl PendingUpload {
    /// The payload length in bytes.
    pub(crate) fn size(&self) -> usize {
        if self.is_text {
            self.text.len()
        } else {
            self.data.len()
        }
    }

    /// The content type of the object.
    pub(crate) fn content_type(&self) -> &str {
        if self.is_text {
            TEXT_UPLOAD_CONTENT_TYPE
        } else {
            &self.mime_type
        }
    }

    /// The bytes of the object.
    pub(crate) fn payload(&self) -> Bytes {
        if self.is_text {
            Bytes::from(self.text.clone().into_bytes())
        } else {
            self.data.clone()
        }
    }
}

/// The `object_ref` of the object at `uri`: `uri`, `details`, and `authorizer` only when it is
/// non-empty (otherwise it is left out and reads as NULL).
pub(crate) fn object_ref(uri: &str, details: &str, authorizer: &str) -> Map<String, Value> {
    let mut obj = Map::new();
    obj.insert("uri".to_owned(), Value::from(uri));
    obj.insert("details".to_owned(), Value::from(details));
    if !authorizer.is_empty() {
        obj.insert("authorizer".to_owned(), Value::from(authorizer));
    }
    obj
}

/// The options of a [`PartsCollector`].
#[derive(Clone, Default)]
pub(crate) struct PartsOptions {
    /// The longest inline text in chars; negative means no limit.
    pub(crate) max_len: i64,
    /// Whether to redact sensitive text and URIs.
    pub(crate) redact: bool,
    /// Queue inline binary data and long text as [`PendingUpload`]s.
    pub(crate) offload: bool,
    /// Text longer than this many bytes is offloaded (positive when `offload`).
    pub(crate) inline_limit: i64,
    /// The authorizer of object references.
    pub(crate) connection_id: String,
    /// Reserves `n` bytes of upload budget; `false` when over the cap.
    pub(crate) reserve_bytes: Option<Arc<dyn Fn(i64) -> bool + Send + Sync>>,
}

impl std::fmt::Debug for PartsOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PartsOptions")
            .field("max_len", &self.max_len)
            .field("redact", &self.redact)
            .field("offload", &self.offload)
            .field("inline_limit", &self.inline_limit)
            .field("connection_id", &self.connection_id)
            .field("reserve_bytes", &self.reserve_bytes.is_some())
            .finish()
    }
}

/// Builds the `content_parts` of one row from one or more messages.
#[derive(Debug, Default)]
pub(crate) struct PartsCollector {
    /// The options.
    pub(crate) opts: PartsOptions,
    /// The `content_parts` entries in order.
    pub(crate) parts: Vec<Map<String, Value>>,
    /// The payloads to upload, each naming its entry in `parts`.
    pub(crate) pending: Vec<PendingUpload>,
    /// Whether any part lost content to truncation or redaction.
    pub(crate) truncated: bool,
    /// Whether any part that is not uploaded lost content to truncation or redaction.
    pub(crate) non_upload_truncated: bool,
}

impl PartsCollector {
    /// A collector with `opts` and nothing collected.
    pub(crate) fn new(opts: PartsOptions) -> PartsCollector {
        PartsCollector {
            opts,
            ..PartsCollector::default()
        }
    }
}

/// One piece of a message summary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SummaryPiece {
    pub(crate) part_idx: usize,
    pub(crate) is_text: bool,
    pub(crate) text: String,
    pub(crate) truncated: bool,
    pub(crate) omitted: bool,
}

/// The summary of one message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct MessageSummary {
    pub(crate) content_ord: usize,
    pub(crate) role: String,
    pub(crate) has_role: bool,
    pub(crate) is_system: bool,
    pub(crate) is_response: bool,
    pub(crate) pieces: Vec<SummaryPiece>,
}

/// Rebuilds a row's `content` summary once its uploads resolve.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ContentSummaryBuilder {
    pub(crate) is_llm_request: bool,
    pub(crate) is_llm_response: bool,
    pub(crate) content_denied: bool,
    pub(crate) max_len: i64,
    pub(crate) redact: bool,
    pub(crate) role_truncated: bool,
    pub(crate) other_truncated: bool,
    pub(crate) needs_rebuild: bool,
    pub(crate) usage: Option<Map<String, Value>>,
    pub(crate) messages: Vec<MessageSummary>,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    #[test]
    fn node_budget_take() {
        let mut b = NodeBudget { remaining: 2 };
        assert!(b.take());
        assert!(b.take());
        assert!(!b.take());
        assert_eq!(b.remaining, 0);
        assert_eq!(NodeBudget::new().remaining, 100_000);
    }

    #[test]
    fn pending_upload_payload_and_content_type() {
        let text = PendingUpload {
            is_text: true,
            text: "héllo".into(),
            mime_type: "text/plain".into(),
            ..PendingUpload::default()
        };
        assert_eq!(text.size(), 6);
        assert_eq!(text.content_type(), "text/plain; charset=utf-8");
        assert_eq!(&text.payload()[..], "héllo".as_bytes());
        let blob = PendingUpload {
            data: Bytes::from_static(&[0xff, 0x00]),
            mime_type: "image/png".into(),
            ..PendingUpload::default()
        };
        assert_eq!(blob.size(), 2);
        assert_eq!(blob.content_type(), "image/png");
        assert_eq!(&blob.payload()[..], &[0xff, 0x00]);
    }

    #[test]
    fn object_ref_omits_empty_authorizer() {
        assert_eq!(
            Value::Object(object_ref("gs://b/o", "{}", "")),
            json!({"uri": "gs://b/o", "details": "{}"})
        );
        assert_eq!(
            Value::Object(object_ref("gs://b/o", "{}", "us.conn")),
            json!({"uri": "gs://b/o", "details": "{}", "authorizer": "us.conn"})
        );
    }

    #[test]
    fn go_any_serializes_bytes_as_bytes() {
        let mut m = BTreeMap::new();
        m.insert("data".to_owned(), GoAny::Bytes(Bytes::from_static(b"ab")));
        m.insert("n".to_owned(), GoAny::Json(json!(1)));
        let v = GoAny::List(vec![GoAny::Map(m)]);
        // serde_json writes bytes as an array of numbers; the sanitizer intercepts them.
        assert_eq!(
            serde_json::to_value(&v).unwrap(),
            json!([{"data": [97, 98], "n": 1}])
        );
    }
}
