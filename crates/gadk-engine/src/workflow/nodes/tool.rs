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

//! Port of Go workflow/tool_node.go.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use gadk_agent::{AgentError, Context, ContextDelta, EventStream, Tool};
use gadk_genai::{Content, Part};
use gadk_session::{Event, EventActions};
use serde_json::{Map, Value, json};

use crate::error::{WorkflowError, WorkflowErrorKind};
use crate::tool::utils::CompiledSchema;
use crate::workflow::route::go_quote;
use crate::workflow::{BaseNode, Node, NodeConfig};

/// The node's state, shared with the `'static` run stream.
struct ToolInner {
    base: BaseNode,
    tool: Arc<dyn Tool>,
}

/// Port of Go workflow.ToolNode: a graph node that runs a directly runnable tool.
pub struct ToolNode {
    inner: Arc<ToolInner>,
}

impl ToolNode {
    /// Port of Go NewToolNode: input and output schemas inferred as `any` (the empty schema `{}`).
    pub fn new(tool: Arc<dyn Tool>, cfg: NodeConfig) -> Result<Arc<ToolNode>, WorkflowError> {
        Self::build("", tool, None, None, cfg)
    }

    /// Port of Go NewNamedToolNode: as [`ToolNode::new`] with an explicit node name.
    pub fn named(
        name: &str,
        tool: Arc<dyn Tool>,
        cfg: NodeConfig,
    ) -> Result<Arc<ToolNode>, WorkflowError> {
        Self::build(name, tool, None, None, cfg)
    }

    /// Port of Go NewToolNodeWithSchemas: a `None` schema is inferred as `any` (`{}`).
    pub fn with_schemas(
        tool: Arc<dyn Tool>,
        input: Option<Value>,
        output: Option<Value>,
        cfg: NodeConfig,
    ) -> Result<Arc<ToolNode>, WorkflowError> {
        Self::build("", tool, input, output, cfg)
    }

    /// Port of Go newToolNodeWithSchemasTyped. (Go's "tool cannot be nil" and "resolved schema is
    /// nil" are unreachable: an `Arc` is never null and a compiled schema always exists.)
    fn build(
        name: &str,
        tool: Arc<dyn Tool>,
        input: Option<Value>,
        output: Option<Value>,
        cfg: NodeConfig,
    ) -> Result<Arc<ToolNode>, WorkflowError> {
        let name = if name.is_empty() {
            tool.name().to_string()
        } else {
            name.to_string()
        };
        let input = input.unwrap_or_else(|| json!({}));
        let output = output.unwrap_or_else(|| json!({}));
        let err = |what: &str, e: String| {
            WorkflowError::new(
                WorkflowErrorKind::Other,
                format!(
                    "resolving {what} schema for tool {}: {e}",
                    go_quote(tool.name())
                ),
            )
        };
        CompiledSchema::new(input.clone()).map_err(|e| err("input", e))?;
        CompiledSchema::new(output.clone()).map_err(|e| err("output", e))?;
        if tool.as_runnable().is_none() {
            return Err(not_runnable(tool.name()));
        }
        let base =
            BaseNode::with_schemas(&name, tool.description(), Some(input), Some(output), cfg)?;
        Ok(Arc::new(ToolNode {
            inner: Arc::new(ToolInner { base, tool }),
        }))
    }
}

/// Go `fmt.Errorf("tool %q (type %T) is not directly runnable in workflow node", ..)`; Rust has
/// no `%T` of a trait object, so the type is `dyn Tool`.
fn not_runnable(tool_name: &str) -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorKind::Other,
        format!(
            "tool {} (type dyn Tool) is not directly runnable in workflow node",
            go_quote(tool_name)
        ),
    )
}

