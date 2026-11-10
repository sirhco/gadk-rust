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

//! The ADK REST API server (port of ADK-Go `server/adkrest`).
//!
//! [`Server::new`] validates the compaction config against every application, builds the routes
//! ADK-Go's `NewServer` registers (sessions, runtime, apps, artifacts, version, the 501 developer
//! stubs, and, with [`DebugApiConfig::include_debug_api`], debug and agent graph) behind the
//! origin guard, and [`Server::router`] serves them as an [`axum::Router`].
//! [`Server::debug_layer`] is the `tracing` layer that feeds the debug trace endpoints.

use std::sync::Arc;
use std::time::Duration;

use gadk_agent::Loader;
use gadk_artifact::ArtifactService;
use gadk_memory::MemoryService;
use gadk_platform::Platform;
use gadk_session::SessionService;

use crate::authn::Authenticator;
use crate::authz::Authorizer;

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
pub use routing::PathVars;

/// Go `adkrest.DefaultMaxPayloadSize`: the request-body limit (10 MiB) when
/// [`ServerConfig::max_payload_size`] is zero or negative.
pub const DEFAULT_MAX_PAYLOAD_SIZE: i64 = 10 << 20;

/// A Go `time.Duration` setting whose zero value selects a default and whose negative values turn
/// the feature off ([`ServerConfig::sse_write_timeout`], [`ServerConfig::live_keepalive_timeout`]
/// and their [`RuntimeApiControllerConfig`](controllers::RuntimeApiControllerConfig) twins).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Timeout {
    /// Go zero: the setting's default (`After(Duration::ZERO)` means the same).
    #[default]
    Default,
    /// Go negative: no timeout.
    Disabled,
    /// Go positive.
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

/// Port of Go `adkrest.ServerConfig`.
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
    /// Write deadline of a `/run_sse` response, from when the request arrives (Go
    /// `SSEWriteTimeout`): default 120 s.
    pub sse_write_timeout: Timeout,
    /// Plugins of every runner the server builds.
    pub plugin_config: gadk_engine::runner::PluginConfig,
    /// The debug trace store.
    pub debug_config: DebugTelemetryConfig,
    /// Whether the debug and agent graph routes are served.
    pub debug_api_config: DebugApiConfig,
    /// Authenticates every non-public route; `None` is Go's nil, i.e. [`authn::noop`].
    ///
    /// [`authn::noop`]: crate::authn::noop
    pub authenticator: Option<Arc<dyn Authenticator>>,
    /// Decides whether the caller may act as the path's user; `None` is [`authz::noop`].
    ///
    /// [`authz::noop`]: crate::authz::noop
    pub authorizer: Option<Arc<dyn Authorizer>>,
    /// Web origins allowed to call the server from a browser (Go `AllowedOrigins`).
    pub allowed_origins: Vec<String>,
    /// The address the caller binds the server to (Go `BindHost`).
    pub bind_host: String,
    /// Context compaction for every runner; `None` disables it.
    pub compaction: Option<gadk_session::compaction::Config>,
    /// Request-body limit in bytes; zero or negative selects [`DEFAULT_MAX_PAYLOAD_SIZE`].
    pub max_payload_size: i64,
    /// Cap on one client message of `/run_live`: zero means 16 MiB, negative no cap.
    pub max_live_message_bytes: i64,
    /// `/run_live` keepalive (Go `LiveKeepaliveTimeout`): default 40 s.
    pub live_keepalive_timeout: Timeout,
    /// Cap on concurrent `/run_live` connections; zero or negative means no cap.
    pub max_live_sessions: i32,
    /// Clock and ID source of every runner the server builds.
    pub platform: Platform,
}

impl ServerConfig {
    /// A config with `session_service` and every other field at Go's zero value.
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

/// Port of Go `adkrest.DebugTelemetryConfig`.
#[derive(Debug, Clone, Copy, Default)]
pub struct DebugTelemetryConfig {
    /// Maximum number of traces kept; zero or negative selects 10 000.
    pub trace_capacity: i32,
}

/// Port of Go `adkrest.DebugAPIConfig`.
#[derive(Debug, Clone, Copy, Default)]
pub struct DebugApiConfig {
    /// Serve the debug and agent graph routes. Do not enable in production: traces carry
    /// tool-call arguments and responses.
    pub include_debug_api: bool,
}

/// A server construction failure (Go's error text).
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ServerError(pub String);

/// Port of Go `adkrest.Server`.
#[allow(dead_code)] // plan 5a Task 13 wires the server
pub struct Server {
    table: Arc<routing::RouteTable>,
    debug: DebugTelemetry,
}

#[allow(unused_variables, clippy::todo)] // plan 5a Task 13
impl Server {
    /// Port of Go `adkrest.NewServer`.
    pub fn new(cfg: ServerConfig) -> Result<Server, ServerError> {
        todo!("plan 5a Task 13")
    }

    /// Every route of the server behind the origin guard, as an [`axum::Router`].
    pub fn router(&self) -> axum::Router {
        todo!("plan 5a Task 13")
    }

    /// The `tracing` layer feeding the debug trace endpoints (Go `SpanProcessor` and
    /// `LogProcessor`); install it in the application's subscriber.
    pub fn debug_layer(&self) -> DebugTelemetry {
        todo!("plan 5a Task 13")
    }
}
