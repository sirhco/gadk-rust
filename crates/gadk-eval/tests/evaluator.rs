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

//! End-to-end tests of `Evaluator::run` with scripted models (no network).

use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures::StreamExt as _;
use futures::stream::BoxStream;
use gadk_agent::Agent;
use gadk_engine::llm::{LlmAgentConfig, agent as llm_agent};
use gadk_engine::testutil::MockModel;
use gadk_eval::{EvalError, EvalSet, Evaluator, Outcome, Variant};
use gadk_genai::{Content, ROLE_MODEL};
use gadk_model::{Llm, LlmRequest, LlmResponse, ModelError};

fn agent(model: Arc<dyn Llm>) -> Arc<dyn Agent> {
    llm_agent::new(LlmAgentConfig {
        name: "a".into(),
        model: Some(model),
        ..Default::default()
    })
    .unwrap()
}

/// A model that answers every call with `text`.
fn always(text: &str, calls: usize) -> Arc<MockModel> {
    MockModel::from_contents(
        (0..calls)
            .map(|_| Content::from_text(text, ROLE_MODEL))
            .collect(),
    )
}

fn judge(verdicts: &[&str]) -> Arc<MockModel> {
    MockModel::from_contents(
        verdicts
            .iter()
            .map(|v| Content::from_text(*v, ROLE_MODEL))
            .collect(),
    )
}

const TWO_CASES: &str = r#"
name: refunds
cases:
  - id: a
    turns: ["I want a refund"]
    expect: [contains: "30 days"]
  - id: b
    turns: ["Can I get a refund?"]
    expect: [contains: "30 days"]
"#;

#[tokio::test]
async fn every_variant_runs_every_case_runs_per_case_times() {
    let set = EvalSet::from_yaml(TWO_CASES).unwrap();
    let builds = AtomicUsize::new(0);
    let report = Evaluator::new(judge(&[]))
        .runs_per_case(2)
        .run(&set, &[Variant::new("old"), Variant::new("new")], |v| {
            builds.fetch_add(1, Ordering::SeqCst);
            let text = if v.name() == "new" {
                "Within 30 days."
            } else {
                "Sure!"
            };
            Ok::<_, String>(agent(always(text, 4)))
        })
        .await
        .unwrap();
    assert_eq!(builds.load(Ordering::SeqCst), 2);
    assert_eq!(report.set, "refunds");
    let names: Vec<_> = report.variants.iter().map(|v| v.name.as_str()).collect();
    assert_eq!(names, ["old", "new"]);
    for v in &report.variants {
        let order: Vec<_> = v.runs.iter().map(|r| (r.case.as_str(), r.run)).collect();
        assert_eq!(order, [("a", 1), ("a", 2), ("b", 1), ("b", 2)]);
    }
    let outcomes = |i: usize| -> Vec<Outcome> {
        report.variants[i]
            .runs
            .iter()
            .map(|r| r.scores[0].outcome)
            .collect()
    };
    assert_eq!(outcomes(0), [Outcome::Fail; 4]);
    assert_eq!(outcomes(1), [Outcome::Pass; 4]);
    assert!(!report.all_passed());
}

#[tokio::test]
async fn a_build_error_only_affects_its_variant() {
    let set = EvalSet::from_yaml(TWO_CASES).unwrap();
    let report = Evaluator::new(judge(&[]))
        .run(&set, &[Variant::new("broken"), Variant::new("ok")], |v| {
            if v.name() == "broken" {
                Err("no such prompt version".to_owned())
            } else {
                Ok(agent(always("Within 30 days.", 2)))
            }
        })
        .await
        .unwrap();
    for r in &report.variants[0].runs {
        assert_eq!(r.scores[0].outcome, Outcome::Error);
        assert_eq!(
            r.scores[0].rationale,
            "building variant broken: no such prompt version"
        );
        assert_eq!(r.transcript.turns.len(), 0);
    }
    assert!(
        report.variants[1]
            .runs
            .iter()
            .all(|r| r.scores[0].outcome == Outcome::Pass)
    );
}

#[tokio::test]
async fn invalid_variants_fail_before_build() {
    let set = EvalSet::from_yaml(TWO_CASES).unwrap();
    for vs in [
        vec![],
        vec![Variant::new("x"), Variant::new("x")],
        vec![Variant::new("")],
    ] {
        let r = Evaluator::new(judge(&[]))
            .run(&set, &vs, |_| -> Result<Arc<dyn Agent>, String> {
                panic!("build must not be called")
            })
            .await;
        assert!(matches!(r, Err(EvalError::Variant(_))), "{vs:?}");
    }
}

