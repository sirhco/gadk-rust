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

use crate::context::builder::test_support::{named_agent, session};
use crate::context::{ContextDelta, InvocationContext};

// parity: skipped TestCommonContextWithoutInvocation: a Rust Context always carries its invocation; the nil-invocation shape cannot be built
// parity: skipped TestReadIdentityRecoversPanickingAccessor: Go recovers panics inside context.Value for http.RoundTripper; Rust has no context.Value identity lookup
// parity: skipped TestIdentityFromPermissiveInvocation: Go context.Value key mechanics (a Value answering every key); no Rust counterpart
// parity: skipped TestIdentityFromDecoratedInvocation: out-of-module InvocationContext decorators cannot exist; Rust contexts are a closed concrete type
// parity: skipped TestIdentityFromBrokenWrapper: hand-built wrapper with a nil inner context; not constructible in Rust
// parity: skipped TestIdentityThroughWrapperDoesNotLogPerLookup: Go callback/tool wrapper logging; Rust has no wrappers
// parity: skipped TestIdentityFromNilContexts: Go typed-nil receivers
// parity: skipped TestWrapperValueFailsClosed: Go wrapper Value() on hand-built receivers
// parity: skipped TestIdentityFromContextContainsAPanickingIntermediary: Go context.Context intermediaries; no Rust counterpart
// parity: skipped TestEmptyDeltaCostsASessionlessForwarderItsUnwrap: out-of-module session-less forwarders cannot exist in Rust

// Port of Go TestIdentityFromUserlessSession: a readable session with no user still reports app and session id,
// through the invocation, its promotion, a tool context and a callback context.
#[test]
fn test_identity_from_userless_session() {
    let ic = InvocationContext::builder()
        .session(session("app", "", "sid"))
        .build_invocation();
    let promoted = ic.clone().into_context();
    let cases = [
        ("the invocation itself", ic.clone()),
        ("Promote", promoted.as_invocation().clone()),
        (
            "NewToolContext",
            promoted
                .with_delta(ContextDelta {
                    function_call_id: Some("fc".into()),
                    ..Default::default()
                })
                .as_invocation()
                .clone(),
        ),
        (
            "NewCallbackContext",
            promoted
                .with_delta(ContextDelta {
                    actions: Some(Default::default()),
                    ..Default::default()
                })
                .as_invocation()
                .clone(),
        ),
    ];
    for (name, c) in cases {
        assert_eq!(
            (c.user_id(), c.app_name(), c.session_id()),
            ("", "app", "sid"),
            "{name}"
        );
    }
}

// parity: adapted: identity is the session's own ids read through the context; there is no enclosing invocation to fall back to
#[test]
fn test_invocation_context_identity_is_owned() {
    let ic = InvocationContext::builder()
        .session(session("app-a", "user-a", "sess-a"))
        .build_invocation();
    let child = ic.with_delta(ContextDelta {
        agent: Some(named_agent("child")),
        ..Default::default()
    });
    for c in [&ic, &child] {
        assert_eq!(
            (c.user_id(), c.app_name(), c.session_id()),
            ("user-a", "app-a", "sess-a")
        );
    }
}

// parity: adapted: nested tool-call deltas stand in for Go's session-less tool wrappers; identity still comes from the invocation's session
#[test]
fn test_identity_through_nested_sessionless_contexts() {
    let ic = InvocationContext::builder()
        .session(session("app", "u", "s"))
        .build_invocation();
    let nested = ic
        .into_context()
        .with_delta(ContextDelta {
            function_call_id: Some("fc-1".into()),
            ..Default::default()
        })
        .with_delta(ContextDelta {
            function_call_id: Some("fc-2".into()),
            ..Default::default()
        });
    assert_eq!(
        (nested.user_id(), nested.app_name(), nested.session_id()),
        ("u", "app", "s")
    );
}

// parity: adapted: ContextDelta::default() on a tool-call context leaves agent and identity readable
#[test]
fn test_nil_delta_keeps_the_context_usable() {
    let ic = InvocationContext::builder()
        .agent(named_agent("parent"))
        .session(session("app", "u", "s"))
        .build_invocation();
    let tool_ctx = ic.into_context().with_delta(ContextDelta {
        function_call_id: Some("fc".into()),
        ..Default::default()
    });
    let c = tool_ctx.with_delta(ContextDelta::default());
    assert_eq!(c.agent_name(), "parent");
    assert_eq!(c.user_id(), "u");
}
