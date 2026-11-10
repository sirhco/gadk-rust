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

//! Ports of ADK-Go `server/adkrest/handler_security_test.go`: the server's request-body limit
//! (`ServerConfig::max_payload_size`, default 10 MiB).

use std::sync::Arc;

use axum::body::Body;
use gadk_server::rest::testutil::record;
use gadk_server::rest::{DEFAULT_MAX_PAYLOAD_SIZE, Server, ServerConfig};
use gadk_session::InMemorySessionService;
use http::StatusCode;

/// Go `defaultPayloadLimit`: hard-coded rather than derived from [`DEFAULT_MAX_PAYLOAD_SIZE`],
/// so changing the constant fails the tests.
const DEFAULT_PAYLOAD_LIMIT: usize = 10 << 20;

/// A server over an in-memory session service with `max_payload_size`.
fn server(max_payload_size: i64) -> Server {
    let mut cfg = ServerConfig::new(Arc::new(InMemorySessionService::new()));
    cfg.max_payload_size = max_payload_size;
    Server::new(cfg).unwrap()
}

/// `{"state": {"padding": "<padding × a>"}}`: valid JSON, so only the limit can refuse it.
fn padded(padding: usize) -> String {
    format!(
        "{{\"state\": {{\"padding\": \"{}\"}}}}",
        "a".repeat(padding)
    )
}

/// Go `srv.ServeHTTP(rec, POST /apps/myapp/users/u1/sessions with payload)`: status and body.
async fn create_session(srv: &Server, payload: String) -> (StatusCode, String) {
    let req = http::Request::post("/apps/myapp/users/u1/sessions")
        .header("Content-Type", "application/json")
        .body(Body::from(payload))
        .unwrap();
    record(&srv.router(), req).await
}

#[tokio::test]
async fn test_server_rejects_oversized_body() {
    let srv = server(1024);
    let (status, body) = create_session(&srv, padded(256 * 1024)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, "http: request body too large\n");
}

// parity: adapted: gadk's body-limit middleware is crate-private, so the default limit and its boundary are pinned through the server (session create with a valid JSON body of exactly the limit, then one byte more) instead of a bare handler
#[tokio::test]
async fn test_max_bytes_middleware_pins_the_default_limit() {
    assert_eq!(DEFAULT_MAX_PAYLOAD_SIZE, 10 << 20);
    let srv = server(0);
    let overhead = padded(0).len();
    let exact = padded(DEFAULT_PAYLOAD_LIMIT - overhead);
    assert_eq!(exact.len(), DEFAULT_PAYLOAD_LIMIT);
    let (status, body) = create_session(&srv, exact).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "body of exactly the default limit: {body}"
    );
    let (status, body) = create_session(&srv, padded(DEFAULT_PAYLOAD_LIMIT - overhead + 1)).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "body one byte over the default limit"
    );
    assert_eq!(body, "http: request body too large\n");
}

#[tokio::test]
async fn test_server_uses_default_max_payload_size() {
    for (name, max) in [("zero", 0), ("negative", -1)] {
        let srv = server(max);
        let (status, body) = create_session(&srv, padded(1024)).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{name}: small body with default limit: {body}"
        );
        let (status, body) = create_session(&srv, padded(DEFAULT_PAYLOAD_LIMIT + 4096)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{name}: over-default body");
        assert_eq!(
            body, "http: request body too large\n",
            "{name}: over-default body response"
        );
    }
}
