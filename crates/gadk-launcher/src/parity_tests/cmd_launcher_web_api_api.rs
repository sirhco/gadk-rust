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

//! Port of ADK-Go `cmd/launcher/web/api/api_test.go`.
//!
//! Go's `httptest.NewRecorder` + `router.ServeHTTP` is `WebRouter::serve` on a request built
//! here (with `httptest.NewRequest`'s `Host: example.com`); `httptest.NewServer` is the web
//! launcher's own server on a loopback listener, and the WebSocket client is a raw RFC 6455
//! handshake over a `TcpStream` (gadk-launcher has no WebSocket client dependency).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use gadk_agent::{AgentConfig, SingleLoader};
use gadk_server::mux::{Handler, Request, Response, handler};
use gadk_server::rest::DEFAULT_MAX_PAYLOAD_SIZE;
use gadk_session::InMemorySessionService;
use http::header::{HOST, LOCATION, ORIGIN};
use http::{HeaderMap, HeaderValue, Method, StatusCode};
use pretty_assertions::assert_eq;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::golog;
use crate::test_support::strings;
use crate::web::api::{self, cors_with_args, register_api_routes, rewrite_redirects};
use crate::web::{self, WebRouter, WebSublauncher, axum_handler, build_base_router, testing};

/// Go `stubREST`: a gorilla router with `StrictSlash(true)` shaped like the real REST router;
/// `ran` records which handler served the request.
struct StubRest {
    handler: Handler,
    ran: Arc<Mutex<String>>,
}

impl StubRest {
    fn ran(&self) -> String {
        self.ran.lock().unwrap().clone()
    }
}

fn new_stub_rest() -> StubRest {
    let ran = Arc::new(Mutex::new(String::new()));
    let mut router = WebRouter::new(true);
    let mark = |name: &'static str| {
        let ran = Arc::clone(&ran);
        handler(move |_req: Request| {
            let ran = Arc::clone(&ran);
            async move {
                *ran.lock().unwrap() = name.to_owned();
                let mut resp = Response::new(Body::from(name));
                resp.headers_mut()
                    .insert("X-Handler", HeaderValue::from_static(name));
                resp
            }
        })
    };
    router.handle(&[Method::GET, Method::HEAD], "/list-apps", mark("ListApps"));
    router.handle(
        &[Method::GET],
        "/apps/{app}/users/{user}/sessions",
        mark("ListSessions"),
    );
    router.handle(
        &[Method::POST],
        "/apps/{app}/users/{user}/sessions",
        mark("CreateSession"),
    );
    router.handle(
        &[Method::PATCH],
        "/apps/{app}/users/{user}/sessions/{session}",
        mark("UpdateSession"),
    );
    router.handle(
        &[Method::DELETE],
        "/apps/{app}/users/{user}/sessions/{session}",
        mark("DeleteSession"),
    );
    router.handle(&[Method::PUT], "/tests/{test}", mark("CreateTest"));
    router.handle(&[Method::OPTIONS], "/list-apps", mark("OptionsListApps"));
    StubRest {
        handler: router_handler(router),
        ran,
    }
}

/// A router served as a handler (Go passes a `*mux.Router` as an `http.Handler`).
fn router_handler(router: WebRouter) -> Handler {
    handler(move |req: Request| {
        let router = router.clone();
        async move { router.serve(req).await }
    })
}

/// Go `httptest.NewRequest(method, target, nil)`.
fn new_request(method: &str, target: &str) -> Request {
    new_request_with_body(method, target, Body::empty())
}

fn new_request_with_body(method: &str, target: &str, body: Body) -> Request {
    http::Request::builder()
        .method(Method::from_bytes(method.as_bytes()).unwrap())
        .uri(target)
        .header(HOST, "example.com")
        .body(body)
        .unwrap()
}

/// A served response: status, headers and body.
struct Recorded {
    code: StatusCode,
    headers: HeaderMap,
    body: String,
}

impl Recorded {
    fn header(&self, name: &str) -> String {
        self.headers
            .get(name)
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default()
    }
}

