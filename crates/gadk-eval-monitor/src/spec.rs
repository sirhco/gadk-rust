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

//! Monitor specs: which production runs to grade, and how.
//!
//! ```yaml
//! name: support-bot-prod            # [A-Za-z0-9_-]{1,64}; names the reports
//! source:
//!   bigquery: { project: my-proj, dataset: agent_analytics, table: agent_events }
//!   # or, from spans in Cloud Trace: cloud_trace: { project: my-proj }
//! window: 24h                       # <n>m | <n>h | <n>d: runs started in [now - window, now)
//! agent: support_bot                # optional: only runs of this root agent
//! limit: 100                        # 1-1000, default 100: the newest runs in the window
//! min_pass_rate: 0.9                # optional, 0.0-1.0
//! judge:                            # required if and only if a scorer is llm_judge
//!   model: gemini-2.5-flash
//!   project: my-proj                # default: the source project
//!   location: us-central1           # default us-central1
//!   timeout: 120s                   # <n>s | <n>m, default 120s
//! scorers:                          # gadk-eval scorer syntax; no reference or equals
//!   - not_contains: "I cannot help"
//!   - tools: [lookup_order]
//!   - llm_judge: { rubric: "Polite and accurate", min_score: 4 }
//! ```

use std::path::Path;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use gadk_eval::{EvalError, ScorerSpec};
use serde_json::{Map, Value};

use crate::error::MonitorError;
use crate::source::TranscriptQuery;

/// The default `limit`.
pub const DEFAULT_LIMIT: usize = 100;
/// The largest `limit`.
pub const MAX_LIMIT: usize = 1000;
/// The default `judge.location`.
pub const DEFAULT_JUDGE_LOCATION: &str = "us-central1";
/// The default `judge.timeout`.
pub const DEFAULT_JUDGE_TIMEOUT: Duration = Duration::from_secs(120);

/// A validated monitor spec.
#[derive(Debug, Clone)]
pub struct MonitorSpec {
    /// The monitor's name (`[A-Za-z0-9_-]{1,64}`); reports are named after it.
    pub name: String,
    /// Where the runs come from.
    pub source: SourceSpec,
    /// How far back from now runs are listed.
    pub window: TimeDelta,
    /// Only runs of this root agent, when set.
    pub agent: Option<String>,
    /// The most runs graded (1-1000).
    pub limit: usize,
    /// The pass rate below which the run fails (0.0-1.0), when set.
    pub min_pass_rate: Option<f64>,
    /// The judge model; set exactly when a scorer is `llm_judge`.
    pub judge: Option<JudgeSpec>,
    /// The scorers applied to every run (never `reference` or `equals`).
    pub scorers: Vec<ScorerSpec>,
}

/// Where a monitor's runs come from. More sources may be added.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SourceSpec {
    /// An agent-analytics table in BigQuery.
    BigQuery {
        /// The Google Cloud project (`[A-Za-z0-9_-]+`).
        project: String,
        /// The dataset (`[A-Za-z0-9_]+`).
        dataset: String,
        /// The table (`[A-Za-z0-9_]+`).
        table: String,
    },
    /// The spans an agent exports to Cloud Trace (with message content on spans).
    CloudTrace {
        /// The Google Cloud project the traces are in (`[A-Za-z0-9_-]+`).
        project: String,
    },
}

impl SourceSpec {
    /// A short description for reports: `bigquery:<project>.<dataset>.<table>` or
    /// `cloud_trace:<project>`.
    pub fn describe(&self) -> String {
        match self {
            SourceSpec::BigQuery {
                project,
                dataset,
                table,
            } => format!("bigquery:{project}.{dataset}.{table}"),
            SourceSpec::CloudTrace { project } => format!("cloud_trace:{project}"),
        }
    }

    /// The Google Cloud project of the source.
    pub fn project(&self) -> &str {
        match self {
            SourceSpec::BigQuery { project, .. } | SourceSpec::CloudTrace { project } => project,
        }
    }
}

/// The Gemini model on Vertex AI that grades `llm_judge` scorers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JudgeSpec {
    /// The model name, for example `gemini-2.5-flash`.
    pub model: String,
    /// The Vertex AI project.
    pub project: String,
    /// The Vertex AI location.
    pub location: String,
    /// The bound on each judge call.
    pub timeout: Duration,
}

