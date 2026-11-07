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

use gadk_agent::{AgentError, Context, SharedToolError, Tool, ToolError};
use parking_lot::Mutex;
use serde_json::{Map, Value, json};

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
