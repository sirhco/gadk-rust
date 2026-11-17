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

//! The event filters and the log / write entry points of the plugin: `should_log_event`,
//! `should_log_session_event`, `denied`, `recover_callback`, `log_event`, `log_session_event`,
//! `log_session_span_event`, `write_event`.
//!
//! A callback body that panics is guarded by `std::panic::catch_unwind(AssertUnwindSafe(…))`
//! around the span and row-building body: a panic drops the row, counts one append-failure
//! drop and logs `agentanalytics: dropped <type> event: recovered panic of type <T>`.

use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};

use super::{AnalyticsPlugin, EventData, LEGACY_EVENT_TYPE};
use crate::redaction::panic_type_name;
use crate::trace::{CallbackCtx, Identity, RowSpan};

impl AnalyticsPlugin {
    /// Whether an event of `event_type` is logged: the denylist, then the allowlist if set.
    pub(crate) fn should_log_event(&self, event_type: &str) -> bool {
        if self.denylist.contains(event_type) {
            return false;
        }
        if !self.allowlist.is_empty() {
            return self.allowlist.contains(event_type);
        }
        true
    }

    /// Like `should_log_event`, with the legacy `EVENT` alias for every session-event row (a
    /// denied `EVENT` drops them all; an allowed `EVENT` admits them all; a per-type deny still
    /// wins).
    pub(crate) fn should_log_session_event(&self, event_type: &str) -> bool {
        if self.denylist.contains(LEGACY_EVENT_TYPE) || self.denylist.contains(event_type) {
            return false;
        }
        if !self.allowlist.is_empty() {
            return self.allowlist.contains(LEGACY_EVENT_TYPE)
                || self.allowlist.contains(event_type);
        }
        true
    }

    /// Whether `column` is denied.
    pub(crate) fn denied(&self, column: &str) -> bool {
        self.denied_columns.contains(column)
    }

    /// Handles a recovered panic with payload `panic`: counts one append-failure drop (enabled
    /// plugin with a processor) and logs the panic's type, never its value. The log call itself
    /// is not guarded: a panicking `Logger` propagates.
    ///
    /// Rust knows only whether the payload is a string (`string`), else the logged type is
    /// `unknown` (see `panic_type_name`). Rust's panic hook also reports the panic (stderr by
    /// default) before `catch_unwind` returns.
    pub(crate) fn recover_callback(&self, event_type: &str, panic: &(dyn Any + Send)) {
        if self.config.enabled
            && let Some(p) = &self.processor
        {
            p.record_append_failure_drop(1);
        }
        self.config.logf(format_args!(
            "agentanalytics: dropped {event_type} event: recovered panic of type {}",
            panic_type_name(panic)
        ));
    }

    /// Runs `span_fn` (always, before the filters, so the spans and latencies of the rows that
    /// are written stay right when the filters drop other rows), then, if `event_type` passes
    /// `should_log_event`, `write_event`; a panic drops the row (`recover_callback`).
    pub(crate) fn log_event(
        &self,
        ctx: &CallbackCtx,
        id: &Identity,
        event_type: &str,
        span_fn: Option<&mut dyn FnMut() -> RowSpan>,
        build: Option<&mut dyn FnMut(&mut EventData)>,
    ) {
        let result = catch_unwind(AssertUnwindSafe(|| {
            let span = span_fn.map(|f| f()).unwrap_or_default();
            if !self.should_log_event(event_type) {
                return;
            }
            self.write_event(ctx, id, event_type, span, build);
        }));
        if let Err(panic) = result {
            self.recover_callback(event_type, panic.as_ref());
        }
    }

    /// Calls `write_event` if `should_log_session_event` (no recovery of its own).
    pub(crate) fn log_session_event(
        &self,
        ctx: &CallbackCtx,
        id: &Identity,
        event_type: &str,
        span: RowSpan,
        build: Option<&mut dyn FnMut(&mut EventData)>,
    ) {
        if !self.should_log_session_event(event_type) {
            return;
        }
        self.write_event(ctx, id, event_type, span, build);
    }

