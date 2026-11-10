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

use std::sync::Arc;

use gadk_agent::Loader;
use http::{Method, StatusCode};

use crate::rest::http_error::{encode_json_response, http_error};
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
    pub async fn list_apps_handler(&self, _req: Request) -> Response {
        match &self.loader {
            Some(loader) => encode_json_response(&loader.list_agents(), StatusCode::OK),
            // parity: Go dereferences the nil loader and panics (net/http then drops the
            // connection); gadk answers the 500 its panic handler would (ledger header).
            None => http_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "agent loader is not configured",
            ),
        }
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use gadk_agent::{Agent, LoaderError};

    use super::*;

    /// A loader that only lists names.
    struct Names(Vec<&'static str>);

    impl Loader for Names {
        fn list_agents(&self) -> Vec<String> {
            self.0.iter().map(|s| (*s).to_owned()).collect()
        }
        fn load_agent(&self, name: &str) -> Result<Arc<dyn Agent>, LoaderError> {
            Err(LoaderError(format!("no agent {name}")))
        }
        fn root_agent(&self) -> Arc<dyn Agent> {
            unreachable!("root_agent")
        }
    }

    async fn read(resp: Response) -> (u16, String, String) {
        let status = resp.status().as_u16();
        let ct = resp.headers()[http::header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .to_owned();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, ct, String::from_utf8(body.to_vec()).unwrap())
    }

    /// ADK-Go's `main_04_list_apps` (the probe's loader lists `assistant` and `pipeline`).
    #[tokio::test]
    async fn list_apps_matches_go_wire_fixture() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/testdata/wire/main_04_list_apps.json"
        );
        let f: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let c = AppsApiController::new(Some(Arc::new(Names(vec!["assistant", "pipeline"]))));
        let got = read(c.list_apps_handler(Request::default()).await).await;
        assert_eq!(
            got,
            (
                200,
                f["response"]["headers"][0][1].as_str().unwrap().to_owned(),
                f["response"]["body"].as_str().unwrap().to_owned()
            )
        );
    }

    #[tokio::test]
    async fn list_apps_without_loader_is_a_500() {
        let got = read(
            AppsApiController::new(None)
                .list_apps_handler(Request::default())
                .await,
        )
        .await;
        assert_eq!(
            got,
            (
                500,
                "text/plain; charset=utf-8".to_owned(),
                "agent loader is not configured\n".to_owned()
            )
        );
    }
}
