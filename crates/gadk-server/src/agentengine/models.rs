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

//! The Agent Engine request and response models, with their wire JSON names. Requests are
//! decoded by [`go_unmarshal`](super::unmarshal::go_unmarshal) (case-insensitive key matching,
//! then serde); the session responses are written by `gadk_model::to_go_json` (field order is
//! the JSON key order); the streaming responses by [`emit_json`](super::encode::emit_json).

use gadk_genai::Content;
use gadk_session::{Event, Session};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The generic query payload; `input` is method-specific.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct Query {
    /// The method to call (`class_method`).
    #[serde(default)]
    pub(crate) class_method: String,
    /// The method-specific input (`input`); `None` when absent or `null`.
    #[serde(default)]
    pub(crate) input: Option<Value>,
}

/// Request of `async_create_session`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct CreateSessionRequest {
    /// The method to call (`class_method`).
    #[serde(default)]
    pub(crate) class_method: String,
    /// The method's input (`input`).
    #[serde(default)]
    pub(crate) input: CreateSessionInput,
}

/// Input of [`CreateSessionRequest`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct CreateSessionInput {
    /// The user ID (`user_id`).
    #[serde(default)]
    pub(crate) user_id: String,
    /// The initial session state (`state`); `None` when absent or `null`.
    #[serde(default)]
    pub(crate) state: Option<Map<String, Value>>,
}

/// Response of `async_create_session`.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct CreateSessionResponse {
    /// The method's output (`output`).
    pub(crate) output: SessionData,
}

/// A session as the Agent Engine API reports it. Field order is the JSON key order.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SessionData {
    /// The user ID (`user_id`).
    pub(crate) user_id: String,
    /// Last update time (`last_update_time`): Unix seconds, as nanoseconds since the epoch
    /// divided by 1e9.
    pub(crate) last_update_time: f64,
    /// The app name (`app_name`).
    pub(crate) app_name: String,
    /// The session ID (`id`).
    pub(crate) id: String,
    /// The session state (`state`): always a map (`{}` when empty).
    pub(crate) state: Map<String, Value>,
    /// The events (`events`): always a list, each in the session event JSON
    /// (`gadk_session::Event`'s `Serialize`).
    pub(crate) events: Vec<Event>,
}

/// Request of `async_list_sessions`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct ListSessionRequest {
    /// The method to call (`class_method`).
    #[serde(default)]
    pub(crate) class_method: String,
    /// The method's input (`input`).
    #[serde(default)]
    pub(crate) input: ListSessionInput,
}

/// Input of [`ListSessionRequest`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct ListSessionInput {
    /// The user ID (`user_id`).
    #[serde(default)]
    pub(crate) user_id: String,
}

/// Response of `async_list_sessions`.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ListSessionResponse {
    /// The method's output (`output`).
    pub(crate) output: Sessions,
}

/// The sessions of a [`ListSessionResponse`].
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Sessions {
    /// The sessions (`sessions`): always a list.
    pub(crate) sessions: Vec<SessionData>,
}

/// Request of `async_get_session`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct GetSessionRequest {
    /// The method to call (`class_method`).
    #[serde(default)]
    pub(crate) class_method: String,
    /// The method's input (`input`).
    #[serde(default)]
    pub(crate) input: GetSessionInput,
}

/// Input of [`GetSessionRequest`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct GetSessionInput {
    /// The user ID (`user_id`).
    #[serde(default)]
    pub(crate) user_id: String,
    /// The session ID (`session_id`).
    #[serde(default)]
    pub(crate) session_id: String,
}

/// Response of `async_get_session`.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct GetSessionResponse {
    /// The method's output (`output`).
    pub(crate) output: SessionData,
}

/// Request of `async_delete_session`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct DeleteSessionRequest {
    /// The method to call (`class_method`).
    #[serde(default)]
    pub(crate) class_method: String,
    /// The method's input (`input`).
    #[serde(default)]
    pub(crate) input: DeleteSessionInput,
}

