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

//! Port of Go workflow/persistence.go: the metadata constants and the history attribution used
//! by the live schedulers (Task 10), and ReconstructRunState with its scan helpers (Task 14).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gadk_genai::{FunctionResponse, Part};
use gadk_session::{Event, Session};
use serde_json::{Map, Value};

use crate::error::{WorkflowError, WorkflowErrorKind};
use crate::workflow::resume::{resume_output, validate_resume_response};
use crate::workflow::route::go_quote;
use crate::workflow::scheduler::{build_nodes_by_name, child_event_output};
use crate::workflow::state::{NodeState, NodeStatus, RunState};
use crate::workflow::{Node, START, WORKFLOW_INPUT_FUNCTION_CALL_NAME, Workflow};

/// Go workflowNodeCompletedKey: a successful activation without output records its own event.
pub(crate) const WORKFLOW_NODE_COMPLETED_KEY: &str = "adk.workflow.node_completed";
/// Go workflowBasePathPrefix: `<prefix><workflowName>` records the graph's base path.
pub(crate) const WORKFLOW_BASE_PATH_PREFIX: &str = "adk.workflow.base_path.";
/// Go workflowFailedChildPathsKey.
pub(crate) const WORKFLOW_FAILED_CHILD_PATHS_KEY: &str = "adk.workflow.failed_child_paths";
/// Go workflowNodeOutcomeKey.
pub(crate) const WORKFLOW_NODE_OUTCOME_KEY: &str = "adk.workflow.node_outcome";
/// Go workflowDelegatedOutputKey.
pub(crate) const WORKFLOW_DELEGATED_OUTPUT_KEY: &str = "adk.workflow.delegated_output";
/// Go workflowNodeFailureOutcome.
pub(crate) const WORKFLOW_NODE_FAILURE_OUTCOME: &str = "failed";
/// Go workflowNodeCancelledOutcome.
pub(crate) const WORKFLOW_NODE_CANCELLED_OUTCOME: &str = "cancelled";
/// Go workflowNodeFailureCode (legacy marker read from older history).
pub(crate) const WORKFLOW_NODE_FAILURE_CODE: &str = "WorkflowNodeFailed";
/// Go workflowNodeCancelledCode (legacy marker read from older history).
pub(crate) const WORKFLOW_NODE_CANCELLED_CODE: &str = "WorkflowNodeCancelled";

/// Port of Go workflowNodeOutcome: "failed", "cancelled" or "".
pub(crate) fn workflow_node_outcome(ev: &Event) -> &'static str {
    if let Some(Value::String(o)) = ev.response.custom_metadata.get(WORKFLOW_NODE_OUTCOME_KEY) {
        if o == WORKFLOW_NODE_FAILURE_OUTCOME {
            return WORKFLOW_NODE_FAILURE_OUTCOME;
        }
        if o == WORKFLOW_NODE_CANCELLED_OUTCOME {
            return WORKFLOW_NODE_CANCELLED_OUTCOME;
        }
    }
    if ev.response.error_code == WORKFLOW_NODE_CANCELLED_CODE {
        return WORKFLOW_NODE_CANCELLED_OUTCOME;
    }
    if !ev.response.error_code.is_empty() {
        return WORKFLOW_NODE_FAILURE_OUTCOME;
    }
    ""
}

/// Port of Go completedDelegatedOutput: `Some(v)` (possibly `Value::Null`, Go nil) when the
/// event is a completion record carrying the delegated-output key; `None` otherwise.
pub(crate) fn completed_delegated_output(ev: &Event) -> Option<Value> {
    let md = &ev.response.custom_metadata;
    if md.get(WORKFLOW_NODE_COMPLETED_KEY) != Some(&Value::Bool(true)) {
        return None;
    }
    md.get(WORKFLOW_DELEGATED_OUTPUT_KEY).cloned()
}

