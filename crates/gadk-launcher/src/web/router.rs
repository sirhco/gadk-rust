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

//! The gorilla/mux v1.8.1 router as the web launcher configures it (plan D6).
//!
//! `mux.go` `Router.ServeHTTP`/`Router.Match`, `route.go` `Route.Match` and `regexp.go`
//! `setMatch` as the launcher uses them: `Handle(path).Methods(...)` and
//! `PathPrefix(prefix).Methods(...)` routes in registration order, the first route whose path and
//! method match wins; a route whose path matched but whose method did not makes an unmatched
//! request a 405 (empty body, no `Allow`), anything else unmatched is `http.NotFound`. Before
//! matching, a path that is not clean is redirected (301) to its clean form. Under
//! `StrictSlash(true)` a `Path` route also matches the path with the other trailing-slash form and
//! answers it with `http.Redirect` (301). `Use` middleware wraps a matched route's handler only
//! (the strict-slash redirect included), never the 404, 405 or clean-path 301.
//!
//! What Go's `net/http` server does before and after any handler is also done here, so a request
//! served through [`WebRouter::serve`] gets Go's server answers: `OPTIONS *` (200,
//! `Content-Length: 0`), a request target whose escapes do not decode (400), and a HEAD response
//! without its body.

use std::convert::Infallible;
use std::sync::{Arc, RwLock};

use axum::body::Body;
use gadk_platform::{go_path_clean, go_url};
use gadk_server::mux::{
    Handler, PathVars, Request, Response, Template, clean_path, clean_path_redirect,
};
use http::header::{CONNECTION, CONTENT_LENGTH, CONTENT_TYPE, LOCATION, X_CONTENT_TYPE_OPTIONS};
use http::{HeaderValue, Method, StatusCode, Uri, Version};
use hyper::body::Body as _;
use tower::ServiceExt;

/// Go `router.Use(mw)`: wraps a matched route's handler.
pub type Middleware = std::sync::Arc<dyn Fn(Handler) -> Handler + Send + Sync>;

/// How a route matches the request path.
enum PathMatcher {
    /// gorilla `Path(tpl)`: the whole path. With `strict_slash`, `template` is the pattern minus
    /// its trailing slash, the path matches with or without a trailing slash, and a match whose
    /// slash differs from the pattern's is redirected.
    Path {
        template: Template,
        strict_slash: bool,
        pattern_ends_with_slash: bool,
    },
    /// gorilla `PathPrefix(tpl)`: a prefix of the path (not segment-aware; `StrictSlash` has no
    /// effect).
    Prefix(Template),
}

/// One registered route.
struct Route {
    /// gorilla `Methods(...)`; empty = any method.
    methods: Vec<Method>,
    matcher: PathMatcher,
    handler: Handler,
}

/// The routing table a router and its clones share.
#[derive(Default)]
struct Inner {
    routes: Vec<Route>,
    middlewares: Vec<Middleware>,
}

/// gorilla/mux v1.8.1 router as the launcher configures it (D6).
///
/// Clones share one routing table, as Go code shares a `*mux.Router`: a route or middleware added
/// through any clone is seen by all of them (Go's web launcher adds the origin guard after the
/// sub-launchers ran, and gorilla builds a route's middleware chain when the route matches, so the
/// guard covers routes registered before it).
#[derive(Clone)]
pub struct WebRouter {
    strict_slash: bool,
    inner: Arc<RwLock<Inner>>,
}

/// The outcome of matching a request against the routes (gorilla `Router.Match`).
enum Matched {
    /// A route matched: its handler wrapped in the middleware, and its path variables.
    Route(Handler, PathVars),
    /// Some route's path matched but none of those routes accepts the method.
    MethodMismatch,
    /// No route's path matched.
    NotFound,
}

impl WebRouter {
    /// gorilla `NewRouter().StrictSlash(strict_slash)`.
    pub fn new(strict_slash: bool) -> WebRouter {
        WebRouter {
            strict_slash,
            inner: Arc::new(RwLock::new(Inner::default())),
        }
    }

