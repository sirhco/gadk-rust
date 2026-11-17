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

//! The toolset configuration and constructor: [`Config`], [`MetadataProvider`] and [`new`].

use std::sync::Arc;

use rmcp::RoleClient;
use rmcp::ServiceExt as _;
use rmcp::model::{ClientCapabilities, ClientConfig, Implementation};
use rmcp::service::RunningService;
use rmcp::transport::IntoTransport;

use crate::client::{ConnectionRefresher, SessionClient};
use crate::error::{BoxError, Error};
use crate::sessions::CredentialSessions;
use crate::set::McpToolset;
use crate::transport::{HttpTarget, build_transport};

/// Request-scoped metadata for MCP tool calls, sent in the `_meta` field of `tools/call`.
///
/// It runs once per call that proceeds to the server: when a tool requires confirmation, only for
/// the confirmed call, with the context carrying the confirmation. It may be called concurrently.
/// `Ok(None)` contributes no metadata; an error, or a reserved MCP key (`progressToken` or any
/// `io.modelcontextprotocol/` key, checked in sorted key order), fails the tool call.
pub type MetadataProvider = Arc<
    dyn Fn(
            &gadk_agent::Context,
        ) -> Result<Option<serde_json::Map<String, serde_json::Value>>, BoxError>
        + Send
        + Sync,
>;

/// Builds the child-process command of [`McpTransport::Command`]; it runs again on every
/// (re)connect, because a spawned `Command` cannot be reused.
pub type CommandBuilder = Arc<dyn Fn() -> tokio::process::Command + Send + Sync>;

/// Opens a fresh, initialized MCP client session. The toolset calls it on every (re)connect, so
/// each call must build a new transport. Implement it for transports [`McpTransport`] does not
/// cover, usually with [`serve_client`].
#[async_trait::async_trait]
pub trait TransportFactory: Send + Sync {
    /// Connects and runs the MCP `initialize` handshake with `client_info`.
    async fn connect(
        &self,
        client_info: ClientConfig,
    ) -> Result<RunningService<RoleClient, ClientConfig>, BoxError>;
}

/// `client_info.serve(transport)` for any rmcp client transport, with rmcp's
/// `ClientInitializeError` boxed: the usual body of a [`TransportFactory::connect`].
pub async fn serve_client<T, E, A>(
    client_info: ClientConfig,
    transport: T,
) -> Result<RunningService<RoleClient, ClientConfig>, BoxError>
where
    T: IntoTransport<RoleClient, E, A> + Send + 'static,
    E: std::error::Error + Send + Sync + 'static,
{
    client_info
        .serve(transport)
        .await
        .map_err(|e| Box::new(e) as BoxError)
}

/// How the toolset reaches the MCP server.
#[derive(Clone)]
#[non_exhaustive]
pub enum McpTransport {
    /// Streamable HTTP to this URL with a default `reqwest::Client`. An empty URL counts as no
    /// transport.
    Endpoint(String),
    /// Streamable HTTP with the caller's client and extra headers. `user-agent` and
    /// `x-goog-api-client` in `headers` are sent after the ADK tracking tokens; a `user-agent`
    /// among the client's own default headers is not merged with them.
    StreamableHttp {
        /// The MCP endpoint URL.
        url: String,
        /// The HTTP client every request goes through.
        client: reqwest::Client,
        /// Headers added to every request.
        headers: http::HeaderMap,
    },
    /// A child process speaking MCP over stdio.
    Command(CommandBuilder),
    /// Any other transport.
    Custom(Arc<dyn TransportFactory>),
}

impl McpTransport {
    /// The variant's name (`Endpoint`, `StreamableHttp`, `Command` or `Custom`), as
    /// [`Error::AuthRequiresHttp`] reports it.
    pub fn variant_name(&self) -> &'static str {
        match self {
            McpTransport::Endpoint(_) => "Endpoint",
            McpTransport::StreamableHttp { .. } => "StreamableHttp",
            McpTransport::Command(_) => "Command",
            McpTransport::Custom(_) => "Custom",
        }
    }
}

