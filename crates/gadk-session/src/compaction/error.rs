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

//! [`CompactionError`] and its message prefix [`ERR_COMPACTION`].

/// The prefix of every [`CompactionError`]'s message.
pub const ERR_COMPACTION: &str = "context compaction failed";

/// A compaction failure rather than a failure of the turn itself.
///
/// Compaction is bookkeeping: the turn's events are already persisted before it runs, so a
/// failure costs a smaller prompt later, not the user's answer. It still surfaces; a caller that
/// would rather log it than fail the turn can tell the two apart with
/// `AgentError::is_compaction` (gadk-agent).
#[derive(Debug)]
pub struct CompactionError {
    detail: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl CompactionError {
    /// Display `context compaction failed: <detail>`, no source.
    pub fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
            source: None,
        }
    }

    /// Wraps `source` with context: Display
    /// `context compaction failed: <context>: <source>`, and `source()` is `source`.
    pub fn wrap(
        context: impl Into<String>,
        source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Self {
        let source = source.into();
        Self {
            detail: format!("{}: {}", context.into(), source),
            source: Some(source),
        }
    }

    /// The text after the `context compaction failed: ` prefix.
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl std::fmt::Display for CompactionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{ERR_COMPACTION}: {}", self.detail)
    }
}

impl std::error::Error for CompactionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|e| e as &(dyn std::error::Error + 'static))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::error::Error as _;

    #[test]
    fn new_has_go_prefix_and_no_source() {
        let e = CompactionError::new("post-invocation: compaction panicked: boom");
        assert_eq!(
            e.to_string(),
            "context compaction failed: post-invocation: compaction panicked: boom"
        );
        assert_eq!(e.detail(), "post-invocation: compaction panicked: boom");
        assert!(e.source().is_none());
    }

    #[test]
    fn wrap_keeps_the_source_chain() {
        let e = CompactionError::wrap("failed to append the summary event", "disk full");
        assert_eq!(
            e.to_string(),
            "context compaction failed: failed to append the summary event: disk full"
        );
        assert_eq!(e.detail(), "failed to append the summary event: disk full");
        assert_eq!(e.source().unwrap().to_string(), "disk full");
        assert!(e.to_string().starts_with(&format!("{ERR_COMPACTION}: ")));
    }
}
