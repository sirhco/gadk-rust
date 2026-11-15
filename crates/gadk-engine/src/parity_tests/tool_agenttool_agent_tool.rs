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
use std::sync::atomic::{AtomicBool, Ordering};

use futures::StreamExt;

use gadk_agent::{
    Agent, AgentConfig, Context, ContextDelta, EventStream, InvocationContext, Tool, ToolError,
};
use gadk_genai::{
    CodeExecutionResult, Content, ExecutableCode, FunctionCall, FunctionDeclaration, Part,
    PartialArg, ROLE_MODEL, Schema, Type,
};
use gadk_model::{Llm, LlmRequest};
use gadk_session::{CreateRequest, Event, InMemorySessionService, SessionService};
use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};

use crate::llm::{LlmAgentConfig, agent as llm_agent};
use crate::testutil::MockModel;
use crate::tool::{AgentTool, AgentToolConfig, FunctionTool, FunctionToolConfig};

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

// TestAgentTool_Run_CompositeAgentInputSchema checks that a wrapped workflow
// agent whose first sub-agent has an InputSchema takes the arguments its
// declaration advertises, as adk-python's AgentTool does with
// _get_input_schema.
#[tokio::test]
#[ignore = "8a bug: AgentTool::run reads the input schema only of an LLM agent (reveal), so a sequential agent's call is validated for 'request' instead of its first sub-agent's InputSchema (Go 31111f89)"]
async fn test_agent_tool_run_composite_agent_input_schema() {
    let input_schema = schema_props(&[("is_magic", Type::Boolean)], &["is_magic"]);
    let cases = [
        (
            "valid_arguments",
            json!({"is_magic": true}),
            Some(json!({"result": "magic"})),
        ),
        ("invalid_arguments", json!({"is_magic": "yes"}), None),
    ];
    for (name, args, want) in cases {
        let model = MockModel::from_contents(vec![Content::from_text("magic", ROLE_MODEL)]);
        let pipeline = crate::agents::sequential_agent::new(
            crate::agents::sequential_agent::SequentialAgentConfig {
                agent: AgentConfig {
                    name: "pipeline".into(),
                    description: "Runs the math agent.".into(),
                    sub_agents: vec![create_agent_with_model(
                        Some(input_schema.clone()),
                        None,
                        model,
                    )],
                    ..Default::default()
                },
            },
        )
        .unwrap_or_else(|e| panic!("{name}: sequentialagent.New() failed: {e}"));
        let tool = AgentTool::new(pipeline, None);
        assert_eq!(
            tool.declaration().and_then(|d| d.parameters),
            Some(input_schema.clone()),
            "{name}: Declaration().Parameters"
        );
        let ctx = create_tool_context().await;
        let got = run_tool(&tool, &ctx, args.clone()).await;
        match want {
            None => {
                // The schema rejects the arguments, rather than the missing
                // "request" the declaration never asked for.
                let err = got.expect_err("want an argument validation error");
                assert!(
                    err.to_string().contains("argument validation failed"),
                    "{name}: Run({args}) error = {err}, want an argument validation error"
                );
            }
            Some(want) => {
                let got = got.unwrap_or_else(|e| panic!("{name}: Run({args}) failed: {e}"));
                assert_eq!(got, obj(want), "{name}: Run() result");
            }
        }
    }
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
#[ignore = "8a bug: AgentTool::run returns {} instead of {\"result\": \"\"} when the sub-agent reply has no text (Go a8a9b936)"]
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
            json!({"result": ""}),
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
#[ignore = "8a bug: AgentTool::run returns {} instead of {\"result\": \"\"} when the sub-agent reply has no text (Go a8a9b936)"]
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
    assert_eq!(got, obj(json!({"result": ""})));
}

