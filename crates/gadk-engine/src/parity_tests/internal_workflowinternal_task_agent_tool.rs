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

//! Port of Go internal/workflowinternal/task_agent_tool_test.go.

use std::any::Any;
use std::sync::Arc;

use futures::stream::BoxStream;
use gadk_agent::{Agent, AgentConfig, AgentError, Context, InvocationContext, Tool};
use gadk_genai::{FunctionDeclaration, Schema};
use gadk_model::{Backend, Llm, LlmRequest, LlmResponse, ModelError};
use gadk_session::Event;
use serde_json::{Value, json};

use crate::llm::LlmAgentConfig;
use crate::llm::agent as llm_agent;
use crate::tool::task_agent_tool::TaskAgentTool;

const TASK_AGENT_NAME: &str = "task_agent";
const TASK_AGENT_DESC: &str = "Solves a delegated task.";
const TASK_SUFFIX: &str = "IMPORTANT: This tool delegates execution to a specialized agent. Do NOT call this tool in parallel with any other tools.";

fn schema(v: Value) -> Schema {
    serde_json::from_value(v).unwrap()
}

/// Go defaultTaskRequestParams.
fn default_task_request_params() -> Schema {
    schema(json!({
        "type": "OBJECT",
        "properties": {"request": {"type": "STRING", "description": "Detailed instructions or context for the task sub-agent."}},
        "required": ["request"]
    }))
}

/// Go sampleInputSchema (single_turn_tool_test.go; same package).
fn sample_input_schema() -> Schema {
    schema(json!({
        "type": "OBJECT",
        "properties": {"is_magic": {"type": "BOOLEAN"}, "name": {"type": "STRING"}},
        "required": ["is_magic", "name"]
    }))
}

/// Go taskAgentOutputSchema.
fn task_agent_output_schema() -> Schema {
    schema(json!({
        "type": "OBJECT",
        "properties": {"answer": {"type": "STRING"}},
        "required": ["answer"]
    }))
}

