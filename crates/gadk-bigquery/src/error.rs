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

//! The error type of the BigQuery client.

use serde_json::Value;

/// The most bytes of a non-JSON error body kept in a message.
const EXCERPT_LIMIT: usize = 1024;

/// Why a BigQuery call failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum BigQueryError {
    /// No request credentials could be produced.
    #[error("credentials: {0}")]
    Credentials(String),
    /// The request could not be built or sent, or the reply could not be read.
    #[error("BigQuery: {0}")]
    Http(String),
    /// A non-2xx reply.
    #[error("BigQuery: HTTP {status}: {message}")]
    Api {
        /// The HTTP status.
        status: u16,
        /// `error.message` of the reply, or an excerpt of the body when it is not JSON.
        message: String,
        /// `error.errors[0].reason` of the reply, such as `invalidQuery` or `notFound`.
        reason: Option<String>,
    },
    /// The query job finished with an error or did not finish in time.
    #[error("BigQuery job failed: {0}")]
    Job(String),
    /// The reply is not the expected JSON.
    #[error("BigQuery: unexpected response: {0}")]
    Decode(String),
    /// The BigQuery session could not be stored in the agent session's state.
    #[error("session state: {0}")]
    State(String),
}

impl BigQueryError {
    /// Whether the error says the BigQuery session no longer exists (expired or not found), so a
    /// new session should be created.
    pub fn is_session_gone(&self) -> bool {
        let text = match self {
            Self::Api { message, .. } => message,
            Self::Job(message) => message,
            _ => return false,
        }
        .to_ascii_lowercase();
        text.contains("session") && (text.contains("not found") || text.contains("expired"))
    }
}

/// The error for a non-2xx reply with `body`.
#[allow(dead_code)]
pub(crate) fn api_error(status: u16, body: &[u8]) -> BigQueryError {
    let parsed: Option<Value> = serde_json::from_slice(body).ok();
    let message = parsed
        .as_ref()
        .and_then(|v| v.pointer("/error/message"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| excerpt(body));
    let reason = parsed
        .as_ref()
        .and_then(|v| v.pointer("/error/errors/0/reason"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    BigQueryError::Api {
        status,
        message,
        reason,
    }
}

/// At most [`EXCERPT_LIMIT`] bytes of `body` (lossy UTF-8), cut on a char boundary.
fn excerpt(body: &[u8]) -> String {
    let mut text = String::from_utf8_lossy(&body[..body.len().min(EXCERPT_LIMIT + 4)]).into_owned();
    text.truncate(text.floor_char_boundary(EXCERPT_LIMIT));
    text
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn api_error_reads_message_and_reason() {
        let body = br#"{"error":{"code":400,"message":"Syntax error: Unexpected end of script","errors":[{"reason":"invalidQuery"}]}}"#;
        assert_eq!(
            api_error(400, body),
            BigQueryError::Api {
                status: 400,
                message: "Syntax error: Unexpected end of script".into(),
                reason: Some("invalidQuery".into()),
            }
        );
    }

    #[test]
    fn api_error_falls_back_to_a_body_excerpt() {
        let err = api_error(502, b"Bad Gateway");
        assert_eq!(err.to_string(), "BigQuery: HTTP 502: Bad Gateway");
    }

    #[test]
    fn session_gone_matches_expired_and_not_found() {
        let gone = |m: &str| {
            api_error(
                400,
                format!(r#"{{"error":{{"message":"{m}"}}}}"#).as_bytes(),
            )
        };
        assert!(gone("Session abc was not found").is_session_gone());
        assert!(gone("Session has expired").is_session_gone());
        assert!(BigQueryError::Job("session not found".into()).is_session_gone());
        assert!(!gone("Table not found").is_session_gone());
        assert!(!BigQueryError::Credentials("session expired".into()).is_session_gone());
    }
}
