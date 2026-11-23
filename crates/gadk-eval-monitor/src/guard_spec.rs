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

//! Guard specs: which prompt rollout to watch, on which production signals, and how strictly.
//!
//! ```yaml
//! name: support-answer-guard        # [A-Za-z0-9_-]{1,64}; names the reports
//! source:                           # BigQuery only
//!   bigquery: { project: my-proj, dataset: agent_analytics, table: agent_events }
//! store: firestore://my-proj/prompts  # the prompt store holding the rollout
//! prompt: support/answer            # the prompt name
//! label: production                 # the label whose rollout is guarded
//! window: 24h                       # <n>m | <n>h | <n>d, at most 30d: calls in [now - window, now)
//! min_calls: 100                    # >= 2, default 100: fewer calls on a side is `insufficient`
//! ramp: [10, 25, 50]                # optional: percents 1-99, strictly increasing
//! checks:                           # at least one; only `feedback` may repeat
//!   - error_rate: { max_increase: 0.01 }          # absolute, 0-1
//!   - feedback:
//!       name: thumbs_up
//!       source: app                 # app | console | any (default any)
//!       max_decrease: 0.05          # absolute for bool scores, relative for number scores, 0-1
//!       min_count: 30               # >= 2, default 30
//!   - cost_per_call: { max_increase: 0.2 }        # relative, >= 0
//!   - latency_p95: { max_increase: 0.25 }         # relative, >= 0
//!   - eval:
//!       scorers:                    # gadk-eval scorer syntax; no reference or equals
//!         - llm_judge: { rubric: "Polite and accurate", min_score: 4 }
//!       max_decrease: 0.05          # absolute pass-rate drop, 0-1
//!       limit: 100                  # 10-500, default 100: runs scored per version
//! judge:                            # required if and only if an eval scorer is llm_judge
//!   model: gemini-3.8-flash
//! ```

use std::path::Path;

use chrono::TimeDelta;
use gadk_eval::ScorerSpec;
use gadk_prompts::{Label, StoreError};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::MonitorError;
use crate::spec::{
    JudgeSpec, SourceSpec, check_keys, is_monitor_name, parse_bigquery, parse_judge, parse_scorer,
    parse_window, required, yaml_value,
};

/// The default `min_calls`.
pub const DEFAULT_MIN_CALLS: u64 = 100;
/// The default `min_count` of a `feedback` check.
pub const DEFAULT_MIN_COUNT: u64 = 30;
/// The default `limit` of an `eval` check.
pub const DEFAULT_EVAL_LIMIT: usize = 100;
/// The fewest scored runs per version an `eval` check compares; fewer is `insufficient`.
pub const MIN_EVAL_RUNS: u64 = 10;

/// The longest `window`.
const MAX_WINDOW_DAYS: i64 = 30;
/// The check kinds, in the order the spec documents them.
const CHECK_KINDS: [&str; 5] = [
    "error_rate",
    "feedback",
    "cost_per_call",
    "latency_p95",
    "eval",
];

/// A validated guard spec.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct GuardSpec {
    /// The guard's name (`[A-Za-z0-9_-]{1,64}`); reports are named after it.
    pub name: String,
    /// Where the calls and feedback come from (always [`SourceSpec::BigQuery`]).
    pub source: SourceSpec,
    /// The prompt-store URL (`file://<dir>` or `firestore://<project>/<database>[/<root>]`).
    pub store: String,
    /// The prompt name.
    pub prompt: String,
    /// The label whose rollout is guarded.
    pub label: Label,
    /// How far back from now calls are counted (at most 30 days).
    pub window: TimeDelta,
    /// The fewest calls per version every check needs (at least 2).
    pub min_calls: u64,
    /// The rollout percents to step through, strictly increasing, each 1-99; empty means
    /// promote on the first `advance`.
    pub ramp: Vec<u8>,
    /// The checks, in spec order (at least one).
    pub checks: Vec<Check>,
    /// The judge model; set exactly when an `eval` check has an `llm_judge` scorer.
    pub judge: Option<JudgeSpec>,
}

/// One check of a guard: a production signal and how much worse the rollout may be.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum Check {
    /// The share of calls that failed.
    ErrorRate {
        /// The largest tolerated increase, absolute (0-1).
        max_increase: f64,
    },
    /// A feedback score recorded for the calls' invocations.
    Feedback {
        /// The score name.
        name: String,
        /// Which recorded scores count.
        source: FeedbackSource,
        /// The largest tolerated decrease (0-1): absolute for bool scores, relative to the
        /// baseline mean for number scores (higher is better).
        max_decrease: f64,
        /// The fewest scores per version (at least 2).
        min_count: u64,
    },
    /// The token cost per call, priced with a price file.
    CostPerCall {
        /// The largest tolerated increase, relative to the baseline (>= 0).
        max_increase: f64,
    },
    /// The 95th percentile of model-call latency.
    LatencyP95 {
        /// The largest tolerated increase, relative to the baseline (>= 0).
        max_increase: f64,
    },
    /// The pass rate of scorers run on each version's recent invocations.
    Eval {
        /// The scorers (never `reference` or `equals`).
        scorers: Vec<ScorerSpec>,
        /// The largest tolerated pass-rate decrease, absolute (0-1).
        max_decrease: f64,
        /// The most invocations scored per version (10-500).
        limit: usize,
    },
}

