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

//! The monitor runner: lists a spec's runs and grades each one.

use std::cmp::Reverse;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::StreamExt as _;
use futures::future::BoxFuture;
use gadk_eval::{
    GeminiJudge, JudgeBackend, JudgeOutcome, JudgeRequest, ScoreResult, ScorerSpec,
    score_transcript_with,
};
use gadk_model::Llm;

use crate::batch::{BatchPredictor, RecordingJudge, judge_in_batch};
use crate::error::MonitorError;
use crate::report::{MonitorReport, MonitoredRun, Window};
use crate::source::{ProductionRun, TranscriptQuery, TranscriptSource};
use crate::spec::{DEFAULT_JUDGE_TIMEOUT, MonitorSpec, grades_with_judge, has_llm_judge};

/// The default number of runs graded at once.
pub const DEFAULT_CONCURRENCY: usize = 8;

/// Grades production runs against a [`MonitorSpec`].
///
/// ```no_run
/// # async fn demo(judge: std::sync::Arc<dyn gadk_model::Llm>) -> Result<(), gadk_eval_monitor::MonitorError> {
/// use gadk_eval_monitor::monitor::Monitor;
/// use gadk_eval_monitor::source::bigquery::BigQueryTranscriptSource;
/// use gadk_eval_monitor::spec::MonitorSpec;
///
/// let spec = MonitorSpec::load("monitors/support.yaml")?;
/// let source = BigQueryTranscriptSource::new("my-proj", "agent_analytics", "agent_events")?;
/// let report = Monitor::new().run(&spec, &source, Some(judge)).await?;
/// print!("{}", report.summary());
/// # Ok(()) }
/// ```
#[derive(Debug, Clone)]
pub struct Monitor {
    concurrency: usize,
    now: Option<DateTime<Utc>>,
}

impl Default for Monitor {
    fn default() -> Self {
        Self::new()
    }
}

impl Monitor {
    /// A monitor grading [`DEFAULT_CONCURRENCY`] runs at once, over the window ending now.
    pub fn new() -> Self {
        Self {
            concurrency: DEFAULT_CONCURRENCY,
            now: None,
        }
    }

    /// Grades at most `n` runs at once (values below 1 mean 1).
    pub fn concurrency(mut self, n: usize) -> Self {
        self.concurrency = n.max(1);
        self
    }

    /// Uses `now` as the end of the window and the report time instead of the current time.
    pub fn now(mut self, now: DateTime<Utc>) -> Self {
        self.now = Some(now);
        self
    }

    /// Lists the runs of `spec`'s window from `source` (newest first, at most `spec.limit`) and
    /// scores each with `spec.scorers`.
    ///
    /// A run whose transcript has an error gets an error result from every scorer. Otherwise,
    /// when its content is incomplete, every scorer except `tools` is an error naming the first
    /// note; when a tool row could not be rebuilt, `tools` scorers are an error naming that row's
    /// note; and a scorer whose `turn` the run does not have is an error. `judge` grades
    /// `llm_judge` scorers; judge failures are error results, not errors of `run`.
    ///
    /// # Errors
    ///
    /// [`MonitorError::Judge`] when a scorer is `llm_judge` and `judge` is `None`; the source's
    /// error when listing fails.
    pub async fn run(
        &self,
        spec: &MonitorSpec,
        source: &dyn TranscriptSource,
        judge: Option<Arc<dyn Llm>>,
    ) -> Result<MonitorReport, MonitorError> {
        let timeout = spec
            .judge
            .as_ref()
            .map_or(DEFAULT_JUDGE_TIMEOUT, |j| j.timeout);
        self.run_with_backend(
            spec,
            source,
            judge.map(|model| gemini_backend(model, timeout)),
        )
        .await
    }

    /// [`Monitor::run`] with any [`JudgeBackend`] grading the `llm_judge` and `reference`
    /// scorers; the backend bounds its own calls.
    ///
    /// # Errors
    ///
    /// As [`Monitor::run`]: [`MonitorError::Judge`] when a scorer is `llm_judge` and `judge` is
    /// `None`; the source's error when listing fails.
    pub async fn run_with_backend(
        &self,
        spec: &MonitorSpec,
        source: &dyn TranscriptSource,
        judge: Option<Arc<dyn JudgeBackend>>,
    ) -> Result<MonitorReport, MonitorError> {
        check_judge(judge.is_some(), &spec.scorers)?;
        let (now, q, runs) = self.list(spec, source).await?;
        let runs = score_runs(judge, &spec.scorers, runs, self.concurrency).await?;
        Ok(report(spec, now, &q, runs))
    }

