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

//! Transport resolution (port of ADK-Go `tool/mcptoolset/set.go` `buildTransport` and
//! `httpClient`): validates a [`Config`] and turns its [`McpTransport`] into a
//! [`TransportFactory`].

use std::sync::Arc;

use gadk_auth::{AuthContext, CredentialProvider};
use rmcp::RoleClient;
use rmcp::model::ClientConfig;
use rmcp::service::RunningService;
use rmcp::service::{RxJsonRpcMessage, TxJsonRpcMessage};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::Transport;
use rmcp::transport::async_rw::AsyncRwTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;

use crate::config::{CommandBuilder, Config, McpTransport, TransportFactory, serve_client};
use crate::error::{BoxError, Error};
use crate::http::AdkHttpClient;

/// Go `buildTransport`: validates `cfg` (R1) and returns the connector factory of its transport.
/// An HTTP transport's factory builds an [`AdkHttpClient`](crate::http::AdkHttpClient) with
/// `auth: None`; with `auth`, the credential session pool instead calls [`http_factory_for`] per
/// session, so the returned factory is unused.
///
/// Errors: [`Error::NoTransport`] for `Endpoint("")` (Go: no `Transport` and an empty
/// `Endpoint`), then [`Error::AuthRequiresHttp`] for `auth` with a `Command` or `Custom`
/// transport. A `Custom` factory is returned as given.
pub(crate) fn build_transport(cfg: &Config) -> Result<Arc<dyn TransportFactory>, Error> {
    let factory: Arc<dyn TransportFactory> = match &cfg.transport {
        McpTransport::Endpoint(_) | McpTransport::StreamableHttp { .. } => {
            let target = HttpTarget::from_transport(&cfg.transport).ok_or(Error::NoTransport)?;
            Arc::new(HttpFactory::new(&target, None))
        }
        McpTransport::Command(build) => Arc::new(CommandFactory {
            build: build.clone(),
        }),
        McpTransport::Custom(factory) => factory.clone(),
    };
    if cfg.auth.is_some()
        && matches!(
            cfg.transport,
            McpTransport::Command(_) | McpTransport::Custom(_)
        )
    {
        return Err(Error::AuthRequiresHttp(cfg.transport.variant_name()));
    }
    Ok(factory)
}

/// The HTTP factory of one pooled session: every request carries `auth`'s credential resolved
/// for `cx`, the identity that created the session (Go `identityOnlyContext`).
pub(crate) fn http_factory_for(
    target: &HttpTarget,
    auth: Arc<dyn CredentialProvider>,
    cx: AuthContext,
) -> Arc<dyn TransportFactory> {
    Arc::new(HttpFactory::new(target, Some((auth, cx))))
}

/// The URL, base client and extra headers of an `Endpoint` or `StreamableHttp` transport.
#[derive(Clone)]
pub(crate) struct HttpTarget {
    /// The MCP endpoint URL.
    pub(crate) url: String,
    /// The base HTTP client (`reqwest::Client::new()` for `Endpoint`).
    pub(crate) client: reqwest::Client,
    /// Extra headers for every request (empty for `Endpoint`).
    pub(crate) headers: http::HeaderMap,
}

impl HttpTarget {
    /// `Some` for `Endpoint` (non-empty URL) and `StreamableHttp`, `None` otherwise.
    ///
    /// `Endpoint` gets a fresh `reqwest::Client::new()`: reqwest's default redirect policy
    /// (follows up to 10, dropping sensitive headers cross-origin, like Go's `http.Client`) and
    /// no cookie store. There is no process-wide default client to share (Go copies
    /// `http.DefaultClient` without auth and starts from a zero client with it).
    pub(crate) fn from_transport(t: &McpTransport) -> Option<HttpTarget> {
        match t {
            McpTransport::Endpoint(url) if !url.is_empty() => Some(HttpTarget {
                url: url.clone(),
                client: reqwest::Client::new(),
                headers: http::HeaderMap::new(),
            }),
            McpTransport::StreamableHttp {
                url,
                client,
                headers,
            } => Some(HttpTarget {
                url: url.clone(),
                client: client.clone(),
                headers: headers.clone(),
            }),
            _ => None,
        }
    }
}

/// The streamable-HTTP transport config of every HTTP session: `reinit_on_expired_session(false)`,
/// so an HTTP 404 on a session reaches the connection refresher, which owns recovery (R8).
/// rmcp's default SSE `retry_config` for the standalone GET stream is kept.
pub(crate) fn transport_config(url: &str) -> StreamableHttpClientTransportConfig {
    StreamableHttpClientTransportConfig::with_uri(url).reinit_on_expired_session(false)
}

/// The factory of an `Endpoint` or `StreamableHttp` transport (Go: the copied
/// `*mcp.StreamableClientTransport` with the client of `httpClient`).
pub(crate) struct HttpFactory {
    /// The MCP endpoint URL.
    pub(crate) url: String,
    /// The client every session of this factory sends through.
    pub(crate) client: AdkHttpClient,
}

