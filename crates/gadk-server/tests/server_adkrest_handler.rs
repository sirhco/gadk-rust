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

//! Ports of ADK-Go `server/adkrest/handler_test.go`: the assembled server's `/health`, the
//! debug API gate, the default authorizer, and the router's HEAD, OPTIONS, 405 and trailing
//! slash answers. Go's `httptest.NewRecorder` cases go through `Server::router()` with
//! `oneshot`; its `httptest.NewServer` cases through a real loopback listener, because the body
//! of a HEAD response is dropped by the transport.

use std::sync::Arc;

use axum::body::Body;
use gadk_server::authz::{self, Authorizer};
use gadk_server::rest::testutil::{
    authenticate_as, serve_http, serve_loopback, single_agent_loader,
};
use gadk_server::rest::{DebugApiConfig, Server, ServerConfig};
use gadk_session::InMemorySessionService;
use http::{Method, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

const TEST_APP_NAME: &str = "test-app";
const TEST_USER_ID: &str = "test-user";

/// Go `testSessions`.
fn test_sessions() -> String {
    format!("/apps/{TEST_APP_NAME}/users/{TEST_USER_ID}/sessions")
}

/// Go `muxNotFound`: what the router writes when no route matches.
const MUX_NOT_FOUND: &str = "404 page not found\n";

/// Go `newAssembledServer`: the server with a single `test-app` agent and the debug API, served
/// on a loopback listener; with `authenticated_user_id` every request is that caller.
async fn new_assembled_server(authenticated_user_id: Option<&'static str>) -> String {
    let mut cfg = ServerConfig::new(Arc::new(InMemorySessionService::new()));
    cfg.agent_loader = Some(single_agent_loader(TEST_APP_NAME));
    cfg.debug_api_config = DebugApiConfig {
        include_debug_api: true,
    };
    if let Some(user) = authenticated_user_id {
        cfg.authenticator = Some(authenticate_as(user));
    }
    let addr = serve_loopback(Server::new(cfg).unwrap().router())
        .await
        .unwrap();
    format!("http://{addr}")
}

/// Go `do` / `doNoRedirect`: one request, redirects not followed; status, `Allow` and body.
async fn send(base: &str, method: Method, path: &str) -> (StatusCode, Option<String>, String) {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let resp = client
        .request(method, format!("{base}{path}"))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let allow = resp
        .headers()
        .get("allow")
        .map(|v| v.to_str().unwrap().to_owned());
    (status, allow, resp.text().await.unwrap())
}

/// Go `parseAllow`: the `Allow` header as a sorted list.
fn parse_allow(allow: Option<&str>) -> Vec<String> {
    let mut got: Vec<String> = allow
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_owned)
        .collect();
    got.sort();
    got
}

fn sorted(methods: &[Method]) -> Vec<String> {
    let mut v: Vec<String> = methods.iter().map(|m| m.as_str().to_owned()).collect();
    v.sort();
    v
}

// parity: adapted: Go's zero ServerConfig has no session service; gadk's ServerConfig::new requires one, so an in-memory service stands in
#[tokio::test]
async fn test_server_health() {
    let server = Server::new(ServerConfig::new(Arc::new(InMemorySessionService::new()))).unwrap();
    let req = http::Request::get("/health").body(Body::empty()).unwrap();
    let resp = server.router().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["content-type"], "application/json");
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(&body[..], b"{\"status\":\"ok\"}\n");
}

/// Go `debugRoutes`: every route the debug API gate covers.
const DEBUG_ROUTES: [&str; 5] = [
    "/debug/trace/evt1",
    "/debug/trace/session/sess1",
    "/apps/app1/users/user1/sessions/sess1/events/evt1/graph",
    "/dev/apps/app1/build_graph",
    "/dev/apps/app1/build_graph_image",
];