impl MonitorSpec {
    /// Reads and validates the monitor spec at `path`.
    ///
    /// # Errors
    ///
    /// [`MonitorError::Spec`] when the file cannot be read or the spec is invalid.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, MonitorError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .map_err(|e| MonitorError::Spec(format!("reading {}: {e}", path.display())))?;
        Self::from_yaml(&text)
    }

    /// Parses and validates a monitor spec from YAML text.
    ///
    /// # Errors
    ///
    /// [`MonitorError::Spec`] naming the offending field.
    pub fn from_yaml(yaml: &str) -> Result<Self, MonitorError> {
        let doc: Value = serde_saphyr::from_str(yaml)
            .map_err(|e| MonitorError::Spec(format!("invalid YAML: {e}")))?;
        parse(&doc).map_err(MonitorError::Spec)
    }

    /// The query for the window ending at `now`: runs started in `[now - window, now)`.
    pub fn query(&self, now: DateTime<Utc>) -> TranscriptQuery {
        TranscriptQuery {
            from: now
                .checked_sub_signed(self.window)
                .unwrap_or(DateTime::<Utc>::MIN_UTC),
            to: now,
            agent: self.agent.clone(),
            limit: self.limit,
        }
    }
}

/// Whether `s` is a usable Google Cloud project id: one or more of `[A-Za-z0-9_-]`.
pub(crate) fn is_project_id(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Whether `s` is a usable BigQuery dataset or table name: one or more of `[A-Za-z0-9_]`.
pub(crate) fn is_dataset_or_table(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Whether `s` is a usable monitor name: 1-64 of `[A-Za-z0-9_-]`.
pub(crate) fn is_monitor_name(s: &str) -> bool {
    is_project_id(s) && s.len() <= 64
}

fn check_keys(m: &Map<String, Value>, field: &str, allowed: &[&str]) -> Result<(), String> {
    match m.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(k) => Err(format!(
            "{field}unknown key `{k}` (expected one of: {})",
            allowed.join(", ")
        )),
        None => Ok(()),
    }
}

/// The string at `key` when it passes `valid`, else `Err(message)`.
fn string_field(
    m: &Map<String, Value>,
    key: &str,
    valid: impl Fn(&str) -> bool,
    message: &str,
) -> Result<Option<String>, String> {
    match m.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if valid(s) => Ok(Some(s.clone())),
        Some(_) => Err(message.to_owned()),
    }
}

/// The string at `key`, which must be present and pass `valid`, else `Err(message)`.
fn required(
    m: &Map<String, Value>,
    key: &str,
    valid: impl Fn(&str) -> bool,
    message: &str,
) -> Result<String, String> {
    string_field(m, key, valid, message)?.ok_or_else(|| message.to_owned())
}

fn parse(doc: &Value) -> Result<MonitorSpec, String> {
    let top = doc.as_object().ok_or("the document must be a mapping")?;
    check_keys(
        top,
        "",
        &[
            "name",
            "source",
            "window",
            "agent",
            "limit",
            "min_pass_rate",
            "judge",
            "scorers",
        ],
    )?;
    let name = required(
        top,
        "name",
        is_monitor_name,
        "`name` must match [A-Za-z0-9_-]{1,64}",
    )?;
    let source = parse_source(top.get("source"))?;
    let window = top
        .get("window")
        .and_then(Value::as_str)
        .and_then(parse_window)
        .ok_or("`window` must be <n>m, <n>h or <n>d with n >= 1")?;
    let agent = string_field(
        top,
        "agent",
        |s| !s.is_empty(),
        "`agent` must be a non-empty string",
    )?;
    let limit = match top.get("limit") {
        None | Some(Value::Null) => DEFAULT_LIMIT,
        Some(v) => v
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| (1..=MAX_LIMIT).contains(n))
            .ok_or("`limit` must be an integer from 1 to 1000")?,
    };
    let min_pass_rate = match top.get("min_pass_rate") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_f64()
                .filter(|r| (0.0..=1.0).contains(r))
                .ok_or("`min_pass_rate` must be a number from 0.0 to 1.0")?,
        ),
    };
    let scorers = match top.get("scorers") {
        Some(Value::Array(a)) if !a.is_empty() => a
            .iter()
            .enumerate()
            .map(|(i, v)| parse_scorer(i + 1, v))
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err("`scorers` must be a non-empty list".into()),
    };
    let wants_judge = scorers
        .iter()
        .any(|s| matches!(s, ScorerSpec::LlmJudge { .. }));
    let judge = match (top.get("judge"), wants_judge) {
        (None | Some(Value::Null), false) => None,
        (None | Some(Value::Null), true) => {
            return Err("`judge` is required when a scorer is `llm_judge`".into());
        }
        (Some(_), false) => return Err("`judge` is set but no scorer is `llm_judge`".into()),
        (Some(v), true) => Some(parse_judge(v, source.project())?),
    };
    Ok(MonitorSpec {
        name,
        source,
        window,
        agent,
        limit,
        min_pass_rate,
        judge,
        scorers,
    })
}

