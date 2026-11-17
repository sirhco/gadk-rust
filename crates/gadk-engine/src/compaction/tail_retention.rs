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

//! Intra-invocation tail-retention compaction, run before a model call once the prompt crosses
//! the token threshold.

use std::sync::Arc;

use gadk_genai::Content;
use gadk_model::LlmResponse;
use gadk_platform::Platform;
use gadk_session::compaction::Config;
use gadk_session::{Event, EventActions, Session};
use serde_json::{Map, Value};

use super::apply::{CoverIndex, has_compaction};
use super::compactor::{Finish, latest_invocation_id, span_params, summarize_traced};
use super::window::{
    latest_compaction_event_in_scope, longest_self_contained_prefix, skip_blocked_head,
    trim_to_one_scope,
};
use super::{BoxError, ProgressGate, err_text, has_tail_retention, wrap_err};
use crate::llm::processors::go_json_marshal;
use crate::telemetry::{COMPACTION_TRIGGER_TOKEN_THRESHOLD, trace_compaction_declined};
use crate::tool::utils::event_belongs_to_branch;

/// Estimates the prompt tokens a list of events would cost. Consulted only when no visible event
/// reports an observed count; `0` means unknown.
pub(crate) type TokenCounter = dyn Fn(&[Event]) -> i64 + Send + Sync;

/// The invocation in flight and the branch and isolation scope it sees.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TurnScope {
    /// The turn in flight, whose opening question must not be summarized.
    pub(crate) invocation_id: String,
    /// The branch the turn runs under.
    pub(crate) branch: String,
    /// The isolation scope the turn runs under.
    pub(crate) isolation_scope: String,
}

impl TurnScope {
    /// Whether `ev` is part of the history this turn can see.
    pub(crate) fn visible(&self, ev: &Event) -> bool {
        event_belongs_to_branch(&self.branch, &ev.branch)
            && ev.isolation_scope == self.isolation_scope
    }
}

/// Summarizes everything but the most recent events once the prompt has grown past
/// `cfg.token_threshold`. `Ok((None, _))` = nothing to do, declined, or gated;
/// the returned [`Finish`] must be called exactly once with what became of a produced summary.
/// A summarizer panic is not caught here: `summarize_traced` records it and the unwind
/// continues.
pub(crate) async fn tail_retention(
    platform: &Platform,
    cfg: &Config,
    sess: Option<&dyn Session>,
    scope: &TurnScope,
    estimate: Option<&TokenCounter>,
    progress: Option<Arc<dyn ProgressGate>>,
) -> Result<(Option<Event>, Finish), BoxError> {
    if !has_tail_retention(Some(cfg)) {
        return Ok((None, Finish::noop()));
    }
    if cfg.summarizer.is_none() {
        return Err(err_text("no Summarizer configured"));
    }
    let Some(sess) = sess else {
        return Ok((None, Finish::noop()));
    };

    let events = sess.events();
    let Some(tokens) = prompt_token_count(&events, scope, estimate) else {
        return Ok((None, Finish::noop()));
    };
    if tokens < i64::from(cfg.token_threshold) {
        // Under the threshold, so any earlier compaction in this turn did its job: re-arm.
        if let Some(p) = &progress {
            p.recovered();
        }
        return Ok((None, Finish::noop()));
    }

    // The last compaction in this turn has not brought the prompt back under the threshold.
    if let Some(p) = &progress
        && !p.allow_at()
    {
        trace_declined(
            cfg,
            Some(sess),
            "the previous compaction did not bring the prompt back under the threshold",
        );
        return Ok((None, Finish::noop()));
    }

    let window = select_tail_retention_window(&events, cfg.event_retention_size, scope);
    if window.is_empty() {
        // Recorded, but deliberately not a failure: "not yet", not "not ever".
        trace_declined(
            cfg,
            Some(sess),
            "no compactable window past the retained tail",
        );
        return Ok((None, Finish::noop()));
    }

    let (summary, finish) = match summarize_traced(
        platform,
        cfg,
        Some(sess),
        &scope.invocation_id,
        COMPACTION_TRIGGER_TOKEN_THRESHOLD,
        &window,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            // The caller gets no Finish on this path, so the gate is closed here.
            if let Some(p) = &progress {
                p.failed();
            }
            return Err(wrap_err("tail-retention summarization failed", e));
        }
    };
    let Some(summary) = summary else {
        // A decline: the caller gets no Finish either.
        if let Some(p) = &progress {
            p.failed();
        }
        return Ok((None, Finish::noop()));
    };
    // The gate learns the outcome before the span closes. A stored summary is recorded, a failure
    // closes the gate, a discard leaves it open.
    let finish = finish.then_after(move |err, discard_reason| {
        if let Some(p) = progress {
            if err.is_some() {
                p.failed();
            } else if discard_reason.is_empty() {
                p.record_at(tokens);
            }
        }
    });
    Ok((Some(summary), finish))
}

/// The characters assumed per token when estimating.
const CHARS_PER_TOKEN: i64 = 4;

/// The character count of the JSON encoding of `v` (as `go_json_marshal` writes it); 0 for an
/// empty map.
fn json_chars(v: &Map<String, Value>) -> i64 {
    if v.is_empty() {
        return 0;
    }
    go_json_marshal(&Value::Object(v.clone())).chars().count() as i64
}

