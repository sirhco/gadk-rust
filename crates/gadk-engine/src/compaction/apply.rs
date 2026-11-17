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

//! Materializing stored summaries into a prompt, the race checks, the session re-read, and the
//! post-append repair.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Timelike, Utc};
use gadk_session::{Event, EventCompaction, EventRef, GetRequest, Session, SessionService};

use super::summary_event::ref_key;
use super::window::{is_subsumed_among, names_hole, same_range};
use super::{BoxError, err_text, wrap_err};
use crate::tool::utils::{content, function_calls, function_responses, text_parts};

/// Unwraps a session wrapper down to the underlying session.
pub(crate) use crate::llm::agent::underlying_session as unwrap_session;

/// How long a post-append repair may take before it is abandoned.
pub(crate) const REPAIR_TIMEOUT: Duration = Duration::from_secs(30);

/// The resolution, in nanoseconds (one microsecond), at which event timestamps are compared when
/// matching event references.
pub(crate) const REF_RESOLUTION_NANOS: u32 = 1_000;

/// Truncates `t` to [`REF_RESOLUTION_NANOS`].
pub(crate) fn truncate_to_ref(t: DateTime<Utc>) -> DateTime<Utc> {
    t.with_nanosecond(t.nanosecond() / REF_RESOLUTION_NANOS * REF_RESOLUTION_NANOS)
        .unwrap_or(t)
}

/// Summaries stand in for the events they cover; `events` is not modified.
pub(crate) fn apply(events: &[Event]) -> Vec<Event> {
    if !events.iter().any(has_compaction) {
        return events.to_vec();
    }
    recover_compacted_function_calls(substitute_summaries(events), events)
}

/// Whether the event declares a compaction.
pub(crate) fn has_compaction(ev: &Event) -> bool {
    ev.actions.compaction.is_some()
}

/// Whether the event's compaction carries summary content: deliberately only a presence check
/// (adk-python parity).
pub(crate) fn has_usable_summary(ev: &Event) -> bool {
    ev.actions
        .compaction
        .as_ref()
        .is_some_and(|c| c.compacted_content.is_some())
}

/// The stream positions and the records of every event declaring a compaction.
pub(crate) fn records(events: &[Event]) -> (Vec<usize>, Vec<&Event>) {
    events
        .iter()
        .enumerate()
        .filter(|(_, ev)| has_compaction(ev))
        .unzip()
}

/// Runs the corrective write detached from the caller and bounded by [`REPAIR_TIMEOUT`]; `None`
/// means it timed out.
///
/// The repair is detached from the caller's cancellation. Dropping a Rust future is its
/// cancellation, so `fut` is spawned onto the runtime (inside the caller's current span) and the
/// returned future only awaits it: dropping the caller mid-repair leaves the repair running to
/// completion or to the timeout. The summary is already stored and claims a range wider than it
/// summarized until this lands. A panic inside `fut` resumes on the caller, as an inline repair
/// would propagate it.
pub(crate) async fn repair<F>(fut: F) -> Option<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    let task = tokio::spawn(tracing::Instrument::in_current_span(tokio::time::timeout(
        REPAIR_TIMEOUT,
        fut,
    )));
    match task.await {
        Ok(done) => done.ok(),
        Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        // Cancelled only by the runtime shutting down: nothing is left to repair on.
        Err(_) => None,
    }
}

/// A surviving summary record: the range it covers and where it came from.
struct KeptRange {
    /// The coverage guard: only events before it can be covered.
    index: usize,
    /// The position of the record supplying the emitted summary event.
    src: usize,
    rng: EventCompaction,
}

