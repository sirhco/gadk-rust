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

//! The guard runner: compares both sides of a prompt rollout, decides, and optionally writes
//! the decision to the prompt store.

mod checks;
#[cfg(test)]
mod tests;

use std::sync::Arc;

use chrono::{DateTime, TimeDelta, Utc};
use gadk_model::Llm;
use gadk_prompts::{Label, PromptStore, Rollout, StoreError};

use crate::error::MonitorError;
use crate::guard_report::{
    Action, CheckResult, GuardReport, GuardStats, Status, Verdict, duration_text,
};
use crate::guard_source::{GuardQuery, GuardSource};
use crate::guard_spec::{Check, GuardSpec};
use crate::guard_stats::bonferroni_z;
use crate::monitor::{DEFAULT_CONCURRENCY, score_runs};
use crate::prices::PriceTable;
use crate::report::Window;
use crate::spec::DEFAULT_JUDGE_TIMEOUT;

use checks::{Sides, ZRule};

/// Runs a [`GuardSpec`]: reads the label's version and rollout from the prompt store, compares
/// both versions' calls over the spec's window, decides whether to advance, abort or wait, and
/// with `apply` writes the decision.
///
/// ```no_run
/// # async fn demo(
/// #     store: &dyn gadk_prompts::PromptStore,
/// #     source: &dyn gadk_eval_monitor::GuardSource,
/// # ) -> Result<(), gadk_eval_monitor::MonitorError> {
/// use gadk_eval_monitor::{Guard, GuardSpec};
///
/// let spec = GuardSpec::load("guards/support.yaml")?;
/// match Guard::new().run(&spec, store, source, None, None, None, false).await? {
///     Some(report) => print!("{}", report.summary()),
///     None => println!("{}/{} has no rollout", spec.prompt, spec.label),
/// }
/// # Ok(()) }
/// ```
#[derive(Debug, Clone)]
pub struct Guard {
    now: Option<DateTime<Utc>>,
    concurrency: usize,
}

impl Default for Guard {
    fn default() -> Self {
        Self::new()
    }
}

impl Guard {
    /// A guard over the window ending now, scoring [`DEFAULT_CONCURRENCY`] eval runs at once.
    pub fn new() -> Self {
        Self {
            now: None,
            concurrency: DEFAULT_CONCURRENCY,
        }
    }

    /// Uses `t` as the end of the window and the report time instead of the current time.
    pub fn now(mut self, t: DateTime<Utc>) -> Self {
        self.now = Some(t);
        self
    }

    /// Scores at most `n` eval runs at once (values below 1 mean 1).
    pub fn concurrency(mut self, n: usize) -> Self {
        self.concurrency = n.max(1);
        self
    }

