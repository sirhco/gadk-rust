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

//! Token counts and their cost.
//!
//! A model call's tokens are four counts ([`Usage`]): input (prompt plus tool-use prompt, cached
//! tokens included), cached input, output (without reasoning) and reasoning. Prices are USD per
//! 1,000,000 tokens from a [`gadk_eval_monitor::PriceTable`]; [`cost_of`] prices a usage and
//! [`CostSum`] adds up call costs, remembering calls without a price. The `LABEL_*` constants
//! are the span labels that carry a model call's model and tokens.
//!
//! ```
//! use std::sync::Arc;
//!
//! use chrono::{TimeZone, Utc};
//! use gadk_analytics::cost::{CostSum, cost_of};
//! use gadk_analytics::testutil::{FakeHttp, reply};
//! use gadk_analytics::{AnalyticsSource, BigQueryTable};
//! use gadk_bigquery::{BigQueryClient, NoAuth};
//!
//! # tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
//! // One scripted `jobs.query` reply: a single breakdown row.
//! let http = FakeHttp::new(vec![reply(
//!     &[
//!         ("model", "STRING"), ("agent", "STRING"), ("prompt", "STRING"), ("calls", "INT64"),
//!         ("input", "INT64"), ("cached", "INT64"), ("output", "INT64"), ("reasoning", "INT64"),
//!     ],
//!     vec![vec![
//!         "gemini-3.8-flash".into(), "triage".into(), "triage@2".into(), "3".into(),
//!         "3000".into(), "1000".into(), "600".into(), "0".into(),
//!     ]],
//!     false,
//! )]);
//! let source = AnalyticsSource::new(
//!     BigQueryClient::with_client(http.clone(), Arc::new(NoAuth)),
//!     "billing-proj",
//!     BigQueryTable::new("data-proj", "agent_analytics", "agent_events"),
//!     1_073_741_824,
//! );
//! let end = Utc.with_ymd_and_hms(2026, 11, 20, 12, 0, 0).unwrap();
//! let breakdown = source.breakdown(end - chrono::TimeDelta::hours(24), end).await?;
//! let prices = gadk_eval_monitor::PriceTable::from_yaml(
//!     "- model: gemini-3.8-flash\n  input: 0.30\n  output: 2.50\n",
//! )
//! .unwrap();
//! let mut total = CostSum::default();
//! for cell in &breakdown.cells {
//!     total.add(cost_of(Some(&prices), &cell.calls.model, &cell.calls.usage));
//! }
//! assert_eq!(total.text(), "$0.002400");
//! # Ok::<(), gadk_analytics::SourceError>(())
//! # }).unwrap();
//! ```

use std::collections::BTreeMap;
use std::ops::AddAssign;

use gadk_eval_monitor::TokenUsage;
use serde::Serialize;

/// The price table [`cost_of`] prices with, re-exported from `gadk-eval-monitor` so it can be
/// named from this crate: USD per 1,000,000 tokens per model, read from a `--prices` YAML file.
pub use gadk_eval_monitor::PriceTable;

/// One model's prices in a [`PriceTable`], re-exported from `gadk-eval-monitor`.
pub use gadk_eval_monitor::ModelPrice;

/// The span label naming a span's operation.
pub const LABEL_OPERATION: &str = "gen_ai.operation.name";
/// The operation of a model-call span.
pub const OP_GENERATE_CONTENT: &str = "generate_content";
/// The span label naming the requested model.
pub const LABEL_MODEL: &str = "gen_ai.request.model";
/// Input tokens: prompt plus tool-use prompt.
pub const LABEL_INPUT: &str = "gen_ai.usage.input_tokens";
/// Output tokens: candidates plus reasoning.
pub const LABEL_OUTPUT: &str = "gen_ai.usage.output_tokens";
/// Cached input tokens.
pub const LABEL_CACHED: &str = "gen_ai.usage.cache_read.input_tokens";
/// Reasoning (thinking) tokens.
pub const LABEL_REASONING: &str = "gen_ai.usage.reasoning.output_tokens";
/// The model shown for a call whose model is not known.
pub const UNKNOWN_MODEL: &str = "(unknown)";

/// The tokens of one model call (or a sum of calls).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct Usage {
    /// Input tokens (prompt plus tool-use prompt), cached ones included.
    pub input: u64,
    /// Cached input tokens.
    pub cached: u64,
    /// Output tokens, without reasoning.
    pub output: u64,
    /// Reasoning (thinking) tokens.
    pub reasoning: u64,
}

impl Usage {
    /// A usage of `input` input tokens (cached ones included), `cached` cached input tokens,
    /// `output` output tokens (without reasoning) and `reasoning` reasoning tokens.
    pub fn new(input: u64, cached: u64, output: u64, reasoning: u64) -> Self {
        Self {
            input,
            cached,
            output,
            reasoning,
        }
    }