/// Replaces each covered run of events with its summary event and drops the compaction records.
fn substitute_summaries(events: &[Event]) -> Vec<Event> {
    let (rec_at, recs) = records(events);
    let mut kept: Vec<KeptRange> = Vec::new();
    for (k, ev) in recs.iter().enumerate() {
        let i = rec_at[k];
        if !has_usable_summary(ev) {
            continue;
        }
        let Some(rng) = ev.actions.compaction.as_ref() else {
            continue;
        };
        // An inverted range covers nothing.
        if rng.end_timestamp < rng.start_timestamp {
            continue;
        }
        if is_subsumed_among(i, rng, &rec_at, &recs) {
            continue;
        }
        let (pos, folded) = fold_corrections(i, rng, &rec_at, &recs);
        kept.push(KeptRange {
            index: pos,
            src: i,
            rng: folded,
        });
    }

    // Each surviving summary is emitted where the first event it covers sat.
    let mut summaries_at: HashMap<usize, Vec<usize>> = HashMap::new();
    for (n, k) in kept.iter().enumerate() {
        summaries_at
            .entry(summary_index(events, k))
            .or_default()
            .push(n);
    }

    let mut out = Vec::with_capacity(events.len());
    for (i, ev) in events.iter().enumerate() {
        if let Some(ns) = summaries_at.get(&i) {
            for &n in ns {
                let k = &kept[n];
                let mut summary = events[k.src].clone();
                summary.author = "model".into();
                summary.timestamp = k.rng.end_timestamp;
                summary.response.content = k.rng.compacted_content.clone();
                out.push(summary);
            }
        }
        if has_compaction(ev) {
            continue;
        }
        if is_covered(i, ev, &kept) {
            continue;
        }
        out.push(ev.clone());
    }
    out
}

/// The index at which the summary is emitted: that of the first event it covers, else its guard.
fn summary_index(events: &[Event], k: &KeptRange) -> usize {
    events
        .iter()
        .enumerate()
        .find(|(i, ev)| !has_compaction(ev) && covered_by(*i, ev, k))
        .map_or(k.index, |(i, _)| i)
}

/// Whether any kept summary covers the event at `i`.
fn is_covered(i: usize, ev: &Event, kept: &[KeptRange]) -> bool {
    kept.iter().any(|k| covered_by(i, ev, k))
}

/// Whether `k` covers the event at `i`: only a record later in the stream covers an event.
fn covered_by(i: usize, ev: &Event, k: &KeptRange) -> bool {
    i < k.index && in_range(ev, &k.rng)
}

/// Re-injects the call events of long-running function calls whose responses survive compaction
/// but whose calls were summarized away, with the freshest responses of their sibling calls, so
/// no response is left without its call.
fn recover_compacted_function_calls(events: Vec<Event>, source: &[Event]) -> Vec<Event> {
    let mut present_calls: HashSet<String> = HashSet::new();
    let mut present_responses: HashSet<String> = HashSet::new();
    for ev in &events {
        for call in function_calls(content(ev)) {
            present_calls.insert(call.id.clone());
        }
        for resp in function_responses(content(ev)) {
            present_responses.insert(resp.id.clone());
        }
    }
    let orphaned: HashSet<&String> = present_responses
        .iter()
        .filter(|id| !id.is_empty() && !present_calls.contains(*id))
        .collect();
    if orphaned.is_empty() {
        return events;
    }

    // The long-running call events matching the orphaned responses (first in source order).
    let mut call_event_by_id: HashMap<String, usize> = HashMap::new();
    for (j, ev) in source.iter().enumerate() {
        for call in function_calls(content(ev)) {
            if !orphaned.contains(&call.id) || call_event_by_id.contains_key(&call.id) {
                continue;
            }
            if ev.long_running_tool_ids.contains(&call.id) {
                call_event_by_id.insert(call.id.clone(), j);
            }
        }
    }
    if call_event_by_id.is_empty() {
        return events;
    }

    // Freshest response event per call ID (a later-or-equal timestamp replaces).
    let mut final_response_by_id: HashMap<String, usize> = HashMap::new();
    for (j, ev) in source.iter().enumerate() {
        for resp in function_responses(content(ev)) {
            let replace = final_response_by_id
                .get(&resp.id)
                .is_none_or(|&p| ev.timestamp >= source[p].timestamp);
            if replace {
                final_response_by_id.insert(resp.id.clone(), j);
            }
        }
    }

    let mut result = Vec::with_capacity(events.len() + call_event_by_id.len());
    let mut reinjected: HashSet<String> = HashSet::new();
    for ev in events {
        let resp_ids: Vec<String> = function_responses(content(&ev))
            .iter()
            .map(|r| r.id.clone())
            .collect();
        for id in resp_ids {
            let Some(&ci) = call_event_by_id.get(&id) else {
                continue;
            };
            if reinjected.contains(&id) {
                continue;
            }
            let call_event = &source[ci];
            result.push(call_event.clone());
            // Every call in the recovered event is now present, siblings included.
            let mut siblings: Vec<usize> = Vec::new();
            for call in function_calls(content(call_event)) {
                reinjected.insert(call.id.clone());
                if present_responses.contains(&call.id) {
                    continue;
                }
                if let Some(&s) = final_response_by_id.get(&call.id)
                    && !siblings.contains(&s)
                {
                    siblings.push(s);
                }
            }
            result.extend(siblings.into_iter().map(|s| source[s].clone()));
        }
        result.push(ev);
    }
    result
}