impl Check {
    /// The check kind as written in YAML: `error_rate`, `feedback`, `cost_per_call`,
    /// `latency_p95` or `eval`.
    pub fn kind(&self) -> &'static str {
        match self {
            Check::ErrorRate { .. } => "error_rate",
            Check::Feedback { .. } => "feedback",
            Check::CostPerCall { .. } => "cost_per_call",
            Check::LatencyP95 { .. } => "latency_p95",
            Check::Eval { .. } => "eval",
        }
    }
}

/// Which feedback scores a `feedback` check counts, by the `attributes.source` of the row.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FeedbackSource {
    /// Scores recorded by the application.
    App,
    /// Scores recorded in the console.
    Console,
    /// Both; each source's latest score of an invocation counts.
    Any,
}

impl FeedbackSource {
    /// The source as written in YAML: `app`, `console` or `any`.
    pub fn as_str(&self) -> &'static str {
        match self {
            FeedbackSource::App => "app",
            FeedbackSource::Console => "console",
            FeedbackSource::Any => "any",
        }
    }
}

impl GuardSpec {
    /// Reads and validates the guard spec at `path`.
    ///
    /// # Errors
    ///
    /// [`MonitorError::Spec`] (`guard spec: …`) when the file cannot be read or the spec is
    /// invalid.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, MonitorError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|e| {
            MonitorError::Spec(format!("guard spec: reading {}: {e}", path.display()))
        })?;
        Self::from_yaml(&text)
    }

    /// Parses and validates a guard spec from YAML text.
    ///
    /// # Errors
    ///
    /// [`MonitorError::Spec`] (`guard spec: …`) naming the offending key.
    pub fn from_yaml(yaml: &str) -> Result<Self, MonitorError> {
        yaml_value(yaml)
            .and_then(|doc| parse(&doc))
            .map_err(|detail| MonitorError::Spec(format!("guard spec: {detail}")))
    }

    /// Whether an `eval` check has an `llm_judge` scorer (and so a judge model is needed).
    pub fn wants_judge(&self) -> bool {
        self.checks.iter().any(|c| match c {
            Check::Eval { scorers, .. } => scorers
                .iter()
                .any(|s| matches!(s, ScorerSpec::LlmJudge { .. })),
            _ => false,
        })
    }
}

fn parse(doc: &Value) -> Result<GuardSpec, String> {
    let top = doc.as_object().ok_or("the document must be a mapping")?;
    check_keys(
        top,
        "",
        &[
            "name",
            "source",
            "store",
            "prompt",
            "label",
            "window",
            "min_calls",
            "ramp",
            "checks",
            "judge",
        ],
    )?;
    let name = required(
        top,
        "name",
        is_monitor_name,
        "`name` must match [A-Za-z0-9_-]{1,64}",
    )?;
    let source = parse_source(top.get("source"))?;
    let store = required(
        top,
        "store",
        |s| !s.trim().is_empty(),
        "`store` must be a non-empty prompt-store URL",
    )?;
    let prompt = required(
        top,
        "prompt",
        |s| !s.trim().is_empty(),
        "`prompt` must be a non-empty string",
    )?;
    let label = top
        .get("label")
        .and_then(Value::as_str)
        .ok_or("`label` must be a string")?;
    let label = Label::new(label).map_err(|e| match e {
        StoreError::Invalid(m) => format!("`label`: {m}"),
        other => format!("`label`: {other}"),
    })?;
    let window = top
        .get("window")
        .and_then(Value::as_str)
        .and_then(parse_window)
        .ok_or("`window` must be <n>m, <n>h or <n>d with n >= 1")?;
    if window > TimeDelta::days(MAX_WINDOW_DAYS) {
        return Err(format!("`window` must be at most {MAX_WINDOW_DAYS}d"));
    }
    let min_calls = match top.get("min_calls") {
        None | Some(Value::Null) => DEFAULT_MIN_CALLS,
        Some(v) => v
            .as_u64()
            .filter(|n| *n >= 2)
            .ok_or("`min_calls` must be an integer of at least 2")?,
    };
    let ramp = parse_ramp(top.get("ramp"))?;
    let checks = match top.get("checks") {
        Some(Value::Array(a)) if !a.is_empty() => a
            .iter()
            .enumerate()
            .map(|(i, v)| parse_check(i + 1, v))
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err("`checks` must be a non-empty list".into()),
    };
    check_repeats(&checks)?;
    let mut spec = GuardSpec {
        name,
        source,
        store,
        prompt,
        label,
        window,
        min_calls,
        ramp,
        checks,
        judge: None,
    };
    spec.judge = match (top.get("judge"), spec.wants_judge()) {
        (None | Some(Value::Null), false) => None,
        (None | Some(Value::Null), true) => {
            return Err(
                "`judge` is required when an `eval` check has an `llm_judge` scorer".into(),
            );
        }
        (Some(_), false) => {
            return Err("`judge` is set but no `eval` check has an `llm_judge` scorer".into());
        }
        (Some(v), true) => Some(parse_judge(v, spec.source.project())?),
    };
    Ok(spec)
}

