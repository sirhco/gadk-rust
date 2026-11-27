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

//! Judge results as `FEEDBACK` rows: the `llm_judge` and `reference` scores of a
//! [`MonitorReport`], in the shape the console writes its own scores, with `source: "judge"`.
//!
//! [`judge_rows()`] builds the rows (pure), and [`write_judge_rows`] writes the ones a
//! [`ScoreSink`] does not hold yet, at most [`JUDGE_ROWS_PER_INSERT`] per insert.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use gadk_eval::Outcome;
use serde_json::{Value, json};

use crate::error::MonitorError;
use crate::report::MonitorReport;

/// The most rows one [`ScoreSink::insert`] call is given.
pub const JUDGE_ROWS_PER_INSERT: usize = 500;

/// The scorer kinds whose results become rows: the ones that set a 1-5 score.
const JUDGE_KINDS: [&str; 2] = ["llm_judge", "reference"];

/// One judge result as a `FEEDBACK` row.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct JudgeRow {
    /// The score name: `<spec name>/<kind>` for the first scorer of a kind, and
    /// `<spec name>/<kind>_<n>` for the n-th (n ≥ 2).
    pub name: String,
    /// The judge's 1-5 score.
    pub score: u8,
    /// Whether the result passed.
    pub pass: bool,
    /// The judge's rationale.
    pub rationale: String,
    /// The run's trace id, when it recorded one.
    pub trace_id: Option<String>,
    /// The run's session.
    pub session_id: String,
    /// The run's invocation.
    pub invocation_id: String,
}

impl JudgeRow {
    /// A row with every field given.
    pub fn new(
        name: String,
        score: u8,
        pass: bool,
        rationale: String,
        trace_id: Option<String>,
        session_id: String,
        invocation_id: String,
    ) -> Self {
        Self {
            name,
            score,
            pass,
            rationale,
            trace_id,
            session_id,
            invocation_id,
        }
    }

    /// The row's `content`: `{"name", "value": <score>, "data_type": "number", "comment":
    /// "pass: <rationale>"}`, or `"fail: <rationale>"` when the result failed.
    pub fn content(&self) -> Value {
        let verdict = if self.pass { "pass" } else { "fail" };
        json!({
            "name": self.name,
            "value": self.score,
            "data_type": "number",
            "comment": format!("{verdict}: {}", self.rationale),
        })
    }

    /// The row's `attributes`: `{"source": "judge", "author": <author>, "adk":
    /// {"invocation_id"}}`.
    pub fn attributes(&self, author: &str) -> Value {
        json!({
            "source": "judge",
            "author": author,
            "adk": {"invocation_id": self.invocation_id},
        })
    }
}

/// The judge rows of `report` for the monitor spec `spec_name`: one per `llm_judge` or
/// `reference` result that passed or failed with a score, in run and scorer order. Results are
/// numbered per kind in the order of a run's scores (an error or unscored result keeps its
/// number but yields no row), so the n-th scorer of a kind is `<spec name>/<kind>_<n>`.
pub fn judge_rows(report: &MonitorReport, spec_name: &str) -> Vec<JudgeRow> {
    let mut rows = Vec::new();
    for monitored in &report.runs {
        let run = &monitored.run;
        let mut counts: Vec<(&str, usize)> = Vec::new();
        for s in &monitored.scores {
            if !JUDGE_KINDS.contains(&s.kind.as_str()) {
                continue;
            }
            let n = match counts.iter().position(|(kind, _)| *kind == s.kind) {
                Some(i) => {
                    counts[i].1 += 1;
                    counts[i].1
                }
                None => {
                    counts.push((&s.kind, 1));
                    1
                }
            };
            let Some(score) = s.score else {
                continue;
            };
            if s.outcome == Outcome::Error {
                continue;
            }
            let name = if n == 1 {
                format!("{spec_name}/{}", s.kind)
            } else {
                format!("{spec_name}/{}_{n}", s.kind)
            };
            rows.push(JudgeRow::new(
                name,
                score,
                s.outcome == Outcome::Pass,
                s.rationale.clone(),
                run.trace_id.clone(),
                run.session_id.clone(),
                run.invocation_id.clone(),
            ));
        }
    }
    rows
}

/// What [`write_judge_rows`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct WriteOutcome {
    /// The rows inserted.
    pub written: usize,
    /// The rows skipped because the sink already held their (invocation id, name) pair.
    pub present: usize,
}

impl WriteOutcome {
    /// `written` rows inserted and `present` skipped.
    pub fn new(written: usize, present: usize) -> Self {
        Self { written, present }
    }
}

