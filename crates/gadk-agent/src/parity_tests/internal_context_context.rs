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

use gadk_platform::Platform;
use tokio_util::sync::CancellationToken;

use crate::context::{ContextDelta, InvocationContext, ReadonlyContext};

// parity: skipped TestInvocationContext_LiveSessionResumptionHandle: live sessions are out of scope for 1b (1b-live)

// parity: adapted: Go asserts by type assertion that a ReadonlyContext is not an InvocationContext; in Rust they are distinct types, so assert as_readonly() returns a ReadonlyContext with the same ids
#[test]
fn test_readonly_context() {
    let inv = InvocationContext::builder().branch("b").build_invocation();
    let r: ReadonlyContext = inv.as_readonly().clone();
    assert_eq!(
        (
            r.invocation_id(),
            r.agent_name(),
            r.branch(),
            r.user_id(),
            r.app_name(),
            r.session_id()
        ),
        (
            inv.invocation_id(),
            inv.agent_name(),
            inv.branch(),
            inv.user_id(),
            inv.app_name(),
            inv.session_id()
        )
    );
}

// parity: adapted: a callback context (Context with actions) derefs to ReadonlyContext
#[test]
fn test_callback_context() {
    let inv = InvocationContext::builder().build_invocation();
    let cc = inv.clone().into_context().with_delta(ContextDelta {
        actions: Some(Default::default()),
        ..Default::default()
    });
    let r: &ReadonlyContext = &cc;
    assert_eq!(r.invocation_id(), inv.invocation_id());
}

// parity: adapted: Go WithICDelta{Context} swaps the embedded context.Context; Rust swaps the cancellation token and nothing else changes
#[test]
fn test_with_context() {
    let inv = InvocationContext::builder()
        .branch("test-branch")
        .build_invocation();
    let token = CancellationToken::new();
    let got = inv.with_delta(ContextDelta {
        cancellation_token: Some(token.clone()),
        ..Default::default()
    });
    token.cancel();
    assert!(got.cancellation_token().is_cancelled());
    assert!(!inv.cancellation_token().is_cancelled());
    assert_eq!(
        (
            got.branch(),
            got.invocation_id(),
            got.agent_name(),
            got.session_id(),
            got.isolation_scope()
        ),
        (
            inv.branch(),
            inv.invocation_id(),
            inv.agent_name(),
            inv.session_id(),
            inv.isolation_scope()
        )
    );
}

#[test]
fn test_new_invocation_context_generates_id_with_provider() {
    let inv = InvocationContext::builder()
        .platform(Platform::default().with_uuid(|| "fixed".to_string()))
        .build_invocation();
    assert_eq!(inv.invocation_id(), "e-fixed");
}

#[test]
fn test_new_invocation_context_respects_explicit_id() {
    let inv = InvocationContext::builder()
        .platform(Platform::default().with_uuid(|| "fixed".to_string()))
        .invocation_id("explicit")
        .build_invocation();
    assert_eq!(inv.invocation_id(), "explicit");
}

#[test]
fn test_new_invocation_context_default_id() {
    let inv = InvocationContext::builder().build_invocation();
    let id = inv.invocation_id();
    let rest = id
        .strip_prefix("e-")
        .unwrap_or_else(|| panic!("InvocationID() = {id:?}, want \"e-\" prefix"));
    // Go uuid.Parse of the canonical 8-4-4-4-12 form; gadk-agent has no uuid dependency.
    let canonical = rest.len() == 36
        && rest.char_indices().all(|(i, c)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                c == '-'
            } else {
                c.is_ascii_hexdigit()
            }
        });
    assert!(
        canonical,
        "InvocationID() = {id:?}, suffix not a valid UUID"
    );
}
