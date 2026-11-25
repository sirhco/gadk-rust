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

//! Alert evaluation for `gadk-eval alerts`: each rule's value over the usage groups its
//! filter keeps, the line printed for it, the run's counts and the `--notify` body.

use std::fmt::Write as _;

use serde_json::{Map, Value, json};

use crate::alert_source::UsageGroup;
use crate::alert_spec::{AlertFilter, AlertMetric, AlertRule, AlertSpec};
use crate::notify::NotifyFormat;
use crate::prices::PriceTable;

/// How a rule came out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuleStatus {
    /// The value is strictly greater than `max`.
    Fired,
    /// The value is at most `max`.
    Ok,
    /// An `error_rate` rule with fewer calls than its `min_calls`.
    Insufficient,
}

/// One rule's outcome and its printed line.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RuleResult {
    /// The rule.
    pub(crate) rule: AlertRule,
    /// Fired, ok or insufficient.
    pub(crate) status: RuleStatus,
    /// The value: USD for `cost`, a 0-1 fraction for `error_rate`, a count otherwise.
    pub(crate) value: f64,
    /// For `cost`: the calls of the groups whose model has no price (else 0).
    pub(crate) unpriced_calls: u64,
    /// The line without its leading `<rule>: ` and its trailing ` — FIRED` or ` — ok`.
    pub(crate) detail: String,
    /// The printed line (without a newline).
    pub(crate) line: String,
}

/// Every rule of `spec` evaluated over `groups`, in spec order. `cost` rules price each
/// group's tokens with `prices`; a group whose model has no price (or every group, without
/// `prices`) counts as unpriced calls.
pub(crate) fn evaluate(
    spec: &AlertSpec,
    groups: &[UsageGroup],
    prices: Option<&PriceTable>,
) -> Vec<RuleResult> {
    spec.rules
        .iter()
        .map(|rule| evaluate_rule(rule, groups, prices))
        .collect()
}

/// Whether `group` has the model, agent and prompt `filter` asks for.
fn matches(filter: &AlertFilter, group: &UsageGroup) -> bool {
    filter.model.as_ref().is_none_or(|m| *m == group.model)
        && filter.agent.as_ref().is_none_or(|a| *a == group.agent)
        && filter
            .prompt
            .as_ref()
            .is_none_or(|p| group.prompt.as_ref() == Some(p))
}

/// `>` when the rule fired, else `≤`.
fn op(fired: bool) -> &'static str {
    if fired { ">" } else { "≤" }
}

/// [`RuleStatus::Fired`] when the rule fired, else [`RuleStatus::Ok`].
fn status(fired: bool) -> RuleStatus {
    if fired {
        RuleStatus::Fired
    } else {
        RuleStatus::Ok
    }
}

/// The decimal places to print `value` and `max` with: `base`, or more (up to `base + 6`) when
/// the two differ but would print the same at `base`, so a line never shows a fired value equal
/// to its limit. Values that print the same at every width keep `base`.
fn places(value: f64, max: f64, base: usize) -> usize {
    (base..=base + 6)
        .find(|&p| format!("{value:.p$}") != format!("{max:.p$}"))
        .unwrap_or(base)
}

