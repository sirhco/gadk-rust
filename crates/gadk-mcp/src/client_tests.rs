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

//! Rust-only tests of the connection refresher: pagination, restart after a reconnect, the
//! double-reconnect text, ping reuse and the failed-refresh text (Task 6).

use std::any::TypeId;
use std::borrow::Cow;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gadk_agent::Context;
use gadk_auth::AuthContext;
use pretty_assertions::assert_eq;
use rmcp::model::{
    CallToolRequestParams, ClientRequest, JsonObject, ListToolsResult, PaginatedRequestParams,
    ProtocolVersion, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::{Peer, RequestContext};
use rmcp::transport::DynamicTransportError;
use rmcp::transport::streamable_http_client::StreamableHttpError;
use rmcp::{ErrorData, RoleClient, RoleServer, ServerHandler, ServiceError};
use serde_json::json;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use super::{CallContext, ConnectionRefresher, McpClient, SessionClient, is_refreshable};
use crate::config::default_client_info;
use crate::error::Error;
use crate::testutil::{ScriptedFactory, ToolServer, weather_server};

/// A server listing `pages` of tools: the first page answers a request without a cursor, page
/// `i` answers cursor `page-<i>`, and every page but the last names the next one. The `n`-th
/// `tools/list` request across all servers (counting from 0) signals `hung` and never answers
/// when `n` is in `hang_on`, so a test can kill the connection mid-listing.
#[derive(Clone)]
struct PagedServer {
    pages: Arc<Vec<Vec<Tool>>>,
    hang_on: Arc<Vec<usize>>,
    hung: Arc<Notify>,
    /// The cursor of every `tools/list` request any server answered or hung on, in order.
    requests: Requests,
}

/// The cursors of the `tools/list` requests the servers saw.
type Requests = Arc<Mutex<Vec<Option<String>>>>;

impl ServerHandler for PagedServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::LATEST_WITH_INITIALIZE)
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(ProtocolVersion::known_up_to(
            &ProtocolVersion::LATEST_WITH_INITIALIZE,
        ))
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let cursor = request.and_then(|r| r.cursor);
        let n = {
            let mut requests = self
                .requests
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            requests.push(cursor.clone());
            requests.len() - 1
        };
        if self.hang_on.contains(&n) {
            self.hung.notify_one();
            std::future::pending::<()>().await;
        }
        let index = match cursor.as_deref() {
            None => 0,
            Some(c) => c
                .strip_prefix("page-")
                .and_then(|i| i.parse::<usize>().ok())
                .filter(|i| *i < self.pages.len())
                .ok_or_else(|| ErrorData::invalid_params(format!("bad cursor {c}"), None))?,
        };
        let mut result = ListToolsResult::with_all_items(self.pages[index].clone());
        if index + 1 < self.pages.len() {
            result.next_cursor = Some(format!("page-{}", index + 1));
        }
        Ok(result)
    }
}

/// A server whose `ping` signals `pinged` and never answers.
#[derive(Clone)]
struct HungPing {
    pinged: Arc<Notify>,
}

impl ServerHandler for HungPing {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::LATEST_WITH_INITIALIZE)
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(ProtocolVersion::known_up_to(
            &ProtocolVersion::LATEST_WITH_INITIALIZE,
        ))
    }

    async fn ping(&self, _context: RequestContext<RoleServer>) -> Result<(), ErrorData> {
        self.pinged.notify_one();
        std::future::pending().await
    }
}

fn tool(name: &str) -> Tool {
    Tool::new(name.to_owned(), "", Arc::new(JsonObject::new()))
}

/// Pages `[a, b] [c] [d, e]`.
fn three_pages() -> Arc<Vec<Vec<Tool>>> {
    Arc::new(vec![
        vec![tool("a"), tool("b")],
        vec![tool("c")],
        vec![tool("d"), tool("e")],
    ])
}

fn names(tools: &[Tool]) -> Vec<String> {
    tools.iter().map(|t| t.name.to_string()).collect()
}

