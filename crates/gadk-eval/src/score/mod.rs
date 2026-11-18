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

pub(crate) mod check;
pub(crate) mod judge;
pub(crate) mod tools;

use std::sync::Arc;
use std::time::Duration;

use gadk_model::Llm;

use crate::result::{RunTranscript, ScoreResult};
use crate::spec::ScorerSpec;
use judge::Judge;

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
    let judge = Judge::new(judge, judge_timeout.max(Duration::from_millis(1)));
    score_scorers(&judge, scorers, t).await
}

/// Scores one transcript: every scorer errors when the run failed; otherwise each scorer grades
/// it.
pub(crate) async fn score_scorers(
    judge: &Judge,
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
            } => judge.reference(t, *turn, answer, *min_score).await,
            ScorerSpec::LlmJudge { rubric, min_score } => judge.rubric(t, rubric, *min_score).await,
        });
    }
    out
}
