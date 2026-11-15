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

//! Port of Go plugin/retryandreflect/plugin_test.go. The Go tests are white-box: they build the
//! unexported `retryAndReflect` directly and read its failure counters.

use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use chrono::{DateTime, Utc};
use futures::future::ready;
use gadk_agent::{
    AfterRunCallback, AfterToolCallback, AgentError, Context, InvocationContext,
    OnToolErrorCallback, Plugin, PluginConfig, SharedToolError, Tool, ToolError,
};
use gadk_genai::{Content, FunctionCall, FunctionResponse, Part};
use gadk_model::LlmResponse;
use gadk_session::{CreateRequest, Event, InMemorySessionService, Session, SessionService, State};
use parking_lot::Mutex;
use serde_json::{Map, Value, json};

use crate::llm::{LlmAgentConfig, agent as llm_agent};
use crate::runner::PluginConfig as RunnerPluginConfig;
use crate::testutil::{MockModel, TestAgentRunner, collect_events};
use crate::tool::{FunctionTool, FunctionToolConfig};

use crate::plugin::retry_and_reflect::{
    GLOBAL_SCOPE_KEY, REFLECT_AND_RETRY_RESPONSE_TYPE, RetryAndReflect, RetryAndReflectConfig,
    TrackingScope, new_retry_and_reflect_plugin,
};

/// Go `mockTool`.
struct MockTool {
    name: String,
}

impl Tool for MockTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        ""
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn mock_tool(name: &str) -> MockTool {
    MockTool {
        name: name.to_string(),
    }
}

/// Go `mockContext{invocationID: id}`.
fn mock_context(invocation_id: &str) -> Context {
    Context::builder().invocation_id(invocation_id).build()
}

/// Go `&retryAndReflect{maxRetries, errorIfRetryExceeded, scope, scopedFailureCounters: {}}`.
fn plugin_state(
    max_retries: i32,
    error_if_retry_exceeded: bool,
    scope: TrackingScope,
) -> RetryAndReflect {
    RetryAndReflect {
        max_retries,
        error_if_retry_exceeded,
        scope,
        scoped_failure_counters: Mutex::new(HashMap::new()),
    }
}

/// Go `r.scopedFailureCounters[scope][tool]`, with `None` for a missing entry.
fn count(r: &RetryAndReflect, scope: &str, tool: &str) -> Option<i32> {
    r.scoped_failure_counters
        .lock()
        .get(scope)
        .and_then(|m| m.get(tool))
        .copied()
}

/// Go `errors.New(msg)`.
fn some_error(msg: &str) -> Arc<ToolError> {
    Arc::new(ToolError::Execution(msg.into()))
}

fn args(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap()
}

// parity: adapted: Go's functional options (WithMaxRetries, WithErrorIfRetryExceeded, WithTrackingScope) are RetryAndReflectConfig fields
#[test]
fn test_new_options() {
    let cases: Vec<(&str, RetryAndReflectConfig, bool)> = vec![
        ("defaults", RetryAndReflectConfig::default(), false),
        (
            "custom options",
            RetryAndReflectConfig {
                max_retries: 5,
                error_if_retry_exceeded: true,
                tracking_scope: TrackingScope::Global,
            },
            false,
        ),
        (
            "negative max retries",
            RetryAndReflectConfig {
                max_retries: -1,
                ..Default::default()
            },
            true,
        ),
    ];
    for (name, cfg, want_err) in cases {
        let res = new_retry_and_reflect_plugin(cfg);
        assert_eq!(
            res.is_err(),
            want_err,
            "{name}: New() error = {:?}, wantErr {want_err}",
            res.as_ref().err()
        );
        let p = match res {
            Ok(p) => p,
            Err(e) => {
                assert_eq!(
                    e.to_string(),
                    "maxRetries must be a non-negative integer",
                    "{name}"
                );
                continue;
            }
        };
        assert_eq!(
            p.name(),
            "RetryAndReflectPlugin",
            "{name}: expected plugin name RetryAndReflectPlugin, got {}",
            p.name()
        );
        assert!(
            p.after_tool_callback().is_some(),
            "{name}: expected AfterToolCallback to be set"
        );
        assert!(
            p.on_tool_error_callback().is_some(),
            "{name}: expected OnToolErrorCallback to be set"
        );
    }
}

