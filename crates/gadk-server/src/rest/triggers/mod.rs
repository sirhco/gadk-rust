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

//! Pub/Sub and Eventarc push triggers (port of Go `server/adkrest/controllers/triggers`): each
//! delivery runs the agent in a fresh session, retrying a 429 with Go's jittered backoff. The
//! controllers' [`handler`](PubSubController::handler) is an axum `MethodRouter` the caller mounts
//! at a path of its choosing (plan 5a Task 10).

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, FailedToBufferBody};
use axum::extract::{FromRequest, FromRequestParts, RawPathParams};
use futures::StreamExt;
use gadk_agent::{AgentError, Loader, RunConfig, StreamingMode};
use gadk_artifact::ArtifactService;
use gadk_engine::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use gadk_genai::{Content, ROLE_USER};
use gadk_memory::MemoryService;
use gadk_platform::{GoWrapped, Platform};
use gadk_session::compaction::Config as CompactionConfig;
use gadk_session::{CreateRequest, SessionService};
use http::StatusCode;
use serde::de::DeserializeOwned;
use tokio::sync::{Semaphore, SemaphorePermit};

use crate::rest::PathVars;
use crate::rest::controllers::handlers::path_vars;
use crate::rest::http_error::encode_json_response;
use crate::rest::middleware::is_max_bytes_error;
use crate::rest::models::TriggerResponse;
use crate::rest::routing::{Request, Response};

mod config;
mod eventarc;
mod pubsub;

pub use config::TriggerConfig;
pub use eventarc::EventarcController;
pub use pubsub::PubSubController;

/// Port of Go `triggers.ControllerConfig`.
#[derive(Clone)]
pub struct ControllerConfig {
    /// The session service.
    pub session_service: Arc<dyn SessionService>,
    /// The agents served, by application name.
    pub agent_loader: Arc<dyn Loader>,
    /// Optional memory service passed to every runner.
    pub memory_service: Option<Arc<dyn MemoryService>>,
    /// Optional artifact service passed to every runner.
    pub artifact_service: Option<Arc<dyn ArtifactService>>,
    /// Plugins of every runner.
    pub plugin_config: PluginConfig,
    /// Retry and concurrency settings.
    pub trigger_config: TriggerConfig,
    /// Context compaction for every runner.
    ///
    /// A delivery gets a session of its own, so a sliding window (which counts completed
    /// invocations within one session) has little to count; tail retention works normally,
    /// because it measures the prompt inside a single run. A sliding-window config is accepted
    /// and logged as a warning (Go `newRetriableRunner`).
    pub compaction: Option<CompactionConfig>,
    /// Clock and ID source of every runner.
    pub platform: Platform,
}

/// Go's `log.Printf` text for a sliding window with `CompactionInterval` 1.
const WARN_INTERVAL_ONE: &str = "adk: sliding-window compaction is configured on a trigger \
     controller with CompactionInterval 1, so it fires on every delivery and writes a summary \
     into a session that is discarded when the delivery ends. Use TokenThreshold and \
     EventRetentionSize to compact within a single run.";

/// Go's `log.Printf` text for a sliding window with `CompactionInterval` above 1.
const WARN_INTERVAL_ABOVE_ONE: &str = "adk: sliding-window compaction is configured on a \
     trigger controller, but a delivery handled in one attempt runs a single invocation, so the \
     window will not reach its interval. Use TokenThreshold and EventRetentionSize to compact \
     within a single run.";

/// Port of Go `triggers.RetriableRunner`: runs an agent with retry semantics, creating a new
/// session per delivery.
pub struct RetriableRunner {
    cfg: ControllerConfig,
}

impl RetriableRunner {
    /// Go `newRetriableRunner`: logs (as a `tracing` warning) a sliding-window compaction config,
    /// which is accepted but cannot do what it does elsewhere.
    fn new(cfg: ControllerConfig) -> Self {
        match cfg.compaction.as_ref().map(|c| c.compaction_interval) {
            Some(1) => tracing::warn!("{WARN_INTERVAL_ONE}"),
            Some(n) if n > 1 => tracing::warn!("{WARN_INTERVAL_ABOVE_ONE}"),
            _ => {}
        }
        Self { cfg }
    }