    /// gorilla `Handle(path).Methods(...)`; empty `methods` = any method.
    pub fn handle(&mut self, methods: &[http::Method], path: &str, h: Handler) {
        let matcher = if self.strict_slash {
            let pattern_ends_with_slash = path.ends_with('/');
            let trimmed = if pattern_ends_with_slash {
                &path[..path.len() - 1]
            } else {
                path
            };
            PathMatcher::Path {
                template: Template::parse(trimmed),
                strict_slash: true,
                pattern_ends_with_slash,
            }
        } else {
            PathMatcher::Path {
                template: Template::parse(path),
                strict_slash: false,
                pattern_ends_with_slash: path.ends_with('/'),
            }
        };
        self.push(methods, matcher, h);
    }

    /// gorilla `PathPrefix(prefix).Methods(...).Handler(h)` (not segment-aware).
    pub fn handle_prefix(&mut self, methods: &[http::Method], prefix: &str, h: Handler) {
        self.push(methods, PathMatcher::Prefix(Template::parse(prefix)), h);
    }

    /// gorilla `Use`.
    pub fn use_middleware(&mut self, mw: Middleware) {
        write(&self.inner).middlewares.push(mw);
    }

    /// Serves one request (gorilla `Router.ServeHTTP`, inside Go's `net/http` server).
    pub async fn serve(&self, req: Request) -> Response {
        let head = req.method() == Method::HEAD;
        let resp = self.dispatch(req).await;
        if head { strip_body(resp) } else { resp }
    }

    /// An axum-compatible service (for `axum::serve`/hyper and tests).
    pub fn into_service(self) -> tower::util::BoxCloneSyncService<Request, Response, Infallible> {
        tower::util::BoxCloneSyncService::new(tower::service_fn(move |req: Request| {
            let router = self.clone();
            async move { Ok::<_, Infallible>(router.serve(req).await) }
        }))
    }

    fn push(&mut self, methods: &[Method], matcher: PathMatcher, handler: Handler) {
        write(&self.inner).routes.push(Route {
            methods: methods.to_vec(),
            matcher,
            handler,
        });
    }

    /// `Router.ServeHTTP` after Go's server decoded the path.
    async fn dispatch(&self, mut req: Request) -> Response {
        if req.method() == Method::OPTIONS && req.uri().path() == "*" {
            return options_star();
        }
        let Ok(path) = go_url::unescape_path(req.uri().path()) else {
            return bad_request();
        };
        // Router.ServeHTTP: clean the path first (skipClean is false).
        let clean = clean_path(&path);
        if clean != path {
            return clean_path_redirect(&go_request_uri(&req), &clean);
        }
        match self.find(&req, &path) {
            Matched::Route(handler, vars) => {
                req.extensions_mut().insert(vars);
                handler(req).await
            }
            Matched::MethodMismatch => method_not_allowed(),
            Matched::NotFound => not_found(),
        }
    }

    /// gorilla `Router.Match`: the first route matching path and method, with the middleware
    /// chain built around its handler (or around the strict-slash redirect that replaces it).
    fn find(&self, req: &Request, path: &[u8]) -> Matched {
        let inner = read(&self.inner);
        let mut method_mismatch = false;
        for route in &inner.routes {
            let Some(vars) = route.path_vars(path) else {
                continue;
            };
            if !route.methods.is_empty() && !route.methods.contains(req.method()) {
                method_mismatch = true;
                continue;
            }
            let mut handler = match route.strict_slash_redirect(req, path) {
                Some(redirect) => redirect,
                None => Arc::clone(&route.handler),
            };
            for mw in inner.middlewares.iter().rev() {
                handler = mw(handler);
            }
            return Matched::Route(handler, vars);
        }
        if method_mismatch {
            Matched::MethodMismatch
        } else {
            Matched::NotFound
        }
    }
}