/// Port of Go graphHistoryEvent: strips the recorded workflow base path (or the legacy
/// "<workflowName>@1" fallback) from `node_info.path` and every `output_for` entry.
pub(crate) fn graph_history_event(ev: &Event, workflow_name: &str) -> Event {
    let mut out = ev.clone();
    if out.node_info.is_none() {
        return out;
    }
    let key = format!("{WORKFLOW_BASE_PATH_PREFIX}{workflow_name}");
    let (mut base, known) = match ev.response.custom_metadata.get(&key) {
        Some(Value::String(b)) => (b.clone(), true),
        _ => (String::new(), false),
    };
    if !known && !workflow_name.is_empty() {
        base = format!("{workflow_name}@1");
    }
    if base.is_empty() {
        return out;
    }
    let prefix = format!("{base}/");
    if let Some(info) = out.node_info.as_mut() {
        info.path = info
            .path
            .strip_prefix(&prefix)
            .unwrap_or(&info.path)
            .to_string();
        info.output_for = info
            .output_for
            .iter()
            .map(|p| p.strip_prefix(&prefix).unwrap_or(p).to_string())
            .collect();
    }
    out
}

/// The node name of a path segment (`name@run_id` → `name`).
fn segment_name(seg: &str) -> &str {
    seg.split('@').next().unwrap_or(seg)
}

/// Port of Go eventNodeName: the first path segment naming a graph node, else the author.
pub(crate) fn event_node_name(
    ev: &Event,
    nodes_by_name: &HashMap<String, Arc<dyn Node>>,
) -> String {
    if let Some(info) = ev.node_info.as_ref().filter(|i| !i.path.is_empty()) {
        for seg in info.path.split('/') {
            let name = segment_name(seg);
            if nodes_by_name.contains_key(name) {
                return name.to_string();
            }
        }
    }
    ev.author.clone()
}

/// Port of Go ownPathNodeName: the graph node only when it is the path's last segment.
pub(crate) fn own_path_node_name(
    path: &str,
    nodes_by_name: &HashMap<String, Arc<dyn Node>>,
) -> String {
    let segments: Vec<&str> = path.split('/').collect();
    for (i, seg) in segments.iter().enumerate() {
        let name = segment_name(seg);
        if nodes_by_name.contains_key(name) {
            return if i == segments.len() - 1 {
                name.to_string()
            } else {
                String::new()
            };
        }
    }
    String::new()
}

/// Port of Go ownEventNodeName.
pub(crate) fn own_event_node_name(
    ev: &Event,
    nodes_by_name: &HashMap<String, Arc<dyn Node>>,
) -> String {
    if let Some(info) = ev.node_info.as_ref().filter(|i| !i.path.is_empty()) {
        return own_path_node_name(&info.path, nodes_by_name);
    }
    if nodes_by_name.contains_key(&ev.author) {
        return ev.author.clone();
    }
    String::new()
}

/// Port of Go eventOutputOwners: the emitter's own node plus every `output_for` owner.
pub(crate) fn event_output_owners(
    ev: &Event,
    nodes_by_name: &HashMap<String, Arc<dyn Node>>,
) -> Vec<String> {
    if child_event_output(ev).is_none() {
        return Vec::new();
    }
    let mut owners = Vec::new();
    let own = own_event_node_name(ev, nodes_by_name);
    if !own.is_empty() {
        owners.push(own);
    }
    if let Some(info) = ev.node_info.as_ref() {
        for path in &info.output_for {
            let own = own_path_node_name(path, nodes_by_name);
            if !own.is_empty() {
                owners.push(own);
            }
        }
    }
    owners
}

/// Port of Go nodeScanState: what the session history says, per node, about a paused run.
#[derive(Debug, Default)]
pub(crate) struct NodeScanState {
    /// The long-running tool ids the node raised (insertion-ordered).
    pub(crate) interrupts: Vec<String>,
    seen: HashSet<String>,
    /// Interrupt id → the (last) user response.
    pub(crate) resolved: HashMap<String, Value>,
    /// Interrupt id → how many user FunctionResponse events resolved it (1 = first time this
    /// turn; more = a duplicate resume replayed an already-consumed response).
    pub(crate) resolved_count: HashMap<String, u32>,
    /// Interrupt id → its declared response schema, re-extracted from the pause event.
    pub(crate) schemas: HashMap<String, Value>,
    pub(crate) branch: String,
    pub(crate) finished: bool,
    pub(crate) failed: bool,
}

impl NodeScanState {
    /// Port of Go nodeScanState.addInterrupt.
    fn add_interrupt(&mut self, id: &str) {
        if self.seen.insert(id.to_string()) {
            self.interrupts.push(id.to_string());
        }
    }
}

