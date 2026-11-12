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

//! Port of ADK-Go `cmd/launcher/web/web_test.go`.
//!
//! Go's `l.Run` is `run_with(…, |_| true)` where a test sets up telemetry: an empty environment
//! and no global tracer provider or dispatcher (the crate's tests share the process). The fake
//! sub-launchers keep what they saw behind shared handles, since `Run` owns them.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use gadk_server::mux::{Request, Response, handler};
use gadk_telemetry::{TelemetryOption, with_resource, with_span_processors};
use http::{Method, StatusCode};
use opentelemetry::Context;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::error::{OTelSdkError, OTelSdkResult};
use opentelemetry_sdk::trace::{Span, SpanData, SpanProcessor};
use pretty_assertions::assert_eq;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

use crate::config::Config;
use crate::error::Error;
use crate::golog;
use crate::launcher::SubLauncher;
use crate::test_support::{NoAgents, free_port, strings};
use crate::web::{
    self, DEFAULT_HOST, WebRouter, WebSublauncher, apply_service_defaults, build_base_router,
    register_health_route, testing,
};

/// Go `&launcher.Config{}`, logging to a buffer.
fn config() -> (Config, golog::Captured) {
    let (log, out) = golog::capture();
    let mut config = Config::new(Arc::new(NoAgents));
    config.log = log.with_clock(|| "2009/11/10 23:00:00 ".to_owned());
    (config, out)
}

/// What a fake sub-launcher saw during `Run`.
#[derive(Default)]
struct Seen {
    bind_host: Option<String>,
    allowed_origins: Option<Vec<String>>,
    router: Option<WebRouter>,
    user_message_called: bool,
}

/// Go's fake sub-launchers (`telemetryFailSublauncher` and the ones embedding it): `keyword`,
/// `Parse` returning its arguments, and a `SetupSubrouters` that records what it was given,
/// optionally contributes an origin (`guardedSublauncher.contributes`), registers
/// `PathPrefix("/"+keyword)` answering 418 (`guardedSublauncher`) and appends a telemetry option
/// (`optionAppendingSublauncher`).
struct Fake {
    keyword: &'static str,
    contributes: Option<&'static str>,
    route: bool,
    option: Mutex<Option<TelemetryOption>>,
    seen: Arc<Mutex<Seen>>,
}

impl Fake {
    fn new(keyword: &'static str) -> Fake {
        Fake {
            keyword,
            contributes: None,
            route: false,
            option: Mutex::new(None),
            seen: Arc::new(Mutex::new(Seen::default())),
        }
    }

    /// Go `guardedSublauncher{keyword, contributes}`.
    fn guarded(keyword: &'static str, contributes: Option<&'static str>) -> Fake {
        Fake {
            contributes,
            route: true,
            ..Fake::new(keyword)
        }
    }
}

impl WebSublauncher for Fake {
    fn keyword(&self) -> String {
        self.keyword.to_owned()
    }
    fn parse(&mut self, args: Vec<String>) -> Result<Vec<String>, Error> {
        Ok(args)
    }
    fn command_line_syntax(&self) -> String {
        String::new()
    }
    fn simple_description(&self) -> String {
        String::new()
    }
    fn setup_subrouters(&self, router: &mut WebRouter, config: &mut Config) -> Result<(), Error> {
        let mut seen = self.seen.lock().unwrap();
        seen.bind_host = Some(config.bind_host.clone());
        seen.allowed_origins = Some(config.allowed_origins.clone());
        if let Some(origin) = self.contributes {
            config.allowed_origins.push(origin.to_owned());
        }
        if let Some(option) = self.option.lock().unwrap().take() {
            config.telemetry_options.push(option);
        }
        if self.route {
            // StatusTeapot so that reaching the handler cannot be confused with any status the
            // guard itself writes.
            router.handle_prefix(
                &[],
                &format!("/{}", self.keyword),
                handler(|_req: Request| async {
                    let mut resp = Response::new(Body::empty());
                    *resp.status_mut() = StatusCode::IM_A_TEAPOT;
                    resp
                }),
            );
            seen.router = Some(router.clone());
        }
        Ok(())
    }
    fn user_message(&self, _web_url: &str, _printer: &mut dyn FnMut(&str)) {
        self.seen.lock().unwrap().user_message_called = true;
    }
}

