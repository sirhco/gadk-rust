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

//! The web server and its sub-launchers.
//!
//! [`WebLauncher`] takes the `web` keyword's flags and the sub-launcher keywords that follow
//! them, and runs in this order: in-memory service defaults, the router (request
//! logger, `/health`, the request-body limit, the sub-launchers' routes in declaration order, the
//! origin guard), telemetry, the startup log, then the server until the token is cancelled.
//!
//! The active sub-launchers' startup lines are printed in declaration order; the server's
//! timeouts are mapped as `serve` describes.

#[cfg(feature = "a2a")]
pub mod a2a;
#[cfg(feature = "agentengine")]
pub mod agentengine;
pub mod api;
mod router;
mod serve;
#[cfg(feature = "triggers")]
pub mod triggers;
#[cfg(feature = "webui")]
pub mod webui;

pub use self::router::{Middleware, WebRouter, axum_handler};

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::body::Body;
use gadk_platform::go_flag::{Flag, FlagSet};
use gadk_platform::{GoDuration, go_format_duration, go_quote};
use gadk_server::mux::{Handler, Request, Response, handler};
use gadk_server::rest::{max_bytes_middleware, origin_guard_middleware};
use http::header::CONTENT_TYPE;
use http::{HeaderValue, Method};
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::error::Error;
use crate::flag_usage::format_flag_usage;
use crate::golog::Logger;
use crate::launcher::{Launcher, SubLauncher};
use crate::telemetry;
use crate::universal::error_on_unparsed_args;

/// The prefix of the startup log line that dumps the flag values.
const LOG_STARTING_WEB_SERVER: &str = "Starting the web server: ";
/// The prefix of the startup log line with the server URL.
const LOG_WEB_SERVER_STARTS_ON: &str = "Web servers starts on ";
/// The server stays off the network unless `-host` says otherwise.
pub(crate) const DEFAULT_HOST: &str = "127.0.0.1";

/// A sub-launcher of the web launcher: it mounts routes on the web router.
pub trait WebSublauncher: Send + Sync {
    /// The command-line keyword that selects this sub-launcher.
    fn keyword(&self) -> String;
    /// Parses this sub-launcher's flags and returns the unparsed rest.
    fn parse(&mut self, args: Vec<String>) -> Result<Vec<String>, Error>;
    /// This sub-launcher's flags and arguments.
    fn command_line_syntax(&self) -> String;
    /// A one-line description.
    fn simple_description(&self) -> String;
    /// Mounts this sub-launcher's routes.
    fn setup_subrouters(&self, router: &mut WebRouter, config: &mut Config) -> Result<(), Error>;
    /// The startup lines this sub-launcher prints.
    fn user_message(&self, web_url: &str, printer: &mut dyn FnMut(&str));
}

/// The `web` flags' values.
struct WebConfig {
    port: Flag<i64>,
    host: Flag<String>,
    allow_origins: Arc<Mutex<Vec<String>>>,
    write_timeout: Flag<GoDuration>,
    read_timeout: Flag<GoDuration>,
    idle_timeout: Flag<GoDuration>,
    shutdown_timeout: Flag<GoDuration>,
    otel_to_cloud: Flag<bool>,
    use_h2c: Flag<bool>,
    max_payload_size: Flag<i64>,
}

impl WebConfig {
    fn allow_origins(&self) -> Vec<String> {
        lock(&self.allow_origins).clone()
    }

    /// The flag values as `&{port:… host:… …}`, durations as nanosecond integers.
    fn go_dump(&self) -> String {
        format!(
            "&{{port:{} host:{} allowOrigins:[{}] writeTimeout:{} readTimeout:{} idleTimeout:{} shutdownTimeout:{} otelToCloud:{} useH2C:{} maxPayloadSize:{}}}",
            self.port.get(),
            self.host.get(),
            self.allow_origins().join(" "),
            self.write_timeout.get().0,
            self.read_timeout.get().0,
            self.idle_timeout.get().0,
            self.shutdown_timeout.get().0,
            self.otel_to_cloud.get(),
            self.use_h2c.get(),
            self.max_payload_size.get(),
        )
    }
}

/// The `web` launcher: the web server with its sub-launchers.
pub struct WebLauncher {
    flags: Mutex<FlagSet>,
    config: WebConfig,
    sublaunchers: Vec<Box<dyn WebSublauncher>>,
    /// The keywords parsed from the command line.
    active: Vec<String>,
}

