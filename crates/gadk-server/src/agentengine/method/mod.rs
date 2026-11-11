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

//! Go `server/agentengine/controllers/method`: `MethodHandler` and the six methods.

pub(crate) mod create_session;
pub(crate) mod delete_session;
pub(crate) mod get_session;
pub(crate) mod list_sessions;
pub(crate) mod stream_query;
pub(crate) mod streaming_agent_run_with_events;

use bytes::Bytes;
use gadk_session::SessionError;
use serde_json::{Map, Value};

use crate::mux::Response;

/// Go `MethodHandler`: one `class_method` of the Agent Engine API.
#[async_trait::async_trait]
pub(crate) trait MethodHandler: Send + Sync {
    /// Go `Name`: the `class_method` it serves.
    fn name(&self) -> &str;

    /// Go `Handle(ctx, rw, payload)`: `payload` is the whole (size-limited) request body.
    /// `Ok` is the response to send (a streaming body for the stream methods); `Err(text)` is a
    /// Go handler error, answered 500 `text` and logged `handleQuery failed: text` by the
    /// controller (only before anything is written, as in Go).
    async fn handle(&self, payload: Bytes) -> Result<Response, String>;

    /// Go `Metadata`: the map Go passes to `structpb.NewStruct` (`api_mode`, `name`,
    /// `parameters`, `description`).
    fn metadata(&self) -> Map<String, Value>;
}

/// Go `Metadata()`'s map: `api_mode`, `name`, `parameters` (a JSON schema) and `description`.
pub(crate) fn method_metadata(
    method_name: &str,
    api_mode: &str,
    parameters: Value,
    description: &str,
) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("api_mode".to_owned(), Value::String(api_mode.to_owned()));
    m.insert("name".to_owned(), Value::String(method_name.to_owned()));
    m.insert("parameters".to_owned(), parameters);
    m.insert(
        "description".to_owned(),
        Value::String(description.to_owned()),
    );
    m
}

