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

//! Workflow graph validation.
//!
//! Node identity everywhere is [`same_node`]/[`NodeId`] (pointer identity). Iteration is in
//! first-appearance order so errors are deterministic.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;

use gadk_agent::Mode;
use gadk_session::{KEY_PREFIX_APP, KEY_PREFIX_TEMP, KEY_PREFIX_USER};
use serde_json::Value;

use crate::error::{WorkflowError, WorkflowErrorKind};
use crate::tool::utils::{CompiledSchema, canonical_schema_json, convert_with_schema};
use crate::workflow::graph::{Graph, NodeId};
use crate::workflow::node::same_node;
use crate::workflow::route::{
    OutgoingSummary, RouteKind, RouteValue, go_quote, is_default_route, validate_outgoing,
};
use crate::workflow::{AgentNode, Edge, JoinNode, Node, RouteSummary, START, WorkflowNode};

/// An error of `kind` whose message is `<kind's sentinel text>: <detail>`.
fn sentinel(kind: WorkflowErrorKind, detail: impl fmt::Display) -> WorkflowError {
    WorkflowError::new(
        kind,
        format!("{}: {detail}", WorkflowError::sentinel_text(kind)),
    )
}

fn is_start(n: &dyn Node) -> bool {
    same_node(n, START.as_ref())
}

/// The edge-set checks.
pub(crate) fn validate_nodes(edges: &[Edge]) -> Result<(), WorkflowError> {
    validate_unique_names(edges)?;
    validate_start_node_present(edges)?;
    validate_start_node_no_incoming(edges)?;
    validate_no_task_mode_graph_nodes(edges)?;
    validate_chat_mode_wiring(edges)?;
    Ok(())
}

/// No sub-workflow has the parent workflow's name.
pub(crate) fn validate_sub_workflow_names(
    workflow_name: &str,
    edges: &[Edge],
) -> Result<(), WorkflowError> {
    for e in edges {
        check_sub_workflow_name(e.from.as_ref(), workflow_name)?;
        check_sub_workflow_name(e.to.as_ref(), workflow_name)?;
    }
    Ok(())
}

/// Checks one node (and, recursively, its nested workflows) against the parent workflow's name.
pub(crate) fn check_sub_workflow_name(
    node: &dyn Node,
    workflow_name: &str,
) -> Result<(), WorkflowError> {
    if let Some(wf) = node.as_any().downcast_ref::<WorkflowNode>()
        && wf.sub_workflow.name() == workflow_name
    {
        return Err(sentinel(
            WorkflowErrorKind::SubWorkflowNameCollision,
            go_quote(workflow_name),
        ));
    }
    Ok(())
}

/// Distinct nodes have distinct names; the same node appearing twice is fine.
pub(crate) fn validate_unique_names(edges: &[Edge]) -> Result<(), WorkflowError> {
    let mut names: HashMap<&str, &dyn Node> = HashMap::new();
    for e in edges {
        for node in [e.from.as_ref(), e.to.as_ref()] {
            match names.get(node.name()) {
                Some(stored) if !same_node(*stored, node) => {
                    return Err(sentinel(WorkflowErrorKind::DuplicateNodeName, node.name()));
                }
                Some(_) => {}
                None => {
                    names.insert(node.name(), node);
                }
            }
        }
    }
    Ok(())
}

/// At least one edge starts at START. An empty edge set is valid: the workflow runs and produces
/// no output.
pub(crate) fn validate_start_node_present(edges: &[Edge]) -> Result<(), WorkflowError> {
    if edges.is_empty() || edges.iter().any(|e| is_start(e.from.as_ref())) {
        return Ok(());
    }
    Err(WorkflowError::sentinel(WorkflowErrorKind::NoStartNode))
}

/// No node points to START.
pub(crate) fn validate_start_node_no_incoming(edges: &[Edge]) -> Result<(), WorkflowError> {
    match edges.iter().find(|e| is_start(e.to.as_ref())) {
        Some(e) => Err(sentinel(
            WorkflowErrorKind::NodePointsToStart,
            e.from.name(),
        )),
        None => Ok(()),
    }
}

