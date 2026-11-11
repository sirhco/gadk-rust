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
    /// An [`Error::A2A`] is unwrapped. Any other error keeps the code and details of the first
    /// A2A error in its source chain with its own text (a2a-go's `ToJSONRPCError`: `errors.Is`
    /// against the code sentinels, `errors.As` for the details, `err.Error()` for the message),
    /// else it is an internal error with its text.
    fn from(err: Error) -> Self {
        match err {
            Error::A2A(e) => e,
            other => to_a2a_error(&other),
        }
    }
}

/// The [`a2a::A2AError`] for `err`, as `From<Error>` maps it, without consuming `err`.
pub(crate) fn to_a2a_error(err: &Error) -> a2a::A2AError {
    if let Error::A2A(e) = err {
        return e.clone();
    }
    match chained_a2a_error(err) {
        Some(inner) => a2a::A2AError {
            code: inner.code,
            message: err.to_string(),
            details: inner.details.clone(),
        },
        None => a2a::A2AError::internal(err.to_string()),
    }
}

/// The first A2A error in `err`'s source chain (as an [`a2a::A2AError`] or an [`Error::A2A`]).
fn chained_a2a_error<'a>(err: &'a (dyn std::error::Error + 'static)) -> Option<&'a a2a::A2AError> {
    let mut current = err.source();
    while let Some(err) = current {
        if let Some(a2a_err) = err.downcast_ref::<a2a::A2AError>() {
            return Some(a2a_err);
        }
        if let Some(Error::A2A(a2a_err)) = err.downcast_ref::<Error>() {
            return Some(a2a_err);
        }
        current = err.source();
    }
    None
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

    /// The whole of an [`a2a::A2AError`] (it has no `PartialEq`).
    fn parts(e: &a2a::A2AError) -> (i32, &str, &Option<Vec<a2a::errordetails::TypedDetail>>) {
        (e.code, &e.message, &e.details)
    }

    #[test]
    fn into_a2a_error_unwraps_a2a_and_maps_the_rest_to_internal() {
        let original = a2a::A2AError::task_not_found("t1").with_details(vec![
            a2a::errordetails::TypedDetail::error_info("TASK_NOT_FOUND", "a2a-protocol.org", None),
        ]);
        let back: a2a::A2AError = Error::from(original.clone()).into();
        assert_eq!(parts(&back), parts(&original));

        let internal: a2a::A2AError = Error::wrap("prefix", Error::msg("cause")).into();
        let expected = a2a::A2AError::internal("prefix: cause");
        assert_eq!(parts(&internal), parts(&expected));
    }

    #[test]
    fn a_wrapped_a2a_error_keeps_its_code_and_details_with_the_outer_text() {
        let inner = a2a::A2AError::invalid_params("bad input").with_details(vec![
            a2a::errordetails::TypedDetail::error_info("INVALID_PARAMS", "a2a-protocol.org", None),
        ]);
        // Go `fmt.Errorf("before execute: %w", fmt.Errorf("check: %w", a2aErr))`.
        let nested = Error::wrap(
            "before execute",
            Error::wrap("check", Error::A2A(inner.clone())),
        );
        let got: a2a::A2AError = nested.into();
        let want = a2a::A2AError {
            code: inner.code,
            message: "before execute: check: bad input".into(),
            details: inner.details.clone(),
        };
        assert_eq!(parts(&got), parts(&want));

        // A bare a2a-rs error as the source.
        let got: a2a::A2AError = Error::wrap("after execute", inner.clone()).into();
        assert_eq!(
            (got.code, got.message.as_str()),
            (inner.code, "after execute: bad input")
        );
    }
}
