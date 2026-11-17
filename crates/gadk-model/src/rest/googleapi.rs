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

//! The Google API HTTP error: [`ApiError`] (its `Display` is the `googleapi: Error {code}: …`
//! text), [`ErrorItem`], and the response check that builds it ([`check_response`], with
//! [`api_error_from_response`] as its error branch).

use std::fmt;

use http::HeaderMap;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// One entry of a Google API error's `errors` list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ErrorItem {
    /// `reason`, e.g. `notFound`.
    pub reason: String,
    /// `message`.
    pub message: String,
}

/// A Google API HTTP error, built by [`check_response`] from a response with status ≥ 300.
/// `Display` renders `googleapi: Error {code}: {message}` plus any details and error items.
#[derive(Debug, Clone, Default)]
pub struct ApiError {
    /// The HTTP status code.
    pub code: u16,
    /// The JSON error body's `error.message`.
    pub message: String,
    /// The JSON error body's `error.details`.
    pub details: Vec<Value>,
    /// The raw response body.
    pub body: String,
    /// The response headers.
    pub header: HeaderMap,
    /// The JSON error body's `error.errors`.
    pub errors: Vec<ErrorItem>,
}

impl fmt::Display for ApiError {
    /// Formats the error as the `googleapi: …` text described on [`ApiError`].
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.errors.is_empty() && self.message.is_empty() {
            return write!(
                f,
                "googleapi: got HTTP response code {} with body: {}",
                self.code, self.body
            );
        }
        let mut buf = format!("googleapi: Error {}: ", self.code);
        buf.push_str(&self.message);
        if !self.details.is_empty()
            && let Ok(compact) = crate::to_go_json(&self.details)
        {
            // Details as pretty-printed JSON with a two-space indent and a trailing newline.
            buf.push_str("\nDetails:");
            buf.push('\n');
            buf.push_str(&go_json_indent(&compact));
            buf.push('\n');
        }
        if self.errors.is_empty() {
            return f.write_str(buf.trim());
        }
        if self.errors.len() == 1 && self.errors[0].message == self.message {
            buf.push_str(", ");
            buf.push_str(&self.errors[0].reason);
            return f.write_str(&buf);
        }
        buf.push_str("\nMore details:\n");
        for e in &self.errors {
            buf.push_str(&format!("Reason: {}, Message: {}\n", e.reason, e.message));
        }
        f.write_str(&buf)
    }
}

impl std::error::Error for ApiError {}

/// Re-indents compact JSON (`to_go_json` output) with two spaces per level.
fn go_json_indent(compact: &[u8]) -> String {
    let text = String::from_utf8_lossy(compact);
    let mut out = String::with_capacity(text.len() * 2);
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut need_indent = false;
    let newline = |out: &mut String, depth: usize| {
        out.push('\n');
        for _ in 0..depth {
            out.push_str("  ");
        }
    };
    for c in text.chars() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '{' | '[' => {
                if need_indent {
                    newline(&mut out, depth);
                }
                out.push(c);
                depth += 1;
                need_indent = true;
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                if need_indent {
                    // An empty object or array stays `{}` / `[]`.
                    need_indent = false;
                } else {
                    newline(&mut out, depth);
                }
                out.push(c);
            }
            ',' => {
                out.push(c);
                newline(&mut out, depth);
            }
            ':' => out.push_str(": "),
            _ => {
                if need_indent {
                    newline(&mut out, depth);
                    need_indent = false;
                }
                if c == '"' {
                    in_string = true;
                }
                out.push(c);
            }
        }
    }
    out
}

/// `Ok` for a 2xx status, else the [`ApiError`] of [`api_error_from_response`].
pub fn check_response(parts: &http::response::Parts, body: &[u8]) -> Result<(), Box<ApiError>> {
    if parts.status.is_success() {
        return Ok(());
    }
    Err(Box::new(api_error_from_response(parts, body)))
}

/// For a response with status ≥ 300: the [`ApiError`] built from the JSON error body
/// (`error.code` / `message` / `errors` / `details`), the raw body and the headers.
pub fn api_error_from_response(parts: &http::response::Parts, body: &[u8]) -> ApiError {
    let status = parts.status.as_u16();
    let mut err = ApiError {
        code: status,
        body: String::from_utf8_lossy(body).into_owned(),
        header: parts.headers.clone(),
        ..ApiError::default()
    };
    if let Some(reply) = error_reply_from_body(body) {
        if reply.code != 0 {
            // The body's `error.code` is kept even when it differs from the HTTP status; a code
            // outside `u16` (negative or above 65535) keeps the HTTP status instead.
            err.code = u16::try_from(reply.code).unwrap_or(status);
        }
        err.message = reply.message;
        err.details = reply.details;
        // A `null` entry becomes a zero `ErrorItem` and is kept.
        err.errors = reply
            .errors
            .into_iter()
            .map(|i| {
                let i = i.unwrap_or_default();
                ErrorItem {
                    reason: i.reason,
                    message: i.message,
                }
            })
            .collect();
    }
    err
}

#[derive(Deserialize)]
struct ErrorReply {
    error: Option<ErrorReplyBody>,
}