/// A plain Go `json.NewEncoder(rw).Encode(v)` response of a session method: 200, the JSON
/// (`gadk_model::to_go_json`) plus `\n`, `Content-Type: text/plain; charset=utf-8` (Go's
/// sniffed type; no other headers). An encoding failure is Go's
/// `json.NewEncoder failed: <err>`.
pub(crate) fn encode_plain<T: serde::Serialize>(v: &T) -> Result<Response, String> {
    let mut body = gadk_model::to_go_json(v).map_err(|e| format!("json.NewEncoder failed: {e}"))?;
    body.push(b'\n');
    let mut resp = Response::new(axum::body::Body::from(body));
    resp.headers_mut().insert(
        http::header::CONTENT_TYPE,
        http::HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    Ok(resp)
}

/// A session service error as the method reports it: Go's in-memory service texts.
/// `NotFound` gains Go's `: "<session_id>"` suffix (the 5a REST rule) and `InvalidRequest` drops
/// gadk's `invalid request: ` prefix (Go's error is the bare text); any other error is its
/// `Display`.
pub(crate) fn session_error_text(err: &SessionError, session_id: &str) -> String {
    match err {
        SessionError::InvalidRequest(msg) => msg.clone(),
        other => crate::rest::controllers::handlers::session_error_text(other, session_id, false),
    }
}

/// Replays `testdata/agentengine` exchange fixtures (Appendix C C.11) through
/// [`new_handler`](crate::agentengine::new_handler) over the probe's scenario.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
pub(crate) mod replay {
    use std::sync::{Arc, Mutex};

    use axum::body::{Body, to_bytes};
    use gadk_agent::SingleLoader;
    use gadk_artifact::InMemoryArtifactService;
    use gadk_memory::InMemoryMemoryService;
    use gadk_platform::GoDuration;
    use gadk_session::InMemorySessionService;
    use http::{Method, Request};
    use pretty_assertions::assert_eq;
    use serde_json::Value;

    use crate::agentengine::{AgentEngineConfig, new_handler};
    use crate::mux::{Handler, WriteDeadline};
    use crate::rest::testutil::wire_scenario::{assistant_agent, fixed_platform};

    /// Response headers the fixtures leave out (server-owned).
    const SERVER_HEADERS: [&str; 3] = ["date", "content-length", "transfer-encoding"];

    /// One fixture configuration's handler (`main`: 4096 bytes, `small`: 32), with its log.
    pub(crate) struct Scenario {
        config: &'static str,
        handler: Handler,
        log: Arc<Mutex<String>>,
    }

    impl Scenario {
        /// Go `NewHandler(cfg, 0, <limit>, "test-engine")` on a fresh fixed platform; the
        /// startup log is discarded.
        pub(crate) fn new(config: &'static str) -> Scenario {
            let max_payload_size = match config {
                "main" => 4096,
                "small" => 32,
                other => panic!("unknown fixture configuration {other}"),
            };
            let platform = fixed_platform();
            let mut cfg = AgentEngineConfig::new(
                Arc::new(InMemorySessionService::with_platform(platform.clone())),
                Arc::new(SingleLoader::new(assistant_agent())),
            );
            cfg.memory_service = Some(Arc::new(InMemoryMemoryService::new()));
            cfg.artifact_service = Some(Arc::new(InMemoryArtifactService::with_platform(
                platform.clone(),
            )));
            cfg.platform = platform;
            let log: Arc<Mutex<String>> = Arc::default();
            let sink = Arc::clone(&log);
            cfg.log = Some(Arc::new(move |m: &str| {
                let mut s = sink.lock().unwrap();
                s.push_str(m);
                if !m.ends_with('\n') {
                    s.push('\n');
                }
            }));
            let handler = new_handler(cfg, GoDuration(0), max_payload_size, "test-engine").unwrap();
            log.lock().unwrap().clear();
            Scenario {
                config,
                handler,
                log,
            }
        }

        /// Sends fixture `name`'s request and asserts status, headers (without
        /// [`SERVER_HEADERS`]), body and log byte for byte (`Processing event: ` lines dropped
        /// on both sides).
        pub(crate) async fn replay(&self, name: &str) {
            let path = format!(
                "{}/testdata/agentengine/{name}.json",
                env!("CARGO_MANIFEST_DIR")
            );
            let fx: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            assert_eq!(fx["name"], name);
            assert_eq!(fx["config"], self.config, "{name}");
            assert_eq!(fx["kind"], "exchange", "{name}");
            assert_eq!(fx["compare"], "exact", "{name}");

            let request = &fx["request"];
            let mut req = Request::builder()
                .method(Method::from_bytes(request["method"].as_str().unwrap().as_bytes()).unwrap())
                .uri(request["target"].as_str().unwrap());
            for h in request["headers"].as_array().unwrap() {
                req = req.header(h[0].as_str().unwrap(), h[1].as_str().unwrap());
            }
            let body = request["body"].as_str().unwrap().to_owned();
            let mut req = req.body(Body::from(body)).unwrap();
            req.extensions_mut().insert(WriteDeadline::new(|_| {}));

            self.log.lock().unwrap().clear();
            let resp = (self.handler)(req).await;

            let want = &fx["response"];
            assert_eq!(
                u64::from(resp.status().as_u16()),
                want["status"].as_u64().unwrap(),
                "{name}: status"
            );
            let mut headers: Vec<(String, String)> = resp
                .headers()
                .iter()
                .filter(|(k, _)| !SERVER_HEADERS.contains(&k.as_str()))
                .map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap().to_owned()))
                .collect();
            headers.sort();
            let want_headers: Vec<(String, String)> = want["headers"]
                .as_array()
                .unwrap()
                .iter()
                .map(|h| {
                    (
                        h[0].as_str().unwrap().to_ascii_lowercase(),
                        h[1].as_str().unwrap().to_owned(),
                    )
                })
                .collect();
            assert_eq!(headers, want_headers, "{name}: headers");
            let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
            assert_eq!(
                String::from_utf8(body.to_vec()).unwrap(),
                want["body"].as_str().unwrap(),
                "{name}: body"
            );
            let log = drop_processing_lines(&self.log.lock().unwrap());
            assert_eq!(
                log,
                drop_processing_lines(fx["log"].as_str().unwrap()),
                "{name}: log"
            );
        }
    }

    /// The text without its `Processing event: ` lines (C.11).
    fn drop_processing_lines(text: &str) -> String {
        text.split_inclusive('\n')
            .filter(|l| !l.starts_with("Processing event: "))
            .collect()
    }

    /// Replays `names` in order on one fresh `config` handler.
    pub(crate) async fn replay_all(config: &'static str, names: &[&str]) {
        let scenario = Scenario::new(config);
        for name in names {
            scenario.replay(name).await;
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::collections::BTreeMap;

    use axum::body::to_bytes;
    use pretty_assertions::assert_eq;

    use super::*;

    #[tokio::test]
    async fn encode_plain_is_go_json_encoder_output() {
        let resp = encode_plain(&serde_json::json!({"b": "<&>", "a": 1.5})).unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let headers: Vec<(String, String)> = resp
            .headers()
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap().to_owned()))
            .collect();
        assert_eq!(
            headers,
            vec![(
                "content-type".to_owned(),
                "text/plain; charset=utf-8".to_owned()
            )]
        );
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], b"{\"a\":1.5,\"b\":\"\\u003c\\u0026\\u003e\"}\n");
    }

    #[test]
    fn encode_plain_failure_is_wrapped() {
        let mut bad = BTreeMap::new();
        bad.insert(vec![1u8], 1);
        assert_eq!(
            encode_plain(&bad).err().as_deref(),
            Some("json.NewEncoder failed: key must be a string")
        );
    }

    #[test]
    fn session_errors_are_go_in_memory_texts() {
        assert_eq!(
            session_error_text(&SessionError::NotFound, "nope"),
            "session not found: \"nope\""
        );
        assert_eq!(
            session_error_text(
                &SessionError::InvalidRequest("app_name is required".into()),
                "s"
            ),
            "app_name is required"
        );
        assert_eq!(
            session_error_text(&SessionError::Backend("boom".into()), "s"),
            "boom"
        );
    }
}
