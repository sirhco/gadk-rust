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

//! Recovery evidence travels beside the business error. Contexts here have no value bag, so the
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

/// Which child paths a failed activation could not vouch for.
/// `known == false` means the inventory is unknown (no producer published one).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FailureInventory {
    pub(crate) known: bool,
    pub(crate) paths: Vec<String>,
}

/// Collects the failure inventory of one activation. Only the activation's actual producer may
/// publish; a second publication makes the inventory ambiguous (reset to unknown), not last-wins.
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
    /// A copy of the inventory published so far.
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

/// The producer of a node's inventory: a WorkflowNode's producer is its sub-workflow's graph; any
/// other node is itself.
pub(crate) fn node_producer_key(node: &Arc<dyn Node>) -> usize {
    match node.as_any().downcast_ref::<WorkflowNode>() {
        Some(w) => graph_key(&w.sub_workflow.graph),
        None => node_key(node),
    }
}

/// A recorder for the activation of `node` (see [`scope`] for making it current).
pub(crate) fn with_failure_inventory(node: &Arc<dyn Node>) -> Arc<FailureInventoryRecorder> {
    Arc::new(FailureInventoryRecorder {
        producer: node_producer_key(node),
        state: Mutex::default(),
    })
}

/// Runs `f` with `recorder` as the current activation's recorder.
pub(crate) async fn scope<F: Future>(recorder: Arc<FailureInventoryRecorder>, f: F) -> F::Output {
    FAILURE_INVENTORY.scope(recorder, f).await
}

/// Publishes an inventory to the current recorder: a no-op outside a scope or for another
/// producer.
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

/// Writes the failed child paths into the metadata: a known inventory is written as a
/// de-duplicated array (an empty array is meaningful); an unknown one writes nothing.
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
