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

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The gadk-server items the launcher reuses, through the public API only: [`gadk_server::mux`]
//! (gorilla/mux templates, `cleanPath` and its redirect) and the
//! [`max_bytes_middleware`], [`origin_guard_middleware`] and [`normalize_origin`] exports.
//! Rust-only tests (no ledger rows); the `normalize_origin` and guard cases are Go's
//! `internal/originguard` `TestNormalizeOrigin` and `TestMiddleware`.

use std::collections::BTreeMap;
use std::net::SocketAddr;

use axum::body::Body;
use axum::extract::ConnectInfo;
use gadk_server::mux::{
    Handler, Request, Response, Template, clean_path, clean_path_redirect, handler,
};
use gadk_server::rest::{
    DEFAULT_MAX_PAYLOAD_SIZE, LocalAddr, PathVars, max_bytes_middleware, normalize_origin,
    origin_guard_middleware,
};
use http::{StatusCode, Uri};

fn vars(pairs: &[(&str, &str)]) -> PathVars {
    PathVars(
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect::<BTreeMap<_, _>>(),
    )
}

async fn text(resp: Response) -> String {
    let b = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8(b.to_vec()).unwrap()
}

#[test]
fn template_whole_path_match() {
    let t = Template::parse("/apps/{app_name}/users/{user_id}/sessions");
    assert_eq!(
        t.matches(b"/apps/my app/users/u/sessions"),
        Some(vars(&[("app_name", "my app"), ("user_id", "u")]))
    );
    assert_eq!(t.matches(b"/apps/a/users/u/sessions/"), None);
    assert_eq!(t.matches(b"/apps/a/users/u/sessions/s"), None);
    assert_eq!(t.matches(b"/apps//users/u/sessions"), None);
    assert_eq!(
        Template::parse("/health").matches(b"/health"),
        Some(PathVars::default())
    );
    // Leftmost-first greedy submatches, like Go's `^/pair/([^/]+)-([^/]+)$`.
    assert_eq!(
        Template::parse("/pair/{a}-{b}").matches(b"/pair/x-y-z"),
        Some(vars(&[("a", "x-y"), ("b", "z")]))
    );
    // A `{name:regexp}` variable keeps its name.
    assert_eq!(
        Template::parse("/x/{id:[0-9]+}").matches(b"/x/12"),
        Some(vars(&[("id", "12")]))
    );
}

#[test]
fn template_prefix_match() {
    // gorilla `PathPrefix` is not segment-aware: `/api` matches `/apifoo`.
    let api = Template::parse("/api");
    assert_eq!(api.matches_prefix(b"/api"), Some((PathVars::default(), 4)));
    assert_eq!(
        api.matches_prefix(b"/api/run"),
        Some((PathVars::default(), 4))
    );
    assert_eq!(
        api.matches_prefix(b"/apifoo"),
        Some((PathVars::default(), 4))
    );
    assert_eq!(api.matches_prefix(b"/ap"), None);
    assert_eq!(api.matches_prefix(b"/other/api"), None);
    // The whole-path match of the same template is anchored.
    assert_eq!(api.matches(b"/api/run"), None);

    let slash = Template::parse("/ui/");
    assert_eq!(slash.matches_prefix(b"/ui"), None);
    assert_eq!(
        slash.matches_prefix(b"/ui/assets/x.js"),
        Some((PathVars::default(), 4))
    );

    // A variable takes the whole segment; later literals must follow it.
    let app = Template::parse("/apps/{app_name}");
    assert_eq!(
        app.matches_prefix(b"/apps/a1/trigger/pubsub"),
        Some((vars(&[("app_name", "a1")]), 8))
    );
    assert_eq!(app.matches_prefix(b"/apps/"), None);
    let trig = Template::parse("/apps/{app_name}/trigger");
    assert_eq!(
        trig.matches_prefix(b"/apps/a1/triggers/x"),
        Some((vars(&[("app_name", "a1")]), 16))
    );
    assert_eq!(trig.matches_prefix(b"/apps/a1/other"), None);
    // Backtracking as in the whole-path match.
    assert_eq!(
        Template::parse("/pair/{a}-{b}").matches_prefix(b"/pair/x-y-z/rest"),
        Some((vars(&[("a", "x-y"), ("b", "z")]), 11))
    );
    // The empty template matches every path with length 0.
    assert_eq!(
        Template::parse("").matches_prefix(b"/anything"),
        Some((PathVars::default(), 0))
    );
}

#[test]
fn clean_path_is_gorilla_clean_path() {
    for (p, want) in [
        ("", "/"),
        ("a", "/a"),
        ("/", "/"),
        ("//", "/"),
        ("/a/b/", "/a/b/"),
        ("/a//b", "/a/b"),
        ("/a/./b/", "/a/b/"),
        ("/a/../../b", "/b"),
        ("/a/b/..", "/a"),
        ("/a/..", "/"),
    ] {
        assert_eq!(
            String::from_utf8(clean_path(p.as_bytes())).unwrap(),
            want,
            "cleanPath({p:?})"
        );
    }
    // Non-UTF-8 bytes pass through.
    assert_eq!(clean_path(b"/a/\xff/../b"), b"/a/b".to_vec());
}

