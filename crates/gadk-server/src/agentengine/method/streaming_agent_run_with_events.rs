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
//! The `streaming_agent_run_with_events` method (the one Gemini Enterprise invokes).

use bytes::Bytes;
use gadk_agent::{EventStream, RunConfig, StreamingMode};
use gadk_engine::runner::RunOptions;
use gadk_session::{CreateRequest, Event, GetRequest};
use serde_json::{Map, Value};

use super::MethodHandler;
use super::stream_query::{ndjson_response, new_runner};
use crate::agentengine::encode::emit_json;
use crate::agentengine::encode_shapes;
use crate::agentengine::models::{
    StreamingAgentRunWithEventsRequest, StreamingAgentRunWithEventsResponse,
    StreamingAgentRunWithEventsRunRequest,
};
use crate::agentengine::unmarshal::go_unmarshal;
use crate::agentengine::{AgentEngineConfig, Log};
use crate::mux::Response;

/// Handler for `streaming_agent_run_with_events`.
pub(crate) struct StreamingAgentRunWithEventsHandler {
    /// Services, loader, plugins, compaction and platform.
    pub(crate) config: AgentEngineConfig,
    /// The runner's and the sessions' app name.
    pub(crate) agent_engine_id: String,
    pub(crate) method_name: String,
    pub(crate) api_mode: String,
    /// Where log lines go (from `config.log`).
    pub(crate) log: Log,
}

impl StreamingAgentRunWithEventsHandler {
    /// A handler serving `method_name` in `api_mode`, running the root agent of `config`.
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

    /// Decodes the request (a decode error naming
    /// `StreamingAgentRunWithEventsRequest`), its `request_json`
    /// ([`decode_streaming_agent_run_with_events_request`];
    /// `invalid streaming_agent_run_with_events request: <err>`), runs
    /// [`Self::ensure_backend_session`] (`failed to ensure backend session: <err>`), builds the
    /// runner (`failed to run agent: …`) — each error also logged — then streams as `stream_query`
    /// does, each event as one `emit_json(&StreamingAgentRunWithEventsResponse { events:
    /// vec![event], session_id: <backend session ID> })` line.
    pub(crate) async fn stream_jsonl(&self, payload: Bytes) -> Result<Response, String> {
        let fail = |err: String| {
            self.log.print(&err);
            err
        };
        let req = go_unmarshal::<StreamingAgentRunWithEventsRequest>(
            &payload,
            &encode_shapes::MODELS_STREAMING_AGENT_RUN_WITH_EVENTS_REQUEST,
        )
        .map_err(|err| {
            fail(format!(
                "failed to decode StreamingAgentRunWithEventsRequest: {err}"
            ))
        })?;

        let (mut run_req, requested_session_id) =
            decode_streaming_agent_run_with_events_request(&req).map_err(|err| {
                fail(format!(
                    "invalid streaming_agent_run_with_events request: {err}"
                ))
            })?;
        self.ensure_backend_session(&mut run_req, &requested_session_id)
            .await
            .map_err(|err| fail(format!("failed to ensure backend session: {err}")))?;

        let events = self
            .run(&run_req)
            .map_err(|err| fail(format!("failed to run agent: {err}")))?;

        let session_id = run_req.session_id;

        Ok(ndjson_response(
            events,
            self.log.clone(),
            envelope_line(session_id),
        ))
    }

    /// With a non-empty `requested_session_id`, requires `user_id`
    /// (`user_id is required for backend session handling`), reuses the session when `get`
    /// finds it (setting `req.session_id` to its ID), else creates one for the user
    /// (`failed to create session: <err>`) and sets `req.session_id` to the new ID. The
    /// config always has a session service, so there is no "service missing" error.
    pub(crate) async fn ensure_backend_session(
        &self,
        req: &mut StreamingAgentRunWithEventsRunRequest,
        requested_session_id: &str,
    ) -> Result<(), String> {
        if requested_session_id.is_empty() {
            return Ok(());
        }
        if req.user_id.is_empty() {
            return Err("user_id is required for backend session handling".to_owned());
        }

        let service = &self.config.session_service;
        let got = service
            .get(GetRequest {
                app_name: self.agent_engine_id.clone(),
                user_id: req.user_id.clone(),
                session_id: requested_session_id.to_owned(),
                ..Default::default()
            })
            .await;
        if let Ok(sess) = got {
            req.session_id = sess.id().to_owned();
            return Ok(());
        }

        let created = service
            .create(CreateRequest {
                app_name: self.agent_engine_id.clone(),
                user_id: req.user_id.clone(),
                session_id: None,
                state: None,
            })
            .await
            .map_err(|err| format!("failed to create session: {err}"))?;
        req.session_id = created.id().to_owned();
        Ok(())
    }

