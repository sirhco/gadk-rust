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

//! Port of ADK-Go internal/workflowinternal/single_turn_tool.go.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use gadk_agent::{Agent, AgentError, Context, RunnableTool, Tool, ToolError};
use gadk_genai::{FunctionDeclaration, Schema, Type};
use serde_json::{Map, Value};

use crate::llm::state::reveal;
use crate::tool::finish_task::go_type_name;
use crate::tool::utils::validate_map_on_schema;
use crate::workflow::{AgentNode, Node, NodeConfig, RunNodeOptions, run_node};

/// Port of Go workflowinternal.SingleTurnTool: runs a single_turn sub-agent as a workflow node
/// and returns its output under `result`.
pub(crate) struct SingleTurnTool {
    agent: Arc<dyn Agent>,
    decl: FunctionDeclaration,
}

impl SingleTurnTool {
    /// Port of Go NewSingleTurnTool (infallible in Go too).
    pub(crate) fn new(agent: Arc<dyn Agent>) -> Arc<SingleTurnTool> {
        let decl = make_function_declaration(agent.as_ref());
        Arc::new(SingleTurnTool { agent, decl })
    }
}

/// Port of Go getInputSchema: an LlmAgent's own input schema, else the first sub-agent's
/// (recursively).
pub(crate) fn input_schema_of(agent: &dyn Agent) -> Option<Schema> {
    if let Some(state) = reveal(agent) {
        return state.cfg.input_schema.clone();
    }
    agent
        .sub_agents()
        .first()
        .and_then(|s| input_schema_of(s.as_ref()))
}

/// Port of Go getOutputSchema: an LlmAgent's own output schema, else the LAST sub-agent's
/// (recursively).
pub(crate) fn output_schema_of(agent: &dyn Agent) -> Option<Schema> {
    if let Some(state) = reveal(agent) {
        return state.cfg.output_schema.clone();
    }
    agent
        .sub_agents()
        .last()
        .and_then(|s| output_schema_of(s.as_ref()))
}

/// Port of Go MakeFunctionDeclaration: the agent's input schema as parameters, else a required
/// `request` string.
pub(crate) fn make_function_declaration(agent: &dyn Agent) -> FunctionDeclaration {
    let parameters = input_schema_of(agent).unwrap_or_else(|| Schema {
        r#type: Some(Type::Object),
        properties: BTreeMap::from([(
            "request".to_string(),
            Schema {
                r#type: Some(Type::String),
                ..Default::default()
            },
        )]),
        required: vec!["request".to_string()],
        ..Default::default()
    });
    FunctionDeclaration {
        name: agent.name().into(),
        description: agent.description().into(),
        parameters: Some(parameters),
        ..Default::default()
    }
}

impl Tool for SingleTurnTool {
    fn name(&self) -> &str {
        self.agent.name()
    }

    fn description(&self) -> &str {
        self.agent.description()
    }

    fn declaration(&self) -> Option<FunctionDeclaration> {
        Some(self.decl.clone())
    }

    // `process_request` is not overridden: Go's ProcessRequest is toolutils.PackTool, which the
    // flow runs via `gadk_agent::pack_tool` before calling `Tool::process_request`.

    fn as_runnable(&self) -> Option<&dyn RunnableTool> {
        Some(self)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Go `fmt.Errorf("<message>: %w", err)` where `message` already includes the source text:
/// Display = `message`, `source()` = the wrapped [`AgentError`] (keeps the `%w` chain).
#[derive(Debug)]
struct ChainedError {
    message: String,
    source: AgentError,
}

impl std::fmt::Display for ChainedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ChainedError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

#[async_trait]
impl RunnableTool for SingleTurnTool {
    /// Port of Go (*SingleTurnTool).Run.
    async fn run(&self, ctx: Context, args: Value) -> Result<Map<String, Value>, ToolError> {
        let margs = match args {
            Value::Object(m) => m,
            other => {
                return Err(ToolError::InvalidArgument(format!(
                    "single turn tool expects map[string]any arguments, got {}",
                    go_type_name(&other)
                )));
            }
        };
        let node_input = match &self.decl.parameters {
            Some(params) => {
                validate_map_on_schema(&margs, params, true).map_err(|e| {
                    ToolError::InvalidArgument(format!(
                        "argument validation failed for agent {}: {}",
                        self.agent.name(),
                        e
                    ))
                })?;
                Value::Object(margs)
            }
            // Unreachable: make_function_declaration always sets parameters (as in Go).
            None => margs.get("request").cloned().unwrap_or(Value::Null),
        };
        let node = AgentNode::new(Arc::clone(&self.agent), NodeConfig::default()).map_err(|e| {
            ToolError::Execution(format!("failed to create agent node: {e}").into())
        })?;
        let node: Arc<dyn Node> = node;
        // Go WithUseSubBranch().
        let result = run_node(
            &ctx,
            node,
            node_input,
            RunNodeOptions {
                use_sub_branch: true,
                ..Default::default()
            },
        )
        .await
        .map_err(|e| {
            ToolError::Execution(Box::new(ChainedError {
                message: format!("failed to run agent node: {e}"),
                source: e,
            }))
        })?;
        let mut out = Map::new();
        out.insert("result".into(), result);
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
    fn default_declaration_has_request_string() {
        let t = SingleTurnTool::new(llm_agent("helper", "Helps.", None, None, None));
        assert_eq!(
            (t.name(), t.description(), t.is_long_running()),
            ("helper", "Helps.", false)
        );
        assert_eq!(
            serde_json::to_value(t.declaration().unwrap()).unwrap(),
            json!({
                "name": "helper", "description": "Helps.",
                "parameters": {"type": "OBJECT", "properties": {"request": {"type": "STRING"}}, "required": ["request"]}
            })
        );
    }

    #[test]
    fn composite_agent_recurses_into_first_sub_agent_for_input_schema() {
        let schema: Schema = serde_json::from_value(json!({"type": "OBJECT", "properties": {"name": {"type": "STRING"}}, "required": ["name"]})).unwrap();
        let child = llm_agent(
            "child_with_schema",
            "Inner agent.",
            Some(schema.clone()),
            None,
            None,
        );
        let parent = gadk_agent::agent::new(AgentConfig {
            name: "parent_no_schema".into(),
            description: "Outer composite.".into(),
            sub_agents: vec![child],
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            make_function_declaration(parent.as_ref()).parameters,
            Some(schema)
        );
    }

    #[tokio::test]
    async fn run_failures_match_go_text() {
        let t = SingleTurnTool::new(llm_agent("helper", "Helps.", None, None, None));
        let r = t.as_runnable().unwrap();
        let ctx = Context::builder().build();
        let e = r
            .run(ctx.clone(), json!("not a map"))
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(
            e,
            "single turn tool expects map[string]any arguments, got string"
        );
        let e = r.run(ctx, json!({})).await.unwrap_err().to_string();
        assert!(
            e.starts_with("argument validation failed for agent helper: ") && e.contains("request"),
            "{e}"
        );
    }
}
