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

//! Port of Go `server/adkrest/controllers/sessions_test.go`, plus byte checks of the session
//! handlers against the ADK-Go wire fixtures `testdata/wire/main_{05..11,66}_*.json`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use axum::body::Body;
use chrono::{DateTime, Utc};
use gadk_platform::Platform;
use gadk_server::authn::Caller;
use gadk_server::rest::PathVars;
use gadk_server::rest::controllers::SessionsApiController;
use gadk_server::rest::testutil::{FakeSessionService, SessionKey, TestSession};
use gadk_session::{
    CreateRequest, DeleteRequest, GetRequest, InMemorySessionService, ListRequest, SessionError,
    SessionService,
};
use http::{Method, StatusCode};
use parking_lot::Mutex;
use serde_json::{Map, Value, json};

type Request = http::Request<Body>;
type Response = axum::response::Response;

fn key(app: &str, user: &str, session: &str) -> SessionKey {
    SessionKey {
        app_name: app.into(),
        user_id: user.into(),
        session_id: session.into(),
    }
}

fn test_id() -> SessionKey {
    key("testApp", "testUser", "testSession")
}

fn state(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap()
}

/// Go `sessionVars`.
fn session_vars(id: &SessionKey) -> PathVars {
    PathVars(BTreeMap::from([
        ("app_name".to_owned(), id.app_name.clone()),
        ("user_id".to_owned(), id.user_id.clone()),
        ("session_id".to_owned(), id.session_id.clone()),
    ]))
}

/// A request carrying `vars` (Go `mux.SetURLVars`) and, when set, the authenticated caller
/// (Go `authn.WithCaller`).
fn request(method: Method, vars: PathVars, body: &str, caller: Option<&str>) -> Request {
    let mut req = http::Request::builder()
        .method(method)
        .uri("/")
        .body(Body::from(body.to_owned()))
        .unwrap();
    req.extensions_mut().insert(vars);
    if let Some(user) = caller {
        req.extensions_mut().insert(Caller {
            user_id: user.into(),
            claims: None,
        });
    }
    req
}

/// Go `newSessionRequest`.
fn new_session_request(method: Method, id: &SessionKey, body: &str, user: &str) -> Request {
    request(
        method,
        session_vars(id),
        body,
        (!user.is_empty()).then_some(user),
    )
}

/// Go `newServiceWithSession`: an in-memory service holding one session.
async fn new_service_with_session(
    id: &SessionKey,
    state: Map<String, Value>,
) -> Arc<InMemorySessionService> {
    let service = Arc::new(InMemorySessionService::new());
    service
        .create(CreateRequest {
            app_name: id.app_name.clone(),
            user_id: id.user_id.clone(),
            session_id: Some(id.session_id.clone()),
            state: Some(state),
        })
        .await
        .unwrap();
    service
}

