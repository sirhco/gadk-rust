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

//! Port of Go internal/compactioninternal/summary_event.go: building and sanitizing the event
//! that carries a summary.

use std::collections::HashSet;

use chrono::{DateTime, Datelike, Timelike, Utc};
use gadk_genai::{Content, GenerateContentResponseUsageMetadata};
use gadk_model::LlmResponse;
use gadk_session::compaction::is_prose_part;
use gadk_session::{Event, EventActions, EventCompaction, EventRef};

use super::apply::{excludes, has_compaction, in_range, truncate_to_ref};
use super::{BoxError, err_text};

/// Port of Go newSummaryEvent: the unstamped compaction event (empty ID and invocation ID, zero
/// timestamp) for a window. Window membership is a counted value match against `all`, the
/// Rust form of Go's pointer identity.
pub(crate) fn new_summary_event(
    events: &[Event],
    all: &[Event],
    summary: &Content,
    usage: Option<&GenerateContentResponseUsageMetadata>,
) -> Result<Event, BoxError> {
    let Some(first) = events.first() else {
        return Err(err_text("cannot summarize an empty event list"));
    };
    if !has_prose(Some(summary)) {
        return Err(err_text(
            "summary content is empty, so compacting would delete the covered events and replace them with nothing",
        ));
    }
    // The bounding box over the window: a true minimum and maximum.
    let (mut start, mut end) = (first.timestamp, first.timestamp);
    for ev in &events[1..] {
        if ev.timestamp < start {
            start = ev.timestamp;
        }
        if ev.timestamp > end {
            end = ev.timestamp;
        }
    }

    // Only prose survives, each part copied whole except for its thought signature.
    let mut content = Content {
        role: "model".into(),
        parts: Vec::new(),
    };
    for p in &summary.parts {
        if !is_prose_part(p) {
            continue;
        }
        let mut part = p.clone();
        part.thought_signature = Default::default();
        content.parts.push(part);
    }
    if content.parts.is_empty() {
        return Err(err_text(
            "summary content holds no prose, so compacting would delete the covered events and replace them with nothing",
        ));
    }

    // The summary inherits the branch and isolation scope of what it covers.
    let (branch, scope) = (first.branch.clone(), first.isolation_scope.clone());

    // Window membership by identity: each window event claims one equal, unclaimed entry.
    let mut summarized = vec![false; all.len()];
    for w in events {
        if let Some(j) = (0..all.len()).find(|&j| {
            !summarized[j] && all[j].id == w.id && all[j].timestamp == w.timestamp && all[j] == *w
        }) {
            summarized[j] = true;
        }
    }

    // A rolled-up earlier summary stands for its own events transitively.
    let rolled: Vec<&EventCompaction> = events
        .iter()
        .filter_map(|ev| ev.actions.compaction.as_ref())
        .collect();
    let covered = |ev: &Event| {
        rolled
            .iter()
            .any(|rng| in_range(ev, rng) && !excludes(rng, ev))
    };

    let mut excluded: Vec<EventRef> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for (j, ev) in all.iter().enumerate() {
        if has_compaction(ev) {
            continue;
        }
        if ev.timestamp < start || ev.timestamp > end {
            continue;
        }
        if summarized[j] {
            continue;
        }
        let k = ref_key(ev);
        if seen.contains(&k) {
            continue;
        }
        if covered(ev) {
            continue;
        }
        seen.insert(k);
        excluded.push(EventRef {
            invocation_id: ev.invocation_id.clone(),
            timestamp: ev.timestamp,
        });
    }

    Ok(Event {
        // Authored as "user"; re-authored as "model" when materialized into a prompt.
        author: "user".into(),
        branch,
        isolation_scope: scope,
        actions: EventActions {
            compaction: Some(EventCompaction {
                start_timestamp: start,
                end_timestamp: end,
                compacted_content: Some(content),
                excluded_events: excluded,
            }),
            ..Default::default()
        },
        response: LlmResponse {
            usage_metadata: usage.cloned(),
            ..Default::default()
        },
        ..Default::default()
    })
}

/// Port of Go hasProse.
pub(crate) fn has_prose(c: Option<&Content>) -> bool {
    c.is_some_and(|c| c.parts.iter().any(is_prose_part))
}

/// Port of Go SanitizeSummary: keeps only prose parts of the record's content; false when nothing
/// usable survives.
pub(crate) fn sanitize_summary(ev: &mut Event) -> bool {
    let Some(rng) = ev.actions.compaction.as_mut() else {
        return false;
    };
    let Some(c) = rng.compacted_content.as_ref() else {
        return false;
    };
    let kept: Vec<_> = c
        .parts
        .iter()
        .filter(|p| is_prose_part(p))
        .cloned()
        .collect();
    if kept.is_empty() {
        return false;
    }
    let content = Content {
        role: c.role.clone(),
        parts: kept,
    };
    rng.compacted_content = Some(content);
    true
}

/// Port of Go refKey: `<invocation id>@<RFC 3339 nano of the truncated UTC time>`.
pub(crate) fn ref_key(ev: &Event) -> String {
    format!(
        "{}@{}",
        ev.invocation_id,
        go_rfc3339_nano(truncate_to_ref(ev.timestamp))
    )
}

/// Go `t.UTC().Format(time.RFC3339Nano)`: fractional seconds with trailing zeros trimmed and
/// the dot omitted when they are all zero.
fn go_rfc3339_nano(t: DateTime<Utc>) -> String {
    let mut s = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        t.year(),
        t.month(),
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    );
    // chrono reports a leap second as nanosecond >= 1e9; Go has no leap seconds.
    let nanos = t.nanosecond() % 1_000_000_000;
    if nanos != 0 {
        let frac = format!("{nanos:09}");
        s.push('.');
        s.push_str(frac.trim_end_matches('0'));
    }
    s.push('Z');
    s
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use chrono::TimeZone;
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn ref_key_formats_like_go_rfc3339_nano() {
        let base = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 1).unwrap();
        let ev = |inv: &str, t: DateTime<Utc>| Event {
            invocation_id: inv.into(),
            timestamp: t,
            ..Default::default()
        };
        assert_eq!(ref_key(&ev("inv1", base)), "inv1@2026-01-01T00:00:01Z");
        let sub = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()
            + chrono::Duration::nanoseconds(2_600_999);
        assert_eq!(ref_key(&ev("inv", sub)), "inv@2026-01-01T00:00:00.0026Z");
    }
}
