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

//! Port of Go workflow/failure_inventory.go. Recovery evidence travels beside the business
//! error. Go carries the recorder in a context value; contexts here have no value bag, so the
//! recorder is a tokio task-local scoped around the future that polls a node's stream.

use std::future::Future;
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::{Map, Value};

use crate::workflow::Node;
use crate::workflow::graph::Graph;
use crate::workflow::nodes::WorkflowNode;
use crate::workflow::persistence::WORKFLOW_FAILED_CHILD_PATHS_KEY;
use crate::workflow::scheduler::node_key;

/// Port of Go failureInventory: which child paths a failed activation could not vouch for.
/// `known == false` means the inventory is unknown (no producer published one).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FailureInventory {
    pub(crate) known: bool,
    pub(crate) paths: Vec<String>,
}

/// Port of Go failureInventoryRecorder. Only the activation's actual producer may publish;
/// a second publication makes the inventory ambiguous (reset to unknown), not last-wins.
pub(crate) struct FailureInventoryRecorder {
    producer: usize,
    state: Mutex<RecorderState>,
}

#[derive(Default)]
struct RecorderState {
    published: bool,
    inventory: FailureInventory,
}

impl FailureInventoryRecorder {
    /// Port of Go (*failureInventoryRecorder).snapshot (returns a deep copy).
    pub(crate) fn snapshot(&self) -> FailureInventory {
        self.state.lock().inventory.clone()
    }
}

tokio::task_local! {
    static FAILURE_INVENTORY: Arc<FailureInventoryRecorder>;
}

/// Producer identity of a graph (a nested workflow's scheduler publishes as its graph).
pub(crate) fn graph_key(g: &Arc<Graph>) -> usize {
    Arc::as_ptr(g) as usize
}

/// Port of the producer choice in Go withFailureInventory: a WorkflowNode's producer is
/// its sub-workflow's graph; any other node is itself.
pub(crate) fn node_producer_key(node: &Arc<dyn Node>) -> usize {
    match node.as_any().downcast_ref::<WorkflowNode>() {
        Some(w) => graph_key(&w.sub_workflow.graph),
        None => node_key(node),
    }
}

/// Port of Go withFailureInventory (the recorder half; [`scope`] is the context half).
pub(crate) fn with_failure_inventory(node: &Arc<dyn Node>) -> Arc<FailureInventoryRecorder> {
    Arc::new(FailureInventoryRecorder {
        producer: node_producer_key(node),
        state: Mutex::default(),
    })
}

/// Runs `f` with `recorder` as the current activation's recorder (Go: the derived context).
pub(crate) async fn scope<F: Future>(recorder: Arc<FailureInventoryRecorder>, f: F) -> F::Output {
    FAILURE_INVENTORY.scope(recorder, f).await
}

/// Port of Go publishFailureInventory: a no-op outside a scope or for another producer.
pub(crate) fn publish_failure_inventory(producer: usize, inventory: &FailureInventory) {
    let _ = FAILURE_INVENTORY.try_with(|r| {
        if r.producer != producer {
            return;
        }
        let mut st = r.state.lock();
        st.inventory = if st.published {
            FailureInventory::default()
        } else {
            inventory.clone()
        };
        st.published = true;
    });
}

/// Port of Go recordFailedChildPaths (dynamic_scheduler.go): a known inventory is written as
/// a de-duplicated array (an empty array is meaningful); an unknown one writes nothing.
pub(crate) fn record_failed_child_paths(
    metadata: &mut Map<String, Value>,
    inventory: &FailureInventory,
) {
    if !inventory.known {
        return;
    }
    let mut seen = std::collections::HashSet::new();
    let paths: Vec<Value> = inventory
        .paths
        .iter()
        .filter(|p| !p.is_empty() && seen.insert(p.as_str()))
        .map(|p| Value::String(p.clone()))
        .collect();
    metadata.insert(
        WORKFLOW_FAILED_CHILD_PATHS_KEY.to_string(),
        Value::Array(paths),
    );
}
