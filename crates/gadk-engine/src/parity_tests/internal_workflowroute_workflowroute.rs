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

//! Port of Go internal/workflowroute/workflowroute_test.go.

use std::sync::Arc;

use crate::error::{WorkflowError, WorkflowErrorKind};
use crate::workflow::node::testing::{TestNode, test_node};
use crate::workflow::route::{
    OutgoingSummary, RouteKind, RouteValue, register_route_validator, validate_outgoing,
};
use crate::workflow::{BaseNode, Node, NodeConfig};

#[test]
fn test_register_and_validate() {
    // Go's local `targetNode` type: one node, matched by pointer identity, so this validator cannot
    // affect any other test sharing the global registry (as Go's private type cannot).
    let target: Arc<dyn Node> = Arc::new(TestNode {
        base: BaseNode::new("target", "", NodeConfig::default()),
    });
    let target_ptr = Arc::as_ptr(&target).cast::<()>() as usize;
    register_route_validator(Arc::new(move |node: &dyn Node, out: &OutgoingSummary| {
        let is_target = (node as *const dyn Node).cast::<()>() as usize == target_ptr;
        if is_target && !out.routes.is_empty() {
            return Err(WorkflowError::new(
                WorkflowErrorKind::Other,
                "bad outgoing route",
            ));
        }
        Ok(())
    }));

    let one_route = || OutgoingSummary {
        routes: vec![RouteValue {
            kind: RouteKind::String,
            label: "\"a\"".into(),
        }],
        ..Default::default()
    };
    let other = test_node("other-node");
    if let Err(e) = validate_outgoing(other.as_ref(), &one_route()) {
        panic!("Validate(other-node) unexpected error: {e}");
    }
    if let Err(e) = validate_outgoing(target.as_ref(), &OutgoingSummary::default()) {
        panic!("Validate(empty summary) unexpected error: {e}");
    }
    // Go errors.Is(err, sentinel): the sentinel is identified by its text.
    let err = validate_outgoing(target.as_ref(), &one_route())
        .expect_err("Validate(targetNode) want error");
    assert_eq!(err.to_string(), "bad outgoing route");
}