    /// Go `validateCompaction`: whether the compaction config can serve every app of the loader.
    /// The base runner config's agent is a placeholder the validation replaces per app.
    fn validate_compaction(&self) -> Result<(), AgentError> {
        let base = RunnerConfig {
            app_name: String::new(),
            agent: self.cfg.agent_loader.root_agent(),
            session_service: self.cfg.session_service.clone(),
            artifact_service: self.cfg.artifact_service.clone(),
            memory_service: self.cfg.memory_service.clone(),
            plugin_config: self.cfg.plugin_config.clone(),
            auto_create_session: false,
            platform: self.cfg.platform.clone(),
            compaction: None,
        };
        gadk_engine::compactionvalidate::against_agents(
            self.cfg.compaction.as_ref(),
            Some(self.cfg.agent_loader.as_ref()),
            &base,
        )
    }

    /// Go `RunAgent`: runs the agent of `app_name` for `message_content` as `user_id` and returns
    /// the resulting events.
    ///
    /// One session per delivery; retries of that delivery reuse it, so a throttled message
    /// accumulates invocations rather than starting over.
    pub async fn run_agent(
        &self,
        app_name: &str,
        user_id: &str,
        message_content: &str,
    ) -> Result<Vec<gadk_session::Event>, AgentError> {
        let session = self
            .cfg
            .session_service
            .create(CreateRequest {
                app_name: app_name.to_owned(),
                user_id: user_id.to_owned(),
                ..Default::default()
            })
            .await
            .map_err(|e| wrap("failed to create session", e))?;

        let user_message = Content::from_text(message_content, ROLE_USER);

        let agent = self
            .cfg
            .agent_loader
            .load_agent(app_name)
            .map_err(|e| wrap("failed to load agent", e))?;

        let runner = Runner::new(RunnerConfig {
            app_name: app_name.to_owned(),
            agent,
            session_service: self.cfg.session_service.clone(),
            artifact_service: self.cfg.artifact_service.clone(),
            memory_service: self.cfg.memory_service.clone(),
            plugin_config: self.cfg.plugin_config.clone(),
            auto_create_session: false,
            platform: self.cfg.platform.clone(),
            compaction: self.cfg.compaction.clone(),
        })
        .map_err(|e| wrap("failed to create runner", e))?;

        self.run_agent_with_retry(&runner, session.user_id(), session.id(), user_message)
            .await
    }

    /// Go `runAgentWithRetry`: exponential backoff with jitter on a 429. After `max_retries` the
    /// error is returned so the upstream service (Pub/Sub, Eventarc) retries at a higher level.
    /// A negative `max_retries` runs nothing and succeeds with no events (Go's loop never
    /// enters).
    async fn run_agent_with_retry(
        &self,
        runner: &Runner,
        user_id: &str,
        session_id: &str,
        user_message: Content,
    ) -> Result<Vec<gadk_session::Event>, AgentError> {
        let tc = self.cfg.trigger_config;
        let mut events = Vec::new();
        for attempt in 0..=tc.max_retries {
            let mut stream = runner.run(
                user_id,
                session_id,
                Some(user_message.clone()),
                RunConfig {
                    streaming_mode: StreamingMode::None,
                    ..Default::default()
                },
                RunOptions::default(),
            );
            let mut run_err = None;
            let mut throttled = false;
            while let Some(item) = stream.next().await {
                match item {
                    Ok(event) => events.push(event),
                    // A compaction failure is bookkeeping, not the delivery: the agent has
                    // answered and its events are persisted, so failing here would NACK a
                    // handled message.
                    Err(e) if e.is_compaction() => tracing::warn!("triggers: {e}"),
                    Err(e) => {
                        throttled = is_resource_exhausted(&e);
                        run_err = Some(e);
                        break;
                    }
                }
            }
            drop(stream);

            let Some(err) = run_err else {
                return Ok(events);
            };
            if attempt < tc.max_retries && throttled {
                let delay = calculate_backoff(attempt, tc.base_delay, tc.max_delay, rand::random());
                tokio::time::sleep(delay).await;
                continue;
            }
            return Err(err);
        }
        Ok(Vec::new())
    }
}

