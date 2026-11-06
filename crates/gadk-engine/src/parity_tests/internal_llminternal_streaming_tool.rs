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

//! Parity port of Go internal/llminternal/streaming_tool_test.go.

use std::{collections::HashMap, sync::Arc};

use gadk_agent::{AgentConfig, Context, InvocationContext, Tool, agent};
use gadk_genai::{Content, FunctionCall, Part};
use gadk_model::LlmResponse;
use gadk_session::Event;
use pretty_assertions::assert_eq;
use serde::Deserialize;
use serde_json::json;

use crate::{
    llm::{flow::Flow, functions::handle_function_calls},
    tool::{FunctionToolConfig, StreamingFunctionTool},
};

#[derive(Deserialize, schemars::JsonSchema)]
struct Args {
    count: i64,
}

// parity: adapted: only the "Non-Live Mode (Aggregation)" subtest is ported; the "Live Mode (Streaming)" subtest needs a live session (deferred to 1b-live)
#[tokio::test]
async fn test_handle_function_calls_streaming() {
    let stream_tool: Arc<dyn Tool> = StreamingFunctionTool::new(
        FunctionToolConfig::new("test_stream", "streams chunks"),
        |_ctx: Context, a: Args| {
            futures::stream::iter((0..a.count).map(|i| Ok(format!("chunk {i}"))))
        },
    )
    .unwrap();
    let tools = HashMap::from([("test_stream".to_owned(), stream_tool.clone())]);
    let call_ev = Event {
        response: LlmResponse {
            content: Some(Content {
                role: "model".into(),
                parts: vec![Part {
                    function_call: Some(FunctionCall {
                        id: "call_1".into(),
                        name: "test_stream".into(),
                        args: json!({"count": 3}).as_object().unwrap().clone(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    let ctx = InvocationContext::builder()
        .invocation_id("inv_1")
        .agent(
            agent::new(AgentConfig {
                name: "agent_1".into(),
                ..Default::default()
            })
            .unwrap(),
        )
        .build_invocation();
    let flow = Flow {
        tools: parking_lot::RwLock::new(vec![stream_tool]),
        ..Default::default()
    };
    let merged = handle_function_calls(&ctx, &flow, &tools, &call_ev, None)
        .await
        .unwrap()
        .expect("non-None merged event");
    let parts = &merged.response.content.as_ref().unwrap().parts;
    assert_eq!(parts.len(), 1);
    let fr = parts[0]
        .function_response
        .as_ref()
        .expect("FunctionResponse part");
    assert_eq!(
        fr.response.get("result"),
        Some(&json!("chunk 0chunk 1chunk 2"))
    );
}

// parity: skipped TestHandleFunctionCalls_LiveControlPlane: stop_streaming / cancelling streaming tools needs a live session (deferred to 1b-live)