/// The MCP toolset configuration.
#[derive(Clone)]
#[non_exhaustive]
pub struct Config {
    /// How to reach the server.
    pub transport: McpTransport,
    /// The client's `initialize` parameters; `None` means client name `adk-mcp-client` with this
    /// crate's version and default capabilities.
    pub client_info: Option<ClientConfig>,
    /// Resolves and applies a credential to every MCP HTTP request; requires a streamable HTTP
    /// transport. Each distinct credential gets its own MCP session; a session idle for 10
    /// minutes is closed, and at most 64 are kept open unless more are in use at once.
    pub auth: Option<Arc<dyn gadk_auth::CredentialProvider>>,
    /// When non-empty, each tool is exposed as `<prefix>_<name>`: `"docs"` turns `search` into
    /// `docs_search`. The `tools/call` request still uses the
    /// server's name. Tool callbacks, confirmation providers and the predicates of
    /// [`filter_toolset`](gadk_agent::filter_toolset) see the exposed name; `tool_filter` runs
    /// before the prefix is applied, so it sees the server's name. Empty leaves names unchanged.
    pub prefix: String,
    /// Which tools to expose. It sees the server's tool names, before [`prefix`](Config::prefix) is
    /// applied; [`filter_toolset`](gadk_agent::filter_toolset) filters by the exposed name instead.
    pub tool_filter: Option<gadk_agent::tool::ToolPredicate>,
    /// Whether every tool call needs user confirmation.
    pub require_confirmation: bool,
    /// Decides per call whether confirmation is needed; overrides `require_confirmation` when set.
    pub require_confirmation_provider: Option<gadk_agent::tool::ConfirmationProvider>,
    /// Supplies `_meta` for each tool call.
    pub metadata_provider: Option<MetadataProvider>,
}

impl Config {
    /// A config for `transport` with every other field at its default (`None`/`false`).
    pub fn new(transport: McpTransport) -> Self {
        Self {
            transport,
            client_info: None,
            auth: None,
            prefix: String::new(),
            tool_filter: None,
            require_confirmation: false,
            require_confirmation_provider: None,
            metadata_provider: None,
        }
    }
}

/// Validates `cfg` and returns the toolset. The MCP session connects lazily, on the first `tools`
/// or tool call.
///
/// Errors: [`Error::NoTransport`] without a transport (or with `Endpoint("")`), and
/// [`Error::AuthRequiresHttp`] when `auth` is set with a `Command` or `Custom` transport.
///
/// Close the toolset with [`McpToolset::close`] when it is no longer used.
pub fn new(cfg: Config) -> Result<McpToolset, Error> {
    let factory = build_transport(&cfg)?;
    let client_info = cfg.client_info.clone().unwrap_or_else(default_client_info);
    let client: Arc<dyn SessionClient> = match &cfg.auth {
        Some(provider) => {
            let target = HttpTarget::from_transport(&cfg.transport)
                .ok_or(Error::AuthRequiresHttp(cfg.transport.variant_name()))?;
            Arc::new(CredentialSessions::new(
                client_info,
                target,
                provider.clone(),
            ))
        }
        None => Arc::new(ConnectionRefresher::new(client_info, factory)),
    };
    Ok(McpToolset::from_parts(client, &cfg))
}

/// The default client identity: name `adk-mcp-client`, this crate's version, default capabilities.
pub(crate) fn default_client_info() -> ClientConfig {
    ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("adk-mcp-client", env!("CARGO_PKG_VERSION")),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn default_client_info_names_the_adk_client() {
        let info = default_client_info();
        assert_eq!(info.client_info.name, "adk-mcp-client");
        assert_eq!(info.client_info.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(info.capabilities, ClientCapabilities::default());
    }

    #[test]
    fn config_new_defaults_everything_but_the_transport() {
        let cfg = Config::new(McpTransport::Endpoint("http://127.0.0.1:1/mcp".into()));
        assert!(
            matches!(&cfg.transport, McpTransport::Endpoint(u) if u == "http://127.0.0.1:1/mcp")
        );
        assert!(cfg.client_info.is_none());
        assert!(cfg.auth.is_none());
        assert!(cfg.prefix.is_empty());
        assert!(cfg.tool_filter.is_none());
        assert!(!cfg.require_confirmation);
        assert!(cfg.require_confirmation_provider.is_none());
        assert!(cfg.metadata_provider.is_none());
    }

    /// A factory that never connects.
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

    #[test]
    fn variant_names() {
        let custom: Arc<dyn TransportFactory> = Arc::new(Unused);
        let cases = [
            (McpTransport::Endpoint(String::new()), "Endpoint"),
            (
                McpTransport::StreamableHttp {
                    url: String::new(),
                    client: reqwest::Client::new(),
                    headers: http::HeaderMap::new(),
                },
                "StreamableHttp",
            ),
            (
                McpTransport::Command(Arc::new(|| tokio::process::Command::new("true"))),
                "Command",
            ),
            (McpTransport::Custom(custom), "Custom"),
        ];
        for (t, want) in cases {
            assert_eq!(t.variant_name(), want);
        }
    }
}
