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

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Port of Go `server/adkrest/controllers/runtime_test.go`, plus focused checks of `/run` and
//! `/run_sse` against the ADK-Go wire fixtures in `testdata/wire` (the full replay is
//! `tests/wire.rs`).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use futures::{FutureExt as _, StreamExt as _};
use gadk_agent::{AgentConfig, AgentError, Plugin, PluginConfig, SingleLoader};
use gadk_engine::llm::LlmAgentConfig;
use gadk_engine::llm::agent as llm_agent;
use gadk_engine::runner::PluginConfig as RunnerPluginConfig;
use gadk_engine::testutil::MockModel;
use gadk_genai::{Content, Part};
use gadk_model::ModelError;
use gadk_platform::{Platform, go_quote};
use gadk_server::authn::Caller;
use gadk_server::rest::Timeout;
use gadk_server::rest::controllers::{RuntimeApiController, RuntimeApiControllerConfig};
use gadk_server::rest::testutil::{FakeSessionService, SessionKey, TestSession};
use gadk_session::compaction::Config as CompactionConfig;
use gadk_session::{
    CreateRequest, DeleteRequest, Event, GetRequest, InMemorySessionService, ListRequest, Session,
    SessionError, SessionService,
};
use http::{Method, StatusCode};
use serde_json::Value;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

type Response = axum::response::Response;

/// `POST <path>` with `body`, authenticated as `user` when given (Go `authn.WithCaller`).
fn post(path: &str, body: &str, user: Option<&str>) -> http::Request<Body> {
    let mut req = http::Request::builder()
        .method(Method::POST)
        .uri(path)
        .body(Body::from(body.to_owned()))
        .unwrap();
    if let Some(user) = user {
        req.extensions_mut().insert(Caller {
            user_id: user.into(),
            claims: None,
        });
    }
    req
}

/// Status, headers (lowercase name, value; in insertion order) and body of `resp`.
async fn parts(resp: Response) -> (u16, Vec<(String, String)>, String) {
    let status = resp.status().as_u16();
    let headers = resp
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap().to_owned()))
        .collect();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, headers, String::from_utf8(body.to_vec()).unwrap())
}

/// Go `[]testAgentResult`: `Ok(text)` is an event with `text`, `Err(msg)` a run error.
type AgentResults = Vec<Result<&'static str, &'static str>>;

/// A custom agent named `name` whose every run yields `results` (Go `testAgent`): `Ok(text)` is
/// Go `makeEvent("invocation-1", name, text)`, `Err(msg)` a run error.
fn test_agent(name: &str, results: AgentResults) -> Arc<dyn gadk_agent::Agent> {
    let author = name.to_owned();
    let results: Vec<Result<String, String>> = results
        .into_iter()
        .map(|r| r.map(str::to_owned).map_err(str::to_owned))
        .collect();
    gadk_agent::new_agent(AgentConfig {
        name: name.into(),
        run: Some(Arc::new(move |_ctx| {
            let items: Vec<Result<Event, AgentError>> = results
                .iter()
                .map(|r| match r {
                    Ok(text) => Ok(make_event("invocation-1", &author, text)),
                    Err(msg) => Err(AgentError::other(msg.clone())),
                })
                .collect();
            futures::stream::iter(items).boxed()
        })),
        ..Default::default()
    })
    .unwrap()
}

/// Go `makeEvent`.
fn make_event(id: &str, author: &str, text: &str) -> Event {
    let mut e = Event::new(&Platform::default(), id);
    e.author = author.into();
    e.response.content = Some(Content::from_parts(vec![Part::from_text(text)], ""));
    e
}

/// Go's `fakes.FakeSessionService` holding the session `testApp`/`testUser`/`testSession`.
fn fake_sessions() -> Arc<FakeSessionService> {
    Arc::new(FakeSessionService::new([TestSession::new(
        SessionKey {
            app_name: "testApp".into(),
            user_id: "testUser".into(),
            session_id: "testSession".into(),
        },
        Default::default(),
        Vec::new(),
        chrono::Utc::now(),
    )]))
}

