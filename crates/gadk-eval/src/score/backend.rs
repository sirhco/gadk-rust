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

//! The judge seam: [`JudgeBackend`] grades the `llm_judge` and `reference` scorers.

use futures::future::BoxFuture;

use crate::result::ScoreResult;

/// What a judge scorer asks a [`JudgeBackend`] to grade.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct JudgeRequest {
    /// Which scorer is asking, with its criteria.
    pub kind: JudgeKind,
    /// The whole conversation, as [`crate::render_conversation`] renders it.
    pub conversation: String,
    /// The reply of the graded turn (the last turn for `llm_judge`).
    pub reply: String,
}

impl JudgeRequest {
    /// A request of `kind` over `conversation`, grading `reply`.
    pub fn new(kind: JudgeKind, conversation: impl Into<String>, reply: impl Into<String>) -> Self {
        JudgeRequest {
            kind,
            conversation: conversation.into(),
            reply: reply.into(),
        }
    }
}

/// The scorer behind a [`JudgeRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum JudgeKind {
    /// `llm_judge`: grade the whole conversation against `rubric`.
    Rubric {
        /// The scorer's rubric.
        rubric: String,
    },
    /// `reference`: whether the reply to `question` agrees in meaning with `answer`.
    Reference {
        /// The user message of the graded turn.
        question: String,
        /// The reference answer.
        answer: String,
    },
}

/// A [`JudgeBackend`]'s answer.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum JudgeOutcome {
    /// A grade. The scorer passes when `pass` is true and `score` is at least its `min_score`.
    Verdict {
        /// The judge's own pass flag.
        pass: bool,
        /// The score, from 1 (worst) to 5 (best).
        score: u8,
        /// Why.
        rationale: String,
    },
    /// The judge could not grade; the scorer is an [`crate::Outcome::Error`] with this text.
    Error(String),
}

/// Grades `llm_judge` and `reference` scorers.
///
/// [`crate::GeminiJudge`] is the built-in backend; [`crate::Evaluator::with_judge_backend`] and
/// [`crate::score_transcript_with`] take any other. `grade` returns a boxed future so the trait
/// works as `dyn JudgeBackend`; implementations write `Box::pin(async move { … })`.
///
/// ```
/// use futures::future::BoxFuture;
/// use gadk_eval::{JudgeBackend, JudgeKind, JudgeOutcome, JudgeRequest};
///
/// /// Passes every reply that mentions the reference answer.
/// struct Mentions;
///
/// impl JudgeBackend for Mentions {
///     fn grade<'a>(&'a self, req: &'a JudgeRequest) -> BoxFuture<'a, JudgeOutcome> {
///         Box::pin(async move {
///             match &req.kind {
///                 JudgeKind::Reference { answer, .. } => JudgeOutcome::Verdict {
///                     pass: req.reply.contains(answer.as_str()),
///                     score: 5,
///                     rationale: "substring check".into(),
///                 },
///                 _ => JudgeOutcome::Error("rubrics are not supported".into()),
///             }
///         })
///     }
/// }
///
/// # futures::executor::block_on(async {
/// let req = JudgeRequest::new(
///     JudgeKind::Reference { question: "Capital?".into(), answer: "Paris".into() },
///     "User: Capital?\nAgent: Paris\n\n",
///     "Paris",
/// );
/// assert!(matches!(Mentions.grade(&req).await, JudgeOutcome::Verdict { pass: true, .. }));
/// # });
/// ```
pub trait JudgeBackend: Send + Sync {
    /// Grades one request.
    fn grade<'a>(&'a self, req: &'a JudgeRequest) -> BoxFuture<'a, JudgeOutcome>;
}

/// The scorer result for `outcome`: a verdict passes when `pass` is set and `score >= min_score`;
/// an error is an [`crate::Outcome::Error`] with its text.
pub(crate) fn score_outcome(kind: &str, outcome: JudgeOutcome, min_score: u8) -> ScoreResult {
    match outcome {
        JudgeOutcome::Verdict {
            pass,
            score,
            rationale,
        } => ScoreResult::judged(kind, pass && score >= min_score, score, rationale),
        JudgeOutcome::Error(e) => ScoreResult::error(kind, e),
    }
}
