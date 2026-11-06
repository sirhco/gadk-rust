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

//! `Workflow` and `WorkflowOptions` (port of Go workflow/workflow.go; Tasks 10 and 14 add impl
//! blocks). `Workflow` is not a [`Node`](crate::workflow::Node); `WorkflowNode` wraps it.

use std::sync::Arc;

use serde_json::Value;

use crate::error::{WorkflowError, WorkflowErrorKind};
use crate::workflow::Edge;
use crate::workflow::base_node::compile_schema;
use crate::workflow::graph::Graph;
use crate::workflow::route::go_quote;
use crate::workflow::validation::{validate_nodes, validate_sub_workflow_names, validate_workflow};

/// Port of Go workflow.Option / workflowOptions: construction-time settings for [`Workflow::new`].
/// Zero values mean "no override / use engine defaults".
#[derive(Clone, Default)]
pub struct WorkflowOptions {
    /// Marks the workflow as a synthetic single-node wrapper created by the runner to drive a
    /// standalone agent.
    pub root_wrapper: bool,
    /// Caps how many graph-scheduled nodes may run concurrently; `None` or `Some(0)` = unlimited.
    pub max_concurrency: Option<usize>,
    /// The workflow's state schema.
    pub state_schema: Option<Value>,
}

impl WorkflowOptions {
    /// Port of Go WithRootWrapper: marks this workflow as a synthetic single-node wrapper created
    /// by the runner to drive a standalone agent. A root wrapper's run does not stamp a parent
    /// namespace prefix onto root contexts.
    pub fn with_root_wrapper(mut self) -> Self {
        self.root_wrapper = true;
        self
    }

    /// Port of Go WithMaxConcurrency: caps how many graph-scheduled nodes may run concurrently in
    /// a single run; nodes beyond the cap queue as pending. 0 disables the cap (unlimited). Does
    /// not apply to dynamic sub-nodes invoked via RunNode.
    pub fn with_max_concurrency(mut self, n: usize) -> Self {
        self.max_concurrency = Some(n);
        self
    }

    /// Port of Go WithStateSchema: sets the state schema for the workflow.
    pub fn with_state_schema(mut self, s: Value) -> Self {
        self.state_schema = Some(s);
        self
    }
}

/// Port of Go workflow.Workflow: a validated workflow graph.
pub struct Workflow {
    /// The validated graph.
    pub(crate) graph: Arc<Graph>,
    /// The node-path namespacing name; empty leaves the workflow unnamed.
    pub(crate) name: String,
    /// Concurrency cap for graph-scheduled nodes; 0 = unlimited.
    pub(crate) max_concurrency: usize,
    /// The raw state schema (compiled and checked by [`Workflow::new`]).
    pub(crate) state_schema: Option<Value>,
    /// Marks a synthetic single-node wrapper created by the runner.
    pub(crate) is_root_wrapper: bool,
}

impl Workflow {
    /// Port of Go workflow.New (workflow.go:268): validates `edges` and builds the workflow.
    pub fn new(
        name: &str,
        edges: Vec<Edge>,
        opts: WorkflowOptions,
    ) -> Result<Arc<Workflow>, WorkflowError> {
        validate_nodes(&edges)?;
        validate_sub_workflow_names(name, &edges)?;
        // The struct keeps the raw schema (D5 pins `state_schema: Option<Value>`); it is compiled
        // here to validate.
        let compiled_state = compile_schema(opts.state_schema.clone()).map_err(|e| {
            WorkflowError::new(
                WorkflowErrorKind::Other,
                format!(
                    "resolving state schema for workflow {}: {e}",
                    go_quote(name)
                ),
            )
        })?;
        let mut graph = Graph::new(edges);
        graph.is_root_wrapper = opts.root_wrapper;
        validate_workflow(&graph, compiled_state.as_deref())?;
        Ok(Arc::new(Workflow {
            graph: Arc::new(graph),
            name: name.to_owned(),
            max_concurrency: opts.max_concurrency.unwrap_or(0),
            state_schema: opts.state_schema,
            is_root_wrapper: opts.root_wrapper,
        }))
    }

    /// Port of Go Workflow.Name: the node-path namespacing name; empty when anonymous.
    pub fn name(&self) -> &str {
        &self.name
    }
}
