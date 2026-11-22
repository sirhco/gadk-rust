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

//! Experiments: one eval set run against two or more variants of an agent, kept as an
//! [`ExperimentReport`] with every run's metrics, and compared per variant and per case.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use gadk_agent::Agent;
use gadk_eval::{
    CaseRun, EvalReport, EvalSet, Evaluator, MeasuredReport, Outcome, RunMetrics, TokenCounts,
    Variant,
};
use serde::{Deserialize, Serialize};

use crate::error::MonitorError;
use crate::spec::is_monitor_name;

/// One variant of an experiment: its name and the prompt selector it used, if any.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperimentVariant {
    /// The variant name.
    pub name: String,
    /// The prompt selector's text (`"v2"`, `"production"`, `"latest"`), or `None` for a variant
    /// without one.
    pub prompt: Option<String>,
}

impl ExperimentVariant {
    /// A variant description from its parts.
    pub fn new(name: impl Into<String>, prompt: Option<String>) -> Self {
        ExperimentVariant {
            name: name.into(),
            prompt,
        }
    }
}

/// The result of [`run_experiment`]: the eval report of every variant plus every run's metrics.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExperimentReport {
    /// The experiment name (`[A-Za-z0-9_-]{1,64}`).
    pub name: String,
    /// The eval set's name.
    pub set: String,
    /// When the runs finished.
    pub generated_at: DateTime<Utc>,
    /// The variants, in the order given; the first is the baseline.
    pub variants: Vec<ExperimentVariant>,
    /// Every run's transcript and scores.
    pub report: EvalReport,
    /// `metrics[v][i]` belongs to `report.variants[v].runs[i]`.
    pub metrics: Vec<Vec<RunMetrics>>,
}

/// One variant's numbers, from [`ExperimentReport::variant_stats`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct VariantStats {
    /// The variant name.
    pub name: String,
    /// The number of runs.
    pub runs: usize,
    /// Runs without a run error whose scores all passed (a run without scores passes).
    pub passed: usize,
    /// Runs without a run error or error score that have a failed score.
    pub failed: usize,
    /// Runs with a run error or an error score.
    pub errors: usize,
    /// The mean judge score over the scores that have one and are not errors.
    pub mean_score: Option<f64>,
    /// The median duration (nearest rank) of the runs without a run error.
    pub p50_ms: Option<u64>,
    /// The 95th percentile duration (nearest rank) of the runs without a run error.
    pub p95_ms: Option<u64>,
    /// The token counts summed over every run and model.
    pub tokens: TokenCounts,
}

impl VariantStats {
    /// `passed / (passed + failed)`; `None` when both are 0.
    pub fn pass_rate(&self) -> Option<f64> {
        let graded = self.passed + self.failed;
        (graded > 0).then(|| self.passed as f64 / graded as f64)
    }
}

/// A run's or a case's verdict for one variant.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Every score passed and nothing errored.
    Pass,
    /// A score failed and nothing errored.
    Fail,
    /// A run error or an error score.
    Error,
}

/// One case's verdict per variant, from [`ExperimentReport::case_verdicts`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct CaseVerdicts {
    /// The case id.
    pub case: String,
    /// One verdict per variant, in variant order: [`Verdict::Error`] when any of the variant's
    /// runs of the case errored, else [`Verdict::Pass`] when all passed, else [`Verdict::Fail`].
    pub verdicts: Vec<Verdict>,
}

impl CaseVerdicts {
    /// True when the variants do not all have the same verdict.
    pub fn disagree(&self) -> bool {
        self.verdicts.windows(2).any(|w| w[0] != w[1])
    }
}

/// A run's verdict: error on a run error or an error score, else pass when every score passed.
fn run_verdict(r: &CaseRun) -> Verdict {
    if r.transcript.error.is_some() || r.scores.iter().any(|s| s.outcome == Outcome::Error) {
        Verdict::Error
    } else if r.scores.iter().all(|s| s.outcome == Outcome::Pass) {
        Verdict::Pass
    } else {
        Verdict::Fail
    }
}