    /// As `log_event` with `should_log_session_event`, so session-event rows emitted outside the
    /// event callback honor the legacy `EVENT` alias while keeping the trace manager's span side
    /// effects.
    pub(crate) fn log_session_span_event(
        &self,
        ctx: &CallbackCtx,
        id: &Identity,
        event_type: &str,
        span_fn: Option<&mut dyn FnMut() -> RowSpan>,
        build: Option<&mut dyn FnMut(&mut EventData)>,
    ) {
        let result = catch_unwind(AssertUnwindSafe(|| {
            let span = span_fn.map(|f| f()).unwrap_or_default();
            if !self.should_log_session_event(event_type) {
                return;
            }
            self.write_event(ctx, id, event_type, span, build);
        }));
        if let Err(panic) = result {
            self.recover_callback(event_type, panic.as_ref());
        }
    }

    /// Builds the row (`build_row`) and appends it to the processor; a panic drops the row. A
    /// plugin without a processor drops the row without a log line.
    pub(crate) fn write_event(
        &self,
        ctx: &CallbackCtx,
        id: &Identity,
        event_type: &str,
        span: RowSpan,
        build: Option<&mut dyn FnMut(&mut EventData)>,
    ) {
        let result = catch_unwind(AssertUnwindSafe(|| {
            let mut d = EventData {
                span,
                ..EventData::default()
            };
            if let Some(build) = build {
                build(&mut d);
            }
            let row = self.build_row(ctx, id, event_type, &mut d);
            if let Some(p) = &self.processor {
                p.append(row);
            }
        }));
        if let Err(panic) = result {
            self.recover_callback(event_type, panic.as_ref());
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::collections::BTreeSet;
    use std::fmt;
    use std::panic::AssertUnwindSafe;
    use std::sync::Arc;

    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;
    use crate::batch_processor::BatchProcessor;
    use crate::bigquery::fake::RecordingLogger;
    use crate::config::{Config, Logger};
    use crate::event_types::{
        EVENT_AGENT_RESPONSE, EVENT_LLM_REQUEST, EVENT_STATE_DELTA, EVENT_TOOL_STARTING,
    };
    use crate::plugin::Payload;
    use crate::row::{QueuedRow, RowExt};

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    /// One filter case: name, allowlist, denylist, event type, whether it is logged.
    type FilterCase = (
        &'static str,
        &'static [&'static str],
        &'static [&'static str],
        &'static str,
        bool,
    );

    fn filtered(allow: &[&str], deny: &[&str]) -> AnalyticsPlugin {
        AnalyticsPlugin {
            allowlist: set(allow),
            denylist: set(deny),
            ..AnalyticsPlugin::default()
        }
    }

    /// An enabled plugin over a bare processor, logging to `logger`.
    fn plugin(logger: &Arc<RecordingLogger>) -> AnalyticsPlugin {
        let config = Config {
            logger: Some(logger.clone() as Arc<dyn Logger>),
            ..Config::default()
        };
        AnalyticsPlugin {
            processor: Some(Arc::new(BatchProcessor::bare(config.clone(), 100))),
            config,
            ..AnalyticsPlugin::default()
        }
    }

    fn drained(p: &AnalyticsPlugin) -> Vec<QueuedRow> {
        p.processor.as_ref().unwrap().drain_queue()
    }

    fn append_failure_drops(p: &AnalyticsPlugin) -> i64 {
        p.processor
            .as_ref()
            .unwrap()
            .drop_stats()
            .append_failure_drops
    }

    // Go TestShouldLogEvent.
    #[test]
    fn should_log_event_table() {
        let cases: [FilterCase; 6] = [
            ("no filters", &[], &[], EVENT_LLM_REQUEST, true),
            (
                "allowlisted",
                &[EVENT_LLM_REQUEST],
                &[],
                EVENT_LLM_REQUEST,
                true,
            ),
            (
                "not allowlisted",
                &[EVENT_TOOL_STARTING],
                &[],
                EVENT_LLM_REQUEST,
                false,
            ),
            (
                "denylisted",
                &[],
                &[EVENT_LLM_REQUEST],
                EVENT_LLM_REQUEST,
                false,
            ),
            (
                "denylist overrides allowlist",
                &[EVENT_LLM_REQUEST],
                &[EVENT_LLM_REQUEST],
                EVENT_LLM_REQUEST,
                false,
            ),
            (
                "denylist leaves other events",
                &[],
                &[EVENT_LLM_REQUEST],
                EVENT_TOOL_STARTING,
                true,
            ),
        ];
        for (name, allow, deny, event_type, want) in cases {
            assert_eq!(
                filtered(allow, deny).should_log_event(event_type),
                want,
                "{name}"
            );
        }
    }

