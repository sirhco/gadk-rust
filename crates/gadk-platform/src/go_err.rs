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

//! Error wrapping with a prefix ([`GoWrapped`]) and OS error texts ([`go_errno_text`]), shared
//! by the gadk crates that build error chains.

use std::error::Error as StdError;
use std::io;

/// An error that displays `{prefix}: {source}` and keeps `source` as its `source()`, so walks
/// of the error chain find the cause.
#[derive(Debug)]
pub struct GoWrapped {
    prefix: String,
    source: Box<dyn StdError + Send + Sync>,
}

impl GoWrapped {
    /// Wraps `source` under `prefix`.
    pub fn new(
        prefix: impl Into<String>,
        source: impl Into<Box<dyn StdError + Send + Sync>>,
    ) -> Self {
        Self {
            prefix: prefix.into(),
            source: source.into(),
        }
    }
}

impl std::fmt::Display for GoWrapped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.prefix, self.source)
    }
}

impl StdError for GoWrapped {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(self.source.as_ref())
    }
}

/// The text of the OS error `err`: the C library's message for the OS error code with its first
/// letter lowercased (e.g. `no such file or directory`, `permission denied`, `is a
/// directory`); an error without an OS code keeps its own text.
pub fn go_errno_text(err: &io::Error) -> String {
    let Some(code) = err.raw_os_error() else {
        return err.to_string();
    };
    let text = io::Error::from_raw_os_error(code).to_string();
    let text = match text.rfind(" (os error ") {
        Some(i) => &text[..i],
        None => text.as_str(),
    };
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error as StdError;
    use std::io;

    use super::{GoWrapped, go_errno_text};

    #[derive(Debug)]
    struct Cause;

    impl std::fmt::Display for Cause {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("root cause")
        }
    }

    impl StdError for Cause {}

    #[test]
    fn display_is_prefix_colon_source() {
        assert_eq!(
            GoWrapped::new("failed to get session", Cause).to_string(),
            "failed to get session: root cause"
        );
        assert_eq!(GoWrapped::new("", "x").to_string(), ": x");
        let nested = GoWrapped::new("outer", GoWrapped::new("inner", "leaf"));
        assert_eq!(nested.to_string(), "outer: inner: leaf");
    }

    #[test]
    fn source_is_the_wrapped_cause() {
        let err = GoWrapped::new("outer", GoWrapped::new("inner", Cause));
        let inner = err.source().expect("outer has a source");
        assert_eq!(inner.to_string(), "inner: root cause");
        assert!(inner.is::<GoWrapped>());
        let root = inner.source().expect("inner has a source");
        assert!(root.is::<Cause>());
        assert!(root.source().is_none());
    }

    #[test]
    fn is_send_sync_static() {
        fn assert_bounds<T: StdError + Send + Sync + 'static>() {}
        assert_bounds::<GoWrapped>();
    }

    #[test]
    fn errno_text_is_lowercased_strerror() {
        assert_eq!(
            go_errno_text(&io::Error::from_raw_os_error(2)),
            "no such file or directory"
        );
        assert_eq!(
            go_errno_text(&io::Error::from_raw_os_error(13)),
            "permission denied"
        );
        assert_eq!(
            go_errno_text(&io::Error::from_raw_os_error(21)),
            "is a directory"
        );
        assert_eq!(go_errno_text(&io::Error::other("custom")), "custom");
    }
}
