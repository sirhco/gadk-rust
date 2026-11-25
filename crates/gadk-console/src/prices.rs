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

//! Model prices from a price file and the cost of model calls.
//!
//! Prices are USD per 1,000,000 tokens and come only from the price file. A model call's tokens
//! are four counts ([`Usage`]): input (prompt plus tool-use prompt, cached tokens included),
//! cached input, output (without reasoning) and reasoning. The price-file rules, [`ModelPrice`]
//! and the cost formula are gadk-eval-monitor's ([`gadk_eval_monitor::PriceTable`],
//! [`gadk_eval_monitor::TokenUsage`]), and [`Usage`], [`CostSum`] and the cost of a call are
//! gadk-analytics' ([`gadk_analytics::cost`]); this module wraps them with the console's error
//! type.

use std::path::Path;

use gadk_analytics::cost::{CostSum, Usage};
use gadk_eval::{RunMetrics, TokenCounts};
pub use gadk_eval_monitor::ModelPrice;

use crate::ConsoleError;

/// Model prices from a price file: a YAML list of
/// `{model, input, output, cached_input?, reasoning?}`, prices in USD per 1,000,000 tokens.
/// `model` matches a model name exactly, or as a prefix when it ends in `*`; an exact match
/// beats a prefix, and a longer prefix beats a shorter one.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PriceTable {
    inner: gadk_eval_monitor::PriceTable,
}

impl PriceTable {
    /// Reads and parses the price file at `path`.
    ///
    /// # Errors
    ///
    /// [`ConsoleError::Prices`] when the file cannot be read, is not a list of price entries, or
    /// has an empty or repeated model or a negative or non-finite price.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConsoleError> {
        let path = path.as_ref();
        let err = |detail: String| ConsoleError::Prices {
            path: path.to_path_buf(),
            detail,
        };
        let text = std::fs::read_to_string(path).map_err(|e| err(e.to_string()))?;
        Self::from_yaml(&text).map_err(err)
    }

    /// Parses price-file text; the error is the detail [`load`](Self::load) reports.
    pub(crate) fn from_yaml(text: &str) -> Result<Self, String> {
        gadk_eval_monitor::PriceTable::from_yaml(text).map(|inner| Self { inner })
    }

    /// The prices of `model`: its exact entry, else the longest matching prefix entry.
    pub fn price_for(&self, model: &str) -> Option<&ModelPrice> {
        self.inner.price_for(model)
    }

    /// The gadk-eval-monitor price table this wraps.
    pub(crate) fn inner(&self) -> &gadk_eval_monitor::PriceTable {
        &self.inner
    }
}

/// The cost of `usage` on `model`; `None` without a price file or a price for the model
/// ([`gadk_analytics::cost::cost_of`] over the wrapped table).
pub(crate) fn cost_of(prices: Option<&PriceTable>, model: &str, usage: &Usage) -> Option<f64> {
    gadk_analytics::cost::cost_of(prices.map(PriceTable::inner), model, usage)
}

/// The usage of an eval run's token counts for one model: the four counts copied as they are
/// (eval runs count output without reasoning already).
pub(crate) fn usage_of(t: &TokenCounts) -> Usage {
    Usage::new(t.input, t.cached, t.output, t.reasoning)
}

/// The cost of eval runs: every model of every run in `runs`, priced with [`cost_of`].
pub(crate) fn runs_cost(prices: Option<&PriceTable>, runs: &[RunMetrics]) -> CostSum {
    let mut sum = CostSum::default();
    for (model, t) in runs.iter().flat_map(|r| r.models.iter()) {
        sum.add(cost_of(prices, model, &usage_of(t)));
    }
    sum
}

/// A cost as `$0.001190` (six decimals), or `—` when there is none.
pub(crate) fn cost_text(cost: Option<f64>) -> String {
    cost.map_or_else(|| "—".to_owned(), |c| format!("${c:.6}"))
}

/// `n` with a comma between each group of three digits (`1,000`).
pub(crate) fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRICES: &str = "\
- model: gemini-3.8-flash
  input: 0.30
  output: 2.50
  cached_input: 0.075
- model: gemini-3.8-flash*
  input: 9
  output: 9
- model: gemini-3.8-*
  input: 1
  output: 4
- model: gemini-*
  input: 2
  output: 8
  reasoning: 10
