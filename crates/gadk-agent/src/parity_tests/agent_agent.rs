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
use std::sync::atomic::{AtomicUsize, Ordering};

use futures::{FutureExt, StreamExt};
use gadk_genai::{Content, Part, ROLE_MODEL};
use gadk_model::LlmResponse;
use gadk_session::{Event, EventActions, zero_time};
use serde_json::Map;
use tokio_util::sync::CancellationToken;

use crate::agent::{Agent, AgentConfig, AgentRunFn, new};
use crate::callback::{AfterAgentCallback, BeforeAgentCallback};
use crate::context::builder::test_support::{SaveOnlyArtifacts, named_agent};
use crate::context::{Context, ContextDelta, InvocationContext};
use crate::error::AgentError;

/// Port of Go customAgent: counts runs, optionally ends the invocation, yields one "hello" event.
fn custom_run(calls: Arc<AtomicUsize>, end_invocation: bool) -> AgentRunFn {
    Arc::new(move |ctx: InvocationContext| {
        calls.fetch_add(1, Ordering::SeqCst);
        if end_invocation {
            ctx.end_invocation();
        }
        futures::stream::iter([Ok(model_event("hello"))]).boxed()
    })
}
fn model_event(text: &str) -> Event {
    Event {
        response: LlmResponse {
            content: Some(Content::from_text(text, ROLE_MODEL)),
            ..Default::default()
        },
        ..Default::default()
    }
}
/// A before/after-agent callback returning `text` (the two aliases are the same type).
fn content_cb(text: &'static str) -> BeforeAgentCallback {
    Arc::new(move |_ctx| async move { Ok(Some(Content::from_text(text, ROLE_MODEL))) }.boxed())
}
fn nil_cb() -> BeforeAgentCallback {
    Arc::new(|_ctx| async { Ok(None) }.boxed())
}
/// Go cmpopts.IgnoreFields(session.Event{}, "ID", "Timestamp", "InvocationID").
fn norm(mut e: Event) -> Event {
    e.id.clear();
    e.timestamp = zero_time();
    e.invocation_id.clear();
    e
}
fn empty_deltas() -> EventActions {
    EventActions {
        state_delta: Some(Map::new()),
        artifact_delta: Some(Default::default()),
        ..Default::default()
    }
}
fn ctx_for(agent: &Arc<dyn Agent>) -> InvocationContext {
    InvocationContext::builder()
        .agent(agent.clone())
        .build_invocation()
}
async fn run_all(agent: &Arc<dyn Agent>, ctx: InvocationContext) -> Vec<Result<Event, AgentError>> {
    agent.run(ctx).collect().await
}
fn ptr(a: &Arc<dyn Agent>) -> *const () {
    Arc::as_ptr(a).cast::<()>()
}

/// (name, before callbacks, after callbacks, want run calls, want events).
type CallbackCase = (
    &'static str,
    Vec<BeforeAgentCallback>,
    Vec<BeforeAgentCallback>,
    usize,
    Vec<Event>,
);

#[tokio::test]
async fn test_agent_callbacks() {
    let cases: Vec<CallbackCase> = vec![
        (
            "before agent callback runs, no llm calls",
            vec![content_cb("hello from before_agent_callback")],
            vec![],
            0,
            vec![Event {
                author: "test".into(),
                actions: empty_deltas(),
                ..model_event("hello from before_agent_callback")
            }],
        ),
        (
            "no callback effect if callbacks return nil",
            vec![nil_cb()],
            vec![nil_cb()],
            1,
            vec![Event {
                author: "test".into(),
                ..model_event("hello")
            }],
        ),
        (
            "after agent callback create a new event with new content",
            vec![],
            vec![content_cb("hello from after_agent_callback")],
            1,
            vec![
                Event {
                    author: "test".into(),
                    ..model_event("hello")
                },
                Event {
                    author: "test".into(),
                    actions: empty_deltas(),
                    ..model_event("hello from after_agent_callback")
                },
            ],
        ),
    ];
    for (name, before, after, want_calls, want_events) in cases {
        let calls = Arc::new(AtomicUsize::new(0));
        let agent = new(AgentConfig {
            name: "test".into(),
            before_agent_callbacks: before,
            after_agent_callbacks: after,
            run: Some(custom_run(calls.clone(), false)),
            ..Default::default()
        })
        .unwrap();
        let got: Vec<Event> = run_all(&agent, ctx_for(&agent))
            .await
            .into_iter()
            .map(|r| norm(r.unwrap()))
            .collect();
        assert_eq!(calls.load(Ordering::SeqCst), want_calls, "{name}");
        assert_eq!(got, want_events, "{name}");
    }
}

