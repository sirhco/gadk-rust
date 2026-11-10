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

//! Port of Go `controllers/debug.go` and `internal/routers/debug.go` (plan 5a Task 12).

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::sync::Arc;

use gadk_agent::Loader;
use gadk_session::SessionService;
use http::Method;

use crate::authz::{self, Authorizer};
use crate::rest::DebugTelemetry;
use crate::rest::routing::{DEV_PREFIX, Request, Response, Route, controller_handler, route};

/// Port of Go `controllers.DebugAPIController`.
pub struct DebugApiController {
    sessions: Arc<dyn SessionService>,
    loader: Option<Arc<dyn Loader>>,
    telemetry: DebugTelemetry,
    authorizer: Arc<dyn Authorizer>,
}

impl DebugApiController {
    /// Go `NewDebugAPIController` (authorizer: [`authz::noop`]).
    pub fn new(
        sessions: Arc<dyn SessionService>,
        loader: Option<Arc<dyn Loader>>,
        telemetry: DebugTelemetry,
    ) -> Self {
        Self {
            sessions,
            loader,
            telemetry,
            authorizer: authz::noop(),
        }
    }

    /// Go `WithAuthorizer`.
    pub fn with_authorizer(mut self, a: Arc<dyn Authorizer>) -> Self {
        self.authorizer = a;
        self
    }

    /// Go `EventSpanHandler`.
    pub async fn event_span_handler(&self, req: Request) -> Response {
        todo!("plan 5a Task 12")
    }

    /// Go `SessionSpansHandler`.
    pub async fn session_spans_handler(&self, req: Request) -> Response {
        todo!("plan 5a Task 12")
    }

    /// Go `EventGraphHandler`.
    pub async fn event_graph_handler(&self, req: Request) -> Response {
        todo!("plan 5a Task 12")
    }
}

/// Go `DebugAPIRouter.Routes`.
pub(crate) fn debug_routes(c: Arc<DebugApiController>) -> Vec<Route> {
    vec![
        route(
            "GetSessionTraceDev",
            &[Method::GET],
            format!("{DEV_PREFIX}/debug/trace/session/{{session_id}}"),
            controller_handler!(c, session_spans_handler),
        ),
        route(
            "GetTraceDictDev",
            &[Method::GET],
            format!("{DEV_PREFIX}/debug/trace/{{event_id}}"),
            controller_handler!(c, event_span_handler),
        ),
        route(
            "GetEventGraphDev",
            &[Method::GET],
            format!(
                "{DEV_PREFIX}/users/{{user_id}}/sessions/{{session_id}}/events/{{event_id}}/graph"
            ),
            controller_handler!(c, event_graph_handler),
        ),
        route(
            "GetTraceDict",
            &[Method::GET],
            "/debug/trace/{event_id}",
            controller_handler!(c, event_span_handler),
        ),
        route(
            "GetEventGraph",
            &[Method::GET],
            "/apps/{app_name}/users/{user_id}/sessions/{session_id}/events/{event_id}/graph",
            controller_handler!(c, event_graph_handler),
        ),
        route(
            "GetSessionTrace",
            &[Method::GET],
            "/debug/trace/session/{session_id}",
            controller_handler!(c, session_spans_handler),
        ),
    ]
}
