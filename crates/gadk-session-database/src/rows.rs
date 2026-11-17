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

//! Rows as stored (sessions, events, app states and user states) and their mapping to gadk
//! values. JSON columns hold compact JSON with HTML-escaped strings and sorted map keys, as
//! [`gadk_model::to_go_json`] writes it, so rows match those of ADK's database session
//! service.

use chrono::{DateTime, Utc};
use gadk_model::{LlmResponse, to_go_json};
use gadk_session::{Event, EventActions, SessionError};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};

use crate::errors;

pub(crate) type StateMap = Map<String, Value>;

/// A `sessions` row. `state` is the JSON text (`""` when the column is NULL).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SessionRow {
    pub(crate) app_name: String,
    pub(crate) user_id: String,
    pub(crate) id: String,
    pub(crate) state: String,
    pub(crate) create_time: DateTime<Utc>,
    pub(crate) update_time: DateTime<Utc>,
}

/// An `app_states` row.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AppStateRow {
    pub(crate) app_name: String,
    pub(crate) state: String,
    pub(crate) update_time: DateTime<Utc>,
}

/// A `user_states` row.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct UserStateRow {
    pub(crate) app_name: String,
    pub(crate) user_id: String,
    pub(crate) state: String,
    pub(crate) update_time: DateTime<Utc>,
}

/// An `events` row, field order = column order. `None` is SQL NULL.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EventRow {
    pub(crate) id: String,
    pub(crate) app_name: String,
    pub(crate) user_id: String,
    pub(crate) session_id: String,
    pub(crate) invocation_id: String,
    pub(crate) author: String,
    pub(crate) actions: Vec<u8>,
    pub(crate) long_running_tool_ids_json: Option<String>,
    pub(crate) routes_json: Option<String>,
    pub(crate) output_json: Option<String>,
    pub(crate) node_info_json: Option<String>,
    pub(crate) requested_input_json: Option<String>,
    pub(crate) branch: Option<String>,
    pub(crate) isolation_scope: Option<String>,
    pub(crate) timestamp: DateTime<Utc>,
    pub(crate) content: Option<String>,
    pub(crate) grounding_metadata: Option<String>,
    pub(crate) custom_metadata: Option<String>,
    pub(crate) usage_metadata: Option<String>,
    pub(crate) citation_metadata: Option<String>,
    pub(crate) input_transcription: Option<String>,
    pub(crate) output_transcription: Option<String>,
    pub(crate) partial: Option<bool>,
    pub(crate) turn_complete: Option<bool>,
    pub(crate) error_code: Option<String>,
    pub(crate) error_message: Option<String>,
    pub(crate) interrupted: Option<bool>,
}

/// Reads one column of a result row in a dialect's storage form. A NULL `time` is
/// [`gadk_session::zero_time`].
pub(crate) trait ColumnReader {
    /// A non-NULL text column.
    fn text(&self, col: &str) -> sqlx::Result<String>;
    /// A nullable text column (JSON columns read back as text).
    fn opt_text(&self, col: &str) -> sqlx::Result<Option<String>>;
    /// A nullable binary column.
    fn opt_bytes(&self, col: &str) -> sqlx::Result<Option<Vec<u8>>>;
    /// A timestamp column.
    fn time(&self, col: &str) -> sqlx::Result<DateTime<Utc>>;
    /// A nullable boolean column.
    fn flag(&self, col: &str) -> sqlx::Result<Option<bool>>;
}

pub(crate) fn session_row(r: &impl ColumnReader) -> sqlx::Result<SessionRow> {
    Ok(SessionRow {
        app_name: r.text("app_name")?,
        user_id: r.text("user_id")?,
        id: r.text("id")?,
        state: r.opt_text("state")?.unwrap_or_default(),
        create_time: r.time("create_time")?,
        update_time: r.time("update_time")?,
    })
}

