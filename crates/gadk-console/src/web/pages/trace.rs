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

//! The trace detail page: waterfall, span details and log payloads.

use chrono::{DateTime, SecondsFormat, Utc};
use maud::{Markup, PreEscaped, html};
use serde_json::{Map, Value, json};

use super::error::banner;
use super::layout::{Nav, page};
use crate::model::{AttachedLogs, SpanRow, prompt_link};
use crate::traces::{LogEntry, SourceError, TraceId};
use crate::url::percent_encode;

/// Payload texts longer than this many chars render collapsed.
pub(crate) const PAYLOAD_LIMIT: usize = 4096;

/// A trace's log entries, joined to its spans.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct LogView {
    pub attached: AttachedLogs,
    /// The source stopped at 1,000 entries with more available.
    pub truncated: bool,
}

/// The trace detail page: notes and banners, the waterfall, the span detail pane (filled by
/// `waterfall.js`), the log entries per span, and the JSON data block.
pub(crate) fn render(
    id: &TraceId,
    rows: &[SpanRow],
    logs: &Result<LogView, SourceError>,
    still_arriving: bool,
) -> Markup {
    let body = html! {
        h1 { "Trace " span class="mono" { (id.as_str()) } }
        @if still_arriving {
            div class="banner info" {
                p { "Log entries may still be arriving: the trace ended less than 2 minutes ago." }
            }
        }
        @match logs {
            Err(e) => {
                (banner(&format!("Log entries could not be loaded: {e}"), e.hint()))
            }
            Ok(view) => {
                @if view.truncated {
                    div class="banner warn" {
                        p { "Showing the first 1,000 log entries; the trace has more." }
                    }
                }
            }
        }
        div class="trace-layout" {
            div class="waterfall" {
                @for (i, row) in rows.iter().enumerate() {
                    (span_row(row, rows.get(i + 1).is_some_and(|next| next.depth > row.depth)))
                }
            }
            aside id="span-detail" {
                p class="empty" { "Select a span to see its labels and log entries." }
            }
        }
        @if let Ok(view) = logs {
            (log_sections(rows, view))
        }
        script type="application/json" id="trace-data" {
            (PreEscaped(script_json(&data(id, rows, logs))))
        }
    };
    page(&format!("Trace {}", id.as_str()), Nav::Traces, body)
}

/// One waterfall row; `waterfall.js` sets the bar's left/width from the data attributes.
fn span_row(row: &SpanRow, has_children: bool) -> Markup {
    let s = &row.span;
    html! {
        div class="span-row" data-span=(s.span_id) data-depth=(row.depth)
            data-offset=(format!("{:.2}", row.offset_pct)) data-width=(format!("{:.2}", row.width_pct)) {
            div class="name" {
                span class="toggle" { @if has_children { "▾" } }
                (s.name)
                @if let Some((name, version)) = prompt_link(s) {
                    " "
                    a class="prompt-link" href={ "/prompts/" (percent_encode(&name)) "?v=" (version) } {
                        "prompt " (name) " v" (version)
                    }
                }
            }
            div class="dur" { (format!("{:.1} ms", row.duration_ms)) }
            div class="track" { div class="bar" {} }
        }
    }
}

/// Log entries grouped under their span (tree order), then the unattached ones.
fn log_sections(rows: &[SpanRow], view: &LogView) -> Markup {
    let a = &view.attached;
    html! {
        h2 { "Log entries" }
        @if a.by_span.is_empty() && a.unattached.is_empty() {
            p class="empty" { "No log entries for this trace." }
        }
        @for row in rows {
            @if let Some(entries) = a.by_span.get(&row.span.span_id) {
                section class="span-logs" id={ "logs-" (row.span.span_id) } {
                    h3 { (row.span.name) " " span class="mono" { (row.span.span_id) } }
                    @for e in entries { (log_entry(e)) }
                }
            }
        }
        @if !a.unattached.is_empty() {
            section class="span-logs" id="logs-unattached" {
                h3 { "Unattached" }
                @for e in &a.unattached { (log_entry(e)) }
            }
        }
    }
}

fn log_entry(e: &LogEntry) -> Markup {
    html! {
        div class="log-entry" {
            div class="meta" {
                (millis(e.timestamp))
                @if let Some(severity) = &e.severity { " · " (severity) }
                @if let Some(event) = event_name(&e.payload) { " · " (event) }
            }
            @for (label, text) in entry_parts(&e.payload) {
                div class="meta" { (label) }
                (payload(&text))
            }
        }
    }
}

/// A payload text; over `PAYLOAD_LIMIT` chars it shows the first `PAYLOAD_LIMIT` chars and an
/// expand button, with the full text already in the page (hidden by CSS until expanded).
fn payload(text: &str) -> Markup {
    match text.char_indices().nth(PAYLOAD_LIMIT) {
        None => html! { pre class="payload" { (text) } },
        Some((cut, _)) => html! {
            div class="payload collapsed" data-full {
                pre class="preview" { (&text[..cut]) "…" }
                button class="expand" type="button" { "expand (" (text.chars().count()) " chars)" }
                pre class="full" { (text) }
            }
        },
    }
}

fn millis(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// The gadk-telemetry `event.name` of a log entry's jsonPayload.
fn event_name(payload: &Value) -> Option<&str> {
    payload.get("event.name").and_then(Value::as_str)
}

/// A string as is; anything else as pretty JSON.
fn display(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        _ => serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string()),
    }
}

/// The displayed parts of a jsonPayload: its `body` and `attributes` when present, else the
/// whole payload (nothing for a null payload).
fn entry_parts(payload: &Value) -> Vec<(&'static str, String)> {
    let mut parts = Vec::new();
    for key in ["body", "attributes"] {
        if let Some(v) = payload.get(key).filter(|v| !v.is_null()) {
            parts.push((key, display(v)));
        }
    }
    if parts.is_empty() && !payload.is_null() {
        parts.push(("payload", display(payload)));
    }
    parts
}

fn entry_json(e: &LogEntry) -> Value {
    let parts: Vec<Value> = entry_parts(&e.payload)
        .into_iter()
        .map(|(label, text)| json!({ "label": label, "text": text }))
        .collect();
    json!({
        "timestamp": millis(e.timestamp),
        "severity": e.severity,
        "event": event_name(&e.payload),
        "parts": parts,
    })
}

/// `{"trace_id", "spans": [{id, parent, name, depth, start, duration_ms, labels}],
/// "logs": {span_id: [entry]}, "unattached": [entry]}` for `waterfall.js`.
fn data(id: &TraceId, rows: &[SpanRow], logs: &Result<LogView, SourceError>) -> Value {
    let spans: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.span.span_id,
                "parent": r.span.parent_id,
                "name": r.span.name,
                "depth": r.depth,
                "start": millis(r.span.start),
                "duration_ms": r.duration_ms,
                "labels": r.span.labels,
            })
        })
        .collect();
    let (by_span, unattached) = match logs {
        Ok(view) => (
            view.attached
                .by_span
                .iter()
                .map(|(span, entries)| (span.clone(), entries.iter().map(entry_json).collect()))
                .collect::<Map<String, Value>>(),
            view.attached.unattached.iter().map(entry_json).collect(),
        ),
        Err(_) => (Map::new(), Vec::new()),
    };
    json!({
        "trace_id": id.as_str(),
        "spans": spans,
        "logs": by_span,
        "unattached": unattached,
    })
}

/// JSON text safe inside `<script type="application/json">`: `<`, `>` and `&` become
/// `\u003c`, `\u003e` and `\u0026`, so no `</script` can appear in it.
fn script_json(v: &Value) -> String {
    v.to_string()
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
}
