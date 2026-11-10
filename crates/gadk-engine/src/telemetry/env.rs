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

//! The telemetry environment (port of the environment rules of Go `internal/telemetry/logger.go`):
//! the process-wide [`EnvProvider`] installed with [`set_env_provider`] (default [`ProcessEnv`]),
//! the content-capture mode (`OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT`, read once and
//! cached until the next [`set_env_provider`] or [`apply_env`], as Go's `sync.Once` plus
//! `ApplyEnv`), and the 1.36 schema opt-in (`ADK_TELEMETRY_SCHEMA_VERSION_OPT_IN`, read on every
//! call, as Go's `useLegacySchema`).

// Read by the span attributes (Task 3) and log records (Task 11).
#![allow(dead_code)]

use std::sync::Arc;

use gadk_platform::env::{EnvProvider, ProcessEnv};
use gadk_platform::{go_to_lower, go_to_upper};
use parking_lot::{Mutex, RwLock};

/// Go `captureMessageContentEnvVar`.
pub(crate) const CAPTURE_MESSAGE_CONTENT_ENV_VAR: &str =
    "OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT";
/// Go `adkTelemetrySchemaVersionOptIn`: `otel_semconv_1_36` or `1` selects the legacy (1.36)
/// schema; anything else the default (1.44).
pub(crate) const SCHEMA_VERSION_OPT_IN_ENV_VAR: &str = "ADK_TELEMETRY_SCHEMA_VERSION_OPT_IN";
/// Go `otelSemconv136`.
pub(crate) const OTEL_SEMCONV_1_36: &str = "otel_semconv_1_36";

/// Go `contentCaptureMode`: which signals may carry message content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContentCaptureMode {
    /// Go `captureNone`.
    None,
    /// Go `captureEventOnly`: log records only (also what a truthy value means).
    EventOnly,
    /// Go `captureSpanOnly`.
    SpanOnly,
    /// Go `captureSpanAndEvent`.
    SpanAndEvent,
}

static PROVIDER: RwLock<Option<Arc<dyn EnvProvider>>> = RwLock::new(None);
static CAPTURE_MODE: Mutex<Option<ContentCaptureMode>> = Mutex::new(None);

/// Installs the process-wide environment the engine's telemetry reads
/// (`OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT`, `ADK_TELEMETRY_SCHEMA_VERSION_OPT_IN`);
/// the default is the process environment. The cached content-capture mode is dropped, so the
/// next read uses `env`.
pub fn set_env_provider(env: Arc<dyn EnvProvider>) {
    *PROVIDER.write() = Some(env);
    *CAPTURE_MODE.lock() = None;
}

/// The installed provider ([`ProcessEnv`] when none was installed).
pub(crate) fn provider() -> Arc<dyn EnvProvider> {
    PROVIDER
        .read()
        .clone()
        .unwrap_or_else(|| Arc::new(ProcessEnv))
}

/// `key` from the installed provider; unset reads as empty (Go `os.Getenv`).
fn getenv(key: &str) -> String {
    provider().var(key).unwrap_or_default()
}

/// Go `parseContentCaptureMode`.
pub(crate) fn parse_content_capture_mode(s: &str) -> ContentCaptureMode {
    match go_to_upper(s.trim()).as_str() {
        "EVENT_ONLY" => return ContentCaptureMode::EventOnly,
        "SPAN_ONLY" => return ContentCaptureMode::SpanOnly,
        "SPAN_AND_EVENT" => return ContentCaptureMode::SpanAndEvent,
        _ => {}
    }
    if evals_to_true(s) {
        return ContentCaptureMode::EventOnly;
    }
    ContentCaptureMode::None
}

/// Go `evalsToTrue`: `1` or `true`, ignoring case and surrounding space.
pub(crate) fn evals_to_true(s: &str) -> bool {
    let u = go_to_lower(s.trim());
    u == "1" || u == "true"
}

/// Go `ApplyEnv`: re-reads the content-capture mode from the installed provider now.
pub(crate) fn apply_env() {
    *CAPTURE_MODE.lock() = Some(parse_content_capture_mode(&getenv(
        CAPTURE_MESSAGE_CONTENT_ENV_VAR,
    )));
}

