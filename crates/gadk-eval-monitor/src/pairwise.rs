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

//! Pairwise comparison of an experiment's variants: each variant after the first against the
//! first (the baseline), run by run, through a [`PairwiseJudge`].

use futures::StreamExt as _;
use gadk_eval::{CaseRun, EvalCase, EvalReport, EvalSet, ScorerSpec, render_conversation};
use serde::{Deserialize, Serialize};

use crate::eval_service::{PairwiseChoice, PairwiseJudge, PairwiseOutcome};

/// The criteria of a case without an `llm_judge` scorer.
pub const DEFAULT_PAIRWISE_CRITERIA: &str = "Which conversation's agent is more helpful and \
correct, and follows the user's instructions better?";

/// The most compare calls [`run_pairwise`] has in flight.
const PAIRWISE_CONCURRENCY: usize = 4;

/// The pairwise judge's verdict on one run of a variant against the same run of the baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum PairwiseVerdict {
    /// The baseline's run is better.
    Baseline,
    /// The variant's run is better.
    Candidate,
    /// Both are equally good.
    Tie,
    /// No comparison: a run failed or the judge could not compare.
    Error,
}

/// One comparison of a variant's run with the baseline's run of the same case and run number.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PairwiseResult {
    /// The compared (candidate) variant.
    pub variant: String,
    /// The case id.
    pub case: String,
    /// The 1-based run number, as in [`CaseRun::run`].
    pub run: u32,
    /// The judge's verdict.
    pub choice: PairwiseVerdict,
    /// The judge's explanation, or the error text for [`PairwiseVerdict::Error`].
    pub explanation: String,
}

impl PairwiseResult {
    /// A result from its parts.
    pub fn new(
        variant: impl Into<String>,
        case: impl Into<String>,
        run: u32,
        choice: PairwiseVerdict,
        explanation: impl Into<String>,
    ) -> Self {
        PairwiseResult {
            variant: variant.into(),
            case: case.into(),
            run,
            choice,
            explanation: explanation.into(),
        }
    }
}

/// One variant's pairwise counts against the baseline, from
/// [`ExperimentReport::pairwise_stats`](crate::ExperimentReport::pairwise_stats).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PairwiseStats {
    /// The compared variant.
    pub variant: String,
    /// The baseline (first) variant.
    pub baseline: String,
    /// Pairs the variant won ([`PairwiseVerdict::Candidate`]).
    pub wins: usize,
    /// Pairs the baseline won ([`PairwiseVerdict::Baseline`]).
    pub losses: usize,
    /// Ties.
    pub ties: usize,
    /// Pairs that could not be compared.
    pub errors: usize,
}

impl PairwiseStats {
    /// Counts from their parts.
    pub fn new(
        variant: impl Into<String>,
        baseline: impl Into<String>,
        wins: usize,
        losses: usize,
        ties: usize,
        errors: usize,
    ) -> Self {
        PairwiseStats {
            variant: variant.into(),
            baseline: baseline.into(),
            wins,
            losses,
            ties,
            errors,
        }
    }
}

/// One pair to compare: its sort key (variant, case and run), the candidate variant's name,
/// both runs and the criteria.
struct Job<'a> {
    key: (usize, usize, u32),
    variant: &'a str,
    base: &'a CaseRun,
    cand: &'a CaseRun,
    criteria: &'a str,
}

/// The criteria `case` is compared on: its first `llm_judge` rubric, else
/// [`DEFAULT_PAIRWISE_CRITERIA`].
fn criteria(case: &EvalCase) -> &str {
    case.expect
        .iter()
        .find_map(|s| match s {
            ScorerSpec::LlmJudge { rubric, .. } => Some(rubric.as_str()),
            _ => None,
        })
        .unwrap_or(DEFAULT_PAIRWISE_CRITERIA)
}

