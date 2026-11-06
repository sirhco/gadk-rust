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

//! Port of Go workflow/persistence.go. Task 10 (this part): the metadata constants and the
//! history attribution used by the live schedulers. Task 14 appends ReconstructRunState and its
//! scan helpers below.

use std::collections::HashMap;
use std::sync::Arc;

use gadk_session::Event;
use serde_json::Value;

use crate::workflow::Node;
use crate::workflow::scheduler::child_event_output;

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
