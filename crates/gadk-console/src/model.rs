// Copyright 2026 Chris Olson
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

//! The waterfall model: span tree order, bar geometry and log attachment.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Utc};

use crate::traces::{LogEntry, RawSpan};

/// The narrowest bar the waterfall draws, as a percentage of the trace extent.
pub(crate) const MIN_WIDTH_PCT: f64 = 0.5;

/// One waterfall row: a span with its tree depth and bar geometry.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SpanRow {
    pub span: RawSpan,
    pub depth: usize,
    /// 0.0..=100.0
    pub offset_pct: f64,
    /// MIN_WIDTH_PCT..=100.0
    pub width_pct: f64,
    /// max(0, end - start)
    pub duration_ms: f64,
}

/// Microseconds from `from` to `to` (saturating; negative when `to` is earlier).
fn micros_between(from: DateTime<Utc>, to: DateTime<Utc>) -> i64 {
    let d = to.signed_duration_since(from);
    d.num_microseconds()
        .unwrap_or(if d < chrono::TimeDelta::zero() {
            i64::MIN
        } else {
            i64::MAX
        })
}

/// Tree order: roots by start time; children by start time under their parent (depth+1).
/// Roots: parent_id None, or not a span id in the trace, or (cycle guard) unreachable from any
/// root — such spans are appended as extra roots in start order. A span whose end < start has
/// duration 0. Extent = earliest start .. latest max(start,end); offset = (start-min)/extent*100;
/// width = max(MIN_WIDTH_PCT, duration/extent*100), and offset+width is clamped to 100 by
/// reducing offset. Zero extent: offset 0, width MIN_WIDTH_PCT.
pub(crate) fn build_tree(spans: &[RawSpan]) -> Vec<SpanRow> {
    let Some(min) = spans.iter().map(|s| s.start).min() else {
        return Vec::new();
    };
    let max = trace_end(spans).unwrap_or(min);
    let extent_us = micros_between(min, max).max(0) as f64;

    // Indices in (start, input order) order; the stable sort keeps ties deterministic.
    let mut order: Vec<usize> = (0..spans.len()).collect();
    order.sort_by_key(|&i| spans[i].start);

    let mut index_of: HashMap<&str, usize> = HashMap::new();
    for &i in &order {
        index_of.entry(spans[i].span_id.as_str()).or_insert(i);
    }
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); spans.len()];
    let mut roots: Vec<usize> = Vec::new();
    for &i in &order {
        match spans[i].parent_id.as_deref().and_then(|p| index_of.get(p)) {
            Some(&p) if p != i => children[p].push(i),
            Some(_) => {} // a span that is its own parent: picked up as an extra root below
            None => roots.push(i),
        }
    }

    let mut visited = vec![false; spans.len()];
    let mut out: Vec<(usize, usize)> = Vec::with_capacity(spans.len());
    let mut walk = |root: usize, visited: &mut Vec<bool>| {
        // Explicit stack: deep chains must not overflow the call stack.
        let mut stack = vec![(root, 0usize)];
        while let Some((i, depth)) = stack.pop() {
            if std::mem::replace(&mut visited[i], true) {
                continue;
            }
            out.push((i, depth));
            for &c in children[i].iter().rev() {
                stack.push((c, depth + 1));
            }
        }
    };
    for &r in &roots {
        walk(r, &mut visited);
    }
    // Cycle guard: whatever no root reaches becomes an extra root, in start order.
    for &i in &order {
        if !visited[i] {
            walk(i, &mut visited);
        }
    }

    out.into_iter()
        .map(|(i, depth)| {
            let s = &spans[i];
            let duration_us = micros_between(s.start, s.end).max(0) as f64;
            let (offset, width) = if extent_us > 0.0 {
                let width = (duration_us / extent_us * 100.0).clamp(MIN_WIDTH_PCT, 100.0);
                let offset = micros_between(min, s.start) as f64 / extent_us * 100.0;
                (offset.min(100.0 - width).max(0.0), width)
            } else {
                (0.0, MIN_WIDTH_PCT)
            };
            SpanRow {
                span: s.clone(),
                depth,
                offset_pct: offset,
                width_pct: width,
                duration_ms: duration_us / 1000.0,
            }
        })
        .collect()
}

