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

//! Agent walk over a workflow graph (port of Go workflow/walk.go). The Go `init` registration
//! into internal/workflowwalk is dropped: the out-of-scope exp/appinfo is its only consumer.

use std::collections::HashSet;
use std::sync::Arc;

use gadk_agent::Agent;

use crate::workflow::graph::NodeId;
use crate::workflow::{AgentNode, Edge, Node, ParallelWorker, WorkflowNode};

/// Port of Go walkAgents: the agents run by the nodes of `edges`, including nested workflows and
/// ParallelWorkers. Each node is visited once; the same agent behind two nodes is returned twice.
pub(crate) fn walk_agents(edges: &[Edge]) -> Vec<Arc<dyn Agent>> {
    let mut agents = Vec::new();
    let mut seen = HashSet::new();
    visit_all(edges, &mut seen, &mut agents);
    agents
}

fn visit_all(edges: &[Edge], seen: &mut HashSet<NodeId>, agents: &mut Vec<Arc<dyn Agent>>) {
    for e in edges {
        visit(&e.from, seen, agents);
        visit(&e.to, seen, agents);
    }
}

fn visit(n: &Arc<dyn Node>, seen: &mut HashSet<NodeId>, agents: &mut Vec<Arc<dyn Agent>>) {
    if !seen.insert(NodeId::of(n.as_ref())) {
        return;
    }
    let any = n.as_any();
    if let Some(a) = any.downcast_ref::<AgentNode>() {
        agents.push(Arc::clone(&a.agent));
    } else if let Some(w) = any.downcast_ref::<WorkflowNode>() {
        visit_all(&w.sub_workflow.graph.all_edges(), seen, agents);
    } else if let Some(p) = any.downcast_ref::<ParallelWorker>() {
        visit(&p.wrapped, seen, agents);
    }
}