impl Route {
    /// The path variables when the route's path template matches `path`.
    fn path_vars(&self, path: &[u8]) -> Option<PathVars> {
        match &self.matcher {
            PathMatcher::Path {
                template,
                strict_slash,
                ..
            } => template.matches(path).or_else(|| {
                // `…[/]?$`: the `[^/]+` variables never take a `/`, so the optional slash is the
                // path's last byte.
                if *strict_slash {
                    path.strip_suffix(b"/").and_then(|p| template.matches(p))
                } else {
                    None
                }
            }),
            PathMatcher::Prefix(template) => template.matches_prefix(path).map(|(vars, _)| vars),
        }
    }

    /// gorilla `setMatch`'s strict-slash check: a redirect handler when the path's trailing slash
    /// differs from the pattern's.
    fn strict_slash_redirect(&self, req: &Request, path: &[u8]) -> Option<Handler> {
        let PathMatcher::Path {
            strict_slash: true,
            pattern_ends_with_slash,
            ..
        } = &self.matcher
        else {
            return None;
        };
        let path_ends_with_slash = path.ends_with(b"/");
        if path_ends_with_slash == *pattern_ends_with_slash {
            return None;
        }
        // `u, _ := url.Parse(req.URL.String())`, then the slash trimmed or added.
        let mut u = go_url::parse(&go_request_uri(req).to_string()).unwrap_or_default();
        if path_ends_with_slash {
            u.path.pop();
        } else {
            u.path.push(b'/');
        }
        let target = u.to_go_string();
        Some(Arc::new(move |req: Request| {
            let resp = redirect(&req, &target, StatusCode::MOVED_PERMANENTLY);
            Box::pin(async move { resp })
        }))
    }
}

/// Go's `r.RequestURI` / `r.URL`: an HTTP/2 request has only its `:path` (hyper adds `:scheme`
/// and `:authority`), an HTTP/1 request its request target as sent (absolute-form included).
pub(crate) fn go_request_uri(req: &Request) -> Uri {
    match req.uri().path_and_query() {
        Some(pq) if req.version() == Version::HTTP_2 => Uri::from(pq.clone()),
        _ => req.uri().clone(),
    }
}

/// Wraps an `axum::Router` as a `Handler` (the request is passed unchanged).
pub fn axum_handler(router: axum::Router) -> Handler {
    Arc::new(move |req: Request| {
        let router = router.clone();
        Box::pin(async move {
            match router.oneshot(req).await {
                Ok(resp) => resp,
                Err(never) => match never {},
            }
        })
    })
}

fn read(inner: &RwLock<Inner>) -> std::sync::RwLockReadGuard<'_, Inner> {
    inner.read().unwrap_or_else(|e| e.into_inner())
}

fn write(inner: &RwLock<Inner>) -> std::sync::RwLockWriteGuard<'_, Inner> {
    inner.write().unwrap_or_else(|e| e.into_inner())
}

/// Go `net/http.Redirect(w, r, url, code)`.
fn redirect(req: &Request, url: &str, code: StatusCode) -> Response {
    let mut url = url.to_owned();
    if let Ok(u) = go_url::parse(&url)
        && u.scheme.is_empty()
        && u.host.is_empty()
    {
        // No scheme or host: make a relative path absolute, clean it, keep a trailing slash.
        let old_path = go_url::unescape_path(req.uri().path())
            .map(|p| String::from_utf8_lossy(&p).into_owned())
            .unwrap_or_default();
        let old_path = if old_path.is_empty() {
            "/".to_owned()
        } else {
            old_path
        };
        if !url.starts_with('/') {
            let old_dir = &old_path[..old_path.rfind('/').map_or(0, |i| i + 1)];
            url = format!("{old_dir}{url}");
        }
        let (path, query) = match url.find('?') {
            Some(i) => (url[..i].to_owned(), url[i..].to_owned()),
            None => (url.clone(), String::new()),
        };
        let trailing = path.ends_with('/');
        let mut cleaned = go_path_clean(&path);
        if trailing && !cleaned.ends_with('/') {
            cleaned.push('/');
        }
        url = cleaned + &query;
    }
    let mut resp = Response::new(Body::empty());
    *resp.status_mut() = code;
    if let Ok(v) = HeaderValue::from_str(&hex_escape_non_ascii(&url)) {
        resp.headers_mut().insert(LOCATION, v);
    }
    let method = req.method();
    if method == Method::GET || method == Method::HEAD {
        resp.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        );
    }
    if method == Method::GET {
        // `fmt.Fprintln(w, body)` where body already ends in a newline.
        let text = code.canonical_reason().unwrap_or_default();
        *resp.body_mut() = Body::from(format!("<a href=\"{}\">{text}</a>.\n\n", html_escape(&url)));
    }
    resp
}

