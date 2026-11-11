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

//! Go `server/agentengine/controllers/method/create_session.go` (`async_create_session`).

use std::sync::Arc;

use bytes::Bytes;
use gadk_session::SessionService;
use serde_json::{Map, Value};

use super::MethodHandler;
use crate::mux::Response;

/// Go `NewCreateSessionHandler`'s handler. Creates a session for user_id with the optional initial state (Go `sessionService.Create` with app name = the agent engine ID); answers `{"output":SessionData}`. Errors: `json.Unmarshal() failed: <err>`, `c.sessionservice.Create() failed: <err>`.
pub(crate) struct CreateSessionHandler {
    pub(crate) session_service: Arc<dyn SessionService>,
    /// Go `agentEngineID`: the app name of every session call.
    pub(crate) agent_engine_id: String,
    pub(crate) method_name: String,
    pub(crate) api_mode: String,
}

impl CreateSessionHandler {
    /// Go `NewCreateSessionHandler(sessionService, agentEngineID, methodName, apiMode)`.
    pub(crate) fn new(
        session_service: Arc<dyn SessionService>,
        agent_engine_id: &str,
        method_name: &str,
        api_mode: &str,
    ) -> Self {
        CreateSessionHandler {
            session_service,
            agent_engine_id: agent_engine_id.to_owned(),
            method_name: method_name.to_owned(),
            api_mode: api_mode.to_owned(),
        }
    }
}

/// Go `Metadata()`'s `description`.
const DESCRIPTION: &str = "Creates a new session.\n\t\t\nArgs:\n    user_id (str):\n\t        Required. The ID of the user.\n    state (dict[str, Any]):\n        Optional. The initial state of the session.\n\nReturns:\n    Session: The newly created session instance.\n";

/// Go `Metadata()`'s map for `method_name` / `api_mode` (description text verbatim).
pub(crate) fn metadata(method_name: &str, api_mode: &str) -> Map<String, Value> {
    super::method_metadata(
        method_name,
        api_mode,
        serde_json::json!({
            "properties": {
                "state": {
                    "nullable": true,
                    "type": "object",
                },
                "user_id": {
                    "type": "string",
                },
            },
            "required": [
                "user_id",
            ],
            "type": "object",
        }),
        DESCRIPTION,
    )
}

#[async_trait::async_trait]
impl MethodHandler for CreateSessionHandler {
    fn name(&self) -> &str {
        &self.method_name
    }

    /// Decodes `models::CreateSessionRequest` with `go_unmarshal`, calls the session service, and answers
    /// with `encode_plain`.
    async fn handle(&self, payload: Bytes) -> Result<Response, String> {
        todo!("Task 7")
    }

    fn metadata(&self) -> Map<String, Value> {
        metadata(&self.method_name, &self.api_mode)
    }
}
