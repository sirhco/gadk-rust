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

//! Cloud Monitoring metrics: the results of `monitor`, `guard`, `guards` and `alerts` as custom
//! gauge points under `custom.googleapis.com/gadk/`.
//!
//! [`monitor_points`], [`guard_points`] and [`alert_points`] turn a report into
//! [`MetricPoint`]s (pure), and a [`MetricsWriter`] ([`CloudMetricsWriter`]) writes them.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use bytes::Bytes;
use chrono::{DateTime, SecondsFormat, Utc};
use futures::future::BoxFuture;
use gadk_eval::Outcome;
use gadk_model::HttpClient;
use http::Method;
use serde_json::{Value, json};

use crate::error::MonitorError;
use crate::gcp::{AdcTokenSource, GcpHttp, TokenSource, encode};
use crate::guard_report::{GuardReport, Status};
use crate::judge_rows::{JUDGE_KINDS, scorer_name, scorer_positions};
use crate::report::MonitorReport;

/// The prefix of every metric type the builders produce.
const METRIC_TYPE_PREFIX: &str = "custom.googleapis.com/gadk/";

/// The Cloud Monitoring API.
const MONITORING_API: &str = "https://monitoring.googleapis.com/v3";

/// The most time series one [`CloudMetricsWriter`] request carries.
pub const SERIES_PER_REQUEST: usize = 200;

/// The longest label value, in bytes; longer values are cut at a character boundary.
pub const LABEL_VALUE_MAX: usize = 1024;

/// A point's value.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum MetricValue {
    /// An `INT64` value.
    Int(i64),
    /// A `DOUBLE` value.
    Double(f64),
}

/// One gauge point of a custom metric.
///
/// ```
/// use std::collections::BTreeMap;
///
/// use chrono::Utc;
/// use gadk_eval_monitor::{MetricPoint, MetricValue};
///
/// let labels = BTreeMap::from([("spec".to_owned(), "support".to_owned())]);
/// let point = MetricPoint::new("monitor/pass_rate", labels, MetricValue::Double(0.9), Utc::now());
/// assert_eq!(point.metric, "monitor/pass_rate");
/// assert_eq!(point.labels["spec"], "support");
/// ```
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct MetricPoint {
    /// The metric type after `custom.googleapis.com/gadk/`, for example `monitor/pass_rate`.
    pub metric: String,
    /// The metric labels; each value is at most [`LABEL_VALUE_MAX`] bytes.
    pub labels: BTreeMap<String, String>,
    /// The value.
    pub value: MetricValue,
    /// The point's time (its interval's end).
    pub time: DateTime<Utc>,
}

impl MetricPoint {
    /// A point of `metric` with `labels`, `value` and `time`. Label values longer than
    /// [`LABEL_VALUE_MAX`] bytes are cut to at most that many, at a character boundary.
    pub fn new(
        metric: impl Into<String>,
        labels: BTreeMap<String, String>,
        value: MetricValue,
        time: DateTime<Utc>,
    ) -> Self {
        let labels = labels
            .into_iter()
            .map(|(key, value)| (key, cut_label(value)))
            .collect();
        Self {
            metric: metric.into(),
            labels,
            value,
            time,
        }
    }
}

/// `value` cut to at most [`LABEL_VALUE_MAX`] bytes, at a character boundary.
fn cut_label(mut value: String) -> String {
    if value.len() > LABEL_VALUE_MAX {
        let end = value.floor_char_boundary(LABEL_VALUE_MAX);
        value.truncate(end);
    }
    value
}

/// Labels from `(key, value)` pairs.
fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

/// `base` with one more label.
fn with_label(base: &BTreeMap<String, String>, key: &str, value: &str) -> BTreeMap<String, String> {
    let mut labels = base.clone();
    labels.insert(key.to_owned(), value.to_owned());
    labels
}