    /// [`Monitor::run_with_backend`] with the `llm_judge` and `reference` scorers graded in one
    /// batch job of `predictor`; also returns the job's name.
    ///
    /// The runs are listed once and scored twice: first with a [`RecordingJudge`], whose results
    /// are thrown away, then, after [`judge_in_batch`] has graded the recorded requests, with the
    /// [`ReplayJudge`](crate::ReplayJudge). A judge request the batch has no reply for is an
    /// error result (`no batch result for this request`). When `spec` has no `llm_judge` or
    /// `reference` scorer, this is `run_with_backend(spec, source, None)` and no job is created;
    /// the name is also `None` when no run needed the judge.
    ///
    /// # Errors
    ///
    /// The source's error when listing fails; the predictor's error when the batch fails.
    pub async fn run_batch(
        &self,
        spec: &MonitorSpec,
        source: &dyn TranscriptSource,
        predictor: &dyn BatchPredictor,
    ) -> Result<(MonitorReport, Option<String>), MonitorError> {
        if !grades_with_judge(&spec.scorers) {
            return Ok((self.run_with_backend(spec, source, None).await?, None));
        }
        let (now, q, runs) = self.list(spec, source).await?;
        let recorder = Arc::new(RecordingJudge::new());
        score_runs(
            Some(recorder.clone()),
            &spec.scorers,
            runs.clone(),
            self.concurrency,
        )
        .await?;
        let (replay, job) = judge_in_batch(predictor, &recorder).await?;
        let runs = score_runs(
            Some(Arc::new(replay)),
            &spec.scorers,
            runs,
            self.concurrency,
        )
        .await?;
        Ok((report(spec, now, &q, runs), job))
    }

    /// The end of the window, its query, and its runs from `source`: newest first, at most
    /// `q.limit`.
    async fn list(
        &self,
        spec: &MonitorSpec,
        source: &dyn TranscriptSource,
    ) -> Result<(DateTime<Utc>, TranscriptQuery, Vec<ProductionRun>), MonitorError> {
        let now = self.now.unwrap_or_else(Utc::now);
        let q = spec.query(now);
        let mut runs = source.list(&q).await?;
        runs.sort_by_key(|r| Reverse(r.started_at));
        runs.truncate(q.limit);
        Ok((now, q, runs))
    }
}

/// The report of `spec` over the window of `q` ending `now`, with the scored `runs`.
fn report(
    spec: &MonitorSpec,
    now: DateTime<Utc>,
    q: &TranscriptQuery,
    runs: Vec<MonitoredRun>,
) -> MonitorReport {
    MonitorReport {
        spec: spec.name.clone(),
        generated_at: now,
        window: Window {
            from: q.from,
            to: q.to,
        },
        source: spec.source.describe(),
        runs,
    }
}

/// `model` as a [`GeminiJudge`] whose calls are bounded by `timeout` (values below 1 ms mean
/// 1 ms), as [`gadk_eval::score_transcript`] bounds them.
pub(crate) fn gemini_backend(model: Arc<dyn Llm>, timeout: Duration) -> Arc<dyn JudgeBackend> {
    let timeout = timeout.max(Duration::from_millis(1));
    Arc::new(GeminiJudge::new(model, timeout))
}

/// `MonitorError::Judge` when `scorers` has an `llm_judge` scorer and there is no judge.
fn check_judge(has_judge: bool, scorers: &[ScorerSpec]) -> Result<(), MonitorError> {
    if has_llm_judge(scorers) && !has_judge {
        return Err(MonitorError::Judge(
            "the spec has llm_judge scorers but no judge model was given".into(),
        ));
    }
    Ok(())
}

