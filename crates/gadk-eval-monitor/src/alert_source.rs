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

//! The data an alert spec checks: the model calls in a window, grouped by model, agent and
//! prompt.
//!
//! An [`AlertSource`] answers one question per spec, [`AlertSource::usage`]: the `LLM_REQUEST`
//! rows logged in `[from, to)`, grouped by model × agent × prompt, each group with its calls,
//! failed calls and tokens. [`BigQueryGuardSource`](crate::BigQueryGuardSource) reads them from
//! the agent-analytics table; tests use a fake.
//!
//! ```
//! use chrono::{TimeDelta, Utc};
//! use gadk_eval_monitor::{AlertQuery, AlertSource, MonitorError, TokenUsage, UsageGroup};
//!
//! /// A source with one fixed group.
//! struct Fixed;
//!
//! #[async_trait::async_trait]
//! impl AlertSource for Fixed {
//!     async fn usage(&self, _q: &AlertQuery) -> Result<Vec<UsageGroup>, MonitorError> {
//!         Ok(vec![UsageGroup::new(
//!             "gemini-3.8-flash",
//!             "support",
//!             Some("support/answer".to_owned()),
//!             120,
//!             3,
//!             TokenUsage::new(90_000, 10_000, 12_000, 0),
//!         )])
//!     }
//! }
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), MonitorError> {
//! let now = Utc::now();
//! let groups = Fixed
//!     .usage(&AlertQuery::new(now - TimeDelta::hours(24), now))
//!     .await?;
//! assert_eq!(groups[0].calls, 120);
//! # Ok(())
//! # }
//! ```

use chrono::{DateTime, Utc};

use crate::error::MonitorError;
use crate::prices::TokenUsage;

/// The calls an alert spec checks: the `LLM_REQUEST` rows logged in `[from, to)`.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlertQuery {
    /// The window start (inclusive).
    pub from: DateTime<Utc>,
    /// The window end (exclusive).
    pub to: DateTime<Utc>,
}

impl AlertQuery {
    /// The calls logged in `[from, to)`.
    pub fn new(from: DateTime<Utc>, to: DateTime<Utc>) -> Self {
        Self { from, to }
    }
}

/// The calls of one model × agent × prompt group: counts, failures and summed tokens.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageGroup {
    /// The model: the request's, else the response's model version, else `(unknown)`.
    pub model: String,
    /// The agent of the request row (`""` when it has none).
    pub agent: String,
    /// The request's prompt name; `None` when it has none (never `Some("")`).
    pub prompt: Option<String>,
    /// The number of calls, failed ones included.
    pub calls: u64,
    /// The calls with an `LLM_ERROR` row.
    pub errors: u64,
    /// The summed tokens of the calls' latest responses (calls without one count 0).
    pub usage: TokenUsage,
}

impl UsageGroup {
    /// `calls` calls to `model` from `agent` with prompt `prompt`, `errors` of them failed,
    /// with `usage` tokens in all.
    pub fn new(
        model: impl Into<String>,
        agent: impl Into<String>,
        prompt: Option<String>,
        calls: u64,
        errors: u64,
        usage: TokenUsage,
    ) -> Self {
        Self {
            model: model.into(),
            agent: agent.into(),
            prompt,
            calls,
            errors,
            usage,
        }
    }
}

/// Where alerts read their data: the model calls in a window, grouped.
#[async_trait::async_trait]
pub trait AlertSource: Send + Sync {
    /// The calls in `[q.from, q.to)` grouped by model × agent × prompt (groups without calls
    /// are left out), sorted by model, agent and prompt.
    async fn usage(&self, q: &AlertQuery) -> Result<Vec<UsageGroup>, MonitorError>;
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;

    use super::*;
    use crate::testutil::at;

    #[test]
    fn constructors_set_every_field() {
        let q = AlertQuery::new(at(100), at(200));
        assert_eq!((q.from, q.to), (at(100), at(200)));
        let g = UsageGroup::new(
            "m-flash",
            "support",
            Some("support/answer".into()),
            10,
            2,
            TokenUsage::new(5, 1, 3, 0),
        );
        assert_eq!(g.model, "m-flash");
        assert_eq!(g.agent, "support");
        assert_eq!(g.prompt.as_deref(), Some("support/answer"));
        assert_eq!((g.calls, g.errors), (10, 2));
        assert_eq!(g.usage, TokenUsage::new(5, 1, 3, 0));
    }

    /// A source returning the window length as the calls of one group.
    struct Window;

    #[async_trait::async_trait]
    impl AlertSource for Window {
        async fn usage(&self, q: &AlertQuery) -> Result<Vec<UsageGroup>, MonitorError> {
            let secs = u64::try_from((q.to - q.from).num_seconds()).unwrap();
            Ok(vec![UsageGroup::new(
                "m",
                "",
                None,
                secs,
                0,
                TokenUsage::default(),
            )])
        }
    }

    #[tokio::test]
    async fn the_trait_is_object_safe() {
        let source: std::sync::Arc<dyn AlertSource> = std::sync::Arc::new(Window);
        let q = AlertQuery::new(at(0), at(0) + TimeDelta::hours(1));
        assert_eq!(source.usage(&q).await.unwrap()[0].calls, 3600);
    }
}