/// Go `ungatedDevRoutes`: the agent builder's routes, served whatever the gate says.
fn ungated_dev_routes() -> [(Method, &'static str, StatusCode); 3] {
    [
        (Method::GET, "/dev/apps/app1/builder", StatusCode::OK),
        (
            Method::POST,
            "/dev/apps/app1/builder/save",
            StatusCode::NOT_IMPLEMENTED,
        ),
        (
            Method::POST,
            "/dev/apps/app1/builder/cancel",
            StatusCode::NOT_IMPLEMENTED,
        ),
    ]
}

#[tokio::test]
async fn test_new_server_debug_api_gate() {
    for (name, include, want_routed) in [
        ("omitted by default", false, false),
        ("included when opted in", true, true),
    ] {
        let mut cfg = ServerConfig::new(Arc::new(InMemorySessionService::new()));
        cfg.agent_loader = Some(single_agent_loader("app1"));
        cfg.debug_api_config = DebugApiConfig {
            include_debug_api: include,
        };
        let srv = Server::new(cfg).unwrap();
        for route in DEBUG_ROUTES {
            let (code, body) = serve_http(&srv, Method::GET, route).await;
            let routed = body != MUX_NOT_FOUND;
            assert_eq!(
                routed, want_routed,
                "{name}: GET {route}: routed (code {code}, body {body:?})"
            );
        }
        for (method, path, want_status) in ungated_dev_routes() {
            let (code, body) = serve_http(&srv, method.clone(), path).await;
            assert_ne!(
                body, MUX_NOT_FOUND,
                "{name}: {method} {path}: not routed, want it served regardless of the gate"
            );
            assert_eq!(code, want_status, "{name}: {method} {path}");
        }
    }
}

#[tokio::test]
async fn test_new_server_always_sets_authorizer() {
    let other_users_sessions = format!("/apps/{TEST_APP_NAME}/users/other-user/sessions");
    type Case = (&'static str, Option<Arc<dyn Authorizer>>, StatusCode);
    let cases: [Case; 2] = [
        (
            "nil defaults to a permit-all authorizer",
            None,
            StatusCode::OK,
        ),
        (
            "provided authorizer is enforced",
            Some(authz::strict()),
            StatusCode::FORBIDDEN,
        ),
    ];
    for (name, authorizer, want_code) in cases {
        let mut cfg = ServerConfig::new(Arc::new(InMemorySessionService::new()));
        cfg.agent_loader = Some(single_agent_loader(TEST_APP_NAME));
        cfg.authenticator = Some(authenticate_as("alice"));
        cfg.authorizer = authorizer;
        let srv = Server::new(cfg).unwrap();
        let (code, body) = serve_http(&srv, Method::GET, &other_users_sessions).await;
        assert_eq!(code, want_code, "{name}; body: {body}");
    }
}

#[tokio::test]
async fn test_head_matches_get() {
    let base = new_assembled_server(Some("test-user")).await;
    let paths = [
        ("apps", "/list-apps".to_owned()),
        ("version", "/version".to_owned()),
        ("sessions", test_sessions()),
        ("eval", format!("/dev/apps/{TEST_APP_NAME}/metrics-info")),
        (
            "agent graph",
            format!("/dev/apps/{TEST_APP_NAME}/build_graph"),
        ),
        ("debug", "/debug/trace/session/test-session".to_owned()),
    ];
    for (name, path) in paths {
        let (get_status, _, get_body) = send(&base, Method::GET, &path).await;
        assert_eq!(
            get_status,
            StatusCode::OK,
            "{name}: GET {path}; body: {get_body}"
        );
        let (head_status, _, head_body) = send(&base, Method::HEAD, &path).await;
        assert_eq!(head_status, get_status, "{name}: HEAD {path}");
        assert_eq!(head_body, "", "{name}: HEAD {path} body");
    }
}

#[tokio::test]
async fn test_options_does_not_delete_session() {
    let base = new_assembled_server(Some("test-user")).await;
    let session_path = format!("{}/keep-me", test_sessions());

    let (status, _, body) = send(&base, Method::POST, &session_path).await;
    assert_eq!(status, StatusCode::OK, "POST {session_path}; body: {body}");

    send(&base, Method::OPTIONS, &session_path).await;

    let (status, _, body) = send(&base, Method::GET, &session_path).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "GET {session_path} after OPTIONS: the preflight deleted the session. Body: {body}"
    );
    let got: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(got["id"], "keep-me");
}

#[tokio::test]
async fn test_method_not_allowed_has_allow_header() {
    let base = new_assembled_server(None).await;
    for (name, method, path, want_allowed) in [(
        "patch on version",
        Method::PATCH,
        "/version",
        [Method::GET, Method::HEAD],
    )] {
        let (status, allow, body) = send(&base, method.clone(), path).await;
        assert_eq!(
            status,
            StatusCode::METHOD_NOT_ALLOWED,
            "{name}: {method} {path}; body: {body}"
        );
        let allow = allow.unwrap_or_else(|| panic!("{name}: no Allow header"));
        for m in want_allowed {
            assert!(
                allow.contains(m.as_str()),
                "{name}: Allow = {allow:?}, want {m}"
            );
        }
    }
}

/// Go `probedMethods`.
const PROBED_METHODS: [Method; 6] = [
    Method::GET,
    Method::HEAD,
    Method::POST,
    Method::PUT,
    Method::PATCH,
    Method::DELETE,
];

#[tokio::test]
async fn test_method_not_allowed_matrix() {
    let base = new_assembled_server(None).await;
    let dev_app = format!("/dev/apps/{TEST_APP_NAME}");
    let sessions = test_sessions();
    let cases: Vec<(&str, String, Vec<Method>)> = vec![
        (
            "version",
            "/version".into(),
            vec![Method::GET, Method::HEAD],
        ),
        (
            "list-apps",
            "/list-apps".into(),
            vec![Method::GET, Method::HEAD],
        ),
        ("run", "/run".into(), vec![Method::POST]),
        (
            "session collection",
            sessions.clone(),
            vec![Method::GET, Method::HEAD, Method::POST],
        ),
        (
            "single session",
            format!("{sessions}/some-session"),
            vec![
                Method::GET,
                Method::HEAD,
                Method::POST,
                Method::PATCH,
                Method::DELETE,
            ],
        ),
        (
            "artifact",
            format!("{sessions}/some-session/artifacts/report.txt"),
            vec![Method::GET, Method::HEAD, Method::DELETE],
        ),
        (
            "test case",
            format!("{dev_app}/tests/some-test"),
            vec![Method::GET, Method::HEAD, Method::PUT, Method::DELETE],
        ),
        (
            "eval case",
            format!("{dev_app}/eval_sets/set-1/evals/case-1"),
            vec![Method::GET, Method::HEAD, Method::PUT, Method::DELETE],
        ),
    ];
    for (name, path, allowed) in cases {
        let want = sorted(&allowed);
        for method in PROBED_METHODS {
            if allowed.contains(&method) {
                continue;
            }
            let (status, allow, body) = send(&base, method.clone(), &path).await;
            assert_eq!(
                status,
                StatusCode::METHOD_NOT_ALLOWED,
                "{name}/{method}: {path} (404 means the router dropped the method mismatch); body: {body}"
            );
            assert_eq!(
                parse_allow(allow.as_deref()),
                want,
                "{name}/{method}: Allow"
            );
        }
    }
}

#[tokio::test]
async fn test_unmatched_path_is_still_not_found() {
    let base = new_assembled_server(None).await;
    for method in PROBED_METHODS {
        let (status, allow, _) = send(&base, method.clone(), "/no-such-endpoint").await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method}");
        assert_eq!(allow, None, "{method}: want no Allow header");
    }
}

