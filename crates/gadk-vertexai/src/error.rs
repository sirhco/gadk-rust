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

//! [`VertexError`] (an HTTP or operation error from the Vertex AI API, displayed as a gRPC
//! status text), the gRPC code tables, and the prefix wrapping the services build on top of it.

use std::error::Error as StdError;

use gadk_memory::MemoryError;
use gadk_platform::GoWrapped;
use gadk_session::SessionError;
use serde_json::Value;

/// A boxed error cause.
pub(crate) type BoxError = Box<dyn StdError + Send + Sync>;

/// An HTTP error from the Vertex AI API (Google's JSON error body), or an operation that
/// completed with an error. It displays as a gRPC status text,
/// `rpc error: code = {name} desc = {message}`, with the CamelCase gRPC code name (`NotFound`).
/// It is reachable through the `source()` chain
/// only from `Backend` service errors; `SessionError::InvalidRequest` (flattened to a string) and
/// `NotFound` do not carry it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VertexError {
    /// The HTTP status (for an operation error: the status Google maps its gRPC code to).
    pub status: u16,
    /// The Google status name from the error body's `error.status` (`NOT_FOUND`, …); `None`
    /// when the body has none.
    pub code: Option<String>,
    /// The error body's `error.message`, or `gadk_model::body_prefix(body)` when the body is
    /// not Google's error JSON.
    pub message: String,
}

/// gRPC code number, Google status name, gRPC code name and HTTP status
/// (google/rpc/code.proto; the HTTP column is Google's mapping, used for replayed errors).
const CODES: [(i64, &str, &str, u16); 17] = [
    (0, "OK", "OK", 200),
    (1, "CANCELLED", "Canceled", 499),
    (2, "UNKNOWN", "Unknown", 500),
    (3, "INVALID_ARGUMENT", "InvalidArgument", 400),
    (4, "DEADLINE_EXCEEDED", "DeadlineExceeded", 504),
    (5, "NOT_FOUND", "NotFound", 404),
    (6, "ALREADY_EXISTS", "AlreadyExists", 409),
    (7, "PERMISSION_DENIED", "PermissionDenied", 403),
    (8, "RESOURCE_EXHAUSTED", "ResourceExhausted", 429),
    (9, "FAILED_PRECONDITION", "FailedPrecondition", 400),
    (10, "ABORTED", "Aborted", 409),
    (11, "OUT_OF_RANGE", "OutOfRange", 400),
    (12, "UNIMPLEMENTED", "Unimplemented", 501),
    (13, "INTERNAL", "Internal", 500),
    (14, "UNAVAILABLE", "Unavailable", 503),
    (15, "DATA_LOSS", "DataLoss", 500),
    (16, "UNAUTHENTICATED", "Unauthenticated", 401),
];

/// The Google status name of a gRPC code number (`5` → `"NOT_FOUND"`); `"UNKNOWN"` for a
/// number outside 0–16.
pub(crate) fn grpc_code_name(code: i64) -> &'static str {
    CODES
        .iter()
        .find(|(n, ..)| *n == code)
        .map_or("UNKNOWN", |(_, name, ..)| name)
}

/// The HTTP status Google maps a status name to (`"NOT_FOUND"` → 404); 500 for an unknown
/// name.
pub(crate) fn http_status_for_code(name: &str) -> u16 {
    CODES
        .iter()
        .find(|(_, n, ..)| *n == name)
        .map_or(500, |(.., status)| *status)
}

/// The gRPC code name of a Google status name (`"NOT_FOUND"` → `"NotFound"`).
fn go_name_for_code(name: &str) -> Option<&'static str> {
    CODES
        .iter()
        .find(|(_, n, ..)| *n == name)
        .map(|(_, _, go, _)| *go)
}

/// The gRPC code name Google's HTTP-to-gRPC mapping gives an HTTP status without a
/// status name.
fn go_name_for_status(status: u16) -> &'static str {
    match status {
        400 => "InvalidArgument",
        401 => "Unauthenticated",
        403 => "PermissionDenied",
        404 => "NotFound",
        409 => "AlreadyExists",
        429 => "ResourceExhausted",
        499 => "Canceled",
        500 => "Internal",
        501 => "Unimplemented",
        503 => "Unavailable",
        504 => "DeadlineExceeded",
        _ => "Unknown",
    }
}

