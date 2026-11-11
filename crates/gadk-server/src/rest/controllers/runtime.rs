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

//! Port of Go `controllers/runtime.go` (`/run`, `/run_sse`) and `internal/routers/runtime.go`.
//! The `/run_live` handler is in [`super::live`].

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use bytes::Bytes;
use futures::{Stream, StreamExt};
use gadk_agent::{EventStream, Loader, RunConfig, StreamingMode};
use gadk_artifact::ArtifactService;
use gadk_engine::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use gadk_memory::MemoryService;
use gadk_platform::{Platform, go_quote};
use gadk_session::compaction::Config as CompactionConfig;
use gadk_session::{GetRequest, SessionService};
use http::header::{CACHE_CONTROL, CONNECTION, CONTENT_TYPE};
use http::{HeaderValue, Method, StatusCode};
use tokio::sync::Semaphore;
use tokio::time::Instant;

use super::handlers::{forbid_unless_can_act_as, session_error_text};
use crate::authz::{self, Authorizer};
use crate::mux::WriteDeadline;
use crate::rest::Timeout;
use crate::rest::http_error::{
    StatusError, encode_json_response, http_error, status_error_response,
};
use crate::rest::models::{Event, RunAgentRequest};
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

    /// Go `RunHandler` (through `NewErrorHandler`): the run's events as a JSON array (`null`
    /// when the run yields none, as Go encodes a nil slice).
    pub async fn run_handler(&self, req: Request) -> Response {
        match self.run(req).await {
            Ok(resp) => resp,
            Err(err) => status_error_response(&err),
        }
    }

    /// Go `RunHandler`'s body; an `Err` is what Go returns to `NewErrorHandler`.
    async fn run(&self, req: Request) -> Result<Response, StatusError> {
        let (parts, body) = req.into_parts();
        let run_agent_request = decode_request_body(body).await?;
        if let Some(resp) = forbid_unless_can_act_as(
            &*self.authorizer,
            &parts.extensions,
            &run_agent_request.user_id,
        ) {
            return Ok(resp);
        }
        let session_events = self.run_agent(run_agent_request).await?;
        let events: Option<Vec<Event>> = (!session_events.is_empty()).then(|| {
            session_events
                .iter()
                .map(Event::from_session_event)
                .collect()
        });
        Ok(encode_json_response(&events, StatusCode::OK))
    }

    /// Go `runAgent`: runs to completion, skipping (and logging) compaction errors.
    async fn run_agent(
        &self,
        req: RunAgentRequest,
    ) -> Result<Vec<gadk_session::Event>, StatusError> {
        self.validate_session_exists(&req.app_name, &req.user_id, &req.session_id)
            .await?;
        let (runner, run_config) = self.get_runner(&req)?;
        let mut stream = start_run(&runner, req, run_config);
        let mut events = Vec::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(event) => events.push(event),
                // A compaction failure is bookkeeping, not the turn: the events are persisted
                // and the agent has answered.
                Err(err) if err.is_compaction() => {
                    tracing::warn!(target: "adk", "adkrest: {err}");
                }
                Err(err) => {
                    return Err(StatusError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("failed to run agent: {err}"),
                    ));
                }
            }
        }
        Ok(events)
    }

    /// Go `RunSSEHandler`: the run's events as server-sent events. The write deadline starts
    /// when the request arrives; once it passes no further frame is written and the
    /// connection is aborted (Go: the next write fails and the handler returns). Where the
    /// server offers a [`WriteDeadline`], the connection's write deadline is moved to it too,
    /// or cleared when there is no SSE timeout (Go `rc.SetWriteDeadline`, which overrides the
    /// server's write timeout).
    pub async fn run_sse_handler(&self, req: Request) -> Response {
        let deadline = self.sse_timeout.map(|t| Instant::now() + t);
        if let Some(hook) = req.extensions().get::<WriteDeadline>() {
            hook.set(deadline);
        }
        let (parts, body) = req.into_parts();
        let run_agent_request = match decode_request_body(body).await {
            Ok(r) => r,
            Err(err) => {
                return http_error(
                    StatusCode::BAD_REQUEST,
                    &format!("failed to decode request body: {err}"),
                );
            }
        };
        if let Some(resp) = forbid_unless_can_act_as(
            &*self.authorizer,
            &parts.extensions,
            &run_agent_request.user_id,
        ) {
            return resp;
        }
        if let Err(err) = self
            .validate_session_exists(
                &run_agent_request.app_name,
                &run_agent_request.user_id,
                &run_agent_request.session_id,
            )
            .await
        {
            return http_error(
                StatusCode::NOT_FOUND,
                &format!("failed to find the session: {err}"),
            );
        }
        let (runner, run_config) = match self.get_runner(&run_agent_request) {
            Ok(r) => r,
            Err(err) => {
                return http_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &format!("failed to get runner: {err}"),
                );
            }
        };

        // The charset is redundant for text/event-stream but stated, as Go does, for clients
        // that still apply the legacy ISO-8859-1 default to text/*.
        let stream = start_run(&runner, run_agent_request, run_config);
        let mut resp = Response::new(Body::from_stream(sse_frames(stream, deadline)));
        let h = resp.headers_mut();
        h.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("text/event-stream; charset=UTF-8"),
        );
        h.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
        h.insert(CONNECTION, HeaderValue::from_static("keep-alive"));
        resp
    }

    /// Go `validateSessionExists`: 404 `failed to get session: <err>`.
    async fn validate_session_exists(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
    ) -> Result<(), StatusError> {
        self.session_service
            .get(GetRequest {
                app_name: app_name.to_owned(),
                user_id: user_id.to_owned(),
                session_id: session_id.to_owned(),
                ..Default::default()
            })
            .await
            .map(|_| ())
            .map_err(|err| {
                StatusError::new(
                    StatusCode::NOT_FOUND,
                    format!(
                        "failed to get session: {}",
                        session_error_text(&err, session_id, false)
                    ),
                )
            })
    }

    /// Go `getRunner`: a runner for `req.app_name` built per request from the controller's
    /// services, plugins, compaction, auto-create flag and platform, and the run config
    /// (`Sse` streaming when `req.streaming`). Errors are 500s.
    pub(crate) fn get_runner(
        &self,
        req: &RunAgentRequest,
    ) -> Result<(Runner, RunConfig), StatusError> {
        let internal = |msg: String| StatusError::new(StatusCode::INTERNAL_SERVER_ERROR, msg);
        // Go dereferences a nil loader and panics; gadk reports it as a load failure.
        let loader = self
            .agent_loader
            .as_ref()
            .ok_or_else(|| internal("failed to load agent: no agent loader".to_owned()))?;
        let agent = loader
            .load_agent(&req.app_name)
            .map_err(|err| internal(format!("failed to load agent: {err}")))?;
        let runner = Runner::new(RunnerConfig {
            app_name: req.app_name.clone(),
            agent,
            session_service: self.session_service.clone(),
            artifact_service: self.artifact_service.clone(),
            memory_service: self.memory_service.clone(),
            plugin_config: self.plugin_config.clone(),
            auto_create_session: self.auto_create_session,
            platform: self.platform.clone(),
            compaction: self.compaction.clone(),
        })
        .map_err(|err| internal(format!("failed to create runner: {err}")))?;
        let streaming_mode = if req.streaming {
            StreamingMode::Sse
        } else {
            StreamingMode::None
        };
        Ok((
            runner,
            RunConfig {
                streaming_mode,
                ..Default::default()
            },
        ))
    }
}

