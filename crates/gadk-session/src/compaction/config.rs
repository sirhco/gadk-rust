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

//! Port of Go session/compaction/compaction.go `Config` and `Config.Validate`.

use std::sync::Arc;

use super::Summarizer;

/// Configures context compaction for an application (Go `compaction.Config`).
///
/// At least one strategy must be enabled; a `Config` that enables neither is rejected by
/// [`Config::validate`]. Use `None` (`Option<Config>`) to disable compaction.
///
/// - Sliding window (`compaction_interval`, `overlap_size`) runs after an invocation completes and
///   summarizes whole invocations at a time.
/// - Tail retention (`token_threshold`, `event_retention_size`) runs inside an invocation before a
///   model call and summarizes everything but the most recent events once the prompt grows past a
///   token budget.
///
/// Both can be enabled at once and they compose, but the sliding window consumes the events tail
/// retention would otherwise summarize: tail retention only fires when more events accumulate
/// between windows than `event_retention_size` holds back.
#[derive(Clone, Default)]
pub struct Config {
    /// Number of new user-initiated invocations that triggers a sliding-window compaction; also
    /// the most invocations one compaction covers. Zero disables sliding-window compaction.
    pub compaction_interval: i32,
    /// How many already-compacted invocations to pull back into the next sliding window, for
    /// continuity between summaries. Only meaningful alongside `compaction_interval`.
    pub overlap_size: i32,
    /// Prompt token count at which tail-retention compaction fires before a model call. Zero
    /// disables tail-retention compaction. Summaries produced by this strategy do not pass
    /// through the plugin pipeline.
    pub token_threshold: i32,
    /// How many of the most recent events are kept raw when tail retention fires. Required with
    /// `token_threshold`.
    pub event_retention_size: i32,
    /// Produces the summary content. When `None`, the runner supplies an `LlmSummarizer` backed by
    /// the root agent's model, which therefore has to be an LLM agent.
    pub summarizer: Option<Arc<dyn Summarizer>>,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("compaction_interval", &self.compaction_interval)
            .field("overlap_size", &self.overlap_size)
            .field("token_threshold", &self.token_threshold)
            .field("event_retention_size", &self.event_retention_size)
            .field(
                "summarizer",
                &self.summarizer.as_ref().map(|s| s.type_name()),
            )
            .finish()
    }
}

/// An unusable compaction configuration; the message is Go's error text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct CompactionConfigError {
    message: String,
}

impl CompactionConfigError {
    /// An error carrying `message` verbatim.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// The error text.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl Config {
    fn has_sliding_window(&self) -> bool {
        self.compaction_interval > 0
    }

    fn has_tail_retention(&self) -> bool {
        self.token_threshold > 0
    }

