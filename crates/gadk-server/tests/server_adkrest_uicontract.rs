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

//! Port of Go `server/adkrest/uicontract_test.go`: every endpoint the ADK web UI calls reaches a
//! handler on a real server (half 1). Half 2 reads the Angular bundle ADK-Go embeds; gadk ships
//! no web UI, so only the golden list and half 1 are ported.

use std::sync::Arc;

use axum::body::Body;
use gadk_agent::{Context, SingleLoader};
use gadk_artifact::InMemoryArtifactService;
use gadk_engine::agents::workflow_agent::{self, WorkflowAgentConfig};
use gadk_engine::workflow::{NodeConfig, START, chain, function_node};
use gadk_memory::InMemoryMemoryService;
use gadk_server::rest::{DebugApiConfig, Server, ServerConfig};
use gadk_session::InMemorySessionService;
use http::{Method, StatusCode};
use tower::ServiceExt;

/// Go `uiEndpoint`: one request the ADK web UI makes.
struct UiEndpoint {
    method: Method,
    /// The URL path as the UI builds it, with named placeholders.
    template: &'static str,
    /// Why an entry needs explaining (optional).
    note: &'static str,
    /// The one endpoint the UI opens as a WebSocket.
    web_socket: bool,
}

const fn ep(method: Method, template: &'static str) -> UiEndpoint {
    UiEndpoint {
        method,
        template,
        note: "",
        web_socket: false,
    }
}

/// Go `uiGolden`: every endpoint the embedded ADK web UI calls (39 method/template pairs).
fn ui_golden() -> Vec<UiEndpoint> {
    vec![
        // Endpoints the UI calls without the developer prefix.
        ep(Method::POST, "/run_sse"),
        ep(Method::GET, "/list-apps"),
        ep(Method::GET, "/version"),
        ep(Method::GET, "/apps/{app_name}/users/{user_id}/sessions"),
        ep(Method::POST, "/apps/{app_name}/users/{user_id}/sessions"),
        ep(
            Method::GET,
            "/apps/{app_name}/users/{user_id}/sessions/{session_id}",
        ),
        UiEndpoint {
            note: "the UI's updateSession(), which it uses to rename a session and to write session state",
            ..ep(
                Method::PATCH,
                "/apps/{app_name}/users/{user_id}/sessions/{session_id}",
            )
        },
        ep(
            Method::DELETE,
            "/apps/{app_name}/users/{user_id}/sessions/{session_id}",
        ),
        ep(
            Method::GET,
            "/apps/{app_name}/users/{user_id}/sessions/{session_id}/artifacts/{artifact_name}",
        ),
        ep(
            Method::GET,
            "/apps/{app_name}/users/{user_id}/sessions/{session_id}/artifacts/{artifact_name}/versions/{version}",
        ),
        UiEndpoint {
            web_socket: true,
            note: "opened as a WebSocket; a plain GET cannot complete the handshake, so this only checks that the route exists",
            ..ep(Method::GET, "/run_live")
        },
        // Developer endpoints, under /dev/apps/{app_name} since ADK v2.
        ep(Method::GET, "/dev/apps/{app_name}/build_graph"),
        ep(Method::GET, "/dev/apps/{app_name}/build_graph_image"),
        ep(Method::GET, "/dev/apps/{app_name}/builder"),
        ep(Method::POST, "/dev/apps/{app_name}/builder/save"),
        ep(Method::POST, "/dev/apps/{app_name}/builder/cancel"),
        ep(Method::GET, "/dev/apps/{app_name}/eval_sets"),
        UiEndpoint {
            note: "the UI creates an eval set at the hyphenated spelling and reads it back at the underscored one",
            ..ep(Method::POST, "/dev/apps/{app_name}/eval-sets")
        },
        ep(Method::GET, "/dev/apps/{app_name}/eval_sets/{eval_set_id}"),
        ep(
            Method::DELETE,
            "/dev/apps/{app_name}/eval_sets/{eval_set_id}",
        ),
        ep(
            Method::GET,
            "/dev/apps/{app_name}/eval_sets/{eval_set_id}/evals",
        ),
        ep(
            Method::GET,
            "/dev/apps/{app_name}/eval_sets/{eval_set_id}/evals/{eval_case_id}",
        ),
        ep(
            Method::PUT,
            "/dev/apps/{app_name}/eval_sets/{eval_set_id}/evals/{eval_case_id}",
        ),
        ep(
            Method::DELETE,
            "/dev/apps/{app_name}/eval_sets/{eval_set_id}/evals/{eval_case_id}",
        ),
        ep(
            Method::POST,
            "/dev/apps/{app_name}/eval_sets/{eval_set_id}/add_session",
        ),
        ep(
            Method::POST,
            "/dev/apps/{app_name}/eval_sets/{eval_set_id}/run_eval",
        ),
        ep(Method::GET, "/dev/apps/{app_name}/eval_results"),
        ep(
            Method::GET,
            "/dev/apps/{app_name}/eval_results/{eval_result_id}",
        ),
        ep(Method::GET, "/dev/apps/{app_name}/metrics-info"),
        ep(Method::GET, "/dev/apps/{app_name}/debug/trace/{event_id}"),
        ep(
            Method::GET,
            "/dev/apps/{app_name}/debug/trace/session/{session_id}",
        ),
        ep(
            Method::GET,
            "/dev/apps/{app_name}/users/{user_id}/sessions/{session_id}/events/{event_id}/graph",
        ),
        ep(Method::GET, "/dev/apps/{app_name}/tests"),
        ep(Method::GET, "/dev/apps/{app_name}/tests/{test_name}"),
        ep(Method::PUT, "/dev/apps/{app_name}/tests/{test_name}"),
        ep(Method::DELETE, "/dev/apps/{app_name}/tests/{test_name}"),
        ep(Method::POST, "/dev/apps/{app_name}/tests/rebuild"),
        ep(Method::POST, "/dev/apps/{app_name}/tests/run"),
    ]
}

