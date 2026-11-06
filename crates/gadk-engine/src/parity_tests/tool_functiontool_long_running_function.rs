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

//! Port of Go tool/functiontool/long_running_function_test.go.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

use gadk_agent::{Context, Tool, ToolError};
use gadk_genai::{Content, FunctionCall, FunctionResponse, Part};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::llm::{LlmAgentConfig, agent as llm_agent};
use crate::testutil::{MockModel, TestAgentRunner, collect_events, collect_parts};
use crate::tool::{FunctionTool, FunctionToolConfig};

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        other => panic!("not an object: {other}"),
    }
}
fn fc_content(name: &str, args: Value) -> Content {
    Content::from_function_call(
        FunctionCall {
            name: name.into(),
            args: obj(args),
            ..Default::default()
        },
        "model",
    )
}
fn fr_content(name: &str, response: Value) -> Content {
    Content::from_function_response(
        FunctionResponse {
            name: name.into(),
            response: obj(response),
            ..Default::default()
        },
        "user",
    )
}
/// Go NewContentFromFunctionResponseWithID.
fn fr_content_with_id(name: &str, response: Value, id: &str) -> Content {
    let mut c = fr_content(name, response);
    c.parts[0].function_response.as_mut().unwrap().id = id.to_string();
    c
}
fn strip_part_ids(mut parts: Vec<Part>) -> Vec<Part> {
    for p in &mut parts {
        if let Some(fc) = p.function_call.as_mut() {
            fc.id.clear();
        }
        if let Some(fr) = p.function_response.as_mut() {
            fr.id.clear();
        }
    }
    parts
}

#[derive(Deserialize, JsonSchema)]
struct IncArgs {}

#[tokio::test]
async fn test_new_long_running_function_tool() {
    #[derive(Deserialize, JsonSchema)]
    struct SumArgs {
        a: i64,
        b: i64,
    }
    #[derive(Serialize, JsonSchema)]
    struct SumResult {
        result: String,
    }
    let sum = FunctionTool::new(
        FunctionToolConfig {
            is_long_running: true,
            ..FunctionToolConfig::new("sum", "sums two integers")
        },
        |_ctx: Context, input: SumArgs| async move {
            let _ = (input.a, input.b);
            Ok::<_, ToolError>(SumResult {
                result: "Processing sum".into(),
            })
        },
    )
    .unwrap();
    assert_eq!(sum.name(), "sum");
    assert_eq!(sum.description(), "sums two integers");
    assert!(sum.is_long_running(), "wrong value for is_long_running");
    assert!(
        sum.as_runnable().is_some(),
        "could not convert to a runnable tool"
    );
    let decl = sum.declaration().expect("no declaration");
    assert!(
        decl.description
            .contains("NOTE: This is a long-running operation"),
        "wrong description note: {}",
        decl.description
    );
}

/// Port of Go testLongRunningFunctionFlow.
async fn long_running_function_flow(tool: Arc<dyn Tool>, result_key: &str, calls: &AtomicUsize) {
    let model = MockModel::from_contents(vec![
        fc_content("increaseByOne", json!({})),
        Content::from_text("response1", "model"),
        Content::from_text("response2", "model"),
        Content::from_text("response3", "model"),
        Content::from_text("response4", "model"),
    ]);
    let a = llm_agent::new(LlmAgentConfig {
        name: "long_running_agent".into(),
        model: Some(model.clone()),
        tools: vec![tool],
        ..Default::default()
    })
    .unwrap();
    let runner = TestAgentRunner::new(a).await;

    let parts = collect_parts(runner.run("test1"))
        .await
        .expect("failed to collect events");
    let reqs = model.requests();
    assert_eq!(reqs.len(), 2, "got {} requests, want 2", reqs.len());
    assert_eq!(calls.load(SeqCst), 1, "function called times");
    assert_eq!(reqs[0].contents, vec![Content::from_text("test1", "user")]);
    assert_eq!(
        reqs[1].contents,
        vec![
            Content::from_text("test1", "user"),
            fc_content("increaseByOne", json!({})),
            fr_content("increaseByOne", json!({ result_key: "pending" })),
        ]
    );
    let call_id = parts[0]
        .function_call
        .as_ref()
        .expect("first part is not a function call")
        .id
        .clone();
    assert_eq!(
        strip_part_ids(parts),
        vec![
            Part::from_function_call("increaseByOne", Map::new()),
            Part::from_function_response("increaseByOne", obj(json!({ result_key: "pending" }))),
            Part::from_text("response1"),
        ]
    );

    for (name, response, want_reqs, want_text) in [
        (
            "function response still waiting",
            json!({"status": "still waiting"}),
            3,
            "response2",
        ),
        (
            "function response result 2",
            json!({"result": 2}),
            4,
            "response3",
        ),
        (
            "function response result 3",
            json!({"result": 3}),
            5,
            "response4",
        ),
    ] {
        let parts = collect_parts(runner.run_content(fr_content_with_id(
            "increaseByOne",
            response.clone(),
            &call_id,
        )))
        .await
        .unwrap_or_else(|e| panic!("{name}: failed to collect events: {e}"));
        let reqs = model.requests();
        assert_eq!(reqs.len(), want_reqs, "{name}");
        let latest = &reqs.last().unwrap().contents;
        // Still 3: the function responses are merged into one by the contents processor.
        assert_eq!(latest.len(), 3, "{name}: latest request contents size");
        assert_eq!(latest[2], fr_content("increaseByOne", response), "{name}");
        assert_eq!(parts.len(), 1, "{name}: got parts {parts:?}");
        assert_eq!(parts[0].text, want_text, "{name}");
    }
    assert_eq!(
        calls.load(SeqCst),
        1,
        "function should still have been called once"
    );
}