/// One rule's value and line over the groups its filter keeps.
fn evaluate_rule(
    rule: &AlertRule,
    groups: &[UsageGroup],
    prices: Option<&PriceTable>,
) -> RuleResult {
    let kept: Vec<&UsageGroup> = groups.iter().filter(|g| matches(&rule.filter, g)).collect();
    let calls: u64 = kept.iter().map(|g| g.calls).sum();
    let mut unpriced_calls = 0;
    let (status, value, detail) = match rule.metric {
        AlertMetric::Cost { max } => {
            let mut cost = 0.0;
            for g in &kept {
                match prices.and_then(|p| p.price_for(&g.model)) {
                    Some(price) => cost += g.usage.cost(price),
                    None => unpriced_calls += g.calls,
                }
            }
            let fired = cost > max;
            let note = if unpriced_calls > 0 {
                format!(" ({unpriced_calls} unpriced calls)")
            } else {
                String::new()
            };
            let p = places(cost, max, 2);
            (
                status(fired),
                cost,
                format!("cost ${cost:.p$} {} ${max:.p$}{note}", op(fired)),
            )
        }
        AlertMetric::ErrorRate { max, min_calls } => {
            let errors: u64 = kept.iter().map(|g| g.errors).sum();
            let rate = if calls == 0 {
                0.0
            } else {
                errors as f64 / calls as f64
            };
            if calls < min_calls {
                (
                    RuleStatus::Insufficient,
                    rate,
                    format!("error_rate — insufficient ({calls} calls < min_calls {min_calls})"),
                )
            } else {
                let fired = rate > max;
                let (percent, max_percent) = (rate * 100.0, max * 100.0);
                let p = places(percent, max_percent, 1);
                (
                    status(fired),
                    rate,
                    format!(
                        "error_rate {percent:.p$}% ({errors}/{calls}) {} {max_percent:.p$}%",
                        op(fired),
                    ),
                )
            }
        }
        AlertMetric::Calls { max } => {
            let fired = calls > max;
            (
                status(fired),
                calls as f64,
                format!("calls {calls} {} {max}", op(fired)),
            )
        }
        AlertMetric::Tokens { max } => {
            let tokens: u64 = kept
                .iter()
                .map(|g| g.usage.input + g.usage.output + g.usage.reasoning)
                .sum();
            let fired = tokens > max;
            (
                status(fired),
                tokens as f64,
                format!("tokens {tokens} {} {max}", op(fired)),
            )
        }
    };
    let line = match status {
        RuleStatus::Fired => format!("{}: {detail} — FIRED", rule.name),
        RuleStatus::Ok => format!("{}: {detail} — ok", rule.name),
        RuleStatus::Insufficient => format!("{}: {detail}", rule.name),
    };
    RuleResult {
        rule: rule.clone(),
        status,
        value,
        unpriced_calls,
        detail,
        line,
    }
}

/// The counts of an `alerts` run, as its summary line prints them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct AlertCounts {
    /// Specs run.
    pub(crate) specs: usize,
    /// Rules of the specs that did not error.
    pub(crate) rules: usize,
    /// Rules that fired.
    pub(crate) fired: usize,
    /// Rules that were ok.
    pub(crate) ok: usize,
    /// Rules with too few calls.
    pub(crate) insufficient: usize,
    /// Specs that errored.
    pub(crate) errors: usize,
}

impl AlertCounts {
    /// Counts the rules of one spec that did not error.
    pub(crate) fn add(&mut self, results: &[RuleResult]) {
        self.rules += results.len();
        for r in results {
            match r.status {
                RuleStatus::Fired => self.fired += 1,
                RuleStatus::Ok => self.ok += 1,
                RuleStatus::Insufficient => self.insufficient += 1,
            }
        }
    }

    /// `alerts: <S> specs, <R> rules — <f> fired, <o> ok, <i> insufficient, <e> errors`.
    pub(crate) fn summary_line(&self) -> String {
        format!(
            "alerts: {} specs, {} rules — {} fired, {} ok, {} insufficient, {} errors",
            self.specs, self.rules, self.fired, self.ok, self.insufficient, self.errors
        )
    }
}

/// A rule that fired or a spec that errored in an `alerts` run, for the `--notify` body.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum AlertOutcome {
    /// A rule of spec `spec` fired.
    Fired {
        /// The spec name.
        spec: String,
        /// The rule's result.
        result: RuleResult,
    },
    /// Spec `spec` errored.
    Error {
        /// The spec name.
        spec: String,
        /// The text printed after `error: `.
        message: String,
    },
}

/// The webhook body for an `alerts` run that printed `summary_line` (without its newline) and
/// counted `counts`, with `outcomes` in output order.
pub(crate) fn alerts_body(
    format: NotifyFormat,
    summary_line: &str,
    counts: &AlertCounts,
    outcomes: &[AlertOutcome],
) -> Value {
    match format {
        NotifyFormat::Slack => {
            let mut text = summary_line.to_owned();
            for outcome in outcomes {
                match outcome {
                    AlertOutcome::Fired { spec, result } => {
                        let _ = write!(text, "\n• {spec}/{}: {}", result.rule.name, result.detail);
                    }
                    AlertOutcome::Error { spec, message } => {
                        let _ = write!(text, "\n• {spec}: error: {message}");
                    }
                }
            }
            json!({ "text": text })
        }
        NotifyFormat::Json => {
            let mut fired = Vec::new();
            let mut errors = Vec::new();
            for outcome in outcomes {
                match outcome {
                    AlertOutcome::Fired { spec, result } => fired.push(fired_json(spec, result)),
                    AlertOutcome::Error { spec, message } => {
                        errors.push(json!({ "spec": spec, "error": message }));
                    }
                }
            }
            json!({
                "summary": {
                    "specs": counts.specs,
                    "rules": counts.rules,
                    "fired": counts.fired,
                    "ok": counts.ok,
                    "insufficient": counts.insufficient,
                    "errors": counts.errors,
                },
                "fired": fired,
                "errors": errors,
            })
        }
    }
}

