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

//! Port of Go `server/adkrest/controllers/artifacts_test.go`, plus byte checks of the artifact
//! handlers against the ADK-Go wire fixtures `testdata/wire/main_{24..31}_*.json`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use gadk_artifact::{
    ArtifactError, ArtifactService, DeleteRequest, GetArtifactVersionRequest,
    GetArtifactVersionResponse, InMemoryArtifactService, ListRequest, ListResponse, LoadRequest,
    LoadResponse, SaveRequest, SaveResponse, VersionsRequest, VersionsResponse,
};
use gadk_genai::Part;
use gadk_server::authn::Caller;
use gadk_server::rest::PathVars;
use gadk_server::rest::controllers::ArtifactsApiController;
use http::{Method, StatusCode};
use serde_json::Value;

type Request = http::Request<Body>;
type Response = axum::response::Response;

const TEST_ARTIFACT_APP: &str = "test-app";
const TEST_ARTIFACT_USER: &str = "test-user";
const TEST_ARTIFACT_SESSION: &str = "test-session";
const TEST_ARTIFACT_NAME: &str = "report.txt";

/// Go `notFoundArtifactService`: every Delete reports a missing artifact (the in-memory service
/// treats deleting a missing artifact as success, so it cannot reach the 404 path). Go embeds a
/// nil `artifact.Service`, so its other methods panic; these do too.
struct NotFoundArtifactService;

#[async_trait]
impl ArtifactService for NotFoundArtifactService {
    async fn save(&self, _: SaveRequest) -> Result<SaveResponse, ArtifactError> {
        unreachable!("save")
    }
    async fn load(&self, _: LoadRequest) -> Result<LoadResponse, ArtifactError> {
        unreachable!("load")
    }
    async fn delete(&self, _: DeleteRequest) -> Result<(), ArtifactError> {
        Err(ArtifactError::NotFound)
    }
    async fn list(&self, _: ListRequest) -> Result<ListResponse, ArtifactError> {
        unreachable!("list")
    }
    async fn versions(&self, _: VersionsRequest) -> Result<VersionsResponse, ArtifactError> {
        unreachable!("versions")
    }
    async fn get_artifact_version(
        &self,
        _: GetArtifactVersionRequest,
    ) -> Result<GetArtifactVersionResponse, ArtifactError> {
        unreachable!("get_artifact_version")
    }
}

/// Go `artifactVars`.
fn artifact_vars(name: &str, version: &str) -> PathVars {
    let mut vars = BTreeMap::from([
        ("app_name".to_owned(), TEST_ARTIFACT_APP.to_owned()),
        ("user_id".to_owned(), TEST_ARTIFACT_USER.to_owned()),
        ("session_id".to_owned(), TEST_ARTIFACT_SESSION.to_owned()),
        ("artifact_name".to_owned(), name.to_owned()),
    ]);
    if !version.is_empty() {
        vars.insert("version".to_owned(), version.to_owned());
    }
    PathVars(vars)
}

/// Go `artifactServiceWithFile`: an in-memory service holding one saved artifact.
async fn artifact_service_with_file() -> Arc<dyn ArtifactService> {
    let svc = InMemoryArtifactService::new();
    svc.save(SaveRequest {
        app_name: TEST_ARTIFACT_APP.into(),
        user_id: TEST_ARTIFACT_USER.into(),
        session_id: TEST_ARTIFACT_SESSION.into(),
        file_name: TEST_ARTIFACT_NAME.into(),
        part: Some(Part::from_text("hello artifact")),
        ..Default::default()
    })
    .await
    .unwrap();
    Arc::new(svc)
}

