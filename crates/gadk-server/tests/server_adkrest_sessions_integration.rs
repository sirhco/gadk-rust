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

//! Port of Go `server/adkrest/sessions_integration_test.go`: session creation with a chunked
//! request body (no `Content-Length`) over a real loopback socket.

use std::net::SocketAddr;
use std::sync::Arc;

use gadk_server::rest::testutil::{authenticate_as, serve_loopback};
use gadk_server::rest::{Server, ServerConfig};
use gadk_session::InMemorySessionService;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const TARGET: &str = "/apps/test-app/users/test-user/sessions/test-session";

/// Go `newChunkedSessionServer`: a server with every caller authenticated as
/// `authenticated_user_id`, on a loopback listener.
async fn new_chunked_session_server(authenticated_user_id: &'static str) -> SocketAddr {
    let mut cfg = ServerConfig::new(Arc::new(InMemorySessionService::new()));
    cfg.authenticator = Some(authenticate_as(authenticated_user_id));
    serve_loopback(Server::new(cfg).unwrap().router())
        .await
        .unwrap()
}

/// POSTs `body` to [`TARGET`] with `Transfer-Encoding: chunked` (one chunk, or none when
/// empty) and no `Content-Length`, and returns the status and body of the answer.
async fn post_chunked(addr: SocketAddr, content_type: Option<&str>, body: &str) -> (u16, String) {
    let mut req = format!(
        "POST {TARGET} HTTP/1.1\r\nHost: {addr}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n"
    );
    if let Some(ct) = content_type {
        req.push_str(&format!("Content-Type: {ct}\r\n"));
    }
    req.push_str("\r\n");
    if !body.is_empty() {
        req.push_str(&format!("{:x}\r\n{body}\r\n", body.len()));
    }
    req.push_str("0\r\n\r\n");
    let mut conn = TcpStream::connect(addr).await.unwrap();
    conn.write_all(req.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    conn.read_to_end(&mut raw).await.unwrap();
    let raw = String::from_utf8(raw).unwrap();
    let (head, rest) = raw.split_once("\r\n\r\n").unwrap();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    let body = if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        dechunk(rest)
    } else {
        rest.to_owned()
    };
    (status, body)
}

/// Decodes a complete chunked body.
fn dechunk(mut s: &str) -> String {
    let mut out = String::new();
    loop {
        let (size, rest) = s.split_once("\r\n").unwrap();
        let size = usize::from_str_radix(size.trim(), 16).unwrap();
        if size == 0 {
            return out;
        }
        out.push_str(&rest[..size]);
        s = &rest[size + 2..];
    }
}

#[tokio::test]
async fn test_create_session_preserves_chunked_body() {
    let addr = new_chunked_session_server("test-user").await;
    let (status, body) = post_chunked(
        addr,
        Some("application/json"),
        r#"{
		"state":{"theme":"dark"},
		"events":[{"id":"event-1","invocationId":"invocation-1","author":"user"}]
	}"#,
    )
    .await;
    assert_eq!(status, 200, "create session status; body: {body}");
    let got: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(got["state"]["theme"], "dark", "created session state theme");
    let events = got["events"].as_array().unwrap();
    assert_eq!(events.len(), 1, "created session event count");
    assert_eq!(events[0]["id"], "event-1", "created session event ID");
    assert_eq!(
        events[0]["invocationId"], "invocation-1",
        "created session event invocation ID"
    );
    assert_eq!(events[0]["author"], "user", "created session event author");
}

#[tokio::test]
async fn test_create_session_allows_empty_chunked_body() {
    let addr = new_chunked_session_server("test-user").await;
    let (status, body) = post_chunked(addr, None, "").await;
    assert_eq!(status, 200, "create session status; body: {body}");
    let got: Value = serde_json::from_str(&body).unwrap();
    assert!(
        got["state"].as_object().is_none_or(|m| m.is_empty()),
        "created session state = {}, want empty state",
        got["state"]
    );
    assert!(
        got["events"].as_array().is_none_or(Vec::is_empty),
        "created session events = {}, want no events",
        got["events"]
    );
}

#[tokio::test]
async fn test_create_session_rejects_malformed_chunked_body() {
    let addr = new_chunked_session_server("test-user").await;
    let (status, body) = post_chunked(addr, Some("application/json"), r#"{"state":"#).await;
    assert_eq!(status, 400, "create session status; body: {body}");
}
