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

//! [`HttpTestServer`]: a streamable-HTTP MCP server on `127.0.0.1` (rmcp
//! `StreamableHttpService` behind a hyper listener), for testing header tracking and per-user
//! sessions.

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use http::{Method, Request, Response, StatusCode};
use hyper::body::{Body, Incoming};
use hyper_util::rt::TokioIo;
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock, JsonObject, Tool};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData, RoleServer, ServerHandler};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt as _;

use super::ToolServer;
use super::lock;

/// One request the server received.
#[derive(Clone, Debug)]
pub struct RecordedRequest {
    /// The request method.
    pub method: http::Method,
    /// The request headers, as received.
    pub headers: http::HeaderMap,
}

/// Answers a request before the MCP service sees it: `Some(response)` is sent instead, `None`
/// passes the request on. It gets the request head and its body bytes.
pub type FrontHook =
    Arc<dyn Fn(&http::request::Parts, &[u8]) -> Option<http::Response<String>> + Send + Sync>;

/// The chunk type of every response body (`bytes::Bytes`, named through `String`'s `Body` impl).
type Chunk = <String as Body>::Data;
/// A response body: rmcp's (JSON or an SSE stream) or a fixed text.
type OutBody = Pin<Box<dyn Body<Data = Chunk, Error = Infallible> + Send>>;
/// The rmcp service, type-erased.
type McpCall = Arc<dyn Fn(Request<String>) -> BoxFuture<'static, Response<OutBody>> + Send + Sync>;

/// A local streamable-HTTP MCP server. Every request is recorded, passed to the front hook, then
/// served. Dropping it stops the listener.
pub struct HttpTestServer {
    addr: SocketAddr,
    state: Arc<State>,
    stop: CancellationToken,
}

struct State {
    requests: Mutex<Vec<RecordedRequest>>,
    front: Mutex<Option<FrontHook>>,
    holds: Mutex<Vec<Arc<HoldState>>>,
    /// `Some` for [`HttpTestServer::user_bound`].
    users: Option<Mutex<UserBook>>,
}

/// Per user, the MCP sessions its requests arrived on and the ones it closed with DELETE, plus the
/// user each MCP session is bound to.
#[derive(Default)]
struct UserBook {
    sessions: HashMap<String, HashSet<String>>,
    deleted: HashMap<String, HashSet<String>>,
    owner: HashMap<String, String>,
}

struct HoldState {
    needle: String,
    initializing: watch::Sender<bool>,
    released: watch::Sender<bool>,
}

impl HttpTestServer {
    /// Serves a fresh `make_server()` per MCP session (stateful streamable HTTP) on an ephemeral
    /// `127.0.0.1` port.
    pub async fn start<S, F>(make_server: F) -> Self
    where
        F: Fn() -> S + Send + Sync + 'static,
        S: ServerHandler,
    {
        Self::serve(make_server, None).await
    }

    /// A server where every request needs `Authorization: Bearer token-of-<user>` (else 401); the
    /// MCP session is bound to the user whose request created it, and a request on another user's
    /// session gets 403 `session user mismatch`; the one tool `whoami` ("returns the caller")
    /// answers the user as text. Records the sessions each user's requests arrived on and those
    /// each user closed with DELETE.
    pub async fn user_bound() -> Self {
        Self::serve(user_bound_server, Some(Mutex::new(UserBook::default()))).await
    }