/// Scores each of `runs` with `scorers` (see [`Monitor::run`] for the per-run rules), at most
/// `concurrency` runs at once (values below 1 mean 1), and returns them in the order given.
/// `judge` grades `llm_judge` and `reference` scorers.
///
/// # Errors
///
/// [`MonitorError::Judge`] when a scorer is `llm_judge` and `judge` is `None`.
pub(crate) async fn score_runs(
    judge: Option<Arc<dyn JudgeBackend>>,
    scorers: &[ScorerSpec],
    runs: Vec<ProductionRun>,
    concurrency: usize,
) -> Result<Vec<MonitoredRun>, MonitorError> {
    check_judge(judge.is_some(), scorers)?;
    let judge: Arc<dyn JudgeBackend> = judge.unwrap_or_else(|| Arc::new(NoJudge));
    let judge = judge.as_ref();
    let mut graded: Vec<(usize, MonitoredRun)> =
        futures::stream::iter(runs.into_iter().enumerate())
            .map(|(i, run)| async move {
                let scores = score_run(judge, scorers, &run).await;
                (i, MonitoredRun { run, scores })
            })
            .buffer_unordered(concurrency.max(1))
            .collect()
            .await;
    graded.sort_by_key(|(i, _)| *i);
    Ok(graded.into_iter().map(|(_, r)| r).collect())
}

/// Scores one run (see [`Monitor::run`]).
async fn score_run(
    judge: &dyn JudgeBackend,
    scorers: &[ScorerSpec],
    run: &ProductionRun,
) -> Vec<ScoreResult> {
    let t = &run.transcript;
    if t.error.is_some() {
        return score_transcript_with(judge, scorers, t).await;
    }
    let mut out: Vec<Option<ScoreResult>> = vec![None; scorers.len()];
    let mut todo: Vec<(usize, ScorerSpec)> = Vec::new();
    for (i, s) in scorers.iter().enumerate() {
        match precheck(s, run) {
            Some(reason) => out[i] = Some(ScoreResult::error(s.kind(), reason)),
            None => todo.push((i, s.clone())),
        }
    }
    if !todo.is_empty() {
        let specs: Vec<ScorerSpec> = todo.iter().map(|(_, s)| s.clone()).collect();
        let scored = score_transcript_with(judge, &specs, t).await;
        for ((i, _), r) in todo.into_iter().zip(scored) {
            out[i] = Some(r);
        }
    }
    out.into_iter().flatten().collect()
}

/// Why `s` cannot grade `run`, if it cannot: `s` is `tools` and a tool row could not be rebuilt,
/// the run's content is incomplete and `s` reads text, or the run has no turn `s.turn`.
fn precheck(s: &ScorerSpec, run: &ProductionRun) -> Option<String> {
    let is_tools = matches!(s, ScorerSpec::Tools { .. });
    if is_tools && !run.tools_complete {
        let note = run
            .notes
            .iter()
            .find(|n| n.starts_with("TOOL_STARTING "))
            .or(run.notes.first())
            .map_or("", String::as_str);
        return Some(format!("tool calls incomplete: {note}"));
    }
    if !is_tools && !run.content_complete {
        let note = run.notes.first().map_or("", String::as_str);
        return Some(format!("content incomplete: {note}"));
    }
    let turn = match s {
        ScorerSpec::Contains { turn, .. }
        | ScorerSpec::NotContains { turn, .. }
        | ScorerSpec::Regex { turn, .. }
        | ScorerSpec::Equals { turn, .. }
        | ScorerSpec::Reference { turn, .. } => *turn,
        ScorerSpec::Tools { .. } | ScorerSpec::LlmJudge { .. } => None,
    };
    match turn {
        Some(n) if run.transcript.turns.len() < n => Some(format!("run has no turn {n}")),
        _ => None,
    }
}

/// The judge used when no `llm_judge` scorer needs one: every grade is an error (a
/// `reference` scorer without a judge), with the text the judge model error had.
struct NoJudge;

impl JudgeBackend for NoJudge {
    fn grade<'a>(&'a self, _req: &'a JudgeRequest) -> BoxFuture<'a, JudgeOutcome> {
        Box::pin(async { JudgeOutcome::Error("judge model error: no judge model".into()) })
    }
}

#[cfg(test)]
mod tests {
    use gadk_eval::Outcome;
    use serde_json::json;