/// Compares every variant after the first in `report` with the first, for every `(case, run)`
/// both have, through `judge`: the criteria are the case's first `llm_judge` rubric (else
/// [`DEFAULT_PAIRWISE_CRITERIA`]), and the baseline and candidate are the rendered
/// conversations ([`render_conversation`]).
///
/// A pair in which either run failed is a [`PairwiseVerdict::Error`] (`baseline run failed: …`
/// or `candidate run failed: …`) and is not sent. At most 4 compare calls are in flight. The
/// results are ordered by variant, then case order in `set`, then run. Cases of `report` that
/// are not in `set` are skipped.
pub async fn run_pairwise(
    judge: &dyn PairwiseJudge,
    set: &EvalSet,
    report: &EvalReport,
) -> Vec<PairwiseResult> {
    let Some((baseline, candidates)) = report.variants.split_first() else {
        return Vec::new();
    };
    let mut jobs: Vec<Job<'_>> = Vec::new();
    for (vi, v) in candidates.iter().enumerate() {
        for (ci, case) in set.cases.iter().enumerate() {
            for cand in v.runs.iter().filter(|r| r.case == case.id) {
                let base = baseline
                    .runs
                    .iter()
                    .find(|b| b.case == case.id && b.run == cand.run);
                if let Some(base) = base {
                    jobs.push(Job {
                        key: (vi, ci, cand.run),
                        variant: &v.name,
                        base,
                        cand,
                        criteria: criteria(case),
                    });
                }
            }
        }
    }
    let mut done: Vec<((usize, usize, u32), PairwiseResult)> = futures::stream::iter(jobs)
        .map(|job| async move {
            let (choice, explanation) = compare(judge, job.criteria, job.base, job.cand).await;
            let (case, run) = (&job.cand.case, job.cand.run);
            (
                job.key,
                PairwiseResult::new(job.variant, case, run, choice, explanation),
            )
        })
        .buffer_unordered(PAIRWISE_CONCURRENCY)
        .collect()
        .await;
    done.sort_by_key(|(key, _)| *key);
    done.into_iter().map(|(_, r)| r).collect()
}

/// One pair's verdict and explanation; a failed run is an error without a call.
async fn compare(
    judge: &dyn PairwiseJudge,
    criteria: &str,
    base: &CaseRun,
    cand: &CaseRun,
) -> (PairwiseVerdict, String) {
    if let Some(e) = &base.transcript.error {
        return (PairwiseVerdict::Error, format!("baseline run failed: {e}"));
    }
    if let Some(e) = &cand.transcript.error {
        return (PairwiseVerdict::Error, format!("candidate run failed: {e}"));
    }
    let baseline = render_conversation(&base.transcript);
    let candidate = render_conversation(&cand.transcript);
    match judge.compare(criteria, &baseline, &candidate).await {
        PairwiseOutcome::Choice {
            choice,
            explanation,
        } => {
            let verdict = match choice {
                PairwiseChoice::Baseline => PairwiseVerdict::Baseline,
                PairwiseChoice::Candidate => PairwiseVerdict::Candidate,
                PairwiseChoice::Tie => PairwiseVerdict::Tie,
            };
            (verdict, explanation)
        }
        PairwiseOutcome::Error(e) => (PairwiseVerdict::Error, e),
    }
}

#[cfg(test)]
mod tests {
    use gadk_eval::{RunTranscript, TurnRecord, VariantReport};

    use super::*;
    use crate::testutil::FakePairwise;

    const SET: &str = r#"
name: pairs
cases:
  - id: a
    turns: ["hi"]
    expect: [contains: x, llm_judge: "Be brief", llm_judge: "Second"]
  - id: b
    turns: ["hi"]
    expect: [contains: x]
"#;

    fn run(case: &str, n: u32, reply: &str, error: Option<&str>) -> CaseRun {
        CaseRun {
            case: case.into(),
            run: n,
            transcript: RunTranscript {
                turns: vec![TurnRecord {
                    user: "hi".into(),
                    reply: reply.into(),
                    calls: vec![],
                }],
                error: error.map(str::to_owned),
            },
            scores: vec![],
        }
    }

    /// `name`'s runs of cases `b` and `a` (out of set order), two runs each, replying
    /// `<name>-<case><run>`.
    fn variant(name: &str) -> VariantReport {
        let mut runs = Vec::new();
        for case in ["b", "a"] {
            for n in [2, 1] {
                runs.push(run(case, n, &format!("{name}-{case}{n}"), None));
            }
        }
        VariantReport {
            name: name.into(),
            runs,
        }
    }

    fn report(variants: Vec<VariantReport>) -> EvalReport {
        EvalReport {
            set: "pairs".into(),
            variants,
        }
    }

    fn keys(results: &[PairwiseResult]) -> Vec<String> {
        results
            .iter()
            .map(|r| format!("{} {}#{}", r.variant, r.case, r.run))
            .collect()
    }

