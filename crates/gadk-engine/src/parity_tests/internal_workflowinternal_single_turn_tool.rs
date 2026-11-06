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

//! Port of Go internal/workflowinternal/single_turn_tool_test.go.

use std::sync::{Arc, Mutex};

use futures::StreamExt;
use gadk_agent::{
    Agent, AgentConfig, AgentError, Context, ContextDelta, InvocationContext, Tool, pack_tool,
};
use gadk_genai::{FunctionDeclaration, Schema};
use gadk_model::LlmRequest;
use gadk_session::{CreateRequest, Event, InMemorySessionService, SessionService};
use serde_json::{Map, Value, json};

use crate::llm::LlmAgentConfig;
use crate::llm::agent as llm_agent;
use crate::tool::single_turn::SingleTurnTool;
use crate::tool::utils::function_decls;
use crate::workflow::{Emit, NodeConfig, START, Workflow, WorkflowOptions, chain, dynamic_node};

const AGENT_NAME: &str = "math_agent";
const AGENT_DESC: &str = "Solves math problems.";

fn schema(v: Value) -> Schema {
    serde_json::from_value(v).unwrap()
}

/// Go sampleInputSchema.
fn sample_input_schema() -> Schema {
    schema(json!({
        "type": "OBJECT",
        "properties": {"is_magic": {"type": "BOOLEAN"}, "name": {"type": "STRING"}},
        "required": ["is_magic", "name"]
    }))
}

/// Go defaultDecl.
fn default_decl() -> FunctionDeclaration {
    FunctionDeclaration {
        name: AGENT_NAME.into(),
        description: AGENT_DESC.into(),
        parameters: Some(schema(json!({
            "type": "OBJECT",
            "properties": {"request": {"type": "STRING"}},
            "required": ["request"]
        }))),
        ..Default::default()
    }
}

