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

//! Port of Go tool/agenttool/agent_tool_test.go.

use std::collections::BTreeMap;
use std::sync::Arc;

use gadk_agent::{
    Agent, AgentConfig, Context, ContextDelta, EventStream, InvocationContext, Tool, ToolError,
};
use gadk_genai::{
    CodeExecutionResult, Content, ExecutableCode, FunctionDeclaration, Part, ROLE_MODEL, Schema,
    Type,
};
use gadk_model::Llm;
use gadk_session::{CreateRequest, Event, InMemorySessionService, SessionService};
use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};

use crate::llm::{LlmAgentConfig, agent as llm_agent};
use crate::testutil::MockModel;
use crate::tool::{AgentTool, AgentToolConfig};

fn obj(v: Value) -> Map<String, Value> {
    let Value::Object(m) = v else {
        panic!("not an object")
    };
    m
}

fn schema_props(props: &[(&str, Type)], required: &[&str]) -> Schema {
    Schema {
        r#type: Some(Type::Object),
        properties: props
            .iter()
            .map(|(k, t)| {
                (
                    (*k).to_string(),
                    Schema {
                        r#type: Some(t.clone()),
                        ..Default::default()
                    },
                )
            })
            .collect::<BTreeMap<_, _>>(),
        required: required.iter().map(|s| (*s).to_string()).collect(),
        ..Default::default()
    }
}

fn output_schema() -> Schema {
    schema_props(
        &[("is_valid", Type::Boolean), ("message", Type::String)],
        &["is_valid", "message"],
    )
}

fn create_agent_with_model(
    input: Option<Schema>,
    output: Option<Schema>,
    model: Arc<dyn Llm>,
) -> Arc<dyn Agent> {
    llm_agent::new(LlmAgentConfig {
        name: "math_agent".into(),
        model: Some(model),
        description: "Solves math problems.".into(),
        instruction: "You solve math problems.".into(),
        input_schema: input,
        output_schema: output,
        ..Default::default()
    })
    .unwrap()
}

// parity: adapted: Go builds a Gemini client with a fake key that is never called; a MockModel
// with no replies stands in.
fn create_agent(input: Option<Schema>, output: Option<Schema>) -> Arc<dyn Agent> {
    create_agent_with_model(input, output, MockModel::from_contents(vec![]))
}

/// Go createToolContext: a tool context over a fresh in-memory session "testApp"/"testUser"/"testSession".
async fn create_tool_context() -> Context {
    let svc = InMemorySessionService::new();
    let sess = svc
        .create(CreateRequest {
            app_name: "testApp".into(),
            user_id: "testUser".into(),
            session_id: Some("testSession".into()),
            state: None,
        })
        .await
        .unwrap();
    // Go agent.NewToolContext(ctx, "", &session.EventActions{}, nil): the function_call_id delta
    // gives the context its own actions, so update_actions from the run is not dropped.
    Context::builder()
        .session(sess)
        .build()
        .with_delta(ContextDelta {
            function_call_id: Some(String::new()),
            ..Default::default()
        })
}

async fn run_tool(
    tool: &Arc<dyn Tool>,
    ctx: &Context,
    args: Value,
) -> Result<Map<String, Value>, ToolError> {
    tool.as_runnable()
        .expect("AgentTool is runnable")
        .run(ctx.clone(), args)
        .await
}

fn request_decl() -> FunctionDeclaration {
    FunctionDeclaration {
        name: "math_agent".into(),
        description: "Solves math problems.".into(),
        parameters: Some(schema_props(&[("request", Type::String)], &["request"])),
        ..Default::default()
    }
}

fn model_content(parts: Vec<Part>) -> Content {
    Content {
        role: ROLE_MODEL.into(),
        parts,
    }
}

fn thought(text: &str) -> Part {
    Part {
        text: text.into(),
        thought: true,
        ..Default::default()
    }
}

#[test]
fn test_agent_tool_declaration() {
    let agent = create_agent(
        Some(schema_props(&[("request", Type::String)], &["request"])),
        None,
    );
    let tool = AgentTool::new(agent, None);
    assert!(
        tool.as_runnable().is_some(),
        "agentTool does not implement FunctionTool"
    );
    assert_eq!(tool.declaration(), Some(request_decl()));
}

#[test]
fn test_agent_tool_declaration_without_schema() {
    let tool = AgentTool::new(create_agent(None, None), None);
    assert!(
        tool.as_runnable().is_some(),
        "agentTool does not implement FunctionTool"
    );
    assert_eq!(tool.declaration(), Some(request_decl()));
}