#[tokio::test]
async fn test_trailing_slash_keeps_method_and_body() {
    let base = new_assembled_server(Some("test-user")).await;
    let session_id = "slashed-session";
    let path = format!("{}/{session_id}", test_sessions());

    let (status, _, body) = send(&base, Method::POST, &format!("{path}/")).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "POST {path}/ (a 3xx here is the redirect that drops the method and the body); body: {body}"
    );
    let (status, _, body) = send(&base, Method::GET, &path).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "GET {path} after the POST: the session was never created. Body: {body}"
    );
    let got: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(got["id"], session_id);
}

#[tokio::test]
async fn test_trailing_slash_on_safe_and_rejected_methods() {
    let base = new_assembled_server(None).await;

    // GET is served without a redirect.
    let (status, _, body) = send(&base, Method::GET, "/list-apps/").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "GET is served without a redirect; body: {body}"
    );
    let apps: Vec<String> = serde_json::from_str(&body).unwrap();
    assert!(
        apps.iter().any(|a| a == TEST_APP_NAME),
        "GET /list-apps/ returned {apps:?}"
    );

    // Unsupported verb is refused.
    let (status, allow, body) = send(&base, Method::DELETE, "/version/").await;
    assert_eq!(
        status,
        StatusCode::METHOD_NOT_ALLOWED,
        "unsupported verb is refused; body: {body}"
    );
    assert_eq!(
        parse_allow(allow.as_deref()),
        sorted(&[Method::GET, Method::HEAD]),
        "unsupported verb is refused: Allow"
    );
}

#[tokio::test]
async fn test_options_describes_the_resource() {
    let base = new_assembled_server(None).await;
    let (status, allow, body) = send(
        &base,
        Method::OPTIONS,
        &format!("{}/some-session", test_sessions()),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body: {body}");
    assert_eq!(
        parse_allow(allow.as_deref()),
        sorted(&[
            Method::DELETE,
            Method::GET,
            Method::HEAD,
            Method::OPTIONS,
            Method::PATCH,
            Method::POST,
        ])
    );
}