/// Where judge rows are written: the agent-analytics table for
/// [`BigQueryTranscriptSource`](crate::BigQueryTranscriptSource).
#[async_trait::async_trait]
pub trait ScoreSink: Send + Sync {
    /// The (invocation_id, name) pairs among `rows` that already have a judge row written at or
    /// after `since` (the report's `window.from`).
    async fn existing(
        &self,
        rows: &[JudgeRow],
        since: DateTime<Utc>,
    ) -> Result<HashSet<(String, String)>, MonitorError>;
    /// Inserts `rows` (at most [`JUDGE_ROWS_PER_INSERT`]) in one statement.
    async fn insert(&self, rows: &[JudgeRow], author: &str) -> Result<(), MonitorError>;
}

/// Writes the `rows` that `sink` does not hold yet, with `author` (the spec name): no call at
/// all for no rows, else one [`ScoreSink::existing`] call with `since`, then one
/// [`ScoreSink::insert`] per [`JUDGE_ROWS_PER_INSERT`] new rows.
///
/// # Errors
///
/// The sink's error. Chunks inserted before a failing one stay written.
pub async fn write_judge_rows(
    sink: &dyn ScoreSink,
    rows: Vec<JudgeRow>,
    since: DateTime<Utc>,
    author: &str,
) -> Result<WriteOutcome, MonitorError> {
    if rows.is_empty() {
        return Ok(WriteOutcome::new(0, 0));
    }
    let present = sink.existing(&rows, since).await?;
    let total = rows.len();
    let new: Vec<JudgeRow> = rows
        .into_iter()
        .filter(|r| !present.contains(&(r.invocation_id.clone(), r.name.clone())))
        .collect();
    for chunk in new.chunks(JUDGE_ROWS_PER_INSERT) {
        sink.insert(chunk, author).await?;
    }
    Ok(WriteOutcome::new(new.len(), total - new.len()))
}

#[cfg(test)]
mod tests {
    use gadk_eval::ScoreResult;
    use serde_json::json;

    use super::*;
    use crate::report::{MonitoredRun, Window};
    use crate::testutil::{FakeSink, at, run};

    fn report(runs: Vec<MonitoredRun>) -> MonitorReport {
        MonitorReport {
            spec: "support-monitor".into(),
            generated_at: at(1_800_000_000),
            window: Window {
                from: at(1_799_913_600),
                to: at(1_800_000_000),
            },
            source: "bigquery:p.d.t".into(),
            runs,
        }
    }

    fn scored(id: &str, trace: Option<&str>, scores: Vec<ScoreResult>) -> MonitoredRun {
        let mut r = run(id, 1_799_990_000, &[("hi", "hello", &[])]);
        r.trace_id = trace.map(str::to_owned);
        MonitoredRun { run: r, scores }
    }

    fn names(rows: &[JudgeRow]) -> Vec<&str> {
        rows.iter().map(|r| r.name.as_str()).collect()
    }

    /// `n` passing `m/llm_judge` rows of invocations `inv-0` .. `inv-<n-1>`.
    fn rows(n: usize) -> Vec<JudgeRow> {
        (0..n)
            .map(|i| {
                JudgeRow::new(
                    "m/llm_judge".into(),
                    4,
                    true,
                    "ok".into(),
                    None,
                    format!("s-{i}"),
                    format!("inv-{i}"),
                )
            })
            .collect()
    }

    #[test]
    fn names_number_each_kind_in_scorer_order() {
        let r = report(vec![scored(
            "inv-1",
            None,
            vec![
                ScoreResult::judged("llm_judge", true, 5, "good"),
                ScoreResult::check("not_contains", true, "ok"),
                ScoreResult::judged("llm_judge", false, 2, "rude"),
                ScoreResult::judged("reference", true, 4, "close"),
            ],
        )]);
        assert_eq!(
            names(&judge_rows(&r, "support-monitor")),
            [
                "support-monitor/llm_judge",
                "support-monitor/llm_judge_2",
                "support-monitor/reference",
            ]
        );
    }

    #[test]
    fn errors_unscored_results_and_other_kinds_yield_no_row_but_keep_their_number() {
        let r = report(vec![scored(
            "inv-1",
            None,
            vec![
                ScoreResult::error("llm_judge", "judge timed out"),
                ScoreResult::judged("llm_judge", true, 4, "fine"),
                ScoreResult::check("llm_judge", true, "no score"),
                ScoreResult::judged("llm_judge", false, 1, "bad"),
                ScoreResult::check("contains", false, "missing"),
                ScoreResult::check("tools", true, "ok"),
                ScoreResult::judged("equals", true, 5, "not a judge kind"),
            ],
        )]);
        assert_eq!(
            names(&judge_rows(&r, "m")),
            ["m/llm_judge_2", "m/llm_judge_4"]
        );
        assert!(judge_rows(&report(Vec::new()), "m").is_empty());
    }