async fn record(resp: Response) -> Recorded {
    let (parts, body) = resp.into_parts();
    let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
    Recorded {
        code: parts.status,
        headers: parts.headers,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

async fn serve(router: &WebRouter, req: Request) -> Recorded {
    record(router.serve(req).await).await
}

/// Go `callAPI`: registers `inner` under `prefix` on a fresh StrictSlash router and serves one
/// request.
async fn call_api(prefix: &str, inner: Handler, method: &str, target: &str) -> Recorded {
    let mut router = WebRouter::new(true);
    register_api_routes(&mut router, prefix, inner);
    serve(&router, new_request(method, target)).await
}

/// Go `agent.New(agent.Config{Name: "HelloWorldAgent", Run: <yields nothing>})`.
fn hello_world_agent() -> Arc<dyn gadk_agent::Agent> {
    gadk_agent::new_agent(AgentConfig {
        name: "HelloWorldAgent".to_owned(),
        description: String::new(),
        sub_agents: Vec::new(),
        before_agent_callbacks: Vec::new(),
        after_agent_callbacks: Vec::new(),
        run: Some(Arc::new(|_ic| Box::pin(futures::stream::empty()))),
    })
    .unwrap()
}

/// Go `&launcher.Config{AgentLoader: agent.NewSingleLoader(agnt), SessionService:
/// session.InMemoryService()}`.
fn launcher_config() -> Config {
    let mut config = Config::new(Arc::new(SingleLoader::new(hello_world_agent())));
    config.session_service = Some(Arc::new(InMemorySessionService::new()));
    let (log, _) = golog::capture();
    config.log = log;
    config
}

/// A parsed api launcher with its routes set up on a fresh StrictSlash router.
fn setup(args: &[&str], config: &mut Config) -> WebRouter {
    let mut l = api::new();
    WebSublauncher::parse(&mut l, strings(args)).unwrap();
    let mut router = WebRouter::new(true);
    l.setup_subrouters(&mut router, config).unwrap();
    router
}

#[tokio::test]
async fn test_cors_headers() {
    struct Case {
        name: &'static str,
        addr: &'static str,
        allowed: &'static [&'static str],
        request_origin: &'static str,
        want_origin: &'static str,
        want_vary: &'static str,
    }
    let tests = [
        Case {
            name: "default flag value gains a scheme",
            addr: "localhost:8080",
            allowed: &[],
            request_origin: "",
            want_origin: "http://localhost:8080",
            want_vary: "Origin",
        },
        Case {
            name: "explicit origin kept",
            addr: "https://ui.example.com",
            allowed: &[],
            request_origin: "",
            want_origin: "https://ui.example.com",
            want_vary: "Origin",
        },
        Case {
            name: "wildcard sets no Vary",
            addr: "*",
            allowed: &[],
            request_origin: "",
            want_origin: "*",
            want_vary: "",
        },
        Case {
            name: "unlisted request Origin is not reflected",
            addr: "localhost:8080",
            allowed: &["https://ui.example.com"],
            request_origin: "http://evil.example.com",
            want_origin: "http://localhost:8080",
            want_vary: "Origin",
        },
        Case {
            name: "allowed request Origin is reflected",
            addr: "localhost:8080",
            allowed: &["https://ui.example.com"],
            request_origin: "https://ui.example.com",
            want_origin: "https://ui.example.com",
            want_vary: "Origin",
        },
        Case {
            name: "allowed bare host is read as http",
            addr: "localhost:8080",
            allowed: &["ui.example.com:3000"],
            request_origin: "http://ui.example.com:3000",
            want_origin: "http://ui.example.com:3000",
            want_vary: "Origin",
        },
        Case {
            name: "star in the allowed origins allows every origin",
            addr: "localhost:8080",
            allowed: &["*"],
            request_origin: "https://any.example.com",
            want_origin: "*",
            want_vary: "",
        },
        Case {
            // A reflected origin would need Vary, and "*" already admits it.
            name: "star web UI address wins over an allowed origin",
            addr: "*",
            allowed: &["https://ui.example.com"],
            request_origin: "https://ui.example.com",
            want_origin: "*",
            want_vary: "",
        },
        Case {
            name: "empty web UI address still reflects an allowed origin",
            addr: "",
            allowed: &["https://ui.example.com"],
            request_origin: "https://ui.example.com",
            want_origin: "https://ui.example.com",
            want_vary: "Origin",
        },
        Case {
            // With no web UI origin there is no fallback header, but an allowed origin would get
            // one, so the response still varies.
            name: "empty web UI address still varies for an unlisted origin",
            addr: "",
            allowed: &["https://ui.example.com"],
            request_origin: "http://evil.example.com",
            want_origin: "",
            want_vary: "Origin",
        },
    ];
    for tt in tests {
        let called = Arc::new(Mutex::new(false));
        let next = {
            let called = Arc::clone(&called);
            handler(move |_req: Request| {
                let called = Arc::clone(&called);
                async move {
                    *called.lock().unwrap() = true;
                    Response::new(Body::empty())
                }
            })
        };
        let h = cors_with_args(tt.addr, &strings(tt.allowed))(next);
        let mut req = new_request("GET", "/list-apps");
        if !tt.request_origin.is_empty() {
            req.headers_mut()
                .insert(ORIGIN, HeaderValue::from_static(tt.request_origin));
        }
        let rec = record(h(req).await).await;

        assert!(
            *called.lock().unwrap(),
            "{}: next handler was not called for a non-preflight request",
            tt.name
        );
        let got_origin = rec.header("Access-Control-Allow-Origin");
        assert_eq!(
            got_origin, tt.want_origin,
            "{}: Access-Control-Allow-Origin",
            tt.name
        );
        assert!(
            got_origin.is_empty() || got_origin == "*" || got_origin.contains("://"),
            "{}: Access-Control-Allow-Origin = {got_origin:?}, want a value carrying a scheme",
            tt.name
        );
        assert_eq!(rec.header("Vary"), tt.want_vary, "{}: Vary", tt.name);
        let allow = rec.header("Access-Control-Allow-Methods");
        for m in ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"] {
            assert!(
                allow.contains(m),
                "{}: Access-Control-Allow-Methods = {allow:?}, want it to list {m}",
                tt.name
            );
        }
    }
}

