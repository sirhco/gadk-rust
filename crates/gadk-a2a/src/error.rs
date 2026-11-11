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

//! The crate error (Go's `error` values of `server/adka2a/v2` and `agent/remoteagent/v2`).

/// An error from a conversion, the executor or the remote agent. Display texts are Go's.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A message whose text is Go's verbatim (config validation, card source checks, conversions).
    #[error("{0}")]
    Message(String),
    /// Go `fmt.Errorf("<prefix>: %w", err)`.
    #[error("{prefix}: {source}")]
    Wrapped {
        /// The text before `: `.
        prefix: String,
        /// The wrapped error.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// An A2A protocol error.
    #[error(transparent)]
    A2A(#[from] a2a::A2AError),
    /// An agent, callback or runner error.
    #[error(transparent)]
    Agent(#[from] gadk_agent::AgentError),
    /// A session service error.
    #[error(transparent)]
    Session(#[from] gadk_session::SessionError),
    /// A JSON encoding or decoding error.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl Error {
    /// [`Error::Message`] with `text`.
    pub fn msg(text: impl Into<String>) -> Self {
        Self::Message(text.into())
    }

    /// [`Error::Wrapped`]: Go `fmt.Errorf("<prefix>: %w", source)`.
    pub fn wrap(
        prefix: impl Into<String>,
        source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Self {
        Self::Wrapped {
            prefix: prefix.into(),
            source: source.into(),
        }
    }
}

impl From<Error> for a2a::A2AError {
    /// An [`Error::A2A`] is unwrapped; any other error becomes an internal error with its text.
    fn from(err: Error) -> Self {
        match err {
            Error::A2A(e) => e,
            other => a2a::A2AError::internal(other.to_string()),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::error::Error as _;

    use super::*;

    #[test]
    fn message_displays_its_text() {
        assert_eq!(
            Error::msg("agent card not provided").to_string(),
            "agent card not provided"
        );
    }

    #[test]
    fn wrapped_displays_prefix_and_source_and_keeps_the_source() {
        let err = Error::wrap("failed to create a client", Error::msg("boom"));
        assert_eq!(err.to_string(), "failed to create a client: boom");
        assert_eq!(err.source().unwrap().to_string(), "boom");
    }

    #[test]
    fn transparent_variants_display_their_source() {
        let err: Error = gadk_agent::AgentError::Config("bad config".into()).into();
        assert_eq!(err.to_string(), "bad config");
        let err: Error = gadk_session::SessionError::NotFound.into();
        assert_eq!(err.to_string(), "session not found");
        let err: Error = serde_json::from_str::<serde_json::Value>("{")
            .unwrap_err()
            .into();
        assert!(matches!(err, Error::Json(_)));
    }

    #[test]
    fn into_a2a_error_unwraps_a2a_and_maps_the_rest_to_internal() {
        let original = a2a::A2AError::task_not_found("t1");
        let back: a2a::A2AError = Error::from(original.clone()).into();
        assert_eq!((back.code, back.message), (original.code, original.message));

        let internal: a2a::A2AError = Error::wrap("prefix", Error::msg("cause")).into();
        let expected = a2a::A2AError::internal("prefix: cause");
        assert_eq!(
            (internal.code, internal.message),
            (expected.code, expected.message)
        );
    }
}
