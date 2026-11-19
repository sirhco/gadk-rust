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

//! The `async_get_session` method.

use std::sync::Arc;

use bytes::Bytes;
use gadk_session::{GetRequest, SessionService};
use serde_json::{Map, Value};

use super::{MethodHandler, encode_plain, session_error_text};
use crate::agentengine::encode_shapes;
use crate::agentengine::models::{GetSessionRequest, GetSessionResponse, from_session};
use crate::agentengine::unmarshal::go_unmarshal;
use crate::mux::Response;

/// Handler for `async_get_session`. Gets the session; answers `{"output":SessionData}`.
/// Errors: the input-decode error and the session-service error, each wrapped with its fixed
/// text (see the literals in this function).
pub(crate) struct GetSessionHandler {
    pub(crate) session_service: Arc<dyn SessionService>,
    /// The app name of every session call.
    pub(crate) agent_engine_id: String,
    pub(crate) method_name: String,
    pub(crate) api_mode: String,
}

impl GetSessionHandler {
    /// A handler serving `method_name` in `api_mode` over `session_service`.
    pub(crate) fn new(
        session_service: Arc<dyn SessionService>,
        agent_engine_id: &str,
        method_name: &str,
        api_mode: &str,
    ) -> Self {
        GetSessionHandler {
            session_service,
            agent_engine_id: agent_engine_id.to_owned(),
            method_name: method_name.to_owned(),
            api_mode: api_mode.to_owned(),
        }
    }
}

/// The method's `description` in its metadata.
const DESCRIPTION: &str = "Get a session for the given user.\n\nArgs:\n    user_id (str):\n        Required. The ID of the user.\n    session_id (str):\n        Required. The ID of the session.\n\nReturns:\n    Session: The session instance (if any). Returns an error on failure\n\n";

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
impl MethodHandler for GetSessionHandler {
    fn name(&self) -> &str {
        &self.method_name
    }

    /// Decodes `models::GetSessionRequest` with `go_unmarshal`, calls the session service, and answers
    /// with `encode_plain`.
    async fn handle(&self, payload: Bytes) -> Result<Response, String> {
        let req: GetSessionRequest =
            go_unmarshal(&payload, &encode_shapes::MODELS_GET_SESSION_REQUEST)
                .map_err(|e| format!("failed to decode request: {e}"))?;
        let session_id = req.input.session_id;
        let sess = self
            .session_service
            .get(GetRequest {
                app_name: self.agent_engine_id.clone(),
                user_id: req.input.user_id,
                session_id: session_id.clone(),
                num_recent_events: 0,
                after: None,
            })
            .await
            .map_err(|e| {
                format!(
                    "failed to get session: {}",
                    session_error_text(&e, &session_id)
                )
            })?;
        encode_plain(&GetSessionResponse {
            output: from_session(&*sess),
        })
    }

    fn metadata(&self) -> Map<String, Value> {
        metadata(&self.method_name, &self.api_mode)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use axum::body::to_bytes;
    use chrono::{DateTime, Utc};
    use gadk_platform::Platform;
    use gadk_session::{CreateRequest, Event, InMemorySessionService};
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::agentengine::method::replay::replay_all;

    fn handler(service: Arc<dyn SessionService>) -> GetSessionHandler {
        GetSessionHandler::new(service, "test-engine", "async_get_session", "async")
    }

    #[tokio::test]
    async fn replays_the_get_fixtures() {
        replay_all(
            "main",
            &[
                "main_02_create_session",
                "main_03_create_session_with_state",
                "main_04_get_session",
                "main_05_get_session_missing",
            ],
        )
        .await;
    }

    #[tokio::test]
    async fn decode_error_is_go_text() {
        let err = handler(Arc::new(InMemorySessionService::new()))
            .handle(Bytes::from(
                r#"{"class_method":"async_get_session","input":{"session_id":["s"]}}"#,
            ))
            .await
            .err();
        assert_eq!(
            err.as_deref(),
            Some(
                "failed to decode request: invalid `GetSessionRequest.input.session_id`: expected a string, found an array"
            )
        );
    }

    #[tokio::test]
    async fn service_errors_are_go_texts() {
        let h = handler(Arc::new(InMemorySessionService::new()));
        let err = h
            .handle(Bytes::from(
                r#"{"class_method":"async_get_session","input":{"user_id":"u"}}"#,
            ))
            .await
            .err();
        assert_eq!(
            err.as_deref(),
            Some(
                "failed to get session: app_name, user_id, session_id are required, got app_name: \"test-engine\", user_id: \"u\", session_id: \"\""
            )
        );
        let err = h
            .handle(Bytes::from(
                r#"{"input":{"user_id":"u","session_id":"a\"b"}}"#,
            ))
            .await
            .err();
        assert_eq!(
            err.as_deref(),
            Some(r#"failed to get session: session not found: "a\"b""#)
        );
    }

    /// Go `FromSession`: every state key, every event as Go's `session.Event` JSON, and
    /// `last_update_time` as `float64(UnixNano()) / 1e9` (fractional seconds kept).
    #[tokio::test]
    async fn answers_the_session_as_go_from_session() {
        let at = DateTime::<Utc>::from_timestamp(1_767_323_045, 500_000_000).unwrap();
        let platform = Platform::default().with_time(move || at);
        let service = Arc::new(InMemorySessionService::with_platform(platform.clone()));
        let state = serde_json::json!({"b": [1, 2.5], "a": {"z": null, "y": "<&>"}});
        let sess = service
            .create(CreateRequest {
                app_name: "test-engine".into(),
                user_id: "u".into(),
                session_id: Some("s".into()),
                state: state.as_object().cloned(),
            })
            .await
            .unwrap();
        let mut event = Event::new(&platform, "inv-1");
        event.author = "user".into();
        event.response.content = Some(gadk_genai::Content::from_text("hi", "user"));
        service.append_event(&*sess, &mut event).await.unwrap();
        let events = service
            .get(gadk_session::GetRequest {
                app_name: "test-engine".into(),
                user_id: "u".into(),
                session_id: "s".into(),
                ..Default::default()
            })
            .await
            .unwrap()
            .events();
        assert_eq!(events.len(), 1);

        let resp = handler(service)
            .handle(Bytes::from(
                r#"{"class_method":"async_get_session","input":{"user_id":"u","session_id":"s"}}"#,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let events_json = String::from_utf8(gadk_model::to_go_json(&events).unwrap()).unwrap();
        assert_eq!(
            String::from_utf8(body.to_vec()).unwrap(),
            format!(
                r#"{{"output":{{"user_id":"u","last_update_time":1767323045.5,"app_name":"test-engine","id":"s","state":{{"a":{{"y":"\u003c\u0026\u003e","z":null}},"b":[1,2.5]}},"events":{events_json}}}}}"#
            ) + "\n"
        );
    }
}
