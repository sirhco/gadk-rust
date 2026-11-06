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

//! `AgentNode` (struct by Task 8, `Node::run` by Task 12; port of Go workflow/agent_node.go).

use std::any::Any;
use std::sync::Arc;

use futures::StreamExt;
use gadk_agent::{Agent, AgentError, Context, EventStream};
use gadk_session::Event;
use serde_json::{Value, json};

use crate::error::{WorkflowError, WorkflowErrorKind};
use crate::workflow::base_node::compile_schema;
use crate::workflow::route::go_quote;
use crate::workflow::{BaseNode, Node, NodeConfig};

/// Port of Go workflow.AgentNode: a graph node that runs an agent.
pub struct AgentNode {
    pub(crate) base: BaseNode,
    pub(crate) agent: Arc<dyn Agent>,
}

impl AgentNode {
    /// Port of Go NewAgentNode: schemas inferred for `any`, i.e. the empty schema `{}`.
    pub fn new(agent: Arc<dyn Agent>, cfg: NodeConfig) -> Result<Arc<AgentNode>, WorkflowError> {
        Self::with_schemas(agent, None, None, cfg)
    }

    /// Port of Go NewAgentNodeWithSchemas / newAgentNodeWithSchemasTyped[any, any]. A missing
    /// schema is Go's `jsonschema.For[any]`, the empty schema `{}`. (Go's "agent cannot be nil"
    /// is unreachable: an `Arc` is never null.)
    pub fn with_schemas(
        agent: Arc<dyn Agent>,
        input: Option<Value>,
        output: Option<Value>,
        mut cfg: NodeConfig,
    ) -> Result<Arc<AgentNode>, WorkflowError> {
        let input = compile_schema(Some(input.unwrap_or_else(|| json!({})))).map_err(|e| {
            WorkflowError::new(
                WorkflowErrorKind::Other,
                format!(
                    "resolving input schema for agent {}: {e}",
                    go_quote(agent.name())
                ),
            )
        })?;
        let output = compile_schema(Some(output.unwrap_or_else(|| json!({})))).map_err(|e| {
            WorkflowError::new(
                WorkflowErrorKind::Other,
                format!(
                    "resolving output schema for agent {}: {e}",
                    go_quote(agent.name())
                ),
            )
        })?;
        // The wrapped agent's run already emits an invoke_agent span.
        cfg.emits_own_span = true;
        let base = BaseNode::from_compiled(agent.name(), agent.description(), cfg, input, output);
        Ok(Arc::new(AgentNode { base, agent }))
    }

    /// The wrapped agent.
    pub fn agent(&self) -> &Arc<dyn Agent> {
        &self.agent
    }
}

impl Node for AgentNode {
    fn name(&self) -> &str {
        self.base.name()
    }
    fn description(&self) -> &str {
        self.base.description()
    }
    fn config(&self) -> &NodeConfig {
        self.base.config()
    }
    fn input_schema(&self) -> Option<&Value> {
        self.base.input_schema()
    }
    fn output_schema(&self) -> Option<&Value> {
        self.base.output_schema()
    }
    fn validate_input(&self, v: Value) -> Result<Value, WorkflowError> {
        self.base.validate_input(v)
    }
    fn validate_output(&self, v: Value) -> Result<Value, WorkflowError> {
        self.base.validate_output(v)
    }
    /// Implemented by Task 12 (port of Go workflow/agent_node.go Run); until then the node emits nothing.
    fn run(&self, _ctx: Context, _input: Value) -> EventStream {
        futures::stream::empty::<Result<Event, AgentError>>().boxed()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
