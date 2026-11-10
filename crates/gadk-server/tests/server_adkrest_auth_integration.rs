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

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Ports of ADK-Go `server/adkrest/auth_integration_test.go`: authentication and the strict
//! authorizer through the assembled server.

use std::sync::Arc;

use gadk_artifact::InMemoryArtifactService;
use gadk_server::authz;
use gadk_server::rest::testutil::{authenticate_as, serve_http, single_agent_loader};
use gadk_server::rest::{DebugApiConfig, Server, ServerConfig};
use gadk_session::InMemorySessionService;
use http::{Method, StatusCode};

const TEST_APP_NAME: &str = "test-app";

/// Go `newAuthenticatedServer`: sessions, artifacts, a single agent, every request authenticated
/// as `authenticated_user_id`, and the strict authorizer.
fn new_authenticated_server(authenticated_user_id: &'static str) -> Server {
    let mut cfg = ServerConfig::new(Arc::new(InMemorySessionService::new()));
    cfg.artifact_service = Some(Arc::new(InMemoryArtifactService::new()));
    cfg.agent_loader = Some(single_agent_loader(TEST_APP_NAME));
    cfg.authenticator = Some(authenticate_as(authenticated_user_id));
    cfg.authorizer = Some(authz::strict());
    Server::new(cfg).unwrap()
}

#[tokio::test]
async fn test_auth_public_endpoints_skip_auth() {
    let srv = new_authenticated_server("");
    for path in ["/health", "/version"] {
        let (code, body) = serve_http(&srv, Method::GET, path).await;
        assert_eq!(
            code,
            StatusCode::OK,
            "{path}: public endpoint must skip auth; body: {body}"
        );
    }
}

#[tokio::test]
async fn test_auth_disabled_by_default() {
    let mut cfg = ServerConfig::new(Arc::new(InMemorySessionService::new()));
    cfg.agent_loader = Some(single_agent_loader(TEST_APP_NAME));
    let srv = Server::new(cfg).unwrap();
    let (code, _) = serve_http(&srv, Method::GET, "/list-apps").await;
    assert_eq!(code, StatusCode::OK, "no authenticator should not gate");
}

#[tokio::test]
async fn test_auth_preflight_not_blocked() {
    let srv = new_authenticated_server("");
    let (code, _) = serve_http(&srv, Method::OPTIONS, "/list-apps").await;
    assert_eq!(
        code,
        StatusCode::NO_CONTENT,
        "preflight must not require auth"
    );
}

#[tokio::test]
async fn test_cross_user_authorization() {
    let srv = new_authenticated_server("alice");
    let base = format!("/apps/{TEST_APP_NAME}/users/");
    let cases = [
        (
            "own session create",
            Method::POST,
            "alice/sessions/s1",
            StatusCode::OK,
        ),
        (
            "own session list",
            Method::GET,
            "alice/sessions",
            StatusCode::OK,
        ),
        (
            "other user session create",
            Method::POST,
            "bob/sessions/s2",
            StatusCode::FORBIDDEN,
        ),
        (
            "other user session list",
            Method::GET,
            "bob/sessions",
            StatusCode::FORBIDDEN,
        ),
        (
            "other user session get",
            Method::GET,
            "bob/sessions/s1",
            StatusCode::FORBIDDEN,
        ),
        (
            "other user session delete",
            Method::DELETE,
            "bob/sessions/s1",
            StatusCode::FORBIDDEN,
        ),
        (
            "other user artifacts list",
            Method::GET,
            "bob/sessions/s1/artifacts",
            StatusCode::FORBIDDEN,
        ),
    ];
    for (name, method, path, want_code) in cases {
        let path = format!("{base}{path}");
        let (code, body) = serve_http(&srv, method.clone(), &path).await;
        assert_eq!(code, want_code, "{name}: {method} {path}; body: {body}");
    }
}

#[tokio::test]
async fn test_debug_event_graph_cross_user_forbidden() {
    let mut cfg = ServerConfig::new(Arc::new(InMemorySessionService::new()));
    cfg.agent_loader = Some(single_agent_loader(TEST_APP_NAME));
    cfg.authenticator = Some(authenticate_as("alice"));
    cfg.authorizer = Some(authz::strict());
    cfg.debug_api_config = DebugApiConfig {
        include_debug_api: true,
    };
    let srv = Server::new(cfg).unwrap();
    let suffix = "/sessions/s1/events/e1/graph";

    let (code, body) = serve_http(
        &srv,
        Method::GET,
        &format!("/apps/{TEST_APP_NAME}/users/bob{suffix}"),
    )
    .await;
    assert_eq!(
        code,
        StatusCode::FORBIDDEN,
        "bob's event graph must be refused before the session lookup; body: {body}"
    );

    let (code, _) = serve_http(
        &srv,
        Method::GET,
        &format!("/apps/{TEST_APP_NAME}/users/alice{suffix}"),
    )
    .await;
    assert_ne!(
        code,
        StatusCode::FORBIDDEN,
        "authorization must pass for the caller's own user"
    );
}
