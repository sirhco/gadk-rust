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

//! Port of Go workflow/dynamic_scheduler_test.go.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

use futures::StreamExt;
use gadk_agent::{AgentError, Context, DynamicSubScheduler};
use gadk_genai::Content;
use gadk_session::{Event, NodeInfo, RequestInput};
use serde_json::{Value, json};

use crate::error::{WorkflowErrorKind, workflow_error};
use crate::tool::utils::infer_schema;
use crate::workflow::dynamic_scheduler::{new_dynamic_sub_scheduler, validate_custom_run_id};
use crate::workflow::persistence::{
    WORKFLOW_FAILED_CHILD_PATHS_KEY, WORKFLOW_NODE_CANCELLED_CODE, WORKFLOW_NODE_FAILURE_CODE,
};
use crate::workflow::scheduler::test_support::*;
use crate::workflow::{Emit, Node, NodeConfig, RunNodeOptions, dynamic_node};

#[rstest::rstest]
#[case("", true)]
#[case("123", true)]
#[case("0", true)]
#[case("a/b", true)]
#[case("a@b", true)]
#[case("order-7", false)]
#[case("abc", false)]
#[case("v2-attempt-3", false)]
#[case("7a", false)]
fn test_validate_custom_run_id(#[case] id: &str, #[case] want_err: bool) {
    let res = validate_custom_run_id(id);
    assert_eq!(
        res.is_err(),
        want_err,
        "validate_custom_run_id({id:?}) = {res:?}"
    );
    if let Err(e) = res {
        assert!(
            e.is(WorkflowErrorKind::InvalidRunId),
            "validate_custom_run_id({id:?}) = {e}, want ErrInvalidRunID"
        );
    }
}