/// A `web` launcher over `sublaunchers`, with its flags.
pub fn new(sublaunchers: Vec<Box<dyn WebSublauncher>>) -> WebLauncher {
    let mut fs = FlagSet::new("web");
    let host = fs.string(
        "host",
        DEFAULT_HOST,
        "Host/IP to bind the web server to. Defaults to 127.0.0.1 (loopback only) so the server is not exposed to the network. Use 0.0.0.0 to listen on all interfaces, which may be required when running adk web inside a container. An empty value is treated as the default.",
    );
    let port = fs.int("port", 8080, "Port for the web server");
    let allow_origins = Arc::new(Mutex::new(Vec::new()));
    let origins = Arc::clone(&allow_origins);
    fs.func(
        "allow_origins",
        "Optional. Origins allowed on every route of the web server (e.g., 'https://example.com'; a bare hostname and optional port is read as http). Repeat the flag to allow more than one. Each entry is admitted by the origin check on every route, is allowed for CORS on the REST API, and on a loopback bind also allows requests whose Host header names that entry's host; other cross-origin browser requests are refused with 403. '*' allows any origin and turns the origin and DNS-rebinding checks off on every route - do not use it on a server reachable from an untrusted network, since these endpoints are unauthenticated.",
        Box::new(move |origin: &str| {
            lock(&origins).push(origin.to_owned());
            Ok(())
        }),
    );
    let write_timeout = fs.duration(
        "write-timeout",
        GoDuration::from_secs(15),
        "Server write timeout (e.g. '10s', '2m', '1h30m': decimal numbers with units ns, us, ms, s, m, h) - for writing the response after reading the headers & body",
    );
    let read_timeout = fs.duration(
        "read-timeout",
        GoDuration::from_secs(15),
        "Server read timeout (e.g. '10s', '2m', '1h30m': decimal numbers with units ns, us, ms, s, m, h) - for reading the whole request including body",
    );
    let idle_timeout = fs.duration(
        "idle-timeout",
        GoDuration::from_secs(60),
        "Server idle timeout (e.g. '10s', '2m', '1h30m': decimal numbers with units ns, us, ms, s, m, h) - for waiting for the next request (only when keep-alive is enabled)",
    );
    let shutdown_timeout = fs.duration(
        "shutdown-timeout",
        GoDuration::from_secs(15),
        "Server shutdown timeout (e.g. '10s', '2m', '1h30m': decimal numbers with units ns, us, ms, s, m, h) - for waiting for active requests to finish during shutdown",
    );
    let otel_to_cloud = fs.bool(
        "otel_to_cloud",
        false,
        "Enables OpenTelemetry export to GCP: telemetry.googleapis.com. When not set, a gadk_telemetry::with_otel_to_cloud option in Config::telemetry_options decides. See the gadk-telemetry crate's README for details about supported options, credentials and environment variables.",
    );
    let use_h2c = fs.bool(
        "h2c",
        false,
        "Enable prior-knowledge cleartext HTTP/2 (h2c; no HTTP/1.1 Upgrade) on the web server listener. Cleartext is insecure; do not expose it to untrusted networks. Long-lived streaming responses may require increasing --write-timeout.",
    );
    let max_payload_size = fs.int64(
        "max_request_body_size",
        0,
        "Maximum request body size in bytes. Larger requests are rejected with HTTP 400. 0 uses the default (10 MiB) or a value set by an embedder. Applied to the base router so all sublauncher and trigger routes are covered.",
    );
    WebLauncher {
        flags: Mutex::new(fs),
        config: WebConfig {
            port,
            host,
            allow_origins,
            write_timeout,
            read_timeout,
            idle_timeout,
            shutdown_timeout,
            otel_to_cloud,
            use_h2c,
            max_payload_size,
        },
        sublaunchers,
        active: Vec::new(),
    }
}

impl WebLauncher {
    /// Replaces where the `web` flag set writes its errors and usage (default
    /// stderr), for tests and the CLI replay.
    pub fn with_flag_output(self, out: Arc<Mutex<dyn Write + Send>>) -> WebLauncher {
        lock(&self.flags).set_output(out);
        self
    }

    /// The flag usage text followed by the sub-launchers' keywords and syntax.
    fn syntax(&self) -> String {
        let mut b = format_flag_usage(&lock(&self.flags));
        b.push_str("  You may specify sublaunchers:\n");
        for l in &self.sublaunchers {
            b.push_str(&format!(
                "    * {} - {}\n",
                l.keyword(),
                l.simple_description()
            ));
        }
        b.push_str("  Sublaunchers syntax:\n");
        for l in &self.sublaunchers {
            b.push_str(&format!(
                "    {}\n  {}\n",
                l.keyword(),
                l.command_line_syntax()
            ));
        }
        b
    }