/// The JSON entry of a fired rule of spec `spec`.
fn fired_json(spec: &str, r: &RuleResult) -> Value {
    let (value, max) = match r.rule.metric {
        AlertMetric::Cost { max } | AlertMetric::ErrorRate { max, .. } => {
            (json!(r.value), json!(max))
        }
        AlertMetric::Calls { max } | AlertMetric::Tokens { max } => {
            (json!(r.value as u64), json!(max))
        }
    };
    let mut filter = Map::new();
    for (key, v) in [
        ("model", &r.rule.filter.model),
        ("agent", &r.rule.filter.agent),
        ("prompt", &r.rule.filter.prompt),
    ] {
        if let Some(v) = v {
            filter.insert(key.to_owned(), json!(v));
        }
    }
    let mut entry = json!({
        "spec": spec,
        "rule": r.rule.name,
        "metric": r.rule.metric.key(),
        "value": value,
        "max": max,
        "where": filter,
    });
    if matches!(r.rule.metric, AlertMetric::Cost { .. }) {
        entry["unpriced_calls"] = json!(r.unpriced_calls);
    }
    entry
}

#[cfg(test)]
mod tests {
    use crate::prices::TokenUsage;

    use super::*;

    /// Four groups: 156 calls, 12 failed, 221,000 tokens; `m-new` has no price.
    fn groups() -> Vec<UsageGroup> {
        vec![
            UsageGroup::new(
                "m-flash",
                "support",
                Some("support/answer".into()),
                100,
                5,
                TokenUsage::new(100_000, 20_000, 30_000, 0),
            ),
            UsageGroup::new(
                "m-flash",
                "triage",
                None,
                40,
                0,
                TokenUsage::new(20_000, 0, 4_000, 1_000),
            ),
            UsageGroup::new(
                "m-pro",
                "support",
                Some("support/answer".into()),
                10,
                1,
                TokenUsage::new(50_000, 0, 10_000, 5_000),
            ),
            UsageGroup::new(
                "m-new",
                "support",
                None,
                6,
                6,
                TokenUsage::new(1_000, 0, 0, 0),
            ),
        ]
    }

    /// `m-flash` costs $0.11 + $0.02 and `m-pro` $0.25 of [`groups`].
    fn prices() -> PriceTable {
        PriceTable::from_yaml(
            "- { model: m-flash, input: 0.5, output: 2 }\n\
             - { model: m-pro, input: 2, output: 10 }\n",
        )
        .unwrap()
    }

    /// A spec named `prod` with `rules` (a YAML list).
    fn spec(rules: &str) -> AlertSpec {
        AlertSpec::from_yaml(&format!(
            "name: prod\nsource: {{ bigquery: {{ project: p, dataset: d, table: t }} }}\n\
             window: 1d\nrules: {rules}\n"
        ))
        .unwrap()
    }

    /// The lines of `rules` evaluated over [`groups`] with [`prices`].
    fn lines(rules: &str) -> Vec<String> {
        evaluate(&spec(rules), &groups(), Some(&prices()))
            .into_iter()
            .map(|r| r.line)
            .collect()
    }

