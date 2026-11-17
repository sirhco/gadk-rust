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

//! The transport's provider-neutral error.

use crate::error::ModelError;

/// Why [`RestTransport::send`](super::RestTransport::send) failed, or (boxed in
/// [`ModelError::Other`]) why a streaming body under a deadline ended.
///
/// `Display` carries no provider prefix: the transport's own texts
/// (`net/http: timeout awaiting response headers`, `context deadline exceeded`), else the
/// cause's `Display` (a [`ModelError`]'s, e.g. `transport error: …`). Each provider maps the
/// variants to its own error type and text instead of using this `Display`.
#[derive(Debug, thiserror::Error)]
pub enum RestError {
    /// The last attempt received no response: [`HttpClient::send`](crate::HttpClient::send)
    /// failed (connection error, invalid request, …). The cause is kept as it is so the
    /// provider can classify it and strip URLs.
    #[error("{0}")]
    Transport(ModelError),
    /// The last attempt's [`RequestHeaders::headers`](super::RequestHeaders::headers) failed
    /// (for example a credential fetch); the provider's own error, boxed.
    #[error("{0}")]
    Headers(Box<dyn std::error::Error + Send + Sync>),
    /// The last attempt's response headers did not arrive within the transport's
    /// `response_header_timeout`.
    #[error("net/http: timeout awaiting response headers")]
    ResponseHeaderTimeout,
    /// The call's deadline passed: the last attempt's
    /// [`CallTimeout::PerAttempt`](super::CallTimeout::PerAttempt) deadline, or the
    /// [`CallTimeout::Call`](super::CallTimeout::Call) deadline at any point of the call.
    #[error("context deadline exceeded")]
    DeadlineExceeded,
    /// The final response's body failed while being read (the body of every status ≥ 400,
    /// and of a non-streaming 2xx); `parts` is that response's head, `source` the body error.
    #[error("{source}")]
    DroppedBody {
        /// The response head (status and headers), boxed to keep the error small.
        parts: Box<http::response::Parts>,
        /// The body stream's error.
        source: ModelError,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_is_the_cause_or_gos_runtime_text() {
        let io = std::io::Error::other("connection refused");
        assert_eq!(
            RestError::Transport(ModelError::Transport(Box::new(io))).to_string(),
            "transport error: connection refused"
        );
        assert_eq!(RestError::Headers("boom".into()).to_string(), "boom");
        assert_eq!(
            RestError::ResponseHeaderTimeout.to_string(),
            "net/http: timeout awaiting response headers"
        );
        assert_eq!(
            RestError::DeadlineExceeded.to_string(),
            "context deadline exceeded"
        );
        let (parts, ()) = http::Response::new(()).into_parts();
        let io = std::io::Error::other("unexpected EOF");
        assert_eq!(
            RestError::DroppedBody {
                parts: Box::new(parts),
                source: ModelError::Transport(Box::new(io)),
            }
            .to_string(),
            "transport error: unexpected EOF"
        );
    }

    #[test]
    fn rest_error_is_send_sync_static() {
        fn assert_bounds<T: std::error::Error + Send + Sync + 'static>() {}
        assert_bounds::<RestError>();
    }
}