    /// `-host`, or the default when it is empty.
    pub(crate) fn bind_host(&self) -> String {
        bind_host_of(&self.config.host.get())
    }

    /// The listen address: `host:port` (IPv6 bracketed).
    pub(crate) fn server_addr(&self) -> String {
        join_host_port(&self.bind_host(), self.config.port.get())
    }

    /// The URL the server is reached at.
    pub(crate) fn web_url(&self) -> String {
        web_url(&self.config.host.get(), self.config.port.get())
    }

    /// The server options from the flags.
    pub(crate) fn serve_options(&self) -> serve::Options {
        serve::Options::new(
            self.config.read_timeout.get(),
            self.config.write_timeout.get(),
            self.config.idle_timeout.get(),
            self.config.use_h2c.get(),
        )
    }

    /// The active sub-launchers, in declaration order.
    fn active_sublaunchers(&self) -> impl Iterator<Item = &dyn WebSublauncher> {
        self.sublaunchers
            .iter()
            .filter(|l| self.active.contains(&l.keyword()))
            .map(|l| &**l)
    }

    /// The base router with `/health` and the body limit, the active
    /// sub-launchers' routes (declaration order), then the origin guard over all of them.
    pub(crate) fn build_router(&self, config: &mut Config) -> Result<WebRouter, Error> {
        let mut router = build_base_router(&config.log);
        register_health_route(&mut router);

        // The flag overrides an embedder's value only when set; the resolved value limits the
        // base router too.
        let max_payload_size = self.config.max_payload_size.get();
        if max_payload_size > 0 {
            config.max_payload_size = max_payload_size;
        }
        router.use_middleware(max_bytes_middleware(config.max_payload_size));

        if self.active.is_empty() {
            let available: Vec<String> = self.sublaunchers.iter().map(|l| l.keyword()).collect();
            return Err(Error::Config(format!(
                "no active sublaunchers found - please specify them in the command line. Possible values: [{}]",
                available.join(" ")
            )));
        }

        config.bind_host = self.bind_host();
        config.allowed_origins.extend(self.config.allow_origins());

        for l in self.active_sublaunchers() {
            l.setup_subrouters(&mut router, config).map_err(|e| {
                Error::Config(format!("{} subrouter setup failed: {e}", l.keyword()))
            })?;
        }

        // Built after setup: sub-launchers add origins while they run. It still wraps every
        // route above.
        router.use_middleware(origin_guard_middleware(
            &config.bind_host,
            &config.allowed_origins,
        ));
        Ok(router)
    }

    /// Runs with the global subscriber installation injected (`install` returns `false` when
    /// one is already set) over an empty environment, leaving the global OpenTelemetry providers
    /// alone: tests never touch the process-global dispatcher, providers or environment.
    #[cfg(test)]
    pub(crate) async fn run_with(
        &self,
        cancel: CancellationToken,
        config: Config,
        install: impl FnOnce(tracing::Dispatch) -> bool + Send + 'static,
    ) -> Result<(), Error> {
        self.run_in(cancel, config, telemetry::Globals::injected(install))
            .await
    }

    /// Runs over the telemetry [`telemetry::Globals`] given: services, router, telemetry,
    /// then the server; telemetry is shut down under `-shutdown-timeout` however the server ends.
    pub(crate) async fn run_in(
        &self,
        cancel: CancellationToken,
        mut config: Config,
        globals: telemetry::Globals,
    ) -> Result<(), Error> {
        apply_service_defaults(&mut config);
        let router = self.build_router(&mut config)?;
        let telemetry = telemetry::init_with(&mut config, self.config.otel_to_cloud.get(), globals)
            .await
            .map_err(|e| Error::Telemetry(format!("telemetry initialization failed: {e}")))?;
        let result = self.serve(cancel, &config.log, router).await;
        let timeout = self
            .config
            .shutdown_timeout
            .get()
            .to_std()
            .unwrap_or_default();
        telemetry.shutdown_logged(timeout, &config.log).await;
        result
    }