    #[test]
    fn each_metric_over_every_call() {
        let results = evaluate(
            &spec(
                "[cost: { max: 0.3 }, error_rate: { max: 0.05 }, calls: { max: 100 }, \
                 tokens: { max: 300000 }]",
            ),
            &groups(),
            Some(&prices()),
        );
        let lines: Vec<&str> = results.iter().map(|r| r.line.as_str()).collect();
        assert_eq!(
            lines,
            [
                "cost: cost $0.38 > $0.30 (6 unpriced calls) — FIRED",
                "error_rate: error_rate 7.7% (12/156) > 5.0% — FIRED",
                "calls: calls 156 > 100 — FIRED",
                "tokens: tokens 221000 ≤ 300000 — ok",
            ]
        );
        let statuses: Vec<RuleStatus> = results.iter().map(|r| r.status).collect();
        assert_eq!(
            statuses,
            [
                RuleStatus::Fired,
                RuleStatus::Fired,
                RuleStatus::Fired,
                RuleStatus::Ok
            ]
        );
        assert!(
            (results[0].value - 0.38).abs() < 1e-9,
            "{}",
            results[0].value
        );
        assert_eq!(results[0].unpriced_calls, 6);
        assert!((results[1].value - 12.0 / 156.0).abs() < 1e-12);
        assert_eq!(results[2].value, 156.0);
        assert_eq!(results[3].value, 221_000.0);
        assert_eq!(results[3].unpriced_calls, 0);
        assert_eq!(results[0].detail, "cost $0.38 > $0.30 (6 unpriced calls)");
        assert_eq!(results[3].detail, "tokens 221000 ≤ 300000");
    }

    #[test]
    fn filters_keep_exactly_matching_groups() {
        assert_eq!(
            lines(
                "[{ name: support, where: { agent: support }, calls: { max: 1000 } }, \
                 { name: flash, where: { model: m-flash }, calls: { max: 1000 } }, \
                 { name: answer, where: { prompt: support/answer }, calls: { max: 1000 } }, \
                 { name: pro-answer, where: { model: m-pro, agent: support, \
                   prompt: support/answer }, calls: { max: 1000 } }, \
                 { name: nothing, where: { model: m-flash, agent: billing }, \
                   calls: { max: 1000 } }, \
                 { name: prefix, where: { model: m- }, calls: { max: 1000 } }, \
                 { name: answer-tokens, where: { prompt: support/answer }, \
                   tokens: { max: 1000000 } }]"
            ),
            [
                "support: calls 116 ≤ 1000 — ok",
                "flash: calls 140 ≤ 1000 — ok",
                "answer: calls 110 ≤ 1000 — ok",
                "pro-answer: calls 10 ≤ 1000 — ok",
                "nothing: calls 0 ≤ 1000 — ok",
                "prefix: calls 0 ≤ 1000 — ok",
                "answer-tokens: tokens 195000 ≤ 1000000 — ok",
            ]
        );
    }

    #[test]
    fn a_value_equal_to_max_is_ok() {
        assert_eq!(
            lines(
                "[{ where: { model: m-pro }, cost: { max: 0.25 } }, \
                 { where: { model: m-pro }, error_rate: { max: 0.1, min_calls: 10 } }, \
                 calls: { max: 156 }, tokens: { max: 221000 }]"
            ),
            [
                "cost: cost $0.25 ≤ $0.25 — ok",
                "error_rate: error_rate 10.0% (1/10) ≤ 10.0% — ok",
                "calls: calls 156 ≤ 156 — ok",
                "tokens: tokens 221000 ≤ 221000 — ok",
            ]
        );
        assert_eq!(
            lines(
                "[{ where: { model: m-pro }, cost: { max: 0.24 } }, calls: { max: 155 }, \
                 tokens: { max: 220999 }]"
            ),
            [
                "cost: cost $0.25 > $0.24 — FIRED",
                "calls: calls 156 > 155 — FIRED",
                "tokens: tokens 221000 > 220999 — FIRED",
            ]
        );
    }

    #[test]
    fn a_value_that_rounds_to_max_prints_enough_places_to_differ() {
        assert_eq!(
            lines(
                "[{ where: { model: m-pro }, cost: { max: 0.2499 } }, \
                 { where: { model: m-pro }, error_rate: { max: 0.0999, min_calls: 10 } }, \
                 { name: close, where: { model: m-pro }, error_rate: { max: 0.10001, min_calls: 10 } }]"
            ),
            [
                "cost: cost $0.2500 > $0.2499 — FIRED",
                "error_rate: error_rate 10.00% (1/10) > 9.99% — FIRED",
                "close: error_rate 10.000% (1/10) ≤ 10.001% — ok",
            ]
        );
    }

