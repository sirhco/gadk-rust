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
use crate::metrics::{MeasuredReport, RunMetrics};
use crate::report::{CaseRun, EvalReport, VariantReport};
use crate::result::RunTranscript;
use crate::run::run_case_measured;
use crate::score::backend::JudgeBackend;
use crate::score::judge::GeminiJudge;
use crate::score::score_scorers;
use crate::spec::EvalSet;
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
    judge: JudgeSource,
    concurrency: usize,
    runs_per_case: u32,
    run_timeout: Duration,
}

/// Where an [`Evaluator`]'s judge scorers are graded.
enum JudgeSource {
    /// A model, wrapped in a [`GeminiJudge`] bounded by the run timeout when a run starts.
    Model(Arc<dyn Llm>),
    /// Any backend, used as is.
    Backend(Arc<dyn JudgeBackend>),
}

impl Evaluator {
    /// An evaluator whose `llm_judge` and `reference` scorers ask `judge`.
    pub fn new(judge: Arc<dyn Llm>) -> Self {
        Self::from_source(JudgeSource::Model(judge))
    }

    /// An evaluator whose `llm_judge` and `reference` scorers are graded by `backend`; no judge
    /// model is needed. The run timeout does not bound `backend`'s calls; the backend bounds its
    /// own.
    pub fn with_backend(backend: Arc<dyn JudgeBackend>) -> Self {
        Self::from_source(JudgeSource::Backend(backend))
    }

    /// An evaluator grading through `judge`, with the default concurrency (4), runs per case (1)
    /// and run timeout (120 s).
    fn from_source(judge: JudgeSource) -> Self {
        Evaluator {
            judge,
            concurrency: 4,
            runs_per_case: 1,
            run_timeout: Duration::from_secs(120),
        }
    }

    /// Grades `llm_judge` and `reference` scorers through `backend` instead of the judge model
    /// given to [`Evaluator::new`].
    pub fn with_judge_backend(mut self, backend: Arc<dyn JudgeBackend>) -> Self {
        self.judge = JudgeSource::Backend(backend);
        self
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
        Ok(self.run_inner(set, variants, build).await?.report)
    }

    /// [`Evaluator::run`], plus every run's duration and token counts per model.
    ///
    /// The report is the one [`Evaluator::run`] returns for the same inputs; `metrics[v][i]`
    /// belongs to `report.variants[v].runs[i]`. The runs of a variant whose build failed get
    /// [`RunMetrics::default`]. Fails as [`Evaluator::run`] does.
    pub async fn run_measured<F, E>(
        &self,
        set: &EvalSet,
        variants: &[Variant],
        build: F,
    ) -> Result<MeasuredReport, EvalError>
    where
        F: Fn(&Variant) -> Result<Arc<dyn Agent>, E>,
        E: std::fmt::Display,
    {
        self.run_inner(set, variants, build).await
    }

    async fn run_inner<F, E>(
        &self,
        set: &EvalSet,
        variants: &[Variant],
        build: F,
    ) -> Result<MeasuredReport, EvalError>
    where
        F: Fn(&Variant) -> Result<Arc<dyn Agent>, E>,
        E: std::fmt::Display,
    {
        check_variants(variants)?;
        let agents: Vec<Result<Arc<dyn Agent>, String>> = variants
            .iter()
            .map(|v| build(v).map_err(|e| format!("building variant {}: {e}", v.name())))
            .collect();
        let gemini;
        let judge: &dyn JudgeBackend = match &self.judge {
            JudgeSource::Model(model) => {
                gemini = GeminiJudge::new(model.clone(), self.run_timeout);
                &gemini
            }
            JudgeSource::Backend(backend) => backend.as_ref(),
        };
        let mut jobs = Vec::new();
        for vi in 0..variants.len() {
            for ci in 0..set.cases.len() {
                for run in 1..=self.runs_per_case {
                    jobs.push((vi, ci, run));
                }
            }
        }
        let (agents, timeout) = (&agents, self.run_timeout);
        let mut done: Vec<((usize, usize, u32), CaseRun, RunMetrics)> = futures::stream::iter(jobs)
            .map(|(vi, ci, run)| async move {
                let case = &set.cases[ci];
                let (transcript, metrics) = match &agents[vi] {
                    Ok(agent) => run_case_measured(agent.clone(), case, timeout).await,
                    Err(e) => (
                        RunTranscript {
                            turns: Vec::new(),
                            error: Some(e.clone()),
                        },
                        RunMetrics::default(),
                    ),
                };
                let scores = score_scorers(judge, &case.expect, &transcript).await;
                (
                    (vi, ci, run),
                    CaseRun {
                        case: case.id.clone(),
                        run,
                        transcript,
                        scores,
                    },
                    metrics,
                )
            })
            .buffer_unordered(self.concurrency)
            .collect()
            .await;
        done.sort_by_key(|(key, _, _)| *key);
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
        let mut metrics: Vec<Vec<RunMetrics>> = vec![Vec::new(); variants.len()];
        for ((vi, _, _), run, m) in done {
            report.variants[vi].runs.push(run);
            metrics[vi].push(m);
        }
        Ok(MeasuredReport::new(report, metrics))
    }
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