/// `{ bigquery: { project, dataset, table } }`; Cloud Trace sources are refused.
fn parse_source(v: Option<&Value>) -> Result<SourceSpec, String> {
    let m = v
        .and_then(Value::as_object)
        .ok_or("`source` must be a mapping with `bigquery`")?;
    if m.contains_key("cloud_trace") {
        return Err(
            "`source.cloud_trace` is not supported by guards; use `source.bigquery`".into(),
        );
    }
    check_keys(m, "`source`: ", &["bigquery"])?;
    let bq = m
        .get("bigquery")
        .ok_or("`source` must be a mapping with `bigquery`")?;
    parse_bigquery(bq)
}

/// The optional `ramp`: a non-empty list of strictly increasing percents 1-99.
fn parse_ramp(v: Option<&Value>) -> Result<Vec<u8>, String> {
    const MESSAGE: &str =
        "`ramp` must be a non-empty list of percents from 1 to 99, strictly increasing";
    let items = match v {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(a)) if !a.is_empty() => a,
        Some(_) => return Err(MESSAGE.into()),
    };
    let mut ramp: Vec<u8> = Vec::with_capacity(items.len());
    for item in items {
        let p = item
            .as_u64()
            .filter(|p| (1..=99).contains(p))
            .and_then(|p| u8::try_from(p).ok())
            .ok_or(MESSAGE)?;
        if ramp.last().is_some_and(|last| *last >= p) {
            return Err(MESSAGE.into());
        }
        ramp.push(p);
    }
    Ok(ramp)
}

/// Check `i` (1-based): a mapping with one key, the kind, whose value maps the check's keys.
fn parse_check(i: usize, v: &Value) -> Result<Check, String> {
    let at = format!("checks[{i}]");
    let (kind, params) = match v.as_object().map(|m| (m.len(), m.iter().next())) {
        Some((1, Some(entry))) => entry,
        _ => {
            return Err(format!(
                "`{at}` must be a mapping with one key: error_rate, feedback, cost_per_call, \
                 latency_p95 or eval"
            ));
        }
    };
    if !CHECK_KINDS.contains(&kind.as_str()) {
        return Err(format!(
            "`{at}`: unknown check `{kind}` (expected one of: {})",
            CHECK_KINDS.join(", ")
        ));
    }
    let at = format!("{at}.{kind}");
    let m = params
        .as_object()
        .ok_or_else(|| format!("`{at}` must be a mapping"))?;
    match kind.as_str() {
        "error_rate" => {
            check_keys(m, &format!("`{at}`: "), &["max_increase"])?;
            Ok(Check::ErrorRate {
                max_increase: fraction(m, "max_increase", &at)?,
            })
        }
        "feedback" => {
            check_keys(
                m,
                &format!("`{at}`: "),
                &["name", "source", "max_decrease", "min_count"],
            )?;
            let name = required(
                m,
                "name",
                |s| !s.trim().is_empty(),
                &format!("`{at}.name` must be a non-empty string"),
            )?;
            let source = match m.get("source") {
                None | Some(Value::Null) => FeedbackSource::Any,
                Some(v) => match v.as_str() {
                    Some("app") => FeedbackSource::App,
                    Some("console") => FeedbackSource::Console,
                    Some("any") => FeedbackSource::Any,
                    _ => return Err(format!("`{at}.source` must be app, console or any")),
                },
            };
            let max_decrease = fraction(m, "max_decrease", &at)?;
            let min_count = match m.get("min_count") {
                None | Some(Value::Null) => DEFAULT_MIN_COUNT,
                Some(v) => v
                    .as_u64()
                    .filter(|n| *n >= 2)
                    .ok_or_else(|| format!("`{at}.min_count` must be an integer of at least 2"))?,
            };
            Ok(Check::Feedback {
                name,
                source,
                max_decrease,
                min_count,
            })
        }
        "cost_per_call" => {
            check_keys(m, &format!("`{at}`: "), &["max_increase"])?;
            Ok(Check::CostPerCall {
                max_increase: non_negative(m, "max_increase", &at)?,
            })
        }
        "latency_p95" => {
            check_keys(m, &format!("`{at}`: "), &["max_increase"])?;
            Ok(Check::LatencyP95 {
                max_increase: non_negative(m, "max_increase", &at)?,
            })
        }
        _ => {
            check_keys(
                m,
                &format!("`{at}`: "),
                &["scorers", "max_decrease", "limit"],
            )?;
            let scorers = match m.get("scorers") {
                Some(Value::Array(a)) if !a.is_empty() => a
                    .iter()
                    .enumerate()
                    .map(|(j, v)| parse_scorer(j + 1, v).map_err(|e| format!("`{at}`: {e}")))
                    .collect::<Result<Vec<_>, _>>()?,
                _ => return Err(format!("`{at}.scorers` must be a non-empty list")),
            };
            let max_decrease = fraction(m, "max_decrease", &at)?;
            let limit = match m.get("limit") {
                None | Some(Value::Null) => DEFAULT_EVAL_LIMIT,
                Some(v) => v
                    .as_u64()
                    .and_then(|n| usize::try_from(n).ok())
                    .filter(|n| (10..=500).contains(n))
                    .ok_or_else(|| format!("`{at}.limit` must be an integer from 10 to 500"))?,
            };
            Ok(Check::Eval {
                scorers,
                max_decrease,
                limit,
            })
        }
    }
}