    #[test]
    fn places_widen_only_while_the_printed_values_match() {
        assert_eq!(places(0.25, 0.25, 2), 2);
        assert_eq!(places(0.26, 0.25, 2), 2);
        assert_eq!(places(0.2500, 0.2499, 2), 4);
        assert_eq!(places(0.25 + 1e-15, 0.25, 2), 2);
    }

    #[test]
    fn too_few_calls_is_insufficient() {
        let results = evaluate(
            &spec(
                "[{ name: new, where: { model: m-new }, error_rate: { max: 0.5 } }, \
                 { name: none, where: { agent: billing }, error_rate: { max: 0.5, min_calls: 1 } }, \
                 { name: enough, where: { model: m-new }, error_rate: { max: 0.5, min_calls: 6 } }]",
            ),
            &groups(),
            Some(&prices()),
        );
        let lines: Vec<&str> = results.iter().map(|r| r.line.as_str()).collect();
        assert_eq!(
            lines,
            [
                "new: error_rate — insufficient (6 calls < min_calls 20)",
                "none: error_rate — insufficient (0 calls < min_calls 1)",
                "enough: error_rate 100.0% (6/6) > 50.0% — FIRED",
            ]
        );
        assert_eq!(results[0].status, RuleStatus::Insufficient);
        assert_eq!(results[1].status, RuleStatus::Insufficient);
        assert_eq!(results[1].value, 0.0);
    }

    #[test]
    fn unpriced_calls_are_counted_not_priced() {
        assert_eq!(
            lines(
                "[{ name: new, where: { model: m-new }, cost: { max: 0 } }, \
                 { name: flash, where: { model: m-flash }, cost: { max: 1 } }]"
            ),
            [
                "new: cost $0.00 ≤ $0.00 (6 unpriced calls) — ok",
                "flash: cost $0.13 ≤ $1.00 — ok",
            ]
        );
        // Without a price table every call is unpriced.
        let results = evaluate(&spec("[cost: { max: 0 }]"), &groups(), None);
        assert_eq!(
            results[0].line,
            "cost: cost $0.00 ≤ $0.00 (156 unpriced calls) — ok"
        );
        assert_eq!(results[0].unpriced_calls, 156);
    }

    #[test]
    fn no_groups_gives_zero_values() {
        let results = evaluate(
            &spec(
                "[cost: { max: 0 }, error_rate: { max: 0 }, calls: { max: 0 }, tokens: { max: 0 }]",
            ),
            &[],
            Some(&prices()),
        );
        let lines: Vec<&str> = results.iter().map(|r| r.line.as_str()).collect();
        assert_eq!(
            lines,
            [
                "cost: cost $0.00 ≤ $0.00 — ok",
                "error_rate: error_rate — insufficient (0 calls < min_calls 20)",
                "calls: calls 0 ≤ 0 — ok",
                "tokens: tokens 0 ≤ 0 — ok",
            ]
        );
    }

    #[test]
    fn counts_add_rules_and_print_the_summary() {
        let mut counts = AlertCounts {
            specs: 3,
            errors: 1,
            ..AlertCounts::default()
        };
        counts.add(&evaluate(
            &spec(
                "[calls: { max: 100 }, tokens: { max: 300000 }, \
                 { where: { model: m-new }, error_rate: { max: 0.5 } }]",
            ),
            &groups(),
            Some(&prices()),
        ));
        counts.add(&evaluate(&spec("[calls: { max: 0 }]"), &groups(), None));
        assert_eq!(
            counts,
            AlertCounts {
                specs: 3,
                rules: 4,
                fired: 2,
                ok: 1,
                insufficient: 1,
                errors: 1,
            }
        );
        assert_eq!(
            counts.summary_line(),
            "alerts: 3 specs, 4 rules — 2 fired, 1 ok, 1 insufficient, 1 errors"
        );
    }

