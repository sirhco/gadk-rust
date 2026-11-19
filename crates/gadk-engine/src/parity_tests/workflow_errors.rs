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

//! Port of Go workflow/errors_test.go.

use std::error::Error as _;
use std::sync::Arc;

use gadk_agent::AgentError;

use crate::error::{NodeRunError, WorkflowError, WorkflowErrorKind as K};

fn sentinel(kind: K) -> Option<Arc<AgentError>> {
    Some(Arc::new(WorkflowError::sentinel(kind).into()))
}

/// Go `&NodeRunError{ChildName, ChildPath, Cause}`.
fn nre(child_name: &str, child_path: &str, cause: Option<Arc<AgentError>>) -> WorkflowError {
    WorkflowError::node_run(NodeRunError {
        child_name: child_name.into(),
        child_path: child_path.into(),
        run_id: String::new(),
        cause,
    })
}

#[test]
fn test_sentinels_have_workflow_prefix() {
    for kind in [
        K::NodeFailed,
        K::NodeInterrupted,
        K::InvalidRunNodeContext,
        K::InvalidRunId,
        K::ParallelHitlUnsupported,
    ] {
        let text = WorkflowError::sentinel_text(kind);
        assert!(
            text.starts_with("workflow:"),
            "{kind:?} sentinel text {text:?}; want prefix \"workflow:\""
        );
    }
}

// parity: adapted: Go Unwrap() ports as WorkflowError::cause() / Error::source(); the "nil receiver" subtest is Go-nil-only
#[test]
fn test_node_run_error_unwrap() {
    // returns Cause
    let e = nre("", "", sentinel(K::NodeFailed));
    assert_eq!(
        e.cause().expect("cause").to_string(),
        WorkflowError::sentinel_text(K::NodeFailed)
    );
    assert_eq!(
        e.source().expect("source").to_string(),
        "workflow: dynamic child failed"
    );
    assert!(e.is(K::NodeFailed));
    assert!(e.node_run_error().is_some());
}

// parity: adapted: the "nil receiver" subtest is Go-nil-only; "nil cause" is `cause: None`
#[test]
fn test_node_run_error_error() {
    // includes ChildPath and cause
    let s = nre("", "code_workflow/fixer@2", sentinel(K::NodeFailed)).to_string();
    assert!(
        s.contains("code_workflow/fixer@2"),
        "Error() = {s:?}, want to contain ChildPath"
    );
    assert!(
        s.contains(WorkflowError::sentinel_text(K::NodeFailed)),
        "Error() = {s:?}, want to contain cause message"
    );
    // falls back to ChildName when ChildPath empty
    let s = nre("fixer", "", sentinel(K::NodeInterrupted)).to_string();
    assert!(
        s.contains("fixer"),
        "Error() = {s:?}, want to contain ChildName when ChildPath unset"
    );
    // nil cause does not panic
    let s = nre("", "x/y@1", None).to_string();
    assert_eq!(s, "workflow: dynamic child x/y@1: <no cause>");
}
