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
//! Tests for prompt providers used by LLM agents.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use gadk_engine::llm::{LlmAgentConfig, agent as llm_agent};
use gadk_engine::testutil::telemetry::capture_spans;
use gadk_engine::testutil::{MockModel, TestAgentRunner, collect_events};
use gadk_genai::{Content, ROLE_MODEL};
use gadk_prompts::{FileStore, Label, NewVersion, PromptRegistry, PromptStore, Selector};
use serde_json::json;

fn nv(name: &str, t: &str) -> NewVersion {
    NewVersion {
        name: name.into(),
        template: t.into(),
        config: None,
        author: String::new(),
        message: String::new(),
    }
}

/// The system instruction text of each request the model received.
fn instructions(model: &MockModel) -> Vec<String> {
    model
        .requests()
        .into_iter()
        .map(|r| {
            r.config
                .and_then(|c| c.system_instruction)
                .map(|si| si.parts.iter().map(|p| p.text.as_str()).collect::<String>())
                .unwrap_or_default()
        })
        .collect()
}

async fn registry_with(
    dir: &std::path::Path,
    setup: &[(&str, &str)],
) -> (Arc<FileStore>, PromptRegistry) {
    let store = Arc::new(FileStore::new(dir));
    for (n, t) in setup {
        store.create_version(nv(n, t)).await.unwrap();
    }
    let r = PromptRegistry::builder(store.clone())
        .refresh_interval(Duration::ZERO)
        .load()
        .await
        .unwrap();
    (store, r)
}

async fn run_turns(
    r: &PromptRegistry,
    instr: gadk_prompts::PromptInstruction,
    turns: usize,
    between: impl AsyncFn(usize),
) -> (Arc<MockModel>, Vec<Result<(), String>>) {
    let model = MockModel::from_contents(
        (0..turns)
            .map(|i| Content::from_text(format!("r{i}"), ROLE_MODEL))
            .collect(),
    );
    let agent = llm_agent::new(LlmAgentConfig {
        name: "a".into(),
        model: Some(model.clone()),
        instruction_provider: Some(instr.into_provider()),
        ..Default::default()
    })
    .unwrap();
    let runner =
        TestAgentRunner::with_state(agent, json!({"who": "Ada"}).as_object().cloned().unwrap())
            .await;
    let mut results = Vec::new();
    for i in 0..turns {
        results.push(
            collect_events(runner.run("hi"))
                .await
                .map(|_| ())
                .map_err(|e| e.to_string()),
        );
        between(i).await;
    }
    let _ = r;
    (model, results)
}

#[tokio::test]
async fn e2e_label_move_applies_on_next_turn() {
    let dir = tempfile::tempdir().unwrap();
    let (store, r) =
        registry_with(dir.path(), &[("p", "v1 for {who}"), ("p", "v2 for {who}")]).await;
    store
        .set_label("p", &Label::production(), 1, None)
        .await
        .unwrap();
    r.refresh().await.unwrap();
    let instr = r.instruction("p", Selector::Label(Label::production()));
    let (model, results) = run_turns(&r, instr, 2, async |i| {
        if i == 0 {
            store
                .set_label("p", &Label::production(), 2, Some(Some(1)))
                .await
                .unwrap();
            r.refresh().await.unwrap();
        }
    })
    .await;
    assert!(results.iter().all(Result::is_ok), "{results:?}");
    // The engine adds its own identity text to the system instruction, so check containment.
    let ins = instructions(&model);
    assert_eq!(ins.len(), 2);
    assert!(
        ins[0].contains("v1 for Ada") && !ins[0].contains("v2"),
        "{ins:?}"
    );
    assert!(
        ins[1].contains("v2 for Ada") && !ins[1].contains("v1"),
        "{ins:?}"
    );
}

