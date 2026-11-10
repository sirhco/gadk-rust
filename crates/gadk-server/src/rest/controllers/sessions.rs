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

//! Port of Go `controllers/sessions.go` and `internal/routers/sessions.go` (plan 5a Task 6).

use std::sync::Arc;

use gadk_model::to_go_json;
use gadk_platform::go_quote;
use gadk_session::{
    CreateRequest, DeleteRequest, GetRequest, KEY_PREFIX_APP, KEY_PREFIX_TEMP, KEY_PREFIX_USER,
    ListRequest, SessionService, State,
};
use http::{Method, StatusCode};
use serde_json::{Map, Value};

use super::handlers::{
    decode_optional_json_body, encode_nil_json_response, forbid_unless_can_act_as, path_vars,
    request_platform, session_error_text,
};
use crate::authz::{self, Authorizer};
use crate::rest::http_error::{encode_json_response, http_error};
use crate::rest::models::{
    CreateSessionRequest, Session, SessionId, UpdateSessionRequest, session_id_from_http_parameters,
};
use crate::rest::routing::{Request, Response, Route, controller_handler, route};

/// Port of Go `controllers.SessionsAPIController`.
pub struct SessionsApiController {
    service: Arc<dyn SessionService>,
    authorizer: Arc<dyn Authorizer>,
}

impl SessionsApiController {
    /// Go `NewSessionsAPIController` (authorizer: [`authz::noop`]).
    pub fn new(service: Arc<dyn SessionService>) -> Self {
        Self {
            service,
            authorizer: authz::noop(),
        }
    }

    /// Go `WithAuthorizer`.
    pub fn with_authorizer(mut self, a: Arc<dyn Authorizer>) -> Self {
        self.authorizer = a;
        self
    }

    /// Go `CreateSessionHandler`: creates the session (with the path's session ID, or one the
    /// service assigns), appends the request's events and answers the session.
    pub async fn create_session_handler(&self, req: Request) -> Response {
        let sid = match session_id_from_http_parameters(&path_vars(&req)) {
            Ok(sid) => sid,
            Err(e) => return http_error(StatusCode::BAD_REQUEST, &e),
        };
        if let Some(resp) = forbid_unless_can_act_as(&*self.authorizer, &req, &sid.user_id) {
            return resp;
        }
        let create: CreateSessionRequest = match decode_optional_json_body(req.into_body()).await {
            Ok(c) => c,
            Err(e) => return http_error(StatusCode::BAD_REQUEST, &e),
        };
        match self.create_session(sid, create).await {
            Ok(s) => encode_json_response(&s, StatusCode::OK),
            Err(e) => http_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
        }
    }

    /// Go `createSession`.
    async fn create_session(
        &self,
        sid: SessionId,
        create: CreateSessionRequest,
    ) -> Result<Session, String> {
        let session = self
            .service
            .create(CreateRequest {
                app_name: sid.app_name,
                user_id: sid.user_id,
                session_id: Some(sid.id).filter(|id| !id.is_empty()),
                state: create.state,
            })
            .await
            .map_err(|e| e.to_string())?;
        for event in create.events.unwrap_or_default() {
            let mut event = event.to_session_event();
            self.service
                .append_event(&*session, &mut event)
                .await
                .map_err(|e| e.to_string())?;
        }
        Session::from_session(&*session)
    }

    /// Go `DeleteSessionHandler`: 200 with the JSON `Content-Type` and no body.
    pub async fn delete_session_handler(&self, req: Request) -> Response {
        let sid = match session_id_from_http_parameters(&path_vars(&req)) {
            Ok(sid) => sid,
            Err(e) => return http_error(StatusCode::BAD_REQUEST, &e),
        };
        if sid.id.is_empty() {
            return http_error(StatusCode::BAD_REQUEST, "session_id parameter is required");
        }
        if let Some(resp) = forbid_unless_can_act_as(&*self.authorizer, &req, &sid.user_id) {
            return resp;
        }
        let deleted = self
            .service
            .delete(DeleteRequest {
                app_name: sid.app_name,
                user_id: sid.user_id,
                session_id: sid.id,
            })
            .await;
        match deleted {
            Ok(()) => encode_nil_json_response(StatusCode::OK),
            Err(e) => http_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
        }
    }

