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

//! Port of Go `controllers/triggers/eventarc.go`.

use std::sync::Arc;

use axum::body::Bytes;
use http::StatusCode;
use http::header::CONTENT_TYPE;
use serde::de::IgnoredAny;
use tokio::sync::Semaphore;

use super::pubsub::message_content_from_pub_sub;
use super::{
    BodyError, ControllerConfig, RetriableRunner, acquire, app_name, decode_first, read_body,
    respond_error, respond_success, semaphore, unmarshal, with_path_vars,
};
use crate::rest::ServerError;
use crate::rest::models::{EventarcTriggerRequest, PubSubTriggerRequest};
use crate::rest::routing::{Request, Response};

/// Go `eventArcDefaultUserID`: the user of an event without a source.
const EVENTARC_DEFAULT_USER_ID: &str = "eventarc-caller";

/// The structured-mode media type (CloudEvents HTTP binding §3.2.1).
const STRUCTURED_CONTENT_TYPE: &str = "application/cloudevents+json";

/// The CloudEvents type of a Pub/Sub-sourced event, whose data is a push envelope.
const PUB_SUB_EVENT_TYPE: &str = "google.cloud.pubsub.topic.v1.messagePublished";

/// Port of Go `triggers.EventarcController`: handles Eventarc (CloudEvents) deliveries.
pub struct EventarcController {
    runner: RetriableRunner,
    semaphore: Option<Semaphore>,
}

impl EventarcController {
    /// Go `NewEventarcControllerWithConfig`: fails when the compaction config cannot serve the
    /// loader's apps (the error text is Go's). A non-positive `max_concurrent_runs` means no
    /// limit (Go's Eventarc controller would block every request on a zero-capacity channel).
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

    /// Go `EventarcTriggerHandler`: a CloudEvent in structured mode (`Content-Type:
    /// application/cloudevents+json`, the event is the body) or binary mode (attributes in `ce-*`
    /// headers, the body is the data). A Pub/Sub event's data is decoded as a push envelope;
    /// any other event is passed to the agent whole, as Go's `json.Marshal` writes it (user: the
    /// event source, or `eventarc-caller`).
    pub async fn eventarc_trigger_handler(&self, req: Request) -> Response {
        let structured = header(&req, CONTENT_TYPE.as_str()) == STRUCTURED_CONTENT_TYPE;
        let app = app_name(&req);

        let (event, data) = if structured {
            // The whole event is the body; its data stays raw JSON text.
            let decoded = match read_body(req).await {
                Ok(body) => decode_first::<EventarcTriggerRequest>(&body),
                Err(e) => Err(e.to_string()),
            };
            let mut event = match decoded {
                Ok(event) => event,
                Err(e) => {
                    return respond_error(
                        StatusCode::BAD_REQUEST,
                        &format!("failed to unmarshal eventarc request: {e}"),
                    );
                }
            };
            let data = event
                .data
                .take()
                .map(|raw| Bytes::copy_from_slice(raw.get().as_bytes()))
                .unwrap_or_default();
            (event, data)
        } else {
            // Metadata in the headers; the whole body is the data.
            let event = EventarcTriggerRequest {
                id: header(&req, "ce-id"),
                type_: header(&req, "ce-type"),
                source: header(&req, "ce-source"),
                spec_version: header(&req, "ce-specversion"),
                time: header(&req, "ce-time"),
                data: None,
            };
            match read_body(req).await {
                Ok(body) => (event, body),
                // A body over the limit is a client error, as in the structured branch.
                Err(e @ BodyError::TooLarge) => {
                    return respond_error(
                        StatusCode::BAD_REQUEST,
                        &format!("failed to read body: {e}"),
                    );
                }
                Err(e) => {
                    return respond_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &format!("failed to read body: {e}"),
                    );
                }
            }
        };