/// A request to `uri` with `vars` and, when set, the authenticated caller.
fn request(method: Method, uri: &str, vars: PathVars, caller: Option<&str>) -> Request {
    let mut req = http::Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
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

async fn read(resp: Response) -> (StatusCode, String) {
    let status = resp.status();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

#[tokio::test]
async fn test_load_artifact_handler() {
    let cases = [
        ("existing_artifact", TEST_ARTIFACT_NAME, "", StatusCode::OK),
        (
            "missing_artifact",
            "no-such-file.txt",
            "",
            StatusCode::NOT_FOUND,
        ),
        (
            "missing_version_of_existing_artifact",
            TEST_ARTIFACT_NAME,
            "?version=42",
            StatusCode::NOT_FOUND,
        ),
        ("empty_artifact_name", "", "", StatusCode::BAD_REQUEST),
    ];
    for (name, artifact_name, query, want) in cases {
        let controller = ArtifactsApiController::new(Some(artifact_service_with_file().await));
        let req = request(
            Method::GET,
            &format!("/artifacts/{artifact_name}{query}"),
            artifact_vars(artifact_name, ""),
            Some(TEST_ARTIFACT_USER),
        );
        let (status, body) = read(controller.load_artifact_handler(req).await).await;
        assert_eq!(status, want, "{name}: body {body:?}");
    }
}

#[tokio::test]
async fn test_load_artifact_version_handler() {
    let cases = [
        ("existing_version", TEST_ARTIFACT_NAME, "1", StatusCode::OK),
        (
            "missing_version",
            TEST_ARTIFACT_NAME,
            "99",
            StatusCode::NOT_FOUND,
        ),
        (
            "missing_artifact",
            "no-such-file.txt",
            "1",
            StatusCode::NOT_FOUND,
        ),
        (
            "non_integer_version",
            TEST_ARTIFACT_NAME,
            "latest",
            StatusCode::BAD_REQUEST,
        ),
    ];
    for (name, artifact_name, version, want) in cases {
        let controller = ArtifactsApiController::new(Some(artifact_service_with_file().await));
        let req = request(
            Method::GET,
            &format!("/artifacts/{artifact_name}/versions/{version}"),
            artifact_vars(artifact_name, version),
            Some(TEST_ARTIFACT_USER),
        );
        let (status, body) = read(controller.load_artifact_version_handler(req).await).await;
        assert_eq!(status, want, "{name}: body {body:?}");
    }
}

#[tokio::test]
async fn test_delete_artifact_handler() {
    let cases: [(&str, Arc<dyn ArtifactService>, StatusCode); 2] = [
        (
            "existing_artifact",
            artifact_service_with_file().await,
            StatusCode::OK,
        ),
        (
            // The in-memory service reports a missing artifact as success, so a stub is needed
            // to reach the not-found branch.
            "missing_artifact",
            Arc::new(NotFoundArtifactService),
            StatusCode::NOT_FOUND,
        ),
    ];
    for (name, service, want) in cases {
        let controller = ArtifactsApiController::new(Some(service));
        let req = request(
            Method::DELETE,
            &format!("/artifacts/{TEST_ARTIFACT_NAME}"),
            artifact_vars(TEST_ARTIFACT_NAME, ""),
            Some(TEST_ARTIFACT_USER),
        );
        let (status, body) = read(controller.delete_artifact_handler(req).await).await;
        assert_eq!(status, want, "{name}: body {body:?}");
    }
}

/// A server built without an artifact service answers 503 with an explanatory body from every
/// artifact handler (Go: instead of a nil-pointer panic).
#[tokio::test]
async fn test_artifact_handlers_nil_service() {
    let controller = ArtifactsApiController::new(None);
    let cases = [
        ("ListArtifactsHandler", Method::GET, artifact_vars("", "")),
        (
            "LoadArtifactHandler",
            Method::GET,
            artifact_vars(TEST_ARTIFACT_NAME, ""),
        ),
        (
            "LoadArtifactVersionHandler",
            Method::GET,
            artifact_vars(TEST_ARTIFACT_NAME, "1"),
        ),
        (
            "DeleteArtifactHandler",
            Method::DELETE,
            artifact_vars(TEST_ARTIFACT_NAME, ""),
        ),
    ];
    for (name, method, vars) in cases {
        let req = request(
            method,
            &format!("/artifacts/{TEST_ARTIFACT_NAME}"),
            vars,
            None,
        );
        let resp = match name {
            "ListArtifactsHandler" => controller.list_artifacts_handler(req).await,
            "LoadArtifactHandler" => controller.load_artifact_handler(req).await,
            "LoadArtifactVersionHandler" => controller.load_artifact_version_handler(req).await,
            _ => controller.delete_artifact_handler(req).await,
        };
        let (status, body) = read(resp).await;
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{name}: body {body:?}"
        );
        assert_eq!(body, "artifact service is not configured\n", "{name}");
    }
}

// ---- Authorization and Go's check order. ----

/// Every artifact handler answers 403 when the caller may not act as the path's user; list checks
/// authorization before the session ID, the others after it (Go's order).
#[tokio::test]
async fn artifact_handlers_enforce_the_authorizer_in_go_order() {
    let controller = ArtifactsApiController::new(Some(artifact_service_with_file().await))
        .with_authorizer(gadk_server::authz::strict());
    let forbidden = (
        StatusCode::FORBIDDEN,
        "forbidden: authz: unauthorized: caller doesn't match the provided userID\n".to_owned(),
    );
    let vars = artifact_vars(TEST_ARTIFACT_NAME, "1");
    let mallory = Some("mallory");
    let got = [
        read(
            controller
                .list_artifacts_handler(request(Method::GET, "/", vars.clone(), mallory))
                .await,
        )
        .await,
        read(
            controller
                .load_artifact_handler(request(Method::GET, "/", vars.clone(), mallory))
                .await,
        )
        .await,
        read(
            controller
                .load_artifact_version_handler(request(Method::GET, "/", vars.clone(), mallory))
                .await,
        )
        .await,
        read(
            controller
                .delete_artifact_handler(request(Method::DELETE, "/", vars, mallory))
                .await,
        )
        .await,
    ];
    for g in got {
        assert_eq!(g, forbidden);
    }

    let mut no_session = artifact_vars(TEST_ARTIFACT_NAME, "");
    no_session.0.remove("session_id");
    let list = read(
        controller
            .list_artifacts_handler(request(Method::GET, "/", no_session.clone(), mallory))
            .await,
    )
    .await;
    assert_eq!(list, forbidden, "list: authz before session_id");
    let load = read(
        controller
            .load_artifact_handler(request(Method::GET, "/", no_session, mallory))
            .await,
    )
    .await;
    assert_eq!(
        load,
        (
            StatusCode::BAD_REQUEST,
            "session_id parameter is required\n".to_owned()
        ),
        "load: session_id before authz"
    );
}

// ---- ADK-Go wire bytes (testdata/wire, config `main`). ----

fn fixture(name: &str) -> (Method, String, u16, Vec<(String, String)>, String) {
    let path = format!("{}/testdata/wire/{name}.json", env!("CARGO_MANIFEST_DIR"));
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    (
        v["request"]["method"].as_str().unwrap().parse().unwrap(),
        v["request"]["target"].as_str().unwrap().to_owned(),
        u16::try_from(v["response"]["status"].as_u64().unwrap()).unwrap(),
        v["response"]["headers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| {
                (
                    h[0].as_str().unwrap().to_ascii_lowercase(),
                    h[1].as_str().unwrap().to_owned(),
                )
            })
            .collect(),
        v["response"]["body"].as_str().unwrap().to_owned(),
    )
}