/// A count as an [`MetricValue::Int`].
fn count(n: usize) -> MetricValue {
    MetricValue::Int(i64::try_from(n).unwrap_or(i64::MAX))
}

/// One scorer position's results across a report's runs.
struct ScorerTally<'a> {
    kind: &'a str,
    n: usize,
    /// Results that are not errors.
    graded: u32,
    /// Graded results that passed.
    passes: u32,
    /// The sum and count of the graded results' scores.
    score_sum: u64,
    scored: u32,
}

/// The points of a monitor report, all at `report.generated_at` with the labels `spec` and
/// `agent` (`agent`, or `""`), in this order:
///
/// - `monitor/runs` and `monitor/errors` (`Int`);
/// - `monitor/pass_rate` (`Double`), when the report has runs;
/// - one `monitor/scorer_pass_rate` (`Double`, passes over the results that are not errors)
///   per scorer with such a result, with the label `scorer`: `<kind>` for the first scorer of a
///   kind and `<kind>_<n>` for the n-th, as [`judge_rows()`](crate::judge_rows()) names them;
/// - one `monitor/judge_score_mean` (`Double`) per `llm_judge` or `reference` scorer with a
///   score on a result that is not an error, with the same `scorer` label.
///
/// Scorers are in the order they first appear in the runs.
pub fn monitor_points(report: &MonitorReport, agent: Option<&str>) -> Vec<MetricPoint> {
    let time = report.generated_at;
    let base = labels(&[("spec", &report.spec), ("agent", agent.unwrap_or(""))]);
    let mut points = vec![
        MetricPoint::new("monitor/runs", base.clone(), count(report.runs.len()), time),
        MetricPoint::new(
            "monitor/errors",
            base.clone(),
            count(report.error_count()),
            time,
        ),
    ];
    if let Some(rate) = report.pass_rate() {
        points.push(MetricPoint::new(
            "monitor/pass_rate",
            base.clone(),
            MetricValue::Double(rate),
            time,
        ));
    }
    let mut tallies: Vec<ScorerTally> = Vec::new();
    for run in &report.runs {
        for (s, (kind, n)) in run.scores.iter().zip(scorer_positions(&run.scores)) {
            let i = match tallies.iter().position(|t| t.kind == kind && t.n == n) {
                Some(i) => i,
                None => {
                    tallies.push(ScorerTally {
                        kind,
                        n,
                        graded: 0,
                        passes: 0,
                        score_sum: 0,
                        scored: 0,
                    });
                    tallies.len() - 1
                }
            };
            if s.outcome == Outcome::Error {
                continue;
            }
            let t = &mut tallies[i];
            t.graded += 1;
            if s.outcome == Outcome::Pass {
                t.passes += 1;
            }
            if let Some(score) = s.score {
                t.score_sum += u64::from(score);
                t.scored += 1;
            }
        }
    }
    for t in tallies.iter().filter(|t| t.graded > 0) {
        points.push(MetricPoint::new(
            "monitor/scorer_pass_rate",
            with_label(&base, "scorer", &scorer_name(t.kind, t.n)),
            MetricValue::Double(f64::from(t.passes) / f64::from(t.graded)),
            time,
        ));
    }
    for t in tallies
        .iter()
        .filter(|t| JUDGE_KINDS.contains(&t.kind) && t.scored > 0)
    {
        points.push(MetricPoint::new(
            "monitor/judge_score_mean",
            with_label(&base, "scorer", &scorer_name(t.kind, t.n)),
            MetricValue::Double(t.score_sum as f64 / f64::from(t.scored)),
            time,
        ));
    }
    points
}