/// Go `hexEscapeNonASCII`: bytes outside ASCII as `%` and lowercase hex.
fn hex_escape_non_ascii(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b >= 0x80 {
            out.push_str(&format!("%{b:x}"));
        } else {
            out.push(char::from(b));
        }
    }
    out
}

/// Go `htmlEscape` (`net/http`'s `htmlReplacer`).
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&#34;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// gorilla `methodNotAllowed`: 405 with nothing but the status.
fn method_not_allowed() -> Response {
    let mut resp = Response::new(Body::empty());
    *resp.status_mut() = StatusCode::METHOD_NOT_ALLOWED;
    resp
}

/// Go `net/http.Error`: `msg` and a newline as `text/plain; charset=utf-8`, with
/// `X-Content-Type-Options: nosniff`.
fn http_error(status: StatusCode, msg: &str) -> Response {
    let mut resp = Response::new(Body::from(format!("{msg}\n")));
    *resp.status_mut() = status;
    let h = resp.headers_mut();
    h.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    h.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    resp
}

/// Go `net/http.NotFound`: 404 `404 page not found`.
fn not_found() -> Response {
    http_error(StatusCode::NOT_FOUND, "404 page not found")
}

/// Go's server answer to a request target whose escapes do not decode: `400 Bad Request` as
/// plain text, and the connection closed.
fn bad_request() -> Response {
    let mut resp = Response::new(Body::from("400 Bad Request"));
    *resp.status_mut() = StatusCode::BAD_REQUEST;
    let h = resp.headers_mut();
    h.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    h.insert(CONNECTION, HeaderValue::from_static("close"));
    resp
}

/// Go `globalOptionsHandler`: `OPTIONS *` answers 200 with `Content-Length: 0`.
fn options_star() -> Response {
    let mut resp = Response::new(Body::empty());
    resp.headers_mut()
        .insert(CONTENT_LENGTH, HeaderValue::from_static("0"));
    resp
}

