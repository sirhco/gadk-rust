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

//! The sub-launcher that mounts the ADK REST API server
//! (gadk-server's [`Server`]) on the web launcher's router.
//!
//! The mount adds CORS headers for the web UI origin and the server-wide allowed origins
//! (`cors_with_args`, preflights answered 200 by the middleware), trims trailing slashes before
//! the REST router sees the path (`trim_trailing_slash`), and under a prefix strips it
//! (`strip_prefix`) with a redirect rewriter (`rewrite_redirects`) that keeps a root-relative
//! `Location` under the prefix and turns 301/302 into 308/307 for a request that is not GET or
//! HEAD. The prefix is mounted segment-aware (`prefix + "/"` and the bare prefix); an empty or `/`
//! prefix is a catch-all registered after whatever the other sub-launchers registered first.

use std::io::Write;
use std::sync::{Arc, Mutex};

use gadk_platform::go_flag::{Flag, FlagSet};
use gadk_platform::{GoDuration, go_url};
use gadk_server::mux::{Handler, Request, Response, not_found};
use gadk_server::rest::{
    DebugApiConfig, DebugTelemetryConfig, Server, ServerConfig, ServerError, Timeout,
    normalize_origin,
};
use http::header::{
    ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN,
    LOCATION, ORIGIN, VARY,
};
use http::uri::PathAndQuery;
use http::{HeaderValue, Method, StatusCode, Uri};

use crate::config::Config;
use crate::error::Error;
use crate::flag_usage::format_flag_usage;
use crate::web::{Middleware, WebRouter, WebSublauncher, axum_handler};

/// The verbs a CORS preflight is told.
const CORS_ALLOW_METHODS: &str = "GET, HEAD, POST, PUT, PATCH, DELETE, OPTIONS";

/// The `Access-Control-Allow-Headers` value.
const CORS_ALLOW_HEADERS: &str = "Content-Type, Authorization";

/// The flags' values.
struct ApiConfig {
    frontend_address: Flag<String>,
    path_prefix: Flag<String>,
    sse_write_timeout: Flag<GoDuration>,
    trace_capacity: Flag<i64>,
    include_debug_api: Flag<bool>,
}

/// The `api` web sub-launcher.
pub struct ApiLauncher {
    /// The `web` flag set (named `"web"`). Behind a `Mutex` because a
    /// `FlagSet` is `Send` but not `Sync`.
    flags: Mutex<FlagSet>,
    config: ApiConfig,
    /// The path prefix after `Parse` normalized it (the flag default until then).
    path_prefix: String,
}

/// An `api` launcher with its flags.
pub fn new() -> ApiLauncher {
    let mut fs = FlagSet::new("web");
    let frontend_address = fs.string(
        "webui_address",
        "localhost:8080",
        "ADK WebUI origin as seen from the user browser. It is sent as the CORS allowed origin and is allowed on every route of the web server, like a web -allow_origins entry; other cross-origin browser requests are refused with 403. Accepts a full origin such as 'http://localhost:8080'; a bare hostname and optional port is read as http. '*' allows any origin and turns the origin and DNS-rebinding checks off on every route - do not use it on a server reachable from an untrusted network, since these endpoints are unauthenticated.",
    );
    let path_prefix = fs.string(
        "path_prefix",
        "/api",
        "ADK REST API path prefix. Default is '/api'.",
    );
    let sse_write_timeout = fs.duration(
        "sse-write-timeout",
        GoDuration::from_secs(120),
        "SSE server write timeout (i.e. '10s', '2m' - see time.ParseDuration for details) - for writing the SSE response after reading the headers & body",
    );
    let trace_capacity = fs.int(
        "trace_capacity",
        10000,
        "Maximum number of traces to keep in memory.",
    );
    let include_debug_api = fs.bool(
        "include_debug_api",
        false,
        "Serve the debug trace and agent graph endpoints, which expose tool-call arguments, responses and tool names. The web UI's Traces and agent structure panels need them. !!! WARNING !!! : debug endpoints are not safe to be used in production environment, do not set them to true in production.",
    );
    let prefix = path_prefix.get();
    ApiLauncher {
        flags: Mutex::new(fs),
        config: ApiConfig {
            frontend_address,
            path_prefix,
            sse_write_timeout,
            trace_capacity,
            include_debug_api,
        },
        path_prefix: prefix,
    }
}