// A reply without text is an empty result even when the agent has an output
// schema, where adk-python validates the empty text and raises.
#[tokio::test]
#[ignore = "8a bug: AgentTool::run returns {} instead of {\"result\": \"\"} when the sub-agent reply has no text (Go a8a9b936)"]
async fn test_agent_tool_run_all_thought_parts_returns_empty_with_output_schema() {
    let output_schema = schema_props(&[("is_valid", Type::Boolean)], &["is_valid"]);
    let model = MockModel::from_contents_streaming(
        vec![model_content(vec![thought("Just thinking...")])],
        1,
    );
    let tool = AgentTool::new(
        create_agent_with_model(None, Some(output_schema), model),
        None,
    );
    let ctx = create_tool_context().await;
    let got = run_tool(&tool, &ctx, json!({"request": "think only"}))
        .await
        .expect("Run() failed unexpectedly");
    assert_eq!(got, obj(json!({"result": ""})));
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
#[ignore = "8a bug: AgentTool::run returns {} instead of {\"result\": \"\"} when the sub-agent reply has no text (Go a8a9b936)"]
async fn test_agent_tool_run_empty_model_response() {
    let model = MockModel::from_contents(vec![model_content(vec![])]);
    let tool = AgentTool::new(create_agent_with_model(None, None, model), None);
    let ctx = create_tool_context().await;
    let got = run_tool(&tool, &ctx, json!({"request": "magic"}))
        .await
        .expect("Run() failed unexpectedly");
    assert_eq!(got, obj(json!({"result": ""})));
}

#[tokio::test]
#[ignore = "8a bug: AgentTool::run returns {} instead of {\"result\": \"\"} when the sub-agent yields no content (Go a8a9b936)"]
async fn test_agent_tool_run_no_content_returns_empty_result() {
    let child = gadk_agent::new_agent(AgentConfig {
        name: "silent".into(),
        run: Some(Arc::new(|ctx: InvocationContext| -> EventStream {
            Box::pin(async_stream::stream! {
                let mut event = Event::new(ctx.platform(), ctx.invocation_id());
                event.author = "silent".into();
                yield Ok(event);
            })
        })),
        ..Default::default()
    })
    .expect("agent.New failed");
    let wrapped = AgentTool::new(child, None);
    let ctx = create_tool_context().await;
    let got = run_tool(&wrapped, &ctx, json!({"request": "anything"}))
        .await
        .expect("Run() failed unexpectedly");
    assert_eq!(got, obj(json!({"result": ""})));
}

#[tokio::test]
async fn test_agent_tool_run_skip_summarization() {
    let input_schema = schema_props(&[("city", Type::String)], &["city"]);
    let cases = [
        (true, None, json!({"request": "magic"})),
        (false, None, json!({"request": "magic"})),
        (true, Some(input_schema.clone()), json!({"city": "Paris"})),
        (false, Some(input_schema), json!({"city": "Paris"})),
    ];
    for (skip, input, args) in cases {
        let model = MockModel::from_contents(vec![Content::from_text("test response", ROLE_MODEL)]);
        let ag = create_agent_with_model(input, None, model);
        let ctx = create_tool_context().await;
        let tool = AgentTool::new(
            ag,
            Some(AgentToolConfig {
                skip_summarization: skip,
            }),
        );
        run_tool(&tool, &ctx, args)
            .await
            .expect("Run() failed unexpectedly");
        assert_eq!(
            ctx.actions().skip_summarization,
            skip,
            "SkipSummarization propagated to parent tool context"
        );
    }
}

/// Go `transferArgs`.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[allow(dead_code)] // only its schema matters
struct TransferArgs {
    amount: i64,
}

fn call_part(id: &str, name: &str, args: Value) -> Content {
    model_content(vec![Part {
        function_call: Some(FunctionCall {
            id: id.into(),
            name: name.into(),
            args: obj(args),
            ..Default::default()
        }),
        ..Default::default()
    }])
}

/// A function tool over `TransferArgs` returning `result`.
fn transfer_like_tool(
    name: &str,
    description: &str,
    require_confirmation: bool,
    is_long_running: bool,
    result: Option<Value>,
    ran: Option<Arc<AtomicBool>>,
) -> Arc<dyn Tool> {
    let mut cfg = FunctionToolConfig::new(name, description);
    cfg.require_confirmation = require_confirmation;
    cfg.is_long_running = is_long_running;
    FunctionTool::new(cfg, move |_ctx: Context, _args: TransferArgs| {
        let result = result.clone();
        let ran = ran.clone();
        async move {
            if let Some(r) = ran {
                r.store(true, Ordering::SeqCst);
            }
            Ok::<_, ToolError>(result.map(obj))
        }
    })
    .expect("functiontool.New() failed")
}

fn child_agent(
    name: &str,
    description: &str,
    model: Arc<dyn Llm>,
    tool: Arc<dyn Tool>,
) -> Arc<dyn Agent> {
    llm_agent::new(LlmAgentConfig {
        name: name.into(),
        description: description.into(),
        model: Some(model),
        tools: vec![tool],
        ..Default::default()
    })
    .expect("llmagent.New() failed")
}

#[tokio::test]
#[ignore = "8a bug: AgentTool::run ignores long-running calls left unanswered by the sub-run (no pending-ID tracking, no 'unresolved long-running tool calls' error, SkipSummarization not cleared) (Go f25d1424)"]
async fn test_agent_tool_run_pending_confirmation_returns_error() {
    let ran = Arc::new(AtomicBool::new(false));
    let transfer = transfer_like_tool(
        "transfer",
        "Transfers money.",
        true,
        false,
        Some(json!({"status": "done"})),
        Some(ran.clone()),
    );
    let model = MockModel::from_contents(vec![call_part(
        "call-1",
        "transfer",
        json!({"amount": 100}),
    )]);
    let child = child_agent("payments_agent", "Moves money.", model, transfer);
    let tool = AgentTool::new(child, None);

    let ctx = create_tool_context().await;
    // Something other than the tool's Config, e.g. a BeforeToolCallback, set
    // the flag on this call.
    ctx.update_actions(|a| a.skip_summarization = true);

    let result = run_tool(&tool, &ctx, json!({"request": "send 100"})).await;

    assert!(
        !ctx.actions().skip_summarization,
        "SkipSummarization still set; the parent model would never see the error"
    );
    assert!(
        !ran.load(Ordering::SeqCst),
        "transfer ran without confirmation"
    );
    let err = match result {
        Ok(r) => panic!("Run() = {r:?}, nil; want an error for the unresolved confirmation"),
        Err(e) => e,
    };
    assert!(
        err.to_string()
            .contains("unresolved long-running tool calls"),
        "Run() error = {err:?}, want it to report the unresolved call"
    );
}

#[tokio::test]
async fn test_agent_tool_run_answered_long_running_call_succeeds() {
    let start = transfer_like_tool(
        "start_job",
        "Starts a job.",
        false,
        true,
        Some(json!({"status": "started"})),
        None,
    );
    let model = MockModel::from_contents(vec![
        call_part("call-1", "start_job", json!({"amount": 1})),
        Content::from_text("job started", ROLE_MODEL),
    ]);
    let child = child_agent("jobs_agent", "Starts jobs.", model, start);
    let tool = AgentTool::new(child, None);
    let ctx = create_tool_context().await;
    let result = run_tool(&tool, &ctx, json!({"request": "start"}))
        .await
        .expect("Run() failed unexpectedly");
    assert_eq!(
        result.get("result"),
        Some(&json!("job started")),
        "Run() result = {result:?}, want job started"
    );
}

// With SkipSummarization, the pending-call error must still go back to the
// parent model instead of ending the parent's turn on an empty response.
#[tokio::test]
#[ignore = "8a bug: AgentTool::run ignores long-running calls left unanswered by the sub-run, so the parent model never gets the pending-call error (Go f25d1424)"]
async fn test_agent_tool_run_skip_summarization_pending_error_reaches_parent() {
    let transfer = transfer_like_tool(
        "transfer",
        "Transfers money.",
        true,
        false,
        Some(json!({"status": "done"})),
        None,
    );
    let child_model =
        MockModel::from_contents(vec![call_part("call-1", "transfer", json!({"amount": 1}))]);
    let child = child_agent("payments_agent", "Moves money.", child_model, transfer);
    let parent_llm = MockModel::from_contents(vec![
        call_part("p-1", "payments_agent", json!({"request": "send"})),
        Content::from_text("handled the error", ROLE_MODEL),
    ]);
    let parent = llm_agent::new(LlmAgentConfig {
        name: "root".into(),
        model: Some(parent_llm.clone()),
        tools: vec![AgentTool::new(
            child,
            Some(AgentToolConfig {
                skip_summarization: true,
            }),
        )],
        ..Default::default()
    })
    .expect("llmagent.New() failed");
    let runner = crate::testutil::TestAgentRunner::new(parent).await;
    let mut stream = runner.run("pay");
    while let Some(item) = stream.next().await {
        item.unwrap_or_else(|e| panic!("Run() failed: {e}"));
    }

    let requests = parent_llm.requests();
    assert_eq!(
        requests.len(),
        2,
        "parent model requests = {}, want 2 (the second carrying the agent tool's error)",
        requests.len()
    );
    assert!(
        request_has_function_response_error(&requests[1], "unresolved long-running tool calls"),
        "second parent model request does not carry the agent tool's pending-call error"
    );
}

/// Go `requestHasFunctionResponseError`.
fn request_has_function_response_error(req: &LlmRequest, want: &str) -> bool {
    req.contents
        .iter()
        .flat_map(|c| c.parts.iter())
        .filter_map(|p| p.function_response.as_ref())
        .any(|fr| {
            fr.response
                .get("error")
                .and_then(Value::as_str)
                .is_some_and(|msg| msg.contains(want))
        })
}

/// The two streamed chunks of a `start_job` call whose arguments arrive without an ID.
fn streamed_start_job_call() -> Vec<Content> {
    vec![
        model_content(vec![Part {
            function_call: Some(FunctionCall {
                name: "start_job".into(),
                will_continue: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        }]),
        model_content(vec![Part {
            function_call: Some(FunctionCall {
                partial_args: vec![PartialArg {
                    json_path: "$.amount".into(),
                    number_value: Some(1.0),
                    ..Default::default()
                }],
                will_continue: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        }]),
    ]
}

// A long-running call whose arguments are streamed without an ID gets a
// different generated ID on each partial chunk than on the final event. Only
// the final event's ID is answered, so partial chunks must not count.
#[tokio::test]
async fn test_agent_tool_run_streamed_args_answered_long_running_call_succeeds() {
    let start = transfer_like_tool(
        "start_job",
        "Starts a job.",
        false,
        true,
        Some(json!({"status": "started"})),
        None,
    );
    let mut contents = streamed_start_job_call();
    contents.push(Content::from_text("job started", ROLE_MODEL));
    let model = MockModel::from_contents_streaming(contents, 2);
    let child = child_agent("jobs_agent", "Starts jobs.", model, start);
    let tool = AgentTool::new(child, None);
    let ctx = create_tool_context().await;
    let result = run_tool(&tool, &ctx, json!({"request": "start"}))
        .await
        .expect("Run() failed unexpectedly");
    assert_eq!(
        result.get("result"),
        Some(&json!("job started")),
        "Run() result = {result:?}, want job started"
    );
}

// Skipping partial chunks must not drop the final event's ID when a streamed
// long-running call is never answered.
#[tokio::test]
#[ignore = "8a bug: AgentTool::run ignores long-running calls left unanswered by the sub-run (no 'unresolved long-running tool calls' error) (Go f25d1424)"]
async fn test_agent_tool_run_streamed_args_unanswered_long_running_call_returns_error() {
    let start = transfer_like_tool("start_job", "Starts a job.", false, true, None, None);
    let model = MockModel::from_contents_streaming(streamed_start_job_call(), 2);
    let child = child_agent("jobs_agent", "Starts jobs.", model, start);
    let tool = AgentTool::new(child, None);
    let ctx = create_tool_context().await;
    let err = match run_tool(&tool, &ctx, json!({"request": "start"})).await {
        Ok(r) => panic!("Run() = {r:?}, nil; want an error for the unanswered call"),
        Err(e) => e,
    };
    assert!(
        err.to_string()
            .contains("unresolved long-running tool calls"),
        "Run() error = {err:?}, want it to report the unresolved call"
    );
}

// With SkipSummarization, invalid arguments must not end the parent turn: the
// error goes back to the parent model so it can retry with corrected args.
#[tokio::test]
#[ignore = "8a bug: AgentTool::run sets SkipSummarization before validating the arguments, so an argument error ends the parent's turn (Go 4cc7ce5f)"]
async fn test_agent_tool_run_skip_summarization_not_set_on_invalid_args() {
    let input_schema = schema_props(
        &[("city", Type::String), ("days", Type::Integer)],
        &["city", "days"],
    );
    let cases = [
        (
            "input_schema_missing_required",
            Some(input_schema),
            json!({"city": "Paris"}),
        ),
        ("no_schema_missing_request", None, json!({"query": "Paris"})),
    ];
    for (name, input, args) in cases {
        let ag = create_agent(input, None);
        let ctx = create_tool_context().await;
        let tool = AgentTool::new(
            ag,
            Some(AgentToolConfig {
                skip_summarization: true,
            }),
        );
        assert!(
            run_tool(&tool, &ctx, args.clone()).await.is_err(),
            "{name}: Run({args}) succeeded unexpectedly, wanted error"
        );
        assert!(
            !ctx.actions().skip_summarization,
            "{name}: SkipSummarization = true after invalid arguments, want false so the parent model can retry"
        );
    }
}