/// The message for a `source` that names no known source.
const SOURCE_KINDS: &str = "`source` must be a mapping with `bigquery` or `cloud_trace`";

fn parse_source(v: Option<&Value>) -> Result<SourceSpec, String> {
    let m = v.and_then(Value::as_object).ok_or(SOURCE_KINDS)?;
    check_keys(m, "`source`: ", &["bigquery", "cloud_trace"])?;
    match (m.get("bigquery"), m.get("cloud_trace")) {
        (Some(bq), None) => parse_bigquery(bq),
        (None, Some(ct)) => parse_cloud_trace(ct),
        (Some(_), Some(_)) => {
            Err("`source` must set exactly one of `bigquery` and `cloud_trace`".into())
        }
        (None, None) => Err(SOURCE_KINDS.into()),
    }
}

fn parse_bigquery(v: &Value) -> Result<SourceSpec, String> {
    let bq = v.as_object().ok_or("`source.bigquery` must be a mapping")?;
    check_keys(bq, "`source.bigquery`: ", &["project", "dataset", "table"])?;
    let project = required(
        bq,
        "project",
        is_project_id,
        "`source.bigquery.project` must match [A-Za-z0-9_-]+",
    )?;
    let dataset = required(
        bq,
        "dataset",
        is_dataset_or_table,
        "`source.bigquery.dataset` must match [A-Za-z0-9_]+",
    )?;
    let table = required(
        bq,
        "table",
        is_dataset_or_table,
        "`source.bigquery.table` must match [A-Za-z0-9_]+",
    )?;
    Ok(SourceSpec::BigQuery {
        project,
        dataset,
        table,
    })
}

fn parse_cloud_trace(v: &Value) -> Result<SourceSpec, String> {
    let ct = v
        .as_object()
        .ok_or("`source.cloud_trace` must be a mapping")?;
    check_keys(ct, "`source.cloud_trace`: ", &["project"])?;
    let project = required(
        ct,
        "project",
        is_project_id,
        "`source.cloud_trace.project` must match [A-Za-z0-9_-]+",
    )?;
    Ok(SourceSpec::CloudTrace { project })
}

fn parse_judge(v: &Value, source_project: &str) -> Result<JudgeSpec, String> {
    let m = v.as_object().ok_or("`judge` must be a mapping")?;
    check_keys(m, "`judge`: ", &["model", "project", "location", "timeout"])?;
    let model = required(
        m,
        "model",
        |s| !s.trim().is_empty(),
        "`judge.model` must be a non-empty string",
    )?;
    let project = string_field(
        m,
        "project",
        is_project_id,
        "`judge.project` must match [A-Za-z0-9_-]+",
    )?
    .unwrap_or_else(|| source_project.to_owned());
    let location = string_field(
        m,
        "location",
        |s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'),
        "`judge.location` must match [A-Za-z0-9-]+",
    )?
    .unwrap_or_else(|| DEFAULT_JUDGE_LOCATION.to_owned());
    let timeout = match m.get("timeout") {
        None | Some(Value::Null) => DEFAULT_JUDGE_TIMEOUT,
        Some(v) => v
            .as_str()
            .and_then(parse_timeout)
            .ok_or("`judge.timeout` must be <n>s or <n>m with n >= 1")?,
    };
    Ok(JudgeSpec {
        model,
        project,
        location,
        timeout,
    })
}

