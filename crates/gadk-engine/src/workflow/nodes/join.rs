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

//! `JoinNode` (port of Go workflow/join_node.go).

use std::any::Any;
use std::sync::Arc;

use futures::StreamExt;
use gadk_agent::{AgentError, Context, EventStream};
use gadk_session::Event;
use serde_json::{Map, Value};

use crate::error::WorkflowError;
use crate::workflow::route::go_quote;
use crate::workflow::{BaseNode, Node, NodeConfig};

/// Port of Go workflow.JoinNode: converges multiple branches, waiting for every predecessor.
pub struct JoinNode {
    pub(crate) base: BaseNode,
}

impl JoinNode {
    /// Port of Go NewJoinNode.
    pub fn new(name: &str) -> Arc<JoinNode> {
        Arc::new(JoinNode {
            base: BaseNode::new(name, "", NodeConfig::default()),
        })
    }

    /// Port of Go NewJoinNodeWithSchema: a JoinNode whose input is validated against `schema`.
    pub fn with_schema(name: &str, schema: Value) -> Result<Arc<JoinNode>, WorkflowError> {
        let base = BaseNode::with_schemas(name, "", Some(schema), None, NodeConfig::default())?;
        Ok(Arc::new(JoinNode { base }))
    }
}

impl Node for JoinNode {
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
    /// Port of Go (*JoinNode).ValidateInput: validates each predecessor's output individually
    /// against the input schema; a non-map input falls back to the default validation.
    fn validate_input(&self, input: Value) -> Result<Value, WorkflowError> {
        if self.base.input_schema().is_none() {
            return Ok(input);
        }
        if let Value::Object(m) = &input {
            let mut out = Map::new();
            for (pred, v) in m {
                let validated = self.base.validate_input(v.clone()).map_err(|e| {
                    WorkflowError::new(e.kind(), format!("predecessor {}: {e}", go_quote(pred)))
                        .with_source(e.into())
                })?;
                out.insert(pred.clone(), validated);
            }
            return Ok(Value::Object(out));
        }
        self.base.validate_input(input)
    }
    fn validate_output(&self, v: Value) -> Result<Value, WorkflowError> {
        self.base.validate_output(v)
    }
    /// Port of Go (*JoinNode).Run: yields the scheduler-aggregated predecessor map as its output.
    fn run(&self, ctx: Context, input: Value) -> EventStream {
        let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
        ev.output = (!input.is_null()).then_some(input);
        futures::stream::once(async move { Ok::<Event, AgentError>(ev) }).boxed()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
