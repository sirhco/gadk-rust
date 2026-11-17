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

//! The artifacts controller and its routes.

use std::sync::Arc;

use gadk_artifact::{ArtifactError, ArtifactService, DeleteRequest, ListRequest, LoadRequest};
use http::{Method, StatusCode};

use super::handlers::{encode_nil_json_response, forbid_unless_can_act_as, path_vars};
use crate::authz::{self, Authorizer};
use crate::rest::http_error::{encode_json_response, http_error};
use crate::rest::models::{SessionId, session_id_from_http_parameters};
use crate::rest::routing::{
    PathVars, Request, Response, Route, controller_handler, query_get, route,
};

/// Serves a session's artifacts; without a service every handler answers 503
/// `artifact service is not configured`.
pub struct ArtifactsApiController {
    service: Option<Arc<dyn ArtifactService>>,
    authorizer: Arc<dyn Authorizer>,
}

impl ArtifactsApiController {
    /// A controller over `service` (authorizer: [`authz::noop`]).
    pub fn new(service: Option<Arc<dyn ArtifactService>>) -> Self {
        Self {
            service,
            authorizer: authz::noop(),
        }
    }

    /// This controller with `a` deciding whether the caller may act as the path's user.
    pub fn with_authorizer(mut self, a: Arc<dyn Authorizer>) -> Self {
        self.authorizer = a;
        self
    }

    /// The session's artifact names as a JSON array (`[]` when none).
    pub async fn list_artifacts_handler(&self, req: Request) -> Response {
        let service = match self.service() {
            Ok(s) => s,
            Err(resp) => return *resp,
        };
        let sid = match session_id_from_http_parameters(&path_vars(&req)) {
            Ok(sid) => sid,
            Err(e) => return http_error(StatusCode::BAD_REQUEST, &e),
        };
        // Authorization is checked before the session ID here (the other handlers check it after).
        if let Some(resp) =
            forbid_unless_can_act_as(&*self.authorizer, req.extensions(), &sid.user_id)
        {
            return resp;
        }
        if sid.id.is_empty() {
            return http_error(StatusCode::BAD_REQUEST, "session_id parameter is required");
        }
        let listed = service
            .list(ListRequest {
                app_name: sid.app_name,
                user_id: sid.user_id,
                session_id: sid.id,
            })
            .await;
        match listed {
            Ok(resp) => encode_json_response(&resp.file_names, StatusCode::OK),
            Err(e) => http_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
        }
    }

    /// The latest version, or `?version=N`, as a JSON content part.
    pub async fn load_artifact_handler(&self, req: Request) -> Response {
        let (service, _, sid, file_name) = match self.artifact_request(&req) {
            Ok(parts) => parts,
            Err(resp) => return *resp,
        };
        let mut load = LoadRequest {
            app_name: sid.app_name,
            user_id: sid.user_id,
            session_id: sid.id,
            file_name,
            version: 0,
        };
        let version = query_get(req.uri().query(), "version");
        if !version.is_empty() {
            match go_atoi(&version) {
                Some(v) => load.version = v,
                None => {
                    return http_error(
                        StatusCode::BAD_REQUEST,
                        "version parameter must be an integer",
                    );
                }
            }
        }
        match service.load(load).await {
            Ok(resp) => encode_json_response(&resp.part, StatusCode::OK),
            Err(e) => write_artifact_error(&e),
        }
    }

    /// The `{version}` of the artifact as a JSON content part.
    pub async fn load_artifact_version_handler(&self, req: Request) -> Response {
        let (service, vars, sid, file_name) = match self.artifact_request(&req) {
            Ok(parts) => parts,
            Err(resp) => return *resp,
        };
        let version = vars.0.get("version").cloned().unwrap_or_default();
        if version.is_empty() {
            return http_error(StatusCode::BAD_REQUEST, "version parameter is required");
        }
        let Some(version) = go_atoi(&version) else {
            return http_error(
                StatusCode::BAD_REQUEST,
                "version parameter must be an integer",
            );
        };
        let loaded = service
            .load(LoadRequest {
                app_name: sid.app_name,
                user_id: sid.user_id,
                session_id: sid.id,
                file_name,
                version,
            })
            .await;
        match loaded {
            Ok(resp) => encode_json_response(&resp.part, StatusCode::OK),
            Err(e) => write_artifact_error(&e),
        }
    }

    /// Deletes every version; 200 with the JSON `Content-Type` and no body.
    pub async fn delete_artifact_handler(&self, req: Request) -> Response {
        let (service, _, sid, file_name) = match self.artifact_request(&req) {
            Ok(parts) => parts,
            Err(resp) => return *resp,
        };
        let deleted = service
            .delete(DeleteRequest {
                app_name: sid.app_name,
                user_id: sid.user_id,
                session_id: sid.id,
                file_name,
                version: 0,
            })
            .await;
        match deleted {
            Ok(()) => encode_nil_json_response(StatusCode::OK),
            Err(e) => write_artifact_error(&e),
        }
    }

    /// The service, or a 503 when the server has no artifact service.
    fn service(&self) -> Result<&Arc<dyn ArtifactService>, Box<Response>> {
        self.service.as_ref().ok_or_else(|| {
            Box::new(http_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "artifact service is not configured",
            ))
        })
    }

    /// The checks the load, load-version and delete handlers share, in order: service, path
    /// variables, session ID, authorization, artifact name. The error is the response to send.
    #[allow(clippy::type_complexity)]
    fn artifact_request(
        &self,
        req: &Request,
    ) -> Result<(&Arc<dyn ArtifactService>, PathVars, SessionId, String), Box<Response>> {
        let service = self.service()?;
        let vars = path_vars(req);
        let sid = session_id_from_http_parameters(&vars)
            .map_err(|e| Box::new(http_error(StatusCode::BAD_REQUEST, &e)))?;
        if sid.id.is_empty() {
            return Err(Box::new(http_error(
                StatusCode::BAD_REQUEST,
                "session_id parameter is required",
            )));
        }
        if let Some(resp) =
            forbid_unless_can_act_as(&*self.authorizer, req.extensions(), &sid.user_id)
        {
            return Err(Box::new(resp));
        }
        let file_name = vars.0.get("artifact_name").cloned().unwrap_or_default();
        if file_name.is_empty() {
            return Err(Box::new(http_error(
                StatusCode::BAD_REQUEST,
                "artifact_name parameter is required",
            )));
        }
        Ok((service, vars, sid, file_name))
    }
}

/// The error response: a missing artifact is a 404, anything else a 500.
fn write_artifact_error(err: &ArtifactError) -> Response {
    let status = match err {
        ArtifactError::NotFound => StatusCode::NOT_FOUND,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    http_error(status, &err.to_string())
}

/// Parses a base-10 integer: an optional sign and ASCII digits in `i64` range.
fn go_atoi(s: &str) -> Option<i64> {
    let digits = s.strip_prefix(['+', '-']).unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// The artifact routes.
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