#[tokio::test]
async fn test_long_running_function_flow() {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let tool = FunctionTool::new(
        FunctionToolConfig {
            is_long_running: true,
            ..FunctionToolConfig::new("increaseByOne", "increaseByOne")
        },
        move |_ctx: Context, _x: IncArgs| {
            c.fetch_add(1, SeqCst);
            async {
                Ok::<_, ToolError>(BTreeMap::from([(
                    "status".to_string(),
                    "pending".to_string(),
                )]))
            }
        },
    )
    .unwrap();
    long_running_function_flow(tool, "status", &calls).await;
}

#[tokio::test]
async fn test_long_running_string_function_flow() {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let tool = FunctionTool::new(
        FunctionToolConfig {
            is_long_running: true,
            ..FunctionToolConfig::new("increaseByOne", "increaseByOne")
        },
        move |_ctx: Context, _x: IncArgs| {
            c.fetch_add(1, SeqCst);
            async { Ok::<_, ToolError>("pending".to_string()) }
        },
    )
    .unwrap();
    long_running_function_flow(tool, "result", &calls).await;
}

#[tokio::test]
async fn test_long_running_tool_i_ds_are_set() {
    let model = MockModel::from_contents(vec![
        fc_content("increaseByOne", json!({})),
        Content::from_text("response1", "model"),
    ]);
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let tool = FunctionTool::new(
        FunctionToolConfig {
            is_long_running: true,
            ..FunctionToolConfig::new("increaseByOne", "increaseByOne")
        },
        move |_ctx: Context, _x: IncArgs| {
            c.fetch_add(1, SeqCst);
            async {
                Ok::<_, ToolError>(BTreeMap::from([(
                    "status".to_string(),
                    "pending".to_string(),
                )]))
            }
        },
    )
    .unwrap();
    let a = llm_agent::new(LlmAgentConfig {
        name: "hello_world_agent".into(),
        model: Some(model),
        tools: vec![tool],
        ..Default::default()
    })
    .unwrap();
    let runner = TestAgentRunner::new(a).await;

    let events = collect_events(runner.run("test1"))
        .await
        .expect("failed to collect events");
    // First event is the function call, second the function response, third the model's message.
    assert_eq!(
        events.len(),
        3,
        "got {} events, want 3: {events:?}",
        events.len()
    );

    let function_call_event = &events[0];
    let function_response_event = &events[1];
    let llm_response_event = &events[2];
    assert_eq!(
        function_call_event.long_running_tool_ids.len(),
        1,
        "Invalid LongRunningToolIDs for functionCallEvent"
    );
    // Go distinguishes nil from empty on the response event; Rust has only empty.
    assert!(
        function_response_event.long_running_tool_ids.is_empty(),
        "Invalid LongRunningToolIDs for functionResponseEvent"
    );
    assert!(
        llm_response_event.long_running_tool_ids.is_empty(),
        "Invalid LongRunningToolIDs for llmResponseEvent"
    );
    let call_id = &function_call_event.response.content.as_ref().unwrap().parts[0]
        .function_call
        .as_ref()
        .unwrap()
        .id;
    assert_eq!(
        &function_call_event.long_running_tool_ids[0], call_id,
        "Invalid LongRunningToolIDs for functionCallEvent"
    );
}