    /// The startup log, then the server until `cancel` fires and its graceful shutdown.
    async fn serve(
        &self,
        cancel: CancellationToken,
        log: &Logger,
        router: WebRouter,
    ) -> Result<(), Error> {
        log.print(&format!(
            "{LOG_STARTING_WEB_SERVER}{}",
            self.config.go_dump()
        ));
        log.println_empty();
        let web_url = self.web_url();
        log.print(&format!("{LOG_WEB_SERVER_STARTS_ON}{web_url}"));
        for l in self.active_sublaunchers() {
            l.user_message(&web_url, &mut |line: &str| log.print(line));
        }
        log.println_empty();

        let listener = serve::listen(&self.server_addr())
            .await
            .map_err(|e| Error::Serve(format!("server failed: {e}")))?;
        let draining =
            serve::serve(listener, router, self.serve_options(), cancel, log.clone()).await;
        log.print("Shutting down the web server...");
        draining
            .shutdown(self.config.shutdown_timeout.get())
            .await
            .map_err(Error::Serve)
    }
}

#[async_trait::async_trait]
impl SubLauncher for WebLauncher {
    fn keyword(&self) -> String {
        "web".to_owned()
    }

    /// Parses the `web` flags, then each known sub-launcher keyword with its own flags,
    /// until an argument that is not a known keyword.
    fn parse(&mut self, args: Vec<String>) -> Result<Vec<String>, Error> {
        let mut keywords: Vec<String> = Vec::with_capacity(self.sublaunchers.len());
        for l in &self.sublaunchers {
            let keyword = l.keyword();
            if keywords.contains(&keyword) {
                return Err(Error::Parse(format!(
                    "cannot create web launcher. Keywords for sublaunchers should be unique and they are not: '{keyword}'"
                )));
            }
            keywords.push(keyword);
        }

        let mut rest = {
            let mut fs = lock(&self.flags);
            if let Err(e) = fs.parse(&args) {
                return Err(Error::Parse(format!("failed to parse web flags: {e}")));
            }
            fs.args()
        };

        self.active.clear();
        while let Some(keyword) = rest.first().cloned() {
            if self.active.contains(&keyword) {
                return Err(Error::Parse(format!(
                    "the keyword {} is specified and processed more than once, which is not allowed",
                    go_quote(&keyword)
                )));
            }
            let Some(i) = keywords.iter().position(|k| *k == keyword) else {
                // Not a known keyword: left for the caller.
                break;
            };
            rest = self.sublaunchers[i]
                .parse(rest[1..].to_vec())
                .map_err(|e| {
                    Error::Parse(format!(
                        "the {} launcher cannot parse arguments: {e}",
                        go_quote(&keyword)
                    ))
                })?;
            self.active.push(keyword);
        }
        Ok(rest)
    }

    fn command_line_syntax(&self) -> String {
        self.syntax()
    }

    fn simple_description(&self) -> String {
        "starts web server with additional sub-servers specified by sublaunchers".to_owned()
    }

    /// Serves until `cancel` is cancelled.
    async fn run(&self, cancel: CancellationToken, config: Config) -> Result<(), Error> {
        self.run_in(cancel, config, telemetry::Globals::process())
            .await
    }
}

#[async_trait::async_trait]
impl Launcher for WebLauncher {
    /// Validates, parses, refuses leftovers, runs.
    async fn execute(
        &mut self,
        cancel: CancellationToken,
        config: Config,
        args: Vec<String>,
    ) -> Result<(), Error> {
        config.validate()?;
        let remaining = SubLauncher::parse(self, args).map_err(|e| Error::Wrapped {
            context: "cannot parse args".to_owned(),
            source: Box::new(e),
        })?;
        error_on_unparsed_args(&remaining).map_err(|e| Error::Wrapped {
            context: "cannot parse all the arguments".to_owned(),
            source: Box::new(e),
        })?;
        SubLauncher::run(self, cancel, config).await
    }

    fn command_line_syntax(&self) -> String {
        self.syntax()
    }
}

/// A strict-slash router (`WebRouter::new(true)`) plus the request logger writing to `log`. It
/// registers no routes (a route here would shadow the caller's route for the same path).
pub fn build_base_router(log: &Logger) -> WebRouter {
    let mut router = WebRouter::new(true);
    router.use_middleware(request_logger(log.clone()));
    router
}

/// After the handler, logs `<method> <RequestURI> <elapsed>` with the elapsed time as a
/// duration such as `1.5ms`. The line is logged when the response body ends or is dropped (a
/// streamed response: after its last frame).
///
/// A WebSocket upgrade is logged when its 101 is sent, not when the session ends.
fn request_logger(log: Logger) -> Middleware {
    Arc::new(move |next: Handler| {
        let log = log.clone();
        handler(move |req: Request| {
            let next = Arc::clone(&next);
            let log = log.clone();
            async move {
                let start = Instant::now();
                let line = format!("{} {}", req.method(), router::go_request_uri(&req));
                let resp = next(req).await;
                resp.map(|body| {
                    Body::new(LoggedBody {
                        inner: body,
                        pending: Some((log, line, start)),
                    })
                })
            }
        })
    })
}

