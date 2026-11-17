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

//! `NodeConfig` and `RetryConfig`.

use std::sync::Arc;
use std::time::Duration;

use gadk_agent::AgentError;

use crate::error::{WorkflowErrorKind, workflow_error};

/// The configuration for a node.
///
/// All fields are optional. The `Option<bool>` fields (`rerun_on_resume`, `wait_for_output`) are
/// tri-state so node implementations can distinguish "user explicitly opted out" (`Some(false)`)
/// from "user did not configure" (`None`).
#[derive(Clone, Default)]
pub struct NodeConfig {
    /// When true, runs the node concurrently for each item of a list-typed input. The engine
    /// collects per-item outputs and emits a single aggregate output event.
    pub parallel_worker: bool,

    /// Controls human-in-the-loop resume behavior: `Some(true)` re-runs the interrupted node from
    /// scratch on resume (re-entry mode), `Some(false)` routes the resume payload to the node's
    /// successor as input (handoff mode), and `None` defers to the engine. The engine currently
    /// treats `None` as handoff.
    pub rerun_on_resume: Option<bool>,

    /// When true, parks the parent (NodeInterrupted) to re-run instead of completing it when a
    /// child dispatched via RunNode finishes without setting an event output — letting a
    /// multi-turn child be retried rather than falsely completed with the zero value. Only honored
    /// on the RunNode (dynamic sub-scheduler) path today. `None` means "use the engine default" —
    /// false for most node kinds.
    pub wait_for_output: Option<bool>,

    /// When set, makes the scheduler retry this node on failure per the policy. `None` means
    /// "no retries".
    pub retry_config: Option<RetryConfig>,

    /// When set and non-zero, bounds a single activation of the node. `None` (and
    /// `Some(Duration::ZERO)`) means the node is bounded only by the parent invocation's deadline.
    pub timeout: Option<Duration>,

    /// When true, tells the scheduler not to wrap the node in an "invoke_node" telemetry span
    /// because the node body already starts its own span (e.g. an LlmAgent node whose wrapped
    /// agent emits "invoke_agent").
    pub emits_own_span: bool,
}

/// The parameters for retrying a failed node.
///
/// Recommended construction is via [`default_retry_config`] and overriding the fields you want to
/// customize. `RetryConfig::default()` is the zero value: a valid "no retry, no backoff, no
/// jitter" policy.
#[derive(Clone, Default)]
pub struct RetryConfig {
    /// Maximum number of attempts, including the original request. 0 or 1 means no retries.
    pub max_attempts: i32,

    /// Initial delay before the first retry.
    pub initial_delay: Duration,

    /// Maximum delay between retries.
    pub max_delay: Duration,

    /// The per-attempt multiplier applied to the delay. 1.0 means a constant `initial_delay`
    /// between retries; 2.0 means classic exponential backoff. Values < 1.0 shrink the delay each
    /// attempt (rare but permitted).
    pub backoff_factor: f64,

    /// A randomness factor in [0.0, 1.0]. The actual delay is sampled from
    /// `delay * (1 ± jitter)`. Zero means deterministic delays.
    pub jitter: f64,

    /// Predicate that defines when to retry (true means retry). `None` retries every error except
    /// input-validation failures.
    #[allow(clippy::type_complexity)] // the pinned C4 field type
    pub should_retry: Option<Arc<dyn Fn(&AgentError) -> bool + Send + Sync>>,
}

/// Retries everything except input-validation failures.
pub(crate) fn default_should_retry(err: &AgentError) -> bool {
    !workflow_error(err).is_some_and(|w| w.is(WorkflowErrorKind::InputValidation))
}

/// The default retry policy: 5 attempts, 1s initial delay, 60s cap, 2x backoff, full jitter. Every
/// error is retried except input-validation failures, which a retry cannot fix because the input is
/// deterministic.
pub fn default_retry_config() -> RetryConfig {
    RetryConfig {
        max_attempts: 5,
        initial_delay: Duration::from_secs(1),
        max_delay: Duration::from_secs(60),
        backoff_factor: 2.0,
        jitter: 1.0,
        should_retry: Some(Arc::new(default_should_retry)),
    }
}
