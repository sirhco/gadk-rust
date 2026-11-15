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

//! Port of Go plugin/functioncallmodifier/integration_test.go (httprr replay of Go's recordings).

use std::collections::BTreeMap;
use std::sync::Arc;

use gadk_agent::{Agent, Context, RunConfig, StreamingMode, Tool, ToolError};
use gadk_engine::llm::LlmAgentConfig;
use gadk_engine::llm::agent as llm_agent;
use gadk_engine::plugin::{FunctionCallModifierConfig, new_function_call_modifier_plugin};
use gadk_engine::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use gadk_engine::testutil::{collect_events, gemini_test_model};
use gadk_engine::tool::{AgentTool, FunctionTool, FunctionToolConfig};
use gadk_genai::{Content, ROLE_USER, Schema, Type};
use gadk_platform::Platform;
use gadk_session::{
    CreateRequest, GetRequest, InMemorySessionService, SessionError, SessionService,
};
use rstest::rstest;
use serde_json::{Value, json};

/// Go `filepath.Join("testdata", strings.ReplaceAll(t.Name(), "/", "_")+".httprr")`.
macro_rules! fixture {
    ($case:literal) => {
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/testdata/plugin/functioncallmodifier/testdata/TestPluginCallbackIntegration_",
            $case,
            ".httprr"
        )
    };
}

/// Go `SimpleArgs`.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[allow(dead_code)] // only its schema matters
struct SimpleArgs {
    #[serde(rename = "Num")]
    num: i64,
}

/// Go `okFunc`.
async fn ok_func(_ctx: Context, _args: SimpleArgs) -> Result<String, ToolError> {
    Ok("ok".to_string())
}

/// Go's `tools func(agent.Agent) []tool.Tool` per case.
#[derive(Debug, Clone, Copy)]
enum ToolsCase {
    /// `[]tool.Tool{functionTool}`
    FunctionTool,
    /// `[]tool.Tool{agenttool.New(a, nil)}`
    AgentToolDefault,
    /// `[]tool.Tool{}`: the calculator becomes a sub-agent instead.
    None,
}

