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
use serde_json::Map;

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
            response: Map::new(),
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