/// Go `trackingSpanProcessor`: records that its `Shutdown` was called.
#[derive(Debug, Default)]
struct TrackingSpanProcessor {
    shutdown_called: Arc<AtomicBool>,
}

impl SpanProcessor for TrackingSpanProcessor {
    fn on_start(&self, _span: &mut Span, _cx: &Context) {}
    fn on_end(&self, _span: SpanData) {}
    fn force_flush(&self) -> OTelSdkResult {
        Ok(())
    }
    fn shutdown_with_timeout(&self, _timeout: Duration) -> OTelSdkResult {
        self.shutdown_called.store(true, Ordering::SeqCst);
        Ok(())
    }
}

/// Go `failingSpanProcessor`: its `Shutdown` fails with `shutdown_err`.
#[derive(Debug)]
struct FailingSpanProcessor {
    shutdown_err: &'static str,
}

impl SpanProcessor for FailingSpanProcessor {
    fn on_start(&self, _span: &mut Span, _cx: &Context) {}
    fn on_end(&self, _span: SpanData) {}
    fn force_flush(&self) -> OTelSdkResult {
        Ok(())
    }
    fn shutdown_with_timeout(&self, _timeout: Duration) -> OTelSdkResult {
        Err(OTelSdkError::InternalFailure(self.shutdown_err.to_owned()))
    }
}

/// `telemetry.WithSpanProcessors(p)` and a handle on `p`'s shutdown flag.
fn tracking() -> (TelemetryOption, Arc<AtomicBool>) {
    let tracker = TrackingSpanProcessor::default();
    let called = Arc::clone(&tracker.shutdown_called);
    (
        with_span_processors([Box::new(tracker) as Box<dyn SpanProcessor>]),
        called,
    )
}

/// Go `resource.NewWithAttributes("https://conflicting.invalid/schema/v1")`: its schema URL
/// conflicts with the default resource's, so `resource.Merge` fails and telemetry init fails
/// without touching the network.
fn conflicting_resource() -> TelemetryOption {
    with_resource(
        Resource::builder_empty()
            .with_schema_url(
                Vec::<opentelemetry::KeyValue>::new(),
                "https://conflicting.invalid/schema/v1",
            )
            .build(),
    )
}

/// Go `l.Run(ctx, config)` with telemetry's globals injected.
async fn run(l: &web::WebLauncher, config: Config) -> Result<(), Error> {
    l.run_with(CancellationToken::new(), config, |_| true).await
}

