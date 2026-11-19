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

//! Output-schema request processor and the `set_model_response` tool.

use std::{any::Any, sync::Arc};

use async_trait::async_trait;
use futures::future::BoxFuture;
use gadk_agent::tool_util::append_instructions;
use gadk_agent::{
    AgentError, Context, InvocationContext, Mode, RunnableTool, Tool, ToolError, pack_tool,
};
use gadk_genai::{Content, FunctionDeclaration, Part, Schema};
use gadk_model::LlmRequest;
use gadk_session::Event;
use serde_json::{Map, Value};

use crate::{
    llm::{
        flow::Flow,
        output_schema_binding::output_schema_for,
        processors::{go_json_marshal, wrap_err},
        state::{LlmAgentState, reveal},
    },
    tool::utils::{json_kind_article, validate_map_on_schema},
};

pub(crate) const SET_MODEL_RESPONSE_TOOL_NAME: &str = "set_model_response";

const INSTRUCTION_FOR_PROCESSOR: &str = "IMPORTANT: You have access to other tools, but you must provide your final response using the set_model_response tool with the required structured format. After using any other tools needed to complete the task, always call set_model_response with your final answer in the specified schema format.";

/// Adds the
/// set_model_response tool to handle structured output.
pub(crate) fn output_schema_request_processor<'a>(
    ctx: &'a InvocationContext,
    req: &'a mut LlmRequest,
    _f: &'a Flow,
) -> BoxFuture<'a, Result<Vec<Event>, AgentError>> {
    Box::pin(async move {
        let agent = ctx.agent();
        let Some(state) = reveal(&*agent) else {
            return Ok(Vec::new());
        };
        // Check if we need the processor in the first place.
        if !need_output_schema_processor(state) {
            return Ok(Vec::new());
        }
        // The declared schema, else one a typed workflow node bound.
        let Some(schema) = output_schema_for(ctx, state) else {
            return Ok(Vec::new());
        };
        let tool: Arc<dyn Tool> = Arc::new(SetModelResponseTool { schema });
        pack_tool(req, &tool).map_err(|e| wrap_err("failed to pack set_model_response tool", e))?;
        append_instructions(req, &[INSTRUCTION_FOR_PROCESSOR]);
        Ok(Vec::new())
    })
}

/// A final model response
/// event from set_model_response JSON.
pub(crate) fn create_final_model_response_event(ctx: &InvocationContext, response: &str) -> Event {
    let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
    ev.author = ctx.agent().name().to_owned();
    ev.branch = ctx.branch();
    ev.response.content = Some(Content {
        role: "model".into(),
        parts: vec![Part {
            text: response.to_owned(),
            ..Default::default()
        }],
    });
    ev
}

/// The JSON of the first
/// set_model_response function response, or "" when there is none.
pub(crate) fn retrieve_structured_model_response(ev: Option<&Event>) -> Result<String, AgentError> {
    let Some(content) = ev.and_then(|e| e.response.content.as_ref()) else {
        return Ok(String::new());
    };
    for part in &content.parts {
        if let Some(fr) = &part.function_response
            && fr.name == SET_MODEL_RESPONSE_TOOL_NAME
        {
            return Ok(go_json_marshal(&Value::Object(fr.response.clone())));
        }
    }
    Ok(String::new())
}

/// Whether the agent needs the `set_model_response` tool: it has a model, is not in task mode,
/// has tools or toolsets, and the model needs it (a Gemini model that cannot combine tools
/// with a response schema).
pub(crate) fn need_output_schema_processor(state: &LlmAgentState) -> bool {
    let Some(model) = state.cfg.model.as_deref() else {
        return false;
    };
    // Task-mode agents already have a structured-output mechanism: the finish_task tool.
    if state.cfg.mode == Mode::Task {
        return false;
    }
    let has_tools = !state.tools.read().is_empty() || !state.cfg.toolsets.is_empty();
    has_tools && gadk_model::needs_output_schema_processor(Some(model))
}

/// The `set_model_response` tool: its parameters are the agent's output schema, and running it
/// validates the arguments against that schema.
pub(crate) struct SetModelResponseTool {
    pub(crate) schema: Schema,
}

impl Tool for SetModelResponseTool {
    fn name(&self) -> &str {
        SET_MODEL_RESPONSE_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Set your final response using the required output schema. Use this tool to provide your final structured answer instead of outputting text directly."
    }

    /// The parameters JSON schema is the output [`Schema`] serialized as JSON.
    fn declaration(&self) -> Option<FunctionDeclaration> {
        Some(FunctionDeclaration {
            name: self.name().into(),
            description: self.description().into(),
            parameters_json_schema: serde_json::to_value(&self.schema).ok(),
            ..Default::default()
        })
    }

    fn as_runnable(&self) -> Option<&dyn RunnableTool> {
        Some(self)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[async_trait]
impl RunnableTool for SetModelResponseTool {
    /// Validates the arguments against the output schema and returns them as the tool result.
    async fn run(&self, _ctx: Context, args: Value) -> Result<Map<String, Value>, ToolError> {
        let Value::Object(m) = args else {
            return Err(ToolError::InvalidArgument(format!(
                "unexpected args type for set_model_response: expected a JSON object, found {}",
                json_kind_article(&args)
            )));
        };
        validate_map_on_schema(&m, &self.schema, false)
            .map_err(|e| ToolError::InvalidArgument(format!("invalid output schema: {e}")))?;
        Ok(m)
    }
}
