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

//! Port of Go `internal/telemetry/functionaltest/scenarios`: the data half (`testcasedata`:
//! which scenario a case drives, under which telemetry configuration) and the scaffolding half
//! (`testcaseimpl`: the agents, models and tools each scenario runs, driven through a real
//! [`Runner`] over an in-memory session service).

use std::sync::Arc;

use futures::StreamExt;
use gadk_agent::{Agent, AgentError, Context, Mode, RunConfig, StreamingMode, ToolError};
use gadk_engine::agents::workflow_agent::{self, WorkflowAgentConfig};
use gadk_engine::llm::LlmAgentConfig;
use gadk_engine::llm::agent as llm_agent;
use gadk_engine::runner::{PluginConfig, RunOptions, Runner, RunnerConfig};
use gadk_engine::testutil::MockModel;
use gadk_engine::tool::{AgentTool, FunctionTool, FunctionToolConfig};
use gadk_engine::workflow::{
    AgentNode, Emit, Node, NodeConfig, RunNodeOptions, START, chain, dynamic_node, function_node,
    run_node,
};
use gadk_genai::{Content, FunctionCall, Part};
use gadk_platform::Platform;
use gadk_session::{CreateRequest, InMemorySessionService, SessionService};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

/// Go `toolcall.Failure`: the component that fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCallFailure {
    /// Both turns run to completion.
    None,
    /// The first model call fails, which ends the run.
    InferenceError,
    /// The tool fails; the error goes back to the model as the tool result, so the run completes.
    ToolError,
}

/// Go `dynamicworkflow.Failure`: the stage that fails. Any failure ends the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowFailure {
    /// Every stage runs to completion.
    None,
    /// The static node fails, before the dynamic one starts.
    StaticNodeError,
    /// The dynamic node fails before it delegates.
    DynamicNodeError,
    /// The model call of the first delegated agent fails.
    FirstAgentError,
    /// The model call of the second delegated agent fails.
    SecondAgentError,
}

/// Go `testcasedata.Scenario`, one variant per scenario package.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scenario {
    /// Go `toolcall.Scenario`: an llmagent calling one function tool, then answering in text.
    ToolCall(ToolCallFailure),
    /// Go `streaming.Scenario`: an llmagent answering in two streamed chunks.
    Streaming,
    /// Go `delegation.Scenario`: an llmagent delegating to another through agenttool.
    Delegation,
    /// Go `dynamicworkflow.Scenario`: a workflow agent running a static node, then a dynamic node
    /// that delegates to two collaborative agents and runs a function node twice.
    DynamicWorkflow(WorkflowFailure),
}

impl Scenario {
    /// Go `Name`: the scenario's testdata directory.
    pub fn name(self) -> &'static str {
        match self {
            Self::ToolCall(_) => "toolcall",
            Self::Streaming => "streaming",
            Self::Delegation => "delegation",
            Self::DynamicWorkflow(_) => "dynamicworkflow",
        }
    }

    /// Go `Variant`: what sets this value apart from the scenario's default, or `""`.
    pub fn variant(self) -> &'static str {
        match self {
            Self::ToolCall(ToolCallFailure::InferenceError) => "inference-error",
            Self::ToolCall(ToolCallFailure::ToolError) => "tool-error",
            Self::DynamicWorkflow(WorkflowFailure::StaticNodeError) => "static-node-error",
            Self::DynamicWorkflow(WorkflowFailure::DynamicNodeError) => "dynamic-node-error",
            Self::DynamicWorkflow(WorkflowFailure::FirstAgentError) => "first-agent-error",
            Self::DynamicWorkflow(WorkflowFailure::SecondAgentError) => "second-agent-error",
            _ => "",
        }
    }

    /// Go `WantErr`: whether running the scenario yields an error.
    pub fn want_err(self) -> bool {
        match self {
            Self::ToolCall(f) => f == ToolCallFailure::InferenceError,
            Self::DynamicWorkflow(f) => f != WorkflowFailure::None,
            _ => false,
        }
    }
}

/// Go `testcasedata.Capture` values (`OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT`):
/// adk-python's experimental `SEMCONV_CONFIGS`.
const CAPTURES: [&str; 4] = ["no_content", "span_only", "event_only", "span_and_event"];

/// Go `testcasedata.SchemaVersion` values (`ADK_TELEMETRY_SCHEMA_VERSION_OPT_IN`).
const SCHEMA_VERSIONS: [&str; 2] = ["otel_semconv_1_36", "otel_semconv_1_44"];

/// Go `testcasedata.Case`: one scenario recorded under one telemetry configuration.
#[derive(Debug, Clone, Copy)]
pub struct Case {
    /// The scenario.
    pub scenario: Scenario,
    /// The capture mode, spelled as the environment sets it.
    pub capture: &'static str,
    /// The schema version, spelled as the environment sets it.
    pub schema_version: &'static str,
}

