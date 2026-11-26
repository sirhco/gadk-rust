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

//! One function per check kind: each side's value, the harm and its interval, and the status.

use crate::guard_report::{CheckResult, SideValue, Status, Unit};
use crate::guard_source::{VersionCalls, VersionFeedback};
use crate::guard_spec::{FeedbackSource, MIN_EVAL_RUNS};
use crate::guard_stats::{Z95, anytime_z, classify, mixture_rho, newcombe_diff, welch_diff};
use crate::prices::PriceTable;
use crate::report::MonitoredRun;

/// The two versions of a rollout and their calls.
pub(super) struct Sides<'a> {
    /// The label's version.
    pub(super) baseline: u32,
    /// The rollout version.
    pub(super) rollout: u32,
    /// The baseline's calls (`None`: no calls).
    pub(super) b: Option<&'a VersionCalls>,
    /// The rollout's calls (`None`: no calls).
    pub(super) r: Option<&'a VersionCalls>,
    /// The fewest calls per side for any check to count.
    pub(super) min_calls: u64,
}

impl Sides<'_> {
    /// The calls of each side.
    fn calls(&self) -> (u64, u64) {
        (self.b.map_or(0, |c| c.calls), self.r.map_or(0, |c| c.calls))
    }

    /// Why the checks are `insufficient`, when a side has fewer than `min_calls` calls.
    pub(super) fn insufficient(&self) -> Option<String> {
        let (nb, nr) = self.calls();
        (nb < self.min_calls || nr < self.min_calls)
            .then(|| self.counts(&format!("fewer than {} calls", self.min_calls), nb, nr))
    }

    /// `<what>: v<B> <nb>, v<R> <nr>`.
    fn counts(&self, what: &str, nb: u64, nr: u64) -> String {
        format!("{what}: v{} {nb}, v{} {nr}", self.baseline, self.rollout)
    }
}

/// How the interval checks (`error_rate`, `feedback`, `eval`) pick their normal quantile z.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum ZRule {
    /// Neither `sequential` nor `confidence` is set: [`Z95`], not recorded in the report.
    Fixed95,
    /// `confidence` without `sequential`: this Bonferroni z for every interval check.
    Bonferroni(f64),
    /// `sequential`: the anytime z at level `a` from each check's total sample.
    Anytime {
        /// The per-check level α ÷ k.
        a: f64,
    },
}

/// The z `rule` gives a check whose two sides hold `n` calls, scores or scored runs in all and
/// whose anytime boundary is sharpest at `n_star`, and the z to record in the report (`None`
/// under [`ZRule::Fixed95`]).
pub(super) fn z_for(rule: &ZRule, n: u64, n_star: f64) -> (f64, Option<f64>) {
    match *rule {
        ZRule::Fixed95 => (Z95, None),
        ZRule::Bonferroni(z) => (z, Some(z)),
        ZRule::Anytime { a } => {
            let z = anytime_z(n, mixture_rho(n_star, a), a);
            (z, Some(z))
        }
    }
}

/// A result with both sides' values and no harm yet.
fn result(name: String, unit: Unit, baseline: SideValue, rollout: SideValue) -> CheckResult {
    CheckResult {
        name,
        unit,
        baseline,
        rollout,
        harm: None,
        interval: None,
        z: None,
        status: Status::Unknown,
        reason: None,
    }
}

fn side(value: Option<f64>, n: u64) -> SideValue {
    SideValue { value, n }
}

/// `successes / n`, `None` when `n` is 0.
fn rate(successes: u64, n: u64) -> Option<f64> {
    (n > 0).then(|| successes as f64 / n as f64)
}

/// Sets `out`'s status and reason.
fn status(mut out: CheckResult, status: Status, reason: Option<String>) -> CheckResult {
    out.status = status;
    out.reason = reason;
    out
}

/// Classifies the interval `[lower, upper]` of the harm `harm` against `tolerance`, recording
/// the interval's z `z` (`None` for the default).
fn interval(
    mut out: CheckResult,
    harm: f64,
    lower: f64,
    upper: f64,
    tolerance: f64,
    z: Option<f64>,
) -> CheckResult {
    out.harm = Some(harm);
    out.interval = Some((lower, upper));
    out.z = z;
    out.status = classify(lower, upper, tolerance);
    out
}

/// `error_rate`: h = rate(R) − rate(B), Newcombe's interval with `rule`'s z (n: both sides'
/// calls; n* = 2 × `min_calls`).
pub(super) fn error_rate(s: &Sides<'_>, max_increase: f64, rule: &ZRule) -> CheckResult {
    let (nb, nr) = s.calls();
    let eb = s.b.map_or(0, |c| c.errors);
    let er = s.r.map_or(0, |c| c.errors);
    let out = result(
        "error_rate".into(),
        Unit::Rate,
        side(rate(eb, nb), nb),
        side(rate(er, nr), nr),
    );
    if let Some(reason) = s.insufficient() {
        return status(out, Status::Insufficient, Some(reason));
    }
    let (z, used) = z_for(rule, nb + nr, 2.0 * s.min_calls as f64);
    let (lower, upper) = newcombe_diff(eb, nb, er, nr, z);
    let harm = er as f64 / nr as f64 - eb as f64 / nb as f64;
    interval(out, harm, lower, upper, max_increase, used)
}

