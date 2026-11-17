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

//! The ADK REST API server.
//!
//! [`Server::new`] validates the compaction config against every application, builds the routes
//! (sessions, runtime, apps, artifacts, version, the 501 developer
//! stubs, and, with [`DebugApiConfig::include_debug_api`], debug and agent graph) behind the
//! origin guard, and [`Server::router`] serves them as an [`axum::Router`].
//! [`Server::debug_layer`] is the `tracing` layer that feeds the debug trace endpoints.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use futures::future::BoxFuture;
use gadk_agent::Loader;
use gadk_artifact::ArtifactService;
use gadk_engine::runner::RunnerConfig;
use gadk_memory::MemoryService;
use gadk_platform::Platform;
use gadk_session::SessionService;
use http::header::CONTENT_TYPE;
use http::{HeaderValue, Method};

use crate::authn::{self, Authenticator};
use crate::authz::{self, Authorizer};
use controllers::agentgraph::agent_graph_routes;
use controllers::apps::apps_routes;
use controllers::artifacts::artifacts_routes;
use controllers::debug::debug_routes;
use controllers::devfeatures::{agent_builder_routes, eval_routes, tests_routes};
use controllers::runtime::runtime_routes;
use controllers::sessions::sessions_routes;
use controllers::version::version_routes;
use controllers::{
    AgentGraphApiController, AppsApiController, ArtifactsApiController, DebugApiController,
    RuntimeApiController, RuntimeApiControllerConfig, SessionsApiController, VersionApiController,
};
use routing::{Handler, Request, Response, Route, RouteTable};

pub mod controllers;
pub mod triggers;

#[cfg(feature = "testutil")]
pub mod testutil;

pub(crate) mod agent_graph;
pub(crate) mod debug_telemetry;
pub(crate) mod http_error;
pub(crate) mod middleware;
pub(crate) mod models;
pub(crate) mod originguard;
pub(crate) mod routing;

pub use debug_telemetry::{DebugLog, DebugSpan, DebugTelemetry};
pub use http_error::encode_json_response;
pub use middleware::max_bytes_middleware;
pub use originguard::{LocalAddr, normalize_origin, origin_guard_middleware};
pub use routing::PathVars;

/// The request-body limit (10 MiB) when
/// [`ServerConfig::max_payload_size`] is zero or negative.
pub const DEFAULT_MAX_PAYLOAD_SIZE: i64 = 10 << 20;

/// A duration setting whose zero value selects a default and whose negative values turn the
/// feature off ([`ServerConfig::sse_write_timeout`], [`ServerConfig::live_keepalive_timeout`]
/// and their [`RuntimeApiControllerConfig`] twins).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Timeout {
    /// The setting's default (`After(Duration::ZERO)` means the same).
    #[default]
    Default,
    /// No timeout.
    Disabled,
    /// This duration.
    After(Duration),
}

impl Timeout {
    /// The effective duration: `default` for [`Timeout::Default`] (and `After(ZERO)`), `None` for
    /// [`Timeout::Disabled`].
    pub fn resolve(self, default: Duration) -> Option<Duration> {
        match self {
            Timeout::Default => Some(default),
            Timeout::After(d) if d.is_zero() => Some(default),
            Timeout::After(d) => Some(d),
            Timeout::Disabled => None,
        }
    }
}

/// Configuration of the REST [`Server`].
#[derive(Clone)]
pub struct ServerConfig {
    /// The session service. Required.
    pub session_service: Arc<dyn SessionService>,
    /// Optional memory service passed to every runner.
    pub memory_service: Option<Arc<dyn MemoryService>>,
    /// The agents served, by application name.
    pub agent_loader: Option<Arc<dyn Loader>>,
    /// Optional artifact service; the artifact routes answer 503 without one.
    pub artifact_service: Option<Arc<dyn ArtifactService>>,
    /// Write deadline of a `/run_sse` response, from when the request arrives: default 120 s.
    pub sse_write_timeout: Timeout,
    /// Plugins of every runner the server builds.
    pub plugin_config: gadk_engine::runner::PluginConfig,
    /// The debug trace store.
    pub debug_config: DebugTelemetryConfig,
    /// Whether the debug and agent graph routes are served.
    pub debug_api_config: DebugApiConfig,
    /// Authenticates every non-public route; `None` is [`authn::noop`].
    ///
    /// [`authn::noop`]: crate::authn::noop
    pub authenticator: Option<Arc<dyn Authenticator>>,
    /// Decides whether the caller may act as the path's user; `None` is [`authz::noop`].
    ///
    /// [`authz::noop`]: crate::authz::noop
    pub authorizer: Option<Arc<dyn Authorizer>>,
    /// Web origins allowed to call the server from a browser.
    pub allowed_origins: Vec<String>,
    /// The address the caller binds the server to.
    pub bind_host: String,
    /// Context compaction for every runner; `None` disables it.
    pub compaction: Option<gadk_session::compaction::Config>,
    /// Request-body limit in bytes; zero or negative selects [`DEFAULT_MAX_PAYLOAD_SIZE`].
    pub max_payload_size: i64,
    /// Cap on one client message of `/run_live`: zero means 16 MiB, negative no cap.
    pub max_live_message_bytes: i64,
    /// `/run_live` keepalive: default 40 s.
    pub live_keepalive_timeout: Timeout,
    /// Cap on concurrent `/run_live` connections; zero or negative means no cap.
    pub max_live_sessions: i32,
    /// Clock and ID source of every runner the server builds.
    pub platform: Platform,
}

