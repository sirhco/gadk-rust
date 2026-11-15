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

//! Port of Go internal/compactioninternal/window.go: sliding-window selection, the
//! self-contained prefix, and record subsumption.

use std::collections::{HashMap, HashSet};

use gadk_genai::FunctionCall;
use gadk_session::{Event, EventCompaction, EventRef};

use super::apply::{CoverIndex, has_compaction, has_usable_summary, truncate_to_ref};
use crate::tool::utils::{content, function_calls, function_responses};

/// Length of Go longestSelfContainedPrefix's result: the longest cut that leaves no call, or
/// request to the user (a tool confirmation or a credential request), open and does not split a group of events sharing a timestamp.
pub(crate) fn self_contained_len(events: &[Event]) -> usize {
    let mut open: HashSet<String> = HashSet::new();
    let mut safe = 0;
    for (i, ev) in events.iter().enumerate() {
        // Responses before calls: a response only closes an obligation of an earlier event.
        for resp in function_responses(content(ev)) {
            open.remove(&resp.id);
        }
        for call in function_calls(content(ev)) {
            open.insert(call_obligation_key(call, i));
        }
        for id in ev.actions.requested_tool_confirmations.keys() {
            open.insert(id.clone());
        }
        for id in ev.actions.requested_auth_configs.keys() {
            open.insert(id.clone());
        }
        if open.is_empty() && tie_safe_cut(events, i + 1) {
            safe = i + 1;
        }
    }
    safe
}

/// Port of Go longestSelfContainedPrefix.
pub(crate) fn longest_self_contained_prefix(events: &[Event]) -> Vec<Event> {
    events[..self_contained_len(events)].to_vec()
}

/// Port of Go tieSafeCut.
fn tie_safe_cut(events: &[Event], length: usize) -> bool {
    if length == 0 || length >= events.len() {
        return true;
    }
    events[length - 1].timestamp < events[length].timestamp
}

/// Port of Go callObligationKey: an ID-less call gets a key no response can match.
fn call_obligation_key(call: &FunctionCall, event_index: usize) -> String {
    if !call.id.is_empty() {
        return call.id.clone();
    }
    format!("\u{0}no-id\u{0}{event_index}\u{0}{}", call.name)
}

/// Port of Go LatestCompactionEventInScope: Go LatestCompactionEvent over the records of
/// one branch and isolation scope.
pub(crate) fn latest_compaction_event_in_scope<'a>(
    events: &'a [Event],
    branch: &str,
    isolation_scope: &str,
) -> Option<&'a Event> {
    let scoped: Vec<&Event> = events
        .iter()
        .filter(|ev| {
            has_compaction(ev) && ev.branch == branch && ev.isolation_scope == isolation_scope
        })
        .collect();
    latest_among(&scoped)
}

/// Port of Go LatestCompactionEvent: the newest record no other record subsumes.
#[cfg(test)]
pub(crate) fn latest_compaction_event(events: &[Event]) -> Option<&Event> {
    let all: Vec<&Event> = events.iter().collect();
    latest_among(&all)
}

/// Go LatestCompactionEvent over a list of references (positions are within `events`).
fn latest_among<'a>(events: &[&'a Event]) -> Option<&'a Event> {
    let (at, recs): (Vec<usize>, Vec<&Event>) = events
        .iter()
        .enumerate()
        .filter(|(_, ev)| has_compaction(ev))
        .map(|(j, ev)| (j, *ev))
        .unzip();
    let mut latest = None;
    for (k, ev) in recs.iter().enumerate() {
        let Some(rng) = ev.actions.compaction.as_ref() else {
            continue;
        };
        // hasCompaction, not HasUsableSummary: a contentless record still marks how far
        // compaction reached.
        if is_subsumed_among(at[k], rng, &at, &recs) {
            continue;
        }
        latest = Some(*ev);
    }
    latest
}

