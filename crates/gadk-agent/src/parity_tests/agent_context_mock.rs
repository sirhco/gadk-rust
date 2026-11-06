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

use crate::context::Context;
use crate::context::builder::test_support::session;

// parity: skipped TestStrictContextMock_NilCtxPanics: Go nil embedded context.Context; a Rust builder always yields a complete Context

// parity: adapted: Go's StrictContextMock delegates Value to a wrapped ctx; ContextBuilder is the Rust test double and must report exactly what it was given
#[test]
fn test_strict_context_mock_value_delegates_to_ctx() {
    let ctx = Context::builder()
        .session(session("app", "user", "sess-1"))
        .invocation_id("inv-1")
        .branch("b")
        .build();
    assert_eq!(
        (
            ctx.app_name(),
            ctx.user_id(),
            ctx.session_id(),
            ctx.invocation_id(),
            ctx.branch()
        ),
        ("app", "user", "sess-1", "inv-1", "b".to_string())
    );
}

// parity: adapted: Go's strict mock panics on unimplemented methods; the Rust builder instead supplies documented defaults
#[test]
fn test_strict_context_mock_adk_method_panics() {
    let ctx = Context::builder().build();
    assert_eq!(ctx.agent_name(), "test_agent");
    assert!(
        ctx.invocation_id().starts_with("e-"),
        "default id {:?}",
        ctx.invocation_id()
    );
    assert_eq!(
        (ctx.app_name(), ctx.user_id(), ctx.session_id()),
        ("app", "user", "session")
    );
    assert_eq!(ctx.function_call_id(), "");
    assert!(ctx.tool_confirmation().is_none() && ctx.sub_scheduler().is_none() && !ctx.ended());
}