/// A refresher over `ScriptedFactory`-served `PagedServer`s that hang on the requests numbered
/// in `hang_on`; returns the factory, the refresher, the hang signal and the cursors the servers
/// were asked for.
fn paged(
    hang_on: &[usize],
) -> (
    Arc<ScriptedFactory>,
    Arc<ConnectionRefresher>,
    Arc<Notify>,
    Requests,
) {
    let server = PagedServer {
        pages: three_pages(),
        hang_on: Arc::new(hang_on.to_vec()),
        hung: Arc::new(Notify::new()),
        requests: Arc::new(Mutex::new(Vec::new())),
    };
    let (hung, requests) = (server.hung.clone(), server.requests.clone());
    let factory = ScriptedFactory::new(move || server.clone());
    let c = Arc::new(ConnectionRefresher::new(
        default_client_info(),
        factory.clone(),
    ));
    (factory, c, hung, requests)
}

fn cursors(requests: &Mutex<Vec<Option<String>>>) -> Vec<Option<String>> {
    requests
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

fn page(n: usize) -> Option<String> {
    Some(format!("page-{n}"))
}

fn weather_params() -> CallToolRequestParams {
    let mut params = CallToolRequestParams::new("get_weather");
    params.arguments = json!({"city": "Paris"}).as_object().cloned();
    params
}

async fn ping(peer: &Peer<RoleClient>) -> Result<(), ServiceError> {
    peer.send_request(ClientRequest::PingRequest(Default::default()))
        .await
        .map(|_| ())
}

fn same_session(a: &Peer<RoleClient>, b: &Peer<RoleClient>) -> bool {
    match (a.peer_info(), b.peer_info()) {
        (Some(a), Some(b)) => Arc::ptr_eq(&a, &b),
        _ => false,
    }
}

/// Waits (bounded) until `cond` holds: server tasks end asynchronously.
async fn eventually(cond: impl Fn() -> bool) {
    for _ in 0..500 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    panic!("condition not reached");
}

#[tokio::test]
async fn list_tools_follows_next_cursor_across_pages() {
    let (factory, c, _, requests) = paged(&[]);
    let tools = c.list_tools(&CallContext::background()).await.unwrap();
    assert_eq!(names(&tools), ["a", "b", "c", "d", "e"]);
    assert_eq!(cursors(&requests), [None, page(1), page(2)]);
    assert_eq!(factory.connects(), 1);
    c.close().await.unwrap();
}

// Paused clock: a killed server waits up to 2 s for its hung handler before closing.
#[tokio::test(start_paused = true)]
async fn list_tools_restarts_from_the_first_page_after_a_reconnect() {
    // Request 1 is the first session's page-1 request.
    let (factory, c, hung, requests) = paged(&[1]);
    let listing = tokio::spawn({
        let c = c.clone();
        async move { c.list_tools(&CallContext::background()).await }
    });
    // The first session is lost while page 1 is pending.
    hung.notified().await;
    factory.kill_last().await;

    let tools = listing.await.unwrap().unwrap();
    // Nothing from before the reconnect is kept, and nothing is listed twice.
    assert_eq!(names(&tools), ["a", "b", "c", "d", "e"]);
    // Go's withRetry retries the failed page-1 request on the new session; ListTools then
    // discards that answer and restarts from the first page.
    assert_eq!(
        cursors(&requests),
        [None, page(1), page(1), None, page(1), page(2)]
    );
    assert_eq!(factory.connects(), 2);
    c.close().await.unwrap();
}

// Paused clock: a killed server waits up to 2 s for its hung handler before closing.
#[tokio::test(start_paused = true)]
async fn list_tools_fails_when_the_connection_is_lost_again_after_a_reconnect() {
    // Requests: first session `None`, `page-1` (hangs: lost, reconnect); second session
    // `page-1` (the retry, discarded), `None` (the restart), `page-1` (hangs: lost again).
    let (factory, c, hung, requests) = paged(&[1, 4]);
    let listing = tokio::spawn({
        let c = c.clone();
        async move { c.list_tools(&CallContext::background()).await }
    });
    hung.notified().await;
    factory.kill_last().await;
    hung.notified().await;
    factory.kill_last().await;

    let err = listing.await.unwrap().unwrap_err();
    assert_eq!(
        err.to_string(),
        "failed to list MCP tools: connection lost again after reconnection"
    );
    assert!(matches!(err, Error::ListToolsReconnectedTwice));
    // The third session answered the second retry, and the listing gave up.
    assert_eq!(
        cursors(&requests),
        [None, page(1), page(1), None, page(1), page(1)]
    );
    assert_eq!(factory.connects(), 3);
    c.close().await.unwrap();
}

#[tokio::test]
async fn refresh_reuses_a_session_another_caller_already_reconnected() {
    let factory = ScriptedFactory::new(weather_server);
    let c = ConnectionRefresher::new(default_client_info(), factory.clone());
    let cx = CallContext::background();
    let stale = c.get_session(&cx).await.unwrap();
    factory.kill_last().await;

    // Caller A finds the session dead: the ping fails, so it reconnects.
    let (_, reconnected) = c
        .with_retry(&cx, |peer| async move {
            peer.call_tool(weather_params()).await.map_err(Error::from)
        })
        .await
        .unwrap();
    assert!(reconnected);
    assert_eq!(factory.connects(), 2);
    let fresh = c.session_peer().unwrap();

    // Caller B still holds the lost session's peer. Its refresh pings the fresh session, which
    // answers, so it is reused rather than replaced.
    let mut held = Some(stale);
    let (used, reconnected) = c
        .with_retry(&cx, |peer| {
            let peer = held.take().unwrap_or(peer);
            async move {
                ping(&peer).await.map_err(Error::from)?;
                Ok(peer)
            }
        })
        .await
        .unwrap();
    assert!(reconnected);
    assert!(same_session(&used, &fresh));
    assert_eq!(factory.connects(), 2);
    assert!(same_session(&c.session_peer().unwrap(), &fresh));
    c.close().await.unwrap();
}

#[tokio::test]
async fn failed_refresh_reports_both_errors() {
    let factory = ScriptedFactory::new(weather_server);
    let c = ConnectionRefresher::new(default_client_info(), factory.clone());
    let cx = CallContext::background();
    c.call_tool(&cx, weather_params()).await.unwrap();
    factory.fail_with(Some("dial failed"));
    factory.kill_last().await;

    let err = c.call_tool(&cx, weather_params()).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "Transport closed (reconnection also failed: failed to refresh MCP session: dial failed)"
    );
    assert!(matches!(err, Error::ReconnectFailed { .. }));
    // The lost session was dropped and none replaced it.
    assert!(c.disconnected());
    assert_eq!(factory.connects(), 2);

    // The next call connects afresh (Go getSession), once the server is reachable again.
    factory.fail_with(None);
    c.call_tool(&cx, weather_params()).await.unwrap();
    assert!(!c.disconnected());
    assert_eq!(factory.connects(), 3);
    c.close().await.unwrap();
}