pub(crate) fn app_state_row(r: &impl ColumnReader) -> sqlx::Result<AppStateRow> {
    Ok(AppStateRow {
        app_name: r.text("app_name")?,
        state: r.opt_text("state")?.unwrap_or_default(),
        update_time: r.time("update_time")?,
    })
}

pub(crate) fn user_state_row(r: &impl ColumnReader) -> sqlx::Result<UserStateRow> {
    Ok(UserStateRow {
        app_name: r.text("app_name")?,
        user_id: r.text("user_id")?,
        state: r.opt_text("state")?.unwrap_or_default(),
        update_time: r.time("update_time")?,
    })
}

pub(crate) fn event_row(r: &impl ColumnReader) -> sqlx::Result<EventRow> {
    Ok(EventRow {
        id: r.text("id")?,
        app_name: r.text("app_name")?,
        user_id: r.text("user_id")?,
        session_id: r.text("session_id")?,
        invocation_id: r.opt_text("invocation_id")?.unwrap_or_default(),
        author: r.opt_text("author")?.unwrap_or_default(),
        actions: r.opt_bytes("actions")?.unwrap_or_default(),
        long_running_tool_ids_json: r.opt_text("long_running_tool_ids_json")?,
        routes_json: r.opt_text("routes_json")?,
        output_json: r.opt_text("output_json")?,
        node_info_json: r.opt_text("node_info_json")?,
        requested_input_json: r.opt_text("requested_input_json")?,
        branch: r.opt_text("branch")?,
        isolation_scope: r.opt_text("isolation_scope")?,
        timestamp: r.time("timestamp")?,
        content: r.opt_text("content")?,
        grounding_metadata: r.opt_text("grounding_metadata")?,
        custom_metadata: r.opt_text("custom_metadata")?,
        usage_metadata: r.opt_text("usage_metadata")?,
        citation_metadata: r.opt_text("citation_metadata")?,
        input_transcription: r.opt_text("input_transcription")?,
        output_transcription: r.opt_text("output_transcription")?,
        partial: r.flag("partial")?,
        turn_complete: r.flag("turn_complete")?,
        error_code: r.opt_text("error_code")?,
        error_message: r.opt_text("error_message")?,
        interrupted: r.flag("interrupted")?,
    })
}

fn go_json<T: Serialize + ?Sized>(v: &T) -> Result<String, serde_json::Error> {
    to_go_json(v).map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// The state as JSON text, in the form [`gadk_model::to_go_json`] writes.
pub(crate) fn encode_state(state: &StateMap) -> Result<String, serde_json::Error> {
    go_json(state)
}

/// Parses stored state: `""` and `null` are the empty map; anything else must be a JSON object.
pub(crate) fn decode_state(text: &str) -> Result<StateMap, serde_json::Error> {
    if text.is_empty() {
        return Ok(StateMap::new());
    }
    match serde_json::from_str::<Value>(text)? {
        Value::Null => Ok(StateMap::new()),
        other => serde_json::from_value(other),
    }
}

/// A JSON column written only when `value` is present (callers pass `None` for an empty
/// collection).
fn column<T: Serialize + ?Sized>(
    what: &str,
    value: Option<&T>,
) -> Result<Option<String>, SessionError> {
    value
        .map(|v| go_json(v).map_err(|e| errors::wrap(&format!("failed to marshal {what}"), e)))
        .transpose()
}

fn non_empty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_string())
}