#[tokio::test]
async fn test_agent_tool_run_input_validation() {
    let input = schema_props(
        &[("is_magic", Type::Boolean), ("name", Type::String)],
        &["is_magic", "name"],
    );
    let tool = AgentTool::new(create_agent(Some(input), None), None);
    let ctx = create_tool_context().await;
    let cases = [
        (
            "extra_field",
            json!({"is_magic": true, "name_invalid": "test_name", "name": "test"}),
        ),
        (
            "invalid_type",
            json!({"is_magic": "invalid_type", "name": "test_name"}),
        ),
        ("missing_required", json!({"is_magic": true})),
    ];
    for (name, args) in cases {
        let res = run_tool(&tool, &ctx, args.clone()).await;
        assert!(
            res.is_err(),
            "{name}: Run({args}) succeeded unexpectedly, wanted error"
        );
    }
}

#[tokio::test]
async fn test_agent_tool_run_output_validation() {
    let model = MockModel::from_contents(vec![Content::from_text(
        r#"{"is_valid": "invalid type", "message": "success"}"#,
        ROLE_MODEL,
    )]);
    let tool = AgentTool::new(
        create_agent_with_model(None, Some(output_schema()), model),
        None,
    );
    let ctx = create_tool_context().await;
    let res = run_tool(&tool, &ctx, json!({"request": "test"})).await;
    assert!(res.is_err(), "Run() succeeded unexpectedly, want error");
}

#[tokio::test]
async fn test_agent_tool_run_successful() {
    let input = schema_props(&[("is_magic", Type::Boolean)], &["is_magic"]);
    let model = MockModel::from_contents(vec![Content::from_text(
        r#"{"is_valid": true, "message": "success"}"#,
        ROLE_MODEL,
    )]);
    let tool = AgentTool::new(
        create_agent_with_model(Some(input), Some(output_schema()), model),
        None,
    );
    let ctx = create_tool_context().await;
    let got = run_tool(&tool, &ctx, json!({"is_magic": true}))
        .await
        .expect("Run() failed unexpectedly");
    assert_eq!(got, obj(json!({"is_valid": true, "message": "success"})));
}

#[tokio::test]
async fn test_agent_tool_run_without_schema() {
    let model = MockModel::from_contents_streaming(
        vec![model_content(vec![
            Part::from_text("First text part is returned"),
            Part::from_text(" This should not be ignored"),
        ])],
        1,
    );
    let tool = AgentTool::new(create_agent_with_model(None, None, model), None);
    let ctx = create_tool_context().await;
    let got = run_tool(&tool, &ctx, json!({"request": "magic"}))
        .await
        .expect("Run() failed unexpectedly");
    assert_eq!(
        got,
        obj(json!({"result": "First text part is returned This should not be ignored"}))
    );
}

fn cer(output: &str) -> Option<CodeExecutionResult> {
    Some(CodeExecutionResult {
        output: output.into(),
        ..Default::default()
    })
}

fn code(code: &str) -> Option<ExecutableCode> {
    Some(ExecutableCode {
        code: code.into(),
        ..Default::default()
    })
}

/// A custom agent "calculator" yielding one model event with `parts`.
fn calculator(parts: Vec<Part>) -> Arc<dyn Agent> {
    gadk_agent::new_agent(AgentConfig {
        name: "calculator".into(),
        run: Some(Arc::new(move |ctx: InvocationContext| -> EventStream {
            let parts = parts.clone();
            Box::pin(async_stream::stream! {
                let mut event = Event::new(ctx.platform(), ctx.invocation_id());
                event.author = "calculator".into();
                event.response.content = Some(model_content(parts));
                yield Ok(event);
            })
        })),
        ..Default::default()
    })
    .expect("agent.New failed")
}