#[test]
fn test_retry_and_reflect_success_resets() {
    let r = plugin_state(3, false, TrackingScope::Invocation);
    let ctx = mock_context("inv1");
    let tl = mock_tool("test-tool");
    let a = args(json!({"arg1": "val1"}));
    let err = some_error("some error");

    // Fail twice
    let _ = r.on_tool_error(&ctx, &tl, &a, &err);
    let _ = r.on_tool_error(&ctx, &tl, &a, &err);

    assert_eq!(count(&r, "inv1", "test-tool"), Some(2), "expected count 2");

    // Succeed
    let _ = r.after_tool(&ctx, &tl, &a, &Map::new(), None);

    assert_eq!(
        count(&r, "inv1", "test-tool"),
        None,
        "expected failure count to be reset"
    );
}

#[test]
fn test_retry_and_reflect_after_tool_no_reset_on_reflection() {
    let r = plugin_state(3, false, TrackingScope::Invocation);
    let ctx = mock_context("inv1");
    let tl = mock_tool("test-tool");
    let a = args(json!({"arg1": "val1"}));
    let err = some_error("some error");

    // Fail once
    let res = r.on_tool_error(&ctx, &tl, &a, &err).unwrap();

    assert_eq!(count(&r, "inv1", "test-tool"), Some(1), "expected count 1");

    // AfterTool is called with the result of onToolError
    let _ = r.after_tool(&ctx, &tl, &a, &res.unwrap_or_default(), None);

    assert_eq!(
        count(&r, "inv1", "test-tool"),
        Some(1),
        "expected failure count NOT to be reset"
    );
}

#[test]
fn test_retry_and_reflect_max_retries() {
    let r = plugin_state(2, false, TrackingScope::Invocation);
    let ctx = mock_context("inv1");
    let tl = mock_tool("test-tool");
    let a = args(json!({"arg1": "val1"}));
    let err = some_error("fail");

    // 1st retry
    let res = r.on_tool_error(&ctx, &tl, &a, &err).unwrap().unwrap();
    assert_eq!(res["retry_count"], json!(1), "expected retry_count 1");
    assert_eq!(
        res["response_type"],
        json!(REFLECT_AND_RETRY_RESPONSE_TYPE),
        "expected reflectAndRetryResponseType"
    );

    // 2nd retry
    let res = r.on_tool_error(&ctx, &tl, &a, &err).unwrap().unwrap();
    assert_eq!(res["retry_count"], json!(2), "expected retry_count 2");

    // 3rd time - exceed
    let res = r.on_tool_error(&ctx, &tl, &a, &err).unwrap().unwrap();
    // It returns maxRetries in createToolRetryExceedMsg
    assert_eq!(res["retry_count"], json!(2), "expected retry_count 2 (max)");
    let guidance = res["reflection_guidance"].as_str().unwrap();
    assert!(
        guidance.contains("exceeded"),
        "expected guidance to mention exceeded, got {guidance}"
    );
    assert!(
        guidance.contains("failed consecutively 2 times"),
        "expected guidance to mention 'failed consecutively 2 times', got {guidance}"
    );
}