const TEST_RUN_BODY: &str = r#"{"appName":"testApp","userId":"testUser","sessionId":"testSession","newMessage":{"parts":[{"text":"Hello"}]},"streaming":true}"#;

#[tokio::test]
// parity: adapted: the controller's plugin list is crate-private, so each case counts the
// plugins whose before-run callback fires during a /run instead of reading the field
async fn test_new_runtime_api_controller_plugins_assignment() {
    let calls = Arc::new(AtomicUsize::new(0));
    let plugin = |name: &str| {
        let calls = calls.clone();
        Plugin::new(PluginConfig {
            name: name.into(),
            before_run_callback: Some(Arc::new(move |_ctx| {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Ok(None) }.boxed()
            })),
            ..Default::default()
        })
    };
    let (p1, p2) = (plugin("plugin1"), plugin("plugin2"));
    let cases: Vec<(&str, Vec<Plugin>, usize)> = vec![
        ("with no plugins", Vec::new(), 0),
        ("with empty plugin list", Vec::new(), 0),
        ("with single plugin", vec![p1.clone()], 1),
        ("with multiple plugins", vec![p1, p2], 2),
    ];
    for (name, plugins, want_plugins) in cases {
        calls.store(0, Ordering::SeqCst);
        let mut cfg = RuntimeApiControllerConfig::new(fake_sessions());
        cfg.agent_loader = Some(Arc::new(SingleLoader::new(test_agent(
            "testApp",
            vec![Ok("hi")],
        ))));
        cfg.sse_timeout = Timeout::After(Duration::from_secs(10));
        cfg.plugin_config = RunnerPluginConfig {
            plugins,
            ..Default::default()
        };
        let controller = RuntimeApiController::new(cfg);
        let resp = controller
            .run_handler(post("/run", TEST_RUN_BODY, Some("testUser")))
            .await;
        assert_eq!(resp.status(), StatusCode::OK, "{name}");
        assert_eq!(calls.load(Ordering::SeqCst), want_plugins, "{name}");
    }
}

#[tokio::test]
async fn test_run_sse_handler() {
    let cases: Vec<(&str, AgentResults, u16, Vec<&str>)> = vec![
        (
            "success case",
            // Non-ASCII text: the stream must carry it through as UTF-8.
            vec![Ok("Hello from agent: 72°F")],
            200,
            vec!["data: {", "Hello from agent: 72°F"],
        ),
        (
            "error case",
            vec![Err("agent failed")],
            200,
            vec!["event: error\ndata: {\"error\":\"agent failed\"}\n\n"],
        ),
        (
            "interleaved success and error",
            vec![
                Ok("Hello from agent"),
                Err("agent failed"),
                Ok("More data"),
                Err("agent failed again"),
            ],
            200,
            vec![
                "data: {",
                "Hello from agent",
                "event: error\ndata: {\"error\":\"agent failed\"}\n\n",
                "data: {",
                "More data",
                "event: error\ndata: {\"error\":\"agent failed again\"}\n\n",
            ],
        ),
    ];
    for (name, results, want_status, want_body) in cases {
        let mut cfg = RuntimeApiControllerConfig::new(fake_sessions());
        cfg.agent_loader = Some(Arc::new(SingleLoader::new(test_agent("testApp", results))));
        cfg.sse_timeout = Timeout::After(Duration::from_secs(10));
        let controller = RuntimeApiController::new(cfg);

        let resp = controller
            .run_sse_handler(post("/run-sse", TEST_RUN_BODY, Some("testUser")))
            .await;
        let (status, headers, body) = parts(resp).await;

        assert_eq!(status, want_status, "{name}");
        // Without the charset, clients that apply the legacy ISO-8859-1 default for text/*
        // mojibake every non-ASCII rune in the stream.
        let content_type = headers.iter().find(|(k, _)| k == "content-type");
        assert_eq!(
            content_type.map(|(_, v)| v.as_str()),
            Some("text/event-stream; charset=UTF-8"),
            "{name}"
        );
        for s in want_body {
            assert!(body.contains(s), "{name}: body {body:?} lacks {s:?}");
        }
    }
}

