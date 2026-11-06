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

//! `WorkflowNode` (struct by Task 8, `Node::run` by Task 10; port of Go workflow/workflow_node.go).

use std::any::Any;
use std::sync::Arc;

use futures::StreamExt;
use gadk_agent::{AgentError, Context, EventStream};
use gadk_session::Event;
use serde_json::Value;

use crate::error::WorkflowError;
use crate::workflow::{BaseNode, Edge, Node, NodeConfig, Workflow, WorkflowOptions};

/// Port of Go workflow.WorkflowNode: a graph node that runs a nested workflow.
pub struct WorkflowNode {
    pub(crate) base: BaseNode,
    pub(crate) sub_workflow: Arc<Workflow>,
}

impl WorkflowNode {
    /// Port of Go NewWorkflowNode: builds the nested workflow named `name` from `edges`.
    pub fn new(name: &str, edges: Vec<Edge>) -> Result<Arc<WorkflowNode>, WorkflowError> {
        let wf = Workflow::new(name, edges, WorkflowOptions::default())?;
        Ok(Arc::new(WorkflowNode {
            base: BaseNode::new(name, "", NodeConfig::default()),
            sub_workflow: wf,
        }))
    }
}

impl Node for WorkflowNode {
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
    /// Implemented by Task 10 (port of Go workflow/workflow_node.go Run); until then the node emits nothing.
    fn run(&self, _ctx: Context, _input: Value) -> EventStream {
        futures::stream::empty::<Result<Event, AgentError>>().boxed()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