#[tokio::test]
async fn cancelled_refresh_ping_closes_the_session_as_go_does() {
    let pinged = Arc::new(Notify::new());
    let factory = ScriptedFactory::new({
        let pinged = pinged.clone();
        move || HungPing {
            pinged: pinged.clone(),
        }
    });
    let c = Arc::new(ConnectionRefresher::new(
        default_client_info(),
        factory.clone(),
    ));
    let cx = CallContext::background();
    c.get_session(&cx).await.unwrap();

    // The operation fails as if the connection were lost; the refresh's ping hangs and the
    // caller is cancelled during it.
    let call = tokio::spawn({
        let (c, cx) = (c.clone(), cx.clone());
        async move {
            c.with_retry(&cx, |_peer| async { Err::<(), _>(Error::ConnectionClosed) })
                .await
        }
    });
    pinged.notified().await;
    cx.cancel.cancel();

    let err = tokio::time::timeout(Duration::from_secs(5), call)
        .await
        .expect("cancelled refresh returned")
        .unwrap()
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "connection closed (reconnection also failed: failed to refresh MCP session: context canceled)"
    );
    // Go closes the session whose ping failed, and the cancelled reconnect leaves none.
    assert!(c.disconnected());
    assert_eq!(factory.connects(), 1);
}

#[tokio::test]
async fn calls_on_one_session_run_concurrently() {
    // Each call waits until both are in the server at once: if a call held the session lock,
    // the second could never start and both would time out.
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let factory = ScriptedFactory::new({
        let barrier = barrier.clone();
        move || {
            let barrier = barrier.clone();
            ToolServer::new("meet", "1").tool(
                tool("meet"),
                move |_params: CallToolRequestParams, _context| {
                    let barrier = barrier.clone();
                    async move {
                        barrier.wait().await;
                        Ok(rmcp::model::CallToolResult::success(vec![
                            rmcp::model::ContentBlock::text("met"),
                        ]))
                    }
                },
            )
        }
    });
    let c = ConnectionRefresher::new(default_client_info(), factory.clone());
    let cx = CallContext::background();
    let (a, b) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            c.call_tool(&cx, CallToolRequestParams::new("meet")),
            c.call_tool(&cx, CallToolRequestParams::new("meet")),
        )
    })
    .await
    .expect("calls on one session did not overlap");
    a.unwrap();
    b.unwrap();
    assert_eq!(factory.connects(), 1);
    c.close().await.unwrap();
}

