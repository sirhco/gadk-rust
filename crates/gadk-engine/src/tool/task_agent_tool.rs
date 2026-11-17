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

//! The task-agent tool: the parent's handle on a task-mode sub-agent.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use gadk_agent::{Agent, Context, RunnableTool, Tool, ToolError, ToolOriginKind};
use gadk_genai::{FunctionDeclaration, Schema, Type};
use gadk_model::is_gemini_api_variant;
use serde_json::{Map, Value, json};

use crate::llm::state::reveal;
use crate::tool::single_turn::{input_schema_of, output_schema_of};

const TASK_DESCRIPTION_SUFFIX: &str = "IMPORTANT: This tool delegates execution to a specialized agent. Do NOT call this tool in parallel with any other tools.";

/// The parent's handle on a task-mode sub-agent. The
/// delegation itself is dispatched by the chat wrapper; running the tool is a no-op.
pub(crate) struct TaskAgentTool {
    agent: Arc<dyn Agent>,
    decl: FunctionDeclaration,
}

impl TaskAgentTool {
    /// Creates the tool for `agent`.
    pub(crate) fn new(agent: Arc<dyn Agent>) -> Arc<TaskAgentTool> {
        let decl = create_task_agent_func_declaration(agent.as_ref());
        Arc::new(TaskAgentTool { agent, decl })
    }
}

/// The function declaration for a task agent: its description (plus a note not to call it in
/// parallel) and its input and output schemas.
fn create_task_agent_func_declaration(agent: &dyn Agent) -> FunctionDeclaration {
    // `str::trim` trims the Unicode White_Space set.
    let description = format!("{}\n{}", agent.description(), TASK_DESCRIPTION_SUFFIX)
        .trim()
        .to_string();
    let parameters = input_schema_of(agent).unwrap_or_else(|| Schema {
        r#type: Some(Type::Object),
        properties: BTreeMap::from([(
            "request".to_string(),
            Schema {
                r#type: Some(Type::String),
                description: "Detailed instructions or context for the task sub-agent.".into(),
                ..Default::default()
            },
        )]),
        required: vec!["request".to_string()],
        ..Default::default()
    });
    let mut decl = FunctionDeclaration {
        name: agent.name().into(),
        description,
        parameters: Some(parameters),
        ..Default::default()
    };
    // A non-LLM composite agent gets no response schema, even though
    // output_schema_of would recurse into its sub-agents.
    if let Some(state) = reveal(agent) {
        // A model-less agent counts as the Gemini API variant and gets one.
        let gemini_api = state
            .cfg
            .model
            .as_deref()
            .is_some_and(is_gemini_api_variant);
        if !gemini_api {
            let ty = if output_schema_of(agent).is_some() {
                "OBJECT"
            } else {
                "STRING"
            };
            // This is exactly the schema `{"type": ty}`.
            decl.response_json_schema = Some(json!({ "type": ty }));
        }
    }
    decl
}

impl Tool for TaskAgentTool {
    fn name(&self) -> &str {
        self.agent.name()
    }

    fn description(&self) -> &str {
        self.agent.description()
    }

    fn declaration(&self) -> Option<FunctionDeclaration> {
        Some(self.decl.clone())
    }

    // `process_request` is not overridden: its job is `pack_tool`, which the
    // flow runs via `gadk_agent::pack_tool` before calling `Tool::process_request`.

    fn as_runnable(&self) -> Option<&dyn RunnableTool> {
        Some(self)
    }

    /// The FunctionResponse is synthesized by the chat wrapper, not by the
    /// standard tool-execution pipeline.
    fn defers_response(&self) -> bool {
        true
    }

    /// Classified as `SubAgent` (`A2A` when its agent is remote).
    fn origin_kind(&self) -> ToolOriginKind {
        ToolOriginKind::SubAgent
    }

    /// The wrapped agent.
    fn origin_agent(&self) -> Option<Arc<dyn Agent>> {
        Some(self.agent.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[async_trait]
impl RunnableTool for TaskAgentTool {
    /// A no-op: returns an empty map.
    async fn run(&self, _ctx: Context, _args: Value) -> Result<Map<String, Value>, ToolError> {
        Ok(Map::new())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use futures::stream::BoxStream;
    use gadk_model::{Backend, Llm, LlmRequest, LlmResponse, ModelError};

    use crate::llm::config::LlmAgentConfig;

    const SUFFIX: &str = "IMPORTANT: This tool delegates execution to a specialized agent. Do NOT call this tool in parallel with any other tools.";

    fn llm_agent(
        name: &str,
        description: &str,
        input: Option<Schema>,
        output: Option<Schema>,
        model: Option<Arc<dyn Llm>>,
    ) -> Arc<dyn Agent> {
        crate::llm::agent::new(LlmAgentConfig {
            name: name.into(),
            description: description.into(),
            input_schema: input,
            output_schema: output,
            model,
            ..Default::default()
        })
        .unwrap()
    }

    struct VariantLlm(Backend);

    impl Llm for VariantLlm {
        fn name(&self) -> &str {
            "gemini-2.5-flash"
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

    #[tokio::test]
    async fn metadata_and_noop_run() {
        let t = TaskAgentTool::new(llm_agent("doer", "Does things.", None, None, None));
        assert_eq!(
            (
                t.name(),
                t.description(),
                t.is_long_running(),
                t.defers_response()
            ),
            ("doer", "Does things.", false, true)
        );
        assert!(
            t.as_runnable()
                .unwrap()
                .run(Context::builder().build(), Value::Null)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn declaration_variants() {
        let d = TaskAgentTool::new(llm_agent("doer", "Does things.", None, None, None))
            .declaration()
            .unwrap();
        assert_eq!(
            serde_json::to_value(d).unwrap(),
            json!({
                "name": "doer", "description": format!("Does things.\n{SUFFIX}"),
                "parameters": {"type": "OBJECT", "properties": {"request": {"type": "STRING", "description": "Detailed instructions or context for the task sub-agent."}}, "required": ["request"]},
                "responseJsonSchema": {"type": "STRING"}
            })
        );
        assert_eq!(
            TaskAgentTool::new(llm_agent("doer", "", None, None, None))
                .declaration()
                .unwrap()
                .description,
            SUFFIX
        );
        let gemini: Arc<dyn Llm> = Arc::new(VariantLlm(Backend::GeminiApi));
        assert_eq!(
            TaskAgentTool::new(llm_agent("doer", "d", None, None, Some(gemini)))
                .declaration()
                .unwrap()
                .response_json_schema,
            None
        );
        let vertex: Arc<dyn Llm> = Arc::new(VariantLlm(Backend::VertexAi));
        let out = Schema {
            r#type: Some(Type::Object),
            ..Default::default()
        };
        assert_eq!(
            TaskAgentTool::new(llm_agent("doer", "d", None, Some(out), Some(vertex)))
                .declaration()
                .unwrap()
                .response_json_schema,
            Some(json!({"type": "OBJECT"}))
        );
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod origin_tests {
    use super::*;
    use crate::llm::config::LlmAgentConfig;

    #[test]
    fn task_agent_tool_is_a_sub_agent_tool_of_its_agent() {
        let agent = crate::llm::agent::new(LlmAgentConfig {
            name: "child".into(),
            ..Default::default()
        })
        .unwrap();
        let t = TaskAgentTool::new(agent.clone());
        assert_eq!(t.origin_kind(), ToolOriginKind::SubAgent);
        assert!(Arc::ptr_eq(&t.origin_agent().unwrap(), &agent));
    }
}
