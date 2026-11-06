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

//! Port of Go internal/compactioninternal/compactor.go: the sliding-window entry point, the
//! traced summarizer call shared with tail retention, and the view of history handed to a
//! summarizer.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use futures::FutureExt;
use gadk_genai::{
    Blob, CodeExecutionResult, Content, ExecutableCode, FileData, FunctionCall, FunctionResponse,
    Part,
};
use gadk_model::Backend;
use gadk_platform::Platform;
use gadk_session::compaction::{Config, Summarizer, failure_usage};
use gadk_session::{Event, EventCompaction, Session, zero_time};
use tracing::Instrument;

use super::summary_event::new_summary_event;
use super::window::select_sliding_window;
use super::{BoxError, err_text, has_sliding_window, wrap_err};
use crate::telemetry::{
    COMPACTION_TRIGGER_SLIDING_WINDOW, CompactEventsSpanParams, CompactionResultParams,
    compact_events_span, record_span_error, trace_compaction_result,
};
use crate::tool::utils::panic_message;

type FinishFn = Box<dyn FnOnce(Option<&(dyn std::error::Error + 'static)>, &str) + Send>;

/// Port of Go Finish: reports what became of a summary and closes its span. Call
/// [`Finish::finish`] exactly once; the summary is not stored until it is made. Dropping an
/// unfinished `Finish` closes the span with no result and no error.
pub(crate) struct Finish {
    f: Option<FinishFn>,
}

impl Finish {
    /// Go's `noop := func(error, string) {}`.
    pub(crate) fn noop() -> Finish {
        Finish { f: None }
    }

    pub(crate) fn new(
        f: impl FnOnce(Option<&(dyn std::error::Error + 'static)>, &str) + Send + 'static,
    ) -> Finish {
        Finish {
            f: Some(Box::new(f)),
        }
    }

    /// `err` set: the summary failed; `discard_reason` non-empty: it was produced but not kept;
    /// both empty: it was stored.
    pub(crate) fn finish(
        self,
        err: Option<&(dyn std::error::Error + 'static)>,
        discard_reason: &str,
    ) {
        if let Some(f) = self.f {
            f(err, discard_reason);
        }
    }

    /// Runs `before` with the same arguments, then this finish (Go tail_retention.go's
    /// `recordOnSuccess` wrapper).
    pub(crate) fn then_after(
        self,
        before: impl FnOnce(Option<&(dyn std::error::Error + 'static)>, &str) + Send + 'static,
    ) -> Finish {
        Finish::new(move |err, reason| {
            before(err, reason);
            self.finish(err, reason);
        })
    }
}

/// Port of Go SlidingWindow: summarizes a window of completed invocations once enough have
/// accumulated. `Ok((None, _))` when there is nothing to do (interval not reached, no
/// self-contained window, a declining summarizer); the returned `Finish` must be called once.
pub(crate) async fn sliding_window(
    platform: &Platform,
    cfg: &Config,
    sess: Option<&dyn Session>,
    invocation_id: &str,
) -> Result<(Option<Event>, Finish), BoxError> {
    if !has_sliding_window(Some(cfg)) {
        return Ok((None, Finish::noop()));
    }
    if cfg.summarizer.is_none() {
        return Err(err_text("no Summarizer configured"));
    }
    let Some(s) = sess else {
        return Ok((None, Finish::noop()));
    };
    let events = s.events();
    let window = select_sliding_window(&events, cfg.compaction_interval, cfg.overlap_size);
    if window.is_empty() {
        return Ok((None, Finish::noop()));
    }
    summarize_traced(
        platform,
        cfg,
        sess,
        invocation_id,
        COMPACTION_TRIGGER_SLIDING_WINDOW,
        &window,
    )
    .await
    .map_err(|e| wrap_err("sliding-window summarization failed", e))
}

/// Port of Go summarizeTraced: runs the summarizer inside a `compact_events` span, validates
/// and stamps what comes back. The span stays open until the returned `Finish` runs. A
/// summarizer panic is recorded on the span (`summarizer panicked: <payload>`), the span is
/// closed, and the panic continues.
pub(crate) async fn summarize_traced(
    platform: &Platform,
    cfg: &Config,
    sess: Option<&dyn Session>,
    invocation_id: &str,
    trigger: &'static str,
    window: &[Event],
) -> Result<(Option<Event>, Finish), BoxError> {
    let Some(summarizer) = cfg.summarizer.clone() else {
        return Err(err_text("no Summarizer configured"));
    };
    let session_id = sess.map(|s| s.id().to_owned()).unwrap_or_default();
    // The caller names the turn; the newest event is only a fallback.
    let invocation_id = if invocation_id.is_empty() {
        latest_invocation_id(sess)
    } else {
        invocation_id.to_owned()
    };
    let span = compact_events_span(&span_params(
        cfg,
        &session_id,
        &invocation_id,
        trigger,
        window.len(),
    ));

    let snapshot = snapshot_for_summarizer(window);
    let outcome = AssertUnwindSafe(
        summarizer
            .summarize_events(&snapshot)
            .instrument(span.clone()),
    )
    .catch_unwind()
    .await;
    let res = match outcome {
        Ok(res) => res,
        Err(payload) => {
            let msg = format!("summarizer panicked: {}", panic_message(&*payload));
            record_span_error(&span, &msg);
            drop(span);
            std::panic::resume_unwind(payload);
        }
    };

    let mut declined = false;
    let mut summary: Option<Event> = None;
    let mut failure: Option<BoxError> = None;
    let usage = match res {
        // A failure reports spend too (Go-parity note 4); any content is discarded.
        Err(e) => {
            let usage = failure_usage(&*e).cloned();
            failure = Some(e);
            usage
        }
        Ok(r) => {
            match &r.content {
                None => declined = true,
                Some(c) => {
                    let all = sess.map(|s| s.events()).unwrap_or_default();
                    match new_summary_event(window, &all, c, r.usage.as_ref()) {
                        Ok(ev) => summary = Some(ev),
                        Err(e) => failure = Some(e),
                    }
                }
            }
            r.usage
        }
    };
    // Stamped only once the result is known to be usable.
    let summary = summary.map(|ev| stamp(platform, ev));

    let stored_candidate = summary.clone();
    let finish = Finish::new(move |err, discard_reason| {
        let stored = if err.is_some() || !discard_reason.is_empty() {
            None
        } else {
            stored_candidate.as_ref()
        };
        trace_compaction_result(
            &span,
            CompactionResultParams {
                result_event: stored,
                error: err,
                discard_reason,
                declined,
                usage: usage.as_ref(),
            },
        );
        drop(span);
    });

    if let Some(e) = failure {
        finish.finish(Some(&*e as &(dyn std::error::Error + 'static)), "");
        return Err(e);
    }
    let Some(summary) = summary else {
        // A decline: nothing further can happen to it, so close the span here.
        finish.finish(None, "");
        return Ok((None, Finish::noop()));
    };
    Ok((Some(summary), finish))
}

/// Port of Go stamp: fills the identity fields a summarizer leaves blank. The invocation ID is
/// fresh (`"e-" + uuid`), never borrowed from a covered turn.
pub(crate) fn stamp(platform: &Platform, mut ev: Event) -> Event {
    if ev.id.is_empty() {
        ev.id = platform.new_uuid();
    }
    if ev.invocation_id.is_empty() {
        ev.invocation_id = format!("e-{}", platform.new_uuid());
    }
    if ev.timestamp == zero_time() {
        ev.timestamp = platform.now();
    }
    ev
}

/// Port of Go snapshotForSummarizer (Go-parity note 1): the conversation, field by field, and
/// for a summary only the range it stood for.
pub(crate) fn snapshot_for_summarizer(events: &[Event]) -> Vec<Event> {
    events
        .iter()
        .map(|ev| {
            let mut clone = Event {
                id: ev.id.clone(),
                timestamp: ev.timestamp,
                invocation_id: ev.invocation_id.clone(),
                branch: ev.branch.clone(),
                isolation_scope: ev.isolation_scope.clone(),
                author: ev.author.clone(),
                ..Default::default()
            };
            clone.response.content = ev.response.content.as_ref().map(copy_content);
            if let Some(rng) = &ev.actions.compaction {
                clone.actions.compaction = Some(EventCompaction {
                    start_timestamp: rng.start_timestamp,
                    end_timestamp: rng.end_timestamp,
                    compacted_content: None,
                    excluded_events: Vec::new(),
                });
            }
            clone
        })
        .collect()
}

/// Port of Go copyContent.
fn copy_content(c: &Content) -> Content {
    let mut out = c.clone();
    out.parts = c.parts.iter().map(copy_part).collect();
    out
}

/// Port of Go copyPart: exactly the members a transcript reads.
fn copy_part(p: &Part) -> Part {
    Part {
        text: p.text.clone(),
        thought: p.thought,
        function_call: p.function_call.as_ref().map(|fc| FunctionCall {
            id: fc.id.clone(),
            name: fc.name.clone(),
            args: fc.args.clone(),
            ..Default::default()
        }),
        function_response: p.function_response.as_ref().map(|fr| FunctionResponse {
            id: fr.id.clone(),
            name: fr.name.clone(),
            response: fr.response.clone(),
            ..Default::default()
        }),
        inline_data: p.inline_data.as_ref().map(|b| Blob {
            data: b.data.clone(),
            display_name: b.display_name.clone(),
            mime_type: b.mime_type.clone(),
        }),
        file_data: p.file_data.as_ref().map(|fd| FileData {
            display_name: fd.display_name.clone(),
            file_uri: fd.file_uri.clone(),
            mime_type: fd.mime_type.clone(),
        }),
        executable_code: p.executable_code.as_ref().map(|ec| ExecutableCode {
            code: ec.code.clone(),
            language: ec.language.clone(),
            id: ec.id.clone(),
        }),
        code_execution_result: p
            .code_execution_result
            .as_ref()
            .map(|cr| CodeExecutionResult {
                outcome: cr.outcome.clone(),
                output: cr.output.clone(),
                id: cr.id.clone(),
            }),
        ..Default::default()
    }
}

/// Port of Go summarizerTypeName: `""` without a summarizer, else its bare type name.
pub(crate) fn summarizer_type_name(s: Option<&Arc<dyn Summarizer>>) -> String {
    s.map(|s| s.type_name().to_owned()).unwrap_or_default()
}

/// Port of Go summarizerBackend.
pub(crate) fn summarizer_backend(s: Option<&Arc<dyn Summarizer>>) -> Option<Backend> {
    s.and_then(|s| s.google_variant())
}

/// Port of Go latestInvocationID: the invocation of the newest event that has one.
pub(crate) fn latest_invocation_id(sess: Option<&dyn Session>) -> String {
    sess.map(|s| s.events())
        .unwrap_or_default()
        .into_iter()
        .rev()
        .find(|ev| !ev.invocation_id.is_empty())
        .map(|ev| ev.invocation_id)
        .unwrap_or_default()
}

/// Port of Go spanParams.
pub(crate) fn span_params(
    cfg: &Config,
    session_id: &str,
    invocation_id: &str,
    trigger: &'static str,
    event_count: usize,
) -> CompactEventsSpanParams {
    CompactEventsSpanParams {
        trigger,
        session_id: session_id.to_owned(),
        invocation_id: invocation_id.to_owned(),
        summarizer_type: summarizer_type_name(cfg.summarizer.as_ref()),
        backend: summarizer_backend(cfg.summarizer.as_ref()),
        event_count,
        compaction_interval: cfg.compaction_interval,
        overlap_size: cfg.overlap_size,
        token_threshold: cfg.token_threshold,
        event_retention_size: cfg.event_retention_size,
    }
}
