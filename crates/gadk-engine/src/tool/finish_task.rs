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

//! Port of ADK-Go internal/workflowinternal/finish_task_tool.go. (task.go's TaskRequest,
//! TaskResult and DefaultTaskInput are unused by every in-scope caller and are not ported.)

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use gadk_agent::tool_util::append_instructions;
use gadk_agent::{Agent, Context, RunnableTool, Tool, ToolError};
use gadk_genai::{FunctionDeclaration, Schema, Type};
use gadk_model::LlmRequest;
use serde_json::{Map, Value};

use crate::llm::state::reveal;
/// Go `%T` of a JSON-decoded `any` (shared with the schema utilities).
pub(crate) use crate::tool::utils::go_type_name;
use crate::tool::utils::validate_map_on_schema;
use crate::workflow::route::go_quote;

/// Go FinishTaskToolName.
pub(crate) const FINISH_TASK_TOOL_NAME: &str = "finish_task";
/// Go FinishTaskSuccessResult.
pub(crate) const FINISH_TASK_SUCCESS_RESULT: &str = "Task completed.";
/// Go defaultWrapperKey.
const DEFAULT_WRAPPER_KEY: &str = "result";
/// Go ProcessRequest's raw-string instructions, byte for byte (leading and trailing newline
/// included).
pub(crate) const FINISH_TASK_INSTRUCTIONS: &str = "\nDo NOT call 'finish_task' prematurely. Use your available tools to\nfully complete every aspect of the delegated task first. If the\ntask is unclear, ask the user for clarification before proceeding.\nOnce the task is fully complete, call 'finish_task' by itself with\nno accompanying text output.\n";
const BASE_DESCRIPTION: &str = "Signal that this agent has completed its delegated task. Call this when you have finished your delegated task.";
const OUTPUT_DESCRIPTION_SUFFIX: &str = " Pass the required output data in the parameters.";

/// Port of Go workflowinternal.FinishTaskTool: lets the model signal that the task agent has
/// completed its task. The agent's output schema (or a default `result` string) validates the
/// arguments; a non-object schema is wrapped under `result`, as function parameters must be an
/// object.
pub(crate) struct FinishTaskTool {
    #[allow(dead_code)] // Go FinishTaskTool.taskAgentName; kept for parity
    task_agent_name: String,
    description: String,
    user_schema: Schema,
    wrapped_schema: Schema,
    wrapper_key: String,
}

impl FinishTaskTool {
    /// Port of Go NewFinishTaskTool.
    pub(crate) fn new(task_agent: &dyn Agent) -> Result<Arc<FinishTaskTool>, ToolError> {
        let state = reveal(task_agent).ok_or_else(|| {
            ToolError::InvalidArgument(format!(
                "NewFinishTaskTool: {} is not an LLMAgent",
                go_quote(task_agent.name())
            ))
        })?;
        let output_schema = state.cfg.output_schema.clone();
        let user_schema = output_schema.clone().unwrap_or_else(|| Schema {
            r#type: Some(Type::Object),
            properties: BTreeMap::from([(
                DEFAULT_WRAPPER_KEY.to_string(),
                Schema {
                    r#type: Some(Type::String),
                    description: "A brief summary of what the agent accomplished.".into(),
                    ..Default::default()
                },
            )]),
            required: vec![DEFAULT_WRAPPER_KEY.to_string()],
            ..Default::default()
        });
        let (wrapped_schema, wrapper_key) = if is_object_schema(&user_schema) {
            (user_schema.clone(), String::new())
        } else {
            (
                Schema {
                    r#type: Some(Type::Object),
                    properties: BTreeMap::from([(
                        DEFAULT_WRAPPER_KEY.to_string(),
                        user_schema.clone(),
                    )]),
                    required: vec![DEFAULT_WRAPPER_KEY.to_string()],
                    ..Default::default()
                },
                DEFAULT_WRAPPER_KEY.to_string(),
            )
        };
        let mut description = BASE_DESCRIPTION.to_string();
        if output_schema.is_some() {
            description.push_str(OUTPUT_DESCRIPTION_SUFFIX);
        }
        Ok(Arc::new(FinishTaskTool {
            task_agent_name: task_agent.name().to_string(),
            description,
            user_schema,
            wrapped_schema,
            wrapper_key,
        }))
    }

