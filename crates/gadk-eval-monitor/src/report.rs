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

//! The monitor report: every graded run with its scores, a text summary and JSON.

use std::fmt::Write as _;
use std::path::Path;

use chrono::{DateTime, SecondsFormat, Utc};
use gadk_eval::{Outcome, ScoreResult};
use serde::{Deserialize, Serialize};

use crate::error::MonitorError;
use crate::source::ProductionRun;

/// The longest rationale quoted in the summary.
const RATIONALE_MAX: usize = 200;

/// The time window a report covers: runs started in `[from, to)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Window {
    /// The start (inclusive).
    pub from: DateTime<Utc>,
    /// The end (exclusive).
    pub to: DateTime<Utc>,
}

/// One graded run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MonitoredRun {
    /// The run.
    pub run: ProductionRun,
    /// One result per scorer of the spec, in spec order.
    pub scores: Vec<ScoreResult>,
}

impl MonitoredRun {
    /// Whether every score passed.
    pub fn passed(&self) -> bool {
        self.scores.iter().all(|s| s.outcome == Outcome::Pass)
    }

    /// Whether any score is an error.
    pub fn errored(&self) -> bool {
        self.scores.iter().any(|s| s.outcome == Outcome::Error)
    }
}

/// The result of one monitor run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MonitorReport {
    /// The monitor spec's name.
    pub spec: String,
    /// When the report was made (the end of the window).
    pub generated_at: DateTime<Utc>,
    /// The window the runs started in.
    pub window: Window,
    /// The transcript source, for example `bigquery:<project>.<dataset>.<table>`.
    pub source: String,
    /// The graded runs, newest first.
    pub runs: Vec<MonitoredRun>,
}

impl MonitorReport {
    /// The share of runs whose scores all passed; `None` when there are no runs.
    pub fn pass_rate(&self) -> Option<f64> {
        if self.runs.is_empty() {
            return None;
        }
        let passed = self.runs.iter().filter(|r| r.passed()).count();
        Some(passed as f64 / self.runs.len() as f64)
    }

    /// The number of runs with at least one error score.
    pub fn error_count(&self) -> usize {
        self.runs.iter().filter(|r| r.errored()).count()
    }