    #[test]
    fn rows_carry_the_run_ids_and_numbering_restarts_per_run() {
        let r = report(vec![
            scored(
                "inv-1",
                Some("trace-1"),
                vec![ScoreResult::judged("llm_judge", true, 5, "good")],
            ),
            scored(
                "inv-2",
                None,
                vec![ScoreResult::judged("llm_judge", false, 3, "meh")],
            ),
        ]);
        assert_eq!(
            judge_rows(&r, "m"),
            vec![
                JudgeRow::new(
                    "m/llm_judge".into(),
                    5,
                    true,
                    "good".into(),
                    Some("trace-1".into()),
                    "session-inv-1".into(),
                    "inv-1".into(),
                ),
                JudgeRow::new(
                    "m/llm_judge".into(),
                    3,
                    false,
                    "meh".into(),
                    None,
                    "session-inv-2".into(),
                    "inv-2".into(),
                ),
            ]
        );
    }

    #[test]
    fn content_and_attributes_are_the_console_feedback_shape() {
        let pass = JudgeRow::new(
            "m/llm_judge".into(),
            4,
            true,
            "polite and accurate".into(),
            Some("t".into()),
            "s".into(),
            "inv-1".into(),
        );
        assert_eq!(
            pass.content(),
            json!({"name": "m/llm_judge", "value": 4, "data_type": "number",
                   "comment": "pass: polite and accurate"})
        );
        assert_eq!(
            pass.attributes("m"),
            json!({"source": "judge", "author": "m", "adk": {"invocation_id": "inv-1"}})
        );
        let fail = JudgeRow::new(
            "m/reference".into(),
            2,
            false,
            "wrong id".into(),
            None,
            "s".into(),
            "inv-2".into(),
        );
        assert_eq!(fail.content()["comment"], "fail: wrong id");
        assert!(fail.content()["value"].is_u64());
        assert_eq!(fail.trace_id, None);
    }

    #[tokio::test]
    async fn no_rows_makes_no_sink_calls() {
        let sink = FakeSink::default();
        assert_eq!(
            write_judge_rows(&sink, Vec::new(), at(0), "m")
                .await
                .unwrap(),
            WriteOutcome::new(0, 0)
        );
        assert!(sink.existing_calls.lock().unwrap().is_empty());
        assert!(sink.inserts.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn present_pairs_are_skipped() {
        let mut sink = FakeSink::default();
        sink.present.insert(("inv-1".into(), "m/llm_judge".into()));
        // Pairs that are not among the rows change nothing.
        sink.present.insert(("inv-9".into(), "m/llm_judge".into()));
        sink.present.insert(("inv-0".into(), "m/other".into()));
        let out = write_judge_rows(&sink, rows(3), at(100), "m")
            .await
            .unwrap();
        assert_eq!(out, WriteOutcome::new(2, 1));
        let inserts = sink.inserts.lock().unwrap();
        assert_eq!(inserts.len(), 1);
        let ids: Vec<&str> = inserts[0]
            .0
            .iter()
            .map(|r| r.invocation_id.as_str())
            .collect();
        assert_eq!(ids, ["inv-0", "inv-2"]);
        assert_eq!(inserts[0].1, "m");
    }

    #[tokio::test]
    async fn rows_go_in_chunks_of_500_after_one_existing_call() {
        let sink = FakeSink::default();
        let since = at(1_799_913_600);
        let out = write_judge_rows(&sink, rows(1_201), since, "m")
            .await
            .unwrap();
        assert_eq!(out, WriteOutcome::new(1_201, 0));
        assert_eq!(*sink.existing_calls.lock().unwrap(), [(1_201, since)]);
        let sizes: Vec<usize> = sink
            .inserts
            .lock()
            .unwrap()
            .iter()
            .map(|(r, _)| r.len())
            .collect();
        assert_eq!(sizes, [500, 500, 201]);
        assert_eq!(JUDGE_ROWS_PER_INSERT, 500);
    }

    #[tokio::test]
    async fn a_failed_insert_is_the_error() {
        let sink = FakeSink {
            fail_insert: Some("HTTP 403: denied".into()),
            ..FakeSink::default()
        };
        let err = write_judge_rows(&sink, rows(2), at(0), "m")
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "transcript source: HTTP 403: denied");
    }
}
