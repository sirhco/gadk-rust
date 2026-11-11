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

//! The route table (port of ADK-Go `server/adkrest/internal/routers/routers.go` on a
//! gorilla/mux v1.8.1 router): each controller's [`Route`]s matched with the [`crate::mux`]
//! path templates, the per-route body limit and authentication, and Go's fallback for unmatched
//! requests (plan decision D4).

use std::convert::Infallible;
use std::sync::Arc;

use axum::body::Body;
use gadk_platform::go_url;
use http::header::ALLOW;
use http::uri::PathAndQuery;
use http::{HeaderValue, Method, StatusCode, Uri};
use tower::util::BoxCloneSyncService;
use tower::{Layer, ServiceExt};

use super::middleware::{catch_panic, with_max_bytes};
use crate::authn::{self, Authenticator};
pub use crate::mux::{Handler, PathVars, Request, Response, handler};
use crate::mux::{
    Template, bad_request, clean_path, clean_path_redirect, go_request_uri, not_found,
    options_star, redirect, strip_body,
};

/// Port of Go `routers.Route`.
pub struct Route {
    /// The route's name (Go `Name`).
    pub name: &'static str,
    /// The methods it serves; GET routes also serve HEAD (added by [`RouteTable::new`]).
    pub methods: Vec<http::Method>,
    /// A gorilla/mux path template (`{name}` matches one non-empty segment).
    pub pattern: String,
    /// Exempt from authentication (Go `Public`).
    pub public: bool,
    /// Registered while the router had gorilla's `StrictSlash(true)` (Go registers `/health`
    /// before `SetupSubRouters` turns it off): the path with the other trailing-slash form also
    /// matches, and is answered with a 301 to the route's own form instead of the handler.
    pub strict_slash: bool,
    /// The handler.
    pub handler: Handler,
}

/// A non-public [`Route`].
pub(crate) fn route(
    name: &'static str,
    methods: &[http::Method],
    pattern: impl Into<String>,
    handler: Handler,
) -> Route {
    Route {
        name,
        methods: methods.to_vec(),
        pattern: pattern.into(),
        public: false,
        strict_slash: false,
        handler,
    }
}

/// A [`Handler`] calling `$controller.$method(req)` on a clone of the `Arc`ed controller.
macro_rules! controller_handler {
    ($controller:expr, $method:ident) => {{
        let c = ::std::sync::Arc::clone(&$controller);
        $crate::rest::routing::handler(move |req| {
            let c = ::std::sync::Arc::clone(&c);
            async move { c.$method(req).await }
        })
    }};
}
pub(crate) use controller_handler;

/// Go `req.URL.Query().Get(key)` on the request target's `query`: the first value of `key`,
/// form-decoded, or `""`. Like Go's `url.ParseQuery`, a `&`-separated pair containing `;` is
/// dropped.
pub(crate) fn query_get(query: Option<&str>, key: &str) -> String {
    query
        .unwrap_or_default()
        .split('&')
        .filter(|pair| !pair.contains(';'))
        .flat_map(|pair| url::form_urlencoded::parse(pair.as_bytes()))
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default()
}

/// Go `routers.DevPrefix`: the path prefix of the developer-only endpoints.
pub const DEV_PREFIX: &str = "/dev/apps/{app_name}";

/// Go `probeMethods`: the methods an unserved request is re-matched against, in the order they
/// appear in an `Allow` header.
const PROBE_METHODS: [Method; 7] = [
    Method::GET,
    Method::HEAD,
    Method::POST,
    Method::PUT,
    Method::PATCH,
    Method::DELETE,
    Method::OPTIONS,
];

/// A matched route's stack: authentication (unless public), the panic catcher, the handler.
type RouteService = BoxCloneSyncService<Request, Response, Infallible>;

/// A registered route with its compiled template and service stack.
struct Entry {
    name: &'static str,
    methods: Vec<Method>,
    pattern: String,
    public: bool,
    /// The pattern's template, minus its trailing slash for a strict-slash route.
    template: Template,
    /// See [`Route::strict_slash`].
    strict_slash: bool,
    service: RouteService,
}

/// A registered route as [`RouteTable::walk`] reports it (Go `mux.Router.Walk`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteInfo {
    /// The route's name.
    pub name: &'static str,
    /// The methods it serves, HEAD included wherever GET is.
    pub methods: Vec<Method>,
    /// Its gorilla/mux path template.
    pub pattern: String,
    /// Exempt from authentication.
    pub public: bool,
}