/// Go's per-event scan gate: skips partial events and, with a non-empty `invocation_id`, events
/// from other invocations.
fn in_scope(ev: &Event, invocation_id: &str) -> bool {
    !ev.response.partial && (invocation_id.is_empty() || ev.invocation_id == invocation_id)
}

/// Port of Go (*Workflow).ReconstructRunState: rebuilds the paused run state by scanning
/// session history (interrupts raised, the user responses that resolved them, their schemas,
/// and each node's latest outcome). Go's `(nil, nil)` is `Ok(None)`: no node has interrupt or
/// failure history. A non-empty `invocation_id` scopes the scan to that invocation.
pub(crate) fn reconstruct_run_state(
    wf: &Workflow,
    session: &dyn Session,
    invocation_id: &str,
) -> Result<Option<RunState>, WorkflowError> {
    let nodes_by_name = build_nodes_by_name(&wf.graph);
    let events = session.events();
    let scans = scan_history(&events, &nodes_by_name, invocation_id, &wf.name);
    let (node_outputs, mut completed) =
        collect_node_outputs(&events, &nodes_by_name, invocation_id, &wf.name);
    let workflow_input = first_user_input(&events, invocation_id);
    let Some(mut state) = build_run_state(
        wf,
        &scans,
        &nodes_by_name,
        &node_outputs,
        workflow_input.as_ref(),
    )?
    else {
        return Ok(None);
    };
    // Restore completed predecessors as well as interrupted nodes: join barriers consult
    // `nodes`, not the completed set, and need the predecessors' outputs after a resume.
    for (name, output) in &node_outputs {
        if !state.nodes.contains_key(name) {
            let mut ns = NodeState {
                status: NodeStatus::Completed,
                output: Some(output.clone()).filter(|v| !v.is_null()),
                ..NodeState::default()
            };
            if let Some(scan) = scans.get(name) {
                ns.branch = scan.branch.clone();
            }
            state.nodes.insert(name.clone(), ns);
        }
    }
    // Pending, failed and waiting nodes cannot suppress replay of a successor.
    completed.retain(|name| {
        state
            .nodes
            .get(name)
            .is_none_or(|ns| ns.status == NodeStatus::Completed)
    });
    state.completed = completed;
    Ok(Some(state))
}

/// Port of Go scanHistory: one pass over history giving, per static graph node, the
/// long-running interrupts it raised, the user responses that resolved them, each interrupt's
/// declared response schema, and the latest result or failure.
pub(crate) fn scan_history(
    events: &[Event],
    nodes_by_name: &HashMap<String, Arc<dyn Node>>,
    invocation_id: &str,
    workflow_name: &str,
) -> HashMap<String, NodeScanState> {
    let mut scans: HashMap<String, NodeScanState> = HashMap::new();
    let mut interrupt_owner: HashMap<String, String> = HashMap::new();
    for raw in events.iter().filter(|e| in_scope(e, invocation_id)) {
        let ev = graph_history_event(raw, workflow_name);
        // A user FunctionResponse resolves an interrupt (last response per interrupt wins).
        if ev.author == "user"
            && let Some(content) = ev.response.content.as_ref()
        {
            for fr in content.parts.iter().filter_map(fr_part) {
                let Some(owner) = interrupt_owner.get(&fr.id) else {
                    continue;
                };
                let sf = scans.entry(owner.clone()).or_default();
                // A repeated answer must not reopen work that completed after its first
                // answer. A first answer invalidates pre-resume output.
                if sf.resolved_count.get(&fr.id).copied().unwrap_or(0) == 0 || sf.failed {
                    sf.finished = false;
                }
                sf.resolved
                    .insert(fr.id.clone(), unwrap_response(&fr.response));
                *sf.resolved_count.entry(fr.id.clone()).or_default() += 1;
            }
            continue;
        }
        let owner = event_node_name(&ev, nodes_by_name);
        if !nodes_by_name.contains_key(&owner) {
            continue;
        }
        scans.entry(owner.clone()).or_default();
        for name in event_output_owners(&ev, nodes_by_name) {
            scans.entry(name).or_default().branch = ev.branch.clone();
        }
        let own = own_event_node_name(&ev, nodes_by_name);
        if !own.is_empty() {
            let has_output = child_event_output(&ev).is_some();
            let completed = ev.response.custom_metadata.get(WORKFLOW_NODE_COMPLETED_KEY)
                == Some(&Value::Bool(true));
            let outcome = workflow_node_outcome(&ev);
            let sf = scans.entry(own).or_default();
            if has_output || completed {
                sf.branch = ev.branch.clone();
                sf.finished = true;
                sf.failed = false;
            } else if !outcome.is_empty() {
                sf.branch = ev.branch.clone();
                sf.finished = false;
                // A sibling failure may cancel an asker just after its pause was persisted;
                // its interrupt still owns the resume.
                sf.failed = outcome != WORKFLOW_NODE_CANCELLED_OUTCOME
                    || unresolved_interrupts(sf).is_empty();
            }
        }
        for id in ev.long_running_tool_ids.iter().filter(|id| !id.is_empty()) {
            let s = scans.entry(owner.clone()).or_default();
            s.add_interrupt(id);
            s.finished = false;
            if s.branch.is_empty() {
                s.branch = ev.branch.clone();
            }
            interrupt_owner.insert(id.clone(), owner.clone());
            if let Some(sc) = schema_from_event(&ev, id) {
                s.schemas.insert(id.clone(), sc);
            }
        }
    }
    scans
}