#[tokio::test]
async fn multi_turn_case_with_judge_scorers_and_json_round_trip() {
    let set = EvalSet::from_yaml(
        r#"
name: support
judge_rubric: "Polite and accurate."
cases:
  - id: refund-window
    state: { tier: gold }
    turns: ["I want a refund", "Order 123, bought 40 days ago"]
    expect:
      - contains: { text: "which order", ignore_case: true, turn: 1 }
      - regex: "store credit"
      - reference: "Refunds only within 30 days; offer store credit."
      - llm_judge: {}
"#,
    )
    .unwrap();
    let model = MockModel::from_contents(vec![
        Content::from_text("Which order?", ROLE_MODEL),
        Content::from_text(
            "That is past 30 days; I can offer store credit.",
            ROLE_MODEL,
        ),
    ]);
    let judge = judge(&[
        r#"{"pass": true, "score": 5, "rationale": "consistent"}"#,
        r#"```json
{"pass": true, "score": 4, "rationale": "polite"}
```"#,
    ]);
    let report = Evaluator::new(judge.clone())
        .concurrency(1)
        .run(&set, &[Variant::new("only")], |_| {
            Ok::<_, String>(agent(model.clone()))
        })
        .await
        .unwrap();
    let run = &report.variants[0].runs[0];
    assert_eq!(run.transcript.error, None);
    assert_eq!(run.transcript.turns.len(), 2);
    let got: Vec<_> = run
        .scores
        .iter()
        .map(|s| (s.kind.as_str(), s.outcome, s.score))
        .collect();
    assert_eq!(
        got,
        [
            ("contains", Outcome::Pass, None),
            ("regex", Outcome::Pass, None),
            ("reference", Outcome::Pass, Some(5)),
            ("llm_judge", Outcome::Pass, Some(4)),
        ]
    );
    assert!(report.all_passed());
    assert_eq!(judge.requests().len(), 2);
    assert!(
        report
            .summary()
            .contains("only     overall    4/4 (100%)  4.50        0"),
        "{}",
        report.summary()
    );

    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("report.json");
    report.write_json(&p).unwrap();
    let back: gadk_eval::EvalReport =
        serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    assert_eq!(back, report);
}

/// A model that never answers.
struct Hang;

impl Llm for Hang {
    fn name(&self) -> &str {
        "hang"
    }
    fn generate_content(
        &self,
        _req: LlmRequest,
        _stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        futures::stream::pending().boxed()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[tokio::test(start_paused = true)]
async fn a_timed_out_run_errors_every_scorer() {
    let set = EvalSet::from_yaml(
        "name: s\ncases:\n  - id: a\n    turns: [hi]\n    expect: [contains: x, llm_judge: be nice]\n",
    )
    .unwrap();
    let judge = judge(&[]);
    let report = Evaluator::new(judge.clone())
        .run_timeout(Duration::from_millis(50))
        .run(&set, &[Variant::new("v")], |_| {
            Ok::<_, String>(agent(Arc::new(Hang)))
        })
        .await
        .unwrap();
    let run = &report.variants[0].runs[0];
    assert_eq!(
        run.transcript.error.as_deref(),
        Some("run timed out after 50ms")
    );
    for s in &run.scores {
        assert_eq!(
            (s.outcome, s.rationale.as_str()),
            (Outcome::Error, "run timed out after 50ms")
        );
    }
    assert_eq!(judge.requests().len(), 0, "a failed run is not judged");
}

/// A model that answers after a delay and records the most calls in flight at once.
#[derive(Default)]
struct Slow {
    in_flight: Arc<AtomicUsize>,
    max: Arc<AtomicUsize>,
}

impl Llm for Slow {
    fn name(&self) -> &str {
        "slow"
    }
    fn generate_content(
        &self,
        _req: LlmRequest,
        _stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        let (in_flight, max) = (self.in_flight.clone(), self.max.clone());
        futures::stream::once(async move {
            let n = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            max.fetch_max(n, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(30)).await;
            in_flight.fetch_sub(1, Ordering::SeqCst);
            Ok(LlmResponse {
                content: Some(Content::from_text("ok", ROLE_MODEL)),
                ..Default::default()
            })
        })
        .boxed()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[tokio::test(start_paused = true)]
async fn concurrency_bounds_runs_in_flight() {
    let cases: String = (0..6)
        .map(|i| format!("  - id: c{i}\n    turns: [hi]\n    expect: [contains: ok]\n"))
        .collect();
    let set = EvalSet::from_yaml(&format!("name: s\ncases:\n{cases}")).unwrap();
    let slow = Arc::new(Slow::default());
    let report = Evaluator::new(judge(&[]))
        .concurrency(2)
        .run(&set, &[Variant::new("v")], |_| {
            Ok::<_, String>(agent(slow.clone()))
        })
        .await
        .unwrap();
    assert!(report.all_passed(), "{}", report.summary());
    assert_eq!(slow.max.load(Ordering::SeqCst), 2);
}
