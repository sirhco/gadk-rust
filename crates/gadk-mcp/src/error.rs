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

//! The crate's error type. Every `Display` equals the Go `Error()` text a Go test asserts
//! (`tool/mcptoolset/{set,client,tool}.go` and go-sdk's `mcp.ErrConnectionClosed`).

use std::error::Error as StdError;

/// A boxed error from outside this crate (a transport factory, a metadata provider, rmcp's
/// `ClientInitializeError`).
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// An MCP toolset error.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// set.go `New`/`buildTransport`: no transport configured (an empty `Endpoint("")` counts as
    /// none).
    #[error("mcptoolset: set Config.Transport or Config.Endpoint")]
    NoTransport,
    /// set.go `buildTransport`: `Config::auth` with a `Command` or `Custom` transport. The
    /// payload is [`McpTransport::variant_name`](crate::McpTransport::variant_name).
    #[error(
        "mcptoolset: Config.Auth requires a streamable HTTP transport; set Config.Endpoint or use McpTransport::StreamableHttp (got {0})"
    )]
    AuthRequiresHttp(&'static str),
    /// go-sdk `mcp.ErrConnectionClosed`: the toolset was closed.
    #[error("connection closed")]
    ConnectionClosed,
    /// The caller's `CancellationToken` fired (Go `ctx.Err()` = `context.Canceled`).
    #[error("context canceled")]
    Cancelled,
    /// client.go `getSession`: the first connect failed.
    #[error("failed to init MCP session: {0}")]
    InitSession(#[source] BoxError),
    /// client.go `refreshConnection`: a reconnect failed.
    #[error("failed to refresh MCP session: {0}")]
    RefreshSession(#[source] BoxError),
    /// client.go `withRetry`: `"%w (reconnection also failed: %v)"`. Only `err` is wrapped.
    #[error("{err} (reconnection also failed: {refresh})")]
    ReconnectFailed {
        /// The operation's error.
        #[source]
        err: Box<Error>,
        /// The reconnect's error (Go `%v`: text only, not in the chain).
        refresh: Box<Error>,
    },
    /// client.go `ListTools`: any listing error.
    #[error("failed to list MCP tools: {0}")]
    ListTools(#[source] Box<Error>),
    /// client.go `ListTools`: the connection was lost again after the one reconnect.
    #[error("failed to list MCP tools: connection lost again after reconnection")]
    ListToolsReconnectedTwice,
    /// An rmcp request error, as is.
    #[error(transparent)]
    Service(#[from] rmcp::ServiceError),
    /// client.go `sessionKey`: the provider failed.
    #[error("auth: resolve credential: {0}")]
    ResolveCredential(#[source] gadk_auth::Error),
    /// client.go `sessionKey`: the credential could not be applied.
    #[error("auth: apply credential: {0}")]
    ApplyCredential(#[source] gadk_auth::Error),
    /// set.go `Tools`: a server tool could not be converted.
    #[error(
        "failed to convert MCP tool {} to adk tool: {source}",
        gadk_agent::go_quote(name)
    )]
    ConvertTool {
        /// The MCP tool name.
        name: String,
        /// Why the conversion failed.
        #[source]
        source: BoxError,
    },
    /// tool.go `Run`: the metadata provider failed.
    #[error(
        "metadata provider for MCP tool {} failed: {source}",
        gadk_agent::go_quote(name)
    )]
    MetadataProvider {
        /// The MCP tool name.
        name: String,
        /// The provider's error.
        #[source]
        source: BoxError,
    },
    /// tool.go `Run`: the metadata provider returned a reserved `_meta` key.
    #[error(
        "metadata provider for MCP tool {} returned reserved _meta key {}",
        gadk_agent::go_quote(name),
        gadk_agent::go_quote(key)
    )]
    ReservedMetaKey {
        /// The MCP tool name.
        name: String,
        /// The first reserved key, in sorted key order.
        key: String,
    },
    /// tool.go `Run`: the `tools/call` request failed.
    #[error(
        "failed to call MCP tool {} with err: {source}",
        gadk_agent::go_quote(name)
    )]
    CallTool {
        /// The MCP tool name.
        name: String,
        /// The call's error.
        #[source]
        source: Box<Error>,
    },
    /// tool.go `Run`: the server answered `isError` (`Tool execution failed.` plus
    /// ` Details: <formatted content>` when that is non-empty).
    #[error("{0}")]
    ToolExecution(String),
    /// A [`TransportFactory`](crate::TransportFactory)'s own failure (wrapped by `InitSession`
    /// or `RefreshSession`).
    #[error(transparent)]
    Transport(BoxError),
    /// Close errors joined as Go `errors.Join` does (newline-separated).
    #[error("{}", .0.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("\n"))]
    Close(Vec<Error>),
}

impl Error {
    /// Go `errors.Is(err, mcp.ErrConnectionClosed)`: whether [`Error::ConnectionClosed`] is this
    /// error or anywhere in its chain (a joined [`Error::Close`] matches when any part does).
    pub fn is_connection_closed(&self) -> bool {
        self.any_in_chain(&|e| matches!(e, Error::ConnectionClosed))
    }

    /// Go `errors.Is(err, context.Canceled)`: whether [`Error::Cancelled`] is this error or
    /// anywhere in its chain.
    pub fn is_cancelled(&self) -> bool {
        self.any_in_chain(&|e| matches!(e, Error::Cancelled))
    }

