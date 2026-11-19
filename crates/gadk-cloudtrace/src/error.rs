// Copyright 2026 Chris Olson
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

//! [`TraceError`] and the helpers that read a Google API error body.

use serde_json::Value;

/// The most bytes of an error body a [`TraceError::Http`] carries.
pub(crate) const EXCERPT_LIMIT: usize = 1024;

const ERROR_INFO: &str = "type.googleapis.com/google.rpc.ErrorInfo";

/// A failed Cloud Trace request.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TraceError {
    /// A non-2xx reply (except a 404 from [`get`](crate::CloudTraceClient::get)).
    #[error("Cloud Trace: HTTP {status}: {excerpt}")]
    Http {
        /// The HTTP status.
        status: u16,
        /// The `reason` of the body's first `google.rpc.ErrorInfo` detail, such as
        /// `SERVICE_DISABLED`.
        reason: Option<String>,
        /// At most 1,024 bytes of the body (lossy UTF-8), cut on a char boundary.
        excerpt: String,
    },
    /// The trace is unknown: [`get`](crate::CloudTraceClient::get) got HTTP 404.
    #[error("trace not found")]
    NotFound,
    /// The request could not be built or sent, or the reply could not be read.
    #[error("Cloud Trace: {0}")]
    Transport(String),
    /// The reply is not the expected JSON, or a span id that must be decimal is not.
    #[error("Cloud Trace: unexpected response: {0}")]
    Decode(String),
    /// No request credentials could be produced.
    #[error("credentials: {0}")]
    Credentials(String),
}

/// At most [`EXCERPT_LIMIT`] bytes of `body` (lossy UTF-8), cut on a char boundary.
pub(crate) fn excerpt(body: &[u8]) -> String {
    // Lossy decoding never shrinks the input, so the first `EXCERPT_LIMIT + 4` bytes cover
    // every character that can end within `EXCERPT_LIMIT` decoded bytes.
    let mut text = String::from_utf8_lossy(&body[..body.len().min(EXCERPT_LIMIT + 4)]).into_owned();
    text.truncate(text.floor_char_boundary(EXCERPT_LIMIT));
    text
}

/// `error.details[*].reason` of the first `google.rpc.ErrorInfo` detail of a Google error body.
pub(crate) fn error_reason(body: &[u8]) -> Option<String> {
    let v: Value = serde_json::from_slice(body).ok()?;
    v.pointer("/error/details")?
        .as_array()?
        .iter()
        .find(|d| d.get("@type").and_then(Value::as_str) == Some(ERROR_INFO))?
        .get("reason")?
        .as_str()
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_texts() {
        let http = TraceError::Http {
            status: 403,
            reason: None,
            excerpt: "{}".into(),
        };
        assert_eq!(http.to_string(), "Cloud Trace: HTTP 403: {}");
        assert_eq!(
            TraceError::Transport("timed out".into()).to_string(),
            "Cloud Trace: timed out"
        );
        assert_eq!(
            TraceError::Decode("missing traces".into()).to_string(),
            "Cloud Trace: unexpected response: missing traces"
        );
        assert_eq!(
            TraceError::Credentials("no ADC".into()).to_string(),
            "credentials: no ADC"
        );
        assert_eq!(TraceError::NotFound.to_string(), "trace not found");
    }

    #[test]
    fn excerpt_keeps_short_ascii() {
        assert_eq!(excerpt(b""), "");
        assert_eq!(excerpt(b"permission denied"), "permission denied");
    }

    #[test]
    fn excerpt_cuts_long_ascii_at_the_limit() {
        let body = vec![b'a'; 3000];
        let out = excerpt(&body);
        assert_eq!(out.len(), EXCERPT_LIMIT);
        assert!(out.bytes().all(|b| b == b'a'));
        assert_eq!(excerpt(&vec![b'b'; EXCERPT_LIMIT]).len(), EXCERPT_LIMIT);
    }

    #[test]
    fn excerpt_never_splits_a_multibyte_char() {
        // "é" (2 bytes) straddles byte 1024: it is dropped whole.
        let mut body = vec![b'a'; EXCERPT_LIMIT - 1];
        body.extend_from_slice("é tail".as_bytes());
        let out = excerpt(&body);
        assert_eq!(out.len(), EXCERPT_LIMIT - 1);
        assert!(out.bytes().all(|b| b == b'a'));

        // "€" (3 bytes) at 1022..1025: dropped whole.
        let mut body = vec![b'a'; EXCERPT_LIMIT - 2];
        body.extend_from_slice("€€".as_bytes());
        assert_eq!(excerpt(&body).len(), EXCERPT_LIMIT - 2);

        // "é" ending exactly at byte 1024: kept.
        let mut body = vec![b'a'; EXCERPT_LIMIT - 2];
        body.extend_from_slice("éé".as_bytes());
        let out = excerpt(&body);
        assert_eq!(out.len(), EXCERPT_LIMIT);
        assert!(out.ends_with('é'));
    }

    #[test]
    fn excerpt_decodes_invalid_utf8_lossily() {
        assert_eq!(excerpt(b"ok\xffok"), "ok\u{FFFD}ok");
        // A replacement char (3 bytes) at byte 1023 would cross the limit: dropped.
        let mut body = vec![b'a'; EXCERPT_LIMIT - 1];
        body.extend_from_slice(b"\xff\xfe");
        let out = excerpt(&body);
        assert_eq!(out.len(), EXCERPT_LIMIT - 1);
        assert!(!out.contains('\u{FFFD}'));
    }

    #[test]
    fn error_reason_reads_the_first_error_info() {
        let body = br#"{"error":{"details":[
            {"@type":"type.googleapis.com/google.rpc.Help","links":[]},
            {"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"SERVICE_DISABLED"},
            {"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"SECOND"}
        ]}}"#;
        assert_eq!(error_reason(body).as_deref(), Some("SERVICE_DISABLED"));
        assert_eq!(error_reason(br#"{"error":{"details":[]}}"#), None);
        assert_eq!(error_reason(b"not json"), None);
    }
}