/// The points of a guard report, all at `report.generated_at` with the labels `spec`, `prompt`
/// and `label`, in this order: `guard/rollout_percent` (`Int`), `guard/verdict` (`Int(1)`, with
/// the label `verdict`: `advance`, `abort` or `wait`) and `guard/worse_checks` (`Int`, the
/// checks whose status is `worse`).
pub fn guard_points(report: &GuardReport) -> Vec<MetricPoint> {
    let time = report.generated_at;
    let base = labels(&[
        ("spec", &report.spec),
        ("prompt", &report.prompt),
        ("label", &report.label),
    ]);
    let worse = report
        .checks
        .iter()
        .filter(|c| c.status == Status::Worse)
        .count();
    vec![
        MetricPoint::new(
            "guard/rollout_percent",
            base.clone(),
            MetricValue::Int(i64::from(report.percent)),
            time,
        ),
        MetricPoint::new(
            "guard/verdict",
            with_label(&base, "verdict", report.verdict.as_str()),
            MetricValue::Int(1),
            time,
        ),
        MetricPoint::new("guard/worse_checks", base, count(worse), time),
    ]
}

/// One alert spec's rules as one `alerts` run checked them.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct AlertRun {
    /// The alert spec's name.
    pub spec: String,
    /// When the rules were checked (the end of the window).
    pub time: DateTime<Utc>,
    /// One value per rule, in spec order.
    pub rules: Vec<AlertRuleValue>,
}

impl AlertRun {
    /// The run of spec `spec` at `time` with `rules`.
    pub fn new(spec: impl Into<String>, time: DateTime<Utc>, rules: Vec<AlertRuleValue>) -> Self {
        Self {
            spec: spec.into(),
            time,
            rules,
        }
    }
}

/// One alert rule's value and outcome.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct AlertRuleValue {
    /// The rule's name.
    pub rule: String,
    /// The value: USD for `cost`, a 0-1 fraction for `error_rate`, a count otherwise.
    pub value: f64,
    /// Whether the rule fired; `None` when it had too few calls to say.
    pub fired: Option<bool>,
}

impl AlertRuleValue {
    /// Rule `rule` with `value` and `fired`.
    pub fn new(rule: impl Into<String>, value: f64, fired: Option<bool>) -> Self {
        Self {
            rule: rule.into(),
            value,
            fired,
        }
    }
}

/// The points of an alerts run, at `run.time` with the labels `spec` and `rule`: for each rule
/// whose `fired` is `Some`, `alerts/rule_value` (`Double`) and `alerts/rule_fired` (`Int`, 0 or
/// 1). A rule with too few calls writes nothing.
pub fn alert_points(run: &AlertRun) -> Vec<MetricPoint> {
    let mut points = Vec::new();
    for rule in &run.rules {
        let Some(fired) = rule.fired else {
            continue;
        };
        let labels = labels(&[("spec", &run.spec), ("rule", &rule.rule)]);
        points.push(MetricPoint::new(
            "alerts/rule_value",
            labels.clone(),
            MetricValue::Double(rule.value),
            run.time,
        ));
        points.push(MetricPoint::new(
            "alerts/rule_fired",
            labels,
            MetricValue::Int(i64::from(fired)),
            run.time,
        ));
    }
    points
}

/// Where metric points are written.
///
/// ```
/// use std::sync::Mutex;
///
/// use futures::future::BoxFuture;
/// use gadk_eval_monitor::{MetricPoint, MetricsWriter, MonitorError};
///
/// /// Keeps every point's metric.
/// #[derive(Default)]
/// struct Recorder(Mutex<Vec<String>>);
///
/// impl MetricsWriter for Recorder {
///     fn write<'a>(
///         &'a self,
///         project: &'a str,
///         points: &'a [MetricPoint],
///     ) -> BoxFuture<'a, Result<(), MonitorError>> {
///         Box::pin(async move {
///             let mut seen = self.0.lock().unwrap();
///             seen.extend(points.iter().map(|p| format!("{project} {}", p.metric)));
///             Ok(())
///         })
///     }
/// }
/// ```
pub trait MetricsWriter: Send + Sync {
    /// Writes `points` to the project `project`.
    ///
    /// # Errors
    ///
    /// Why some or all of the points were not written.
    fn write<'a>(
        &'a self,
        project: &'a str,
        points: &'a [MetricPoint],
    ) -> BoxFuture<'a, Result<(), MonitorError>>;
}

