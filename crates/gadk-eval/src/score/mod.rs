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

//! The scorers, and [`score_scorers`], which applies a list of [`ScorerSpec`]s to a transcript
//! for both [`crate::Evaluator`] and [`score_transcript`].

pub(crate) mod backend;
pub(crate) mod check;
pub(crate) mod judge;
pub(crate) mod tools;

use std::sync::Arc;
use std::time::Duration;

use gadk_model::Llm;

use crate::result::{RunTranscript, ScoreResult};
use crate::spec::ScorerSpec;
use backend::{JudgeBackend, JudgeKind, JudgeRequest, score_outcome};
use judge::{GeminiJudge, render_conversation};

/// Grades the transcript `t` with `scorers`, in order, the way [`crate::Evaluator`] grades a run:
/// when `t.error` is set every scorer is an [`crate::Outcome::Error`] carrying that error;
/// otherwise each scorer grades `t`. `llm_judge` and `reference` scorers ask `judge`, each call
/// bounded by `judge_timeout` (values below 1 ms mean 1 ms).
pub async fn score_transcript(
    judge: Arc<dyn Llm>,
    judge_timeout: Duration,
    scorers: &[ScorerSpec],
    t: &RunTranscript,
) -> Vec<ScoreResult> {
    let judge = GeminiJudge::new(judge, judge_timeout.max(Duration::from_millis(1)));
    score_scorers(&judge, scorers, t).await
}

/// [`score_transcript`] with any [`JudgeBackend`]: `llm_judge` and `reference` scorers are graded
/// by `backend`, and a verdict passes when its `pass` flag is set and its score is at least the
/// scorer's `min_score`. The other scorers and a failed run behave as in [`score_transcript`].
pub async fn score_transcript_with(
    backend: &dyn JudgeBackend,
    scorers: &[ScorerSpec],
    t: &RunTranscript,
) -> Vec<ScoreResult> {
    score_scorers(backend, scorers, t).await
}

/// Scores one transcript: every scorer errors when the run failed; otherwise each scorer grades
/// it.
pub(crate) async fn score_scorers(
    judge: &dyn JudgeBackend,
    scorers: &[ScorerSpec],
    t: &RunTranscript,
) -> Vec<ScoreResult> {
    if let Some(e) = &t.error {
        return scorers
            .iter()
            .map(|s| ScoreResult::error(s.kind(), e.clone()))
            .collect();
    }
    let mut out = Vec::with_capacity(scorers.len());
    for s in scorers {
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
            } => {
                let question = t.turn(*turn).map_or("", |r| r.user.as_str());
                let kind = JudgeKind::Reference {
                    question: question.to_owned(),
                    answer: answer.clone(),
                };
                let req = JudgeRequest::new(kind, render_conversation(t), t.reply(*turn));
                score_outcome("reference", judge.grade(&req).await, *min_score)
            }
            ScorerSpec::LlmJudge { rubric, min_score } => {
                let kind = JudgeKind::Rubric {
                    rubric: rubric.clone(),
                };
                let req = JudgeRequest::new(kind, render_conversation(t), t.reply(None));
                score_outcome("llm_judge", judge.grade(&req).await, *min_score)
            }
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::{Outcome, TurnRecord};
    use backend::JudgeOutcome;
    use futures::future::BoxFuture;
    use std::sync::Mutex;

    /// Answers every request with the next scripted outcome and records the requests.
    struct Scripted {
        outcomes: Mutex<Vec<JudgeOutcome>>,
        seen: Mutex<Vec<JudgeRequest>>,
    }

    impl Scripted {
        fn new(mut outcomes: Vec<JudgeOutcome>) -> Self {
            outcomes.reverse();
            Scripted {
                outcomes: Mutex::new(outcomes),
                seen: Mutex::new(Vec::new()),
            }
        }
    }

    impl JudgeBackend for Scripted {
        fn grade<'a>(&'a self, req: &'a JudgeRequest) -> BoxFuture<'a, JudgeOutcome> {
            self.seen.lock().unwrap().push(req.clone());
            let next = self.outcomes.lock().unwrap().pop().unwrap();
            Box::pin(async move { next })
        }
    }

    fn verdict(pass: bool, score: u8) -> JudgeOutcome {
        JudgeOutcome::Verdict {
            pass,
            score,
            rationale: format!("s{score}"),
        }
    }

    fn transcript() -> RunTranscript {
        RunTranscript {
            turns: vec![
                TurnRecord {
                    user: "hi".into(),
                    reply: "hello".into(),
                    calls: vec![],
                },
                TurnRecord {
                    user: "refund?".into(),
                    reply: "within 30 days".into(),
                    calls: vec![],
                },
            ],
            error: None,
        }
    }

    fn scorer(v: serde_json::Value) -> ScorerSpec {
        ScorerSpec::from_value(&v).unwrap()
    }

    #[tokio::test]
    async fn verdicts_and_errors_map_to_score_results() {
        let judge = Scripted::new(vec![
            verdict(true, 2),
            verdict(true, 4),
            verdict(false, 5),
            JudgeOutcome::Error("x".into()),
        ]);
        let scorers = [
            scorer(serde_json::json!({"llm_judge": {"rubric": "r", "min_score": 3}})),
            scorer(serde_json::json!({"llm_judge": {"rubric": "r", "min_score": 3}})),
            scorer(serde_json::json!({"reference": {"answer": "a", "min_score": 3}})),
            scorer(serde_json::json!({"reference": {"answer": "a", "min_score": 3}})),
        ];
        let got = score_transcript_with(&judge, &scorers, &transcript()).await;
        let summary: Vec<_> = got
            .iter()
            .map(|r| (r.kind.as_str(), r.outcome, r.score, r.rationale.as_str()))
            .collect();
        assert_eq!(
            summary,
            [
                ("llm_judge", Outcome::Fail, Some(2), "s2"),
                ("llm_judge", Outcome::Pass, Some(4), "s4"),
                ("reference", Outcome::Fail, Some(5), "s5"),
                ("reference", Outcome::Error, None, "x"),
            ]
        );
    }

    #[tokio::test]
    async fn requests_carry_the_conversation_reply_and_criteria() {
        let judge = Scripted::new(vec![verdict(true, 5), verdict(true, 5)]);
        let scorers = [
            scorer(serde_json::json!({"llm_judge": "polite"})),
            scorer(serde_json::json!({"reference": {"answer": "hello there", "turn": 1}})),
        ];
        let t = transcript();
        score_transcript_with(&judge, &scorers, &t).await;
        let seen = judge.seen.lock().unwrap();
        let conversation = render_conversation(&t);
        assert_eq!(
            seen[0],
            JudgeRequest::new(
                JudgeKind::Rubric {
                    rubric: "polite".into()
                },
                conversation.clone(),
                "within 30 days",
            )
        );
        assert_eq!(
            seen[1],
            JudgeRequest::new(
                JudgeKind::Reference {
                    question: "hi".into(),
                    answer: "hello there".into()
                },
                conversation,
                "hello",
            )
        );
    }

    #[tokio::test]
    async fn a_failed_run_never_asks_the_backend() {
        let judge = Scripted::new(vec![]);
        let mut t = transcript();
        t.error = Some("boom".into());
        let got = score_transcript_with(
            &judge,
            &[scorer(serde_json::json!({"llm_judge": "polite"}))],
            &t,
        )
        .await;
        assert_eq!(
            (got[0].outcome, got[0].rationale.as_str()),
            (Outcome::Error, "boom")
        );
        assert!(judge.seen.lock().unwrap().is_empty());
    }
}