impl ServerConfig {
    /// A config with `session_service` and every other field at its zero value.
    pub fn new(session_service: Arc<dyn SessionService>) -> Self {
        Self {
            session_service,
            memory_service: None,
            agent_loader: None,
            artifact_service: None,
            sse_write_timeout: Timeout::Default,
            plugin_config: Default::default(),
            debug_config: DebugTelemetryConfig::default(),
            debug_api_config: DebugApiConfig::default(),
            authenticator: None,
            authorizer: None,
            allowed_origins: Vec::new(),
            bind_host: String::new(),
            compaction: None,
            max_payload_size: 0,
            max_live_message_bytes: 0,
            live_keepalive_timeout: Timeout::Default,
            max_live_sessions: 0,
            platform: Platform::default(),
        }
    }
}

/// Configuration of the debug trace store.
#[derive(Debug, Clone, Copy, Default)]
pub struct DebugTelemetryConfig {
    /// Maximum number of traces kept; zero or negative selects 10 000.
    pub trace_capacity: i32,
}

/// Whether the debug routes are served.
#[derive(Debug, Clone, Copy, Default)]
pub struct DebugApiConfig {
    /// Serve the debug and agent graph routes. Do not enable in production: traces carry
    /// tool-call arguments and responses.
    pub include_debug_api: bool,
}

/// A server construction failure.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ServerError(pub String);

/// The REST server: the route table behind the origin guard, and the debug trace
/// store its debug routes read.
pub struct Server {
    /// The route table (requests reach it only through `handler`).
    table: Arc<RouteTable>,
    /// The platform extension, then the origin guard, then the route table.
    handler: Handler,
    /// The debug trace store.
    debug: DebugTelemetry,
}

impl Server {
    /// Validates the compaction config against every application of the loader (so an unservable
    /// config fails here rather than on every request), then builds the routes in registration
    /// order (`/health`, sessions, runtime, apps,
    /// artifacts, version, the agent builder, tests and eval stubs, and with
    /// [`DebugApiConfig::include_debug_api`] the debug and agent graph routes) behind
    /// [`ServerConfig::authenticator`] (default [`authn::noop`]) and the origin guard.
    ///
    /// [`authn::noop`]: crate::authn::noop
    pub fn new(cfg: ServerConfig) -> Result<Server, ServerError> {
        validate_compaction_against_agents(&cfg)?;
        let debug = DebugTelemetry::new(cfg.debug_config);
        let policy = Arc::new(originguard::Policy::new(&originguard::Config {
            bind_host: cfg.bind_host.clone(),
            allowed_origins: cfg.allowed_origins.clone(),
        }));
        let authorizer = cfg.authorizer.clone().unwrap_or_else(authz::noop);

        let sessions = Arc::new(
            SessionsApiController::new(Arc::clone(&cfg.session_service))
                .with_authorizer(Arc::clone(&authorizer)),
        );
        let artifacts = Arc::new(
            ArtifactsApiController::new(cfg.artifact_service.clone())
                .with_authorizer(Arc::clone(&authorizer)),
        );
        let runtime = Arc::new(RuntimeApiController::new(RuntimeApiControllerConfig {
            session_service: Arc::clone(&cfg.session_service),
            memory_service: cfg.memory_service.clone(),
            agent_loader: cfg.agent_loader.clone(),
            artifact_service: cfg.artifact_service.clone(),
            sse_timeout: cfg.sse_write_timeout,
            plugin_config: cfg.plugin_config.clone(),
            auto_create_session: false,
            authorizer: Some(Arc::clone(&authorizer)),
            compaction: cfg.compaction.clone(),
            // The guard already refuses a disallowed Origin; the upgrade applies the same rule
            // instead of the WebSocket library's own.
            check_origin: Some(policy.check_origin_fn()),
            max_live_message_bytes: cfg.max_live_message_bytes,
            live_keepalive_timeout: cfg.live_keepalive_timeout,
            max_live_sessions: cfg.max_live_sessions,
            platform: cfg.platform.clone(),
        }));

        let mut routes = vec![health_route()];
        routes.extend(sessions_routes(sessions));
        routes.extend(runtime_routes(runtime));
        routes.extend(apps_routes(Arc::new(AppsApiController::new(
            cfg.agent_loader.clone(),
        ))));
        routes.extend(artifacts_routes(artifacts));
        routes.extend(version_routes(Arc::new(VersionApiController::new())));
        // Ungated on purpose: the builder stub reveals nothing.
        routes.extend(agent_builder_routes());
        routes.extend(tests_routes());
        routes.extend(eval_routes());
        // Opt-in: traces carry tool-call arguments and responses, and the agent graph names
        // every tool the agent can call.
        if cfg.debug_api_config.include_debug_api {
            let debug_controller = DebugApiController::new(
                Arc::clone(&cfg.session_service),
                cfg.agent_loader.clone(),
                debug.clone(),
            )
            .with_authorizer(Arc::clone(&authorizer));
            routes.extend(debug_routes(Arc::new(debug_controller)));
            routes.extend(agent_graph_routes(Arc::new(AgentGraphApiController::new(
                cfg.agent_loader.clone(),
            ))));
        }

        let authenticator = cfg.authenticator.clone().unwrap_or_else(authn::noop);
        let table = Arc::new(RouteTable::new(
            routes,
            Some(authenticator),
            cfg.max_payload_size,
        ));
        let routed = Arc::clone(&table);
        let guarded = policy.middleware(routing::handler(move |req| {
            let table = Arc::clone(&routed);
            async move { table.serve(req).await }
        }));
        // The platform travels with the request as an extension.
        let platform = cfg.platform;
        let handler = routing::handler(move |mut req: Request| {
            // `OPTIONS *` and an undecodable target are answered here, before
            // `policy.Middleware` sees the request. `RouteTable::dispatch` decodes the path again
            // on purpose: tests drive the table directly, without this handler.
            if let Err(answer) = routing::decoded_path(&req) {
                return Box::pin(std::future::ready(answer.response()))
                    as BoxFuture<'static, Response>;
            }
            req.extensions_mut().insert(platform.clone());
            guarded(req)
        });
        Ok(Server {
            table,
            handler,
            debug,
        })
    }

