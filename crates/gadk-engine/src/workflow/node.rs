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

//! `Node` trait and the `START` sentinel (port of Go workflow/workflow.go).

use std::any::Any;
use std::sync::{Arc, LazyLock};

use futures::StreamExt;
use gadk_agent::{AgentError, Context, EventStream};
use gadk_session::Event;
use serde_json::Value;

use crate::error::WorkflowError;
use crate::workflow::{BaseNode, NodeConfig};

/// Port of Go workflow.Node (workflow/workflow.go:39) plus StateParamsAware (workflow.go:62),
/// folded in as `state_field_names`. Go `any` inputs/outputs are `serde_json::Value`; Go nil is
/// `Value::Null`.
pub trait Node: Send + Sync + 'static {
    /// The node's name, unique within a workflow graph.
    fn name(&self) -> &str;
    /// The node's human-readable description.
    fn description(&self) -> &str;
    /// The node's configuration.
    fn config(&self) -> &NodeConfig;
    /// `None` = no schema, no input validation.
    fn input_schema(&self) -> Option<&Value>;
    /// `None` = no schema, no output validation.
    fn output_schema(&self) -> Option<&Value>;
    /// Called by the scheduler before `run` on every activation.
    fn validate_input(&self, v: Value) -> Result<Value, WorkflowError>;
    /// Called by the scheduler on every yielded event with an output.
    fn validate_output(&self, v: Value) -> Result<Value, WorkflowError>;
    /// Runs one activation of the node.
    fn run(&self, ctx: Context, input: Value) -> EventStream;
    /// Port of StateParamsAware.StateFieldNames; `None` = node does not bind state params.
    fn state_field_names(&self) -> Option<&[String]> {
        None
    }
    /// Downcasting support (Go type switches on concrete node types).
    fn as_any(&self) -> &dyn Any;
}

/// Go `a == b` on Node interface values (pointer identity). Compares data addresses only.
pub(crate) fn same_node(a: &dyn Node, b: &dyn Node) -> bool {
    std::ptr::addr_eq(a as *const dyn Node, b as *const dyn Node)
}

/// Port of Go startNode (workflow.go:144).
struct StartNode {
    base: BaseNode,
}

impl Node for StartNode {
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
    /// Go startNode.ValidateOutput: the start node's output is passed through unchanged.
    fn validate_output(&self, v: Value) -> Result<Value, WorkflowError> {
        Ok(v)
    }
    fn run(&self, _ctx: Context, _input: Value) -> EventStream {
        futures::stream::empty::<Result<Event, AgentError>>().boxed()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Port of Go workflow.Start: the entry-point sentinel, named "START" with description "Start node".
pub static START: LazyLock<Arc<dyn Node>> = LazyLock::new(|| {
    Arc::new(StartNode {
        base: BaseNode::new("START", "Start node", NodeConfig::default()),
    })
});

#[cfg(test)]
pub(crate) mod testing {
    use std::any::Any;
    use std::sync::Arc;

    use futures::StreamExt;
    use gadk_agent::{AgentError, Context, EventStream};
    use gadk_session::Event;
    use serde_json::Value;

    use super::Node;
    use crate::error::WorkflowError;
    use crate::workflow::{BaseNode, Edge, NodeConfig, Route};

    /// Port of Go workflow test helpers dummyNode / testNode: a BaseNode with a no-op run.
    pub(crate) struct TestNode {
        pub(crate) base: BaseNode,
    }

    impl Node for TestNode {
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
        fn run(&self, _ctx: Context, _input: Value) -> EventStream {
            futures::stream::empty::<Result<Event, AgentError>>().boxed()
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    pub(crate) fn test_node(name: &str) -> Arc<dyn Node> {
        Arc::new(TestNode {
            base: BaseNode::new(name, "", NodeConfig::default()),
        })
    }

    pub(crate) fn test_node_with_schemas(
        name: &str,
        input: Option<Value>,
        output: Option<Value>,
    ) -> Arc<dyn Node> {
        Arc::new(TestNode {
            base: BaseNode::with_schemas(name, "", input, output, NodeConfig::default())
                .expect("schemas compile"),
        })
    }

    pub(crate) fn edge(from: &Arc<dyn Node>, to: &Arc<dyn Node>) -> Edge {
        Edge {
            from: Arc::clone(from),
            to: Arc::clone(to),
            route: None,
        }
    }

    pub(crate) fn route_edge(
        from: &Arc<dyn Node>,
        to: &Arc<dyn Node>,
        route: Arc<dyn Route>,
    ) -> Edge {
        Edge {
            from: Arc::clone(from),
            to: Arc::clone(to),
            route: Some(route),
        }
    }
}
