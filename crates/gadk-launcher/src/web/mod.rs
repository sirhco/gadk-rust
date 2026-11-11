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

//! Go `cmd/launcher/web`: the web server and its sub-launchers.
//!
//! [`WebLauncher`] is Go's `webLauncher`: the `web` keyword's flags, the sub-launcher keywords
//! that follow them, and `Run` in Go's order — in-memory service defaults, the router (request
//! logger, `/health`, the request-body limit, the sub-launchers' routes in declaration order, the
//! origin guard), telemetry, the startup log, then the server until the token is cancelled.
//!
//! Divergences (ledger 6): the active sub-launchers' `UserMessage` lines are printed in
//! declaration order (Go: map order); the server's timeouts are mapped as `serve` describes.

#[cfg(feature = "a2a")]
pub mod a2a;
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
use tracing::Dispatch;

use crate::config::Config;
use crate::error::Error;
use crate::flag_usage::format_flag_usage;
use crate::golog::Logger;
use crate::launcher::{Launcher, SubLauncher};
use crate::telemetry;
use crate::universal::error_on_unparsed_args;

/// Go `logStartingWebServer`.
const LOG_STARTING_WEB_SERVER: &str = "Starting the web server: ";
/// Go `logWebServerStartsOn`.
const LOG_WEB_SERVER_STARTS_ON: &str = "Web servers starts on ";
/// Go `defaultHost`: the server stays off the network unless `-host` says otherwise.
pub(crate) const DEFAULT_HOST: &str = "127.0.0.1";

/// Go `web.Sublauncher`.
pub trait WebSublauncher: Send + Sync {
    /// Go `Keyword()`.
    fn keyword(&self) -> String;
    /// Go `Parse(args)`: parses this sub-launcher's flags and returns the unparsed rest.
    fn parse(&mut self, args: Vec<String>) -> Result<Vec<String>, Error>;
    /// Go `CommandLineSyntax()`.
    fn command_line_syntax(&self) -> String;
    /// Go `SimpleDescription()`.
    fn simple_description(&self) -> String;
    /// Go `SetupSubrouters(router, config)`: mounts this sub-launcher's routes.
    fn setup_subrouters(&self, router: &mut WebRouter, config: &mut Config) -> Result<(), Error>;
    /// Go `UserMessage(webURL, printer)`: the startup lines this sub-launcher prints.
    fn user_message(&self, web_url: &str, printer: &mut dyn FnMut(&str));
}

/// Go `webConfig`: the `web` flags' values.
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

    /// Go's `%+v` of the `*webConfig` (unexported durations print as nanosecond integers).
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

/// Go `web.webLauncher`.
pub struct WebLauncher {
    flags: Mutex<FlagSet>,
    config: WebConfig,
    sublaunchers: Vec<Box<dyn WebSublauncher>>,
    /// The keywords parsed from the command line (Go `activeSublaunchers`).
    active: Vec<String>,
}

