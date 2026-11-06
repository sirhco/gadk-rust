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

//! Parity target for Go tool/context_test.go.

use std::sync::Arc;

use gadk_session::EventActions;
use parking_lot::Mutex;
use serde_json::json;

use crate::context::{ContextDelta, InvocationContext, ReadonlyContext};

// parity: adapted: Go type-asserts the tool context to ReadonlyContext; in Rust a tool context (Context with a call id) derefs to ReadonlyContext
#[test]
fn test_new_tool_context_interfaces() {
    let base = InvocationContext::builder().build();
    let tool_ctx = base.with_delta(ContextDelta {
        function_call_id: Some("fn1".into()),
        actions: Some(Arc::new(Mutex::new(EventActions::default()))),
        ..Default::default()
    });
    let r: &ReadonlyContext = &tool_ctx;
    assert_eq!(r.invocation_id(), base.invocation_id());
}

#[test]
fn test_new_tool_context_request_confirmation_sets_skip_summarization() {
    let actions = Arc::new(Mutex::new(EventActions::default()));
    let tool_ctx = InvocationContext::builder()
        .build()
        .with_delta(ContextDelta {
            function_call_id: Some("fn1".into()),
            actions: Some(actions.clone()),
            tool_confirmation: Some(None),
            ..Default::default()
        });
    tool_ctx
        .request_confirmation("please confirm", Some(json!({"key": "value"})))
        .unwrap();
    let a = actions.lock();
    assert!(
        a.skip_summarization,
        "RequestConfirmation did not set SkipSummarization to true"
    );
    let tc = a
        .requested_tool_confirmations
        .get("fn1")
        .expect("no confirmation for function call ID 'fn1'");
    assert_eq!(tc.hint, "please confirm");
    assert!(!tc.confirmed);
}

#[test]
fn test_new_tool_context_request_confirmation_auto_generates_id_when_empty() {
    let actions = Arc::new(Mutex::new(EventActions::default()));
    let tool_ctx = InvocationContext::builder()
        .build()
        .with_delta(ContextDelta {
            function_call_id: Some(String::new()),
            actions: Some(actions.clone()),
            ..Default::default()
        });
    tool_ctx.request_confirmation("hint", None).unwrap();
    let a = actions.lock();
    assert!(
        a.skip_summarization,
        "RequestConfirmation did not set SkipSummarization to true"
    );
    assert_eq!(
        a.requested_tool_confirmations.len(),
        1,
        "expected exactly 1 requested confirmation"
    );
    let tc = a.requested_tool_confirmations.values().next().unwrap();
    assert_eq!(tc.hint, "hint");
    assert!(!tc.confirmed);
}