    use crate::testutil::{FakeJudge, FakeSource, at, run};

    use super::*;

    const SPEC: &str = r#"
name: support
source: { bigquery: { project: p, dataset: d, table: t } }
window: 1d
agent: support_bot
judge: { model: m, timeout: 5s }
scorers:
  - contains: "order"
  - tools: [lookup_order]
  - llm_judge: "Polite"
"#;

    const VERDICT: &str = r#"{"pass": true, "score": 5, "rationale": "polite"}"#;

    fn spec(yaml: &str) -> MonitorSpec {
        MonitorSpec::from_yaml(yaml).unwrap()
    }

    fn incomplete(id: &str, started: i64) -> ProductionRun {
        let mut r = run(id, started, &[("Refund?", "", &["lookup_order"])]);
        r.content_complete = false;
        r.notes = vec!["turn 1 has no AGENT_RESPONSE".into()];
        r
    }

    #[tokio::test]
    async fn a_reference_scorer_without_a_judge_grades_as_an_error() {
        let mut spec = spec(
            "name: m\nsource: { bigquery: { project: p, dataset: d, table: t } }\nwindow: 1d\n\
             scorers: [contains: order]\n",
        );
        spec.scorers = vec![ScorerSpec::Reference {
            answer: "Your order ships today.".into(),
            min_score: 3,
            turn: None,
        }];
        let source = FakeSource::new(vec![
            run("inv-a", 20_000, &[("Where is my order?", "Today.", &[])]),
            run("inv-b", 40_000, &[("hi", "hello", &[])]),
        ]);
        let report = Monitor::new()
            .now(at(100_000))
            .run(&spec, &source, None)
            .await
            .unwrap();
        assert_eq!(report.runs.len(), 2);
        for r in &report.runs {
            let [score] = r.scores.as_slice() else {
                panic!("expected one score, got {:?}", r.scores);
            };
            assert_eq!(
                (score.kind.as_str(), score.outcome),
                ("reference", Outcome::Error)
            );
            assert!(score.rationale.contains("no judge model"), "{score:?}");
        }
    }

    #[tokio::test]
    async fn golden_report() {
        let source = FakeSource::new(vec![
            run(
                "inv-old",
                20_000,
                &[(
                    "Where is my order?",
                    "Your order ships today.",
                    &["lookup_order"],
                )],
            ),
            run("inv-new", 40_000, &[("hi", "hello", &[])]),
            incomplete("inv-mid", 30_000),
        ]);
        let judge = FakeJudge::new(&[VERDICT, VERDICT]);
        let report = Monitor::new()
            .now(at(100_000))
            .run(&spec(SPEC), &source, Some(judge.clone()))
            .await
            .unwrap();
        assert_eq!(judge.calls(), 2);
        let q = source.queries.lock().unwrap()[0].clone();
        assert_eq!((q.from, q.to, q.limit), (at(13_600), at(100_000), 100));
        assert_eq!(q.agent.as_deref(), Some("support_bot"));
        assert_eq!(
            serde_json::to_value(&report).unwrap(),
            json!({
                "spec": "support",
                "generated_at": "1970-01-02T03:46:40Z",
                "window": {"from": "1970-01-01T03:46:40Z", "to": "1970-01-02T03:46:40Z"},
                "source": "bigquery:p.d.t",
                "runs": [
                    {
                        "run": {
                            "invocation_id": "inv-new",
                            "session_id": "session-inv-new",
                            "trace_id": null,
                            "agent": "agent",
                            "started_at": "1970-01-01T11:06:40Z",
                            "transcript": {
                                "turns": [{"user": "hi", "reply": "hello", "calls": []}],
                                "error": null
                            },
                            "content_complete": true,
                            "tools_complete": true,
                            "notes": []
                        },
                        "scores": [
                            {"kind": "contains", "outcome": "fail", "score": null,
                             "rationale": "reply does not contain \"order\""},
                            {"kind": "tools", "outcome": "fail", "score": null,
                             "rationale": "missing call lookup_order; called []"},
                            {"kind": "llm_judge", "outcome": "pass", "score": 5,
                             "rationale": "polite"}
                        ]
                    },
                    {
                        "run": {
                            "invocation_id": "inv-mid",
                            "session_id": "session-inv-mid",
                            "trace_id": null,
                            "agent": "agent",
                            "started_at": "1970-01-01T08:20:00Z",
                            "transcript": {
                                "turns": [{"user": "Refund?", "reply": "",
                                           "calls": [{"name": "lookup_order", "args": {}}]}],
                                "error": null
                            },
                            "content_complete": false,
                            "tools_complete": true,
                            "notes": ["turn 1 has no AGENT_RESPONSE"]
                        },
                        "scores": [
                            {"kind": "contains", "outcome": "error", "score": null,
                             "rationale": "content incomplete: turn 1 has no AGENT_RESPONSE"},
                            {"kind": "tools", "outcome": "pass", "score": null,
                             "rationale": "called [lookup_order]; called [lookup_order]"},
                            {"kind": "llm_judge", "outcome": "error", "score": null,
                             "rationale": "content incomplete: turn 1 has no AGENT_RESPONSE"}
                        ]
                    },
                    {
                        "run": {
                            "invocation_id": "inv-old",
                            "session_id": "session-inv-old",
                            "trace_id": null,
                            "agent": "agent",
                            "started_at": "1970-01-01T05:33:20Z",
                            "transcript": {
                                "turns": [{"user": "Where is my order?",
                                           "reply": "Your order ships today.",
                                           "calls": [{"name": "lookup_order", "args": {}}]}],
                                "error": null
                            },
                            "content_complete": true,
                            "tools_complete": true,
                            "notes": []
                        },
                        "scores": [
                            {"kind": "contains", "outcome": "pass", "score": null,
                             "rationale": "reply contains \"order\""},
                            {"kind": "tools", "outcome": "pass", "score": null,
                             "rationale": "called [lookup_order]; called [lookup_order]"},
                            {"kind": "llm_judge", "outcome": "pass", "score": 5,
                             "rationale": "polite"}
                        ]
                    }
                ]
            })
        );
        assert_eq!(report.pass_rate(), Some(1.0 / 3.0));
        assert_eq!(report.error_count(), 1);
    }