        let message_content = if event.type_ == PUB_SUB_EVENT_TYPE {
            let envelope: PubSubTriggerRequest = match unmarshal(&data) {
                Ok(envelope) => envelope,
                Err(e) => {
                    return respond_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &format!("failed to unmarshal pubsub data: {e}"),
                    );
                }
            };
            match message_content_from_pub_sub(&envelope) {
                Ok(content) => content,
                Err(e) => {
                    return respond_error(
                        StatusCode::BAD_REQUEST,
                        &format!("failed to retrieve message content: {e}"),
                    );
                }
            }
        } else {
            match marshal_event(&event, &data) {
                Ok(content) => content,
                Err(e) => {
                    return respond_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &format!("failed to marshal agent message: {e}"),
                    );
                }
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

        let user_id = if event.source.is_empty() {
            EVENTARC_DEFAULT_USER_ID
        } else {
            event.source.as_str()
        };

        let _permit = acquire(self.semaphore.as_ref()).await;

        if let Err(e) = self.runner.run_agent(&app, user_id, &message_content).await {
            return respond_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("failed to run agent: {e}"),
            );
        }

        respond_success()
    }

    /// The trigger endpoint (POST) as an axum `MethodRouter` to mount at a path of the caller's
    /// choosing with an `{app_name}` parameter (the Go launcher uses
    /// `/apps/{app_name}/trigger/eventarc`). Other methods answer axum's 405.
    ///
    /// The body is read under the router's `axum::extract::DefaultBodyLimit`; set it explicitly
    /// where you mount the handler (`DefaultBodyLimit::max(10 << 20)` is the Go launcher's
    /// 10 MiB), since axum's own default is 2 MB.
    pub fn handler(self: Arc<Self>) -> axum::routing::MethodRouter {
        axum::routing::post(move |req: Request| async move {
            self.eventarc_trigger_handler(with_path_vars(req).await)
                .await
        })
    }
}

/// Go `r.Header.Get(name)`: the first value, or empty.
fn header(req: &Request, name: &str) -> String {
    req.headers()
        .get(name)
        .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
        .unwrap_or_default()
}

/// Go `json.Marshal(event)` with `event.Data = data`: the attributes, then `data` (Go
/// `json.RawMessage`, `omitempty`) compacted and HTML-escaped as Go's encoder does. Data that is
/// not JSON fails as Go's `RawMessage` marshalling does.
fn marshal_event(event: &EventarcTriggerRequest, data: &[u8]) -> Result<String, String> {
    let mut out = gadk_model::to_go_json(event).map_err(|e| e.to_string())?;
    if !data.is_empty() {
        serde_json::from_slice::<IgnoredAny>(data).map_err(|e| {
            format!("json: error calling MarshalJSON for type json.RawMessage: {e}")
        })?;
        out.pop(); // the closing brace; `data` is the last member
        out.extend_from_slice(b",\"data\":");
        go_compact(data, &mut out);
        out.push(b'}');
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// Go `encoding/json` `appendCompact(dst, src, escapeHTML=true)` over valid JSON: whitespace
/// outside strings dropped; `<`, `>`, `&`, U+2028 and U+2029 written as `\u` escapes.
fn go_compact(src: &[u8], out: &mut Vec<u8>) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let (mut in_string, mut escaped) = (false, false);
    let mut i = 0;
    while i < src.len() {
        let c = src[i];
        if matches!(c, b'<' | b'>' | b'&') {
            out.extend_from_slice(b"\\u00");
            out.push(HEX[usize::from(c >> 4)]);
            out.push(HEX[usize::from(c & 0xF)]);
            i += 1;
            continue;
        }
        if c == 0xE2 && i + 2 < src.len() && src[i + 1] == 0x80 && src[i + 2] & !1 == 0xA8 {
            out.extend_from_slice(b"\\u202");
            out.push(HEX[usize::from(src[i + 2] & 0xF)]);
            i += 3;
            continue;
        }
        if in_string {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_string = false;
            }
        } else if matches!(c, b' ' | b'\t' | b'\n' | b'\r') {
            i += 1;
            continue;
        } else if c == b'"' {
            in_string = true;
        }
        out.push(c);
        i += 1;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn compact(src: &str) -> String {
        let mut out = Vec::new();
        go_compact(src.as_bytes(), &mut out);
        String::from_utf8(out).unwrap()
    }

    /// Go 1.27 `json.Marshal(json.RawMessage(src))` outputs.
    #[test]
    fn go_compact_matches_go() {
        assert_eq!(
            compact("{\n  \"a\" : [1, 2.50],\t\"b\": \"x y\\\" <&>\"\r\n}"),
            r#"{"a":[1,2.50],"b":"x y\" \u003c\u0026\u003e"}"#
        );
        assert_eq!(compact("\"\u{2028}\u{2029}\""), r#""\u2028\u2029""#);
        assert_eq!(compact(" null "), "null");
    }

    #[test]
    fn marshal_event_appends_compacted_data_last() {
        let event = EventarcTriggerRequest {
            id: "1".into(),
            type_: "t".into(),
            ..Default::default()
        };
        assert_eq!(
            marshal_event(&event, b"").unwrap(),
            r#"{"id":"1","source":"","type":"t","specversion":""}"#
        );
        assert_eq!(
            marshal_event(&event, b"{ \"k\": \"v\" }").unwrap(),
            r#"{"id":"1","source":"","type":"t","specversion":"","data":{"k":"v"}}"#
        );
        assert!(
            marshal_event(&event, b"not json")
                .unwrap_err()
                .starts_with("json: error calling MarshalJSON for type json.RawMessage: ")
        );
    }
}