#[tokio::test]
async fn test_cors_preflight_stops_at_middleware() {
    let called = Arc::new(Mutex::new(false));
    let next = {
        let called = Arc::clone(&called);
        handler(move |_req: Request| {
            let called = Arc::clone(&called);
            async move {
                *called.lock().unwrap() = true;
                let mut resp = Response::new(Body::empty());
                *resp.status_mut() = StatusCode::IM_A_TEAPOT;
                resp
            }
        })
    };
    let h = cors_with_args("localhost:8080", &[])(next);
    let rec = record(h(new_request("OPTIONS", "/list-apps")).await).await;

    assert!(
        !*called.lock().unwrap(),
        "OPTIONS preflight reached the next handler, want it answered by the middleware"
    );
    assert_eq!(rec.code, StatusCode::OK);
    assert_eq!(
        rec.header("Access-Control-Allow-Origin"),
        "http://localhost:8080"
    );
}

/// Covers the blanket 405 the outer mount used to return for PUT, PATCH and HEAD.
#[tokio::test]
async fn test_register_api_routes_forwards_every_method() {
    let prefixes = [
        ("default prefix", "/api"),
        ("custom prefix", "/adk"),
        ("empty prefix", ""),
    ];
    let tests = [
        ("GET", "GET", "/list-apps", "ListApps"),
        ("HEAD", "HEAD", "/list-apps", "ListApps"),
        ("POST", "POST", "/apps/a/users/u/sessions", "CreateSession"),
        ("PUT", "PUT", "/tests/t1", "CreateTest"),
        (
            "PATCH",
            "PATCH",
            "/apps/a/users/u/sessions/s1",
            "UpdateSession",
        ),
        (
            "DELETE",
            "DELETE",
            "/apps/a/users/u/sessions/s1",
            "DeleteSession",
        ),
        ("OPTIONS", "OPTIONS", "/list-apps", "OptionsListApps"),
    ];
    for (pname, prefix) in prefixes {
        for (name, method, path, want_ran) in tests {
            let stub = new_stub_rest();
            let rec = call_api(
                prefix,
                Arc::clone(&stub.handler),
                method,
                &format!("{prefix}{path}"),
            )
            .await;
            assert_eq!(
                rec.code,
                StatusCode::OK,
                "{pname}/{name}: status (405 means the outer mount rejected {method})"
            );
            assert_eq!(stub.ran(), want_ran, "{pname}/{name}: handler that ran");
        }
    }
}