// parity: adapted: the launcher's server options serve a router on a pre-bound listener (Go: buildHTTPServer(handler).Serve(listener)); reqwest's prior-knowledge HTTP/2 client stands in for Go's UnencryptedHTTP2 transport
#[tokio::test]
async fn test_h2_c_flag() {
    for (name, args, want_h2c) in [
        ("disabled by default", vec![], false),
        ("enabled", vec!["--h2c"], true),
    ] {
        let mut launcher = web::new(Vec::new());
        launcher
            .parse(strings(&args))
            .unwrap_or_else(|e| panic!("{name}: Parse({args:?}) failed: {e}"));

        let mut router = WebRouter::new(true);
        router.handle_prefix(
            &[],
            "/",
            handler(|req: Request| async move {
                let mut resp = Response::new(Body::empty());
                *resp.status_mut() = StatusCode::NO_CONTENT;
                resp.headers_mut().insert(
                    "X-Request-Protocol",
                    format!("{:?}", req.version()).parse().unwrap(),
                );
                resp
            }),
        );
        let listener = testing::listen("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let cancel = CancellationToken::new();
        let (log, _) = golog::capture();
        let server = tokio::spawn(testing::serve(
            listener,
            router,
            launcher.serve_options(),
            cancel.clone(),
            log,
        ));

        let url = format!("http://{addr}/");
        let resp = reqwest::get(&url).await.unwrap();
        assert_eq!(resp.version(), http::Version::HTTP_11, "{name}");

        let h2c_client = reqwest::Client::builder()
            .http2_prior_knowledge()
            .build()
            .unwrap();
        let result = h2c_client.get(&url).send().await;
        if want_h2c {
            let resp = result.unwrap_or_else(|e| panic!("{name}: h2c request failed: {e}"));
            assert_eq!(resp.version(), http::Version::HTTP_2, "{name}");
            assert_eq!(
                resp.headers()["X-Request-Protocol"],
                "HTTP/2.0",
                "{name}: handler request protocol"
            );
        } else if let Ok(resp) = result {
            panic!(
                "{name}: h2c request unexpectedly succeeded with protocol {:?}",
                resp.version()
            );
        }
        cancel.cancel();
        let draining = server.await.unwrap();
        draining
            .shutdown(gadk_platform::GoDuration::from_secs(5))
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn test_run_does_not_leak_listener_when_telemetry_init_fails() {
    let port = free_port();
    let mut l = web::new(vec![Box::new(Fake::new("repro"))]);
    l.parse(strings(&["--port", &port.to_string(), "repro"]))
        .unwrap();

    let (mut config, _) = config();
    config.telemetry_options = vec![conflicting_resource()];
    assert!(
        run(&l, config).await.is_err(),
        "Run() succeeded, want telemetry initialization failure"
    );

    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(100)).is_ok() {
            panic!("port {port} still bound after Run() returned an error: listener leaked");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn test_apply_service_defaults_fills_empty_config() {
    let (mut config, _) = config();
    apply_service_defaults(&mut config);
    assert!(config.session_service.is_some(), "SessionService is nil");
    assert!(config.artifact_service.is_some(), "ArtifactService is nil");
    assert!(config.memory_service.is_some(), "MemoryService is nil");
}

/// Go `assertService`: set, and the caller's own value when one was supplied (pointer identity).
fn assert_service<T: ?Sized>(name: &str, got: &Option<Arc<T>>, want: &Option<Arc<T>>) {
    let Some(got) = got else {
        panic!("{name} is nil after applyServiceDefaults, want a default in-memory service");
    };
    if let Some(want) = want {
        assert!(
            std::ptr::addr_eq(Arc::as_ptr(got), Arc::as_ptr(want)),
            "{name}: want the caller-supplied service"
        );
    }
}

#[test]
fn test_apply_service_defaults_keeps_supplied_services() {
    for (name, supply_session, supply_artifact, supply_memory) in [
        ("all supplied", true, true, true),
        ("only session supplied", true, false, false),
        ("only artifact supplied", false, true, false),
        ("only memory supplied", false, false, true),
    ] {
        let (mut config, _) = config();
        let want_session: Option<Arc<dyn gadk_session::SessionService>> =
            supply_session.then(|| Arc::new(gadk_session::InMemorySessionService::new()) as _);
        let want_artifact: Option<Arc<dyn gadk_artifact::ArtifactService>> =
            supply_artifact.then(|| Arc::new(gadk_artifact::InMemoryArtifactService::new()) as _);
        let want_memory: Option<Arc<dyn gadk_memory::MemoryService>> =
            supply_memory.then(|| Arc::new(gadk_memory::InMemoryMemoryService::new()) as _);
        config.session_service = want_session.clone();
        config.artifact_service = want_artifact.clone();
        config.memory_service = want_memory.clone();

        apply_service_defaults(&mut config);

        assert_service(
            &format!("{name}: SessionService"),
            &config.session_service,
            &want_session,
        );
        assert_service(
            &format!("{name}: ArtifactService"),
            &config.artifact_service,
            &want_artifact,
        );
        assert_service(
            &format!("{name}: MemoryService"),
            &config.memory_service,
            &want_memory,
        );
    }
}

/// A case of `TestApplyServiceDefaultsLogsWhatItDefaulted`: name, what the caller supplies, the
/// services logged, the services not logged.
type LogCase = (
    &'static str,
    fn(&mut Config),
    &'static [&'static str],
    &'static [&'static str],
);

#[test]
fn test_apply_service_defaults_logs_what_it_defaulted() {
    let cases: [LogCase; 3] = [
        (
            "nothing supplied",
            |_| {},
            &["session", "artifact", "memory"],
            &[],
        ),
        (
            "only session supplied",
            |c| c.session_service = Some(Arc::new(gadk_session::InMemorySessionService::new())),
            &["artifact", "memory"],
            &["session"],
        ),
        (
            "all supplied",
            |c| {
                c.session_service = Some(Arc::new(gadk_session::InMemorySessionService::new()));
                c.artifact_service = Some(Arc::new(gadk_artifact::InMemoryArtifactService::new()));
                c.memory_service = Some(Arc::new(gadk_memory::InMemoryMemoryService::new()));
            },
            &[],
            &["session", "artifact", "memory"],
        ),
    ];
    for (name, supply, want, not_want) in cases {
        // Go `log.SetFlags(0)`: no timestamp.
        let (log, out) = golog::capture();
        let mut config = Config::new(Arc::new(NoAgents));
        config.log = log.with_clock(String::new);
        supply(&mut config);

        apply_service_defaults(&mut config);

        let got = out.contents();
        if want.is_empty() {
            assert_eq!(got, "", "{name}: every service was supplied");
        }
        for service in want {
            let line = format!("No {service} service configured");
            assert!(got.contains(&line), "{name}: logged {got:?}, want {line:?}");
        }
        for service in not_want {
            let line = format!("No {service} service configured");
            assert!(
                !got.contains(&line),
                "{name}: logged {got:?}, want no {line:?}"
            );
        }
    }
}

// parity: adapted: the REST server gets the config's agent loader (Go passes a nil AgentLoader, which the listed routes never call)
#[tokio::test]
async fn test_apply_service_defaults_serves_rest_routes() {
    let (mut config, _) = config();
    apply_service_defaults(&mut config);

    let mut cfg = gadk_server::rest::ServerConfig::new(config.session_service.clone().unwrap());
    cfg.artifact_service = config.artifact_service.clone();
    cfg.memory_service = config.memory_service.clone();
    cfg.agent_loader = Some(Arc::clone(&config.agent_loader));
    let server = gadk_server::rest::Server::new(cfg).unwrap();

    for (name, path) in [
        ("list artifacts", "/apps/a/users/u/sessions/s/artifacts"),
        ("list sessions", "/apps/a/users/u/sessions"),
    ] {
        let req = http::Request::builder()
            .uri(path)
            .header("Host", "example.com")
            .body(Body::empty())
            .unwrap();
        let resp = server.router().oneshot(req).await.unwrap();
        let status = resp.status();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            status,
            StatusCode::OK,
            "{name}: GET {path} ({})",
            String::from_utf8_lossy(&body)
        );
    }
}

#[tokio::test]
async fn test_apply_service_defaults_memory_service_is_callable() {
    let (mut config, _) = config();
    apply_service_defaults(&mut config);
    let memory = config.memory_service.unwrap();
    memory
        .search_memory(gadk_memory::SearchRequest {
            app_name: "a".into(),
            user_id: "u".into(),
            query: "anything".into(),
        })
        .await
        .unwrap();
}

fn get(method: Method, path: &str) -> Request {
    http::Request::builder()
        .method(method)
        .uri(path)
        .header("Host", "example.com")
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn test_register_health_route() {
    let (log, _) = golog::capture();
    let mut router = build_base_router(&log);
    register_health_route(&mut router);

    // GET
    let resp = router.serve(get(Method::GET, "/health")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["Content-Type"], "application/json");
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let got: std::collections::HashMap<String, String> = serde_json::from_slice(&body).unwrap();
    assert_eq!(got["status"], "ok");

    // HEAD
    let resp = router.serve(get(Method::HEAD, "/health")).await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_build_base_router_leaves_health_to_the_caller() {
    let (log, _) = golog::capture();
    let mut router = build_base_router(&log);
    router.handle(
        &[Method::GET],
        "/health",
        handler(|_req: Request| async {
            let mut resp = Response::new(Body::empty());
            *resp.status_mut() = StatusCode::IM_A_TEAPOT;
            resp
        }),
    );
    let resp = router.serve(get(Method::GET, "/health")).await;
    assert_eq!(
        resp.status(),
        StatusCode::IM_A_TEAPOT,
        "the embedder's handler was shadowed"
    );
}

#[tokio::test]
async fn test_run_shuts_down_telemetry_when_server_fails_to_start() {
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port();
    let mut l = web::new(vec![Box::new(Fake::new("repro"))]);
    l.parse(strings(&["--port", &port.to_string(), "repro"]))
        .unwrap();

    let (option, shutdown_called) = tracking();
    let (mut config, _) = config();
    config.telemetry_options = vec![option];

    assert!(
        run(&l, config).await.is_err(),
        "Run() succeeded, want server bind failure"
    );
    assert!(
        shutdown_called.load(Ordering::SeqCst),
        "telemetry shutdown was not called after server startup failure"
    );
}

// parity: adapted: opentelemetry_sdk wraps the processor's error (`Operation failed: Shutdown errors: [InternalFailure("…")]`), so the logged line is checked whole instead of Go's `telemetry shutdown failed: simulated flush error` substring
#[tokio::test]
async fn test_run_logs_when_telemetry_shutdown_fails() {
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port();
    let mut l = web::new(vec![Box::new(Fake::new("repro"))]);
    l.parse(strings(&["--port", &port.to_string(), "repro"]))
        .unwrap();

    let failing_processor = FailingSpanProcessor {
        shutdown_err: "simulated flush error",
    };
    let (mut config, out) = config();
    config.telemetry_options = vec![with_span_processors([
        Box::new(failing_processor) as Box<dyn SpanProcessor>
    ])];

    assert!(
        run(&l, config).await.is_err(),
        "Run() succeeded, want server bind failure"
    );
    let got = out.contents();
    assert!(
        got.contains(
            "telemetry shutdown failed: Operation failed: Shutdown errors: [InternalFailure(\"simulated flush error\")]\n"
        ),
        "expected log output to contain telemetry shutdown error, got {got:?}"
    );
}

#[tokio::test]
async fn test_run_does_not_announce_url_when_telemetry_init_fails() {
    let port = free_port();
    let sub = Fake::new("repro");
    let seen = Arc::clone(&sub.seen);
    let mut l = web::new(vec![Box::new(sub)]);
    l.parse(strings(&["--port", &port.to_string(), "repro"]))
        .unwrap();

    let (mut config, out) = config();
    config.telemetry_options = vec![conflicting_resource()];
    let err = run(&l, config).await.expect_err("Run() succeeded");
    assert!(
        err.to_string().contains("telemetry initialization failed"),
        "Run() error = {err}, want error containing \"telemetry initialization failed\""
    );
    assert!(
        !seen.lock().unwrap().user_message_called,
        "UserMessage was called before telemetry initialization succeeded"
    );
    let got = out.contents();
    assert!(
        !got.contains("Starting the web server: ") && !got.contains("Web servers starts on "),
        "startup banner was logged before telemetry initialization succeeded: {got:?}"
    );
}

#[tokio::test]
async fn test_run_setup_subrouters_can_append_telemetry_options() {
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port();
    let (option, shutdown_called) = tracking();
    let sub = Fake::new("appending");
    *sub.option.lock().unwrap() = Some(option);
    let mut l = web::new(vec![Box::new(sub)]);
    l.parse(strings(&["--port", &port.to_string(), "appending"]))
        .unwrap();

    let (config, _) = config();
    assert!(
        run(&l, config).await.is_err(),
        "Run() succeeded, want server bind failure"
    );
    assert!(
        shutdown_called.load(Ordering::SeqCst),
        "span processor appended in SetupSubrouters was not initialized/shut down"
    );
}

#[test]
fn test_host_binding() {
    for (name, args, want) in [
        ("loopback by default", vec![], "127.0.0.1:8080"),
        (
            "explicit loopback",
            vec!["--host", "127.0.0.1"],
            "127.0.0.1:8080",
        ),
        ("all interfaces", vec!["--host", "0.0.0.0"], "0.0.0.0:8080"),
        (
            "empty host falls back to loopback",
            vec!["--host", ""],
            "127.0.0.1:8080",
        ),
        ("IPv6 loopback", vec!["--host", "::1"], "[::1]:8080"),
    ] {
        let mut launcher = web::new(Vec::new());
        launcher.parse(strings(&args)).unwrap();
        assert_eq!(launcher.server_addr(), want, "{name}: server Addr");
    }
}

#[test]
fn test_web_url() {
    for (name, host, port, want) in [
        ("localhost", "localhost", 8080, "http://localhost:8080"),
        ("IPv4 loopback", "127.0.0.1", 8080, "http://localhost:8080"),
        ("IPv6 loopback", "::1", 8080, "http://localhost:8080"),
        (
            "all interfaces IPv4",
            "0.0.0.0",
            8080,
            "http://localhost:8080",
        ),
        ("all interfaces IPv6", "::", 8080, "http://localhost:8080"),
        (
            "custom hostname",
            "example.com",
            8080,
            "http://example.com:8080",
        ),
        (
            "custom IP",
            "192.168.1.10",
            8080,
            "http://192.168.1.10:8080",
        ),
        (
            "custom IPv6",
            "2001:db8::1",
            8080,
            "http://[2001:db8::1]:8080",
        ),
        ("empty host", "", 8080, "http://localhost:8080"),
    ] {
        assert_eq!(web::web_url(host, port), want, "{name}: webURL()");
    }
}

#[tokio::test]
async fn test_run_passes_resolved_bind_host_to_sublaunchers() {
    for (name, args, want) in [
        ("default", vec![], DEFAULT_HOST),
        (
            "empty host resolves to the default",
            vec!["--host", ""],
            DEFAULT_HOST,
        ),
        (
            "explicit loopback",
            vec!["--host", "127.0.0.1"],
            "127.0.0.1",
        ),
        ("all interfaces", vec!["--host", "0.0.0.0"], "0.0.0.0"),
    ] {
        // Occupy the port so Run fails at bind, after SetupSubrouters.
        let taken = std::net::TcpListener::bind(format!("{want}:0")).unwrap();
        let port = taken.local_addr().unwrap().port().to_string();

        let sub = Fake::new("recording");
        let seen = Arc::clone(&sub.seen);
        let mut l = web::new(vec![Box::new(sub)]);
        let mut all = args.clone();
        all.extend(["--port", &port, "recording"]);
        l.parse(strings(&all)).unwrap();

        let (config, _) = config();
        assert!(
            l.run(CancellationToken::new(), config).await.is_err(),
            "{name}: Run() succeeded, want server bind failure"
        );

        let seen = seen.lock().unwrap().bind_host.clone().unwrap_or_default();
        assert_eq!(seen, want, "{name}: sublauncher saw BindHost");
        let addr = l.server_addr();
        let (host, _) = addr.rsplit_once(':').unwrap();
        assert_eq!(
            host, seen,
            "{name}: sublauncher saw BindHost {seen:?}, but the server binds {host:?}"
        );
    }
}

/// Go `runForRouter`: runs the launcher with `subs` far enough to install the guard (an occupied
/// port fails the bind after setup and the guard) and returns the router the first one captured.
async fn run_for_router(config: Config, args: &[&str], subs: Vec<Fake>) -> WebRouter {
    // The bind host the flags resolve to (Go reads `l.bindHost()` after Parse).
    let mut probe = web::new(Vec::new());
    probe.parse(strings(args)).unwrap();
    let taken = std::net::TcpListener::bind(format!("{}:0", probe.bind_host())).unwrap();
    let port = taken.local_addr().unwrap().port().to_string();

    let first = Arc::clone(&subs[0].seen);
    let mut all = strings(args);
    all.extend(["--port".to_owned(), port]);
    all.extend(subs.iter().map(|s| s.keyword.to_owned()));
    let boxed: Vec<Box<dyn WebSublauncher>> = subs
        .into_iter()
        .map(|s| Box::new(s) as Box<dyn WebSublauncher>)
        .collect();
    let mut l = web::new(boxed);
    l.parse(all).unwrap();
    assert!(
        l.run(CancellationToken::new(), config).await.is_err(),
        "Run() succeeded, want server bind failure"
    );
    let router = first.lock().unwrap().router.clone();
    router.expect("Run() returned before SetupSubrouters, so no router was captured")
}

/// Go `statusFor`: a GET for `path` with the Host and Origin given ("" host: httptest's
/// `example.com`; "" origin: none).
async fn status_for(router: &WebRouter, path: &str, host: &str, origin: &str) -> u16 {
    let host = if host.is_empty() { "example.com" } else { host };
    let mut req = http::Request::builder().uri(path).header("Host", host);
    if !origin.is_empty() {
        req = req.header("Origin", origin);
    }
    router
        .serve(req.body(Body::empty()).unwrap())
        .await
        .status()
        .as_u16()
}

/// A case of `TestRunGuardsEverySublauncherRoute`: name, args, contributed origin, path, Host,
/// Origin, status.
type GuardCase = (
    &'static str,
    Vec<&'static str>,
    Option<&'static str>,
    &'static str,
    &'static str,
    &'static str,
    u16,
);

#[tokio::test]
async fn test_run_guards_every_sublauncher_route() {
    let two_flags = vec![
        "--allow_origins",
        "https://one.example.com",
        "--allow_origins",
        "https://two.example.com",
    ];
    let cases: Vec<GuardCase> = vec![
        (
            "loopback host is served",
            vec![],
            None,
            "/guarded/x",
            "localhost:8080",
            "",
            418,
        ),
        (
            "rebound host is refused",
            vec![],
            None,
            "/guarded/x",
            "rebind.attacker.com:8080",
            "http://rebind.attacker.com:8080",
            403,
        ),
        (
            "rebound host with no origin is refused",
            vec![],
            None,
            "/guarded/x",
            "rebind.attacker.com:8080",
            "",
            403,
        ),
        (
            "rebound host is refused when -host is empty",
            vec!["--host", ""],
            None,
            "/guarded/x",
            "rebind.attacker.com:8080",
            "",
            403,
        ),
        (
            "root health route is guarded too",
            vec![],
            None,
            "/health",
            "rebind.attacker.com:8080",
            "",
            403,
        ),
        (
            "health is served over loopback",
            vec![],
            None,
            "/health",
            "127.0.0.1:8080",
            "",
            200,
        ),
        (
            "origin a sublauncher contributed is served",
            vec![],
            Some("https://ui.example.com"),
            "/guarded/x",
            "localhost:8080",
            "https://ui.example.com",
            418,
        ),
        (
            "host of a contributed origin is served",
            vec![],
            Some("https://ui.example.com"),
            "/guarded/x",
            "ui.example.com",
            "",
            418,
        ),
        (
            "first -allow_origins value is served",
            two_flags.clone(),
            None,
            "/guarded/x",
            "localhost:8080",
            "https://one.example.com",
            418,
        ),
        (
            "repeated -allow_origins value is served",
            two_flags.clone(),
            None,
            "/guarded/x",
            "localhost:8080",
            "https://two.example.com",
            418,
        ),
        (
            "unlisted origin is refused",
            vec!["--allow_origins", "https://one.example.com"],
            None,
            "/guarded/x",
            "localhost:8080",
            "https://evil.example.com",
            403,
        ),
        (
            "star allows a rebound host",
            vec!["--allow_origins", "*"],
            None,
            "/guarded/x",
            "rebind.attacker.com:8080",
            "",
            418,
        ),
        (
            "no host check when bound to all interfaces",
            vec!["--host", "0.0.0.0"],
            None,
            "/guarded/x",
            "anything.example.com",
            "",
            418,
        ),
    ];
    for (name, args, contributes, path, host, origin, want) in cases {
        let (config, _) = config();
        let router =
            run_for_router(config, &args, vec![Fake::guarded("guarded", contributes)]).await;
        assert_eq!(
            status_for(&router, path, host, origin).await,
            want,
            "{name}: GET {path} with Host {host:?} and Origin {origin:?}"
        );
    }
}

// parity: adapted: Config is moved into Run, so the caller's slice has no spare capacity to protect; the order each sub-launcher sees and the guard's list are checked as in Go
#[tokio::test]
async fn test_run_allowed_origins_reach_sublaunchers() {
    const FROM_CALLER: &str = "https://caller.example.com";
    const FROM_FLAG: &str = "https://flag.example.com";
    const FROM_FIRST: &str = "https://first.example.com";
    const FROM_SECOND: &str = "https://second.example.com";
    let first = Fake::guarded("first", Some(FROM_FIRST));
    let second = Fake::guarded("second", Some(FROM_SECOND));
    let (first_seen, second_seen) = (Arc::clone(&first.seen), Arc::clone(&second.seen));

    let (mut config, _) = config();
    config.allowed_origins = vec![FROM_CALLER.to_owned()];
    let router = run_for_router(config, &["--allow_origins", FROM_FLAG], vec![first, second]).await;

    assert_eq!(
        first_seen.lock().unwrap().allowed_origins.clone().unwrap(),
        vec![FROM_CALLER, FROM_FLAG],
        "first sublauncher saw AllowedOrigins"
    );
    assert_eq!(
        second_seen.lock().unwrap().allowed_origins.clone().unwrap(),
        vec![FROM_CALLER, FROM_FLAG, FROM_FIRST],
        "second sublauncher saw AllowedOrigins"
    );
    for origin in [FROM_CALLER, FROM_FLAG, FROM_FIRST, FROM_SECOND] {
        assert_eq!(
            status_for(&router, "/first/x", "localhost:8080", origin).await,
            418,
            "GET /first/x with Origin {origin:?}"
        );
    }
}
