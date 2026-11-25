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

//! Alert specs: fixed limits on the model calls in an agent-analytics table over a recent
//! window.
//!
//! ```yaml
//! name: prod-usage                  # [A-Za-z0-9_-]{1,64}
//! source:                           # BigQuery only
//!   bigquery: { project: my-proj, dataset: agent_analytics, table: agent_events }
//! window: 24h                       # <n>m | <n>h | <n>d, at most 30d: calls in [now - window, now)
//! rules:                            # 1-50 rules, each with exactly one metric
//!   - name: daily-cost              # optional, [A-Za-z0-9_-]{1,64}; default: the metric key
//!     cost: { max: 50 }             # USD, >= 0 (needs --prices)
//!   - error_rate: { max: 0.05, min_calls: 50 }   # 0-1; min_calls >= 1, default 20
//!   - name: support-calls
//!     where: { agent: support }     # optional: model, agent and/or prompt, matched exactly
//!     calls: { max: 10000 }         # integer >= 0
//!   - tokens: { max: 5000000 }      # input + output + reasoning, integer >= 0
//! ```
//!
//! A rule fires when its value is strictly greater than `max`.

use chrono::TimeDelta;
use serde_json::{Map, Value};

use crate::error::MonitorError;
use crate::guard_spec::{duration, parse_source};
use crate::spec::{SourceSpec, check_keys, is_monitor_name, required, string_field, yaml_value};

/// The most rules one alert spec has.
pub const MAX_ALERT_RULES: usize = 50;
/// The default `min_calls` of an `error_rate` rule.
pub const DEFAULT_ERROR_RATE_MIN_CALLS: u64 = 20;

/// The metric keys, in the order the spec documents them.
const METRICS: [&str; 4] = ["cost", "error_rate", "calls", "tokens"];

/// A validated alert spec.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct AlertSpec {
    /// The spec's name (`[A-Za-z0-9_-]{1,64}`).
    pub name: String,
    /// Where the calls come from (always [`SourceSpec::BigQuery`]).
    pub source: SourceSpec,
    /// How far back from now calls are counted (at most 30 days).
    pub window: TimeDelta,
    /// The rules, in spec order (1-50, with unique names).
    pub rules: Vec<AlertRule>,
}

impl AlertSpec {
    /// A spec named `name` checking `rules` on the calls in `source` over the last `window`.
    /// Nothing is validated: [`AlertSpec::from_yaml`] validates a spec it parses.
    pub fn new(
        name: impl Into<String>,
        source: SourceSpec,
        window: TimeDelta,
        rules: Vec<AlertRule>,
    ) -> Self {
        Self {
            name: name.into(),
            source,
            window,
            rules,
        }
    }

    /// Parses and validates an alert spec from YAML text.
    ///
    /// # Errors
    ///
    /// [`MonitorError::Spec`] (`alert spec: …`) naming the offending key.
    pub fn from_yaml(yaml: &str) -> Result<Self, MonitorError> {
        yaml_value(yaml)
            .and_then(|doc| parse(&doc))
            .map_err(|detail| MonitorError::Spec(format!("alert spec: {detail}")))
    }
}

/// One rule of an alert spec: a metric over the calls its filter keeps, and its limit.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct AlertRule {
    /// The rule's name (`[A-Za-z0-9_-]{1,64}`, unique in its spec); by default the metric key.
    pub name: String,
    /// Which calls count (`where`).
    pub filter: AlertFilter,
    /// What is measured, and its limit.
    pub metric: AlertMetric,
}

impl AlertRule {
    /// A rule named `name` measuring `metric` on the calls `filter` keeps.
    pub fn new(name: impl Into<String>, filter: AlertFilter, metric: AlertMetric) -> Self {
        Self {
            name: name.into(),
            filter,
            metric,
        }
    }
}

/// What a rule measures over its calls, and the most it may be. A value strictly greater
/// than `max` fires the rule.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AlertMetric {
    /// The summed token cost in USD of the calls whose model has a price.
    Cost {
        /// The most USD (>= 0).
        max: f64,
    },
    /// The share of calls that failed.
    ErrorRate {
        /// The largest share (0-1).
        max: f64,
        /// The fewest calls the rate needs (>= 1); fewer is `insufficient`.
        min_calls: u64,
    },
    /// The number of calls.
    Calls {
        /// The most calls.
        max: u64,
    },
    /// The summed input (cached included), output and reasoning tokens.
    Tokens {
        /// The most tokens.
        max: u64,
    },
}

