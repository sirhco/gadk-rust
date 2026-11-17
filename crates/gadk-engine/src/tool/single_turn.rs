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

//! The single_turn tool: runs a single_turn sub-agent as a workflow node.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use gadk_agent::{Agent, AgentError, Context, RunnableTool, Tool, ToolError, ToolOriginKind};
use gadk_genai::{FunctionDeclaration, Schema, Type};
use serde_json::{Map, Value};

use crate::llm::state::reveal;
use crate::tool::finish_task::go_type_name;
use crate::tool::utils::validate_map_on_schema;
use crate::workflow::{AgentNode, Node, NodeConfig, RunNodeOptions, run_node};

/// Runs a single_turn sub-agent as a workflow node
/// and returns its output under `result`.
pub(crate) struct SingleTurnTool {
    agent: Arc<dyn Agent>,
    decl: FunctionDeclaration,
}

impl SingleTurnTool {
    /// Creates the tool (infallible).
    pub(crate) fn new(agent: Arc<dyn Agent>) -> Arc<SingleTurnTool> {
        let decl = make_function_declaration(agent.as_ref());
        Arc::new(SingleTurnTool { agent, decl })
    }
}

/// An LlmAgent's own input schema, else the first sub-agent's
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

/// An LlmAgent's own output schema, else the LAST sub-agent's
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

/// The agent's input schema as parameters, else a required
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

    // `process_request` is not overridden: its job is `pack_tool`, which the
    // flow runs via `gadk_agent::pack_tool` before calling `Tool::process_request`.

    fn as_runnable(&self) -> Option<&dyn RunnableTool> {
        Some(self)
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

/// An error whose `message` already includes the source text:
/// Display = `message`, `source()` = the wrapped [`AgentError`].
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
    /// Runs the agent as a workflow node and returns its output under `result`.
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
            // Unreachable: make_function_declaration always sets parameters.
            None => margs.get("request").cloned().unwrap_or(Value::Null),
        };
        let node = AgentNode::new(Arc::clone(&self.agent), NodeConfig::default()).map_err(|e| {
            ToolError::Execution(format!("failed to create agent node: {e}").into())
        })?;
        let node: Arc<dyn Node> = node;
        // Put the sub-agent's events on "<agent>@<function_call_id>", so the calling agent's
        // contents builder can tell them apart from a workflow node's branch. Only
        // the branch carries the id: the run id stays on the scheduler's counter, so a call id the
        // model reuses still runs the sub-agent, as in adk-python. Without a call id the run
        // uses `use_sub_branch`.
        let call_id = ctx.function_call_id();
        let opts = if call_id.is_empty() {
            RunNodeOptions {
                use_sub_branch: true,
                ..Default::default()
            }
        } else {
            let mut branch = format!("{}@{call_id}", self.agent.name());
            let base = ctx.branch();
            if !base.is_empty() {
                branch = format!("{base}.{branch}");
            }
            RunNodeOptions {
                override_branch: Some(branch),
                ..Default::default()
            }
        };
        let result = run_node(&ctx, node, node_input, opts).await.map_err(|e| {
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod origin_tests {
    use super::*;
    use crate::llm::config::LlmAgentConfig;

    #[test]
    fn single_turn_tool_is_a_sub_agent_tool_of_its_agent() {
        let agent = crate::llm::agent::new(LlmAgentConfig {
            name: "child".into(),
            ..Default::default()
        })
        .unwrap();
        let t = SingleTurnTool::new(agent.clone());
        assert_eq!(t.origin_kind(), ToolOriginKind::SubAgent);
        assert!(Arc::ptr_eq(&t.origin_agent().unwrap(), &agent));
    }
}