/// The Cloud Monitoring API as a [`MetricsWriter`]: each point is one `GAUGE` time series of
/// the `global` resource, written with `projects.timeSeries.create`, at most
/// [`SERIES_PER_REQUEST`] per request. Metric descriptors are created on the first write.
#[non_exhaustive]
#[derive(Clone)]
pub struct CloudMetricsWriter {
    gcp: GcpHttp,
}

impl fmt::Debug for CloudMetricsWriter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CloudMetricsWriter").finish_non_exhaustive()
    }
}

impl CloudMetricsWriter {
    /// A writer with Application Default Credentials. Must be called inside a Tokio runtime.
    ///
    /// # Errors
    ///
    /// [`MonitorError::Store`] when no credentials can be built.
    pub fn new() -> Result<Self, MonitorError> {
        let auth = AdcTokenSource::adc()
            .map_err(|e| MonitorError::Store(format!("Application Default Credentials: {e}")))?;
        Ok(Self::with_http(GcpHttp::default_http(), Arc::new(auth)))
    }

    /// A writer with an explicit HTTP client and credentials (tests).
    pub fn with_http(http: Arc<dyn HttpClient>, auth: Arc<dyn TokenSource>) -> Self {
        Self {
            gcp: GcpHttp::new(http, auth),
        }
    }

    /// Sends every chunk of `points`, even after a failed one, and sums up the failures.
    async fn write_all(&self, project: &str, points: &[MetricPoint]) -> Result<(), MonitorError> {
        let url = format!("{MONITORING_API}/projects/{}/timeSeries", encode(project));
        let mut failed = 0;
        let mut first_error = None;
        for chunk in points.chunks(SERIES_PER_REQUEST) {
            let series: Vec<Value> = chunk.iter().map(|p| time_series(project, p)).collect();
            let body = json!({ "timeSeries": series }).to_string();
            let error = match self
                .gcp
                .send(
                    Method::POST,
                    &url,
                    Some(("application/json", Bytes::from(body))),
                )
                .await
            {
                Ok(reply) if reply.ok() => None,
                Ok(reply) => Some(reply.error_text()),
                Err(e) => Some(e),
            };
            if let Some(e) = error {
                failed += chunk.len();
                first_error.get_or_insert(e);
            }
        }
        match first_error {
            None => Ok(()),
            Some(e) => Err(MonitorError::Store(format!(
                "{failed} of {} series not written: {e}",
                points.len()
            ))),
        }
    }
}

impl MetricsWriter for CloudMetricsWriter {
    fn write<'a>(
        &'a self,
        project: &'a str,
        points: &'a [MetricPoint],
    ) -> BoxFuture<'a, Result<(), MonitorError>> {
        Box::pin(self.write_all(project, points))
    }
}