#[tokio::test]
async fn test_agent_tool_run_code_parts() {
    let cases: Vec<(&str, Vec<Part>, Value)> = vec![
        (
            "code execution output",
            vec![Part {
                code_execution_result: cer("\n 42 \n\n"),
                ..Default::default()
            }],
            json!({"result": "\n 42 "}),
        ),
        (
            "executable code",
            vec![Part {
                executable_code: code("print(42)\n"),
                ..Default::default()
            }],
            json!({"result": "print(42)\n"}),
        ),
        (
            "mixed content",
            vec![
                Part::from_text("Calculating."),
                Part {
                    executable_code: code("print(42)"),
                    ..Default::default()
                },
                Part {
                    code_execution_result: cer("42\n"),
                    ..Default::default()
                },
                Part::from_text("Done."),
            ],
            json!({"result": "Calculating.\nprint(42)\n42\nDone."}),
        ),
        (
            "empty and thought parts",
            vec![
                // parity: adapted: Go's nil part has no Rust equivalent; a default part stands in.
                Part::default(),
                Part::default(),
                Part {
                    code_execution_result: cer(""),
                    ..Default::default()
                },
                Part {
                    code_execution_result: cer("\n\n"),
                    ..Default::default()
                },
                Part {
                    executable_code: code(""),
                    ..Default::default()
                },
                Part {
                    thought: true,
                    code_execution_result: cer("hidden"),
                    ..Default::default()
                },
                Part {
                    thought: true,
                    executable_code: code("hidden"),
                    ..Default::default()
                },
            ],
            json!({}),
        ),
        (
            "text and code precedence",
            vec![
                Part {
                    text: "text".into(),
                    code_execution_result: cer("unused"),
                    executable_code: code("unused"),
                    ..Default::default()
                },
                Part {
                    code_execution_result: cer("output"),
                    executable_code: code("unused"),
                    ..Default::default()
                },
                Part {
                    code_execution_result: cer(""),
                    executable_code: code("code"),
                    ..Default::default()
                },
            ],
            json!({"result": "text\noutput\ncode"}),
        ),
    ];
    for (name, parts, want) in cases {
        let wrapped = AgentTool::new(calculator(parts), None);
        let ctx = create_tool_context().await;
        let got = run_tool(&wrapped, &ctx, json!({"request": "calculate"}))
            .await
            .unwrap_or_else(|e| panic!("{name}: Run failed: {e}"));
        assert_eq!(
            got,
            obj(want),
            "{name}: Run returned an unexpected tool result"
        );
    }
}

#[tokio::test]
async fn test_agent_tool_run_filters_thought_parts() {
    let model = MockModel::from_contents_streaming(
        vec![model_content(vec![
            thought("Let me think about this..."),
            thought(""), // edge case: empty text + thought
            Part::from_text("The answer is 42"),
            thought("Still reasoning..."),
        ])],
        1,
    );
    let tool = AgentTool::new(create_agent_with_model(None, None, model), None);
    let ctx = create_tool_context().await;
    let got = run_tool(&tool, &ctx, json!({"request": "what is the answer?"}))
        .await
        .expect("Run() failed unexpectedly");
    assert_eq!(got, obj(json!({"result": "The answer is 42"})));
}

#[tokio::test]
async fn test_agent_tool_run_all_thought_parts_returns_empty() {
    let model = MockModel::from_contents_streaming(
        vec![model_content(vec![thought("Just thinking...")])],
        1,
    );
    let tool = AgentTool::new(create_agent_with_model(None, None, model), None);
    let ctx = create_tool_context().await;
    let got = run_tool(&tool, &ctx, json!({"request": "think only"}))
        .await
        .expect("Run() failed unexpectedly");
    assert_eq!(got, Map::new());
}

#[tokio::test]
async fn test_agent_tool_run_filters_thought_parts_with_output_schema() {
    let model = MockModel::from_contents_streaming(
        vec![model_content(vec![
            thought("Let me validate the input carefully..."),
            Part::from_text(r#"{"is_valid": true, "message": "success"}"#),
        ])],
        1,
    );
    let tool = AgentTool::new(
        create_agent_with_model(None, Some(output_schema()), model),
        None,
    );
    let ctx = create_tool_context().await;
    let got = run_tool(&tool, &ctx, json!({"request": "validate"}))
        .await
        .expect("Run() failed unexpectedly");
    assert_eq!(got, obj(json!({"is_valid": true, "message": "success"})));
}

#[tokio::test]
async fn test_agent_tool_run_empty_model_response() {
    let model = MockModel::from_contents(vec![model_content(vec![])]);
    let tool = AgentTool::new(create_agent_with_model(None, None, model), None);
    let ctx = create_tool_context().await;
    let got = run_tool(&tool, &ctx, json!({"request": "magic"}))
        .await
        .expect("Run() failed unexpectedly");
    assert_eq!(got, Map::new());
}

#[tokio::test]
async fn test_agent_tool_run_skip_summarization() {
    for skip in [true, false] {
        let model = MockModel::from_contents(vec![Content::from_text("test response", ROLE_MODEL)]);
        let ag = create_agent_with_model(None, None, model);
        let ctx = create_tool_context().await;
        let tool = AgentTool::new(
            ag,
            Some(AgentToolConfig {
                skip_summarization: skip,
            }),
        );
        run_tool(&tool, &ctx, json!({"request": "magic"}))
            .await
            .expect("Run() failed unexpectedly");
        assert_eq!(
            ctx.actions().skip_summarization,
            skip,
            "SkipSummarization propagated to parent tool context"
        );
    }
}