/// Port of Go collectNodeOutputs: each graph node's last own or explicitly delegated output
/// (`Value::Null` is Go's nil, from a no-output completion record), plus the set of nodes that
/// emitted an event without a subsequent failure.
pub(crate) fn collect_node_outputs(
    events: &[Event],
    nodes_by_name: &HashMap<String, Arc<dyn Node>>,
    invocation_id: &str,
    workflow_name: &str,
) -> (HashMap<String, Value>, HashSet<String>) {
    let mut outputs: HashMap<String, Value> = HashMap::new();
    let mut completed: HashSet<String> = HashSet::new();
    let mut interrupt_owner: HashMap<String, String> = HashMap::new();
    let mut resolved: HashSet<String> = HashSet::new();
    for raw in events.iter().filter(|e| in_scope(e, invocation_id)) {
        let ev = graph_history_event(raw, workflow_name);
        if ev.author == "user"
            && let Some(content) = ev.response.content.as_ref()
        {
            for fr in content.parts.iter().filter_map(fr_part) {
                if let Some(owner) = interrupt_owner.get(&fr.id)
                    && !resolved.contains(&fr.id)
                {
                    outputs.remove(owner);
                    completed.remove(owner);
                    resolved.insert(fr.id.clone());
                }
            }
            continue;
        }
        let name = event_node_name(&ev, nodes_by_name);
        if !nodes_by_name.contains_key(&name) {
            continue;
        }
        completed.insert(name.clone());
        for id in ev.long_running_tool_ids.iter().filter(|id| !id.is_empty()) {
            interrupt_owner.insert(id.clone(), name.clone());
        }
        let own = own_event_node_name(&ev, nodes_by_name);
        if !own.is_empty() {
            if ev.response.custom_metadata.get(WORKFLOW_NODE_COMPLETED_KEY)
                == Some(&Value::Bool(true))
            {
                match completed_delegated_output(&ev) {
                    Some(value) => {
                        if !value.is_null() || !outputs.contains_key(&own) {
                            outputs.insert(own, value);
                        }
                    }
                    None => {
                        outputs.insert(own, Value::Null);
                    }
                }
            } else if child_event_output(&ev).is_none() && !workflow_node_outcome(&ev).is_empty() {
                outputs.remove(&own);
                completed.remove(&own);
            }
        }
        // Prefer an explicit output; otherwise the model message of a MessageAsOutput event.
        let Some(out) = child_event_output(&ev) else {
            continue;
        };
        for owner in event_output_owners(&ev, nodes_by_name) {
            outputs.insert(owner, out.clone());
        }
    }
    (outputs, completed)
}

