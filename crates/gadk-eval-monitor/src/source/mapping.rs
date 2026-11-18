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

//! Agent-analytics rows to [`ProductionRun`]s.
//!
//! The agent-analytics plugin logs a summary of each event's content, not the content itself:
//! a user message is `{"text_summary": "<pieces joined by ' | '>"}` and an agent response is
//! `{"response": "<pieces joined by ' | '>"}`, where a text part renders as `text: '<text>'`
//! and other parts as `call: <name>`, `resp: <name>`, `Executable code (…): …`,
//! `Code execution result (…): …` or `other`. [`build_run`] rebuilds the turns from those
//! summaries:
//!
//! - each `USER_MESSAGE_RECEIVED` starts a turn (`user` is its `text_summary`), except one that
//!   holds only `Function response: <name>` pieces (a resumed invocation), which continues the
//!   current turn;
//! - a turn's `reply` is the text pieces of its `AGENT_RESPONSE` rows, in order, joined with
//!   `"\n"`; the response is split on `" | "` only where a piece starts (a known prefix, or the
//!   piece `other` on its own), so a text that itself contains `" | text: '"`, `" | call: "`,
//!   `" | resp: "`, `" | Executable code ("` or `" | Code execution result ("` is split there (a
//!   known limitation);
//! - each `TOOL_STARTING` adds a tool call to the current turn;
//! - the first `AGENT_ERROR` / `INVOCATION_ERROR` traceback is the run error.
//!
//! The run is marked incomplete, with a note per problem, when a mapped row has NULL or
//! unexpected content or `is_truncated` set, when a content part was offloaded (not `INLINE`, or
//! with a `uri` / `object_ref`), when there is no user message, when a turn has no response and
//! the run has no error, or when rows come before the first user message (they are ignored).

use chrono::{DateTime, SecondsFormat, Utc};
use gadk_eval::{RunTranscript, ToolCall, TurnRecord};
use serde_json::{Map, Value};

use crate::source::ProductionRun;

/// The event types [`build_run`] reads; rows of other types are ignored.
pub const MAPPED_EVENT_TYPES: [&str; 5] = [
    "USER_MESSAGE_RECEIVED",
    "AGENT_RESPONSE",
    "TOOL_STARTING",
    "AGENT_ERROR",
    "INVOCATION_ERROR",
];

/// The prefixes a piece of an `AGENT_RESPONSE` summary starts with (besides the piece `other`).
const PIECE_PREFIXES: [&str; 5] = [
    "text: '",
    "call: ",
    "resp: ",
    "Executable code (",
    "Code execution result (",
];

/// One logged event of an invocation, as read from the agent-analytics table.
#[derive(Debug, Clone, PartialEq)]
pub struct EventRow {
    /// The `event_type` column.
    pub event_type: String,
    /// The `timestamp` column.
    pub timestamp: DateTime<Utc>,
    /// The `content` column; `None` when NULL.
    pub content: Option<Value>,
    /// The `content_parts` column, one JSON object per part.
    pub content_parts: Vec<Value>,
    /// The `attributes` column; `None` when NULL.
    pub attributes: Option<Value>,
    /// The `is_truncated` column (NULL reads as `false`).
    pub is_truncated: bool,
    /// The `trace_id` column; `None` when NULL or empty.
    pub trace_id: Option<String>,
}

/// The invocation the rows belong to.
#[derive(Debug, Clone, PartialEq)]
pub struct RunHeader {
    /// The invocation id.
    pub invocation_id: String,
    /// The session id.
    pub session_id: String,
    /// The root agent's name.
    pub agent: String,
    /// When the invocation started.
    pub started_at: DateTime<Utc>,
}

/// A turn being rebuilt.
struct Turn {
    user: String,
    texts: Vec<String>,
    calls: Vec<ToolCall>,
    responded: bool,
}