/// The routes of a server in registration order, with their matcher, body limit and
/// authentication (Go `SetupSubRouters` on a `mux.Router` with `MaxBytesMiddleware`).
pub struct RouteTable {
    routes: Vec<Entry>,
    max_payload_size: i64,
}

impl RouteTable {
    /// Builds the table: HEAD added to GET routes (Go `withHead`), every non-public route wrapped
    /// in [`authn::middleware`] when `authenticator` is set, and `max_payload_size` (zero or
    /// negative: [`DEFAULT_MAX_PAYLOAD_SIZE`]) applied to every matched route.
    ///
    /// [`DEFAULT_MAX_PAYLOAD_SIZE`]: super::DEFAULT_MAX_PAYLOAD_SIZE
    pub fn new(
        routes: Vec<Route>,
        authenticator: Option<Arc<dyn Authenticator>>,
        max_payload_size: i64,
    ) -> Self {
        let authn_layer = authenticator.map(authn::middleware);
        let routes = routes
            .into_iter()
            .map(|r| {
                let h = r.handler;
                let inner = catch_panic(tower::service_fn(move |req: Request| {
                    let h = Arc::clone(&h);
                    async move { Ok::<_, Infallible>(h(req).await) }
                }));
                let service = match &authn_layer {
                    Some(layer) if !r.public => BoxCloneSyncService::new(layer.layer(inner)),
                    _ => BoxCloneSyncService::new(inner),
                };
                let template = if r.strict_slash {
                    Template::parse(r.pattern.strip_suffix('/').unwrap_or(&r.pattern))
                } else {
                    Template::parse(&r.pattern)
                };
                Entry {
                    name: r.name,
                    methods: with_head(r.methods),
                    template,
                    strict_slash: r.strict_slash,
                    pattern: r.pattern,
                    public: r.public,
                    service,
                }
            })
            .collect();
        RouteTable {
            routes,
            max_payload_size,
        }
    }

    /// Every route in registration order (Go `mux.Router.Walk`).
    pub fn walk(&self) -> Vec<RouteInfo> {
        self.routes
            .iter()
            .map(|e| RouteInfo {
                name: e.name,
                methods: e.methods.clone(),
                pattern: e.pattern.clone(),
                public: e.public,
            })
            .collect()
    }

    /// Serves `req`: the first matching route (path variables as [`PathVars`]) runs the body
    /// limit, then authentication unless public, then its handler; an unmatched request gets Go's
    /// fallback (trailing slash re-dispatch, OPTIONS 204, 405 with `Allow`, 404). A HEAD response
    /// keeps its headers (and gains the would-be `Content-Length`) but loses its body, as Go's
    /// `net/http` answers HEAD.
    pub async fn serve(&self, req: Request) -> Response {
        let head = req.method() == Method::HEAD;
        let resp = self.dispatch(req).await;
        if head { strip_body(resp) } else { resp }
    }

    async fn dispatch(&self, req: Request) -> Response {
        let path = match decoded_path(&req) {
            Ok(path) => path,
            Err(answer) => return answer.response(),
        };
        let raw = req.uri().path().to_owned();
        // mux matches the decoded path and first redirects one that is not clean.
        let clean = clean_path(&path);
        if clean != path {
            return clean_path_redirect(&go_request_uri(&req), &clean);
        }
        if let Some((entry, vars)) = self.find(req.method(), &path) {
            return self.call(entry, vars, req).await;
        }
        // Go newFallbackHandler. A trailing slash is normalised first and the request
        // re-dispatched with its method and body; the trimmed path has no trailing slash, so
        // this happens at most once.
        let (mut req, mut path) = (req, path);
        if let Some((uri, trimmed)) = without_trailing_slash(req.uri(), &raw, &path) {
            *req.uri_mut() = uri;
            path = trimmed;
            if let Some((entry, vars)) = self.find(req.method(), &path) {
                return self.call(entry, vars, req).await;
            }
        }
        let mut allowed: Vec<&str> = PROBE_METHODS
            .iter()
            .filter(|m| self.find(m, &path).is_some())
            .map(Method::as_str)
            .collect();
        if allowed.is_empty() {
            return not_found();
        }
        let status = if req.method() == Method::OPTIONS {
            // No route declares OPTIONS; a resource that exists still describes itself.
            allowed.push(Method::OPTIONS.as_str());
            StatusCode::NO_CONTENT
        } else {
            StatusCode::METHOD_NOT_ALLOWED
        };
        let mut resp = Response::new(Body::empty());
        *resp.status_mut() = status;
        if let Ok(v) = HeaderValue::from_str(&allowed.join(", ")) {
            resp.headers_mut().insert(ALLOW, v);
        }
        resp
    }