impl VertexError {
    /// The error of a non-2xx response: `error.status` and `error.message` of Google's JSON
    /// error body (`{"error": {"code": …, "message": …, "status": …}}`; a missing message is
    /// empty, a missing or empty status is `None`), or no code and
    /// `gadk_model::body_prefix(body)` when the body is not that JSON.
    pub(crate) fn from_response(status: u16, body: &[u8]) -> Self {
        let parsed = serde_json::from_slice::<Value>(body).ok();
        let error = parsed
            .as_ref()
            .and_then(|v| v.get("error"))
            .filter(|e| e.is_object());
        match error {
            Some(error) => Self {
                status,
                code: error
                    .get("status")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned),
                message: error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            },
            None => Self {
                status,
                code: None,
                message: gadk_model::body_prefix(body),
            },
        }
    }

    /// The error of an operation that completed with `error` (a `google.rpc.Status` in protojson:
    /// `{"code": <gRPC number>, "message": …}`, a zero code omitted). It displays the status
    /// text of that error.
    pub(crate) fn from_operation_error(error: &Value) -> Self {
        let number = error
            .get("code")
            .and_then(crate::protojson::int64_value)
            .unwrap_or(0);
        let name = grpc_code_name(number);
        Self {
            status: http_status_for_code(name),
            code: Some(name.to_owned()),
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        }
    }

    /// Whether this is a not-found error: HTTP 404 or status `NOT_FOUND`.
    pub fn is_not_found(&self) -> bool {
        self.status == 404 || self.code.as_deref() == Some("NOT_FOUND")
    }

    /// Whether this is an invalid-argument error: HTTP 400 with status `INVALID_ARGUMENT` or no
    /// status.
    pub fn is_invalid_argument(&self) -> bool {
        self.status == 400 && matches!(self.code.as_deref(), None | Some("INVALID_ARGUMENT"))
    }

    /// The gRPC code name this error displays: from [`code`](Self::code) when it is a
    /// Google status name, else from the HTTP status per Google's mapping (400
    /// `InvalidArgument`, 401 `Unauthenticated`, 403 `PermissionDenied`, 404 `NotFound`, 409
    /// `AlreadyExists`, 429 `ResourceExhausted`, 499 `Canceled`, 500 `Internal`, 501
    /// `Unimplemented`, 503 `Unavailable`, 504 `DeadlineExceeded`, other `Unknown`).
    pub fn go_code_name(&self) -> &'static str {
        self.code
            .as_deref()
            .and_then(go_name_for_code)
            .unwrap_or_else(|| go_name_for_status(self.status))
    }
}

impl std::fmt::Display for VertexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "rpc error: code = {} desc = {}",
            self.go_code_name(),
            self.message
        )
    }
}

impl StdError for VertexError {}

/// Wraps `err` as `{prefix}: {err}`, boxed; `err` stays the `source()`.
pub(crate) fn wrap(prefix: &str, err: impl Into<BoxError>) -> BoxError {
    Box::new(GoWrapped::new(prefix, err))
}

