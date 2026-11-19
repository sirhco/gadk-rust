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

//! Tests of `VertexAiMemoryService` against gadk-model's 127.0.0.1 fake server.

use std::collections::HashMap;
use std::sync::Arc;

use gadk_genai::Content;
use gadk_memory::{MemoryEntry, MemoryError, MemoryService, SearchRequest};
use gadk_model::gcp_auth::NO_RUNTIME;
use gadk_model::rest::test_util::{FakeResponse, FakeServer, RecordedRequest, ScriptedClient};
use gadk_platform::env::MapEnv;
use gadk_session::SessionError;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use super::{VertexAiMemoryService, VertexAiMemoryServiceConfig};
use crate::AgentEngineData;
use crate::error::find_vertex_error;
use crate::session::VertexSession;
use crate::testutil::{ENGINE_ID, LOCATION, PROJECT_ID};

/// The engine every test uses.
const ENGINE: &str =
    "projects/adk-go-e2e/locations/us-central1/reasoningEngines/1491331942182813696";

/// Go `url.QueryEscape(parent)` in the routing header.
const ROUTING: &str = "parent=projects%2Fadk-go-e2e%2Flocations%2Fus-central1%2FreasoningEngines%2F1491331942182813696";

/// The `$alt` query Go's REST client adds.
const ALT_QUERY: &str = "%24alt=json%3Benum-encoding%3Dint";

/// A pending `GenerateMemories` operation.
const OP_NAME: &str =
    "projects/adk-go-e2e/locations/us-central1/reasoningEngines/1491331942182813696/operations/op1";

fn engine() -> AgentEngineData {
    AgentEngineData {
        project_id: PROJECT_ID.into(),
        location: LOCATION.into(),
        reasoning_engine: ENGINE_ID.into(),
    }
}

/// A config for a fake at `url`: no credentials, an empty environment.
fn config(url: &str) -> VertexAiMemoryServiceConfig {
    VertexAiMemoryServiceConfig {
        engine: engine(),
        endpoint: Some(url.to_owned()),
        without_authentication: true,
        env: Arc::new(MapEnv(HashMap::new())),
        ..Default::default()
    }
}

fn service(config: VertexAiMemoryServiceConfig) -> VertexAiMemoryService {
    VertexAiMemoryService::new(config).unwrap()
}

/// Session `s1` of user `u1` with `state`.
fn session(state: Value) -> VertexSession {
    let Value::Object(state) = state else {
        panic!("state must be an object");
    };
    VertexSession::new(
        "app".into(),
        "u1".into(),
        "s1".into(),
        state,
        chrono::DateTime::UNIX_EPOCH,
    )
}

/// A fake answering `memories:generate` with `generate` and `/ui/` polls with `poll`.
fn generate_server(generate: Value, poll: Value) -> FakeServer {
    FakeServer::start(move |req: &RecordedRequest| {
        if req.path.starts_with("/ui/") {
            FakeResponse::json(poll.to_string())
        } else {
            FakeResponse::json(generate.to_string())
        }
    })
}

/// The `Backend` error's text, or a panic.
fn backend_text(err: &MemoryError) -> String {
    match err {
        MemoryError::Backend(e) => e.to_string(),
        other => panic!("want Backend, got {other:?}"),
    }
}

/// Asserts `req` is Go's REST `GenerateMemories`/`RetrieveMemories` call for `method`.
fn assert_memory_call(req: &RecordedRequest, method: &str) {
    assert_eq!(req.method, "POST");
    assert_eq!(
        req.path_and_query(),
        format!("/v1beta1/{ENGINE}/memories:{method}?{ALT_QUERY}")
    );
    assert_eq!(req.header("x-goog-request-params"), ROUTING);
    assert_eq!(req.header("content-type"), "application/json");
    assert!(req.headers.get("authorization").is_none());
}