    /// Runs `spec`; `Ok(None)` when the prompt and label exist but the label has no rollout
    /// (nothing to guard).
    ///
    /// Reads the label's version B and its rollout (version R at p%) from `store` in one
    /// `load_prompt`, gets both versions' calls in `[now − window, now)` from
    /// `source`, and classifies each check: `insufficient` when a side has fewer than
    /// `min_calls` calls (then neither feedback nor eval runs are queried), for `feedback`
    /// fewer than `min_count` scores, for `eval` fewer
    /// than [`MIN_EVAL_RUNS`](crate::guard_spec::MIN_EVAL_RUNS) scored runs; `unknown` when a
    /// value cannot be computed; else `worse`, `ok` or `unsure` from the one-sided interval of
    /// the rollout's harm (cost and p95 latency: the point harm, `worse` or `ok`). Each bound is
    /// 95% on its own; with the spec's `confidence` the `error_rate`, `feedback` and `eval`
    /// checks share 1 − confidence (Bonferroni), and with `sequential` each of their intervals
    /// is also anytime-valid (the report's [`stats`](GuardReport::stats) and each check's `z`
    /// record this). The
    /// verdict is `abort` when a check is `worse`, `advance` when all are `ok`, else `wait`;
    /// `advance` ramps to the first `ramp` step above p, or promotes R without one.
    ///
    /// `previous` is the newest stored report of the same spec that applied a ramp step
    /// (the CLI skips held, waiting, dry-run and other reports; one made after `now` counts
    /// as made at `now`). With the spec's `min_step_age` set, an `advance` is held back
    /// (verdict `wait`, action none, and a [`note`](GuardReport::note)
    /// `step at <q>% is <age> old (< min_step_age <min>)`) when `previous` applied `Ramp(q)`
    /// to the same prompt, label and rollout version, the rollout is still at q%, and
    /// `previous` was made less than `min_step_age` before now. An `abort` is never held back.
    ///
    /// `eval` checks score up to `limit` runs per version with `judge` for `llm_judge`
    /// scorers; runs with a scorer error are left out of the pass rates and counted in the
    /// reason. `prices` prices `cost_per_call`; without it that check is `unknown`.
    ///
    /// With `apply`, a ramp or abort is a `set_rollout_if` expecting B and the rollout read,
    /// and a promote re-reads the store (the label must still be at B with the same rollout)
    /// and then calls `promote_rollout_if` expecting B and the rollout read, which moves the
    /// label to R and ends the rollout in one atomic write. Without `apply` nothing is written.
    ///
    /// # Errors
    ///
    /// [`MonitorError::Store`] when the prompt or the label is missing (naming the prompt
    /// and label), when the store fails (`prompt store: …`), and when it changed
    /// since it was read (`prompt store changed since it was read (v<B>, v<R> at <p>%): …`,
    /// nothing written); [`MonitorError::Judge`] when an `eval` check has an `llm_judge`
    /// scorer and `judge` is `None`; the source's errors.
    #[allow(clippy::too_many_arguments)]
    pub async fn run(
        &self,
        spec: &GuardSpec,
        store: &dyn PromptStore,
        source: &dyn GuardSource,
        prices: Option<&PriceTable>,
        judge: Option<Arc<dyn Llm>>,
        previous: Option<&GuardReport>,
        apply: bool,
    ) -> Result<Option<GuardReport>, MonitorError> {
        if spec.wants_judge() && judge.is_none() {
            return Err(MonitorError::Judge(
                "the spec has llm_judge scorers but no judge model was given".into(),
            ));
        }
        let (baseline, Some(read)) = read_rollout(store, &spec.prompt, &spec.label).await? else {
            return Ok(None);
        };
        let (rule, stats) = z_rule(spec);
        let now = self.now.unwrap_or_else(Utc::now);
        let from = now - spec.window;
        let q = GuardQuery::new(
            spec.prompt.clone(),
            spec.label.as_str(),
            baseline,
            read.version,
            from,
            now,
        );
        let calls = source.version_calls(&q).await?;
        let sides = Sides {
            baseline,
            rollout: read.version,
            b: calls.iter().find(|c| c.version == baseline),
            r: calls.iter().find(|c| c.version == read.version),
            min_calls: spec.min_calls,
        };
        let timeout = spec
            .judge
            .as_ref()
            .map_or(DEFAULT_JUDGE_TIMEOUT, |j| j.timeout);
        let mut results: Vec<CheckResult> = Vec::with_capacity(spec.checks.len());
        for check in &spec.checks {
            let result = match check {
                Check::ErrorRate { max_increase } => {
                    checks::error_rate(&sides, *max_increase, &rule)
                }
                Check::Feedback {
                    name,
                    source: from_source,
                    max_decrease,
                    min_count,
                } => {
                    let rows = match sides.insufficient() {
                        Some(_) => Vec::new(),
                        None => source.version_feedback(&q, name, *from_source).await?,
                    };
                    checks::feedback(
                        &sides,
                        name,
                        *from_source,
                        *max_decrease,
                        *min_count,
                        &rows,
                        &rule,
                    )
                }
                Check::CostPerCall { max_increase } => {
                    checks::cost_per_call(&sides, prices, *max_increase)
                }
                Check::LatencyP95 { max_increase } => checks::latency_p95(&sides, *max_increase),
                Check::Eval {
                    scorers,
                    max_decrease,
                    limit,
                } => {
                    let runs = match sides.insufficient() {
                        Some(_) => None,
                        None => {
                            let mut scored = Vec::with_capacity(2);
                            for version in [baseline, read.version] {
                                let runs = source.version_runs(&q, version, *limit).await?;
                                scored.push(
                                    score_runs(
                                        judge.clone(),
                                        timeout,
                                        scorers,
                                        runs,
                                        self.concurrency,
                                    )
                                    .await?,
                                );
                            }
                            let r = scored.pop().unwrap_or_default();
                            let b = scored.pop().unwrap_or_default();
                            Some((b, r))
                        }
                    };
                    checks::eval(&sides, *max_decrease, runs.as_ref(), &rule)
                }
            };
            results.push(result);
        }
        let mut verdict = verdict(&results);
        let mut action = action(verdict, &spec.ramp, read.percent);
        let note = match (verdict, spec.min_step_age, previous) {
            (Verdict::Advance, Some(min), Some(p)) => young_step(spec, read, p, now, min),
            _ => None,
        };
        if note.is_some() {
            verdict = Verdict::Wait;
            action = Action::None;
        }
        if apply {
            write(store, &spec.prompt, &spec.label, baseline, read, action).await?;
        }
        Ok(Some(GuardReport {
            spec: spec.name.clone(),
            generated_at: now,
            window: Window { from, to: now },
            source: spec.source.describe(),
            prompt: spec.prompt.clone(),
            label: spec.label.as_str().to_owned(),
            baseline,
            rollout: read.version,
            percent: read.percent,
            checks: results,
            verdict,
            action,
            applied: apply && action != Action::None,
            note,
            stats,
        }))
    }
}

