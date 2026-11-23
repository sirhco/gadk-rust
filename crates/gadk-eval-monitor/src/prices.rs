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
//! are four counts ([`TokenUsage`]): input (prompt plus tool-use prompt, cached tokens included),
//! cached input, output (without reasoning) and reasoning.

use std::collections::BTreeMap;
use std::ops::AddAssign;
use std::path::Path;

use serde::Deserialize;

use crate::MonitorError;

/// One entry of the price file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    model: String,
    input: f64,
    output: f64,
    #[serde(default)]
    cached_input: Option<f64>,
    #[serde(default)]
    reasoning: Option<f64>,
}

/// The prices of one model, in USD per 1,000,000 tokens.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ModelPrice {
    /// Input (prompt) tokens that were not cached.
    pub input: f64,
    /// Output tokens.
    pub output: f64,
    /// Cached input tokens.
    pub cached_input: f64,
    /// Reasoning (thinking) tokens.
    pub reasoning: f64,
}

impl ModelPrice {
    /// `input` and `output` prices; cached input costs `input` and reasoning costs `output`.
    pub fn new(input: f64, output: f64) -> Self {
        Self {
            input,
            output,
            cached_input: input,
            reasoning: output,
        }
    }

    /// Sets the cached-input price.
    pub fn with_cached_input(mut self, price: f64) -> Self {
        self.cached_input = price;
        self
    }

    /// Sets the reasoning price.
    pub fn with_reasoning(mut self, price: f64) -> Self {
        self.reasoning = price;
        self
    }
}

/// Model prices from a price file: a YAML list of
/// `{model, input, output, cached_input?, reasoning?}`, prices in USD per 1,000,000 tokens.
/// `model` matches a model name exactly, or as a prefix when it ends in `*`; an exact match
/// beats a prefix, and a longer prefix beats a shorter one.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PriceTable {
    exact: BTreeMap<String, ModelPrice>,
    /// Prefix entries (without the `*`), longest first.
    prefixes: Vec<(String, ModelPrice)>,
}

impl PriceTable {
    /// Reads and parses the price file at `path`.
    ///
    /// # Errors
    ///
    /// [`MonitorError::Spec`] with `price file <path>: <detail>` when the file cannot be read,
    /// is not a list of price entries, or has an empty or repeated model or a negative or
    /// non-finite price.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, MonitorError> {
        let path = path.as_ref();
        let err =
            |detail: String| MonitorError::Spec(format!("price file {}: {detail}", path.display()));
        let text = std::fs::read_to_string(path).map_err(|e| err(e.to_string()))?;
        Self::from_yaml(&text).map_err(err)
    }

    /// Parses price-file text.
    ///
    /// # Errors
    ///
    /// The detail [`load`](Self::load) reports: the YAML error, `entry <n>: empty model`,
    /// `model <m>: prices must be finite and not negative` or `model <m>: listed twice`.
    pub fn from_yaml(text: &str) -> Result<Self, String> {
        let entries: Vec<Entry> = serde_saphyr::from_str(text).map_err(|e| e.to_string())?;
        let mut exact = BTreeMap::new();
        let mut prefixes: Vec<(String, ModelPrice)> = Vec::new();
        for (i, e) in entries.into_iter().enumerate() {
            if e.model.is_empty() {
                return Err(format!("entry {}: empty model", i + 1));
            }
            let prices = [Some(e.input), Some(e.output), e.cached_input, e.reasoning];
            if prices.iter().flatten().any(|p| !p.is_finite() || *p < 0.0) {
                return Err(format!(
                    "model {}: prices must be finite and not negative",
                    e.model
                ));
            }
            let mut price = ModelPrice::new(e.input, e.output);
            if let Some(p) = e.cached_input {
                price = price.with_cached_input(p);
            }
            if let Some(p) = e.reasoning {
                price = price.with_reasoning(p);
            }
            let repeated = match e.model.strip_suffix('*') {
                Some(prefix) => {
                    let seen = prefixes.iter().any(|(p, _)| p == prefix);
                    prefixes.push((prefix.to_owned(), price));
                    seen
                }
                None => exact.insert(e.model.clone(), price).is_some(),
            };
            if repeated {
                return Err(format!("model {}: listed twice", e.model));
            }
        }
        prefixes.sort_by_key(|(p, _)| std::cmp::Reverse(p.len()));
        Ok(Self { exact, prefixes })
    }

    /// The prices of `model`: its exact entry, else the longest matching prefix entry.
    pub fn price_for(&self, model: &str) -> Option<&ModelPrice> {
        self.exact.get(model).or_else(|| {
            self.prefixes
                .iter()
                .find(|(prefix, _)| model.starts_with(prefix.as_str()))
                .map(|(_, price)| price)
        })
    }
}

