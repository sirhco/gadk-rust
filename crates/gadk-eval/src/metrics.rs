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

//! Per-run metrics: how long each run took and the tokens it used per model.

use std::collections::BTreeMap;

use gadk_genai::GenerateContentResponseUsageMetadata;
use serde::{Deserialize, Serialize};

use crate::report::EvalReport;

/// The model key of usage from a response without a model version.
pub(crate) const UNKNOWN_MODEL: &str = "unknown";

/// Token counts of one or more model calls.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenCounts {
    /// Prompt tokens, tool-use prompt tokens included.
    pub input: u64,
    /// Prompt tokens served from the context cache.
    pub cached: u64,
    /// Response tokens, thought tokens excluded.
    pub output: u64,
    /// Thought tokens.
    pub reasoning: u64,
}

impl TokenCounts {
    /// Counts with the given values.
    pub fn new(input: u64, cached: u64, output: u64, reasoning: u64) -> Self {
        TokenCounts {
            input,
            cached,
            output,
            reasoning,
        }
    }

    /// The counts of one response's usage metadata: `input` is the prompt plus tool-use prompt
    /// tokens; negative values count as 0.
    pub(crate) fn from_usage(u: &GenerateContentResponseUsageMetadata) -> Self {
        let n = |v: i32| u64::try_from(v).unwrap_or(0);
        TokenCounts {
            input: n(u.prompt_token_count).saturating_add(n(u.tool_use_prompt_token_count)),
            cached: n(u.cached_content_token_count),
            output: n(u.candidates_token_count),
            reasoning: n(u.thoughts_token_count),
        }
    }
}

impl std::ops::AddAssign for TokenCounts {
    /// Adds field by field, saturating at `u64::MAX`.
    fn add_assign(&mut self, rhs: TokenCounts) {
        self.input = self.input.saturating_add(rhs.input);
        self.cached = self.cached.saturating_add(rhs.cached);
        self.output = self.output.saturating_add(rhs.output);
        self.reasoning = self.reasoning.saturating_add(rhs.reasoning);
    }
}

/// What one run cost: its duration and its token counts per model.
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunMetrics {
    /// Milliseconds from just before the run's runner and session were created until the run
    /// completed, failed or timed out (0 for a variant whose build failed).
    pub duration_ms: u64,
    /// Token counts per model version (`"unknown"` when a response named none), summed over the
    /// run's model calls.
    pub models: BTreeMap<String, TokenCounts>,
}

impl RunMetrics {
    /// Metrics with the given values.
    pub fn new(duration_ms: u64, models: BTreeMap<String, TokenCounts>) -> Self {
        RunMetrics {
            duration_ms,
            models,
        }
    }

    /// The token counts summed over every model.
    pub fn total(&self) -> TokenCounts {
        let mut t = TokenCounts::default();
        for c in self.models.values() {
            t += *c;
        }
        t
    }
}

/// The result of [`crate::Evaluator::run_measured`]: the report and every run's metrics.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeasuredReport {
    /// The same report [`crate::Evaluator::run`] returns.
    pub report: EvalReport,
    /// `metrics[v][i]` belongs to `report.variants[v].runs[i]`.
    pub metrics: Vec<Vec<RunMetrics>>,
}

impl MeasuredReport {
    /// A measured report from its parts.
    pub fn new(report: EvalReport, metrics: Vec<Vec<RunMetrics>>) -> Self {
        MeasuredReport { report, metrics }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_maps_fields_and_clamps_negatives() {
        let u = GenerateContentResponseUsageMetadata {
            prompt_token_count: 10,
            tool_use_prompt_token_count: 2,
            cached_content_token_count: 3,
            candidates_token_count: -4,
            thoughts_token_count: 5,
            total_token_count: 99,
            ..Default::default()
        };
        assert_eq!(TokenCounts::from_usage(&u), TokenCounts::new(12, 3, 0, 5));
        let neg = GenerateContentResponseUsageMetadata {
            prompt_token_count: -1,
            tool_use_prompt_token_count: 7,
            ..Default::default()
        };
        assert_eq!(TokenCounts::from_usage(&neg), TokenCounts::new(7, 0, 0, 0));
    }

    #[test]
    fn add_assign_saturates_and_total_sums_models() {
        let mut t = TokenCounts::new(u64::MAX - 1, 1, 2, 3);
        t += TokenCounts::new(5, 1, 1, 1);
        assert_eq!(t, TokenCounts::new(u64::MAX, 2, 3, 4));
        let m = RunMetrics::new(
            7,
            BTreeMap::from([
                ("a".to_owned(), TokenCounts::new(1, 2, 3, 4)),
                ("b".to_owned(), TokenCounts::new(10, 20, 30, 40)),
            ]),
        );
        assert_eq!(m.total(), TokenCounts::new(11, 22, 33, 44));
        assert_eq!(RunMetrics::default().total(), TokenCounts::default());
    }

    #[test]
    fn metrics_round_trip_through_json() {
        let m = RunMetrics::new(
            1234,
            BTreeMap::from([("gemini-3.8-flash".to_owned(), TokenCounts::new(1, 0, 2, 3))]),
        );
        let json = serde_json::to_string(&m).unwrap();
        assert_eq!(
            json,
            r#"{"duration_ms":1234,"models":{"gemini-3.8-flash":{"input":1,"cached":0,"output":2,"reasoning":3}}}"#
        );
        assert_eq!(serde_json::from_str::<RunMetrics>(&json).unwrap(), m);
    }
}
