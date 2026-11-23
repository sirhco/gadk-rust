// Copyright 2026 Chris Olson
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

//! The data a guard compares: per-version calls, errors, latency and tokens, feedback scores
//! and runs of the two sides of a prompt rollout.
//!
//! A [`GuardSource`] answers three questions about the guard's calls — the `LLM_REQUEST` rows
//! of one prompt and label at the baseline or rollout version in `[from, to)`:
//! [`GuardSource::version_calls`], [`GuardSource::version_feedback`] and
//! [`GuardSource::version_runs`]. [`BigQueryGuardSource`](crate::BigQueryGuardSource) reads
//! them from the agent-analytics table; tests use a fake.

use chrono::{DateTime, Utc};

use crate::error::MonitorError;
use crate::guard_spec::FeedbackSource;
use crate::prices::TokenUsage;
use crate::source::ProductionRun;

/// The guard's calls: the `LLM_REQUEST` rows with prompt `prompt`, label `label` and version
/// `baseline` or `rollout`, logged in `[from, to)`.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardQuery {
    /// The prompt name.
    pub prompt: String,
    /// The label both sides of the rollout carry.
    pub label: String,
    /// The label's version (the baseline side).
    pub baseline: u32,
    /// The rollout version.
    pub rollout: u32,
    /// The window start (inclusive).
    pub from: DateTime<Utc>,
    /// The window end (exclusive).
    pub to: DateTime<Utc>,
}

impl GuardQuery {
    /// The calls of `prompt` / `label` at version `baseline` or `rollout` in `[from, to)`.
    pub fn new(
        prompt: impl Into<String>,
        label: impl Into<String>,
        baseline: u32,
        rollout: u32,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Self {
        Self {
            prompt: prompt.into(),
            label: label.into(),
            baseline,
            rollout,
            from,
            to,
        }
    }
}

/// The calls of one version to one model and their summed tokens.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct ModelTokens {
    /// The model of the calls (from the `LLM_REQUEST` row; `(unknown)` when no row names one).
    pub model: String,
    /// The number of calls, failed ones included.
    pub calls: u64,
    /// The tokens of the calls' responses (failed calls have none).
    pub usage: TokenUsage,
}

impl ModelTokens {
    /// `calls` calls to `model` with `usage` tokens in all.
    pub fn new(model: impl Into<String>, calls: u64, usage: TokenUsage) -> Self {
        Self {
            model: model.into(),
            calls,
            usage,
        }
    }
}

/// The calls of one version: counts, failures, p95 latency and tokens per model.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct VersionCalls {
    /// The prompt version.
    pub version: u32,
    /// The number of calls.
    pub calls: u64,
    /// The calls with an `LLM_ERROR` row.
    pub errors: u64,
    /// The approximate 95th percentile of the responses' `latency_ms.total_ms`, `None` without
    /// responses.
    pub p95_ms: Option<f64>,
    /// The calls and tokens per model, by model name.
    pub models: Vec<ModelTokens>,
}

impl VersionCalls {
    /// `calls` calls of `version`, `errors` of them failed, with p95 latency `p95_ms` and
    /// `models`.
    pub fn new(
        version: u32,
        calls: u64,
        errors: u64,
        p95_ms: Option<f64>,
        models: Vec<ModelTokens>,
    ) -> Self {
        Self {
            version,
            calls,
            errors,
            p95_ms,
            models,
        }
    }
}

/// The latest scores of one name on the calls of one version, from one source and of one data
/// type.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct VersionFeedback {
    /// The prompt version.
    pub version: u32,
    /// Who recorded the scores: `app` or `console`.
    pub source: String,
    /// The scores' data type: `bool`, `number` or `category`.
    pub data_type: String,
    /// The number of scores (the latest per invocation).
    pub count: u64,
    /// The scores whose value is `true`.
    pub positive: u64,
    /// The mean of the number values, `None` without any.
    pub mean: Option<f64>,
    /// The sample standard deviation of the number values, `None` with fewer than two.
    pub stddev: Option<f64>,
}

impl VersionFeedback {
    /// `count` scores of `version` from `source` with data type `data_type`, `positive` of them
    /// `true`, number values with `mean` and sample standard deviation `stddev`.
    pub fn new(
        version: u32,
        source: impl Into<String>,
        data_type: impl Into<String>,
        count: u64,
        positive: u64,
        mean: Option<f64>,
        stddev: Option<f64>,
    ) -> Self {
        Self {
            version,
            source: source.into(),
            data_type: data_type.into(),
            count,
            positive,
            mean,
            stddev,
        }
    }
}

/// Where a guard reads its data: the calls, feedback and runs of the two sides of a rollout.
#[async_trait::async_trait]
pub trait GuardSource: Send + Sync {
    /// The guard's calls per version (versions without calls are left out), by version.
    async fn version_calls(&self, q: &GuardQuery) -> Result<Vec<VersionCalls>, MonitorError>;

    /// The latest score named `name` per (invocation, source) of the invocations with a guard
    /// call, restricted to `source` ([`FeedbackSource::Any`] keeps both sources), one entry per
    /// (version, source, data type), by version.
    async fn version_feedback(
        &self,
        q: &GuardQuery,
        name: &str,
        source: FeedbackSource,
    ) -> Result<Vec<VersionFeedback>, MonitorError>;

    /// Up to `limit` runs with a guard call at `version`, newest call first, rebuilt as the
    /// `monitor` command rebuilds them.
    async fn version_runs(
        &self,
        q: &GuardQuery,
        version: u32,
        limit: usize,
    ) -> Result<Vec<ProductionRun>, MonitorError>;
}
