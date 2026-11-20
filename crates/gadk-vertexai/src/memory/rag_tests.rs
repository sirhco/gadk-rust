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

//! Tests of `VertexAiRagMemoryService` against gadk-model's 127.0.0.1 fake server.

use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;
use chrono::{DateTime, Utc};
use gadk_genai::{Content, Part, ROLE_USER};
use gadk_memory::{
    AddEventsRequest, MemoryEntry, MemoryError, MemoryService, SearchRequest, add_events_to_memory,
};
use gadk_model::LlmResponse;
use gadk_model::ReqwestClient;
use gadk_model::gcp_auth::NO_RUNTIME;
use gadk_model::rest::test_util::{FakeResponse, FakeServer, RecordedRequest, bearer_credentials};
use gadk_platform::env::MapEnv;
use gadk_session::{Event, zero_time};
use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};

use super::rag::{
    VertexAiRagMemoryConfig, VertexAiRagMemoryService, memories_from_contexts, session_file,
};
use crate::session::VertexSession;

const CORPUS: &str = "projects/p/locations/us-central1/ragCorpora/c";
const PARENT: &str = "projects/p/locations/us-central1";
/// `parent=<corpus>` with the corpus query-escaped.
const ROUTING_CORPUS: &str = "parent=projects%2Fp%2Flocations%2Fus-central1%2FragCorpora%2Fc";
/// `parent=<project/location>` query-escaped.
const ROUTING_PARENT: &str = "parent=projects%2Fp%2Flocations%2Fus-central1";
/// The `$alt` query every `call` adds.
const ALT_QUERY: &str = "%24alt=json%3Benum-encoding%3Dint";

/// A config for a fake at `url`: no credentials, an empty environment.
fn config(url: &str) -> VertexAiRagMemoryConfig {
    VertexAiRagMemoryConfig::new(CORPUS)
        .with_endpoint(url)
        .without_authentication()
        .with_env(Arc::new(MapEnv(HashMap::new())))
}

fn service(config: VertexAiRagMemoryConfig) -> VertexAiRagMemoryService {
    VertexAiRagMemoryService::new(config).unwrap()
}

/// The time `secs` + `nanos` after the Unix epoch.
fn at(secs: i64, nanos: u32) -> DateTime<Utc> {
    DateTime::from_timestamp(secs, nanos).unwrap()
}