    #[tokio::test]
    async fn unusable_tool_rows_error_tools_scorers() {
        let mut r = run("inv", 50_000, &[("Where is my order?", "order", &[])]);
        r.content_complete = false;
        r.tools_complete = false;
        r.notes = vec![
            "AGENT_RESPONSE at 1970-01-01T00:00:02.000000Z: content truncated (is_truncated)"
                .into(),
            "TOOL_STARTING at 1970-01-01T00:00:03.000000Z: content is NULL".into(),
        ];
        let spec = spec(
            "name: m\nsource: { bigquery: { project: p, dataset: d, table: t } }\nwindow: 1d\n\
             scorers: [tools: [lookup_order]]\n",
        );
        let report = Monitor::new()
            .now(at(100_000))
            .run(&spec, &FakeSource::new(vec![r]), None)
            .await
            .unwrap();
        let score = &report.runs[0].scores[0];
        assert_eq!(score.outcome, Outcome::Error);
        assert_eq!(
            score.rationale,
            "tool calls incomplete: TOOL_STARTING at 1970-01-01T00:00:03.000000Z: content is NULL"
        );
    }

    #[tokio::test]
    async fn an_errored_run_errors_every_scorer() {
        let mut r = run("inv", 50_000, &[("hi", "order", &["lookup_order"])]);
        r.transcript.error = Some("model failure 404".into());
        let judge = FakeJudge::new(&[]);
        let report = Monitor::new()
            .now(at(100_000))
            .run(&spec(SPEC), &FakeSource::new(vec![r]), Some(judge.clone()))
            .await
            .unwrap();
        let scores = &report.runs[0].scores;
        assert_eq!(scores.len(), 3);
        assert!(
            scores
                .iter()
                .all(|s| s.outcome == Outcome::Error && s.rationale == "model failure 404")
        );
        assert_eq!(judge.calls(), 0);
        assert_eq!(report.pass_rate(), Some(0.0));
    }