/// The nearest-rank `pct` percentile of `sorted` (ascending); `None` when it is empty.
fn nearest_rank(sorted: &[u64], pct: usize) -> Option<u64> {
    let rank = (pct * sorted.len()).div_ceil(100).max(1);
    sorted.get(rank - 1).copied()
}

impl ExperimentReport {
    /// A report named `name` from the runs of `variants`, which must be the variants passed to
    /// [`Evaluator::run_measured`] for `measured`.
    pub fn new(
        name: impl Into<String>,
        generated_at: DateTime<Utc>,
        variants: &[Variant],
        measured: MeasuredReport,
    ) -> Self {
        ExperimentReport {
            name: name.into(),
            set: measured.report.set.clone(),
            generated_at,
            variants: variants
                .iter()
                .map(|v| ExperimentVariant::new(v.name(), v.selector().map(|s| s.to_string())))
                .collect(),
            report: measured.report,
            metrics: measured.metrics,
        }
    }

    /// The report as pretty-printed JSON.
    ///
    /// # Errors
    ///
    /// [`MonitorError::Io`] if serialization fails.
    pub fn to_json(&self) -> Result<String, MonitorError> {
        serde_json::to_string_pretty(self).map_err(|e| MonitorError::Io(e.into()))
    }

    /// A report from its JSON.
    ///
    /// # Errors
    ///
    /// [`MonitorError::Io`] (`InvalidData`) when `json` is not an experiment report.
    pub fn from_json(json: &str) -> Result<Self, MonitorError> {
        serde_json::from_str(json).map_err(|e| MonitorError::Io(e.into()))
    }

    /// One entry per variant of the eval report, in variant order. A run without metrics (a
    /// hand-edited report) counts with [`RunMetrics::default`].
    pub fn variant_stats(&self) -> Vec<VariantStats> {
        let none = Vec::new();
        self.report
            .variants
            .iter()
            .enumerate()
            .map(|(vi, v)| {
                let metrics = self.metrics.get(vi).unwrap_or(&none);
                let default = RunMetrics::default();
                let (mut passed, mut failed, mut errors) = (0, 0, 0);
                let mut scores: Vec<u8> = Vec::new();
                let mut durations: Vec<u64> = Vec::new();
                let mut tokens = TokenCounts::default();
                for (i, r) in v.runs.iter().enumerate() {
                    let m = metrics.get(i).unwrap_or(&default);
                    match run_verdict(r) {
                        Verdict::Pass => passed += 1,
                        Verdict::Fail => failed += 1,
                        Verdict::Error => errors += 1,
                    }
                    scores.extend(
                        r.scores
                            .iter()
                            .filter(|s| s.outcome != Outcome::Error)
                            .filter_map(|s| s.score),
                    );
                    if r.transcript.error.is_none() {
                        durations.push(m.duration_ms);
                    }
                    tokens += m.total();
                }
                durations.sort_unstable();
                let mean_score = (!scores.is_empty()).then(|| {
                    let sum: u32 = scores.iter().map(|&s| u32::from(s)).sum();
                    f64::from(sum) / scores.len() as f64
                });
                VariantStats {
                    name: v.name.clone(),
                    runs: v.runs.len(),
                    passed,
                    failed,
                    errors,
                    mean_score,
                    p50_ms: nearest_rank(&durations, 50),
                    p95_ms: nearest_rank(&durations, 95),
                    tokens,
                }
            })
            .collect()
    }

    /// One entry per case, in set order (the order case ids first appear in the runs), with
    /// each variant's verdict for it.
    pub fn case_verdicts(&self) -> Vec<CaseVerdicts> {
        let mut cases: Vec<&str> = Vec::new();
        for r in self.report.variants.iter().flat_map(|v| v.runs.iter()) {
            if !cases.contains(&r.case.as_str()) {
                cases.push(&r.case);
            }
        }
        cases
            .into_iter()
            .map(|case| CaseVerdicts {
                case: case.to_owned(),
                verdicts: self
                    .report
                    .variants
                    .iter()
                    .map(|v| {
                        let verdicts: Vec<Verdict> = v
                            .runs
                            .iter()
                            .filter(|r| r.case == case)
                            .map(run_verdict)
                            .collect();
                        if verdicts.contains(&Verdict::Error) {
                            Verdict::Error
                        } else if verdicts.iter().all(|v| *v == Verdict::Pass) {
                            Verdict::Pass
                        } else {
                            Verdict::Fail
                        }
                    })
                    .collect(),
            })
            .collect()
    }