/// Go `fmt.Errorf("<prefix>: %w", err)` as an [`AgentError`].
fn wrap(prefix: &str, err: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> AgentError {
    AgentError::Other(Box::new(GoWrapped::new(prefix, err)))
}

/// Go `isResourceExhausted`: whether the error text names a transient rate limit.
fn is_resource_exhausted(err: &AgentError) -> bool {
    let text = err.to_string();
    text.contains("429") || text.contains("ResourceExhausted")
}

/// Go `calculateBackoff` with Go's float math: `min(base * 2^attempt, max_delay)` plus a jitter
/// of up to half of that; `random` is Go's `rand.Float64()` (in `[0, 1)`). Float-to-duration
/// conversions truncate as Go's do; a backoff beyond `u64` nanoseconds saturates (Go's
/// conversion is implementation-defined there).
fn calculate_backoff(attempt: i32, base: Duration, max_delay: Duration, random: f64) -> Duration {
    let backoff = base.as_nanos() as f64 * 2f64.powi(attempt);
    let delay = Duration::from_nanos(backoff as u64).min(max_delay);
    let jitter = Duration::from_nanos((random * delay.as_nanos() as f64 * 0.5) as u64);
    delay.saturating_add(jitter)
}

/// Go `respondError`: `{"status":"<msg>"}` with `code`.
fn respond_error(code: StatusCode, msg: &str) -> Response {
    encode_json_response(
        &TriggerResponse {
            status: msg.to_owned(),
        },
        code,
    )
}

/// Go `respondSuccess`: 200 `{"status":"success"}`.
fn respond_success() -> Response {
    encode_json_response(
        &TriggerResponse {
            status: "success".to_owned(),
        },
        StatusCode::OK,
    )
}

/// Go `appName(r)`: the `app_name` path variable.
fn app_name(req: &Request) -> Result<String, String> {
    path_vars(req)
        .0
        .remove("app_name")
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "no application name provided".to_owned())
}

/// A request body that could not be read.
#[derive(Debug)]
enum BodyError {
    /// The body exceeded the configured limit (Go `*http.MaxBytesError`).
    TooLarge,
    /// Any other read failure.
    Other(String),
}

impl fmt::Display for BodyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // Go `http.MaxBytesError.Error()`.
            BodyError::TooLarge => f.write_str("http: request body too large"),
            BodyError::Other(msg) => f.write_str(msg),
        }
    }
}

/// Reads the whole body under axum's limit: the caller's `DefaultBodyLimit` (axum's default when
/// none is set), a length-limited body, or a body limited by [`max_bytes_middleware`] (Go
/// `http.MaxBytesReader`, which the web launcher applies to every route); exceeding any of them is
/// [`BodyError::TooLarge`].
///
/// [`max_bytes_middleware`]: crate::rest::max_bytes_middleware
async fn read_body(req: Request) -> Result<Bytes, BodyError> {
    match Bytes::from_request(req, &()).await {
        Ok(body) => Ok(body),
        Err(BytesRejection::FailedToBufferBody(FailedToBufferBody::LengthLimitError(_))) => {
            Err(BodyError::TooLarge)
        }
        Err(BytesRejection::FailedToBufferBody(FailedToBufferBody::UnknownBodyError(e)))
            if is_max_bytes_error(&e) =>
        {
            Err(BodyError::TooLarge)
        }
        Err(e) => Err(BodyError::Other(e.to_string())),
    }
}

/// Go `json.NewDecoder(r.Body).Decode(&v)` into a struct: decodes the first JSON value and
/// ignores what follows it; an empty body is Go's `EOF`; `null` leaves the zero value.
fn decode_first<T: DeserializeOwned + Default>(body: &[u8]) -> Result<T, String> {
    // Divergence (ledger 5a): nesting past 127 is serde's error (Go: 10000); see 5c.md.
    match serde_json::Deserializer::from_slice(body)
        .into_iter::<Option<T>>()
        .next()
    {
        None => Err("EOF".to_owned()),
        Some(Ok(v)) => Ok(v.unwrap_or_default()),
        Some(Err(e)) => Err(e.to_string()),
    }
}

/// Go `json.Unmarshal(data, &v)` into a struct: the whole input is one JSON value; an empty
/// input is Go's `unexpected end of JSON input`; `null` leaves the zero value.
fn unmarshal<T: DeserializeOwned + Default>(data: &[u8]) -> Result<T, String> {
    if data.iter().all(u8::is_ascii_whitespace) {
        return Err("unexpected end of JSON input".to_owned());
    }
    serde_json::from_slice::<Option<T>>(data)
        .map(Option::unwrap_or_default)
        .map_err(|e| e.to_string())
}

/// Go's `semaphore` channel: `None` (no limit) when `max_concurrent_runs` is not positive.
fn semaphore(max_concurrent_runs: i32) -> Option<Semaphore> {
    usize::try_from(max_concurrent_runs)
        .ok()
        .filter(|&n| n > 0)
        .map(Semaphore::new)
}

/// Waits for a run slot (Go `c.semaphore <- struct{}{}`); the permit frees it when dropped.
async fn acquire(semaphore: Option<&Semaphore>) -> Option<SemaphorePermit<'_>> {
    match semaphore {
        // The semaphore is never closed, so acquiring only fails if it were.
        Some(s) => s.acquire().await.ok(),
        None => None,
    }
}

