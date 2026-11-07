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

//! The MCP client seam and the connection refresher (port of ADK-Go
//! `tool/mcptoolset/client.go`: `MCPClient`, `connectionRefresher`, `withRetry`).

use std::future::Future;
use std::sync::Arc;

use gadk_auth::AuthContext;
use rmcp::RoleClient;
use rmcp::model::{CallToolRequestParams, CallToolResult, ClientConfig, Tool};
use rmcp::service::Peer;
use tokio_util::sync::CancellationToken;

use crate::config::TransportFactory;
use crate::error::Error;

/// What one MCP operation runs with: the identity credentials are resolved for (`auth`) and the
/// caller's cancellation (`cancel`, Go's `ctx.Done()`).
#[derive(Clone, Debug)]
pub struct CallContext {
    /// The identity a credential provider resolves for.
    pub auth: AuthContext,
    /// Cancels waiting for the session lock or a connect in progress.
    pub cancel: CancellationToken,
}

impl CallContext {
    /// A context for `auth` cancelled by `cancel`.
    pub fn new(auth: AuthContext, cancel: CancellationToken) -> Self {
        Self { auth, cancel }
    }

    /// No identity and a token nothing cancels (Go `context.Background()`).
    #[allow(clippy::todo)]
    pub fn background() -> Self {
        todo!("Task 6")
    }
}

impl From<&gadk_agent::Context> for CallContext {
    /// The context's identity and its invocation's cancellation token.
    #[allow(unused_variables, clippy::todo)]
    fn from(ctx: &gadk_agent::Context) -> Self {
        todo!("Task 6")
    }
}

impl From<&gadk_agent::ReadonlyContext> for CallContext {
    /// The context's identity and its invocation's cancellation token
    /// ([`ReadonlyContext::cancellation_token`](gadk_agent::ReadonlyContext::cancellation_token)).
    #[allow(unused_variables, clippy::todo)]
    fn from(ctx: &gadk_agent::ReadonlyContext) -> Self {
        todo!("Task 6")
    }
}

/// Go `MCPClient`: the two MCP operations a toolset and its tools need.
#[async_trait::async_trait]
pub trait McpClient: Send + Sync {
    /// `tools/call`.
    async fn call_tool(
        &self,
        cx: &CallContext,
        params: CallToolRequestParams,
    ) -> Result<CallToolResult, Error>;
    /// `tools/list`, every page, in server order.
    async fn list_tools(&self, cx: &CallContext) -> Result<Vec<Tool>, Error>;
}

/// Go `sessionClient`: an [`McpClient`] the toolset can close.
#[async_trait::async_trait]
#[allow(dead_code)]
pub(crate) trait SessionClient: McpClient {
    /// Closes every session. Idempotent and concurrency-safe: every call returns the first
    /// call's result. Later operations fail with [`Error::ConnectionClosed`].
    async fn close(&self) -> Result<(), Arc<Error>>;

    /// The credential session pool behind a toolset built with `auth` (for the D4 test hooks);
    /// `None` for a [`ConnectionRefresher`].
    fn as_credential_sessions(&self) -> Option<&crate::sessions::CredentialSessions> {
        None
    }
}

/// Go `connectionRefresher`: one lazily connected session, reconnected once when an operation
/// fails because the connection was lost.
#[allow(dead_code)]
pub(crate) struct ConnectionRefresher {}

#[allow(unused_variables, dead_code, clippy::todo)]
impl ConnectionRefresher {
    /// A refresher that connects through `factory` on first use.
    pub(crate) fn new(client_info: ClientConfig, factory: Arc<dyn TransportFactory>) -> Self {
        todo!("Task 6")
    }

    /// No session and no connect in progress (a `try_lock` that finds the lock busy answers
    /// `false`).
    pub(crate) fn disconnected(&self) -> bool {
        todo!("Task 6")
    }

    /// Go `withRetry`: runs `op` on the current session's peer; on a refreshable error, refreshes
    /// the connection and runs `op` once more. Returns `op`'s value and whether a reconnect
    /// happened. A failed refresh returns [`Error::ReconnectFailed`].
    pub(crate) async fn with_retry<T, F, Fut>(
        &self,
        cx: &CallContext,
        op: F,
    ) -> Result<(T, bool), Error>
    where
        F: FnMut(Peer<RoleClient>) -> Fut + Send,
        Fut: Future<Output = Result<T, Error>> + Send,
        T: Send,
    {
        todo!("Task 6")
    }
}

#[async_trait::async_trait]
#[allow(unused_variables, clippy::todo)]
impl McpClient for ConnectionRefresher {
    async fn call_tool(
        &self,
        cx: &CallContext,
        params: CallToolRequestParams,
    ) -> Result<CallToolResult, Error> {
        todo!("Task 6")
    }

    async fn list_tools(&self, cx: &CallContext) -> Result<Vec<Tool>, Error> {
        todo!("Task 6")
    }
}

#[async_trait::async_trait]
#[allow(clippy::todo)]
impl SessionClient for ConnectionRefresher {
    async fn close(&self) -> Result<(), Arc<Error>> {
        todo!("Task 6")
    }
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