/// A response body that writes the request log line once it ends, fails or is dropped.
struct LoggedBody {
    inner: Body,
    pending: Option<(Logger, String, Instant)>,
}

impl LoggedBody {
    fn log(&mut self) {
        if let Some((log, line, start)) = self.pending.take() {
            log.print(&format!(
                "{line} {}",
                go_format_duration(GoDuration::from_std(start.elapsed()))
            ));
        }
    }
}

impl Drop for LoggedBody {
    fn drop(&mut self) {
        self.log();
    }
}

impl hyper::body::Body for LoggedBody {
    type Data = bytes::Bytes;
    type Error = axum::Error;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        let frame = std::pin::Pin::new(&mut this.inner).poll_frame(cx);
        if let std::task::Poll::Ready(None | Some(Err(_))) = &frame {
            this.log();
        }
        frame
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        self.inner.size_hint()
    }
}

/// `/health` for GET and HEAD.
pub(crate) fn register_health_route(router: &mut WebRouter) {
    router.handle(&[Method::GET, Method::HEAD], "/health", health_handler());
}

/// `{"status":"ok"}` and a newline as `application/json` (the body and type of the REST
/// server's `/api/health`).
fn health_handler() -> Handler {
    handler(|_req: Request| async {
        let mut resp = Response::new(Body::from("{\"status\":\"ok\"}\n"));
        resp.headers_mut()
            .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        resp
    })
}

/// In-memory session, artifact and memory services where the caller
/// left them unset, each logged.
pub(crate) fn apply_service_defaults(config: &mut Config) {
    let mut defaulted = Vec::new();
    if config.session_service.is_none() {
        config.session_service = Some(Arc::new(gadk_session::InMemorySessionService::new()));
        defaulted.push("session");
    }
    if config.artifact_service.is_none() {
        config.artifact_service = Some(Arc::new(gadk_artifact::InMemoryArtifactService::new()));
        defaulted.push("artifact");
    }
    if config.memory_service.is_none() {
        config.memory_service = Some(Arc::new(gadk_memory::InMemoryMemoryService::new()));
        defaulted.push("memory");
    }
    for name in defaulted {
        config.log.print(&format!(
            "No {name} service configured. Using an in-memory one, so whatever it holds is lost when the process exits."
        ));
    }
}

/// The bind host for a `-host` value.
fn bind_host_of(host: &str) -> String {
    if host.is_empty() {
        DEFAULT_HOST.to_owned()
    } else {
        host.to_owned()
    }
}

/// Loopback-ish listen hosts shown as `localhost`.
fn display_host(host: &str) -> &str {
    match host {
        "127.0.0.1" | "::1" | "0.0.0.0" | "::" => "localhost",
        other => other,
    }
}

/// `host:port`, with an IPv6 host in brackets.
fn join_host_port(host: &str, port: i64) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// The server URL for a `-host` value and port.
pub(crate) fn web_url(host: &str, port: i64) -> String {
    format!(
        "http://{}",
        join_host_port(display_host(&bind_host_of(host)), port)
    )
}

fn lock<T: ?Sized>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Test access to the server (H2C and serving tests in `parity_tests`).
#[cfg(test)]
pub(crate) mod testing {
    pub(crate) use super::serve::{listen, serve};
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::golog::Captured;
    use crate::test_support::{self as ts, NoAgents, strings};

    /// The fixed `golog` clock of these tests.
    const TS: &str = "2009/11/10 23:00:00 ";

    fn test_config() -> (Config, Captured) {
        let (log, out) = crate::golog::capture();
        let mut config = Config::new(Arc::new(NoAgents));
        config.log = log.with_clock(|| TS.to_owned());
        (config, out)
    }

    /// A sub-launcher with flags-free parsing, no routes and one user-message line.
    struct Quiet {
        keyword: &'static str,
        syntax: &'static str,
        fail_parse: bool,
    }

    fn quiet(keyword: &'static str) -> Box<dyn WebSublauncher> {
        Box::new(Quiet {
            keyword,
            syntax: "",
            fail_parse: false,
        })
    }