#[tokio::test]
async fn call_tool_reconnects_once_after_the_connection_is_lost() {
    let factory = ScriptedFactory::new(weather_server);
    let c = ConnectionRefresher::new(default_client_info(), factory.clone());
    let cx = CallContext::background();
    let first = c.call_tool(&cx, weather_params()).await.unwrap();
    factory.kill_last().await;
    let second = c.call_tool(&cx, weather_params()).await.unwrap();
    assert_eq!(first, second);
    assert_eq!(factory.connects(), 2);
    c.close().await.unwrap();
}

#[tokio::test]
async fn non_refreshable_errors_are_returned_without_a_reconnect() {
    let factory = ScriptedFactory::new(weather_server);
    let c = ConnectionRefresher::new(default_client_info(), factory.clone());
    let err = c
        .call_tool(
            &CallContext::background(),
            CallToolRequestParams::new("nope"),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::Service(ServiceError::McpError(_))),
        "{err:?}"
    );
    assert_eq!(factory.connects(), 1);
    c.close().await.unwrap();
}

#[tokio::test]
async fn connect_failures_read_failed_to_init() {
    let factory = ScriptedFactory::failing("dial failed");
    let c = ConnectionRefresher::new(default_client_info(), factory.clone());
    let cx = CallContext::background();
    let err = c.call_tool(&cx, weather_params()).await.unwrap_err();
    assert_eq!(err.to_string(), "failed to init MCP session: dial failed");
    let err = c.list_tools(&cx).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "failed to list MCP tools: failed to init MCP session: dial failed"
    );
    // A failed first connect is not retried within the call.
    assert_eq!(factory.connects(), 2);
    assert!(c.disconnected());
}

#[tokio::test]
async fn close_is_idempotent_and_final() {
    let factory = ScriptedFactory::new(weather_server);
    let c = Arc::new(ConnectionRefresher::new(
        default_client_info(),
        factory.clone(),
    ));
    let cx = CallContext::background();
    assert!(c.disconnected());
    c.call_tool(&cx, weather_params()).await.unwrap();
    assert!(!c.disconnected());
    assert_eq!(factory.open_sessions(), 1);

    let (a, b) = tokio::join!(c.close(), c.close());
    a.unwrap();
    b.unwrap();
    c.close().await.unwrap();
    eventually(|| factory.open_sessions() == 0).await;
    assert!(c.disconnected());

    let err = c.call_tool(&cx, weather_params()).await.unwrap_err();
    assert!(err.is_connection_closed(), "{err:?}");
    assert_eq!(err.to_string(), "connection closed");
    let err = c.list_tools(&cx).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "failed to list MCP tools: connection closed"
    );
    let err = c.refresh_connection(&cx).await.unwrap_err();
    assert!(err.is_connection_closed());
    assert_eq!(factory.connects(), 1);
}

