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

//! `NodeStatus`, `NodeState` and `RunState` (port of Go workflow/state.go).

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value};

/// Port of Go workflow.NodeStatus: the lifecycle status of a node in the workflow graph.
///
/// The status is the engine's single source of truth for what a node is doing. It mediates
/// between the trigger buffer (what wants to run), the in-process task table (what is currently
/// running), and the persisted node history (what has run).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum NodeStatus {
    /// The node has not been touched yet. This is the zero value.
    #[default]
    Inactive,
    /// The node is ready to run. It may be queued because its input has arrived (consumed from
    /// the trigger buffer) or because it is being retried after a failure. The scheduler may keep
    /// a pending node from starting if the engine's max-concurrency cap is reached.
    Pending,
    /// The engine has started a task for this node. The task is in flight in the current
    /// process. A running entry that has no live task in the run state (e.g. after a process
    /// restart) must be re-scheduled.
    Running,
    /// The node finished and produced its output. This is a terminal status for normal
    /// execution, but a completed node may still be re-triggered by a fresh entry in the trigger
    /// buffer (this is what enables loops as graph cycles).
    Completed,
    /// The node is paused. Two cases share this status: human-in-the-loop (the node yielded a
    /// RequestInput and is blocked until a function-response payload resumes it), and fan-in
    /// (`wait_for_output`, e.g. JoinNode: the node ran but did not yet produce its final output
    /// because not all predecessors have triggered it). While any node is waiting the workflow
    /// does not finalize.
    Waiting,
    /// The node returned an error and the retry policy (if any) has been exhausted. Terminal.
    Failed,
    /// The node was cancelled: either because a sibling node failed and the engine cancelled all
    /// running tasks, or because the invocation was cancelled from outside the engine. Terminal in
    /// both cases — the two are told apart by whether the run reports an error, not by this status.
    Cancelled,
}

/// Port of Go workflow.NodeState: the per-node lifecycle record. A [`RunState`] holds one of
/// these for every node the engine has touched.
#[derive(Debug, Clone, Default)]
pub struct NodeState {
    /// The current lifecycle position.
    pub status: NodeStatus,
    /// The value most recently handed to the node's `run`. Set when the node is scheduled.
    pub input: Option<Value>,
    /// The value the node emitted via the event output. Set when status transitions to
    /// `Completed`.
    pub output: Option<Value>,
    /// The name of the upstream node that produced the current input. Empty for the initial
    /// START activation.
    pub triggered_by: String,
    /// The composite branch string assigned to this activation at scheduling time. Empty for
    /// nodes that inherit the root branch (single-successor chains); populated for nodes scheduled
    /// after a fan-out and for JoinNodes resolving the common branch prefix of their predecessors.
    /// Persisted so resume can reconstruct the same branch tree on re-entry.
    pub branch: String,
    /// The long-running tool call IDs the node is waiting on; lets resume match a human's
    /// FunctionResponse to the node. Non-empty for a node waiting on long-running interrupts, and
    /// also while status is `Pending` for a partially resumed re-entry node. A node parked by
    /// `wait_for_output` reaches `Waiting` with this field empty.
    pub interrupts: Vec<String>,
    /// The number of times this node has failed.
    pub attempt: i32,
    /// Accumulated response payloads for re-entry-mode nodes that yield RequestInput more than
    /// once during a single activation lifecycle, keyed by interrupt ID. Cleared when the node
    /// transitions to `Completed`; `None` (Go's nil map) for handoff-mode nodes.
    pub resumed_inputs: Option<Map<String, Value>>,
    /// Maps an interrupt ID to its declared response schema, re-extracted from the pause event
    /// during rehydration. Not persisted; consumed by resume to validate the payload.
    pub(crate) interrupt_schemas: HashMap<String, Value>,
    /// True when this node's interrupt was resolved by a user response that appeared in history
    /// for the first time on the current resume turn. Not persisted; rebuilt each turn.
    pub(crate) answered_this_turn: bool,
}

/// Port of Go workflow.RunState: the per-invocation lifecycle state for a workflow run. A resume
/// turn rebuilds it by scanning session event history.
#[derive(Debug, Clone, Default)]
pub struct RunState {
    /// The per-node lifecycle map. Absent entries are inactive.
    pub nodes: HashMap<String, NodeState>,
    /// Names of nodes that already produced an output in session history. Reconstructed from
    /// history and used by resume for idempotency. Not persisted.
    pub(crate) completed: HashSet<String>,
}

impl RunState {
    /// Port of Go NewRunState: an empty state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Port of Go RunState.EnsureNode: the `NodeState` for `name`, creating an inactive entry if
    /// none exists. The returned reference may be mutated in place.
    pub fn ensure_node(&mut self, name: &str) -> &mut NodeState {
        self.nodes.entry(name.to_owned()).or_default()
    }
}