/// A HEAD response as Go's server writes it: the headers of the GET response, the would-be body
/// length as `Content-Length` when the handler wrote a body of known length (Go sets it only when
/// something was written), and no body.
fn strip_body(mut resp: Response) -> Response {
    if !resp.headers().contains_key(CONTENT_LENGTH)
        && let Some(len) = resp.body().size_hint().exact()
        && len > 0
    {
        resp.headers_mut()
            .insert(CONTENT_LENGTH, HeaderValue::from(len));
    }
    *resp.body_mut() = Body::empty();
    resp
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Mutex;

    use gadk_server::mux::handler;
    use pretty_assertions::assert_eq;

    use super::*;

    /// A handler answering `name` and its path variables.
    fn named(name: &'static str) -> Handler {
        handler(move |req: Request| async move {
            let vars = req
                .extensions()
                .get::<PathVars>()
                .map(|v| format!("{:?}", v.0))
                .unwrap_or_default();
            Response::new(Body::from(format!("{name} {vars}")))
        })
    }

    fn req(method: &str, target: &str) -> Request {
        http::Request::builder()
            .method(method)
            .uri(target)
            .body(Body::empty())
            .unwrap()
    }

    async fn parts(resp: Response) -> (u16, Vec<(String, String)>, String) {
        let status = resp.status().as_u16();
        let headers = resp
            .headers()
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap().to_owned()))
            .collect();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, headers, String::from_utf8(body.to_vec()).unwrap())
    }

    fn h(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    /// A middleware recording `tag` on every call.
    fn recording(tag: &'static str, seen: &Arc<Mutex<Vec<String>>>) -> Middleware {
        let seen = Arc::clone(seen);
        Arc::new(move |next: Handler| {
            let seen = Arc::clone(&seen);
            handler(move |req: Request| {
                let next = Arc::clone(&next);
                seen.lock().unwrap().push(tag.to_owned());
                async move { next(req).await }
            })
        })
    }

    #[tokio::test]
    async fn first_registered_route_wins_and_vars_are_set() {
        let mut r = WebRouter::new(true);
        r.handle(&[Method::GET], "/apps/{app}", named("first"));
        r.handle(&[Method::GET], "/apps/{other}", named("second"));
        let (status, _, body) = parts(r.serve(req("GET", "/apps/a%20b")).await).await;
        assert_eq!((status, body.as_str()), (200, "first {\"app\": \"a b\"}"));
    }

    #[tokio::test]
    async fn method_mismatch_is_bare_405_and_unknown_path_is_not_found() {
        let mut r = WebRouter::new(true);
        r.handle(&[Method::GET, Method::HEAD], "/health", named("h"));
        assert_eq!(
            parts(r.serve(req("DELETE", "/health")).await).await,
            (405, vec![], String::new())
        );
        assert_eq!(
            parts(r.serve(req("GET", "/nope")).await).await,
            (
                404,
                h(&[
                    ("content-type", "text/plain; charset=utf-8"),
                    ("x-content-type-options", "nosniff")
                ]),
                "404 page not found\n".to_owned()
            )
        );
        // A later route with the same path and the right method still wins.
        r.handle(&[Method::DELETE], "/health", named("d"));
        assert_eq!(parts(r.serve(req("DELETE", "/health")).await).await.0, 200);
    }

    #[tokio::test]
    async fn strict_slash_redirects_both_ways_keeping_the_query() {
        let mut r = WebRouter::new(true);
        r.handle(&[], "/health", named("h"));
        r.handle(&[], "/dir/", named("d"));
        assert_eq!(
            parts(r.serve(req("GET", "/health/?x=1")).await).await,
            (
                301,
                h(&[
                    ("location", "/health?x=1"),
                    ("content-type", "text/html; charset=utf-8")
                ]),
                "<a href=\"/health?x=1\">Moved Permanently</a>.\n\n".to_owned()
            )
        );
        assert_eq!(
            parts(r.serve(req("POST", "/dir")).await).await,
            (301, h(&[("location", "/dir/")]), String::new())
        );
        // HEAD: the Content-Type, no body and (nothing written) no Content-Length.
        assert_eq!(
            parts(r.serve(req("HEAD", "/dir")).await).await,
            (
                301,
                h(&[
                    ("location", "/dir/"),
                    ("content-type", "text/html; charset=utf-8")
                ]),
                String::new()
            )
        );
        assert_eq!(parts(r.serve(req("GET", "/dir/")).await).await.0, 200);
        // Without StrictSlash the other form does not match.
        let mut plain = WebRouter::new(false);
        plain.handle(&[], "/health", named("h"));
        assert_eq!(
            parts(plain.serve(req("GET", "/health/")).await).await.0,
            404
        );
    }

    #[tokio::test]
    async fn prefix_routes_are_not_segment_aware_and_ignore_strict_slash() {
        let mut r = WebRouter::new(true);
        r.handle_prefix(&[Method::GET], "/ui/", named("ui"));
        r.handle_prefix(&[], "/api", named("api"));
        assert_eq!(parts(r.serve(req("GET", "/apifoo")).await).await.0, 200);
        assert_eq!(parts(r.serve(req("GET", "/ui")).await).await.0, 404);
        assert_eq!(parts(r.serve(req("POST", "/ui/x")).await).await.0, 405);
        assert_eq!(
            parts(r.serve(req("GET", "/ui/x")).await).await.2,
            "ui {}".to_owned()
        );
    }

    #[tokio::test]
    async fn unclean_path_redirects_before_matching_and_without_middleware() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut r = WebRouter::new(true);
        r.use_middleware(recording("mw", &seen));
        r.handle(&[], "/health", named("h"));
        assert_eq!(
            parts(r.serve(req("GET", "/api/../health?x=1")).await).await,
            (301, h(&[("location", "/health?x=1")]), String::new())
        );
        assert_eq!(
            parts(r.serve(req("POST", "//health")).await).await,
            (301, h(&[("location", "/health")]), String::new())
        );
        r.serve(req("GET", "/nope")).await;
        r.serve(req("POST", "/nope")).await;
        assert!(seen.lock().unwrap().is_empty(), "no middleware ran");
    }

    #[tokio::test]
    async fn middleware_wraps_matched_routes_in_use_order_including_redirects() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut r = WebRouter::new(true);
        r.use_middleware(recording("outer", &seen));
        r.handle(&[Method::GET], "/health", named("h"));
        // Added after the route, through a clone: still covers it (shared table).
        let mut clone = r.clone();
        clone.use_middleware(recording("inner", &seen));
        r.serve(req("GET", "/health")).await;
        assert_eq!(*seen.lock().unwrap(), vec!["outer", "inner"]);
        seen.lock().unwrap().clear();
        assert_eq!(parts(r.serve(req("GET", "/health/")).await).await.0, 301);
        assert_eq!(*seen.lock().unwrap(), vec!["outer", "inner"]);
        seen.lock().unwrap().clear();
        r.serve(req("DELETE", "/health")).await;
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn head_keeps_headers_and_length_but_not_the_body() {
        let mut r = WebRouter::new(true);
        r.handle(&[Method::GET, Method::HEAD], "/x", named("x"));
        let (status, headers, body) = parts(r.serve(req("HEAD", "/x")).await).await;
        assert_eq!((status, body.as_str()), (200, ""));
        assert_eq!(headers, h(&[("content-length", "4")]));
        let (status, headers, body) = parts(r.serve(req("HEAD", "/nope")).await).await;
        assert_eq!((status, body.as_str()), (404, ""));
        assert!(headers.contains(&("content-length".to_owned(), "19".to_owned())));
        assert_eq!(
            parts(r.serve(req("HEAD", "/x/")).await).await.1,
            h(&[
                ("location", "/x"),
                ("content-type", "text/html; charset=utf-8")
            ])
        );
    }

    #[tokio::test]
    async fn server_answers_come_before_routing() {
        let r = WebRouter::new(true);
        assert_eq!(
            parts(r.serve(req("OPTIONS", "*")).await).await,
            (200, h(&[("content-length", "0")]), String::new())
        );
        assert_eq!(
            parts(r.serve(req("GET", "/a%zz")).await).await,
            (
                400,
                h(&[
                    ("content-type", "text/plain; charset=utf-8"),
                    ("connection", "close")
                ]),
                "400 Bad Request".to_owned()
            )
        );
    }

    #[tokio::test]
    async fn axum_handler_and_into_service_pass_requests_through() {
        let app = axum::Router::new().route("/a/b", axum::routing::get(|| async { "axum" }));
        let mut r = WebRouter::new(true);
        r.handle_prefix(&[], "/a", axum_handler(app));
        let svc = r.into_service();
        let resp = svc.oneshot(req("GET", "/a/b")).await.unwrap();
        assert_eq!(parts(resp).await.2, "axum");
    }

    #[test]
    fn redirect_helpers_match_go() {
        assert_eq!(hex_escape_non_ascii("/a\u{e9}"), "/a%c3%a9");
        assert_eq!(html_escape("<a&'\">"), "&lt;a&amp;&#39;&#34;&gt;");
        let r = req("GET", "/dir/sub");
        let resp = redirect(&r, "../x?y=1", StatusCode::FOUND);
        assert_eq!(resp.headers()[LOCATION], "/x?y=1");
    }
}