impl ApiLauncher {
    /// Replaces where the api flag set writes its errors and usage (default
    /// stderr), for tests and the CLI replay.
    pub fn with_flag_output(self, out: Arc<Mutex<dyn Write + Send>>) -> ApiLauncher {
        lock(&self.flags).set_output(out);
        self
    }

    /// The REST server's config from the launcher's `Config` and the flags.
    fn server_config(&self, config: &Config) -> Result<ServerConfig, ServerError> {
        // The server requires a session service. The web launcher's `Run` always supplies one
        // (see `apply_service_defaults`).
        let Some(session_service) = config.session_service.clone() else {
            return Err(ServerError("no session service configured".to_owned()));
        };
        let mut cfg = ServerConfig::new(session_service);
        cfg.memory_service = config.memory_service.clone();
        cfg.agent_loader = Some(Arc::clone(&config.agent_loader));
        cfg.artifact_service = config.artifact_service.clone();
        cfg.max_payload_size = config.max_payload_size;
        cfg.sse_write_timeout = go_timeout(self.config.sse_write_timeout.get());
        cfg.plugin_config = config.plugin_config.clone();
        cfg.authenticator = config.authenticator.clone();
        cfg.authorizer = config.authorizer.clone();
        cfg.compaction = config.compaction.clone();
        cfg.bind_host = config.bind_host.clone();
        // Everything allowed so far: the web UI origin appended by the caller, the operator's
        // -allow_origins entries, and whatever the sub-launchers set up before this one added.
        cfg.allowed_origins = config.allowed_origins.clone();
        cfg.debug_config = DebugTelemetryConfig {
            trace_capacity: go_int_to_i32(self.config.trace_capacity.get()),
        };
        cfg.debug_api_config = DebugApiConfig {
            include_debug_api: self.config.include_debug_api.get(),
        };
        Ok(cfg)
    }
}

impl WebSublauncher for ApiLauncher {
    /// The keyword `api`.
    fn keyword(&self) -> String {
        "api".to_owned()
    }

    /// Parses the api flags, then the prefix normalized to a leading slash and no trailing
    /// slash.
    fn parse(&mut self, args: Vec<String>) -> Result<Vec<String>, Error> {
        let mut fs = lock(&self.flags);
        if let Err(e) = fs.parse(&args) {
            return Err(Error::Parse(format!("failed to parse api flags: {e}")));
        }
        let mut p = self.config.path_prefix.get();
        if !p.starts_with('/') {
            p.insert(0, '/');
        }
        self.path_prefix = p.strip_suffix('/').unwrap_or(&p).to_owned();
        Ok(fs.args())
    }

    /// The flag usage text.
    fn command_line_syntax(&self) -> String {
        format_flag_usage(&lock(&self.flags))
    }

    /// A one-line description.
    fn simple_description(&self) -> String {
        "starts ADK REST API server, accepting origins specified by webui_address (CORS)".to_owned()
    }

    /// Adds the web UI origin to the server-wide allowed origins, builds
    /// the REST server (which validates the compaction config against the loader's agents), adds
    /// its debug layer to the telemetry layers, and mounts it behind CORS under the prefix.
    fn setup_subrouters(&self, router: &mut WebRouter, config: &mut Config) -> Result<(), Error> {
        let frontend_address = self.config.frontend_address.get();
        // The web UI origin belongs to the whole server, not to this mount alone.
        config.allowed_origins.push(frontend_address.clone());

        let rest_server = self
            .server_config(config)
            .and_then(Server::new)
            .map_err(|e| Error::Wrapped {
                context: "failed to create REST server".to_owned(),
                source: Box::new(e),
            })?;

        // The server's debug layer joins the telemetry layers.
        config
            .telemetry_layers
            .push(Box::new(rest_server.debug_layer()));

        // CORS over the same origins the REST server was given.
        let cors = cors_with_args(&frontend_address, &config.allowed_origins);
        let cors_handler = cors(axum_handler(rest_server.router()));

        register_api_routes(router, &self.path_prefix, cors_handler);
        Ok(())
    }

