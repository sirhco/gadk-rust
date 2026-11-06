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

//! `WorkflowNode` (port of Go workflow/workflow_node.go).

use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;

use futures::StreamExt;
use gadk_agent::{AgentError, Context, EventStream};
use gadk_session::Event;
use serde_json::Value;

use crate::error::{WorkflowError, WorkflowErrorKind};
use crate::workflow::persistence::{
    completed_delegated_output, graph_history_event, own_event_node_name,
};
use crate::workflow::route::go_quote;
use crate::workflow::scheduler::{build_nodes_by_name, child_event_output, wf_error};
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
    /// Port of Go (*WorkflowNode).Run: runs the nested workflow, forwarding its events with
    /// the output stripped, then yields the single terminal output as this node's output.
    fn run(&self, ctx: Context, input: Value) -> EventStream {
        let sub = Arc::clone(&self.sub_workflow);
        let terminals = sub.graph.terminal_node_names();
        let nodes_by_name = build_nodes_by_name(&sub.graph);
        Box::pin(async_stream::stream! {
            let mut terminal_outputs: HashMap<String, Value> = HashMap::new();
            let mut pending_err: Option<AgentError> = None;
            // The guard lives in the stream state: dropping the stream cancels the nested run.
            let (sub_ctx, guard) = ctx.with_agent_cancel();
            let mut events = sub.run_node(sub_ctx.clone(), input);
            while let Some(item) = events.next().await {
                let mut ev = match item {
                    Err(e) => {
                        pending_err = Some(e); // Go: last error wins
                        // Keep draining so the nested scheduler unwinds.
                        guard.cancel();
                        continue;
                    }
                    Ok(ev) => ev,
                };
                if let (Some(out), Some(info)) = (child_event_output(&ev), ev.node_info.as_ref()) {
                    for seg in info.path.split('/') {
                        let n = seg.split('@').next().unwrap_or(seg);
                        if terminals.contains(n) {
                            terminal_outputs.insert(n.to_string(), out);
                            break;
                        }
                    }
                } else if let Some(out) = completed_delegated_output(&ev) {
                    let owner = own_event_node_name(&graph_history_event(&ev, sub.name()), &nodes_by_name);
                    if terminals.contains(&owner) && !out.is_null() {
                        terminal_outputs.insert(owner, out);
                    }
                }
                // Forward with the output stripped: the terminal output is re-emitted below.
                ev.output = None;
                yield Ok(ev);
            }
            if let Some(e) = pending_err {
                yield Err(e);
                return;
            }
            if terminal_outputs.len() > 1 {
                yield Err(wf_error(
                    WorkflowErrorKind::MultipleOutputs,
                    format!(
                        ": sub-workflow {} has {} terminal nodes producing output; a workflow must have at most one terminal output",
                        go_quote(sub.name()),
                        terminal_outputs.len()
                    ),
                ));
                return;
            }
            if let Some(out) = terminal_outputs.into_values().next() {
                let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
                ev.output = Some(out);
                yield Ok(ev);
            }
        })
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