// parity: adapted: Go's identity check (gotErr != err) is a downcast to SharedToolError plus Arc::ptr_eq on the original Arc<ToolError>
#[test]
fn test_retry_and_reflect_error_if_retry_exceeded() {
    let r = plugin_state(1, true, TrackingScope::Invocation);
    let ctx = mock_context("inv1");
    let tl = mock_tool("test-tool");
    let err = some_error("fail");

    // 1st retry
    let _ = r.on_tool_error(&ctx, &tl, &Map::new(), &err);

    // 2nd time - exceed, should return error
    let got = r.on_tool_error(&ctx, &tl, &Map::new(), &err).unwrap_err();
    match &got {
        AgentError::Other(inner) => {
            let shared = inner
                .downcast_ref::<SharedToolError>()
                .unwrap_or_else(|| panic!("expected error {err}, got {got}"));
            assert!(
                Arc::ptr_eq(&shared.0, &err),
                "expected error {err}, got {got}"
            );
        }
        other => panic!("expected error {err}, got {other:?}"),
    }
    assert_eq!(got.to_string(), "fail");
}

#[test]
fn test_retry_and_reflect_scopes() {
    let r_invocation = plugin_state(3, false, TrackingScope::Invocation);
    let r_global = plugin_state(3, false, TrackingScope::Global);

    let ctx1 = mock_context("inv1");
    let ctx2 = mock_context("inv2");
    let tl = mock_tool("test-tool");
    let err = some_error("fail");

    // Invocation scope
    let _ = r_invocation.on_tool_error(&ctx1, &tl, &Map::new(), &err);
    assert_eq!(
        count(&r_invocation, "inv1", "test-tool"),
        Some(1),
        "expected 1 failure in inv1"
    );
    let _ = r_invocation.on_tool_error(&ctx2, &tl, &Map::new(), &err);
    assert_eq!(
        count(&r_invocation, "inv2", "test-tool"),
        Some(1),
        "expected 1 failure in inv2"
    );

    // Global scope
    let _ = r_global.on_tool_error(&ctx1, &tl, &Map::new(), &err);
    assert_eq!(
        count(&r_global, GLOBAL_SCOPE_KEY, "test-tool"),
        Some(1),
        "expected 1 failure in global scope"
    );
    let _ = r_global.on_tool_error(&ctx2, &tl, &Map::new(), &err);
    assert_eq!(
        count(&r_global, GLOBAL_SCOPE_KEY, "test-tool"),
        Some(2),
        "expected 2 failures in global scope"
    );
}

/// Go `fmt.Errorf("<msg>: %w", err)` over a tool error.
#[derive(Debug)]
struct Wrapped {
    msg: &'static str,
    source: ToolError,
}

impl std::fmt::Display for Wrapped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.msg, self.source)
    }
}