";

    #[test]
    fn exact_beats_prefix_and_longer_prefix_beats_shorter() {
        let t = PriceTable::from_yaml(PRICES).unwrap();
        assert_eq!(
            t.price_for("gemini-3.8-flash"),
            Some(&ModelPrice::new(0.30, 2.50).with_cached_input(0.075))
        );
        assert_eq!(
            t.price_for("gemini-3.8-flash-lite"),
            Some(&ModelPrice::new(9.0, 9.0))
        );
        assert_eq!(
            t.price_for("gemini-3.8-pro"),
            Some(&ModelPrice::new(1.0, 4.0))
        );
        assert_eq!(
            t.price_for("gemini-4-hydrogen"),
            Some(&ModelPrice::new(2.0, 8.0).with_reasoning(10.0))
        );
        assert_eq!(t.price_for("claude"), None);
    }

    #[test]
    fn defaults_follow_input_and_output() {
        let p = ModelPrice::new(1.0, 4.0);
        assert_eq!((p.cached_input, p.reasoning), (1.0, 4.0));
    }

    #[test]
    fn bad_price_files_are_rejected() {
        for (yaml, want) in [
            (
                "- model: m\n  input: -1\n  output: 1\n",
                "model m: prices must be finite and not negative",
            ),
            (
                "- model: m\n  input: 1\n  output: 1\n  reasoning: -0.5\n",
                "model m: prices must be finite and not negative",
            ),
            (
                "- model: ''\n  input: 1\n  output: 1\n",
                "entry 1: empty model",
            ),
            (
                "- model: m\n  input: 1\n  output: 1\n- model: m\n  input: 2\n  output: 2\n",
                "model m: listed twice",
            ),
            (
                "- model: m*\n  input: 1\n  output: 1\n- model: m*\n  input: 2\n  output: 2\n",
                "model m*: listed twice",
            ),
        ] {
            assert_eq!(PriceTable::from_yaml(yaml).unwrap_err(), want, "{yaml}");
        }
        for yaml in [
            "model: m\n",
            "- model: m\n  input: 1\n",
            "- model: m\n  input: 1\n  output: 1\n  cached-input: 1\n",
            "- model: m\n  input: x\n  output: 1\n",
        ] {
            assert!(PriceTable::from_yaml(yaml).is_err(), "{yaml}");
        }
    }

    #[test]
    fn load_reads_a_file_and_names_it_in_errors() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("prices.yaml");
        std::fs::write(&good, PRICES).unwrap();
        let t = PriceTable::load(&good).unwrap();
        assert!(t.price_for("gemini-3.8-flash").is_some());

        let bad = dir.path().join("bad.yaml");
        std::fs::write(&bad, "- model: m\n  input: -1\n  output: 1\n").unwrap();
        let err = PriceTable::load(&bad).unwrap_err();
        assert!(matches!(err, ConsoleError::Prices { .. }), "{err}");
        assert_eq!(
            err.to_string(),
            format!(
                "price file {}: model m: prices must be finite and not negative",
                bad.display()
            )
        );

        let missing = dir.path().join("missing.yaml");
        let err = PriceTable::load(&missing).unwrap_err();
        assert!(
            err.to_string()
                .starts_with(&format!("price file {}: ", missing.display())),
            "{err}"
        );
    }
    #[test]
    fn cost_of_and_texts() {
        let t = PriceTable::from_yaml(PRICES).unwrap();
        let u = Usage::new(1200, 200, 300, 50);
        assert_eq!(
            cost_text(cost_of(Some(&t), "gemini-3.8-flash", &u)),
            "$0.001190"
        );
        assert_eq!(cost_of(Some(&t), "claude", &u), None);
        assert_eq!(cost_of(None, "gemini-3.8-flash", &u), None);
        assert_eq!(cost_text(None), "—");
        for (n, want) in [
            (0, "0"),
            (999, "999"),
            (1000, "1,000"),
            (1_234_567, "1,234,567"),
        ] {
            assert_eq!(thousands(n), want);
        }
    }

    #[test]
    fn usage_of_copies_the_counts() {
        assert_eq!(
            usage_of(&TokenCounts::new(1200, 200, 300, 50)),
            Usage::new(1200, 200, 300, 50)
        );
    }

    #[test]
    fn runs_cost_prices_every_model_of_every_run() {
        let t = PriceTable::from_yaml(PRICES).unwrap();
        let counts = TokenCounts::new(1200, 200, 300, 50);
        let run = |models: &[&str]| {
            RunMetrics::new(
                10,
                models.iter().map(|m| ((*m).to_owned(), counts)).collect(),
            )
        };
        // $0.001190 per priced model call (see `cost_of_and_texts`).
        let all = runs_cost(
            Some(&t),
            &[run(&["gemini-3.8-flash"]), run(&["gemini-3.8-flash"])],
        );
        assert_eq!(all.text(), "$0.002380");
        let partial = runs_cost(Some(&t), &[run(&["gemini-3.8-flash", "claude"])]);
        assert_eq!(partial.text(), "$0.001190*");
        assert!(partial.partial());
        assert_eq!(runs_cost(Some(&t), &[run(&["claude"])]).text(), "—");
        assert_eq!(runs_cost(None, &[run(&["gemini-3.8-flash"])]).text(), "—");
        assert_eq!(runs_cost(Some(&t), &[]).text(), "—");
    }
}