impl ToolInner {
    /// Port of Go (*ToolNode).runTool: a JSON-object string input is decoded into a map first
    /// (upstream LLM nodes often produce serialized arguments), then validated and run.
    async fn run_tool(&self, tool_ctx: Context, input: Value) -> Result<Value, AgentError> {
        let input = match input {
            Value::String(s) => match serde_json::from_str::<Map<String, Value>>(&s) {
                Ok(m) => Value::Object(m),
                Err(_) => Value::String(s),
            },
            v => v,
        };
        let name = self.tool.name();
        let tool_input = self.base.validate_input(input).map_err(|e| {
            AgentError::from(
                WorkflowError::new(
                    e.kind(),
                    format!("converting input for tool {}: {e}", go_quote(name)),
                )
                .with_source(e.into()),
            )
        })?;
        let Some(runnable) = self.tool.as_runnable() else {
            return Err(not_runnable(name).into());
        };
        match runnable.run(tool_ctx, tool_input).await {
            Ok(out) => Ok(Value::Object(out)),
            Err(e) => {
                let msg = format!("tool {} execution failed: {e}", go_quote(name));
                Err(WorkflowError::new(WorkflowErrorKind::Other, msg)
                    .with_source(AgentError::Tool {
                        tool: name.to_string(),
                        source: e,
                    })
                    .into())
            }
        }
    }

    /// Port of Go (*ToolNode).ValidateOutput: direct validation, then the FunctionTool
    /// `{"result": X}` unwrap; when both fail, the original output's error wins.
    fn validate_output(&self, out: Value) -> Result<Value, WorkflowError> {
        if self.base.output_schema().is_none() {
            return Ok(out);
        }
        if let Ok(v) = self.base.validate_output(out.clone()) {
            return Ok(v);
        }
        if let Some(inner) = out.as_object().and_then(|m| m.get("result"))
            && let Ok(v) = self.base.validate_output(inner.clone())
        {
            return Ok(v);
        }
        self.base.validate_output(out)
    }
}

impl Node for ToolNode {
    fn name(&self) -> &str {
        self.inner.base.name()
    }
    fn description(&self) -> &str {
        self.inner.base.description()
    }
    fn config(&self) -> &NodeConfig {
        self.inner.base.config()
    }
    fn input_schema(&self) -> Option<&Value> {
        self.inner.base.input_schema()
    }
    fn output_schema(&self) -> Option<&Value> {
        self.inner.base.output_schema()
    }
    fn validate_input(&self, v: Value) -> Result<Value, WorkflowError> {
        self.inner.base.validate_input(v)
    }
    fn validate_output(&self, v: Value) -> Result<Value, WorkflowError> {
        self.inner.validate_output(v)
    }
    /// Port of Go (*ToolNode).Run: runs the tool under a tool context with its own call id and
    /// actions, and emits one event carrying the tool's actions (minus any compaction) and its
    /// output; a string output is also the event's content.
    fn run(&self, ctx: Context, input: Value) -> EventStream {
        let inner = Arc::clone(&self.inner);
        Box::pin(async_stream::stream! {
            let actions = Arc::new(parking_lot::Mutex::new(EventActions {
                state_delta: Some(Map::new()),
                artifact_delta: Some(BTreeMap::new()),
                ..EventActions::default()
            }));
            // Go agent.NewToolContext(ctx, uuid.NewString(), eventActions, nil).
            let tool_ctx = ctx.with_delta(ContextDelta {
                function_call_id: Some(ctx.platform().new_uuid()),
                actions: Some(Arc::clone(&actions)),
                tool_confirmation: Some(None),
                ..ContextDelta::default()
            });
            let output = match inner.run_tool(tool_ctx, input).await {
                Ok(o) => o,
                Err(e) => {
                    yield Err(e);
                    return;
                }
            };
            let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
            let mut acts = actions.lock().clone();
            // Compaction is the framework's to write, not the tool's.
            acts.compaction = None;
            ev.actions = acts;
            if let Value::String(s) = &output {
                ev.response.content = Some(Content {
                    parts: vec![Part::from_text(s.clone())],
                    ..Content::default()
                });
            }
            ev.output = Some(output);
            yield Ok(ev);
        })
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