impl std::error::Error for Wrapped {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Go `toolinternal.NewPanicError("test-tool", "secret-user-data-123", stack)`: the text gadk's
/// FunctionTool gives a recovered panic (no stack; Rust cannot capture it after unwinding).
fn panic_error() -> ToolError {
    ToolError::Panic("panic in tool \"test-tool\": secret-user-data-123".into())
}

// parity: adapted: Go's PanicError is gadk's ToolError::Panic (text without a stack), and the wrapping fmt.Errorf("%w") is a local Wrapped error inside ToolError::Execution
#[test]
fn test_retry_and_reflect_tool_panic() {
    // calls is how many times the tool fails; with maxRetries 1 the first
    // failure builds the reflection response and the second the
    // retry-limit-exceeded one.
    let cases: Vec<(&str, Arc<ToolError>, usize, &str)> = vec![
        ("reflection response", Arc::new(panic_error()), 1, "failed"),
        (
            "retry limit exceeded response",
            Arc::new(panic_error()),
            2,
            "exceeded",
        ),
        (
            // An error on its way out of a tool is wrapped more than once:
            // agenttool wraps what the sub-agent run returned, and an earlier
            // OnToolError callback is free to wrap it before this plugin sees
            // it.
            "wrapped panic",
            Arc::new(ToolError::Execution(Box::new(Wrapped {
                msg: "lookup failed",
                source: panic_error(),
            }))),
            1,
            "failed",
        ),
    ];
    for (name, err, calls, want_top_of_guidance) in cases {
        let r = plugin_state(1, false, TrackingScope::Invocation);
        let ctx = mock_context("inv1");
        let tl = mock_tool("test-tool");

        let mut res = Map::new();
        for _ in 0..calls {
            res = r
                .on_tool_error(&ctx, &tl, &args(json!({"order_id": "1"})), &err)
                .unwrap()
                .unwrap_or_default();
        }

        assert_eq!(
            res["error_details"],
            json!("Invoking `test-tool()` failed with an internal error."),
            "{name}: error_details"
        );
        assert_eq!(res["error_type"], json!("panic"), "{name}: error_type");
        let guidance = res["reflection_guidance"].as_str().unwrap_or_default();
        assert!(
            guidance.contains(want_top_of_guidance),
            "{name}: reflection_guidance does not mention {want_top_of_guidance:?}: {guidance:?}"
        );
        // Nothing the plugin answers with may carry the panic value, the
        // stack or the internal error type: the model, the session and REST
        // clients all see this response.
        for (key, value) in &res {
            let Some(text) = value.as_str() else { continue };
            for leaked in ["secret-user-data-123", "goroutine", "PanicError"] {
                assert!(
                    !text.contains(leaked),
                    "{name}: response[{key:?}] leaks {leaked:?}"
                );
            }
        }
    }
}

// parity: adapted: Go's %T text "*errors.errorString" is gadk's variant path "ToolError::Execution" (the plugin's existing error_type divergence)
#[test]
fn test_retry_and_reflect_returned_error_reaches_the_model() {
    let r = plugin_state(1, false, TrackingScope::Invocation);
    let res = r
        .on_tool_error(
            &mock_context("inv1"),
            &mock_tool("test-tool"),
            &Map::new(),
            &some_error("order not found"),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        res["error_details"],
        json!("order not found"),
        "error_details"
    );
    assert_eq!(
        res["error_type"],
        json!("ToolError::Execution"),
        "error_type"
    );
    let guidance = res["reflection_guidance"].as_str().unwrap_or_default();
    assert!(
        guidance.contains("order not found"),
        "reflection_guidance does not carry the tool's error: {guidance:?}"
    );
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct NoArgs {}

// TestToolPanicThroughRunner drives a panicking tool through a full run with
// the plugin installed, because the plugin answers OnToolError with its own
// response and so decides on its own what the model sees.
#[tokio::test]
async fn test_tool_panic_through_runner() {
    let panic_tool = FunctionTool::new(
        FunctionToolConfig::new("lookup_order", ""),
        |_ctx: Context, _args: NoArgs| async move {
            if true {
                panic!("secret-user-data-123");
            }
            Ok::<Map<String, Value>, ToolError>(Map::new())
        },
    )
    .unwrap();
    let m = MockModel::from_contents(vec![
        Content {
            role: "model".into(),
            parts: vec![Part {
                function_call: Some(FunctionCall {
                    id: "c1".into(),
                    name: "lookup_order".into(),
                    ..Default::default()
                }),
                ..Default::default()
            }],
        },
        Content::from_text("done", "model"),
    ]);
    let a = llm_agent::new(LlmAgentConfig {
        name: "a".into(),
        model: Some(m),
        tools: vec![panic_tool],
        ..Default::default()
    })
    .unwrap();
    let p = new_retry_and_reflect_plugin(RetryAndReflectConfig::default()).unwrap();
    let r = TestAgentRunner::with_plugins(
        a,
        RunnerPluginConfig {
            plugins: vec![p],
            ..Default::default()
        },
    )
    .await;

    let events = collect_events(r.run("hi")).await.unwrap();
    let responses: Vec<String> = events
        .iter()
        .filter_map(|ev| ev.response.content.as_ref())
        .flat_map(|c| c.parts.iter())
        .filter_map(|p| p.function_response.as_ref())
        .map(|fr| Value::Object(fr.response.clone()).to_string())
        .collect();

    assert!(
        !responses.is_empty(),
        "the run produced no function response"
    );
    // Without this the test would also pass when the plugin stopped answering
    // and the flow's own substitution took over, which is not what it covers.
    assert!(
        responses[0].contains(REFLECT_AND_RETRY_RESPONSE_TYPE),
        "the plugin did not answer the failed call: {:.200}",
        responses[0]
    );
    for got in &responses {
        for leaked in ["secret-user-data-123", "goroutine", "PanicError"] {
            assert!(
                !got.contains(leaked),
                "function response leaks {leaked:?}: {got:.200}"
            );
        }
    }
}

/// Go `r.afterRun(ictx)` (7fafcbbc); gadk's `RetryAndReflect::after_run` is synchronous.
#[allow(clippy::unused_async)]
async fn after_run(r: &RetryAndReflect, ictx: InvocationContext) {
    r.after_run(&ictx);
}

/// Go `newRunContext`: the context of a run of `invocation_id` over a session holding `events`.
async fn new_run_context(invocation_id: &str, events: Vec<Event>) -> InvocationContext {
    let service = InMemorySessionService::new();
    let sess = service
        .create(CreateRequest {
            app_name: "test".into(),
            user_id: "user".into(),
            session_id: Some("ses1".into()),
            state: None,
        })
        .await
        .unwrap_or_else(|e| panic!("Failed to create session: {e}"));
    for mut ev in events {
        service
            .append_event(&*sess, &mut ev)
            .await
            .unwrap_or_else(|e| panic!("Failed to append event: {e}"));
    }
    let sess = service
        .get(gadk_session::GetRequest {
            app_name: "test".into(),
            user_id: "user".into(),
            session_id: "ses1".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    InvocationContext::builder()
        .session(sess)
        .invocation_id(invocation_id)
        .build_invocation()
}

/// Go `longRunningCallEvent`.
fn long_running_call_event(invocation_id: &str, call_ids: &[&str]) -> Event {
    let content = Some(Content {
        role: "model".into(),
        parts: call_ids
            .iter()
            .map(|id| Part {
                function_call: Some(FunctionCall {
                    id: (*id).into(),
                    name: "ask_approval".into(),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .collect(),
    });
    Event {
        invocation_id: invocation_id.into(),
        author: "agent".into(),
        long_running_tool_ids: call_ids.iter().map(|s| (*s).to_string()).collect(),
        response: LlmResponse {
            content,
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Go `functionResponseEvent`.
fn function_response_event(
    invocation_id: &str,
    author: &str,
    call_id: &str,
    response: Value,
) -> Event {
    let content = Some(Content {
        role: "user".into(),
        parts: vec![Part {
            function_response: Some(FunctionResponse {
                id: call_id.into(),
                name: "ask_approval".into(),
                response: args(response),
                ..Default::default()
            }),
            ..Default::default()
        }],
    });
    Event {
        invocation_id: invocation_id.into(),
        author: author.into(),
        response: LlmResponse {
            content,
            ..Default::default()
        },
        ..Default::default()
    }
}

#[tokio::test]
async fn test_retry_and_reflect_after_run_drops_invocation_counts() {
    let p = new_retry_and_reflect_plugin(RetryAndReflectConfig::default())
        .unwrap_or_else(|e| panic!("New() failed: {e}"));
    assert!(
        p.after_run_callback().is_some(),
        "expected New to register an AfterRunCallback"
    );

    let r = plugin_state(3, false, TrackingScope::Invocation);
    let tl = mock_tool("test-tool");
    let err = some_error("some error");
    let _ = r.on_tool_error(&mock_context("inv1"), &tl, &Map::new(), &err);
    let _ = r.on_tool_error(&mock_context("inv2"), &tl, &Map::new(), &err);

    after_run(&r, new_run_context("inv1", vec![]).await).await;

    assert!(
        !r.scoped_failure_counters.lock().contains_key("inv1"),
        "expected the counts of the ended invocation inv1 to be dropped"
    );
    assert_eq!(
        count(&r, "inv2", "test-tool"),
        Some(1),
        "expected the counts of invocation inv2 to be kept"
    );
}

#[tokio::test]
async fn test_retry_and_reflect_after_run_keeps_counts_of_paused_invocation() {
    let cases: Vec<(&str, Vec<Event>, bool)> = vec![
        (
            "unanswered long-running call",
            vec![long_running_call_event("inv1", &["call-1"])],
            true,
        ),
        (
            "long-running call answered by the user",
            vec![
                long_running_call_event("inv1", &["call-1"]),
                function_response_event("inv1", "user", "call-1", json!({"approved": true})),
            ],
            false,
        ),
        (
            "long-running call settled by a failure response of the plugin",
            vec![
                long_running_call_event("inv1", &["call-1"]),
                function_response_event(
                    "inv1",
                    "agent",
                    "call-1",
                    json!({"response_type": REFLECT_AND_RETRY_RESPONSE_TYPE}),
                ),
            ],
            false,
        ),
        (
            "intermediate response of the tool itself",
            vec![
                long_running_call_event("inv1", &["call-1"]),
                function_response_event("inv1", "agent", "call-1", json!({"status": "pending"})),
            ],
            true,
        ),
        (
            "one of two parallel long-running calls answered",
            vec![
                long_running_call_event("inv1", &["call-1", "call-2"]),
                function_response_event("inv1", "user", "call-1", json!({"approved": true})),
            ],
            true,
        ),
        (
            "both parallel long-running calls answered",
            vec![
                long_running_call_event("inv1", &["call-1", "call-2"]),
                function_response_event("inv1", "user", "call-1", json!({"approved": true})),
                function_response_event("inv1", "user", "call-2", json!({"approved": false})),
            ],
            false,
        ),
        (
            "response to an unrelated call ID",
            vec![
                long_running_call_event("inv1", &["call-1"]),
                function_response_event("inv1", "user", "call-unknown", json!({"approved": true})),
            ],
            true,
        ),
        (
            "unanswered long-running call of another invocation",
            vec![long_running_call_event("inv0", &["call-0"])],
            false,
        ),
    ];
    for (name, events, want_kept) in cases {
        let r = plugin_state(3, false, TrackingScope::Invocation);
        let _ = r.on_tool_error(
            &mock_context("inv1"),
            &mock_tool("test-tool"),
            &Map::new(),
            &some_error("some error"),
        );

        after_run(&r, new_run_context("inv1", events).await).await;

        let kept = r.scoped_failure_counters.lock().contains_key("inv1");
        assert_eq!(kept, want_kept, "{name}: counts of inv1 kept");
    }
}

#[tokio::test]
async fn test_retry_and_reflect_after_run_keeps_global_counts() {
    let r = plugin_state(3, false, TrackingScope::Global);
    let _ = r.on_tool_error(
        &mock_context("inv1"),
        &mock_tool("test-tool"),
        &Map::new(),
        &some_error("some error"),
    );

    after_run(&r, new_run_context("inv1", vec![]).await).await;

    assert_eq!(
        count(&r, GLOBAL_SCOPE_KEY, "test-tool"),
        Some(1),
        "expected the global counts to be kept"
    );
}

// parity: adapted: gadk's TrackingScope is a closed enum, so only Go's "Invocation" case exists (Go's "" and "INVOCATION" values cannot be built)
#[tokio::test]
async fn test_retry_and_reflect_after_run_drops_counts_of_every_per_invocation_scope() {
    let r = plugin_state(3, false, TrackingScope::Invocation);
    let _ = r.on_tool_error(
        &mock_context("inv1"),
        &mock_tool("test-tool"),
        &Map::new(),
        &some_error("some error"),
    );
    assert!(
        r.scoped_failure_counters.lock().contains_key("inv1"),
        "expected the failure to be counted under invocation inv1"
    );

    after_run(&r, new_run_context("inv1", vec![]).await).await;

    assert!(
        !r.scoped_failure_counters.lock().contains_key("inv1"),
        "expected the counts of the ended invocation inv1 to be dropped"
    );
}

/// Go `eventsCountingSession`: counts the reads of its events.
struct EventsCountingSession {
    inner: Arc<dyn Session>,
    events_reads: AtomicUsize,
}

impl Session for EventsCountingSession {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn app_name(&self) -> &str {
        self.inner.app_name()
    }
    fn user_id(&self) -> &str {
        self.inner.user_id()
    }
    fn state(&self) -> &dyn State {
        self.inner.state()
    }
    fn events(&self) -> Vec<Event> {
        self.events_reads.fetch_add(1, Ordering::SeqCst);
        self.inner.events()
    }
    fn last_update_time(&self) -> DateTime<Utc> {
        self.inner.last_update_time()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[tokio::test]
async fn test_retry_and_reflect_after_run_scans_events_only_of_invocation_with_counts() {
    let cases = [
        ("no failure in the ended invocation", "inv2", 0),
        ("failure in the ended invocation", "inv1", 1),
    ];
    for (name, failed_invocation, want_events_reads) in cases {
        let r = plugin_state(3, false, TrackingScope::Invocation);
        let _ = r.on_tool_error(
            &mock_context(failed_invocation),
            &mock_tool("test-tool"),
            &Map::new(),
            &some_error("some error"),
        );
        let sess = Arc::new(EventsCountingSession {
            inner: new_run_context("inv1", vec![]).await.session(),
            events_reads: AtomicUsize::new(0),
        });

        after_run(
            &r,
            InvocationContext::builder()
                .session(sess.clone())
                .invocation_id("inv1")
                .build_invocation(),
        )
        .await;

        let reads = sess.events_reads.load(Ordering::SeqCst);
        assert_eq!(
            reads, want_events_reads,
            "{name}: session events read {reads} times, want {want_events_reads}"
        );
    }
}

/// Go `plugin.New(plugin.Config{Name, AfterToolCallback: r.afterTool, OnToolErrorCallback:
/// r.onToolError, AfterRunCallback: r.afterRun})` over a shared `r`.
fn plugin_over(r: Arc<RetryAndReflect>) -> Plugin {
    let after_tool: AfterToolCallback = {
        let r = r.clone();
        Arc::new(
            move |ctx, tool: Arc<dyn Tool>, args, result, err: Option<Arc<ToolError>>| {
                Box::pin(ready(r.after_tool(
                    &ctx,
                    &*tool,
                    &args,
                    &result,
                    err.as_deref(),
                )))
            },
        )
    };
    let on_tool_error: OnToolErrorCallback = {
        let r = r.clone();
        Arc::new(move |ctx, tool: Arc<dyn Tool>, args, err| {
            Box::pin(ready(r.on_tool_error(&ctx, &*tool, &args, &err)))
        })
    };
    let after_run_cb: AfterRunCallback = Arc::new(move |ictx| {
        let r = r.clone();
        Box::pin(async move { after_run(&r, ictx).await })
    });
    Plugin::new(PluginConfig {
        name: "RetryAndReflectPlugin".into(),
        after_tool_callback: Some(after_tool),
        on_tool_error_callback: Some(on_tool_error),
        after_run_callback: Some(after_run_cb),
        ..Default::default()
    })
}

fn bare_call(name: &str) -> Content {
    Content::from_function_call(
        FunctionCall {
            name: name.into(),
            ..Default::default()
        },
        "model",
    )
}

// TestRetryAndReflect_RunnerContinuesCountsOfAnsweredRun runs the plugin
// through runner.Run. The run that answers a long-running call reuses the
// invocation ID of the paused run, so it continues that run's retry budget,
// and the counts are dropped once it ends.
#[tokio::test]
async fn test_retry_and_reflect_runner_continues_counts_of_answered_run() {
    let flaky = FunctionTool::new(
        FunctionToolConfig::new("flaky", "fails"),
        |_ctx: Context, _args: NoArgs| async move {
            Err::<Map<String, Value>, ToolError>(ToolError::Execution("backend unavailable".into()))
        },
    )
    .unwrap_or_else(|e| panic!("Failed to create the flaky tool: {e}"));
    let mut ask_cfg = FunctionToolConfig::new("ask_approval", "asks the user");
    ask_cfg.is_long_running = true;
    let ask_approval = FunctionTool::new(ask_cfg, |_ctx: Context, _args: NoArgs| async move {
        Ok::<_, ToolError>(args(json!({"status": "pending"})))
    })
    .unwrap_or_else(|e| panic!("Failed to create the long-running tool: {e}"));
    let mock_model = MockModel::from_contents(vec![
        // The paused run: a failure, then a call the user has not answered yet.
        bare_call("flaky"),
        bare_call("ask_approval"),
        Content::from_text("waiting for approval", "model"),
        // The run that answers the call: the same tool fails again.
        bare_call("flaky"),
        Content::from_text("done", "model"),
    ]);
    let a = llm_agent::new(LlmAgentConfig {
        name: "agent".into(),
        model: Some(mock_model),
        tools: vec![flaky, ask_approval],
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("Failed to create the agent: {e}"));
    let r = Arc::new(plugin_state(3, false, TrackingScope::Invocation));
    let test_runner = TestAgentRunner::with_plugins(
        a,
        RunnerPluginConfig {
            plugins: vec![plugin_over(r.clone())],
            ..Default::default()
        },
    )
    .await;
    let failure_count = |invocation_id: &str| -> (i32, bool) {
        let counters = r.scoped_failure_counters.lock();
        match counters.get(invocation_id) {
            Some(m) => (m.get("flaky").copied().unwrap_or(0), true),
            None => (0, false),
        }
    };

    let paused = collect_events(test_runner.run("start"))
        .await
        .unwrap_or_else(|e| panic!("Paused run failed: {e}"));
    let (mut call_id, mut invocation_id) = (String::new(), String::new());
    for ev in &paused {
        if let Some(id) = ev.long_running_tool_ids.first() {
            call_id = id.clone();
            invocation_id = ev.invocation_id.clone();
        }
    }
    assert!(
        !call_id.is_empty(),
        "expected the first run to pause on a long-running call"
    );
    let (count, ok) = failure_count(&invocation_id);
    assert!(
        ok && count == 1,
        "after the paused run: count {count} (kept {ok}), want 1 kept"
    );

    let answer = Content {
        role: "user".into(),
        parts: vec![Part {
            function_response: Some(FunctionResponse {
                id: call_id.clone(),
                name: "ask_approval".into(),
                response: args(json!({"approved": true})),
                ..Default::default()
            }),
            ..Default::default()
        }],
    };
    let answered = collect_events(test_runner.run_content(answer))
        .await
        .unwrap_or_else(|e| panic!("Answering run failed: {e}"));
    let mut retry_count = None;
    for ev in &answered {
        assert_eq!(
            ev.invocation_id, invocation_id,
            "event of the answering run has invocation ID {:?}, want {invocation_id:?}",
            ev.invocation_id
        );
        for part in ev.response.content.iter().flat_map(|c| c.parts.iter()) {
            if let Some(fr) = &part.function_response
                && fr.name == "flaky"
            {
                retry_count = fr.response.get("retry_count").cloned();
            }
        }
    }
    assert_eq!(
        retry_count,
        Some(json!(2)),
        "retry_count of the failure in the answering run"
    );
    let (count, ok) = failure_count(&invocation_id);
    assert!(
        !ok,
        "after the answering run: count {count} kept, want the counts dropped"
    );
}
