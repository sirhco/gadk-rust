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

//! Rows as stored (Go `storageSession`, `storageEvent`, `storageAppState`, `storageUserState`)
//! and their mapping to gadk values. JSON columns hold Go `json.Marshal` bytes
//! ([`gadk_model::to_go_json`]).
#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use chrono::{DateTime, Utc};
use gadk_session::{Event, SessionError};
use serde_json::{Map, Value};

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

/// Go `stateMap.Value`: the state as Go `json.Marshal` text.
pub(crate) fn encode_state(state: &StateMap) -> Result<String, serde_json::Error> {
    todo!("Task 5")
}

/// Go `stateMap.Scan`: `""` and `null` are the empty map; anything else must be a JSON object.
pub(crate) fn decode_state(text: &str) -> Result<StateMap, serde_json::Error> {
    todo!("Task 5")
}

/// Go `createStorageEvent`.
pub(crate) fn event_to_row(
    app_name: &str,
    user_id: &str,
    session_id: &str,
    event: &Event,
) -> Result<EventRow, SessionError> {
    todo!("Task 5")
}

/// Go `createEventFromStorageEvent`.
pub(crate) fn row_to_event(row: &EventRow) -> Result<Event, SessionError> {
    todo!("Task 5")
}
