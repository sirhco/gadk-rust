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

//! Port of Go internal/llminternal/skip_summarization_test.go.

use std::sync::Arc;

use futures::StreamExt;
use gadk_agent::{Agent, Context, RunConfig, StreamingMode, ToolError};
use gadk_engine::llm::{LlmAgentConfig, agent as llm_agent};
use gadk_engine::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use gadk_engine::testutil::MockModel;
use gadk_engine::tool::{AgentTool, AgentToolConfig, FunctionTool, FunctionToolConfig};
use gadk_genai::{Content, FunctionCall};
use gadk_model::LlmResponse;
use gadk_platform::Platform;
use gadk_session::{CreateRequest, Event, InMemorySessionService, SessionService};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// Go `singleCallMockModel`: a function call with id `fc_id` first, then "done".
fn single_call_model(tool_name: &str, fc_id: &str, args: Value) -> Arc<MockModel> {
    let args = match args {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    let call = Content::from_function_call(
        FunctionCall {
            id: fc_id.into(),
            name: tool_name.into(),
            args,
            ..Default::default()
        },
        "model",
    );
    MockModel::new(vec![
        Ok(LlmResponse {
            content: Some(call),
            ..Default::default()
        }),
        Ok(LlmResponse {
            content: Some(Content::from_text("done", "model")),
            ..Default::default()
        }),
    ])
}

/// Port of Go runSkipSummarizationScenario.
async fn run_skip_summarization_scenario(root: Arc<dyn Agent>) -> Vec<Event> {
    let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    svc.create(CreateRequest {
        app_name: "testApp".into(),
        user_id: "testUser".into(),
        session_id: Some("testSession".into()),
        state: None,
    })
    .await
    .unwrap();
    let runner = Runner::new(RunnerConfig {
        app_name: "testApp".into(),
        agent: root,
        session_service: svc,
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig::default(),
        auto_create_session: false,
        platform: Platform::default(),
        compaction: None,
    })
    .unwrap();
    let mut stream = runner.run(
        "testUser",
        "testSession",
        Some(Content::from_text("go", "user")),
        RunConfig {
            streaming_mode: StreamingMode::Sse,
            ..Default::default()
        },
        RunOptions::default(),
    );
    let mut events = Vec::new();
    while let Some(item) = stream.next().await {
        events.push(item.unwrap());
    }
    events
}

#[tokio::test]
async fn test_handle_function_calls_skip_summarization_agent_tool_displays_result() {
    let sub_model = MockModel::from_contents(vec![Content::from_text("sub-agent answer", "model")]);
    let sub_agent = llm_agent::new(LlmAgentConfig {
        name: "sub_agent".into(),
        description: "answers questions".into(),
        instruction: "Answer the question.".into(),
        model: Some(sub_model),
        ..Default::default()
    })
    .unwrap();
    let agent_tool = AgentTool::new(
        sub_agent,
        Some(AgentToolConfig {
            skip_summarization: true,
        }),
    );
    let root_model = single_call_model(
        agent_tool.name(),
        "call_1",
        json!({"request": "what is the answer?"}),
    );
    let root = llm_agent::new(LlmAgentConfig {
        name: "root".into(),
        description: "root agent".into(),
        instruction: "Delegate to the sub-agent.".into(),
        model: Some(root_model.clone()),
        tools: vec![agent_tool],
        ..Default::default()
    })
    .unwrap();

    let events = run_skip_summarization_scenario(root).await;

    assert_eq!(
        events.len(),
        2,
        "expected 2 events (function call, function response)"
    );
    assert_eq!(
        root_model.requests().len(),
        1,
        "SkipSummarization should end the run after the first function response"
    );
    let resp = &events[1];
    assert!(
        resp.is_final_response(),
        "expected function response event to be the final response"
    );
    let parts = &resp.response.content.as_ref().unwrap().parts;
    assert!(
        parts.iter().any(|p| p.function_response.is_some()),
        "expected final event to retain the FunctionResponse part"
    );
    let text: Vec<&str> = parts
        .iter()
        .filter(|p| !p.text.is_empty())
        .map(|p| p.text.as_str())
        .collect();
    assert_eq!(
        text.last().copied(),
        Some("sub-agent answer"),
        "expected a visible text part with the sub-agent's answer"
    );
}

#[derive(Deserialize, Serialize, schemars::JsonSchema)]
struct Empty {}

#[tokio::test]
async fn test_handle_function_calls_skip_summarization_plain_tool_result_not_displayed() {
    const TOOL_NAME: &str = "widget_ack";
    let widget = FunctionTool::new(
        FunctionToolConfig::new(TOOL_NAME, "acknowledges a widget interaction"),
        |ctx: Context, _in: Empty| async move {
            ctx.update_actions(|a| a.skip_summarization = true);
            let mut out = std::collections::BTreeMap::new();
            out.insert("status".to_string(), "ok".to_string());
            Ok::<_, ToolError>(out)
        },
    )
    .unwrap();
    let root_model = single_call_model(TOOL_NAME, "call_1", Value::Null);
    let root = llm_agent::new(LlmAgentConfig {
        name: "root".into(),
        description: "root agent".into(),
        instruction: "Use the widget tool.".into(),
        model: Some(root_model.clone()),
        tools: vec![widget],
        ..Default::default()
    })
    .unwrap();

    let events = run_skip_summarization_scenario(root).await;

    assert_eq!(
        events.len(),
        2,
        "expected 2 events (function call, function response)"
    );
    assert_eq!(
        root_model.requests().len(),
        1,
        "SkipSummarization should end the run after the first function response"
    );
    let resp = &events[1];
    assert!(resp.is_final_response());
    let parts = &resp.response.content.as_ref().unwrap().parts;
    assert!(
        parts.len() == 1 && parts[0].function_response.is_some(),
        "expected only the FunctionResponse part, got {parts:?}"
    );
}
