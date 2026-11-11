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

//! Go `server/agentengine/controllers/method/stream_query.go` (`async_stream_query`).

use bytes::Bytes;
use serde_json::{Map, Value};

use super::MethodHandler;
use crate::agentengine::models::StreamQueryRequest;
use crate::agentengine::{AgentEngineConfig, Log};
use crate::mux::Response;

/// Go `NewStreamQueryHandler`'s handler.
pub(crate) struct StreamQueryHandler {
    /// Go `config`: services, loader, plugins, compaction, platform.
    pub(crate) config: AgentEngineConfig,
    /// Go `agentEngineID`: the runner's app name.
    pub(crate) agent_engine_id: String,
    pub(crate) method_name: String,
    pub(crate) api_mode: String,
    /// Go `log` (from `config.log`).
    pub(crate) log: Log,
}

impl StreamQueryHandler {
    /// Go `NewStreamQueryHandler(config, agentEngineID, methodName, apiMode)`.
    pub(crate) fn new(
        config: AgentEngineConfig,
        agent_engine_id: &str,
        method_name: &str,
        api_mode: &str,
    ) -> Self {
        let log = Log(config.log.clone());
        StreamQueryHandler {
            config,
            agent_engine_id: agent_engine_id.to_owned(),
            method_name: method_name.to_owned(),
            api_mode: api_mode.to_owned(),
            log,
        }
    }

    /// Go `streamJSONL`: decodes `payload` as `models.StreamQueryRequest` then
    /// `models.StreamQueryTextRequest` (both through `go_unmarshal`; both failing →
    /// `Err("json.Unmarshal() failed both for models.StreamQueryRequest (<e1>) and
    /// models.StreamQueryTextRequest (<e2>)")`, also logged; a text message becomes a user
    /// `Content` with one text part), builds the runner (`Err("s.run() failed: failed to create
    /// runner: <err>")`, logged), and answers 200 with `Content-Type: application/json`,
    /// `Cache-Control: no-cache`, `Connection: keep-alive` and a streaming body: per runner
    /// item, log `Processing event: … err: …`; a compaction error is logged `agentengine: <err>`
    /// and skipped; any other error is logged `error in events: <err>\n`, written as one
    /// `emit_json_error` line, and ends the stream; a `None`/content-less event is skipped; an
    /// event is one `emit_json(&event)` line (an emit failure is logged
    /// `helper.EmitJSON() failed: <err>`, written as an error line, and ends the stream).
    pub(crate) async fn stream_jsonl(&self, payload: Bytes) -> Result<Response, String> {
        todo!("Task 9")
    }

    /// Go `run`: a runner (`app_name` = the agent engine ID, the loader's root agent, the config's
    /// services, plugins, compaction and platform, `auto_create_session: true`) run for
    /// `req.input` with `StreamingMode::Sse`.
    pub(crate) fn run(&self, req: &StreamQueryRequest) -> Result<gadk_agent::EventStream, String> {
        todo!("Task 9")
    }
}

/// Go `Metadata()`'s `description`.
const DESCRIPTION: &str = "Streams responses asynchronously from the ADK application.\nArgs:\n    message (genai.Content):\n        Required. The message to stream responses for.\n    user_id (str):\n        Required. The ID of the user.\n    session_id (str):\n        Optional. The ID of the session. If not provided, a new session will be created for the user.\n\nYields:\n    Single lines with JSON encoded event each. Errors are also emitted as JSON.\n\n";

/// Go `Metadata()`'s map for `method_name` / `api_mode` (description text verbatim).
pub(crate) fn metadata(method_name: &str, api_mode: &str) -> Map<String, Value> {
    super::method_metadata(
        method_name,
        api_mode,
        serde_json::json!({
            "properties": {
                "message": {
                    "anyOf": [
                        {
                            "type": "string",
                        },
                        {
                            "additionalProperties": true,
                            "type": "object",
                        },
                    ],
                },
                "session_id": {
                    "nullable": true,
                    "type": "string",
                },
                "user_id": {
                    "type": "string",
                },
            },
            "required": [
                "message",
                "user_id",
            ],
            "type": "object",
        }),
        DESCRIPTION,
    )
}

#[async_trait::async_trait]
impl MethodHandler for StreamQueryHandler {
    fn name(&self) -> &str {
        &self.method_name
    }

    /// Go `Handle`: [`StreamQueryHandler::stream_jsonl`], an error wrapped as
    /// `s.streamJSONL() failed: <err>`.
    async fn handle(&self, payload: Bytes) -> Result<Response, String> {
        todo!("Task 9")
    }

    fn metadata(&self) -> Map<String, Value> {
        metadata(&self.method_name, &self.api_mode)
    }
}