async fn read(resp: Response) -> (StatusCode, String) {
    let status = resp.status();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

/// Go `json.Decode` into `any`: every number is a `float64`.
fn go_any(v: Value) -> Value {
    match v {
        Value::Number(n) => n.as_f64().map(Value::from).unwrap_or(Value::Null),
        Value::Array(a) => Value::Array(a.into_iter().map(go_any).collect()),
        Value::Object(o) => Value::Object(o.into_iter().map(|(k, v)| (k, go_any(v))).collect()),
        other => other,
    }
}

/// A response body decoded as Go decodes it into `models.Session` (`state` values are `any`).
fn decode(body: &str) -> Value {
    let mut v: Value =
        serde_json::from_str(body).unwrap_or_else(|e| panic!("decode response {body:?}: {e}"));
    if let Some(state) = v.get_mut("state") {
        *state = go_any(state.take());
    }
    v
}

/// Go `EquateApproxInt(int64(time.Second))` on `lastUpdateTime`: within one second of now.
fn assert_recent(session: &Value, case: &str) {
    let t = session["lastUpdateTime"].as_i64().unwrap();
    assert!(
        (Utc::now().timestamp() - t).abs() <= 1,
        "{case}: lastUpdateTime {t} is not now"
    );
}

fn stored(id: SessionKey, s: Value) -> TestSession {
    TestSession::new(id, state(s), vec![], Utc::now())
}

#[tokio::test]
async fn test_get_session() {
    let id = test_id();
    struct Case {
        name: &'static str,
        stored: Vec<(SessionKey, TestSession)>,
        session_id: SessionKey,
        want_session: Option<Value>,
        want_err: &'static str,
        want_status: StatusCode,
    }
    let cases = [
        Case {
            name: "session exists",
            stored: vec![(id.clone(), stored(id.clone(), json!({"foo": "bar"})))],
            session_id: id.clone(),
            want_session: Some(json!({
                "id": "testSession", "appName": "testApp", "userId": "testUser",
                "events": [], "state": {"foo": "bar"},
            })),
            want_err: "",
            want_status: StatusCode::OK,
        },
        Case {
            // The fake reports a missing session with SessionError::NotFound, as the real
            // services do, so the handler answers 404. A service failure that is not a missing
            // session stays a 500: see test_get_session_service_failure_is_internal_error.
            name: "missing session is not found",
            stored: vec![],
            session_id: id.clone(),
            want_session: None,
            want_err: "session not found: \"testSession\"",
            want_status: StatusCode::NOT_FOUND,
        },
        Case {
            name: "user ID is missing in input",
            stored: vec![(id.clone(), stored(id.clone(), json!({"foo": "bar"})))],
            session_id: key("testApp", "", "testSession"),
            want_session: None,
            want_err: "user_id parameter is required",
            want_status: StatusCode::BAD_REQUEST,
        },
        Case {
            name: "session ID is missing",
            stored: vec![(
                id.clone(),
                stored(key("testApp", "testUser", ""), json!({"foo": "bar"})),
            )],
            session_id: id.clone(),
            want_session: None,
            want_err: "session_id is empty in received session",
            want_status: StatusCode::INTERNAL_SERVER_ERROR,
        },
    ];
    for tt in cases {
        let service = Arc::new(FakeSessionService {
            sessions: Mutex::new(tt.stored.into_iter().collect()),
        });
        let controller = SessionsApiController::new(service);
        let req = request(
            Method::GET,
            session_vars(&tt.session_id),
            "",
            Some("testUser"),
        );
        let (status, body) = read(controller.get_session_handler(req).await).await;
        assert_eq!(
            status, tt.want_status,
            "{}: status (body {body:?})",
            tt.name
        );
        if !tt.want_err.is_empty() {
            assert_eq!(body.trim_matches('\n'), tt.want_err, "{}", tt.name);
            continue;
        }
        let mut got = decode(&body);
        assert_recent(&got, tt.name);
        got.as_object_mut().unwrap().remove("lastUpdateTime");
        assert_eq!(Some(got), tt.want_session, "{}", tt.name);
    }
}

#[tokio::test]
async fn test_create_session() {
    let id = test_id();
    struct Case {
        name: &'static str,
        stored: Vec<TestSession>,
        session_id: SessionKey,
        body: Value,
        want_session: Option<Value>,
        want_err: &'static str,
        want_status: StatusCode,
    }
    let cases = [
        Case {
            name: "session exists",
            stored: vec![stored(id.clone(), json!({"foo": "bar"}))],
            session_id: id.clone(),
            body: json!({}),
            want_session: None,
            want_err: "session already exists",
            want_status: StatusCode::INTERNAL_SERVER_ERROR,
        },
        Case {
            name: "successful create operation",
            stored: vec![],
            session_id: id.clone(),
            body: json!({
                "state": {"foo": "bar"},
                "events": [{"id": "eventID", "author": "testUser"}],
            }),
            want_session: Some(json!({
                "id": "testSession", "appName": "testApp", "userId": "testUser",
                "state": {"foo": "bar"},
            })),
            want_err: "",
            want_status: StatusCode::OK,
        },
        Case {
            name: "user id is missing",
            stored: vec![],
            session_id: key("testApp", "", "testSession"),
            body: json!({}),
            want_session: None,
            want_err: "user_id parameter is required",
            want_status: StatusCode::BAD_REQUEST,
        },
    ];
    for tt in cases {
        let service = Arc::new(FakeSessionService::new(tt.stored));
        let controller = SessionsApiController::new(service);
        let req = request(
            Method::POST,
            session_vars(&tt.session_id),
            &tt.body.to_string(),
            Some("testUser"),
        );
        let (status, body) = read(controller.create_session_handler(req).await).await;
        assert_eq!(
            status, tt.want_status,
            "{}: status (body {body:?})",
            tt.name
        );
        if !tt.want_err.is_empty() {
            assert_eq!(body.trim_matches('\n'), tt.want_err, "{}", tt.name);
            continue;
        }
        let mut got = decode(&body);
        let obj = got.as_object_mut().unwrap();
        // Go ignores UpdatedAt here (cmpopts.IgnoreFields) and compares the events' set fields.
        obj.remove("lastUpdateTime");
        let events = obj.remove("events").unwrap();
        let events = events.as_array().unwrap();
        assert_eq!(events.len(), 1, "{}: events {events:?}", tt.name);
        assert_eq!(events[0]["id"], "eventID", "{}", tt.name);
        assert_eq!(events[0]["author"], "testUser", "{}", tt.name);
        assert_eq!(Some(got), tt.want_session, "{}", tt.name);
    }
}

#[tokio::test]
async fn test_delete_session() {
    let id = test_id();
    struct Case {
        name: &'static str,
        stored: Vec<TestSession>,
        want_status: StatusCode,
    }
    let cases = [
        Case {
            name: "session exists",
            stored: vec![stored(id.clone(), json!({"foo": "bar"}))],
            want_status: StatusCode::OK,
        },
        Case {
            name: "session does not exist",
            stored: vec![],
            want_status: StatusCode::INTERNAL_SERVER_ERROR,
        },
    ];
    for tt in cases {
        let service = Arc::new(FakeSessionService::new(tt.stored));
        let controller = SessionsApiController::new(service.clone());
        let req = request(Method::DELETE, session_vars(&id), "", Some("testUser"));
        let (status, body) = read(controller.delete_session_handler(req).await).await;
        assert_eq!(
            status, tt.want_status,
            "{}: status (body {body:?})",
            tt.name
        );
        assert!(
            service.session(&id).is_none(),
            "{}: session was not deleted",
            tt.name
        );
    }
}

#[tokio::test]
async fn test_list_sessions() {
    let id = test_id();
    let new_id = key("testApp", "testUser", "newSession");
    let old_id = key("testApp", "testUser", "oldSession");
    struct Case {
        name: &'static str,
        stored: Vec<TestSession>,
        want_sessions: Value,
        want_status: StatusCode,
    }
    let cases = [Case {
        name: "session exists",
        stored: vec![
            stored(id.clone(), json!({"foo": "bar"})),
            stored(new_id, json!({"xyz": "abc"})),
            stored(old_id, json!({})),
        ],
        want_sessions: json!([
            {"id": "newSession", "appName": "testApp", "userId": "testUser", "events": [], "state": {"xyz": "abc"}},
            {"id": "oldSession", "appName": "testApp", "userId": "testUser", "events": [], "state": {}},
            {"id": "testSession", "appName": "testApp", "userId": "testUser", "events": [], "state": {"foo": "bar"}},
        ]),
        want_status: StatusCode::OK,
    }];
    for tt in cases {
        let controller = SessionsApiController::new(Arc::new(FakeSessionService::new(tt.stored)));
        let vars = PathVars(BTreeMap::from([
            ("app_name".to_owned(), "testApp".to_owned()),
            ("user_id".to_owned(), "testUser".to_owned()),
        ]));
        let req = request(Method::DELETE, vars, "", Some("testUser"));
        let (status, body) = read(controller.list_sessions_handler(req).await).await;
        assert_eq!(
            status, tt.want_status,
            "{}: status (body {body:?})",
            tt.name
        );
        let mut got: Vec<Value> = serde_json::from_str(&body).unwrap();
        for s in &mut got {
            assert_recent(s, tt.name);
            s.as_object_mut().unwrap().remove("lastUpdateTime");
        }
        // Go: cmpopts.SortSlices by ID.
        got.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        assert_eq!(Value::Array(got), tt.want_sessions, "{}", tt.name);
    }
}

/// A service that reports a missing session with `SessionError::NotFound` answers 404.
#[tokio::test]
async fn test_get_session_missing_session_is_not_found() {
    let id = key("testApp", "testUser", "missingSession");
    let controller = SessionsApiController::new(Arc::new(InMemorySessionService::new()));
    let (status, body) = read(
        controller
            .get_session_handler(new_session_request(Method::GET, &id, "", &id.user_id))
            .await,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");
}

/// Go `getFailingService`: `Get` fails with an error that is not a missing session.
struct GetFailingService;

#[async_trait::async_trait]
impl SessionService for GetFailingService {
    async fn create(
        &self,
        _req: CreateRequest,
    ) -> Result<Arc<dyn gadk_session::Session>, SessionError> {
        Err(SessionError::Backend("not implemented".into()))
    }
    async fn get(&self, _req: GetRequest) -> Result<Arc<dyn gadk_session::Session>, SessionError> {
        Err(SessionError::Backend("session store unavailable".into()))
    }
    async fn list(
        &self,
        _req: ListRequest,
    ) -> Result<Vec<Arc<dyn gadk_session::Session>>, SessionError> {
        Err(SessionError::Backend("not implemented".into()))
    }
    async fn delete(&self, _req: DeleteRequest) -> Result<(), SessionError> {
        Err(SessionError::Backend("not implemented".into()))
    }
    async fn append_event(
        &self,
        _session: &dyn gadk_session::Session,
        _event: &mut gadk_session::Event,
    ) -> Result<(), SessionError> {
        Err(SessionError::Backend("not implemented".into()))
    }
}

#[tokio::test]
async fn test_get_session_service_failure_is_internal_error() {
    let id = test_id();
    let controller = SessionsApiController::new(Arc::new(GetFailingService));
    let (status, body) = read(
        controller
            .get_session_handler(new_session_request(Method::GET, &id, "", &id.user_id))
            .await,
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "body: {body}");
}

#[tokio::test]
async fn test_get_session_existing_session_is_ok() {
    let id = test_id();
    let controller = SessionsApiController::new(
        new_service_with_session(&id, state(json!({"foo": "bar"}))).await,
    );
    let (status, body) = read(
        controller
            .get_session_handler(new_session_request(Method::GET, &id, "", &id.user_id))
            .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(decode(&body)["id"], id.session_id.as_str());
}

/// The raw bytes: a user without sessions gets `[]`, never `null`.
#[tokio::test]
async fn test_list_sessions_without_sessions_encodes_empty_array() {
    let controller = SessionsApiController::new(Arc::new(FakeSessionService::default()));
    let vars = PathVars(BTreeMap::from([
        ("app_name".to_owned(), "testApp".to_owned()),
        ("user_id".to_owned(), "testUser".to_owned()),
    ]));
    let (status, body) = read(
        controller
            .list_sessions_handler(request(Method::GET, vars, "", Some("testUser")))
            .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.trim_end_matches('\n'), "[]");
}

#[tokio::test]
async fn test_update_session() {
    let id = test_id();
    struct Case {
        name: &'static str,
        create_session: bool,
        session_id: SessionKey,
        body: &'static str,
        want_status: StatusCode,
        want_state: Value,
    }
    let cases = [
        Case {
            name: "state delta is applied",
            create_session: true,
            session_id: id.clone(),
            body: r#"{"stateDelta":{"foo":"baz","count":2}}"#,
            want_status: StatusCode::OK,
            want_state: json!({"foo": "baz", "count": 2.0}),
        },
        Case {
            // The rename path: the UI stores the new name in session state.
            name: "session metadata round-trips",
            create_session: true,
            session_id: id.clone(),
            body: r#"{"stateDelta":{"__session_metadata__":{"displayName":"My renamed chat"}}}"#,
            want_status: StatusCode::OK,
            want_state: json!({
                "foo": "bar",
                "__session_metadata__": {"displayName": "My renamed chat"},
            }),
        },
        Case {
            name: "empty delta leaves the session unchanged",
            create_session: true,
            session_id: id.clone(),
            body: "{}",
            want_status: StatusCode::OK,
            want_state: json!({"foo": "bar"}),
        },
        Case {
            name: "absent delta leaves the session unchanged",
            create_session: true,
            session_id: id.clone(),
            body: r#"{"stateDelta":{}}"#,
            want_status: StatusCode::OK,
            want_state: json!({"foo": "bar"}),
        },
        Case {
            name: "session does not exist",
            create_session: false,
            session_id: id.clone(),
            body: r#"{"stateDelta":{"foo":"baz"}}"#,
            want_status: StatusCode::NOT_FOUND,
            want_state: Value::Null,
        },
        Case {
            name: "malformed json",
            create_session: true,
            session_id: id.clone(),
            body: r#"{"stateDelta":"#,
            want_status: StatusCode::BAD_REQUEST,
            want_state: Value::Null,
        },
        Case {
            name: "user ID is missing in input",
            create_session: true,
            session_id: key("testApp", "", "testSession"),
            body: r#"{"stateDelta":{"foo":"baz"}}"#,
            want_status: StatusCode::BAD_REQUEST,
            want_state: Value::Null,
        },
    ];
    for tt in cases {
        let service = if tt.create_session {
            new_service_with_session(&id, state(json!({"foo": "bar"}))).await
        } else {
            Arc::new(InMemorySessionService::new())
        };
        let controller = SessionsApiController::new(service);
        let (status, body) = read(
            controller
                .update_session_handler(new_session_request(
                    Method::PATCH,
                    &tt.session_id,
                    tt.body,
                    &id.user_id,
                ))
                .await,
        )
        .await;
        assert_eq!(
            status, tt.want_status,
            "{}: status (body {body:?})",
            tt.name
        );
        if tt.want_status != StatusCode::OK {
            continue;
        }
        assert_eq!(
            decode(&body)["state"],
            tt.want_state,
            "{}: response state",
            tt.name
        );

        // A follow-up GET shows the same state: the delta reached the store.
        let (status, body) = read(
            controller
                .get_session_handler(new_session_request(
                    Method::GET,
                    &tt.session_id,
                    "",
                    &id.user_id,
                ))
                .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{}: get (body {body:?})", tt.name);
        assert_eq!(
            decode(&body)["state"],
            tt.want_state,
            "{}: re-read state",
            tt.name
        );
    }
}

/// The delta is applied through the service as an appended event.
#[tokio::test]
async fn test_update_session_applies_delta_through_an_event() {
    let id = test_id();
    let service = new_service_with_session(&id, state(json!({"foo": "bar"}))).await;
    let controller = SessionsApiController::new(service.clone());
    let (status, body) = read(
        controller
            .update_session_handler(new_session_request(
                Method::PATCH,
                &id,
                r#"{"stateDelta":{"foo":"baz"}}"#,
                &id.user_id,
            ))
            .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let stored = service
        .get(GetRequest {
            app_name: id.app_name.clone(),
            user_id: id.user_id.clone(),
            session_id: id.session_id.clone(),
            ..Default::default()
        })
        .await
        .unwrap();
    let events = stored.events();
    assert_eq!(events.len(), 1, "want 1 event carrying the delta");
    assert_eq!(
        events[0].actions.state_delta,
        Some(state(json!({"foo": "baz"})))
    );
}

#[tokio::test]
async fn test_update_session_writes_an_unchanged_plain_key() {
    let id = test_id();
    let service = new_service_with_session(&id, state(json!({"theme": "dark"}))).await;
    let controller = SessionsApiController::new(service.clone());
    let (status, body) = read(
        controller
            .update_session_handler(new_session_request(
                Method::PATCH,
                &id,
                r#"{"stateDelta":{"theme":"dark"}}"#,
                &id.user_id,
            ))
            .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let stored = service
        .get(GetRequest {
            app_name: id.app_name.clone(),
            user_id: id.user_id.clone(),
            session_id: id.session_id.clone(),
            ..Default::default()
        })
        .await
        .unwrap();
    let events = stored.events();
    assert_eq!(
        events.len(),
        1,
        "session has {} events, want 1 carrying the unchanged key",
        events.len()
    );
    assert_eq!(
        events[0].actions.state_delta,
        Some(state(json!({"theme": "dark"})))
    );
}

/// The PATCH response is what a GET answers.
#[tokio::test]
async fn test_update_session_response_matches_get_session() {
    let id = test_id();
    let controller = SessionsApiController::new(
        new_service_with_session(&id, state(json!({"foo": "bar"}))).await,
    );
    let (status, patched) = read(
        controller
            .update_session_handler(new_session_request(
                Method::PATCH,
                &id,
                r#"{"stateDelta":{"foo":"baz"}}"#,
                &id.user_id,
            ))
            .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {patched}");
    let (status, fetched) = read(
        controller
            .get_session_handler(new_session_request(Method::GET, &id, "", &id.user_id))
            .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {fetched}");
    assert_eq!(decode(&fetched), decode(&patched));
}

/// An in-memory service holding one session for `app`/`user` with a service-assigned ID.
async fn created(state: Map<String, Value>) -> (Arc<InMemorySessionService>, String) {
    let service = Arc::new(InMemorySessionService::new());
    let session = service
        .create(CreateRequest {
            app_name: "app".into(),
            user_id: "user".into(),
            state: Some(state),
            ..Default::default()
        })
        .await
        .unwrap();
    let session_id = session.id().to_owned();
    (service, session_id)
}

/// PATCH `{"stateDelta": delta}` on `app`/`user`/`session_id` as caller `user`.
async fn patch_delta(
    controller: &SessionsApiController,
    session_id: &str,
    delta: Value,
) -> (StatusCode, String) {
    let body = json!({ "stateDelta": delta }).to_string();
    let req = request(
        Method::PATCH,
        session_vars(&key("app", "user", session_id)),
        &body,
        Some("user"),
    );
    read(controller.update_session_handler(req).await).await
}

/// The tenancy boundary: a delta may not write app-, user- or temp-scoped state.
#[tokio::test]
async fn test_update_session_rejects_scoped_state_keys() {
    let cases = [
        (
            "app-scoped key",
            json!({"app:shared": "x"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "user-scoped key",
            json!({"user:shared": "x"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "temp-scoped key",
            json!({"temp:scratch": "x"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "scoped mixed with a session key",
            json!({"mine": "ok", "app:shared": "x"}),
            StatusCode::BAD_REQUEST,
        ),
        ("session-scoped key", json!({"mine": "ok"}), StatusCode::OK),
        (
            "the UI's rename key",
            json!({"__session_metadata__": {"displayName": "n"}}),
            StatusCode::OK,
        ),
    ];
    for (name, delta, want) in cases {
        let (service, session_id) = created(Map::new()).await;
        let controller = SessionsApiController::new(service);
        let (status, body) = patch_delta(&controller, &session_id, delta).await;
        assert_eq!(status, want, "{name}: body {body:?}");
    }
}

/// The web UI's rename posts back every scoped key it read; an unchanged echo is accepted, a
/// changed scoped value is still refused.
#[tokio::test]
async fn test_update_session_accepts_echoed_scoped_state_keys() {
    let stored_state = state(json!({
        "plain": "kept",
        "app:config": {"theme": "dark"},
        "user:locale": "en",
        "app:count": 7,
    }));
    struct Case {
        name: &'static str,
        delta: Value,
        want_status: StatusCode,
        want_rejected: &'static [&'static str],
    }
    let cases = [
        Case {
            name: "the UI's rename payload echoes every scoped key back",
            delta: json!({
                "plain": "kept",
                "app:config": {"theme": "dark"},
                "user:locale": "en",
                "app:count": 7,
                "__session_metadata__": {"displayName": "renamed"},
            }),
            want_status: StatusCode::OK,
            want_rejected: &[],
        },
        Case {
            name: "an echoed int survives the client's JSON round-trip",
            delta: json!({"app:count": 7.0}),
            want_status: StatusCode::OK,
            want_rejected: &[],
        },
        Case {
            name: "changing an echoed scoped key is still refused",
            delta: json!({"app:config": {"theme": "light"}}),
            want_status: StatusCode::BAD_REQUEST,
            want_rejected: &["app:config"],
        },
        Case {
            name: "only the changed key is named, not the echoed ones",
            delta: json!({"app:config": {"theme": "dark"}, "user:locale": "fr"}),
            want_status: StatusCode::BAD_REQUEST,
            want_rejected: &["user:locale"],
        },
        Case {
            name: "a scoped key the session does not hold is a write",
            delta: json!({"app:brandnew": "x"}),
            want_status: StatusCode::BAD_REQUEST,
            want_rejected: &["app:brandnew"],
        },
    ];
    for tt in cases {
        let (service, session_id) = created(stored_state.clone()).await;
        let controller = SessionsApiController::new(service);
        let (status, body) = patch_delta(&controller, &session_id, tt.delta).await;
        assert_eq!(status, tt.want_status, "{}: body {body:?}", tt.name);
        if tt.want_status != StatusCode::OK {
            for k in tt.want_rejected {
                assert!(
                    body.contains(k),
                    "{}: {body:?} does not name {k:?}",
                    tt.name
                );
            }
            // An echoed key is not blamed for someone else's write.
            for k in ["plain", "__session_metadata__"] {
                assert!(!body.contains(k), "{}: {body:?} names {k:?}", tt.name);
            }
            continue;
        }
        let got = decode(&body);
        // The scoped keys come back untouched: filtering them out of the delta did not
        // delete them.
        for (k, want) in [
            ("app:config", json!({"theme": "dark"})),
            ("user:locale", json!("en")),
            ("app:count", json!(7.0)),
            ("plain", json!("kept")),
        ] {
            assert_eq!(got["state"][k], want, "{}: state[{k:?}]", tt.name);
        }
    }
}

/// End to end: rename a session of an agent that uses scoped state, then read the name back.
#[tokio::test]
async fn test_update_session_rename_applies_with_scoped_state_in_play() {
    let (service, session_id) =
        created(state(json!({"app:config": "shared", "user:locale": "en"}))).await;
    let controller = SessionsApiController::new(service.clone());
    let (status, body) = patch_delta(
        &controller,
        &session_id,
        json!({
            "app:config": "shared",
            "user:locale": "en",
            "__session_metadata__": {"displayName": "My renamed chat"},
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "rename: body {body:?}");
    let reread = service
        .get(GetRequest {
            app_name: "app".into(),
            user_id: "user".into(),
            session_id,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        reread.state().get("__session_metadata__").unwrap(),
        json!({"displayName": "My renamed chat"})
    );
}

// parity: skipped TestUpdateSessionEchoedRedactedCredentials: 8c — sign-in (Go 1523862f #2231: credential secrets redacted from session reads, and a rename echoing the redacted state keeps the stored secrets)

// ---- Authorization (Go `CanActAsUser` before any service call). ----

/// Every session handler answers 403 `forbidden: authz: unauthorized: …` when the caller may not
/// act as the path's user (Go `authz.WriteHTTPStatusForAuthError`).
#[tokio::test]
async fn session_handlers_enforce_the_authorizer() {
    let id = test_id();
    let service = new_service_with_session(&id, Map::new()).await;
    let controller =
        SessionsApiController::new(service).with_authorizer(gadk_server::authz::strict());
    let want = "forbidden: authz: unauthorized: caller doesn't match the provided userID\n";
    for (name, resp) in [
        (
            "get",
            controller
                .get_session_handler(new_session_request(Method::GET, &id, "", "mallory"))
                .await,
        ),
        (
            "create",
            controller
                .create_session_handler(new_session_request(Method::POST, &id, "", "mallory"))
                .await,
        ),
        (
            "update",
            controller
                .update_session_handler(new_session_request(Method::PATCH, &id, "{}", "mallory"))
                .await,
        ),
        (
            "delete",
            controller
                .delete_session_handler(new_session_request(Method::DELETE, &id, "", "mallory"))
                .await,
        ),
        (
            "list",
            controller
                .list_sessions_handler(new_session_request(Method::GET, &id, "", "mallory"))
                .await,
        ),
    ] {
        let (status, body) = read(resp).await;
        assert_eq!(
            (status, body.as_str()),
            (StatusCode::FORBIDDEN, want),
            "{name}"
        );
    }
}

// ---- ADK-Go wire bytes (testdata/wire, config `main`). ----

/// A fixture's request and response.
struct Fixture {
    method: Method,
    body: String,
    status: u16,
    headers: Vec<(String, String)>,
    resp_body: String,
}

fn fixture(name: &str) -> Fixture {
    let path = format!("{}/testdata/wire/{name}.json", env!("CARGO_MANIFEST_DIR"));
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    Fixture {
        method: v["request"]["method"].as_str().unwrap().parse().unwrap(),
        body: v["request"]["body"].as_str().unwrap().to_owned(),
        status: u16::try_from(v["response"]["status"].as_u64().unwrap()).unwrap(),
        headers: v["response"]["headers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| {
                (
                    h[0].as_str().unwrap().to_owned(),
                    h[1].as_str().unwrap().to_owned(),
                )
            })
            .collect(),
        resp_body: v["response"]["body"].as_str().unwrap().to_owned(),
    }
}

/// Status, headers (Go canonical names, sorted) and body of `resp`.
async fn wire(resp: Response) -> (u16, Vec<(String, String)>, String) {
    let status = resp.status().as_u16();
    let mut headers: Vec<(String, String)> = resp
        .headers()
        .iter()
        .map(|(k, v)| {
            let canonical = k
                .as_str()
                .split('-')
                .map(|w| {
                    let mut c = w.chars();
                    c.next()
                        .map(|f| f.to_ascii_uppercase().to_string() + c.as_str())
                        .unwrap_or_default()
                })
                .collect::<Vec<_>>()
                .join("-");
            (canonical, v.to_str().unwrap().to_owned())
        })
        .collect();
    headers.sort();
    let (_, body) = read(resp).await;
    (status, headers, body)
}

/// The probe's fixed clock (`2026-01-02T03:04:05Z`, one second per read) and UUID sequence
/// (`00000000-0000-4000-8000-%012d` from 1), shared by the service and the handlers as the
/// probe's request-context providers are.
fn scenario_platform() -> Platform {
    let secs = Arc::new(AtomicI64::new(1_767_323_045));
    let ids = Arc::new(AtomicU64::new(1));
    Platform::default()
        .with_time(move || -> DateTime<Utc> {
            DateTime::from_timestamp(secs.fetch_add(1, Ordering::SeqCst), 0).unwrap()
        })
        .with_uuid(move || {
            format!(
                "00000000-0000-4000-8000-{:012}",
                ids.fetch_add(1, Ordering::SeqCst)
            )
        })
}

/// Replays `main_05` … `main_11` and `main_66` (session create with and without an ID, get,
/// list, PATCH with an event from the request platform, the scoped-key refusal, the missing
/// session, delete) and compares status, headers and body bytes with ADK-Go's.
#[tokio::test]
async fn session_handlers_match_go_wire_fixtures() {
    let platform = scenario_platform();
    let service = Arc::new(InMemorySessionService::with_platform(platform.clone()));
    let controller = SessionsApiController::new(service);
    let alice = |session: &str| {
        let mut vars = BTreeMap::from([
            ("app_name".to_owned(), "assistant".to_owned()),
            ("user_id".to_owned(), "alice".to_owned()),
        ]);
        if !session.is_empty() {
            vars.insert("session_id".to_owned(), session.to_owned());
        }
        PathVars(vars)
    };
    let steps: [(&str, &str); 7] = [
        ("main_05_session_create_with_id", "s1"),
        ("main_06_session_create_without_id", ""),
        ("main_07_session_get", "s1"),
        ("main_08_session_list", ""),
        ("main_09_session_patch", "s1"),
        ("main_10_session_patch_scoped", "s1"),
        ("main_11_session_get_missing", "missing"),
    ];
    for (name, session) in steps {
        let f = fixture(name);
        let mut req = request(f.method.clone(), alice(session), &f.body, None);
        req.extensions_mut().insert(platform.clone());
        let resp = match (f.method.as_str(), session.is_empty()) {
            ("POST", _) => controller.create_session_handler(req).await,
            ("GET", true) => controller.list_sessions_handler(req).await,
            ("GET", false) => controller.get_session_handler(req).await,
            ("PATCH", _) => controller.update_session_handler(req).await,
            other => panic!("{name}: unexpected {other:?}"),
        };
        assert_eq!(
            wire(resp).await,
            (f.status, f.headers, f.resp_body),
            "{name}"
        );
    }

    // main_66: a delete answers 200 with the JSON Content-Type and an empty body.
    let f = fixture("main_66_session_delete");
    let resp = controller
        .delete_session_handler(request(Method::DELETE, alice("s1"), "", None))
        .await;
    assert_eq!(
        wire(resp).await,
        (f.status, f.headers, f.resp_body),
        "main_66"
    );
}