    /// Walks the wrapped errors as Go's `errors.Is` does. Variants holding a `Box<Error>` are
    /// followed directly; a [`BoxError`] is searched for an `Error` along its `source()` chain
    /// (a `transparent` variant's own `source()` would skip the boxed error itself).
    fn any_in_chain(&self, pred: &dyn Fn(&Error) -> bool) -> bool {
        if pred(self) {
            return true;
        }
        match self {
            Error::ReconnectFailed { err, .. } => err.any_in_chain(pred),
            Error::ListTools(e) | Error::CallTool { source: e, .. } => e.any_in_chain(pred),
            Error::InitSession(e)
            | Error::RefreshSession(e)
            | Error::Transport(e)
            | Error::ConvertTool { source: e, .. }
            | Error::MetadataProvider { source: e, .. } => boxed_any_in_chain(e.as_ref(), pred),
            Error::Close(errs) => errs.iter().any(|e| e.any_in_chain(pred)),
            _ => false,
        }
    }
}

/// The first `Error` along `e`'s `source()` chain (starting at `e`), searched with `pred`.
fn boxed_any_in_chain(e: &(dyn StdError + 'static), pred: &dyn Fn(&Error) -> bool) -> bool {
    let mut cur = Some(e);
    while let Some(c) = cur {
        if let Some(found) = c.downcast_ref::<Error>() {
            return found.any_in_chain(pred);
        }
        cur = c.source();
    }
    false
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// A foreign error wrapping a crate error, as a transport factory might return.
    #[derive(Debug, thiserror::Error)]
    #[error("wrapped: {0}")]
    struct Wrapped(#[source] Error);

    #[test]
    fn display_matches_go_text() {
        let cases: Vec<(Error, &str)> = vec![
            (
                Error::NoTransport,
                "mcptoolset: set Config.Transport or Config.Endpoint",
            ),
            (
                Error::AuthRequiresHttp("Command"),
                "mcptoolset: Config.Auth requires a streamable HTTP transport; set Config.Endpoint or use McpTransport::StreamableHttp (got Command)",
            ),
            (Error::ConnectionClosed, "connection closed"),
            (Error::Cancelled, "context canceled"),
            (
                Error::InitSession("boom".into()),
                "failed to init MCP session: boom",
            ),
            (
                Error::RefreshSession("boom".into()),
                "failed to refresh MCP session: boom",
            ),
            (
                Error::ReconnectFailed {
                    err: Box::new(Error::ConnectionClosed),
                    refresh: Box::new(Error::RefreshSession("dial".into())),
                },
                "connection closed (reconnection also failed: failed to refresh MCP session: dial)",
            ),
            (
                Error::ListTools(Box::new(Error::ConnectionClosed)),
                "failed to list MCP tools: connection closed",
            ),
            (
                Error::ListToolsReconnectedTwice,
                "failed to list MCP tools: connection lost again after reconnection",
            ),
            (
                Error::Service(rmcp::ServiceError::TransportClosed),
                "Transport closed",
            ),
            (
                Error::ResolveCredential(gadk_auth::Error::BearerMissingToken),
                "auth: resolve credential: auth: bearer credential missing token",
            ),
            (
                Error::ApplyCredential(gadk_auth::Error::BearerMissingToken),
                "auth: apply credential: auth: bearer credential missing token",
            ),
            (
                Error::ConvertTool {
                    name: "get_weather".into(),
                    source: "bad".into(),
                },
                "failed to convert MCP tool \"get_weather\" to adk tool: bad",
            ),
            (
                Error::MetadataProvider {
                    name: "get_weather".into(),
                    source: "provider failed".into(),
                },
                "metadata provider for MCP tool \"get_weather\" failed: provider failed",
            ),
            (
                Error::ReservedMetaKey {
                    name: "get_weather".into(),
                    key: "progressToken".into(),
                },
                "metadata provider for MCP tool \"get_weather\" returned reserved _meta key \"progressToken\"",
            ),
            (
                Error::CallTool {
                    name: "get_weather".into(),
                    source: Box::new(Error::ConnectionClosed),
                },
                "failed to call MCP tool \"get_weather\" with err: connection closed",
            ),
            (
                Error::ToolExecution("Tool execution failed.".into()),
                "Tool execution failed.",
            ),
            (Error::Transport("dial tcp".into()), "dial tcp"),
            (
                Error::Close(vec![Error::ConnectionClosed, Error::Cancelled]),
                "connection closed\ncontext canceled",
            ),
        ];
        for (err, want) in cases {
            assert_eq!(err.to_string(), want, "{err:?}");
        }
    }

    #[test]
    fn is_connection_closed_walks_the_chain() {
        assert!(Error::ConnectionClosed.is_connection_closed());
        assert!(
            Error::CallTool {
                name: "t".into(),
                source: Box::new(Error::ListTools(Box::new(Error::ConnectionClosed))),
            }
            .is_connection_closed()
        );
        assert!(
            Error::ReconnectFailed {
                err: Box::new(Error::ConnectionClosed),
                refresh: Box::new(Error::Cancelled),
            }
            .is_connection_closed()
        );
        // Go `%v`: the refresh error is text only.
        let refresh_only = Error::ReconnectFailed {
            err: Box::new(Error::Cancelled),
            refresh: Box::new(Error::ConnectionClosed),
        };
        assert!(!refresh_only.is_connection_closed());
        assert!(refresh_only.is_cancelled());
        // Through a foreign error in a BoxError, and through a joined close error.
        assert!(
            Error::InitSession(Box::new(Wrapped(Error::ConnectionClosed))).is_connection_closed()
        );
        assert!(
            Error::Close(vec![Error::Cancelled, Error::ConnectionClosed]).is_connection_closed()
        );
        assert!(!Error::Service(rmcp::ServiceError::TransportClosed).is_connection_closed());
        assert!(!Error::NoTransport.is_cancelled());
    }
}