/// Input of [`DeleteSessionRequest`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct DeleteSessionInput {
    /// The user ID (`user_id`).
    #[serde(default)]
    pub(crate) user_id: String,
    /// The session ID (`session_id`).
    #[serde(default)]
    pub(crate) session_id: String,
}

/// Response of `async_delete_session`: `{"output":""}`.
#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct DeleteSessionResponse {
    /// The method's output (`output`).
    pub(crate) output: String,
}

/// Converts a session to [`SessionData`]: the session's user, `last_update_time` (nanoseconds
/// since the epoch / 1e9), app name, ID, state (all keys) and events.
pub(crate) fn from_session(sess: &dyn Session) -> SessionData {
    let t = sess.last_update_time();
    // Nanoseconds since the epoch: seconds * 1e9 + nanoseconds, wrapping on overflow.
    let unix_nano = t
        .timestamp()
        .wrapping_mul(1_000_000_000)
        .wrapping_add(i64::from(t.timestamp_subsec_nanos()));
    SessionData {
        user_id: sess.user_id().to_owned(),
        last_update_time: unix_nano as f64 / 1e9,
        app_name: sess.app_name().to_owned(),
        id: sess.id().to_owned(),
        state: sess.state().all().into_iter().collect(),
        events: sess.events(),
    }
}

/// Request of `async_stream_query` with a full [`Content`] message.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct StreamQueryRequest {
    /// The method to call (`class_method`).
    #[serde(default)]
    pub(crate) class_method: String,
    /// The method's input (`input`).
    #[serde(default)]
    pub(crate) input: StreamQueryInput,
}

/// Input of [`StreamQueryRequest`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct StreamQueryInput {
    /// The user ID (`user_id`).
    #[serde(default)]
    pub(crate) user_id: String,
    /// The session ID (`session_id`).
    #[serde(default)]
    pub(crate) session_id: String,
    /// The message (`message`): a value, empty when absent.
    #[serde(default)]
    pub(crate) message: Content,
}

/// Request of `async_stream_query` with a plain-text message.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct StreamQueryTextRequest {
    /// The method to call (`class_method`).
    #[serde(default)]
    pub(crate) class_method: String,
    /// The method's input (`input`).
    #[serde(default)]
    pub(crate) input: StreamQueryTextInput,
}

/// Input of [`StreamQueryTextRequest`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct StreamQueryTextInput {
    /// The user ID (`user_id`).
    #[serde(default)]
    pub(crate) user_id: String,
    /// The session ID (`session_id`).
    #[serde(default)]
    pub(crate) session_id: String,
    /// The message text (`message`).
    #[serde(default)]
    pub(crate) message: String,
}

/// Request of `streaming_agent_run_with_events`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct StreamingAgentRunWithEventsRequest {
    /// The method to call (`class_method`).
    #[serde(default)]
    pub(crate) class_method: String,
    /// The method's input (`input`).
    #[serde(default)]
    pub(crate) input: StreamingAgentRunWithEventsInput,
}

/// Input of [`StreamingAgentRunWithEventsRequest`]: `request_json` is a JSON-encoded
/// [`StreamingAgentRunWithEventsRunRequest`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct StreamingAgentRunWithEventsInput {
    /// The JSON-encoded run request (`request_json`).
    #[serde(default)]
    pub(crate) request_json: String,
}

/// The run request, decoded from `input.request_json`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct StreamingAgentRunWithEventsRunRequest {
    /// The user ID (`user_id`).
    #[serde(default)]
    pub(crate) user_id: String,
    /// The session ID (`session_id`).
    #[serde(default)]
    pub(crate) session_id: String,
    /// The user message (`message`).
    #[serde(default)]
    pub(crate) message: Content,
}

/// One NDJSON line of `streaming_agent_run_with_events` (written through `GoSnake`;
/// `Serialize` is the `Failed to convert` fallback, the plain JSON of the struct).
#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct StreamingAgentRunWithEventsResponse {
    /// The events (`events`); omitted when empty.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) events: Vec<Event>,
    /// The artifacts (`artifacts`); omitted when empty. Never set.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) artifacts: Vec<Value>,
    /// The session ID (`session_id`); omitted when empty.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) session_id: String,
}
