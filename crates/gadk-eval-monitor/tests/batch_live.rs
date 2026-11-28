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

//! Live test: one 2-line Vertex AI batch prediction job (a rubric and a `reference` check),
//! staged in Cloud Storage, graded in location `us`.
//!
//! ```sh
//! GADK_EVAL_MONITOR_LIVE_PROJECT=<project> \
//! GADK_EVAL_MONITOR_LIVE_BATCH_STAGING=gs://<bucket>/<prefix> \
//!   cargo test -p gadk-eval-monitor --all-features live_batch_judging -- --ignored --nocapture
//! ```
//!
//! Uses Application Default Credentials (Vertex AI User, and Storage Object User on the staging
//! bucket). A tiny job takes about 6 minutes. The staged objects are kept. Optional:
//! `GADK_EVAL_MONITOR_LIVE_JUDGE_MODEL` (default `gemini-3.8-flash`).

use std::time::Duration;

use gadk_eval::{JudgeBackend, JudgeKind, JudgeOutcome, JudgeRequest};
use gadk_eval_monitor::{RecordingJudge, VertexBatchPredictor, judge_in_batch};

#[tokio::test]
#[ignore = "needs GADK_EVAL_MONITOR_LIVE_PROJECT, GADK_EVAL_MONITOR_LIVE_BATCH_STAGING and ADC"]
async fn live_batch_judging() {
    let Ok(project) = std::env::var("GADK_EVAL_MONITOR_LIVE_PROJECT") else {
        panic!("set GADK_EVAL_MONITOR_LIVE_PROJECT");
    };
    let Ok(staging) = std::env::var("GADK_EVAL_MONITOR_LIVE_BATCH_STAGING") else {
        panic!("set GADK_EVAL_MONITOR_LIVE_BATCH_STAGING to gs://<bucket>/<prefix>");
    };
    let model = std::env::var("GADK_EVAL_MONITOR_LIVE_JUDGE_MODEL")
        .unwrap_or_else(|_| "gemini-3.8-flash".into());
    let predictor =
        VertexBatchPredictor::new(&project, "us", &model, &staging, Duration::from_secs(3600))
            .unwrap();

    let requests = [
        JudgeRequest::new(
            JudgeKind::Rubric {
                rubric: "The agent answers the question correctly and politely.".into(),
            },
            "User: What is 2+2?\nAgent: 4\n\n",
            "4",
        ),
        JudgeRequest::new(
            JudgeKind::Reference {
                question: "What is the capital of France?".into(),
                answer: "Paris".into(),
            },
            "User: What is the capital of France?\nAgent: Lyon\n\n",
            "Lyon",
        ),
    ];
    let recorder = RecordingJudge::new();
    for req in &requests {
        recorder.grade(req).await;
    }
    let (replay, job) = judge_in_batch(&predictor, &recorder).await.unwrap();
    println!("batch job: {job:?}");
    assert!(job.is_some());
    for req in &requests {
        let outcome = replay.grade(req).await;
        println!("{:?}: {outcome:?}", req.kind);
        let JudgeOutcome::Verdict { score, .. } = outcome else {
            panic!("no verdict: {outcome:?}");
        };
        assert!((1..=5).contains(&score), "{score}");
    }
}