/// Covers the guaranteed 404, and the POST that silently ran a GET handler, on any URL with a
/// trailing slash.
#[tokio::test]
async fn test_register_api_routes_trailing_slash() {
    let tests = [
        (
            "GET list-apps",
            "/api",
            "GET",
            "/api/list-apps/",
            "ListApps",
        ),
        (
            "GET list-apps custom prefix",
            "/adk",
            "GET",
            "/adk/list-apps/",
            "ListApps",
        ),
        (
            "GET list-apps empty prefix",
            "",
            "GET",
            "/list-apps/",
            "ListApps",
        ),
        (
            "GET sessions",
            "/api",
            "GET",
            "/api/apps/a/users/u/sessions/",
            "ListSessions",
        ),
        (
            "POST sessions",
            "/api",
            "POST",
            "/api/apps/a/users/u/sessions/",
            "CreateSession",
        ),
        (
            "POST sessions custom prefix",
            "/adk",
            "POST",
            "/adk/apps/a/users/u/sessions/",
            "CreateSession",
        ),
        ("PUT test", "/api", "PUT", "/api/tests/t1/", "CreateTest"),
        (
            "no trailing slash still works",
            "/api",
            "GET",
            "/api/list-apps",
            "ListApps",
        ),
    ];
    for (name, prefix, method, target, want_ran) in tests {
        let stub = new_stub_rest();
        let rec = call_api(prefix, Arc::clone(&stub.handler), method, target).await;
        assert_eq!(
            stub.ran(),
            want_ran,
            "{name}: handler that ran (status {}, Location {:?})",
            rec.code,
            rec.header("Location")
        );
        assert_eq!(rec.code, StatusCode::OK, "{name}: status");
        assert_eq!(rec.header("X-Handler"), want_ran, "{name}: X-Handler");
    }
}

/// The POST must not be answered by the GET handler registered on the same path.
#[tokio::test]
async fn test_trailing_slash_post_never_becomes_get() {
    let stub = new_stub_rest();
    let rec = call_api(
        "/api",
        Arc::clone(&stub.handler),
        "POST",
        "/api/apps/a/users/u/sessions/",
    )
    .await;

    assert_ne!(
        stub.ran(),
        "ListSessions",
        "POST with a trailing slash ran ListSessions, the GET handler on the same path"
    );
    assert!(
        !(rec.code == StatusCode::MOVED_PERMANENTLY || rec.code == StatusCode::FOUND),
        "POST answered with {}, which a client re-issues as GET; want 200, 308 or 404",
        rec.code
    );
    assert_eq!(stub.ran(), "CreateSession");
}

/// A handler that redirects to `target` with `code`. Go calls `http.Redirect(w, r, target,
/// code)`, whose `Location` for an absolute path is the path itself; the HTML body Go adds is not
/// what these tests check.
fn redirector(target: &'static str, code: StatusCode) -> Handler {
    handler(move |_req: Request| async move {
        let mut resp = Response::new(Body::empty());
        *resp.status_mut() = code;
        resp.headers_mut()
            .insert(LOCATION, HeaderValue::from_static(target));
        resp
    })
}

/// Covers a redirect written below the mount, which is served a path with the prefix already
/// stripped.
// parity: adapted: the redirecting handler sets Location and the status itself (gadk-launcher's Go http.Redirect is private to the router); Go's Location for an absolute path is the same
#[tokio::test]
async fn test_redirect_location_keeps_prefix() {
    const TARGET: &str = "/apps/a/users/u/sessions";
    let tests = [
        (
            "GET keeps 301",
            "/api",
            "GET",
            StatusCode::MOVED_PERMANENTLY,
            StatusCode::MOVED_PERMANENTLY,
        ),
        (
            "GET custom prefix",
            "/adk",
            "GET",
            StatusCode::MOVED_PERMANENTLY,
            StatusCode::MOVED_PERMANENTLY,
        ),
        (
            "POST is promoted to 308",
            "/api",
            "POST",
            StatusCode::MOVED_PERMANENTLY,
            StatusCode::PERMANENT_REDIRECT,
        ),
        (
            "POST 302 is promoted to 307",
            "/api",
            "POST",
            StatusCode::FOUND,
            StatusCode::TEMPORARY_REDIRECT,
        ),
    ];
    for (name, prefix, method, code, want_status) in tests {
        let rec = call_api(
            prefix,
            redirector(TARGET, code),
            method,
            &format!("{prefix}/redirect-me"),
        )
        .await;
        assert_eq!(rec.code, want_status, "{name}: status");
        let got = rec.header("Location");
        assert_eq!(got, format!("{prefix}{TARGET}"), "{name}: Location");
        assert!(
            got.starts_with(&format!("{prefix}/")),
            "{name}: Location = {got:?}, want it to start with {prefix:?}/"
        );
    }
}

/// Follows the rewritten Location the way a client would, and checks it resolves to the POST
/// handler rather than the GET one.
// parity: adapted: the redirecting handler sets Location and the status itself, as in test_redirect_location_keeps_prefix
#[tokio::test]
async fn test_redirect_follow_up_reaches_intended_handler() {
    let first = call_api(
        "/api",
        redirector("/apps/a/users/u/sessions", StatusCode::MOVED_PERMANENTLY),
        "POST",
        "/api/redirect-me",
    )
    .await;
    assert_eq!(first.code, StatusCode::PERMANENT_REDIRECT);

    let stub = new_stub_rest();
    // 308 tells the client to repeat the request unchanged, so the follow-up is still a POST.
    let second = call_api(
        "/api",
        Arc::clone(&stub.handler),
        "POST",
        &first.header("Location"),
    )
    .await;
    assert_eq!(second.code, StatusCode::OK, "follow-up status");
    assert_eq!(stub.ran(), "CreateSession", "follow-up ran");
}