/// One side's feedback rows merged across sources.
struct Pooled {
    /// The data types seen, sorted and without repeats.
    types: Vec<String>,
    count: u64,
    positive: u64,
    mean: Option<f64>,
    stddev: Option<f64>,
}

/// Merges the rows of `version`: counts and positives add up; number rows pool their means
/// (weighted by count) and sample standard deviations (within- plus between-row sums of
/// squares over `count − 1`).
fn pool(rows: &[VersionFeedback], version: u32) -> Pooled {
    let rows: Vec<&VersionFeedback> = rows.iter().filter(|r| r.version == version).collect();
    let mut types: Vec<String> = rows.iter().map(|r| r.data_type.clone()).collect();
    types.sort();
    types.dedup();
    let count = rows.iter().map(|r| r.count).sum();
    let positive = rows.iter().map(|r| r.positive).sum();
    let numbers: Vec<(f64, f64, f64)> = rows
        .iter()
        .filter(|r| r.count > 0)
        .filter_map(|r| r.mean.map(|m| (r.count as f64, m, r.stddev.unwrap_or(0.0))))
        .collect();
    let n: f64 = numbers.iter().map(|(c, _, _)| c).sum();
    let mean = (n > 0.0).then(|| numbers.iter().map(|(c, m, _)| c * m).sum::<f64>() / n);
    let stddev = match mean {
        Some(mean) if n >= 2.0 => {
            let squares: f64 = numbers
                .iter()
                .map(|(c, m, sd)| (c - 1.0) * sd * sd + c * (m - mean) * (m - mean))
                .sum();
            Some((squares / (n - 1.0)).sqrt())
        }
        _ => None,
    };
    Pooled {
        types,
        count,
        positive,
        mean,
        stddev,
    }
}

/// `feedback`: bool scores h = rate(B) − rate(R) with Newcombe's interval; number scores
/// h = (mean(B) − mean(R)) ÷ |mean(B)| with Welch's interval divided by |mean(B)|; both with
/// `rule`'s z (n: both sides' scores; n* = 2 × `min_count`).
pub(super) fn feedback(
    s: &Sides<'_>,
    name: &str,
    source: FeedbackSource,
    max_decrease: f64,
    min_count: u64,
    rows: &[VersionFeedback],
    rule: &ZRule,
) -> CheckResult {
    let b = pool(rows, s.baseline);
    let r = pool(rows, s.rollout);
    let mut all: Vec<&String> = b.types.iter().chain(&r.types).collect();
    all.sort();
    all.dedup();
    let number = all.iter().any(|t| *t == "number");
    let value = |p: &Pooled| {
        if number {
            p.mean
        } else {
            rate(p.positive, p.count)
        }
    };
    let out = result(
        format!("feedback {name} ({})", source.as_str()),
        if number { Unit::Number } else { Unit::Rate },
        side(value(&b), b.count),
        side(value(&r), r.count),
    );
    if let Some(reason) = s.insufficient() {
        return status(out, Status::Insufficient, Some(reason));
    }
    if all.iter().any(|t| *t == "category") {
        let reason = "category scores are not supported".to_owned();
        return status(out, Status::Unknown, Some(reason));
    }
    if all.len() > 1 {
        let reason = format!(
            "data types differ: v{} {}, v{} {}",
            s.baseline,
            b.types.join("+"),
            s.rollout,
            r.types.join("+")
        );
        return status(out, Status::Unknown, Some(reason));
    }
    if b.count < min_count || r.count < min_count {
        let reason = s.counts(&format!("fewer than {min_count} scores"), b.count, r.count);
        return status(out, Status::Insufficient, Some(reason));
    }
    let (z, used) = z_for(rule, b.count + r.count, 2.0 * min_count as f64);
    if !number {
        let (lower, upper) = newcombe_diff(r.positive, r.count, b.positive, b.count, z);
        let harm = b.positive as f64 / b.count as f64 - r.positive as f64 / r.count as f64;
        return interval(out, harm, lower, upper, max_decrease, used);
    }
    let (mb, mr) = (b.mean.unwrap_or(0.0), r.mean.unwrap_or(0.0));
    if mb == 0.0 {
        let reason = "the baseline mean is 0".to_owned();
        return status(out, Status::Unknown, Some(reason));
    }
    let scale = mb.abs();
    let (l, u) = welch_diff(
        mb,
        b.stddev.unwrap_or(0.0),
        b.count,
        mr,
        r.stddev.unwrap_or(0.0),
        r.count,
        z,
    );
    // (l, u) bounds mean(R) − mean(B); the harm is its negation, relative to |mean(B)|.
    interval(
        out,
        (mb - mr) / scale,
        -u / scale,
        -l / scale,
        max_decrease,
        used,
    )
}