    /// Go WrapperKey(): `""` when the user schema is already an object.
    pub(crate) fn wrapper_key_str(&self) -> &str {
        &self.wrapper_key
    }
}

/// Port of Go isObjectSchema: case-insensitive `OBJECT` (fixtures carry both `OBJECT` and
/// `object`).
fn is_object_schema(s: &Schema) -> bool {
    match &s.r#type {
        Some(Type::Object) => true,
        Some(Type::Unknown(t)) => t.eq_ignore_ascii_case("object"),
        _ => false,
    }
}

#[async_trait]
impl Tool for FinishTaskTool {
    fn name(&self) -> &str {
        FINISH_TASK_TOOL_NAME
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn declaration(&self) -> Option<FunctionDeclaration> {
        Some(FunctionDeclaration {
            name: FINISH_TASK_TOOL_NAME.into(),
            description: self.description.clone(),
            parameters: Some(self.wrapped_schema.clone()),
            ..Default::default()
        })
    }

    /// Port of Go ProcessRequest minus `toolutils.PackTool`: the flow's tools processor calls
    /// `pack_tool` for every tool with a declaration (this one has one) before
    /// `process_request`, so packing here would fail with a duplicate-tool error. Only the
    /// instruction append remains.
    async fn process_request(&self, _ctx: &Context, req: &mut LlmRequest) -> Result<(), ToolError> {
        append_instructions(req, &[FINISH_TASK_INSTRUCTIONS]);
        Ok(())
    }

    fn as_runnable(&self) -> Option<&dyn RunnableTool> {
        Some(self)
    }

    fn wrapper_key(&self) -> Option<&str> {
        (!self.wrapper_key.is_empty()).then_some(self.wrapper_key.as_str())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[async_trait]
impl RunnableTool for FinishTaskTool {
    /// Port of Go (*FinishTaskTool).Run + validateArgs. A validation failure is a successful
    /// result carrying `"error"`, so the model sees it and can retry.
    async fn run(&self, _ctx: Context, args: Value) -> Result<Map<String, Value>, ToolError> {
        let m = match args {
            Value::Null => return Err(ToolError::InvalidArgument("missing argument".into())),
            Value::Object(m) => m,
            other => {
                return Err(ToolError::InvalidArgument(format!(
                    "unexpected args type: {}, expected map[string]any",
                    go_type_name(&other)
                )));
            }
        };
        let schema = if self.wrapper_key.is_empty() {
            &self.user_schema
        } else {
            &self.wrapped_schema
        };
        let mut out = Map::new();
        if let Err(err) = validate_map_on_schema(&m, schema, false) {
            out.insert(
                "error".into(),
                Value::String(format!(
                    "Invoking `{FINISH_TASK_TOOL_NAME}()` failed due to validation errors:\n{err}\nYou could retry calling this tool, but it is IMPORTANT for you to provide all the mandatory parameters with correct types."
                )),
            );
            return Ok(out);
        }
        out.insert(
            "result".into(),
            Value::String(FINISH_TASK_SUCCESS_RESULT.into()),
        );
        Ok(out)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use gadk_agent::AgentConfig;
    use gadk_model::Llm;
    use serde_json::json;

    use crate::llm::config::LlmAgentConfig;

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

    #[test]
    fn default_declaration_matches_go_wire() {
        let ft =
            FinishTaskTool::new(llm_agent("test_agent", "", None, None, None).as_ref()).unwrap();
        assert_eq!(ft.wrapper_key_str(), "");
        assert_eq!(
            serde_json::to_value(ft.declaration().unwrap()).unwrap(),
            json!({
                "name": "finish_task",
                "description": "Signal that this agent has completed its delegated task. Call this when you have finished your delegated task.",
                "parameters": {"type": "OBJECT", "properties": {"result": {"type": "STRING", "description": "A brief summary of what the agent accomplished."}}, "required": ["result"]}
            })
        );
    }

    #[test]
    fn object_output_schema_is_used_as_is_and_description_mentions_output_data() {
        // lowercase "object" as in TestDelegation_06
        let schema: Schema = serde_json::from_value(json!({"type": "object", "properties": {"done": {"type": "BOOLEAN"}}, "required": ["done"]})).unwrap();
        let ft = FinishTaskTool::new(
            llm_agent("test_agent", "", None, Some(schema.clone()), None).as_ref(),
        )
        .unwrap();
        assert_eq!(ft.wrapper_key_str(), "");
        assert_eq!(ft.declaration().unwrap().parameters, Some(schema));
        assert!(
            ft.description()
                .ends_with(" Pass the required output data in the parameters.")
        );
    }

    #[test]
    fn non_object_output_schema_is_wrapped_under_result() {
        let s = Schema {
            r#type: Some(Type::String),
            ..Default::default()
        };
        let ft =
            FinishTaskTool::new(llm_agent("test_agent", "", None, Some(s.clone()), None).as_ref())
                .unwrap();
        assert_eq!(ft.wrapper_key(), Some("result"));
        let p = ft.declaration().unwrap().parameters.unwrap();
        assert_eq!(p.r#type, Some(Type::Object));
        assert_eq!(p.properties.get("result"), Some(&s));
        assert_eq!(p.required, vec!["result".to_string()]);
    }

    #[test]
    fn new_rejects_non_llm_agent() {
        let custom = gadk_agent::agent::new(AgentConfig {
            name: "custom".into(),
            ..Default::default()
        })
        .unwrap();
        let err = FinishTaskTool::new(custom.as_ref()).err().unwrap();
        assert_eq!(
            err.to_string(),
            "NewFinishTaskTool: \"custom\" is not an LLMAgent"
        );
    }

    #[tokio::test]
    async fn run_success_validation_error_and_bad_args() {
        let schema: Schema = serde_json::from_value(json!({"type": "OBJECT", "properties": {"result": {"type": "STRING"}, "count": {"type": "INTEGER"}}, "required": ["result", "count"]})).unwrap();
        let ft =
            FinishTaskTool::new(llm_agent("test_agent", "", None, Some(schema), None).as_ref())
                .unwrap();
        let r = ft.as_runnable().unwrap();
        let ctx = Context::builder().build();
        assert_eq!(
            Value::Object(
                r.run(ctx.clone(), json!({"result": "ok", "count": 2}))
                    .await
                    .unwrap()
            ),
            json!({"result": "Task completed."})
        );
        let bad = r
            .run(ctx.clone(), json!({"result": "success"}))
            .await
            .unwrap();
        let msg = bad["error"].as_str().unwrap();
        assert!(msg.starts_with("Invoking `finish_task()` failed due to validation errors:\n"));
        assert!(msg.contains("count"));
        assert!(msg.ends_with("\nYou could retry calling this tool, but it is IMPORTANT for you to provide all the mandatory parameters with correct types."));
        assert_eq!(
            r.run(ctx.clone(), Value::Null)
                .await
                .unwrap_err()
                .to_string(),
            "missing argument"
        );
        assert_eq!(
            r.run(ctx, json!("not a map"))
                .await
                .unwrap_err()
                .to_string(),
            "unexpected args type: string, expected map[string]any"
        );
    }

    #[tokio::test]
    async fn process_request_appends_go_instructions() {
        let ft =
            FinishTaskTool::new(llm_agent("test_agent", "", None, None, None).as_ref()).unwrap();
        let mut req = LlmRequest::default();
        ft.process_request(&Context::builder().build(), &mut req)
            .await
            .unwrap();
        let si = req.config.unwrap().system_instruction.unwrap();
        assert_eq!(si.role, "user");
        assert_eq!(si.parts[0].text, FINISH_TASK_INSTRUCTIONS);
        assert!(
            req.tools.is_empty(),
            "packing is the flow's job (pack_tool before process_request)"
        );
    }

    #[test]
    fn constants() {
        assert_eq!(FINISH_TASK_TOOL_NAME, "finish_task");
        assert_eq!(FINISH_TASK_SUCCESS_RESULT, "Task completed.");
    }
}