    /// Go `GetSessionHandler`.
    pub async fn get_session_handler(&self, req: Request) -> Response {
        let sid = match session_id_from_http_parameters(&path_vars(&req)) {
            Ok(sid) => sid,
            Err(e) => return http_error(StatusCode::BAD_REQUEST, &e),
        };
        if sid.id.is_empty() {
            return http_error(StatusCode::BAD_REQUEST, "session_id parameter is required");
        }
        if let Some(resp) = forbid_unless_can_act_as(&*self.authorizer, &req, &sid.user_id) {
            return resp;
        }
        let stored = match self.service.get(get_request(&sid)).await {
            Ok(s) => s,
            Err(e) => return write_session_service_error(&e, &sid.id, false),
        };
        match Session::from_session(&*stored) {
            Ok(s) => encode_json_response(&s, StatusCode::OK),
            Err(e) => http_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
        }
    }

    /// Go `UpdateSessionHandler` (PATCH state delta): the session-scoped keys of `stateDelta`
    /// are applied through an appended `user` event, scoped keys (`app:`, `user:`, `temp:`) are
    /// refused unless they echo the stored value, and the re-read session is answered.
    pub async fn update_session_handler(&self, req: Request) -> Response {
        let sid = match session_id_from_http_parameters(&path_vars(&req)) {
            Ok(sid) => sid,
            Err(e) => return http_error(StatusCode::BAD_REQUEST, &e),
        };
        if sid.id.is_empty() {
            return http_error(StatusCode::BAD_REQUEST, "session_id parameter is required");
        }
        if let Some(resp) = forbid_unless_can_act_as(&*self.authorizer, &req, &sid.user_id) {
            return resp;
        }
        let platform = request_platform(&req);
        let update: UpdateSessionRequest = match decode_optional_json_body(req.into_body()).await {
            Ok(u) => u,
            Err(e) => return http_error(StatusCode::BAD_REQUEST, &e),
        };

        let mut stored = match self.service.get(get_request(&sid)).await {
            Ok(s) => s,
            Err(e) => return write_session_service_error(&e, &sid.id, false),
        };

        let state_delta =
            drop_echoed_scoped_keys(update.state_delta.unwrap_or_default(), stored.state());
        let scoped = scoped_state_keys(&state_delta);
        if !scoped.is_empty() {
            return http_error(
                StatusCode::BAD_REQUEST,
                &format!(
                    "stateDelta may only contain session-scoped keys; remove [{}] (the {}, {} and {} prefixes write outside this session)",
                    scoped.join(" "),
                    go_quote(KEY_PREFIX_APP),
                    go_quote(KEY_PREFIX_USER),
                    go_quote(KEY_PREFIX_TEMP)
                ),
            );
        }

        // An empty or absent delta is a no-op: the session is answered as it stands.
        if !state_delta.is_empty() {
            let invocation_id = platform.new_uuid();
            let mut event = gadk_session::Event::new(&platform, invocation_id);
            event.author = "user".to_owned();
            event.actions.state_delta = Some(state_delta);
            if let Err(e) = self.service.append_event(&*stored, &mut event).await {
                return write_session_service_error(&e, &sid.id, true);
            }
            // Re-read: app- and user-scoped keys are merged back in only on read.
            stored = match self.service.get(get_request(&sid)).await {
                Ok(s) => s,
                Err(e) => return write_session_service_error(&e, &sid.id, false),
            };
        }

        match Session::from_session(&*stored) {
            Ok(s) => encode_json_response(&s, StatusCode::OK),
            Err(e) => http_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
        }
    }