/// Go `addWholeSession`: `{parent, vertexSessionSource: {session}, scope: {user_id}}`, and no
/// wait without `WaitForCompletion` (a pending operation is not polled).
#[tokio::test]
async fn add_whole_session_sends_gos_generate_request() {
    let server = generate_server(json!({"name": OP_NAME}), json!({}));
    let svc = service(config(&server.url()));
    svc.add_session_to_memory(&session(json!({"last": "2026-01-02T03:04:05Z"})))
        .await
        .unwrap();
    let reqs = server.requests();
    assert_eq!(reqs.len(), 1);
    assert_memory_call(&reqs[0], "generate");
    assert_eq!(
        reqs[0].json(),
        json!({
            "parent": ENGINE,
            "vertexSessionSource": {"session": format!("{ENGINE}/sessions/s1")},
            "scope": {"user_id": "u1"},
        })
    );
}

/// Go checks `StateKeySessionLastUpdateTime == ""`: an empty key also sends the whole session.
#[tokio::test]
async fn an_empty_state_key_sends_the_whole_session() {
    let server = generate_server(json!({"name": OP_NAME}), json!({}));
    let svc = service(VertexAiMemoryServiceConfig {
        state_key_session_last_update_time: Some(String::new()),
        ..config(&server.url())
    });
    svc.add_session_to_memory(&session(json!({})))
        .await
        .unwrap();
    let body = server.last_request().json();
    assert_eq!(
        body["vertexSessionSource"],
        json!({"session": format!("{ENGINE}/sessions/s1")})
    );
}

/// Go `addEventsNewerThan`: the state time becomes `vertexSessionSource.startTime` in protojson
/// form (UTC, 0/3/6/9 fraction digits).
#[tokio::test]
async fn add_events_newer_than_sends_the_state_time_as_start_time() {
    for (stored, want) in [
        ("2026-01-02T03:04:05Z", "2026-01-02T03:04:05Z"),
        ("2026-01-02T03:04:05.12+02:00", "2026-01-02T01:04:05.120Z"),
        (
            "2026-01-02T03:04:05.123456789-01:30",
            "2026-01-02T04:34:05.123456789Z",
        ),
    ] {
        let server = generate_server(json!({"name": OP_NAME}), json!({}));
        let svc = service(VertexAiMemoryServiceConfig {
            state_key_session_last_update_time: Some("last".into()),
            ..config(&server.url())
        });
        svc.add_session_to_memory(&session(json!({"last": stored})))
            .await
            .unwrap();
        let reqs = server.requests();
        assert_eq!(reqs.len(), 1);
        assert_memory_call(&reqs[0], "generate");
        assert_eq!(
            reqs[0].json(),
            json!({
                "parent": ENGINE,
                "vertexSessionSource": {
                    "session": format!("{ENGINE}/sessions/s1"),
                    "startTime": want,
                },
                "scope": {"user_id": "u1"},
            }),
            "{stored}"
        );
    }
}

/// Go `fmt.Errorf("state.Get(%s) failed: %w", key, err)`; nothing is sent.
#[tokio::test]
async fn a_missing_state_key_is_gos_state_get_error() {
    let server = generate_server(json!({"name": OP_NAME}), json!({}));
    let svc = service(VertexAiMemoryServiceConfig {
        state_key_session_last_update_time: Some("last".into()),
        ..config(&server.url())
    });
    let err = svc
        .add_session_to_memory(&session(json!({})))
        .await
        .unwrap_err();
    assert_eq!(
        backend_text(&err),
        "state.Get(last) failed: state key does not exist"
    );
    let MemoryError::Backend(e) = &err else {
        unreachable!()
    };
    let source = e.source().unwrap();
    assert!(matches!(
        source.downcast_ref::<SessionError>(),
        Some(SessionError::StateKeyNotExist)
    ));
    assert!(server.requests().is_empty());
}