    #[tokio::test]
    async fn a_missing_turn_is_an_error() {
        let spec = spec(
            "name: m\nsource: { bigquery: { project: p, dataset: d, table: t } }\nwindow: 1h\n\
             scorers: [contains: { text: x, turn: 3 }, regex: { pattern: x, turn: 1 }]\n",
        );
        let report = Monitor::new()
            .now(at(100_000))
            .run(
                &spec,
                &FakeSource::new(vec![run(
                    "inv",
                    99_000,
                    &[("a", "x", &[]), ("b", "y", &[])],
                )]),
                None,
            )
            .await
            .unwrap();
        let scores = &report.runs[0].scores;
        assert_eq!(scores[0].outcome, Outcome::Error);
        assert_eq!(scores[0].rationale, "run has no turn 3");
        assert_eq!(scores[1].outcome, Outcome::Pass);
    }

    #[tokio::test]
    async fn runs_are_sorted_newest_first_and_limited() {
        let spec = spec(
            "name: m\nsource: { bigquery: { project: p, dataset: d, table: t } }\nwindow: 1h\n\
             limit: 2\nscorers: [contains: x]\n",
        );
        let source = FakeSource::new(vec![
            run("a", 10, &[("u", "x", &[])]),
            run("c", 30, &[("u", "x", &[])]),
            run("b", 20, &[("u", "x", &[])]),
        ]);
        let report = Monitor::new()
            .now(at(3_600))
            .concurrency(0)
            .run(&spec, &source, None)
            .await
            .unwrap();
        let ids: Vec<&str> = report
            .runs
            .iter()
            .map(|r| r.run.invocation_id.as_str())
            .collect();
        assert_eq!(ids, ["c", "b"]);
        assert_eq!(source.queries.lock().unwrap()[0].limit, 2);
    }

    #[tokio::test]
    async fn zero_runs_have_no_pass_rate() {
        let report = Monitor::new()
            .run(
                &spec(SPEC),
                &FakeSource::new(Vec::new()),
                Some(FakeJudge::new(&[])),
            )
            .await
            .unwrap();
        assert!(report.runs.is_empty());
        assert_eq!(report.pass_rate(), None);
    }

    #[tokio::test]
    async fn llm_judge_needs_a_judge() {
        let err = Monitor::new()
            .run(&spec(SPEC), &FakeSource::new(Vec::new()), None)
            .await
            .unwrap_err();
        assert!(matches!(err, MonitorError::Judge(_)), "{err}");
    }

