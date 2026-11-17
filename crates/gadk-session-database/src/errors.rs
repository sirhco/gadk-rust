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

//! Error texts of the form `<prefix>: <cause>`, as a [`SessionError::Backend`].

use std::error::Error as StdError;
use std::fmt;

use gadk_session::SessionError;

/// A wrapped error: `Display` is the whole message, `source` the cause.
#[derive(Debug)]
pub(crate) struct GoError {
    msg: String,
    source: Option<Box<dyn StdError + Send + Sync>>,
}

impl fmt::Display for GoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.msg)
    }
}

impl StdError for GoError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source
            .as_deref()
            .map(|e| e as &(dyn StdError + 'static))
    }
}

/// Wraps `cause` under `prefix`: displays `<prefix>: <cause>` and keeps `cause` as the source.
pub(crate) fn wrap(
    prefix: &str,
    cause: impl Into<Box<dyn StdError + Send + Sync>>,
) -> SessionError {
    let cause = cause.into();
    SessionError::Backend(Box::new(GoError {
        msg: format!("{prefix}: {cause}"),
        source: Some(cause),
    }))
}

/// An error with just a message (no cause).
pub(crate) fn message(msg: impl Into<String>) -> SessionError {
    SessionError::Backend(Box::new(GoError {
        msg: msg.into(),
        source: None,
    }))
}

/// A database error returned unwrapped (transaction begin/commit, connect).
pub(crate) fn raw(err: sqlx::Error) -> SessionError {
    SessionError::Backend(Box::new(err))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn wrap_renders_go_text_and_keeps_the_cause() {
        let e = wrap("failed to save event", message("disk full"));
        assert_eq!(e.to_string(), "failed to save event: disk full");
        let SessionError::Backend(inner) = &e else {
            panic!("not Backend")
        };
        assert_eq!(inner.source().unwrap().to_string(), "disk full");
    }

    #[test]
    fn nested_wraps_compose_like_go() {
        let e = wrap(
            "error on create session",
            wrap("failed to fetch app state", message("boom")),
        );
        assert_eq!(
            e.to_string(),
            "error on create session: failed to fetch app state: boom"
        );
    }
}
