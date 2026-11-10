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

//! Go's `%w` error wrapping ([`GoWrapped`]), shared by the gadk crates that reproduce Go's
//! error chains.

use std::error::Error as StdError;

/// Go `fmt.Errorf("{prefix}: %w", source)`: displays `{prefix}: {source}` and keeps `source`
/// as its `source()`, so `errors.Is` / `errors.As` style walks of the chain find the cause.
#[derive(Debug)]
pub struct GoWrapped {
    prefix: String,
    source: Box<dyn StdError + Send + Sync>,
}

impl GoWrapped {
    /// Wraps `source` under `prefix`, as Go's `fmt.Errorf("{prefix}: %w", source)` does.
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

#[cfg(test)]
mod tests {
    use std::error::Error as StdError;

    use super::GoWrapped;

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
}