    /// The startup lines for this sub-launcher.
    fn user_message(&self, web_url: &str, printer: &mut dyn FnMut(&str)) {
        let prefix = &self.path_prefix;
        printer(&format!(
            "       api:  you can access API using {web_url}{prefix}"
        ));
        printer(&format!(
            "       api:      for instance: {web_url}{prefix}/list-apps"
        ));
        if !self.config.include_debug_api.get() {
            // Otherwise the only signal is a 404 in the browser console.
            printer(
                "       api:      debug excluded: for the web UI's Traces and agent structure panels, use -include_debug_api",
            );
        }
    }
}

/// A flag duration as the REST server's [`Timeout`] (zero: default, negative: none).
fn go_timeout(d: GoDuration) -> Timeout {
    match d.to_std() {
        None => Timeout::Disabled,
        Some(std) if std.is_zero() => Timeout::Default,
        Some(std) => Timeout::After(std),
    }
}

/// A 64-bit flag integer as the REST server's `i32` trace capacity, saturating.
fn go_int_to_i32(v: i64) -> i32 {
    i32::try_from(v).unwrap_or(if v < 0 { i32::MIN } else { i32::MAX })
}

fn lock<T: ?Sized>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The CORS middleware for `frontend_address` and `allowed_origins`.
///
/// `Access-Control-Allow-Origin` names the request's `Origin` when it is one of `allowed_origins`,
/// and `frontend_address` otherwise; a `*` in either makes it `*`. Addresses are normalized with
/// [`normalize_origin`] once, when the middleware is built. An OPTIONS request is answered 200 by
/// the middleware.
pub(crate) fn cors_with_args(frontend_address: &str, allowed_origins: &[String]) -> Middleware {
    let mut origin = normalize_origin(frontend_address);
    let mut allowed: Vec<String> = Vec::new();
    for o in allowed_origins {
        match normalize_origin(o) {
            o if o.is_empty() => {}
            o if o == "*" => origin = "*".to_owned(),
            o => {
                if !allowed.contains(&o) {
                    allowed.push(o);
                }
            }
        }
    }
    if origin == "*" {
        allowed.clear();
    }
    // Whether the header can differ between two requests only by their Origin.
    let vary = origin != "*" && (!origin.is_empty() || !allowed.is_empty());
    let origin = Arc::new(origin);
    let allowed = Arc::new(allowed);
    Arc::new(move |next: Handler| {
        let origin = Arc::clone(&origin);
        let allowed = Arc::clone(&allowed);
        Arc::new(move |req: Request| {
            let mut allow_origin = origin.as_str().to_owned();
            // Looked up as sent, not parsed.
            if let Some(request_origin) = req
                .headers()
                .get(ORIGIN)
                .and_then(|v| v.to_str().ok())
                .filter(|o| allowed.iter().any(|a| a == o))
            {
                allow_origin = request_origin.to_owned();
            }
            let preflight = req.method() == Method::OPTIONS;
            let next = Arc::clone(&next);
            Box::pin(async move {
                let resp = if preflight {
                    let mut resp = Response::new(axum::body::Body::empty());
                    *resp.status_mut() = StatusCode::OK;
                    resp
                } else {
                    next(req).await
                };
                // A WebSocket upgrade hijacks the connection and writes its 101 itself, without
                // these headers.
                if resp.status() == StatusCode::SWITCHING_PROTOCOLS {
                    return resp;
                }
                with_cors_headers(resp, &allow_origin, vary)
            })
        })
    })
}

/// The CORS headers set on the response: a header the handler set itself wins, and
/// `Vary: Origin` comes before the handler's own `Vary` values.
fn with_cors_headers(mut resp: Response, allow_origin: &str, vary: bool) -> Response {
    let h = resp.headers_mut();
    if !allow_origin.is_empty()
        && !h.contains_key(ACCESS_CONTROL_ALLOW_ORIGIN)
        && let Ok(v) = HeaderValue::from_str(allow_origin)
    {
        h.insert(ACCESS_CONTROL_ALLOW_ORIGIN, v);
    }
    if vary {
        let theirs: Vec<HeaderValue> = h.get_all(VARY).iter().cloned().collect();
        h.remove(VARY);
        h.append(VARY, HeaderValue::from_static("Origin"));
        for v in theirs {
            h.append(VARY, v);
        }
    }
    if !h.contains_key(ACCESS_CONTROL_ALLOW_METHODS) {
        h.insert(
            ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static(CORS_ALLOW_METHODS),
        );
    }
    if !h.contains_key(ACCESS_CONTROL_ALLOW_HEADERS) {
        h.insert(
            ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static(CORS_ALLOW_HEADERS),
        );
    }
    resp
}