/// Go `r.Run(ctx, userId, sessionId, &newMessage, cfg, opts...)` with `WithStateDelta` when the
/// request carries a state delta. `functionCallEventId` is accepted and ignored, as in Go.
fn start_run(runner: &Runner, req: RunAgentRequest, run_config: RunConfig) -> EventStream {
    runner.run(
        &req.user_id,
        &req.session_id,
        Some(req.new_message),
        run_config,
        RunOptions {
            state_delta: req.state_delta,
            ..Default::default()
        },
    )
}

/// Go `decodeRequestBody`: a `json.Decoder` with `DisallowUnknownFields` reading the first JSON
/// value of the body (trailing bytes are not read); errors are 400 `failed to decode request:
/// <Go decoder text>`.
async fn decode_request_body(body: Body) -> Result<RunAgentRequest, StatusError> {
    let bad_request = |msg: String| {
        StatusError::new(
            StatusCode::BAD_REQUEST,
            format!("failed to decode request: {msg}"),
        )
    };
    let bytes = axum::body::to_bytes(body, usize::MAX)
        .await
        .map_err(|err| bad_request(err.to_string()))?;
    let mut values = serde_json::Deserializer::from_slice(&bytes).into_iter::<RunAgentRequest>();
    match values.next() {
        Some(Ok(req)) => Ok(req),
        Some(Err(err)) => Err(bad_request(go_decode_error(&err))),
        // Go's Decoder on an empty (or all-whitespace) body.
        None => Err(bad_request("EOF".to_owned())),
    }
}

