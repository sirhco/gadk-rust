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

//! Port of Go workflow/state_test.go.

use serde_json::json;

use crate::workflow::{NodeStatus, RunState};

// parity: adapted: Go compares *NodeState pointers; Rust checks that writes through the returned &mut are visible in nodes. "allocates Nodes map on demand" holds by construction (RunState::default has an empty map)
#[test]
fn test_run_state_ensure_node() {
    // creates inactive node when absent
    let mut s = RunState::new();
    assert_eq!(s.ensure_node("A").status, NodeStatus::Inactive);
    assert!(s.nodes.contains_key("A"));

    // returns existing node without resetting it
    let mut s = RunState::new();
    {
        let first = s.ensure_node("A");
        first.status = NodeStatus::Running;
        first.input = Some(json!("in"));
        first.triggered_by = "X".into();
    }
    let second = s.ensure_node("A");
    assert_eq!(second.status, NodeStatus::Running);
    assert_eq!(second.input, Some(json!("in")));
    assert_eq!(second.triggered_by, "X");
    assert_eq!(s.nodes.len(), 1);

    // allocates on demand from the zero value
    let mut s = RunState::default();
    s.ensure_node("A");
    assert_eq!(s.nodes.len(), 1);
}