/// A trace's log entries grouped by the span they belong to.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct AttachedLogs {
    /// span_id → entries in input order
    pub by_span: BTreeMap<String, Vec<LogEntry>>,
    pub unattached: Vec<LogEntry>,
}

/// Entries whose span_id equals a span's id go under it; others (None or unknown id) are unattached.
pub(crate) fn attach_logs(spans: &[RawSpan], entries: Vec<LogEntry>) -> AttachedLogs {
    let known: std::collections::HashSet<&str> = spans.iter().map(|s| s.span_id.as_str()).collect();
    let mut out = AttachedLogs::default();
    for entry in entries {
        match entry.span_id.as_deref() {
            Some(id) if known.contains(id) => {
                out.by_span.entry(id.to_string()).or_default().push(entry);
            }
            _ => out.unattached.push(entry),
        }
    }
    out
}

/// ("greet", 3) when span.name starts with "render_prompt" and labels prompt.name and
/// prompt.version (a u32) are present.
pub(crate) fn prompt_link(span: &RawSpan) -> Option<(String, u32)> {
    if !span.name.starts_with("render_prompt") {
        return None;
    }
    let name = span.labels.get("prompt.name")?;
    let version = span.labels.get("prompt.version")?.parse::<u32>().ok()?;
    Some((name.clone(), version))
}