/// An event by `author` at `timestamp` whose content holds `parts`.
fn event(author: &str, timestamp: DateTime<Utc>, parts: Vec<Part>) -> Event {
    Event {
        author: author.into(),
        timestamp,
        response: LlmResponse {
            content: Some(Content::from_parts(parts, "model")),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// A thought part.
fn thought(text: &str) -> Part {
    Part {
        thought: true,
        ..Part::from_text(text)
    }
}

/// Session `id` of `user` in `app` holding `events`.
fn session(app: &str, user: &str, id: &str, events: Vec<Event>) -> VertexSession {
    let s = VertexSession::new(
        app.into(),
        user.into(),
        id.into(),
        Map::new(),
        DateTime::UNIX_EPOCH,
    );
    s.set_events(events);
    s
}

/// A session `app`/`u1`/`s1` with one text event.
fn text_session() -> VertexSession {
    session(
        "app",
        "u1",
        "s1",
        vec![event(
            "user",
            at(1_700_000_000, 500_000_000),
            vec![Part::from_text("hello")],
        )],
    )
}

/// The file [`text_session`] uploads.
const TEXT_FILE: &str = r#"{"author":"user","timestamp":1700000000.5,"text":"hello"}"#;

/// A fake RAG Engine: a list answers `files` (one page); a DELETE answers a pending operation
/// `<file>/operations/op`; a `/ui/` poll answers that operation done; any other request (the
/// upload) answers `upload`.
fn rag_server(files: Value, upload: Value) -> FakeServer {
    FakeServer::start(move |req: &RecordedRequest| {
        if req.method == "GET" && req.path.ends_with("/ragFiles") {
            FakeResponse::json(json!({ "ragFiles": files }).to_string())
        } else if req.method == "DELETE" {
            let name = req.path.trim_start_matches("/v1/");
            FakeResponse::json(json!({ "name": format!("{name}/operations/op") }).to_string())
        } else if let Some(op) = req.path.strip_prefix("/ui/") {
            FakeResponse::json(json!({ "name": op, "done": true }).to_string())
        } else {
            FakeResponse::json(upload.to_string())
        }
    })
}

/// A RAG file entry of a list reply.
fn file(id: &str, display: &str) -> Value {
    json!({ "name": format!("{CORPUS}/ragFiles/{id}"), "displayName": display })
}

/// The `Backend` error's text, or a panic.
fn backend_text(err: &MemoryError) -> String {
    match err {
        MemoryError::Backend(e) => e.to_string(),
        other => panic!("want Backend, got {other:?}"),
    }
}

/// Each request as `METHOD path?query`.
fn lines(server: &FakeServer) -> Vec<String> {
    server
        .requests()
        .iter()
        .map(|r| format!("{} {}", r.method, r.path_and_query()))
        .collect()
}

/// Asserts `req` is the multipart upload of `file` for display name `display`.
fn assert_upload(req: &RecordedRequest, display: &str, file: &str) {
    assert_eq!(req.method, "POST");
    assert_eq!(
        req.path_and_query(),
        format!("/upload/v1/{CORPUS}/ragFiles:upload")
    );
    assert_eq!(req.header("x-goog-upload-protocol"), "multipart");
    assert_eq!(req.header("x-goog-request-params"), ROUTING_CORPUS);
    let content_type = req.header("content-type");
    let boundary = content_type
        .strip_prefix("multipart/form-data; boundary=")
        .unwrap();
    let metadata = serde_json::to_string(
        &json!({"rag_file": {"display_name": display, "description": "gadk session memory"}}),
    )
    .unwrap();
    let want = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"metadata\"\r\nContent-Type: \
         application/json\r\n\r\n{metadata}\r\n--{boundary}\r\nContent-Disposition: form-data; \
         name=\"file\"; filename=\"{display}.txt\"\r\nContent-Type: text/plain\r\n\r\n{file}\r\n\
         --{boundary}--\r\n"
    );
    assert_eq!(req.body_text(), want);
}

// ---- R1, R2: config and construction ----

#[test]
fn config_debug_redacts_credentials_and_client() {
    let config = VertexAiRagMemoryConfig::new(CORPUS)
        .with_similarity_top_k(7)
        .with_vector_distance_threshold(0.25)
        .with_endpoint("http://127.0.0.1:1")
        .with_credentials(bearer_credentials("secret-token"))
        .without_authentication()
        .with_http_client(Arc::new(ReqwestClient::default()))
        .with_env(Arc::new(MapEnv(HashMap::new())));
    let text = format!("{config:?}");
    assert_eq!(
        text,
        "VertexAiRagMemoryConfig { rag_corpus: \"projects/p/locations/us-central1/ragCorpora/c\", \
         similarity_top_k: Some(7), vector_distance_threshold: Some(0.25), endpoint: \
         Some(\"http://127.0.0.1:1\"), credentials: true, without_authentication: true, \
         http_client: true, .. }"
    );
    assert!(!text.contains("secret-token"));
}

#[test]
fn new_rejects_malformed_corpus_names() {
    for name in [
        "",
        "projects/p",
        "projects/p/locations/l/ragCorpora/",
        "projects//locations/l/ragCorpora/c",
        "projects/p/locations//ragCorpora/c",
        "projects/p/locations/l/ragCorpora/c/extra",
        "project/p/locations/l/ragCorpora/c",
        "projects/p/locations/l/corpora/c",
    ] {
        let err = VertexAiRagMemoryService::new(
            VertexAiRagMemoryConfig::new(name).without_authentication(),
        )
        .err()
        .unwrap();
        match err {
            MemoryError::InvalidRequest(text) => assert_eq!(
                text,
                format!(
                    "invalid RAG corpus name {name}: expected \
                     projects/{{project}}/locations/{{location}}/ragCorpora/{{id}}"
                )
            ),
            other => panic!("{name}: want InvalidRequest, got {other:?}"),
        }
    }
    assert!(
        VertexAiRagMemoryService::new(
            VertexAiRagMemoryConfig::new(CORPUS).without_authentication()
        )
        .is_ok()
    );
}

/// An Application Default Credentials failure is `failed to create the RAG memory client: …`;
/// outside a Tokio runtime `new` refuses instead of panicking.
#[test]
fn new_wraps_adc_errors() {
    assert!(tokio::runtime::Handle::try_current().is_err());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("creds.json");
    let env = Arc::new(MapEnv(HashMap::from([(
        "GOOGLE_APPLICATION_CREDENTIALS".to_owned(),
        path.to_string_lossy().into_owned(),
    )])));
    let adc_config = || VertexAiRagMemoryConfig::new(CORPUS).with_env(env.clone());

    std::fs::write(&path, r#"{"type":"bogus"}"#).unwrap();
    let err = VertexAiRagMemoryService::new(adc_config()).err().unwrap();
    assert_eq!(
        backend_text(&err),
        "failed to create the RAG memory client: unknown credential type: bogus"
    );

    std::fs::write(&path, r#"{"type":"authorized_user"}"#).unwrap();
    let err = VertexAiRagMemoryService::new(adc_config()).err().unwrap();
    assert_eq!(
        backend_text(&err),
        format!("failed to create the RAG memory client: {NO_RUNTIME}")
    );
}

// ---- R3: file format ----

#[test]
fn session_file_has_one_json_line_per_text_event() {
    let events = vec![
        event(
            "user",
            at(1_700_000_000, 500_000_000),
            vec![Part::from_text("hello")],
        ),
        // Thoughts and empty parts are skipped; the rest join with "\n"; zero time is 0.
        event(
            "model",
            zero_time(),
            vec![
                thought("thinking"),
                Part::from_text("a"),
                Part::from_text(""),
                Part::from_text("b"),
            ],
        ),
        // No text: image-only, function-call-only, thought-only, no content.
        event(
            "user",
            at(1, 0),
            vec![Part::from_bytes(Bytes::from_static(b"png"), "image/png")],
        ),
        event(
            "model",
            at(2, 0),
            vec![Part::from_function_call("lookup", Map::new())],
        ),
        event("model", at(3, 0), vec![thought("only thinking")]),
        Event {
            author: "user".into(),
            timestamp: at(4, 0),
            ..Default::default()
        },
        // Quotes and newlines are escaped, so the line stays one line.
        event(
            "user",
            at(1_700_000_001, 0),
            vec![Part::from_text("say \"hi\"\nbye")],
        ),
    ];
    assert_eq!(
        session_file(&events),
        "{\"author\":\"user\",\"timestamp\":1700000000.5,\"text\":\"hello\"}\n\
         {\"author\":\"model\",\"timestamp\":0.0,\"text\":\"a\\nb\"}\n\
         {\"author\":\"user\",\"timestamp\":1700000001.0,\"text\":\"say \\\"hi\\\"\\nbye\"}"
    );
    assert_eq!(session_file(&[]), "");
}

// ---- R4: adding a session ----

#[tokio::test]
async fn add_session_lists_then_uploads_a_new_file() {
    let server = rag_server(
        json!([file("9", "app.u1.s2")]),
        json!({"ragFile": {"name": "x"}}),
    );
    let svc = service(config(&server.url()));
    svc.add_session_to_memory(&text_session()).await.unwrap();
    let reqs = server.requests();
    assert_eq!(
        lines(&server),
        vec![
            format!("GET /v1/{CORPUS}/ragFiles?{ALT_QUERY}"),
            format!("POST /upload/v1/{CORPUS}/ragFiles:upload"),
        ]
    );
    assert_eq!(reqs[0].header("x-goog-request-params"), ROUTING_CORPUS);
    assert!(reqs[0].headers.get("authorization").is_none());
    assert_upload(&reqs[1], "app.u1.s1", TEXT_FILE);
}

/// Every page is listed; each file with the session's display name is deleted and its
/// operation waited for, in list order, before the upload.
#[tokio::test]
async fn add_session_replaces_earlier_files_on_every_page() {
    let server = FakeServer::start(|req: &RecordedRequest| {
        let path = req.path.as_str();
        if req.method == "GET" && path.ends_with("/ragFiles") {
            let page = if req
                .query
                .as_deref()
                .unwrap_or_default()
                .contains("pageToken=t2")
            {
                json!({"ragFiles": [file("3", "app.u1.s1")]})
            } else {
                json!({
                    "ragFiles": [file("1", "app.u1.s1"), file("2", "app.u1.s10")],
                    "nextPageToken": "t2",
                })
            };
            FakeResponse::json(page.to_string())
        } else if req.method == "DELETE" {
            let name = path.trim_start_matches("/v1/");
            FakeResponse::json(json!({"name": format!("{name}/operations/op")}).to_string())
        } else if let Some(op) = path.strip_prefix("/ui/") {
            FakeResponse::json(json!({"name": op, "done": true}).to_string())
        } else {
            FakeResponse::json(r#"{"ragFile":{}}"#)
        }
    });
    let svc = service(config(&server.url()));
    svc.add_session_to_memory(&text_session()).await.unwrap();
    assert_eq!(
        lines(&server),
        vec![
            format!("GET /v1/{CORPUS}/ragFiles?{ALT_QUERY}"),
            format!("GET /v1/{CORPUS}/ragFiles?{ALT_QUERY}&pageToken=t2"),
            format!("DELETE /v1/{CORPUS}/ragFiles/1?{ALT_QUERY}"),
            format!("GET /ui/{CORPUS}/ragFiles/1/operations/op"),
            format!("DELETE /v1/{CORPUS}/ragFiles/3?{ALT_QUERY}"),
            format!("GET /ui/{CORPUS}/ragFiles/3/operations/op"),
            format!("POST /upload/v1/{CORPUS}/ragFiles:upload"),
        ]
    );
    let reqs = server.requests();
    assert_eq!(
        reqs[2].header("x-goog-request-params"),
        "name=projects%2Fp%2Flocations%2Fus-central1%2FragCorpora%2Fc%2FragFiles%2F1"
    );
    assert_upload(&reqs[6], "app.u1.s1", TEXT_FILE);
}

/// A session without text deletes its earlier file and uploads nothing.
#[tokio::test]
async fn add_session_without_text_only_deletes() {
    let server = rag_server(json!([file("1", "app.u1.s1")]), json!({}));
    let svc = service(config(&server.url()));
    let s = session(
        "app",
        "u1",
        "s1",
        vec![event("user", at(1, 0), vec![thought("hmm")])],
    );
    svc.add_session_to_memory(&s).await.unwrap();
    assert_eq!(
        lines(&server),
        vec![
            format!("GET /v1/{CORPUS}/ragFiles?{ALT_QUERY}"),
            format!("DELETE /v1/{CORPUS}/ragFiles/1?{ALT_QUERY}"),
            format!("GET /ui/{CORPUS}/ragFiles/1/operations/op"),
        ]
    );
}

/// A delete answered with 404 (already gone) counts as done: no wait, the upload follows.
#[tokio::test]
async fn add_session_treats_a_missing_file_as_deleted() {
    let server = FakeServer::start(|req: &RecordedRequest| {
        if req.method == "GET" {
            FakeResponse::json(json!({"ragFiles": [file("1", "app.u1.s1")]}).to_string())
        } else if req.method == "DELETE" {
            FakeResponse::json_status(
                404,
                r#"{"error":{"code":404,"message":"gone","status":"NOT_FOUND"}}"#,
            )
        } else {
            FakeResponse::json(r#"{"ragFile":{}}"#)
        }
    });
    let svc = service(config(&server.url()));
    svc.add_session_to_memory(&text_session()).await.unwrap();
    assert_eq!(
        lines(&server),
        vec![
            format!("GET /v1/{CORPUS}/ragFiles?{ALT_QUERY}"),
            format!("DELETE /v1/{CORPUS}/ragFiles/1?{ALT_QUERY}"),
            format!("POST /upload/v1/{CORPUS}/ragFiles:upload"),
        ]
    );
}

// ---- R5: id validation ----

#[tokio::test]
async fn add_session_rejects_dotted_or_empty_app_and_user_ids() {
    let server = rag_server(json!([]), json!({}));
    let svc = service(config(&server.url()));
    for (app, user) in [("a.b", "u1"), ("app", "u.v"), ("", "u1"), ("app", "")] {
        let err = svc
            .add_session_to_memory(&session(app, user, "s1", vec![]))
            .await
            .unwrap_err();
        match err {
            MemoryError::InvalidRequest(text) => assert_eq!(
                text,
                format!(
                    "invalid RAG memory scope: app name {app:?} and user id {user:?} must be \
                     non-empty and contain no '.'"
                )
            ),
            other => panic!("{app}/{user}: want InvalidRequest, got {other:?}"),
        }
    }
    assert_eq!(server.hits(), 0);
}

// ---- R6: search request ----

#[tokio::test]
async fn search_posts_retrieve_contexts_without_unset_options() {
    let server = FakeServer::json(r#"{"contexts":{}}"#);
    let svc = service(config(&server.url()));
    let resp = svc
        .search_memory(SearchRequest {
            query: "coffee".into(),
            user_id: "u1".into(),
            app_name: "app".into(),
        })
        .await
        .unwrap();
    assert!(resp.memories.is_empty());
    let req = server.last_request();
    assert_eq!(req.method, "POST");
    assert_eq!(
        req.path_and_query(),
        format!("/v1/{PARENT}:retrieveContexts?{ALT_QUERY}")
    );
    assert_eq!(req.header("x-goog-request-params"), ROUTING_PARENT);
    assert_eq!(req.header("content-type"), "application/json");
    assert_eq!(
        req.json(),
        json!({
            "vertexRagStore": {"ragResources": [{"ragCorpus": CORPUS}]},
            "query": {"text": "coffee"},
        })
    );
}

#[tokio::test]
async fn search_sends_top_k_and_the_distance_threshold_when_set() {
    let server = FakeServer::json("{}");
    let both = service(
        config(&server.url())
            .with_similarity_top_k(5)
            .with_vector_distance_threshold(0.5),
    );
    let top_k_only = service(config(&server.url()).with_similarity_top_k(3));
    let req = || SearchRequest {
        query: "q".into(),
        user_id: "u1".into(),
        app_name: "app".into(),
    };
    both.search_memory(req()).await.unwrap();
    top_k_only.search_memory(req()).await.unwrap();
    let reqs = server.requests();
    assert_eq!(
        reqs[0].json()["query"],
        json!({"text": "q", "ragRetrievalConfig": {"topK": 5, "filter": {"vectorDistanceThreshold": 0.5}}})
    );
    assert_eq!(
        reqs[1].json()["query"],
        json!({"text": "q", "ragRetrievalConfig": {"topK": 3}})
    );
}

#[tokio::test]
async fn search_with_an_empty_query_sends_nothing() {
    let server = FakeServer::json("{}");
    let svc = service(config(&server.url()));
    let resp = svc
        .search_memory(SearchRequest {
            query: String::new(),
            user_id: "u1".into(),
            app_name: "app".into(),
        })
        .await
        .unwrap();
    assert!(resp.memories.is_empty());
    assert_eq!(server.hits(), 0);
}

// ---- R7: parsing retrieved contexts ----

/// One memory entry as R7 builds it.
fn entry(display: &str, author: &str, timestamp: Option<DateTime<Utc>>, text: &str) -> MemoryEntry {
    let mut custom_metadata = Map::new();
    custom_metadata.insert("source_display_name".into(), Value::String(display.into()));
    MemoryEntry {
        id: String::new(),
        content: Some(Content::from_text(text, ROLE_USER)),
        author: author.into(),
        timestamp,
        custom_metadata,
    }
}

#[test]
fn contexts_are_filtered_parsed_and_deduplicated_in_order() {
    let a = r#"{"author":"user","timestamp":1700000000.5,"text":"I like coffee"}"#;
    let b = r#"{"author":"model","timestamp":0.0,"text":"Noted"}"#;
    let f = r#"{"author":"user","timestamp":1700000002.0,"text":"Tea too"}"#;
    let resp = json!({"contexts": {"contexts": [
        {"sourceDisplayName": "app.u1.s1", "text": format!(
            "{a}\n{{\"author\":\"model\",\"timest\nnot json\n{{\"author\":\"x\",\"timestamp\":1.0}}\n{b}"
        ), "score": 0.1},
        {"sourceDisplayName": "app.u2.s1", "text": a},
        {"sourceDisplayName": "other.u1.s1", "text": a},
        {"sourceDisplayName": "app.u10.s1", "text": f},
        {"sourceDisplayName": "app.u1.s2", "text": format!("{a}\n{f}")},
        {"text": a},
    ]}});
    assert_eq!(
        memories_from_contexts(&resp, "app.u1."),
        vec![
            entry(
                "app.u1.s1",
                "user",
                Some(at(1_700_000_000, 500_000_000)),
                "I like coffee"
            ),
            entry("app.u1.s1", "model", None, "Noted"),
            entry("app.u1.s2", "user", Some(at(1_700_000_002, 0)), "Tea too"),
        ]
    );
    assert!(memories_from_contexts(&json!({}), "app.u1.").is_empty());
}

#[tokio::test]
async fn search_returns_the_users_memories() {
    let line = r#"{"author":"user","timestamp":0.0,"text":"I like coffee"}"#;
    let server = FakeServer::json(
        json!({"contexts": {"contexts": [
            {"sourceDisplayName": "app.u1.s1", "text": line},
            {"sourceDisplayName": "app.u2.s1", "text": line},
        ]}})
        .to_string(),
    );
    let svc = service(config(&server.url()));
    let resp = svc
        .search_memory(SearchRequest {
            query: "coffee".into(),
            user_id: "u1".into(),
            app_name: "app".into(),
        })
        .await
        .unwrap();
    assert_eq!(
        resp.memories,
        vec![entry("app.u1.s1", "user", None, "I like coffee")]
    );
}

// ---- R8: error chains ----

fn google_error(status: u16, name: &str, message: &str) -> FakeResponse {
    FakeResponse::json_status(
        status,
        json!({"error": {"code": status, "message": message, "status": name}}).to_string(),
    )
}

#[tokio::test]
async fn failures_carry_their_step_in_the_error_chain() {
    // List fails.
    let server = FakeServer::always(google_error(500, "INTERNAL", "boom"));
    let err = service(config(&server.url()))
        .add_session_to_memory(&text_session())
        .await
        .unwrap_err();
    assert_eq!(
        backend_text(&err),
        "list rag files failed: rpc error: code = Internal desc = boom"
    );

    // Delete fails.
    let server = FakeServer::start(|req: &RecordedRequest| {
        if req.method == "GET" {
            FakeResponse::json(json!({"ragFiles": [file("1", "app.u1.s1")]}).to_string())
        } else {
            google_error(403, "PERMISSION_DENIED", "no")
        }
    });
    let err = service(config(&server.url()))
        .add_session_to_memory(&text_session())
        .await
        .unwrap_err();
    assert_eq!(
        backend_text(&err),
        format!(
            "delete rag file {CORPUS}/ragFiles/1 failed: rpc error: code = PermissionDenied \
             desc = no"
        )
    );

    // The delete operation finishes with an error.
    let server = FakeServer::start(|req: &RecordedRequest| {
        if req.method == "GET" && req.path.ends_with("/ragFiles") {
            FakeResponse::json(json!({"ragFiles": [file("1", "app.u1.s1")]}).to_string())
        } else {
            FakeResponse::json(
                json!({"name": "op", "done": true, "error": {"code": 9, "message": "busy"}})
                    .to_string(),
            )
        }
    });
    let err = service(config(&server.url()))
        .add_session_to_memory(&text_session())
        .await
        .unwrap_err();
    assert_eq!(
        backend_text(&err),
        format!(
            "delete rag file {CORPUS}/ragFiles/1 failed: rpc error: code = FailedPrecondition \
             desc = busy"
        )
    );

    // Upload fails with a status.
    let server = FakeServer::start(|req: &RecordedRequest| {
        if req.method == "GET" {
            FakeResponse::json(r#"{"ragFiles":[]}"#)
        } else {
            google_error(400, "INVALID_ARGUMENT", "bad")
        }
    });
    let err = service(config(&server.url()))
        .add_session_to_memory(&text_session())
        .await
        .unwrap_err();
    assert_eq!(
        backend_text(&err),
        "upload rag file failed: rpc error: code = InvalidArgument desc = bad"
    );

    // Upload answered 200 with an `error` object.
    let server = rag_server(
        json!([]),
        json!({"error": {"code": 3, "message": "bad file"}}),
    );
    let err = service(config(&server.url()))
        .add_session_to_memory(&text_session())
        .await
        .unwrap_err();
    assert_eq!(backend_text(&err), "upload rag file failed: bad file");

    // Retrieval fails.
    let server = FakeServer::always(google_error(500, "INTERNAL", "boom"));
    let err = service(config(&server.url()))
        .search_memory(SearchRequest {
            query: "q".into(),
            user_id: "u1".into(),
            app_name: "app".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(
        backend_text(&err),
        "retrieve contexts failed: rpc error: code = Internal desc = boom"
    );
}

// ---- R9 ----

#[tokio::test]
async fn adding_events_is_unsupported() {
    let server = FakeServer::json("{}");
    let svc = service(config(&server.url()));
    let err = add_events_to_memory(&svc, AddEventsRequest::default())
        .await
        .unwrap_err();
    assert!(matches!(err, MemoryError::AddEventsUnsupported), "{err:?}");
    assert_eq!(server.hits(), 0);
}