/// A request target's path as decoded bytes, plus the raw escaped form when it differs from the
/// default escaping of the decoded path.
struct GoPath {
    path: Vec<u8>,
    raw_path: String,
}

impl GoPath {
    /// `None` when the escapes do not decode (a server answers 400 before any handler).
    fn of(uri: &Uri) -> Option<GoPath> {
        let raw = uri.path();
        let path = go_url::unescape_path(raw).ok()?;
        let raw_path = if go_url::escape_path(&path) == raw {
            String::new()
        } else {
            raw.to_owned()
        };
        Some(GoPath { path, raw_path })
    }

    /// The escaped path: the raw form when it is a valid encoding of the path, else the default
    /// escaping.
    fn escaped(&self) -> String {
        if !self.raw_path.is_empty()
            && go_url::valid_encoded_path(&self.raw_path)
            && go_url::unescape_path(&self.raw_path).is_ok_and(|p| p == self.path)
        {
            return self.raw_path.clone();
        }
        if self.path == b"*" {
            return "*".to_owned();
        }
        go_url::escape_path(&self.path)
    }

    /// The request with its target's path replaced by this one (query, scheme and authority kept).
    fn apply(&self, req: &mut Request) {
        let mut target = self.escaped();
        if let Some(q) = req.uri().query() {
            target.push('?');
            target.push_str(q);
        }
        let mut parts = req.uri().clone().into_parts();
        // An escaped path and the request's own query always form a valid target; were it not
        // so, the request would keep its target (and so the mount's unstripped path).
        let Ok(pq) = PathAndQuery::try_from(target) else {
            return;
        };
        parts.path_and_query = Some(pq);
        if let Ok(uri) = Uri::from_parts(parts) {
            *req.uri_mut() = uri;
        }
    }
}

/// Drops trailing slashes from the request path before the REST router
/// sees it (counted on the escaped path, so an encoded `%2F` is left alone), keeping a rooted path.
pub(crate) fn trim_trailing_slash(next: Handler) -> Handler {
    Arc::new(move |mut req: Request| {
        if let Some(mut p) = GoPath::of(req.uri()) {
            let escaped = p.escaped();
            let mut n = escaped.len() - escaped.trim_end_matches('/').len();
            if n == escaped.len() {
                // The path is nothing but slashes ("/" or ""): keep one, so the router always
                // sees a rooted path (`n` drops by one; for "" it wraps to `usize::MAX`).
                n = n.wrapping_sub(1);
            }
            let changes = !(n == 0 || n == usize::MAX) || p.path.is_empty();
            if changes {
                if n > 0 && n != usize::MAX {
                    p.path.truncate(p.path.len().saturating_sub(n));
                    if !p.raw_path.is_empty() {
                        let len = p.raw_path.len().saturating_sub(n);
                        p.raw_path.truncate(len);
                    }
                }
                if p.path.is_empty() {
                    p.path = b"/".to_vec();
                }
                p.apply(&mut req);
            }
        }
        next(req)
    })
}

/// The prefix removed from the path and the raw path, or a `404 page not found` when either does
/// not start with it.
pub(crate) fn strip_prefix(prefix: &str, next: Handler) -> Handler {
    let prefix = prefix.to_owned();
    Arc::new(move |mut req: Request| {
        let stripped = GoPath::of(req.uri()).and_then(|p| {
            let path = p.path.strip_prefix(prefix.as_bytes())?;
            let raw_path = if p.raw_path.is_empty() {
                String::new()
            } else {
                p.raw_path.strip_prefix(&prefix)?.to_owned()
            };
            Some(GoPath {
                path: path.to_vec(),
                raw_path,
            })
        });
        match stripped {
            Some(p) => {
                p.apply(&mut req);
                next(req)
            }
            None => Box::pin(std::future::ready(not_found())),
        }
    })
}