impl AlertMetric {
    /// The metric key as written in YAML: `cost`, `error_rate`, `calls` or `tokens`.
    pub fn key(&self) -> &'static str {
        match self {
            AlertMetric::Cost { .. } => "cost",
            AlertMetric::ErrorRate { .. } => "error_rate",
            AlertMetric::Calls { .. } => "calls",
            AlertMetric::Tokens { .. } => "tokens",
        }
    }
}

/// Which calls a rule counts: those whose model, agent and prompt name equal the fields that
/// are set. An empty filter keeps every call.
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AlertFilter {
    /// Only calls to this model.
    pub model: Option<String>,
    /// Only calls from this agent.
    pub agent: Option<String>,
    /// Only calls of this prompt name.
    pub prompt: Option<String>,
}

impl AlertFilter {
    /// A filter on `model`, `agent` and `prompt`; `None` matches anything.
    pub fn new(model: Option<String>, agent: Option<String>, prompt: Option<String>) -> Self {
        Self {
            model,
            agent,
            prompt,
        }
    }
}

fn parse(doc: &Value) -> Result<AlertSpec, String> {
    let top = doc.as_object().ok_or("the document must be a mapping")?;
    check_keys(top, "", &["name", "source", "window", "rules"])?;
    let name = required(
        top,
        "name",
        is_monitor_name,
        "`name` must match [A-Za-z0-9_-]{1,64}",
    )?;
    let source = parse_source(top.get("source"), "alerts")?;
    let window = duration(top.get("window"), "window")?;
    let rules = match top.get("rules") {
        Some(Value::Array(a)) if (1..=MAX_ALERT_RULES).contains(&a.len()) => a
            .iter()
            .enumerate()
            .map(|(i, v)| parse_rule(i + 1, v))
            .collect::<Result<Vec<_>, _>>()?,
        _ => {
            return Err(format!(
                "`rules` must be a list of 1 to {MAX_ALERT_RULES} rules"
            ));
        }
    };
    for (i, rule) in rules.iter().enumerate() {
        if rules[..i].iter().any(|r| r.name == rule.name) {
            return Err(format!("duplicate rule name `{}`", rule.name));
        }
    }
    Ok(AlertSpec::new(name, source, window, rules))
}

/// Rule `i` (1-based): a mapping with exactly one metric key, an optional `name` and an
/// optional `where`.
fn parse_rule(i: usize, v: &Value) -> Result<AlertRule, String> {
    let at = format!("rules[{i}]");
    let shape = || {
        format!("`{at}` must be a mapping with exactly one of cost, error_rate, calls or tokens")
    };
    let m = v.as_object().ok_or_else(shape)?;
    let mut keys = m.keys().filter(|k| METRICS.contains(&k.as_str()));
    let (Some(key), None) = (keys.next(), keys.next()) else {
        return Err(shape());
    };
    let mut allowed = vec!["name", "where"];
    allowed.extend(METRICS);
    check_keys(m, &format!("`{at}`: "), &allowed)?;
    let name = string_field(
        m,
        "name",
        is_monitor_name,
        &format!("`{at}.name` must match [A-Za-z0-9_-]{{1,64}}"),
    )?
    .unwrap_or_else(|| key.clone());
    let filter = parse_filter(m.get("where"), &at)?;
    let metric = parse_metric(key, &m[key], &at)?;
    Ok(AlertRule::new(name, filter, metric))
}

/// The optional `where` of the rule at `at`: a mapping of `model`, `agent` and `prompt`, each a
/// non-empty string.
fn parse_filter(v: Option<&Value>, at: &str) -> Result<AlertFilter, String> {
    let w = match v {
        None | Some(Value::Null) => return Ok(AlertFilter::default()),
        Some(Value::Object(w)) => w,
        Some(_) => {
            return Err(format!(
                "`{at}.where` must be a mapping of model, agent and prompt"
            ));
        }
    };
    check_keys(w, &format!("`{at}.where`: "), &["model", "agent", "prompt"])?;
    let field = |key: &str| match w.get(key) {
        None => Ok(None),
        Some(Value::String(s)) if !s.is_empty() => Ok(Some(s.clone())),
        Some(_) => Err(format!("`{at}.where.{key}` must be a non-empty string")),
    };
    Ok(AlertFilter::new(
        field("model")?,
        field("agent")?,
        field("prompt")?,
    ))
}

