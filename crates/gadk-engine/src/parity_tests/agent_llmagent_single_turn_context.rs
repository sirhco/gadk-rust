// Copyright 2026 Chris Olson
// Portions derived from ADK-Go (https://github.com/google/adk-go), Copyright 2026 Google LLC.
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

//! Port of Go agent/llmagent/single_turn_context_test.go.

use std::sync::Arc;

use gadk_agent::{Agent, Mode};
use gadk_genai::{Content, FunctionCall, FunctionResponse, ROLE_MODEL, ROLE_USER};
use serde_json::{Map, Value, json};

use crate::llm::LlmAgentConfig;
use crate::llm::agent as llm_agent;
use crate::testutil::{MockModel, TestAgentRunner, collect_events};

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        other => panic!("not an object: {other}"),
    }
}

fn translator_call(id: &str, request: &str) -> Content {
    Content::from_function_call(
        FunctionCall {
            id: id.into(),
            name: "translator".into(),
            args: obj(json!({ "request": request })),
            ..Default::default()
        },
        ROLE_MODEL,
    )
}

fn single_turn_translator(description: &str, model: Arc<MockModel>) -> Arc<dyn Agent> {
    llm_agent::new(LlmAgentConfig {
        name: "translator".into(),
        description: description.into(),
        model: Some(model),
        mode: Mode::SingleTurn,
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("llmagent.New(translator): {e}"))
}

fn chat_coordinator(model: Arc<MockModel>, translator: Arc<dyn Agent>) -> Arc<dyn Agent> {
    llm_agent::new(LlmAgentConfig {
        name: "coordinator".into(),
        model: Some(model),
        mode: Mode::Chat,
        sub_agents: vec![translator],
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("llmagent.New(coordinator): {e}"))
}

/// A root chat agent that calls a single_turn sub-agent gets the sub-agent's output once, in the
/// function response. The sub-agent's own events live on the tool's sub-branch and must not come
/// back in later turns.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "8a bug: the contents processor (llm/processors/contents.rs, not owned by the D2b fix task) does not skip a tool sub-branch event (Go ab78c5ae isToolSubBranchEvent)"]
async fn test_single_turn_sub_agent_transcript_stays_out_of_coordinator_context() {
    let cases = [
        ("branch keyed by call id", "fc1", "translator@fc1"),
        // RunNode rejects these as run ids, but the branch can still carry them.
        ("numeric call id", "7", "translator@7"),
        ("call id with slash", "a/b", "translator@a/b"),
    ];
    for (name, call_id, want_branch) in cases {
        let inner_model = MockModel::from_contents(vec![Content::from_text("hola", ROLE_MODEL)]);
        let translator = single_turn_translator("Translates a phrase.", inner_model);
        let call = translator_call(call_id, "hello");
        let coord_model = MockModel::from_contents(vec![
            call.clone(),
            Content::from_text("done1", ROLE_MODEL),
            Content::from_text("done2", ROLE_MODEL),
        ]);
        let coordinator = chat_coordinator(coord_model.clone(), translator);

        let r = TestAgentRunner::new(coordinator).await;
        let events = collect_events(r.run("turn1"))
            .await
            .unwrap_or_else(|e| panic!("{name}: turn1: {e}"));
        collect_events(r.run("turn2"))
            .await
            .unwrap_or_else(|e| panic!("{name}: turn2: {e}"));

        let branches: Vec<&str> = events
            .iter()
            .filter(|ev| ev.author == "translator")
            .map(|ev| ev.branch.as_str())
            .collect();
        assert_eq!(
            branches,
            vec![want_branch],
            "{name}: translator event branches mismatch"
        );

        let requests = coord_model.requests();
        assert_eq!(requests.len(), 3, "{name}: coordinator requests");
        // The ids are client side only when the model did not assign them, so the ones the
        // model sent come back unchanged.
        let resp = Content::from_function_response(
            FunctionResponse {
                id: call_id.into(),
                name: "translator".into(),
                response: obj(json!({"result": "hola"})),
                ..Default::default()
            },
            ROLE_USER,
        );
        let want = vec![
            Content::from_text("turn1", ROLE_USER),
            call,
            resp,
            Content::from_text("done1", ROLE_MODEL),
            Content::from_text("turn2", ROLE_USER),
        ];
        assert_eq!(
            requests[2].contents, want,
            "{name}: turn2 coordinator contents mismatch"
        );
    }
}

/// Call ids come from the model, so one can repeat within an invocation. Each call must still
/// run the sub-agent with its own args.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_single_turn_sub_agent_reused_call_id_runs_twice() {
    let inner = MockModel::from_contents(vec![
        Content::from_text("hola", ROLE_MODEL),
        Content::from_text("bonjour", ROLE_MODEL),
    ]);
    let translator = single_turn_translator("Translates.", inner.clone());
    let coord_model = MockModel::from_contents(vec![
        translator_call("call_0", "hello to Spanish"),
        translator_call("call_0", "hello to French"),
        Content::from_text("done", ROLE_MODEL),
    ]);
    let coordinator = chat_coordinator(coord_model, translator);

    let r = TestAgentRunner::new(coordinator).await;
    collect_events(r.run("turn1"))
        .await
        .unwrap_or_else(|e| panic!("turn1: {e}"));
    assert_eq!(
        inner.requests().len(),
        2,
        "translator model called {} times, want 2",
        inner.requests().len()
    );
}
