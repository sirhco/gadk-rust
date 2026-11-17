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

//! [`MethodHandler`] and the six Agent Engine methods.

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

/// One `class_method` of the Agent Engine API.
#[async_trait::async_trait]
pub(crate) trait MethodHandler: Send + Sync {
    /// The `class_method` it serves.
    fn name(&self) -> &str;

    /// Handles one query: `payload` is the whole (size-limited) request body.
    /// `Ok` is the response to send (a streaming body for the stream methods); `Err(text)` is a
    /// handler error, answered 500 `text` and logged `handleQuery failed: text` by the
    /// controller (only before anything is written).
    async fn handle(&self, payload: Bytes) -> Result<Response, String>;

    /// The method's metadata map (`api_mode`, `name`, `parameters`, `description`).
    fn metadata(&self) -> Map<String, Value>;
}

/// A method's metadata map: `api_mode`, `name`, `parameters` (a JSON schema) and `description`.
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

/// A plain JSON response of a session method: 200, the JSON (`gadk_model::to_go_json`) plus
/// `\n`, `Content-Type: text/plain; charset=utf-8` (the type a sniffing server would pick; no
/// other headers). An encoding failure is the `Err` of the encoder's error text.
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

/// A session service error as the method reports it, using the in-memory service's texts.
/// `NotFound` gains a `: "<session_id>"` suffix (as in the REST server) and `InvalidRequest`
/// drops gadk's `invalid request: ` prefix (the bare text is reported); any other error is its
/// `Display`.
pub(crate) fn session_error_text(err: &SessionError, session_id: &str) -> String {
    match err {
        SessionError::InvalidRequest(msg) => msg.clone(),
        other => crate::rest::controllers::handlers::session_error_text(other, session_id, false),
    }
}