/// Port of Go isSubsumedAmong: `rng` is the record at stream position `i`; `at[k]` is the
/// position of `recs[k]`.
pub(crate) fn is_subsumed_among(
    i: usize,
    rng: &EventCompaction,
    at: &[usize],
    recs: &[&Event],
) -> bool {
    for (k, other) in recs.iter().enumerate() {
        let j = at[k];
        // Only a record carrying usable content may evict another.
        if j == i || !has_usable_summary(other) {
            continue;
        }
        let Some(o) = other.actions.compaction.as_ref() else {
            continue;
        };
        // An identical range is decided by stream position alone.
        if same_range(o, rng) {
            if j > i {
                return true;
            }
            continue;
        }
        if !covers_all_of(o, rng) {
            continue;
        }
        // Only a later record may subsume an earlier one.
        if j > i {
            return true;
        }
    }
    false
}

/// Port of Go selectSlidingWindow: the contiguous slice a sliding-window compaction should
/// summarize, or empty when there is nothing to compact yet.
pub(crate) fn select_sliding_window(events: &[Event], interval: i32, overlap: i32) -> Vec<Event> {
    if interval <= 0 {
        return Vec::new();
    }

    // Invocations in first-seen order, and whether each still holds anything uncovered.
    let mut order: Vec<&str> = Vec::new();
    let mut is_new: HashMap<&str, bool> = HashMap::new();
    let cover = CoverIndex::new(events);
    for (i, ev) in events.iter().enumerate() {
        if has_compaction(ev) || ev.invocation_id.is_empty() {
            continue;
        }
        let id = ev.invocation_id.as_str();
        if !is_new.contains_key(id) {
            order.push(id);
            is_new.insert(id, false);
        }
        if !cover.covers_after(i, ev) {
            is_new.insert(id, true);
        }
    }

    let new_positions: Vec<usize> = order
        .iter()
        .enumerate()
        .filter(|(_, id)| is_new.get(*id).copied().unwrap_or(false))
        .map(|(i, _)| i)
        .collect();
    let Some(&first_new) = new_positions.first() else {
        return Vec::new();
    };
    if (new_positions.len() as i64) < i64::from(interval) {
        return Vec::new();
    }

    // Cover at most `interval` new invocations; reach back `overlap` before the first new one.
    // Go indexes `order` without a bound; the start is clamped so no input can panic.
    let last_order = order.len() as i64 - 1;
    let start_pos = (first_new as i64 - i64::from(overlap)).clamp(0, last_order) as usize;
    let cap = usize::try_from(interval - 1).unwrap_or(0);
    let end_pos = new_positions[(new_positions.len() - 1).min(cap)];

    let position: HashMap<&str, usize> = order.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    // Go `position[ev.InvocationID] >= firstNew`, where a missing key reads as 0.
    let stale_at = |idx: usize, ev: &Event| {
        position
            .get(ev.invocation_id.as_str())
            .copied()
            .unwrap_or(0)
            >= first_new
            && cover.covers_after(idx, ev)
    };

    // Slice from the first uncovered event of the start invocation through the last event of
    // an invocation at or before the end position.
    let (mut first, mut last): (Option<usize>, Option<usize>) = (None, None);
    for (i, ev) in events.iter().enumerate() {
        if has_compaction(ev) || stale_at(i, ev) {
            continue;
        }
        let Some(&pos) = position.get(ev.invocation_id.as_str()) else {
            continue;
        };
        if first.is_none() && pos >= start_pos {
            first = Some(i);
        }
        if pos <= end_pos {
            last = Some(i);
        }
    }
    let (Some(first), Some(last)) = (first, last) else {
        return Vec::new();
    };
    if last < first {
        return Vec::new();
    }

    let mut window: Vec<Event> = Vec::with_capacity(last - first + 1);
    for (off, ev) in events[first..=last].iter().enumerate() {
        if has_compaction(ev) || stale_at(first + off, ev) {
            continue;
        }
        window.push(ev.clone());
    }

    let mut window = trim_to_one_scope(window);
    let n = self_contained_len(&window);
    if n > 0 {
        window.truncate(n);
        return window;
    }
    skip_blocked_head(window)
}