    #[tokio::test]
    async fn every_later_variant_is_compared_with_the_first_in_order() {
        let set = EvalSet::from_yaml(SET).unwrap();
        let judge = FakePairwise::new(PairwiseChoice::Candidate);
        let got = run_pairwise(
            judge.as_ref(),
            &set,
            &report(vec![variant("v0"), variant("v1"), variant("v2")]),
        )
        .await;
        assert_eq!(
            keys(&got),
            [
                "v1 a#1", "v1 a#2", "v1 b#1", "v1 b#2", "v2 a#1", "v2 a#2", "v2 b#1", "v2 b#2"
            ]
        );
        assert!(got.iter().all(|r| r.choice == PairwiseVerdict::Candidate));
        assert_eq!(got[0].explanation, "fake pairwise");
        let calls = judge.calls();
        assert_eq!(calls.len(), 8);
        let a1 = calls
            .iter()
            .find(|c| c.2.contains("v1-a1"))
            .expect("v1 a#1 was compared");
        assert_eq!(a1.0, "Be brief");
        assert_eq!(a1.1, "User: hi\nAgent: v0-a1\n\n");
        assert_eq!(a1.2, "User: hi\nAgent: v1-a1\n\n");
        let b1 = calls.iter().find(|c| c.2.contains("v2-b1")).unwrap();
        assert_eq!(b1.0, DEFAULT_PAIRWISE_CRITERIA);
        assert_eq!(judge.max_in_flight(), 4, "the comparisons run 4 at a time");
    }

    #[tokio::test]
    async fn failed_runs_are_errors_without_a_call() {
        let set = EvalSet::from_yaml(SET).unwrap();
        let mut base = variant("v0");
        base.runs[0].transcript.error = Some("boom".into()); // b#2
        let mut cand = variant("v1");
        cand.runs[3].transcript.error = Some("timed out".into()); // a#1
        let judge = FakePairwise::new(PairwiseChoice::Tie);
        let got = run_pairwise(judge.as_ref(), &set, &report(vec![base, cand])).await;
        let summary: Vec<(String, PairwiseVerdict, &str)> = got
            .iter()
            .map(|r| {
                (
                    format!("{}#{}", r.case, r.run),
                    r.choice,
                    r.explanation.as_str(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                (
                    "a#1".to_owned(),
                    PairwiseVerdict::Error,
                    "candidate run failed: timed out"
                ),
                ("a#2".to_owned(), PairwiseVerdict::Tie, "fake pairwise"),
                ("b#1".to_owned(), PairwiseVerdict::Tie, "fake pairwise"),
                (
                    "b#2".to_owned(),
                    PairwiseVerdict::Error,
                    "baseline run failed: boom"
                ),
            ]
        );
        assert_eq!(judge.calls().len(), 2);
    }

    #[tokio::test]
    async fn only_pairs_both_variants_have_are_compared() {
        let set = EvalSet::from_yaml(SET).unwrap();
        let base = variant("v0");
        let mut cand = variant("v1");
        cand.runs.retain(|r| r.case == "a" && r.run == 2);
        cand.runs.push(run("a", 3, "extra", None));
        let judge = FakePairwise::new(PairwiseChoice::Baseline);
        let got = run_pairwise(judge.as_ref(), &set, &report(vec![base, cand])).await;
        assert_eq!(keys(&got), ["v1 a#2"]);
        assert_eq!(got[0].choice, PairwiseVerdict::Baseline);
        let one = run_pairwise(judge.as_ref(), &set, &report(vec![variant("v0")])).await;
        assert!(one.is_empty());
    }

    #[tokio::test]
    async fn judge_errors_are_error_pairs() {
        let set = EvalSet::from_yaml(SET).unwrap();
        let judge = FakePairwise::failing("evaluation service: HTTP 403: denied");
        let got = run_pairwise(
            judge.as_ref(),
            &set,
            &report(vec![variant("v0"), variant("v1")]),
        )
        .await;
        assert_eq!(got.len(), 4);
        assert!(got.iter().all(|r| r.choice == PairwiseVerdict::Error
            && r.explanation == "evaluation service: HTTP 403: denied"));
    }

    #[test]
    fn verdicts_serialize_lowercase() {
        let r = PairwiseResult::new("v1", "a", 1, PairwiseVerdict::Candidate, "why");
        assert_eq!(
            serde_json::to_value(&r).unwrap(),
            serde_json::json!({
                "variant": "v1", "case": "a", "run": 1, "choice": "candidate", "explanation": "why"
            })
        );
        for (v, text) in [
            (PairwiseVerdict::Baseline, "\"baseline\""),
            (PairwiseVerdict::Tie, "\"tie\""),
            (PairwiseVerdict::Error, "\"error\""),
        ] {
            assert_eq!(serde_json::to_string(&v).unwrap(), text);
        }
    }
}
