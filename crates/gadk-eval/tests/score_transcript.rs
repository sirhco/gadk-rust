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

//! `score_transcript` grades a transcript exactly as `Evaluator::run` does.

use std::sync::Arc;
use std::time::Duration;

use gadk_agent::Agent;
use gadk_engine::llm::{LlmAgentConfig, agent as llm_agent};
use gadk_engine::testutil::MockModel;
use gadk_eval::{
    EvalSet, Evaluator, Outcome, RunTranscript, ScorerSpec, TurnRecord, Variant, score_transcript,
};
use gadk_genai::{Content, ROLE_MODEL};

const VERDICT: &str = r#"{"pass": true, "score": 4, "rationale": "fine"}"#;

const SET: &str = r#"
name: parity
cases:
  - id: a
    turns: ["I want a refund"]
    expect:
      - contains: "30 days"
      - not_contains: "never"
      - regex: "(?i)within"
      - equals: "Within 30 days."
      - tools: [lookup_order]
      - reference: "Refunds within 30 days."
      - llm_judge: "Polite"
"#;

fn model(texts: &[&str]) -> Arc<MockModel> {
    MockModel::from_contents(
        texts
            .iter()
            .map(|t| Content::from_text(*t, ROLE_MODEL))
            .collect(),
    )
}

fn agent() -> Arc<dyn Agent> {
    llm_agent::new(LlmAgentConfig {
        name: "a".into(),
        model: Some(model(&["Within 30 days."])),
        ..Default::default()
    })
    .unwrap()
}

#[tokio::test]
async fn same_results_as_the_evaluator() {
    let set = EvalSet::from_yaml(SET).unwrap();
    let report = Evaluator::new(model(&[VERDICT, VERDICT]))
        .run(&set, &[Variant::new("v")], |_| Ok::<_, String>(agent()))
        .await
        .unwrap();
    let run = &report.variants[0].runs[0];
    let scores = score_transcript(
        model(&[VERDICT, VERDICT]),
        Duration::from_secs(120),
        &set.cases[0].expect,
        &run.transcript,
    )
    .await;
    assert_eq!(scores, run.scores);
    let outcomes: Vec<_> = scores.iter().map(|s| s.outcome).collect();
    assert_eq!(
        outcomes,
        [
            Outcome::Pass,
            Outcome::Pass,
            Outcome::Pass,
            Outcome::Pass,
            Outcome::Fail,
            Outcome::Pass,
            Outcome::Pass
        ]
    );
}

#[tokio::test]
async fn a_failed_run_errors_every_scorer() {
    let scorers = [
        ScorerSpec::from_value(&serde_json::json!({"contains": "x"})).unwrap(),
        ScorerSpec::from_value(&serde_json::json!({"llm_judge": "be nice"})).unwrap(),
    ];
    let t = RunTranscript {
        turns: vec![TurnRecord {
            user: "u".into(),
            reply: "x".into(),
            calls: Vec::new(),
        }],
        error: Some("agent failed".into()),
    };
    let judge = model(&[]);
    let scores = score_transcript(judge.clone(), Duration::from_secs(1), &scorers, &t).await;
    assert_eq!(scores.len(), 2);
    for s in &scores {
        assert_eq!(s.outcome, Outcome::Error);
        assert_eq!(s.rationale, "agent failed");
    }
    assert_eq!(scores[1].kind, "llm_judge");
}