/// Covers `PathPrefix("/api")` capturing `/apifoo`.
#[tokio::test]
async fn test_prefix_is_segment_aware() {
    let tests = [
        ("prefix root", "/api", true, false),
        ("under prefix", "/api/list-apps", true, false),
        ("prefix as a word start", "/apifoo", false, true),
        (
            "prefix as a word start with path",
            "/apifoo/bar",
            false,
            true,
        ),
    ];
    for (name, target, want_api, want_outside) in tests {
        let stub = new_stub_rest();
        let api_hit = Arc::new(Mutex::new(false));
        let outside_hit = Arc::new(Mutex::new(false));
        let mut router = WebRouter::new(true);
        {
            let api_hit = Arc::clone(&api_hit);
            let stub = Arc::clone(&stub.handler);
            register_api_routes(
                &mut router,
                "/api",
                Arc::new(move |req: Request| {
                    *api_hit.lock().unwrap() = true;
                    stub(req)
                }),
            );
        }
        {
            let outside_hit = Arc::clone(&outside_hit);
            router.handle_prefix(
                &[],
                "/apifoo",
                handler(move |_req: Request| {
                    let outside_hit = Arc::clone(&outside_hit);
                    async move {
                        *outside_hit.lock().unwrap() = true;
                        Response::new(Body::empty())
                    }
                }),
            );
        }

        let rec = serve(&router, new_request("GET", target)).await;

        assert_eq!(
            *api_hit.lock().unwrap(),
            want_api,
            "{name}: API mount hit (status {}, Location {:?})",
            rec.code,
            rec.header("Location")
        );
        assert_eq!(
            *outside_hit.lock().unwrap(),
            want_outside,
            "{name}: route outside the mount hit"
        );
    }
}

/// Drives the real registration path with the real ADK REST server.
// parity: adapted: the agent's Run yields nothing (Go's yields one "hi" event); no request here runs the agent
#[tokio::test]
async fn test_setup_subrouters_serves_real_restapi() {
    let mut config = launcher_config();
    let router = setup(
        &["-webui_address", "localhost:8080", "-path_prefix", "/api"],
        &mut config,
    );

    let tests = [
        ("GET list-apps", "GET", "/api/list-apps"),
        ("GET list-apps trailing slash", "GET", "/api/list-apps/"),
        ("HEAD list-apps", "HEAD", "/api/list-apps"),
        ("HEAD list-apps trailing slash", "HEAD", "/api/list-apps/"),
    ];
    for (name, method, target) in tests {
        let rec = serve(&router, new_request(method, target)).await;
        assert_eq!(
            rec.code,
            StatusCode::OK,
            "{name}: status (body {:?}, Location {:?})",
            rec.body,
            rec.header("Location")
        );
        assert_eq!(
            rec.header("Access-Control-Allow-Origin"),
            "http://localhost:8080",
            "{name}"
        );
    }

    // A verb the path does not serve must be refused by the REST router, which knows what that
    // path supports (the Allow header is the tell), rather than by the mount.
    for method in ["PATCH", "PUT", "DELETE", "PURGE"] {
        let rec = serve(&router, new_request(method, "/api/list-apps")).await;
        assert_eq!(
            rec.code,
            StatusCode::METHOD_NOT_ALLOWED,
            "unsupported verb {method}: status (404 or a 405 without Allow means the mount answered, not the REST router)"
        );
        assert_eq!(
            rec.header("Allow"),
            "GET, HEAD",
            "unsupported verb {method}"
        );
    }
}

/// Go `websocket.DefaultDialer.Dial`: an RFC 6455 handshake on a new connection; the status line
/// and the connection.
async fn ws_dial(addr: std::net::SocketAddr, target: &str) -> (String, TcpStream) {
    let mut conn = TcpStream::connect(addr).await.unwrap();
    let request = format!(
        "GET {target} HTTP/1.1\r\nHost: {addr}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
    );
    conn.write_all(request.as_bytes()).await.unwrap();
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        let n = conn.read(&mut byte).await.unwrap();
        assert_eq!(n, 1, "connection closed during the handshake: {head:?}");
        head.push(byte[0]);
    }
    let head = String::from_utf8(head).unwrap();
    let status = head.lines().next().unwrap().to_owned();
    (status, conn)
}