/// `point` as one time series of the `global` resource in `project`.
fn time_series(project: &str, point: &MetricPoint) -> Value {
    let (value_type, value) = match point.value {
        MetricValue::Int(n) => ("INT64", json!({ "int64Value": n.to_string() })),
        MetricValue::Double(x) => ("DOUBLE", json!({ "doubleValue": x })),
    };
    json!({
        "metric": {
            "type": format!("{METRIC_TYPE_PREFIX}{}", point.metric),
            "labels": point.labels,
        },
        "resource": {"type": "global", "labels": {"project_id": project}},
        "metricKind": "GAUGE",
        "valueType": value_type,
        "points": [{
            "interval": {"endTime": point.time.to_rfc3339_opts(SecondsFormat::Nanos, true)},
            "value": value,
        }],
    })
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;
    use gadk_eval::ScoreResult;
    use gadk_model::rest::test_util::{FakeResponse, ScriptedClient};
    use regex::Regex;

    use super::*;
    use crate::gcp::NoAuth;
    use crate::guard_report::{Action, CheckResult, SideValue, Unit, Verdict};
    use crate::report::{MonitoredRun, Window};
    use crate::testutil::{at, run};

    fn report(runs: Vec<Vec<ScoreResult>>) -> MonitorReport {
        MonitorReport {
            spec: "support".into(),
            generated_at: at(1_800_000_000),
            window: Window {
                from: at(1_799_913_600),
                to: at(1_800_000_000),
            },
            source: "bigquery:p.d.t".into(),
            runs: runs
                .into_iter()
                .enumerate()
                .map(|(i, scores)| MonitoredRun {
                    run: run(&format!("inv-{i}"), 1_799_990_000, &[("hi", "hello", &[])]),
                    scores,
                })
                .collect(),
        }
    }

    fn point(metric: &str, pairs: &[(&str, &str)], value: MetricValue) -> MetricPoint {
        MetricPoint::new(metric, labels(pairs), value, at(1_800_000_000))
    }

    #[test]
    fn scorer_positions_number_every_kind() {
        let scores = [
            ScoreResult::check("contains", true, "ok"),
            ScoreResult::judged("llm_judge", true, 5, "good"),
            ScoreResult::check("contains", false, "missing"),
            ScoreResult::error("llm_judge", "timed out"),
            ScoreResult::check("tools", true, "ok"),
        ];
        assert_eq!(
            scorer_positions(&scores),
            [
                ("contains", 1),
                ("llm_judge", 1),
                ("contains", 2),
                ("llm_judge", 2),
                ("tools", 1),
            ]
        );
        assert_eq!(scorer_name("contains", 1), "contains");
        assert_eq!(scorer_name("contains", 2), "contains_2");
    }

    #[test]
    fn monitor_points_name_each_scorer_and_skip_ungraded_ones() {
        let r = report(vec![
            vec![
                ScoreResult::check("contains", true, "ok"),
                ScoreResult::check("contains", false, "missing"),
                ScoreResult::judged("llm_judge", true, 4, "good"),
            ],
            vec![
                ScoreResult::check("contains", true, "ok"),
                ScoreResult::check("contains", true, "ok"),
                ScoreResult::judged("llm_judge", false, 2, "rude"),
            ],
            vec![
                ScoreResult::check("contains", true, "ok"),
                ScoreResult::error("contains", "bad pattern"),
                ScoreResult::error("llm_judge", "judge timed out"),
            ],
        ]);
        let base = [("agent", "support-agent"), ("spec", "support")];
        let scorer = |name| {
            [
                ("agent", "support-agent"),
                ("scorer", name),
                ("spec", "support"),
            ]
        };
        assert_eq!(
            monitor_points(&r, Some("support-agent")),
            vec![
                point("monitor/runs", &base, MetricValue::Int(3)),
                point("monitor/errors", &base, MetricValue::Int(1)),
                point("monitor/pass_rate", &base, MetricValue::Double(0.0)),
                point(
                    "monitor/scorer_pass_rate",
                    &scorer("contains"),
                    MetricValue::Double(1.0)
                ),
                point(
                    "monitor/scorer_pass_rate",
                    &scorer("contains_2"),
                    MetricValue::Double(0.5)
                ),
                point(
                    "monitor/scorer_pass_rate",
                    &scorer("llm_judge"),
                    MetricValue::Double(0.5)
                ),
                point(
                    "monitor/judge_score_mean",
                    &scorer("llm_judge"),
                    MetricValue::Double(3.0)
                ),
            ]
        );
    }

    #[test]
    fn an_empty_report_has_counts_but_no_pass_rate() {
        let base = [("agent", ""), ("spec", "support")];
        assert_eq!(
            monitor_points(&report(Vec::new()), None),
            vec![
                point("monitor/runs", &base, MetricValue::Int(0)),
                point("monitor/errors", &base, MetricValue::Int(0)),
            ]
        );
    }

    #[test]
    fn an_all_error_scorer_writes_no_scorer_points() {
        let r = report(vec![
            vec![
                ScoreResult::check("contains", true, "ok"),
                ScoreResult::error("reference", "judge timed out"),
            ],
            vec![
                ScoreResult::check("contains", true, "ok"),
                ScoreResult::error("reference", "judge timed out"),
            ],
        ]);
        let metrics: Vec<(String, Option<String>)> = monitor_points(&r, None)
            .into_iter()
            .map(|p| (p.metric, p.labels.get("scorer").cloned()))
            .collect();
        assert_eq!(
            metrics,
            [
                ("monitor/runs".to_owned(), None),
                ("monitor/errors".to_owned(), None),
                ("monitor/pass_rate".to_owned(), None),
                (
                    "monitor/scorer_pass_rate".to_owned(),
                    Some("contains".to_owned())
                ),
            ]
        );
    }

    #[test]
    fn a_judge_result_without_a_score_counts_for_the_pass_rate_only() {
        let r = report(vec![vec![ScoreResult::check(
            "llm_judge",
            true,
            "no score",
        )]]);
        let metrics: Vec<String> = monitor_points(&r, None)
            .into_iter()
            .map(|p| p.metric)
            .collect();
        assert_eq!(
            metrics,
            [
                "monitor/runs",
                "monitor/errors",
                "monitor/pass_rate",
                "monitor/scorer_pass_rate",
            ]
        );
    }

    fn check(status: Status) -> CheckResult {
        CheckResult {
            name: "error_rate".into(),
            unit: Unit::Rate,
            baseline: SideValue {
                value: Some(0.01),
                n: 1000,
            },
            rollout: SideValue {
                value: Some(0.02),
                n: 100,
            },
            harm: Some(0.01),
            interval: None,
            z: None,
            status,
            reason: None,
        }
    }

    fn guard(verdict: Verdict, checks: Vec<CheckResult>) -> GuardReport {
        let to = at(1_800_000_000);
        GuardReport {
            spec: "support-guard".into(),
            generated_at: to,
            window: Window {
                from: to - TimeDelta::hours(1),
                to,
            },
            source: "bigquery:p.d.t".into(),
            prompt: "support/answer".into(),
            label: "production".into(),
            baseline: 3,
            rollout: 4,
            percent: 25,
            checks,
            verdict,
            action: Action::None,
            applied: false,
            note: None,
            stats: None,
        }
    }

    #[test]
    fn guard_points_give_the_percent_the_verdict_and_the_worse_checks() {
        let base = [
            ("label", "production"),
            ("prompt", "support/answer"),
            ("spec", "support-guard"),
        ];
        let cases = [
            (Verdict::Advance, vec![check(Status::Ok)], "advance", 0),
            (
                Verdict::Abort,
                vec![
                    check(Status::Worse),
                    check(Status::Ok),
                    check(Status::Worse),
                ],
                "abort",
                2,
            ),
            (
                Verdict::Wait,
                vec![check(Status::Unsure), check(Status::Insufficient)],
                "wait",
                0,
            ),
        ];
        for (verdict, checks, name, worse) in cases {
            let mut with_verdict = base.to_vec();
            with_verdict.push(("verdict", name));
            assert_eq!(
                guard_points(&guard(verdict, checks)),
                vec![
                    point("guard/rollout_percent", &base, MetricValue::Int(25)),
                    point("guard/verdict", &with_verdict, MetricValue::Int(1)),
                    point("guard/worse_checks", &base, MetricValue::Int(worse)),
                ],
                "{name}"
            );
        }
    }

    #[test]
    fn alert_points_skip_insufficient_rules() {
        let run = AlertRun::new(
            "usage",
            at(1_800_000_000),
            vec![
                AlertRuleValue::new("daily-cost", 0.36, Some(true)),
                AlertRuleValue::new("new-errors", 1.0, None),
                AlertRuleValue::new("error_rate", 0.1, Some(false)),
            ],
        );
        let cost = [("rule", "daily-cost"), ("spec", "usage")];
        let errors = [("rule", "error_rate"), ("spec", "usage")];
        assert_eq!(
            alert_points(&run),
            vec![
                point("alerts/rule_value", &cost, MetricValue::Double(0.36)),
                point("alerts/rule_fired", &cost, MetricValue::Int(1)),
                point("alerts/rule_value", &errors, MetricValue::Double(0.1)),
                point("alerts/rule_fired", &errors, MetricValue::Int(0)),
            ]
        );
        let insufficient = AlertRun::new(
            "usage",
            at(1_800_000_000),
            vec![AlertRuleValue::new("new-errors", 1.0, None)],
        );
        assert!(alert_points(&insufficient).is_empty());
    }

    #[test]
    fn long_label_values_are_cut_at_a_char_boundary() {
        let ascii = "a".repeat(2000);
        let p = point("check/ping", &[("spec", &ascii)], MetricValue::Int(1));
        assert_eq!(p.labels["spec"], "a".repeat(1024));
        // One byte, then two-byte characters: byte 1024 falls inside a character.
        let wide = format!("a{}", "é".repeat(1000));
        assert_eq!(wide.len(), 2001);
        let p = point("check/ping", &[("spec", &wide)], MetricValue::Int(1));
        assert_eq!(p.labels["spec"].len(), 1023);
        assert!(wide.starts_with(&p.labels["spec"]));
        let r = MonitorReport {
            spec: "x".repeat(2000),
            ..report(Vec::new())
        };
        for p in monitor_points(&r, Some(&"y".repeat(1500))) {
            assert!(
                p.labels.values().all(|v| v.len() == LABEL_VALUE_MAX),
                "{p:?}"
            );
        }
    }

    #[test]
    fn every_builder_suffix_is_lower_case_words_and_slashes() {
        let suffix = Regex::new("^[a-z_/]+$").unwrap();
        let r = report(vec![vec![
            ScoreResult::check("contains", true, "ok"),
            ScoreResult::judged("llm_judge", true, 4, "good"),
        ]]);
        let run = AlertRun::new(
            "usage",
            at(1_800_000_000),
            vec![AlertRuleValue::new("cost", 0.1, Some(false))],
        );
        let points: Vec<MetricPoint> = monitor_points(&r, None)
            .into_iter()
            .chain(guard_points(&guard(Verdict::Wait, vec![check(Status::Ok)])))
            .chain(alert_points(&run))
            .collect();
        let mut metrics: Vec<&str> = points.iter().map(|p| p.metric.as_str()).collect();
        metrics.dedup();
        assert_eq!(
            metrics,
            [
                "monitor/runs",
                "monitor/errors",
                "monitor/pass_rate",
                "monitor/scorer_pass_rate",
                "monitor/judge_score_mean",
                "guard/rollout_percent",
                "guard/verdict",
                "guard/worse_checks",
                "alerts/rule_value",
                "alerts/rule_fired",
            ]
        );
        for m in metrics {
            assert!(suffix.is_match(m), "{m}");
        }
    }

    const URL: &str = "https://monitoring.googleapis.com/v3/projects/my-proj/timeSeries";

    fn writer(client: Arc<ScriptedClient>) -> CloudMetricsWriter {
        CloudMetricsWriter::with_http(client, Arc::new(NoAuth))
    }

    #[tokio::test]
    async fn the_body_has_one_gauge_series_per_point() {
        let client = ScriptedClient::json("{}");
        let time = DateTime::from_timestamp(1_800_000_000, 123_456_789).unwrap();
        let points = [
            MetricPoint::new(
                "monitor/runs",
                labels(&[("spec", "support"), ("agent", "")]),
                MetricValue::Int(12),
                time,
            ),
            MetricPoint::new(
                "monitor/pass_rate",
                labels(&[("spec", "support"), ("agent", "")]),
                MetricValue::Double(0.75),
                time,
            ),
        ];
        writer(client.clone())
            .write("my-proj", &points)
            .await
            .unwrap();
        assert_eq!(client.hits(), 1);
        let req = client.last_request();
        assert_eq!(req.method, "POST");
        assert_eq!(req.url, URL);
        assert_eq!(req.header("content-type"), "application/json");
        let series = |metric: &str, value_type: &str, value: Value| {
            json!({
                "metric": {
                    "type": format!("custom.googleapis.com/gadk/{metric}"),
                    "labels": {"agent": "", "spec": "support"},
                },
                "resource": {"type": "global", "labels": {"project_id": "my-proj"}},
                "metricKind": "GAUGE",
                "valueType": value_type,
                "points": [{
                    "interval": {"endTime": "2027-01-15T08:00:00.123456789Z"},
                    "value": value,
                }],
            })
        };
        assert_eq!(
            req.json(),
            json!({"timeSeries": [
                series("monitor/runs", "INT64", json!({"int64Value": "12"})),
                series("monitor/pass_rate", "DOUBLE", json!({"doubleValue": 0.75})),
            ]})
        );
    }

    /// `n` points of `check/ping`, one per `spec` label `s0`, `s1`, ….
    fn pings(n: usize) -> Vec<MetricPoint> {
        (0..n)
            .map(|i| {
                MetricPoint::new(
                    "check/ping",
                    labels(&[("spec", &format!("s{i}"))]),
                    MetricValue::Int(1),
                    at(1_800_000_000),
                )
            })
            .collect()
    }

    #[tokio::test]
    async fn points_are_sent_200_series_at_a_time() {
        let client = ScriptedClient::json("{}");
        writer(client.clone())
            .write("my-proj", &pings(450))
            .await
            .unwrap();
        let sizes: Vec<usize> = client
            .requests()
            .iter()
            .map(|r| r.json()["timeSeries"].as_array().unwrap().len())
            .collect();
        assert_eq!(sizes, [200, 200, 50]);
        let last = &client.requests()[2].json()["timeSeries"];
        assert_eq!(last[0]["metric"]["labels"]["spec"], "s400");
        assert_eq!(last[49]["metric"]["labels"]["spec"], "s449");
    }

    #[tokio::test]
    async fn a_failed_chunk_is_summed_up_and_the_rest_are_still_sent() {
        let client = ScriptedClient::sequence(vec![
            FakeResponse::json("{}"),
            FakeResponse::json_status(
                400,
                r#"{"error":{"code":400,"message":"One or more TimeSeries could not be written: timeSeries[0]: write for resource failed: Points must be written in order.","status":"INVALID_ARGUMENT"}}"#,
            ),
            FakeResponse::reset("connection reset"),
        ]);
        let err = writer(client.clone())
            .write("my-proj", &pings(450))
            .await
            .unwrap_err();
        assert_eq!(client.hits(), 3);
        assert_eq!(
            err.to_string(),
            "report store: 250 of 450 series not written: HTTP 400: One or more \
             TimeSeries could not be written: timeSeries[0]: write for resource failed: Points \
             must be written in order."
        );
    }

    #[tokio::test]
    async fn a_transport_error_is_the_error_text() {
        let client = ScriptedClient::always(FakeResponse::reset("connection reset"));
        let err = writer(client)
            .write("my-proj", &pings(1))
            .await
            .unwrap_err();
        let text = err.to_string();
        assert!(
            text.starts_with(&format!("report store: 1 of 1 series not written: {URL}: ")),
            "{text}"
        );
        assert!(text.contains("connection reset"), "{text}");
    }

    #[tokio::test]
    async fn no_points_send_nothing() {
        let client = ScriptedClient::json("{}");
        writer(client.clone()).write("my-proj", &[]).await.unwrap();
        assert_eq!(client.hits(), 0);
    }
}