/// Replays `testdata/agentengine` fixtures (Appendix C C.11) through
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
    use gadk_session::{GetRequest, InMemorySessionService, SessionService};
    use http::{Method, Request};
    use pretty_assertions::assert_eq;
    use serde_json::Value;

    use crate::agentengine::unmarshal::MAX_DEPTH;
    use crate::agentengine::{AgentEngineConfig, list_class_methods, new_handler};
    use crate::mux::{Handler, WriteDeadline};
    use crate::rest::testutil::wire_scenario::{assistant_agent, fixed_platform};

    /// Response headers the fixtures leave out (server-owned). Dropping the framing headers
    /// hides two divergences the replay cannot see (ledgered in 5c): a stream
    /// with no line is Go's `Content-Length: 0` but gadk's empty chunked body, and a session
    /// body over 2048 bytes is chunked in Go but has a `Content-Length` here.
    const SERVER_HEADERS: [&str; 3] = ["date", "content-length", "transfer-encoding"];

    /// One fixture configuration's handler (`main`: 4096 bytes, `small`: 32), with its log.
    pub(crate) struct Scenario {
        config: &'static str,
        handler: Handler,
        /// The handler's session service, for a fixture's `seed` event.
        sessions: Arc<InMemorySessionService>,
        log: Arc<Mutex<String>>,
        /// What `new_handler` logged.
        startup_log: String,
    }

    impl Scenario {
        /// Go `NewHandler(cfg, 0, <limit>, "test-engine")` on a fresh fixed platform; the
        /// startup log is kept for the `startup` fixture.
        pub(crate) fn new(config: &'static str) -> Scenario {
            let max_payload_size = match config {
                "main" => 4096,
                "small" => 32,
                other => panic!("unknown fixture configuration {other}"),
            };
            Scenario::with_max_payload_size(config, max_payload_size)
        }

        /// [`Scenario::new`]'s handler with another payload limit.
        pub(crate) fn with_max_payload_size(
            config: &'static str,
            max_payload_size: i64,
        ) -> Scenario {
            let platform = fixed_platform();
            let sessions = Arc::new(InMemorySessionService::with_platform(platform.clone()));
            let mut cfg = AgentEngineConfig::new(
                sessions.clone(),
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
            let startup_log = std::mem::take(&mut *log.lock().unwrap());
            Scenario {
                config,
                handler,
                sessions,
                log,
                startup_log,
            }
        }

        /// Replays fixture `name` by its `kind`: `startup` compares the startup log,
        /// `class_methods` compares [`list_class_methods`], and `exchange` sends the request and
        /// compares status, headers (without [`SERVER_HEADERS`]), body and log byte for byte
        /// (`Processing event: ` lines dropped on both sides).
        pub(crate) async fn replay(&self, name: &str) {
            let path = format!(
                "{}/testdata/agentengine/{name}.json",
                env!("CARGO_MANIFEST_DIR")
            );
            let fx: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            assert_eq!(fx["name"], name);
            assert_eq!(fx["config"], self.config, "{name}");
            assert_eq!(fx["compare"], "exact", "{name}");
            match fx["kind"].as_str().unwrap() {
                "exchange" => {}
                "startup" => {
                    assert_eq!(
                        drop_processing_lines(&self.startup_log),
                        drop_processing_lines(fx["log"].as_str().unwrap()),
                        "{name}: startup log"
                    );
                    return;
                }
                "class_methods" => {
                    let got = Value::Array(
                        list_class_methods()
                            .into_iter()
                            .map(Value::Object)
                            .collect(),
                    );
                    assert_eq!(got, fx["methods"], "{name}: methods");
                    return;
                }
                other => panic!("{name}: unknown fixture kind {other}"),
            }

            if let Some(seed) = fx.get("seed") {
                self.append_seed(seed).await;
            }
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

        /// POSTs `body` (JSON) to `target`: the status and the body text.
        pub(crate) async fn post(&self, target: &str, body: String) -> (u16, String) {
            let mut req = Request::builder()
                .method(Method::POST)
                .uri(target)
                .header("Content-Type", "application/json")
                .body(Body::from(body))
                .unwrap();
            req.extensions_mut().insert(WriteDeadline::new(|_| {}));
            let resp = (self.handler)(req).await;
            let status = resp.status().as_u16();
            let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
            (status, String::from_utf8(body.to_vec()).unwrap())
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

    impl Scenario {
        /// The probe's `appendSeed`: appends the fixture's `seed.event` (session event JSON) to
        /// the `test-engine` session `seed.session_id` of `seed.user_id` through the session
        /// service, not over HTTP.
        async fn append_seed(&self, seed: &Value) {
            let mut event: gadk_session::Event =
                serde_json::from_value(seed["event"].clone()).unwrap();
            let session = self
                .sessions
                .get(GetRequest {
                    app_name: "test-engine".into(),
                    user_id: seed["user_id"].as_str().unwrap().into(),
                    session_id: seed["session_id"].as_str().unwrap().into(),
                    ..Default::default()
                })
                .await
                .unwrap();
            self.sessions
                .append_event(&*session, &mut event)
                .await
                .unwrap();
        }
    }

    /// The server fixtures (C.11), sorted: `*.json` without `*.deltas.json`.
    fn fixture_names() -> Vec<String> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/agentengine");
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| n.ends_with(".json") && !n.ends_with(".deltas.json"))
            .map(|n| n.trim_end_matches(".json").to_owned())
            .collect();
        names.sort();
        names
    }

    /// Every server fixture the Go probe recorded from ADK-Go (30, spec R19), replayed in
    /// file-name order through the real handlers, one handler per configuration (`main_*`,
    /// then `small_*`), so later requests see earlier state and draw later clock and UUID
    /// values exactly as in the probe.
    #[tokio::test]
    async fn replays_every_server_fixture_in_order() {
        let names = fixture_names();
        assert_eq!(names.len(), 30, "{names:?}");
        let mut replayed = 0;
        for config in ["main", "small"] {
            let prefix = format!("{config}_");
            let of_config: Vec<&str> = names
                .iter()
                .map(String::as_str)
                .filter(|n| n.starts_with(&prefix))
                .collect();
            assert!(!of_config.is_empty(), "{config}");
            replayed += of_config.len();
            replay_all(config, &of_config).await;
        }
        assert_eq!(replayed, names.len());
    }

    /// `levels` nested objects `{"a":…}` around `1`.
    fn nested(levels: usize) -> String {
        format!("{}1{}", r#"{"a":"#.repeat(levels), "}".repeat(levels))
    }

    /// Runs `f` on a current-thread runtime on a thread with a tokio worker's default 2 MiB
    /// stack.
    fn on_2_mib_stack<F: std::future::Future<Output = ()>>(f: impl FnOnce() -> F + Send + 'static) {
        std::thread::Builder::new()
            .stack_size(2 << 20)
            .spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(f());
            })
            .unwrap()
            .join()
            .unwrap();
    }

    /// The deepest session state the request limit ([`MAX_DEPTH`], 127) accepts goes through the
    /// real handler on a 2 MiB stack (debug build) without aborting: `async_create_session`
    /// stores it (body 1, `input` 2, `state` 3, then `MAX_DEPTH - 3` levels) and
    /// `async_get_session` answers it; one more level is Go's 400 `exceeded max depth`.
    #[test]
    fn deepest_session_state_is_created_and_read_on_a_2_mib_stack() {
        on_2_mib_stack(|| async {
            let scenario = Scenario::with_max_payload_size("main", 1 << 20);
            let levels = MAX_DEPTH - 3;
            let create = |levels: usize| {
                format!(
                    r#"{{"class_method":"async_create_session","input":{{"user_id":"u","state":{{"k":{}}}}}}}"#,
                    nested(levels)
                )
            };
            let (status, body) = scenario.post("/reasoning_engine", create(levels + 1)).await;
            assert_eq!((status, body.as_str()), (400, "exceeded max depth\n"));

            let (status, body) = scenario.post("/reasoning_engine", create(levels)).await;
            assert_eq!(status, 200, "{}", &body[..body.len().min(200)]);
            assert_eq!(body.matches(r#"{"a":"#).count(), levels);
            let id_at = body.find(r#""id":""#).unwrap() + 6;
            let id = &body[id_at..id_at + body[id_at..].find('"').unwrap()];

            let get = format!(
                r#"{{"class_method":"async_get_session","input":{{"user_id":"u","session_id":"{id}"}}}}"#
            );
            let (status, body) = scenario.post("/reasoning_engine", get).await;
            assert_eq!(status, 200, "{}", &body[..body.len().min(200)]);
            assert_eq!(body.matches(r#"{"a":"#).count(), levels);
        });
    }

    /// The deepest message the request limit ([`MAX_DEPTH`], 127) accepts runs through the real
    /// handler, the runner and the scripted model on a 2 MiB stack (debug build) without
    /// aborting: a function response part (body 1, `input` 2, `message` 3, `parts` 4, the part
    /// 5, `functionResponse` 6, `response` 7, then `MAX_DEPTH - 7` levels); one more level is
    /// Go's 400 `exceeded max depth`.
    #[test]
    fn deepest_stream_query_message_runs_on_a_2_mib_stack() {
        on_2_mib_stack(|| async {
            let scenario = Scenario::with_max_payload_size("main", 1 << 20);
            let levels = MAX_DEPTH - 7;
            let query = |levels: usize| {
                format!(
                    r#"{{"class_method":"async_stream_query","input":{{"user_id":"u","message":{{"role":"user","parts":[{{"functionResponse":{{"name":"save_note","response":{{"r":{}}}}}}}]}}}}}}"#,
                    nested(levels)
                )
            };
            let (status, body) = scenario
                .post("/stream_reasoning_engine", query(levels + 1))
                .await;
            assert_eq!((status, body.as_str()), (400, "exceeded max depth\n"));

            let (status, body) = scenario
                .post("/stream_reasoning_engine", query(levels))
                .await;
            assert_eq!(status, 200, "{}", &body[..body.len().min(200)]);
            assert!(body.contains("Saved your note."), "{body}");
            assert!(!body.contains(r#""error""#), "{body}");
        });
    }

    /// The deepest inner `request_json` message the request limit ([`MAX_DEPTH`], 127) accepts
    /// runs through `streaming_agent_run_with_events`, the runner and the scripted model on a
    /// 2 MiB stack (debug build) without aborting. `request_json` is a string, so the limit
    /// counts from the inner document's top level: a function response part (top 1, `message`
    /// 2, `parts` 3, the part 4, `functionResponse` 5, `response` 6, then `MAX_DEPTH - 6`
    /// levels). One more level fails the inner decode, which Go (past its 10000) answers as any
    /// `request_json` decode error: fixture `main_16`'s 500 and log lines with the wrapped
    /// `exceeded max depth` text.
    #[test]
    fn deepest_run_with_events_request_json_runs_on_a_2_mib_stack() {
        on_2_mib_stack(|| async {
            let scenario = Scenario::with_max_payload_size("main", 1 << 20);
            let levels = MAX_DEPTH - 6;
            let run = |levels: usize| {
                let inner = format!(
                    r#"{{"user_id":"u","message":{{"role":"user","parts":[{{"functionResponse":{{"name":"save_note","response":{{"r":{}}}}}}}]}}}}"#,
                    nested(levels)
                );
                format!(
                    r#"{{"class_method":"streaming_agent_run_with_events","input":{{"request_json":{}}}}}"#,
                    serde_json::to_string(&inner).unwrap()
                )
            };
            scenario.log.lock().unwrap().clear();
            let (status, body) = scenario
                .post("/stream_reasoning_engine", run(levels + 1))
                .await;
            assert_eq!(
                (status, body.as_str()),
                (
                    500,
                    "s.streamJSONL() failed: decodeStreamingAgentRunWithEventsRequest() failed: \
                     json.Unmarshal(input.request_json) failed: exceeded max depth\n"
                )
            );
            assert_eq!(
                *scenario.log.lock().unwrap(),
                "decodeStreamingAgentRunWithEventsRequest() failed: \
                 json.Unmarshal(input.request_json) failed: exceeded max depth\n\
                 handleQuery failed: s.streamJSONL() failed: \
                 decodeStreamingAgentRunWithEventsRequest() failed: \
                 json.Unmarshal(input.request_json) failed: exceeded max depth\n"
            );

            let (status, body) = scenario.post("/stream_reasoning_engine", run(levels)).await;
            assert_eq!(status, 200, "{}", &body[..body.len().min(200)]);
            assert!(body.contains("Saved your note."), "{body}");
            assert!(body.starts_with(r#"{"events":[{"#), "{body}");
            assert!(!body.contains(r#""error""#), "{body}");
        });
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