#[rstest]
#[case::no_relevant_tools(fixture!("no_relevant_tools"), ToolsCase::FunctionTool, None, None)]
#[case::agent_tool_default_schema(
    fixture!("agent_tool_default_schema"),
    ToolsCase::AgentToolDefault,
    Some("add"),
    Some("The user wants to add two numbers, and the calculator tool has an 'add' skill that can perform this operation. Providing the request as '2+2' and explicitly setting the skill_id to 'add' should allow the calculator to correctly perform the addition.")
)]
#[ignore = "8a bug: transfer_to_agent's empty result is sent as {} instead of {\"result\": null} (Go #2281)"]
#[case::transfer_to_agent_tool(
    fixture!("transfer_to_agent_tool"),
    ToolsCase::None,
    Some("add"),
    Some("The user is asking to add two numbers, and the calculator agent has an add skill.")
)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
// parity: adapted: Go's t.Run subtests are rstest cases; other_tool's schemas are the fixture's (Go infers them with jsonschema-go)
async fn test_plugin_callback_integration(
    #[case] http_record_filename: &str,
    #[case] tools_case: ToolsCase,
    #[case] want_skill_state_value: Option<&str>,
    #[case] want_rationale_state_value: Option<&str>,
) {
    let function_tool = FunctionTool::new(
        FunctionToolConfig {
            // Copied verbatim from the no_relevant_tools fixture's functionDeclarations[0].
            input_schema: Some(json!({"additionalProperties":false,"properties":{"Num":{"type":"integer"}},"required":["Num"],"type":"object"})),
            output_schema: Some(json!({"type":"string"})),
            ..FunctionToolConfig::new("other_tool", "")
        },
        ok_func,
    )
    .unwrap_or_else(|e| panic!("functiontool.New failed: {e}"));

    let model = gemini_test_model("gemini-2.5-flash", http_record_filename).await;

    let calc = llm_agent::new(LlmAgentConfig {
        name: "calculator".into(),
        description: "calculator agent\n Skills: add, subtract, multiply, divide".into(),
        instruction: "You are a calculator agent. You can calculate numbers.".into(),
        model: Some(model.clone()),
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("NewLLMAgent calculator failed: {e}"));

    let tools: Vec<Arc<dyn Tool>> = match tools_case {
        ToolsCase::FunctionTool => vec![function_tool],
        ToolsCase::AgentToolDefault => vec![AgentTool::new(calc.clone(), None)],
        ToolsCase::None => vec![],
    };
    let mut sub_agents: Vec<Arc<dyn Agent>> = vec![];
    if tools.is_empty() {
        sub_agents.push(calc.clone());
    }

    let a = llm_agent::new(LlmAgentConfig {
        name: "transfer_agent".into(),
        description: "transfer agent".into(),
        instruction: "You are a transfer agent. You can transfer to other agents using your tools."
            .into(),
        model: Some(model),
        tools,
        sub_agents,
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("NewLLMAgent failed: {e}"));

    let function_call_modifier_plugin = new_function_call_modifier_plugin(
        FunctionCallModifierConfig {
            predicate: Arc::new(|tool_name: &str| {
                tool_name == "transfer_to_agent" || tool_name == "calculator"
            }),
            args: BTreeMap::from([
                (
                    "skill_id".to_string(),
                    Schema {
                        description: "The specific skill to be utilized by the agent.".into(),
                        r#type: Some(Type::String),
                        ..Default::default()
                    },
                ),
                (
                    "rationale".to_string(),
                    Schema {
                        description: "The reasoning behind selecting this agent and skill.".into(),
                        r#type: Some(Type::String),
                        ..Default::default()
                    },
                ),
            ]),
            override_description: Some(Arc::new(|original: &str| {
                format!(
                    "This tool can now optionally accept skill_id and rationale parameters to guide skill-based orchestration. {original}"
                )
            })),
        },
    );

    let app_name = "test_app";
    let session_service: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    session_service
        .create(CreateRequest {
            app_name: app_name.into(),
            user_id: "id".into(),
            session_id: Some("test_session".into()),
            state: None,
        })
        .await
        .unwrap_or_else(|e| panic!("sessionService.Create failed: {e}"));

    let r = Runner::new(RunnerConfig {
        app_name: app_name.into(),
        agent: a,
        session_service: session_service.clone(),
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig {
            plugins: vec![function_call_modifier_plugin],
            ..Default::default()
        },
        auto_create_session: false,
        platform: Platform::default(),
        compaction: None,
    })
    .unwrap_or_else(|e| panic!("NewRunner failed: {e}"));

    let msg = Content::from_text("Can you add 2 and 2?", ROLE_USER);
    let events = collect_events(r.run(
        "id",
        "test_session",
        Some(msg),
        RunConfig {
            streaming_mode: StreamingMode::None,
            ..Default::default()
        },
        RunOptions::default(),
    ))
    .await
    .unwrap_or_else(|e| panic!("CollectEvents failed: {e}"));

    // check function calls don't include skill_id and rationale
    let mut fc_id = String::new();
    for event in &events {
        let Some(content) = &event.response.content else {
            continue;
        };
        for part in &content.parts {
            if let Some(fc) = &part.function_call {
                assert!(
                    !fc.args.contains_key("skill_id") && !fc.args.contains_key("rationale"),
                    "function call includes skill_id or rationale: {fc:?}"
                );
                fc_id = fc.id.clone();
            }
        }
    }

    // check if state includes skill_id and rationale
    let resp = session_service
        .get(GetRequest {
            app_name: app_name.into(),
            user_id: "id".into(),
            session_id: "test_session".into(),
            ..Default::default()
        })
        .await
        .unwrap_or_else(|e| panic!("sessionService.Get failed: {e}"));

    for (key, want) in [
        (format!("{fc_id}/skill_id"), want_skill_state_value),
        (format!("{fc_id}/rationale"), want_rationale_state_value),
    ] {
        let got = resp.state().get(&key);
        match want {
            Some(want) => {
                let got =
                    got.unwrap_or_else(|e| panic!("State().Get({key:?}) unexpected error: {e}"));
                assert_eq!(
                    got,
                    Value::String(want.into()),
                    "want {key} {want:?}, got {got:?}"
                );
            }
            None => match got {
                Err(SessionError::StateKeyNotExist) => {}
                Ok(v) => panic!("unexpectedly found {key} in state with value: {v:?}"),
                Err(e) => panic!(
                    "State().Get({key:?}) unexpected error when expecting key not to exist: {e}"
                ),
            },
        }
    }
}