#[tokio::test]
async fn provider_errors_and_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let (_store, r) =
        registry_with(dir.path(), &[("p", "hello {who}"), ("bad", "needs {x}")]).await;

    // missing label, no fallback
    let (_m, res) = run_turns(
        &r,
        r.instruction("p", Selector::Label(Label::production())),
        1,
        async |_| {},
    )
    .await;
    let e = res[0].clone().unwrap_err();
    assert!(e.contains("prompts: p@production: "), "{e}");

    // missing version: v<n> in prefix
    let (_m, res) = run_turns(
        &r,
        r.instruction("p", Selector::Version(9)),
        1,
        async |_| {},
    )
    .await;
    assert!(res[0].clone().unwrap_err().contains("prompts: p@v9: "));

    // fallback rendered with state
    let fb = r
        .instruction("nope", Selector::Latest)
        .with_fallback("fallback for {who}");
    let (m, res) = run_turns(&r, fb, 1, async |_| {}).await;
    assert!(res[0].is_ok(), "{res:?}");
    assert!(
        instructions(&m)[0].contains("fallback for Ada"),
        "{:?}",
        instructions(&m)
    );

    // render error is not masked by the fallback
    let bad = r
        .instruction("bad", Selector::Latest)
        .with_fallback("unused");
    let (_m, res) = run_turns(&r, bad, 1, async |_| {}).await;
    assert!(
        res[0]
            .clone()
            .unwrap_err()
            .contains("prompts: bad@latest: "),
        "{res:?}"
    );
}

fn attr<'a>(
    s: &'a gadk_engine::testutil::telemetry::SpanStub,
    k: &str,
) -> Option<&'a serde_json::Value> {
    s.attributes.iter().find(|(n, _)| n == k).map(|(_, v)| v)
}

#[tokio::test(flavor = "current_thread")]
async fn render_span_is_child_of_invoke_agent_without_text() {
    let dir = tempfile::tempdir().unwrap();
    let (store, r) = registry_with(dir.path(), &[("p", "SECRET TEMPLATE {who}")]).await;
    store
        .set_label("p", &Label::production(), 1, None)
        .await
        .unwrap();
    r.refresh().await.unwrap();
    let hash = r.get("p", &Selector::Latest).unwrap().meta.hash.clone();

    let ((), spans) = capture_spans(async {
        run_turns(
            &r,
            r.instruction("p", Selector::Label(Label::production())),
            1,
            async |_| {},
        )
        .await;
    })
    .await;
    let render = spans
        .iter()
        .find(|s| s.name == "render_prompt p")
        .expect("render span");
    let invoke = spans
        .iter()
        .find(|s| s.name.starts_with("invoke_agent"))
        .expect("invoke span");
    assert_eq!(render.parent_span_id, Some(invoke.span_id));
    assert_eq!(attr(render, "prompt.name"), Some(&json!("p")));
    assert_eq!(attr(render, "prompt.version"), Some(&json!(1)));
    assert_eq!(attr(render, "prompt.label"), Some(&json!("production")));
    assert_eq!(attr(render, "prompt.hash"), Some(&json!(hash)));
    assert_eq!(attr(render, "prompt.fallback"), Some(&json!(false)));
    for (_, v) in &render.attributes {
        assert!(!v.to_string().contains("SECRET"), "template leaked: {v}");
    }

    // fallback span
    let ((), spans) = capture_spans(async {
        run_turns(
            &r,
            r.instruction("nope", Selector::Latest).with_fallback("x"),
            1,
            async |_| {},
        )
        .await;
    })
    .await;
    let fb = spans
        .iter()
        .find(|s| s.name == "render_prompt nope")
        .expect("fallback span");
    assert_eq!(attr(fb, "prompt.fallback"), Some(&json!(true)));
    assert_eq!(attr(fb, "prompt.version"), Some(&json!(0)));

    // not found without fallback: no span
    let ((), spans) = capture_spans(async {
        run_turns(&r, r.instruction("nope", Selector::Latest), 1, async |_| {}).await;
    })
    .await;
    assert!(
        spans.iter().all(|s| !s.name.starts_with("render_prompt")),
        "{spans:?}"
    );
}
