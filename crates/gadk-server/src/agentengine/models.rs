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
#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

//! Go `server/agentengine/internal/models`: the request and response models, with Go's JSON
//! names. Requests are decoded by [`go_unmarshal`](super::unmarshal::go_unmarshal) (Go key
//! matching, then serde); the session responses are written by `gadk_model::to_go_json` (field
//! order as Go's struct order); the streaming responses by [`emit_json`](super::encode::emit_json).

use gadk_genai::Content;
use gadk_session::{Event, Session};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Go `Query`: the generic payload; `input` is method-specific.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct Query {
    /// Go `ClassMethod` (`json:"class_method"`).
    #[serde(default)]
    pub(crate) class_method: String,
    /// Go `Input any` (`json:"input"`); `None` is Go's nil.
    #[serde(default)]
    pub(crate) input: Option<Value>,
}

/// Go `CreateSessionRequest`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct CreateSessionRequest {
    /// Go `ClassMethod` (`json:"class_method"`).
    #[serde(default)]
    pub(crate) class_method: String,
    /// Go `Input` (`json:"input"`).
    #[serde(default)]
    pub(crate) input: CreateSessionInput,
}

/// Go `CreateSessionInput`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct CreateSessionInput {
    /// Go `UserID` (`json:"user_id"`).
    #[serde(default)]
    pub(crate) user_id: String,
    /// Go `State map[string]any` (`json:"state,omitempty"`); `None` is Go's nil map.
    #[serde(default)]
    pub(crate) state: Option<Map<String, Value>>,
}

/// Go `CreateSessionResponse`.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct CreateSessionResponse {
    /// Go `Output` (`json:"output"`).
    pub(crate) output: SessionData,
}

/// Go `SessionData`. Field order is Go's (it is the JSON key order).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SessionData {
    /// Go `UserID` (`json:"user_id"`).
    pub(crate) user_id: String,
    /// Go `LastUpdateTime float64` (`json:"last_update_time"`): Unix seconds,
    /// `UnixNano() as f64 / 1e9`.
    pub(crate) last_update_time: f64,
    /// Go `AppName` (`json:"app_name"`).
    pub(crate) app_name: String,
    /// Go `ID` (`json:"id"`).
    pub(crate) id: String,
    /// Go `State map[string]any` (`json:"state"`): always a map (`{}` when empty).
    pub(crate) state: Map<String, Value>,
    /// Go `Events []session.Event` (`json:"events"`): always a list, each in Go's
    /// `session.Event` JSON (`gadk_session::Event`'s `Serialize`).
    pub(crate) events: Vec<Event>,
}

/// Go `ListSessionRequest`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct ListSessionRequest {
    /// Go `ClassMethod` (`json:"class_method"`).
    #[serde(default)]
    pub(crate) class_method: String,
    /// Go `Input` (`json:"input"`).
    #[serde(default)]
    pub(crate) input: ListSessionInput,
}

/// Go `ListSessionInput`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct ListSessionInput {
    /// Go `UserID` (`json:"user_id"`).
    #[serde(default)]
    pub(crate) user_id: String,
}

/// Go `ListSessionResponse`.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ListSessionResponse {
    /// Go `Output` (`json:"output"`).
    pub(crate) output: Sessions,
}

/// Go `Sessions`.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Sessions {
    /// Go `Sessions` (`json:"sessions"`): always a list.
    pub(crate) sessions: Vec<SessionData>,
}

/// Go `GetSessionRequest`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct GetSessionRequest {
    /// Go `ClassMethod` (`json:"class_method"`).
    #[serde(default)]
    pub(crate) class_method: String,
    /// Go `Input` (`json:"input"`).
    #[serde(default)]
    pub(crate) input: GetSessionInput,
}

/// Go `GetSessionInput`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct GetSessionInput {
    /// Go `UserID` (`json:"user_id"`).
    #[serde(default)]
    pub(crate) user_id: String,
    /// Go `SessionID` (`json:"session_id"`).
    #[serde(default)]
    pub(crate) session_id: String,
}

/// Go `GetSessionResponse`.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct GetSessionResponse {
    /// Go `Output` (`json:"output"`).
    pub(crate) output: SessionData,
}

/// Go `DeleteSessionRequest`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct DeleteSessionRequest {
    /// Go `ClassMethod` (`json:"class_method"`).
    #[serde(default)]
    pub(crate) class_method: String,
    /// Go `Input` (`json:"input"`).
    #[serde(default)]
    pub(crate) input: DeleteSessionInput,
}