/// Builds the stored row of an event.
pub(crate) fn event_to_row(
    app_name: &str,
    user_id: &str,
    session_id: &str,
    event: &Event,
) -> Result<EventRow, SessionError> {
    let actions = to_go_json(&event.actions)
        .map_err(|e| errors::wrap("failed to marshal event actions", e))?;
    let r = &event.response;
    Ok(EventRow {
        id: event.id.clone(),
        app_name: app_name.to_string(),
        user_id: user_id.to_string(),
        session_id: session_id.to_string(),
        invocation_id: event.invocation_id.clone(),
        author: event.author.clone(),
        actions,
        long_running_tool_ids_json: column(
            "long running tool IDs",
            Some(&event.long_running_tool_ids).filter(|v| !v.is_empty()),
        )?,
        routes_json: column("event route", Some(&event.routes).filter(|v| !v.is_empty()))?,
        output_json: column("output", event.output.as_ref().filter(|v| !v.is_null()))?,
        node_info_json: column("node info", event.node_info.as_ref())?,
        requested_input_json: column("requested input", event.requested_input.as_ref())?,
        branch: non_empty(&event.branch),
        isolation_scope: non_empty(&event.isolation_scope),
        timestamp: event.timestamp,
        content: column("content", r.content.as_ref())?,
        grounding_metadata: column("grounding metadata", r.grounding_metadata.as_ref())?,
        custom_metadata: column(
            "custom metadata",
            Some(&r.custom_metadata).filter(|m| !m.is_empty()),
        )?,
        usage_metadata: column("usage metadata", r.usage_metadata.as_ref())?,
        citation_metadata: column("citation metadata", r.citation_metadata.as_ref())?,
        input_transcription: column("input transcription", r.input_transcription.as_ref())?,
        output_transcription: column("output transcription", r.output_transcription.as_ref())?,
        partial: Some(r.partial),
        turn_complete: Some(r.turn_complete),
        error_code: non_empty(&r.error_code),
        error_message: non_empty(&r.error_message),
        interrupted: Some(r.interrupted),
    })
}

/// A JSON column read back; NULL and `""` are absent, `null` decodes to `None`.
fn parse<T: DeserializeOwned>(what: &str, text: Option<&str>) -> Result<Option<T>, SessionError> {
    match text {
        None | Some("") => Ok(None),
        Some(s) => serde_json::from_str::<Option<T>>(s)
            .map_err(|e| errors::wrap(&format!("failed to unmarshal {what}"), e)),
    }
}

