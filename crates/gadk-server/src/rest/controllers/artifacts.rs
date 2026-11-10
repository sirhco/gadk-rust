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

//! Port of Go `controllers/artifacts.go` and `internal/routers/artifacts.go` (plan 5a Task 6).

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::sync::Arc;

use gadk_artifact::ArtifactService;
use http::Method;

use crate::authz::{self, Authorizer};
use crate::rest::routing::{Request, Response, Route, controller_handler, route};

/// Port of Go `controllers.ArtifactsAPIController`; without a service every handler answers 503
/// `artifact service is not configured`.
pub struct ArtifactsApiController {
    service: Option<Arc<dyn ArtifactService>>,
    authorizer: Arc<dyn Authorizer>,
}

impl ArtifactsApiController {
    /// Go `NewArtifactsAPIController` (authorizer: [`authz::noop`]).
    pub fn new(service: Option<Arc<dyn ArtifactService>>) -> Self {
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

    /// Go `ListArtifactsHandler`.
    pub async fn list_artifacts_handler(&self, req: Request) -> Response {
        todo!("plan 5a Task 6")
    }

    /// Go `LoadArtifactHandler`.
    pub async fn load_artifact_handler(&self, req: Request) -> Response {
        todo!("plan 5a Task 6")
    }

    /// Go `LoadArtifactVersionHandler`.
    pub async fn load_artifact_version_handler(&self, req: Request) -> Response {
        todo!("plan 5a Task 6")
    }

    /// Go `DeleteArtifactHandler`.
    pub async fn delete_artifact_handler(&self, req: Request) -> Response {
        todo!("plan 5a Task 6")
    }
}

/// Go `ArtifactsAPIRouter.Routes`.
pub(crate) fn artifacts_routes(c: Arc<ArtifactsApiController>) -> Vec<Route> {
    const BASE: &str = "/apps/{app_name}/users/{user_id}/sessions/{session_id}/artifacts";
    vec![
        route(
            "ListArtifacts",
            &[Method::GET],
            BASE,
            controller_handler!(c, list_artifacts_handler),
        ),
        route(
            "LoadArtifact",
            &[Method::GET],
            format!("{BASE}/{{artifact_name}}"),
            controller_handler!(c, load_artifact_handler),
        ),
        route(
            "LoadArtifactVersion",
            &[Method::GET],
            format!("{BASE}/{{artifact_name}}/versions/{{version}}"),
            controller_handler!(c, load_artifact_version_handler),
        ),
        route(
            "DeleteArtifact",
            &[Method::DELETE],
            format!("{BASE}/{{artifact_name}}"),
            controller_handler!(c, delete_artifact_handler),
        ),
    ]
}