    async fn serve<S, F>(make_server: F, users: Option<Mutex<UserBook>>) -> Self
    where
        F: Fn() -> S + Send + Sync + 'static,
        S: ServerHandler,
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap_or_else(|e| panic!("HttpTestServer: bind 127.0.0.1:0: {e}"));
        let addr = listener
            .local_addr()
            .unwrap_or_else(|e| panic!("HttpTestServer: local_addr: {e}"));
        let stop = CancellationToken::new();
        let mut config = StreamableHttpServerConfig::default();
        config.cancellation_token = stop.child_token();
        let service = StreamableHttpService::new(
            move || Ok(make_server()),
            Arc::new(LocalSessionManager::default()),
            config,
        );
        let mcp: McpCall = Arc::new(move |req| {
            let service = service.clone();
            Box::pin(async move {
                match service.oneshot(req).await {
                    Ok(resp) => resp.map(|body| Box::pin(body) as OutBody),
                    Err(never) => match never {},
                }
            })
        });
        let state = Arc::new(State {
            requests: Mutex::new(Vec::new()),
            front: Mutex::new(None),
            holds: Mutex::new(Vec::new()),
            users,
        });
        tokio::spawn(accept_loop(listener, state.clone(), mcp, stop.clone()));
        Self { addr, state, stop }
    }

    /// The MCP endpoint URL (`http://127.0.0.1:<port>/`).
    pub fn url(&self) -> String {
        format!("http://{}/", self.addr)
    }

    /// Every request received so far, in arrival order.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        lock(&self.state.requests).clone()
    }

    /// How many distinct MCP sessions `user`'s requests arrived on.
    pub fn sessions_of(&self, user: &str) -> usize {
        self.state.users.as_ref().map_or(0, |book| {
            lock(book).sessions.get(user).map_or(0, HashSet::len)
        })
    }

    /// How many distinct MCP sessions `user` closed with DELETE.
    pub fn deleted_of(&self, user: &str) -> usize {
        self.state.users.as_ref().map_or(0, |book| {
            lock(book).deleted.get(user).map_or(0, HashSet::len)
        })
    }

    /// Holds the `initialize` POST of any request whose `Authorization` header contains `needle`
    /// until [`InitializeHold::release`].
    pub fn hold_initialize(&self, needle: &str) -> InitializeHold {
        let hold = Arc::new(HoldState {
            needle: needle.to_owned(),
            initializing: watch::Sender::new(false),
            released: watch::Sender::new(false),
        });
        lock(&self.state.holds).push(hold.clone());
        InitializeHold { hold }
    }

    /// Installs (or with `None` removes) the front hook.
    pub fn set_front(&self, hook: Option<FrontHook>) {
        *lock(&self.state.front) = hook;
    }
}

impl Drop for HttpTestServer {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

/// A held `initialize` request (see [`HttpTestServer::hold_initialize`]).
pub struct InitializeHold {
    hold: Arc<HoldState>,
}

impl InitializeHold {
    /// Waits until a matching `initialize` request has arrived and is being held.
    pub async fn initializing(&self) {
        let mut rx = self.hold.initializing.subscribe();
        // The sender lives in `self.hold`, so this only returns once a request is held.
        let _ = rx.wait_for(|held| *held).await;
    }

    /// Lets the held request (and later matching ones) through.
    pub fn release(&self) {
        self.hold.released.send_replace(true);
    }
}

async fn accept_loop(
    listener: tokio::net::TcpListener,
    state: Arc<State>,
    mcp: McpCall,
    stop: CancellationToken,
) {
    loop {
        let stream = tokio::select! {
            () = stop.cancelled() => return,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => stream,
                Err(_) => continue,
            },
        };
        let (state, mcp, stop) = (state.clone(), mcp.clone(), stop.clone());
        tokio::spawn(async move {
            let service = hyper::service::service_fn(move |req| {
                let (state, mcp) = (state.clone(), mcp.clone());
                async move { Ok::<_, Infallible>(handle(&state, &mcp, req).await) }
            });
            let conn = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service);
            tokio::select! {
                _ = conn => {}
                () = stop.cancelled() => {}
            }
        });
    }
}