/// Port of Go fakeLLM: a model with a backend variant that never generates.
struct FakeLlm(Backend);
impl Llm for FakeLlm {
    fn name(&self) -> &str {
        "fake-model"
    }
    fn generate_content(
        &self,
        _req: LlmRequest,
        _stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        Box::pin(futures::stream::empty())
    }
    fn google_variant(&self) -> Option<Backend> {
        Some(self.0)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Port of Go newLLMAgentWithModel (undeclared mode).
fn new_llm_agent_with_model(
    name: &str,
    description: &str,
    input_schema: Option<Schema>,
    output_schema: Option<Schema>,
    model: Option<Arc<dyn Llm>>,
) -> Arc<dyn Agent> {
    llm_agent::new(LlmAgentConfig {
        name: name.into(),
        description: description.into(),
        input_schema,
        output_schema,
        model,
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("llmagent.New({name:?}): {e}"))
}

/// Port of Go newLLMAgent (single_turn_tool_test.go; same package).
fn new_llm_agent(name: &str, description: &str, input_schema: Option<Schema>) -> Arc<dyn Agent> {
    new_llm_agent_with_model(name, description, input_schema, None, None)
}

/// Port of Go newCompositeAgent (single_turn_tool_test.go; same package).
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

fn fake(backend: Backend) -> Option<Arc<dyn Llm>> {
    Some(Arc::new(FakeLlm(backend)) as Arc<dyn Llm>)
}

// parity: adapted: Go Run(nil, nil) returns a nil map; the Rust Run returns an empty Map
#[tokio::test]
async fn test_task_agent_tool_metadata() {
    let tt = TaskAgentTool::new(new_llm_agent(TASK_AGENT_NAME, TASK_AGENT_DESC, None));
    assert_eq!(tt.name(), TASK_AGENT_NAME, "Name()");
    assert_eq!(tt.description(), TASK_AGENT_DESC, "Description()");
    assert!(!tt.is_long_running(), "IsLongRunning() = true, want false");
    assert!(tt.defers_response(), "DefersResponse() = false, want true");
    let result = tt
        .as_runnable()
        .expect("runnable")
        .run(Context::builder().build(), Value::Null)
        .await;
    let result = result.unwrap_or_else(|e| panic!("Run() returned err = {e}, want nil"));
    assert!(
        result.is_empty(),
        "Run() = {result:?}, want nil (Run must be a no-op so the DefersResponse gate skips auto-FR build)"
    );
}

#[test]
fn test_task_agent_tool_declaration() {
    let string_resp = Some(json!({"type": "STRING"}));
    let decl = |name: &str,
                description: String,
                parameters: Schema,
                response_json_schema: Option<Value>| FunctionDeclaration {
        name: name.into(),
        description,
        parameters: Some(parameters),
        response_json_schema,
        ..Default::default()
    };
    let with_suffix = |d: &str| format!("{d}\n{TASK_SUFFIX}");
    let cases: Vec<(&str, Arc<dyn Agent>, FunctionDeclaration)> = vec![
        (
            "no input schema falls back to default {request: STRING}",
            new_llm_agent(TASK_AGENT_NAME, TASK_AGENT_DESC, None),
            // Model is None → not the Gemini API variant → Vertex branch; no output schema → STRING.
            decl(
                TASK_AGENT_NAME,
                with_suffix(TASK_AGENT_DESC),
                default_task_request_params(),
                string_resp.clone(),
            ),
        ),
        (
            "uses wrapped agent's input schema as is",
            new_llm_agent(
                TASK_AGENT_NAME,
                TASK_AGENT_DESC,
                Some(sample_input_schema()),
            ),
            decl(
                TASK_AGENT_NAME,
                with_suffix(TASK_AGENT_DESC),
                sample_input_schema(),
                string_resp.clone(),
            ),
        ),
        (
            "composite agent recurses into first sub-agent for input schema",
            new_composite_agent(
                "parent_no_schema",
                "Outer composite.",
                vec![new_llm_agent(
                    "child_with_schema",
                    "Inner agent.",
                    Some(sample_input_schema()),
                )],
            ),
            decl(
                "parent_no_schema",
                with_suffix("Outer composite."),
                sample_input_schema(),
                None,
            ),
        ),
        (
            "empty wrapped-agent description is trimmed (no leading newline)",
            new_llm_agent(TASK_AGENT_NAME, "", None),
            decl(
                TASK_AGENT_NAME,
                TASK_SUFFIX.into(),
                default_task_request_params(),
                string_resp.clone(),
            ),
        ),
        (
            "wrapped-agent description with trailing whitespace is trimmed",
            new_llm_agent(TASK_AGENT_NAME, "Solves problems.  ", None),
            // Go's want keeps the trailing spaces before "\n" (only the empty-description case drops the newline).
            decl(
                TASK_AGENT_NAME,
                with_suffix("Solves problems.  "),
                default_task_request_params(),
                string_resp.clone(),
            ),
        ),
        (
            "Gemini API variant suppresses ResponseJsonSchema",
            new_llm_agent_with_model(
                TASK_AGENT_NAME,
                TASK_AGENT_DESC,
                None,
                None,
                fake(Backend::GeminiApi),
            ),
            decl(
                TASK_AGENT_NAME,
                with_suffix(TASK_AGENT_DESC),
                default_task_request_params(),
                None,
            ),
        ),
        (
            "Vertex AI variant sets ResponseJsonSchema to STRING when no output schema",
            new_llm_agent_with_model(
                TASK_AGENT_NAME,
                TASK_AGENT_DESC,
                None,
                None,
                fake(Backend::VertexAi),
            ),
            decl(
                TASK_AGENT_NAME,
                with_suffix(TASK_AGENT_DESC),
                default_task_request_params(),
                string_resp.clone(),
            ),
        ),
        (
            "Vertex AI variant sets ResponseJsonSchema to OBJECT when output schema is present",
            new_llm_agent_with_model(
                TASK_AGENT_NAME,
                TASK_AGENT_DESC,
                None,
                Some(task_agent_output_schema()),
                fake(Backend::VertexAi),
            ),
            decl(
                TASK_AGENT_NAME,
                with_suffix(TASK_AGENT_DESC),
                default_task_request_params(),
                Some(json!({"type": "OBJECT"})),
            ),
        ),
    ];
    for (name, agent, want) in cases {
        let got = TaskAgentTool::new(agent).declaration();
        assert_eq!(got, Some(want), "{name}: Declaration()");
    }
}