#[tokio::test]
async fn clean_path_redirect_keeps_the_query() {
    for (target, clean, want) in [
        ("/x/../run?a=1&b=2", "/run", "/run?a=1&b=2"),
        ("//version", "/version", "/version"),
        ("/a%20b/./c", "/a b/c", "/a%20b/c"),
        ("http://h:8080//x?q", "/x", "http://h:8080/x?q"),
    ] {
        let uri: Uri = target.parse().unwrap();
        let resp = clean_path_redirect(&uri, clean.as_bytes());
        assert_eq!(resp.status(), StatusCode::MOVED_PERMANENTLY, "{target}");
        assert_eq!(resp.headers()["location"], want, "{target}");
        assert_eq!(text(resp).await, "", "{target}");
    }
}

/// Go `TestMaxBytesMiddlewarePinsTheDefaultLimit`'s handler shape: read the whole body; a failed
/// read answers 400 with the error's text in `http.Error` framing, success echoes the length.
fn read_all() -> Handler {
    handler(|req: Request| async move {
        match axum::body::to_bytes(req.into_body(), usize::MAX).await {
            Ok(b) => Response::new(Body::from(b.len().to_string())),
            Err(e) => {
                let mut r = Response::new(Body::from(format!("{e}\n")));
                *r.status_mut() = StatusCode::BAD_REQUEST;
                r
            }
        }
    })
}

fn post(body: Vec<u8>) -> Request {
    http::Request::post("/").body(Body::from(body)).unwrap()
}

#[tokio::test]
async fn max_bytes_middleware_limits_the_body() {
    let h = max_bytes_middleware(8)(read_all());
    let ok = h(post(b"12345678".to_vec())).await;
    assert_eq!(ok.status(), StatusCode::OK);
    assert!(ok.headers().get("connection").is_none());
    assert_eq!(text(ok).await, "8");

    let over = h(post(b"123456789".to_vec())).await;
    assert_eq!(over.status(), StatusCode::BAD_REQUEST);
    assert_eq!(over.headers()["connection"], "close");
    assert_eq!(text(over).await, "http: request body too large\n");

    // A handler that does not read the body is served normally.
    let ignore = max_bytes_middleware(1)(handler(|_req: Request| async {
        Response::new(Body::from("ok"))
    }));
    let resp = ignore(post(b"too long".to_vec())).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers().get("connection").is_none());
}

#[tokio::test]
async fn max_bytes_middleware_zero_or_negative_is_the_default() {
    let limit = usize::try_from(DEFAULT_MAX_PAYLOAD_SIZE).unwrap();
    for configured in [0, -1] {
        let h = max_bytes_middleware(configured)(read_all());
        let at = h(post(vec![b'a'; limit])).await;
        assert_eq!(at.status(), StatusCode::OK, "{configured}: at the limit");
        let over = h(post(vec![b'a'; limit + 1])).await;
        assert_eq!(
            over.status(),
            StatusCode::BAD_REQUEST,
            "{configured}: one over"
        );
    }
}

/// Go `on(localAddr, host)`: a GET of `/list-apps` with `Host: host`, accepted at
/// `localAddr:8080`.
fn on(local: [u8; 4], host: &str, origin: Option<&str>) -> Request {
    let mut b = http::Request::get("/list-apps").header("host", host);
    if let Some(o) = origin {
        b = b.header("origin", o);
    }
    let mut r = b.body(Body::empty()).unwrap();
    r.extensions_mut()
        .insert(ConnectInfo(LocalAddr(SocketAddr::from((local, 8080)))));
    r
}

fn served() -> Handler {
    handler(|_req: Request| async { Response::new(Body::from("served")) })
}

/// Go `TestMiddleware` (`loopbackBind`), plus the Origin refusal and the allow list.
#[tokio::test]
async fn origin_guard_middleware_refuses_and_passes() {
    let guard = origin_guard_middleware("127.0.0.1", &[])(served());
    let ok = guard(on([127, 0, 0, 1], "localhost:8080", None)).await;
    assert_eq!(ok.status(), StatusCode::OK);
    assert_eq!(text(ok).await, "served");

    let rebound = guard(on([127, 0, 0, 1], "evil.com:8080", None)).await;
    assert_eq!(rebound.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        rebound.headers()["content-type"],
        "text/plain; charset=utf-8"
    );
    assert_eq!(text(rebound).await, "Forbidden: host not allowed\n");

    let cross = guard(on(
        [127, 0, 0, 1],
        "localhost:8080",
        Some("http://localhost:4200"),
    ))
    .await;
    assert_eq!(cross.status(), StatusCode::FORBIDDEN);
    assert_eq!(text(cross).await, "Forbidden: origin not allowed\n");

    // A configured origin (bare host:port read as http) passes.
    let allowed = origin_guard_middleware("127.0.0.1", &["localhost:4200".to_owned()])(served());
    let resp = allowed(on(
        [127, 0, 0, 1],
        "localhost:8080",
        Some("http://localhost:4200"),
    ))
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // `*` turns both checks off.
    let all = origin_guard_middleware("127.0.0.1", &["*".to_owned()])(served());
    let resp = all(on([127, 0, 0, 1], "evil.com:8080", Some("http://evil.com"))).await;
    assert_eq!(resp.status(), StatusCode::OK);
}

/// Go `TestNormalizeOrigin`.
#[test]
fn normalize_origin_matches_go() {
    for (addr, want) in [
        ("", ""),
        ("*", "*"),
        ("  localhost:8080  ", "http://localhost:8080"),
        ("localhost", "http://localhost"),
        ("http://localhost:8080/", "http://localhost:8080"),
        ("https://ui.example.com/app?x=1", "https://ui.example.com"),
        ("://nonsense", "://nonsense"),
    ] {
        assert_eq!(normalize_origin(addr), want, "NormalizeOrigin({addr:?})");
    }
}