/// Record, hold, front hook, then (user-bound) the per-user recording, the bearer-token check and
/// the session-user check, then the MCP service.
async fn handle(state: &State, mcp: &McpCall, req: Request<Incoming>) -> Response<OutBody> {
    let (parts, mut body) = req.into_parts();
    let mut bytes = Vec::new();
    while let Some(frame) = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await {
        match frame {
            Ok(frame) => {
                if let Ok(data) = frame.into_data() {
                    bytes.extend_from_slice(&data);
                }
            }
            Err(_) => break,
        }
    }
    lock(&state.requests).push(RecordedRequest {
        method: parts.method.clone(),
        headers: parts.headers.clone(),
    });

    let authorization = header(&parts, http::header::AUTHORIZATION);
    if parts.method == Method::POST && contains(&bytes, b"\"initialize\"") {
        let holds: Vec<_> = lock(&state.holds)
            .iter()
            .filter(|h| authorization.contains(h.needle.as_str()))
            .cloned()
            .collect();
        for hold in holds {
            hold.initializing.send_replace(true);
            let mut released = hold.released.subscribe();
            let _ = released.wait_for(|r| *r).await;
        }
    }

    let front = lock(&state.front).clone();
    if let Some(resp) = front.and_then(|hook| hook(&parts, &bytes)) {
        return resp.map(|body| Box::pin(body) as OutBody);
    }

    let session = parts
        .headers
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let mut user = None;
    if let Some(book) = &state.users {
        // Record the user's request before the bearer token is verified.
        let recorded = authorization
            .strip_prefix("Bearer token-of-")
            .unwrap_or(authorization);
        if let Some(id) = &session {
            let mut book = lock(book);
            record(&mut book.sessions, recorded, id);
            if parts.method == Method::DELETE {
                record(&mut book.deleted, recorded, id);
            }
        }
        let verified = match verify_bearer(authorization) {
            Ok(verified) => verified,
            Err((status, msg)) => return text(status, msg),
        };
        if let Some(id) = &session
            && let Some(owner) = lock(book).owner.get(id)
            && !owner.is_empty()
            && *owner != verified
        {
            return text(StatusCode::FORBIDDEN, "session user mismatch");
        }
        user = Some(verified);
    }

    let method = parts.method.clone();
    let resp = mcp(Request::from_parts(
        parts,
        String::from_utf8_lossy(&bytes).into_owned(),
    ))
    .await;

    if let (Some(book), Some(user)) = (&state.users, user) {
        let mut book = lock(book);
        match &session {
            None => {
                if let Some(id) = resp
                    .headers()
                    .get("mcp-session-id")
                    .and_then(|v| v.to_str().ok())
                {
                    book.owner.insert(id.to_owned(), user);
                }
            }
            Some(id) if method == Method::DELETE && resp.status().is_success() => {
                book.owner.remove(id);
            }
            Some(_) => {}
        }
    }
    resp
}

