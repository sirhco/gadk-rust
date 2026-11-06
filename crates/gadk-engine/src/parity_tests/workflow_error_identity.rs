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

//! Port of Go workflow/error_identity_test.go.

use std::sync::Arc;

use gadk_agent::{AgentError, Context};
use parking_lot::Mutex;
use serde_json::Value;

use crate::error::{WorkflowError, WorkflowErrorKind, workflow_error};
use crate::workflow::dynamic_scheduler::node_run_error;
use crate::workflow::scheduler::test_support::*;
use crate::workflow::{Emit, NodeConfig, RunNodeOptions, dynamic_node, run_node};

#[derive(Debug, thiserror::Error)]
#[error("scripted body failure")]
struct IdentityBodyError;

#[rstest::rstest]
#[case::sentinel("sentinel")]
#[case::typed("typed")]
#[case::user_wrapper("user_wrapper")]
#[case::node_run_error("node_run_error")]
#[tokio::test]
// parity: adapted: Go compares error interface identity; AgentError is neither Clone nor comparable, so Rust asserts the body's own error (type + text) reaches the stream unwrapped.
async fn test_dynamic_node_error_identity(#[case] mode: &'static str) {
    let make = move || -> AgentError {
        match mode {
            "sentinel" => test_err("scripted sentinel"),
            "typed" => AgentError::Other(Box::new(IdentityBodyError)),
            "user_wrapper" => AgentError::other("scripted wrapper: scripted sentinel"),
            _ => node_run_error(
                "child",
                "",
                "",
                WorkflowError::new(WorkflowErrorKind::Other, "scripted sentinel"),
            )
            .into(),
        }
    };
    let want = make().to_string();
    let n = dynamic_node::<Value, Value, _, _>(
        "body",
        move |_c: Context, _i: Value, _e: Emit| async move { Err(make()) },
        NodeConfig::default(),
    );
    let (_, got) = drain_dynamic_with_err(&n, Value::Null).await;
    let got = got.expect("body error");
    assert_eq!(got.to_string(), want, "body error identity changed");
    match mode {
        "typed" => assert!(
            find_cause::<IdentityBodyError>(&got).is_some(),
            "typed body error lost"
        ),
        "sentinel" => assert_eq!(
            find_cause::<TestError>(&got),
            Some(&TestError("scripted sentinel"))
        ),
        _ => {}
    }
}

// parity: adapted: Go checks pointer identity of the errors.Join pair inside NodeRunError.Cause; AgentError has no identity, so Rust reads the NodeRunError via node_run_error(), checks its child path and its cause's NodeFailed kind, and finds the child's typed error through the source chain.
#[tokio::test]
async fn test_run_node_dynamic_child_error_identity() {
    let child = dynamic_node::<Value, Value, _, _>(
        "child",
        |_c: Context, _i: Value, _e: Emit| async {
            Err::<Value, _>(AgentError::Other(Box::new(IdentityBodyError)))
        },
        NodeConfig::default(),
    );
    let child_err: Arc<Mutex<Option<String>>> = Arc::default();
    let ce = Arc::clone(&child_err);
    let parent = dynamic_node::<Value, Value, _, _>(
        "parent",
        move |ctx: Context, _i: Value, _e: Emit| {
            let (child, ce) = (Arc::clone(&child), Arc::clone(&ce));
            async move {
                let e = run_node(&ctx, child, Value::Null, RunNodeOptions::default())
                    .await
                    .expect_err("child must fail");
                *ce.lock() = Some(e.to_string());
                Err::<Value, _>(e)
            }
        },
        NodeConfig::default(),
    );
    let (_, got) = drain_dynamic_with_err(&parent, Value::Null).await;
    let got = got.expect("parent error");
    assert_eq!(
        Some(got.to_string()),
        child_err.lock().clone(),
        "parent body error was replaced"
    );
    assert_eq!(
        got.to_string(),
        "workflow: dynamic child parent/child@1: workflow: dynamic child failed: scripted body failure"
    );
    let nre = workflow_error(&got)
        .and_then(|w| w.node_run_error())
        .expect("existing NodeRunError wrapper was lost");
    assert_eq!(nre.child_name, "child");
    assert_eq!(nre.child_path, "parent/child@1");
    let cause = nre
        .cause
        .as_deref()
        .expect("existing child failure wrapper was lost");
    assert!(
        is_kind(cause, WorkflowErrorKind::NodeFailed),
        "child failure wrapper lost ErrNodeFailed"
    );
    assert!(
        is_kind(&got, WorkflowErrorKind::NodeFailed),
        "NodeRunError kind must follow its cause"
    );
    assert!(
        find_cause::<IdentityBodyError>(&got).is_some(),
        "child business error identity changed inside NodeRunError"
    );
}
