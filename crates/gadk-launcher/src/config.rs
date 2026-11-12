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

//! Go `launcher.Config`: the services, agents and server settings a launcher runs with.

use std::sync::Arc;

use gadk_agent::Loader;
use gadk_artifact::ArtifactService;
use gadk_memory::MemoryService;
use gadk_session::SessionService;

use crate::error::Error;
use crate::golog::Logger;

/// A `tracing` layer the launcher installs in its global subscriber (Go
/// `launcher.Config.TelemetryOptions`).
pub type BoxedLayer =
    Box<dyn tracing_subscriber::Layer<tracing_subscriber::Registry> + Send + Sync>;

/// One of Go's `a2asrv.RequestHandlerOption`s that configures the handler itself: receives the
/// a2a-rs default request handler the `a2a` sub-launcher builds and returns it configured
/// through its builders (`with_extended_agent_card`, `with_extended_agent_card_resolver`,
/// `with_authorizer`, `with_push_config_store`, `with_push_notifications`, `with_capabilities`,
/// `with_default_input_modes`).
#[cfg(feature = "a2a")]
pub type A2AHandlerOption = Arc<
    dyn Fn(
            gadk_a2a::a2a_server::DefaultRequestHandler,
        ) -> gadk_a2a::a2a_server::DefaultRequestHandler
        + Send
        + Sync,
>;

/// One of Go's `a2asrv.RequestHandlerOption`s that wraps the handler (Go call interceptors):
/// receives the handler built so far and returns the one to serve, typically a type of the
/// caller's that implements `a2a_server::RequestHandler` by checking or rewriting a call and
/// delegating to the inner handler. (a2a-rs 0.5.3's `InterceptedHandler` does not implement
/// `RequestHandler`, so it cannot be returned here.)
#[cfg(feature = "a2a")]
pub type A2AHandlerWrapper = Arc<
    dyn Fn(
            Arc<dyn gadk_a2a::a2a_server::RequestHandler>,
        ) -> Arc<dyn gadk_a2a::a2a_server::RequestHandler>
        + Send
        + Sync,
>;

/// Go `launcher.Config`: parameters for web and console execution (sessions, artifacts, agents
/// and so on).
pub struct Config {
    /// Go `SessionService`; `None` makes the web launcher use an in-memory one.
    pub session_service: Option<Arc<dyn SessionService>>,
    /// Go `ArtifactService`; `None` makes the web launcher use an in-memory one.
    pub artifact_service: Option<Arc<dyn ArtifactService>>,
    /// Go `MemoryService`; `None` makes the web launcher use an in-memory one.
    pub memory_service: Option<Arc<dyn MemoryService>>,
    /// Go `AgentLoader`.
    pub agent_loader: Arc<dyn Loader>,
    /// Go `A2AOptions` that configure the `a2a` sub-launcher's default request handler, applied
    /// in order before [`Config::a2a_wrappers`].
    #[cfg(feature = "a2a")]
    pub a2a_options: Vec<A2AHandlerOption>,
    /// Go `A2AOptions` that wrap the `a2a` sub-launcher's request handler, applied in order
    /// (each wraps the previous one) after [`Config::a2a_options`].
    #[cfg(feature = "a2a")]
    pub a2a_wrappers: Vec<A2AHandlerWrapper>,
    /// Go `PluginConfig`.
    pub plugin_config: gadk_engine::runner::PluginConfig,
    /// Go `TelemetryOptions`: the options the launcher builds gadk-telemetry's providers with
    /// (Go `telemetry.New`). The launcher clones the list (appending
    /// `with_otel_to_cloud(true)` for `-otel_to_cloud`), so this one is never changed.
    pub telemetry_options: Vec<gadk_telemetry::TelemetryOption>,
    /// Rust-only: extra `tracing` layers for the launcher's global subscriber, installed after
    /// the providers' bridge layer (Go's `api` sub-launcher appends the REST server's debug span
    /// and log processors to `TelemetryOptions`; gadk's adds its debug layer here). The launcher
    /// takes them when it installs the subscriber.
    pub telemetry_layers: Vec<BoxedLayer>,
    /// Go `Authenticator`: when set, authenticates inbound requests to every REST API endpoint
    /// except the public ones (like `/health` and `/version`). It protects the REST API alone; the
    /// other routes, A2A among them, have no authentication. `None` uses `authn::noop()`.
    #[cfg(feature = "web")]
    pub authenticator: Option<Arc<dyn gadk_server::authn::Authenticator>>,
    /// Go `BindHost`: the address the server is bound to, as the web launcher resolved it from
    /// `-host`. A loopback value arms the REST server's Host check; empty turns it off.
    pub bind_host: String,
    /// Go `AllowedOrigins`: the web origins allowed to call the web launcher's server from a
    /// browser (`scheme://host[:port]`; a bare host or `host:port` is read as http; `*` turns the
    /// origin and DNS-rebinding checks off). The web launcher's `-allow_origins` flag appends to
    /// it, and a sub-launcher appends the origins it serves in `setup_subrouters`.
    pub allowed_origins: Vec<String>,
    /// Go `Authorizer`: checks that the calling user and the user in the payload match. `None`
    /// uses `authz::noop()`.
    #[cfg(feature = "web")]
    pub authorizer: Option<Arc<dyn gadk_server::authz::Authorizer>>,
    /// Go `Compaction`: context compaction for every session the launcher's runners drive
    /// (process-wide); `None`, the default, disables it.
    pub compaction: Option<gadk_session::compaction::Config>,
    /// Go `MaxPayloadSize`: the request body limit in bytes; the web launcher sets it from
    /// `-max_request_body_size`. Zero or negative uses the REST server's default (10 MiB).
    pub max_payload_size: i64,
    /// Rust-only: where the launcher's Go `log` lines go (default stderr).
    pub log: Logger,
}

impl Config {
    /// A config serving `agent_loader`, every other field at Go's zero value; `log` writes to
    /// stderr.
    pub fn new(agent_loader: Arc<dyn Loader>) -> Config {
        Config {
            session_service: None,
            artifact_service: None,
            memory_service: None,
            agent_loader,
            #[cfg(feature = "a2a")]
            a2a_options: Vec::new(),
            #[cfg(feature = "a2a")]
            a2a_wrappers: Vec::new(),
            plugin_config: gadk_engine::runner::PluginConfig::default(),
            telemetry_options: Vec::new(),
            telemetry_layers: Vec::new(),
            #[cfg(feature = "web")]
            authenticator: None,
            bind_host: String::new(),
            allowed_origins: Vec::new(),
            #[cfg(feature = "web")]
            authorizer: None,
            compaction: None,
            max_payload_size: 0,
            log: Logger::stderr(),
        }
    }

    /// Go `(*Config).Validate`: reports a config that cannot work, before anything starts
    /// serving. An unusable compaction config is `invalid Compaction: <cause>`; `None` is fine.
    pub fn validate(&self) -> Result<(), Error> {
        if let Some(compaction) = &self.compaction {
            compaction.validate().map_err(|e| Error::Wrapped {
                context: "invalid Compaction".to_owned(),
                source: Box::new(e),
            })?;
        }
        Ok(())
    }
}