/// Rebuilds an event from its stored row.
pub(crate) fn row_to_event(row: &EventRow) -> Result<Event, SessionError> {
    let actions: EventActions = if row.actions.is_empty() {
        EventActions::default()
    } else {
        serde_json::from_slice(&row.actions)
            .map_err(|e| errors::wrap("failed to unmarshal actions", e))?
    };
    Ok(Event {
        response: LlmResponse {
            content: parse("content", row.content.as_deref())?,
            grounding_metadata: parse("grounding metadata", row.grounding_metadata.as_deref())?,
            custom_metadata: parse("custom metadata", row.custom_metadata.as_deref())?
                .unwrap_or_default(),
            usage_metadata: parse("usage metadata", row.usage_metadata.as_deref())?,
            citation_metadata: parse("citation metadata", row.citation_metadata.as_deref())?,
            input_transcription: parse("input transcription", row.input_transcription.as_deref())?,
            output_transcription: parse(
                "output transcription",
                row.output_transcription.as_deref(),
            )?,
            error_code: row.error_code.clone().unwrap_or_default(),
            error_message: row.error_message.clone().unwrap_or_default(),
            partial: row.partial.unwrap_or(false),
            turn_complete: row.turn_complete.unwrap_or(false),
            interrupted: row.interrupted.unwrap_or(false),
            ..Default::default()
        },
        id: row.id.clone(),
        timestamp: row.timestamp,
        invocation_id: row.invocation_id.clone(),
        branch: row.branch.clone().unwrap_or_default(),
        isolation_scope: row.isolation_scope.clone().unwrap_or_default(),
        author: row.author.clone(),
        actions,
        long_running_tool_ids: parse(
            "long running tool IDs",
            row.long_running_tool_ids_json.as_deref(),
        )?
        .unwrap_or_default(),
        routes: parse("event route", row.routes_json.as_deref())?.unwrap_or_default(),
        requested_input: parse("requested input", row.requested_input_json.as_deref())?,
        output: parse("output", row.output_json.as_deref())?,
        node_info: parse("node info", row.node_info_json.as_deref())?,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::field_reassign_with_default)]
    use super::*;
    use chrono::TimeZone;
    use gadk_session::{NodeInfo, RequestInput};
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn obj(v: Value) -> StateMap {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn encode_state_writes_go_json() {
        assert_eq!(
            encode_state(&obj(json!({"b": "<x>", "a": 1.5, "big": 1e21, "i": 2}))).unwrap(),
            r#"{"a":1.5,"b":"\u003cx\u003e","big":1e+21,"i":2}"#
        );
        assert_eq!(encode_state(&StateMap::new()).unwrap(), "{}");
    }

    #[test]
    fn decode_state_treats_empty_and_null_as_empty() {
        assert_eq!(decode_state("").unwrap(), StateMap::new());
        assert_eq!(decode_state("null").unwrap(), StateMap::new());
        assert_eq!(decode_state(r#"{"k":1}"#).unwrap(), obj(json!({"k": 1})));
        assert!(decode_state("[1]").is_err());
    }

    #[test]
    fn a_bare_event_stores_nulls_and_explicit_flags() {
        let mut ev = Event::default();
        ev.id = "e".into();
        let row = event_to_row("app", "u", "s", &ev).unwrap();
        assert_eq!(row.actions, b"{}".to_vec());
        assert_eq!(
            (row.partial, row.turn_complete, row.interrupted),
            (Some(false), Some(false), Some(false))
        );
        assert_eq!(row.branch, None);
        assert_eq!(row.content, None);
        assert_eq!(row.long_running_tool_ids_json, None);
        assert_eq!(row.custom_metadata, None);
        assert_eq!(row.output_json, None);
        assert_eq!(
            (
                row.app_name.as_str(),
                row.user_id.as_str(),
                row.session_id.as_str()
            ),
            ("app", "u", "s")
        );
    }

    #[test]
    fn event_round_trips_through_a_row() {
        let mut ev = Event::default();
        ev.id = "e1".into();
        ev.invocation_id = "inv".into();
        ev.author = "agent".into();
        ev.timestamp = Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap();
        ev.branch = "a.b".into();
        ev.isolation_scope = "task-1".into();
        ev.long_running_tool_ids = vec!["c1".into()];
        ev.routes = vec!["r".into()];
        ev.output = Some(json!({"x": 1}));
        ev.node_info = Some(NodeInfo {
            path: "ask".into(),
            ..Default::default()
        });
        ev.requested_input = Some(RequestInput {
            interrupt_id: "ask".into(),
            message: "Name?".into(),
            ..Default::default()
        });
        ev.actions.state_delta = Some(obj(json!({"k": "v"})));
        ev.response.content = Some(gadk_genai::Content::from_text("hi <b>", "model"));
        ev.response.custom_metadata = obj(json!({"t": "x"}));
        ev.response.turn_complete = true;
        ev.response.error_code = "E".into();
        ev.response.error_message = "m".into();
        let row = event_to_row("app", "u", "s", &ev).unwrap();
        assert_eq!(
            row.content.as_deref(),
            Some(r#"{"parts":[{"text":"hi \u003cb\u003e"}],"role":"model"}"#)
        );
        assert_eq!(row.routes_json.as_deref(), Some(r#"["r"]"#));
        assert_eq!(row.output_json.as_deref(), Some(r#"{"x":1}"#));
        assert_eq!(row_to_event(&row).unwrap(), ev);
    }

    #[test]
    fn a_null_output_is_not_stored() {
        let mut ev = Event::default();
        ev.output = Some(Value::Null);
        assert_eq!(event_to_row("a", "u", "s", &ev).unwrap().output_json, None);
    }

    #[test]
    fn bad_actions_name_the_column_like_go() {
        let mut row = event_to_row("a", "u", "s", &Event::default()).unwrap();
        row.actions = b"[".to_vec();
        let err = row_to_event(&row).unwrap_err().to_string();
        assert!(err.starts_with("failed to unmarshal actions: "), "{err}");
    }
}