/// The metric `key` of the rule at `at`, from its mapping `v`.
fn parse_metric(key: &str, v: &Value, at: &str) -> Result<AlertMetric, String> {
    let at = format!("{at}.{key}");
    let empty = Map::new();
    let m = v.as_object().unwrap_or(&empty);
    match key {
        "cost" => {
            check_keys(m, &format!("`{at}`: "), &["max"])?;
            let max = m
                .get("max")
                .and_then(Value::as_f64)
                .filter(|x| x.is_finite() && *x >= 0.0)
                .ok_or_else(|| format!("`{at}.max` must be a number of at least 0"))?;
            Ok(AlertMetric::Cost { max })
        }
        "error_rate" => {
            check_keys(m, &format!("`{at}`: "), &["max", "min_calls"])?;
            let max = m
                .get("max")
                .and_then(Value::as_f64)
                .filter(|x| (0.0..=1.0).contains(x))
                .ok_or_else(|| format!("`{at}.max` must be a number from 0 to 1"))?;
            let min_calls = match m.get("min_calls") {
                None | Some(Value::Null) => DEFAULT_ERROR_RATE_MIN_CALLS,
                Some(v) => v
                    .as_u64()
                    .filter(|n| *n >= 1)
                    .ok_or_else(|| format!("`{at}.min_calls` must be an integer of at least 1"))?,
            };
            Ok(AlertMetric::ErrorRate { max, min_calls })
        }
        _ => {
            check_keys(m, &format!("`{at}`: "), &["max"])?;
            let max = m
                .get("max")
                .and_then(Value::as_u64)
                .ok_or_else(|| format!("`{at}.max` must be an integer of at least 0"))?;
            Ok(if key == "calls" {
                AlertMetric::Calls { max }
            } else {
                AlertMetric::Tokens { max }
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"
name: prod-usage
source:
  bigquery: { project: my-proj, dataset: agent_analytics, table: agent_events }
window: 24h
rules:
  - name: daily-cost
    cost: { max: 50 }
  - error_rate: { max: 0.05, min_calls: 50 }
  - name: support-calls
    where: { agent: support }
    calls: { max: 10000 }
  - name: flash-answer-tokens
    where: { model: gemini-3.8-flash, agent: support, prompt: support/answer }
    tokens: { max: 5000000 }
"#;

    /// The smallest valid spec, with `line` (`key: value`) replacing the line of that key or
    /// appended when no line has that key; an empty value removes the key.
    fn with(line: &str) -> String {
        let (key, value) = line.split_once(':').expect("key: value");
        let mut lines: Vec<String> = [
            "name: a",
            "source: { bigquery: { project: p, dataset: d, table: t } }",
            "window: 1h",
            "rules: [calls: { max: 10 }]",
        ]
        .iter()
        .map(|l| (*l).to_owned())
        .collect();
        lines.retain(|l| !l.starts_with(&format!("{key}:")));
        if !value.trim().is_empty() {
            lines.push(line.to_owned());
        }
        lines.join("\n")
    }

    /// The printed error of an invalid spec.
    fn printed(yaml: &str) -> String {
        match AlertSpec::from_yaml(yaml) {
            Err(e @ MonitorError::Spec(_)) => e.to_string(),
            other => panic!("expected a spec error, got {other:?}"),
        }
    }

    /// The printed error of a spec whose `rules` is `rules`, without the `monitor spec: alert
    /// spec: ` prefix.
    fn rule_error(rules: &str) -> String {
        let e = printed(&with(&format!("rules: {rules}")));
        e.strip_prefix("monitor spec: alert spec: ")
            .unwrap_or_else(|| panic!("{e}"))
            .to_owned()
    }

    /// The rules of a valid spec whose `rules` is `rules`.
    fn rules(rules: &str) -> Vec<AlertRule> {
        AlertSpec::from_yaml(&with(&format!("rules: {rules}")))
            .unwrap()
            .rules
    }

    #[test]
    fn parses_a_full_spec() {
        let s = AlertSpec::from_yaml(FULL).unwrap();
        assert_eq!(s.name, "prod-usage");
        assert_eq!(
            s.source,
            SourceSpec::BigQuery {
                project: "my-proj".into(),
                dataset: "agent_analytics".into(),
                table: "agent_events".into(),
            }
        );
        assert_eq!(s.window, TimeDelta::hours(24));
        assert_eq!(
            s.rules,
            vec![
                AlertRule::new(
                    "daily-cost",
                    AlertFilter::default(),
                    AlertMetric::Cost { max: 50.0 }
                ),
                AlertRule::new(
                    "error_rate",
                    AlertFilter::default(),
                    AlertMetric::ErrorRate {
                        max: 0.05,
                        min_calls: 50
                    }
                ),
                AlertRule::new(
                    "support-calls",
                    AlertFilter::new(None, Some("support".into()), None),
                    AlertMetric::Calls { max: 10_000 }
                ),
                AlertRule::new(
                    "flash-answer-tokens",
                    AlertFilter::new(
                        Some("gemini-3.8-flash".into()),
                        Some("support".into()),
                        Some("support/answer".into())
                    ),
                    AlertMetric::Tokens { max: 5_000_000 }
                ),
            ]
        );
        let keys: Vec<_> = s.rules.iter().map(|r| r.metric.key()).collect();
        assert_eq!(keys, ["cost", "error_rate", "calls", "tokens"]);
    }

    #[test]
    fn applies_every_default() {
        let r = rules("[error_rate: { max: 0.1 }, cost: { max: 1.5 }, tokens: { max: 7 }]");
        assert_eq!(
            r,
            vec![
                AlertRule::new(
                    "error_rate",
                    AlertFilter::default(),
                    AlertMetric::ErrorRate {
                        max: 0.1,
                        min_calls: DEFAULT_ERROR_RATE_MIN_CALLS
                    }
                ),
                AlertRule::new(
                    "cost",
                    AlertFilter::default(),
                    AlertMetric::Cost { max: 1.5 }
                ),
                AlertRule::new(
                    "tokens",
                    AlertFilter::default(),
                    AlertMetric::Tokens { max: 7 }
                ),
            ]
        );
        assert_eq!(DEFAULT_ERROR_RATE_MIN_CALLS, 20);
        // A null `where` and an empty one keep every call.
        let r =
            rules("[{ where: , calls: { max: 1 } }, { name: b, where: {}, calls: { max: 1 } }]");
        assert_eq!(r[0].filter, AlertFilter::default());
        assert_eq!(r[1].filter, AlertFilter::default());
    }

    #[test]
    fn accepts_the_bounds() {
        let s = AlertSpec::from_yaml(&with("window: 1m")).unwrap();
        assert_eq!(s.window, TimeDelta::minutes(1));
        let s = AlertSpec::from_yaml(&with("window: 30d")).unwrap();
        assert_eq!(s.window, TimeDelta::days(30));
        let s = AlertSpec::from_yaml(&with("window: 720h")).unwrap();
        assert_eq!(s.window, TimeDelta::days(30));
        let s = AlertSpec::from_yaml(&with(&format!("name: {}", "a".repeat(64)))).unwrap();
        assert_eq!(s.name.len(), 64);
        let r = rules(
            "[cost: { max: 0 }, \
             { name: e0, error_rate: { max: 0, min_calls: 1 } }, \
             { name: e1, error_rate: { max: 1 } }, \
             calls: { max: 0 }, tokens: { max: 0 }]",
        );
        let metrics: Vec<_> = r.iter().map(|r| r.metric).collect();
        assert_eq!(
            metrics,
            [
                AlertMetric::Cost { max: 0.0 },
                AlertMetric::ErrorRate {
                    max: 0.0,
                    min_calls: 1
                },
                AlertMetric::ErrorRate {
                    max: 1.0,
                    min_calls: 20
                },
                AlertMetric::Calls { max: 0 },
                AlertMetric::Tokens { max: 0 },
            ]
        );
        let fifty: Vec<String> = (0..MAX_ALERT_RULES)
            .map(|i| format!("{{ name: r{i}, calls: {{ max: 1 }} }}"))
            .collect();
        assert_eq!(rules(&format!("[{}]", fifty.join(", "))).len(), 50);
    }

    #[test]
    fn rejects_a_non_mapping_document() {
        assert_eq!(
            printed("- a\n- b\n"),
            "monitor spec: alert spec: the document must be a mapping"
        );
    }

    #[test]
    fn rejects_an_unknown_key() {
        assert_eq!(
            printed(&with("checks: []")),
            "monitor spec: alert spec: unknown key `checks` (expected one of: name, source, \
             window, rules)"
        );
    }

    #[test]
    fn rejects_a_bad_name() {
        for name in ["\"has space\"", "", "5", &"a".repeat(65)] {
            assert_eq!(
                printed(&with(&format!("name: {name}"))),
                "monitor spec: alert spec: `name` must match [A-Za-z0-9_-]{1,64}",
                "{name}"
            );
        }
    }

    #[test]
    fn rejects_bad_sources() {
        for (source, detail) in [
            ("", "`source` must be a mapping with `bigquery`"),
            ("{}", "`source` must be a mapping with `bigquery`"),
            (
                "{ trace: {} }",
                "`source`: unknown key `trace` (expected one of: bigquery)",
            ),
            (
                "{ cloud_trace: { project: p } }",
                "`source.cloud_trace` is not supported by alerts; use `source.bigquery`",
            ),
            (
                "{ bigquery: { project: p, dataset: d } }",
                "`source.bigquery.table` must match [A-Za-z0-9_]+",
            ),
            (
                "{ bigquery: { project: \"p.q\", dataset: d, table: t } }",
                "`source.bigquery.project` must match [A-Za-z0-9_-]+",
            ),
        ] {
            assert_eq!(
                printed(&with(&format!("source: {source}"))),
                format!("monitor spec: alert spec: {detail}"),
                "{source}"
            );
        }
    }

    #[test]
    fn rejects_a_bad_window() {
        for window in ["", "0h", "5s", "h", "\"\"", "1"] {
            assert_eq!(
                printed(&with(&format!("window: {window}"))),
                "monitor spec: alert spec: `window` must be <n>m, <n>h or <n>d with n >= 1",
                "{window}"
            );
        }
        for window in ["31d", "721h", "43201m"] {
            assert_eq!(
                printed(&with(&format!("window: {window}"))),
                "monitor spec: alert spec: `window` must be at most 30d",
                "{window}"
            );
        }
    }

    #[test]
    fn rejects_a_bad_rule_list() {
        let fifty_one: Vec<String> = (0..=MAX_ALERT_RULES)
            .map(|i| format!("{{ name: r{i}, calls: {{ max: 1 }} }}"))
            .collect();
        for list in [
            "".to_owned(),
            "[]".to_owned(),
            "{ calls: { max: 1 } }".to_owned(),
            "calls".to_owned(),
            format!("[{}]", fifty_one.join(", ")),
        ] {
            assert_eq!(
                printed(&with(&format!("rules: {list}"))),
                "monitor spec: alert spec: `rules` must be a list of 1 to 50 rules",
                "{list}"
            );
        }
    }

    #[test]
    fn a_rule_needs_exactly_one_metric_before_unknown_keys() {
        for rule in [
            "calls",
            "[calls]",
            "{}",
            "{ name: a }",
            "{ cost: { max: 1 }, calls: { max: 1 } }",
            "{ bogus: 1 }",
            "{ bogus: 1, cost: { max: 1 }, tokens: { max: 1 } }",
        ] {
            assert_eq!(
                rule_error(&format!("[{{ calls: {{ max: 1 }} }}, {rule}]")),
                "`rules[2]` must be a mapping with exactly one of cost, error_rate, calls or \
                 tokens",
                "{rule}"
            );
        }
        assert_eq!(
            rule_error("[{ calls: { max: 1 }, max: 2 }]"),
            "`rules[1]`: unknown key `max` (expected one of: name, where, cost, error_rate, \
             calls, tokens)"
        );
    }

    #[test]
    fn rejects_a_bad_rule_name() {
        for name in ["\"a b\"", "5", "[]", "\"\"", &"r".repeat(65)] {
            assert_eq!(
                rule_error(&format!("[{{ name: {name}, calls: {{ max: 1 }} }}]")),
                "`rules[1].name` must match [A-Za-z0-9_-]{1,64}",
                "{name}"
            );
        }
    }

    #[test]
    fn rejects_a_bad_where() {
        for w in ["5", "[model]", "agent"] {
            assert_eq!(
                rule_error(&format!("[{{ where: {w}, calls: {{ max: 1 }} }}]")),
                "`rules[1].where` must be a mapping of model, agent and prompt",
                "{w}"
            );
        }
        assert_eq!(
            rule_error("[{ where: { label: production }, calls: { max: 1 } }]"),
            "`rules[1].where`: unknown key `label` (expected one of: model, agent, prompt)"
        );
        for (key, value) in [
            ("model", "\"\""),
            ("agent", "5"),
            ("prompt", ""),
            ("model", "[m]"),
        ] {
            assert_eq!(
                rule_error(&format!(
                    "[{{ where: {{ {key}: {value} }}, calls: {{ max: 1 }} }}]"
                )),
                format!("`rules[1].where.{key}` must be a non-empty string"),
                "{key}: {value}"
            );
        }
    }

    #[test]
    fn rejects_bad_metric_values() {
        for (rule, detail) in [
            (
                "cost: { max: -1 }",
                "`rules[1].cost.max` must be a number of at least 0",
            ),
            (
                "cost: { max: x }",
                "`rules[1].cost.max` must be a number of at least 0",
            ),
            (
                "cost: {}",
                "`rules[1].cost.max` must be a number of at least 0",
            ),
            (
                "cost: 5",
                "`rules[1].cost.max` must be a number of at least 0",
            ),
            (
                "error_rate: { max: 1.5 }",
                "`rules[1].error_rate.max` must be a number from 0 to 1",
            ),
            (
                "error_rate: { max: -0.1 }",
                "`rules[1].error_rate.max` must be a number from 0 to 1",
            ),
            (
                "error_rate: { min_calls: 5 }",
                "`rules[1].error_rate.max` must be a number from 0 to 1",
            ),
            (
                "error_rate: { max: 0.1, min_calls: 0 }",
                "`rules[1].error_rate.min_calls` must be an integer of at least 1",
            ),
            (
                "error_rate: { max: 0.1, min_calls: 2.5 }",
                "`rules[1].error_rate.min_calls` must be an integer of at least 1",
            ),
            (
                "calls: { max: -1 }",
                "`rules[1].calls.max` must be an integer of at least 0",
            ),
            (
                "calls: { max: 1.5 }",
                "`rules[1].calls.max` must be an integer of at least 0",
            ),
            (
                "calls: {}",
                "`rules[1].calls.max` must be an integer of at least 0",
            ),
            (
                "tokens: { max: -5 }",
                "`rules[1].tokens.max` must be an integer of at least 0",
            ),
            (
                "tokens: { max: \"10\" }",
                "`rules[1].tokens.max` must be an integer of at least 0",
            ),
        ] {
            assert_eq!(rule_error(&format!("[{{ {rule} }}]")), detail, "{rule}");
        }
    }

    #[test]
    fn rejects_unknown_metric_keys() {
        for (rule, detail) in [
            (
                "cost: { max: 1, min: 0 }",
                "`rules[1].cost`: unknown key `min` (expected one of: max)",
            ),
            (
                "error_rate: { max: 0.1, window: 1h }",
                "`rules[1].error_rate`: unknown key `window` (expected one of: max, min_calls)",
            ),
            (
                "calls: { max: 1, min_calls: 2 }",
                "`rules[1].calls`: unknown key `min_calls` (expected one of: max)",
            ),
            (
                "tokens: { maximum: 1 }",
                "`rules[1].tokens`: unknown key `maximum` (expected one of: max)",
            ),
        ] {
            assert_eq!(rule_error(&format!("[{{ {rule} }}]")), detail, "{rule}");
        }
    }

    #[test]
    fn rejects_duplicate_rule_names() {
        assert_eq!(
            rule_error("[calls: { max: 1 }, calls: { max: 2 }]"),
            "duplicate rule name `calls`"
        );
        assert_eq!(
            rule_error("[{ name: tokens, calls: { max: 1 } }, tokens: { max: 2 }]"),
            "duplicate rule name `tokens`"
        );
        assert_eq!(
            rule_error("[{ name: x, cost: { max: 1 } }, { name: x, calls: { max: 2 } }]"),
            "duplicate rule name `x`"
        );
        // Distinct names make the same metric twice valid.
        assert_eq!(
            rules("[{ name: a, calls: { max: 1 } }, { name: b, calls: { max: 2 } }]").len(),
            2
        );
    }

    #[test]
    fn rule_errors_name_their_position() {
        assert_eq!(
            rule_error("[calls: { max: 1 }, tokens: { max: 1 }, cost: { max: -1 }]"),
            "`rules[3].cost.max` must be a number of at least 0"
        );
    }
}
