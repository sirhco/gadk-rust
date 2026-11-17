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

//! The `async_list_sessions` method.

use std::sync::Arc;

use bytes::Bytes;
use gadk_session::{ListRequest, SessionService};
use serde_json::{Map, Value};

use super::{MethodHandler, encode_plain, session_error_text};
use crate::agentengine::encode_shapes;
use crate::agentengine::models::{ListSessionRequest, ListSessionResponse, Sessions, from_session};
use crate::agentengine::unmarshal::go_unmarshal;
use crate::mux::Response;

/// Handler for `async_list_sessions`. Lists the user's sessions; answers
/// `{"output":{"sessions":[SessionData…]}}`.
/// Errors: `json.Unmarshal() failed: <err>`, `c.sessionservice.List() failed: <err>`.
pub(crate) struct ListSessionsHandler {
    pub(crate) session_service: Arc<dyn SessionService>,
    /// The app name of every session call.
    pub(crate) agent_engine_id: String,
    pub(crate) method_name: String,
    pub(crate) api_mode: String,
}

impl ListSessionsHandler {
    /// A handler serving `method_name` in `api_mode` over `session_service`.
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

/// The method's `description` in its metadata.
const DESCRIPTION: &str = "List sessions for the given user.\n\nArgs:\n    user_id (str):\n        Required. The ID of the user.\n\nReturns:\n    ListSessionsResponse: The list of sessions with data.\n";

/// The method's metadata map for `method_name` / `api_mode` (description text verbatim).
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
        let req: ListSessionRequest =
            go_unmarshal(&payload, &encode_shapes::MODELS_LIST_SESSION_REQUEST)
                .map_err(|e| format!("json.Unmarshal() failed: {e}"))?;
        let sessions = self
            .session_service
            .list(ListRequest {
                app_name: self.agent_engine_id.clone(),
                user_id: req.input.user_id,
            })
            .await
            .map_err(|e| {
                format!(
                    "c.sessionservice.List() failed: {}",
                    session_error_text(&e, "")
                )
            })?;
        encode_plain(&ListSessionResponse {
            output: Sessions {
                sessions: sessions.iter().map(|s| from_session(&**s)).collect(),
            },
        })
    }

    fn metadata(&self) -> Map<String, Value> {
        metadata(&self.method_name, &self.api_mode)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use gadk_session::InMemorySessionService;
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::agentengine::method::replay::replay_all;

    #[tokio::test]
    async fn replays_the_session_fixtures_in_order() {
        replay_all(
            "main",
            &[
                "main_02_create_session",
                "main_03_create_session_with_state",
                "main_04_get_session",
                "main_05_get_session_missing",
                "main_06_list_sessions",
                "main_07_delete_session",
                "main_08_delete_session_missing",
                "main_22_uppercase_keys",
                "main_27_reasoning_engine_query",
            ],
        )
        .await;
    }

    #[tokio::test]
    async fn decode_error_is_go_text() {
        let err = ListSessionsHandler::new(
            Arc::new(InMemorySessionService::new()),
            "test-engine",
            "async_list_sessions",
            "async",
        )
        .handle(Bytes::from(r#"{"input":{"user_id":true}}"#))
        .await
        .err();
        assert_eq!(
            err.as_deref(),
            Some(
                "json.Unmarshal() failed: json: cannot unmarshal bool into Go struct field ListSessionRequest.input.user_id of type string"
            )
        );
    }

    #[tokio::test]
    async fn service_error_is_go_text() {
        let err = ListSessionsHandler::new(
            Arc::new(InMemorySessionService::new()),
            "",
            "async_list_sessions",
            "async",
        )
        .handle(Bytes::from(r#"{"input":{"user_id":"u"}}"#))
        .await
        .err();
        assert_eq!(
            err.as_deref(),
            Some("c.sessionservice.List() failed: app_name is required, got app_name: \"\"")
        );
    }
}