    #[test]
    fn should_log_session_event_honors_the_legacy_alias() {
        let cases: [(&str, &[&str], &[&str], bool); 7] = [
            ("no filters", &[], &[], true),
            ("legacy denied", &[], &["EVENT"], false),
            ("type denied", &[], &[EVENT_STATE_DELTA], false),
            ("legacy allowed", &["EVENT"], &[], true),
            ("type allowed", &[EVENT_STATE_DELTA], &[], true),
            ("other type allowed", &[EVENT_AGENT_RESPONSE], &[], false),
            (
                "type deny beats legacy allow",
                &["EVENT"],
                &[EVENT_STATE_DELTA],
                false,
            ),
        ];
        for (name, allow, deny, want) in cases {
            assert_eq!(
                filtered(allow, deny).should_log_session_event(EVENT_STATE_DELTA),
                want,
                "{name}"
            );
        }
        // The plain filter knows no alias.
        assert!(!filtered(&["EVENT"], &[]).should_log_event(EVENT_STATE_DELTA));
    }

    #[test]
    fn denied_reads_the_denied_columns() {
        let p = AnalyticsPlugin {
            denied_columns: set(&["content"]),
            ..AnalyticsPlugin::default()
        };
        assert!(p.denied("content"));
        assert!(!p.denied("attributes"));
    }