fn header(parts: &http::request::Parts, name: http::header::HeaderName) -> &str {
    parts
        .headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn record(m: &mut HashMap<String, HashSet<String>>, user: &str, id: &str) {
    m.entry(user.to_owned()).or_default().insert(id.to_owned());
}

/// Verifies the bearer token: the user of a `Bearer token-of-<user>` header, else 401 with a
/// message.
fn verify_bearer(authorization: &str) -> Result<String, (StatusCode, &'static str)> {
    let fields: Vec<&str> = authorization.split_whitespace().collect();
    if fields.len() != 2 || !fields[0].eq_ignore_ascii_case("bearer") {
        return Err((StatusCode::UNAUTHORIZED, "no bearer token"));
    }
    match fields[1].strip_prefix("token-of-") {
        Some(user) => Ok(user.to_owned()),
        None => Err((StatusCode::UNAUTHORIZED, "invalid token")),
    }
}

/// A plain-text error response: `msg` and a newline, `nosniff`, with `status`.
fn text(status: StatusCode, msg: &str) -> Response<OutBody> {
    let mut resp = Response::new(Box::pin(format!("{msg}\n")) as OutBody);
    *resp.status_mut() = status;
    let headers = resp.headers_mut();
    headers.insert(
        http::header::CONTENT_TYPE,
        http::HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    headers.insert(
        http::header::X_CONTENT_TYPE_OPTIONS,
        http::HeaderValue::from_static("nosniff"),
    );
    resp
}

/// `user_bound_server` `v1.0.0` with a `whoami` tool: the caller's user ID as text.
fn user_bound_server() -> ToolServer {
    let mut schema = JsonObject::new();
    schema.insert("additionalProperties".into(), false.into());
    schema.insert("type".into(), "object".into());
    ToolServer::new("user_bound_server", "v1.0.0").tool(
        Tool::new("whoami", "returns the caller", schema),
        |_: CallToolRequestParams, context: RequestContext<RoleServer>| async move {
            let user = context
                .extensions
                .get::<http::request::Parts>()
                .and_then(|parts| verify_bearer(header(parts, http::header::AUTHORIZATION)).ok())
                .unwrap_or_default();
            let mut result = CallToolResult::success(vec![ContentBlock::text(user)]);
            result.is_error = None;
            Ok::<_, ErrorData>(result)
        },
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::time::Duration;

    use pretty_assertions::assert_eq;
    use rmcp::RoleClient;
    use rmcp::model::ClientConfig;
    use rmcp::service::RunningService;
    use rmcp::transport::StreamableHttpClientTransport;
    use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;

    use super::*;
    use crate::config::{default_client_info, serve_client};
    use crate::testutil::weather_server;

    const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"raw","version":"1"}}}"#;

    async fn connect(
        url: &str,
        token: Option<&str>,
    ) -> Result<RunningService<RoleClient, ClientConfig>, crate::BoxError> {
        let mut config = StreamableHttpClientTransportConfig::with_uri(url);
        if let Some(token) = token {
            config = config.auth_header(token);
        }
        let transport = StreamableHttpClientTransport::with_client(reqwest::Client::new(), config);
        serve_client(default_client_info(), transport).await
    }

    async fn whoami(client: &RunningService<RoleClient, ClientConfig>) -> String {
        let result = client
            .call_tool(CallToolRequestParams::new("whoami"))
            .await
            .unwrap();
        match result.content.as_slice() {
            [block] => block.as_text().unwrap().text.clone(),
            other => panic!("want one block, got {other:?}"),
        }
    }

    /// A raw `initialize` POST with `authorization`, as reqwest sends it.
    async fn raw_initialize(url: &str, authorization: Option<&str>) -> reqwest::Response {
        let mut req = reqwest::Client::new()
            .post(url)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .body(INITIALIZE);
        if let Some(authorization) = authorization {
            req = req.header("authorization", authorization);
        }
        req.send().await.unwrap()
    }

    #[tokio::test]
    async fn start_serves_mcp_and_records_every_request() {
        let server = HttpTestServer::start(weather_server).await;
        assert!(server.url().starts_with("http://127.0.0.1:"));
        assert!(server.url().ends_with('/'));
        let client = connect(&server.url(), None).await.unwrap();
        let tools = client.list_tools(None).await.unwrap().tools;
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "get_weather");
        client.cancel().await.unwrap();

        let requests = server.requests();
        assert!(requests.len() >= 3, "{requests:?}");
        assert_eq!(requests[0].method, Method::POST);
        assert_eq!(requests[0].headers["content-type"], "application/json");
        // rmcp's client closes its session with DELETE.
        assert!(
            requests.iter().any(|r| r.method == Method::DELETE),
            "{requests:?}"
        );
    }

    #[tokio::test]
    async fn front_hook_answers_instead_of_the_service() {
        let server = HttpTestServer::start(weather_server).await;
        let seen: Arc<Mutex<Vec<String>>> = Arc::default();
        let seen2 = seen.clone();
        server.set_front(Some(Arc::new(move |parts, body| {
            lock(&seen2).push(format!(
                "{} {}",
                parts.method,
                String::from_utf8_lossy(body)
            ));
            Some(
                Response::builder()
                    .status(StatusCode::UNAUTHORIZED)
                    .header("www-authenticate", r#"Bearer error="invalid_token""#)
                    .body("nope".to_owned())
                    .unwrap(),
            )
        })));
        let resp = raw_initialize(&server.url(), None).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            resp.headers()["www-authenticate"],
            r#"Bearer error="invalid_token""#
        );
        assert_eq!(resp.text().await.unwrap(), "nope");
        assert!(connect(&server.url(), None).await.is_err());
        let seen = lock(&seen).clone();
        assert_eq!(seen[0], format!("POST {INITIALIZE}"));
        assert_eq!(server.requests().len(), seen.len());

        server.set_front(None);
        let client = connect(&server.url(), None).await.unwrap();
        client.list_tools(None).await.unwrap();
        client.cancel().await.unwrap();
    }

    #[tokio::test]
    async fn user_bound_gives_each_user_a_session_bound_to_them() {
        let server = HttpTestServer::user_bound().await;
        let alice = connect(&server.url(), Some("token-of-alice"))
            .await
            .unwrap();
        let bob = connect(&server.url(), Some("token-of-bob")).await.unwrap();
        let tools = alice.list_tools(None).await.unwrap().tools;
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "whoami");
        assert_eq!(tools[0].description.as_deref(), Some("returns the caller"));
        assert_eq!(whoami(&alice).await, "alice");
        assert_eq!(whoami(&bob).await, "bob");
        assert_eq!(whoami(&alice).await, "alice");
        assert_eq!(
            (server.sessions_of("alice"), server.sessions_of("bob")),
            (1, 1)
        );
        assert_eq!(
            (server.deleted_of("alice"), server.deleted_of("bob")),
            (0, 0)
        );

        alice.cancel().await.unwrap();
        assert_eq!(
            (server.deleted_of("alice"), server.deleted_of("bob")),
            (1, 0)
        );
        assert_eq!(whoami(&bob).await, "bob");
        bob.cancel().await.unwrap();
        assert_eq!(server.deleted_of("bob"), 1);
        assert_eq!(server.sessions_of("carol"), 0);
    }

    #[tokio::test]
    async fn user_bound_rejects_bad_tokens_and_foreign_sessions_as_go_sdk_does() {
        let server = HttpTestServer::user_bound().await;
        for (authorization, want) in [
            (None, "no bearer token\n"),
            (Some("Basic dXNlcg=="), "no bearer token\n"),
            (Some("Bearer revoked-alice"), "invalid token\n"),
        ] {
            let resp = raw_initialize(&server.url(), authorization).await;
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{authorization:?}");
            assert!(!resp.headers().contains_key("www-authenticate"));
            assert_eq!(resp.headers()["content-type"], "text/plain; charset=utf-8");
            assert_eq!(resp.text().await.unwrap(), want);
        }
        assert!(connect(&server.url(), Some("revoked-alice")).await.is_err());

        let resp = raw_initialize(&server.url(), Some("Bearer token-of-alice")).await;
        assert!(resp.status().is_success(), "{resp:?}");
        let session = resp.headers()["mcp-session-id"]
            .to_str()
            .unwrap()
            .to_owned();
        let foreign = reqwest::Client::new()
            .post(server.url())
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .header("authorization", "Bearer token-of-bob")
            .header("mcp-session-id", &session)
            .body(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#)
            .send()
            .await
            .unwrap();
        assert_eq!(foreign.status(), StatusCode::FORBIDDEN);
        assert_eq!(foreign.text().await.unwrap(), "session user mismatch\n");
        // Recorded before the check, as Go's recording handler does.
        assert_eq!(server.sessions_of("bob"), 1);
        assert_eq!(server.sessions_of("alice"), 0);
    }

    #[tokio::test]
    async fn static_token_is_one_user_for_everyone() {
        let server = HttpTestServer::user_bound().await;
        let first = connect(&server.url(), Some("token-of-svc")).await.unwrap();
        let second = connect(&server.url(), Some("token-of-svc")).await.unwrap();
        assert_eq!(whoami(&first).await, "svc");
        assert_eq!(whoami(&second).await, "svc");
        assert_eq!(server.sessions_of("svc"), 2);
        first.cancel().await.unwrap();
        second.cancel().await.unwrap();
        assert_eq!(server.deleted_of("svc"), 2);
    }

    #[tokio::test]
    async fn hold_initialize_holds_the_matching_users_initialize_until_released() {
        let server = HttpTestServer::user_bound().await;
        let hold = server.hold_initialize("token-of-slow");
        let url = server.url();
        let slow = tokio::spawn(async move { connect(&url, Some("token-of-slow")).await });
        hold.initializing().await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!slow.is_finished(), "held initialize finished");

        // Other users' connects are not held.
        let alice = connect(&server.url(), Some("token-of-alice"))
            .await
            .unwrap();
        assert_eq!(whoami(&alice).await, "alice");

        hold.release();
        let slow = slow.await.unwrap().unwrap();
        assert_eq!(whoami(&slow).await, "slow");
        // Released holds let later initializes through.
        let again = connect(&server.url(), Some("token-of-slow")).await.unwrap();
        for client in [alice, slow, again] {
            client.cancel().await.unwrap();
        }
    }

    #[tokio::test]
    async fn dropping_the_server_stops_it() {
        let server = HttpTestServer::start(weather_server).await;
        let url = server.url();
        drop(server);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            reqwest::Client::new()
                .post(&url)
                .body(INITIALIZE)
                .send()
                .await
                .is_err()
        );
    }
}