    /// Runs like `StreamQueryHandler::run` but with `StreamingMode::None`.
    pub(crate) fn run(
        &self,
        req: &StreamingAgentRunWithEventsRunRequest,
    ) -> Result<EventStream, String> {
        let runner = new_runner(&self.config, &self.agent_engine_id)?;
        // The path mirrors Python AdkApp.streaming_agent_run_with_events, which does not force
        // SSE mode. Sending both partial and final events here causes Gemini Enterprise to
        // render duplicate answer text.
        Ok(runner.run(
            &req.user_id,
            &req.session_id,
            Some(req.message.clone()),
            RunConfig {
                streaming_mode: StreamingMode::None,
                ..Default::default()
            },
            RunOptions::default(),
        ))
    }
}

/// The per-event line writer: a `StreamingAgentRunWithEventsResponse` holding the one event and
/// `session_id`, the AgentSpace envelope.
pub(crate) fn envelope_line(
    session_id: String,
) -> impl Fn(&Event, &Log) -> Result<Bytes, String> + Send + 'static {
    move |event, log| {
        emit_json(
            &StreamingAgentRunWithEventsResponse {
                events: vec![event.clone()],
                artifacts: Vec::new(),
                session_id: session_id.clone(),
            },
            log,
        )
    }
}

/// Decodes `input.request_json` (via `go_unmarshal`; a failure is
/// the decode error text, see the literal in this function), and returns it with the session ID it
/// requested.
pub(crate) fn decode_streaming_agent_run_with_events_request(
    req: &StreamingAgentRunWithEventsRequest,
) -> Result<(StreamingAgentRunWithEventsRunRequest, String), String> {
    let run_req = go_unmarshal::<StreamingAgentRunWithEventsRunRequest>(
        req.input.request_json.as_bytes(),
        &encode_shapes::MODELS_STREAMING_AGENT_RUN_WITH_EVENTS_RUN_REQUEST,
    )
    .map_err(|err| format!("failed to decode input.request_json: {err}"))?;
    let requested_session_id = run_req.session_id.clone();
    Ok((run_req, requested_session_id))
}

/// The method's `description` in its metadata.
const DESCRIPTION: &str = "Streams responses asynchronously from the ADK application.\n\nThis method is primarily meant for invocation from Gemini Enterprise.\n\nArgs:\n    request_json (str):\n        Required. A JSON-encoded request object with:\n        - user_id (str): Required. The user ID to run the agent for.\n        - session_id (str): Optional. The session ID. Gemini Enterprise may\n          send its Discovery Engine session resource on the first turn; later\n          turns should use the backend ADK session ID returned in the response.\n        - message (Content): Required. The user message, using the genai\n          Content JSON shape, for example:\n          {\"role\":\"user\",\"parts\":[{\"text\":\"Hi\"}]}\n\n";

/// The method's metadata map for `method_name` / `api_mode` (description text verbatim).
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

    /// Runs [`StreamingAgentRunWithEventsHandler::stream_jsonl`], an error wrapped as
    /// `streaming JSONL failed: <err>`.
    async fn handle(&self, payload: Bytes) -> Result<Response, String> {
        // stream_jsonl fails only before streaming; the controller answers that with a status.
        self.stream_jsonl(payload)
            .await
            .map_err(|err| format!("streaming JSONL failed: {err}"))
    }

    fn metadata(&self) -> Map<String, Value> {
        metadata(&self.method_name, &self.api_mode)
    }
}