impl Case {
    /// Go `ID`: `<scenario>/[<variant>-]<capture>-<schema>`, also the golden's path.
    pub fn id(&self) -> String {
        let mut id = format!("{}-{}", self.capture, self.schema_version);
        let v = self.scenario.variant();
        if !v.is_empty() {
            id = format!("{v}-{id}");
        }
        format!("{}/{id}", self.scenario.name())
    }
}

/// Go `testcasedata.Matrix`: every variant under every capture mode and schema version.
fn matrix(variants: &[Scenario]) -> Vec<Case> {
    let mut out = Vec::new();
    for &scenario in variants {
        for capture in CAPTURES {
            for schema_version in SCHEMA_VERSIONS {
                out.push(Case {
                    scenario,
                    capture,
                    schema_version,
                });
            }
        }
    }
    out
}

/// Go `testcaseimpl.Cases`: every case of every scenario.
pub fn cases() -> Vec<Case> {
    use ToolCallFailure as T;
    use WorkflowFailure as W;
    matrix(&[
        Scenario::ToolCall(T::None),
        Scenario::ToolCall(T::InferenceError),
        Scenario::ToolCall(T::ToolError),
        Scenario::Streaming,
        Scenario::Delegation,
        Scenario::DynamicWorkflow(W::None),
        Scenario::DynamicWorkflow(W::StaticNodeError),
        Scenario::DynamicWorkflow(W::DynamicNodeError),
        Scenario::DynamicWorkflow(W::FirstAgentError),
        Scenario::DynamicWorkflow(W::SecondAgentError),
    ])
}

const APP_NAME: &str = "test_app";
const USER_ID: &str = "test_user";
const PROMPT: &str = "hello";

/// Go `Run`: drives `s` through one user turn of a real [`Runner`], backed by an in-memory session
/// service, and returns the first error the run yields. Failing to set the run up panics.
pub async fn run(s: Scenario) -> Result<(), AgentError> {
    let (root, cfg) = build(s);
    let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let r = Runner::new(RunnerConfig {
        app_name: APP_NAME.into(),
        agent: root,
        session_service: svc.clone(),
        artifact_service: None,
        memory_service: None,
        plugin_config: PluginConfig::default(),
        auto_create_session: false,
        platform: Platform::default(),
        compaction: None,
    })
    .expect("runner.New");
    let sess = svc
        .create(CreateRequest {
            app_name: APP_NAME.into(),
            user_id: USER_ID.into(),
            ..Default::default()
        })
        .await
        .expect("session create");
    let msg = Content::from_text(PROMPT, "user");
    let mut events = r.run(USER_ID, sess.id(), Some(msg), cfg, RunOptions::default());
    while let Some(item) = events.next().await {
        item?;
    }
    Ok(())
}

/// Go `build`: the root agent `s` runs, and the config to run it with.
fn build(s: Scenario) -> (Arc<dyn Agent>, RunConfig) {
    match s {
        Scenario::ToolCall(f) => (new_tool_call(f), RunConfig::default()),
        Scenario::Streaming => (
            new_streaming(),
            RunConfig {
                streaming_mode: StreamingMode::Sse,
                ..Default::default()
            },
        ),
        Scenario::Delegation => (new_delegation(), RunConfig::default()),
        Scenario::DynamicWorkflow(f) => (new_dynamic_workflow(f), RunConfig::default()),
    }
}

fn model_text(text: &str) -> Content {
    Content::from_text(text, "model")
}

fn model_call(name: &str, args: Value) -> Content {
    Content {
        role: "model".into(),
        parts: vec![Part {
            function_call: Some(FunctionCall {
                name: name.into(),
                args: args.as_object().cloned().unwrap_or_default(),
                ..Default::default()
            }),
            ..Default::default()
        }],
    }
}

#[derive(Deserialize, JsonSchema)]
struct Args {
    arg1: String,
}

/// Go `newToolCall`.
fn new_tool_call(f: ToolCallFailure) -> Arc<dyn Agent> {
    // A MockModel with no responses left fails the call.
    let responses = if f == ToolCallFailure::InferenceError {
        vec![]
    } else {
        vec![
            model_call("some_tool", json!({"arg1": "val1"})),
            model_text("text response"),
        ]
    };
    let sample_tool = FunctionTool::new(
        FunctionToolConfig::new("some_tool", "A sample tool."),
        move |_: Context, input: Args| async move {
            if f == ToolCallFailure::ToolError {
                return Err(ToolError::Execution("this tool always fails".into()));
            }
            Ok(format!("processed {}", input.arg1))
        },
    )
    .expect("functiontool.New");
    llm_agent::new(LlmAgentConfig {
        name: "some_root_agent".into(),
        description: "A sample root agent.".into(),
        model: Some(MockModel::from_contents(responses)),
        instruction: "you are helpful".into(),
        tools: vec![sample_tool],
        ..Default::default()
    })
    .expect("llmagent.New")
}