/// The z rule of `spec`'s interval checks (`error_rate`, `feedback` and `eval`) and the
/// report's stats: [`ZRule::Fixed95`] and `None` unless the spec sets `sequential` or
/// `confidence`. With k interval checks and α = 1 − confidence, each check gets level α ÷ k:
/// the Bonferroni z, or with `sequential` the anytime z (no z is computed when k is 0).
fn z_rule(spec: &GuardSpec) -> (ZRule, Option<GuardStats>) {
    let Some(confidence) = spec.effective_confidence() else {
        return (ZRule::Fixed95, None);
    };
    let k = spec
        .checks
        .iter()
        .filter(|c| {
            matches!(
                c,
                Check::ErrorRate { .. } | Check::Feedback { .. } | Check::Eval { .. }
            )
        })
        .count();
    let k = u32::try_from(k).unwrap_or(u32::MAX);
    let alpha = 1.0 - confidence;
    let rule = match (k, spec.sequential) {
        (0, _) => ZRule::Fixed95,
        (_, false) => ZRule::Bonferroni(bonferroni_z(alpha, k)),
        (_, true) => ZRule::Anytime {
            a: alpha / f64::from(k),
        },
    };
    let stats = GuardStats::new(spec.sequential, confidence, k, spec.window);
    (rule, Some(stats))
}

/// `step at <q>% is <age> old (< min_step_age <min>)` when `previous` applied the ramp step
/// the rollout `read` is still at, on the same prompt, label and rollout version, less than
/// `min` before `now`; else `None`.
fn young_step(
    spec: &GuardSpec,
    read: Rollout,
    previous: &GuardReport,
    now: DateTime<Utc>,
    min: TimeDelta,
) -> Option<String> {
    let Action::Ramp(q) = previous.action else {
        return None;
    };
    // A report from the future (clock skew) counts as made now.
    let age = (now - previous.generated_at).max(TimeDelta::zero());
    (previous.applied
        && previous.prompt == spec.prompt
        && previous.label == spec.label.as_str()
        && previous.rollout == read.version
        && read.percent == q
        && age < min)
        .then(|| {
            format!(
                "step at {q}% is {} old (< min_step_age {})",
                duration_text(age),
                duration_text(min)
            )
        })
}

