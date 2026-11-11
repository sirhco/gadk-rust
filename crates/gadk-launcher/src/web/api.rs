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

//! Port of ADK-Go `cmd/launcher/web/api`: the sub-launcher that mounts the ADK REST API server
//! (gadk-server's [`Server`]) on the web launcher's router.
//!
//! The mount is Go's: CORS headers for the web UI origin and the server-wide allowed origins
//! (`corsWithArgs`, preflights answered 200 by the middleware), trailing slashes trimmed before
//! the REST router sees the path (`trimTrailingSlash`), and under a prefix `http.StripPrefix` with
//! a redirect rewriter that keeps a root-relative `Location` under the prefix and turns 301/302
//! into 308/307 for a request that is not GET or HEAD. The prefix is mounted segment-aware
//! (`prefix + "/"` and the bare prefix); an empty or `/` prefix is a catch-all registered after
//! whatever the other sub-launchers registered first.

use std::io::Write;
use std::sync::{Arc, Mutex};

use gadk_platform::go_flag::{Flag, FlagSet};
use gadk_platform::{GoDuration, go_url};
use gadk_server::mux::{Handler, Request, Response};
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

/// Go `corsAllowMethods`: `apiMethods` joined, the verbs a CORS preflight is told.
const CORS_ALLOW_METHODS: &str = "GET, HEAD, POST, PUT, PATCH, DELETE, OPTIONS";

/// Go `Access-Control-Allow-Headers` value.
const CORS_ALLOW_HEADERS: &str = "Content-Type, Authorization";

/// Go `api.apiConfig`: the flags' values.
struct ApiConfig {
    frontend_address: Flag<String>,
    path_prefix: Flag<String>,
    sse_write_timeout: Flag<GoDuration>,
    trace_capacity: Flag<i64>,
    include_debug_api: Flag<bool>,
}

/// Go `api.apiLauncher`.
pub struct ApiLauncher {
    /// The `web` flag set (Go names the api launcher's set `"web"`). Behind a `Mutex` because a
    /// `FlagSet` is `Send` but not `Sync`.
    flags: Mutex<FlagSet>,
    config: ApiConfig,
    /// Go `config.pathPrefix` after `Parse` normalized it (the flag default until then).
    path_prefix: String,
}

/// Go `api.NewLauncher()`.
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
    /// Replaces where the api flag set writes its errors and usage (Go `fs.SetOutput`; default
    /// stderr), for tests and the CLI replay.
    pub fn with_flag_output(self, out: Arc<Mutex<dyn Write + Send>>) -> ApiLauncher {
        lock(&self.flags).set_output(out);
        self
    }

    /// The REST server's config from the launcher's `Config` and the flags (Go
    /// `adkrest.ServerConfig{…}` in `SetupSubrouters`).
    fn server_config(&self, config: &Config) -> Result<ServerConfig, ServerError> {
        // Go passes a nil SessionService through; gadk's server requires one. The web launcher's
        // `Run` always supplies it (applyServiceDefaults).
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
    /// Go `Keyword`.
    fn keyword(&self) -> String {
        "api".to_owned()
    }

    /// Go `Parse`: the api flags, then the prefix normalized to a leading slash and no trailing
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

    /// Go `CommandLineSyntax`.
    fn command_line_syntax(&self) -> String {
        format_flag_usage(&lock(&self.flags))
    }

    /// Go `SimpleDescription`.
    fn simple_description(&self) -> String {
        "starts ADK REST API server, accepting origins specified by webui_address (CORS)".to_owned()
    }

    /// Go `SetupSubrouters`: adds the web UI origin to the server-wide allowed origins, builds
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

        // Go appends the server's span and log processors to TelemetryOptions.
        config
            .telemetry_layers
            .push(Box::new(rest_server.debug_layer()));

        // CORS over the same origins the REST server was given.
        let cors = cors_with_args(&frontend_address, &config.allowed_origins);
        let cors_handler = cors(axum_handler(rest_server.router()));

        register_api_routes(router, &self.path_prefix, cors_handler);
        Ok(())
    }

    /// Go `UserMessage`.
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