/// Go `newStreaming`: no tool call, unlike adk-python's (MockModel streams every call in
/// `StreamResponsesCount` chunks, a tool call included).
fn new_streaming() -> Arc<dyn Agent> {
    llm_agent::new(LlmAgentConfig {
        name: "some_root_agent".into(),
        description: "A sample root agent.".into(),
        model: Some(MockModel::from_contents_streaming(
            vec![model_text("text "), model_text("response")],
            2,
        )),
        instruction: "you are helpful".into(),
        ..Default::default()
    })
    .expect("llmagent.New")
}

/// Go `newDelegation`.
fn new_delegation() -> Arc<dyn Agent> {
    let delegate = llm_agent::new(LlmAgentConfig {
        name: "some_delegate_agent".into(),
        description: "A delegate agent.".into(),
        model: Some(MockModel::from_contents(vec![model_text(
            "delegate response",
        )])),
        instruction: "you are helpful".into(),
        ..Default::default()
    })
    .expect("llmagent.New(delegate)");
    llm_agent::new(LlmAgentConfig {
        name: "some_delegating_agent".into(),
        description: "A delegating agent.".into(),
        model: Some(MockModel::from_contents(vec![
            model_call("some_delegate_agent", json!({"request": "delegate this"})),
            model_text("text response"),
        ])),
        instruction: "you are helpful".into(),
        tools: vec![AgentTool::new(delegate, None)],
        ..Default::default()
    })
    .expect("llmagent.New(root)")
}

/// Go `newDynamicWorkflow`.
fn new_dynamic_workflow(f: WorkflowFailure) -> Arc<dyn Agent> {
    // No RetryConfig: a failing node fails on its first attempt, so a failure emits one span per
    // node rather than one per retry.
    let node_cfg = NodeConfig::default;

    let static_node = function_node(
        "static_node",
        move |_: Context, input: String| async move {
            if f == WorkflowFailure::StaticNodeError {
                return Err(AgentError::other("boom: static node failed"));
            }
            Ok(input.to_uppercase())
        },
        node_cfg(),
    );

    let task_agent = new_collaborative_agent(
        "task_agent",
        Mode::Task,
        f != WorkflowFailure::FirstAgentError,
        "task complete",
    );
    let task_node: Arc<dyn Node> =
        AgentNode::new(task_agent, node_cfg()).expect("NewAgentNode(task_agent)");

    let single_turn_agent = new_collaborative_agent(
        "single_turn_agent",
        Mode::SingleTurn,
        f != WorkflowFailure::SecondAgentError,
        "single turn complete",
    );
    let single_turn_node: Arc<dyn Node> =
        AgentNode::new(single_turn_agent, node_cfg()).expect("NewAgentNode(single_turn_agent)");

    // A plain function node, unlike the agent nodes, gets an invoke_node span of its own, a cache
    // hit included.
    let echo_node = function_node(
        "echo_node",
        |_: Context, input: String| async move { Ok(input) },
        node_cfg(),
    );

    let router_node = dynamic_node(
        "router_node",
        move |ctx: Context, input: String, _emit: Emit| {
            let (task_node, single_turn_node, echo_node) = (
                task_node.clone(),
                single_turn_node.clone(),
                echo_node.clone(),
            );
            async move {
                if f == WorkflowFailure::DynamicNodeError {
                    return Err(AgentError::other("boom: dynamic node failed"));
                }
                let input = Value::String(input);
                run_node(&ctx, task_node, input.clone(), RunNodeOptions::default()).await?;
                run_node(
                    &ctx,
                    single_turn_node,
                    input.clone(),
                    RunNodeOptions::default(),
                )
                .await?;
                // The same run id twice: the second call is served from the RunNode cache.
                let echo = || RunNodeOptions {
                    run_id: Some("echo".into()),
                    ..Default::default()
                };
                run_node(&ctx, echo_node.clone(), input.clone(), echo()).await?;
                run_node(&ctx, echo_node, input, echo()).await?;
                Ok("done".to_owned())
            }
        },
        node_cfg(),
    );

    workflow_agent::new(WorkflowAgentConfig {
        name: "my_workflow".into(),
        description: "static node then a dynamic node delegating to two collaborative agents and a cached function node".into(),
        edges: chain(&[START.clone(), static_node, router_node]),
        ..Default::default()
    })
    .expect("workflowagent.New")
}

/// Go `newCollaborativeAgent`: an llmagent in `mode`; when `ok` is false its model has no
/// response to give, so the agent fails.
fn new_collaborative_agent(name: &str, mode: Mode, ok: bool, reply: &str) -> Arc<dyn Agent> {
    let responses = if ok { vec![model_text(reply)] } else { vec![] };
    llm_agent::new(LlmAgentConfig {
        name: name.into(),
        description: format!("collaborative {name}"),
        model: Some(MockModel::from_contents(responses)),
        instruction: "you are helpful".into(),
        mode,
        ..Default::default()
    })
    .expect("llmagent.New")
}