/// Go `DeleteSessionInput`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct DeleteSessionInput {
    /// Go `UserID` (`json:"user_id"`).
    #[serde(default)]
    pub(crate) user_id: String,
    /// Go `SessionID` (`json:"session_id"`).
    #[serde(default)]
    pub(crate) session_id: String,
}

/// Go `DeleteSessionResponse`: `{"output":""}`.
#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct DeleteSessionResponse {
    /// Go `Output` (`json:"output"`).
    pub(crate) output: String,
}

/// Go `FromSession`: the session's user, `last_update_time` (`UnixNano() / 1e9`), app name, ID,
/// state (all keys) and events.
pub(crate) fn from_session(sess: &dyn Session) -> SessionData {
    todo!("Task 7")
}

/// Go `StreamQueryRequest`: `async_stream_query` with a full `genai.Content` message.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct StreamQueryRequest {
    /// Go `ClassMethod` (`json:"class_method"`).
    #[serde(default)]
    pub(crate) class_method: String,
    /// Go `Input` (`json:"input"`).
    #[serde(default)]
    pub(crate) input: StreamQueryInput,
}

/// Go `StreamQueryInput`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct StreamQueryInput {
    /// Go `UserID` (`json:"user_id"`).
    #[serde(default)]
    pub(crate) user_id: String,
    /// Go `SessionID` (`json:"session_id"`).
    #[serde(default)]
    pub(crate) session_id: String,
    /// Go `Message genai.Content` (`json:"message"`): a value, zero when absent.
    #[serde(default)]
    pub(crate) message: Content,
}

/// Go `StreamQueryTextRequest`: `async_stream_query` with a plain-text message.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct StreamQueryTextRequest {
    /// Go `ClassMethod` (`json:"class_method"`).
    #[serde(default)]
    pub(crate) class_method: String,
    /// Go `Input` (`json:"input"`).
    #[serde(default)]
    pub(crate) input: StreamQueryTextInput,
}

/// Go `StreamQueryTextInput`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct StreamQueryTextInput {
    /// Go `UserID` (`json:"user_id"`).
    #[serde(default)]
    pub(crate) user_id: String,
    /// Go `SessionID` (`json:"session_id"`).
    #[serde(default)]
    pub(crate) session_id: String,
    /// Go `Message string` (`json:"message"`).
    #[serde(default)]
    pub(crate) message: String,
}

/// Go `StreamingAgentRunWithEventsRequest`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct StreamingAgentRunWithEventsRequest {
    /// Go `ClassMethod` (`json:"class_method"`).
    #[serde(default)]
    pub(crate) class_method: String,
    /// Go `Input` (`json:"input"`).
    #[serde(default)]
    pub(crate) input: StreamingAgentRunWithEventsInput,
}

/// Go `StreamingAgentRunWithEventsInput`: `request_json` is a JSON-encoded
/// [`StreamingAgentRunWithEventsRunRequest`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct StreamingAgentRunWithEventsInput {
    /// Go `RequestJSON` (`json:"request_json"`).
    #[serde(default)]
    pub(crate) request_json: String,
}

/// Go `StreamingAgentRunWithEventsRunRequest`: decoded from `input.request_json`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub(crate) struct StreamingAgentRunWithEventsRunRequest {
    /// Go `UserID` (`json:"user_id"`).
    #[serde(default)]
    pub(crate) user_id: String,
    /// Go `SessionID` (`json:"session_id"`).
    #[serde(default)]
    pub(crate) session_id: String,
    /// Go `Message genai.Content` (`json:"message"`).
    #[serde(default)]
    pub(crate) message: Content,
}

/// Go `StreamingAgentRunWithEventsResponse`: one NDJSON line of
/// `streaming_agent_run_with_events` (written through `GoSnake`; `Serialize` is the
/// `Failed to convert` fallback, Go's `encoding/json` of the struct).
#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct StreamingAgentRunWithEventsResponse {
    /// Go `Events []*session.Event` (`json:"events,omitempty"`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) events: Vec<Event>,
    /// Go `Artifacts []any` (`json:"artifacts,omitempty"`): never set by Go.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) artifacts: Vec<Value>,
    /// Go `SessionID` (`json:"session_id,omitempty"`).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) session_id: String,
}