/// Go `contentCapture`: the cached mode, read on first use.
pub(crate) fn capture_mode() -> ContentCaptureMode {
    let mut cached = CAPTURE_MODE.lock();
    *cached
        .get_or_insert_with(|| parse_content_capture_mode(&getenv(CAPTURE_MESSAGE_CONTENT_ENV_VAR)))
}

/// Go `getGenAICaptureMessageContent`: whether log records may carry message content.
pub(crate) fn capture_message_content() -> bool {
    matches!(
        capture_mode(),
        ContentCaptureMode::EventOnly | ContentCaptureMode::SpanAndEvent
    )
}

/// Go `captureContentOnSpans`: whether span attributes may carry message content (a truthy value
/// does not count).
pub(crate) fn capture_content_on_spans() -> bool {
    matches!(
        capture_mode(),
        ContentCaptureMode::SpanOnly | ContentCaptureMode::SpanAndEvent
    )
}

/// Go `useLegacySchema`: whether `ADK_TELEMETRY_SCHEMA_VERSION_OPT_IN` selects the 1.36 schema;
/// read on every call.
pub(crate) fn use_legacy_schema() -> bool {
    matches!(
        go_to_lower(getenv(SCHEMA_VERSION_OPT_IN_ENV_VAR).trim()).as_str(),
        OTEL_SEMCONV_1_36 | "1"
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::collections::HashMap;

    use gadk_platform::env::MapEnv;

    use super::*;
    use crate::testutil::telemetry::ENV_LOCK;

    fn env(pairs: &[(&str, &str)]) -> Arc<dyn EnvProvider> {
        Arc::new(MapEnv(
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect::<HashMap<_, _>>(),
        ))
    }

    #[test]
    fn parse_rules_follow_go() {
        use ContentCaptureMode::*;
        for (s, want) in [
            ("EVENT_ONLY", EventOnly),
            (" event_only ", EventOnly),
            ("SPAN_ONLY", SpanOnly),
            ("span_and_event", SpanAndEvent),
            ("1", EventOnly),
            ("TRUE", EventOnly),
            (" true\n", EventOnly),
            ("yes", None),
            ("", None),
            ("0", None),
        ] {
            assert_eq!(parse_content_capture_mode(s), want, "{s:?}");
        }
    }

    #[test]
    fn capture_mode_is_cached_until_the_provider_changes() {
        let _g = ENV_LOCK.lock();
        set_env_provider(env(&[(CAPTURE_MESSAGE_CONTENT_ENV_VAR, "SPAN_AND_EVENT")]));
        assert!(capture_message_content());
        assert!(capture_content_on_spans());
        // A provider change is what drops the cache; the old mode is kept until then.
        *PROVIDER.write() = Some(env(&[]));
        assert_eq!(capture_mode(), ContentCaptureMode::SpanAndEvent);
        apply_env();
        assert_eq!(capture_mode(), ContentCaptureMode::None);
        set_env_provider(env(&[(CAPTURE_MESSAGE_CONTENT_ENV_VAR, "true")]));
        assert!(capture_message_content());
        assert!(!capture_content_on_spans());
        set_env_provider(Arc::new(MapEnv::default()));
        assert_eq!(capture_mode(), ContentCaptureMode::None);
    }

    #[test]
    fn schema_opt_in_is_read_on_every_call() {
        let _g = ENV_LOCK.lock();
        set_env_provider(env(&[(
            SCHEMA_VERSION_OPT_IN_ENV_VAR,
            " OTEL_SEMCONV_1_36 ",
        )]));
        assert!(use_legacy_schema());
        *PROVIDER.write() = Some(env(&[(SCHEMA_VERSION_OPT_IN_ENV_VAR, "1")]));
        assert!(use_legacy_schema());
        *PROVIDER.write() = Some(env(&[(SCHEMA_VERSION_OPT_IN_ENV_VAR, "2")]));
        assert!(!use_legacy_schema());
        set_env_provider(Arc::new(MapEnv::default()));
        assert!(!use_legacy_schema());
    }
}
