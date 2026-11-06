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

//! Port of Go workflow/workflow_node_test.go.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use gadk_agent::{AgentError, Context, InvocationContext};
use gadk_genai::{Content, Part};
use gadk_platform::Platform;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::error::WorkflowErrorKind;
use crate::workflow::scheduler::is_cancelled;
use crate::workflow::scheduler::test_support::*;
use crate::workflow::{EdgeBuilder, Node, NodeConfig, WorkflowNode, function_node};

/// Port of Go newMockCtx with `userContent = {Parts: [{Text: text}]}`.
fn ctx_with_text(text: &str) -> InvocationContext {
    InvocationContext::builder()
        .invocation_id(TEST_INVOCATION_ID)
        .platform(Platform::deterministic(1))
        .user_content(Content {
            parts: vec![Part::from_text(text)],
            ..Content::default()
        })
        .build_invocation()
}

fn suffix_node(name: &str, suffix: &'static str) -> Arc<dyn Node> {
    function_node::<String, String, _, _>(
        name,
        move |_c: Context, input: String| async move { Ok(format!("{input}{suffix}")) },
        NodeConfig::default(),
    )
}

fn last_output(events: &[gadk_session::Event]) -> Option<Value> {
    events.iter().filter_map(|e| e.output.clone()).last()
}

#[tokio::test]
async fn test_nested_workflow() {
    let inner = suffix_node("inner_node", "-inner");
    let wf_node: Arc<dyn Node> = WorkflowNode::new("nested_step", chain(&[&start(), &inner]))
        .expect("failed to create workflow node");
    let outer = suffix_node("outer_node", "-outer");
    let outer_wf = must_new(chain(&[&start(), &wf_node, &outer]));
    let events = drain(outer_wf.run(ctx_with_text("input"))).await;
    assert_eq!(last_output(&events), Some(json!("input-inner-outer")));
}

#[tokio::test]
async fn test_nested_workflow_multiple_outputs() {
    let (n1, n2) = (
        suffix_node("inner_node1", "-inner1"),
        suffix_node("inner_node2", "-inner2"),
    );
    let wf_node: Arc<dyn Node> = WorkflowNode::new("nested_step", chain(&[&start(), &n1, &n2]))
        .expect("failed to create workflow node");
    let outer_wf = must_new(chain(&[&start(), &wf_node]));
    let events = drain(outer_wf.run(ctx_with_text("input"))).await;
    assert_eq!(last_output(&events), Some(json!("input-inner1-inner2")));
}

#[tokio::test]
async fn test_nested_workflow_multiple_terminals() {
    let passthrough = |name: &str| {
        function_node::<String, String, _, _>(
            name,
            |_c: Context, input: String| async move { Ok(input) },
            NodeConfig::default(),
        )
    };
    let (a, b, c) = (passthrough("a"), passthrough("b"), passthrough("c"));
    let inner_edges = EdgeBuilder::new()
        .add(&start(), &a)
        .add_fan_out(&a, &[b, c])
        .build();
    let wf_node: Arc<dyn Node> =
        WorkflowNode::new("nested_step", inner_edges).expect("failed to create workflow node");
    let outer_wf = must_new(chain(&[&start(), &wf_node]));
    let (_, err) = collect(outer_wf.run(ctx_with_text("input"))).await;
    // Two terminals (b, c) each produce output, so the run finalize invariant fires first.
    let err = err.expect("want ErrMultipleTerminalOutputs");
    assert!(
        is_kind(&err, WorkflowErrorKind::MultipleTerminalOutputs),
        "got error {err}, want ErrMultipleTerminalOutputs"
    );
}

// parity: adapted: Go's sink returns an output-less *session.Event; the Rust sink returns Value::Null (no output).
#[tokio::test]
async fn test_nested_workflow_silent_terminal() {
    let producer = function_node::<String, String, _, _>(
        "producer",
        |_c: Context, _i: String| async { Ok("intermediate".to_string()) },
        NodeConfig::default(),
    );
    let sink = function_node::<String, Value, _, _>(
        "sink",
        |_c: Context, _i: String| async { Ok(Value::Null) },
        NodeConfig::default(),
    );
    let wf_node: Arc<dyn Node> =
        WorkflowNode::new("nested_step", chain(&[&start(), &producer, &sink]))
            .expect("failed to create workflow node");
    let outer_wf = must_new(chain(&[&start(), &wf_node]));
    let events = drain(outer_wf.run(ctx_with_text("input"))).await;
    for ev in events {
        assert!(
            ev.output.is_none(),
            "expected no output, got {:?}",
            ev.output
        );
    }
}

#[tokio::test]
async fn test_nested_workflow_message_as_output_terminal() {
    let inner = message_as_output_node("terminal", "from-message");
    let wf_node: Arc<dyn Node> = WorkflowNode::new("nested_step", chain(&[&start(), &inner]))
        .expect("failed to create workflow node");
    let outer_wf = must_new(chain(&[&start(), &wf_node]));
    let events = drain(outer_wf.run(ctx_with_text("input"))).await;
    assert_eq!(last_output(&events), Some(json!("from-message")));
}

