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

//! The evaluation report: every run's transcript and scores, a text summary and JSON output.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::EvalError;
use crate::result::{Outcome, RunTranscript, ScoreResult};

/// The result of [`crate::Evaluator::run`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalReport {
    /// The eval set's name.
    pub set: String,
    /// One entry per variant, in the order given.
    pub variants: Vec<VariantReport>,
}

/// One variant's runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VariantReport {
    /// The variant name.
    pub name: String,
    /// One entry per case × run, ordered by case (set order) then run.
    pub runs: Vec<CaseRun>,
}

/// One run of one case.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaseRun {
    /// The case id.
    pub case: String,
    /// The run number, from 1.
    pub run: u32,
    /// What the agent did.
    pub transcript: RunTranscript,
    /// One result per scorer of the case, in case order.
    pub scores: Vec<ScoreResult>,
}

/// Tallies for one summary row.
#[derive(Default)]
struct Tally {
    pass: usize,
    fail: usize,
    error: usize,
    scores: Vec<u8>,
}

impl Tally {
    fn add(&mut self, s: &ScoreResult) {
        match s.outcome {
            Outcome::Pass => self.pass += 1,
            Outcome::Fail => self.fail += 1,
            Outcome::Error => self.error += 1,
        }
        if s.outcome != Outcome::Error
            && let Some(n) = s.score
        {
            self.scores.push(n);
        }
    }

    fn cells(&self) -> [String; 3] {
        let graded = self.pass + self.fail;
        let rate = if graded == 0 {
            "-".to_owned()
        } else {
            format!(
                "{}/{graded} ({:.0}%)",
                self.pass,
                100.0 * self.pass as f64 / graded as f64
            )
        };
        let mean = if self.scores.is_empty() {
            "-".to_owned()
        } else {
            let sum: u32 = self.scores.iter().map(|&s| u32::from(s)).sum();
            format!("{:.2}", f64::from(sum) / self.scores.len() as f64)
        };
        [rate, mean, self.error.to_string()]
    }
}

impl EvalReport {
    /// True when there is at least one score, no score failed or errored, and no run recorded an
    /// error (a run or build error counts even when its case has no scorers).
    pub fn all_passed(&self) -> bool {
        self.variants
            .iter()
            .flat_map(|v| v.runs.iter())
            .all(|r| r.transcript.error.is_none())
            && self.scores().next().is_some()
            && self.scores().all(|s| s.outcome == Outcome::Pass)
    }

    fn scores(&self) -> impl Iterator<Item = &ScoreResult> {
        self.variants
            .iter()
            .flat_map(|v| v.runs.iter())
            .flat_map(|r| r.scores.iter())
    }