/// The newest visible observed prompt count plus an estimate of what was appended after it, else
/// the estimate of everything. `None` = unknown.
pub(crate) fn prompt_token_count(
    events: &[Event],
    scope: &TurnScope,
    estimate: Option<&TokenCounter>,
) -> Option<i64> {
    for (i, ev) in events.iter().enumerate().rev() {
        // Only this turn's view, and never a summary's own (summarizer) usage.
        if !scope.visible(ev) || has_compaction(ev) {
            continue;
        }
        if let Some(usage) = &ev.response.usage_metadata
            && usage.prompt_token_count > 0
        {
            let mut tokens = i64::from(usage.prompt_token_count);
            if let Some(est) = estimate
                && i < events.len() - 1
            {
                tokens += est(&events[i + 1..]);
            }
            return Some(tokens);
        }
    }
    let est = estimate?;
    let tokens = est(events);
    (tokens > 0).then_some(tokens)
}

/// Estimates tokens from contents: characters of text, tool names and JSON payloads,
/// four per token. A floor, not an estimate: it sees nothing outside `contents`.
pub(crate) fn estimate_tokens_from_contents(contents: &[Content]) -> i64 {
    let mut chars: i64 = 0;
    for content in contents {
        for part in &content.parts {
            chars += part.text.chars().count() as i64;
            if let Some(fc) = &part.function_call {
                chars += fc.name.chars().count() as i64 + json_chars(&fc.args);
            }
            if let Some(fr) = &part.function_response {
                chars += fr.name.chars().count() as i64 + json_chars(&fr.response);
            }
        }
    }
    if chars <= 0 {
        return 0;
    }
    chars / CHARS_PER_TOKEN
}

/// The uncovered, visible events older than the retained tail (the live turn's opening question
/// held back), trimmed to one scope and a self-contained prefix, seeded with the scope's
/// previous summary. Empty = nothing to compact.
pub(crate) fn select_tail_retention_window(
    events: &[Event],
    retention_size: i32,
    scope: &TurnScope,
) -> Vec<Event> {
    if retention_size < 0 {
        return Vec::new();
    }
    let retention = retention_size as usize;

    // Scoped to the window's own branch: a sibling compacting more recently must not stop this
    // one rolling up its own previous summary.
    let latest = latest_compaction_event_in_scope(events, &scope.branch, &scope.isolation_scope);

    // The turn being answered opens with the user's own question; hold that one event back.
    let live_head = if scope.invocation_id.is_empty() {
        ""
    } else {
        events
            .iter()
            .find(|ev| ev.invocation_id == scope.invocation_id && !has_compaction(ev))
            .map_or("", |ev| ev.id.as_str())
    };

    let cover = CoverIndex::new(events);
    let mut candidates: Vec<Event> = Vec::new();
    for (i, ev) in events.iter().enumerate() {
        if has_compaction(ev) || !scope.visible(ev) {
            continue;
        }
        if !live_head.is_empty() && ev.id == live_head {
            continue;
        }
        if cover.covers_after(i, ev) {
            continue;
        }
        candidates.push(ev.clone());
    }
    if candidates.len() <= retention {
        return Vec::new();
    }

    // Where the raw tail begins. The cut moves back past a same-timestamp group, because
    // coverage is inclusive of EndTimestamp.
    let mut first_retained = candidates.len();
    if retention > 0 {
        first_retained -= retention;
        let boundary = candidates[first_retained].timestamp;
        while first_retained > 0 && candidates[first_retained - 1].timestamp >= boundary {
            first_retained -= 1;
        }
    }
    candidates.truncate(first_retained);

    let scoped = trim_to_one_scope(candidates);
    let mut window = longest_self_contained_prefix(&scoped);
    if window.is_empty() {
        // The head holds a call nothing answered: step past it as the sliding window does.
        window = skip_blocked_head(scoped);
    }
    if window.is_empty() {
        return Vec::new();
    }

    let Some(latest) = latest else {
        return window;
    };
    let Some(prev) = latest.actions.compaction.clone() else {
        return window;
    };
    // The previous summary under its own identity, timestamped at the start of its range, so
    // the new compaction spans a strictly wider range and supersedes it.
    let seed = Event {
        id: latest.id.clone(),
        author: "model".to_owned(),
        timestamp: prev.start_timestamp,
        branch: latest.branch.clone(),
        isolation_scope: latest.isolation_scope.clone(),
        response: LlmResponse {
            content: prev.compacted_content.clone(),
            ..Default::default()
        },
        actions: EventActions {
            compaction: Some(prev),
            ..Default::default()
        },
        ..Default::default()
    };
    if seed.branch != window[0].branch || seed.isolation_scope != window[0].isolation_scope {
        // The rolling summary belongs to another scope: compact the window on its own.
        return window;
    }
    let mut out = Vec::with_capacity(window.len() + 1);
    out.push(seed);
    out.extend(window);
    out
}

/// Traces a compaction that was triggered but could not run.
fn trace_declined(cfg: &Config, sess: Option<&dyn Session>, reason: &str) {
    let id = sess.map(|s| s.id().to_owned()).unwrap_or_default();
    let params = span_params(
        cfg,
        &id,
        &latest_invocation_id(sess),
        COMPACTION_TRIGGER_TOKEN_THRESHOLD,
        0,
    );
    trace_compaction_declined(&params, reason);
}
