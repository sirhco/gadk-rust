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

use crate::context::builder::test_support::named_agent;
use crate::context::{Context, ContextDelta};

fn callback_ctx() -> Context {
    Context::builder()
        .agent(named_agent("test-agent"))
        .invocation_id("inv-1")
        .build()
        .with_delta(ContextDelta {
            actions: Some(Default::default()),
            ..Default::default()
        })
}

/// Awaits `ctx`'s token on a paused clock and checks it fired after 5 s and not before.
async fn assert_fires_after_five_seconds(parent: &Context, ctx: &Context) {
    let start = tokio::time::Instant::now();
    ctx.cancellation_token().cancelled().await;
    let waited = start.elapsed();
    assert!(
        waited >= Duration::from_secs(5) && waited < Duration::from_secs(6),
        "waited {waited:?}"
    );
    assert!(
        !parent.cancellation_token().is_cancelled(),
        "the timeout canceled the parent context"
    );
}

// parity: adapted: Go CancelFunc/ctx.Err() become CancelGuard::cancel()/CancellationToken::is_cancelled()
#[tokio::test]
async fn test_with_agent_cancel_on_callback_context() {
    let parent = callback_ctx();
    let (ctx, guard) = parent.with_agent_cancel();
    assert!(
        !ctx.cancellation_token().is_cancelled(),
        "derived context was canceled before cancel was called"
    );
    assert_eq!(
        (ctx.agent_name(), ctx.invocation_id()),
        (parent.agent_name(), parent.invocation_id())
    );
    guard.cancel();
    assert!(
        ctx.cancellation_token().is_cancelled(),
        "cancel did not close the derived context"
    );
    assert!(
        !parent.cancellation_token().is_cancelled(),
        "canceling the derived context canceled its parent"
    );
}

// parity: adapted: no Deadline() accessor; with a paused clock the derived token must fire after 5 s and not before
#[tokio::test(start_paused = true)]
async fn test_with_agent_timeout_on_tool_context() {
    let tool_ctx = Context::builder()
        .agent(named_agent("test-agent"))
        .build()
        .with_delta(ContextDelta {
            function_call_id: Some("call-1".into()),
            ..Default::default()
        });
    let (ctx, _guard) = tool_ctx.with_agent_timeout(Duration::from_secs(5));
    assert_fires_after_five_seconds(&tool_ctx, &ctx).await;
}

// parity: adapted: no Deadline() accessor; with a paused clock the derived token must fire after 5 s and not before
#[tokio::test(start_paused = true)]
async fn test_with_agent_timeout_on_callback_context() {
    let parent = callback_ctx();
    let (ctx, _guard) = parent.with_agent_timeout(Duration::from_secs(5));
    assert_eq!(
        (ctx.agent_name(), ctx.invocation_id()),
        (parent.agent_name(), parent.invocation_id())
    );
    assert_fires_after_five_seconds(&parent, &ctx).await;
}