/// The graph checks.
pub(crate) fn validate_workflow(
    g: &Graph,
    state_schema: Option<&CompiledSchema>,
) -> Result<(), WorkflowError> {
    validate_unique_edges(g)?;
    validate_default_route(g)?;
    validate_connectivity(g)?;
    validate_cycles(g)?;
    validate_fan_in(g)?;
    validate_static_schemas(g)?;
    validate_state_schema_consistency(g, state_schema)?;
    validate_node_outgoing_edges(g)?;
    Ok(())
}

/// Task-mode LlmAgents cannot be static workflow graph nodes (the scheduler would overwrite the
/// task brief on re-entry).
pub(crate) fn validate_no_task_mode_graph_nodes(edges: &[Edge]) -> Result<(), WorkflowError> {
    let mut seen = HashSet::new();
    for e in edges {
        for node in [e.from.as_ref(), e.to.as_ref()] {
            if !seen.insert(NodeId::of(node)) {
                continue;
            }
            if agent_node_mode(node) == Some(Mode::Task) {
                return Err(WorkflowError::new(
                    WorkflowErrorKind::Other,
                    format!(
                        "Agent {} has mode='task' and cannot be used as a workflow graph node. Use a chat coordinator with task sub-agents, or dispatch dynamically via RunNode from a function node",
                        go_quote(node.name())
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// A chat-mode LlmAgent may only be reached from START (it ignores the node input handed down an
/// edge).
pub(crate) fn validate_chat_mode_wiring(edges: &[Edge]) -> Result<(), WorkflowError> {
    for e in edges {
        if is_start(e.from.as_ref()) {
            continue;
        }
        if agent_node_mode(e.to.as_ref()) == Some(Mode::Chat) {
            return Err(WorkflowError::new(
                WorkflowErrorKind::Other,
                format!(
                    "Agent {} has mode='chat' and cannot follow node {}: chat agents rely on conversation history and cannot consume a predecessor's node input. Use mode='single_turn', or wire the agent directly from Start",
                    go_quote(e.to.name()),
                    go_quote(e.from.name())
                ),
            ));
        }
    }
    Ok(())
}

/// The mode the LlmAgent wrapped by `node` runs under as a graph node, or `None` when `node` is not
/// an AgentNode wrapping an LlmAgent.
pub(crate) fn agent_node_mode(node: &dyn Node) -> Option<Mode> {
    let an = node.as_any().downcast_ref::<AgentNode>()?;
    let st = crate::llm::state::reveal(an.agent.as_ref())?;
    Some(crate::llm::mode::resolve_mode(
        st.cfg.mode,
        Mode::SingleTurn,
    ))
}

/// Two edges with the same (from, to) are rejected regardless of route.
pub(crate) fn validate_unique_edges(g: &Graph) -> Result<(), WorkflowError> {
    for (node, edges) in g.sources() {
        let mut targets = HashSet::new();
        for e in edges {
            if !targets.insert(NodeId::of(e.to.as_ref())) {
                return Err(sentinel(
                    WorkflowErrorKind::DuplicateEdge,
                    format!(
                        "from {} to {}",
                        go_quote(node.name()),
                        go_quote(e.to.name())
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// At most one DEFAULT route per node.
pub(crate) fn validate_default_route(g: &Graph) -> Result<(), WorkflowError> {
    for (node, edges) in g.sources() {
        let mut has_default = false;
        for e in edges {
            if e.route.as_ref().is_some_and(is_default_route) {
                if has_default {
                    return Err(sentinel(
                        WorkflowErrorKind::MultipleDefaultRoutes,
                        go_quote(node.name()),
                    ));
                }
                has_default = true;
            }
        }
    }
    Ok(())
}

/// Every node is reachable from START.
pub(crate) fn validate_connectivity(g: &Graph) -> Result<(), WorkflowError> {
    if g.successors.is_empty() {
        return Ok(());
    }
    let mut visited: HashSet<NodeId> = HashSet::new();
    let mut stack: Vec<Arc<dyn Node>> = vec![Arc::clone(&START)];
    while let Some(n) = stack.pop() {
        if !visited.insert(NodeId::of(n.as_ref())) {
            continue;
        }
        for e in g.successors_of(&n) {
            if !visited.contains(&NodeId::of(e.to.as_ref())) {
                stack.push(Arc::clone(&e.to));
            }
        }
    }
    let mut unreachable: Vec<&str> = g
        .all_nodes()
        .iter()
        .filter(|n| !visited.contains(&NodeId::of(n.as_ref())))
        .map(|n| n.name())
        .collect();
    unreachable.sort_unstable();
    if unreachable.is_empty() {
        return Ok(());
    }
    Err(sentinel(
        WorkflowErrorKind::NodesNotReachable,
        go_quote(&unreachable.join(", ")),
    ))
}

/// No cycle made only of unconditional
/// (`route == None`) edges. DEFAULT and every other route count as conditional.
pub(crate) fn validate_cycles(g: &Graph) -> Result<(), WorkflowError> {
    fn traverse(
        g: &Graph,
        n: &Arc<dyn Node>,
        visited: &mut HashSet<NodeId>,
        in_stack: &mut HashSet<NodeId>,
    ) -> Result<(), WorkflowError> {
        let id = NodeId::of(n.as_ref());
        if in_stack.contains(&id) {
            return Err(sentinel(
                WorkflowErrorKind::UnconditionalCycle,
                go_quote(n.name()),
            ));
        }
        if visited.contains(&id) {
            return Ok(());
        }
        in_stack.insert(id);
        visited.insert(id);
        for e in g.successors_of(n) {
            if e.route.is_none() {
                traverse(g, &e.to, visited, in_stack)?;
            }
        }
        in_stack.remove(&id);
        Ok(())
    }

    let mut visited = HashSet::new();
    for (node, _) in g.sources() {
        if !visited.contains(&NodeId::of(node.as_ref())) {
            let mut in_stack = HashSet::new();
            traverse(g, node, &mut visited, &mut in_stack)?;
        }
    }
    Ok(())
}

/// A non-JoinNode target may have at most one unconditional incoming edge.
pub(crate) fn validate_fan_in(g: &Graph) -> Result<(), WorkflowError> {
    for (node, edges) in g.targets() {
        if node.as_any().is::<JoinNode>() {
            continue;
        }
        let unconditional = edges.iter().filter(|e| e.route.is_none()).count();
        if unconditional > 1 {
            return Err(sentinel(
                WorkflowErrorKind::UnsupportedFanIn,
                format!(
                    "node {} has {} unconditional incoming edges; use a JoinNode to converge branches",
                    go_quote(node.name()),
                    unconditional
                ),
            ));
        }
    }
    Ok(())
}

/// Validates `data` against `schema`. When
/// `data` is a string and the schema expects a non-string shape, it first tries to parse the
/// string as JSON, then the raw string (enum/literal schemas), then standard validation.
pub(crate) fn default_validate_input(
    data: Value,
    schema: Option<&CompiledSchema>,
) -> Result<Value, String> {
    let Some(schema) = schema else {
        return Ok(data);
    };
    if data.is_null() {
        return Ok(Value::Null);
    }
    if let Value::String(text) = &data {
        if schema_is_string(Some(schema)) {
            // The schema expects a string: validate the raw string directly.
            schema.validate(&data)?;
            return Ok(data);
        }
        // Step 1: try JSON parse.
        if let Ok(parsed) = serde_json::from_str::<Value>(text)
            && schema.validate(&parsed).is_ok()
        {
            return Ok(parsed);
        }
        // Step 2: the raw string may match an enum/literal schema.
        if schema.validate(&data).is_ok() {
            return Ok(data);
        }
        // Step 3: fall through to standard validation (which returns the error).
    }
    convert_with_schema::<Value, Value>(&data, Some(schema))
}

/// Whether the schema expects a JSON string at the top level (`"type": "string"` or a `"type"`
/// array containing it).
pub(crate) fn schema_is_string(schema: Option<&CompiledSchema>) -> bool {
    let Some(schema) = schema else { return false };
    match schema.schema().get("type") {
        Some(Value::String(t)) => t == "string",
        Some(Value::Array(ts)) => ts.iter().any(|t| t == "string"),
        _ => false,
    }
}

/// Every state field a node binds is declared in the workflow's state schema (app:/user:/temp: keys
/// excepted).
pub(crate) fn validate_state_schema_consistency(
    g: &Graph,
    schema: Option<&CompiledSchema>,
) -> Result<(), WorkflowError> {
    let Some(schema) = schema else { return Ok(()) };
    let fields = extract_field_names(schema.schema());
    for n in g.all_nodes() {
        let Some(names) = n.state_field_names() else {
            continue;
        };
        for field in names {
            if field.starts_with(KEY_PREFIX_APP)
                || field.starts_with(KEY_PREFIX_USER)
                || field.starts_with(KEY_PREFIX_TEMP)
            {
                continue;
            }
            if !fields.contains(field) {
                return Err(WorkflowError::new(
                    WorkflowErrorKind::Other,
                    format!(
                        "node {} references state field {} which is not declared in StateSchema (declared: [{}])",
                        go_quote(n.name()),
                        go_quote(field),
                        fields.join(" ")
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// The sorted keys of the schema's `"properties"`.
pub(crate) fn extract_field_names(schema: &Value) -> Vec<String> {
    let mut fields: Vec<String> = schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|p| p.keys().cloned().collect())
        .unwrap_or_default();
    fields.sort();
    fields
}

/// When both ends of an edge declare a schema, they must be canonically equal. (Canonicalizing a
/// `Value` cannot fail, so there is no comparison-error case.)
pub(crate) fn validate_static_schemas(g: &Graph) -> Result<(), WorkflowError> {
    for e in g.all_edges() {
        let (Some(out), Some(input)) = (e.from.output_schema(), e.to.input_schema()) else {
            // Validate only when both ends declare schemas (Python parity).
            continue;
        };
        if !schemas_equal_canonical(out, input) {
            return Err(WorkflowError::new(
                WorkflowErrorKind::Other,
                format!(
                    "graph validation failed: schema mismatch on edge {} -> {}",
                    e.from.name(),
                    e.to.name()
                ),
            ));
        }
    }
    Ok(())
}

/// Whether two schemas are equal in their canonical form.
pub(crate) fn schemas_equal_canonical(a: &Value, b: &Value) -> bool {
    canonical_schema_json(a) == canonical_schema_json(b)
}

/// Runs the registered outgoing-edge validators on every node, in name order.
pub(crate) fn validate_node_outgoing_edges(g: &Graph) -> Result<(), WorkflowError> {
    let mut nodes = g.all_nodes().to_vec();
    nodes.sort_by(|a, b| a.name().cmp(b.name()));
    for n in &nodes {
        validate_outgoing(n.as_ref(), &summarize_outgoing_edges(g.successors_of(n)))?;
    }
    Ok(())
}

/// Summarizes a node's outgoing edges: whether it has unrouted and default edges, and a labelled
/// value per route (a MultiRoute counts once per element).
pub(crate) fn summarize_outgoing_edges(edges: &[Edge]) -> OutgoingSummary {
    let mut out = OutgoingSummary::default();
    for e in edges {
        let Some(r) = &e.route else {
            out.has_unrouted = true;
            continue;
        };
        if is_default_route(r) {
            out.has_default = true;
            continue;
        }
        match r.summary() {
            RouteSummary::Multi(items) if items.is_empty() => out.routes.push(RouteValue {
                kind: RouteKind::Other,
                label: "[]".into(),
            }),
            // A MultiRoute is flattened one level: one value per element.
            RouteSummary::Multi(items) => out.routes.extend(items.iter().map(route_value)),
            other => out.routes.push(route_value(&other)),
        }
    }
    out
}

/// The labelled value for one route or MultiRoute element.
fn route_value(s: &RouteSummary) -> RouteValue {
    match s {
        RouteSummary::String(v) => RouteValue {
            kind: RouteKind::String,
            label: go_quote(v),
        },
        RouteSummary::Bool(b) => RouteValue {
            kind: RouteKind::Bool,
            label: b.to_string(),
        },
        RouteSummary::Int(i) => RouteValue {
            kind: RouteKind::Other,
            label: i.to_string(),
        },
        RouteSummary::Other(l) => RouteValue {
            kind: RouteKind::Other,
            label: l.clone(),
        },
        RouteSummary::Default => RouteValue {
            kind: RouteKind::Other,
            label: "Default".into(),
        },
        // A nested multi: labelled with its debug form.
        RouteSummary::Multi(_) => RouteValue {
            kind: RouteKind::Other,
            label: format!("{s:?}"),
        },
    }
}