#[tokio::test]
async fn test_sub_scheduler_counter_auto_increments_per_child_name() {
    let sub = new_dynamic_sub_scheduler(top_level_ctx(), "parent".into(), noop_emit());
    for i in 1..=3 {
        let got = sub
            .resolve_by_run_id("childA", "")
            .expect("resolveRunID childA");
        assert_eq!(got, i.to_string(), "childA #{i}");
    }
    // Independent counter per child name.
    assert_eq!(
        sub.resolve_by_run_id("childB", "").expect("childB"),
        "1",
        "childB first invocation"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_sub_scheduler_counter_concurrent_safe() {
    let sub = new_dynamic_sub_scheduler(top_level_ctx(), "parent".into(), noop_emit());
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..64 {
        let sub = Arc::clone(&sub);
        set.spawn(async move { sub.resolve_by_run_id("worker", "").expect("resolveRunID") });
    }
    let mut seen = HashSet::new();
    while let Some(id) = set.join_next().await {
        let id = id.expect("task");
        assert!(seen.insert(id.clone()), "duplicate run id {id:?}");
    }
    assert_eq!(seen.len(), 64);
}

#[tokio::test]
async fn test_sub_scheduler_counter_custom_id_does_not_increment() {
    let sub = new_dynamic_sub_scheduler(top_level_ctx(), "parent".into(), noop_emit());
    sub.resolve_by_run_id("c", "order-1").expect("custom id");
    assert_eq!(
        sub.resolve_by_run_id("c", "").expect("auto id"),
        "1",
        "custom must not bump counter"
    );
}

#[tokio::test]
async fn test_sub_scheduler_run_node_fresh_execution() {
    let (child, _) = stub_node("greeter", json!("hello"));
    let (emit, forwarded) = recording_emit();
    let sub = new_dynamic_sub_scheduler(top_level_ctx(), "wf".into(), emit);
    let out = sub
        .run_child(child, json!("world"), RunNodeOptions::default())
        .await
        .expect("runNode");
    assert_eq!(out, json!("hello"), "output");
    let forwarded = forwarded.lock();
    assert_eq!(forwarded.len(), 1, "forwarded events");
    assert_eq!(
        forwarded[0].output,
        Some(json!("hello")),
        "forwarded event Output"
    );
}

#[tokio::test]
async fn test_sub_scheduler_run_node_custom_id_in_path() {
    let (child, seen) = stub_node("processor", Value::Null);
    let sub = new_dynamic_sub_scheduler(top_level_ctx(), "wf".into(), noop_emit());
    sub.run_child(
        child,
        Value::Null,
        RunNodeOptions {
            run_id: Some("order-42".into()),
            ..RunNodeOptions::default()
        },
    )
    .await
    .expect("runNode");
    assert_eq!(seen.lock().path, "wf/processor@order-42", "child Path()");
}

#[tokio::test]
async fn test_sub_scheduler_run_node_hitl_returns_interrupted() {
    let child = request_input_node("asker", "approve?");
    let (emit, forwarded) = recording_emit();
    let sub = new_dynamic_sub_scheduler(top_level_ctx(), "wf".into(), emit);
    let err = sub
        .run_child(child, Value::Null, RunNodeOptions::default())
        .await
        .expect_err("want ErrNodeInterrupted");
    assert!(
        is_kind(&err, WorkflowErrorKind::NodeInterrupted),
        "err = {err}, want ErrNodeInterrupted"
    );
    let nre = workflow_error(&err)
        .and_then(|w| w.node_run_error())
        .unwrap_or_else(|| panic!("err is not NodeRunError: {err}"));
    assert_eq!(nre.child_path, "wf/asker@1");
    let forwarded = forwarded.lock();
    assert!(
        forwarded.len() == 1 && forwarded[0].requested_input.is_some(),
        "forwarded events = {forwarded:?}, want 1 RequestedInput event"
    );
}

/// Port of Go newInterruptThenFailNode: a RequestedInput event, then an error.
fn interrupt_then_fail_node(name: &str) -> Arc<dyn Node> {
    TestNode::arc(name, NodeConfig::default(), |_, _| {
        let ev = Event {
            requested_input: Some(RequestInput {
                interrupt_id: "iid".into(),
                message: "ask".into(),
                ..RequestInput::default()
            }),
            ..Event::default()
        };
        futures::stream::iter([Ok(ev), Err(test_err("boom"))]).boxed()
    })
}

#[tokio::test]
async fn test_sub_scheduler_run_node_error_wins_over_interrupt() {
    let child = interrupt_then_fail_node("flaky");
    let sub = new_dynamic_sub_scheduler(top_level_ctx(), "wf".into(), noop_emit());
    let err = sub
        .run_child(child, Value::Null, RunNodeOptions::default())
        .await
        .expect_err("want an error");
    assert!(
        is_kind(&err, WorkflowErrorKind::NodeFailed),
        "err = {err}, want ErrNodeFailed"
    );
    assert!(
        !is_kind(&err, WorkflowErrorKind::NodeInterrupted),
        "err = {err}; ErrNodeInterrupted must not leak when child fails after RequestInput"
    );
}

#[rstest::rstest]
#[case::other_invocation_ignored(vec![("prev-invocation", "stale")], None)]
#[case::current_invocation_wins_over_prior(vec![("prev-invocation", "stale"), ("test-invocation-id", "fresh")], Some("fresh"))]
#[tokio::test]
async fn test_sub_scheduler_rehydrate_cache_invocation_scope(
    #[case] events: Vec<(&str, &str)>,
    #[case] want: Option<&str>,
) {
    const CHILD_PATH: &str = "wf/child@1";
    let events = events
        .into_iter()
        .map(|(inv, out)| Event {
            invocation_id: inv.into(),
            output: Some(json!(out)),
            node_info: Some(NodeInfo {
                path: CHILD_PATH.into(),
                ..NodeInfo::default()
            }),
            ..Event::default()
        })
        .collect();
    let sub = new_dynamic_sub_scheduler(ctx_with_events(events), "wf".into(), noop_emit());
    let res = sub.await_or_lead(CHILD_PATH).await;
    assert_eq!(
        res.is_some(),
        want.is_some(),
        "awaitOrLead({CHILD_PATH:?}) hit = {}, want {}",
        res.is_some(),
        want.is_some()
    );
    if let (Some(res), Some(want)) = (res, want) {
        assert_eq!(res.expect("cached output"), json!(want), "cached output");
    }
}

/// Port of Go modelEvent (persistence_test.go).
fn model_event(path: &str, text: &str, message_as_output: bool) -> Event {
    let mut ev = Event {
        node_info: Some(NodeInfo {
            path: path.into(),
            message_as_output,
            ..NodeInfo::default()
        }),
        ..Event::default()
    };
    ev.response.content = Some(Content::from_text(text, "model"));
    ev
}

#[rstest::rstest]
#[case::failure_after_output("failure_after_output", false)]
#[case::later_result("later_result", true)]
#[case::result_with_error_code("result_with_error_code", true)]
#[case::message_output("message_output", true)]
#[case::parent_recorded_child_failure("parent_recorded_child_failure", false)]
#[case::parent_cancelled_without_child_path("parent_cancelled_without_child_path", false)]
#[tokio::test]
async fn test_sub_scheduler_rehydrate_cache_latest_outcome(
    #[case] case: &str,
    #[case] want_hit: bool,
) {
    const PATH: &str = "parent/child@1";
    let at = |path: &str| Event {
        node_info: Some(NodeInfo {
            path: path.into(),
            ..NodeInfo::default()
        }),
        ..Event::default()
    };
    let output = || Event {
        output: Some(json!("result")),
        ..at(PATH)
    };
    let failure = || {
        let mut ev = at(PATH);
        ev.response.error_code = WORKFLOW_NODE_FAILURE_CODE.into();
        ev
    };
    let mut parent_failure = at("parent");
    parent_failure
        .response
        .custom_metadata
        .insert(WORKFLOW_FAILED_CHILD_PATHS_KEY.into(), json!([PATH]));
    let mut result_with_code = output();
    result_with_code.response.error_code = "finish_reason".into();
    let mut parent_cancelled = at("parent");
    parent_cancelled.response.error_code = WORKFLOW_NODE_CANCELLED_CODE.into();
    let mut events = match case {
        "failure_after_output" => vec![output(), failure()],
        "later_result" => vec![failure(), output()],
        "result_with_error_code" => vec![result_with_code],
        "message_output" => vec![model_event(PATH, "result", true)],
        "parent_recorded_child_failure" => vec![output(), parent_failure],
        _ => vec![output(), parent_cancelled],
    };
    for ev in &mut events {
        ev.invocation_id = TEST_INVOCATION_ID.into();
    }
    let sub = new_dynamic_sub_scheduler(ctx_with_events(events), "parent".into(), noop_emit());
    let hit = sub.await_or_lead(PATH).await.is_some();
    assert_eq!(
        hit, want_hit,
        "cache did not reflect the child's latest outcome"
    );
}

#[tokio::test]
async fn test_sub_scheduler_rehydrate_cache_child_failure_marker() {
    let attempts = Arc::new(AtomicI32::new(0));
    let a = Arc::clone(&attempts);
    let child = dynamic_node::<Value, Value, _, _>(
        "child",
        move |ctx: Context, _i: Value, emit: Emit| {
            let a = Arc::clone(&a);
            async move {
                if a.fetch_add(1, Ordering::SeqCst) + 1 > 1 {
                    return Ok(json!("retried"));
                }
                emit(output_event(&ctx, json!("partial"))).await?;
                Err(test_err("scripted failure"))
            }
        },
        NodeConfig::default(),
    );
    let (emit, history) = recording_emit();
    let first = new_dynamic_sub_scheduler(top_level_ctx(), "parent".into(), emit);
    let err = first
        .run_child(Arc::clone(&child), Value::Null, RunNodeOptions::default())
        .await
        .expect_err("fixture did not fail after emitting output");
    assert!(
        is_kind(&err, WorkflowErrorKind::NodeFailed),
        "fixture did not fail after emitting output: {err}"
    );
    let history = history.lock().clone();
    let second = new_dynamic_sub_scheduler(ctx_with_events(history), "parent".into(), noop_emit());
    let out = second
        .run_child(child, Value::Null, RunNodeOptions::default())
        .await;
    assert!(
        matches!(&out, Ok(v) if *v == json!("retried")) && attempts.load(Ordering::SeqCst) == 2,
        "failed dynamic child was replayed from its partial output: {out:?}"
    );
}

/// Port of Go newSchemaStubNode: a stub node carrying an output schema.
fn schema_stub_node(name: &str, out: Value) -> Arc<dyn Node> {
    TestNode::with_schemas(
        name,
        None,
        Some(infer_schema::<SchemaInput>()),
        NodeConfig::default(),
        move |_, _| {
            once(Event {
                output: Some(out.clone()),
                ..Event::default()
            })
        },
    )
}

/// Port of Go testSchemaInput (base_node_test.go).
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[allow(dead_code)]
struct SchemaInput {
    value: String,
}

#[rstest::rstest]
#[case::valid_passes("ok", json!({"value": "hi"}), true)]
#[case::invalid_fails("bad", json!({"value": 123}), false)]
#[tokio::test]
async fn test_sub_scheduler_run_node_validates_output(
    #[case] name: &str,
    #[case] out: Value,
    #[case] valid: bool,
) {
    let child = schema_stub_node(name, out);
    let sub = new_dynamic_sub_scheduler(top_level_ctx(), "wf".into(), noop_emit());
    let res = sub
        .run_child(child, Value::Null, RunNodeOptions::default())
        .await;
    if valid {
        let out = res.expect("runNode");
        assert_eq!(
            out["value"],
            json!("hi"),
            "output = {out}, want map value=hi"
        );
        return;
    }
    let err = res.expect_err("want ErrNodeFailed");
    assert!(
        is_kind(&err, WorkflowErrorKind::NodeFailed),
        "err = {err}, want ErrNodeFailed"
    );
    assert!(
        err.to_string().contains("output validation failed"),
        "err = {err:?}, want substring \"output validation failed\""
    );
}

// parity: adapted: the OTel in-memory exporter becomes the SpanCapture tracing layer (current-thread runtime); codes.Error/Unset are "ERROR"/"".
#[rstest::rstest]
#[case::genuine_failure_marks_error(false, "ERROR")]
#[case::parent_cancellation_not_marked_error(true, "")]
#[tokio::test]
async fn test_sub_scheduler_run_node_span_status_on_child_error(
    #[case] cancelled: bool,
    #[case] want_status: &str,
) {
    let (cap, _guard) = SpanCapture::install();
    let child = TestNode::arc("c", NodeConfig::default(), move |_, _| {
        fail(if cancelled {
            AgentError::Cancelled
        } else {
            test_err("boom")
        })
    });
    let sub = new_dynamic_sub_scheduler(top_level_ctx(), "wf".into(), noop_emit());
    sub.run_child(child, Value::Null, RunNodeOptions::default())
        .await
        .expect_err("RunNode: got nil error, want non-nil");
    let spans = cap.node_spans();
    assert_eq!(spans.len(), 1, "got {} spans, want 1", spans.len());
    assert_eq!(spans[0].otel_name, "invoke_node c", "span name");
    assert_eq!(spans[0].status, want_status, "span status");
}
