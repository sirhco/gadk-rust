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

//! Ports of ADK-Go `server/adkrest/originguard_integration_test.go`: the origin guard and the
//! DNS-rebinding check of a whole [`Server`] on a real loopback listener (issue #1154), for
//! `/run_live` handshakes and REST calls. A rebound page controls `Host` and `Origin`, so the
//! clients dial the real listener and send whatever `Host` the case names.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use gadk_artifact::InMemoryArtifactService;
use gadk_memory::InMemoryMemoryService;
use gadk_server::rest::testutil::{serve_loopback, single_agent_loader};
use gadk_server::rest::{Server, ServerConfig};
use gadk_session::InMemorySessionService;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Error as WsError;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

const GUARD_TEST_APP: &str = "test-agent";

/// A server with the guard tests' agent and in-memory services; `bind_host` and
/// `allowed_origins` as the case configures them.
fn guard_config(bind_host: &str, allowed_origins: &[&str]) -> ServerConfig {
    let mut cfg = ServerConfig::new(Arc::new(InMemorySessionService::new()));
    cfg.agent_loader = Some(single_agent_loader(GUARD_TEST_APP));
    cfg.allowed_origins = allowed_origins.iter().map(|o| (*o).to_owned()).collect();
    cfg.bind_host = bind_host.into();
    cfg
}

/// Go `httptest.NewServer(srv)`: serves `cfg` on 127.0.0.1 and returns the listener's address.
async fn serve(cfg: ServerConfig) -> SocketAddr {
    serve_loopback(Server::new(cfg).unwrap().router())
        .await
        .unwrap()
}

/// Go `newGuardServer`: a loopback bind declared, with in-memory memory and artifact services.
async fn new_guard_server(allowed_origins: &[&str]) -> SocketAddr {
    let mut cfg = guard_config("127.0.0.1", allowed_origins);
    cfg.memory_service = Some(Arc::new(InMemoryMemoryService::new()));
    cfg.artifact_service = Some(Arc::new(InMemoryArtifactService::new()));
    serve(cfg).await
}

/// Go `newUndeclaredBindServer`: no bind declared.
async fn new_undeclared_bind_server(allowed_origins: &[&str]) -> SocketAddr {
    serve(guard_config("", allowed_origins)).await
}

/// Go `portOf`: the `:port` suffix of the listener's address.
fn port_of(addr: SocketAddr) -> String {
    format!(":{}", addr.port())
}

/// Go `dialRunLive`: a `/run_live` handshake on the real listener with `host` as `Host` and
/// `origin` (when not empty) as `Origin`; returns the handshake status.
async fn dial_run_live(addr: SocketAddr, host: &str, origin: &str) -> u16 {
    let url = format!("ws://{host}/run_live?appName={GUARD_TEST_APP}&userId=u1&sessionId=s1");
    let mut req = url.as_str().into_client_request().unwrap();
    if !origin.is_empty() {
        req.headers_mut().insert("Origin", origin.parse().unwrap());
    }
    let tcp = TcpStream::connect(addr).await.unwrap();
    match tokio_tungstenite::client_async(req, tcp).await {
        Ok((_conn, resp)) => resp.status().as_u16(),
        Err(WsError::Http(resp)) => resp.status().as_u16(),
        Err(e) => panic!("Dial({url}) returned no response: {e}"),
    }
}