    #[test]
    fn log_event_runs_the_span_before_the_filters() {
        let logger = Arc::new(RecordingLogger::default());
        let mut p = plugin(&logger);
        p.denylist = set(&[EVENT_LLM_REQUEST]);
        let id = Identity {
            invocation_id: "inv".into(),
            ..Identity::default()
        };
        let mut spans = 0;
        let mut span_fn = || {
            spans += 1;
            RowSpan {
                span_id: "s1".into(),
                ..RowSpan::default()
            }
        };
        let mut builds = 0;
        let mut build = |d: &mut EventData| {
            builds += 1;
            d.content = Some(Payload::Json(json!("x")));
        };
        p.log_event(
            &CallbackCtx::background(),
            &id,
            EVENT_LLM_REQUEST,
            Some(&mut span_fn),
            Some(&mut build),
        );
        p.log_event(
            &CallbackCtx::background(),
            &id,
            EVENT_TOOL_STARTING,
            Some(&mut span_fn),
            Some(&mut build),
        );
        assert_eq!((spans, builds), (2, 1));
        let rows = drained(&p);
        assert_eq!(rows.len(), 1);
        let row = &rows[0].row;
        assert_eq!(row.str("event_type"), Some(EVENT_TOOL_STARTING));
        assert_eq!(row.str("span_id"), Some("s1"));
        assert_eq!(row.str("content"), Some(r#""x""#));
        assert!(logger.lines().is_empty());
    }

    // Go TestLogEvent_RecoversFromPanics, on the entry points directly.
    #[test]
    fn panics_drop_the_row_and_count_it() {
        let logger = Arc::new(RecordingLogger::default());
        let p = plugin(&logger);
        let ctx = CallbackCtx::background();
        let id = Identity::default();
        let mut panicking_build = |_: &mut EventData| panic!("tool description unavailable");
        p.log_event(
            &ctx,
            &id,
            EVENT_TOOL_STARTING,
            None,
            Some(&mut panicking_build),
        );
        let mut panicking_span = || -> RowSpan { panic!("no span") };
        p.log_session_span_event(
            &ctx,
            &id,
            EVENT_STATE_DELTA,
            Some(&mut panicking_span),
            None,
        );
        p.write_event(
            &ctx,
            &id,
            EVENT_AGENT_RESPONSE,
            RowSpan::default(),
            Some(&mut |_: &mut EventData| std::panic::panic_any(7_u8)),
        );
        assert!(drained(&p).is_empty());
        assert_eq!(append_failure_drops(&p), 3);
        assert_eq!(
            logger.lines(),
            vec![
                "agentanalytics: dropped TOOL_STARTING event: recovered panic of type string",
                "agentanalytics: dropped STATE_DELTA event: recovered panic of type string",
                "agentanalytics: dropped AGENT_RESPONSE event: recovered panic of type unknown",
            ]
        );
        assert!(!logger.contains("tool description unavailable"));
    }

    #[test]
    fn a_disabled_plugin_logs_without_counting() {
        let logger = Arc::new(RecordingLogger::default());
        let mut p = plugin(&logger);
        p.config.enabled = false;
        p.recover_callback(EVENT_TOOL_STARTING, &"boom");
        assert_eq!(append_failure_drops(&p), 0);
        assert!(logger.contains("dropped TOOL_STARTING event"));
        // Without a processor nothing is counted either, and writing drops the row quietly.
        let p = AnalyticsPlugin {
            config: Config {
                logger: Some(logger.clone() as Arc<dyn Logger>),
                ..Config::default()
            },
            ..AnalyticsPlugin::default()
        };
        p.recover_callback(EVENT_TOOL_STARTING, &"boom");
        let before = logger.lines().len();
        p.write_event(
            &CallbackCtx::background(),
            &Identity::default(),
            EVENT_TOOL_STARTING,
            RowSpan::default(),
            None,
        );
        assert_eq!(logger.lines().len(), before);
    }

    struct PanickingLogger;

    impl Logger for PanickingLogger {
        fn printf(&self, _: fmt::Arguments<'_>) {
            panic!("logger broke");
        }
        fn println(&self, _: fmt::Arguments<'_>) {
            panic!("logger broke");
        }
    }

    // R7: as in Go, the recovery's own log call is not guarded.
    #[test]
    fn a_panicking_logger_propagates_from_the_recovery() {
        let config = Config {
            logger: Some(Arc::new(PanickingLogger)),
            ..Config::default()
        };
        let p = AnalyticsPlugin {
            processor: Some(Arc::new(BatchProcessor::bare(config.clone(), 100))),
            config,
            ..AnalyticsPlugin::default()
        };
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            p.log_event(
                &CallbackCtx::background(),
                &Identity::default(),
                EVENT_TOOL_STARTING,
                None,
                Some(&mut |_: &mut EventData| panic!("row")),
            );
        }));
        assert!(result.is_err());
        // As Go's nested deferred recoveries: write_event's recovery counts the row and its
        // log panics; log_event's recovery catches that panic, counts again and re-panics.
        assert_eq!(append_failure_drops(&p), 2);
    }

    #[test]
    fn session_events_use_the_session_filter() {
        let logger = Arc::new(RecordingLogger::default());
        let mut p = plugin(&logger);
        p.denylist = set(&["EVENT"]);
        let ctx = CallbackCtx::background();
        let id = Identity::default();
        p.log_session_event(&ctx, &id, EVENT_STATE_DELTA, RowSpan::default(), None);
        let mut spans = 0;
        p.log_session_span_event(
            &ctx,
            &id,
            EVENT_AGENT_RESPONSE,
            Some(&mut || {
                spans += 1;
                RowSpan::default()
            }),
            None,
        );
        assert_eq!(spans, 1);
        // log_event knows no alias.
        p.log_event(&ctx, &id, EVENT_AGENT_RESPONSE, None, None);
        let rows = drained(&p);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].row.str("event_type"), Some(EVENT_AGENT_RESPONSE));

        p.denylist.clear();
        p.allowlist = set(&["EVENT"]);
        p.log_session_event(&ctx, &id, EVENT_STATE_DELTA, RowSpan::default(), None);
        p.log_event(&ctx, &id, EVENT_STATE_DELTA, None, None);
        let rows = drained(&p);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].row.str("event_type"), Some(EVENT_STATE_DELTA));
    }
}