/// Whether `latest` gained an event inside `summary`'s range that `selected_from` did not hold.
pub(crate) fn range_raced(
    latest: &dyn Session,
    selected_from: &dyn Session,
    summary: &Event,
) -> bool {
    range_raced_since(latest, &known_event_ids(selected_from), summary)
}

/// The reference keys ([`ref_key`]) of the session's events now.
pub(crate) fn known_event_ids(sess: &dyn Session) -> HashSet<String> {
    sess.events().iter().map(ref_key).collect()
}

/// [`range_raced`] against identities captured earlier.
pub(crate) fn range_raced_since(
    latest: &dyn Session,
    known: &HashSet<String>,
    summary: &Event,
) -> bool {
    let Some(rng) = summary.actions.compaction.as_ref() else {
        return false;
    };
    for ev in latest.events() {
        // Already present when the window was selected, so not a racer.
        if known.contains(&ref_key(&ev)) {
            continue;
        }
        if let Some(other) = ev.actions.compaction.as_ref() {
            if overlaps(rng, other) {
                return true;
            }
            continue;
        }
        if ev.timestamp >= rng.start_timestamp && ev.timestamp <= rng.end_timestamp {
            return true;
        }
    }
    false
}

/// Whether `rng` names `ev` as a hole, compared at [`REF_RESOLUTION_NANOS`].
pub(crate) fn excludes(rng: &EventCompaction, ev: &Event) -> bool {
    let ev_at = truncate_to_ref(ev.timestamp);
    rng.excluded_events
        .iter()
        .any(|r| r.invocation_id == ev.invocation_id && truncate_to_ref(r.timestamp) == ev_at)
}

/// The usable, unsuperseded records of a session in stream order.
pub(crate) struct CoverIndex {
    at: Vec<usize>,
    rngs: Vec<EventCompaction>,
}

impl CoverIndex {
    /// Indexes the records of `events`: superseded and contentless records are skipped, as
    /// assembly skips them.
    pub(crate) fn new(events: &[Event]) -> CoverIndex {
        let (rec_at, recs) = records(events);
        let mut idx = CoverIndex {
            at: Vec::new(),
            rngs: Vec::new(),
        };
        for (k, ev) in recs.iter().enumerate() {
            let Some(rng) = ev.actions.compaction.as_ref() else {
                continue;
            };
            if is_subsumed_among(rec_at[k], rng, &rec_at, &recs) {
                continue;
            }
            if !has_usable_summary(ev) {
                continue;
            }
            let (pos, folded) = fold_corrections(rec_at[k], rng, &rec_at, &recs);
            idx.at.push(pos);
            idx.rngs.push(folded);
        }
        idx
    }

    /// Whether a record later in the stream than `i` stands in for `ev`.
    pub(crate) fn covers_after(&self, i: usize, ev: &Event) -> bool {
        self.at
            .iter()
            .zip(&self.rngs)
            .any(|(&at, rng)| at > i && in_range(ev, rng))
    }
}