    impl WebSublauncher for Quiet {
        fn keyword(&self) -> String {
            self.keyword.to_owned()
        }
        fn parse(&mut self, args: Vec<String>) -> Result<Vec<String>, Error> {
            if self.fail_parse {
                return Err(Error::Parse("bad".to_owned()));
            }
            Ok(args)
        }
        fn command_line_syntax(&self) -> String {
            self.syntax.to_owned()
        }
        fn simple_description(&self) -> String {
            format!("the {} sublauncher", self.keyword)
        }
        fn setup_subrouters(&self, _: &mut WebRouter, _: &mut Config) -> Result<(), Error> {
            if self.keyword == "broken" {
                return Err(Error::Config("no".to_owned()));
            }
            Ok(())
        }
        fn user_message(&self, web_url: &str, printer: &mut dyn FnMut(&str)) {
            printer(&format!("       {}:  at {web_url}", self.keyword));
        }
    }

    #[test]
    fn parse_activates_keywords_and_leaves_the_rest() {
        let mut l = new(vec![quiet("a"), quiet("b")]);
        let rest =
            SubLauncher::parse(&mut l, strings(&["-port", "9", "b", "a", "x", "a"])).unwrap();
        assert_eq!(rest, strings(&["x", "a"]));
        assert_eq!(l.active, strings(&["b", "a"]));
        assert_eq!(l.config.port.get(), 9);
    }

    #[test]
    fn parse_errors_are_go_text() {
        let mut dup = new(vec![quiet("a"), quiet("a")]);
        assert_eq!(
            SubLauncher::parse(&mut dup, Vec::new())
                .unwrap_err()
                .to_string(),
            "cannot create web launcher. Keywords for sublaunchers should be unique and they are not: 'a'"
        );
        let mut l = new(vec![quiet("a")]).with_flag_output(Arc::new(Mutex::new(Vec::<u8>::new())));
        assert_eq!(
            SubLauncher::parse(&mut l, strings(&["-nope"]))
                .unwrap_err()
                .to_string(),
            "failed to parse web flags: flag provided but not defined: -nope"
        );
        let mut l = new(vec![quiet("a")]);
        assert_eq!(
            SubLauncher::parse(&mut l, strings(&["a", "a"]))
                .unwrap_err()
                .to_string(),
            "the keyword \"a\" is specified and processed more than once, which is not allowed"
        );
        let mut l = new(vec![Box::new(Quiet {
            keyword: "a",
            syntax: "",
            fail_parse: true,
        })]);
        assert_eq!(
            SubLauncher::parse(&mut l, strings(&["a"]))
                .unwrap_err()
                .to_string(),
            "the \"a\" launcher cannot parse arguments: bad"
        );
    }