/// Port of Go (*Workflow).buildRunState: interrupted or failed nodes' scans as NodeStates
/// (names in sorted order, so the first validation error is deterministic). `None` when there
/// is no interrupt or failure history.
fn build_run_state(
    wf: &Workflow,
    scans: &HashMap<String, NodeScanState>,
    nodes_by_name: &HashMap<String, Arc<dyn Node>>,
    node_outputs: &HashMap<String, Value>,
    workflow_input: Option<&Value>,
) -> Result<Option<RunState>, WorkflowError> {
    let mut names: Vec<&String> = scans.keys().collect();
    names.sort();
    let mut state: Option<RunState> = None;
    for name in names {
        let scan = &scans[name];
        if scan.interrupts.is_empty() && !scan.failed {
            continue;
        }
        let ns = infer_node_state(
            wf,
            nodes_by_name.get(name),
            scan,
            node_outputs,
            workflow_input,
        )?;
        state
            .get_or_insert_with(RunState::new)
            .nodes
            .insert(name.clone(), ns);
    }
    Ok(state)
}

/// Port of Go unresolvedInterrupts: the raised interrupts no user response resolved yet, in
/// insertion order.
fn unresolved_interrupts(scan: &NodeScanState) -> Vec<String> {
    scan.interrupts
        .iter()
        .filter(|id| !scan.resolved.contains_key(*id))
        .cloned()
        .collect()
}

/// Port of Go rerunsOnResume: whether the node opted into re-entry mode.
fn reruns_on_resume(node: Option<&Arc<dyn Node>>) -> bool {
    node.is_some_and(|n| n.config().rerun_on_resume == Some(true))
}

/// Port of Go validateResolved: validates each surviving (last-wins) response against its
/// declared schema (ids in sorted order, so the first error is deterministic).
fn validate_resolved(scan: &NodeScanState) -> Result<Map<String, Value>, WorkflowError> {
    let mut ids: Vec<&String> = scan.resolved.keys().collect();
    ids.sort();
    let mut resumed = Map::new();
    for id in ids {
        let mut resp = scan.resolved[id].clone();
        if let Some(sc) = scan.schemas.get(id) {
            resp = validate_resume_response(resp, sc).map_err(|e| {
                WorkflowError::new(
                    WorkflowErrorKind::InvalidResumeResponse,
                    format!(
                        "{}: interrupt {}: {e}",
                        WorkflowError::sentinel_text(WorkflowErrorKind::InvalidResumeResponse),
                        go_quote(id)
                    ),
                )
            })?;
        }
        resumed.insert(id.clone(), resp);
    }
    Ok(resumed)
}

/// Port of Go (*Workflow).inferNodeState. Status priority: unresolved + re-entry + some
/// resolved → Pending (partial resume); unresolved → Waiting; failed → Failed; finished →
/// Completed; all resolved + re-entry → Pending; all resolved, handoff → Completed with the
/// response as output.
fn infer_node_state(
    wf: &Workflow,
    node: Option<&Arc<dyn Node>>,
    scan: &NodeScanState,
    node_outputs: &HashMap<String, Value>,
    workflow_input: Option<&Value>,
) -> Result<NodeState, WorkflowError> {
    let unresolved = unresolved_interrupts(scan);
    let reenter = reruns_on_resume(node);
    let resumed = validate_resolved(scan)?;
    let mut ns = NodeState {
        branch: scan.branch.clone(),
        interrupt_schemas: scan.schemas.clone(),
        ..NodeState::default()
    };
    if !unresolved.is_empty() && reenter && !resumed.is_empty() {
        ns.status = NodeStatus::Pending;
        ns.resumed_inputs = Some(resumed);
        ns.interrupts = unresolved;
        (ns.input, ns.triggered_by) = predecessor_input(wf, node, node_outputs, workflow_input);
    } else if !unresolved.is_empty() {
        ns.status = NodeStatus::Waiting;
        ns.interrupts = unresolved;
        if !resumed.is_empty() {
            ns.resumed_inputs = Some(resumed);
        }
    } else if scan.failed {
        ns.status = NodeStatus::Failed;
        ns.resumed_inputs = Some(resumed);
        (ns.input, ns.triggered_by) = predecessor_input(wf, node, node_outputs, workflow_input);
    } else if scan.finished {
        // A re-entry node may have finished on an earlier resume turn.
        ns.status = NodeStatus::Completed;
        ns.output = node
            .and_then(|n| node_outputs.get(n.name()))
            .filter(|v| !v.is_null())
            .cloned();
    } else if reenter {
        ns.status = NodeStatus::Pending;
        ns.resumed_inputs = Some(resumed);
        (ns.input, ns.triggered_by) = predecessor_input(wf, node, node_outputs, workflow_input);
    } else {
        // Handoff: the response is the node's output, forwarded to successors by resume. A
        // response seen for the first time this turn (count == 1) marks a genuine first resume.
        ns.status = NodeStatus::Completed;
        ns.answered_this_turn = resumed
            .keys()
            .any(|id| scan.resolved_count.get(id).copied() == Some(1));
        ns.output = Some(resume_output(resumed.clone())).filter(|v| !v.is_null());
        ns.resumed_inputs = Some(resumed);
    }
    Ok(ns)
}