/// `abort` when a check is `worse`, `advance` when every check is `ok`, else `wait`.
fn verdict(results: &[CheckResult]) -> Verdict {
    if results.iter().any(|c| c.status == Status::Worse) {
        Verdict::Abort
    } else if results.iter().all(|c| c.status == Status::Ok) {
        Verdict::Advance
    } else {
        Verdict::Wait
    }
}

/// The action for `verdict` at `percent`: `advance` ramps to the first `ramp` step above
/// `percent`, else promotes.
fn action(verdict: Verdict, ramp: &[u8], percent: u8) -> Action {
    match verdict {
        Verdict::Advance => match ramp.iter().find(|q| **q > percent) {
            Some(q) => Action::Ramp(*q),
            None => Action::Promote,
        },
        Verdict::Abort => Action::Abort,
        Verdict::Wait => Action::None,
    }
}

/// `prompt store: <error>`.
fn store_error(e: StoreError) -> MonitorError {
    MonitorError::Store(format!("prompt store: {e}"))
}

/// The label's version and its rollout (`None`: no rollout), read with one `load_prompt`.
async fn read_rollout(
    store: &dyn PromptStore,
    prompt: &str,
    label: &Label,
) -> Result<(u32, Option<Rollout>), MonitorError> {
    let (prompts, rollouts) = store.load_prompt(prompt).await.map_err(store_error)?;
    let named = |what: &str| MonitorError::Store(format!("prompt store: {prompt}/{label}: {what}"));
    let entry = prompts
        .prompts
        .get(prompt)
        .ok_or_else(|| named("no such prompt"))?;
    let baseline = *entry
        .labels
        .get(label)
        .ok_or_else(|| named("no such label"))?;
    Ok((baseline, rollouts.get(prompt, label).copied()))
}

/// Writes `action` with compare-and-set on what was read (`baseline` and `read`).
async fn write(
    store: &dyn PromptStore,
    prompt: &str,
    label: &Label,
    baseline: u32,
    read: Rollout,
    action: Action,
) -> Result<(), MonitorError> {
    let changed = |e: StoreError| {
        MonitorError::Store(format!(
            "prompt store changed since it was read (v{baseline}, v{} at {}%): {e}",
            read.version, read.percent
        ))
    };
    let fail = |e: StoreError| match e {
        StoreError::Conflict { .. } => changed(e),
        other => store_error(other),
    };
    match action {
        Action::Ramp(q) => {
            let next = Rollout::new(read.version, q).map_err(store_error)?;
            store
                .set_rollout_if(prompt, label, Some(next), baseline, Some(read))
                .await
                .map_err(fail)
        }
        Action::Abort => store
            .set_rollout_if(prompt, label, None, baseline, Some(read))
            .await
            .map_err(fail),
        Action::Promote => {
            let (prompts, rollouts) = store.load_prompt(prompt).await.map_err(store_error)?;
            let now = prompts
                .prompts
                .get(prompt)
                .and_then(|e| e.labels.get(label))
                .copied();
            if now != Some(baseline) {
                return Err(changed(StoreError::Conflict {
                    expected: Some(baseline),
                    actual: now,
                }));
            }
            let current = rollouts.get(prompt, label).copied();
            if current != Some(read) {
                return Err(changed(StoreError::Conflict {
                    expected: Some(read.version),
                    actual: current.map(|r| r.version),
                }));
            }
            store
                .promote_rollout_if(prompt, label, baseline, read)
                .await
                .map_err(fail)
        }
        Action::None => Ok(()),
    }
}