    /// A text table with one row per variant: runs, pass rate (passed / (passed + failed)
    /// runs), mean judge score, errors, p50 and p95 duration, input and output tokens.
    pub fn summary(&self) -> String {
        let mut rows: Vec<[String; 9]> = vec![[
            "variant".into(),
            "runs".into(),
            "pass rate".into(),
            "mean score".into(),
            "errors".into(),
            "p50".into(),
            "p95".into(),
            "input tokens".into(),
            "output tokens".into(),
        ]];
        let ms = |d: Option<u64>| d.map_or_else(|| "-".to_owned(), |d| format!("{d}ms"));
        for s in self.variant_stats() {
            let graded = s.passed + s.failed;
            let rate = match s.pass_rate() {
                None => "-".to_owned(),
                Some(r) => format!("{}/{graded} ({:.0}%)", s.passed, 100.0 * r),
            };
            let mean = s
                .mean_score
                .map_or_else(|| "-".to_owned(), |m| format!("{m:.2}"));
            rows.push([
                s.name.clone(),
                s.runs.to_string(),
                rate,
                mean,
                s.errors.to_string(),
                ms(s.p50_ms),
                ms(s.p95_ms),
                s.tokens.input.to_string(),
                s.tokens.output.to_string(),
            ]);
        }
        let mut widths = [0usize; 9];
        for r in &rows {
            for (w, c) in widths.iter_mut().zip(r) {
                *w = (*w).max(c.chars().count());
            }
        }
        let mut out = format!("experiment: {}\neval set: {}\n\n", self.name, self.set);
        for r in &rows {
            let line: Vec<String> = r
                .iter()
                .zip(widths)
                .map(|(c, w)| format!("{c:<w$}"))
                .collect();
            out.push_str(line.join("  ").trim_end());
            out.push('\n');
        }
        out
    }
}

