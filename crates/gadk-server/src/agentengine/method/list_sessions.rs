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

//! Go `server/agentengine/controllers/method/list_sessions.go` (`async_list_sessions`).

use std::sync::Arc;

use bytes::Bytes;
use gadk_session::SessionService;
use serde_json::{Map, Value};

use super::MethodHandler;
use crate::mux::Response;

/// Go `NewListSessionHandler`'s handler. Lists the user's sessions; answers `{"output":{"sessions":[SessionData…]}}`. Errors: `json.Unmarshal() failed: <err>`, `c.sessionservice.List() failed: <err>`.
pub(crate) struct ListSessionsHandler {
    pub(crate) session_service: Arc<dyn SessionService>,
    /// Go `agentEngineID`: the app name of every session call.
    pub(crate) agent_engine_id: String,
    pub(crate) method_name: String,
    pub(crate) api_mode: String,
}

impl ListSessionsHandler {
    /// Go `NewListSessionHandler(sessionService, agentEngineID, methodName, apiMode)`.
    pub(crate) fn new(
        session_service: Arc<dyn SessionService>,
        agent_engine_id: &str,
        method_name: &str,
        api_mode: &str,
    ) -> Self {
        ListSessionsHandler {
            session_service,
            agent_engine_id: agent_engine_id.to_owned(),
            method_name: method_name.to_owned(),
            api_mode: api_mode.to_owned(),
        }
    }
}

/// Go `Metadata()`'s `description`.
const DESCRIPTION: &str = "List sessions for the given user.\n\nArgs:\n    user_id (str):\n        Required. The ID of the user.\n\nReturns:\n    ListSessionsResponse: The list of sessions with data.\n";

/// Go `Metadata()`'s map for `method_name` / `api_mode` (description text verbatim).
pub(crate) fn metadata(method_name: &str, api_mode: &str) -> Map<String, Value> {
    super::method_metadata(
        method_name,
        api_mode,
        serde_json::json!({
            "properties": {
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
impl MethodHandler for ListSessionsHandler {
    fn name(&self) -> &str {
        &self.method_name
    }

    /// Decodes `models::ListSessionRequest` with `go_unmarshal`, calls the session service, and answers
    /// with `encode_plain`.
    async fn handle(&self, payload: Bytes) -> Result<Response, String> {
        todo!("Task 7")
    }

    fn metadata(&self) -> Map<String, Value> {
        metadata(&self.method_name, &self.api_mode)
    }
}