    /// The first route (registration order) serving `method` on `path`, with its variables.
    fn find(&self, method: &Method, path: &[u8]) -> Option<(&Entry, PathVars)> {
        self.routes
            .iter()
            .filter(|e| e.methods.contains(method))
            .find_map(|e| e.path_vars(path).map(|vars| (e, vars)))
    }

    /// Runs a matched route: body limit (mux middleware), then its service stack, or for a
    /// strict-slash route reached with the other trailing-slash form, gorilla's redirect in place
    /// of the stack (so without authentication, as gorilla replaces the route's handler).
    async fn call(&self, entry: &Entry, vars: PathVars, mut req: Request) -> Response {
        if let Some(target) = entry.strict_slash_target(&req) {
            return with_max_bytes(self.max_payload_size, req, |req| async move {
                redirect(&req, &target, StatusCode::MOVED_PERMANENTLY)
            })
            .await;
        }
        req.extensions_mut().insert(vars);
        let service = entry.service.clone();
        with_max_bytes(self.max_payload_size, req, |req| async move {
            match service.oneshot(req).await {
                Ok(resp) => resp,
                Err(never) => match never {},
            }
        })
        .await
    }
}

impl Entry {
    /// The path variables when the route's path matches `path` (gorilla `…[/]?$` under
    /// `StrictSlash(true)`: the `[^/]+` variables never take a `/`, so the optional slash is the
    /// path's last byte).
    fn path_vars(&self, path: &[u8]) -> Option<PathVars> {
        self.template.matches(path).or_else(|| {
            if self.strict_slash {
                path.strip_suffix(b"/")
                    .and_then(|p| self.template.matches(p))
            } else {
                None
            }
        })
    }

    /// gorilla `setMatch`'s strict-slash check: when the decoded path's trailing slash differs
    /// from the pattern's, the redirect target, Go `URL.String` of `url.Parse(req.URL.String())`
    /// with the slash trimmed or added.
    fn strict_slash_target(&self, req: &Request) -> Option<String> {
        if !self.strict_slash {
            return None;
        }
        let path = go_url::unescape_path(req.uri().path()).ok()?;
        let path_ends_with_slash = path.ends_with(b"/");
        if path_ends_with_slash == self.pattern.ends_with('/') {
            return None;
        }
        let mut u = go_url::parse(&go_request_uri(req).to_string()).unwrap_or_default();
        if path_ends_with_slash {
            u.path.pop();
        } else {
            u.path.push(b'/');
        }
        Some(u.to_go_string())
    }
}

impl std::fmt::Debug for RouteTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RouteTable")
            .field("routes", &self.walk())
            .field("max_payload_size", &self.max_payload_size)
            .finish()
    }
}

/// Go `withHead`: HEAD added wherever a route serves GET.
fn with_head(mut methods: Vec<Method>) -> Vec<Method> {
    if !methods.contains(&Method::HEAD) && methods.contains(&Method::GET) {
        methods.push(Method::HEAD);
    }
    methods
}

/// Go `withoutTrailingSlash`: the request target with the trailing slashes of its path removed,
/// and the trimmed decoded path; `None` when there is no trailing slash or nothing would be left.
/// Slashes are counted on the escaped path (Go `EscapedPath`), so an encoded `%2F` is left alone.
fn without_trailing_slash(uri: &Uri, raw: &str, path: &[u8]) -> Option<(Uri, Vec<u8>)> {
    let escaped = if go_url::valid_encoded_path(raw) {
        raw.to_owned()
    } else {
        go_url::escape_path(path)
    };
    let n = escaped.len() - escaped.trim_end_matches('/').len();
    if n == 0 || n == escaped.len() || n > path.len() {
        return None;
    }
    let trimmed_path = path[..path.len() - n].to_vec();
    let trimmed_raw = if escaped == raw {
        raw[..raw.len() - n].to_owned()
    } else {
        go_url::escape_path(&trimmed_path)
    };
    let target = match uri.query() {
        Some(q) => format!("{trimmed_raw}?{q}"),
        None => trimmed_raw,
    };
    let mut parts = uri.clone().into_parts();
    parts.path_and_query = Some(PathAndQuery::try_from(target).ok()?);
    Some((Uri::from_parts(parts).ok()?, trimmed_path))
}