/// Reads one unfragmented server frame: `(opcode, payload)`.
async fn ws_read_frame(conn: &mut TcpStream) -> std::io::Result<(u8, Vec<u8>)> {
    let mut hdr = [0u8; 2];
    conn.read_exact(&mut hdr).await?;
    let len = match hdr[1] & 0x7f {
        126 => {
            let mut b = [0u8; 2];
            conn.read_exact(&mut b).await?;
            usize::from(u16::from_be_bytes(b))
        }
        127 => {
            let mut b = [0u8; 8];
            conn.read_exact(&mut b).await?;
            usize::try_from(u64::from_be_bytes(b)).unwrap()
        }
        n => usize::from(n),
    };
    let mut payload = vec![0u8; len];
    conn.read_exact(&mut payload).await?;
    Ok((hdr[0] & 0x0f, payload))
}

/// Serves `router` on a loopback listener (Go `httptest.NewServer`) until the token is cancelled.
async fn start_server(router: WebRouter) -> (std::net::SocketAddr, CancellationToken) {
    let listener = testing::listen("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let cancel = CancellationToken::new();
    let (log, _) = golog::capture();
    let opts = web::new(Vec::new()).serve_options();
    tokio::spawn(testing::serve(listener, router, opts, cancel.clone(), log));
    (addr, cancel)
}

/// Covers `/run_live`, which takes over the connection instead of writing a response: the
/// prefixed mount must carry the upgrade.
// parity: adapted: the echo endpoint is axum's WebSocket upgrade (Go: gorilla/websocket) and the client a raw handshake
#[tokio::test]
async fn test_web_socket_upgrade_through_mount() {
    for prefix in ["/api", ""] {
        let echo = axum_handler(axum::Router::new().fallback(
            |ws: axum::extract::ws::WebSocketUpgrade| async move {
                ws.on_upgrade(|mut socket| async move {
                    let _ = socket
                        .send(axum::extract::ws::Message::Text("live".into()))
                        .await;
                })
            },
        ));
        let mut router = WebRouter::new(true);
        let mut inner = WebRouter::new(true);
        inner.handle(&[], "/run_live", echo);
        register_api_routes(&mut router, prefix, router_handler(inner));

        let (addr, cancel) = start_server(router).await;
        let (status, mut conn) = ws_dial(addr, &format!("{prefix}/run_live")).await;
        assert!(
            status.starts_with("HTTP/1.1 101"),
            "prefix={prefix}: Dial status {status:?}, want a successful upgrade"
        );
        // A deadline, so an upgrade that never sends a frame fails this test instead of hanging.
        let (opcode, msg) = tokio::time::timeout(Duration::from_secs(10), ws_read_frame(&mut conn))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(opcode, 1, "prefix={prefix}: a text frame");
        assert_eq!(String::from_utf8(msg).unwrap(), "live", "prefix={prefix}");
        cancel.cancel();
    }
}

/// Covers the startup hint naming what the debug flag costs.
#[test]
fn test_user_message_names_what_the_debug_flag_costs() {
    for (name, args, want_hint) in [
        ("flag absent", &[][..], true),
        ("flag set", &["--include_debug_api"][..], false),
    ] {
        let mut l = api::new();
        WebSublauncher::parse(&mut l, strings(args)).unwrap();

        let mut lines = Vec::new();
        l.user_message("http://localhost:8080", &mut |line: &str| {
            lines.push(line.to_owned())
        });
        let out = lines.join("\n");

        assert_eq!(
            out.contains("-include_debug_api"),
            want_hint,
            "{name}: hint present; output:\n{out}"
        );
        if want_hint {
            assert!(
                out.contains("Traces"),
                "{name}: hint does not name the panels it costs; output:\n{out}"
            );
        }
    }
}

/// The mount must not change what the handler underneath learns about taking over the
/// connection: on a request that cannot be upgraded the handler sees the same failure as
/// unmounted.
// parity: adapted: Rust has no http.Hijacker; the connection takeover is hyper's upgrade extension, which the redirect rewriter passes through unchanged (a request without one fails to upgrade under the mount exactly as without it)
#[tokio::test]
async fn test_hijack_reports_not_supported() {
    let seen = Arc::new(Mutex::new(None));
    let upgrading = {
        let seen = Arc::clone(&seen);
        Arc::new(move |mut req: Request| {
            let seen = Arc::clone(&seen);
            Box::pin(async move {
                let upgraded = hyper::upgrade::on(&mut req).await;
                *seen.lock().unwrap() = Some(upgraded.map(|_| ()).map_err(|e| e.to_string()));
                Response::new(Body::empty())
            }) as futures::future::BoxFuture<'static, Response>
        }) as Handler
    };

    let _ = Arc::clone(&upgrading)(new_request("GET", "/run_live")).await;
    let unmounted = seen.lock().unwrap().take().unwrap();
    let _ = rewrite_redirects("/api", upgrading)(new_request("GET", "/run_live")).await;
    let mounted = seen.lock().unwrap().take().unwrap();

    assert!(
        mounted.is_err(),
        "upgrade = {mounted:?}, want a failure on a request that cannot be upgraded"
    );
    assert_eq!(mounted, unmounted);
}

/// Drives the actual REST server's `/run_live` through the mount: the handshake must complete.
// parity: adapted: raw WebSocket handshake client (Go: gorilla/websocket's dialer); the read after it may see a close frame, other frames or EOF, as in Go
#[tokio::test]
async fn test_run_live_upgrades_through_the_real_mount() {
    let mut config = launcher_config();
    let router = setup(&["-path_prefix", "/api"], &mut config);

    let (addr, cancel) = start_server(router).await;
    let (status, mut conn) = ws_dial(
        addr,
        "/api/run_live?app_name=HelloWorldAgent&user_id=u&session_id=does-not-exist",
    )
    .await;
    assert!(
        status.starts_with("HTTP/1.1 101"),
        "handshake status {status:?}, want 101"
    );

    // The server closes because the session is missing; the deadline keeps a silent server from
    // hanging the test.
    let _ = tokio::time::timeout(Duration::from_secs(10), ws_read_frame(&mut conn)).await;
    cancel.cancel();
}

/// Pins that `-webui_address` reaches the REST server's origin check, not only the CORS header.
#[tokio::test]
async fn test_setup_subrouters_passes_web_ui_origin_to_rest_server() {
    const DEV_UI: &str = "http://localhost:4200";
    let mut config = launcher_config();
    let router = setup(
        &["-webui_address", DEV_UI, "-path_prefix", "/api"],
        &mut config,
    );

    for (name, origin, want_status) in [
        ("configured web UI origin", DEV_UI, StatusCode::OK),
        ("any other origin", "http://evil.com", StatusCode::FORBIDDEN),
    ] {
        let mut req = new_request("GET", "/api/list-apps");
        req.headers_mut()
            .insert(ORIGIN, HeaderValue::from_static(origin));
        let rec = serve(&router, req).await;
        assert_eq!(
            rec.code, want_status,
            "{name}: status (body {:?})",
            rec.body
        );
    }
}

/// Pins that the launcher's bind address reaches the REST server's Host check.
#[tokio::test]
async fn test_setup_subrouters_passes_bind_host_to_rest_server() {
    for (name, bind_host, host, want_status) in [
        (
            "rebound host on a loopback bind",
            "127.0.0.1",
            "evil.com:8080",
            StatusCode::FORBIDDEN,
        ),
        (
            "loopback host on a loopback bind",
            "127.0.0.1",
            "localhost:8080",
            StatusCode::OK,
        ),
        (
            // A routable bind is an operator exposing the server on purpose.
            "rebound host on an all-interfaces bind",
            "0.0.0.0",
            "evil.com:8080",
            StatusCode::OK,
        ),
    ] {
        let mut config = launcher_config();
        config.bind_host = bind_host.to_owned();
        let router = setup(&["-path_prefix", "/api"], &mut config);

        let mut req = new_request("GET", "/api/list-apps");
        req.headers_mut()
            .insert(HOST, HeaderValue::from_static(host));
        let rec = serve(&router, req).await;
        assert_eq!(
            rec.code, want_status,
            "{name}: status (body {:?})",
            rec.body
        );
    }
}

/// Verifies that `Config::max_payload_size` reaches the REST server, so its body limit can be
/// raised above the 10 MiB default.
// parity: adapted: Go builds an apiLauncher literal with an empty prefix and no flags parsed; here -path_prefix "" is parsed (same prefix, and the zero sse-write-timeout/trace_capacity of the literal select the same defaults the flags give); gadk's Config always carries an agent loader (Go's is nil)
#[tokio::test]
async fn test_api_server_honors_config_max_payload_size() {
    // 20 MiB - twice the adkrest default, clearly above the default limit.
    const CONFIGURED_MAX: i64 = 20 << 20;
    let mut config = launcher_config();
    config.max_payload_size = CONFIGURED_MAX;
    let router = setup(
        &["-webui_address", "localhost:8080", "-path_prefix", ""],
        &mut config,
    );

    let send_session_body = |padding: usize| {
        let router = router.clone();
        async move {
            let body = format!(r#"{{"state": {{"padding": "{}"}}}}"#, "a".repeat(padding));
            serve(
                &router,
                new_request_with_body("POST", "/apps/my-app/users/u1/sessions", Body::from(body)),
            )
            .await
        }
    };

    // A body between the 10 MiB default and the configured 20 MiB limit.
    let default_max = usize::try_from(DEFAULT_MAX_PAYLOAD_SIZE).unwrap();
    let raise_rec = send_session_body(default_max + 4096).await;
    assert_eq!(
        raise_rec.code,
        StatusCode::OK,
        "raised-limit request ({})",
        raise_rec.body
    );

    // The configured limit is still enforced above it.
    let over_rec = send_session_body(usize::try_from(CONFIGURED_MAX).unwrap() + 4096).await;
    assert_eq!(
        over_rec.code,
        StatusCode::BAD_REQUEST,
        "over-configured request ({})",
        over_rec.body
    );
    assert!(
        over_rec.body.contains("http: request body too large"),
        "over-configured request: got body {:?}",
        over_rec.body
    );
}

/// Pins the two directions the allowed origins move in: the launcher adds the web UI origin to
/// the server-wide list, and the REST server and its CORS headers honor the whole list.
#[tokio::test]
async fn test_setup_subrouters_origins() {
    struct Case {
        name: &'static str,
        configured: &'static [&'static str],
        origin: &'static str,
        want_status: StatusCode,
        want_cors: &'static str,
    }
    for tc in [
        Case {
            name: "the web UI origin is served",
            configured: &[],
            origin: "http://localhost:8080",
            want_status: StatusCode::OK,
            want_cors: "http://localhost:8080",
        },
        Case {
            name: "a server-wide origin is served and may read the response",
            configured: &["https://ui.example.com"],
            origin: "https://ui.example.com",
            want_status: StatusCode::OK,
            want_cors: "https://ui.example.com",
        },
        Case {
            name: "an unlisted origin is refused",
            configured: &["https://ui.example.com"],
            origin: "https://evil.example.com",
            want_status: StatusCode::FORBIDDEN,
            want_cors: "http://localhost:8080",
        },
    ] {
        let mut config = launcher_config();
        config.bind_host = "127.0.0.1".to_owned();
        config.allowed_origins = strings(tc.configured);
        let router = setup(
            &["-path_prefix", "/api", "-webui_address", "localhost:8080"],
            &mut config,
        );

        assert!(
            config
                .allowed_origins
                .contains(&"localhost:8080".to_owned()),
            "{}: AllowedOrigins = {:?}, want it to contain the -webui_address value",
            tc.name,
            config.allowed_origins
        );

        // A Host that differs from every Origin here, so that no request passes as same-origin.
        let mut req = new_request("GET", "/api/list-apps");
        req.headers_mut()
            .insert(HOST, HeaderValue::from_static("127.0.0.1:8080"));
        req.headers_mut()
            .insert(ORIGIN, HeaderValue::from_static(tc.origin));
        let rec = serve(&router, req).await;

        assert_eq!(
            rec.code, tc.want_status,
            "{}: GET /api/list-apps with Origin {:?}",
            tc.name, tc.origin
        );
        assert_eq!(
            rec.header("Access-Control-Allow-Origin"),
            tc.want_cors,
            "{}: Access-Control-Allow-Origin",
            tc.name
        );
    }
}

/// Pins that `SetupSubrouters` leaves the caller's own list of allowed origins alone.
// parity: adapted: Rust's Config owns its Vec, so there is no shared backing array to overwrite; the caller's list (cloned into the config) stays as it was and the config gains the web UI origin
#[tokio::test]
async fn test_setup_subrouters_does_not_overwrite_caller_allowed_origins() {
    let backing = strings(&["https://a.example", "https://untouched.example"]);
    let mut config = launcher_config();
    config.allowed_origins = backing[..1].to_vec();
    let mut l = api::new();
    WebSublauncher::parse(&mut l, Vec::new()).unwrap();
    let mut router = build_base_router(&config.log);
    l.setup_subrouters(&mut router, &mut config).unwrap();

    assert_eq!(
        backing,
        strings(&["https://a.example", "https://untouched.example"]),
        "caller backing array"
    );
    assert_eq!(
        config.allowed_origins,
        strings(&["https://a.example", "localhost:8080"])
    );
}
