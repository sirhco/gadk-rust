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

//! Model errors.

use serde::Deserialize;
use serde_json::Value;

/// Maximum number of body bytes kept by [`body_prefix`].
const BODY_PREFIX_LIMIT: usize = 2048;

/// Errors produced by model backends.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ModelError {
    /// The API returned an error response.
    #[error("model API error {code} {status}: {message}")]
    Api {
        /// HTTP / API error code.
        code: u16,
        /// API status string (e.g. `RESOURCE_EXHAUSTED`); empty if unknown.
        status: String,
        /// Error message.
        message: String,
        /// Error details as returned by the API.
        details: Vec<Value>,
    },
    /// The request could not be sent or the response could not be read.
    #[error("transport error: {0}")]
    Transport(Box<dyn std::error::Error + Send + Sync>),
    /// The response body could not be decoded.
    #[error("decode error: {message}")]
    Decode {
        /// What went wrong.
        message: String,
        /// The start of the offending body (see [`body_prefix`]).
        body_prefix: String,
    },
    /// The model returned no response.
    #[error("empty response")]
    EmptyResponse,
    /// Invalid configuration.
    #[error("config error: {0}")]
    Config(String),
    /// Any other error.
    #[error("{0}")]
    Other(Box<dyn std::error::Error + Send + Sync>),
}

#[derive(Deserialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Deserialize)]
struct ErrorBody {
    #[serde(default)]
    code: Option<i64>,
    #[serde(default)]
    status: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    details: Vec<Value>,
}

/// Builds a [`ModelError::Api`] from an HTTP error status and body.
///
/// Parses Google's `{"error":{"code","status","message","details"}}` envelope; when the
/// body does not parse, `status` is empty and `message` is [`body_prefix`] of the body.
pub fn api_error_from_body(status: u16, body: &[u8]) -> ModelError {
    match serde_json::from_slice::<ErrorEnvelope>(body) {
        Ok(ErrorEnvelope { error }) => ModelError::Api {
            code: error
                .code
                .and_then(|c| u16::try_from(c).ok())
                .filter(|c| *c != 0)
                .unwrap_or(status),
            status: error.status,
            message: error.message,
            details: error.details,
        },
        Err(_) => ModelError::Api {
            code: status,
            status: String::new(),
            message: body_prefix(body),
            details: Vec::new(),
        },
    }
}

/// The first ≤ 2048 bytes of `body` as lossy UTF-8, never cutting a multi-byte character.
pub fn body_prefix(body: &[u8]) -> String {
    let mut end = body.len().min(BODY_PREFIX_LIMIT);
    if end < body.len() {
        // Back off over UTF-8 continuation bytes (at most 3) so the cut lands on a
        // character boundary.
        let mut backed = 0;
        while end > 0 && backed < 3 && body.get(end).is_some_and(|b| b & 0xC0 == 0x80) {
            end -= 1;
            backed += 1;
        }
    }
    String::from_utf8_lossy(body.get(..end).unwrap_or_default()).into_owned()
}
