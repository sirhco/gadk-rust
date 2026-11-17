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

//! `Workflow` and `WorkflowOptions` (the scheduler and persistence modules add impl blocks).
//! `Workflow` is not a [`Node`](crate::workflow::Node); `WorkflowNode` wraps it.

use std::sync::Arc;

use gadk_agent::{Context, ContextDelta, EventStream, InvocationContext};
use gadk_session::Session;
use serde_json::{Map, Value};

use crate::error::{WorkflowError, WorkflowErrorKind};
use crate::workflow::base_node::compile_schema;
use crate::workflow::graph::Graph;
use crate::workflow::route::go_quote;
use crate::workflow::scheduler::Scheduler;
use crate::workflow::state::RunState;
use crate::workflow::validation::{validate_nodes, validate_sub_workflow_names, validate_workflow};
use crate::workflow::{Edge, START};

/// Construction-time settings for [`Workflow::new`].
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
    /// Marks this workflow as a synthetic single-node wrapper created
    /// by the runner to drive a standalone agent. A root wrapper's run does not stamp a parent
    /// namespace prefix onto root contexts.
    pub fn with_root_wrapper(mut self) -> Self {
        self.root_wrapper = true;
        self
    }

    /// Caps how many graph-scheduled nodes may run concurrently in a single run; nodes beyond the
    /// cap queue as pending. 0 disables the cap (unlimited). Does not apply to dynamic sub-nodes
    /// invoked via RunNode.
    pub fn with_max_concurrency(mut self, n: usize) -> Self {
        self.max_concurrency = Some(n);
        self
    }

    /// Sets the state schema for the workflow.
    pub fn with_state_schema(mut self, s: Value) -> Self {
        self.state_schema = Some(s);
        self
    }
}

/// A validated workflow graph.
pub struct Workflow {
    /// The validated graph.
    pub(crate) graph: Arc<Graph>,
    /// The node-path namespacing name; empty leaves the workflow unnamed.
    pub(crate) name: String,
    /// Concurrency cap for graph-scheduled nodes; 0 = unlimited.
    pub(crate) max_concurrency: usize,
    /// The raw state schema (compiled and checked by [`Workflow::new`]).
    #[allow(dead_code)] // never read after construction either.
    pub(crate) state_schema: Option<Value>,
    /// Marks a synthetic single-node wrapper created by the runner.
    pub(crate) is_root_wrapper: bool,
}

impl Workflow {
    /// Validates `edges` and builds the workflow.
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

    /// The node-path namespacing name; empty when anonymous.
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl Workflow {
    /// Namespaces a named, non-root-wrapper workflow's node paths
    /// under `<parent>/<name>@1` and seeds START with the user content's text.
    pub fn run(self: &Arc<Self>, ctx: InvocationContext) -> EventStream {
        let c = ctx.into_context();
        let c = if !self.is_root_wrapper && !self.name.is_empty() {
            let parent = c.path();
            let wf_path = if parent.is_empty() {
                format!("{}@1", self.name)
            } else {
                format!("{parent}/{}@1", self.name)
            };
            let ancestors = c.output_for_ancestors();
            c.with_delta(ContextDelta {
                path: Some(wf_path),
                run_id: Some("1".to_string()),
                output_for_ancestors: Some(ancestors),
                sub_scheduler: Some(None),
                ..ContextDelta::default()
            })
        } else {
            c
        };
        let input = user_input(&c);
        self.run_node(c, input)
    }

    /// Runs the graph from START with `input` under `ctx`. One
    /// scheduler per call, created on the stream's first poll; dropping the stream stops the
    /// run (every node task is aborted and every node context cancelled).
    pub fn run_node(self: &Arc<Self>, ctx: Context, input: Value) -> EventStream {
        let wf = Arc::clone(self);
        Box::pin(async_stream::stream! {
            let mut s = Scheduler::new(ctx.clone(), Arc::clone(&wf.graph), wf.max_concurrency);
            s.workflow_name = wf.name.clone();
            let input = (!input.is_null()).then_some(input);
            s.state.ensure_node(START.name()).input = input.clone();
            s.schedule_node(Arc::clone(&START), input, String::new(), ctx.branch());
            while let Some(item) = s.next().await {
                yield item;
            }
        })
    }
}

impl Workflow {
    /// Rebuilds the paused run from session history. `Ok(None)` means no interrupt or failure
    /// history; a non-empty `invocation_id` scopes the scan to that invocation.
    pub fn reconstruct_run_state(
        &self,
        session: &dyn Session,
        invocation_id: &str,
    ) -> Result<Option<RunState>, WorkflowError> {
        crate::workflow::persistence::reconstruct_run_state(self, session, invocation_id)
    }

    /// Continues a paused run from `state` with `responses`
    /// (interrupt id → payload). Empty `responses` yields an empty stream.
    pub fn resume(
        self: &Arc<Self>,
        ctx: Context,
        state: RunState,
        responses: Map<String, Value>,
    ) -> EventStream {
        crate::workflow::resume::resume_stream(Arc::clone(self), ctx, state, responses)
    }
}

/// The concatenated text parts of the user content; null without one.
fn user_input(ctx: &Context) -> Value {
    match ctx.user_content() {
        None => Value::Null,
        Some(uc) => Value::String(uc.parts.iter().map(|p| p.text.as_str()).collect()),
    }
}