/// Go `uiContractApp`.
const UI_CONTRACT_APP: &str = "ui_contract_agent";

/// Go `uiPathValues`. `{test_name}` avoids `rebuild` and `run`, which are literal sibling
/// routes.
fn ui_path_value(name: &str) -> Option<&'static str> {
    Some(match name {
        "app_name" => UI_CONTRACT_APP,
        "user_id" => "ui-contract-user",
        "session_id" => "ui-contract-session",
        "artifact_name" => "ui-contract-artifact",
        "version" => "0",
        "event_id" => "ui-contract-event",
        "eval_set_id" => "ui-contract-eval-set",
        "eval_case_id" => "ui-contract-eval-case",
        "eval_result_id" => "ui-contract-eval-result",
        "test_name" => "ui-contract-test",
        _ => return None,
    })
}

/// Go `concretePath`: every `{name}` placeholder replaced by its value; an unknown placeholder
/// is a test bug.
fn concrete_path(template: &str) -> String {
    let mut out = String::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let close = rest[open..].find('}').unwrap() + open;
        let name = &rest[open + 1..close];
        let value = ui_path_value(name).unwrap_or_else(|| {
            panic!(
                "golden template {template:?} uses placeholder {{{name}}} with no value; add one"
            )
        });
        out.push_str(&rest[..open]);
        out.push_str(value);
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
}

/// Go `muxNotFoundBody`: the only 404 that means "no route matched".
const MUX_NOT_FOUND_BODY: &str = "404 page not found";

/// Go `isMuxNotFound`.
fn is_mux_not_found(status: StatusCode, body: &str) -> bool {
    status == StatusCode::NOT_FOUND && body.trim() == MUX_NOT_FOUND_BODY
}

/// Go `noteSuffix`.
fn note_suffix(endpoint: &UiEndpoint) -> String {
    if endpoint.note.is_empty() {
        String::new()
    } else {
        format!("\nNote: {}", endpoint.note)
    }
}

/// Go `newUIContractServer`: in-memory services, a one-node workflow agent, and the debug API
/// on (three of the UI's endpoints are debug-trace routes).
fn new_ui_contract_server() -> axum::Router {
    let echo = function_node::<String, String, _, _>(
        "echo",
        |_ctx: Context, input: String| async move { Ok(input) },
        NodeConfig::default(),
    );
    let root = workflow_agent::new(WorkflowAgentConfig {
        name: UI_CONTRACT_APP.into(),
        edges: chain(&[Arc::clone(&START), echo]),
        ..Default::default()
    })
    .unwrap();
    let mut cfg = ServerConfig::new(Arc::new(InMemorySessionService::new()));
    cfg.artifact_service = Some(Arc::new(InMemoryArtifactService::new()));
    cfg.memory_service = Some(Arc::new(InMemoryMemoryService::new()));
    cfg.agent_loader = Some(Arc::new(SingleLoader::new(root)));
    cfg.debug_api_config = DebugApiConfig {
        include_debug_api: true,
    };
    Server::new(cfg).unwrap().router()
}

/// Half 1: every endpoint the web UI calls reaches a handler. Only 405 and the router's
/// plain-text 404 mean the request never reached one; 200, 400, 500 and 501 all come from a
/// route that exists.
#[tokio::test]
async fn test_ui_contract_every_ui_endpoint_routes() {
    let server = new_ui_contract_server();
    for endpoint in ui_golden() {
        let mut path = concrete_path(endpoint.template);
        let name = format!("{} {}", endpoint.method, endpoint.template);
        if endpoint.web_socket {
            // The UI sends these as query parameters; the handshake cannot complete here, the
            // point is only that the request reaches the handler.
            path.push_str(&format!(
                "?app_name={UI_CONTRACT_APP}&user_id=u&session_id=s"
            ));
        }
        let req = http::Request::builder()
            .method(endpoint.method.clone())
            .uri(&path)
            .header("Content-Type", "application/json")
            .body(Body::from("{}"))
            .unwrap();
        let resp = server.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let allow = resp
            .headers()
            .get("Allow")
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8_lossy(&body);
        assert_ne!(
            status,
            StatusCode::METHOD_NOT_ALLOWED,
            "{name}: {} {path}: server answered 405 Method Not Allowed, so the path is routed but not for this method.\nThe web UI makes this exact request, so it is broken in the browser.\nAllow header: {allow:?}{}",
            endpoint.method,
            note_suffix(&endpoint)
        );
        assert!(
            !is_mux_not_found(status, &body),
            "{name}: {} {path}: server answered the router's {:?}, so no route matches this path at all.\nThe web UI makes this exact request, so it is broken in the browser.{}",
            endpoint.method,
            body.trim(),
            note_suffix(&endpoint)
        );
    }
}

// parity: skipped TestUIContractGoldenListMatchesBundle: it reads the Angular bundle embedded in ADK-Go's cmd/launcher/web/webui/distr; gadk ships no web UI bundle, so there is nothing to extract from