/// Go `web.NewLauncher(sublaunchers...)`.
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
        "Server write timeout (i.e. '10s', '2m' - see time.ParseDuration for details) - for writing the response after reading the headers & body",
    );
    let read_timeout = fs.duration(
        "read-timeout",
        GoDuration::from_secs(15),
        "Server read timeout (i.e. '10s', '2m' - see time.ParseDuration for details) - for reading the whole request including body",
    );
    let idle_timeout = fs.duration(
        "idle-timeout",
        GoDuration::from_secs(60),
        "Server idle timeout (i.e. '10s', '2m' - see time.ParseDuration for details) - for waiting for the next request (only when keep-alive is enabled)",
    );
    let shutdown_timeout = fs.duration(
        "shutdown-timeout",
        GoDuration::from_secs(15),
        "Server shutdown timeout (i.e. '10s', '2m' - see time.ParseDuration for details) - for waiting for active requests to finish during shutdown",
    );
    let otel_to_cloud = fs.bool(
        "otel_to_cloud",
        false,
        "Enables OpenTelemetry export to GCP: telemetry.googleapis.com. When not set, a telemetry.WithOtelToCloud option in launcher.Config.TelemetryOptions decides. See adk-go/telemetry package for details about supported options, credentials and environment variables.",
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
    /// Replaces where the `web` flag set writes its errors and usage (Go `fs.SetOutput`; default
    /// stderr), for tests and the CLI replay.
    pub fn with_flag_output(self, out: Arc<Mutex<dyn Write + Send>>) -> WebLauncher {
        lock(&self.flags).set_output(out);
        self
    }

    /// Go `CommandLineSyntax()`.
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

    /// Go `bindHost()`: `-host`, or the default when it is empty.
    pub(crate) fn bind_host(&self) -> String {
        bind_host_of(&self.config.host.get())
    }

    /// Go `buildHTTPServer(..).Addr`: `host:port` (IPv6 bracketed).
    pub(crate) fn server_addr(&self) -> String {
        join_host_port(&self.bind_host(), self.config.port.get())
    }

    /// Go `webURL()`.
    pub(crate) fn web_url(&self) -> String {
        web_url(&self.config.host.get(), self.config.port.get())
    }

    /// The server options from the flags (Go `buildHTTPServer`).
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

    /// Go `buildRouter(config)`: the base router with `/health` and the body limit, the active
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

        // Built after setup: sub-launchers add origins while they run. gorilla builds the
        // middleware chain when a route matches, so this still wraps every route above.
        router.use_middleware(origin_guard_middleware(
            &config.bind_host,
            &config.allowed_origins,
        ));
        Ok(router)
    }

    /// Go `Run` with the global subscriber installation injected (`install` returns `false` when
    /// one is already set); tests never touch the process-global dispatcher.
    pub(crate) async fn run_with(
        &self,
        cancel: CancellationToken,
        mut config: Config,
        install: impl FnOnce(Dispatch) -> bool + Send,
    ) -> Result<(), Error> {
        apply_service_defaults(&mut config);
        let router = self.build_router(&mut config)?;
        let telemetry = telemetry::init_with(&mut config, self.config.otel_to_cloud.get(), install)
            .map_err(|e| Error::Telemetry(format!("telemetry initialization failed: {e}")))?;
        let result = self.serve(cancel, &config.log, router).await;
        if let Err(e) = telemetry.shutdown() {
            config.log.print(&format!("telemetry shutdown failed: {e}"));
        }
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

    /// Go `Parse`: the `web` flags, then each known sub-launcher keyword with its own flags,
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

    /// Go `Run`: serves until `cancel` is cancelled.
    async fn run(&self, cancel: CancellationToken, config: Config) -> Result<(), Error> {
        self.run_with(cancel, config, |dispatch| {
            tracing::dispatcher::set_global_default(dispatch).is_ok()
        })
        .await
    }
}

#[async_trait::async_trait]
impl Launcher for WebLauncher {
    /// Go `webLauncher.Execute`: validate, parse, refuse leftovers, run.
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

/// Go `BuildBaseRouter()`: StrictSlash(true) + the request logger writing to `log`. It registers
/// no routes (a route here would shadow the caller's route for the same path).
pub fn build_base_router(log: &Logger) -> WebRouter {
    let mut router = WebRouter::new(true);
    router.use_middleware(request_logger(log.clone()));
    router
}

/// Go `logger`: after the handler, logs `<method> <RequestURI> <elapsed>` (Go `time.Duration`).
fn request_logger(log: Logger) -> Middleware {
    Arc::new(move |next: Handler| {
        let log = log.clone();
        handler(move |req: Request| {
            let next = Arc::clone(&next);
            let log = log.clone();
            async move {
                let start = Instant::now();
                let method = req.method().to_string();
                let request_uri = router::go_request_uri(&req).to_string();
                let resp = next(req).await;
                log.print(&format!(
                    "{method} {request_uri} {}",
                    go_format_duration(GoDuration::from_std(start.elapsed()))
                ));
                resp
            }
        })
    })
}

/// Go `registerHealthRoute`: `/health` for GET and HEAD.
pub(crate) fn register_health_route(router: &mut WebRouter) {
    router.handle(&[Method::GET, Method::HEAD], "/health", health_handler());
}

/// Go `healthHandler`: `{"status":"ok"}` and a newline as `application/json` (adkrest's
/// `/api/health` body and type).
fn health_handler() -> Handler {
    handler(|_req: Request| async {
        let mut resp = Response::new(Body::from("{\"status\":\"ok\"}\n"));
        resp.headers_mut()
            .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        resp
    })
}

/// Go `applyServiceDefaults`: in-memory session, artifact and memory services where the caller
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

/// Go `bindHost()` for a `-host` value.
fn bind_host_of(host: &str) -> String {
    if host.is_empty() {
        DEFAULT_HOST.to_owned()
    } else {
        host.to_owned()
    }
}

/// Go `displayHost`: loopback-ish listen hosts shown as `localhost`.
fn display_host(host: &str) -> &str {
    match host {
        "127.0.0.1" | "::1" | "0.0.0.0" | "::" => "localhost",
        other => other,
    }
}

/// Go `net.JoinHostPort(host, strconv.Itoa(port))`.
fn join_host_port(host: &str, port: i64) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// Go `webURL()` for a `-host` value and port.
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
    use std::time::Duration;