#[tokio::test]
async fn test_end_invocation_ends_before_main_call() {
    let calls = Arc::new(AtomicUsize::new(0));
    let agent = new(AgentConfig {
        name: "test".into(),
        before_agent_callbacks: vec![nil_cb()],
        run: Some(custom_run(calls.clone(), false)),
        ..Default::default()
    })
    .unwrap();
    let ctx = ctx_for(&agent);
    ctx.end_invocation(); // Go: invocationContext{endInvocation: true}
    for r in run_all(&agent, ctx).await {
        r.unwrap();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn test_end_invocation_ends_after_main_call() {
    let calls = Arc::new(AtomicUsize::new(0));
    let agent = new(AgentConfig {
        name: "test".into(),
        after_agent_callbacks: vec![content_cb("hello from after_agent_callback")],
        run: Some(custom_run(calls.clone(), true)),
        ..Default::default()
    })
    .unwrap();
    // Go cmpopts.IgnoreFields(session.EventActions{}, "StateDelta").
    let ignore_state_delta = |mut e: Event| {
        e.actions.state_delta = None;
        e
    };
    let got: Vec<Event> = run_all(&agent, ctx_for(&agent))
        .await
        .into_iter()
        .map(|r| ignore_state_delta(norm(r.unwrap())))
        .collect();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        got,
        vec![ignore_state_delta(Event {
            author: "test".into(),
            ..model_event("hello")
        })]
    );
}

// parity: adapted: Go WithContext swaps the embedded context.Context; Rust swaps the cancellation token via ContextDelta and every other accessor is unchanged
#[test]
fn test_with_context() {
    let inv = InvocationContext::builder()
        .invocation_id("test")
        .branch("branch")
        .build_invocation();
    let token = CancellationToken::new();
    let got = inv.with_delta(ContextDelta {
        cancellation_token: Some(token.clone()),
        ..Default::default()
    });
    assert!(!got.cancellation_token().is_cancelled());
    token.cancel();
    assert!(got.cancellation_token().is_cancelled());
    assert_eq!(got.invocation_id(), inv.invocation_id());
    assert_eq!(got.branch(), inv.branch());
    assert_eq!(got.agent_name(), inv.agent_name());
    assert_eq!(got.session_id(), inv.session_id());
}

#[test]
fn test_find_agent() {
    let child1 = named_agent("child1");
    let grandchild = named_agent("grandchild");
    let child2 = new(AgentConfig {
        name: "child2".into(),
        sub_agents: vec![grandchild.clone()],
        ..Default::default()
    })
    .unwrap();
    let root = new(AgentConfig {
        name: "root".into(),
        sub_agents: vec![child1.clone(), child2.clone()],
        ..Default::default()
    })
    .unwrap();
    for (name, want) in [
        ("root", &root),
        ("child1", &child1),
        ("child2", &child2),
        ("grandchild", &grandchild),
    ] {
        let got = root
            .find_agent(name)
            .unwrap_or_else(|| panic!("find_agent({name:?}) = None"));
        assert_eq!(ptr(&got), ptr(want), "find_agent({name:?})");
    }
    assert!(root.find_agent("unknown").is_none());
}

#[tokio::test]
async fn test_run_without_run_func() {
    let cases: [(&str, Option<&'static str>, bool, usize); 2] = [
        ("reports an error instead of panicking", None, true, 0),
        (
            "before-agent content still short-circuits",
            Some("cached"),
            false,
            1,
        ),
    ];
    for (name, before_content, want_err, want_events) in cases {
        let after_calls = Arc::new(AtomicUsize::new(0));
        let counter = after_calls.clone();
        let before: BeforeAgentCallback = Arc::new(move |_ctx| {
            async move { Ok(before_content.map(|t| Content::from_text(t, ROLE_MODEL))) }.boxed()
        });
        let after: AfterAgentCallback = Arc::new(move |_ctx| {
            counter.fetch_add(1, Ordering::SeqCst);
            async { Ok(None) }.boxed()
        });
        let agent = new(AgentConfig {
            name: "no_run".into(),
            before_agent_callbacks: vec![before],
            after_agent_callbacks: vec![after],
            ..Default::default()
        })
        .unwrap();
        let mut errs = Vec::new();
        let mut events = 0;
        for r in run_all(&agent, ctx_for(&agent)).await {
            match r {
                Ok(_) => events += 1,
                Err(e) => errs.push(e.to_string()),
            }
        }
        if want_err {
            assert!(
                errs.len() == 1 && errs[0].contains("\"no_run\""),
                "{name}: errors = {errs:?}"
            );
        } else {
            assert!(errs.is_empty(), "{name}: errors = {errs:?}");
        }
        assert_eq!(events, want_events, "{name}");
        assert_eq!(after_calls.load(Ordering::SeqCst), 0, "{name}");
    }
}

#[tokio::test]
async fn test_agent_callbacks_artifact_only_delta_emits_event() {
    let save_only: BeforeAgentCallback = Arc::new(|ctx: Context| {
        async move {
            ctx.artifacts()
                .unwrap()
                .save("report.txt", Part::from_text("x"))
                .await?;
            Ok(None)
        }
        .boxed()
    });
    let cases = [
        (
            "before_agent",
            AgentConfig {
                before_agent_callbacks: vec![save_only.clone()],
                ..Default::default()
            },
        ),
        (
            "after_agent",
            AgentConfig {
                after_agent_callbacks: vec![save_only.clone()],
                ..Default::default()
            },
        ),
    ];
    for (name, cfg) in cases {
        let calls = Arc::new(AtomicUsize::new(0));
        let agent = new(AgentConfig {
            name: "test".into(),
            run: Some(custom_run(calls.clone(), false)),
            ..cfg
        })
        .unwrap();
        let ctx = InvocationContext::builder()
            .agent(agent.clone())
            .artifacts(Arc::new(SaveOnlyArtifacts::version(1)))
            .build_invocation();
        let deltas: Vec<_> = run_all(&agent, ctx)
            .await
            .into_iter()
            .map(|r| r.unwrap_or_else(|e| panic!("{name}: unexpected error from the agent: {e}")))
            .filter_map(|ev| ev.actions.artifact_delta.filter(|d| !d.is_empty()))
            .collect();
        assert_eq!(
            deltas,
            vec![[("report.txt".to_string(), 1)].into_iter().collect()],
            "{name}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1, "{name}");
    }
}