/// Runs `set` against every variant with [`Evaluator::run_measured`] and returns the result as
/// an experiment named `name`, stamped with the time the runs finished.
///
/// `build` is called once per variant, as [`Evaluator::run`] does; a build error makes every run
/// of that variant an error.
///
/// # Errors
///
/// [`MonitorError::Spec`] (message prefix `experiment: `) when `name` is not
/// `[A-Za-z0-9_-]{1,64}`, before `build` is called, and when the variants are empty or have
/// empty or duplicate names.
pub async fn run_experiment<F, E>(
    name: &str,
    evaluator: &Evaluator,
    set: &EvalSet,
    variants: &[Variant],
    build: F,
) -> Result<ExperimentReport, MonitorError>
where
    F: Fn(&Variant) -> Result<Arc<dyn Agent>, E>,
    E: std::fmt::Display,
{
    if !is_monitor_name(name) {
        return Err(MonitorError::Spec(format!(
            "experiment: invalid name {name:?} (expected [A-Za-z0-9_-]{{1,64}})"
        )));
    }
    let measured = evaluator
        .run_measured(set, variants, build)
        .await
        .map_err(|e| MonitorError::Spec(format!("experiment: {e}")))?;
    Ok(ExperimentReport::new(name, Utc::now(), variants, measured))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use futures::StreamExt as _;
    use gadk_agent::{AgentConfig, new_agent};
    use gadk_eval::{RunTranscript, ScoreResult, TurnRecord, VariantReport};
    use gadk_prompts::Selector;

    use crate::testutil::{FakeJudge, at};

    use super::*;

    fn run(case: &str, n: u32, error: Option<&str>, scores: Vec<ScoreResult>) -> CaseRun {
        CaseRun {
            case: case.into(),
            run: n,
            transcript: RunTranscript {
                turns: vec![TurnRecord {
                    user: "hi".into(),
                    reply: "hello".into(),
                    calls: vec![],
                }],
                error: error.map(str::to_owned),
            },
            scores,
        }
    }

    fn metrics(ms: u64, models: &[(&str, TokenCounts)]) -> RunMetrics {
        RunMetrics::new(
            ms,
            models
                .iter()
                .map(|(m, t)| ((*m).to_owned(), *t))
                .collect::<BTreeMap<_, _>>(),
        )
    }

    fn pass(kind: &str) -> ScoreResult {
        ScoreResult::check(kind, true, "")
    }

    fn fail(kind: &str) -> ScoreResult {
        ScoreResult::check(kind, false, "")
    }

    /// v1: a#1 pass, a#2 fail, b#1 run error, c#1 no scores; v2: a#1 and a#2 pass, b#1 an
    /// error score, c#1 no scores.
    fn report() -> ExperimentReport {
        let v1 = VariantReport {
            name: "v1".into(),
            runs: vec![
                run(
                    "a",
                    1,
                    None,
                    vec![
                        pass("contains"),
                        ScoreResult::judged("llm_judge", true, 4, ""),
                    ],
                ),
                run(
                    "a",
                    2,
                    None,
                    vec![
                        fail("contains"),
                        ScoreResult::judged("llm_judge", false, 2, ""),
                    ],
                ),
                run(
                    "b",
                    1,
                    Some("turn 1: boom"),
                    vec![ScoreResult::error("contains", "turn 1: boom")],
                ),
                run("c", 1, None, vec![]),
            ],
        };
        let v2 = VariantReport {
            name: "v2".into(),
            runs: vec![
                run(
                    "a",
                    1,
                    None,
                    vec![
                        pass("contains"),
                        ScoreResult::judged("llm_judge", true, 5, ""),
                    ],
                ),
                run(
                    "a",
                    2,
                    None,
                    vec![
                        pass("contains"),
                        ScoreResult::judged("llm_judge", true, 3, ""),
                    ],
                ),
                run(
                    "b",
                    1,
                    None,
                    vec![
                        pass("contains"),
                        ScoreResult::error("llm_judge", "unparseable"),
                    ],
                ),
                run("c", 1, None, vec![]),
            ],
        };
        let m1 = vec![
            metrics(100, &[("m", TokenCounts::new(10, 1, 5, 2))]),
            metrics(
                300,
                &[
                    ("m", TokenCounts::new(20, 0, 7, 0)),
                    ("n", TokenCounts::new(1, 0, 1, 0)),
                ],
            ),
            metrics(5000, &[]),
            metrics(200, &[]),
        ];
        let m2 = vec![
            metrics(400, &[("m", TokenCounts::new(100, 0, 50, 0))]),
            metrics(600, &[]),
            metrics(800, &[]),
            metrics(1000, &[]),
        ];
        ExperimentReport::new(
            "exp",
            at(1_700_000_000),
            &[
                Variant::new("v1"),
                Variant::prompt("v2", Selector::Version(2)),
            ],
            MeasuredReport::new(
                EvalReport {
                    set: "support".into(),
                    variants: vec![v1, v2],
                },
                vec![m1, m2],
            ),
        )
    }

    #[test]
    fn new_takes_the_set_and_variant_prompts() {
        let r = report();
        assert_eq!(r.name, "exp");
        assert_eq!(r.set, "support");
        assert_eq!(r.generated_at, at(1_700_000_000));
        assert_eq!(
            r.variants,
            [
                ExperimentVariant::new("v1", None),
                ExperimentVariant::new("v2", Some("v2".into())),
            ]
        );
    }

    #[test]
    fn variant_stats_classify_runs() {
        let s = report().variant_stats();
        assert_eq!(
            s[0],
            VariantStats {
                name: "v1".into(),
                runs: 4,
                passed: 2,
                failed: 1,
                errors: 1,
                mean_score: Some(3.0),
                // The run with a run error (5000 ms) is left out.
                p50_ms: Some(200),
                p95_ms: Some(300),
                tokens: TokenCounts::new(31, 1, 13, 2),
            }
        );
        assert_eq!(
            s[1],
            VariantStats {
                name: "v2".into(),
                runs: 4,
                passed: 3,
                failed: 0,
                errors: 1,
                mean_score: Some(4.0),
                // An error score is not a run error: its duration counts.
                p50_ms: Some(600),
                p95_ms: Some(1000),
                tokens: TokenCounts::new(100, 0, 50, 0),
            }
        );
        assert_eq!(s[0].pass_rate(), Some(2.0 / 3.0));
        assert_eq!(s[1].pass_rate(), Some(1.0));
    }

    #[test]
    fn stats_without_graded_runs_or_durations_are_none() {
        let mut r = report();
        r.report.variants[0].runs = vec![run("a", 1, Some("building variant v1: boom"), vec![])];
        r.metrics[0] = vec![RunMetrics::default()];
        let s = &r.variant_stats()[0];
        assert_eq!((s.runs, s.passed, s.failed, s.errors), (1, 0, 0, 1));
        assert_eq!(s.pass_rate(), None);
        assert_eq!((s.mean_score, s.p50_ms, s.p95_ms), (None, None, None));
    }

    #[test]
    fn nearest_rank_percentiles() {
        assert_eq!(nearest_rank(&[], 50), None);
        assert_eq!(nearest_rank(&[7], 50), Some(7));
        assert_eq!(nearest_rank(&[7], 95), Some(7));
        let twenty: Vec<u64> = (1..=20).collect();
        assert_eq!(nearest_rank(&twenty, 50), Some(10));
        assert_eq!(nearest_rank(&twenty, 95), Some(19));
        let twenty_one: Vec<u64> = (1..=21).collect();
        assert_eq!(nearest_rank(&twenty_one, 50), Some(11));
        assert_eq!(nearest_rank(&twenty_one, 95), Some(20));
    }

    #[test]
    fn case_verdicts_and_disagreements() {
        let v = report().case_verdicts();
        assert_eq!(
            v,
            [
                CaseVerdicts {
                    case: "a".into(),
                    verdicts: vec![Verdict::Fail, Verdict::Pass],
                },
                CaseVerdicts {
                    case: "b".into(),
                    verdicts: vec![Verdict::Error, Verdict::Error],
                },
                CaseVerdicts {
                    case: "c".into(),
                    verdicts: vec![Verdict::Pass, Verdict::Pass],
                },
            ]
        );
        let disagree: Vec<bool> = v.iter().map(CaseVerdicts::disagree).collect();
        assert_eq!(disagree, [true, false, false]);
    }

    #[test]
    fn a_case_errors_when_any_run_errors() {
        let mut r = report();
        // v2's case a: one run passes, one errors.
        r.report.variants[1].runs[1] = run("a", 2, Some("run timed out after 50ms"), vec![]);
        assert_eq!(
            r.case_verdicts()[0].verdicts,
            [Verdict::Fail, Verdict::Error]
        );
    }

    #[test]
    fn summary_table() {
        let want = "\
experiment: exp
eval set: support

variant  runs  pass rate   mean score  errors  p50    p95     input tokens  output tokens
v1       4     2/3 (67%)   3.00        1       200ms  300ms   31            13
v2       4     3/3 (100%)  4.00        1       600ms  1000ms  100           50
";
        assert_eq!(report().summary(), want);
    }

    #[test]
    fn json_round_trips() {
        let r = report();
        let back = ExperimentReport::from_json(&r.to_json().unwrap()).unwrap();
        assert_eq!(back, r);
        assert!(matches!(
            ExperimentReport::from_json("{\"name\": 1}"),
            Err(MonitorError::Io(_))
        ));
    }

    const SET: &str = r#"
name: quiet
cases:
  - id: silent
    turns: ["hi"]
    expect: [regex: "^$"]
  - id: talks
    turns: ["hi"]
    expect: [contains: "hello"]
"#;

    /// An agent that answers every turn with no events, so every reply is empty.
    fn quiet() -> Arc<dyn Agent> {
        new_agent(AgentConfig {
            name: "quiet".into(),
            run: Some(Arc::new(|_ctx| futures::stream::empty().boxed())),
            ..Default::default()
        })
        .unwrap()
    }

    #[tokio::test]
    async fn invalid_names_fail_before_build() {
        let set = EvalSet::from_yaml(SET).unwrap();
        let evaluator = Evaluator::new(FakeJudge::new(&[]));
        let builds = AtomicUsize::new(0);
        let long = "x".repeat(65);
        for name in ["", "a b", "a/b", "é", long.as_str()] {
            let e = run_experiment(name, &evaluator, &set, &[Variant::new("v1")], |_| {
                builds.fetch_add(1, Ordering::SeqCst);
                Ok::<_, String>(quiet())
            })
            .await
            .unwrap_err();
            assert!(
                matches!(&e, MonitorError::Spec(m) if *m == format!("experiment: invalid name {name:?} (expected [A-Za-z0-9_-]{{1,64}})")),
                "{e}"
            );
        }
        assert_eq!(builds.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn invalid_variants_are_spec_errors() {
        let set = EvalSet::from_yaml(SET).unwrap();
        let evaluator = Evaluator::new(FakeJudge::new(&[]));
        let e = run_experiment("exp", &evaluator, &set, &[], |_| Ok::<_, String>(quiet()))
            .await
            .unwrap_err();
        assert_eq!(
            e.to_string(),
            "monitor spec: experiment: variants: at least one variant is required"
        );
        let e = run_experiment(
            "exp",
            &evaluator,
            &set,
            &[Variant::new("v"), Variant::new("v")],
            |_| Ok::<_, String>(quiet()),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(&e, MonitorError::Spec(m) if m.starts_with("experiment: variants: duplicate")),
            "{e}"
        );
    }

    #[tokio::test]
    async fn a_valid_run_fills_every_field() {
        let set = EvalSet::from_yaml(SET).unwrap();
        let evaluator = Evaluator::new(FakeJudge::new(&[])).runs_per_case(2);
        let variants = [
            Variant::new("v1"),
            Variant::prompt("v2", Selector::Version(2)),
            Variant::new("broken"),
        ];
        let before = Utc::now();
        let r = run_experiment("exp-e2e", &evaluator, &set, &variants, |v| {
            if v.name() == "broken" {
                Err("no such prompt version".to_owned())
            } else {
                Ok(quiet())
            }
        })
        .await
        .unwrap();
        assert_eq!(r.name, "exp-e2e");
        assert_eq!(r.set, "quiet");
        assert!(before <= r.generated_at && r.generated_at <= Utc::now());
        assert_eq!(
            r.variants,
            [
                ExperimentVariant::new("v1", None),
                ExperimentVariant::new("v2", Some("v2".into())),
                ExperimentVariant::new("broken", None),
            ]
        );
        let shape: Vec<usize> = r.metrics.iter().map(Vec::len).collect();
        assert_eq!(shape, [4, 4, 4]);
        let names: Vec<&str> = r.report.variants.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["v1", "v2", "broken"]);
        let stats = r.variant_stats();
        let counts: Vec<(usize, usize, usize)> = stats
            .iter()
            .map(|s| (s.passed, s.failed, s.errors))
            .collect();
        assert_eq!(counts, [(2, 2, 0), (2, 2, 0), (0, 0, 4)]);
        assert_eq!(r.metrics[2], vec![RunMetrics::default(); 4]);
        let verdicts: Vec<(String, Vec<Verdict>)> = r
            .case_verdicts()
            .into_iter()
            .map(|c| (c.case, c.verdicts))
            .collect();
        assert_eq!(
            verdicts,
            [
                (
                    "silent".to_owned(),
                    vec![Verdict::Pass, Verdict::Pass, Verdict::Error]
                ),
                (
                    "talks".to_owned(),
                    vec![Verdict::Fail, Verdict::Fail, Verdict::Error]
                ),
            ]
        );
    }
}
