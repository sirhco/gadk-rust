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

//! Port of Go `tool/mcptoolset/transport_config_test.go` (package `mcptoolset`).
//!
//! Go's `buildTransport` returns a copied `mcp.Transport` whose `http.Client` carries the
//! tracking and auth `RoundTripper`s; the Rust counterpart returns a [`TransportFactory`], and
//! the client it builds is [`HttpFactory`]'s [`AdkHttpClient`](crate::http::AdkHttpClient) over the
//! caller's `reqwest::Client` ([`HttpTarget`]). With auth, the pool builds that client per
//! session (`http_factory_for`), so the auth checks inspect [`HttpFactory::new`] with the
//! provider.

use std::sync::Arc;
use std::time::Duration;

use gadk_auth::{AuthContext, CredentialProvider, static_token};
use http::HeaderMap;
use pretty_assertions::assert_eq;
use rmcp::RoleClient;
use rmcp::model::{ClientCapabilities, ClientConfig, Implementation};
use rmcp::service::RunningService;

use crate::config::{Config, McpTransport, TransportFactory};
use crate::error::BoxError;
use crate::transport::{HttpFactory, HttpTarget, build_transport, transport_config};

const TEST_ENDPOINT: &str = "https://mcp.example/mcp";

const NO_TRANSPORT: &str = "mcptoolset: set Config.Transport or Config.Endpoint";

fn auth() -> Arc<dyn CredentialProvider> {
    static_token("tok")
}

fn with_auth(mut cfg: Config) -> Config {
    cfg.auth = Some(auth());
    cfg
}

/// The caller's client of Go's `base := &http.Client{Timeout: 7 * time.Second}`.
fn base() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(7))
        .build()
        .unwrap()
}

fn caller_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("x-caller", "1".parse().unwrap());
    headers
}

/// Go `callerStreamable`.
fn caller_streamable(client: &reqwest::Client) -> McpTransport {
    McpTransport::StreamableHttp {
        url: TEST_ENDPOINT.into(),
        client: client.clone(),
        headers: caller_headers(),
    }
}

/// A factory that never connects (Go `&mcp.CommandTransport{}` passed through as is).
struct Unused;

#[async_trait::async_trait]
impl TransportFactory for Unused {
    async fn connect(
        &self,
        _client_info: ClientConfig,
    ) -> Result<RunningService<RoleClient, ClientConfig>, BoxError> {
        Err("unused".into())
    }
}

fn build_err(cfg: &Config) -> String {
    match build_transport(cfg) {
        Ok(_) => panic!("buildTransport() = nil error, want error"),
        Err(e) => e.to_string(),
    }
}

/// Go `mustStreamable` + `wantsAuthTransport`: the HTTP client the session would use, and its
/// endpoint.
fn http_client(cfg: &Config) -> HttpFactory {
    let target = HttpTarget::from_transport(&cfg.transport).unwrap_or_else(|| {
        panic!(
            "transport = {}, want streamable HTTP",
            cfg.transport.variant_name()
        )
    });
    HttpFactory::new(
        &target,
        cfg.auth.clone().map(|p| (p, AuthContext::anonymous())),
    )
}

// parity: adapted: Go's zero `Config` has no transport; `McpTransport` is required in Rust, so `Endpoint("")` (which counts as none, R1) stands for it, and Go's `Client` is `client_info`.
#[test]
fn test_new_requires_connection() {
    let empty = Config::new(McpTransport::Endpoint(String::new()));
    let mut client_without_transport = empty.clone();
    client_without_transport.client_info = Some(ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("test", "1"),
    ));
    for (name, cfg) in [
        ("empty config", empty),
        ("client without transport", client_without_transport),
    ] {
        match crate::new(cfg) {
            Ok(_) => panic!("{name}: New() succeeded without a transport or endpoint"),
            Err(e) => assert_eq!(e.to_string(), NO_TRANSPORT, "{name}"),
        }
    }
}