/// Rebuilds the invocation `header` from its `rows`, which must be in log order (by timestamp).
pub fn build_run(header: RunHeader, rows: &[EventRow]) -> ProductionRun {
    let mut turns: Vec<Turn> = Vec::new();
    let mut error: Option<String> = None;
    let mut notes: Vec<String> = Vec::new();
    let mut saw_user = false;
    for row in rows
        .iter()
        .filter(|r| MAPPED_EVENT_TYPES.contains(&r.event_type.as_str()))
    {
        let mut note = |reason: &str| {
            notes.push(format!(
                "{} at {}: {reason}",
                row.event_type,
                row.timestamp.to_rfc3339_opts(SecondsFormat::Micros, true)
            ));
        };
        if row.is_truncated {
            note("content truncated (is_truncated)");
        }
        if let Some(reason) = offloaded(&row.content_parts) {
            note(&reason);
        }
        let Some(content) = row.content.as_ref().filter(|c| !c.is_null()) else {
            note("content is NULL");
            continue;
        };
        if row.event_type == "USER_MESSAGE_RECEIVED" {
            let Some(text) = content.get("text_summary").and_then(Value::as_str) else {
                note("unexpected content (no `text_summary` string)");
                continue;
            };
            if is_function_response_only(text) && !turns.is_empty() {
                continue;
            }
            if is_function_response_only(text) {
                note("function response before the first user message (ignored)");
                continue;
            }
            saw_user = true;
            turns.push(Turn {
                user: text.to_owned(),
                texts: Vec::new(),
                calls: Vec::new(),
                responded: false,
            });
            continue;
        }
        let Some(turn) = turns.last_mut() else {
            note("before the first USER_MESSAGE_RECEIVED (ignored)");
            continue;
        };
        match row.event_type.as_str() {
            "AGENT_RESPONSE" => {
                match content
                    .get("response")
                    .and_then(Value::as_str)
                    .and_then(response_texts)
                {
                    Some(texts) => {
                        turn.texts.extend(texts);
                        turn.responded = true;
                    }
                    None => note("unexpected content (not a response summary)"),
                }
            }
            "TOOL_STARTING" => {
                let name = content.get("tool").and_then(Value::as_str);
                let args = match content.get("args") {
                    None | Some(Value::Null) => Some(Map::new()),
                    Some(Value::Object(a)) => Some(a.clone()),
                    Some(_) => None,
                };
                match (name, args) {
                    (Some(name), Some(args)) => turn.calls.push(ToolCall {
                        name: name.to_owned(),
                        args,
                    }),
                    _ => note("unexpected content (no `tool` string or `args` mapping)"),
                }
            }
            _ => match content.get("error_traceback").and_then(Value::as_str) {
                Some(e) => {
                    error.get_or_insert_with(|| e.to_owned());
                }
                None => note("unexpected content (no `error_traceback` string)"),
            },
        }
    }
    if !saw_user {
        notes.push("no USER_MESSAGE_RECEIVED row".to_owned());
    }
    if error.is_none() {
        for (i, t) in turns.iter().enumerate() {
            if !t.responded {
                notes.push(format!("turn {} has no AGENT_RESPONSE", i + 1));
            }
        }
    }
    let trace_id = rows.iter().find_map(|r| r.trace_id.clone());
    ProductionRun {
        invocation_id: header.invocation_id,
        session_id: header.session_id,
        trace_id,
        agent: header.agent,
        started_at: header.started_at,
        transcript: RunTranscript {
            turns: turns
                .into_iter()
                .map(|t| TurnRecord {
                    user: t.user,
                    reply: t.texts.join("\n"),
                    calls: t.calls,
                })
                .collect(),
            error,
        },
        content_complete: notes.is_empty(),
        notes,
    }
}

/// Why `parts` hold offloaded content (a part not stored `INLINE`, or with a `uri` or an
/// `object_ref`), or `None`.
fn offloaded(parts: &[Value]) -> Option<String> {
    let non_empty = |v: Option<&Value>| match v {
        None | Some(Value::Null) => false,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Object(o)) => o.values().any(|v| !v.is_null()),
        Some(_) => true,
    };
    parts.iter().enumerate().find_map(|(i, p)| {
        let mode = p.get("storage_mode").and_then(Value::as_str).unwrap_or("");
        if mode != "INLINE" {
            Some(format!(
                "content part {i} is offloaded (storage_mode {mode:?})"
            ))
        } else if non_empty(p.get("uri")) || non_empty(p.get("object_ref")) {
            Some(format!(
                "content part {i} is offloaded (has a uri or object_ref)"
            ))
        } else {
            None
        }
    })
}

/// Whether a user message summary holds only function responses (a resumed invocation).
fn is_function_response_only(text: &str) -> bool {
    !text.is_empty()
        && text
            .split(" | ")
            .all(|p| p.starts_with("Function response: "))
}

/// Whether `s` starts with a response piece: a known prefix, or exactly `other` (alone or before
/// the next `" | "`).
fn starts_piece(s: &str) -> bool {
    PIECE_PREFIXES.iter().any(|p| s.starts_with(p)) || s == "other" || s.starts_with("other | ")
}

