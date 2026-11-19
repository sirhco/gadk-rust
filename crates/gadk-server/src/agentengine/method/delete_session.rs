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

//! The `async_delete_session` method.

use std::sync::Arc;

use bytes::Bytes;
use gadk_session::{DeleteRequest, SessionService};
use serde_json::{Map, Value};

use super::{MethodHandler, encode_plain, session_error_text};
use crate::agentengine::encode_shapes;
use crate::agentengine::models::{DeleteSessionRequest, DeleteSessionResponse};
use crate::agentengine::unmarshal::go_unmarshal;
use crate::mux::Response;

/// Handler for `async_delete_session`. Deletes the session; answers `{"output":""}`.
/// Errors: the input-decode error and the session-service error, each wrapped with its fixed
/// text (see the literals in this function).
pub(crate) struct DeleteSessionHandler {
    pub(crate) session_service: Arc<dyn SessionService>,
    /// The app name of every session call.
    pub(crate) agent_engine_id: String,
    pub(crate) method_name: String,
    pub(crate) api_mode: String,
}

impl DeleteSessionHandler {
    /// A handler serving `method_name` in `api_mode` over `session_service`.
    pub(crate) fn new(
        session_service: Arc<dyn SessionService>,
        agent_engine_id: &str,
        method_name: &str,
        api_mode: &str,
    ) -> Self {
        DeleteSessionHandler {
            session_service,
            agent_engine_id: agent_engine_id.to_owned(),
            method_name: method_name.to_owned(),
            api_mode: api_mode.to_owned(),
        }
    }
}

/// The method's `description` in its metadata.
const DESCRIPTION: &str = "Deletes a session for the given user.\n\nArgs:\n    user_id (str):\n        Required. The ID of the user.\n    session_id (str):\n        Required. The ID of the session.\n\nReturns:\n\ton success returns an empty string. On error returns an error message.\n\n";

/// The method's metadata map for `method_name` / `api_mode` (description text verbatim).
pub(crate) fn metadata(method_name: &str, api_mode: &str) -> Map<String, Value> {
    super::method_metadata(
        method_name,
        api_mode,
        serde_json::json!({
            "properties": {
                "session_id": {
                    "type": "string",
                },
                "user_id": {
                    "type": "string",
                },
            },
            "required": [
                "user_id",
                "session_id",
            ],
            "type": "object",
        }),
        DESCRIPTION,
    )
}

#[async_trait::async_trait]
impl MethodHandler for DeleteSessionHandler {
    fn name(&self) -> &str {
        &self.method_name
    }

    /// Decodes `models::DeleteSessionRequest` with `go_unmarshal`, calls the session service, and answers
    /// with `encode_plain`.
    async fn handle(&self, payload: Bytes) -> Result<Response, String> {
        let req: DeleteSessionRequest =
            go_unmarshal(&payload, &encode_shapes::MODELS_DELETE_SESSION_REQUEST)
                .map_err(|e| format!("failed to decode request: {e}"))?;
        let session_id = req.input.session_id;
        self.session_service
            .delete(DeleteRequest {
                app_name: self.agent_engine_id.clone(),
                user_id: req.input.user_id,
                session_id: session_id.clone(),
            })
            .await
            .map_err(|e| {
                format!(
                    "failed to delete session: {}",
                    session_error_text(&e, &session_id)
                )
            })?;
        encode_plain(&DeleteSessionResponse::default())
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

    fn handler() -> DeleteSessionHandler {
        DeleteSessionHandler::new(
            Arc::new(InMemorySessionService::new()),
            "test-engine",
            "async_delete_session",
            "async",
        )
    }

    #[tokio::test]
    async fn replays_the_delete_fixtures() {
        replay_all(
            "main",
            &[
                "main_02_create_session",
                "main_03_create_session_with_state",
                "main_07_delete_session",
                "main_08_delete_session_missing",
            ],
        )
        .await;
    }

    #[tokio::test]
    async fn decode_error_is_go_text() {
        let err = handler()
            .handle(Bytes::from(r#"{"input":{"user_id":"u","session_id":1.5}}"#))
            .await
            .err();
        assert_eq!(
            err.as_deref(),
            Some(
                "failed to decode request: invalid `DeleteSessionRequest.input.session_id`: expected a string, found a number"
            )
        );
    }

    #[tokio::test]
    async fn service_error_is_go_text() {
        let err = handler()
            .handle(Bytes::from(r#"{"input":{"session_id":"s"}}"#))
            .await
            .err();
        assert_eq!(
            err.as_deref(),
            Some(
                "failed to delete session: app_name, user_id, session_id are required, got app_name: \"test-engine\", user_id: \"\", session_id: \"s\""
            )
        );
    }
}