    /// Reports whether the configuration is usable (port of Go `(*Config).Validate`; the checks
    /// and their texts are Go's, in Go's order). A config with no strategy enabled is an error;
    /// `None` already expresses "disabled".
    pub fn validate(&self) -> Result<(), CompactionConfigError> {
        let err = |m: String| Err(CompactionConfigError::new(m));
        if self.compaction_interval < 0 {
            return err(format!(
                "CompactionInterval must not be negative, got {}",
                self.compaction_interval
            ));
        }
        if self.overlap_size < 0 {
            return err(format!(
                "OverlapSize must not be negative, got {}",
                self.overlap_size
            ));
        }
        if self.token_threshold < 0 {
            return err(format!(
                "TokenThreshold must not be negative, got {}",
                self.token_threshold
            ));
        }
        if self.event_retention_size < 0 {
            return err(format!(
                "EventRetentionSize must not be negative, got {}",
                self.event_retention_size
            ));
        }
        if self.overlap_size > 0 && self.compaction_interval == 0 {
            return err(format!(
                "OverlapSize is set to {} but CompactionInterval is 0, so sliding-window compaction never runs",
                self.overlap_size
            ));
        }
        if self.token_threshold > 0 && self.event_retention_size == 0 {
            return err(format!(
                "TokenThreshold is set to {} but EventRetentionSize is 0, so a compaction would summarize the whole conversation including the turn being answered",
                self.token_threshold
            ));
        }
        if self.event_retention_size > 0 && self.token_threshold == 0 {
            return err(format!(
                "EventRetentionSize is set to {} but TokenThreshold is 0, so tail-retention compaction never runs",
                self.event_retention_size
            ));
        }
        if !self.has_sliding_window() && !self.has_tail_retention() {
            return err(
                "no compaction strategy is enabled, set CompactionInterval or TokenThreshold (or leave the whole config nil to disable compaction)"
                    .to_string(),
            );
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::Event;
    use crate::compaction::SummarizeResult;

    struct Decliner;

    #[async_trait::async_trait]
    impl Summarizer for Decliner {
        async fn summarize_events(
            &self,
            _events: &[Event],
        ) -> Result<SummarizeResult, Box<dyn std::error::Error + Send + Sync>> {
            Ok(SummarizeResult::default())
        }
    }

    fn cfg(interval: i32, overlap: i32, threshold: i32, retention: i32) -> Config {
        Config {
            compaction_interval: interval,
            overlap_size: overlap,
            token_threshold: threshold,
            event_retention_size: retention,
            summarizer: None,
        }
    }

    #[test]
    fn validate_messages_are_go_texts() {
        let cases = [
            (
                cfg(-1, 0, 0, 0),
                "CompactionInterval must not be negative, got -1",
            ),
            (cfg(2, -2, 0, 0), "OverlapSize must not be negative, got -2"),
            (
                cfg(2, 0, -3, 0),
                "TokenThreshold must not be negative, got -3",
            ),
            (
                cfg(2, 0, 0, -4),
                "EventRetentionSize must not be negative, got -4",
            ),
            (
                cfg(0, 1, 0, 0),
                "OverlapSize is set to 1 but CompactionInterval is 0, so sliding-window compaction never runs",
            ),
            (
                cfg(0, 0, 100, 0),
                "TokenThreshold is set to 100 but EventRetentionSize is 0, so a compaction would summarize the whole conversation including the turn being answered",
            ),
            (
                cfg(0, 0, 0, 5),
                "EventRetentionSize is set to 5 but TokenThreshold is 0, so tail-retention compaction never runs",
            ),
            (
                cfg(0, 0, 0, 0),
                "no compaction strategy is enabled, set CompactionInterval or TokenThreshold (or leave the whole config nil to disable compaction)",
            ),
        ];
        for (c, want) in cases {
            let got = c.validate().unwrap_err();
            assert_eq!(got.message(), want, "{c:?}");
            assert_eq!(got.to_string(), want, "{c:?}");
        }
    }

    #[test]
    fn validate_checks_in_go_order() {
        // Every check fails: the first one (CompactionInterval) is reported.
        assert_eq!(
            cfg(-1, -2, -3, -4).validate().unwrap_err().message(),
            "CompactionInterval must not be negative, got -1"
        );
        // Negative checks come before the pairing checks.
        assert_eq!(
            cfg(0, -2, 100, 0).validate().unwrap_err().message(),
            "OverlapSize must not be negative, got -2"
        );
        assert_eq!(
            cfg(0, 1, -3, 0).validate().unwrap_err().message(),
            "TokenThreshold must not be negative, got -3"
        );
        assert_eq!(
            cfg(0, 1, 0, -4).validate().unwrap_err().message(),
            "EventRetentionSize must not be negative, got -4"
        );
        // OverlapSize pairing before TokenThreshold pairing, before EventRetentionSize pairing.
        assert_eq!(
            cfg(0, 1, 100, 0).validate().unwrap_err().message(),
            "OverlapSize is set to 1 but CompactionInterval is 0, so sliding-window compaction never runs"
        );
        assert_eq!(
            cfg(2, 0, 100, 0).validate().unwrap_err().message(),
            "TokenThreshold is set to 100 but EventRetentionSize is 0, so a compaction would summarize the whole conversation including the turn being answered"
        );
    }

    #[test]
    fn validate_accepts_each_strategy_and_both() {
        assert_eq!(cfg(2, 1, 0, 0).validate(), Ok(()));
        assert_eq!(cfg(3, 0, 0, 0).validate(), Ok(()));
        assert_eq!(cfg(0, 0, 100, 5).validate(), Ok(()));
        assert_eq!(cfg(2, 1, 100, 5).validate(), Ok(()));
    }

    #[test]
    fn config_debug_names_the_summarizer() {
        let mut c = cfg(2, 1, 0, 0);
        assert_eq!(
            format!("{c:?}"),
            "Config { compaction_interval: 2, overlap_size: 1, token_threshold: 0, event_retention_size: 0, summarizer: None }"
        );
        c.summarizer = Some(Arc::new(Decliner));
        assert_eq!(
            format!("{c:?}"),
            "Config { compaction_interval: 2, overlap_size: 1, token_threshold: 0, event_retention_size: 0, summarizer: Some(\"Decliner\") }"
        );
    }

    #[test]
    fn config_default_and_clone() {
        let d = Config::default();
        assert_eq!(
            (
                d.compaction_interval,
                d.overlap_size,
                d.token_threshold,
                d.event_retention_size
            ),
            (0, 0, 0, 0)
        );
        assert!(d.summarizer.is_none());
        let c = Config {
            summarizer: Some(Arc::new(Decliner)),
            ..cfg(2, 0, 0, 0)
        };
        let c2 = c.clone();
        assert!(Arc::ptr_eq(
            c.summarizer.as_ref().unwrap(),
            c2.summarizer.as_ref().unwrap()
        ));
    }
}
