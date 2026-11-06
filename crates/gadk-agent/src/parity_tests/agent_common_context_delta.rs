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

use std::sync::Arc;

use crate::context::builder::test_support::{StubSubScheduler, named_agent};
use crate::context::{ContextDelta, InvocationContext};

// parity: skipped TestDeltaOnInvocationThatReturnsNil: an out-of-module WithICDelta returning nil cannot exist; Rust contexts are one concrete type with no user-implementable invocation
// parity: skipped TestDiscardReportNamesWhatWasLost: the discard report exists only for out-of-module WithICDelta implementations returning nil
// parity: skipped TestDiscardReportIsEmittedOnce: the discard report exists only for out-of-module WithICDelta implementations returning nil
// parity: skipped TestDiscardReportsEachDistinctLoss: the discard report exists only for out-of-module WithICDelta implementations returning nil
// parity: skipped TestDiscardRepeatDoesNotAllocate: the discard report exists only for out-of-module WithICDelta implementations returning nil
// parity: skipped TestDiscardWithNothingToReport: the discard report exists only for out-of-module WithICDelta implementations returning nil
// parity: skipped TestDiscardNamesContextOnBothEntryPoints: the discard report exists only for out-of-module WithICDelta implementations returning nil
// parity: skipped TestDiscardWithNoInvocationDelta: Go nil-InvocationContextDelta guard; ContextDelta fields are Options with no nil-pointer shape

// parity: adapted: Go's tool-context wrapper unwrap becomes a tool-call delta; a later path-only delta must keep the agent
#[test]
fn test_nil_delta_still_reaches_the_invocation() {
    let inner = InvocationContext::builder()
        .agent(named_agent("parent"))
        .build_invocation();
    let tool_ctx = inner.into_context().with_delta(ContextDelta {
        function_call_id: Some("fc-1".into()),
        ..Default::default()
    });
    let c = tool_ctx.with_delta(ContextDelta {
        path: Some("wf/n@1".into()),
        ..Default::default()
    });
    assert_eq!(c.agent().name(), "parent");
    assert_eq!(c.agent_name(), "parent");
}

// parity: adapted: only the ADK-invocation case exists in Rust (no out-of-module decorators); asserts the delta lands
#[test]
fn test_delta_reaches_the_invocation() {
    let ic = InvocationContext::builder()
        .agent(named_agent("parent"))
        .branch("parent-branch")
        .build_invocation();
    let c = ic.into_context().with_delta(ContextDelta {
        agent: Some(named_agent("child")),
        branch: Some("child-branch".into()),
        ..Default::default()
    });
    assert_eq!(c.agent().name(), "child");
    assert_eq!(c.branch(), "child-branch");
}

// parity: adapted: no discard path in Rust; asserts every CommonContextDelta field lands together
#[test]
fn test_discard_keeps_the_rest_of_the_delta() {
    let ic = InvocationContext::builder()
        .agent(named_agent("parent"))
        .build_invocation();
    let c = ic.into_context().with_delta(ContextDelta {
        branch: Some("child-branch".into()),
        run_id: Some("run-7".into()),
        path: Some("wf/n@1".into()),
        output_for_ancestors: Some(vec!["a".into(), "b".into()]),
        sub_scheduler: Some(Some(Arc::new(StubSubScheduler))),
        ..Default::default()
    });
    assert_eq!(c.run_id(), "run-7");
    assert!(c.sub_scheduler().is_some());
    assert_eq!(c.path(), "wf/n@1");
    assert_eq!(c.branch(), "child-branch");
    assert_eq!(
        c.output_for_ancestors(),
        vec!["a".to_string(), "b".to_string()]
    );
}