    /// A text summary: run counts and pass rate, per-scorer tallies (first-appearance order) and
    /// the runs that did not pass with their first failing or erroring score.
    pub fn summary(&self) -> String {
        let passed = self.runs.iter().filter(|r| r.passed()).count();
        let errors = self.error_count();
        let failed = self.runs.len() - passed - errors;
        let rate = self
            .pass_rate()
            .map_or_else(|| "n/a".to_owned(), |r| format!("{:.1}%", r * 100.0));
        let mut out = format!(
            "monitor {}: {} runs, {passed} passed, {failed} failed, {errors} with errors; \
             pass rate {rate}\nwindow {} .. {}, source {}\n",
            self.spec,
            self.runs.len(),
            time(self.window.from),
            time(self.window.to),
            self.source
        );
        let mut kinds: Vec<(&str, [usize; 3])> = Vec::new();
        for s in self.runs.iter().flat_map(|r| r.scores.iter()) {
            let i = match kinds.iter().position(|(k, _)| *k == s.kind) {
                Some(i) => i,
                None => {
                    kinds.push((&s.kind, [0; 3]));
                    kinds.len() - 1
                }
            };
            let slot = match s.outcome {
                Outcome::Pass => 0,
                Outcome::Fail => 1,
                Outcome::Error => 2,
            };
            kinds[i].1[slot] += 1;
        }
        for (kind, [p, f, e]) in &kinds {
            let _ = writeln!(out, "  {kind:<14} pass {p}  fail {f}  error {e}");
        }
        let failing: Vec<&MonitoredRun> = self.runs.iter().filter(|r| !r.passed()).collect();
        if !failing.is_empty() {
            out.push_str("not passing:\n");
        }
        for r in failing {
            let Some(s) = r.scores.iter().find(|s| s.outcome != Outcome::Pass) else {
                continue;
            };
            let outcome = if s.outcome == Outcome::Error {
                "error"
            } else {
                "fail"
            };
            let line = s.rationale.lines().next().unwrap_or("");
            let line = &line[..line.floor_char_boundary(RATIONALE_MAX.min(line.len()))];
            let _ = writeln!(
                out,
                "  {} ({}) {outcome}: {}: {line}",
                r.run.invocation_id,
                time(r.run.started_at),
                s.kind
            );
        }
        out
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
    /// [`MonitorError::Io`] (`InvalidData`) when `json` is not a report.
    pub fn from_json(json: &str) -> Result<Self, MonitorError> {
        serde_json::from_str(json).map_err(|e| MonitorError::Io(e.into()))
    }

    /// Writes the report as pretty-printed JSON to `path`.
    ///
    /// # Errors
    ///
    /// [`MonitorError::Io`] when the file cannot be written.
    pub fn write_json(&self, path: impl AsRef<Path>) -> Result<(), MonitorError> {
        std::fs::write(path, self.to_json()?)?;
        Ok(())
    }

    /// Reads a report written by [`MonitorReport::write_json`].
    ///
    /// # Errors
    ///
    /// [`MonitorError::Io`] when the file cannot be read or is not a report.
    pub fn read_json(path: impl AsRef<Path>) -> Result<Self, MonitorError> {
        Self::from_json(&std::fs::read_to_string(path)?)
    }
}

fn time(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use crate::testutil::{at, run};

    use super::*;

    fn scored(id: &str, started: i64, outcomes: &[(&str, Outcome, &str)]) -> MonitoredRun {
        MonitoredRun {
            run: run(id, started, &[("hi", "hello", &[])]),
            scores: outcomes
                .iter()
                .map(|(kind, outcome, why)| ScoreResult {
                    kind: (*kind).into(),
                    outcome: *outcome,
                    score: None,
                    rationale: (*why).into(),
                })
                .collect(),
        }
    }

    fn report(runs: Vec<MonitoredRun>) -> MonitorReport {
        MonitorReport {
            spec: "support".into(),
            generated_at: at(1_800_000_000),
            window: Window {
                from: at(1_799_913_600),
                to: at(1_800_000_000),
            },
            source: "bigquery:p.d.t".into(),
            runs,
        }
    }

    fn three() -> MonitorReport {
        report(vec![
            scored(
                "inv-3",
                1_799_990_000,
                &[
                    ("contains", Outcome::Pass, "ok"),
                    ("llm_judge", Outcome::Pass, "fine"),
                ],
            ),
            scored(
                "inv-2",
                1_799_980_000,
                &[
                    (
                        "contains",
                        Outcome::Fail,
                        "reply does not contain \"x\"\nmore",
                    ),
                    ("llm_judge", Outcome::Pass, "fine"),
                ],
            ),
            scored(
                "inv-1",
                1_799_970_000,
                &[
                    ("contains", Outcome::Pass, "ok"),
                    (
                        "llm_judge",
                        Outcome::Error,
                        "content incomplete: no USER_MESSAGE_RECEIVED row",
                    ),
                ],
            ),
        ])
    }

    #[test]
    fn pass_rate_and_error_count() {
        let r = three();
        assert_eq!(r.pass_rate(), Some(1.0 / 3.0));
        assert_eq!(r.error_count(), 1);
        let empty = report(Vec::new());
        assert_eq!(empty.pass_rate(), None);
        assert_eq!(empty.error_count(), 0);
    }

    #[test]
    fn summary_lists_tallies_and_failing_runs() {
        assert_eq!(
            three().summary(),
            "monitor support: 3 runs, 1 passed, 1 failed, 1 with errors; pass rate 33.3%\n\
             window 2027-01-14T08:00:00Z .. 2027-01-15T08:00:00Z, source bigquery:p.d.t\n  \
             contains       pass 2  fail 1  error 0\n  \
             llm_judge      pass 2  fail 0  error 1\n\
             not passing:\n  \
             inv-2 (2027-01-15T02:26:40Z) fail: contains: reply does not contain \"x\"\n  \
             inv-1 (2027-01-14T23:40:00Z) error: llm_judge: content incomplete: \
             no USER_MESSAGE_RECEIVED row\n"
        );
        assert_eq!(
            report(Vec::new()).summary(),
            "monitor support: 0 runs, 0 passed, 0 failed, 0 with errors; pass rate n/a\n\
             window 2027-01-14T08:00:00Z .. 2027-01-15T08:00:00Z, source bigquery:p.d.t\n"
        );
    }

    #[test]
    fn json_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("r.json");
        let r = three();
        r.write_json(&p).unwrap();
        assert_eq!(MonitorReport::read_json(&p).unwrap(), r);
        let v: serde_json::Value = serde_json::from_str(&r.to_json().unwrap()).unwrap();
        assert_eq!(v["window"]["from"], "2027-01-14T08:00:00Z");
        assert_eq!(v["runs"][1]["scores"][0]["outcome"], "fail");
        std::fs::write(&p, "{}").unwrap();
        assert!(matches!(
            MonitorReport::read_json(&p),
            Err(MonitorError::Io(_))
        ));
    }
}
