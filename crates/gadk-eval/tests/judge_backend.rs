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
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The judge seam from outside the crate: the root re-exports, `Evaluator::with_backend`,
//! `Evaluator::with_judge_backend` and `score_transcript_with`.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures::future::BoxFuture;
use gadk_agent::Agent;
use gadk_engine::llm::{LlmAgentConfig, agent as llm_agent};
use gadk_engine::testutil::MockModel;
use gadk_eval::{
    EvalSet, Evaluator, GeminiJudge, JudgeBackend, JudgeKind, JudgeOutcome, JudgeRequest, Outcome,
    RunTranscript, TurnRecord, Variant, render_conversation, score_transcript_with,
};
use gadk_genai::{Content, ROLE_MODEL};

/// Scores 4 for every rubric and 2 for every reference, counting calls.
#[derive(Default)]
struct Fake {
    calls: AtomicUsize,
}

impl JudgeBackend for Fake {
    fn grade<'a>(&'a self, req: &'a JudgeRequest) -> BoxFuture<'a, JudgeOutcome> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let score = match &req.kind {
                JudgeKind::Rubric { .. } => 4,
                _ => 2,
            };
            JudgeOutcome::Verdict {
                pass: true,
                score,
                rationale: format!("fake: {}", req.reply),
            }
        })
    }
}

const SET: &str = r#"
name: seam
cases:
  - id: a
    turns: ["I want a refund"]
    expect:
      - llm_judge: { rubric: "Polite", min_score: 3 }
      - reference: { answer: "Within 30 days.", min_score: 3 }
"#;

fn agent() -> Arc<dyn Agent> {
    llm_agent::new(LlmAgentConfig {
        name: "a".into(),
        model: Some(MockModel::from_contents(vec![Content::from_text(
            "Within 30 days.",
            ROLE_MODEL,
        )])),
        ..Default::default()
    })
    .unwrap()
}

fn outcomes(scores: &[gadk_eval::ScoreResult]) -> Vec<(Outcome, Option<u8>)> {
    scores.iter().map(|s| (s.outcome, s.score)).collect()
}

#[tokio::test]
async fn with_backend_grades_through_the_backend() {
    let fake = Arc::new(Fake::default());
    let set = EvalSet::from_yaml(SET).unwrap();
    let report = Evaluator::with_backend(fake.clone())
        .run(&set, &[Variant::new("v")], |_| Ok::<_, String>(agent()))
        .await
        .unwrap();
    let run = &report.variants[0].runs[0];
    assert_eq!(
        outcomes(&run.scores),
        [(Outcome::Pass, Some(4)), (Outcome::Fail, Some(2))]
    );
    assert_eq!(run.scores[0].rationale, "fake: Within 30 days.");
    assert_eq!(fake.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn with_judge_backend_replaces_the_judge_model() {
    let fake = Arc::new(Fake::default());
    let unused = MockModel::from_contents(vec![]);
    let set = EvalSet::from_yaml(SET).unwrap();
    let report = Evaluator::new(unused.clone())
        .with_judge_backend(fake.clone())
        .run_measured(&set, &[Variant::new("v")], |_| Ok::<_, String>(agent()))
        .await
        .unwrap();
    let run = &report.report.variants[0].runs[0];
    assert_eq!(
        outcomes(&run.scores),
        [(Outcome::Pass, Some(4)), (Outcome::Fail, Some(2))]
    );
    assert_eq!(fake.calls.load(Ordering::SeqCst), 2);
    assert!(unused.requests().is_empty());
}

#[tokio::test]
async fn score_transcript_with_grades_through_the_backend() {
    let set = EvalSet::from_yaml(SET).unwrap();
    let t = RunTranscript {
        turns: vec![TurnRecord {
            user: "I want a refund".into(),
            reply: "Within 30 days.".into(),
            calls: Vec::new(),
        }],
        error: None,
    };
    let fake = Fake::default();
    let scores = score_transcript_with(&fake, &set.cases[0].expect, &t).await;
    assert_eq!(
        outcomes(&scores),
        [(Outcome::Pass, Some(4)), (Outcome::Fail, Some(2))]
    );
    assert_eq!(
        render_conversation(&t),
        "User: I want a refund\nAgent: Within 30 days.\n\n"
    );
}

#[tokio::test]
async fn gemini_judge_is_a_backend() {
    let model = MockModel::from_contents(vec![Content::from_text(
        r#"{"pass": true, "score": 5, "rationale": "ok"}"#,
        ROLE_MODEL,
    )]);
    let judge: Arc<dyn JudgeBackend> = Arc::new(GeminiJudge::new(model, Duration::from_secs(5)));
    let req = JudgeRequest::new(
        JudgeKind::Rubric {
            rubric: "Polite".into(),
        },
        "User: hi\nAgent: hello\n\n",
        "hello",
    );
    assert_eq!(
        judge.grade(&req).await,
        JudgeOutcome::Verdict {
            pass: true,
            score: 5,
            rationale: "ok".into()
        }
    );
}