/// Replays `main_24` … `main_31` against an in-memory service holding the probe's `note.txt`
/// (version 1 `buy milk`, version 2 `buy eggs`, saved by the agent's `save_note` runs) and
/// compares status, headers and body bytes with ADK-Go's.
#[tokio::test]
async fn artifact_handlers_match_go_wire_fixtures() {
    let svc = InMemoryArtifactService::new();
    for text in ["buy milk", "buy eggs"] {
        svc.save(SaveRequest {
            app_name: "assistant".into(),
            user_id: "alice".into(),
            session_id: "s1".into(),
            file_name: "note.txt".into(),
            part: Some(Part::from_text(text)),
            ..Default::default()
        })
        .await
        .unwrap();
    }
    let controller = ArtifactsApiController::new(Some(Arc::new(svc)));
    for name in [
        "main_24_artifacts_list",
        "main_25_artifact_get",
        "main_26_artifact_get_query_version",
        "main_27_artifact_get_version",
        "main_28_artifact_get_version_missing",
        "main_29_artifact_get_version_invalid",
        "main_30_artifact_delete",
        "main_31_artifacts_list_after_delete",
    ] {
        let (method, target, status, headers, body) = fixture(name);
        // The route's path variables (Go `mux.Vars`), from
        // /apps/{app}/users/{user}/sessions/{session}/artifacts[/{name}[/versions/{version}]].
        let path = target.split('?').next().unwrap();
        let segs: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        let mut vars = BTreeMap::from([
            ("app_name".to_owned(), segs[1].to_owned()),
            ("user_id".to_owned(), segs[3].to_owned()),
            ("session_id".to_owned(), segs[5].to_owned()),
        ]);
        if let Some(a) = segs.get(7) {
            vars.insert("artifact_name".to_owned(), (*a).to_owned());
        }
        if let Some(v) = segs.get(9) {
            vars.insert("version".to_owned(), (*v).to_owned());
        }
        let req = request(method.clone(), &target, PathVars(vars), None);
        let resp = match (method.as_str(), segs.len()) {
            ("GET", 7) => controller.list_artifacts_handler(req).await,
            ("GET", 8) => controller.load_artifact_handler(req).await,
            ("GET", 10) => controller.load_artifact_version_handler(req).await,
            ("DELETE", 8) => controller.delete_artifact_handler(req).await,
            other => panic!("{name}: unexpected {other:?}"),
        };
        let got_status = resp.status().as_u16();
        let mut got_headers: Vec<(String, String)> = resp
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap().to_owned()))
            .collect();
        got_headers.sort();
        let (_, got_body) = read(resp).await;
        assert_eq!(
            (got_status, got_headers, got_body),
            (status, headers, body),
            "{name}"
        );
    }
}