/// Latest max(start, end) over the spans; None when empty.
pub(crate) fn trace_end(spans: &[RawSpan]) -> Option<DateTime<Utc>> {
    spans.iter().map(|s| s.start.max(s.end)).max()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traces::span_id_hex;
    use chrono::{DateTime, Utc};
    use std::collections::BTreeMap;

    fn at(ms: i64) -> DateTime<Utc> {
        DateTime::from_timestamp_millis(1_700_000_000_000 + ms).unwrap()
    }

    fn span(id: &str, parent: Option<&str>, start_ms: i64, end_ms: i64) -> RawSpan {
        RawSpan {
            span_id: id.to_string(),
            parent_id: parent.map(str::to_string),
            name: format!("span-{id}"),
            start: at(start_ms),
            end: at(end_ms),
            labels: BTreeMap::new(),
        }
    }

    fn named(name: &str, labels: &[(&str, &str)]) -> RawSpan {
        let mut s = span("a", None, 0, 10);
        s.name = name.to_string();
        s.labels = labels
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        s
    }

    fn log(span_id: Option<&str>, ms: i64, text: &str) -> LogEntry {
        LogEntry {
            timestamp: at(ms),
            span_id: span_id.map(str::to_string),
            severity: None,
            payload: serde_json::json!({ "message": text }),
        }
    }

    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    fn ids(rows: &[SpanRow]) -> Vec<&str> {
        rows.iter().map(|r| r.span.span_id.as_str()).collect()
    }

    #[test]
    fn empty_input_gives_no_rows() {
        assert!(build_tree(&[]).is_empty());
    }

    #[test]
    fn three_span_trace_has_exact_geometry() {
        // root 0..1000, a 100..400 under root, b 400..1000 under root; extent 1000 ms.
        let spans = [
            span("c", Some("r"), 400, 1000),
            span("r", None, 0, 1000),
            span("a", Some("r"), 100, 400),
        ];
        let rows = build_tree(&spans);
        assert_eq!(ids(&rows), ["r", "a", "c"]);
        let depths: Vec<usize> = rows.iter().map(|r| r.depth).collect();
        assert_eq!(depths, [0, 1, 1]);
        let expect = [
            (0.0, 100.0, 1000.0),
            (10.0, 30.0, 300.0),
            (40.0, 60.0, 600.0),
        ];
        for (row, (offset, width, dur)) in rows.iter().zip(expect) {
            assert!(near(row.offset_pct, offset), "offset {row:?}");
            assert!(near(row.width_pct, width), "width {row:?}");
            assert!(near(row.duration_ms, dur), "duration {row:?}");
        }
    }

    #[test]
    fn children_nest_depth_first_in_start_order() {
        let spans = [
            span("r", None, 0, 100),
            span("b", Some("r"), 50, 60),
            span("a", Some("r"), 10, 40),
            span("a1", Some("a"), 20, 30),
        ];
        let rows = build_tree(&spans);
        assert_eq!(ids(&rows), ["r", "a", "a1", "b"]);
        let depths: Vec<usize> = rows.iter().map(|r| r.depth).collect();
        assert_eq!(depths, [0, 1, 2, 1]);
    }

    #[test]
    fn orphan_parent_becomes_a_root() {
        let spans = [span("r", None, 0, 100), span("o", Some("missing"), 50, 80)];
        let rows = build_tree(&spans);
        assert_eq!(ids(&rows), ["r", "o"]);
        assert_eq!(rows[1].depth, 0);
    }

    #[test]
    fn several_roots_are_ordered_by_start() {
        let spans = [
            span("late", None, 300, 400),
            span("early", None, 0, 100),
            span("mid", None, 100, 200),
        ];
        let rows = build_tree(&spans);
        assert_eq!(ids(&rows), ["early", "mid", "late"]);
        assert!(rows.iter().all(|r| r.depth == 0));
    }

    #[test]
    fn parent_cycle_terminates_and_keeps_every_span() {
        let spans = [
            span("a", Some("b"), 0, 100),
            span("b", Some("a"), 10, 90),
            span("r", None, 5, 50),
        ];
        let rows = build_tree(&spans);
        assert_eq!(rows.len(), 3);
        // The real root comes first; the cycle is appended starting from its earliest span.
        assert_eq!(ids(&rows), ["r", "a", "b"]);
        assert_eq!(rows[1].depth, 0);
    }

    #[test]
    fn self_parent_is_an_extra_root() {
        let spans = [span("r", None, 0, 100), span("s", Some("s"), 10, 20)];
        let rows = build_tree(&spans);
        assert_eq!(ids(&rows), ["r", "s"]);
        assert_eq!(rows[1].depth, 0);
    }

    #[test]
    fn negative_duration_is_zero_and_gets_the_minimum_width() {
        let spans = [span("r", None, 0, 1000), span("n", Some("r"), 500, 200)];
        let rows = build_tree(&spans);
        assert_eq!(rows[1].duration_ms, 0.0);
        assert!(near(rows[1].width_pct, MIN_WIDTH_PCT));
        assert!(near(rows[1].offset_pct, 50.0));
    }

    #[test]
    fn zero_extent_gives_offset_zero_and_minimum_width() {
        let spans = [span("a", None, 100, 100), span("b", None, 100, 100)];
        let rows = build_tree(&spans);
        assert_eq!(rows.len(), 2);
        for row in &rows {
            assert_eq!(row.offset_pct, 0.0);
            assert!(near(row.width_pct, MIN_WIDTH_PCT));
            assert_eq!(row.duration_ms, 0.0);
        }
    }

    #[test]
    fn tiny_span_is_clamped_to_the_minimum_width() {
        // 1 ms of 10,000 ms is 0.01%, below the 0.5% floor.
        let spans = [span("r", None, 0, 10_000), span("t", Some("r"), 1000, 1001)];
        let rows = build_tree(&spans);
        assert!(near(rows[1].width_pct, MIN_WIDTH_PCT));
        assert!(near(rows[1].offset_pct, 10.0));
        assert!(near(rows[1].duration_ms, 1.0));
    }

    #[test]
    fn offset_plus_width_never_exceeds_100() {
        // A tiny span at the very end would overflow; its offset is pulled back.
        let spans = [span("r", None, 0, 1000), span("e", Some("r"), 1000, 1000)];
        let rows = build_tree(&spans);
        assert!(near(rows[1].width_pct, MIN_WIDTH_PCT));
        assert!(near(rows[1].offset_pct, 100.0 - MIN_WIDTH_PCT));
        for row in &rows {
            assert!(row.offset_pct >= 0.0);
            assert!(row.width_pct >= MIN_WIDTH_PCT && row.width_pct <= 100.0);
            assert!(row.offset_pct + row.width_pct <= 100.0 + 1e-9, "{row:?}");
        }
    }

    #[test]
    fn extent_uses_the_latest_end_even_for_a_child_outliving_its_parent() {
        let spans = [span("r", None, 0, 100), span("c", Some("r"), 50, 400)];
        let rows = build_tree(&spans);
        assert!(near(rows[0].width_pct, 25.0));
        assert!(near(rows[1].offset_pct, 12.5));
        assert!(near(rows[1].width_pct, 87.5));
    }

    #[test]
    fn logs_join_by_hex_of_the_decimal_span_id() {
        let hex = span_id_hex("12345").unwrap();
        assert_eq!(hex, "0000000000003039");
        let spans = [
            span(&hex, None, 0, 100),
            span("00000000000000ff", None, 0, 10),
        ];
        let entries = vec![
            log(Some("0000000000003039"), 1, "mine-1"),
            log(Some("ffffffffffffffff"), 2, "unknown"),
            log(None, 3, "no-span"),
            log(Some("0000000000003039"), 4, "mine-2"),
        ];
        let joined = attach_logs(&spans, entries);
        let mine = &joined.by_span["0000000000003039"];
        let texts: Vec<&str> = mine
            .iter()
            .map(|e| e.payload["message"].as_str().unwrap())
            .collect();
        assert_eq!(texts, ["mine-1", "mine-2"]);
        assert!(!joined.by_span.contains_key("00000000000000ff"));
        let lost: Vec<&str> = joined
            .unattached
            .iter()
            .map(|e| e.payload["message"].as_str().unwrap())
            .collect();
        assert_eq!(lost, ["unknown", "no-span"]);
    }

    #[test]
    fn attaching_nothing_gives_the_default() {
        assert_eq!(
            attach_logs(&[span("a", None, 0, 1)], vec![]),
            AttachedLogs::default()
        );
    }

    #[test]
    fn prompt_link_reads_name_and_version() {
        let s = named(
            "render_prompt greet",
            &[("prompt.name", "greet"), ("prompt.version", "3")],
        );
        assert_eq!(prompt_link(&s), Some(("greet".to_string(), 3)));
        let bare = named(
            "render_prompt",
            &[("prompt.name", "greet"), ("prompt.version", "3")],
        );
        assert_eq!(prompt_link(&bare), Some(("greet".to_string(), 3)));
    }

    #[test]
    fn prompt_link_rejects_a_bad_version() {
        for bad in ["x", "-1", "", "3.5", "4294967296"] {
            let s = named(
                "render_prompt greet",
                &[("prompt.name", "greet"), ("prompt.version", bad)],
            );
            assert_eq!(prompt_link(&s), None, "version {bad:?}");
        }
    }

    #[test]
    fn prompt_link_rejects_a_wrong_name_prefix() {
        let s = named(
            "invoke render_prompt",
            &[("prompt.name", "greet"), ("prompt.version", "3")],
        );
        assert_eq!(prompt_link(&s), None);
    }

    #[test]
    fn prompt_link_needs_both_labels() {
        let no_version = named("render_prompt greet", &[("prompt.name", "greet")]);
        assert_eq!(prompt_link(&no_version), None);
        let no_name = named("render_prompt greet", &[("prompt.version", "3")]);
        assert_eq!(prompt_link(&no_name), None);
    }

    #[test]
    fn trace_end_is_the_latest_start_or_end() {
        assert_eq!(trace_end(&[]), None);
        let spans = [
            span("a", None, 0, 100),
            span("b", None, 500, 200), // end before start: its start counts
            span("c", None, 50, 300),
        ];
        assert_eq!(trace_end(&spans), Some(at(500)));
    }
}
