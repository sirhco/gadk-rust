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

use std::time::Duration;

use gadk_session::{CreateRequest, InMemorySessionService, SessionService};

use crate::context::builder::test_support::{TestSession, session};
use crate::context::{ContextDelta, InvocationContext};

// parity: skipped TestIdentityFromContextAbsent: Go plain context.Context without an ADK ancestor; Rust identity exists only on ADK contexts
// parity: skipped TestIdentityAfterWithContext: Go WithContext replacing the embedded parent context; no Rust counterpart
// parity: skipped TestNilGuardsAreDirectlyExercised: Go nil receivers on Value()
// parity: skipped TestValueWithNilEmbeddedContext: Go nil embedded context.Context
// parity: skipped TestIdentityThroughSessionlessWrappers: Go session-less tool/callback wrappers; Rust tool contexts hold the session (covered by agent_identity::test_identity_through_nested_sessionless_contexts)
// parity: skipped TestIdentityFromPanickingSession: Go recover() around a panicking Session() accessor in context.Value
// parity: skipped TestReadonlyContextDelegatesUnknownKeys: Go context.Value delegation for non-identity keys; Rust has no Value bag
// parity: skipped TestValueDelegatesUnknownKeys: Go context.Value delegation for non-identity keys; Rust has no Value bag
// parity: skipped TestIdentityDecisionMatrixCrossPackage: Go decorator/promotion identity matrix; Rust contexts are a closed concrete type

// parity: adapted: Go recovers identity through non-ADK context wrappers via context.Value; Rust reads it through every ADK derivation (delta, cancel, timeout, promote)
#[tokio::test]
async fn test_identity_from_context_recovers_identity() {
    let s = InMemorySessionService::new()
        .create(CreateRequest {
            app_name: "app-1".into(),
            user_id: "user-42".into(),
            session_id: None,
            state: None,
        })
        .await
        .unwrap();
    let session_id = s.id().to_string();
    let ic = InvocationContext::builder().session(s).build_invocation();
    let ctx = ic.clone().into_context();
    let (cancel_ctx, _g1) = ctx.with_agent_cancel();
    let (timeout_ctx, _g2) = ctx.with_agent_timeout(Duration::from_secs(60));
    let tool_ctx = ctx.with_delta(ContextDelta {
        function_call_id: Some("fc-1".into()),
        ..Default::default()
    });
    for c in [
        ic,
        ctx.as_invocation().clone(),
        cancel_ctx.as_invocation().clone(),
        timeout_ctx.as_invocation().clone(),
        tool_ctx.as_invocation().clone(),
    ] {
        assert_eq!(
            (c.user_id(), c.app_name(), c.session_id()),
            ("user-42", "app-1", session_id.as_str())
        );
    }
}

// parity: adapted: the Go pointer and struct-value shapes are both a Rust Session impl; the nil and typed-nil shapes cannot be built
#[test]
fn test_identity_from_context_session_shapes() {
    for (name, s) in [
        ("pointer", session("app-1", "user-42", "sid-1")),
        (
            "struct value",
            TestSession::shared("app-1", "user-42", "sid-1"),
        ),
    ] {
        let ic = InvocationContext::builder().session(s).build_invocation();
        let promoted = ic.clone().into_context();
        let tool = promoted.with_delta(ContextDelta {
            function_call_id: Some("fc-1".into()),
            ..Default::default()
        });
        for (ctx_name, c) in [
            ("invocation context", ic),
            ("promoted common context", promoted.as_invocation().clone()),
            ("tool context", tool.as_invocation().clone()),
        ] {
            assert_eq!(
                (c.user_id(), c.app_name(), c.session_id()),
                ("user-42", "app-1", "sid-1"),
                "{name}/{ctx_name}"
            );
        }
    }
}

// parity: adapted: a Rust context always owns a session, so a nested invocation reports its own session, never the enclosing one
#[test]
fn test_identity_does_not_inherit_enclosing_invocation() {
    let outer = InvocationContext::builder()
        .session(session("app-1", "alice", "s1"))
        .build_invocation();
    let inner = InvocationContext::builder()
        .session(session("app-1", "inner", "s2"))
        .cancellation_token(outer.cancellation_token().child_token())
        .build_invocation();
    let promoted = inner.clone().into_context();
    let tool = promoted.with_delta(ContextDelta {
        function_call_id: Some("fc-1".into()),
        ..Default::default()
    });
    for (name, c) in [
        ("inner", inner),
        ("promoted", promoted.as_invocation().clone()),
        ("tool context", tool.as_invocation().clone()),
    ] {
        assert_eq!(
            (c.user_id(), c.app_name(), c.session_id()),
            ("inner", "app-1", "s2"),
            "{name}"
        );
    }
    assert_eq!(outer.user_id(), "alice");
}
