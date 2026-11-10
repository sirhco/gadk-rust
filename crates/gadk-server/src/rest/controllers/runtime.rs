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

//! Port of Go `controllers/runtime.go` (`/run`, `/run_sse`; plan 5a Task 7) and
//! `internal/routers/runtime.go`. The controller's config and constructor are complete; the
//! `/run_live` handler is in [`super::live`].

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::sync::Arc;
use std::time::Duration;

use gadk_agent::Loader;
use gadk_artifact::ArtifactService;
use gadk_engine::runner::PluginConfig;
use gadk_memory::MemoryService;
use gadk_platform::Platform;
use gadk_session::SessionService;
use gadk_session::compaction::Config as CompactionConfig;
use http::Method;
use tokio::sync::Semaphore;

use crate::authz::{self, Authorizer};
use crate::rest::Timeout;
use crate::rest::routing::{Request, Response, Route, controller_handler, route};

/// Go `RuntimeAPIControllerConfig.CheckOrigin`: whether a `/run_live` upgrade carrying this
/// request's `Origin` may proceed.
pub type CheckOrigin = Arc<dyn Fn(&http::request::Parts) -> bool + Send + Sync>;

/// Go `defaultSSETimeout`.
pub(crate) const DEFAULT_SSE_TIMEOUT: Duration = Duration::from_secs(120);
/// Go `defaultMaxLiveMessageBytes` (16 MiB, uvicorn's `ws_max_size`).
pub(crate) const DEFAULT_MAX_LIVE_MESSAGE_BYTES: i64 = 16 << 20;
/// Go `defaultLiveKeepaliveTimeout`.
pub(crate) const DEFAULT_LIVE_KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(40);

/// Port of Go `controllers.RuntimeAPIControllerConfig`.
#[derive(Clone)]
pub struct RuntimeApiControllerConfig {
    /// The session service.
    pub session_service: Arc<dyn SessionService>,
    /// Optional memory service passed to every runner.
    pub memory_service: Option<Arc<dyn MemoryService>>,
    /// The agents served, by application name.
    pub agent_loader: Option<Arc<dyn Loader>>,
    /// Optional artifact service passed to every runner.
    pub artifact_service: Option<Arc<dyn ArtifactService>>,
    /// Write deadline of a `/run_sse` response, from when the request arrives: default 120 s.
    pub sse_timeout: Timeout,
    /// Plugins of every runner.
    pub plugin_config: PluginConfig,
    /// Create the session on run when it does not exist.
    pub auto_create_session: bool,
    /// `None` is [`authz::noop`].
    pub authorizer: Option<Arc<dyn Authorizer>>,
    /// Context compaction for every runner.
    pub compaction: Option<CompactionConfig>,
    /// Whether a `/run_live` upgrade with this request's `Origin` may proceed (false: 403);
    /// `None` keeps gorilla/websocket's default (no `Origin`, or its host equals `Host`).
    pub check_origin: Option<CheckOrigin>,
    /// Cap on one client message: zero means 16 MiB, negative no cap.
    pub max_live_message_bytes: i64,
    /// Drops a `/run_live` peer that stops answering pings: default 40 s.
    pub live_keepalive_timeout: Timeout,
    /// Cap on concurrent `/run_live` connections; zero or negative means no cap.
    pub max_live_sessions: i32,
    /// Clock and ID source of every runner.
    pub platform: Platform,
}

impl RuntimeApiControllerConfig {
    /// A config with `session_service` and every other field at Go's zero value.
    pub fn new(session_service: Arc<dyn SessionService>) -> Self {
        Self {
            session_service,
            memory_service: None,
            agent_loader: None,
            artifact_service: None,
            sse_timeout: Timeout::Default,
            plugin_config: PluginConfig::default(),
            auto_create_session: false,
            authorizer: None,
            compaction: None,
            check_origin: None,
            max_live_message_bytes: 0,
            live_keepalive_timeout: Timeout::Default,
            max_live_sessions: 0,
            platform: Platform::default(),
        }
    }
}

/// Port of Go `controllers.RuntimeAPIController`. Fields hold the resolved settings.
pub struct RuntimeApiController {
    /// `None`: no write deadline.
    pub(crate) sse_timeout: Option<Duration>,
    pub(crate) session_service: Arc<dyn SessionService>,
    pub(crate) memory_service: Option<Arc<dyn MemoryService>>,
    pub(crate) artifact_service: Option<Arc<dyn ArtifactService>>,
    pub(crate) agent_loader: Option<Arc<dyn Loader>>,
    pub(crate) plugin_config: PluginConfig,
    pub(crate) auto_create_session: bool,
    pub(crate) authorizer: Arc<dyn Authorizer>,
    pub(crate) check_origin: Option<CheckOrigin>,
    /// Resolved cap (zero became 16 MiB); negative: no cap.
    pub(crate) max_live_message_bytes: i64,
    /// `None`: keepalive off.
    pub(crate) live_keepalive_timeout: Option<Duration>,
    /// Half the keepalive timeout (at least 1 ns); `None` when the keepalive is off.
    pub(crate) live_ping_interval: Option<Duration>,
    /// One permit per live session the controller may serve at once; `None`: no cap.
    pub(crate) live_slots: Option<Arc<Semaphore>>,
    /// The permits `live_slots` was built with (Go `cap(c.liveSlots)`); 0 when uncapped.
    pub(crate) live_slot_cap: usize,
    pub(crate) compaction: Option<CompactionConfig>,
    pub(crate) platform: Platform,
}

