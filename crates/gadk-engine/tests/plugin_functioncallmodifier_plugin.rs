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

//! Port of Go plugin/functioncallmodifier/plugin_test.go.

use std::any::Any;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use gadk_agent::{AgentConfig, Context, Tool, ToolError, new_agent};
use gadk_engine::plugin::{FunctionCallModifierConfig, new_function_call_modifier_plugin};
use gadk_engine::tool::{AgentTool, FunctionTool, FunctionToolConfig};
use gadk_genai::{
    Content, FunctionCall, FunctionDeclaration, GenerateContentConfig, Part, Schema,
    Tool as GenaiTool, Type,
};
use gadk_model::{LlmRequest, LlmResponse};
use gadk_session::{CreateRequest, InMemorySessionService, Session, SessionError, SessionService};
use serde_json::{Value, json};

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

/// Go's plugin config, shared by both tests: `predicate` accepts exactly `names`.
fn modifier_config(names: &'static [&'static str]) -> FunctionCallModifierConfig {
    FunctionCallModifierConfig {
        predicate: Arc::new(move |tool_name: &str| names.contains(&tool_name)),
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
    }
}

/// Go `&llminternal.TransferToAgentTool{}` (no supported agents). The engine's transfer tool is
/// crate-private, so this stub has Go's name, description and declaration
/// (internal/llminternal/agent_transfer.go:123-154).
struct TransferToAgentTool;

impl Tool for TransferToAgentTool {
    fn name(&self) -> &str {
        "transfer_to_agent"
    }

    fn description(&self) -> &str {
        "Transfer the question to another agent.\nThis tool hands off control to another agent when it's more suitable to answer the user's question according to the agent's description."
    }

    fn declaration(&self) -> Option<FunctionDeclaration> {
        Some(FunctionDeclaration {
            name: self.name().into(),
            description: self.description().into(),
            parameters: Some(Schema {
                r#type: Some(Type::Object),
                properties: BTreeMap::from([(
                    "agent_name".to_string(),
                    Schema {
                        r#type: Some(Type::String),
                        description: "the agent name to transfer to".into(),
                        ..Default::default()
                    },
                )]),
                required: vec!["agent_name".into()],
                ..Default::default()
            }),
            ..Default::default()
        })
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Go `createAgentTool(t, name, desc, nil)`: an agent tool over an agent with no input schema.
fn create_agent_tool(name: &str, desc: &str) -> Arc<dyn Tool> {
    let agent = new_agent(AgentConfig {
        name: name.into(),
        description: desc.into(),
        ..Default::default()
    })
    .unwrap();
    AgentTool::new(agent, None)
}

/// A request with one declaration in `config.tools` and `tool` packed under `name`.
fn request(decl: FunctionDeclaration, name: &str, tool: &Arc<dyn Tool>) -> LlmRequest {
    LlmRequest {
        config: Some(GenerateContentConfig {
            tools: vec![GenaiTool {
                function_declarations: vec![decl],
                ..Default::default()
            }],
            ..Default::default()
        }),
        tools: HashMap::from([(
            name.to_string(),
            Arc::new(tool.clone()) as Arc<dyn Any + Send + Sync>,
        )]),
        ..Default::default()
    }
}

/// Go `findDeclaration`.
fn find_declaration<'a>(req: &'a LlmRequest, tool_name: &str) -> Option<&'a FunctionDeclaration> {
    req.config
        .as_ref()?
        .tools
        .iter()
        .flat_map(|t| t.function_declarations.iter())
        .find(|d| d.name == tool_name)
}

struct BeforeCase {
    name: &'static str,
    req: LlmRequest,
    want_params: &'static [&'static str],
    check_tools: &'static [&'static str],
}