/// Go `time.Duration` → the REST server's [`Timeout`] (zero: default, negative: none).
fn go_timeout(d: GoDuration) -> Timeout {
    match d.to_std() {
        None => Timeout::Disabled,
        Some(std) if std.is_zero() => Timeout::Default,
        Some(std) => Timeout::After(std),
    }
}

/// Go `int` (64-bit) into the REST server's `i32` trace capacity, saturating.
fn go_int_to_i32(v: i64) -> i32 {
    i32::try_from(v).unwrap_or(if v < 0 { i32::MIN } else { i32::MAX })
}

fn lock<T: ?Sized>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Go `corsWithArgs(frontendAddress, allowedOrigins)`.
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
            // Looked up as sent, not parsed (Go `r.Header.Get("Origin")`).
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
                with_cors_headers(resp, &allow_origin, vary)
            })
        })
    })
}

/// The CORS headers Go sets before calling the handler: a header the handler set itself wins
/// (Go `Set` replaces), and `Vary: Origin` comes before the handler's own `Vary` values (Go `Add`).
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

/// Go's `URL.Path` and `URL.RawPath` of a request target's path, as `url.setPath` sets them.
struct GoPath {
    path: Vec<u8>,
    raw_path: String,
}

impl GoPath {
    /// `None` when the escapes do not decode (Go's server answered 400 before any handler).
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

    /// Go `URL.EscapedPath()`.
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
        let Ok(pq) = PathAndQuery::try_from(target) else {
            return;
        };
        parts.path_and_query = Some(pq);
        if let Ok(uri) = Uri::from_parts(parts) {
            *req.uri_mut() = uri;
        }
    }
}

