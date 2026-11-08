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

//! Port of Go `tool/mcptoolset/tracking_test.go` (package `mcptoolset`; Task 7).
//!
//! Go's `trackingTransport` is an `http.RoundTripper` under the caller's `http.Client`; the Rust
//! counterpart is [`AdkHttpClient`], which adds the same headers before delegating to the
//! caller's `reqwest::Client`. Go's `headerRecorder` is [`HttpTestServer::requests`].

use std::sync::Arc;

use gadk_auth::{AuthContext, static_token};
use http::header::USER_AGENT;
use http::{HeaderMap, HeaderName};
use pretty_assertions::assert_eq;
use rmcp::model::{ClientCapabilities, ClientConfig, Implementation};

use crate::config::{Config, McpTransport, TransportFactory};
use crate::http::AdkHttpClient;
use crate::testutil::{HttpTestServer, ToolServer};
use crate::transport::{HttpFactory, HttpTarget, build_transport, http_factory_for};

const TEST_ENDPOINT: &str = "https://mcp.example/mcp";

fn ua_client() -> ClientConfig {
    ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("ua_client", "v1"),
    )
}

fn header_map(pairs: &[(&'static str, &str)]) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (name, value) in pairs {
        headers.append(*name, value.parse().unwrap());
    }
    headers
}

/// One `TestTrackingHeadersOnMCPRequests` case: the factory for the test server's URL.
struct Case {
    name: &'static str,
    transport: fn(&str) -> Arc<dyn TransportFactory>,
    /// The `authorization` every request must carry, if any.
    want_auth: Option<&'static str>,
    /// The `user-agent` every request must carry, when not the labels alone.
    want_ua: Option<String>,
}

// parity: adapted: Go's caller `http.Client` transports become reqwest clients and configured headers. A user-agent set by the caller's own client (Go "replaces the labels" / "is not sent") is overridden by the labels: reqwest only adds a client default header the request lacks (accepted divergence P5). The configured `headers` take the place of a caller value that is merged. The SSE case is not ported (rmcp has no legacy SSE client), and "endpoint with auth" builds the pooled session's factory (`http_factory_for`).
#[tokio::test]
async fn test_tracking_headers_on_mcp_requests() {
    let want = gadk_platform::tracking_header();
    let cases = [
        Case {
            name: "endpoint",
            transport: |url| {
                build_transport(&Config::new(McpTransport::Endpoint(url.into()))).unwrap()
            },
            want_auth: None,
            want_ua: None,
        },
        Case {
            name: "caller streamable transport with its own client",
            transport: |url| {
                build_transport(&Config::new(McpTransport::StreamableHttp {
                    url: url.into(),
                    client: reqwest::Client::new(),
                    headers: HeaderMap::new(),
                }))
                .unwrap()
            },
            want_auth: None,
            want_ua: None,
        },
        Case {
            name: "caller client that sets User-Agent keeps the labels",
            transport: |url| {
                build_transport(&Config::new(McpTransport::StreamableHttp {
                    url: url.into(),
                    client: reqwest::Client::builder()
                        .user_agent("caller-app/1.0")
                        .build()
                        .unwrap(),
                    headers: HeaderMap::new(),
                }))
                .unwrap()
            },
            want_auth: None,
            want_ua: None,
        },
        Case {
            name: "caller headers' User-Agent is kept after the labels",
            transport: |url| {
                build_transport(&Config::new(McpTransport::StreamableHttp {
                    url: url.into(),
                    client: reqwest::Client::new(),
                    headers: header_map(&[("user-agent", "caller-app/1.0")]),
                }))
                .unwrap()
            },
            want_auth: None,
            want_ua: Some(format!(
                "{} caller-app/1.0",
                gadk_platform::tracking_header()
            )),
        },
        Case {
            name: "endpoint with auth",
            transport: |url| {
                let target =
                    HttpTarget::from_transport(&McpTransport::Endpoint(url.into())).unwrap();
                http_factory_for(&target, static_token("tok"), AuthContext::anonymous())
            },
            want_auth: Some("Bearer tok"),
            want_ua: None,
        },
    ];
    for tt in cases {
        let server = HttpTestServer::start(|| ToolServer::new("ua_server", "v1")).await;
        let factory = (tt.transport)(&server.url());
        let session = factory
            .connect(ua_client())
            .await
            .unwrap_or_else(|e| panic!("{}: connect error = {e}", tt.name));
        // Concurrent calls share one client.
        let calls = (0..4).map(|_| {
            let peer = session.peer().clone();
            tokio::spawn(async move { peer.list_tools(None).await })
        });
        for call in futures::future::join_all(calls).await {
            if let Err(e) = call.unwrap() {
                panic!("{}: ListTools() error = {e}", tt.name);
            }
        }
        session.cancel().await.unwrap();

        let got = server.requests();
        assert!(!got.is_empty(), "{}: server received no requests", tt.name);
        // Rust addition: the session's DELETE carries them too (R14).
        for method in [http::Method::POST, http::Method::DELETE] {
            assert!(
                got.iter().any(|r| r.method == method),
                "{}: no {method} among {got:?}",
                tt.name
            );
        }
        for (i, r) in got.iter().enumerate() {
            let want_ua = tt.want_ua.clone().unwrap_or_else(|| want.clone());
            for (name, want) in [("user-agent", &want_ua), ("x-goog-api-client", &want)] {
                let values: Vec<_> = r.headers.get_all(name).iter().collect();
                assert_eq!(values, [want.as_str()], "{}: request {i}: {name}", tt.name);
            }
            if let Some(want_auth) = tt.want_auth {
                assert_eq!(
                    r.headers.get("authorization").map(|v| v.to_str().unwrap()),
                    Some(want_auth),
                    "{}: request {i}: Authorization header missing or wrong",
                    tt.name
                );
            }
        }
    }
}

// parity: adapted: Go drives `trackingTransport.RoundTrip` with a stub base; the Rust step is `AdkHttpClient::tracked_headers` (the configured headers stand in for the request's), and Rust tests run in parallel by default.
#[test]
fn test_tracking_transport_merges_caller_headers() {
    let adk = gadk_platform::tracking_header();
    struct Tt {
        name: &'static str,
        header: HeaderMap,
        want: String,
        want_xgac: Option<String>,
    }
    let tests = [
        Tt {
            name: "no caller value",
            header: HeaderMap::new(),
            want: adk.clone(),
            want_xgac: None,
        },
        Tt {
            name: "caller x-goog-api-client is kept after ADK tokens",
            header: header_map(&[("x-goog-api-client", "gl-go/1.0 my-lib/2")]),
            want: adk.clone(),
            want_xgac: Some(format!("{adk} gl-go/1.0 my-lib/2")),
        },
        Tt {
            name: "caller value is kept after ADK tokens",
            header: header_map(&[("user-agent", "my-app/1.0")]),
            want: format!("{adk} my-app/1.0"),
            want_xgac: None,
        },
        Tt {
            name: "duplicate tokens are dropped",
            header: header_map(&[("user-agent", &format!("my-app/1.0 {adk}"))]),
            want: format!("{adk} my-app/1.0"),
            want_xgac: None,
        },
        Tt {
            name: "repeated header values are joined",
            header: header_map(&[("user-agent", "a/1"), ("user-agent", "b/2")]),
            want: format!("{adk} a/1 b/2"),
            want_xgac: None,
        },
    ];
    for tt in tests {
        let orig = tt.header.clone();
        let client = AdkHttpClient {
            inner: reqwest::Client::new(),
            headers: tt.header,
            auth: None,
        };
        let sent = client.tracked_headers(Default::default());
        assert_eq!(
            sent.get(&USER_AGENT).map(|v| v.to_str().unwrap()),
            Some(tt.want.as_str()),
            "{}: User-Agent",
            tt.name
        );
        let want_xgac = tt.want_xgac.unwrap_or_else(|| adk.clone());
        assert_eq!(
            sent.get(&HeaderName::from_static("x-goog-api-client"))
                .map(|v| v.to_str().unwrap()),
            Some(want_xgac.as_str()),
            "{}: X-Goog-Api-Client",
            tt.name
        );
        assert_eq!(
            client.headers, orig,
            "{}: caller header was mutated",
            tt.name
        );
    }
}

// parity: adapted: reqwest has no process-wide default client to copy (Go `http.DefaultClient`); `Endpoint` starts every factory, with or without auth, from a fresh `reqwest::Client::new()`, compared through its `Debug` (which shows a timeout when one is set).
#[test]
fn test_tracking_client_starts_from_default_client() {
    let fresh = format!("{:?}", reqwest::Client::new());
    assert!(!fresh.contains("Timeout"), "{fresh}");
    for auth in [false, true] {
        let target =
            HttpTarget::from_transport(&McpTransport::Endpoint(TEST_ENDPOINT.into())).unwrap();
        let provider = auth.then(|| (static_token("tok"), AuthContext::anonymous()));
        let factory = HttpFactory::new(&target, provider);
        assert_eq!(
            format!("{:?}", factory.client.inner),
            fresh,
            "auth {auth}: the Endpoint client is not reqwest's default"
        );
        assert_eq!(factory.client.auth.is_some(), auth);
    }
}

// parity: adapted: Go's swapped `http.DefaultClient` transport becomes a caller client whose default `authorization` header stands for a credential of its own; the request goes through the auth `AdkHttpClient` of `http_factory_for`'s kind, and the server must see only the provider's credential.
#[tokio::test]
async fn test_auth_credential_bypasses_default_client_transport() {
    let server = HttpTestServer::start(|| ToolServer::new("ua_server", "v1")).await;
    server.set_front(Some(Arc::new(|_, _| {
        Some(http::Response::new(String::new()))
    })));
    let mut service = HeaderMap::new();
    service.insert("authorization", "Bearer service-tok".parse().unwrap());
    let transports = [
        McpTransport::Endpoint(server.url()),
        McpTransport::StreamableHttp {
            url: server.url(),
            client: reqwest::Client::builder()
                .default_headers(service)
                .build()
                .unwrap(),
            headers: HeaderMap::new(),
        },
    ];
    for transport in transports {
        let target = HttpTarget::from_transport(&transport).unwrap();
        let factory = HttpFactory::new(
            &target,
            Some((static_token("user-tok"), AuthContext::anonymous())),
        );
        use rmcp::transport::streamable_http_client::StreamableHttpClient as _;
        factory
            .client
            .delete_session(server.url().into(), "s-1".into(), None, Default::default())
            .await
            .unwrap();
    }
    let got: Vec<Vec<_>> = server
        .requests()
        .iter()
        .map(|r| r.headers.get_all("authorization").iter().cloned().collect())
        .collect();
    assert_eq!(
        got,
        [["Bearer user-tok"], ["Bearer user-tok"]],
        "server received Authorization {got:?}"
    );
}