#[tokio::test]
async fn close_before_any_connect_connects_nothing() {
    let factory = ScriptedFactory::new(weather_server);
    let c = ConnectionRefresher::new(default_client_info(), factory.clone());
    c.close().await.unwrap();
    let err = c
        .call_tool(&CallContext::background(), weather_params())
        .await
        .unwrap_err();
    assert!(err.is_connection_closed());
    assert_eq!(factory.connects(), 0);
}

#[tokio::test]
async fn disconnected_is_false_while_a_connect_is_in_progress() {
    let factory = ScriptedFactory::new(weather_server);
    factory.close_gate();
    let c = Arc::new(ConnectionRefresher::new(
        default_client_info(),
        factory.clone(),
    ));
    let owner = tokio::spawn({
        let c = c.clone();
        async move { c.get_session(&CallContext::background()).await.map(|_| ()) }
    });
    factory.entered().await;
    assert!(!c.disconnected());
    factory.open_gate();
    owner.await.unwrap().unwrap();
    assert!(!c.disconnected());
    c.close().await.unwrap();
}

#[test]
fn refreshable_errors_are_the_lost_connection_ones() {
    fn send(error: Box<dyn std::error::Error + Send + Sync>) -> Error {
        Error::Service(ServiceError::TransportSend(
            DynamicTransportError::from_parts("test", TypeId::of::<()>(), error),
        ))
    }
    type HttpError = StreamableHttpError<reqwest::Error>;

    let refreshable = [
        Error::ConnectionClosed,
        Error::Service(ServiceError::TransportClosed),
        send(Box::new(HttpError::SessionExpired)),
        send(Box::new(HttpError::TransportChannelClosed)),
        send(Box::new(HttpError::UnexpectedEndOfStream)),
        send(Box::new(io::Error::from(io::ErrorKind::BrokenPipe))),
        send(Box::new(io::Error::from(io::ErrorKind::UnexpectedEof))),
        Error::Transport(Box::new(io::Error::from(io::ErrorKind::BrokenPipe))),
        Error::CallTool {
            name: "t".into(),
            source: Box::new(Error::ConnectionClosed),
        },
    ];
    for err in &refreshable {
        assert!(is_refreshable(err), "want refreshable: {err:?}");
    }

    let not_refreshable = [
        Error::Cancelled,
        Error::Service(ServiceError::UnexpectedResponse),
        Error::Service(ServiceError::McpError(ErrorData::invalid_params("x", None))),
        send(Box::new(HttpError::UnexpectedServerResponse("x".into()))),
        send(Box::new(io::Error::from(io::ErrorKind::ConnectionRefused))),
        Error::ToolExecution("Tool execution failed.".into()),
        Error::InitSession("dial failed".into()),
    ];
    for err in &not_refreshable {
        assert!(!is_refreshable(err), "want not refreshable: {err:?}");
    }
}

#[test]
fn call_contexts_carry_identity_and_token() {
    let background = CallContext::background();
    assert_eq!(background.auth, AuthContext::anonymous());
    assert!(!background.cancel.is_cancelled());

    let token = CancellationToken::new();
    let ctx = Context::builder()
        .invocation_id("inv-1")
        .cancellation_token(token.clone())
        .build();
    let from_ctx = CallContext::from(&ctx);
    let from_readonly = CallContext::from(ctx.as_readonly());
    assert_eq!(from_ctx.auth, AuthContext::from(&ctx));
    assert_eq!(from_readonly.auth, AuthContext::from(ctx.as_readonly()));
    token.cancel();
    assert!(from_ctx.cancel.is_cancelled());
    assert!(from_readonly.cancel.is_cancelled());
}

/// A server whose `hang` tool and whose `tools/list` signal `entered` and never answer; its
/// `weather` tools answer normally.
#[derive(Clone)]
struct HangingServer {
    inner: ToolServer,
    hang_list: Arc<std::sync::atomic::AtomicBool>,
    entered: Arc<Notify>,
}

