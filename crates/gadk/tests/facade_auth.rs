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

#![cfg(feature = "gcp")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! R13: `gadk::auth` (with its `gcp` module, on by default) and `gadk::tools::toolauth` resolve
//! through the façade and work end to end.

// The R13 acceptance imports. rustfmt sorts the braced names, so `client, adc` is written
// `adc, client`.
use gadk::auth::{adc, client, gcp::new_provider};
use gadk::tools::toolauth::Config;

use gadk::auth::gcp::ProviderConfig;
use gadk::auth::{AuthContext, Identity, static_token};
use gadk::tools::toolauth::REQUEST_CREDENTIAL_FUNCTION_CALL_NAME;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Accepts one connection, reads the request head, and answers `200 OK` with the request's
/// `Authorization` value (empty when it has none) as the body. Returns that value.
async fn echo_authorization_once(listener: TcpListener) -> String {
    let (mut conn, _) = listener.accept().await.unwrap();
    let mut head = Vec::new();
    let mut chunk = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = conn.read(&mut chunk).await.unwrap();
        assert!(n > 0, "connection closed before the request head ended");
        head.extend_from_slice(&chunk[..n]);
    }
    let head = String::from_utf8(head).unwrap();
    // reqwest sends lowercase header names; match the name case-insensitively anyway.
    let auth = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("authorization")
                .then(|| value.trim().to_string())
        })
        .unwrap_or_default();
    let reply = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        auth.len(),
        auth
    );
    conn.write_all(reply.as_bytes()).await.unwrap();
    conn.shutdown().await.unwrap();
    auth
}

// A runtime is present in case a provider spawns on first use; building `adc` reaches no
// network, because it is lazy.
#[tokio::test]
async fn facade_auth_paths_resolve() {
    // gadk::auth::client over a lazy ADC provider: builds without credentials or network.
    let _http = client(Default::default(), adc(&[]), AuthContext::anonymous());

    // gadk::auth::gcp::new_provider: Go NewProvider's first check (provider.go:184).
    let err = new_provider(ProviderConfig::default())
        .err()
        .expect("an empty scheme name is rejected");
    assert_eq!(err.to_string(), "gcp: new_provider requires a scheme name");

    // gadk::auth::Identity is gadk::agent::Identity.
    let id: gadk::agent::Identity = Identity::default();
    assert_eq!(AuthContext::for_identity(id.clone()).identity, Some(id));

    // gadk::tools::toolauth::Config: an explicit credential key wins (auth_tool.go:105-107),
    // and without_secrets keeps it.
    let cfg = Config {
        credential_key: "my-key".to_string(),
        ..Default::default()
    };
    assert_eq!(cfg.key(), "my-key");
    assert_eq!(cfg.without_secrets().credential_key, "my-key");
    assert_eq!(
        REQUEST_CREDENTIAL_FUNCTION_CALL_NAME,
        "adk_request_credential"
    );
}

#[tokio::test]
async fn facade_auth_client_applies_the_provider() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(echo_authorization_once(listener));

    let http = client(
        Default::default(),
        static_token("t0k3n"),
        AuthContext::anonymous(),
    );
    let body = http
        .get(format!("http://{addr}/"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert_eq!(body, "Bearer t0k3n");
    assert_eq!(server.await.unwrap(), "Bearer t0k3n");
}