impl RuntimeApiController {
    /// Go `NewRuntimeAPIControllerWithConfig`: resolves the defaults and builds the live-session
    /// semaphore.
    pub fn new(cfg: RuntimeApiControllerConfig) -> Self {
        let max_live_message_bytes = match cfg.max_live_message_bytes {
            0 => DEFAULT_MAX_LIVE_MESSAGE_BYTES,
            n => n,
        };
        let live_keepalive_timeout = cfg
            .live_keepalive_timeout
            .resolve(DEFAULT_LIVE_KEEPALIVE_TIMEOUT);
        let live_ping_interval =
            live_keepalive_timeout.map(|t| (t / 2).max(Duration::from_nanos(1)));
        let live_slot_cap = usize::try_from(cfg.max_live_sessions).unwrap_or(0);
        let live_slots = (live_slot_cap > 0).then(|| Arc::new(Semaphore::new(live_slot_cap)));
        Self {
            sse_timeout: cfg.sse_timeout.resolve(DEFAULT_SSE_TIMEOUT),
            session_service: cfg.session_service,
            memory_service: cfg.memory_service,
            artifact_service: cfg.artifact_service,
            agent_loader: cfg.agent_loader,
            plugin_config: cfg.plugin_config,
            auto_create_session: cfg.auto_create_session,
            authorizer: cfg.authorizer.unwrap_or_else(authz::noop),
            check_origin: cfg.check_origin,
            max_live_message_bytes,
            live_keepalive_timeout,
            live_ping_interval,
            live_slots,
            live_slot_cap,
            compaction: cfg.compaction,
            platform: cfg.platform,
        }
    }

    /// Go `RunHandler` (through `NewErrorHandler`): the run's events as a JSON array.
    pub async fn run_handler(&self, req: Request) -> Response {
        todo!("plan 5a Task 7")
    }

    /// Go `RunSSEHandler`: the run's events as server-sent events.
    pub async fn run_sse_handler(&self, req: Request) -> Response {
        todo!("plan 5a Task 7")
    }
}

/// Go `RuntimeAPIRouter.Routes`.
pub(crate) fn runtime_routes(c: Arc<RuntimeApiController>) -> Vec<Route> {
    vec![
        route(
            "RunAgent",
            &[Method::POST],
            "/run",
            controller_handler!(c, run_handler),
        ),
        route(
            "RunAgentSse",
            &[Method::POST],
            "/run_sse",
            controller_handler!(c, run_sse_handler),
        ),
        route(
            "RunAgentLive",
            &[Method::GET],
            "/run_live",
            controller_handler!(c, run_live_handler),
        ),
    ]
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn controller(f: impl FnOnce(&mut RuntimeApiControllerConfig)) -> RuntimeApiController {
        let mut cfg =
            RuntimeApiControllerConfig::new(Arc::new(gadk_session::InMemorySessionService::new()));
        f(&mut cfg);
        RuntimeApiController::new(cfg)
    }

    #[test]
    fn new_resolves_go_defaults() {
        let c = controller(|_| {});
        assert_eq!(c.sse_timeout, Some(DEFAULT_SSE_TIMEOUT));
        assert_eq!(c.max_live_message_bytes, 16 << 20);
        assert_eq!(c.live_keepalive_timeout, Some(Duration::from_secs(40)));
        assert_eq!(c.live_ping_interval, Some(Duration::from_secs(20)));
        assert!(c.live_slots.is_none());
    }

    #[test]
    fn new_honors_disabled_and_explicit_settings() {
        let c = controller(|cfg| {
            cfg.sse_timeout = Timeout::Disabled;
            cfg.max_live_message_bytes = -1;
            cfg.live_keepalive_timeout = Timeout::Disabled;
            cfg.max_live_sessions = 2;
        });
        assert_eq!(c.sse_timeout, None);
        assert_eq!(c.max_live_message_bytes, -1);
        assert_eq!(
            (c.live_keepalive_timeout, c.live_ping_interval),
            (None, None)
        );
        assert_eq!(c.live_slots.unwrap().available_permits(), 2);

        // Go: a timeout under 2 ns would halve to zero, which time.NewTicker rejects.
        let c =
            controller(|cfg| cfg.live_keepalive_timeout = Timeout::After(Duration::from_nanos(1)));
        assert_eq!(c.live_ping_interval, Some(Duration::from_nanos(1)));
    }
}