/// Port of Go trimToOneScope: cuts the window at the first change of branch or isolation scope.
pub(crate) fn trim_to_one_scope(mut window: Vec<Event>) -> Vec<Event> {
    let Some(first) = window.first() else {
        return window;
    };
    let (branch, scope) = (first.branch.clone(), first.isolation_scope.clone());
    if let Some(i) = window
        .iter()
        .position(|ev| ev.branch != branch || ev.isolation_scope != scope)
    {
        window.truncate(i);
    }
    window
}

/// Port of Go skipBlockedHead: the longest self-contained run after a head whose call never got
/// a response, refusing a run that answers a call left in the skipped head.
pub(crate) fn skip_blocked_head(window: Vec<Event>) -> Vec<Event> {
    for start in 1..window.len() {
        // Resume just after an event that changed the set of open obligations.
        let prev = &window[start - 1];
        if function_calls(content(prev)).is_empty()
            && function_responses(content(prev)).is_empty()
            && prev.actions.requested_tool_confirmations.is_empty()
            && prev.actions.requested_auth_configs.is_empty()
        {
            continue;
        }
        let n = self_contained_len(&window[start..]);
        if n == 0 {
            continue;
        }
        let tail = &window[start..start + n];
        if answers_any_of(tail, &open_call_ids(&window[..start])) {
            continue;
        }
        return tail.to_vec();
    }
    Vec::new()
}

/// Port of Go openCallIDs: the call IDs opened by `events` and not answered by them.
pub(crate) fn open_call_ids(events: &[Event]) -> HashSet<String> {
    let mut open = HashSet::new();
    for (i, ev) in events.iter().enumerate() {
        for resp in function_responses(content(ev)) {
            open.remove(&resp.id);
        }
        for call in function_calls(content(ev)) {
            open.insert(call_obligation_key(call, i));
        }
        for id in ev.actions.requested_tool_confirmations.keys() {
            open.insert(id.clone());
        }
        for id in ev.actions.requested_auth_configs.keys() {
            open.insert(id.clone());
        }
    }
    open
}

/// Port of Go answersAnyOf: whether `events` answer any of `ids`.
pub(crate) fn answers_any_of(events: &[Event], ids: &HashSet<String>) -> bool {
    if ids.is_empty() {
        return false;
    }
    events.iter().any(|ev| {
        function_responses(content(ev))
            .iter()
            .any(|r| ids.contains(&r.id))
    })
}

/// Port of Go coversAllOf: `a`'s range contains `b`'s, and every hole of `a` inside `b` (at
/// reference resolution) is a hole of `b` too.
pub(crate) fn covers_all_of(a: &EventCompaction, b: &EventCompaction) -> bool {
    if a.start_timestamp > b.start_timestamp || a.end_timestamp < b.end_timestamp {
        return false;
    }
    let (b_start, b_end) = (
        truncate_to_ref(b.start_timestamp),
        truncate_to_ref(b.end_timestamp),
    );
    for r in &a.excluded_events {
        let ref_at = truncate_to_ref(r.timestamp);
        if ref_at < b_start || ref_at > b_end {
            continue;
        }
        // An event `a` leaves out is fine only if `b` leaves it out too.
        if !names_hole(b, r) {
            return false;
        }
    }
    true
}

/// Port of Go namesHole: `rng` excludes the event `r` names, compared at reference resolution.
pub(crate) fn names_hole(rng: &EventCompaction, r: &EventRef) -> bool {
    let at = truncate_to_ref(r.timestamp);
    rng.excluded_events
        .iter()
        .any(|o| o.invocation_id == r.invocation_id && truncate_to_ref(o.timestamp) == at)
}

/// Port of Go sameRange.
pub(crate) fn same_range(a: &EventCompaction, b: &EventCompaction) -> bool {
    a.start_timestamp == b.start_timestamp && a.end_timestamp == b.end_timestamp
}
