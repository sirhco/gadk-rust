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

//! The guard report: each check's values on both sides of a rollout, the verdict and the
//! action, as text and JSON.

use std::fmt::Write as _;

use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};

use crate::error::MonitorError;
use crate::report::Window;

/// The outcome of one check.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// The rollout is no worse than the tolerance allows.
    Ok,
    /// The rollout is worse than the tolerance allows.
    Worse,
    /// The interval of harm straddles the tolerance: more data is needed.
    Unsure,
    /// A side has too few calls, scores or scored runs.
    Insufficient,
    /// The check cannot be computed (see the reason).
    Unknown,
}

impl Status {
    /// The status as printed: `ok`, `worse`, `unsure`, `insufficient` or `unknown`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Worse => "worse",
            Status::Unsure => "unsure",
            Status::Insufficient => "insufficient",
            Status::Unknown => "unknown",
        }
    }
}

/// What the checks say about the rollout as a whole.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Every check is `ok`: ramp up or promote.
    Advance,
    /// A check is `worse`: remove the rollout.
    Abort,
    /// Neither: run the guard again later.
    Wait,
}

impl Verdict {
    /// The verdict as printed: `advance`, `abort` or `wait`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Verdict::Advance => "advance",
            Verdict::Abort => "abort",
            Verdict::Wait => "wait",
        }
    }
}

/// The prompt-store change a verdict calls for.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Set the rollout's percent to this ramp step.
    Ramp(u8),
    /// Point the label at the rollout version (which clears the rollout).
    Promote,
    /// Remove the rollout.
    Abort,
    /// Change nothing.
    None,
}

/// How a check's values print.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    /// A proportion (`1.20%`); its harm is absolute, in percentage points (`+0.20pt`).
    Rate,
    /// A plain number (`4.10`); its harm is relative (`+12.0%`).
    Number,
    /// US dollars (`$0.000123`); its harm is relative.
    Usd,
    /// Milliseconds (`1234 ms`); its harm is relative.
    Millis,
}

impl Unit {
    /// A value in this unit: `1.20%` (a rate times 100), `4.10`, `$0.000123` or `1234 ms`, or
    /// `—` when `v` is `None`.
    pub fn format_value(self, v: Option<f64>) -> String {
        let Some(v) = v else {
            return "—".to_owned();
        };
        match self {
            Unit::Rate => format!("{:.2}%", v * 100.0),
            Unit::Number => format!("{v:.2}"),
            Unit::Usd => format!("${v:.6}"),
            Unit::Millis => format!("{v:.0} ms"),
        }
    }

    /// A harm in this unit: signed percentage points for a rate (`+0.20pt`), a signed percent
    /// otherwise (`+12.0%`), or `—` when `h` is `None`.
    pub fn format_harm(self, h: Option<f64>) -> String {
        match (self, h) {
            (_, None) => "—".to_owned(),
            (Unit::Rate, Some(h)) => format!("{:+.2}pt", h * 100.0),
            (_, Some(h)) => format!("{:+.1}%", h * 100.0),
        }
    }
}

/// One side's value of a check and the sample size behind it.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SideValue {
    /// The value (rate, mean, cost per call or latency), when there is one.
    pub value: Option<f64>,
    /// The calls, scores or scored runs counted.
    pub n: u64,
}

/// The result of one check.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckResult {
    /// The check as printed, for example `error_rate`, `feedback thumbs_up (any)` or `eval`.
    pub name: String,
    /// How the values and the harm print.
    pub unit: Unit,
    /// The label's version.
    pub baseline: SideValue,
    /// The rollout version.
    pub rollout: SideValue,
    /// The rollout's harm, positive = worse: absolute for rates, relative to the baseline
    /// otherwise.
    pub harm: Option<f64>,
    /// The one-sided 95% bounds of the harm, when an interval was computed.
    pub interval: Option<(f64, f64)>,
    /// The outcome.
    pub status: Status,
    /// Why the status is `insufficient` or `unknown`, or other detail.
    pub reason: Option<String>,
}