/// Merges `rng` (the record at stream position `i`) with the corrections of it over the same
/// interval and summary text.
fn fold_corrections(
    i: usize,
    rng: &EventCompaction,
    at: &[usize],
    recs: &[&Event],
) -> (usize, EventCompaction) {
    let mut earliest = i;
    let mut extra: Vec<EventRef> = Vec::new();
    for (k, other) in recs.iter().enumerate() {
        let Some(o) = other.actions.compaction.as_ref() else {
            continue;
        };
        // `o` is the record itself.
        if at[k] == i || !same_range(o, rng) || !same_summary_content(o, rng) {
            continue;
        }
        earliest = earliest.min(at[k]);
        for r in &o.excluded_events {
            if !names_hole(rng, r) {
                extra.push(r.clone());
            }
        }
    }
    if extra.is_empty() {
        return (earliest, rng.clone());
    }
    let mut merged = rng.clone();
    merged.excluded_events.extend(extra);
    (earliest, merged)
}

/// Whether the two compactions' intervals overlap (inclusive).
pub(crate) fn overlaps(a: &EventCompaction, b: &EventCompaction) -> bool {
    a.start_timestamp <= b.end_timestamp && b.start_timestamp <= a.end_timestamp
}

/// Re-reads the session from its service. There is no separate "session disappeared" outcome: a
/// failed read is an error.
pub(crate) async fn reload_session(
    svc: Option<&dyn SessionService>,
    s: &dyn Session,
) -> Result<Arc<dyn Session>, BoxError> {
    let Some(svc) = svc else {
        return Err(err_text("cannot re-read the session: no session service"));
    };
    svc.get(GetRequest {
        app_name: s.app_name().to_owned(),
        user_id: s.user_id().to_owned(),
        session_id: s.id().to_owned(),
        ..Default::default()
    })
    .await
    .map_err(|e| wrap_err("failed to re-read the session", Box::new(e)))
}

/// Whether `ev` is inside the inclusive interval and not named as a hole.
pub(crate) fn in_range(ev: &Event, rng: &EventCompaction) -> bool {
    if ev.timestamp < rng.start_timestamp || ev.timestamp > rng.end_timestamp {
        return false;
    }
    !excludes(rng, ev)
}

/// A corrected copy of `stored` naming every event that landed inside its range unseen, or `None`
/// when nothing did.
pub(crate) fn repair_after_append(
    stored: &Event,
    known: &HashSet<String>,
    latest: &dyn Session,
) -> Option<Event> {
    let rng = stored.actions.compaction.as_ref()?;
    let mut stragglers: Vec<EventRef> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for ev in latest.events() {
        if has_compaction(&ev) {
            continue;
        }
        if !in_range(&ev, rng) || excludes(rng, &ev) {
            continue;
        }
        let k = ref_key(&ev);
        // Present when the window was chosen, so it is what the summary was built from.
        if known.contains(&k) || !seen.insert(k) {
            continue;
        }
        stragglers.push(EventRef {
            invocation_id: ev.invocation_id.clone(),
            timestamp: ev.timestamp,
        });
    }
    if stragglers.is_empty() {
        return None;
    }
    let mut corrected = stored.clone();
    corrected.id = String::new();
    // Strictly after the record it corrects, at the resolution storage keeps.
    corrected.timestamp =
        stored.timestamp + chrono::Duration::nanoseconds(i64::from(REF_RESOLUTION_NANOS));
    let mut fixed = rng.clone();
    fixed.excluded_events.extend(stragglers);
    corrected.actions.compaction = Some(fixed);
    Some(corrected)
}

/// Whether both summaries have the same non-empty text.
fn same_summary_content(a: &EventCompaction, b: &EventCompaction) -> bool {
    let at = text_parts(a.compacted_content.as_ref()).join("\n");
    let bt = text_parts(b.compacted_content.as_ref()).join("\n");
    !at.is_empty() && at == bt
}