impl ServerHandler for HangingServer {
    fn get_info(&self) -> ServerConfig {
        self.inner.get_info()
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        self.inner.supported_protocol_versions()
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        if self.hang_list.load(std::sync::atomic::Ordering::SeqCst) {
            self.entered.notify_one();
            std::future::pending::<()>().await;
        }
        self.inner.list_tools(request, context).await
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::CallToolResponse, ErrorData> {
        self.inner.call_tool(request, context).await
    }
}

/// A refresher over a `HangingServer` whose `hang` tool never answers; returns the factory, the
/// refresher, the "request reached the server" signal and the switch that hangs `tools/list`.
fn hanging() -> (
    Arc<ScriptedFactory>,
    Arc<ConnectionRefresher>,
    Arc<Notify>,
    Arc<std::sync::atomic::AtomicBool>,
) {
    let entered = Arc::new(Notify::new());
    let hang_list = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let factory = ScriptedFactory::new({
        let (entered, hang_list) = (entered.clone(), hang_list.clone());
        move || {
            let gate = entered.clone();
            HangingServer {
                inner: weather_server().tool(
                    tool("hang"),
                    move |_params: CallToolRequestParams, _context| {
                        let gate = gate.clone();
                        async move {
                            gate.notify_one();
                            std::future::pending().await
                        }
                    },
                ),
                hang_list: hang_list.clone(),
                entered: entered.clone(),
            }
        }
    });
    let c = Arc::new(ConnectionRefresher::new(
        default_client_info(),
        factory.clone(),
    ));
    (factory, c, entered, hang_list)
}

#[tokio::test]
async fn cancelling_an_in_flight_call_returns_at_once_and_keeps_the_session() {
    let (factory, c, entered, _) = hanging();
    let cx = CallContext::background();
    let call = tokio::spawn({
        let (c, cx) = (c.clone(), cx.clone());
        async move { c.call_tool(&cx, CallToolRequestParams::new("hang")).await }
    });
    entered.notified().await;
    let peer = c.session_peer().expect("a session");
    cx.cancel.cancel();

    // Go passes ctx into session.CallTool: cancellation ends the call with `context canceled`.
    let err = tokio::time::timeout(Duration::from_secs(5), call)
        .await
        .expect("cancelled call returned")
        .unwrap()
        .unwrap_err();
    assert!(err.is_cancelled(), "{err:?}");
    assert_eq!(err.to_string(), "context canceled");

    // The session survives: the next call reuses it without a reconnect.
    assert!(!c.disconnected());
    c.call_tool(&CallContext::background(), weather_params())
        .await
        .unwrap();
    assert!(same_session(&peer, &c.session_peer().expect("a session")));
    assert_eq!(factory.connects(), 1);
    c.close().await.unwrap();
}

#[tokio::test]
async fn cancelling_an_in_flight_listing_returns_at_once_and_keeps_the_session() {
    let (factory, c, entered, hang_list) = hanging();
    c.list_tools(&CallContext::background()).await.unwrap();
    let peer = c.session_peer().expect("a session");
    hang_list.store(true, std::sync::atomic::Ordering::SeqCst);
    let cx = CallContext::background();
    let list = tokio::spawn({
        let (c, cx) = (c.clone(), cx.clone());
        async move { c.list_tools(&cx).await }
    });
    entered.notified().await;
    cx.cancel.cancel();

    let err = tokio::time::timeout(Duration::from_secs(5), list)
        .await
        .expect("cancelled listing returned")
        .unwrap()
        .unwrap_err();
    assert!(err.is_cancelled(), "{err:?}");

    hang_list.store(false, std::sync::atomic::Ordering::SeqCst);
    c.list_tools(&CallContext::background()).await.unwrap();
    assert!(same_session(&peer, &c.session_peer().expect("a session")));
    assert_eq!(factory.connects(), 1);
    c.close().await.unwrap();
}