/// Inserts the route's path parameters as [`PathVars`] (Go `mux.Vars`) unless the request
/// already carries them, so a handler mounted on an axum route reads `{app_name}` as Go does.
async fn with_path_vars(req: Request) -> Request {
    if req.extensions().get::<PathVars>().is_some() {
        return req;
    }
    let (mut parts, body) = req.into_parts();
    if let Ok(params) = RawPathParams::from_request_parts(&mut parts, &()).await {
        let vars = params
            .iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        parts.extensions.insert(PathVars(vars));
    }
    Request::from_parts(parts, body)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    /// A body cut by `max_bytes_middleware` (the web launcher's Go `http.MaxBytesReader`) is Go's
    /// `*http.MaxBytesError`, like axum's own length limit.
    #[tokio::test]
    async fn read_body_reports_the_max_bytes_limit_as_too_large() {
        let seen = Arc::new(std::sync::Mutex::new(None));
        let inner = {
            let seen = Arc::clone(&seen);
            crate::mux::handler(move |req: Request| {
                let seen = Arc::clone(&seen);
                async move {
                    *seen.lock().unwrap() = Some(read_body(req).await.map_err(|e| e.to_string()));
                    Response::default()
                }
            })
        };
        let limited = crate::rest::max_bytes_middleware(4)(inner);
        let req = |body: &'static str| {
            http::Request::builder()
                .method("POST")
                .uri("/")
                .body(axum::body::Body::from(body))
                .unwrap()
        };
        limited(req("abcdef")).await;
        assert!(matches!(
            seen.lock().unwrap().take(),
            Some(Err(e)) if e == "http: request body too large"
        ));
        limited(req("abcd")).await;
        assert!(matches!(
            seen.lock().unwrap().take(),
            Some(Ok(b)) if b.as_ref() == b"abcd"
        ));
    }

    /// Go `calculateBackoff(attempt, 1ms, 5ms)` with `rand.Float64()` at its extremes.
    #[test]
    fn calculate_backoff_follows_go_math() {
        let cases = [
            // (attempt, random, want)
            (0, 0.0, MS),
            (0, 0.5, MS + MS / 4),
            (1, 0.0, 2 * MS),
            (2, 0.0, 4 * MS),
            (3, 0.0, 5 * MS),
            (3, 0.999, Duration::from_nanos(5_000_000 + 2_497_500)),
            (40, 0.0, 5 * MS),
        ];
        for (attempt, random, want) in cases {
            assert_eq!(
                calculate_backoff(attempt, MS, 5 * MS, random),
                want,
                "attempt {attempt}, random {random}"
            );
        }
        // Truncation, as Go's time.Duration(float64) conversion.
        assert_eq!(
            calculate_backoff(0, Duration::from_nanos(3), Duration::from_secs(1), 0.5),
            Duration::from_nanos(3)
        );
        // A backoff past u64 nanoseconds saturates and is then capped.
        assert_eq!(
            calculate_backoff(200, Duration::from_secs(1), 5 * MS, 0.0),
            5 * MS
        );
    }

    #[test]
    fn is_resource_exhausted_matches_go_substrings() {
        assert!(is_resource_exhausted(&AgentError::other("429 Too Many")));
        assert!(is_resource_exhausted(&AgentError::other(
            "rpc error: ResourceExhausted"
        )));
        assert!(!is_resource_exhausted(&AgentError::other(
            "RESOURCE_EXHAUSTED"
        )));
        assert!(!is_resource_exhausted(&AgentError::other("boom")));
    }

    #[test]
    fn decode_first_ignores_trailing_data_and_reports_eof() {
        let v: serde_json::Value = decode_first(br#"{"a":1} trailing"#).unwrap();
        assert_eq!(v, serde_json::json!({"a": 1}));
        assert_eq!(decode_first::<serde_json::Value>(b"").unwrap_err(), "EOF");
        assert_eq!(
            decode_first::<serde_json::Value>(b" \n").unwrap_err(),
            "EOF"
        );
        assert_eq!(
            unmarshal::<serde_json::Value>(b"").unwrap_err(),
            "unexpected end of JSON input"
        );
        assert!(unmarshal::<serde_json::Value>(br#"{"a":1} x"#).is_err());
        // Go leaves a struct untouched on `null`.
        let v: crate::rest::models::TriggerResponse = unmarshal(b" null ").unwrap();
        assert_eq!(v.status, "");
        let v: crate::rest::models::TriggerResponse = decode_first(b"null").unwrap();
        assert_eq!(v.status, "");
    }

    #[test]
    fn semaphore_is_none_unless_positive() {
        assert!(semaphore(0).is_none());
        assert!(semaphore(-1).is_none());
        assert_eq!(semaphore(3).unwrap().available_permits(), 3);
    }
}