    use pretty_assertions::assert_eq;
    use serde_json::{Value, json};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    use super::*;
    use crate::golog::Captured;

    /// The fixed `golog` clock of these tests.
    const TS: &str = "2009/11/10 23:00:00 ";

    struct NoAgents;

    impl gadk_agent::Loader for NoAgents {
        fn list_agents(&self) -> Vec<String> {
            Vec::new()
        }
        fn load_agent(
            &self,
            name: &str,
        ) -> Result<Arc<dyn gadk_agent::Agent>, gadk_agent::LoaderError> {
            Err(gadk_agent::LoaderError(format!("no agent {name}")))
        }
        fn root_agent(&self) -> Arc<dyn gadk_agent::Agent> {
            unimplemented!("the web tests load no agent")
        }
    }

    fn test_config() -> (Config, Captured) {
        let (log, out) = crate::golog::capture();
        let mut config = Config::new(Arc::new(NoAgents));
        config.log = log.with_clock(|| TS.to_owned());
        (config, out)
    }

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_owned()).collect()
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

        let (config, _) = test_config();
        let mut l = new(vec![quiet("a")]);
        SubLauncher::parse(&mut l, strings(&["-otel_to_cloud", "a"])).unwrap();
        let err = l.run(CancellationToken::new(), config).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "telemetry initialization failed: -otel_to_cloud requires OpenTelemetry export, which gadk adds in sub-project 7"
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
                // N3: `<ts> METHOD URI elapsed`.
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

    /// A response read to EOF as the fixture's `response` (N2, N7; header names lowercased and
    /// sorted, as the fixtures' are compared case-insensitively).
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
        for (k, v) in &mut headers {
            *v = if k.eq_ignore_ascii_case("date") {
                "<date>".to_owned()
            } else {
                normalize_port(v, port)
            };
        }
        let body_text = String::from_utf8(body.clone()).unwrap();
        response_value(
            status.parse().unwrap(),
            status_text,
            headers,
            &normalize_port(&body_text, port),
            body.len(),
        )
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

    /// The `full` server's exchanges, startup and shutdown that the web core answers without the
    /// api/webui/a2a/trigger sub-launchers (Task 12 replays the rest), byte for byte after the
    /// SCENARIO.md normalizations: health, the gorilla 405/404 and clean-path 301, StrictSlash,
    /// the origin guard's host check, the request log, the startup banner and the shutdown line.
    #[tokio::test]
    async fn the_web_core_matches_go_wire_fixtures() {
        let startup = fixture("full_00_startup");
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        // The fixture's `web` flags; a quiet sub-launcher stands in for the five real ones.
        let args: Vec<String> = startup["args"].as_array().unwrap()[1..]
            .iter()
            .map(|a| a.as_str().unwrap().replace("<port>", &port.to_string()))
            .take_while(|a| a != "api")
            .chain(["fake".to_owned()])
            .collect();
        let mut l = new(vec![Box::new(Quiet {
            keyword: "fake",
            syntax: "",
            fail_parse: false,
        }) as Box<dyn WebSublauncher>]);
        // No user message from the stand-in, like the fixture lines filtered below.
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
        let deadline = Instant::now() + Duration::from_secs(5);
        while TcpStream::connect(("127.0.0.1", port)).await.is_err() {
            assert!(Instant::now() < deadline, "the server did not start");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        // Startup: every Go line but the sub-launchers' own (trigger warnings, user messages).
        let want: Vec<String> = log_lines(&startup)
            .into_iter()
            .filter(|l| {
                let rest = l.strip_prefix("<ts> ").unwrap_or(l);
                !(rest.starts_with("adk: ")
                    || (rest.starts_with(' ')
                        && rest
                            .trim_start()
                            .split_once(':')
                            .is_some_and(|(k, _)| k.bytes().all(|b| b.is_ascii_alphanumeric()))))
            })
            .collect();
        let got: Vec<String> = normalize_log(&out.contents(), port)
            .into_iter()
            .filter(|l| !l.starts_with("<ts>        fake:"))
            .collect();
        assert_eq!(got, want, "startup log");

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
            log_lines(&fixture("full_99_shutdown")),
            "shutdown log"
        );
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