#[tokio::test]
// parity: adapted: decodeRequestBody is private, so the body goes through /run: a decoded
// request reaches the session lookup (404 from the empty fake) instead of failing to decode (400)
async fn test_decode_request_body_accepts_function_call_event_id() {
    let body = r#"{
		"appName": "a",
		"userId": "u",
		"sessionId": "s",
		"newMessage": {"role": "user", "parts": [{"text": "hi"}]},
		"functionCallEventId": "fce-1"
	}"#;
    let controller = RuntimeApiController::new(RuntimeApiControllerConfig::new(Arc::new(
        FakeSessionService::default(),
    )));
    let (status, _, got) = parts(controller.run_handler(post("/run", body, None)).await).await;
    assert_eq!(
        (status, got.as_str()),
        (404, "failed to get session: not found\n")
    );
}

#[tokio::test]
// parity: adapted: decodeRequestBody is private, so the body goes through /run and the test
// also pins Go's decoder text
async fn test_decode_request_body_rejects_unknown_fields() {
    let body = r#"{
		"appName": "a",
		"userId": "u",
		"sessionId": "s",
		"newMessage": {},
		"totallyMadeUpField": 123
	}"#;
    let controller = RuntimeApiController::new(RuntimeApiControllerConfig::new(Arc::new(
        FakeSessionService::default(),
    )));
    let (status, _, got) = parts(controller.run_handler(post("/run", body, None)).await).await;
    assert_eq!(
        (status, got.as_str()),
        (
            400,
            "failed to decode request: json: unknown field \"totallyMadeUpField\"\n"
        )
    );
}

// parity: skipped TestNewRuntimeAPIController_BackwardCompatible: Go-deprecated NewRuntimeAPIController is not ported (spec Scope)