    /// The usage of a model-call span from its labels: input = `gen_ai.usage.input_tokens`,
    /// cached = `gen_ai.usage.cache_read.input_tokens`, reasoning =
    /// `gen_ai.usage.reasoning.output_tokens`, output = `gen_ai.usage.output_tokens` minus
    /// reasoning (floored at 0). A missing label, or one that is not a count, is 0.
    pub fn from_labels(labels: &BTreeMap<String, String>) -> Self {
        let count = |key: &str| {
            labels
                .get(key)
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0)
        };
        let reasoning = count(LABEL_REASONING);
        Self {
            input: count(LABEL_INPUT),
            cached: count(LABEL_CACHED),
            output: count(LABEL_OUTPUT).saturating_sub(reasoning),
            reasoning,
        }
    }

    /// The cost in USD: `(input − cached) × input + cached × cached_input + output × output +
    /// reasoning × reasoning`, prices per 1,000,000 tokens (`input − cached` floors at 0).
    pub(crate) fn cost(&self, p: &ModelPrice) -> f64 {
        TokenUsage::from(*self).cost(p)
    }
}

impl From<Usage> for TokenUsage {
    fn from(u: Usage) -> Self {
        TokenUsage::new(u.input, u.cached, u.output, u.reasoning)
    }
}

impl AddAssign for Usage {
    fn add_assign(&mut self, other: Self) {
        self.input = self.input.saturating_add(other.input);
        self.cached = self.cached.saturating_add(other.cached);
        self.output = self.output.saturating_add(other.output);
        self.reasoning = self.reasoning.saturating_add(other.reasoning);
    }
}

/// The cost of `usage` on `model`; `None` without a price file or a price for the model.
pub fn cost_of(prices: Option<&PriceTable>, model: &str, usage: &Usage) -> Option<f64> {
    prices?.price_for(model).map(|p| usage.cost(p))
}

/// A sum of call costs that remembers whether some calls had no price.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
#[non_exhaustive]
pub struct CostSum {
    /// The sum of the priced calls' costs, in USD.
    pub usd: f64,
    /// Whether any call had a price.
    pub priced: bool,
    /// Whether any call had no price.
    pub unpriced: bool,
}

impl CostSum {
    /// Adds one call's cost (`None`: the call had no price).
    pub fn add(&mut self, cost: Option<f64>) {
        match cost {
            Some(c) => {
                self.usd += c;
                self.priced = true;
            }
            None => self.unpriced = true,
        }
    }

    /// Whether the sum leaves out unpriced calls while covering priced ones.
    pub fn partial(&self) -> bool {
        self.priced && self.unpriced
    }

    /// `$x` (six decimals), `$x*` when it leaves out unpriced calls, `—` when no call had a price.
    pub fn text(&self) -> String {
        if !self.priced {
            "—".to_owned()
        } else if self.unpriced {
            format!("${:.6}*", self.usd)
        } else {
            format!("${:.6}", self.usd)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn usage_from_labels_takes_reasoning_out_of_output() {
        let u = Usage::from_labels(&labels(&[
            (LABEL_INPUT, "120"),
            (LABEL_OUTPUT, "50"),
            (LABEL_CACHED, "20"),
            (LABEL_REASONING, "30"),
        ]));
        assert_eq!(
            u,
            Usage {
                input: 120,
                cached: 20,
                output: 20,
                reasoning: 30
            }
        );
        let floored = Usage::from_labels(&labels(&[(LABEL_OUTPUT, "10"), (LABEL_REASONING, "30")]));
        assert_eq!(floored.output, 0, "output minus reasoning floors at 0");
        assert_eq!(Usage::from_labels(&BTreeMap::new()), Usage::default());
        assert_eq!(
            Usage::from_labels(&labels(&[(LABEL_INPUT, "x")])),
            Usage::default(),
            "a label that is not a count is 0"
        );
    }

    #[test]
    fn cost_prices_each_count_per_million_tokens() {
        let u = Usage {
            input: 1_000_000,
            cached: 200_000,
            output: 500_000,
            reasoning: 100_000,
        };
        let p = ModelPrice::new(0.30, 2.50).with_cached_input(0.075);
        assert!((u.cost(&p) - 1.755).abs() < 1e-9, "{}", u.cost(&p));
        // Defaults (cached = input, reasoning = output) and the uncached floor.
        let u = Usage {
            input: 100,
            cached: 300,
            output: 10,
            reasoning: 5,
        };
        assert!((u.cost(&ModelPrice::new(1.0, 4.0)) - 0.000_36).abs() < 1e-12);
    }

    #[test]
    fn cost_of_prices_a_usage_with_the_model_price() {
        let prices = PriceTable::from_yaml(
            "- model: gemini-3.8-flash\n  input: 0.30\n  output: 2.50\n  cached_input: 0.075\n",
        )
        .unwrap();
        let u = Usage::new(1200, 200, 300, 50);
        let cost = cost_of(Some(&prices), "gemini-3.8-flash", &u).unwrap();
        assert!((cost - 0.001_19).abs() < 1e-12, "{cost}");
        assert_eq!(cost_of(Some(&prices), "claude", &u), None);
        assert_eq!(cost_of(None, "gemini-3.8-flash", &u), None);
    }

    #[test]
    fn cost_sums_mark_unpriced_calls() {
        let mut s = CostSum::default();
        assert_eq!(s.text(), "—");
        s.add(None);
        assert_eq!(s.text(), "—");
        assert!(!s.partial());
        s.add(Some(0.5));
        assert_eq!(s.text(), "$0.500000*");
        assert!(s.partial());
        let mut all = CostSum::default();
        all.add(Some(0.25));
        all.add(Some(0.25));
        assert_eq!(all.text(), "$0.500000");
        assert!(!all.partial());
    }
}