/// Go `json.Decoder` text for the serde errors Go and the ADK clients can tell apart: an
/// unknown field is `json: unknown field "<name>"` and a truncated value `unexpected EOF`.
/// Other syntax and type errors keep serde_json's text (a recorded 5a divergence).
fn go_decode_error(err: &serde_json::Error) -> String {
    if err.is_eof() {
        return "unexpected EOF".to_owned();
    }
    let msg = err.to_string();
    if let Some(rest) = msg.strip_prefix("unknown field `")
        && let Some(end) = rest.find("`, ")
    {
        return format!("json: unknown field {}", go_quote(&rest[..end]));
    }
    msg
}

/// The SSE write deadline passed: ends the body with an error, so the connection is aborted
/// without the chunked terminator, as Go's failed write leaves it.
#[derive(Debug, thiserror::Error)]
#[error("i/o timeout")]
struct WriteTimeout;

/// Go `RunSSEHandler`'s event loop: one `data: <event JSON>\n\n` frame per event, one
/// `event: error\ndata: {"error":"<text>"}\n\n` frame per run error (the stream continues),
/// compaction errors logged and skipped, and an [`WriteTimeout`] error once `deadline` passes.
fn sse_frames(
    events: EventStream,
    deadline: Option<Instant>,
) -> impl Stream<Item = Result<Bytes, WriteTimeout>> + Send + 'static {
    futures::stream::unfold(Some(events), move |state| async move {
        let mut events = state?;
        loop {
            let next = match deadline {
                Some(d) => match tokio::time::timeout_at(d, events.next()).await {
                    Ok(next) => next,
                    Err(_) => return Some((Err(WriteTimeout), None)),
                },
                None => events.next().await,
            }?;
            let frame = match next {
                Ok(event) => match gadk_model::to_go_json(&Event::from_session_event(&event)) {
                    Ok(json) => data_frame(&json),
                    Err(err) => {
                        // Not reported to the client, to avoid recursive error reporting.
                        tracing::warn!(target: "adk", "failed to marshal event: {err}");
                        return None;
                    }
                },
                // Bookkeeping, not the turn: the client already has its answer.
                Err(err) if err.is_compaction() => {
                    tracing::warn!(target: "adk", "adkrest: {err}");
                    continue;
                }
                Err(err) => {
                    let payload = BTreeMap::from([("error", err.to_string())]);
                    match gadk_model::to_go_json(&payload) {
                        Ok(json) => {
                            let mut frame = b"event: error\n".to_vec();
                            frame.extend_from_slice(&data_frame(&json));
                            Bytes::from(frame)
                        }
                        Err(err) => {
                            tracing::warn!(
                                target: "adk",
                                "failed to flash error event: marshal error event: {err}"
                            );
                            return None;
                        }
                    }
                }
            };
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return Some((Err(WriteTimeout), None));
            }
            return Some((Ok(frame), Some(events)));
        }
    })
}

/// Go `flashEvent`: `data: <json>\n\n`.
fn data_frame(json: &[u8]) -> Bytes {
    let mut frame = Vec::with_capacity(json.len() + 8);
    frame.extend_from_slice(b"data: ");
    frame.extend_from_slice(json);
    frame.extend_from_slice(b"\n\n");
    Bytes::from(frame)
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

    async fn decode(body: &str) -> Result<RunAgentRequest, String> {
        decode_request_body(Body::from(body.to_owned()))
            .await
            .map_err(|e| {
                assert_eq!(e.status, StatusCode::BAD_REQUEST);
                e.message
            })
    }

    /// Go 1.27 `json.Decoder` (with `DisallowUnknownFields`) texts for the errors a client can
    /// provoke without a type mismatch, and the decoder's first-value-only read.
    #[tokio::test]
    async fn decode_request_body_follows_go_decoder() {
        for (body, want) in [
            ("", "failed to decode request: EOF"),
            ("  \n", "failed to decode request: EOF"),
            (r#"{"appName":"#, "failed to decode request: unexpected EOF"),
            (
                r#"{"appName":"a","bogus":1}"#,
                r#"failed to decode request: json: unknown field "bogus""#,
            ),
            (
                r#"{"we\"ird":1}"#,
                r#"failed to decode request: json: unknown field "we\"ird""#,
            ),
        ] {
            assert_eq!(decode(body).await.unwrap_err(), want, "{body:?}");
        }
        // Go reads one value and leaves the rest of the body unread.
        let req = decode(r#"{"appName":"a","streaming":null} trailing"#)
            .await
            .unwrap();
        assert_eq!((req.app_name.as_str(), req.streaming), ("a", false));
    }

    #[test]
    fn sse_frames_use_go_framing() {
        assert_eq!(&data_frame(b"{}")[..], b"data: {}\n\n");
    }
}