    /// Every route of the server behind the origin guard, as an [`axum::Router`]. Serve it with
    /// `into_make_service_with_connect_info::<`[`LocalAddr`]`>()` so the guard knows the address
    /// each connection was accepted on.
    pub fn router(&self) -> axum::Router {
        let handler = Arc::clone(&self.handler);
        axum::Router::new().fallback_service(tower::service_fn(move |req: Request| {
            let handler = Arc::clone(&handler);
            async move { Ok::<_, Infallible>(handler(req).await) }
        }))
    }

    /// The `tracing` layer feeding the debug trace endpoints: the store the debug routes read.
    /// Install it in the application's subscriber.
    pub fn debug_layer(&self) -> DebugTelemetry {
        self.debug.clone()
    }

    /// Every route the server registered, in registration order.
    #[cfg(feature = "testutil")]
    pub(crate) fn routes(&self) -> Vec<routing::RouteInfo> {
        self.table.walk()
    }
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server")
            .field("routes", &self.table)
            .finish_non_exhaustive()
    }
}

/// The agent of a compaction check's base runner config, which every check replaces. The loader's
/// root agent is not used because a loader may have none (the conformance loader panics).
pub(crate) fn compaction_check_placeholder_agent()
-> Result<Arc<dyn gadk_agent::Agent>, gadk_agent::AgentError> {
    gadk_agent::new_agent(gadk_agent::AgentConfig {
        name: "placeholder".into(),
        ..Default::default()
    })
}

/// [`compactionvalidate::against_agents`] with the
/// server's services. The base config's agent is a placeholder each check replaces.
///
/// [`compactionvalidate::against_agents`]: gadk_engine::compactionvalidate::against_agents
fn validate_compaction_against_agents(cfg: &ServerConfig) -> Result<(), ServerError> {
    let Some(compaction) = cfg.compaction.as_ref() else {
        return Ok(());
    };
    let agent = compaction_check_placeholder_agent().map_err(|e| ServerError(e.to_string()))?;
    let base = RunnerConfig {
        app_name: String::new(),
        agent,
        session_service: Arc::clone(&cfg.session_service),
        artifact_service: cfg.artifact_service.clone(),
        memory_service: cfg.memory_service.clone(),
        plugin_config: cfg.plugin_config.clone(),
        auto_create_session: false,
        platform: cfg.platform.clone(),
        compaction: None,
    };
    gadk_engine::compactionvalidate::against_agents(
        Some(compaction),
        cfg.agent_loader.as_deref(),
        &base,
    )
    .map_err(|e| ServerError(e.to_string()))
}

/// The `GET`/`HEAD /health` route, outside authentication: `{"status":"ok"}` as
/// `application/json` (no charset, unlike every other JSON answer). Registered with strict
/// slash on.
fn health_route() -> Route {
    Route {
        name: "Health",
        methods: vec![Method::GET, Method::HEAD],
        pattern: "/health".to_owned(),
        public: true,
        // Strict slash: `/health/` is answered 301 → `/health`.
        strict_slash: true,
        handler: routing::handler(|_req: Request| async {
            let mut resp = Response::new(Body::from("{\"status\":\"ok\"}\n"));
            resp.headers_mut()
                .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            resp
        }),
    }
}
