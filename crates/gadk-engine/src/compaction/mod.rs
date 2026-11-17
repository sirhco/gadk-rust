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

//! Context compaction internals: sliding-window and tail-retention summarization, applying stored
//! summaries to a prompt, and the per-invocation runtime. The public API is
//! `gadk_session::compaction`.

pub(crate) mod apply;
pub(crate) mod compactor;
pub(crate) mod runtime;
pub(crate) mod summary_event;
pub(crate) mod tail_retention;
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
pub(crate) mod test_support;
pub(crate) mod window;

use gadk_session::compaction::Config;

/// The error type of the engine's compaction steps.
pub(crate) type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// An error with added context: displays as `<context>: <err>` and has `err` as its source.
#[derive(Debug)]
struct Wrapped {
    context: String,
    source: BoxError,
}

impl std::fmt::Display for Wrapped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.context, self.source)
    }
}

impl std::error::Error for Wrapped {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&*self.source)
    }
}

/// Wraps `err` with `context`: Display `<context>: <err>`, `source()` = `err`.
pub(crate) fn wrap_err(context: impl Into<String>, err: BoxError) -> BoxError {
    Box::new(Wrapped {
        context: context.into(),
        source: err,
    })
}

/// An error with just a message, no source.
pub(crate) fn err_text(text: impl Into<String>) -> BoxError {
    text.into().into()
}

/// Whether the config enables sliding-window compaction (a positive `compaction_interval`).
pub(crate) fn has_sliding_window(cfg: Option<&Config>) -> bool {
    cfg.is_some_and(|c| c.compaction_interval > 0)
}

/// Whether the config enables tail-retention compaction (a positive `token_threshold`).
pub(crate) fn has_tail_retention(cfg: Option<&Config>) -> bool {
    cfg.is_some_and(|c| c.token_threshold > 0)
}

/// Decides whether another tail-retention compaction in this invocation can still help.
pub(crate) trait ProgressGate: Send + Sync {
    /// Whether a compaction may run now.
    fn allow_at(&self) -> bool;
    /// A compaction succeeded at a prompt of `tokens`.
    fn record_at(&self, tokens: i64);
    /// The prompt came back under the threshold.
    fn recovered(&self);
    /// A compaction attempt failed or was declined.
    fn failed(&self);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn wrap_err_formats_and_chains_like_go() {
        let e = wrap_err("sliding-window summarization failed", err_text("boom"));
        assert_eq!(e.to_string(), "sliding-window summarization failed: boom");
        assert_eq!(e.source().unwrap().to_string(), "boom");
    }

    #[test]
    fn strategy_predicates_follow_go() {
        assert!(!has_sliding_window(None));
        assert!(!has_tail_retention(None));
        let c = Config {
            compaction_interval: 1,
            token_threshold: 1,
            ..Default::default()
        };
        assert!(has_sliding_window(Some(&c)) && has_tail_retention(Some(&c)));
        assert!(!has_sliding_window(Some(&Config::default())));
        assert!(!has_tail_retention(Some(&Config::default())));
    }
}