    /// The outcomes of a run: `prod` fired a cost rule (on `m-flash`) and a calls rule, and
    /// `staging` errored.
    fn outcomes() -> (AlertCounts, Vec<AlertOutcome>) {
        let results = evaluate(
            &spec(
                "[{ name: daily-cost, where: { model: m-flash, agent: support }, \
                   cost: { max: 0.1 } }, \
                 { name: answer-errors, where: { prompt: support/answer }, \
                   error_rate: { max: 0.1 } }, \
                 calls: { max: 100 }, tokens: { max: 300000 }]",
            ),
            &groups(),
            Some(&prices()),
        );
        let mut counts = AlertCounts {
            specs: 2,
            errors: 1,
            ..AlertCounts::default()
        };
        counts.add(&results);
        let mut outcomes: Vec<AlertOutcome> = results
            .into_iter()
            .filter(|r| r.status == RuleStatus::Fired)
            .map(|result| AlertOutcome::Fired {
                spec: "prod".into(),
                result,
            })
            .collect();
        outcomes.push(AlertOutcome::Error {
            spec: "staging".into(),
            message: "transcript source: HTTP 403: denied".into(),
        });
        (counts, outcomes)
    }

    #[test]
    fn slack_body_lists_fired_rules_and_errors() {
        let (counts, outcomes) = outcomes();
        let summary = counts.summary_line();
        assert_eq!(
            summary,
            "alerts: 2 specs, 4 rules — 2 fired, 2 ok, 0 insufficient, 1 errors"
        );
        let text = "alerts: 2 specs, 4 rules — 2 fired, 2 ok, 0 insufficient, 1 errors\n\
                    • prod/daily-cost: cost $0.11 > $0.10\n\
                    • prod/calls: calls 156 > 100\n\
                    • staging: error: transcript source: HTTP 403: denied";
        assert_eq!(
            alerts_body(NotifyFormat::Slack, &summary, &counts, &outcomes),
            json!({ "text": text })
        );
    }

    #[test]
    fn json_body_has_the_counts_each_fired_rule_and_each_error() {
        let (counts, outcomes) = outcomes();
        let summary = counts.summary_line();
        let body = alerts_body(NotifyFormat::Json, &summary, &counts, &outcomes);
        let cost = body["fired"][0]["value"].as_f64().unwrap();
        assert!((cost - 0.11).abs() < 1e-9, "{cost}");
        assert_eq!(
            body,
            json!({
                "summary": {
                    "specs": 2, "rules": 4, "fired": 2, "ok": 2, "insufficient": 0, "errors": 1
                },
                "fired": [
                    {
                        "spec": "prod", "rule": "daily-cost", "metric": "cost",
                        "value": cost, "max": 0.1,
                        "where": {"model": "m-flash", "agent": "support"},
                        "unpriced_calls": 0
                    },
                    {
                        "spec": "prod", "rule": "calls", "metric": "calls",
                        "value": 156, "max": 100, "where": {}
                    }
                ],
                "errors": [
                    {"spec": "staging", "error": "transcript source: HTTP 403: denied"}
                ]
            })
        );
    }

    #[test]
    fn json_body_gives_error_rates_as_fractions_and_tokens_as_integers() {
        let results = evaluate(
            &spec(
                "[{ where: { prompt: support/answer }, error_rate: { max: 0.05 } }, \
                 tokens: { max: 1000 }]",
            ),
            &groups(),
            Some(&prices()),
        );
        let outcomes: Vec<AlertOutcome> = results
            .into_iter()
            .map(|result| AlertOutcome::Fired {
                spec: "prod".into(),
                result,
            })
            .collect();
        let body = alerts_body(NotifyFormat::Json, "s", &AlertCounts::default(), &outcomes);
        assert_eq!(
            body["fired"],
            json!([
                {
                    "spec": "prod", "rule": "error_rate", "metric": "error_rate",
                    "value": 6.0 / 110.0, "max": 0.05,
                    "where": {"prompt": "support/answer"}
                },
                {
                    "spec": "prod", "rule": "tokens", "metric": "tokens",
                    "value": 221000, "max": 1000, "where": {}
                }
            ])
        );
        assert_eq!(body["errors"], json!([]));
        let slack = alerts_body(NotifyFormat::Slack, "s", &AlertCounts::default(), &outcomes);
        assert_eq!(
            slack,
            json!({
                "text": "s\n• prod/error_rate: error_rate 5.5% (6/110) > 5.0%\n\
                         • prod/tokens: tokens 221000 > 1000"
            })
        );
    }
}