// parity: adapted: TransferToAgentTool is crate-private, so a stub with Go's declaration is used; Go's cloneLLMRequest is unnecessary because each case builds its own request
#[tokio::test]
async fn test_before_model_callback() {
    let ctx = Context::builder().build();

    let transfer_tool: Arc<dyn Tool> = Arc::new(TransferToAgentTool);
    let transfer_tool_decl = transfer_tool.declaration().unwrap();

    let agent_tool_default = create_agent_tool("agent_default", "desc");
    let agent_tool_default_decl = agent_tool_default.declaration().unwrap();
    let function_tool = FunctionTool::new(FunctionToolConfig::new("other_tool", ""), ok_func)
        .unwrap_or_else(|e| panic!("functiontool.New failed: {e}"));

    let cases = vec![
        BeforeCase {
            name: "no relevant tools",
            req: request(
                FunctionDeclaration {
                    name: "other_tool".into(),
                    ..Default::default()
                },
                "other_tool",
                &function_tool,
            ),
            want_params: &[],
            check_tools: &[],
        },
        BeforeCase {
            name: "agent tool default schema",
            req: request(
                agent_tool_default_decl,
                "agent_default",
                &agent_tool_default,
            ),
            want_params: &["request", "skill_id", "rationale"],
            check_tools: &["agent_default"],
        },
        BeforeCase {
            name: "transfer to agent tool",
            req: request(transfer_tool_decl, "transfer_to_agent", &transfer_tool),
            want_params: &["agent_name", "skill_id", "rationale"],
            check_tools: &["transfer_to_agent"],
        },
    ];

    for tc in cases {
        let plugin = new_function_call_modifier_plugin(modifier_config(&[
            "transfer_to_agent",
            "agent_default",
        ]));

        let before_model_callback = plugin.before_model_callback().unwrap();
        let (current_req, res) = before_model_callback(ctx.clone(), tc.req).await;
        if let Err(e) = res {
            panic!("{}: BeforeModelCallback failed: {e}", tc.name);
        }

        for tool_name in tc.check_tools {
            let Some(decl) = find_declaration(&current_req, tool_name) else {
                panic!(
                    "{}: Tool {tool_name}: Declaration not found in request",
                    tc.name
                );
            };
            let props = &decl
                .parameters
                .as_ref()
                .unwrap_or_else(|| panic!("{}: Tool {tool_name}: no parameters", tc.name))
                .properties;
            assert_eq!(
                props.len(),
                tc.want_params.len(),
                "{}: Tool {tool_name}: Expected {} parameters, got {}",
                tc.name,
                tc.want_params.len(),
                props.len()
            );
            let params: BTreeSet<&str> = props.keys().map(String::as_str).collect();
            let want: BTreeSet<&str> = tc.want_params.iter().copied().collect();
            assert_eq!(
                want, params,
                "{}: Tool {tool_name}: Parameter mismatch",
                tc.name
            );
            assert!(
                decl.description.contains("skill-based orchestration"),
                "{}: Tool {tool_name}: Description not updated",
                tc.name
            );
        }
    }
}

/// A content with one function-call part.
fn call(id: &str, name: &str, args: Value) -> Content {
    Content {
        parts: vec![Part {
            function_call: Some(FunctionCall {
                id: id.into(),
                name: name.into(),
                args: args.as_object().cloned().unwrap(),
                ..Default::default()
            }),
            ..Default::default()
        }],
        ..Default::default()
    }
}

/// Go's `originalDecls` field is unused by the Go test body and is not ported.
struct AfterCase {
    name: &'static str,
    content: Content,
    want_args: Option<Value>,
    want_skill_state_key: &'static str,
    want_skill_state_value: &'static str,
    want_rationale_state_key: &'static str,
    want_rationale_state_value: &'static str,
    should_have_skill_state: bool,
    should_have_rationale_state: bool,
}

/// Go's per-key state check: a string value when expected; otherwise (for a non-empty key)
/// `ErrStateKeyNotExist`.
fn check_state(ses: &Arc<dyn Session>, case: &str, key: &str, should_have: bool, want: &str) {
    if should_have {
        match ses.state().get(key) {
            Ok(Value::String(got)) => {
                assert_eq!(want, got, "{case}: State value mismatch for key {key}")
            }
            Ok(other) => panic!("{case}: State value for key {key} is not string: {other:?}"),
            Err(e) => panic!("{case}: State().Get({key:?}) unexpected error: {e}"),
        }
    } else if !key.is_empty() {
        match ses.state().get(key) {
            Ok(v) => panic!("{case}: Unexpected state key {key} found: {v:?}"),
            Err(SessionError::StateKeyNotExist) => {}
            Err(e) => panic!(
                "{case}: State().Get({key:?}) unexpected error when expecting key not to exist: {e}"
            ),
        }
    }
}