    /// A text table: per variant, one row per scorer kind (first-appearance order) and an
    /// `overall` row, with pass rate (passes / graded), mean judge score and error count.
    pub fn summary(&self) -> String {
        let mut rows: Vec<[String; 5]> = vec![[
            "variant".into(),
            "scorer".into(),
            "pass rate".into(),
            "mean score".into(),
            "errors".into(),
        ]];
        for v in &self.variants {
            let mut kinds: Vec<(String, Tally)> = Vec::new();
            let mut overall = Tally::default();
            for s in v.runs.iter().flat_map(|r| r.scores.iter()) {
                overall.add(s);
                match kinds.iter_mut().find(|(k, _)| *k == s.kind) {
                    Some((_, t)) => t.add(s),
                    None => {
                        let mut t = Tally::default();
                        t.add(s);
                        kinds.push((s.kind.clone(), t));
                    }
                }
            }
            kinds.push(("overall".into(), overall));
            for (kind, t) in kinds {
                let [rate, mean, errors] = t.cells();
                rows.push([v.name.clone(), kind, rate, mean, errors]);
            }
        }
        let mut widths = [0usize; 5];
        for r in &rows {
            for (w, c) in widths.iter_mut().zip(r) {
                *w = (*w).max(c.chars().count());
            }
        }
        let mut out = format!("eval set: {}\n\n", self.set);
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

    /// Writes the whole report to `path` as pretty JSON.
    pub fn write_json(&self, path: impl AsRef<Path>) -> Result<(), EvalError> {
        let path = path.as_ref();
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| EvalError::Io(format!("encoding the report: {e}")))?;
        std::fs::write(path, json)
            .map_err(|e| EvalError::Io(format!("writing {}: {e}", path.display())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::TurnRecord;

    fn run(case: &str, n: u32, scores: Vec<ScoreResult>) -> CaseRun {
        CaseRun {
            case: case.into(),
            run: n,
            transcript: RunTranscript {
                turns: vec![TurnRecord {
                    user: "hi".into(),
                    reply: "hello".into(),
                    calls: vec![],
                }],
                error: None,
            },
            scores,
        }
    }

    fn report() -> EvalReport {
        EvalReport {
            set: "support-bot".into(),
            variants: vec![
                VariantReport {
                    name: "prod".into(),
                    runs: vec![
                        run(
                            "a",
                            1,
                            vec![
                                ScoreResult::check("contains", true, ""),
                                ScoreResult::judged("llm_judge", true, 4, ""),
                            ],
                        ),
                        run(
                            "b",
                            1,
                            vec![
                                ScoreResult::check("contains", false, ""),
                                ScoreResult::judged("llm_judge", false, 2, ""),
                            ],
                        ),
                    ],
                },
                VariantReport {
                    name: "v7".into(),
                    runs: vec![run(
                        "a",
                        1,
                        vec![
                            ScoreResult::check("contains", true, ""),
                            ScoreResult::error("llm_judge", "unparseable"),
                        ],
                    )],
                },
            ],
        }
    }

    #[test]
    fn summary_table() {
        let want = "\
eval set: support-bot

variant  scorer     pass rate   mean score  errors
prod     contains   1/2 (50%)   -           0
prod     llm_judge  1/2 (50%)   3.00        0
prod     overall    2/4 (50%)   3.00        0
v7       contains   1/1 (100%)  -           0
v7       llm_judge  -           -           1
v7       overall    1/1 (100%)  -           1
";
        assert_eq!(report().summary(), want);
    }

    #[test]
    fn all_passed_requires_no_fail_and_no_error() {
        let pass = |kind| ScoreResult::check(kind, true, "");
        let mut r = EvalReport {
            set: "s".into(),
            variants: vec![VariantReport {
                name: "v".into(),
                runs: vec![run("a", 1, vec![pass("contains"), pass("regex")])],
            }],
        };
        assert!(r.all_passed());
        r.variants[0].runs[0].scores[1] = ScoreResult::check("regex", false, "");
        assert!(!r.all_passed());
        r.variants[0].runs[0].scores[1] = ScoreResult::error("regex", "boom");
        assert!(!r.all_passed());
        assert!(!report().all_passed());
    }

    #[test]
    fn all_passed_is_false_with_a_run_error_and_no_scores() {
        let mut r = EvalReport {
            set: "s".into(),
            variants: vec![VariantReport {
                name: "v".into(),
                runs: vec![
                    run("a", 1, vec![ScoreResult::check("contains", true, "")]),
                    run("b", 1, vec![]),
                ],
            }],
        };
        assert!(r.all_passed());
        r.variants[0].runs[1].transcript.error = Some("building variant v: boom".into());
        assert!(!r.all_passed(), "a run error with no scorers is not a pass");
    }

    #[test]
    fn all_passed_is_false_without_any_score() {
        let mut r = EvalReport {
            set: "s".into(),
            variants: vec![],
        };
        assert!(!r.all_passed(), "an empty report");
        r.variants.push(VariantReport {
            name: "v".into(),
            runs: vec![run("a", 1, vec![])],
        });
        assert!(!r.all_passed(), "runs without scores");
    }

    #[test]
    fn write_json_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("out.json");
        report().write_json(&p).unwrap();
        let back: EvalReport = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(back, report());
        let err = report()
            .write_json(dir.path().join("no/such/dir/out.json"))
            .unwrap_err();
        assert!(matches!(err, EvalError::Io(_)));
    }
}