// parity: adapted: Go's copied transports become `HttpTarget` clones of `McpTransport` (a clone cannot mutate the caller's), the `RoundTripper` chain becomes `AdkHttpClient { inner, headers, auth }`, base client settings are compared through `reqwest::Client`'s `Debug` (which shows the 7s timeout), and `MaxRetries`/`DisableStandaloneSSE` have no rmcp field (the transport config's `reinit_on_expired_session(false)` is checked instead). The SSE and typed-nil cases are not ported: rmcp has no legacy SSE client and `McpTransport` has no nil variant. Error cases also assert the R1 texts.
#[test]
fn test_build_transport() {
    let base = base();
    let base_debug = format!("{base:?}");
    assert!(base_debug.contains("Timeout: 7s"), "{base_debug}");

    // "endpoint with auth builds a streamable client carrying the auth transport"
    let cfg = with_auth(Config::new(McpTransport::Endpoint(TEST_ENDPOINT.into())));
    build_transport(&cfg).unwrap();
    let st = http_client(&cfg);
    assert_eq!(st.url, TEST_ENDPOINT);
    assert!(st.client.auth.is_some(), "client carries no auth");

    // "auth wraps a caller streamable transport on a copy, without mutating it"
    let caller = caller_streamable(&base);
    let cfg = with_auth(Config::new(caller.clone()));
    build_transport(&cfg).unwrap();
    let st = http_client(&cfg);
    assert_eq!(st.url, TEST_ENDPOINT);
    assert_eq!(st.client.headers, caller_headers());
    assert!(st.client.auth.is_some(), "client carries no auth");
    assert_eq!(
        format!("{:?}", st.client.inner),
        base_debug,
        "base settings should be copied"
    );
    let McpTransport::StreamableHttp {
        url,
        client,
        headers,
    } = &caller
    else {
        unreachable!()
    };
    assert_eq!(
        (url.as_str(), format!("{client:?}"), headers),
        (TEST_ENDPOINT, base_debug.clone(), &caller_headers()),
        "caller's transport/client was mutated"
    );

    // "auth chains onto the caller's existing HTTP transport"
    let own = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .build()
        .unwrap();
    let cfg = with_auth(Config::new(McpTransport::StreamableHttp {
        url: TEST_ENDPOINT.into(),
        client: own.clone(),
        headers: HeaderMap::new(),
    }));
    let st = http_client(&cfg);
    assert!(st.client.auth.is_some(), "client carries no auth");
    assert_eq!(
        format!("{:?}", st.client.inner),
        format!("{own:?}"),
        "the client under the tracking and auth layers is not the caller's"
    );

    // "auth with a non-streamable transport is an error"
    let command = McpTransport::Command(Arc::new(|| tokio::process::Command::new("true")));
    assert_eq!(
        build_err(&with_auth(Config::new(command))),
        "mcptoolset: Config.Auth requires a streamable HTTP transport; set Config.Endpoint or use McpTransport::StreamableHttp (got Command)"
    );
    assert_eq!(
        build_err(&with_auth(Config::new(McpTransport::Custom(Arc::new(
            Unused
        ))))),
        "mcptoolset: Config.Auth requires a streamable HTTP transport; set Config.Endpoint or use McpTransport::StreamableHttp (got Custom)"
    );

    // "auth without an endpoint or transport is an error"
    assert_eq!(
        build_err(&with_auth(Config::new(McpTransport::Endpoint(
            String::new()
        )))),
        NO_TRANSPORT
    );

    // "endpoint without auth builds a streamable client carrying only the tracking transport"
    let cfg = Config::new(McpTransport::Endpoint(TEST_ENDPOINT.into()));
    build_transport(&cfg).unwrap();
    let st = http_client(&cfg);
    assert_eq!(st.url, TEST_ENDPOINT);
    assert!(st.client.auth.is_none(), "client carries auth");
    assert!(st.client.headers.is_empty());
    assert!(!transport_config(&st.url).reinit_on_expired_session);
    assert_eq!(&*transport_config(&st.url).uri, TEST_ENDPOINT);

    // "streamable transport without auth is copied without mutating the caller's"
    let cfg = Config::new(caller.clone());
    build_transport(&cfg).unwrap();
    let st = http_client(&cfg);
    assert_eq!(st.url, TEST_ENDPOINT);
    assert_eq!(st.client.headers, caller_headers());
    assert!(st.client.auth.is_none(), "client carries auth");
    assert_eq!(
        format!("{:?}", st.client.inner),
        base_debug,
        "preserved fields lost"
    );

    // "a non-HTTP transport is passed through unchanged"
    let custom: Arc<dyn TransportFactory> = Arc::new(Unused);
    let got = build_transport(&Config::new(McpTransport::Custom(custom.clone()))).unwrap();
    assert!(
        Arc::ptr_eq(&got, &custom),
        "transport is not the caller's factory"
    );
    let command = McpTransport::Command(Arc::new(|| tokio::process::Command::new("true")));
    build_transport(&Config::new(command)).unwrap();
}

#[cfg(unix)]
// parity: adapted: Go's `Connect` only starts the process; a gadk `TransportFactory::connect` also runs the MCP `initialize` handshake, so the helper is an `sh -c` child (not Go's re-run test binary) that writes its working directory to stderr and exits, and every connect fails the handshake with `connection closed`. The test checks that each of the three connects started the builder's command with its `current_dir` and `stderr`. Go's "first connect runs the caller's own command" check has no counterpart: `McpTransport::Command` is a `CommandBuilder` run on every connect.
#[tokio::test]
async fn test_command_transport_reconnect() {
    use crate::config::default_client_info;

    let dir = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let stderr_path = out.path().join("stderr");
    let stderr = std::fs::File::create(&stderr_path).unwrap();
    let cwd = dir.path().to_path_buf();
    let cfg = Config::new(McpTransport::Command(Arc::new(move || {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.args(["-c", r#"echo "helper started in $(pwd -P)" >&2"#]);
        cmd.current_dir(&cwd);
        cmd.stderr(stderr.try_clone().unwrap());
        cmd
    })));
    let tr = build_transport(&cfg).unwrap();
    for i in 1..=3 {
        match tr.connect(default_client_info()).await {
            Ok(_) => panic!("Connect #{i}: the helper does not speak MCP, want a handshake error"),
            Err(e) => assert!(
                e.to_string().contains("connection closed"),
                "Connect #{i} error = {e}"
            ),
        }
    }

    let got = std::fs::read_to_string(&stderr_path).unwrap();
    let real_dir = std::fs::canonicalize(dir.path()).unwrap();
    let want = format!("helper started in {}\n", real_dir.display()).repeat(3);
    assert_eq!(got, want, "helper output");
}

// parity: skipped TestCommandTransportReconnectKeepsSettings: gadk's McpTransport::Command is a CommandBuilder run on every connect, so there is no copied exec.Cmd/CommandTransport (Go newCommandTransport/reconnectTransport) whose fields could be dropped
// parity: skipped TestCommandTransportSpecIsIndependent: gadk keeps no saved copy (Go commandTransport.spec) of the caller's exec.Cmd whose Args/Env/ExtraFiles slices could alias the caller's; the CommandBuilder builds a fresh command per connect
// parity: skipped TestCommandTransportHelper: the child process of Go's TestCommandTransportReconnect (the test binary re-run with -test.run); the Rust port uses an `sh -c` child instead