/// The tokens of one model call (or a sum of calls).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct TokenUsage {
    /// Input tokens (prompt plus tool-use prompt), cached ones included.
    pub input: u64,
    /// Cached input tokens.
    pub cached: u64,
    /// Output tokens, without reasoning.
    pub output: u64,
    /// Reasoning (thinking) tokens.
    pub reasoning: u64,
}

impl TokenUsage {
    /// Usage of `input` tokens (cached ones included), `cached` input tokens, `output` tokens
    /// (without reasoning) and `reasoning` tokens.
    pub fn new(input: u64, cached: u64, output: u64, reasoning: u64) -> Self {
        Self {
            input,
            cached,
            output,
            reasoning,
        }
    }

    /// The cost in USD: `(input − cached) × input + cached × cached_input + output × output +
    /// reasoning × reasoning`, prices per 1,000,000 tokens (`input − cached` floors at 0).
    pub fn cost(&self, p: &ModelPrice) -> f64 {
        let uncached = self.input.saturating_sub(self.cached) as f64;
        (uncached * p.input
            + self.cached as f64 * p.cached_input
            + self.output as f64 * p.output
            + self.reasoning as f64 * p.reasoning)
            / 1_000_000.0
    }
}

/// Adds each count, saturating at `u64::MAX`.
impl AddAssign for TokenUsage {
    fn add_assign(&mut self, other: Self) {
        self.input = self.input.saturating_add(other.input);
        self.cached = self.cached.saturating_add(other.cached);
        self.output = self.output.saturating_add(other.output);
        self.reasoning = self.reasoning.saturating_add(other.reasoning);
    }
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
    fn prices_exact_beats_prefix_and_longer_prefix_beats_shorter() {
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
    fn prices_defaults_follow_input_and_output() {
        let p = ModelPrice::new(1.0, 4.0);
        assert_eq!((p.cached_input, p.reasoning), (1.0, 4.0));
    }

    #[test]
    fn prices_bad_price_files_are_rejected() {
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
    fn prices_load_reads_a_file_and_names_it_in_spec_errors() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("prices.yaml");
        std::fs::write(&good, PRICES).unwrap();
        let t = PriceTable::load(&good).unwrap();
        assert!(t.price_for("gemini-3.8-flash").is_some());

        let bad = dir.path().join("bad.yaml");
        std::fs::write(&bad, "- model: m\n  input: -1\n  output: 1\n").unwrap();
        match PriceTable::load(&bad).unwrap_err() {
            MonitorError::Spec(detail) => assert_eq!(
                detail,
                format!(
                    "price file {}: model m: prices must be finite and not negative",
                    bad.display()
                )
            ),
            other => panic!("want Spec, got {other:?}"),
        }

        let missing = dir.path().join("missing.yaml");
        match PriceTable::load(&missing).unwrap_err() {
            MonitorError::Spec(detail) => assert!(
                detail.starts_with(&format!("price file {}: ", missing.display())),
                "{detail}"
            ),
            other => panic!("want Spec, got {other:?}"),
        }
    }

    #[test]
    fn prices_token_usage_cost_prices_each_count_per_million_tokens() {
        let u = TokenUsage::new(1_000_000, 200_000, 500_000, 100_000);
        assert_eq!(
            u,
            TokenUsage {
                input: 1_000_000,
                cached: 200_000,
                output: 500_000,
                reasoning: 100_000
            }
        );
        let p = ModelPrice::new(0.30, 2.50).with_cached_input(0.075);
        assert!((u.cost(&p) - 1.755).abs() < 1e-9, "{}", u.cost(&p));
        // Defaults (cached = input, reasoning = output) and the uncached floor.
        let u = TokenUsage::new(100, 300, 10, 5);
        assert!((u.cost(&ModelPrice::new(1.0, 4.0)) - 0.000_36).abs() < 1e-12);
        assert_eq!(TokenUsage::default().cost(&p), 0.0);
    }

    #[test]
    fn prices_token_usage_add_assign_saturates() {
        let mut u = TokenUsage::new(1, 2, 3, 4);
        u += TokenUsage::new(10, 20, 30, 40);
        assert_eq!(u, TokenUsage::new(11, 22, 33, 44));
        u += TokenUsage::new(u64::MAX, 0, 0, 0);
        assert_eq!(u, TokenUsage::new(u64::MAX, 22, 33, 44));
    }
}