// parity: adapted: Go checks the args edited in place in tc.content; Rust checks the response the callback hands back (R1)
#[tokio::test]
async fn test_after_model_callback() {
    let cases = vec![
        AfterCase {
            name: "no function calls",
            content: Content {
                parts: vec![Part::from_text("hello")],
                ..Default::default()
            },
            want_args: None,
            want_skill_state_key: "",
            want_skill_state_value: "",
            want_rationale_state_key: "",
            want_rationale_state_value: "",
            should_have_skill_state: false,
            should_have_rationale_state: false,
        },
        AfterCase {
            name: "unmodified function call",
            content: call("", "other_tool", json!({"foo": "bar"})),
            want_args: Some(json!({"foo": "bar"})),
            want_skill_state_key: "",
            want_skill_state_value: "",
            want_rationale_state_key: "",
            want_rationale_state_value: "",
            should_have_skill_state: false,
            should_have_rationale_state: false,
        },
        AfterCase {
            name: "agent tool with skill and rationale",
            content: call(
                "fcId",
                "test_agent",
                json!({"request": "do", "skill_id": "s1", "rationale": "r1"}),
            ),
            want_args: Some(json!({"request": "do"})),
            want_skill_state_key: "fcId/skill_id",
            want_skill_state_value: "s1",
            want_rationale_state_key: "fcId/rationale",
            want_rationale_state_value: "r1",
            should_have_skill_state: true,
            should_have_rationale_state: true,
        },
        AfterCase {
            name: "agent tool with only skill_id",
            content: call(
                "fcId",
                "test_agent",
                json!({"request": "do", "skill_id": "s1"}),
            ),
            want_args: Some(json!({"request": "do"})),
            want_skill_state_key: "fcId/skill_id",
            want_skill_state_value: "s1",
            want_rationale_state_key: "",
            want_rationale_state_value: "",
            should_have_skill_state: true,
            should_have_rationale_state: false,
        },
        AfterCase {
            name: "agent tool without skill/rationale",
            content: call("fcId", "test_agent", json!({"request": "do"})),
            want_args: Some(json!({"request": "do"})),
            want_skill_state_key: "",
            want_skill_state_value: "",
            want_rationale_state_key: "",
            want_rationale_state_value: "",
            should_have_skill_state: false,
            should_have_rationale_state: false,
        },
        AfterCase {
            name: "transfer tool with skill and rationale",
            content: call(
                "fcId",
                "transfer_to_agent",
                json!({"agent_name": "a1", "skill_id": "s2", "rationale": "r2"}),
            ),
            want_args: Some(json!({"agent_name": "a1"})),
            want_skill_state_key: "fcId/skill_id",
            want_skill_state_value: "s2",
            want_rationale_state_key: "fcId/rationale",
            want_rationale_state_value: "r2",
            should_have_skill_state: true,
            should_have_rationale_state: true,
        },
    ];

    for tc in cases {
        let plugin = new_function_call_modifier_plugin(modifier_config(&[
            "transfer_to_agent",
            "test_agent",
        ]));
        let service = InMemorySessionService::new();
        let ses = service
            .create(CreateRequest {
                app_name: "test".into(),
                user_id: "user".into(),
                session_id: Some("ses1".into()),
                state: None,
            })
            .await
            .unwrap_or_else(|e| panic!("{}: Failed to create session: {e}", tc.name));
        let ctx = Context::builder().session(ses.clone()).build();

        let after_model_callback = plugin.after_model_callback().unwrap();
        let (resp, res) = after_model_callback(
            ctx,
            LlmResponse {
                content: Some(tc.content.clone()),
                ..Default::default()
            },
            None,
        )
        .await;
        if let Err(e) = res {
            panic!("{}: AfterModelCallback failed: {e}", tc.name);
        }

        // Check if args are pruned
        if let (Some(want_args), Some(part)) = (
            &tc.want_args,
            resp.content.as_ref().and_then(|c| c.parts.first()),
        ) && let Some(fc) = &part.function_call
        {
            assert_eq!(
                want_args,
                &Value::Object(fc.args.clone()),
                "{}: Args mismatch",
                tc.name
            );
        }

        // Check session state
        check_state(
            &ses,
            tc.name,
            tc.want_skill_state_key,
            tc.should_have_skill_state,
            tc.want_skill_state_value,
        );
        check_state(
            &ses,
            tc.name,
            tc.want_rationale_state_key,
            tc.should_have_rationale_state,
            tc.want_rationale_state_value,
        );
    }
}