/// The text pieces of an `AGENT_RESPONSE` summary (`"None"` has none), or `None` when the
/// summary does not start with a known piece.
fn response_texts(response: &str) -> Option<Vec<String>> {
    if response == "None" {
        return Some(Vec::new());
    }
    let mut pieces = Vec::new();
    let mut start = 0;
    for (i, sep) in response.match_indices(" | ") {
        let next = &response[i + sep.len()..];
        if i >= start && starts_piece(next) {
            pieces.push(&response[start..i]);
            start = i + sep.len();
        }
    }
    pieces.push(&response[start..]);
    let mut texts = Vec::new();
    for piece in pieces {
        if !starts_piece(piece) {
            return None;
        }
        if let Some(text) = piece.strip_prefix("text: '") {
            texts.push(text.strip_suffix('\'').unwrap_or(text).to_owned());
        }
    }
    Some(texts)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).unwrap()
    }

    fn header() -> RunHeader {
        RunHeader {
            invocation_id: "invocation-1".into(),
            session_id: "session-1".into(),
            agent: "root".into(),
            started_at: at(0),
        }
    }

    /// The rows of a gadk-agentanalytics plugin fixture, timestamped 1 s apart.
    fn fixture(name: &str) -> Vec<EventRow> {
        let path = format!(
            "{}/../gadk-agentanalytics/tests/fixtures/plugin/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        doc["rows"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .map(|(i, r)| EventRow {
                event_type: r["event_type"].as_str().unwrap().into(),
                timestamp: at(i64::try_from(i).unwrap()),
                content: r.get("content").cloned().filter(|c| !c.is_null()),
                content_parts: r["content_parts"].as_array().cloned().unwrap_or_default(),
                attributes: r.get("attributes").cloned(),
                is_truncated: r["is_truncated"].as_bool().unwrap_or(false),
                trace_id: r["trace_id"].as_str().map(str::to_owned),
            })
            .collect()
    }

    /// A synthetic row with inline content parts.
    fn row(secs: i64, event_type: &str, content: Value) -> EventRow {
        EventRow {
            event_type: event_type.into(),
            timestamp: at(secs),
            content: Some(content).filter(|c| !c.is_null()),
            content_parts: vec![json!({"storage_mode": "INLINE", "part_index": 0})],
            attributes: None,
            is_truncated: false,
            trace_id: None,
        }
    }

    fn user(secs: i64, text: &str) -> EventRow {
        row(secs, "USER_MESSAGE_RECEIVED", json!({"text_summary": text}))
    }

    fn response(secs: i64, summary: &str) -> EventRow {
        row(secs, "AGENT_RESPONSE", json!({"response": summary}))
    }

    fn tool(secs: i64, name: &str, args: Value) -> EventRow {
        row(
            secs,
            "TOOL_STARTING",
            json!({"tool": name, "args": args, "tool_origin": "LOCAL"}),
        )
    }

    fn call(name: &str, args: Value) -> ToolCall {
        ToolCall {
            name: name.into(),
            args: args.as_object().unwrap().clone(),
        }
    }

    #[test]
    fn tool_call_fixture() {
        let run = build_run(header(), &fixture("tool_call"));
        assert_eq!(run.notes, Vec::<String>::new());
        assert!(run.content_complete);
        assert_eq!(run.trace_id.as_deref(), Some("trace-1"));
        assert_eq!(run.invocation_id, "invocation-1");
        assert_eq!(run.agent, "root");
        assert_eq!(
            run.transcript.turns,
            [TurnRecord {
                user: "What is the weather in Paris?".into(),
                reply: "It is sunny in Paris.".into(),
                calls: vec![call("get_weather", json!({"city": "Paris"}))],
            }]
        );
        assert_eq!(run.transcript.error, None);
    }

    #[test]
    fn transfer_fixture() {
        let run = build_run(header(), &fixture("transfer"));
        assert!(run.content_complete, "{:?}", run.notes);
        let t = &run.transcript.turns[0];
        assert_eq!(t.user, "I need help.");
        assert_eq!(t.reply, "Helper here, how can I help?");
        assert_eq!(
            t.calls,
            [call("transfer_to_agent", json!({"agent_name": "helper"}))]
        );
    }

    #[test]
    fn model_error_fixture() {
        let run = build_run(header(), &fixture("model_error"));
        assert_eq!(run.transcript.error.as_deref(), Some("model failure 404"));
        assert_eq!(run.transcript.turns[0].user, "Hello?");
        assert_eq!(run.transcript.turns[0].reply, "");
        // A failed run with no response is complete: the error explains the missing reply.
        assert!(run.content_complete, "{:?}", run.notes);
    }

    #[test]
    fn confirmation_fixture_has_a_turn_without_a_response() {
        let run = build_run(header(), &fixture("confirmation"));
        assert_eq!(
            run.transcript.turns[0].calls,
            [call("transfer_funds", json!({"amount": 100}))]
        );
        assert!(!run.content_complete);
        assert_eq!(run.notes, ["turn 1 has no AGENT_RESPONSE"]);
    }

    #[test]
    fn session_signals_fixture() {
        let run = build_run(header(), &fixture("session_signals"));
        assert!(run.content_complete, "{:?}", run.notes);
        assert_eq!(run.transcript.turns[0].reply, "All signals sent.");
    }

    #[test]
    fn multiple_responses_and_pieces_join_with_newlines() {
        let run = build_run(
            header(),
            &[
                user(1, "hi"),
                response(2, "text: 'one' | call: lookup | text: 'two'"),
                response(3, "text: 'three'"),
            ],
        );
        assert_eq!(run.transcript.turns[0].reply, "one\ntwo\nthree");
        assert!(run.content_complete);
    }

    #[test]
    fn non_text_pieces_are_dropped() {
        let run = build_run(
            header(),
            &[
                user(1, "hi"),
                response(
                    2,
                    "call: f | resp: f | Executable code (PYTHON): print(1) | \
                     Code execution result (OUTCOME_OK): 1 | other",
                ),
                response(3, "None"),
            ],
        );
        assert_eq!(run.transcript.turns[0].reply, "");
        assert!(run.content_complete, "{:?}", run.notes);
    }

    #[test]
    fn separator_inside_text_is_kept_unless_a_piece_prefix_follows() {
        let run = build_run(
            header(),
            &[user(1, "hi"), response(2, "text: 'a | b and c | d'")],
        );
        assert_eq!(run.transcript.turns[0].reply, "a | b and c | d");
        // The documented limitation: a text containing " | text: '" is split there.
        let run = build_run(
            header(),
            &[user(1, "hi"), response(2, "text: 'x | text: 'y'")],
        );
        assert_eq!(run.transcript.turns[0].reply, "x\ny");
    }

    #[test]
    fn other_is_a_piece_only_on_its_own() {
        let run = build_run(
            header(),
            &[
                user(1, "hi"),
                response(2, "text: 'A | otherwise' | other | text: 'B'"),
            ],
        );
        assert_eq!(run.transcript.turns[0].reply, "A | otherwise\nB");
        assert!(run.content_complete, "{:?}", run.notes);
        let run = build_run(header(), &[user(1, "hi"), response(2, "otherwise")]);
        assert!(!run.content_complete);
    }

    #[test]
    fn null_content_is_incomplete() {
        let run = build_run(
            header(),
            &[user(1, "hi"), row(2, "AGENT_RESPONSE", Value::Null)],
        );
        assert!(!run.content_complete);
        assert_eq!(
            run.notes,
            [
                "AGENT_RESPONSE at 1970-01-01T00:00:02.000000Z: content is NULL",
                "turn 1 has no AGENT_RESPONSE"
            ]
        );
    }

    #[test]
    fn truncated_rows_are_incomplete_but_mapped() {
        let mut r = response(2, "text: 'It is sun...[TRUNCATED]'");
        r.is_truncated = true;
        let run = build_run(header(), &[user(1, "hi"), r]);
        assert!(!run.content_complete);
        assert_eq!(
            run.notes,
            ["AGENT_RESPONSE at 1970-01-01T00:00:02.000000Z: content truncated (is_truncated)"]
        );
        assert_eq!(run.transcript.turns[0].reply, "It is sun...[TRUNCATED]");
    }

    #[test]
    fn formatter_shaped_content_is_incomplete() {
        let run = build_run(
            header(),
            &[
                row(1, "USER_MESSAGE_RECEIVED", json!({"text": "hi"})),
                user(2, "hi"),
                row(3, "AGENT_RESPONSE", json!({"custom": true})),
                response(4, "plain words"),
                row(5, "TOOL_STARTING", json!({"tool": "f", "args": [1]})),
                row(6, "AGENT_ERROR", json!("boom")),
            ],
        );
        assert!(!run.content_complete);
        let reasons: Vec<&str> = run
            .notes
            .iter()
            .map(|n| n.split_once(": ").map_or(n.as_str(), |(_, reason)| reason))
            .collect();
        assert_eq!(
            reasons,
            [
                "unexpected content (no `text_summary` string)",
                "unexpected content (not a response summary)",
                "unexpected content (not a response summary)",
                "unexpected content (no `tool` string or `args` mapping)",
                "unexpected content (no `error_traceback` string)",
                "turn 1 has no AGENT_RESPONSE",
            ]
        );
    }

    #[test]
    fn rows_before_the_first_user_message_are_ignored() {
        let run = build_run(
            header(),
            &[
                tool(1, "early", json!({})),
                user(2, "hi"),
                response(3, "text: 'hello'"),
            ],
        );
        assert!(run.transcript.turns[0].calls.is_empty());
        assert_eq!(run.transcript.turns[0].reply, "hello");
        assert_eq!(
            run.notes,
            [
                "TOOL_STARTING at 1970-01-01T00:00:01.000000Z: before the first \
                 USER_MESSAGE_RECEIVED (ignored)"
            ]
        );
    }

    #[test]
    fn offloaded_content_parts_are_incomplete() {
        let mut gcs = response(2, "text: 'hello'");
        gcs.content_parts = vec![json!({"storage_mode": "GCS_REFERENCE", "part_index": 0})];
        let mut uri = response(3, "text: 'again'");
        uri.content_parts = vec![
            json!({"storage_mode": "INLINE"}),
            json!({"storage_mode": "INLINE", "uri": "gs://b/o"}),
        ];
        let mut object_ref = response(4, "text: 'more'");
        object_ref.content_parts = vec![
            json!({"storage_mode": "INLINE", "uri": "", "object_ref": {"uri": "gs://b/o", "version": null}}),
        ];
        let mut empty_ref = response(5, "text: 'fine'");
        empty_ref.content_parts =
            vec![json!({"storage_mode": "INLINE", "uri": null, "object_ref": {"uri": null}})];
        let run = build_run(header(), &[user(1, "hi"), gcs, uri, object_ref, empty_ref]);
        assert_eq!(
            run.notes,
            [
                "AGENT_RESPONSE at 1970-01-01T00:00:02.000000Z: content part 0 is offloaded \
                 (storage_mode \"GCS_REFERENCE\")",
                "AGENT_RESPONSE at 1970-01-01T00:00:03.000000Z: content part 1 is offloaded \
                 (has a uri or object_ref)",
                "AGENT_RESPONSE at 1970-01-01T00:00:04.000000Z: content part 0 is offloaded \
                 (has a uri or object_ref)",
            ]
        );
        assert_eq!(run.transcript.turns[0].reply, "hello\nagain\nmore\nfine");
    }

    #[test]
    fn a_run_without_a_user_message_is_incomplete() {
        let run = build_run(header(), &[]);
        assert!(!run.content_complete);
        assert_eq!(run.notes, ["no USER_MESSAGE_RECEIVED row"]);
        assert!(run.transcript.turns.is_empty());
    }

    #[test]
    fn a_turn_without_a_response_is_incomplete() {
        let run = build_run(
            header(),
            &[
                user(1, "first"),
                response(2, "text: 'one'"),
                user(3, "second"),
            ],
        );
        assert_eq!(run.transcript.turns.len(), 2);
        assert_eq!(run.notes, ["turn 2 has no AGENT_RESPONSE"]);
    }

    #[test]
    fn a_function_response_resume_continues_the_turn() {
        let run = build_run(
            header(),
            &[
                user(1, "Send 100 to Bob."),
                tool(2, "transfer_funds", json!({"amount": 100})),
                user(3, "Function response: transfer_funds"),
                response(4, "text: 'Done.'"),
            ],
        );
        assert!(run.content_complete, "{:?}", run.notes);
        assert_eq!(
            run.transcript.turns,
            [TurnRecord {
                user: "Send 100 to Bob.".into(),
                reply: "Done.".into(),
                calls: vec![call("transfer_funds", json!({"amount": 100}))],
            }]
        );
    }

    #[test]
    fn the_first_error_wins() {
        let run = build_run(
            header(),
            &[
                user(1, "hi"),
                row(2, "AGENT_ERROR", json!({"error_traceback": "first"})),
                row(3, "INVOCATION_ERROR", json!({"error_traceback": "second"})),
            ],
        );
        assert_eq!(run.transcript.error.as_deref(), Some("first"));
        assert!(run.content_complete);
    }
}
