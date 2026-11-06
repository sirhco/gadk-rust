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

//! Parity port of Go internal/llminternal/identity_test.go.

use std::sync::Arc;

use gadk_agent::{Context, ContextDelta, InvocationContext};
use gadk_session::{CreateRequest, EventActions, InMemorySessionService, SessionService};
use pretty_assertions::assert_eq;

fn identity(ctx: &Context) -> (String, String, String) {
    (
        ctx.app_name().to_owned(),
        ctx.user_id().to_owned(),
        ctx.session_id().to_owned(),
    )
}

// parity: adapted: gadk has no Go Identity/Promote marker or IdentityFromContext; contexts carry the session structurally, so this checks app name, user id and session id on the cancel-scoped tool context (with_agent_cancel) and on a tool context and a callback context re-derived from it (Go's "promoted" and log-capture checks have no counterpart)
#[tokio::test]
async fn test_cancelled_tool_context_keeps_identity() {
    let svc = InMemorySessionService::new();
    let sess = svc
        .create(CreateRequest {
            app_name: "app".into(),
            user_id: "u".into(),
            session_id: None,
            state: None,
        })
        .await
        .unwrap();
    let want = ("app".to_owned(), "u".to_owned(), sess.id().to_owned());

    // Go: agent.NewToolContext(ic, "fc", nil, nil), then WithAgentCancel.
    let tool_ctx = InvocationContext::builder()
        .session(sess.clone())
        .build_invocation()
        .into_context()
        .with_delta(ContextDelta {
            function_call_id: Some("fc".into()),
            ..Default::default()
        });
    let (cancel_ctx, guard) = tool_ctx.with_agent_cancel();

    let cases: Vec<(&str, Context)> = vec![
        ("the cancel-scoped context itself", cancel_ctx.clone()),
        (
            "a tool context re-derived from it",
            cancel_ctx.with_delta(ContextDelta {
                function_call_id: Some("fc2".into()),
                ..Default::default()
            }),
        ),
        (
            "a callback context re-derived from it",
            cancel_ctx.with_delta(ContextDelta {
                actions: Some(Arc::new(parking_lot::Mutex::new(EventActions::default()))),
                ..Default::default()
            }),
        ),
    ];
    for (name, ctx) in &cases {
        assert_eq!(identity(ctx), want, "{name}");
    }
    // Go "a readonly context over it".
    let ro = cancel_ctx.as_readonly();
    assert_eq!(
        (
            ro.app_name().to_owned(),
            ro.user_id().to_owned(),
            ro.session_id().to_owned()
        ),
        want,
        "a readonly context over it"
    );

    // The scope is a real child: cancelling it leaves the tool context's own token alone.
    guard.cancel();
    assert!(cancel_ctx.cancellation_token().is_cancelled());
    assert!(!tool_ctx.cancellation_token().is_cancelled());
}