    /// Go `ListSessionsHandler`: always a JSON array (`[]` for a user without sessions).
    pub async fn list_sessions_handler(&self, req: Request) -> Response {
        let sid = match session_id_from_http_parameters(&path_vars(&req)) {
            Ok(sid) => sid,
            Err(e) => return http_error(StatusCode::BAD_REQUEST, &e),
        };
        if let Some(resp) = forbid_unless_can_act_as(&*self.authorizer, &req, &sid.user_id) {
            return resp;
        }
        let listed = self
            .service
            .list(ListRequest {
                app_name: sid.app_name,
                user_id: sid.user_id,
            })
            .await;
        let listed = match listed {
            Ok(l) => l,
            Err(e) => return http_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
        };
        let mut sessions = Vec::with_capacity(listed.len());
        for s in &listed {
            match Session::from_session(&**s) {
                Ok(s) => sessions.push(s),
                Err(e) => return http_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
            }
        }
        encode_json_response(&sessions, StatusCode::OK)
    }
}

/// The `session.GetRequest` Go builds from the path's session ID.
fn get_request(sid: &SessionId) -> GetRequest {
    GetRequest {
        app_name: sid.app_name.clone(),
        user_id: sid.user_id.clone(),
        session_id: sid.id.clone(),
        ..Default::default()
    }
}

/// Go `writeSessionServiceError`: a missing session is a 404, anything else a 500.
fn write_session_service_error(
    err: &gadk_session::SessionError,
    session_id: &str,
    append: bool,
) -> Response {
    let status = match err {
        gadk_session::SessionError::NotFound => StatusCode::NOT_FOUND,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    http_error(status, &session_error_text(err, session_id, append))
}

/// Go `isScopedStateKey`: `app:`, `user:` and `temp:` keys write outside the session.
fn is_scoped_state_key(key: &str) -> bool {
    key.starts_with(KEY_PREFIX_APP)
        || key.starts_with(KEY_PREFIX_USER)
        || key.starts_with(KEY_PREFIX_TEMP)
}

/// Go `scopedStateKeys`: the scoped keys of `delta`, sorted.
fn scoped_state_keys(delta: &Map<String, Value>) -> Vec<String> {
    let mut scoped: Vec<String> = delta
        .keys()
        .filter(|k| is_scoped_state_key(k))
        .cloned()
        .collect();
    scoped.sort();
    scoped
}

/// Go `dropEchoedScopedKeys`: `delta` without the scoped keys whose value equals the stored one
/// (the web UI's rename posts back the whole state it read).
fn drop_echoed_scoped_keys(delta: Map<String, Value>, stored: &dyn State) -> Map<String, Value> {
    delta
        .into_iter()
        .filter(|(k, v)| !(is_scoped_state_key(k) && matches_stored_value(stored, k, v)))
        .collect()
}

/// Go `matchesStoredValue`: the values compared as Go's `json.Marshal` bytes, so a stored
/// integer equals the float the client echoes back. A missing key is a change.
fn matches_stored_value(stored: &dyn State, key: &str, value: &Value) -> bool {
    let Ok(current) = stored.get(key) else {
        return false;
    };
    match (to_go_json(&current), to_go_json(value)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Go `SessionsAPIRouter.Routes`.
// Mounted by `Server::new` (plan 5a Task 13).
#[allow(dead_code)]
pub(crate) fn sessions_routes(c: Arc<SessionsApiController>) -> Vec<Route> {
    const SESSION: &str = "/apps/{app_name}/users/{user_id}/sessions/{session_id}";
    const SESSIONS: &str = "/apps/{app_name}/users/{user_id}/sessions";
    vec![
        route(
            "GetSession",
            &[Method::GET],
            SESSION,
            controller_handler!(c, get_session_handler),
        ),
        route(
            "CreateSession",
            &[Method::POST],
            SESSIONS,
            controller_handler!(c, create_session_handler),
        ),
        route(
            "CreateSessionWithId",
            &[Method::POST],
            SESSION,
            controller_handler!(c, create_session_handler),
        ),
        route(
            "UpdateSession",
            &[Method::PATCH],
            SESSION,
            controller_handler!(c, update_session_handler),
        ),
        route(
            "DeleteSession",
            &[Method::DELETE],
            SESSION,
            controller_handler!(c, delete_session_handler),
        ),
        route(
            "ListSessions",
            &[Method::GET],
            SESSIONS,
            controller_handler!(c, list_sessions_handler),
        ),
    ]
}