/// A state value that is not an RFC 3339 string names its JSON kind and shows the value as JSON,
/// for each JSON value kind and for a string that is not RFC 3339.
#[tokio::test]
async fn a_state_value_that_is_not_a_time_is_a_type_error() {
    let server = generate_server(json!({"name": OP_NAME}), json!({}));
    let svc = service(VertexAiMemoryServiceConfig {
        state_key_session_last_update_time: Some("last".into()),
        ..config(&server.url())
    });
    for (value, want) in [
        (json!(1), "a number (1)"),
        (json!(1.5), "a number (1.5)"),
        (json!(1e21), "a number (1e+21)"),
        (json!(1_000_000), "a number (1000000)"),
        (json!(true), "a boolean (true)"),
        (json!(false), "a boolean (false)"),
        (Value::Null, "null"),
        (json!({"b": 1, "a": "x"}), r#"an object ({"a":"x","b":1})"#),
        (json!({}), "an object ({})"),
        (json!([1, "a"]), r#"an array ([1,"a"])"#),
        (json!("yesterday"), r#"a string ("yesterday")"#),
        (json!(""), r#"a string ("")"#),
        (json!("2026-01-02"), r#"a string ("2026-01-02")"#),
    ] {
        let err = svc
            .add_session_to_memory(&session(json!({"last": value.clone()})))
            .await
            .unwrap_err();
        assert_eq!(
            backend_text(&err),
            format!("expected an RFC 3339 timestamp, found {want}"),
            "{value}"
        );
    }
    assert!(server.requests().is_empty());
}

/// Go's wrapper chains around a failed `GenerateMemories` call, by path.
#[tokio::test]
async fn generate_errors_carry_gos_wrappers() {
    let server = FakeServer::always(FakeResponse::json_status(
        403,
        r#"{"error":{"code":403,"message":"denied","status":"PERMISSION_DENIED"}}"#,
    ));
    let whole = service(config(&server.url()));
    let err = whole
        .add_session_to_memory(&session(json!({})))
        .await
        .unwrap_err();
    assert_eq!(
        backend_text(&err),
        "v.client.addWholeSession failed: v.client.GenerateMemories failed: \
         rpc error: code = PermissionDenied desc = denied"
    );
    let MemoryError::Backend(e) = &err else {
        unreachable!()
    };
    assert_eq!(find_vertex_error(e.as_ref()).unwrap().status, 403);

    let newer = service(VertexAiMemoryServiceConfig {
        state_key_session_last_update_time: Some("last".into()),
        ..config(&server.url())
    });
    let err = newer
        .add_session_to_memory(&session(json!({"last": "2026-01-02T03:04:05Z"})))
        .await
        .unwrap_err();
    assert_eq!(
        backend_text(&err),
        "v.client.addEventsNewerThan failed: v.client.GenerateMemories failed: \
         rpc error: code = PermissionDenied desc = denied"
    );
}

/// `WaitForCompletion`: a pending operation is polled at `GET /ui/{name}` (no query, routing
/// `name=…`) until done; done without a result is success (Go accepts `op.Wait`'s
/// `operation finished without a response`).
#[tokio::test]
async fn wait_for_completion_polls_until_done() {
    let server = generate_server(
        json!({"name": OP_NAME, "done": false}),
        json!({"name": OP_NAME, "done": true}),
    );
    let svc = service(VertexAiMemoryServiceConfig {
        wait_for_completion: true,
        ..config(&server.url())
    });
    svc.add_session_to_memory(&session(json!({})))
        .await
        .unwrap();
    let reqs = server.requests();
    assert_eq!(reqs.len(), 2);
    assert_memory_call(&reqs[0], "generate");
    assert_eq!(reqs[1].method, "GET");
    assert_eq!(reqs[1].path_and_query(), format!("/ui/{OP_NAME}"));
    assert_eq!(
        reqs[1].header("x-goog-request-params"),
        format!("name={}", gadk_platform::go_query_escape(OP_NAME))
    );
    assert!(reqs[1].body.is_empty());
}

/// An operation already done when `GenerateMemories` returns is not polled.
#[tokio::test]
async fn wait_for_completion_of_a_done_operation_sends_no_poll() {
    let server = generate_server(
        json!({"name": OP_NAME, "done": true, "response": {}}),
        json!({}),
    );
    let svc = service(VertexAiMemoryServiceConfig {
        wait_for_completion: true,
        ..config(&server.url())
    });
    svc.add_session_to_memory(&session(json!({})))
        .await
        .unwrap();
    assert_eq!(server.requests().len(), 1);
}

/// An operation that fails is `op.Wait for GenerateMemories (whole session) failed: …` on both
/// paths (Go uses the whole-session text for `addEventsNewerThan` too).
#[tokio::test]
async fn an_operation_error_carries_gos_wait_wrapper_on_both_paths() {
    let failed = json!({
        "name": OP_NAME,
        "done": true,
        "error": {"code": 9, "message": "session has no events"},
    });
    let server = generate_server(json!({"name": OP_NAME}), failed);
    let whole = service(VertexAiMemoryServiceConfig {
        wait_for_completion: true,
        ..config(&server.url())
    });
    let err = whole
        .add_session_to_memory(&session(json!({})))
        .await
        .unwrap_err();
    assert_eq!(
        backend_text(&err),
        "v.client.addWholeSession failed: op.Wait for GenerateMemories (whole session) failed: \
         rpc error: code = FailedPrecondition desc = session has no events"
    );

    let newer = service(VertexAiMemoryServiceConfig {
        wait_for_completion: true,
        state_key_session_last_update_time: Some("last".into()),
        ..config(&server.url())
    });
    let err = newer
        .add_session_to_memory(&session(json!({"last": "2026-01-02T03:04:05Z"})))
        .await
        .unwrap_err();
    assert_eq!(
        backend_text(&err),
        "v.client.addEventsNewerThan failed: op.Wait for GenerateMemories (whole session) \
         failed: rpc error: code = FailedPrecondition desc = session has no events"
    );
}

/// Go `searchMemory`: `{parent, similaritySearchParams: {searchQuery}, scope}`; each
/// `retrievedMemories[i].memory.fact` becomes `genai.NewContentFromText(fact, "user")`, in order.
#[tokio::test]
async fn search_sends_gos_retrieve_request_and_maps_facts() {
    let server = FakeServer::json(
        json!({
            "retrievedMemories": [
                {"memory": {"name": "m1", "fact": "likes tea", "scope": {"user_id": "u1"}}, "distance": 0.1},
                {"memory": {"fact": "lives in Oslo"}},
                {"memory": {"name": "m3"}},
            ],
            "nextPageToken": "ignored",
        })
        .to_string(),
    );
    let svc = service(config(&server.url()));
    let resp = svc
        .search_memory(SearchRequest {
            query: "what does the user drink?".into(),
            user_id: "u1".into(),
            app_name: "ignored-app".into(),
        })
        .await
        .unwrap();
    let req = server.last_request();
    assert_memory_call(&req, "retrieve");
    assert_eq!(
        req.json(),
        json!({
            "parent": ENGINE,
            "similaritySearchParams": {"searchQuery": "what does the user drink?"},
            "scope": {"user_id": "u1"},
        })
    );
    let entry = |fact: &str| MemoryEntry {
        content: Some(Content::from_text(fact, "user")),
        ..MemoryEntry::default()
    };
    assert_eq!(
        resp.memories,
        vec![entry("likes tea"), entry("lives in Oslo"), entry("")]
    );
}

/// No retrieved memories is an empty (non-error) response; protojson omits an empty
/// `searchQuery` but keeps the set `similaritySearchParams`.
#[tokio::test]
async fn search_with_no_results_is_empty() {
    let server = FakeServer::json("{}");
    let svc = service(config(&server.url()));
    let resp = svc
        .search_memory(SearchRequest {
            query: String::new(),
            user_id: String::new(),
            app_name: String::new(),
        })
        .await
        .unwrap();
    assert!(resp.memories.is_empty());
    assert_eq!(
        server.last_request().json(),
        json!({
            "parent": ENGINE,
            "similaritySearchParams": {},
            "scope": {"user_id": ""},
        })
    );
}

/// Go `fmt.Errorf("v.client.RetrieveMemories failed: %w", err)`, with no service-level wrapper.
#[tokio::test]
async fn search_errors_carry_gos_wrapper() {
    let server = FakeServer::always(FakeResponse::json_status(
        404,
        r#"{"error":{"code":404,"message":"engine not found","status":"NOT_FOUND"}}"#,
    ));
    let svc = service(config(&server.url()));
    let err = svc
        .search_memory(SearchRequest {
            query: "q".into(),
            user_id: "u1".into(),
            app_name: "app".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(
        backend_text(&err),
        "v.client.RetrieveMemories failed: rpc error: code = NotFound desc = engine not found"
    );
}

/// Go `option.WithEndpoint(aiplatformutil.HostPortURL(location))`: the location's regional host
/// (the global host for `""`/`global`) over HTTPS when no endpoint is given.
#[tokio::test]
async fn new_defaults_to_the_regional_endpoint() {
    for (location, host) in [
        (
            "us-central1",
            "https://us-central1-aiplatform.googleapis.com",
        ),
        ("global", "https://aiplatform.googleapis.com"),
        ("", "https://aiplatform.googleapis.com"),
    ] {
        let scripted = ScriptedClient::json("{}");
        let svc = service(VertexAiMemoryServiceConfig {
            engine: AgentEngineData {
                location: location.into(),
                ..engine()
            },
            endpoint: None,
            http_client: Some(scripted.clone()),
            ..config("")
        });
        svc.search_memory(SearchRequest::default()).await.unwrap();
        let url = scripted.requests()[0].url.clone();
        let parent =
            format!("projects/adk-go-e2e/locations/{location}/reasoningEngines/{ENGINE_ID}");
        assert_eq!(
            url,
            format!("{host}/v1beta1/{parent}/memories:retrieve?{ALT_QUERY}"),
            "{location:?}"
        );
    }
}

/// An Application Default Credentials failure is Go's `newVertexAIClient failed:
/// aiplatform.NewMemoryBankClient failed: …`; outside a Tokio runtime `new` refuses instead of
/// panicking.
#[test]
fn new_wraps_adc_errors_like_go() {
    assert!(tokio::runtime::Handle::try_current().is_err());
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("creds.json");
    let env = Arc::new(MapEnv(HashMap::from([(
        "GOOGLE_APPLICATION_CREDENTIALS".to_owned(),
        file.to_string_lossy().into_owned(),
    )])));
    let adc_config = || VertexAiMemoryServiceConfig {
        engine: engine(),
        env: env.clone(),
        ..Default::default()
    };

    std::fs::write(&file, r#"{"type":"bogus"}"#).unwrap();
    let err = VertexAiMemoryService::new(adc_config()).err().unwrap();
    assert_eq!(
        backend_text(&err),
        "newVertexAIClient failed: aiplatform.NewMemoryBankClient failed: \
         unknown credential type: bogus"
    );

    std::fs::write(&file, r#"{"type":"authorized_user"}"#).unwrap();
    let err = VertexAiMemoryService::new(adc_config()).err().unwrap();
    assert_eq!(
        backend_text(&err),
        format!("newVertexAIClient failed: aiplatform.NewMemoryBankClient failed: {NO_RUNTIME}")
    );

    // `without_authentication` builds without a runtime.
    assert!(
        VertexAiMemoryService::new(VertexAiMemoryServiceConfig {
            without_authentication: true,
            ..adc_config()
        })
        .is_ok()
    );
}

/// The config's `Debug` hides credentials and the HTTP client.
#[test]
fn config_debug_hides_secrets() {
    let text = format!("{:?}", config("http://127.0.0.1:1"));
    assert!(text.contains("without_authentication: true"), "{text}");
    assert!(text.contains("credentials: false"), "{text}");
}
