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

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::sync::Arc;

use gadk_session::SessionService;
use http::Method;

use crate::authz::{self, Authorizer};
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

    /// Go `CreateSessionHandler`.
    pub async fn create_session_handler(&self, req: Request) -> Response {
        todo!("plan 5a Task 6")
    }

    /// Go `DeleteSessionHandler`.
    pub async fn delete_session_handler(&self, req: Request) -> Response {
        todo!("plan 5a Task 6")
    }

    /// Go `GetSessionHandler`.
    pub async fn get_session_handler(&self, req: Request) -> Response {
        todo!("plan 5a Task 6")
    }

    /// Go `UpdateSessionHandler` (PATCH state delta).
    pub async fn update_session_handler(&self, req: Request) -> Response {
        todo!("plan 5a Task 6")
    }

    /// Go `ListSessionsHandler`.
    pub async fn list_sessions_handler(&self, req: Request) -> Response {
        todo!("plan 5a Task 6")
    }
}

/// Go `SessionsAPIRouter.Routes`.
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