#[tokio::test]
async fn test_nested_workflow_updates_state_outer_reads() {
    let nested = function_node::<String, String, _, _>(
        "nested_state_updater",
        |ctx: Context, _i: String| async move {
            ctx.session().state().set("my_key", json!("my_value"))?;
            Ok("nested agent finished".to_string())
        },
        NodeConfig::default(),
    );
    let wf_node: Arc<dyn Node> = WorkflowNode::new("nested_agent", chain(&[&start(), &nested]))
        .expect("failed to create workflow node");
    let outer = function_node::<String, String, _, _>(
        "outer_state_reader",
        |ctx: Context, input: String| async move {
            let val = ctx.session().state().get("my_key")?;
            let shown = val.as_str().map_or_else(|| val.to_string(), str::to_string);
            Ok(format!(
                "Nested agent output: {input}, state value: {shown}"
            ))
        },
        NodeConfig::default(),
    );
    let outer_wf = must_new(chain(&[&start(), &wf_node, &outer]));
    // Go mockWorkflowSession: a state that writes through immediately.
    let ctx = InvocationContext::builder()
        .invocation_id(TEST_INVOCATION_ID)
        .platform(Platform::deterministic(1))
        .session(Arc::new(EventsSession::new(Vec::new())))
        .user_content(Content {
            parts: vec![Part::from_text("input")],
            ..Content::default()
        })
        .build_invocation();
    let events = drain(outer_wf.run(ctx)).await;
    assert_eq!(
        last_output(&events),
        Some(json!(
            "Nested agent output: nested agent finished, state value: my_value"
        ))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_nested_workflow_cancellation() {
    let release = Gate::new();
    let started = Gate::new();
    let (r, s) = (Arc::clone(&release), Arc::clone(&started));
    let waiting = function_node::<String, String, _, _>(
        "waiting_node",
        move |ctx: Context, _i: String| {
            let (r, s) = (Arc::clone(&r), Arc::clone(&s));
            async move {
                s.release();
                tokio::select! {
                    () = ctx.cancellation_token().cancelled() => Err(AgentError::Cancelled),
                    () = r.wait() => Ok("done".to_string()),
                }
            }
        },
        NodeConfig::default(),
    );
    let wf_node: Arc<dyn Node> = WorkflowNode::new("nested_agent", chain(&[&start(), &waiting]))
        .expect("failed to create workflow node");
    let outer_wf = must_new(chain(&[&start(), &wf_node]));
    let token = CancellationToken::new();
    let ctx = InvocationContext::builder()
        .invocation_id(TEST_INVOCATION_ID)
        .platform(Platform::deterministic(1))
        .cancellation_token(token.clone())
        .build_invocation();
    let run = tokio::spawn(async move { collect(outer_wf.run(ctx)).await });
    // Wait for the node to start running deterministically.
    started.wait().await;
    token.cancel();
    let (_, run_err) = tokio::time::timeout(Duration::from_secs(2), run)
        .await
        .expect("timeout waiting for cancellation")
        .expect("run task");
    let err = run_err.expect("expected context.Canceled on cancellation, got nil");
    assert!(
        is_cancelled(&err),
        "expected context.Canceled on cancellation, got {err}"
    );
    assert!(
        token.is_cancelled(),
        "expected the base token to be cancelled"
    );
}

#[tokio::test]
async fn test_nested_workflow_error_propagation() {
    let failing = function_node::<String, String, _, _>(
        "failing_node",
        |_c: Context, _i: String| async { Err(test_err("intentional failure")) },
        NodeConfig::default(),
    );
    let wf_node: Arc<dyn Node> = WorkflowNode::new("nested_agent", chain(&[&start(), &failing]))
        .expect("failed to create workflow node");
    let outer_wf = must_new(chain(&[&start(), &wf_node]));
    let (_, run_err) = collect(outer_wf.run(mock_ctx())).await;
    let err = run_err.expect("expected error, got nil");
    assert!(
        err.to_string().contains("intentional failure"),
        "expected error containing 'intentional failure', got {err}"
    );
}

// parity: adapted: Go "break must not panic" is dropping the stream after the first event.
#[tokio::test]
async fn test_nested_workflow_break_mid_stream() {
    let (n1, n2) = (suffix_node("inner1", "-a"), suffix_node("inner2", "-b"));
    let wf_node: Arc<dyn Node> = WorkflowNode::new("nested", chain(&[&start(), &n1, &n2]))
        .expect("failed to create workflow node");
    let mut count = 0;
    {
        let mut s = wf_node.run(top_level_ctx(), json!("in"));
        if let Some(item) = s.next().await {
            item.unwrap_or_else(|e| panic!("unexpected error: {e}"));
            count += 1;
        }
    }
    assert_eq!(count, 1, "expected to consume exactly 1 event before break");
}
