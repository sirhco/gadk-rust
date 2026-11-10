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

//! Port of Go `controllers/triggers/pubsub.go`.

use std::sync::Arc;

use http::StatusCode;
use serde_json::{Map, Value};
use tokio::sync::Semaphore;

use super::{
    ControllerConfig, RetriableRunner, acquire, app_name, decode_first, read_body, respond_error,
    respond_success, semaphore, with_path_vars,
};
use crate::rest::ServerError;
use crate::rest::models::PubSubTriggerRequest;
use crate::rest::routing::{Request, Response};

/// Go `pubSubDefaultUserID`: the user of a delivery without a subscription.
const PUB_SUB_DEFAULT_USER_ID: &str = "pubsub-caller";

/// Port of Go `triggers.PubSubController`: handles Pub/Sub push deliveries.
pub struct PubSubController {
    runner: RetriableRunner,
    semaphore: Option<Semaphore>,
}

impl PubSubController {
    /// Go `NewPubSubControllerWithConfig`: fails when the compaction config cannot serve the
    /// loader's apps (the error text is Go's). A non-positive `max_concurrent_runs` means no
    /// limit.
    pub fn new(cfg: ControllerConfig) -> Result<Self, ServerError> {
        let max_concurrent_runs = cfg.trigger_config.max_concurrent_runs;
        let runner = RetriableRunner::new(cfg);
        runner
            .validate_compaction()
            .map_err(|e| ServerError(e.to_string()))?;
        Ok(Self {
            runner,
            semaphore: semaphore(max_concurrent_runs),
        })
    }

    /// Go `PubSubTriggerHandler`: decodes the push envelope, runs the agent of the `app_name`
    /// path variable with the message as JSON text (user: the subscription, or `pubsub-caller`)
    /// and answers `{"status":"success"}` or `{"status":"<error>"}`.
    pub async fn pub_sub_trigger_handler(&self, req: Request) -> Response {
        let app = app_name(&req);
        let body = match read_body(req).await {
            Ok(body) => body,
            Err(e) => {
                return respond_error(
                    StatusCode::BAD_REQUEST,
                    &format!("failed to decode request: {e}"),
                );
            }
        };
        let envelope: PubSubTriggerRequest = match decode_first(&body) {
            Ok(envelope) => envelope,
            Err(e) => {
                return respond_error(
                    StatusCode::BAD_REQUEST,
                    &format!("failed to decode request: {e}"),
                );
            }
        };

        let agent_message = match message_content_from_pub_sub(&envelope) {
            Ok(message) => message,
            Err(e) => {
                return respond_error(
                    StatusCode::BAD_REQUEST,
                    &format!("failed to retrieve message content: {e}"),
                );
            }
        };

        let app = match app {
            Ok(app) => app,
            Err(e) => {
                return respond_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &format!("failed to retrieve app name: {e}"),
                );
            }
        };

        let user_id = if envelope.subscription.is_empty() {
            PUB_SUB_DEFAULT_USER_ID
        } else {
            envelope.subscription.as_str()
        };

        let _permit = acquire(self.semaphore.as_ref()).await;

        if let Err(e) = self.runner.run_agent(&app, user_id, &agent_message).await {
            return respond_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("failed to run agent: {e}"),
            );
        }

        respond_success()
    }

    /// The trigger endpoint (POST) as an axum `MethodRouter` to mount at a path of the caller's
    /// choosing with an `{app_name}` parameter (the Go launcher uses
    /// `/apps/{app_name}/trigger/pubsub`). Other methods answer axum's 405.
    pub fn handler(self: Arc<Self>) -> axum::routing::MethodRouter {
        axum::routing::post(move |req: Request| async move {
            self.pub_sub_trigger_handler(with_path_vars(req).await)
                .await
        })
    }
}

/// Go `messageContentFromPubSub`: the agent's input, Go's `json.Marshal` of
/// `{"attributes": …, "data": "<decoded data>"}` with empty members left out.
pub(super) fn message_content_from_pub_sub(req: &PubSubTriggerRequest) -> Result<String, String> {
    let mut content = Map::new();
    if !req.message.data.is_empty() {
        content.insert(
            "data".to_owned(),
            Value::String(String::from_utf8_lossy(&req.message.data).into_owned()),
        );
    }
    if let Some(attributes) = req.message.attributes.as_ref().filter(|a| !a.is_empty()) {
        let attributes = attributes
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect();
        content.insert("attributes".to_owned(), Value::Object(attributes));
    }

    if content.is_empty() {
        return Err("empty message data and attributes".to_owned());
    }

    gadk_model::to_go_json(&content)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .map_err(|e| format!("failed to marshal agent message: {e}"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::collections::BTreeMap;

    use bytes::Bytes;

    use super::*;
    use crate::rest::models::PubSubMessage;

    fn request(data: &[u8], attributes: Option<&[(&str, &str)]>) -> PubSubTriggerRequest {
        PubSubTriggerRequest {
            message: PubSubMessage {
                data: Bytes::copy_from_slice(data),
                attributes: attributes.map(|a| {
                    a.iter()
                        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                        .collect::<BTreeMap<_, _>>()
                }),
                ..Default::default()
            },
            subscription: String::new(),
        }
    }

    /// Bytes of Go's `json.Marshal(map[string]any{...})`: keys sorted, HTML escaped.
    #[test]
    fn message_content_is_gos_json() {
        assert_eq!(
            message_content_from_pub_sub(&request(b"a<b", Some(&[("k", "v&")]))).unwrap(),
            r#"{"attributes":{"k":"v\u0026"},"data":"a\u003cb"}"#
        );
        assert_eq!(
            message_content_from_pub_sub(&request(b"hi", Some(&[]))).unwrap(),
            r#"{"data":"hi"}"#
        );
        assert_eq!(
            message_content_from_pub_sub(&request(b"", None)).unwrap_err(),
            "empty message data and attributes"
        );
    }
}