/// Scorer `i` (1-based) in the gadk-eval syntax; `reference` and `equals` are rejected.
fn parse_scorer(i: usize, v: &Value) -> Result<ScorerSpec, String> {
    let spec = ScorerSpec::from_value(v).map_err(|e| match e {
        EvalError::Load { message, .. } => format!("scorers[{i}]: {message}"),
        other => format!("scorers[{i}]: {other}"),
    })?;
    match spec {
        ScorerSpec::Reference { .. } | ScorerSpec::Equals { .. } => Err(format!(
            "scorers[{i}]: `{}` needs a reference answer, which production runs do not have",
            spec.kind()
        )),
        _ => Ok(spec),
    }
}

/// `n` and its one-letter unit, with `n >= 1`.
fn amount(s: &str) -> Option<(i64, char)> {
    let unit = s.chars().last()?;
    let n: i64 = s[..s.len() - unit.len_utf8()].parse().ok()?;
    (n >= 1).then_some((n, unit))
}

fn parse_window(s: &str) -> Option<TimeDelta> {
    match amount(s)? {
        (n, 'm') => TimeDelta::try_minutes(n),
        (n, 'h') => TimeDelta::try_hours(n),
        (n, 'd') => TimeDelta::try_days(n),
        _ => None,
    }
}

