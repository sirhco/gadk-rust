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

//! Port of Go workflow/scheduler_branch_test.go.

use std::collections::HashMap;
use std::sync::Arc;

use gadk_agent::Context;
use parking_lot::Mutex;
use serde_json::Value;

use crate::workflow::scheduler::test_support::*;
use crate::workflow::{JoinNode, Node, NodeConfig, function_node};

type SeenMap = Arc<Mutex<HashMap<String, String>>>;

/// Port of Go branchRecorder.
fn branch_recorder(name: &str, seen: &SeenMap) -> Arc<dyn Node> {
    let (s, n) = (Arc::clone(seen), name.to_string());
    function_node::<Value, String, _, _>(
        name,
        move |ctx: Context, _i: Value| {
            s.lock().insert(n.clone(), ctx.branch());
            let out = format!("{n}-done");
            async move { Ok(out) }
        },
        NodeConfig::default(),
    )
}

fn seen_of(seen: &SeenMap, name: &str) -> Option<String> {
    seen.lock().get(name).cloned()
}

#[tokio::test]
async fn test_scheduler_single_successor_chain_inherits_root_branch() {
    let seen: SeenMap = Arc::default();
    let (a, b, c) = (
        branch_recorder("a", &seen),
        branch_recorder("b", &seen),
        branch_recorder("c", &seen),
    );
    let w = must_new(chain(&[&start(), &a, &b, &c]));
    drain(w.run(seeded_mock_ctx())).await;
    for name in ["a", "b", "c"] {
        assert_eq!(
            seen_of(&seen, name),
            Some(String::new()),
            "seen[{name}] (chain inherits root)"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_scheduler_multiple_successors_assigns_sub_branches() {
    let seen: SeenMap = Arc::default();
    let (a, b, c) = (
        branch_recorder("a", &seen),
        branch_recorder("b", &seen),
        branch_recorder("c", &seen),
    );
    let join: Arc<dyn Node> = JoinNode::new("join");
    let w = must_new(vec![
        edge(&start(), &a),
        edge(&start(), &b),
        edge(&start(), &c),
        edge(&a, &join),
        edge(&b, &join),
        edge(&c, &join),
    ]);
    drain(w.run(seeded_mock_ctx())).await;
    for (name, want) in [("a", "a@1"), ("b", "b@1"), ("c", "c@1")] {
        assert_eq!(
            seen_of(&seen, name).as_deref(),
            Some(want),
            "seen[{name}] (fan-out should derive sub-branch)"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_scheduler_fan_out_chained_deeper_branches() {
    let seen: SeenMap = Arc::default();
    let a = branch_recorder("a", &seen);
    let b1 = branch_recorder("b1", &seen);
    let b2 = branch_recorder("b2", &seen);
    let join: Arc<dyn Node> = JoinNode::new("join");
    let w = must_new(vec![
        edge(&start(), &a), // a stays on root (single successor)
        edge(&a, &b1),      // a fans out: b1 and b2 get sub-branches
        edge(&a, &b2),
        edge(&b1, &join),
        edge(&b2, &join),
    ]);
    drain(w.run(seeded_mock_ctx())).await;
    assert_eq!(
        seen_of(&seen, "a").as_deref(),
        Some(""),
        "seen[a] (root, since START's only successor is a)"
    );
    assert_eq!(seen_of(&seen, "b1").as_deref(), Some("b1@1"), "seen[b1]");
    assert_eq!(seen_of(&seen, "b2").as_deref(), Some("b2@1"), "seen[b2]");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_scheduler_join_node_uses_common_prefix() {
    let seen: SeenMap = Arc::default();
    let branch_a = branch_recorder("branchA", &seen);
    let branch_b = branch_recorder("branchB", &seen);
    let join: Arc<dyn Node> = JoinNode::new("join");
    let handler = branch_recorder("handler", &seen);
    let w = must_new(vec![
        edge(&start(), &branch_a),
        edge(&start(), &branch_b),
        edge(&branch_a, &join),
        edge(&branch_b, &join),
        edge(&join, &handler),
    ]);
    drain(w.run(seeded_mock_ctx())).await;
    assert_eq!(
        seen_of(&seen, "branchA").as_deref(),
        Some("branchA@1"),
        "seen[branchA]"
    );
    assert_eq!(
        seen_of(&seen, "branchB").as_deref(),
        Some("branchB@1"),
        "seen[branchB]"
    );
    assert_eq!(
        seen_of(&seen, "handler").as_deref(),
        Some(""),
        "seen[handler] (common prefix of branchA@1/branchB@1 is root)"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_scheduler_join_node_nested_fan_out() {
    let seen: SeenMap = Arc::default();
    let outer = branch_recorder("outer", &seen);
    let leaf1 = branch_recorder("leaf1", &seen);
    let leaf2 = branch_recorder("leaf2", &seen);
    let join: Arc<dyn Node> = JoinNode::new("join");
    let handler = branch_recorder("handler", &seen);
    let w = must_new(vec![
        edge(&start(), &outer),
        edge(&outer, &leaf1),
        edge(&outer, &leaf2),
        edge(&leaf1, &join),
        edge(&leaf2, &join),
        edge(&join, &handler),
    ]);
    drain(w.run(seeded_mock_ctx())).await;
    assert_eq!(seen_of(&seen, "outer").as_deref(), Some(""), "seen[outer]");
    assert_eq!(
        seen_of(&seen, "leaf1").as_deref(),
        Some("leaf1@1"),
        "seen[leaf1]"
    );
    assert_eq!(
        seen_of(&seen, "leaf2").as_deref(),
        Some("leaf2@1"),
        "seen[leaf2]"
    );
    assert_eq!(
        seen_of(&seen, "handler").as_deref(),
        Some(""),
        "seen[handler]"
    );
}

// parity: adapted: Go groups events by Author, which function-node events leave empty (so its loop checks nothing); Rust groups by the stamped node_info.path ("a@1"/"b@1") and requires both nodes' events to carry their sub-branch.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_scheduler_events_are_branch_stamped() {
    let seen: SeenMap = Arc::default();
    let (a, b) = (branch_recorder("a", &seen), branch_recorder("b", &seen));
    let join: Arc<dyn Node> = JoinNode::new("join");
    let w = must_new(vec![
        edge(&start(), &a),
        edge(&start(), &b),
        edge(&a, &join),
        edge(&b, &join),
    ]);
    let events = drain(w.run(seeded_mock_ctx())).await;
    let mut got_by_node: HashMap<String, Vec<String>> = HashMap::new();
    for ev in &events {
        let Some(info) = ev.node_info.as_ref() else {
            continue;
        };
        got_by_node
            .entry(info.path.clone())
            .or_default()
            .push(ev.branch.clone());
    }
    for node in ["a", "b"] {
        let path = format!("{node}@1");
        let branches = got_by_node
            .get(&path)
            .unwrap_or_else(|| panic!("no events from {node:?}"));
        for got in branches {
            assert_eq!(got, &path, "event from {node:?} has Branch");
        }
    }
}