/// The required number at `<at>.<key>`, from 0 to 1.
fn fraction(m: &Map<String, Value>, key: &str, at: &str) -> Result<f64, String> {
    m.get(key)
        .and_then(Value::as_f64)
        .filter(|x| (0.0..=1.0).contains(x))
        .ok_or_else(|| format!("`{at}.{key}` must be a number from 0 to 1"))
}

/// The required finite number at `<at>.<key>`, at least 0.
fn non_negative(m: &Map<String, Value>, key: &str, at: &str) -> Result<f64, String> {
    m.get(key)
        .and_then(Value::as_f64)
        .filter(|x| x.is_finite() && *x >= 0.0)
        .ok_or_else(|| format!("`{at}.{key}` must be a number >= 0"))
}

/// `Err` naming the first check that repeats a kind (other than `feedback`) or a feedback
/// `name` and `source`.
fn check_repeats(checks: &[Check]) -> Result<(), String> {
    for (i, c) in checks.iter().enumerate() {
        let earlier = &checks[..i];
        match c {
            Check::Feedback { name, source, .. } => {
                let repeated = earlier.iter().any(|e| {
                    matches!(e, Check::Feedback { name: n, source: s, .. } if n == name && s == source)
                });
                if repeated {
                    return Err(format!(
                        "`checks[{}]`: feedback `{name}` from `{}` is listed twice",
                        i + 1,
                        source.as_str()
                    ));
                }
            }
            _ => {
                if earlier.iter().any(|e| e.kind() == c.kind()) {
                    return Err(format!(
                        "`checks[{}]`: `{}` is listed twice (only `feedback` may repeat)",
                        i + 1,
                        c.kind()
                    ));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    const FULL: &str = r#"
name: support-answer-guard
source:
  bigquery: { project: my-proj, dataset: agent_analytics, table: agent_events }
store: firestore://my-proj/prompts
prompt: support/answer
label: production
window: 24h
min_calls: 200
ramp: [10, 25, 50]
checks:
  - error_rate: { max_increase: 0.01 }
  - feedback:
      name: thumbs_up
      source: app
      max_decrease: 0.05
      min_count: 40
  - feedback: { name: thumbs_up, source: console, max_decrease: 0.1 }
  - cost_per_call: { max_increase: 0.2 }
  - latency_p95: { max_increase: 0.25 }
  - eval:
      scorers:
        - not_contains: "I cannot help"
        - llm_judge: { rubric: "Polite and accurate", min_score: 4 }
      max_decrease: 0.05
      limit: 50
judge:
  model: gemini-3.8-flash
  location: us
  timeout: 60s
"#;

    /// The smallest valid spec, with `line` (`key: value`) replacing the line of that key or
    /// appended when no line has that key; an empty value removes the key.
    fn with(line: &str) -> String {
        let (key, value) = line.split_once(':').expect("key: value");
        let mut lines: Vec<String> = [
            "name: g",
            "source: { bigquery: { project: p, dataset: d, table: t } }",
            "store: file:///tmp/prompts",
            "prompt: support/answer",
            "label: production",
            "window: 1h",
            "checks: [error_rate: { max_increase: 0.02 }]",
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
        match GuardSpec::from_yaml(yaml) {
            Err(e @ MonitorError::Spec(_)) => e.to_string(),
            other => panic!("expected a spec error, got {other:?}"),
        }
    }

    /// The printed error of a spec whose `checks` is `checks`.
    fn check_error(checks: &str) -> String {
        printed(&with(&format!("checks: {checks}")))
    }

    #[test]
    fn parses_a_full_spec() {
        let s = GuardSpec::from_yaml(FULL).unwrap();
        assert_eq!(s.name, "support-answer-guard");
        assert_eq!(
            s.source,
            SourceSpec::BigQuery {
                project: "my-proj".into(),
                dataset: "agent_analytics".into(),
                table: "agent_events".into(),
            }
        );
        assert_eq!(s.store, "firestore://my-proj/prompts");
        assert_eq!(s.prompt, "support/answer");
        assert_eq!(s.label, Label::production());
        assert_eq!(s.window, TimeDelta::hours(24));
        assert_eq!(s.min_calls, 200);
        assert_eq!(s.ramp, [10, 25, 50]);
        let kinds: Vec<_> = s.checks.iter().map(Check::kind).collect();
        assert_eq!(
            kinds,
            [
                "error_rate",
                "feedback",
                "feedback",
                "cost_per_call",
                "latency_p95",
                "eval"
            ]
        );
        assert!(matches!(s.checks[0], Check::ErrorRate { max_increase } if max_increase == 0.01));
        assert!(matches!(
            &s.checks[1],
            Check::Feedback { name, source: FeedbackSource::App, max_decrease, min_count: 40 }
                if name == "thumbs_up" && *max_decrease == 0.05
        ));
        assert!(matches!(
            &s.checks[2],
            Check::Feedback { name, source: FeedbackSource::Console, max_decrease, min_count: DEFAULT_MIN_COUNT }
                if name == "thumbs_up" && *max_decrease == 0.1
        ));
        assert!(matches!(s.checks[3], Check::CostPerCall { max_increase } if max_increase == 0.2));
        assert!(matches!(s.checks[4], Check::LatencyP95 { max_increase } if max_increase == 0.25));
        let Check::Eval {
            scorers,
            max_decrease,
            limit,
        } = &s.checks[5]
        else {
            panic!("expected an eval check, got {:?}", s.checks[5]);
        };
        let scorer_kinds: Vec<_> = scorers.iter().map(ScorerSpec::kind).collect();
        assert_eq!(scorer_kinds, ["not_contains", "llm_judge"]);
        assert_eq!(*max_decrease, 0.05);
        assert_eq!(*limit, 50);
        assert!(s.wants_judge());
        assert_eq!(
            s.judge,
            Some(JudgeSpec {
                model: "gemini-3.8-flash".into(),
                project: "my-proj".into(),
                location: "us".into(),
                timeout: Duration::from_secs(60),
            })
        );
    }

    #[test]
    fn applies_every_default() {
        let s = GuardSpec::from_yaml(&with(
            "checks: [feedback: { name: thumbs_up, max_decrease: 0.05 }, \
             eval: { scorers: [contains: x], max_decrease: 0.1 }]",
        ))
        .unwrap();
        assert_eq!(s.min_calls, DEFAULT_MIN_CALLS);
        assert_eq!(DEFAULT_MIN_CALLS, 100);
        assert!(s.ramp.is_empty());
        assert!(s.judge.is_none());
        assert!(!s.wants_judge());
        assert!(matches!(
            &s.checks[0],
            Check::Feedback { source: FeedbackSource::Any, min_count, .. } if *min_count == DEFAULT_MIN_COUNT
        ));
        assert_eq!(DEFAULT_MIN_COUNT, 30);
        assert!(matches!(&s.checks[1], Check::Eval { limit, .. } if *limit == DEFAULT_EVAL_LIMIT));
        assert_eq!(DEFAULT_EVAL_LIMIT, 100);
        assert_eq!(MIN_EVAL_RUNS, 10);
        let s = GuardSpec::from_yaml(&with(
            "checks: [eval: { scorers: [llm_judge: be nice], max_decrease: 0.1 }]\n\
             judge: { model: gemini-3.8-flash }",
        ))
        .unwrap();
        assert_eq!(
            s.judge,
            Some(JudgeSpec {
                model: "gemini-3.8-flash".into(),
                project: "p".into(),
                location: crate::spec::DEFAULT_JUDGE_LOCATION.into(),
                timeout: crate::spec::DEFAULT_JUDGE_TIMEOUT,
            })
        );
    }

    #[test]
    fn accepts_the_bounds() {
        let s = GuardSpec::from_yaml(&with("window: 30d")).unwrap();
        assert_eq!(s.window, TimeDelta::days(30));
        let s = GuardSpec::from_yaml(&with("window: 720h")).unwrap();
        assert_eq!(s.window, TimeDelta::days(30));
        let s = GuardSpec::from_yaml(&with("min_calls: 2")).unwrap();
        assert_eq!(s.min_calls, 2);
        let s = GuardSpec::from_yaml(&with("ramp: [1, 99]")).unwrap();
        assert_eq!(s.ramp, [1, 99]);
        let s = GuardSpec::from_yaml(&with(
            "checks: [error_rate: { max_increase: 0 }, \
             feedback: { name: csat, source: any, max_decrease: 1, min_count: 2 }, \
             cost_per_call: { max_increase: 0 }, latency_p95: { max_increase: 3.5 }, \
             eval: { scorers: [contains: x], max_decrease: 1.0, limit: 10 }]",
        ))
        .unwrap();
        assert!(matches!(s.checks[0], Check::ErrorRate { max_increase } if max_increase == 0.0));
        assert!(matches!(
            s.checks[1],
            Check::Feedback { max_decrease, min_count: 2, .. } if max_decrease == 1.0
        ));
        assert!(matches!(s.checks[2], Check::CostPerCall { max_increase } if max_increase == 0.0));
        assert!(matches!(s.checks[3], Check::LatencyP95 { max_increase } if max_increase == 3.5));
        assert!(matches!(s.checks[4], Check::Eval { limit: 10, .. }));
        let s = GuardSpec::from_yaml(&with(
            "checks: [eval: { scorers: [contains: x], max_decrease: 0, limit: 500 }]",
        ))
        .unwrap();
        assert!(matches!(s.checks[0], Check::Eval { limit: 500, .. }));
    }

    #[test]
    fn rejects_an_unknown_key() {
        assert_eq!(
            printed(&with("rollout: 10")),
            "monitor spec: guard spec: unknown key `rollout` (expected one of: name, source, \
             store, prompt, label, window, min_calls, ramp, checks, judge)"
        );
    }

    #[test]
    fn rejects_a_bad_name() {
        assert_eq!(
            printed(&with("name: \"has space\"")),
            "monitor spec: guard spec: `name` must match [A-Za-z0-9_-]{1,64}"
        );
        assert_eq!(
            printed(&with(&format!("name: {}", "a".repeat(65)))),
            "monitor spec: guard spec: `name` must match [A-Za-z0-9_-]{1,64}"
        );
    }

    #[test]
    fn rejects_a_bad_label() {
        assert_eq!(
            printed(&with("label: Prod")),
            "monitor spec: guard spec: `label`: label \"Prod\": must match [a-z0-9][a-z0-9-]{0,62}"
        );
        assert_eq!(
            printed(&with("label: ")),
            "monitor spec: guard spec: `label` must be a string"
        );
    }

    #[test]
    fn rejects_a_bad_window() {
        for window in ["0h", "5s", "h", "\"\""] {
            assert_eq!(
                printed(&with(&format!("window: {window}"))),
                "monitor spec: guard spec: `window` must be <n>m, <n>h or <n>d with n >= 1",
                "{window}"
            );
        }
    }

    #[test]
    fn rejects_a_window_over_30_days() {
        for window in ["31d", "721h", "43201m"] {
            assert_eq!(
                printed(&with(&format!("window: {window}"))),
                "monitor spec: guard spec: `window` must be at most 30d",
                "{window}"
            );
        }
    }

    #[test]
    fn rejects_min_calls_below_2() {
        for v in ["1", "0", "-3", "ten"] {
            assert_eq!(
                printed(&with(&format!("min_calls: {v}"))),
                "monitor spec: guard spec: `min_calls` must be an integer of at least 2",
                "{v}"
            );
        }
    }

    #[test]
    fn rejects_a_bad_ramp() {
        for ramp in [
            "[]",
            "[0, 10]",
            "[10, 100]",
            "[25, 10]",
            "[10, 10]",
            "[1.5]",
            "50",
        ] {
            assert_eq!(
                printed(&with(&format!("ramp: {ramp}"))),
                "monitor spec: guard spec: `ramp` must be a non-empty list of percents from 1 \
                 to 99, strictly increasing",
                "{ramp}"
            );
        }
    }

    #[test]
    fn rejects_empty_checks() {
        for checks in ["[]", "{}", "error_rate"] {
            assert_eq!(
                check_error(checks),
                "monitor spec: guard spec: `checks` must be a non-empty list",
                "{checks}"
            );
        }
        assert_eq!(
            printed(&with("checks: ")),
            "monitor spec: guard spec: `checks` must be a non-empty list"
        );
    }

    #[test]
    fn rejects_a_repeated_check_kind_other_than_feedback() {
        assert_eq!(
            check_error(
                "[error_rate: { max_increase: 0.01 }, latency_p95: { max_increase: 0.2 }, \
                 error_rate: { max_increase: 0.02 }]"
            ),
            "monitor spec: guard spec: `checks[3]`: `error_rate` is listed twice (only \
             `feedback` may repeat)"
        );
    }

    #[test]
    fn rejects_a_repeated_feedback_name_and_source() {
        assert_eq!(
            check_error(
                "[feedback: { name: thumbs_up, max_decrease: 0.1 }, \
                 feedback: { name: thumbs_up, source: app, max_decrease: 0.1 }, \
                 feedback: { name: stars, source: any, max_decrease: 0.1 }, \
                 feedback: { name: thumbs_up, source: any, max_decrease: 0.2 }]"
            ),
            "monitor spec: guard spec: `checks[4]`: feedback `thumbs_up` from `any` is listed twice"
        );
    }

    #[test]
    fn rejects_a_missing_judge() {
        assert_eq!(
            check_error(
                "[eval: { scorers: [contains: x, llm_judge: be nice], max_decrease: 0.1 }]"
            ),
            "monitor spec: guard spec: `judge` is required when an `eval` check has an \
             `llm_judge` scorer"
        );
    }

    #[test]
    fn rejects_a_judge_that_is_not_needed() {
        assert_eq!(
            printed(&with("judge: { model: gemini-3.8-flash }")),
            "monitor spec: guard spec: `judge` is set but no `eval` check has an `llm_judge` \
             scorer"
        );
        assert_eq!(
            check_error(
                "[eval: { scorers: [contains: x], max_decrease: 0.1 }]\njudge: { model: m }"
            ),
            "monitor spec: guard spec: `judge` is set but no `eval` check has an `llm_judge` \
             scorer"
        );
    }

    #[test]
    fn rejects_bad_judge_fields() {
        assert_eq!(
            check_error(
                "[eval: { scorers: [llm_judge: be nice], max_decrease: 0.1 }]\n\
                 judge: { model: m, timeout: 0s }"
            ),
            "monitor spec: guard spec: `judge.timeout` must be <n>s or <n>m with n >= 1"
        );
    }

    #[test]
    fn rejects_a_cloud_trace_source() {
        assert_eq!(
            printed(&with("source: { cloud_trace: { project: p } }")),
            "monitor spec: guard spec: `source.cloud_trace` is not supported by guards; use \
             `source.bigquery`"
        );
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
                format!("monitor spec: guard spec: {detail}"),
                "{source}"
            );
        }
    }

    #[test]
    fn rejects_a_bad_store_and_prompt() {
        for (line, detail) in [
            ("store: ", "`store` must be a non-empty prompt-store URL"),
            (
                "store: \" \"",
                "`store` must be a non-empty prompt-store URL",
            ),
            ("store: 5", "`store` must be a non-empty prompt-store URL"),
            ("prompt: ", "`prompt` must be a non-empty string"),
            ("prompt: \"\"", "`prompt` must be a non-empty string"),
        ] {
            assert_eq!(
                printed(&with(line)),
                format!("monitor spec: guard spec: {detail}"),
                "{line}"
            );
        }
    }

    #[test]
    fn rejects_malformed_checks() {
        for (checks, detail) in [
            (
                "[bogus: {}]",
                "`checks[1]`: unknown check `bogus` (expected one of: error_rate, feedback, \
                 cost_per_call, latency_p95, eval)",
            ),
            (
                "[error_rate]",
                "`checks[1]` must be a mapping with one key: error_rate, feedback, \
                 cost_per_call, latency_p95 or eval",
            ),
            (
                "[{ error_rate: { max_increase: 0.1 }, latency_p95: { max_increase: 0.1 } }]",
                "`checks[1]` must be a mapping with one key: error_rate, feedback, \
                 cost_per_call, latency_p95 or eval",
            ),
            (
                "[error_rate: 0.1]",
                "`checks[1].error_rate` must be a mapping",
            ),
        ] {
            assert_eq!(
                check_error(checks),
                format!("monitor spec: guard spec: {detail}"),
                "{checks}"
            );
        }
    }

    #[test]
    fn error_rate_keys_and_bounds() {
        for (params, detail) in [
            (
                "{}",
                "`checks[1].error_rate.max_increase` must be a number from 0 to 1",
            ),
            (
                "{ max_increase: -0.01 }",
                "`checks[1].error_rate.max_increase` must be a number from 0 to 1",
            ),
            (
                "{ max_increase: 1.5 }",
                "`checks[1].error_rate.max_increase` must be a number from 0 to 1",
            ),
            (
                "{ max_increase: high }",
                "`checks[1].error_rate.max_increase` must be a number from 0 to 1",
            ),
            (
                "{ max_increase: 0.1, max_decrease: 0.1 }",
                "`checks[1].error_rate`: unknown key `max_decrease` (expected one of: \
                 max_increase)",
            ),
        ] {
            assert_eq!(
                check_error(&format!("[error_rate: {params}]")),
                format!("monitor spec: guard spec: {detail}"),
                "{params}"
            );
        }
    }

    #[test]
    fn feedback_keys_and_bounds() {
        for (params, detail) in [
            (
                "{ max_decrease: 0.1 }",
                "`checks[1].feedback.name` must be a non-empty string",
            ),
            (
                "{ name: \" \", max_decrease: 0.1 }",
                "`checks[1].feedback.name` must be a non-empty string",
            ),
            (
                "{ name: csat, source: user, max_decrease: 0.1 }",
                "`checks[1].feedback.source` must be app, console or any",
            ),
            (
                "{ name: csat }",
                "`checks[1].feedback.max_decrease` must be a number from 0 to 1",
            ),
            (
                "{ name: csat, max_decrease: 1.01 }",
                "`checks[1].feedback.max_decrease` must be a number from 0 to 1",
            ),
            (
                "{ name: csat, max_decrease: 0.1, min_count: 1 }",
                "`checks[1].feedback.min_count` must be an integer of at least 2",
            ),
            (
                "{ name: csat, max_decrease: 0.1, min_count: 2.5 }",
                "`checks[1].feedback.min_count` must be an integer of at least 2",
            ),
            (
                "{ name: csat, max_decrease: 0.1, data_type: bool }",
                "`checks[1].feedback`: unknown key `data_type` (expected one of: name, source, \
                 max_decrease, min_count)",
            ),
        ] {
            assert_eq!(
                check_error(&format!("[feedback: {params}]")),
                format!("monitor spec: guard spec: {detail}"),
                "{params}"
            );
        }
    }

    #[test]
    fn cost_and_latency_keys_and_bounds() {
        for kind in ["cost_per_call", "latency_p95"] {
            for (params, detail) in [
                (
                    "{}".to_owned(),
                    format!("`checks[1].{kind}.max_increase` must be a number >= 0"),
                ),
                (
                    "{ max_increase: -0.1 }".to_owned(),
                    format!("`checks[1].{kind}.max_increase` must be a number >= 0"),
                ),
                (
                    "{ max_increase: 0.1, min_count: 5 }".to_owned(),
                    format!(
                        "`checks[1].{kind}`: unknown key `min_count` (expected one of: \
                         max_increase)"
                    ),
                ),
            ] {
                assert_eq!(
                    check_error(&format!("[{kind}: {params}]")),
                    format!("monitor spec: guard spec: {detail}"),
                    "{kind} {params}"
                );
            }
        }
    }

    #[test]
    fn eval_keys_and_bounds() {
        for (params, detail) in [
            (
                "{ max_decrease: 0.1 }",
                "`checks[1].eval.scorers` must be a non-empty list",
            ),
            (
                "{ scorers: [], max_decrease: 0.1 }",
                "`checks[1].eval.scorers` must be a non-empty list",
            ),
            (
                "{ scorers: [contains: x] }",
                "`checks[1].eval.max_decrease` must be a number from 0 to 1",
            ),
            (
                "{ scorers: [contains: x], max_decrease: 0.1, limit: 9 }",
                "`checks[1].eval.limit` must be an integer from 10 to 500",
            ),
            (
                "{ scorers: [contains: x], max_decrease: 0.1, limit: 501 }",
                "`checks[1].eval.limit` must be an integer from 10 to 500",
            ),
            (
                "{ scorers: [contains: x], max_decrease: 0.1, agent: a }",
                "`checks[1].eval`: unknown key `agent` (expected one of: scorers, max_decrease, \
                 limit)",
            ),
            (
                "{ scorers: [bogus: x], max_decrease: 0.1 }",
                "`checks[1].eval`: scorers[1]: unknown scorer `bogus`",
            ),
        ] {
            let msg = check_error(&format!("[eval: {params}]"));
            assert!(
                msg.starts_with(&format!("monitor spec: guard spec: {detail}")),
                "{params}\n=> {msg}"
            );
        }
    }

    #[test]
    fn eval_scorers_follow_the_monitor_rules() {
        assert_eq!(
            check_error(
                "[eval: { scorers: [contains: x, reference: 'the answer'], max_decrease: 0.1 }]"
            ),
            "monitor spec: guard spec: `checks[1].eval`: scorers[2]: `reference` needs a \
             reference answer, which production runs do not have"
        );
        assert_eq!(
            check_error("[eval: { scorers: [equals: 'the answer'], max_decrease: 0.1 }]"),
            "monitor spec: guard spec: `checks[1].eval`: scorers[1]: `equals` needs a \
             reference answer, which production runs do not have"
        );
    }

    #[test]
    fn feedback_source_names() {
        assert_eq!(FeedbackSource::App.as_str(), "app");
        assert_eq!(FeedbackSource::Console.as_str(), "console");
        assert_eq!(FeedbackSource::Any.as_str(), "any");
        assert_eq!(
            serde_json::to_string(&FeedbackSource::Console).unwrap(),
            "\"console\""
        );
        assert_eq!(
            serde_json::from_str::<FeedbackSource>("\"any\"").unwrap(),
            FeedbackSource::Any
        );
    }

    #[test]
    fn the_example_spec_parses() {
        let s = GuardSpec::from_yaml(include_str!("../examples/guard.yaml")).unwrap();
        assert_eq!(s.name, "support-answer-guard");
        let kinds: Vec<&str> = s.checks.iter().map(Check::kind).collect();
        assert_eq!(
            kinds,
            [
                "error_rate",
                "feedback",
                "feedback",
                "cost_per_call",
                "latency_p95",
                "eval"
            ]
        );
        assert!(s.wants_judge());
        assert_eq!(
            s.judge.map(|j| j.model).as_deref(),
            Some("gemini-3.8-flash")
        );
    }

    #[test]
    fn load_reads_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("g.yaml");
        std::fs::write(&p, FULL).unwrap();
        assert_eq!(GuardSpec::load(&p).unwrap().name, "support-answer-guard");
        let missing = dir.path().join("missing.yaml");
        let msg = GuardSpec::load(&missing).unwrap_err().to_string();
        assert!(
            msg.starts_with(&format!(
                "monitor spec: guard spec: reading {}: ",
                missing.display()
            )),
            "{msg}"
        );
        let msg = GuardSpec::from_yaml("name: [").unwrap_err().to_string();
        assert!(
            msg.starts_with("monitor spec: guard spec: invalid YAML"),
            "{msg}"
        );
        assert_eq!(
            GuardSpec::from_yaml("- a").unwrap_err().to_string(),
            "monitor spec: guard spec: the document must be a mapping"
        );
    }
}
