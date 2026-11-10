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

//! R26: `gadk::server` (feature `server`, not on by default) is `gadk_server`; a server built
//! through the façade serves the ADK REST API on a loopback socket.

use std::sync::Arc;

use gadk::agent::{AgentConfig, SingleLoader};
use gadk::server::authn::{self, Authenticator, Caller};
use gadk::server::authz::{self, Authorizer};
use gadk::server::rest::{LocalAddr, Server, ServerConfig, ServerError};
use gadk::session::InMemorySessionService;
use pretty_assertions::assert_eq;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Compiles only if each façade path names the `gadk_server` item.
#[test]
fn facade_server_paths_are_gadk_server() {
    fn same<T: ?Sized>(_: Option<&T>, _: Option<&T>) {}
    same::<Server>(None, None::<&gadk_server::rest::Server>);
    same::<ServerConfig>(None, None::<&gadk_server::rest::ServerConfig>);
    same::<ServerError>(None, None::<&gadk_server::rest::ServerError>);
    same::<LocalAddr>(None, None::<&gadk_server::rest::LocalAddr>);
    same::<Caller>(None, None::<&gadk_server::authn::Caller>);
    same::<dyn Authenticator>(None, None::<&dyn gadk_server::authn::Authenticator>);
    same::<dyn Authorizer>(None, None::<&dyn gadk_server::authz::Authorizer>);
    assert_eq!(authn::USER_ID_HEADER, "X-Goog-Authenticated-User-Id");
    let _ = (authn::noop(), authz::strict());
}

/// `GET <path>` on a fresh connection; the raw response.
async fn get(addr: std::net::SocketAddr, path: &str) -> String {
    let mut conn = tokio::net::TcpStream::connect(addr).await.unwrap();
    conn.write_all(
        format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n").as_bytes(),
    )
    .await
    .unwrap();
    let mut out = String::new();
    conn.read_to_string(&mut out).await.unwrap();
    out
}

#[tokio::test]
async fn facade_server_serves_the_rest_api() {
    let agent = gadk::agent::new_agent(AgentConfig {
        name: "app".into(),
        ..Default::default()
    })
    .unwrap();
    let mut cfg = ServerConfig::new(Arc::new(InMemorySessionService::new()));
    cfg.agent_loader = Some(Arc::new(SingleLoader::new(agent)));
    cfg.bind_host = "127.0.0.1".into();
    let server = Server::new(cfg).unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = server
        .router()
        .into_make_service_with_connect_info::<LocalAddr>();
    let serve = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let health = get(addr, "/health").await;
    assert!(health.starts_with("HTTP/1.1 200 OK\r\n"), "{health}");
    assert!(
        health.ends_with("\r\n\r\n{\"status\":\"ok\"}\n"),
        "{health}"
    );
    let apps = get(addr, "/list-apps").await;
    assert!(apps.starts_with("HTTP/1.1 200 OK\r\n"), "{apps}");
    assert!(apps.ends_with("\r\n\r\n[\"app\"]\n"), "{apps}");
    serve.abort();
}
