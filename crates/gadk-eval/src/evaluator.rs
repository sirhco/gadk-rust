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

//! The evaluator: runs an eval set against each variant of an agent and scores every run.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use gadk_agent::Agent;
use gadk_model::Llm;

use crate::error::EvalError;
use crate::report::{CaseRun, EvalReport, VariantReport};
use crate::result::{RunTranscript, ScoreResult};
use crate::run::run_case;
use crate::score::judge::Judge;
use crate::score::{check, tools};
use crate::spec::{EvalCase, EvalSet, ScorerSpec};
use crate::variant::{Variant, check_variants};

/// Runs eval sets. Defaults: concurrency 4, one run per case, 120 s per run.
///
/// ```no_run
/// # use std::sync::Arc;
/// # async fn demo(
/// #     judge: Arc<dyn gadk_model::Llm>,
/// #     build_agent: impl Fn(Option<&gadk_prompts::Selector>) -> Result<Arc<dyn gadk_agent::Agent>, String>,
/// # ) -> Result<(), Box<dyn std::error::Error>> {
/// use gadk_eval::{EvalSet, Evaluator, Variant};
/// use gadk_prompts::{Label, Selector};
///
/// let set = EvalSet::load("evals/support.yaml")?;
/// let report = Evaluator::new(judge)
///     .run(
///         &set,
///         &[
///             Variant::prompt("prod", Selector::Label(Label::production())),
///             Variant::prompt("v7", Selector::Version(7)),
///         ],
///         |v| build_agent(v.selector()),
///     )
///     .await?;
/// print!("{}", report.summary());
/// report.write_json("eval-out.json")?;
/// # Ok(()) }
/// ```
pub struct Evaluator {
    judge: Arc<dyn Llm>,
    concurrency: usize,
    runs_per_case: u32,
    run_timeout: Duration,
}

impl Evaluator {
    /// An evaluator whose `llm_judge` and `reference` scorers ask `judge`.
    pub fn new(judge: Arc<dyn Llm>) -> Self {
        Evaluator {
            judge,
            concurrency: 4,
            runs_per_case: 1,
            run_timeout: Duration::from_secs(120),
        }
    }

    /// At most `n` runs at once (values below 1 mean 1).
    pub fn concurrency(mut self, n: usize) -> Self {
        self.concurrency = n.max(1);
        self
    }

    /// Runs each case `n` times per variant (values below 1 mean 1).
    pub fn runs_per_case(mut self, n: u32) -> Self {
        self.runs_per_case = n.max(1);
        self
    }

    /// Bounds each run (all of a case's turns) and each judge call; a run or judge call that takes
    /// longer is an error (values below 1 ms mean 1 ms).
    pub fn run_timeout(mut self, d: Duration) -> Self {
        self.run_timeout = d.max(Duration::from_millis(1));
        self
    }

    /// Runs every case of `set` `runs_per_case` times against the agent `build` returns for each
    /// variant, and scores each run.
    ///
    /// `build` is called once per variant, before any run. A `build` error makes every run of
    /// that variant an error; it does not stop the other variants. Fails only when `variants` is
    /// empty or has empty or duplicate names, before `build` is called.
    pub async fn run<F, E>(
        &self,
        set: &EvalSet,
        variants: &[Variant],
        build: F,
    ) -> Result<EvalReport, EvalError>
    where
        F: Fn(&Variant) -> Result<Arc<dyn Agent>, E>,
        E: std::fmt::Display,
    {
        check_variants(variants)?;
        let agents: Vec<Result<Arc<dyn Agent>, String>> = variants
            .iter()
            .map(|v| build(v).map_err(|e| format!("building variant {}: {e}", v.name())))
            .collect();
        let judge = Judge::new(self.judge.clone(), self.run_timeout);
        let mut jobs = Vec::new();
        for vi in 0..variants.len() {
            for ci in 0..set.cases.len() {
                for run in 1..=self.runs_per_case {
                    jobs.push((vi, ci, run));
                }
            }
        }
        let (agents, judge, timeout) = (&agents, &judge, self.run_timeout);
        let mut done: Vec<((usize, usize, u32), CaseRun)> = futures::stream::iter(jobs)
            .map(|(vi, ci, run)| async move {
                let case = &set.cases[ci];
                let transcript = match &agents[vi] {
                    Ok(agent) => run_case(agent.clone(), case, timeout).await,
                    Err(e) => RunTranscript {
                        turns: Vec::new(),
                        error: Some(e.clone()),
                    },
                };
                let scores = score_run(judge, case, &transcript).await;
                (
                    (vi, ci, run),
                    CaseRun {
                        case: case.id.clone(),
                        run,
                        transcript,
                        scores,
                    },
                )
            })
            .buffer_unordered(self.concurrency)
            .collect()
            .await;
        done.sort_by_key(|(key, _)| *key);
        let mut report = EvalReport {
            set: set.name.clone(),
            variants: variants
                .iter()
                .map(|v| VariantReport {
                    name: v.name().to_owned(),
                    runs: Vec::new(),
                })
                .collect(),
        };
        for ((vi, _, _), run) in done {
            report.variants[vi].runs.push(run);
        }
        Ok(report)
    }
}

/// Scores one run: every scorer errors when the run failed; otherwise each scorer grades it.
async fn score_run(judge: &Judge, case: &EvalCase, t: &RunTranscript) -> Vec<ScoreResult> {
    if let Some(e) = &t.error {
        return case
            .expect
            .iter()
            .map(|s| ScoreResult::error(s.kind(), e.clone()))
            .collect();
    }
    let mut out = Vec::with_capacity(case.expect.len());
    for s in &case.expect {
        out.push(match s {
            ScorerSpec::Contains {
                text,
                ignore_case,
                turn,
            } => check::contains(t.reply(*turn), text, *ignore_case, false),
            ScorerSpec::NotContains {
                text,
                ignore_case,
                turn,
            } => check::contains(t.reply(*turn), text, *ignore_case, true),
            ScorerSpec::Regex { pattern, turn } => check::regex(t.reply(*turn), pattern),
            ScorerSpec::Equals { text, turn } => check::equals(t.reply(*turn), text),
            ScorerSpec::Tools {
                calls,
                ordered,
                exact,
            } => tools::tools(&t.all_calls(), calls, *ordered, *exact),
            ScorerSpec::Reference {
                answer,
                min_score,
                turn,
            } => judge.reference(t, *turn, answer, *min_score).await,
            ScorerSpec::LlmJudge { rubric, min_score } => judge.rubric(t, rubric, *min_score).await,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use gadk_engine::testutil::MockModel;

    #[test]
    fn run_timeout_is_at_least_one_millisecond() {
        let e = Evaluator::new(MockModel::from_contents(vec![]));
        assert_eq!(
            e.run_timeout(Duration::ZERO).run_timeout,
            Duration::from_millis(1)
        );
        let e = Evaluator::new(MockModel::from_contents(vec![]));
        assert_eq!(
            e.run_timeout(Duration::from_secs(5)).run_timeout,
            Duration::from_secs(5)
        );
    }
}