/// Point harm `(r − b) ÷ b`: `worse` when above `max_increase`, else `ok`.
fn point(mut out: CheckResult, b: f64, r: f64, max_increase: f64) -> CheckResult {
    let harm = (r - b) / b;
    out.harm = Some(harm);
    out.status = if harm > max_increase {
        Status::Worse
    } else {
        Status::Ok
    };
    out
}

/// `cost_per_call`: each side's token cost ÷ calls, priced with `prices`; point harm.
pub(super) fn cost_per_call(
    s: &Sides<'_>,
    prices: Option<&PriceTable>,
    max_increase: f64,
) -> CheckResult {
    let (nb, nr) = s.calls();
    let models = s.b.into_iter().chain(s.r).flat_map(|c| c.models.iter());
    let missing = match prices {
        None => Some("no price file given".to_owned()),
        Some(table) => models
            .clone()
            .find(|m| table.price_for(&m.model).is_none())
            .map(|m| format!("no price for model {}", m.model)),
    };
    let cost = |c: Option<&VersionCalls>, n: u64| match (prices, &missing, c) {
        (Some(table), None, Some(c)) if n > 0 => {
            let total: f64 = c
                .models
                .iter()
                .filter_map(|m| table.price_for(&m.model).map(|p| m.usage.cost(p)))
                .sum();
            Some(total / n as f64)
        }
        _ => None,
    };
    let (vb, vr) = (cost(s.b, nb), cost(s.r, nr));
    let out = result(
        "cost_per_call".into(),
        Unit::Usd,
        side(vb, nb),
        side(vr, nr),
    );
    if let Some(reason) = s.insufficient() {
        return status(out, Status::Insufficient, Some(reason));
    }
    if missing.is_some() {
        return status(out, Status::Unknown, missing);
    }
    match (vb, vr) {
        (Some(b), Some(r)) if b != 0.0 => point(out, b, r, max_increase),
        _ => {
            let reason = "the baseline cost is 0".to_owned();
            status(out, Status::Unknown, Some(reason))
        }
    }
}

/// `latency_p95`: each side's p95 latency; point harm.
pub(super) fn latency_p95(s: &Sides<'_>, max_increase: f64) -> CheckResult {
    let (nb, nr) = s.calls();
    let vb = s.b.and_then(|c| c.p95_ms);
    let vr = s.r.and_then(|c| c.p95_ms);
    let out = result(
        "latency_p95".into(),
        Unit::Millis,
        side(vb, nb),
        side(vr, nr),
    );
    if let Some(reason) = s.insufficient() {
        return status(out, Status::Insufficient, Some(reason));
    }
    let reason = match (vb, vr) {
        (Some(b), Some(r)) if b != 0.0 => return point(out, b, r, max_increase),
        (Some(_), Some(_)) => "the baseline p95 latency is 0".to_owned(),
        (None, _) => format!("no latency for v{}", s.baseline),
        (Some(_), None) => format!("no latency for v{}", s.rollout),
    };
    status(out, Status::Unknown, Some(reason))
}

/// `eval`: pass rates of the scored runs (runs with a scorer error excluded and counted),
/// h = rate(B) − rate(R) with Newcombe's interval and `rule`'s z (n: both sides' scored runs;
/// n* = 2 × [`MIN_EVAL_RUNS`]). `runs` is `None` when the runs were not scored because a side
/// has too few calls.
pub(super) fn eval(
    s: &Sides<'_>,
    max_decrease: f64,
    runs: Option<&(Vec<MonitoredRun>, Vec<MonitoredRun>)>,
    rule: &ZRule,
) -> CheckResult {
    let tally = |runs: &[MonitoredRun]| {
        let errored = runs.iter().filter(|r| r.errored()).count() as u64;
        let passed = runs.iter().filter(|r| r.passed()).count() as u64;
        (runs.len() as u64 - errored, passed, errored)
    };
    let ((nb, pb, eb), (nr, pr, er)) = match runs {
        Some((b, r)) => (tally(b), tally(r)),
        None => ((0, 0, 0), (0, 0, 0)),
    };
    let out = result(
        "eval".into(),
        Unit::Rate,
        side(rate(pb, nb), nb),
        side(rate(pr, nr), nr),
    );
    let excluded = (eb + er > 0).then(|| s.counts("excluded runs with scorer errors", eb, er));
    let with_excluded = |reason: String| match &excluded {
        Some(e) => format!("{reason}; {e}"),
        None => reason,
    };
    if let Some(reason) = s.insufficient() {
        return status(out, Status::Insufficient, Some(reason));
    }
    if nb < MIN_EVAL_RUNS || nr < MIN_EVAL_RUNS {
        let reason = s.counts(&format!("fewer than {MIN_EVAL_RUNS} scored runs"), nb, nr);
        return status(out, Status::Insufficient, Some(with_excluded(reason)));
    }
    let (z, used) = z_for(rule, nb + nr, 2.0 * MIN_EVAL_RUNS as f64);
    let (lower, upper) = newcombe_diff(pr, nr, pb, nb, z);
    let harm = pb as f64 / nb as f64 - pr as f64 / nr as f64;
    let mut out = interval(out, harm, lower, upper, max_decrease, used);
    out.reason = excluded;
    out
}
