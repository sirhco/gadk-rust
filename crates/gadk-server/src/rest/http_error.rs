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

//! Go's response helpers, byte for byte: `net/http` `Error` and `NotFound`, ADK-Go
//! `controllers.EncodeJSONResponse`, and the `NewErrorHandler` rendering of a `statusError`.

// Used by the controllers (plan 5a Tasks 4-12).
#![allow(dead_code)]

use axum::body::Body;
use http::header::{CONTENT_TYPE, X_CONTENT_TYPE_OPTIONS};
use http::{HeaderValue, StatusCode};
use serde::Serialize;

use super::routing::Response;

/// Go `net/http.Error`: `msg` and a newline as `text/plain; charset=utf-8`, with
/// `X-Content-Type-Options: nosniff`.
pub fn http_error(status: StatusCode, msg: &str) -> Response {
    let mut resp = Response::new(Body::from(format!("{msg}\n")));
    *resp.status_mut() = status;
    let h = resp.headers_mut();
    h.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    h.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    resp
}

/// Go `net/http.NotFound`: 404 `404 page not found`.
pub fn not_found() -> Response {
    http_error(StatusCode::NOT_FOUND, "404 page not found")
}

/// Go `controllers.EncodeJSONResponse`: `value` as Go's `json.Encoder` writes it (HTML-escaped,
/// newline-terminated; [`gadk_model::to_go_json`]) with `Content-Type: application/json;
/// charset=UTF-8`. A value that cannot be encoded keeps the status and has the encoder's error
/// text as its body (Go's `http.Error` after the header was written).
pub fn encode_json_response<T: Serialize + ?Sized>(value: &T, status: StatusCode) -> Response {
    let body = match gadk_model::to_go_json(value) {
        Ok(mut b) => {
            b.push(b'\n');
            b
        }
        Err(e) => format!("{e}\n").into_bytes(),
    };
    let mut resp = Response::new(Body::from(body));
    *resp.status_mut() = status;
    resp.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=UTF-8"),
    );
    resp
}

/// Go `controllers.statusError`: an error and the status it answers with.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct StatusError {
    /// The response status.
    pub status: StatusCode,
    /// Go `Err.Error()`.
    pub message: String,
}

impl StatusError {
    /// Go `newStatusError(err, code)`.
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

/// Go `controllers.NewErrorHandler`'s rendering of a returned `statusError`:
/// `http.Error(w, err.Error(), err.Status())`.
pub fn status_error_response(err: &StatusError) -> Response {
    http_error(err.status, &err.message)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    /// Status, headers (name → value) and body of `resp`.
    async fn parts(resp: Response) -> (u16, Vec<(String, String)>, String) {
        let status = resp.status().as_u16();
        let headers = resp
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap().to_owned()))
            .collect();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, headers, String::from_utf8(body.to_vec()).unwrap())
    }

    /// Bytes of Go 1.27 `http.Error(w, "failed to decode request: boom", 400)` on an
    /// `httptest.ResponseRecorder`.
    #[tokio::test]
    async fn http_error_matches_go() {
        let got = parts(http_error(
            StatusCode::BAD_REQUEST,
            "failed to decode request: boom",
        ))
        .await;
        assert_eq!(
            got,
            (
                400,
                vec![
                    ("content-type".into(), "text/plain; charset=utf-8".into()),
                    ("x-content-type-options".into(), "nosniff".into()),
                ],
                "failed to decode request: boom\n".into()
            )
        );
    }

    #[tokio::test]
    async fn not_found_matches_go() {
        let (status, headers, body) = parts(not_found()).await;
        assert_eq!(status, 404);
        assert_eq!(body, "404 page not found\n");
        assert_eq!(headers[0].1, "text/plain; charset=utf-8");
    }

    #[tokio::test]
    async fn status_error_renders_like_new_error_handler() {
        let err = StatusError::new(StatusCode::NOT_FOUND, "failed to get session: not found");
        assert_eq!(err.to_string(), "failed to get session: not found");
        let (status, _, body) = parts(status_error_response(&err)).await;
        assert_eq!(
            (status, body.as_str()),
            (404, "failed to get session: not found\n")
        );
    }

    /// Go `json.NewEncoder(w).Encode(map[string]any{"a": "<&>", "b": 1.0})`: sorted keys,
    /// HTML-escaped strings, Go float text, a trailing newline.
    #[tokio::test]
    async fn encode_json_response_matches_go_encoder() {
        let value = BTreeMap::from([
            ("b", serde_json::json!(1.0)),
            ("a", serde_json::json!("<&>")),
        ]);
        let (status, headers, body) =
            parts(encode_json_response(&value, StatusCode::CREATED)).await;
        assert_eq!(status, 201);
        assert_eq!(
            headers,
            vec![(
                "content-type".to_owned(),
                "application/json; charset=UTF-8".to_owned()
            )]
        );
        assert_eq!(body, "{\"a\":\"\\u003c\\u0026\\u003e\",\"b\":1}\n");
    }
}