/// The result of one guard run.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuardReport {
    /// The guard spec's name.
    pub spec: String,
    /// When the report was made (the end of the window).
    pub generated_at: DateTime<Utc>,
    /// The window the calls were made in.
    pub window: Window,
    /// The source, for example `bigquery:<project>.<dataset>.<table>`.
    pub source: String,
    /// The prompt name.
    pub prompt: String,
    /// The guarded label.
    pub label: String,
    /// The label's version.
    pub baseline: u32,
    /// The rollout version.
    pub rollout: u32,
    /// The rollout percent when the guard read it.
    pub percent: u8,
    /// One result per check, in spec order.
    pub checks: Vec<CheckResult>,
    /// The verdict.
    pub verdict: Verdict,
    /// The action the verdict calls for.
    pub action: Action,
    /// Whether the action was written to the prompt store.
    pub applied: bool,
    /// Why the verdict differs from what the checks say, for example
    /// `step at 25% is 3h old (< min_step_age 1d)` when `min_step_age` held back an `advance`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl GuardReport {
    /// A text summary: a header line, one row per check (baseline and rollout values with
    /// their sample sizes, harm, status and reason), the verdict, the note (when there is one)
    /// and the action.
    pub fn summary(&self) -> String {
        let mut out = format!(
            "guard {}: {}/{} v{} → v{} ({}%), window {}\n",
            self.spec,
            self.prompt,
            self.label,
            self.baseline,
            self.rollout,
            self.percent,
            duration_text(self.window.to - self.window.from)
        );
        let header = ["check", "baseline", "rollout", "harm"];
        let rows: Vec<[String; 5]> = self
            .checks
            .iter()
            .map(|c| {
                let status = match &c.reason {
                    Some(reason) => format!("{}: {reason}", c.status.as_str()),
                    None => c.status.as_str().to_owned(),
                };
                [
                    c.name.clone(),
                    side(c.unit, &c.baseline),
                    side(c.unit, &c.rollout),
                    c.unit.format_harm(c.harm),
                    status,
                ]
            })
            .collect();
        let mut widths = header.map(|h| h.chars().count());
        for row in &rows {
            for (w, cell) in widths.iter_mut().zip(row.iter()) {
                *w = (*w).max(cell.chars().count());
            }
        }
        let [w0, w1, w2, w3] = widths;
        let _ = writeln!(
            out,
            "  {:<w0$}  {:<w1$}  {:<w2$}  {:<w3$}  status",
            header[0], header[1], header[2], header[3]
        );
        for [name, baseline, rollout, harm, status] in &rows {
            let _ = writeln!(
                out,
                "  {name:<w0$}  {baseline:<w1$}  {rollout:<w2$}  {harm:<w3$}  {status}"
            );
        }
        let _ = writeln!(out, "verdict: {}", self.verdict.as_str());
        if let Some(note) = &self.note {
            let _ = writeln!(out, "note: {note}");
        }
        let _ = writeln!(out, "action: {}", self.action_text());
        out
    }

    /// The action as printed, by whether it was applied: `would ramp to 25%` / `ramped to 25%`,
    /// `would promote v4` / `promoted v4` (the rollout version), `would abort` / `aborted`, or
    /// `none`.
    pub fn action_text(&self) -> String {
        match (self.action, self.applied) {
            (Action::Ramp(q), false) => format!("would ramp to {q}%"),
            (Action::Ramp(q), true) => format!("ramped to {q}%"),
            (Action::Promote, false) => format!("would promote v{}", self.rollout),
            (Action::Promote, true) => format!("promoted v{}", self.rollout),
            (Action::Abort, false) => "would abort".to_owned(),
            (Action::Abort, true) => "aborted".to_owned(),
            (Action::None, _) => "none".to_owned(),
        }
    }

    /// The process exit code for this report: 0 for `advance`, 1 for `abort`, 3 for `wait`.
    pub fn exit_code(&self) -> i32 {
        match self.verdict {
            Verdict::Advance => 0,
            Verdict::Abort => 1,
            Verdict::Wait => 3,
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
    /// [`MonitorError::Io`] (`InvalidData`) when `json` is not a guard report.
    pub fn from_json(json: &str) -> Result<Self, MonitorError> {
        serde_json::from_str(json).map_err(|e| MonitorError::Io(e.into()))
    }
}

/// A duration in whole minutes (rounded down): `<n>d` when whole days, else `<n>h` when
/// whole hours, else `<n>m`.
pub(crate) fn duration_text(d: TimeDelta) -> String {
    let minutes = d.num_minutes();
    if minutes != 0 && minutes % (24 * 60) == 0 {
        format!("{}d", minutes / (24 * 60))
    } else if minutes != 0 && minutes % 60 == 0 {
        format!("{}h", minutes / 60)
    } else {
        format!("{minutes}m")
    }
}

/// One side's cell: the value and `(n=<n>)`.
fn side(unit: Unit, v: &SideValue) -> String {
    format!("{} (n={})", unit.format_value(v.value), v.n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).unwrap()
    }

    fn sv(value: Option<f64>, n: u64) -> SideValue {
        SideValue { value, n }
    }

    fn check(
        name: &str,
        unit: Unit,
        baseline: SideValue,
        rollout: SideValue,
        harm: Option<f64>,
        status: Status,
        reason: Option<&str>,
    ) -> CheckResult {
        CheckResult {
            name: name.into(),
            unit,
            baseline,
            rollout,
            harm,
            interval: harm.map(|h| (h - 0.01, h + 0.01)),
            status,
            reason: reason.map(str::to_owned),
        }
    }

    fn report() -> GuardReport {
        let to = at(1_800_000_000);
        GuardReport {
            spec: "support-guard".into(),
            generated_at: to,
            window: Window {
                from: to - TimeDelta::hours(24),
                to,
            },
            source: "bigquery:my-proj.agent_analytics.agent_events".into(),
            prompt: "support/answer".into(),
            label: "production".into(),
            baseline: 3,
            rollout: 4,
            percent: 10,
            checks: vec![
                check(
                    "error_rate",
                    Unit::Rate,
                    sv(Some(0.012), 1000),
                    sv(Some(0.014), 120),
                    Some(0.002),
                    Status::Ok,
                    None,
                ),
                check(
                    "feedback stars (any)",
                    Unit::Number,
                    sv(Some(4.1), 300),
                    sv(Some(3.6), 40),
                    Some(0.12195),
                    Status::Worse,
                    None,
                ),
                check(
                    "cost_per_call",
                    Unit::Usd,
                    sv(Some(0.000123), 1000),
                    sv(None, 120),
                    None,
                    Status::Unknown,
                    Some("no price for model gemini-x"),
                ),
                check(
                    "latency_p95",
                    Unit::Millis,
                    sv(Some(1234.4), 1000),
                    sv(Some(1100.0), 120),
                    Some(-0.1089),
                    Status::Ok,
                    None,
                ),
                check(
                    "eval",
                    Unit::Rate,
                    sv(Some(0.9), 8),
                    sv(None, 0),
                    None,
                    Status::Insufficient,
                    Some("fewer than 10 scored runs"),
                ),
            ],
            verdict: Verdict::Abort,
            action: Action::Abort,
            applied: false,
            note: None,
        }
    }

    #[test]
    fn summary_golden() {
        let expected = "\
guard support-guard: support/answer/production v3 → v4 (10%), window 1d
  check                 baseline            rollout          harm     status
  error_rate            1.20% (n=1000)      1.40% (n=120)    +0.20pt  ok
  feedback stars (any)  4.10 (n=300)        3.60 (n=40)      +12.2%   worse
  cost_per_call         $0.000123 (n=1000)  — (n=120)        —        unknown: no price for model gemini-x
  latency_p95           1234 ms (n=1000)    1100 ms (n=120)  -10.9%   ok
  eval                  90.00% (n=8)        — (n=0)          —        insufficient: fewer than 10 scored runs
verdict: abort
action: would abort
";
        assert_eq!(report().summary(), expected);
    }

    #[test]
    fn summary_prints_the_note_after_the_verdict() {
        let mut r = report();
        r.verdict = Verdict::Wait;
        r.action = Action::None;
        r.note = Some("step at 10% is 3h old (< min_step_age 1d)".into());
        let summary = r.summary();
        assert!(
            summary.ends_with(
                "verdict: wait\nnote: step at 10% is 3h old (< min_step_age 1d)\naction: none\n"
            ),
            "{summary}"
        );
    }

    #[test]
    fn durations_print_in_the_largest_whole_unit() {
        for (d, text) in [
            (TimeDelta::days(7), "7d"),
            (TimeDelta::hours(36), "36h"),
            (TimeDelta::minutes(90), "90m"),
            (TimeDelta::minutes(45), "45m"),
            (TimeDelta::seconds(179), "2m"),
            (TimeDelta::seconds(59), "0m"),
        ] {
            assert_eq!(duration_text(d), text);
        }
    }

    #[test]
    fn summary_window_lengths() {
        let mut r = report();
        let to = r.window.to;
        for (length, text) in [
            (TimeDelta::days(7), "window 7d"),
            (TimeDelta::hours(36), "window 36h"),
            (TimeDelta::minutes(90), "window 90m"),
            (TimeDelta::minutes(45), "window 45m"),
        ] {
            r.window.from = to - length;
            let summary = r.summary();
            let first = summary.lines().next().unwrap();
            assert!(first.ends_with(text), "{first}");
        }
    }

    #[test]
    fn summary_action_texts() {
        let mut r = report();
        for (verdict, action, applied, text) in [
            (
                Verdict::Advance,
                Action::Ramp(25),
                false,
                "action: would ramp to 25%",
            ),
            (
                Verdict::Advance,
                Action::Ramp(25),
                true,
                "action: ramped to 25%",
            ),
            (
                Verdict::Advance,
                Action::Promote,
                false,
                "action: would promote v4",
            ),
            (
                Verdict::Advance,
                Action::Promote,
                true,
                "action: promoted v4",
            ),
            (Verdict::Abort, Action::Abort, false, "action: would abort"),
            (Verdict::Abort, Action::Abort, true, "action: aborted"),
            (Verdict::Wait, Action::None, false, "action: none"),
            (Verdict::Wait, Action::None, true, "action: none"),
        ] {
            r.verdict = verdict;
            r.action = action;
            r.applied = applied;
            let summary = r.summary();
            let lines: Vec<&str> = summary.lines().collect();
            assert_eq!(
                lines[lines.len() - 2],
                format!("verdict: {}", verdict.as_str())
            );
            assert_eq!(lines[lines.len() - 1], text);
        }
    }

    #[test]
    fn exit_codes() {
        let mut r = report();
        for (verdict, code) in [
            (Verdict::Advance, 0),
            (Verdict::Abort, 1),
            (Verdict::Wait, 3),
        ] {
            r.verdict = verdict;
            assert_eq!(r.exit_code(), code);
            r.applied = !r.applied;
            assert_eq!(r.exit_code(), code);
        }
    }

    #[test]
    fn json_round_trip() {
        let mut r = report();
        r.action = Action::Ramp(25);
        r.verdict = Verdict::Advance;
        r.applied = true;
        let json = r.to_json().unwrap();
        assert!(json.contains("\"verdict\": \"advance\""), "{json}");
        assert!(json.contains("\"ramp\": 25"), "{json}");
        assert!(json.contains("\"status\": \"insufficient\""), "{json}");
        assert!(json.contains("\"unit\": \"millis\""), "{json}");
        assert_eq!(GuardReport::from_json(&json).unwrap(), r);
        for action in [Action::Promote, Action::Abort, Action::None] {
            r.action = action;
            assert_eq!(GuardReport::from_json(&r.to_json().unwrap()).unwrap(), r);
        }
        assert!(!r.to_json().unwrap().contains("note"));
        r.note = Some("step at 25% is 1h old (< min_step_age 1d)".into());
        let json = r.to_json().unwrap();
        assert!(
            json.contains("\"note\": \"step at 25% is 1h old (< min_step_age 1d)\""),
            "{json}"
        );
        assert_eq!(GuardReport::from_json(&json).unwrap(), r);
        // A report stored before `note` existed still parses.
        r.note = None;
        let mut old: serde_json::Value = serde_json::from_str(&r.to_json().unwrap()).unwrap();
        assert!(old.as_object_mut().unwrap().remove("note").is_none());
        assert_eq!(GuardReport::from_json(&old.to_string()).unwrap(), r);
        assert!(matches!(
            GuardReport::from_json("{}"),
            Err(MonitorError::Io(e)) if e.kind() == std::io::ErrorKind::InvalidData
        ));
    }

    #[test]
    fn values_print_in_their_unit() {
        assert_eq!(Unit::Rate.format_value(Some(0.012)), "1.20%");
        assert_eq!(Unit::Number.format_value(Some(4.1)), "4.10");
        assert_eq!(Unit::Usd.format_value(Some(0.000123)), "$0.000123");
        assert_eq!(Unit::Millis.format_value(Some(1234.4)), "1234 ms");
        for unit in [Unit::Rate, Unit::Number, Unit::Usd, Unit::Millis] {
            assert_eq!(unit.format_value(None), "—");
        }
    }

    #[test]
    fn harms_print_as_points_for_rates_and_percent_otherwise() {
        assert_eq!(Unit::Rate.format_harm(Some(0.002)), "+0.20pt");
        assert_eq!(Unit::Rate.format_harm(Some(-0.015)), "-1.50pt");
        assert_eq!(Unit::Number.format_harm(Some(0.12195)), "+12.2%");
        assert_eq!(Unit::Usd.format_harm(Some(0.5)), "+50.0%");
        assert_eq!(Unit::Millis.format_harm(Some(-0.1089)), "-10.9%");
        for unit in [Unit::Rate, Unit::Number, Unit::Usd, Unit::Millis] {
            assert_eq!(unit.format_harm(None), "—");
        }
    }

    #[test]
    fn action_texts() {
        let mut r = report();
        for (action, applied, text) in [
            (Action::Ramp(25), false, "would ramp to 25%"),
            (Action::Ramp(25), true, "ramped to 25%"),
            (Action::Promote, false, "would promote v4"),
            (Action::Promote, true, "promoted v4"),
            (Action::Abort, false, "would abort"),
            (Action::Abort, true, "aborted"),
            (Action::None, false, "none"),
            (Action::None, true, "none"),
        ] {
            r.action = action;
            r.applied = applied;
            assert_eq!(r.action_text(), text);
        }
    }

    #[test]
    fn status_and_verdict_names() {
        let statuses = [
            Status::Ok,
            Status::Worse,
            Status::Unsure,
            Status::Insufficient,
            Status::Unknown,
        ];
        let names: Vec<_> = statuses.iter().map(Status::as_str).collect();
        assert_eq!(names, ["ok", "worse", "unsure", "insufficient", "unknown"]);
        for s in statuses {
            assert_eq!(
                serde_json::to_string(&s).unwrap(),
                format!("\"{}\"", s.as_str())
            );
        }
        let verdicts = [Verdict::Advance, Verdict::Abort, Verdict::Wait];
        let names: Vec<_> = verdicts.iter().map(Verdict::as_str).collect();
        assert_eq!(names, ["advance", "abort", "wait"]);
        for v in verdicts {
            assert_eq!(
                serde_json::to_string(&v).unwrap(),
                format!("\"{}\"", v.as_str())
            );
        }
    }
}
