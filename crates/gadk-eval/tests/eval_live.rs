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

//! Live test: a tiny eval set with Gemini on Vertex AI as both agent and judge.
//!
//! ```sh
//! GADK_EVAL_LIVE_PROJECT=<project> cargo test -p gadk-eval --test eval_live -- --ignored --nocapture
//! ```
//!
//! Uses Application Default Credentials. Optional: `GADK_EVAL_LIVE_LOCATION` (default
//! `us-central1`), `GADK_EVAL_LIVE_MODEL` (default `gemini-2.5-flash`).

use gadk_engine::llm::{LlmAgentConfig, agent as llm_agent};
use gadk_eval::{EvalSet, Evaluator, Variant};
use gadk_gemini::{ClientConfig, GeminiModel};
use gadk_model::Backend;

const SET: &str = r#"
name: live-smoke
cases:
  - id: capital
    turns: ["What is the capital of France?"]
    expect:
      - contains: { text: "paris", ignore_case: true }
      - reference: "Paris is the capital of France."
      - llm_judge: "The answer is correct and at most two sentences."
"#;

#[tokio::test]
#[ignore = "needs GADK_EVAL_LIVE_PROJECT and Application Default Credentials"]
async fn gemini_agent_and_judge() {
    let Ok(project) = std::env::var("GADK_EVAL_LIVE_PROJECT") else {
        panic!("set GADK_EVAL_LIVE_PROJECT to run this test");
    };
    let location =
        std::env::var("GADK_EVAL_LIVE_LOCATION").unwrap_or_else(|_| "us-central1".into());
    let model_name =
        std::env::var("GADK_EVAL_LIVE_MODEL").unwrap_or_else(|_| "gemini-2.5-flash".into());
    let model = GeminiModel::new(
        &model_name,
        ClientConfig {
            backend: Some(Backend::VertexAi),
            project: Some(project),
            location: Some(location),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let set = EvalSet::from_yaml(SET).unwrap();
    let report = Evaluator::new(model.clone())
        .run(&set, &[Variant::new("live")], |_| {
            llm_agent::new(LlmAgentConfig {
                name: "geo".into(),
                model: Some(model.clone()),
                instruction: "Answer in one short sentence.".into(),
                ..Default::default()
            })
        })
        .await
        .unwrap();
    println!("{}", report.summary());
    for s in &report.variants[0].runs[0].scores {
        println!("{}: {:?} {:?} {}", s.kind, s.outcome, s.score, s.rationale);
    }
    assert!(report.all_passed(), "{report:#?}");
}
