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

use std::error::Error as StdError;
use std::future::Future;
use std::io;
use std::sync::Arc;

use gadk_auth::AuthContext;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ClientConfig, ClientRequest, PaginatedRequestParams,
    Tool,
};
use rmcp::service::{Peer, RunningService};
use rmcp::transport::streamable_http_client::StreamableHttpError;
use rmcp::{ClientCacheConfig, RoleClient, ServiceError};
use tokio::sync::{Mutex, MutexGuard, OnceCell};
use tokio_util::sync::CancellationToken;

use crate::config::TransportFactory;
use crate::error::{BoxError, Error};

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
    pub fn background() -> Self {
        Self::new(AuthContext::anonymous(), CancellationToken::new())
    }
}

impl From<&gadk_agent::Context> for CallContext {
    /// The context's identity and its invocation's cancellation token.
    fn from(ctx: &gadk_agent::Context) -> Self {
        Self::new(AuthContext::from(ctx), ctx.cancellation_token().clone())
    }
}

impl From<&gadk_agent::ReadonlyContext> for CallContext {
    /// The context's identity and its invocation's cancellation token
    /// ([`ReadonlyContext::cancellation_token`](gadk_agent::ReadonlyContext::cancellation_token)).
    fn from(ctx: &gadk_agent::ReadonlyContext) -> Self {
        Self::new(AuthContext::from(ctx), ctx.cancellation_token().clone())
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
pub(crate) trait SessionClient: McpClient {
    /// Closes every session. Idempotent and concurrency-safe: every call returns the first
    /// call's result. Later operations fail with [`Error::ConnectionClosed`].
    async fn close(&self) -> Result<(), Arc<Error>>;

    /// The credential session pool behind a toolset built with `auth` (for the D4 test hooks);
    /// `None` for a [`ConnectionRefresher`].
    // Only the `#[cfg(test)]` hooks on `McpToolset` call it.
    #[cfg_attr(not(test), allow(dead_code))]
    fn as_credential_sessions(&self) -> Option<&crate::sessions::CredentialSessions> {
        None
    }
}

/// A connected client session.
type Session = RunningService<RoleClient, ClientConfig>;

/// What the session lock guards (Go `connectionRefresher.session` and `.closed`).
#[derive(Default)]
pub(crate) struct State {
    session: Option<Session>,
    /// Set by `close`: closing is final, so failed in-flight calls cannot reconnect.
    closed: bool,
}

/// Go `connectionRefresher`: one lazily connected session, reconnected once when an operation
/// fails because the connection was lost.
///
/// The session lock is held only to connect or refresh. Operations run on a cloned [`Peer`]
/// outside it, so calls on the session run concurrently (R11).
pub(crate) struct ConnectionRefresher {
    client_info: ClientConfig,
    factory: Arc<dyn TransportFactory>,
    /// Go's one-slot `mu` channel plus the state it guards. Waiters leave when their own token
    /// fires without disturbing the owner (see [`lock`](ConnectionRefresher::lock)).
    state: Mutex<State>,
    /// Go `closeOnce` + `closeErr`: the first `close`'s result, shared by every call.
    close_result: OnceCell<Result<(), Arc<Error>>>,
}

impl ConnectionRefresher {
    /// A refresher that connects through `factory` on first use.
    pub(crate) fn new(client_info: ClientConfig, factory: Arc<dyn TransportFactory>) -> Self {
        Self {
            client_info,
            factory,
            state: Mutex::new(State::default()),
            close_result: OnceCell::new(),
        }
    }

    /// No session and no connect in progress (a `try_lock` that finds the lock busy answers
    /// `false`).
    pub(crate) fn disconnected(&self) -> bool {
        match self.state.try_lock() {
            Ok(state) => state.session.is_none(),
            Err(_) => false,
        }
    }

    /// Go `lock`: takes the session lock, or fails with [`Error::Cancelled`] once `cx.cancel`
    /// fires. A token that is cancelled by the time the lock is taken fails too, even when the
    /// lock was free (Go's post-acquire `ctx.Err()` check). Leaving the wait does not disturb
    /// the lock's owner.
    pub(crate) async fn lock(&self, cx: &CallContext) -> Result<MutexGuard<'_, State>, Error> {
        let state = unless_cancelled(cx, self.state.lock()).await?;
        // The token may have fired while this call waited for the lock to be handed over.
        if cx.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        Ok(state)
    }

    /// Go `connect`: opens a fresh session with the response cache off (R8), giving up with
    /// [`Error::Cancelled`] when `cx.cancel` fires. The caller holds the lock.
    async fn connect(&self, cx: &CallContext) -> Result<Session, BoxError> {
        let connected = unless_cancelled(cx, self.factory.connect(self.client_info.clone()))
            .await
            .map_err(|cancelled| -> BoxError { Box::new(cancelled) })??;
        // A listing must never be served from a cache: tools change between sessions.
        connected
            .peer()
            .set_response_cache_config(ClientCacheConfig::disabled())
            .await;
        Ok(connected)
    }

    /// Go `getSession`: the current session's peer, connecting first when there is none.
    pub(crate) async fn get_session(&self, cx: &CallContext) -> Result<Peer<RoleClient>, Error> {
        let mut state = self.lock(cx).await?;
        if state.closed {
            return Err(Error::ConnectionClosed);
        }
        if let Some(session) = &state.session {
            return Ok(session.peer().clone());
        }
        let session = self.connect(cx).await.map_err(Error::InitSession)?;
        let peer = session.peer().clone();
        state.session = Some(session);
        Ok(peer)
    }

    /// Go `refreshConnection`: pings the current session and keeps it if it answers (another
    /// caller already reconnected); otherwise closes it and connects a new one.
    pub(crate) async fn refresh_connection(
        &self,
        cx: &CallContext,
    ) -> Result<Peer<RoleClient>, Error> {
        let mut state = self.lock(cx).await?;
        if state.closed {
            return Err(Error::ConnectionClosed);
        }
        if let Some(session) = &state.session {
            let peer = session.peer().clone();
            // Go pings on the caller's ctx: a cancelled ping counts as failed, so the session is
            // closed and the reconnect below fails with `context canceled`.
            let ping = peer.send_request(ClientRequest::PingRequest(Default::default()));
            let answered = matches!(unless_cancelled(cx, ping).await, Ok(Ok(_)));
            if answered {
                return Ok(peer);
            }
            if let Some(stale) = state.session.take()
                && let Err(err) = stale.cancel().await
            {
                tracing::warn!("failed to close MCP session: {err}");
            }
        }
        let session = self.connect(cx).await.map_err(Error::RefreshSession)?;
        let peer = session.peer().clone();
        state.session = Some(session);
        Ok(peer)
    }

    /// Go `withRetry`: runs `op` on the current session's peer; on a refreshable error, refreshes
    /// the connection and runs `op` once more. Returns `op`'s value and whether a reconnect
    /// happened. A failed refresh returns [`Error::ReconnectFailed`].
    ///
    /// Each `op` run gives up with [`Error::Cancelled`] once `cx.cancel` fires (Go passes ctx into
    /// `session.CallTool` / `ListTools`); dropping rmcp's request sends `notifications/cancelled`
    /// as go-sdk does. Cancellation is not refreshable, so the session stays.
    pub(crate) async fn with_retry<T, F, Fut>(
        &self,
        cx: &CallContext,
        mut op: F,
    ) -> Result<(T, bool), Error>
    where
        F: FnMut(Peer<RoleClient>) -> Fut + Send,
        Fut: Future<Output = Result<T, Error>> + Send,
        T: Send,
    {
        let peer = self.get_session(cx).await?;
        let err = match unless_cancelled(cx, op(peer)).await.and_then(|r| r) {
            Ok(value) => return Ok((value, false)),
            Err(err) => err,
        };
        if !is_refreshable(&err) {
            return Err(err);
        }
        let peer = match self.refresh_connection(cx).await {
            Ok(peer) => peer,
            Err(refresh) => {
                return Err(Error::ReconnectFailed {
                    err: Box::new(err),
                    refresh: Box::new(refresh),
                });
            }
        };
        let value = unless_cancelled(cx, op(peer)).await??;
        Ok((value, true))
    }
}

/// Runs `fut` unless `cx.cancel` fires first, which fails with [`Error::Cancelled`] (Go's
/// ctx-aware blocking calls). A token already cancelled wins over a ready `fut`.
async fn unless_cancelled<T>(cx: &CallContext, fut: impl Future<Output = T>) -> Result<T, Error> {
    tokio::select! {
        biased;
        () = cx.cancel.cancelled() => Err(Error::Cancelled),
        value = fut => Ok(value),
    }
}

/// Go `shouldRefreshConnection`: whether `err` means the connection was lost (go-sdk
/// `ErrConnectionClosed` / `ErrSessionMissing`, `io.ErrClosedPipe`, `io.EOF`). rmcp spells these
/// `ServiceError::TransportClosed`, a send failure wrapping streamable HTTP's
/// `SessionExpired` / `TransportChannelClosed` / `UnexpectedEndOfStream`, and an `io::Error` of
/// kind `BrokenPipe` or `UnexpectedEof`; the crate's own [`Error::ConnectionClosed`] counts too.
fn is_refreshable(err: &Error) -> bool {
    match err {
        Error::ConnectionClosed => true,
        Error::Service(service) => service_is_refreshable(service),
        Error::ReconnectFailed { err, .. } => is_refreshable(err),
        Error::ListTools(err) | Error::CallTool { source: err, .. } => is_refreshable(err),
        Error::InitSession(err)
        | Error::RefreshSession(err)
        | Error::Transport(err)
        | Error::ConvertTool { source: err, .. }
        | Error::MetadataProvider { source: err, .. } => chain_is_refreshable(err.as_ref()),
        Error::Close(errs) => errs.iter().any(is_refreshable),
        _ => false,
    }
}

fn service_is_refreshable(err: &ServiceError) -> bool {
    match err {
        ServiceError::TransportClosed => true,
        ServiceError::TransportSend(send) => chain_is_refreshable(send.error.as_ref()),
        _ => false,
    }
}

/// Walks `err`'s `source()` chain (starting at `err`) for a refreshable error.
fn chain_is_refreshable(err: &(dyn StdError + 'static)) -> bool {
    let mut cur = Some(err);
    while let Some(e) = cur {
        if let Some(own) = e.downcast_ref::<Error>() {
            return is_refreshable(own);
        }
        if let Some(service) = e.downcast_ref::<ServiceError>()
            && service_is_refreshable(service)
        {
            return true;
        }
        if let Some(http) = e.downcast_ref::<StreamableHttpError<reqwest::Error>>()
            && matches!(
                http,
                StreamableHttpError::SessionExpired
                    | StreamableHttpError::TransportChannelClosed
                    | StreamableHttpError::UnexpectedEndOfStream
            )
        {
            return true;
        }
        if let Some(io) = e.downcast_ref::<io::Error>()
            && matches!(
                io.kind(),
                io::ErrorKind::BrokenPipe | io::ErrorKind::UnexpectedEof
            )
        {
            return true;
        }
        cur = e.source();
    }
    false
}

#[async_trait::async_trait]
impl McpClient for ConnectionRefresher {
    /// Go `CallTool`: `tools/call` on the session, reconnecting once if needed.
    async fn call_tool(
        &self,
        cx: &CallContext,
        params: CallToolRequestParams,
    ) -> Result<CallToolResult, Error> {
        let (result, _) = self
            .with_retry(cx, |peer| {
                let params = params.clone();
                async move { peer.call_tool(params).await.map_err(Error::from) }
            })
            .await?;
        Ok(result)
    }

    /// Go `ListTools`: every page, following `nextCursor`. Cursors do not survive a session
    /// (MCP spec), so after a reconnect the listing restarts from the first page; a second
    /// reconnect in the same listing fails.
    async fn list_tools(&self, cx: &CallContext) -> Result<Vec<Tool>, Error> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        let mut has_reconnected = false;
        loop {
            let page_cursor = cursor.clone();
            let (page, reconnected) = self
                .with_retry(cx, |peer| {
                    let params = PaginatedRequestParams::default().with_cursor(page_cursor.clone());
                    async move { peer.list_tools(Some(params)).await.map_err(Error::from) }
                })
                .await
                .map_err(|err| Error::ListTools(Box::new(err)))?;
            if reconnected {
                if has_reconnected {
                    return Err(Error::ListToolsReconnectedTwice);
                }
                // On reconnection, restart pagination from scratch per MCP spec.
                has_reconnected = true;
                cursor = None;
                tools.clear();
                continue;
            }
            tools.extend(page.tools);
            match page.next_cursor {
                Some(next) if !next.is_empty() => cursor = Some(next),
                _ => break,
            }
        }
        Ok(tools)
    }
}

#[async_trait::async_trait]
impl SessionClient for ConnectionRefresher {
    /// Go `Close`: waits for a connect or refresh in progress (no caller token), marks the
    /// refresher closed, and closes the session after releasing the lock.
    async fn close(&self) -> Result<(), Arc<Error>> {
        self.close_result
            .get_or_init(|| async {
                let session = {
                    let mut state = self.state.lock().await;
                    state.closed = true;
                    state.session.take()
                };
                match session {
                    Some(session) => session
                        .cancel()
                        .await
                        .map(|_| ())
                        .map_err(|err| Arc::new(Error::Transport(Box::new(err)))),
                    None => Ok(()),
                }
            })
            .await
            .clone()
    }
}

#[cfg(test)]
impl ConnectionRefresher {
    /// The current session's peer without connecting (Go tests' `c.session`); `None` while the
    /// lock is held.
    pub(crate) fn session_peer(&self) -> Option<Peer<RoleClient>> {
        let state = self.state.try_lock().ok()?;
        state.session.as_ref().map(|s| s.peer().clone())
    }
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