/// A 3xx response's
/// root-relative `Location` outside the prefix gets the prefix, and for a request that is not GET
/// or HEAD 301 becomes 308 and 302 becomes 307. Everything else (the body, a streaming response,
/// a connection upgrade) passes through untouched.
pub(crate) fn rewrite_redirects(prefix: &str, next: Handler) -> Handler {
    let prefix = Arc::new(prefix.to_owned());
    Arc::new(move |req: Request| {
        let safe_method = req.method() == Method::GET || req.method() == Method::HEAD;
        let prefix = Arc::clone(&prefix);
        let next = Arc::clone(&next);
        Box::pin(async move {
            let mut resp = next(req).await;
            let code = resp.status().as_u16();
            if (300..400).contains(&code) {
                let rewritten = resp
                    .headers()
                    .get(LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .filter(|loc| {
                        loc.starts_with('/')
                            && *loc != prefix.as_str()
                            && !loc.starts_with(&format!("{prefix}/"))
                    })
                    .and_then(|loc| HeaderValue::from_str(&format!("{prefix}{loc}")).ok());
                if let Some(v) = rewritten {
                    resp.headers_mut().insert(LOCATION, v);
                }
                if !safe_method {
                    // 301 and 302 license a client to re-issue the request as a GET; 308 and 307
                    // keep the method and the body.
                    match resp.status() {
                        StatusCode::MOVED_PERMANENTLY => {
                            *resp.status_mut() = StatusCode::PERMANENT_REDIRECT;
                        }
                        StatusCode::FOUND => *resp.status_mut() = StatusCode::TEMPORARY_REDIRECT,
                        _ => {}
                    }
                }
            }
            resp
        })
    })
}

/// Mounts `handler` under `path_prefix`, matching on path only (the REST
/// router answers a verb it does not serve with an accurate `Allow`). An empty or `/` prefix is a
/// catch-all route, so routes registered before it match first;
/// otherwise `prefix + "/"` (first, so the mount root is not answered with an outer StrictSlash
/// redirect) and the bare prefix, which keeps the mount on segment boundaries.
pub(crate) fn register_api_routes(router: &mut WebRouter, path_prefix: &str, handler: Handler) {
    if path_prefix.is_empty() || path_prefix == "/" {
        router.handle_prefix(&[], "", trim_trailing_slash(handler));
        return;
    }
    let mounted = rewrite_redirects(
        path_prefix,
        strip_prefix(path_prefix, trim_trailing_slash(handler)),
    );
    router.handle_prefix(&[], &format!("{path_prefix}/"), Arc::clone(&mounted));
    router.handle(&[], path_prefix, mounted);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::time::Duration;

    use gadk_agent::{AgentConfig, SingleLoader};
    use pretty_assertions::assert_eq;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::launcher::SubLauncher;
    use crate::test_support::{self as ts, strings};

    /// gorilla's `Upgrade` writes the 101 itself, without the headers the CORS middleware set on
    /// `w`: a WebSocket upgrade (`/run_live`) carries no CORS headers.
    #[tokio::test]
    async fn cors_headers_are_not_added_to_a_101_upgrade() {
        let mw = cors_with_args("http://localhost:8080", &strings(&["http://a.example"]));
        let upgrade: Handler = Arc::new(|_req: Request| {
            Box::pin(async {
                let mut resp = Response::new(axum::body::Body::empty());
                *resp.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
                resp
            })
        });
        let req = || {
            http::Request::builder()
                .uri("/run_live")
                .header(ORIGIN, "http://a.example")
                .body(axum::body::Body::empty())
                .unwrap()
        };
        let resp = mw(upgrade)(req()).await;
        assert_eq!(resp.status(), StatusCode::SWITCHING_PROTOCOLS);
        assert!(resp.headers().is_empty(), "{:?}", resp.headers());
        let ok: Handler =
            Arc::new(|_req: Request| Box::pin(async { Response::new(axum::body::Body::empty()) }));
        let resp = mw(ok)(req()).await;
        assert_eq!(
            resp.headers()[ACCESS_CONTROL_ALLOW_ORIGIN],
            "http://a.example"
        );
    }

    #[test]
    fn parse_normalizes_the_prefix() {
        for (arg, want) in [
            ("/api", "/api"),
            ("api", "/api"),
            ("/api/", "/api"),
            ("adk/v1/", "/adk/v1"),
            ("", ""),
            ("/", ""),
        ] {
            let mut l = new();
            let rest = WebSublauncher::parse(&mut l, strings(&["-path_prefix", arg, "rest"]));
            assert_eq!(rest.unwrap(), strings(&["rest"]), "{arg:?}");
            assert_eq!(l.path_prefix, want, "{arg:?}");
        }
        // Before Parse, Go's config holds the flag default.
        assert_eq!(new().path_prefix, "/api");
    }

    #[test]
    fn descriptions_and_user_message_are_go_text() {
        let mut l = new();
        assert_eq!(l.keyword(), "api");
        assert_eq!(
            l.simple_description(),
            "starts ADK REST API server, accepting origins specified by webui_address (CORS)"
        );
        WebSublauncher::parse(&mut l, strings(&["-path_prefix", "adk/"])).unwrap();
        let mut lines = Vec::new();
        l.user_message("http://localhost:8080", &mut |line: &str| {
            lines.push(line.to_owned())
        });
        assert_eq!(
            lines,
            strings(&[
                "       api:  you can access API using http://localhost:8080/adk",
                "       api:      for instance: http://localhost:8080/adk/list-apps",
                "       api:      debug excluded: for the web UI's Traces and agent structure panels, use -include_debug_api",
            ])
        );
    }

    #[test]
    fn server_config_comes_from_config_and_flags() {
        let mut l = new();
        WebSublauncher::parse(
            &mut l,
            strings(&[
                "-sse-write-timeout",
                "-1s",
                "-trace_capacity",
                "7",
                "-include_debug_api",
            ]),
        )
        .unwrap();
        let mut config = Config::new(Arc::new(SingleLoader::new(assistant())));
        config.session_service = Some(Arc::new(gadk_session::InMemorySessionService::new()));
        config.max_payload_size = 99;
        config.bind_host = "127.0.0.1".to_owned();
        config.allowed_origins = strings(&["a"]);
        let cfg = l.server_config(&config).unwrap();
        assert_eq!(cfg.sse_write_timeout, Timeout::Disabled);
        assert_eq!(cfg.debug_config.trace_capacity, 7);
        assert!(cfg.debug_api_config.include_debug_api);
        assert_eq!(cfg.max_payload_size, 99);
        assert_eq!(cfg.bind_host, "127.0.0.1");
        assert_eq!(cfg.allowed_origins, strings(&["a"]));
        assert!(cfg.agent_loader.is_some());

        assert_eq!(go_timeout(GoDuration(0)), Timeout::Default);
        assert_eq!(
            go_timeout(GoDuration::from_secs(120)),
            Timeout::After(Duration::from_secs(120))
        );
        assert_eq!(go_int_to_i32(i64::MAX), i32::MAX);
        assert_eq!(go_int_to_i32(i64::MIN), i32::MIN);
    }

    #[test]
    fn setup_errors_are_go_text() {
        // Compaction is validated by the REST server against the loader's agents.
        let mut config = Config::new(Arc::new(SingleLoader::new(assistant())));
        config.session_service = Some(Arc::new(gadk_session::InMemorySessionService::new()));
        config.compaction = Some(gadk_session::compaction::Config::default());
        let mut l = new();
        WebSublauncher::parse(&mut l, Vec::new()).unwrap();
        let err = l
            .setup_subrouters(&mut WebRouter::new(true), &mut config)
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("failed to create REST server: "), "{err}");
        assert!(config.telemetry_layers.is_empty());

        let mut config = Config::new(Arc::new(SingleLoader::new(assistant())));
        assert_eq!(
            l.setup_subrouters(&mut WebRouter::new(true), &mut config)
                .unwrap_err()
                .to_string(),
            "failed to create REST server: no session service configured"
        );
    }

    #[test]
    fn setup_adds_the_debug_layer() {
        let mut config = Config::new(Arc::new(SingleLoader::new(assistant())));
        config.session_service = Some(Arc::new(gadk_session::InMemorySessionService::new()));
        let mut l = new();
        WebSublauncher::parse(&mut l, Vec::new()).unwrap();
        l.setup_subrouters(&mut WebRouter::new(true), &mut config)
            .unwrap();
        assert_eq!(config.telemetry_layers.len(), 1);
        assert_eq!(config.allowed_origins, strings(&["localhost:8080"]));
    }

    /// The api parts of Task 2's CLI fixtures `help_web_api` and `api_bad_flag`: the `web` flag
    /// set's usage text (Go names the api launcher's set `"web"`) and the error chain.
    #[test]
    fn flag_errors_and_usage_match_go_cli_fixtures() {
        for name in ["help_web_api", "api_bad_flag"] {
            let f = ts::cli(name);
            let out = Arc::new(Mutex::new(Vec::<u8>::new()));
            let mut l = crate::web::new(vec![Box::new(
                new().with_flag_output(Arc::clone(&out) as Arc<Mutex<dyn Write + Send>>),
            )]);
            let args: Vec<String> = f["args"].as_array().unwrap()[1..]
                .iter()
                .map(|a| a.as_str().unwrap().to_owned())
                .collect();
            let err = SubLauncher::parse(&mut l, args).unwrap_err().to_string();
            assert_eq!(err, f["error"].as_str().unwrap(), "{name}: error");
            let stderr = f["stderr"].as_str().unwrap();
            let flag_output = &stderr[..stderr.find("<ts> Run failed: ").unwrap()];
            assert_eq!(
                String::from_utf8(out.lock().unwrap().clone()).unwrap(),
                flag_output,
                "{name}: flag output"
            );
            // The api section of the web syntax is this launcher's usage.
            assert!(
                stderr.contains(&format!(
                    "    api\n  {}\n",
                    WebSublauncher::command_line_syntax(&new())
                )),
                "{name}: syntax"
            );
        }
    }

    /// A model that streams one chunk, then answers 500ms later.
    struct SlowModel;

    impl gadk_model::Llm for SlowModel {
        fn name(&self) -> &str {
            "slow"
        }

        fn generate_content(
            &self,
            _req: gadk_model::LlmRequest,
            _stream: bool,
        ) -> futures::stream::BoxStream<
            'static,
            Result<gadk_model::LlmResponse, gadk_model::ModelError>,
        > {
            use futures::StreamExt as _;
            let text = |t: &str, partial: bool| gadk_model::LlmResponse {
                content: Some(gadk_genai::Content::from_text(t, gadk_genai::ROLE_MODEL)),
                partial,
                ..Default::default()
            };
            let (first, last) = (text("Slow ", true), text("Slow answer.", false));
            futures::stream::once(async move { Ok(first) })
                .chain(futures::stream::once(async move {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    Ok(last)
                }))
                .boxed()
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    /// Go `RunSSEHandler` moves the connection's write deadline to `-sse-write-timeout`: an SSE
    /// turn outlasting the web server's `-write-timeout` is streamed to the end.
    #[tokio::test]
    async fn run_sse_outlives_the_server_write_timeout() {
        let port = ts::free_port();
        let mut l = crate::web::new(vec![Box::new(new()) as Box<dyn WebSublauncher>]);
        let args = strings(&[
            "-port",
            &port.to_string(),
            "-write-timeout",
            "200ms",
            "api",
            "-sse-write-timeout",
            "5s",
        ]);
        assert_eq!(
            SubLauncher::parse(&mut l, args).unwrap(),
            Vec::<String>::new()
        );
        let l = Arc::new(l);
        let (log, _) = crate::golog::capture();
        let agent = ts::probe_agent_with(Arc::new(SlowModel));
        let mut config = Config::new(Arc::new(SingleLoader::new(agent)));
        config.log = log;
        let cancel = CancellationToken::new();
        let running = {
            let (l, cancel) = (Arc::clone(&l), cancel.clone());
            tokio::spawn(async move { l.run_with(cancel, config, |_| true).await })
        };
        ts::wait_listening(port).await;

        let base = format!("http://127.0.0.1:{port}/api");
        let client = reqwest::Client::new();
        let created = client
            .post(format!("{base}/apps/assistant/users/u/sessions/s"))
            .header(http::header::CONTENT_TYPE, "application/json")
            .body("{}")
            .send()
            .await
            .unwrap();
        assert_eq!(created.status(), 200);
        let resp = client
            .post(format!("{base}/run_sse"))
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(
                r#"{"appName":"assistant","userId":"u","sessionId":"s","streaming":true,"newMessage":{"role":"user","parts":[{"text":"hi"}]}}"#,
            )
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = resp
            .text()
            .await
            .unwrap_or_else(|e| format!("cut off: {e}"));
        assert!(body.contains("Slow answer."), "{body}");

        cancel.cancel();
        running.await.unwrap().unwrap();
    }

    /// The scripted agent's identity (SCENARIO.md "Agent and model"); no fixture used here runs
    /// it.
    fn assistant() -> Arc<dyn gadk_agent::Agent> {
        gadk_agent::new_agent(AgentConfig {
            name: "assistant".to_owned(),
            description: "Answers greetings and transfers funds.".to_owned(),
            sub_agents: Vec::new(),
            before_agent_callbacks: Vec::new(),
            after_agent_callbacks: Vec::new(),
            run: None,
        })
        .unwrap()
    }

    /// Replays `exchanges` of one fixture server against a web launcher with only the api
    /// sub-launcher, started on the server's flags (the other sub-launcher keywords, with their
    /// flags, dropped), byte for byte after the SCENARIO.md normalizations: the startup log
    /// (minus the other sub-launchers' lines), each response and its log lines, and the shutdown
    /// line.
    async fn replay(server: &str, exchanges: &[&str]) {
        let startup = ts::wire(&format!("{server}_00_startup"));
        let port = ts::free_port();
        let mut args: Vec<String> = Vec::new();
        let mut keep = true;
        for a in &ts::args(&startup, port)[1..] {
            if ["webui", "a2a", "pubsub", "eventarc"].contains(&a.as_str()) {
                keep = false;
                continue;
            }
            if a == "api" {
                keep = true;
            }
            if keep {
                args.push(a.clone());
            }
        }
        let mut l = crate::web::new(vec![Box::new(new()) as Box<dyn WebSublauncher>]);
        assert_eq!(
            SubLauncher::parse(&mut l, args).unwrap(),
            Vec::<String>::new()
        );
        let l = Arc::new(l);
        let (log, out) = crate::golog::capture();
        let mut config = Config::new(Arc::new(SingleLoader::new(assistant())));
        config.log = log.with_clock(|| ts::TS.to_owned());
        let cancel = CancellationToken::new();
        let running = {
            let (l, cancel) = (Arc::clone(&l), cancel.clone());
            tokio::spawn(async move { l.run_with(cancel, config, |_| true).await })
        };
        ts::wait_listening(port).await;

        assert_eq!(
            ts::normalize_log(&out.contents(), port),
            ts::startup_log_of(&startup, &["api"]),
            "{server}: startup log"
        );
        for name in exchanges {
            ts::assert_exchange(port, name, || out.contents()).await;
        }
        let logged = out.contents().len();
        cancel.cancel();
        running.await.unwrap().unwrap();
        assert_eq!(
            ts::normalize_log(&out.contents()[logged..], port),
            ts::log_lines(&ts::wire(&format!("{server}_99_shutdown"))),
            "{server}: shutdown log"
        );
    }

    /// The `full` server's exchanges that reach (or pass by) the API mount: origin and CORS
    /// handling, the REST routes under `/api`, segment-aware mounting, trailing slashes, the
    /// REST server's StrictSlash `/health` behind the redirect rewriter (`full_23`–`full_25`),
    /// 405 with `Allow` and the body limit.
    #[tokio::test]
    async fn the_api_mount_matches_go_wire_fixtures() {
        replay(
            "full",
            &[
                "full_01_health_get",
                "full_02_health_head",
                "full_03_health_wrong_method",
                "full_04_health_trailing_slash",
                "full_05_non_clean_path",
                "full_06_non_clean_path_post",
                "full_07_unmatched_path",
                "full_08_unmatched_path_cross_origin",
                "full_09_origin_forbidden",
                "full_10_origin_allowed_flag",
                "full_11_origin_allowed_webui",
                "full_12_host_forbidden",
                "full_13_cors_preflight",
                "full_14_cors_preflight_flag_origin",
                "full_15_api_list_apps",
                "full_16_api_health",
                "full_17_api_bare_prefix",
                "full_18_api_prefix_word",
                "full_19_api_session_create",
                "full_20_api_session_create_generated_id",
                "full_21_api_session_get_trailing_slash",
                "full_22_api_session_post_trailing_slash",
                "full_23_api_redirect_get",
                "full_24_api_redirect_head",
                "full_25_api_redirect_post",
                "full_26_api_wrong_method",
                "full_27_api_body_too_large",
                "full_49_pubsub_trigger_get",
            ],
        )
        .await;
    }

    /// The `noprefix` server (`api -path_prefix=`): the catch-all mount, complete.
    #[tokio::test]
    async fn the_catch_all_mount_matches_go_wire_fixtures() {
        replay(
            "noprefix",
            &[
                "noprefix_01_health",
                "noprefix_02_list_apps",
                "noprefix_03_list_apps_trailing_slash",
                "noprefix_04_unknown_path",
                "noprefix_05_root",
            ],
        )
        .await;
    }
}