/// Go `trimTrailingSlash`: drops trailing slashes from the request path before the REST router
/// sees it (counted on the escaped path, so an encoded `%2F` is left alone), keeping a rooted path.
pub(crate) fn trim_trailing_slash(next: Handler) -> Handler {
    Arc::new(move |mut req: Request| {
        if let Some(mut p) = GoPath::of(req.uri()) {
            let escaped = p.escaped();
            let mut n = escaped.len() - escaped.trim_end_matches('/').len();
            if n == escaped.len() {
                // The path is nothing but slashes ("/" or ""): keep one, so the router always
                // sees a rooted path (Go `n--`, which makes "" -1).
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

/// Go `http.StripPrefix(prefix, h)`: the prefix removed from `Path` and `RawPath`, or Go's
/// `404 page not found` when either does not start with it.
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

/// Go `rewriteRedirects(pathPrefix, next)` with its `redirectRewriter`: a 3xx response's
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

/// Go `registerAPIRoutes`: mounts `handler` under `path_prefix`, matching on path only (the REST
/// router answers a verb it does not serve with an accurate `Allow`). An empty or `/` prefix is a
/// catch-all route (gorilla `NewRoute().Handler`), so routes registered before it match first;
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

/// Go `net/http.NotFound`.
fn not_found() -> Response {
    let mut resp = Response::new(axum::body::Body::from("404 page not found\n"));
    *resp.status_mut() = StatusCode::NOT_FOUND;
    let h = resp.headers_mut();
    h.insert(
        http::header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    h.insert(
        http::header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    resp
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::time::{Duration, Instant};

    use gadk_agent::{AgentConfig, SingleLoader};
    use pretty_assertions::assert_eq;
    use serde_json::{Value, json};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::golog::Captured;
    use crate::launcher::SubLauncher;

    /// The fixed `golog` clock of these tests.
    const TS: &str = "2009/11/10 23:00:00 ";

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_owned()).collect()
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
            let f: Value = serde_json::from_str(
                &std::fs::read_to_string(format!(
                    "{}/testdata/cli/{name}.json",
                    env!("CARGO_MANIFEST_DIR")
                ))
                .unwrap(),
            )
            .unwrap();
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

    // ---- Task 2's web fixtures (SCENARIO.md) for the API mount ----

    fn fixture(name: &str) -> Value {
        let path = format!("{}/testdata/wire/{name}.json", env!("CARGO_MANIFEST_DIR"));
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
    }

    /// SCENARIO.md N1 (fixed clock), N2 and N3 on log text.
    fn normalize_log(text: &str, port: u16) -> Vec<String> {
        text.lines()
            .map(|line| {
                let line = line.replacen(TS, "<ts> ", 1);
                let line = normalize_port(&line, port);
                let fields: Vec<&str> = line.split(' ').collect();
                if fields.len() == 4
                    && fields[0] == "<ts>"
                    && !fields[1].is_empty()
                    && fields[1].bytes().all(|b| b.is_ascii_uppercase())
                    && fields[3].ends_with('s')
                    && fields[3].starts_with(|c: char| c.is_ascii_digit())
                {
                    format!("<ts> {} {} <elapsed>", fields[1], fields[2])
                } else {
                    line
                }
            })
            .collect()
    }

    /// SCENARIO.md N2.
    fn normalize_port(s: &str, port: u16) -> String {
        let mut s = s.to_owned();
        for host in ["127.0.0.1", "localhost", "port"] {
            s = s.replace(&format!("{host}:{port}"), &format!("{host}:<port>"));
        }
        s
    }

    /// SCENARIO.md N5: `"lastUpdateTime"` and `"timestamp"` values become `"<t>"`.
    fn normalize_times(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut rest = s;
        'scan: while !rest.is_empty() {
            for key in ["\"lastUpdateTime\":", "\"timestamp\":"] {
                if let Some(after) = rest.strip_prefix(key) {
                    let value_len = if let Some(v) = after.strip_prefix('"') {
                        v.find('"').map(|i| i + 2)
                    } else {
                        let b = after.as_bytes();
                        let start = usize::from(b.first() == Some(&b'-'));
                        if b.get(start).is_some_and(u8::is_ascii_digit) {
                            let n = b[start + 1..]
                                .iter()
                                .take_while(|c| {
                                    c.is_ascii_digit()
                                        || matches!(c, b'.' | b'e' | b'E' | b'+' | b'-')
                                })
                                .count();
                            Some(start + 1 + n)
                        } else {
                            None
                        }
                    };
                    if let Some(len) = value_len {
                        out.push_str(key);
                        out.push_str("\"<t>\"");
                        rest = &after[len..];
                        continue 'scan;
                    }
                }
            }
            let c = rest.chars().next().unwrap();
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
        out
    }

    /// SCENARIO.md N6: UUIDs become `<uuid-N>` in order of first appearance.
    fn normalize_uuids(s: &str) -> String {
        fn is_uuid(b: &[u8]) -> bool {
            b.len() >= 36
                && b[..36].iter().enumerate().all(|(i, c)| {
                    if matches!(i, 8 | 13 | 18 | 23) {
                        *c == b'-'
                    } else {
                        c.is_ascii_hexdigit()
                    }
                })
        }
        let mut seen: Vec<String> = Vec::new();
        let mut out = String::with_capacity(s.len());
        let mut i = 0;
        while i < s.len() {
            if s.is_char_boundary(i) && is_uuid(&s.as_bytes()[i..]) {
                let id = s[i..i + 36].to_ascii_lowercase();
                let n = match seen.iter().position(|u| *u == id) {
                    Some(n) => n + 1,
                    None => {
                        seen.push(id);
                        seen.len()
                    }
                };
                out.push_str(&format!("<uuid-{n}>"));
                i += 36;
            } else {
                let c = s[i..].chars().next().unwrap();
                out.push(c);
                i += c.len_utf8();
            }
        }
        out
    }

    /// The raw request of a fixture (SCENARIO.md "Request").
    fn raw_request(request: &Value, port: u16) -> Vec<u8> {
        let mut raw = format!(
            "{} {} HTTP/1.1\r\n",
            request["method"].as_str().unwrap(),
            request["target"].as_str().unwrap()
        );
        for h in request["headers"].as_array().unwrap() {
            let value = h[1].as_str().unwrap().replace("<port>", &port.to_string());
            raw.push_str(&format!("{}: {value}\r\n", h[0].as_str().unwrap()));
        }
        raw.push_str("\r\n");
        raw.push_str(request["body"].as_str().unwrap());
        raw.into_bytes()
    }

    fn dechunk(mut raw: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let eol = raw.windows(2).position(|w| w == b"\r\n").unwrap();
            let len = usize::from_str_radix(std::str::from_utf8(&raw[..eol]).unwrap(), 16).unwrap();
            raw = &raw[eol + 2..];
            if len == 0 {
                return out;
            }
            out.extend_from_slice(&raw[..len]);
            raw = &raw[len + 2..];
        }
    }

    /// A response read to EOF as the fixture's `response` (N2, N5–N7; header names lowercased
    /// and sorted, as the fixtures' are compared case-insensitively).
    fn parse_response(raw: &[u8], head_request: bool, port: u16) -> Value {
        let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let head = String::from_utf8(raw[..split].to_vec()).unwrap();
        let mut body = raw[split + 4..].to_vec();
        let mut lines = head.split("\r\n");
        let status_line = lines.next().unwrap();
        let (_, rest) = status_line.split_once(' ').unwrap();
        let (status, status_text) = rest.split_once(' ').unwrap();
        let mut headers: Vec<(String, String)> = lines
            .map(|l| {
                let (k, v) = l.split_once(':').unwrap();
                (k.to_owned(), v.trim().to_owned())
            })
            .collect();
        if headers.iter().any(|(k, v)| {
            k.eq_ignore_ascii_case("transfer-encoding") && v.eq_ignore_ascii_case("chunked")
        }) {
            body = dechunk(&body);
        }
        if head_request {
            body.clear();
        }
        let raw_body = String::from_utf8(body.clone()).unwrap();
        let normalized = normalize_uuids(&normalize_times(&normalize_port(&raw_body, port)));
        for (k, v) in &mut headers {
            *v = if k.eq_ignore_ascii_case("date") {
                "<date>".to_owned()
            } else if k.eq_ignore_ascii_case("content-length") && normalized != raw_body {
                "<n>".to_owned()
            } else {
                normalize_port(v, port)
            };
        }
        response_value(
            status.parse().unwrap(),
            status_text,
            headers,
            &normalized,
            body.len(),
        )
    }

    fn response_value(
        status: u64,
        status_text: &str,
        headers: Vec<(String, String)>,
        body: &str,
        body_length: usize,
    ) -> Value {
        let mut headers: Vec<(String, String)> = headers
            .into_iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v))
            .collect();
        headers.sort();
        json!({
            "status": status,
            "status_text": status_text,
            "headers": headers,
            "body": body,
            "body_length": body_length,
        })
    }

    /// The fixture's response in the shape [`parse_response`] produces.
    fn expected_response(fixture: &Value) -> Value {
        let r = &fixture["response"];
        let headers = r["headers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| {
                (
                    h[0].as_str().unwrap().to_owned(),
                    h[1].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        response_value(
            r["status"].as_u64().unwrap(),
            r["status_text"].as_str().unwrap(),
            headers,
            r["body"].as_str().unwrap(),
            usize::try_from(r["body_length"].as_u64().unwrap()).unwrap(),
        )
    }

    fn log_lines(fixture: &Value) -> Vec<String> {
        fixture["log"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l.as_str().unwrap().to_owned())
            .collect()
    }

    /// Starts a web launcher with only the api sub-launcher on the `server`'s `00_startup` flags
    /// (sub-launcher keywords other than `api`, with their flags, dropped); returns the port, the
    /// log capture, the token and the running task.
    async fn start(
        startup: &Value,
    ) -> (
        u16,
        Captured,
        CancellationToken,
        tokio::task::JoinHandle<Result<(), Error>>,
    ) {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let mut args: Vec<String> = Vec::new();
        let mut keep = true;
        for a in &startup["args"].as_array().unwrap()[1..] {
            let a = a.as_str().unwrap().replace("<port>", &port.to_string());
            if ["webui", "a2a", "pubsub", "eventarc"].contains(&a.as_str()) {
                keep = false;
                continue;
            }
            if a == "api" {
                keep = true;
            }
            if keep {
                args.push(a);
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
        config.log = log.with_clock(|| TS.to_owned());
        let cancel = CancellationToken::new();
        let running = {
            let (l, cancel) = (Arc::clone(&l), cancel.clone());
            tokio::spawn(async move { l.run_with(cancel, config, |_| true).await })
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while TcpStream::connect(("127.0.0.1", port)).await.is_err() {
            assert!(Instant::now() < deadline, "the server did not start");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        (port, out, cancel, running)
    }

    /// Replays `exchanges` of one fixture server against an api-only web launcher, byte for byte
    /// after the SCENARIO.md normalizations: the startup log (minus the other sub-launchers'
    /// lines), each response and its log lines, and the shutdown line.
    async fn replay(server: &str, exchanges: &[&str]) {
        let startup = fixture(&format!("{server}_00_startup"));
        let (port, out, cancel, running) = start(&startup).await;

        let want: Vec<String> = log_lines(&startup)
            .into_iter()
            .filter(|l| {
                let rest = l.strip_prefix("<ts> ").unwrap_or(l);
                if rest.starts_with("adk: ") {
                    return false;
                }
                match rest.strip_prefix(' ').map(str::trim_start) {
                    Some(r) => match r.split_once(':') {
                        Some((k, _)) if k.bytes().all(|b| b.is_ascii_alphanumeric()) => k == "api",
                        _ => true,
                    },
                    None => true,
                }
            })
            .collect();
        assert_eq!(
            normalize_log(&out.contents(), port),
            want,
            "{server}: startup log"
        );

        for name in exchanges {
            let f = fixture(name);
            let logged = out.contents().len();
            let mut conn = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            conn.write_all(&raw_request(&f["request"], port))
                .await
                .unwrap();
            let mut raw = Vec::new();
            conn.read_to_end(&mut raw).await.unwrap();
            let head = f["request"]["method"] == "HEAD";
            assert_eq!(
                parse_response(&raw, head, port),
                expected_response(&f),
                "{name}: response"
            );
            assert_eq!(
                normalize_log(&out.contents()[logged..], port),
                log_lines(&f),
                "{name}: log"
            );
        }

        let logged = out.contents().len();
        cancel.cancel();
        running.await.unwrap().unwrap();
        assert_eq!(
            normalize_log(&out.contents()[logged..], port),
            log_lines(&fixture(&format!("{server}_99_shutdown"))),
            "{server}: shutdown log"
        );
    }

    /// The `full` server's exchanges that reach (or pass by) the API mount: origin and CORS
    /// handling, the REST routes under `/api`, segment-aware mounting, trailing slashes, the
    /// redirect rewriter, 405 with `Allow` and the body limit.
    ///
    /// Not here: `full_23`–`full_25` (`/api/health%2F`). Go's REST server registers `/health`
    /// before `SetupSubRouters` turns StrictSlash off, so its decoded path `/health/` gets a
    /// StrictSlash 301 (GET, HEAD) or a 405 with `Allow: GET, HEAD` (POST); gadk-server's
    /// `/health` route has no StrictSlash and answers 404. That difference is the REST server's
    /// (Task 12); the mount's part (the `Location` rewrite and the 308/307 promotion) is covered
    /// by the `api_test.go` ports.
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
