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

//! Go `server/agentengine/controllers/method/streaming_agent_run_with_events.go`
//! (`streaming_agent_run_with_events`, Gemini Enterprise's method).

use bytes::Bytes;
use serde_json::{Map, Value};

use super::MethodHandler;
use crate::agentengine::models::{
    StreamingAgentRunWithEventsRequest, StreamingAgentRunWithEventsRunRequest,
};
use crate::agentengine::{AgentEngineConfig, Log};
use crate::mux::Response;

/// Go `NewStreamingAgentRunWithEventsHandler`'s handler.
pub(crate) struct StreamingAgentRunWithEventsHandler {
    /// Go `config`.
    pub(crate) config: AgentEngineConfig,
    /// Go `agentEngineID`: the runner's and the sessions' app name.
    pub(crate) agent_engine_id: String,
    pub(crate) method_name: String,
    pub(crate) api_mode: String,
    /// Go `log` (from `config.log`).
    pub(crate) log: Log,
}

impl StreamingAgentRunWithEventsHandler {
    /// Go `NewStreamingAgentRunWithEventsHandler(config, agentEngineID, methodName, apiMode)`.
    pub(crate) fn new(
        config: AgentEngineConfig,
        agent_engine_id: &str,
        method_name: &str,
        api_mode: &str,
    ) -> Self {
        let log = Log(config.log.clone());
        StreamingAgentRunWithEventsHandler {
            config,
            agent_engine_id: agent_engine_id.to_owned(),
            method_name: method_name.to_owned(),
            api_mode: api_mode.to_owned(),
            log,
        }
    }

    /// Go `streamJSONL`: decodes the request (`json.Unmarshal() failed for
    /// models.StreamingAgentRunWithEventsRequest: <err>`), its `request_json`
    /// ([`decode_streaming_agent_run_with_events_request`];
    /// `decodeStreamingAgentRunWithEventsRequest() failed: <err>`), runs
    /// [`Self::ensure_backend_session`] (`s.ensureBackendSession() failed: <err>`), builds the
    /// runner (`s.run() failed: …`) — each error also logged — then streams as `stream_query`
    /// does, each event as one `emit_json(&StreamingAgentRunWithEventsResponse { events:
    /// vec![event], session_id: <backend session ID> })` line.
    pub(crate) async fn stream_jsonl(&self, payload: Bytes) -> Result<Response, String> {
        todo!("Task 9")
    }

    /// Go `ensureBackendSession`: with a non-empty `requested_session_id`, requires `user_id`
    /// (`user_id is required for backend session handling`), reuses the session when `get`
    /// finds it (setting `req.session_id` to its ID), else creates one for the user
    /// (`sessionService.Create() failed: <err>`) and sets `req.session_id` to the new ID. Go's
    /// `session service is required …` branch has no counterpart (the config always has one).
    pub(crate) async fn ensure_backend_session(
        &self,
        req: &mut StreamingAgentRunWithEventsRunRequest,
        requested_session_id: &str,
    ) -> Result<(), String> {
        todo!("Task 9")
    }

    /// Go `run`: as `StreamQueryHandler::run` with `StreamingMode::None`.
    pub(crate) fn run(
        &self,
        req: &StreamingAgentRunWithEventsRunRequest,
    ) -> Result<gadk_agent::EventStream, String> {
        todo!("Task 9")
    }
}

/// Go `decodeStreamingAgentRunWithEventsRequest`: `input.request_json` decoded (`go_unmarshal`;
/// `json.Unmarshal(input.request_json) failed: <err>`), and the session ID it requested.
pub(crate) fn decode_streaming_agent_run_with_events_request(
    req: &StreamingAgentRunWithEventsRequest,
) -> Result<(StreamingAgentRunWithEventsRunRequest, String), String> {
    todo!("Task 9")
}

/// Go `Metadata()`'s `description`.
const DESCRIPTION: &str = "Streams responses asynchronously from the ADK application.\n\nThis method is primarily meant for invocation from Gemini Enterprise.\n\nArgs:\n    request_json (str):\n        Required. A JSON-encoded request object with:\n        - user_id (str): Required. The user ID to run the agent for.\n        - session_id (str): Optional. The session ID. Gemini Enterprise may\n          send its Discovery Engine session resource on the first turn; later\n          turns should use the backend ADK session ID returned in the response.\n        - message (Content): Required. The user message, using the genai\n          Content JSON shape, for example:\n          {\"role\":\"user\",\"parts\":[{\"text\":\"Hi\"}]}\n\n";

/// Go `Metadata()`'s map for `method_name` / `api_mode` (description text verbatim).
pub(crate) fn metadata(method_name: &str, api_mode: &str) -> Map<String, Value> {
    super::method_metadata(
        method_name,
        api_mode,
        serde_json::json!({
            "properties": {
                "request_json": {
                    "type": "string",
                },
            },
            "required": [
                "request_json",
            ],
            "type": "object",
        }),
        DESCRIPTION,
    )
}

#[async_trait::async_trait]
impl MethodHandler for StreamingAgentRunWithEventsHandler {
    fn name(&self) -> &str {
        &self.method_name
    }

    /// Go `Handle`: [`StreamingAgentRunWithEventsHandler::stream_jsonl`], an error wrapped as
    /// `s.streamJSONL() failed: <err>`.
    async fn handle(&self, payload: Bytes) -> Result<Response, String> {
        todo!("Task 9")
    }

    fn metadata(&self) -> Map<String, Value> {
        metadata(&self.method_name, &self.api_mode)
    }
}
