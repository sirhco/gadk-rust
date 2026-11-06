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

//! Port of Go workflow/run_node_raise_on_wait_test.go.

use std::sync::Arc;

use futures::StreamExt;
use gadk_genai::{Content, FunctionCall, FunctionResponse, Part};
use gadk_session::Event;
use serde_json::{Value, json};

use crate::error::WorkflowErrorKind;
use crate::workflow::scheduler::test_support::*;
use crate::workflow::{Node, NodeConfig, RunNodeOptions, run_node};

/// Port of Go newLongRunningChild: one long-running FunctionCall, optionally its FR.
fn long_running_child(
    name: &str,
    fc_name: &str,
    fc_id: &str,
    yield_matching_fr: bool,
) -> Arc<dyn Node> {
    let (n, fc_name, fc_id) = (name.to_string(), fc_name.to_string(), fc_id.to_string());
    TestNode::arc(name, NodeConfig::default(), move |_ctx, _| {
        let mut fc_event = Event {
            author: n.clone(),
            long_running_tool_ids: vec![fc_id.clone()],
            ..Event::default()
        };
        fc_event.response.content = Some(Content {
            role: "model".into(),
            parts: vec![Part {
                function_call: Some(FunctionCall {
                    name: fc_name.clone(),
                    id: fc_id.clone(),
                    ..FunctionCall::default()
                }),
                ..Part::default()
            }],
        });
        let mut items = vec![Ok(fc_event)];
        if yield_matching_fr {
            let mut fr_event = Event {
                author: n.clone(),
                ..Event::default()
            };
            let mut response = serde_json::Map::new();
            response.insert("result".into(), json!("ok"));
            fr_event.response.content = Some(Content {
                role: "user".into(),
                parts: vec![Part {
                    function_response: Some(FunctionResponse {
                        name: fc_name.clone(),
                        id: fc_id.clone(),
                        response,
                        ..FunctionResponse::default()
                    }),
                    ..Part::default()
                }],
            });
            items.push(Ok(fr_event));
        }
        futures::stream::iter(items).boxed()
    })
}

fn raise_on_wait() -> RunNodeOptions {
    RunNodeOptions {
        raise_on_wait: true,
        ..RunNodeOptions::default()
    }
}

#[tokio::test]
async fn test_run_node_with_raise_on_wait_unresolved_lrt_returns_err_node_interrupted() {
    let child = long_running_child("paused_tool_caller", "confirmation", "fc-1", false);
    let err = run_in_orchestrator_with_err(move |ctx| {
        let child = Arc::clone(&child);
        async move { run_node(&ctx, child, Value::Null, raise_on_wait()).await }
    })
    .await
    .expect_err("want ErrNodeInterrupted");
    assert!(
        is_kind(&err, WorkflowErrorKind::NodeInterrupted),
        "err = {err}, want NodeInterrupted"
    );
}

#[tokio::test]
async fn test_run_node_with_raise_on_wait_resolved_lrt_completes_normally() {
    let child = long_running_child("resolves_in_place", "tool", "fc-1", true);
    let res = run_in_orchestrator_with_err(move |ctx| {
        let child = Arc::clone(&child);
        async move { run_node(&ctx, child, Value::Null, raise_on_wait()).await }
    })
    .await;
    assert!(
        res.is_ok(),
        "err = {res:?}, want nil (in-iteration FR clears the pending LRT)"
    );
}

#[tokio::test]
async fn test_run_node_without_raise_on_wait_unresolved_lrt_completes_normally() {
    let child = long_running_child("paused_tool_caller", "confirmation", "fc-1", false);
    let res = run_in_orchestrator_with_err(move |ctx| {
        let child = Arc::clone(&child);
        async move { run_node(&ctx, child, Value::Null, RunNodeOptions::default()).await }
    })
    .await;
    assert!(
        res.is_ok(),
        "err = {res:?}, want nil (LRT tracking must be opt-in via WithRaiseOnWait)"
    );
}
