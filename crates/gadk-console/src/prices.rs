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
//! cached input, output (without reasoning) and reasoning.

use std::collections::BTreeMap;
use std::ops::AddAssign;
use std::path::Path;

use serde::Deserialize;

use crate::ConsoleError;

/// The span label naming a span's operation.
pub(crate) const LABEL_OPERATION: &str = "gen_ai.operation.name";
/// The operation of a model-call span.
pub(crate) const OP_GENERATE_CONTENT: &str = "generate_content";
/// The span label naming the requested model.
pub(crate) const LABEL_MODEL: &str = "gen_ai.request.model";
/// Input tokens: prompt plus tool-use prompt.
pub(crate) const LABEL_INPUT: &str = "gen_ai.usage.input_tokens";
/// Output tokens: candidates plus reasoning.
pub(crate) const LABEL_OUTPUT: &str = "gen_ai.usage.output_tokens";
/// Cached input tokens.
pub(crate) const LABEL_CACHED: &str = "gen_ai.usage.cache_read.input_tokens";
/// Reasoning (thinking) tokens.
pub(crate) const LABEL_REASONING: &str = "gen_ai.usage.reasoning.output_tokens";
/// The model shown for a call whose model is not known.
pub(crate) const UNKNOWN_MODEL: &str = "(unknown)";

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
pub(crate) struct Usage {
    /// Input tokens (prompt plus tool-use prompt), cached ones included.
    pub(crate) input: u64,
    /// Cached input tokens.
    pub(crate) cached: u64,
    /// Output tokens, without reasoning.
    pub(crate) output: u64,
    /// Reasoning (thinking) tokens.
    pub(crate) reasoning: u64,
}

impl Usage {
    /// The usage of a model-call span from its labels: input = `gen_ai.usage.input_tokens`,
    /// cached = `gen_ai.usage.cache_read.input_tokens`, reasoning =
    /// `gen_ai.usage.reasoning.output_tokens`, output = `gen_ai.usage.output_tokens` minus
    /// reasoning (floored at 0). A missing label, or one that is not a count, is 0.
    pub(crate) fn from_labels(labels: &BTreeMap<String, String>) -> Self {
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
        let uncached = self.input.saturating_sub(self.cached) as f64;
        (uncached * p.input
            + self.cached as f64 * p.cached_input
            + self.output as f64 * p.output
            + self.reasoning as f64 * p.reasoning)
            / 1_000_000.0
    }
}

impl AddAssign for Usage {
    fn add_assign(&mut self, other: Self) {
        self.input += other.input;
        self.cached += other.cached;
        self.output += other.output;
        self.reasoning += other.reasoning;
    }
}

/// The cost of `usage` on `model`; `None` without a price file or a price for the model.
pub(crate) fn cost_of(prices: Option<&PriceTable>, model: &str, usage: &Usage) -> Option<f64> {
    prices?.price_for(model).map(|p| usage.cost(p))
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

/// A sum of call costs that remembers whether some calls had no price.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct CostSum {
    /// The sum of the priced calls' costs, in USD.
    pub(crate) usd: f64,
    /// Whether any call had a price.
    pub(crate) priced: bool,
    /// Whether any call had no price.
    pub(crate) unpriced: bool,
}

impl CostSum {
    /// Adds one call's cost (`None`: the call had no price).
    pub(crate) fn add(&mut self, cost: Option<f64>) {
        match cost {
            Some(c) => {
                self.usd += c;
                self.priced = true;
            }
            None => self.unpriced = true,
        }
    }

    /// Whether the sum leaves out unpriced calls while covering priced ones.
    pub(crate) fn partial(&self) -> bool {
        self.priced && self.unpriced
    }

    /// `$x` (six decimals), `$x*` when it leaves out unpriced calls, `—` when no call had a price.
    pub(crate) fn text(&self) -> String {
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
    fn cost_of_and_texts() {
        let t = PriceTable::from_yaml(PRICES).unwrap();
        let u = Usage {
            input: 1200,
            cached: 200,
            output: 300,
            reasoning: 50,
        };
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