/// `GET /list-apps` on the real listener with `host` as `Host` and `origin` (when not empty) as
/// `Origin`; returns the status.
async fn get_list_apps(addr: SocketAddr, host: &str, origin: &str) -> u16 {
    let mut conn = TcpStream::connect(addr).await.unwrap();
    let mut req = format!("GET /list-apps HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
    if !origin.is_empty() {
        req.push_str(&format!("Origin: {origin}\r\n"));
    }
    req.push_str("\r\n");
    conn.write_all(req.as_bytes()).await.unwrap();
    let mut resp = Vec::new();
    conn.read_to_end(&mut resp).await.unwrap();
    let resp = String::from_utf8_lossy(&resp);
    resp.split(' ').nth(1).unwrap().parse().unwrap()
}

#[tokio::test]
async fn test_run_live_rejects_rebinding_and_foreign_origins() {
    let addr = new_guard_server(&[]).await;
    let loopback = addr.to_string();
    let rebound = format!("evil.com{}", port_of(addr));
    let rebound_origin = format!("http://{rebound}");
    let same_origin = format!("http://{loopback}");
    for (name, host, origin, want) in [
        // The PoC's DNS-rebinding case: the page controls Origin and Host alike.
        (
            "rebound origin and host",
            rebound.as_str(),
            rebound_origin.as_str(),
            403,
        ),
        // A browser omits Origin on a request it considers same-origin.
        ("rebound host with no origin", rebound.as_str(), "", 403),
        (
            "cross-site browser",
            loopback.as_str(),
            "http://evil.com",
            403,
        ),
        // A non-browser client on this machine; the loopback bind keeps it local.
        ("loopback client with no origin", loopback.as_str(), "", 101),
        (
            "same-origin browser",
            loopback.as_str(),
            same_origin.as_str(),
            101,
        ),
    ] {
        assert_eq!(
            dial_run_live(addr, host, origin).await,
            want,
            "{name}: /run_live handshake status"
        );
    }
}

/// The web launcher's case until it binds loopback: no declared bind, so the Host check is off,
/// and the Origin check refuses a rebound page on its own, even with an allowlist configured.
#[tokio::test]
async fn test_run_live_rejects_rebinding_without_declared_bind() {
    let addr = new_undeclared_bind_server(&["http://localhost:8080"]).await;
    let rebound = format!("evil.com{}", port_of(addr));
    assert_eq!(
        dial_run_live(addr, &rebound, &format!("http://{rebound}")).await,
        403,
        "/run_live handshake status"
    );
}

/// With no declared bind, a request naming a host we do not recognize is served (sidecar
/// proxies and the like arrive over loopback under such a Host, without an Origin).
#[tokio::test]
async fn test_undeclared_bind_serves_foreign_hosts() {
    let addr = new_undeclared_bind_server(&[]).await;
    assert_eq!(
        get_list_apps(addr, "adk.internal", "").await,
        200,
        "GET /list-apps status"
    );
}

/// A web UI served by a separate dev server: Origin and Host differ, the origin is listed.
#[tokio::test]
async fn test_run_live_accepts_configured_origin() {
    const DEV_UI: &str = "http://localhost:4200";
    let addr = new_guard_server(&[DEV_UI]).await;
    assert_eq!(
        dial_run_live(addr, &addr.to_string(), DEV_UI).await,
        101,
        "/run_live handshake status"
    );
}

#[tokio::test]
async fn test_rest_rejects_rebinding_and_foreign_origins() {
    let addr = new_guard_server(&[]).await;
    let loopback = addr.to_string();
    let same_origin = format!("http://{loopback}");
    for (name, host, origin, want) in [
        ("rebound host", "evil.com", "", 403),
        (
            "cross-site browser",
            loopback.as_str(),
            "http://evil.com",
            403,
        ),
        (
            "same-origin browser",
            loopback.as_str(),
            same_origin.as_str(),
            200,
        ),
        ("loopback client with no origin", loopback.as_str(), "", 200),
    ] {
        assert_eq!(
            get_list_apps(addr, host, origin).await,
            want,
            "{name}: GET /list-apps status"
        );
    }
}

/// The documented escape hatch: a single `*` turns both checks off.
#[tokio::test]
async fn test_wildcard_origin_disables_both_checks() {
    let addr = new_guard_server(&["*"]).await;
    assert_eq!(
        get_list_apps(addr, "anything.example.com", "http://evil.com").await,
        200,
        "GET /list-apps status"
    );
}

/// Not a Go test: Go's `net/http` server answers `OPTIONS *` (200, `Content-Length: 0`) and a
/// request target whose escapes do not decode (`400 Bad Request`, `Connection: close`) before
/// any handler runs, so a forbidden Host or Origin never reaches the guard for them.
#[tokio::test]
async fn server_answers_precede_the_origin_guard() {
    let addr = new_guard_server(&[]).await;
    let loopback = addr.to_string();
    for (host, origin) in [
        ("evil.com", ""),
        (loopback.as_str(), "http://evil.com"),
        ("evil.com", "http://evil.com"),
    ] {
        // The 400 case sends no `Connection: close` of its own, so the `connection: close` it
        // gets back (and the closed connection that ends the read) is the server's answer.
        for (method, target, close, want_head, want_header, want_body) in [
            (
                "OPTIONS",
                "*",
                true,
                "HTTP/1.1 200 OK",
                "content-length: 0",
                "",
            ),
            (
                "GET",
                "/list-apps%zz",
                false,
                "HTTP/1.1 400 Bad Request",
                "connection: close",
                "400 Bad Request",
            ),
        ] {
            let mut conn = TcpStream::connect(addr).await.unwrap();
            let mut req = format!("{method} {target} HTTP/1.1\r\nHost: {host}\r\n");
            if close {
                req.push_str("Connection: close\r\n");
            }
            if !origin.is_empty() {
                req.push_str(&format!("Origin: {origin}\r\n"));
            }
            req.push_str("\r\n");
            conn.write_all(req.as_bytes()).await.unwrap();
            let mut resp = Vec::new();
            tokio::time::timeout(Duration::from_secs(5), conn.read_to_end(&mut resp))
                .await
                .unwrap_or_else(|_| {
                    panic!("{method} {target}: the server did not close the connection")
                })
                .unwrap();
            let resp = String::from_utf8_lossy(&resp).into_owned();
            let (head, body) = resp.split_once("\r\n\r\n").unwrap();
            let what = format!("{method} {target} (Host {host:?}, Origin {origin:?})");
            assert!(head.starts_with(want_head), "{what}: {resp}");
            assert!(
                head.to_ascii_lowercase().lines().any(|l| l == want_header),
                "{what}: want `{want_header}`: {resp}"
            );
            assert_eq!(body, want_body, "{what}");
        }
    }
}