    #[tokio::test]
    async fn execute_wraps_parse_errors_and_refuses_leftovers() {
        let (config, _) = test_config();
        let mut l = new(vec![quiet("a")]).with_flag_output(Arc::new(Mutex::new(Vec::<u8>::new())));
        let err = l
            .execute(CancellationToken::new(), config, strings(&["-port", "x"]))
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "cannot parse args: failed to parse web flags: invalid value \"x\" for flag -port: parse error"
        );
        let (config, _) = test_config();
        let err = l
            .execute(CancellationToken::new(), config, strings(&["a", "extra"]))
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "cannot parse all the arguments: cannot parse following arguments: [extra]"
        );
    }

    #[tokio::test]
    async fn run_errors_are_go_text() {
        let (config, out) = test_config();
        let mut l = new(vec![quiet("a"), quiet("b")]);
        SubLauncher::parse(&mut l, Vec::new()).unwrap();
        let err = l.run(CancellationToken::new(), config).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "no active sublaunchers found - please specify them in the command line. Possible values: [a b]"
        );
        // The service defaults were applied (and logged) first.
        assert_eq!(out.contents().lines().count(), 3);

        let (config, _) = test_config();
        let mut l = new(vec![quiet("broken")]);
        SubLauncher::parse(&mut l, strings(&["broken"])).unwrap();
        let err = l.run(CancellationToken::new(), config).await.unwrap_err();
        assert_eq!(err.to_string(), "broken subrouter setup failed: no");

        // A resource whose schema URL conflicts with the default resource's fails telemetry
        // initialization without touching the network (Go web_test.go:176).
        let (mut config, _) = test_config();
        config.telemetry_options = vec![gadk_telemetry::with_resource(
            opentelemetry_sdk::Resource::builder_empty()
                .with_schema_url(
                    Vec::<opentelemetry::KeyValue>::new(),
                    "https://conflicting.invalid/schema/v1",
                )
                .build(),
        )];
        let mut l = new(vec![quiet("a")]);
        SubLauncher::parse(&mut l, strings(&["a"])).unwrap();
        let err = l
            .run_with(CancellationToken::new(), config, |_| true)
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "telemetry initialization failed: failed to resolve resource: failed to merge with config resource: conflicting Schema URL: https://opentelemetry.io/schemas/1.43.0 and https://conflicting.invalid/schema/v1"
        );
    }

    #[test]
    fn command_line_syntax_layout_is_go() {
        let l = new(vec![
            Box::new(Quiet {
                keyword: "a",
                syntax: "  -x string\n    \tx\n",
                fail_parse: false,
            }),
            quiet("b"),
        ]);
        let syntax = Launcher::command_line_syntax(&l);
        let defaults = l.flags.lock().unwrap().print_defaults();
        assert!(
            defaults.starts_with("  -allow_origins value\n"),
            "{defaults}"
        );
        assert!(
            defaults.contains("  -port int\n    \tPort for the web server (default 8080)\n"),
            "{defaults}"
        );
        assert_eq!(
            syntax,
            format!(
                "{defaults}  You may specify sublaunchers:\n    * a - the a sublauncher\n    * b - the b sublauncher\n  Sublaunchers syntax:\n    a\n    -x string\n    \tx\n\n    b\n  \n"
            )
        );
        assert_eq!(SubLauncher::command_line_syntax(&l), syntax);
        assert_eq!(l.keyword(), "web");
        assert_eq!(
            l.simple_description(),
            "starts web server with additional sub-servers specified by sublaunchers"
        );
    }

    #[test]
    fn config_dump_is_go_percent_plus_v() {
        let mut l = new(Vec::new());
        assert_eq!(
            l.config.go_dump(),
            "&{port:8080 host:127.0.0.1 allowOrigins:[] writeTimeout:15000000000 readTimeout:15000000000 idleTimeout:60000000000 shutdownTimeout:15000000000 otelToCloud:false useH2C:false maxPayloadSize:0}"
        );
        SubLauncher::parse(
            &mut l,
            strings(&[
                "-host=",
                "-allow_origins",
                "a",
                "-allow_origins",
                "b",
                "-h2c",
                "-read-timeout",
                "1ms",
            ]),
        )
        .unwrap();
        assert_eq!(
            l.config.go_dump(),
            "&{port:8080 host: allowOrigins:[a b] writeTimeout:15000000000 readTimeout:1000000 idleTimeout:60000000000 shutdownTimeout:15000000000 otelToCloud:false useH2C:true maxPayloadSize:0}"
        );
    }

    // ---- Task 2's web fixtures (SCENARIO.md), for the exchanges the web core answers alone ----

    /// The `full` server's exchanges, startup and shutdown that the web core answers without the
    /// api/webui/a2a/trigger sub-launchers (`tests/wire_replay.rs` replays the rest), byte for
    /// byte after the SCENARIO.md normalizations: health, the gorilla 405/404 and clean-path 301,
    /// StrictSlash, the origin guard's host check, the request log, the startup banner and the
    /// shutdown line.
    #[tokio::test]
    async fn the_web_core_matches_go_wire_fixtures() {
        let startup = ts::wire("full_00_startup");
        let port = ts::free_port();
        // The fixture's `web` flags; a quiet sub-launcher stands in for the five real ones.
        let args: Vec<String> = ts::args(&startup, port)[1..]
            .iter()
            .take_while(|a| *a != "api")
            .cloned()
            .chain(["fake".to_owned()])
            .collect();
        let mut l = new(vec![Box::new(Quiet {
            keyword: "fake",
            syntax: "",
            fail_parse: false,
        }) as Box<dyn WebSublauncher>]);
        assert_eq!(
            SubLauncher::parse(&mut l, args).unwrap(),
            Vec::<String>::new()
        );
        let l = Arc::new(l);
        let (config, out) = test_config();
        let cancel = CancellationToken::new();
        let running = {
            let (l, cancel) = (Arc::clone(&l), cancel.clone());
            tokio::spawn(async move { l.run_with(cancel, config, |_| true).await })
        };
        ts::wait_listening(port).await;

        // Startup: every Go line but the sub-launchers' own (trigger warnings, user messages).
        let got: Vec<String> = ts::normalize_log(&out.contents(), port)
            .into_iter()
            .filter(|l| !ts::is_line_of_other(l, &[]))
            .collect();
        assert_eq!(got, ts::startup_log_of(&startup, &[]), "startup log");

        for name in [
            "full_01_health_get",
            "full_02_health_head",
            "full_03_health_wrong_method",
            "full_04_health_trailing_slash",
            "full_05_non_clean_path",
            "full_06_non_clean_path_post",
            "full_07_unmatched_path",
            "full_08_unmatched_path_cross_origin",
            "full_12_host_forbidden",
        ] {
            ts::assert_exchange(port, name, || out.contents()).await;
        }

        let logged = out.contents().len();
        cancel.cancel();
        running.await.unwrap().unwrap();
        assert_eq!(
            ts::normalize_log(&out.contents()[logged..], port),
            ts::log_lines(&ts::wire("full_99_shutdown")),
            "shutdown log"
        );
    }

    /// Go's logger logs after `ServeHTTP` returns: a streamed response is logged after its last
    /// chunk, with the whole time taken.
    #[tokio::test]
    async fn a_streamed_response_is_logged_when_it_ends() {
        let (log, captured) = crate::golog::capture();
        let mut router = build_base_router(&log);
        router.handle(
            &[Method::GET],
            "/stream",
            handler(|_req: Request| async {
                let chunks = futures::stream::unfold(0, |n| async move {
                    match n {
                        0 => Some((Ok::<_, std::io::Error>(bytes::Bytes::from("a")), 1)),
                        1 => {
                            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                            Some((Ok(bytes::Bytes::from("b")), 2))
                        }
                        _ => None,
                    }
                });
                Response::new(Body::from_stream(chunks))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let cancel = CancellationToken::new();
        let opts = serve::Options::new(
            GoDuration::from_secs(5),
            GoDuration::from_secs(5),
            GoDuration::from_secs(5),
            false,
        );
        tokio::spawn(testing::serve(listener, router, opts, cancel.clone(), log));
        let resp = reqwest::get(format!("http://{addr}/stream")).await.unwrap();
        assert!(
            !captured.contents().contains("GET /stream"),
            "logged before the body: {}",
            captured.contents()
        );
        assert_eq!(resp.text().await.unwrap(), "ab");
        let mut line = None;
        for _ in 0..100 {
            line = captured
                .contents()
                .lines()
                .find(|l| l.contains("GET /stream"))
                .map(str::to_owned);
            if line.is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let line = line.expect("the request was logged");
        let elapsed = line.rsplit_once(' ').unwrap().1;
        let elapsed = gadk_platform::go_parse_duration(elapsed).unwrap();
        assert!(elapsed.0 >= 300_000_000, "{line}");
        cancel.cancel();
    }

    /// Go's HTTP/2 requests carry only `:path` (`RequestURI` and `URL` have no scheme or host):
    /// the request log line and redirect `Location`s are origin-form under `-h2c`.
    #[tokio::test]
    async fn h2c_requests_log_and_redirect_with_the_path_only() {
        let (log, captured) = crate::golog::capture();
        let mut router = build_base_router(&log);
        register_health_route(&mut router);
        router.handle(
            &[Method::GET],
            "/foo/",
            handler(|_req: Request| async { Response::new(Body::empty()) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let cancel = CancellationToken::new();
        let opts = serve::Options::new(
            GoDuration::from_secs(5),
            GoDuration::from_secs(5),
            GoDuration::from_secs(5),
            true,
        );
        let server = tokio::spawn(testing::serve(listener, router, opts, cancel.clone(), log));
        let client = reqwest::Client::builder()
            .http2_prior_knowledge()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();

        let resp = client
            .get(format!("http://{addr}/health?x=1"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.version(), http::Version::HTTP_2);
        assert_eq!(resp.status(), 200);
        let resp = client
            .get(format!("http://{addr}/foo?y=2"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 301);
        assert_eq!(resp.headers()[http::header::LOCATION], "/foo/?y=2");

        cancel.cancel();
        let draining = server.await.unwrap();
        draining.shutdown(GoDuration::from_secs(5)).await.unwrap();
        let logged: Vec<String> = captured
            .contents()
            .lines()
            .map(|l| l.rsplit_once(' ').map_or(l, |(head, _)| head).to_owned())
            .collect();
        assert_eq!(
            logged
                .iter()
                .filter(|l| l.contains("GET "))
                .map(|l| &l[l.find("GET ").unwrap()..])
                .collect::<Vec<_>>(),
            ["GET /health?x=1", "GET /foo?y=2"],
            "{logged:?}"
        );
    }
}