/// The answers Go's `net/http` server gives before any handler runs (so before the origin guard
/// too): `OPTIONS *` (`globalOptionsHandler`) and a request target whose escapes do not decode
/// (`400 Bad Request`). `Ok` is the decoded path of any other request.
pub(crate) fn decoded_path(req: &Request) -> Result<Vec<u8>, ServerAnswer> {
    if req.method() == Method::OPTIONS && req.uri().path() == "*" {
        return Err(ServerAnswer::OptionsStar);
    }
    go_url::unescape_path(req.uri().path()).map_err(|_| ServerAnswer::BadRequest)
}

/// An answer of Go's `net/http` server itself (see [`decoded_path`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ServerAnswer {
    /// `OPTIONS *`: 200 with `Content-Length: 0`.
    OptionsStar,
    /// An undecodable request target: `400 Bad Request`, connection closed.
    BadRequest,
}

impl ServerAnswer {
    /// The response Go's server writes.
    pub(crate) fn response(self) -> Response {
        match self {
            ServerAnswer::OptionsStar => options_star(),
            ServerAnswer::BadRequest => bad_request(),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::net::SocketAddr;

    use axum::extract::ConnectInfo;
    use axum::response::IntoResponse;
    use gadk_session::InMemorySessionService;
    use http::header::CONTENT_TYPE;
    use serde_json::Value;

    use super::*;
    use crate::authn::Caller;
    use crate::rest::controllers::{
        AppsApiController, RuntimeApiController, RuntimeApiControllerConfig, SessionsApiController,
        apps::apps_routes, runtime::runtime_routes, sessions::sessions_routes,
    };
    use crate::rest::http_error::http_error;
    use crate::rest::middleware::is_max_bytes_error;
    use crate::rest::originguard::{self, LocalAddr, Policy};

    /// A handler echoing `<method> <uri> <vars> <body>` and the authenticated caller's ID.
    fn echo() -> Handler {
        handler(|req: Request| async move {
            let (parts, body) = req.into_parts();
            let vars = parts
                .extensions
                .get::<PathVars>()
                .cloned()
                .unwrap_or_default();
            let caller = parts
                .extensions
                .get::<Caller>()
                .map(|c| c.user_id.clone())
                .unwrap_or_else(|| "-".to_owned());
            let body = axum::body::to_bytes(body, usize::MAX).await.unwrap();
            format!(
                "{} {} {:?} {} {}",
                parts.method,
                parts.uri,
                vars.0,
                caller,
                String::from_utf8_lossy(&body)
            )
            .into_response()
        })
    }

    fn public(mut r: Route) -> Route {
        r.public = true;
        r
    }

    /// A small table: a public GET, a session-like resource, a run endpoint.
    fn table(authenticator: Option<Arc<dyn Authenticator>>, max: i64) -> RouteTable {
        const ONE: &str = "/apps/{app_name}/users/{user_id}/sessions/{session_id}";
        const ALL: &str = "/apps/{app_name}/users/{user_id}/sessions";
        RouteTable::new(
            vec![
                public(route("GetVersion", &[Method::GET], "/version", echo())),
                route("GetSession", &[Method::GET], ONE, echo()),
                route("CreateSession", &[Method::POST], ALL, echo()),
                route("CreateSessionWithId", &[Method::POST], ONE, echo()),
                route("UpdateSession", &[Method::PATCH], ONE, echo()),
                route("DeleteSession", &[Method::DELETE], ONE, echo()),
                route("ListSessions", &[Method::GET], ALL, echo()),
                route("RunAgent", &[Method::POST], "/run", echo()),
                route("Pair", &[Method::GET], "/pair/{a}-{b}", echo()),
            ],
            authenticator,
            max,
        )
    }

    fn req(method: &str, target: &str, body: &str) -> Request {
        http::Request::builder()
            .method(method)
            .uri(target)
            .body(Body::from(body.to_owned()))
            .unwrap()
    }

    /// Status, headers (lower-case name, value; sorted) and body.
    async fn parts(resp: Response) -> (u16, Vec<(String, String)>, String) {
        let status = resp.status().as_u16();
        let mut headers: Vec<(String, String)> = resp
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap().to_owned()))
            .collect();
        headers.sort();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, headers, String::from_utf8(body.to_vec()).unwrap())
    }

    async fn serve(
        t: &RouteTable,
        method: &str,
        target: &str,
        body: &str,
    ) -> (u16, Vec<(String, String)>, String) {
        parts(t.serve(req(method, target, body)).await).await
    }

    fn h(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn with_head_adds_head_to_get_routes_only() {
        assert_eq!(
            with_head(vec![Method::GET]),
            vec![Method::GET, Method::HEAD]
        );
        assert_eq!(
            with_head(vec![Method::HEAD, Method::GET]),
            vec![Method::HEAD, Method::GET]
        );
        assert_eq!(with_head(vec![Method::POST]), vec![Method::POST]);
    }

    #[tokio::test]
    async fn path_variables_are_decoded() {
        let t = table(None, 0);
        let (status, _, body) =
            serve(&t, "GET", "/apps/my%20app/users/u%3F/sessions/s%C3%A9", "").await;
        assert_eq!(status, 200);
        assert_eq!(
            body,
            "GET /apps/my%20app/users/u%3F/sessions/s%C3%A9 \
             {\"app_name\": \"my app\", \"session_id\": \"sé\", \"user_id\": \"u?\"} - "
        );
        // An encoded slash decodes to a separator, so it matches no single-segment variable.
        let (status, _, _) = serve(&t, "GET", "/apps/a%2Fb/users/u/sessions/s", "").await;
        assert_eq!(status, 404);
    }

    #[tokio::test]
    async fn unknown_path_is_go_not_found() {
        let t = table(None, 0);
        for method in ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"] {
            let (status, headers, _) = serve(&t, method, "/no-such-endpoint", "").await;
            assert_eq!(status, 404, "{method}");
            assert!(
                headers.iter().all(|(k, _)| k != "allow"),
                "{method}: no Allow"
            );
        }
        assert_eq!(
            serve(&t, "GET", "/no/such/path", "").await,
            (
                404,
                h(&[
                    ("content-type", "text/plain; charset=utf-8"),
                    ("x-content-type-options", "nosniff"),
                ]),
                "404 page not found\n".to_owned()
            )
        );
    }

    #[tokio::test]
    async fn wrong_method_is_405_with_allow_in_probe_order() {
        let t = table(None, 0);
        let one = "/apps/a/users/u/sessions/s";
        for (method, target, allow) in [
            ("PUT", "/version", "GET, HEAD"),
            ("DELETE", "/version", "GET, HEAD"),
            ("GET", "/run", "POST"),
            ("PUT", one, "GET, HEAD, POST, PATCH, DELETE"),
            ("DELETE", "/apps/a/users/u/sessions", "GET, HEAD, POST"),
            ("TRACE", "/run", "POST"),
        ] {
            assert_eq!(
                serve(&t, method, target, "").await,
                (405, h(&[("allow", allow)]), String::new()),
                "{method} {target}"
            );
        }
    }

    #[tokio::test]
    async fn options_describes_an_existing_resource() {
        let t = table(None, 0);
        assert_eq!(
            serve(&t, "OPTIONS", "/apps/a/users/u/sessions/s", "").await,
            (
                204,
                h(&[("allow", "GET, HEAD, POST, PATCH, DELETE, OPTIONS")]),
                String::new()
            )
        );
        // OPTIONS never reaches a handler (it used to delete sessions in Go).
        let (status, _, _) = serve(&t, "OPTIONS", "/run", "").await;
        assert_eq!(status, 204);
        assert_eq!(
            serve(&t, "OPTIONS", "*", "").await,
            (200, h(&[("content-length", "0")]), String::new())
        );
    }

    #[tokio::test]
    async fn trailing_slash_redispatches_with_method_and_body() {
        let t = table(None, 0);
        let (status, _, body) =
            serve(&t, "POST", "/apps/a/users/u/sessions/s2/?q=1", "{\"x\":1}").await;
        assert_eq!(status, 200);
        assert_eq!(
            body,
            "POST /apps/a/users/u/sessions/s2?q=1 \
             {\"app_name\": \"a\", \"session_id\": \"s2\", \"user_id\": \"u\"} - {\"x\":1}"
        );
        let (status, _, body) = serve(&t, "GET", "/version/", "").await;
        assert_eq!((status, body.as_str()), (200, "GET /version {} - "));
        // The 405 is computed on the trimmed path.
        assert_eq!(
            serve(&t, "DELETE", "/version/", "").await,
            (405, h(&[("allow", "GET, HEAD")]), String::new())
        );
        // A path that is nothing but slashes is not trimmed.
        let (status, _, body) = serve(&t, "GET", "/", "").await;
        assert_eq!((status, body.as_str()), (404, "404 page not found\n"));
        // An encoded slash is not a trailing slash.
        let (status, _, _) = serve(&t, "GET", "/version%2F", "").await;
        assert_eq!(status, 404);
    }

    #[tokio::test]
    async fn unclean_path_redirects_like_mux() {
        let t = table(None, 0);
        assert_eq!(
            serve(&t, "POST", "/x/../run?a=1", "").await,
            (301, h(&[("location", "/run?a=1")]), String::new())
        );
        assert_eq!(
            serve(&t, "GET", "//version", "").await,
            (301, h(&[("location", "/version")]), String::new())
        );
        assert_eq!(
            serve(&t, "GET", "/a%20b/./c", "").await,
            (301, h(&[("location", "/a%20b/c")]), String::new())
        );
    }

    /// Go's `r.URL` of an HTTP/2 request is its `:path` alone (hyper's request URI also carries
    /// `:scheme` and `:authority`), so neither redirect gains `scheme://authority`; an HTTP/1
    /// absolute-form target keeps them, as Go's `URL.String` does.
    #[tokio::test]
    async fn http2_redirects_use_the_path_only_request_uri() {
        let mut health = public(route("Health", &[Method::GET], "/health", echo()));
        health.strict_slash = true;
        let t = RouteTable::new(vec![health], None, 0);
        let h2 = |target: &str| {
            let mut r = req("GET", target, "");
            *r.version_mut() = http::Version::HTTP_2;
            r
        };
        let resp = t.serve(h2("http://example.com/x/../health?a=1")).await;
        assert_eq!(resp.headers()[http::header::LOCATION], "/health?a=1");
        let resp = t.serve(h2("http://example.com/health/?a=1")).await;
        assert_eq!(resp.headers()[http::header::LOCATION], "/health?a=1");
        let resp = t
            .serve(req("GET", "http://example.com/x/../health", ""))
            .await;
        assert_eq!(
            resp.headers()[http::header::LOCATION],
            "http://example.com/health"
        );
    }

    /// Go's `/health`, registered before `SetupSubRouters` turns StrictSlash off: the other
    /// trailing-slash form (`%2F` included, as gorilla matches the decoded path) is a 301 for
    /// GET and HEAD and, through the fallback's probe, a 405 with `Allow` for other methods.
    #[tokio::test]
    async fn strict_slash_route_redirects_like_gorilla() {
        let mut health = public(route(
            "Health",
            &[Method::GET, Method::HEAD],
            "/health",
            echo(),
        ));
        health.strict_slash = true;
        let t = RouteTable::new(
            vec![health, route("RunAgent", &[Method::POST], "/run", echo())],
            None,
            0,
        );
        assert_eq!(
            serve(&t, "GET", "/health%2F?q=1", "").await,
            (
                301,
                h(&[
                    ("content-type", "text/html; charset=utf-8"),
                    ("location", "/health?q=1")
                ]),
                "<a href=\"/health?q=1\">Moved Permanently</a>.\n\n".to_owned()
            )
        );
        assert_eq!(
            serve(&t, "HEAD", "/health%2F", "").await,
            (
                301,
                h(&[
                    ("content-type", "text/html; charset=utf-8"),
                    ("location", "/health")
                ]),
                String::new()
            )
        );
        assert_eq!(
            serve(&t, "GET", "/health/", "").await.1,
            h(&[
                ("content-type", "text/html; charset=utf-8"),
                ("location", "/health")
            ])
        );
        assert_eq!(
            serve(&t, "POST", "/health%2F", "").await,
            (405, h(&[("allow", "GET, HEAD")]), String::new())
        );
        assert_eq!(serve(&t, "GET", "/health", "").await.0, 200);
        // Without StrictSlash the trailing slash is the fallback's to trim.
        let (status, _, body) = serve(&t, "POST", "/run/", "x").await;
        assert_eq!((status, body.as_str()), (200, "POST /run {} - x"));
    }

    #[tokio::test]
    async fn undecodable_target_is_go_bad_request() {
        let t = table(None, 0);
        assert_eq!(
            serve(&t, "GET", "/apps/%zz/users/u/sessions", "").await,
            (
                400,
                h(&[
                    ("connection", "close"),
                    ("content-type", "text/plain; charset=utf-8"),
                ]),
                "400 Bad Request".to_owned()
            )
        );
    }

    #[tokio::test]
    async fn head_runs_the_get_handler_without_a_body() {
        let t = table(None, 0);
        let (status, headers, body) = serve(&t, "HEAD", "/version", "").await;
        assert_eq!(status, 200);
        assert_eq!(body, "");
        // The handler sees HEAD (`HEAD /version {} - ` is 19 bytes); the length is kept.
        assert!(
            headers.contains(&("content-length".to_owned(), "19".to_owned())),
            "{headers:?}"
        );
    }

    #[tokio::test]
    async fn first_registered_route_wins() {
        let first = handler(|_req: Request| async { "first".into_response() });
        let second = handler(|_req: Request| async { "second".into_response() });
        let t = RouteTable::new(
            vec![
                route("A", &[Method::GET], "/x/{id}", first),
                route("B", &[Method::GET], "/x/{id}", second),
            ],
            None,
            0,
        );
        assert_eq!(serve(&t, "GET", "/x/1", "").await.2, "first");
    }

    #[tokio::test]
    async fn non_public_routes_require_authentication() {
        let t = table(Some(authn::header("X-User")), 0);
        let (status, _, body) = serve(&t, "GET", "/apps/a/users/u/sessions", "").await;
        assert_eq!((status, body.as_str()), (401, "unauthorized\n"));
        // Public routes and the fallback run without credentials.
        assert_eq!(serve(&t, "GET", "/version", "").await.0, 200);
        assert_eq!(
            serve(&t, "PUT", "/apps/a/users/u/sessions", "").await.0,
            405
        );
        assert_eq!(serve(&t, "GET", "/nope", "").await.0, 404);
        let mut r = req("GET", "/apps/a/users/u/sessions", "");
        r.headers_mut().insert("x-user", "alice".parse().unwrap());
        let (status, _, body) = parts(t.serve(r).await).await;
        assert_eq!(status, 200);
        assert!(body.ends_with(" alice "), "{body}");
        // Without an authenticator (Go nil) nothing is authenticated.
        assert_eq!(
            serve(&table(None, 0), "GET", "/apps/a/users/u/sessions", "")
                .await
                .0,
            200
        );
    }

    /// Go's `http.Error(w, err.Error(), 400)` on a failed body read.
    fn read_body_handler() -> Handler {
        handler(|req: Request| async move {
            match axum::body::to_bytes(req.into_body(), usize::MAX).await {
                Ok(b) => String::from_utf8_lossy(&b).into_owned().into_response(),
                Err(e) if is_max_bytes_error(&e) => {
                    http_error(StatusCode::BAD_REQUEST, &e.to_string())
                }
                Err(e) => http_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
            }
        })
    }

    #[tokio::test]
    async fn matched_routes_limit_the_body() {
        let t = RouteTable::new(
            vec![route("Read", &[Method::POST], "/read", read_body_handler())],
            Some(authn::noop()),
            8,
        );
        assert_eq!(serve(&t, "POST", "/read", "12345678").await.2, "12345678");
        assert_eq!(
            serve(&t, "POST", "/read", "123456789").await,
            (
                400,
                h(&[
                    ("connection", "close"),
                    ("content-type", "text/plain; charset=utf-8"),
                    ("x-content-type-options", "nosniff"),
                ]),
                "http: request body too large\n".to_owned()
            )
        );
        // The trailing-slash re-dispatch still runs the limit.
        assert_eq!(serve(&t, "POST", "/read/", "123456789").await.0, 400);
    }

    #[tokio::test]
    async fn handler_panic_answers_500() {
        let t = RouteTable::new(
            vec![route(
                "Boom",
                &[Method::GET],
                "/boom",
                handler(|_req: Request| async { todo!("not yet") }),
            )],
            Some(authn::noop()),
            0,
        );
        let (status, _, body) = serve(&t, "GET", "/boom", "").await;
        assert_eq!((status, body.as_str()), (500, "Internal Server Error\n"));
    }

    /// The Go probe's fixtures for the router's own answers (Task 2, `testdata/wire`), against
    /// the real sessions, apps and runtime route tables behind the main configuration's origin
    /// guard (`BindHost: 127.0.0.1`, `AllowedOrigins: [http://localhost:4200]`, 1024-byte
    /// limit). `/health` and the session-create body read are stand-ins for Task 13's and Task 6's
    /// handlers with Go's exact output.
    #[tokio::test]
    async fn router_answers_match_go_wire_fixtures() {
        let sessions: Arc<dyn gadk_session::SessionService> =
            Arc::new(InMemorySessionService::new());
        let mut routes = vec![public(route(
            "Health",
            &[Method::GET],
            "/health",
            handler(|_req: Request| async {
                let mut r = "{\"status\":\"ok\"}\n".into_response();
                r.headers_mut()
                    .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
                r
            }),
        ))];
        routes.push(route(
            "CreateSession",
            &[Method::POST],
            "/apps/{app_name}/users/{user_id}/sessions",
            read_body_handler(),
        ));
        routes.extend(sessions_routes(Arc::new(SessionsApiController::new(
            Arc::clone(&sessions),
        ))));
        routes.extend(runtime_routes(Arc::new(RuntimeApiController::new(
            RuntimeApiControllerConfig::new(sessions),
        ))));
        routes.extend(apps_routes(Arc::new(AppsApiController::new(None))));
        let table = Arc::new(RouteTable::new(routes, Some(authn::noop()), 1024));
        let policy = Arc::new(Policy::new(&originguard::Config {
            bind_host: "127.0.0.1".into(),
            allowed_origins: vec!["http://localhost:4200".into()],
        }));
        let t = Arc::clone(&table);
        let server = policy.middleware(handler(move |req| {
            let t = Arc::clone(&t);
            async move { t.serve(req).await }
        }));

        for name in [
            "main_01_health_get",
            "main_02_health_head",
            "main_53_unknown_path",
            "main_54_wrong_method",
            "main_55_wrong_method_run",
            "main_56_options_session",
            "main_57_options_unknown_path",
            "main_61_trailing_slash_root",
            "main_62_oversized_body",
            "main_63_origin_forbidden",
            "main_64_origin_allowed",
            "main_65_host_forbidden",
        ] {
            let path = format!("{}/testdata/wire/{name}.json", env!("CARGO_MANIFEST_DIR"));
            let fixture: Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            let rq = &fixture["request"];
            let mut b = http::Request::builder()
                .method(rq["method"].as_str().unwrap())
                .uri(rq["target"].as_str().unwrap());
            let mut has_host = false;
            for kv in rq["headers"].as_array().unwrap() {
                let (k, v) = (kv[0].as_str().unwrap(), kv[1].as_str().unwrap());
                has_host |= k.eq_ignore_ascii_case("host");
                b = b.header(k, v);
            }
            if !has_host {
                b = b.header("host", "127.0.0.1:8080");
            }
            let mut r = b
                .body(Body::from(rq["body"].as_str().unwrap().to_owned()))
                .unwrap();
            r.extensions_mut()
                .insert(ConnectInfo(LocalAddr(SocketAddr::from((
                    [127, 0, 0, 1],
                    8080,
                )))));
            let (status, headers, body) = parts(server(r).await).await;
            let headers: Vec<(String, String)> = headers
                .into_iter()
                .filter(|(k, _)| k != "content-length")
                .collect();
            let want = &fixture["response"];
            let mut want_headers: Vec<(String, String)> = want["headers"]
                .as_array()
                .unwrap()
                .iter()
                .map(|kv| {
                    (
                        kv[0].as_str().unwrap().to_ascii_lowercase(),
                        kv[1].as_str().unwrap().to_owned(),
                    )
                })
                .collect();
            want_headers.sort();
            assert_eq!(
                (status, headers, body),
                (
                    u16::try_from(want["status"].as_u64().unwrap()).unwrap(),
                    want_headers,
                    want["body"].as_str().unwrap().to_owned()
                ),
                "{name}"
            );
        }
    }
}
