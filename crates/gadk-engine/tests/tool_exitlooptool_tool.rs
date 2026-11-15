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
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Port of Go tool/exitlooptool/tool_test.go.

use futures::StreamExt;
use gadk_agent::AgentConfig;
use gadk_engine::agents::loop_agent::{self, LoopAgentConfig};
use gadk_engine::llm::{LlmAgentConfig, agent as llm_agent};
use gadk_engine::testutil::{MockModel, TestAgentRunner};
use gadk_engine::tool::exit_loop;
use gadk_genai::{Content, FunctionCall, FunctionResponse};
use serde_json::{Map, Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

fn text(t: &str) -> Content {
    Content::from_text(t, "model")
}
fn exit_call() -> Content {
    Content::from_function_call(
        FunctionCall {
            name: "exit_loop".into(),
            args: Map::new(),
            ..Default::default()
        },
        "model",
    )
}
fn exit_response() -> Content {
    Content::from_function_response(
        FunctionResponse {
            name: "exit_loop".into(),
            response: Map::from_iter([("result".to_owned(), Value::Null)]),
            ..Default::default()
        },
        "user",
    )
}
fn strip_ids(mut c: Content) -> Content {
    for p in &mut c.parts {
        if let Some(fc) = p.function_call.as_mut() {
            fc.id.clear();
        }
        if let Some(fr) = p.function_response.as_mut() {
            fr.id.clear();
        }
    }
    c
}
#[tokio::test]
async fn test_exit_loop_tool_exits_loop_agent() {
    let cases: Vec<(&str, Vec<Content>, u32, Vec<Content>)> = vec![
        (
            "ExitLoopToolStopsMidLoop",
            vec![
                text("response1"),
                text("response2"),
                exit_call(),
                text("this should not be processed"),
                text("this should not be processed"),
            ],
            5,
            vec![
                text("response1"),
                text("response2"),
                exit_call(),
                exit_response(),
            ],
        ),
        (
            "MaxIterationsStopsLoop",
            vec![
                text("iteration 1 response"),
                text("iteration 2 response"),
                text("iteration 3 response"),
            ],
            2,
            vec![text("iteration 1 response"), text("iteration 2 response")],
        ),
        (
            "ExitLoopToolStopsImmediately",
            vec![
                exit_call(),
                text("this should not be processed"),
                text("this should not be processed"),
            ],
            3,
            vec![exit_call(), exit_response()],
        ),
    ];
    for (name, responses, max_iterations, want) in cases {
        let a = llm_agent::new(LlmAgentConfig {
            name: "simple agent".into(),
            model: Some(MockModel::from_contents(responses)),
            tools: vec![exit_loop()],
            ..Default::default()
        })
        .unwrap();
        let looper = loop_agent::new(LoopAgentConfig {
            agent: AgentConfig {
                name: "looper".into(),
                sub_agents: vec![a],
                ..Default::default()
            },
            max_iterations,
        })
        .unwrap();
        let runner = TestAgentRunner::new(looper).await;
        let mut stream = runner.run("message");
        let mut count = 0;
        while let Some(item) = stream.next().await {
            let ev =
                item.unwrap_or_else(|e| panic!("{name}: runner returned unexpected error: {e}"));
            assert!(
                count < want.len(),
                "{name}: stream generated more values than the expected {}: {:?}",
                want.len(),
                ev.response.content
            );
            assert_eq!(
                strip_ids(ev.response.content.clone().unwrap()),
                want[count],
                "{name}: LoopAgent Run() mismatch at {count}"
            );
            count += 1;
        }
        assert_eq!(count, want.len(), "{name}: unexpected stream length");
    }
}

// exit_loop returns no result of its own, so after-tool callbacks see nil. The
// model is still sent {"result": null}.
// parity: adapted: Rust tool results are a Map with no nil, so Go's nil result is an empty map
#[tokio::test]
async fn test_exit_loop_tool_after_tool_callback_sees_no_result() {
    let calls = Arc::new(AtomicUsize::new(0));
    let got_result = Arc::new(parking_lot::Mutex::new(None::<Map<String, Value>>));
    let (c, g) = (calls.clone(), got_result.clone());
    let a = llm_agent::new(LlmAgentConfig {
        name: "simple agent".into(),
        model: Some(MockModel::from_contents(vec![exit_call()])),
        tools: vec![exit_loop()],
        after_tool_callbacks: vec![Arc::new(move |_ctx, _tool, _args, result, _err| {
            c.fetch_add(1, Ordering::SeqCst);
            *g.lock() = Some(result);
            Box::pin(async { Ok(None) })
        })],
        ..Default::default()
    })
    .unwrap();
    let runner = TestAgentRunner::new(a).await;
    let mut stream = runner.run("message");
    while let Some(item) = stream.next().await {
        item.unwrap_or_else(|e| panic!("runner returned unexpected error: {e}"));
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "after-tool callback ran {} times, want 1",
        calls.load(Ordering::SeqCst)
    );
    let got = got_result.lock().clone().unwrap();
    assert!(
        got.is_empty(),
        "after-tool callback result = {got:?}, want nil"
    );
}

// A map an after-tool callback returns in place of exit_loop's nil result is
// sent to the model as it is, not wrapped as {"result": ...}.
#[tokio::test]
async fn test_exit_loop_tool_after_tool_callback_replaces_result() {
    let a = llm_agent::new(LlmAgentConfig {
        name: "simple agent".into(),
        model: Some(MockModel::from_contents(vec![exit_call()])),
        tools: vec![exit_loop()],
        after_tool_callbacks: vec![Arc::new(|_ctx, _tool, _args, _result, _err| {
            Box::pin(async {
                Ok(Some(
                    json!({"exited": true})
                        .as_object()
                        .cloned()
                        .unwrap_or_default(),
                ))
            })
        })],
        ..Default::default()
    })
    .unwrap();
    let runner = TestAgentRunner::new(a).await;
    let mut stream = runner.run("message");
    let mut responses = Vec::new();
    while let Some(item) = stream.next().await {
        let ev = item.unwrap_or_else(|e| panic!("runner returned unexpected error: {e}"));
        for p in ev.response.content.iter().flat_map(|c| c.parts.iter()) {
            if let Some(fr) = &p.function_response {
                responses.push(Value::Object(fr.response.clone()));
            }
        }
    }
    assert_eq!(
        responses,
        vec![json!({"exited": true})],
        "function responses mismatch"
    );
}
