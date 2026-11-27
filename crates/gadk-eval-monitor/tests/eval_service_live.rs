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

//! Live test: three pointwise calls (a rubric, a `reference` check and a rubric with literal
//! braces) and one pairwise call to the Vertex AI Gen AI evaluation service.
//!
//! ```sh
//! GADK_EVAL_MONITOR_LIVE_PROJECT=<project> \
//!   cargo test -p gadk-eval-monitor --all-features live_eval_service -- --ignored --nocapture
//! ```
//!
//! Uses Application Default Credentials (Vertex AI User). Optional:
//! `GADK_EVAL_MONITOR_LIVE_EVAL_LOCATION` (default `global`) and
//! `GADK_EVAL_MONITOR_LIVE_JUDGE_MODEL` (default `gemini-3.8-flash`).

use std::time::Duration;

use gadk_eval::{JudgeBackend, JudgeKind, JudgeOutcome, JudgeRequest};
use gadk_eval_monitor::{
    DEFAULT_PAIRWISE_CRITERIA, EvalServiceJudge, PairwiseChoice, PairwiseJudge, PairwiseOutcome,
};

#[tokio::test]
#[ignore = "needs GADK_EVAL_MONITOR_LIVE_PROJECT and ADC"]
async fn live_eval_service() {
    let Ok(project) = std::env::var("GADK_EVAL_MONITOR_LIVE_PROJECT") else {
        panic!("set GADK_EVAL_MONITOR_LIVE_PROJECT");
    };
    let location =
        std::env::var("GADK_EVAL_MONITOR_LIVE_EVAL_LOCATION").unwrap_or_else(|_| "global".into());
    let model = std::env::var("GADK_EVAL_MONITOR_LIVE_JUDGE_MODEL")
        .unwrap_or_else(|_| "gemini-3.8-flash".into());
    let judge =
        EvalServiceJudge::new(&project, &location, &model, Duration::from_secs(120)).unwrap();

    let req = JudgeRequest::new(
        JudgeKind::Rubric {
            rubric: "The agent answers the question correctly and politely.".into(),
        },
        "User: What is 2+2?\nAgent: 4\n\n",
        "4",
    );
    let outcome = judge.grade(&req).await;
    println!("pointwise: {outcome:?}");
    let JudgeOutcome::Verdict { score, .. } = outcome else {
        panic!("no pointwise verdict: {outcome:?}");
    };
    assert!((1..=5).contains(&score), "{score}");

    // The `reference` template.
    let req = JudgeRequest::new(
        JudgeKind::Reference {
            question: "What is the capital of France?".into(),
            answer: "Paris".into(),
        },
        "User: What is the capital of France?\nAgent: The capital of France is Paris.\n\n",
        "The capital of France is Paris.",
    );
    let outcome = judge.grade(&req).await;
    println!("reference: {outcome:?}");
    let JudgeOutcome::Verdict { score, .. } = outcome else {
        panic!("no reference verdict: {outcome:?}");
    };
    assert!((1..=5).contains(&score), "{score}");

    // A rubric with literal braces, which must not be read as template placeholders.
    let req = JudgeRequest::new(
        JudgeKind::Rubric {
            rubric: "The reply names the JSON key {name} and keeps the text {response} as is."
                .into(),
        },
        "User: Which key holds the user's name?\nAgent: The key {name} holds it.\n\n",
        "The key {name} holds it.",
    );
    let outcome = judge.grade(&req).await;
    println!("rubric with braces: {outcome:?}");
    let JudgeOutcome::Verdict { score, .. } = outcome else {
        panic!("no verdict for a rubric with braces: {outcome:?}");
    };
    assert!((1..=5).contains(&score), "{score}");

    let outcome = judge
        .compare(
            DEFAULT_PAIRWISE_CRITERIA,
            "User: What is the capital of France?\nAgent: I think it might be Lyon.\n\n",
            "User: What is the capital of France?\nAgent: Paris.\n\n",
        )
        .await;
    println!("pairwise: {outcome:?}");
    let PairwiseOutcome::Choice { choice, .. } = outcome else {
        panic!("no pairwise choice: {outcome:?}");
    };
    assert!(
        matches!(
            choice,
            PairwiseChoice::Baseline | PairwiseChoice::Candidate | PairwiseChoice::Tie
        ),
        "{choice:?}"
    );
}