fn parse_timeout(s: &str) -> Option<Duration> {
    let (n, unit) = amount(s)?;
    let n = u64::try_from(n).ok()?;
    match unit {
        's' => Some(Duration::from_secs(n)),
        'm' => n.checked_mul(60).map(Duration::from_secs),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"
name: support-bot-prod
source:
  bigquery: { project: my-proj, dataset: agent_analytics, table: agent_events }
window: 24h
agent: support_bot
limit: 50
min_pass_rate: 0.9
judge:
  model: gemini-2.5-flash
  project: judge-proj
  location: europe-west4
  timeout: 2m
scorers:
  - not_contains: "I cannot help"
  - contains: { text: "sorry", ignore_case: true, turn: 3 }
  - tools: [lookup_order]
  - llm_judge: { rubric: "Polite and accurate", min_score: 4 }
"#;

    const MINIMAL: &str = r#"
name: m
source: { bigquery: { project: p, dataset: d, table: t } }
window: 30m
scorers: [contains: "x"]
"#;

    fn spec_err(yaml: &str) -> String {
        match MonitorSpec::from_yaml(yaml) {
            Err(MonitorError::Spec(m)) => m,
            other => panic!("expected a spec error, got {other:?}"),
        }
    }

    #[test]
    fn parses_a_full_spec() {
        let s = MonitorSpec::from_yaml(FULL).unwrap();
        assert_eq!(s.name, "support-bot-prod");
        assert_eq!(
            s.source,
            SourceSpec::BigQuery {
                project: "my-proj".into(),
                dataset: "agent_analytics".into(),
                table: "agent_events".into(),
            }
        );
        assert_eq!(
            s.source.describe(),
            "bigquery:my-proj.agent_analytics.agent_events"
        );
        assert_eq!(s.window, TimeDelta::hours(24));
        assert_eq!(s.agent.as_deref(), Some("support_bot"));
        assert_eq!(s.limit, 50);
        assert_eq!(s.min_pass_rate, Some(0.9));
        assert_eq!(
            s.judge,
            Some(JudgeSpec {
                model: "gemini-2.5-flash".into(),
                project: "judge-proj".into(),
                location: "europe-west4".into(),
                timeout: Duration::from_secs(120),
            })
        );
        let kinds: Vec<_> = s.scorers.iter().map(ScorerSpec::kind).collect();
        assert_eq!(kinds, ["not_contains", "contains", "tools", "llm_judge"]);
        assert!(matches!(
            s.scorers[1],
            ScorerSpec::Contains { turn: Some(3), .. }
        ));
    }

    #[test]
    fn applies_defaults() {
        let s = MonitorSpec::from_yaml(MINIMAL).unwrap();
        assert_eq!(s.window, TimeDelta::minutes(30));
        assert_eq!(s.agent, None);
        assert_eq!(s.limit, DEFAULT_LIMIT);
        assert_eq!(s.min_pass_rate, None);
        assert!(s.judge.is_none());
        let s = MonitorSpec::from_yaml(
            "name: m\nsource: { bigquery: { project: p, dataset: d, table: t } }\nwindow: 7d\n\
             judge: { model: gemini-2.5-flash }\nscorers: [llm_judge: be nice]\n",
        )
        .unwrap();
        assert_eq!(s.window, TimeDelta::days(7));
        assert_eq!(
            s.judge,
            Some(JudgeSpec {
                model: "gemini-2.5-flash".into(),
                project: "p".into(),
                location: DEFAULT_JUDGE_LOCATION.into(),
                timeout: DEFAULT_JUDGE_TIMEOUT,
            })
        );
    }

    #[test]
    fn query_covers_the_window_ending_now() {
        let s = MonitorSpec::from_yaml(FULL).unwrap();
        let now = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let q = s.query(now);
        assert_eq!(q.to, now);
        assert_eq!(q.from, now - TimeDelta::hours(24));
        assert_eq!(q.agent.as_deref(), Some("support_bot"));
        assert_eq!(q.limit, 50);
    }

    #[test]
    fn every_invalid_field_is_named() {
        let base = |field: &str, value: &str| {
            let mut lines: Vec<String> = vec![
                "name: m".into(),
                "source: { bigquery: { project: p, dataset: d, table: t } }".into(),
                "window: 1h".into(),
                "scorers: [contains: x]".into(),
            ];
            lines.retain(|l| !l.starts_with(&format!("{field}:")));
            if !value.is_empty() {
                lines.push(format!("{field}: {value}"));
            }
            lines.join("\n")
        };
        let cases = [
            (base("name", ""), "`name`"),
            (base("name", "\"has space\""), "`name`"),
            (base("name", &"a".repeat(65)), "`name`"),
            (base("source", ""), "`source`"),
            (
                base("source", "{ bigquery: { project: p, dataset: d } }"),
                "`source.bigquery.table`",
            ),
            (
                base(
                    "source",
                    "{ bigquery: { project: \"p.q\", dataset: d, table: t } }",
                ),
                "`source.bigquery.project`",
            ),
            (
                base(
                    "source",
                    "{ bigquery: { project: p, dataset: d-x, table: t } }",
                ),
                "`source.bigquery.dataset`",
            ),
            (
                base(
                    "source",
                    "{ bigquery: { project: p, dataset: d, table: \"t`; x\" } }",
                ),
                "`source.bigquery.table`",
            ),
            (
                base(
                    "source",
                    "{ bigquery: { project: p, dataset: d, table: t, x: 1 } }",
                ),
                "`source.bigquery`: unknown key `x`",
            ),
            (
                base("source", "{ trace: {} }"),
                "`source`: unknown key `trace`",
            ),
            (base("window", ""), "`window`"),
            (base("window", "0h"), "`window`"),
            (base("window", "5s"), "`window`"),
            (base("window", "h"), "`window`"),
            (base("agent", "\"\""), "`agent`"),
            (base("limit", "0"), "`limit`"),
            (base("limit", "1001"), "`limit`"),
            (base("limit", "ten"), "`limit`"),
            (base("min_pass_rate", "1.5"), "`min_pass_rate`"),
            (base("min_pass_rate", "-0.1"), "`min_pass_rate`"),
            (base("scorers", "[]"), "`scorers`"),
            (
                base("scorers", "[bogus: x]"),
                "scorers[1]: unknown scorer `bogus`",
            ),
            (
                base("scorers", "[contains: x, reference: y]"),
                "scorers[2]: `reference`",
            ),
            (base("scorers", "[equals: y]"), "scorers[1]: `equals`"),
            (
                base("scorers", "[llm_judge: {}]"),
                "scorers[1]: `llm_judge` needs a `rubric`",
            ),
            (
                base("scorers", "[llm_judge: be nice]"),
                "`judge` is required",
            ),
            (
                base("judge", "{ model: m }"),
                "`judge` is set but no scorer is `llm_judge`",
            ),
            (base("bogus", "1"), "unknown key `bogus`"),
        ];
        for (yaml, needle) in cases {
            let msg = spec_err(&yaml);
            assert!(msg.contains(needle), "{yaml}\n=> {msg}");
        }
    }

    #[test]
    fn invalid_judge_fields_are_named() {
        let judge = |j: &str| {
            format!(
                "name: m\nsource: {{ bigquery: {{ project: p, dataset: d, table: t }} }}\n\
                 window: 1h\njudge: {j}\nscorers: [llm_judge: be nice]\n"
            )
        };
        for (j, needle) in [
            ("{ project: p }", "`judge.model`"),
            ("{ model: \" \" }", "`judge.model`"),
            ("{ model: m, project: \"a b\" }", "`judge.project`"),
            ("{ model: m, location: \"us/central\" }", "`judge.location`"),
            ("{ model: m, timeout: 0s }", "`judge.timeout`"),
            ("{ model: m, timeout: 2h }", "`judge.timeout`"),
            (
                "{ model: m, temperature: 0 }",
                "`judge`: unknown key `temperature`",
            ),
            ("gemini", "`judge` must be a mapping"),
        ] {
            let msg = spec_err(&judge(j));
            assert!(msg.contains(needle), "{j}\n=> {msg}");
        }
        let s = MonitorSpec::from_yaml(&judge("{ model: m, timeout: 45s }")).unwrap();
        assert_eq!(s.judge.unwrap().timeout, Duration::from_secs(45));
    }

    #[test]
    fn parses_a_cloud_trace_source() {
        let s = MonitorSpec::from_yaml(
            "name: m\nsource: { cloud_trace: { project: my-proj } }\nwindow: 1h\n\
             judge: { model: gemini-2.5-flash }\nscorers: [llm_judge: be nice]\n",
        )
        .unwrap();
        assert_eq!(
            s.source,
            SourceSpec::CloudTrace {
                project: "my-proj".into()
            }
        );
        assert_eq!(s.source.describe(), "cloud_trace:my-proj");
        assert_eq!(s.source.project(), "my-proj");
        assert_eq!(s.judge.unwrap().project, "my-proj");
    }

    #[test]
    fn invalid_cloud_trace_sources_are_named() {
        for (source, needle) in [
            (
                "{ cloud_trace: { project: \"a b\" } }",
                "`source.cloud_trace.project` must match [A-Za-z0-9_-]+",
            ),
            (
                "{ cloud_trace: {} }",
                "`source.cloud_trace.project` must match [A-Za-z0-9_-]+",
            ),
            (
                "{ cloud_trace: { project: p, dataset: d } }",
                "`source.cloud_trace`: unknown key `dataset`",
            ),
            (
                "{ cloud_trace: p }",
                "`source.cloud_trace` must be a mapping",
            ),
            (
                "{ cloud_trace: { project: p }, bigquery: { project: p, dataset: d, table: t } }",
                "`source` must set exactly one of `bigquery` and `cloud_trace`",
            ),
            (
                "{}",
                "`source` must be a mapping with `bigquery` or `cloud_trace`",
            ),
            (
                "{ trace: {} }",
                "`source`: unknown key `trace` (expected one of: bigquery, cloud_trace)",
            ),
        ] {
            let msg = spec_err(&format!(
                "name: m\nsource: {source}\nwindow: 1h\nscorers: [contains: x]\n"
            ));
            assert!(msg.contains(needle), "{source}\n=> {msg}");
        }
    }

    #[test]
    fn load_reads_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("m.yaml");
        std::fs::write(&p, MINIMAL).unwrap();
        assert_eq!(MonitorSpec::load(&p).unwrap().name, "m");
        let missing = dir.path().join("missing.yaml");
        assert!(matches!(
            MonitorSpec::load(&missing),
            Err(MonitorError::Spec(m)) if m.starts_with("reading ")
        ));
        assert!(matches!(
            MonitorSpec::from_yaml("name: ["),
            Err(MonitorError::Spec(m)) if m.starts_with("invalid YAML")
        ));
    }
}