/// Port of Go (*Workflow).predecessorInput: a resuming node's input — a non-START
/// predecessor's cached output, else the workflow seed input for a START successor.
fn predecessor_input(
    wf: &Workflow,
    node: Option<&Arc<dyn Node>>,
    node_outputs: &HashMap<String, Value>,
    workflow_input: Option<&Value>,
) -> (Option<Value>, String) {
    let Some(node) = node else {
        return (None, String::new());
    };
    let incoming = wf.graph.predecessors_of(node);
    let Some(first) = incoming.first() else {
        return (None, String::new());
    };
    let non_null = |v: Option<&Value>| v.filter(|v| !v.is_null()).cloned();
    for e in incoming {
        let from = e.from.name();
        if from != START.name()
            && let Some(out) = node_outputs.get(from)
        {
            return (non_null(Some(out)), from.to_string());
        }
    }
    if incoming.iter().any(|e| e.from.name() == START.name()) {
        return (non_null(workflow_input), START.name().to_string());
    }
    let from = first.from.name();
    (non_null(node_outputs.get(from)), from.to_string())
}

/// Port of Go firstUserInput: the text of the first user event (skipping FunctionResponse
/// resume turns) in scope; `None` without one.
pub(crate) fn first_user_input(events: &[Event], invocation_id: &str) -> Option<Value> {
    for ev in events {
        if ev.author != "user" {
            continue;
        }
        let Some(content) = ev.response.content.as_ref() else {
            continue;
        };
        if !invocation_id.is_empty() && ev.invocation_id != invocation_id {
            continue;
        }
        if content.parts.iter().any(|p| p.function_response.is_some()) {
            continue;
        }
        let text: String = content.parts.iter().map(|p| p.text.as_str()).collect();
        if !text.is_empty() {
            return Some(Value::String(text));
        }
    }
    None
}

/// Port of Go frPart: the part's FunctionResponse when present and keyed.
fn fr_part(p: &Part) -> Option<&FunctionResponse> {
    p.function_response.as_ref().filter(|fr| !fr.id.is_empty())
}

/// Port of Go schemaFromEvent: the response schema for interrupt `id` from the pause event
/// (its RequestedInput, else the `adk_request_input` FunctionCall's `responseSchema` arg). Go
/// accepts only an in-memory `*jsonschema.Schema` arg; Rust events carry JSON, so a JSON object
/// is accepted.
pub(crate) fn schema_from_event(ev: &Event, id: &str) -> Option<Value> {
    if let Some(ri) = ev
        .requested_input
        .as_ref()
        .filter(|ri| ri.interrupt_id == id)
    {
        return ri.response_schema.clone();
    }
    let content = ev.response.content.as_ref()?;
    content
        .parts
        .iter()
        .filter_map(|p| p.function_call.as_ref())
        .filter(|fc| fc.name == WORKFLOW_INPUT_FUNCTION_CALL_NAME && fc.id == id)
        .find_map(|fc| {
            fc.args
                .get("responseSchema")
                .filter(|v| v.is_object())
                .cloned()
        })
}

/// Port of Go unwrapResponse: a sole single-key `result`/`response`/`payload` wrapper is
/// unwrapped (string values JSON-parsed when possible); anything else passes through.
pub(crate) fn unwrap_response(data: &Map<String, Value>) -> Value {
    if data.len() != 1 {
        return Value::Object(data.clone());
    }
    for key in ["result", "response", "payload"] {
        let Some(v) = data.get(key) else { continue };
        if let Value::String(s) = v {
            return serde_json::from_str::<Value>(s).unwrap_or_else(|_| Value::String(s.clone()));
        }
        return v.clone();
    }
    Value::Object(data.clone())
}