/// The first [`VertexError`] in `err`'s `source()` chain, `err` itself included.
pub(crate) fn find_vertex_error<'a>(err: &'a (dyn StdError + 'static)) -> Option<&'a VertexError> {
    let mut cur = Some(err);
    while let Some(e) = cur {
        if let Some(v) = e.downcast_ref::<VertexError>() {
            return Some(v);
        }
        cur = e.source();
    }
    None
}

/// A session failure: `InvalidRequest(err.to_string())` when the chain holds an
/// invalid-argument [`VertexError`] (the stand-in for a gRPC `InvalidArgument`), else
/// `Backend(err)`.
pub(crate) fn session_error(err: BoxError) -> SessionError {
    if find_vertex_error(err.as_ref()).is_some_and(VertexError::is_invalid_argument) {
        SessionError::InvalidRequest(err.to_string())
    } else {
        SessionError::Backend(err)
    }
}

/// A memory failure: `Backend(err)`.
pub(crate) fn memory_error(err: BoxError) -> MemoryError {
    MemoryError::Backend(err)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    fn http(status: u16, code: Option<&str>, message: &str) -> VertexError {
        VertexError {
            status,
            code: code.map(str::to_owned),
            message: message.to_owned(),
        }
    }

    #[test]
    fn from_response_reads_googles_error_body() {
        let body = br#"{"error":{"code":404,"message":"Session not found.","status":"NOT_FOUND"}}"#;
        assert_eq!(
            VertexError::from_response(404, body),
            http(404, Some("NOT_FOUND"), "Session not found.")
        );
        // No status, no message.
        assert_eq!(
            VertexError::from_response(400, br#"{"error":{"code":400}}"#),
            http(400, None, "")
        );
        assert_eq!(
            VertexError::from_response(400, br#"{"error":{"status":"","message":"m"}}"#),
            http(400, None, "m")
        );
    }

    #[test]
    fn from_response_falls_back_to_the_body_prefix() {
        assert_eq!(
            VertexError::from_response(502, b"<html>bad gateway</html>"),
            http(502, None, "<html>bad gateway</html>")
        );
        assert_eq!(
            VertexError::from_response(500, br#"{"error":"flat"}"#),
            http(500, None, r#"{"error":"flat"}"#)
        );
        assert_eq!(VertexError::from_response(503, b""), http(503, None, ""));
        let long = vec![b'x'; 5000];
        assert_eq!(
            VertexError::from_response(500, &long).message,
            gadk_model::body_prefix(&long)
        );
    }

    #[test]
    fn from_operation_error_maps_the_grpc_code() {
        assert_eq!(
            VertexError::from_operation_error(&json!({"code": 5, "message": "gone"})),
            http(404, Some("NOT_FOUND"), "gone")
        );
        assert_eq!(
            VertexError::from_operation_error(&json!({"code": 3, "message": "bad"})),
            http(400, Some("INVALID_ARGUMENT"), "bad")
        );
        assert_eq!(
            VertexError::from_operation_error(&json!({"code": "13"})),
            http(500, Some("INTERNAL"), "")
        );
        assert_eq!(
            VertexError::from_operation_error(&json!({"code": 99, "message": "x"})),
            http(500, Some("UNKNOWN"), "x")
        );
        // protojson omits a zero code.
        assert_eq!(
            VertexError::from_operation_error(&json!({"message": "x"})),
            http(200, Some("OK"), "x")
        );
        assert_eq!(
            VertexError::from_operation_error(&json!({"code": 9, "message": "cond"})).to_string(),
            "rpc error: code = FailedPrecondition desc = cond"
        );
    }

    /// Display is Go's `status.Error` text (`rpc error: code = %s desc = %s`).
    #[test]
    fn display_is_gos_grpc_status_text() {
        for (code, go) in [
            ("OK", "OK"),
            ("CANCELLED", "Canceled"),
            ("UNKNOWN", "Unknown"),
            ("INVALID_ARGUMENT", "InvalidArgument"),
            ("DEADLINE_EXCEEDED", "DeadlineExceeded"),
            ("NOT_FOUND", "NotFound"),
            ("ALREADY_EXISTS", "AlreadyExists"),
            ("PERMISSION_DENIED", "PermissionDenied"),
            ("RESOURCE_EXHAUSTED", "ResourceExhausted"),
            ("FAILED_PRECONDITION", "FailedPrecondition"),
            ("ABORTED", "Aborted"),
            ("OUT_OF_RANGE", "OutOfRange"),
            ("UNIMPLEMENTED", "Unimplemented"),
            ("INTERNAL", "Internal"),
            ("UNAVAILABLE", "Unavailable"),
            ("DATA_LOSS", "DataLoss"),
            ("UNAUTHENTICATED", "Unauthenticated"),
        ] {
            // The status name wins over the HTTP status.
            let err = http(418, Some(code), "msg");
            assert_eq!(err.go_code_name(), go, "{code}");
            assert_eq!(
                err.to_string(),
                format!("rpc error: code = {go} desc = msg")
            );
        }
        for (status, go) in [
            (400, "InvalidArgument"),
            (401, "Unauthenticated"),
            (403, "PermissionDenied"),
            (404, "NotFound"),
            (409, "AlreadyExists"),
            (429, "ResourceExhausted"),
            (499, "Canceled"),
            (500, "Internal"),
            (501, "Unimplemented"),
            (503, "Unavailable"),
            (504, "DeadlineExceeded"),
            (502, "Unknown"),
            (418, "Unknown"),
        ] {
            assert_eq!(http(status, None, "m").go_code_name(), go, "{status}");
        }
        // An unrecognized status name falls back to the HTTP status.
        assert_eq!(
            http(404, Some("SOMETHING_NEW"), "m").go_code_name(),
            "NotFound"
        );
    }

    #[test]
    fn code_tables_round_trip() {
        for (number, name, status) in [
            (0, "OK", 200),
            (1, "CANCELLED", 499),
            (2, "UNKNOWN", 500),
            (3, "INVALID_ARGUMENT", 400),
            (4, "DEADLINE_EXCEEDED", 504),
            (5, "NOT_FOUND", 404),
            (6, "ALREADY_EXISTS", 409),
            (7, "PERMISSION_DENIED", 403),
            (8, "RESOURCE_EXHAUSTED", 429),
            (9, "FAILED_PRECONDITION", 400),
            (10, "ABORTED", 409),
            (11, "OUT_OF_RANGE", 400),
            (12, "UNIMPLEMENTED", 501),
            (13, "INTERNAL", 500),
            (14, "UNAVAILABLE", 503),
            (15, "DATA_LOSS", 500),
            (16, "UNAUTHENTICATED", 401),
        ] {
            assert_eq!(grpc_code_name(number), name);
            assert_eq!(http_status_for_code(name), status, "{name}");
        }
        assert_eq!(grpc_code_name(17), "UNKNOWN");
        assert_eq!(grpc_code_name(-1), "UNKNOWN");
        assert_eq!(http_status_for_code("NOPE"), 500);
    }

    #[test]
    fn not_found_and_invalid_argument_predicates() {
        assert!(http(404, None, "").is_not_found());
        assert!(http(404, Some("NOT_FOUND"), "").is_not_found());
        assert!(http(200, Some("NOT_FOUND"), "").is_not_found());
        assert!(!http(403, Some("PERMISSION_DENIED"), "").is_not_found());

        assert!(http(400, None, "").is_invalid_argument());
        assert!(http(400, Some("INVALID_ARGUMENT"), "").is_invalid_argument());
        assert!(!http(400, Some("FAILED_PRECONDITION"), "").is_invalid_argument());
        assert!(!http(400, Some("OUT_OF_RANGE"), "").is_invalid_argument());
        assert!(!http(500, Some("INVALID_ARGUMENT"), "").is_invalid_argument());
        assert!(!http(404, None, "").is_invalid_argument());
    }

    #[test]
    fn wrap_and_find_follow_the_chain() {
        let inner = http(400, Some("INVALID_ARGUMENT"), "author is required");
        let err = wrap(
            "failed to append event",
            wrap("error appending event", inner.clone()),
        );
        assert_eq!(
            err.to_string(),
            "failed to append event: error appending event: rpc error: code = InvalidArgument \
             desc = author is required"
        );
        assert_eq!(find_vertex_error(err.as_ref()), Some(&inner));
        assert_eq!(find_vertex_error(&inner), Some(&inner));
        let plain = wrap("x", "no vertex error here");
        assert_eq!(find_vertex_error(plain.as_ref()), None);
    }

    #[test]
    fn session_error_maps_invalid_argument_to_invalid_request() {
        let invalid = wrap(
            "failed to append event",
            http(400, Some("INVALID_ARGUMENT"), "bad"),
        );
        match session_error(invalid) {
            SessionError::InvalidRequest(text) => assert_eq!(
                text,
                "failed to append event: rpc error: code = InvalidArgument desc = bad"
            ),
            other => panic!("want InvalidRequest, got {other:?}"),
        }
        let denied = wrap(
            "failed to get session",
            http(403, Some("PERMISSION_DENIED"), "no"),
        );
        let err = session_error(denied);
        assert!(matches!(err, SessionError::Backend(_)));
        assert_eq!(
            err.to_string(),
            "failed to get session: rpc error: code = PermissionDenied desc = no"
        );
        assert!(matches!(
            session_error("conversion failed".into()),
            SessionError::Backend(_)
        ));
    }

    #[test]
    fn memory_error_is_backend() {
        let err = memory_error(wrap(
            "v.client.RetrieveMemories failed",
            http(500, Some("INTERNAL"), "boom"),
        ));
        assert!(matches!(err, MemoryError::Backend(_)));
        assert_eq!(
            err.to_string(),
            "v.client.RetrieveMemories failed: rpc error: code = Internal desc = boom"
        );
    }

    #[test]
    fn errors_are_send_sync_static() {
        fn assert_bounds<T: StdError + Send + Sync + 'static>() {}
        assert_bounds::<VertexError>();
    }
}
