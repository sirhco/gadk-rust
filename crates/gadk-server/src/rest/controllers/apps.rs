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

//! Port of Go `controllers/apps.go` and `internal/routers/apps.go` (plan 5a Task 6).

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::sync::Arc;

use gadk_agent::Loader;
use http::Method;

use crate::rest::routing::{Request, Response, Route, controller_handler, route};

/// Port of Go `controllers.AppsAPIController`.
pub struct AppsApiController {
    loader: Option<Arc<dyn Loader>>,
}

impl AppsApiController {
    /// Go `NewAppsAPIController`.
    pub fn new(loader: Option<Arc<dyn Loader>>) -> Self {
        Self { loader }
    }

    /// Go `ListAppsHandler`: [`Loader::list_agents`] as a JSON array.
    pub async fn list_apps_handler(&self, req: Request) -> Response {
        todo!("plan 5a Task 6")
    }
}

/// Go `AppsAPIRouter.Routes`.
pub(crate) fn apps_routes(c: Arc<AppsApiController>) -> Vec<Route> {
    vec![route(
        "ListApps",
        &[Method::GET],
        "/list-apps",
        controller_handler!(c, list_apps_handler),
    )]
}