#[derive(Deserialize)]
struct ErrorReplyBody {
    #[serde(default, deserialize_with = "nullable")]
    code: i64,
    #[serde(default, deserialize_with = "nullable")]
    message: String,
    #[serde(default, deserialize_with = "nullable")]
    details: Vec<Value>,
    #[serde(default, deserialize_with = "nullable")]
    errors: Vec<Option<ErrorItemReply>>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ErrorItemReply {
    #[serde(deserialize_with = "nullable")]
    reason: String,
    #[serde(deserialize_with = "nullable")]
    message: String,
}

/// A JSON object `{"error": …}`, or the first element of a JSON array
/// of them; `None` when the body is neither or has no `error`.
fn error_reply_from_body(body: &[u8]) -> Option<ErrorReplyBody> {
    if body.first() == Some(&b'[') {
        let mut replies: Vec<ErrorReply> = serde_json::from_slice(body).ok()?;
        if replies.is_empty() {
            return None;
        }
        return replies.swap_remove(0).error;
    }
    serde_json::from_slice::<ErrorReply>(body).ok()?.error
}

/// A JSON `null` (or a missing field) as the type's zero value.
fn nullable<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use serde_json::json;

    use super::*;

    fn parts(status: u16) -> http::response::Parts {
        let (mut parts, ()) = http::Response::new(()).into_parts();
        parts.status = http::StatusCode::from_u16(status).unwrap();
        parts
    }

    #[test]
    fn display_is_gos_error_text() {
        let no_message = ApiError {
            code: 502,
            body: "bad gateway".into(),
            ..ApiError::default()
        };
        assert_eq!(
            no_message.to_string(),
            "googleapi: got HTTP response code 502 with body: bad gateway"
        );
        let one_item = ApiError {
            code: 404,
            message: "nf".into(),
            errors: vec![ErrorItem {
                reason: "notFound".into(),
                message: "nf".into(),
            }],
            ..ApiError::default()
        };
        assert_eq!(one_item.to_string(), "googleapi: Error 404: nf, notFound");
        let many = ApiError {
            code: 400,
            message: "bad".into(),
            errors: vec![
                ErrorItem {
                    reason: "invalid".into(),
                    message: "a".into(),
                },
                ErrorItem::default(),
            ],
            ..ApiError::default()
        };
        assert_eq!(
            many.to_string(),
            "googleapi: Error 400: bad\nMore details:\nReason: invalid, Message: a\nReason: , \
             Message: \n"
        );
        let details = ApiError {
            code: 400,
            message: "bad".into(),
            details: vec![json!({"@type": "t<x>", "empty": {}, "list": [1, 2], "none": []})],
            ..ApiError::default()
        };
        assert_eq!(
            details.to_string(),
            "googleapi: Error 400: bad\nDetails:\n[\n  {\n    \"@type\": \"t\\u003cx\\u003e\",\n    \
             \"empty\": {},\n    \"list\": [\n      1,\n      2\n    ],\n    \"none\": []\n  }\n]"
        );
    }

    #[test]
    fn check_response_passes_2xx_and_builds_the_error_otherwise() {
        for status in [200, 201, 204, 299] {
            assert!(
                check_response(&parts(status), b"not json").is_ok(),
                "{status}"
            );
        }
        let mut p = parts(404);
        p.headers
            .insert("x-goog-request-id", http::HeaderValue::from_static("r1"));
        let body = br#"{"error":{"code":404,"message":"gone","errors":[{"reason":"notFound","message":"gone"}],"details":[{"k":1}]}}"#;
        let err = check_response(&p, body).unwrap_err();
        assert_eq!(err.code, 404);
        assert_eq!(err.message, "gone");
        assert_eq!(err.details, vec![json!({"k": 1})]);
        assert_eq!(err.body, String::from_utf8_lossy(body));
        assert_eq!(err.header.get("x-goog-request-id").unwrap(), "r1");
        // A 3xx is an error too (Go checks 200..=299).
        let err = check_response(&parts(304), b"").unwrap_err();
        assert_eq!(
            err.to_string(),
            "googleapi: got HTTP response code 304 with body: "
        );
    }

    #[test]
    fn api_error_from_response_reads_go_error_replies() {
        // The body's code wins; a JSON array's first element is read.
        let err =
            api_error_from_response(&parts(400), br#"[{"error":{"code":409,"message":"dup"}}]"#);
        assert_eq!((err.code, err.message.as_str()), (409, "dup"));
        // A zero, negative or too large body code keeps the status.
        for body in [
            &br#"{"error":{"code":0,"message":"m"}}"#[..],
            br#"{"error":{"code":-1,"message":"m"}}"#,
            br#"{"error":{"code":70000,"message":"m"}}"#,
        ] {
            assert_eq!(api_error_from_response(&parts(412), body).code, 412);
        }
        // `null` fields are zero values; a `null` item is a zero item.
        let err = api_error_from_response(
            &parts(403),
            br#"{"error":{"code":null,"message":null,"details":null,"errors":[null,{"reason":null,"message":"m"}]}}"#,
        );
        assert_eq!(err.code, 403);
        assert_eq!(
            err.errors,
            vec![
                ErrorItem::default(),
                ErrorItem {
                    reason: String::new(),
                    message: "m".into(),
                },
            ]
        );
        // Not an error reply: the status and the raw body.
        for body in [
            &b"down"[..],
            b"[]",
            br#"{"error":null}"#,
            br#"{"error":"x"}"#,
        ] {
            let err = api_error_from_response(&parts(502), body);
            assert_eq!(err.code, 502);
            assert!(err.message.is_empty() && err.errors.is_empty());
            assert_eq!(err.body, String::from_utf8_lossy(body));
        }
    }
}