impl HttpFactory {
    /// Go `httpClient`: the target's client and headers behind the tracking headers and, with
    /// `auth`, the credential.
    pub(crate) fn new(
        target: &HttpTarget,
        auth: Option<(Arc<dyn CredentialProvider>, AuthContext)>,
    ) -> Self {
        Self {
            url: target.url.clone(),
            client: AdkHttpClient {
                inner: target.client.clone(),
                headers: target.headers.clone(),
                auth,
            },
        }
    }
}

#[async_trait::async_trait]
impl TransportFactory for HttpFactory {
    async fn connect(
        &self,
        client_info: ClientConfig,
    ) -> Result<RunningService<RoleClient, ClientConfig>, BoxError> {
        let transport = StreamableHttpClientTransport::with_client(
            self.client.clone(),
            transport_config(&self.url),
        );
        serve_client(client_info, transport).await
    }
}

/// The factory of a `Command` transport (Go `*mcp.CommandTransport`): every connect spawns a
/// fresh child from the builder, since a spawned `Command` cannot be reused.
struct CommandFactory {
    /// Builds the child's command.
    build: CommandBuilder,
}

#[async_trait::async_trait]
impl TransportFactory for CommandFactory {
    async fn connect(
        &self,
        client_info: ClientConfig,
    ) -> Result<RunningService<RoleClient, ClientConfig>, BoxError> {
        let child = ChildTransport::spawn((self.build)())?;
        serve_client(client_info, child).await
    }
}

/// How long [`ChildTransport::close`] waits for the child to exit after closing its stdin (go-sdk
/// `CommandTransport`'s default `terminateDuration`).
const CHILD_TERMINATE_DURATION: std::time::Duration = std::time::Duration::from_secs(5);

/// The client side of a child process speaking MCP over stdio (go-sdk `CommandTransport` /
/// `pipeRWC`). Unlike rmcp's `TokioChildProcess`, whose builder always sets the child's stderr
/// (to `Stdio::inherit()` by default), it keeps every setting of the caller's command, its
/// stderr included (Go 1a84f04f keeps `cmd.Stderr` on every connect); only stdin and stdout are
/// replaced by pipes.
struct ChildTransport {
    /// The running child; it is killed when the transport is dropped (`kill_on_drop`).
    child: tokio::process::Child,
    /// JSON-RPC over the child's stdout (read) and stdin (write).
    io: AsyncRwTransport<RoleClient, tokio::process::ChildStdout, tokio::process::ChildStdin>,
}

impl ChildTransport {
    /// Spawns `cmd` with piped stdin and stdout.
    fn spawn(mut cmd: tokio::process::Command) -> std::io::Result<Self> {
        // Divergence (ledger 2c): a command whose stderr the caller left unset inherits the host's
        // stderr (tokio's default), where Go's os/exec with a nil `cmd.Stderr` discards it;
        // `tokio::process::Command` has no getter to tell an unset stderr from a set one.
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("stdin was already taken"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("stdout was already taken"))?;
        Ok(Self {
            child,
            io: AsyncRwTransport::new_client(stdout, stdin),
        })
    }
}

impl Transport<RoleClient> for ChildTransport {
    type Error = std::io::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleClient>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        self.io.send(item)
    }

    fn receive(&mut self) -> impl Future<Output = Option<RxJsonRpcMessage<RoleClient>>> + Send {
        self.io.receive()
    }

    /// go-sdk `pipeRWC.Close`: closes the child's stdin, waits up to
    /// [`CHILD_TERMINATE_DURATION`] for it to exit, then kills it.
    // Divergence (ledger 2c): go-sdk sends SIGTERM and waits once more before killing; without
    // a signal API (no `unsafe`, no libc dependency) the child is killed at once.
    async fn close(&mut self) -> Result<(), Self::Error> {
        self.io.close().await?;
        match tokio::time::timeout(CHILD_TERMINATE_DURATION, self.child.wait()).await {
            Ok(status) => status.map(|_| ()),
            Err(_) => self.child.kill().await,
        }
    }
}

#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::config::default_client_info;

    /// A child that exits at once: the `initialize` handshake fails because stdout closes.
    #[tokio::test]
    async fn command_factory_spawns_a_fresh_child_per_connect() {
        let built = Arc::new(AtomicUsize::new(0));
        let counter = built.clone();
        let cfg = Config::new(McpTransport::Command(Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            let mut cmd = tokio::process::Command::new("sh");
            cmd.args(["-c", "exit 0"]);
            cmd
        })));
        let factory = build_transport(&cfg).unwrap();
        assert_eq!(
            built.load(Ordering::SeqCst),
            0,
            "the builder ran before connect"
        );
        for n in 1..=2 {
            let err = factory.connect(default_client_info()).await.err().unwrap();
            assert!(err.to_string().contains("connection closed"), "{err}");
            assert_eq!(built.load(Ordering::SeqCst), n);
        }
    }

    #[tokio::test]
    async fn command_factory_reports_a_spawn_error() {
        let cfg = Config::new(McpTransport::Command(Arc::new(|| {
            tokio::process::Command::new("/nonexistent/gadk-mcp-test-server")
        })));
        let err = build_transport(&cfg)
            .unwrap()
            .connect(default_client_info())
            .await
            .err()
            .unwrap();
        let io = err.downcast_ref::<std::io::Error>().unwrap();
        assert_eq!(io.kind(), std::io::ErrorKind::NotFound);
    }
}
