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
use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::value::RawValue;
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
    ///
    /// The body is read under the router's `axum::extract::DefaultBodyLimit`; set it explicitly
    /// where you mount the handler (`DefaultBodyLimit::max(10 << 20)` is the Go launcher's
    /// 10 MiB), since axum's own default is 2 MB.
    ///
    /// The handler does not authenticate: it runs the agent as a user ID taken from the
    /// delivery itself, so any caller that reaches it chooses who the agent runs as. Mount it
    /// behind [`authn::middleware`](crate::authn::middleware) as the Go launcher does, normally
    /// with [`google_oidc`](crate::authn::google_oidc) so that only an allow-listed service
    /// account may call it:
    ///
    /// ```no_run
    /// # use std::sync::Arc;
    /// # use gadk_server::authn::{self, AuthnError, GoogleOidcConfig};
    /// # use gadk_server::rest::triggers::PubSubController;
    /// # fn mount(ctrl: Arc<PubSubController>) -> Result<axum::Router, AuthnError> {
    /// let auth = authn::google_oidc(GoogleOidcConfig {
    ///     audience: "https://my-agent.example.com".into(),
    ///     allowed_service_accounts: vec!["trigger@my-project.iam.gserviceaccount.com".into()],
    /// })?;
    /// let app = axum::Router::new()
    ///     .route(
    ///         "/apps/{app_name}/trigger/pubsub",
    ///         ctrl.handler().layer(authn::middleware(auth)),
    ///     )
    ///     .layer(axum::extract::DefaultBodyLimit::max(10 << 20));
    /// # Ok(app)
    /// # }
    /// ```
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
    /// The marshaled map: fields in Go's sorted key order.
    #[derive(Serialize)]
    struct MessageContent<'a> {
        #[serde(skip_serializing_if = "Option::is_none")]
        attributes: Option<&'a BTreeMap<String, String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        data: Option<Box<RawValue>>,
    }

    let data = if req.message.data.is_empty() {
        None
    } else {
        Some(go_json_string(&req.message.data)?)
    };
    let attributes = req.message.attributes.as_ref().filter(|a| !a.is_empty());
    if data.is_none() && attributes.is_none() {
        return Err("empty message data and attributes".to_owned());
    }

    gadk_model::to_go_json(&MessageContent { attributes, data })
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .map_err(|e| format!("failed to marshal agent message: {e}"))
}

/// Go's `json.Marshal(string(data))` for arbitrary bytes: valid UTF-8 runs escaped as Go's
/// `encoding/json` escapes them, and `\ufffd` once per byte of each invalid sequence.
fn go_json_string(data: &[u8]) -> Result<Box<RawValue>, String> {
    let marshal_err = |e: serde_json::Error| format!("failed to marshal agent message: {e}");
    let mut out = String::from("\"");
    for chunk in data.utf8_chunks() {
        let valid = gadk_model::to_go_json(chunk.valid()).map_err(marshal_err)?;
        let valid = String::from_utf8_lossy(&valid);
        out.push_str(&valid[1..valid.len() - 1]);
        for _ in chunk.invalid() {
            out.push_str("\\ufffd");
        }
    }
    out.push('"');
    RawValue::from_string(out).map_err(marshal_err)
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

    /// Invalid UTF-8 in `data` is Go's `json.Marshal(string(data))`: `\ufffd` per invalid
    /// byte, valid runs (multibyte, `"`, HTML-significant, U+2028) escaped as Go escapes them.
    /// Expected bytes from a go1.27.2 `json.Marshal(map[string]any{...})` program.
    #[test]
    fn message_content_escapes_invalid_utf8_per_byte_as_go() {
        assert_eq!(
            message_content_from_pub_sub(&request(&[0x61, 0xff, 0xfe, 0x62], Some(&[("k", "v")])))
                .unwrap(),
            r#"{"attributes":{"k":"v"},"data":"a\ufffd\ufffdb"}"#
        );
        let mut mixed = "<\"é".as_bytes().to_vec();
        mixed.extend_from_slice(&[0xff, 0xe2, 0x82]);
        mixed.extend_from_slice(">&\u{2028}".as_bytes());
        assert_eq!(
            message_content_from_pub_sub(&request(&mixed, Some(&[("k", "v")]))).unwrap(),
            r#"{"attributes":{"k":"v"},"data":"\u003c\"é\ufffd\ufffd\ufffd\u003e\u0026\u2028"}"#
        );
    }
}