#[tokio::test]
// parity: adapted: the controller's compaction config is crate-private, so the test shows every
// runner gets it: a config without a summarizer on a custom (non-LLM) root agent fails runner
// creation, and the same controller without compaction runs
async fn test_new_runtime_api_controller_carries_compaction() {
    let controller = |compaction: Option<CompactionConfig>| {
        let mut cfg = RuntimeApiControllerConfig::new(fake_sessions());
        cfg.agent_loader = Some(Arc::new(SingleLoader::new(test_agent(
            "testApp",
            vec![Ok("hi")],
        ))));
        cfg.sse_timeout = Timeout::After(Duration::from_secs(10));
        cfg.compaction = compaction;
        RuntimeApiController::new(cfg)
    };
    let with = controller(Some(CompactionConfig {
        compaction_interval: 2,
        ..Default::default()
    }));
    let (status, _, got) = parts(with.run_handler(post("/run", TEST_RUN_BODY, None)).await).await;
    assert_eq!(
        (status, got.as_str()),
        (
            500,
            "failed to create runner: the Compaction config needs a Summarizer: root agent \"testApp\" is not an LLM agent, so no default model is available\n"
        )
    );
    let without = controller(None);
    let resp = without.run_handler(post("/run", TEST_RUN_BODY, None)).await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
#[ignore = "run_live_handler is plan 5a Task 8's; Task 13 removes this ignore"]
async fn test_run_live_handler_uses_configured_check_origin() {
    let seen_origin = Arc::new(parking_lot::Mutex::new(None::<String>));
    let mut cfg = RuntimeApiControllerConfig::new(Arc::new(InMemorySessionService::new()));
    cfg.agent_loader = Some(Arc::new(SingleLoader::new(test_agent("a", Vec::new()))));
    let seen = seen_origin.clone();
    cfg.check_origin = Some(Arc::new(move |parts: &http::request::Parts| {
        *seen.lock() = parts
            .headers
            .get(http::header::ORIGIN)
            .map(|v| v.to_str().unwrap().to_owned());
        false
    }));
    let controller = RuntimeApiController::new(cfg);

    let req = http::Request::builder()
        .method(Method::GET)
        .uri("/run_live?appName=a&userId=u&sessionId=s")
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-Websocket-Version", "13")
        .header("Sec-Websocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
        .header("Origin", "http://localhost:4200")
        .body(Body::empty())
        .unwrap();
    let resp = controller.run_live_handler(req).await;

    assert_eq!(
        seen_origin.lock().as_deref(),
        Some("http://localhost:4200"),
        "CheckOrigin was not called, want the upgrader to use it"
    );
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

// ---- Focused wire checks (ADK-Go fixtures; the full replay is tests/wire.rs) ----

/// The fixture `testdata/wire/<name>.json`: request body, status, headers (lowercase name,
/// value) and body.
fn fixture(name: &str) -> (String, u16, Vec<(String, String)>, String) {
    let path = format!("{}/testdata/wire/{name}.json", env!("CARGO_MANIFEST_DIR"));
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let headers = v["response"]["headers"]
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
    (
        v["request"]["body"].as_str().unwrap().to_owned(),
        u16::try_from(v["response"]["status"].as_u64().unwrap()).unwrap(),
        headers,
        v["response"]["body"].as_str().unwrap().to_owned(),
    )
}

/// The in-memory session service answering a missing session with Go's
/// `session not found: "<id>"` (gadk's in-memory text has no ID; this check is about the
/// controller's framing).
struct GoNotFound(InMemorySessionService);

#[async_trait]
impl SessionService for GoNotFound {
    async fn create(&self, req: CreateRequest) -> Result<Arc<dyn Session>, SessionError> {
        self.0.create(req).await
    }
    async fn get(&self, req: GetRequest) -> Result<Arc<dyn Session>, SessionError> {
        let id = req.session_id.clone();
        self.0.get(req).await.map_err(|err| match err {
            SessionError::NotFound => {
                SessionError::Backend(format!("session not found: {}", go_quote(&id)).into())
            }
            other => other,
        })
    }
    async fn list(&self, req: ListRequest) -> Result<Vec<Arc<dyn Session>>, SessionError> {
        self.0.list(req).await
    }
    async fn delete(&self, req: DeleteRequest) -> Result<(), SessionError> {
        self.0.delete(req).await
    }
    async fn append_event(
        &self,
        session: &dyn Session,
        event: &mut Event,
    ) -> Result<(), SessionError> {
        self.0.append_event(session, event).await
    }
}

/// The probe's `assistant` (SCENARIO.md) reduced to its `fail` reply (twice: one `/run`, one
/// `/run_sse`), with session
/// `assistant`/`alice`/`s1`.
async fn failing_assistant() -> RuntimeApiController {
    let fail = || Err(ModelError::Other("scripted model failure".into()));
    let model = MockModel::new(vec![fail(), fail()]).name_as("scripted-assistant");
    let assistant = llm_agent::new(LlmAgentConfig {
        name: "assistant".into(),
        description: "Answers greetings and saves notes.".into(),
        model: Some(model),
        ..Default::default()
    })
    .unwrap();
    let sessions = Arc::new(GoNotFound(InMemorySessionService::new()));
    sessions
        .create(CreateRequest {
            app_name: "assistant".into(),
            user_id: "alice".into(),
            session_id: Some("s1".into()),
            state: None,
        })
        .await
        .unwrap();
    let mut cfg = RuntimeApiControllerConfig::new(sessions);
    cfg.agent_loader = Some(Arc::new(SingleLoader::new(assistant)));
    RuntimeApiController::new(cfg)
}

/// `handler` on fixture `name`'s request matches its status, headers and body exactly.
async fn assert_fixture<F, Fut>(name: &str, handler: F)
where
    F: FnOnce(http::Request<Body>) -> Fut,
    Fut: Future<Output = Response>,
{
    let (req_body, want_status, mut want_headers, want_body) = fixture(name);
    let (status, mut headers, body) = parts(handler(post("/", &req_body, None)).await).await;
    headers.sort();
    want_headers.sort();
    assert_eq!(
        (status, headers, body),
        (want_status, want_headers, want_body),
        "{name}"
    );
}

#[tokio::test]
async fn wire_run_error_and_bad_requests_match_go() {
    let c = failing_assistant().await;
    assert_fixture("main_15_run_error", |r| c.run_handler(r)).await;
    assert_fixture("main_16_run_unknown_session", |r| c.run_handler(r)).await;
    assert_fixture("main_17_run_unknown_field", |r| c.run_handler(r)).await;
    assert_fixture("main_19_run_sse_error", |r| c.run_sse_handler(r)).await;
    assert_fixture("main_20_run_sse_unknown_session", |r| c.run_sse_handler(r)).await;
}

/// `/run_sse` over a real socket: the raw response head carries Go's three headers, including
/// `Connection: keep-alive`, and the error frame arrives chunked.
#[tokio::test]
async fn wire_run_sse_headers_on_the_socket() {
    let c = Arc::new(failing_assistant().await);
    let app = axum::Router::new().fallback(move |req: axum::extract::Request| {
        let c = c.clone();
        async move { c.run_sse_handler(req).await }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let (req_body, ..) = fixture("main_19_run_sse_error");
    let mut conn = tokio::net::TcpStream::connect(addr).await.unwrap();
    let head = format!(
        "POST /run_sse HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        req_body.len()
    );
    conn.write_all(head.as_bytes()).await.unwrap();
    conn.write_all(req_body.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    let mut buf = [0u8; 4096];
    while !raw.ends_with(b"0\r\n\r\n") {
        let n = tokio::time::timeout(Duration::from_secs(10), conn.read(&mut buf))
            .await
            .unwrap()
            .unwrap();
        assert!(
            n > 0,
            "connection closed early: {}",
            String::from_utf8_lossy(&raw)
        );
        raw.extend_from_slice(&buf[..n]);
    }
    let raw = String::from_utf8(raw).unwrap();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap();
    let mut lines = head.lines();
    assert_eq!(lines.next(), Some("HTTP/1.1 200 OK"));
    let mut headers: Vec<String> = lines
        .map(str::to_ascii_lowercase)
        .filter(|l| !l.starts_with("date:"))
        .collect();
    headers.sort();
    assert_eq!(
        headers,
        [
            "cache-control: no-cache",
            "connection: keep-alive",
            "content-type: text/event-stream; charset=utf-8",
            "transfer-encoding: chunked",
        ]
    );
    let frame = "event: error\ndata: {\"error\":\"scripted model failure\"}\n\n";
    assert!(body.contains(frame), "{body:?}");
}

/// A run that outlives the SSE write timeout: frames written before the deadline arrive, then
/// the body fails (the connection is aborted, as Go's failed write leaves it);
/// `Timeout::Disabled` sets no deadline.
#[tokio::test(start_paused = true)]
async fn sse_write_timeout_ends_the_stream() {
    let controller = |timeout: Timeout| {
        let agent = gadk_agent::new_agent(AgentConfig {
            name: "testApp".into(),
            run: Some(Arc::new(|_ctx| {
                futures::stream::once(async { Ok(make_event("inv", "testApp", "first")) })
                    .chain(futures::stream::once(async {
                        tokio::time::sleep(Duration::from_secs(60)).await;
                        Ok(make_event("inv", "testApp", "late"))
                    }))
                    .boxed()
            })),
            ..Default::default()
        })
        .unwrap();
        let mut cfg = RuntimeApiControllerConfig::new(fake_sessions());
        cfg.agent_loader = Some(Arc::new(SingleLoader::new(agent)));
        cfg.sse_timeout = timeout;
        RuntimeApiController::new(cfg)
    };

    let resp = controller(Timeout::After(Duration::from_secs(30)))
        .run_sse_handler(post("/run_sse", TEST_RUN_BODY, None))
        .await;
    let mut body = resp.into_body().into_data_stream();
    let first = body.next().await.unwrap().unwrap();
    assert!(String::from_utf8_lossy(&first).contains("first"));
    let err = body.next().await.unwrap().unwrap_err();
    assert_eq!(err.to_string(), "i/o timeout");
    assert!(body.next().await.is_none());

    let (status, _, got) = parts(
        controller(Timeout::Disabled)
            .run_sse_handler(post("/run_sse", TEST_RUN_BODY, None))
            .await,
    )
    .await;
    assert_eq!(status, 200);
    assert!(got.contains("first") && got.contains("late"), "{got}");
}