/// Port of Go newLLMAgent (undeclared mode, no model).
fn new_llm_agent(name: &str, description: &str, input_schema: Option<Schema>) -> Arc<dyn Agent> {
    llm_agent::new(LlmAgentConfig {
        name: name.into(),
        description: description.into(),
        input_schema,
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("llmagent.New({name:?}): {e}"))
}

/// Port of Go newCompositeAgent.
fn new_composite_agent(
    name: &str,
    description: &str,
    sub_agents: Vec<Arc<dyn Agent>>,
) -> Arc<dyn Agent> {
    gadk_agent::new_agent(AgentConfig {
        name: name.into(),
        description: description.into(),
        sub_agents,
        run: Some(Arc::new(|_ctx: InvocationContext| {
            Box::pin(futures::stream::once(async {
                Err::<Event, AgentError>(AgentError::other(
                    "custom agent Run should not be invoked in unit tests",
                ))
            }))
        })),
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("agent.New({name:?}): {e}"))
}

/// Go's echo_agent: a custom agent yielding one event with `Output = "world"`.
fn echo_agent(want_output: &'static str) -> Arc<dyn Agent> {
    gadk_agent::new_agent(AgentConfig {
        name: "echo_agent".into(),
        description: "Yields a single fixed-output event.".into(),
        run: Some(Arc::new(move |_ctx: InvocationContext| {
            Box::pin(futures::stream::once(async move {
                Ok::<Event, AgentError>(Event {
                    output: Some(json!(want_output)),
                    ..Default::default()
                })
            }))
        })),
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("agent.New: {e}"))
}

/// Go `session.InMemoryService().Create(test_app/test_user/test_session)` + `fakeInvocationContext{sess}`.
async fn fake_invocation_context() -> InvocationContext {
    let svc = InMemorySessionService::new();
    let sess = svc
        .create(CreateRequest {
            app_name: "test_app".into(),
            user_id: "test_user".into(),
            session_id: Some("test_session".into()),
            state: None,
        })
        .await
        .unwrap_or_else(|e| panic!("session.Create: {e}"));
    InvocationContext::builder()
        .session(sess)
        .invocation_id("test-invocation-id")
        .build_invocation()
}

/// Go `for ev, err := range w.Run(ic)`: every event, failing on the first error (unlike
/// testutil::collect_events, events without content are fine).
async fn drain(mut s: gadk_agent::EventStream) -> Result<Vec<Event>, AgentError> {
    let mut out = Vec::new();
    while let Some(item) = s.next().await {
        out.push(item?);
    }
    Ok(out)
}

type RunResult = Arc<Mutex<Option<Result<Map<String, Value>, String>>>>;

/// Go's orchestrator dynamic node: builds a tool context over the node context (with
/// `function_call_id = "fc-id"`) and runs the single-turn tool with `{"request": "hello"}`.
/// `rewrap` re-wraps the node context once more first (Go NewCleanToolContextTestOnly).
fn orchestrator(
    st: Arc<SingleTurnTool>,
    got: RunResult,
    rewrap: bool,
) -> Arc<dyn crate::workflow::Node> {
    dynamic_node(
        "orchestrator",
        move |ctx: Context, _in: Value, _emit: Emit| {
            let (st, got) = (st.clone(), got.clone());
            async move {
                let ctx = if rewrap {
                    ctx.with_delta(ContextDelta::default())
                } else {
                    ctx
                };
                let tool_ctx = ctx.with_delta(ContextDelta {
                    function_call_id: Some("fc-id".into()),
                    ..Default::default()
                });
                let res = st
                    .as_runnable()
                    .expect("runnable")
                    .run(tool_ctx, json!({"request": "hello"}))
                    .await;
                let failed = res.as_ref().err().map(ToString::to_string);
                *got.lock().unwrap() = Some(res.map_err(|e| e.to_string()));
                match failed {
                    Some(e) => Err(AgentError::other(e)),
                    None => Ok(Value::Null),
                }
            }
        },
        NodeConfig::default(),
    )
}

#[test]
fn test_single_turn_tool_metadata() {
    let st = SingleTurnTool::new(new_llm_agent(AGENT_NAME, AGENT_DESC, None));
    assert_eq!(st.name(), AGENT_NAME, "Name()");
    assert_eq!(st.description(), AGENT_DESC, "Description()");
    assert!(!st.is_long_running(), "IsLongRunning() = true, want false");
}

#[test]
fn test_single_turn_tool_declaration() {
    let cases: Vec<(&str, Arc<dyn Agent>, FunctionDeclaration)> = vec![
        (
            "no input schema falls back to {request: STRING}",
            new_llm_agent(AGENT_NAME, AGENT_DESC, None),
            default_decl(),
        ),
        (
            "uses wrapped agent's input schema as is",
            new_llm_agent(AGENT_NAME, AGENT_DESC, Some(sample_input_schema())),
            FunctionDeclaration {
                name: AGENT_NAME.into(),
                description: AGENT_DESC.into(),
                parameters: Some(sample_input_schema()),
                ..Default::default()
            },
        ),
        (
            "composite agent recurses into first sub-agent's schema",
            new_composite_agent(
                "parent_no_schema",
                "Outer composite.",
                vec![new_llm_agent(
                    "child_with_schema",
                    "Inner agent.",
                    Some(sample_input_schema()),
                )],
            ),
            FunctionDeclaration {
                name: "parent_no_schema".into(),
                description: "Outer composite.".into(),
                parameters: Some(sample_input_schema()),
                ..Default::default()
            },
        ),
    ];
    for (name, agent, want) in cases {
        let got = SingleTurnTool::new(agent).declaration();
        assert_eq!(got, Some(want), "{name}: Declaration()");
    }
}

#[tokio::test]
async fn test_single_turn_tool_run_failures() {
    let cases: Vec<(&str, Option<Schema>, Value, Vec<&str>)> = vec![
        (
            "args is not a map",
            None,
            json!("not a map"),
            vec!["map[string]any"],
        ),
        (
            "schema: extra field rejected",
            Some(sample_input_schema()),
            json!({"is_magic": true, "name": "x", "name_invalid": "y"}),
            vec!["argument validation failed", AGENT_NAME, "name_invalid"],
        ),
        (
            "schema: missing required field",
            Some(sample_input_schema()),
            json!({"is_magic": true}),
            vec!["argument validation failed", AGENT_NAME, "name"],
        ),
        (
            "schema: wrong type for boolean field",
            Some(sample_input_schema()),
            json!({"is_magic": "not a bool", "name": "x"}),
            vec!["argument validation failed", AGENT_NAME, "is_magic"],
        ),
        (
            "schema: wrong type for string field",
            Some(sample_input_schema()),
            json!({"is_magic": true, "name": 42.0}),
            vec!["argument validation failed", AGENT_NAME, "name"],
        ),
        (
            "default schema: empty args missing required \"request\"",
            None,
            json!({}),
            vec!["argument validation failed", "request"],
        ),
    ];
    for (name, input_schema, args, want_substr) in cases {
        let st = SingleTurnTool::new(new_llm_agent(AGENT_NAME, AGENT_DESC, input_schema));
        let got = st
            .as_runnable()
            .expect("runnable")
            .run(Context::builder().build(), args.clone())
            .await;
        let err = match got {
            Ok(res) => panic!("{name}: Run({args}) err = nil, want non-nil; result = {res:?}"),
            Err(e) => e.to_string(),
        };
        for sub in want_substr {
            assert!(
                err.contains(sub),
                "{name}: Run({args}) err = {err:?}, want substring {sub:?}"
            );
        }
    }
}

// parity: adapted: Go ProcessRequest packs the tool itself; the Rust flow calls pack_tool before process_request (spec § Tools), so the port does both in the flow's order
#[test]
fn test_single_turn_tool_process_request_registers_tool() {
    let st = SingleTurnTool::new(new_llm_agent(AGENT_NAME, AGENT_DESC, None));
    let mut req = LlmRequest::default();
    let t: Arc<dyn Tool> = st.clone();
    pack_tool(&mut req, &t).expect("pack_tool");
    st.process_request(&Context::builder().build(), &mut req)
        .unwrap_or_else(|e| panic!("ProcessRequest() err = {e}"));

    assert!(
        req.tools.contains_key(AGENT_NAME),
        "req.Tools missing {AGENT_NAME:?}; got keys {:?}",
        req.tools.keys().collect::<Vec<_>>()
    );
    let decls = function_decls(req.config.as_ref());
    assert!(
        decls.iter().any(|d| d.name == AGENT_NAME),
        "req.Config function declarations missing {AGENT_NAME:?}; got {:?}",
        decls.iter().map(|d| d.name.as_str()).collect::<Vec<_>>()
    );
}

// parity: adapted: Go agent.NewToolContext(ctx, "fc-id", ..) → ctx.with_delta(function_call_id); Go fakeInvocationContext → InvocationContext::builder() over the session; the orchestrator's unused string input is a Value (START carries no input)
#[tokio::test]
async fn test_single_turn_tool_run_happy_path() {
    let st = SingleTurnTool::new(echo_agent("world"));
    let got: RunResult = Arc::default();
    let w = Workflow::new(
        "root",
        chain(&[START.clone(), orchestrator(st, got.clone(), false)]),
        WorkflowOptions::default(),
    )
    .unwrap_or_else(|e| panic!("workflow.New: {e}"));

    let ic = fake_invocation_context().await;
    drain(w.run(ic))
        .await
        .unwrap_or_else(|e| panic!("workflow.Run error: {e}"));

    let res = got.lock().unwrap().clone().expect("tool ran");
    let res = res.unwrap_or_else(|e| panic!("SingleTurnTool.Run err = {e}, want nil"));
    let Value::Object(want) = json!({"result": "world"}) else {
        unreachable!()
    };
    assert_eq!(res, want, "SingleTurnTool.Run result");
}

// parity: adapted: Go agent.NewCleanToolContextTestOnly re-wraps the context and re-stashes the sub-scheduler; Rust contexts are values, so the re-wrap is a second with_delta on the node context before the function_call_id delta
#[tokio::test]
async fn test_single_turn_tool_run_survives_context_rewrap() {
    let st = SingleTurnTool::new(echo_agent("world"));
    let got: RunResult = Arc::default();
    let w = Workflow::new(
        "root",
        chain(&[START.clone(), orchestrator(st, got.clone(), true)]),
        WorkflowOptions::default(),
    )
    .unwrap_or_else(|e| panic!("workflow.New: {e}"));

    let ic = fake_invocation_context().await;
    let events = drain(w.run(ic))
        .await
        .unwrap_or_else(|e| panic!("workflow.Run error: {e}"));
    let saw_sub_output = events.iter().any(|ev| ev.output == Some(json!("world")));

    let res = got.lock().unwrap().clone().expect("tool ran");
    let res = res.unwrap_or_else(|e| panic!("SingleTurnTool.Run err = {e}, want nil"));
    let Value::Object(want) = json!({"result": "world"}) else {
        unreachable!()
    };
    assert_eq!(res, want, "SingleTurnTool.Run result");
    assert!(
        saw_sub_output,
        "sub-agent output \"world\" did not reach the session events"
    );
}