    #[tokio::test]
    async fn source_errors_are_returned() {
        let mut source = FakeSource::new(Vec::new());
        source.fail = Some("HTTP 403: denied".into());
        let err = Monitor::new()
            .run(&spec(SPEC), &source, Some(FakeJudge::new(&[])))
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "transcript source: HTTP 403: denied");
    }

    #[tokio::test]
    async fn judge_failures_are_error_results() {
        let report = Monitor::new()
            .now(at(100_000))
            .run(
                &spec(SPEC),
                &FakeSource::new(vec![run(
                    "inv",
                    90_000,
                    &[("u", "order", &["lookup_order"])],
                )]),
                Some(FakeJudge::new(&[])),
            )
            .await
            .unwrap();
        let judge = &report.runs[0].scores[2];
        assert_eq!(judge.outcome, Outcome::Error);
        assert!(
            judge.rationale.starts_with("judge model error"),
            "{}",
            judge.rationale
        );
    }

    #[tokio::test]
    async fn run_with_backend_grades_through_the_backend() {
        use crate::testutil::FakeBackend;

        let backend = FakeBackend::new(4);
        let report = Monitor::new()
            .now(at(100_000))
            .run_with_backend(
                &spec(SPEC),
                &FakeSource::new(vec![run(
                    "inv",
                    90_000,
                    &[("u", "order", &["lookup_order"])],
                )]),
                Some(backend.clone()),
            )
            .await
            .unwrap();
        let judge = &report.runs[0].scores[2];
        assert_eq!(
            (judge.kind.as_str(), judge.outcome, judge.score),
            ("llm_judge", Outcome::Pass, Some(4))
        );
        assert_eq!(judge.rationale, "fake backend");
        assert_eq!(backend.calls(), 1);
        assert_eq!(backend.requests.lock().unwrap()[0].reply, "order");
    }

    #[tokio::test]
    async fn run_with_backend_without_a_backend_needs_no_judge_scorers() {
        let err = Monitor::new()
            .run_with_backend(&spec(SPEC), &FakeSource::new(Vec::new()), None)
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "judge: the spec has llm_judge scorers but no judge model was given"
        );
    }

    /// Runs for the batch tests: two with the same conversation (one judge request), an
    /// incomplete one (no judge request) and an errored one.
    fn batch_runs() -> Vec<ProductionRun> {
        let mut errored = run("inv-err", 10_000, &[("hi", "order", &[])]);
        errored.transcript.error = Some("model failure 404".into());
        vec![
            run(
                "inv-a",
                40_000,
                &[("Where is my order?", "Your order ships.", &["lookup_order"])],
            ),
            run(
                "inv-b",
                30_000,
                &[("Where is my order?", "Your order ships.", &["lookup_order"])],
            ),
            run("inv-c", 20_000, &[("hi", "hello", &[])]),
            incomplete("inv-mid", 15_000),
            errored,
        ]
    }

    #[tokio::test]
    async fn run_batch_matches_a_live_run_with_the_same_verdicts() {
        use crate::testutil::{FAKE_JOB, FakeBackend, FakePredictor};

        let live = Monitor::new()
            .now(at(100_000))
            .run_with_backend(
                &spec(SPEC),
                &FakeSource::new(batch_runs()),
                Some(FakeBackend::new(4)),
            )
            .await
            .unwrap();
        let predictor =
            FakePredictor::new(r#"{"pass": true, "score": 4, "rationale": "fake backend"}"#);
        let (batch, job) = Monitor::new()
            .now(at(100_000))
            .run_batch(
                &spec(SPEC),
                &FakeSource::new(batch_runs()),
                predictor.as_ref(),
            )
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(&batch).unwrap(),
            serde_json::to_value(&live).unwrap()
        );
        assert_eq!(job.as_deref(), Some(FAKE_JOB));
        let calls = predictor.calls();
        assert_eq!(calls.len(), 1);
        let keys: Vec<&str> = calls[0].iter().map(|l| l.key.as_str()).collect();
        assert_eq!(keys, ["r0", "r1"]);
    }

    #[tokio::test]
    async fn run_batch_reports_missing_replies_as_errors() {
        use crate::testutil::FakePredictor;

        let predictor = FakePredictor::with(|_| None);
        let (report, _) = Monitor::new()
            .now(at(100_000))
            .run_batch(
                &spec(SPEC),
                &FakeSource::new(vec![run("inv", 90_000, &[("u", "order", &[])])]),
                predictor.as_ref(),
            )
            .await
            .unwrap();
        let judge = &report.runs[0].scores[2];
        assert_eq!(judge.outcome, Outcome::Error);
        assert_eq!(judge.rationale, "no batch result for this request");
    }

    #[tokio::test]
    async fn run_batch_without_judge_scorers_or_requests_creates_no_job() {
        use crate::testutil::FakePredictor;

        let predictor = FakePredictor::new("unused");
        let no_judge = spec(
            "name: m\nsource: { bigquery: { project: p, dataset: d, table: t } }\nwindow: 1d\n\
             scorers: [contains: order]\n",
        );
        let (report, job) = Monitor::new()
            .now(at(100_000))
            .run_batch(
                &no_judge,
                &FakeSource::new(batch_runs()),
                predictor.as_ref(),
            )
            .await
            .unwrap();
        assert_eq!((report.runs.len(), job), (5, None));
        let (report, job) = Monitor::new()
            .now(at(100_000))
            .run_batch(
                &spec(SPEC),
                &FakeSource::new(Vec::new()),
                predictor.as_ref(),
            )
            .await
            .unwrap();
        assert_eq!((report.runs.len(), job), (0, None));
        assert!(predictor.calls().is_empty());
    }

    #[tokio::test]
    async fn run_batch_returns_a_failed_batch() {
        use crate::testutil::FakePredictor;

        let predictor = FakePredictor::failing("batch: HTTP 403: denied");
        let err = Monitor::new()
            .now(at(100_000))
            .run_batch(
                &spec(SPEC),
                &FakeSource::new(batch_runs()),
                predictor.as_ref(),
            )
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "judge: batch: HTTP 403: denied");
    }
}
