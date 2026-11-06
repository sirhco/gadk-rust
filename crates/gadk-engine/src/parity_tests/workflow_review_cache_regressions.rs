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

//! Port of Go workflow/review_cache_regressions_test.go.

use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Duration;

use futures::StreamExt;
use gadk_agent::Context;
use gadk_genai::{Content, FunctionResponse};
use gadk_session::{Event, NodeInfo, RequestInput};
use parking_lot::Mutex;
use serde_json::{Map, Value, json};

use crate::error::WorkflowErrorKind;
use crate::workflow::dynamic_scheduler::new_dynamic_sub_scheduler;
use crate::workflow::failure_inventory::FailureInventory;
use crate::workflow::persistence::{
    WORKFLOW_DELEGATED_OUTPUT_KEY, WORKFLOW_FAILED_CHILD_PATHS_KEY, WORKFLOW_NODE_CANCELLED_CODE,
    WORKFLOW_NODE_COMPLETED_KEY, WORKFLOW_NODE_FAILURE_OUTCOME, WORKFLOW_NODE_OUTCOME_KEY,
    collect_node_outputs, workflow_node_outcome,
};
use crate::workflow::scheduler::test_support::*;
use crate::workflow::scheduler::{Scheduler, build_nodes_by_name};
use crate::workflow::state::{NodeState, NodeStatus, RunState};
use crate::workflow::{
    Emit, JoinNode, Node, NodeConfig, RunNodeOptions, WORKFLOW_INPUT_FUNCTION_CALL_NAME, Workflow,
    WorkflowNode, WorkflowOptions, dynamic_node, dynamic_node_with_schema, function_node,
    resume_or_request_input, run_node,
};

const PARENT: &str = "root@1/c@1/mid@1";

fn leaf() -> String {
    format!("{PARENT}/leaf@1")
}
fn at(path: &str) -> Event {
    Event {
        node_info: Some(NodeInfo {
            path: path.into(),
            ..NodeInfo::default()
        }),
        ..Event::default()
    }
}
fn output(path: &str) -> Event {
    Event {
        output: Some(json!("result")),
        ..at(path)
    }
}
fn marker(path: &str, outcome: &str, failed_paths: Option<Vec<&str>>) -> Event {
    let mut ev = at(path);
    ev.response
        .custom_metadata
        .insert("adk.workflow.node_outcome".into(), json!(outcome));
    if let Some(paths) = failed_paths {
        ev.response
            .custom_metadata
            .insert(WORKFLOW_FAILED_CHILD_PATHS_KEY.into(), json!(paths));
    }
    ev
}
fn with_md(mut ev: Event, key: &str, v: Value) -> Event {
    ev.response.custom_metadata.insert(key.into(), v);
    ev
}
fn with_code(mut ev: Event, code: &str) -> Event {
    ev.response.error_code = code.into();
    ev
}
fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => panic!("not an object"),
    }
}
fn round_trip(events: &[Event]) -> Vec<Event> {
    serde_json::from_value(serde_json::to_value(events).expect("history serialization failed"))
        .expect("history deserialization failed")
}
fn node_state<'a>(state: &'a Option<RunState>, name: &str) -> &'a NodeState {
    let state = state
        .as_ref()
        .unwrap_or_else(|| panic!("ReconstructRunState returned nil state, want node {name:?}"));
    state
        .nodes
        .get(name)
        .unwrap_or_else(|| panic!("no NodeState for {name:?}; nodes = {:?}", state.nodes))
}
fn reconstruct(wf: &Arc<Workflow>, events: Vec<Event>) -> Option<RunState> {
    wf.reconstruct_run_state(&EventsSession::new(events), "")
        .expect("state reconstruction failed")
}
/// Go `&session.Event{Author: "user"}` resolving `id` with `{"result": "approved"}`.
fn resolved(id: &str) -> Event {
    let mut ev = Event {
        author: "user".into(),
        ..Event::default()
    };
    ev.response.content = Some(Content::from_function_response(
        FunctionResponse {
            id: id.into(),
            name: WORKFLOW_INPUT_FUNCTION_CALL_NAME.into(),
            response: obj(json!({"result": "approved"})),
            ..FunctionResponse::default()
        },
        "user",
    ));
    ev
}
fn asker(name: &str, id: &'static str) -> Arc<dyn Node> {
    dynamic_node::<Value, Value, _, _>(
        name,
        move |ctx: Context, _i: Value, emit: Emit| async move {
            let req = RequestInput {
                interrupt_id: id.into(),
                message: "approve".into(),
                ..RequestInput::default()
            };
            resume_or_request_input(&ctx, &emit, req)
                .await
                .map(|v| v.unwrap_or(Value::Null))
        },
        NodeConfig::default(),
    )
}
fn silent(name: &str) -> Arc<dyn Node> {
    dynamic_node::<Value, Value, _, _>(
        name,
        |_c: Context, _i: Value, _e: Emit| async { Ok(Value::Null) },
        NodeConfig::default(),
    )
}
fn wait_cfg() -> NodeConfig {
    NodeConfig {
        wait_for_output: Some(true),
        ..NodeConfig::default()
    }
}
fn use_as_output() -> RunNodeOptions {
    RunNodeOptions {
        use_as_output: true,
        ..RunNodeOptions::default()
    }
}
/// A dynamic node that runs `child` with WithUseAsOutput and returns nil.
fn delegating(name: &str, child: Arc<dyn Node>, cfg: NodeConfig) -> Arc<dyn Node> {
    dynamic_node::<Value, Value, _, _>(
        name,
        move |ctx: Context, input: Value, _e: Emit| {
            let child = Arc::clone(&child);
            async move {
                run_node(&ctx, child, input, use_as_output()).await?;
                Ok(Value::Null)
            }
        },
        cfg,
    )
}
fn delegating_with_integer_schema(name: &str, child: Arc<dyn Node>) -> Arc<dyn Node> {
    dynamic_node_with_schema::<Value, Value, _, _>(
        name,
        move |ctx: Context, input: Value, _e: Emit| {
            let child = Arc::clone(&child);
            async move {
                run_node(&ctx, child, input, use_as_output()).await?;
                Ok(Value::Null)
            }
        },
        None,
        Some(json!({"type": "integer"})),
        NodeConfig::default(),
    )
    .expect("fixture construction failed")
}

/// Go's table for TestSubScheduler_RehydrateCache_AncestorOutcomes: (history, wanted cache).
fn ancestor_case(name: &str) -> (Vec<Event>, Value) {
    let l = leaf();
    let keep = json!({ l.clone(): "result" });
    let empty = json!({});
    match name {
        "recorded_parent_path" => (
            vec![
                output(&l),
                marker("root@1/c@1", "cancelled", Some(vec![PARENT])),
            ],
            empty,
        ),
        "recorded_ancestor_path" => (
            vec![
                output(&l),
                marker("root@1", "failed", Some(vec!["root@1/c@1"])),
            ],
            empty,
        ),
        "direct_parent_failure" => (vec![output(&l), marker(PARENT, "failed", None)], empty),
        "ancestor_unknown_cancel" => (
            vec![output(&l), marker("root@1/c@1", "cancelled", None)],
            empty,
        ),
        "ancestor_empty_paths" => (
            vec![output(&l), marker("root@1/c@1", "cancelled", Some(vec![]))],
            keep,
        ),
        "direct_parent_empty_failure" => (
            vec![output(&l), marker(PARENT, "failed", Some(vec![]))],
            keep,
        ),
        "direct_parent_null_paths" => (
            vec![
                output(&l),
                with_md(
                    marker(PARENT, "failed", None),
                    WORKFLOW_FAILED_CHILD_PATHS_KEY,
                    Value::Null,
                ),
            ],
            empty,
        ),
        "direct_parent_invalid_type" => (
            vec![
                output(&l),
                with_md(
                    marker(PARENT, "failed", None),
                    WORKFLOW_FAILED_CHILD_PATHS_KEY,
                    json!("invalid"),
                ),
            ],
            empty,
        ),
        "ancestor_invalid_paths" => (
            vec![
                output(&l),
                with_md(
                    marker("root@1/c@1", "cancelled", None),
                    WORKFLOW_FAILED_CHILD_PATHS_KEY,
                    json!(["", false]),
                ),
            ],
            empty,
        ),
        "legacy_ancestor_cancel" => (
            vec![
                output(&l),
                with_code(at("root@1/c@1"), WORKFLOW_NODE_CANCELLED_CODE),
            ],
            empty,
        ),
        "failed_descendant_subtree" => (
            vec![
                output(&format!("{PARENT}/branch@1/leaf@1")),
                output(&format!("{PARENT}/stable@1")),
                marker(
                    "root@1/c@1",
                    "failed",
                    Some(vec![&format!("{PARENT}/branch@1")]),
                ),
            ],
            json!({ format!("{PARENT}/stable@1"): "result" }),
        ),
        "known_sibling_failure" => (
            vec![
                output(&l),
                marker("root@1/c@1", "cancelled", Some(vec!["root@1/c@1/other@1"])),
            ],
            keep,
        ),
        "path_boundary" => (
            vec![output(&l), marker("root@1/c@10", "cancelled", None)],
            keep,
        ),
        "recorded_path_boundary" => (
            vec![
                output(&l),
                marker("root@1/c@1", "cancelled", Some(vec!["root@1/c@1/mid@10"])),
            ],
            keep,
        ),
        "partial_marker" => {
            let mut partial = marker(PARENT, "cancelled", None);
            partial.response.partial = true;
            (vec![output(&l), partial], keep)
        }
        "parent_output_with_error_code" => (
            vec![output(&l), with_code(output(PARENT), "finish_reason")],
            keep,
        ),
        "unknown_outcome" => (vec![output(&l), marker(PARENT, "progress", None)], keep),
        "uncommitted_delegation" => (
            vec![with_md(
                at(&l),
                WORKFLOW_DELEGATED_OUTPUT_KEY,
                json!("result"),
            )],
            empty,
        ),
        "later_success" => (
            vec![
                output(&l),
                marker("root@1/c@1", "cancelled", Some(vec![PARENT])),
                output(&l),
            ],
            keep,
        ),
        "child_failure" => (vec![output(&l), marker(&l, "failed", None)], empty),
        other => panic!("unknown case {other}"),
    }
}

#[rstest::rstest]
#[tokio::test]
async fn test_sub_scheduler_rehydrate_cache_ancestor_outcomes(
    #[values(
        "recorded_parent_path",
        "recorded_ancestor_path",
        "direct_parent_failure",
        "ancestor_unknown_cancel",
        "ancestor_empty_paths",
        "direct_parent_empty_failure",
        "direct_parent_null_paths",
        "direct_parent_invalid_type",
        "ancestor_invalid_paths",
        "legacy_ancestor_cancel",
        "failed_descendant_subtree",
        "known_sibling_failure",
        "path_boundary",
        "recorded_path_boundary",
        "partial_marker",
        "parent_output_with_error_code",
        "unknown_outcome",
        "uncommitted_delegation",
        "later_success",
        "child_failure"
    )]
    name: &str,
) {
    let (mut events, want) = ancestor_case(name);
    for ev in &mut events {
        ev.invocation_id = TEST_INVOCATION_ID.into();
    }
    let mut other = marker(PARENT, "cancelled", None);
    other.invocation_id = "different-invocation".into();
    events.push(other);
    // Exercise the persisted metadata representation, not just in-memory values.
    let persisted = round_trip(&events);
    for history in [events, persisted] {
        let sub = new_dynamic_sub_scheduler(ctx_with_events(history), PARENT.into(), noop_emit());
        let got = Value::Object(sub.state().result_by_path.clone().into_iter().collect());
        assert_eq!(
            got, want,
            "{name}: cache did not respect outcome path ancestry"
        );
    }
}

#[tokio::test]
async fn test_sub_scheduler_failure_inventory() {
    let ctx = top_level_ctx();
    let sub = new_dynamic_sub_scheduler(ctx.clone(), "parent".into(), noop_emit());
    let calls = Arc::new(AtomicI32::new(0));
    let c = Arc::clone(&calls);
    let retry = function_node::<Value, String, _, _>(
        "retry",
        move |_c: Context, _i: Value| {
            let n = c.fetch_add(1, Ordering::SeqCst) + 1;
            async move {
                if n == 1 {
                    Err(test_err("scripted failure"))
                } else {
                    Ok("ok".to_string())
                }
            }
        },
        NodeConfig::default(),
    );
    let opts = RunNodeOptions {
        run_id: Some("stable".into()),
        ..RunNodeOptions::default()
    };
    let err = sub
        .run_child(Arc::clone(&retry), Value::Null, opts.clone())
        .await
        .expect_err("fixture did not fail");
    assert!(
        find_cause::<TestError>(&err).is_some(),
        "fixture did not fail"
    );
    sub.run_child(Arc::clone(&retry), Value::Null, opts.clone())
        .await
        .expect("fixture did not recover");
    let failed = function_node::<Value, String, _, _>(
        "failed",
        |_c: Context, _i: Value| async { Err(test_err("scripted failure")) },
        NodeConfig::default(),
    );
    let failed_err = sub
        .run_child(failed, Value::Null, opts.clone())
        .await
        .expect_err("fixture did not fail its second child");
    assert!(
        find_cause::<TestError>(&failed_err).is_some(),
        "fixture did not fail its second child"
    );
    assert!(
        sub.await_or_lead("parent/inflight@1").await.is_none(),
        "fixture did not register an in-flight path"
    );
    let inventory = sub.child_failures();
    let node = dummy_node("parent");
    let wf = must_new(vec![edge(&start(), &node)]);
    let mut sched = Scheduler::new(ctx.clone(), Arc::clone(&wf.graph), 0);
    sched.state.nodes.insert(
        "parent".into(),
        NodeState {
            status: NodeStatus::Failed,
            ..NodeState::default()
        },
    );
    let ev = sched
        .completion_event("parent", Some("parent"), &inventory)
        .expect("failure record");
    let paths = ev
        .response
        .custom_metadata
        .get(WORKFLOW_FAILED_CHILD_PATHS_KEY)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        paths.len(),
        2,
        "inventory lost an uncertain path or retained duplicate/recovered paths"
    );
    assert!(
        paths.contains(&json!("parent/failed@stable"))
            && paths.contains(&json!("parent/inflight@1")),
        "inventory did not cover both failed and in-flight children"
    );
    sub.finish_run(
        "parent/inflight@1",
        Ok(json!("ok")),
        false,
        &FailureInventory::default(),
    );
}

#[rstest::rstest]
#[case::nonterminal(false)]
#[case::terminal(true)]
#[tokio::test]
async fn test_workflow_node_delegated_control_ownership(#[case] terminal: bool) {
    let compute = function_node::<Value, String, _, _>(
        "compute",
        |_c: Context, _i: Value| async { Ok("42".to_string()) },
        NodeConfig::default(),
    );
    let parent = delegating_with_integer_schema("parent", compute);
    let mut edges = vec![edge(&start(), &parent)];
    if !terminal {
        let done = function_node::<Value, Value, _, _>(
            "done",
            |_c: Context, _i: Value| async { Ok(Value::Null) },
            NodeConfig::default(),
        );
        edges.push(edge(&parent, &done));
    }
    let node: Arc<dyn Node> = WorkflowNode::new("sub", edges).expect("fixture construction failed");
    let mut s = node.run(top_level_ctx(), Value::Null);
    let mut got = None;
    while let Some(item) = s.next().await {
        let ev = item.expect("nested workflow failed");
        if let Some(out) = ev.output.filter(|v| !v.is_null()) {
            got = Some(out);
        }
    }
    let want = terminal.then(|| json!(42));
    assert_eq!(
        got, want,
        "nested workflow confused delegated control ownership or schema projection"
    );
}

/// Go `defer ended.Add(1)`: increments on drop, so it also runs when the task is aborted.
struct EndGuard(Arc<AtomicI32>);
impl Drop for EndGuard {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
// parity: adapted: Go loops 20 trials hoping map order schedules a valid node before the rejection
// and breaks the range loop; Rust's resume iterates node names sorted, so the rejected node is
// named "zinvalid" (its interrupt id stays "invalid") to make every valid node schedule first,
// the consumer drops the stream after the rejection, and `defer ended.Add(1)` is a drop guard
// (an aborted node task drops its future).
async fn test_resume_rejected_response_consumer_exit() {
    let (started, ended) = (Arc::new(AtomicI32::new(0)), Arc::new(AtomicI32::new(0)));
    let (entered_tx, mut entered_rx) = tokio::sync::mpsc::channel::<()>(10);
    let mut edges = Vec::new();
    let mut state = RunState::new();
    let mut responses = obj(json!({"invalid": "wrong"}));
    for i in 0..10 {
        let name = format!("valid{i}");
        let (st, en, tx) = (Arc::clone(&started), Arc::clone(&ended), entered_tx.clone());
        let node = dynamic_node::<Value, Value, _, _>(
            &name,
            move |ctx: Context, _i: Value, emit: Emit| {
                let (st, en, tx) = (Arc::clone(&st), Arc::clone(&en), tx.clone());
                async move {
                    st.fetch_add(1, Ordering::SeqCst);
                    let _guard = EndGuard(en);
                    let _ = tx.send(()).await;
                    emit(Event::new(ctx.platform(), ctx.invocation_id())).await?;
                    Ok(Value::Null)
                }
            },
            NodeConfig::default(),
        );
        edges.push(edge(&start(), &node));
        state.nodes.insert(
            name.clone(),
            NodeState {
                status: NodeStatus::Pending,
                resumed_inputs: Some(obj(json!({ name.clone(): "ok" }))),
                ..NodeState::default()
            },
        );
        responses.insert(name, json!("ok"));
    }
    let invalid = silent("zinvalid");
    edges.push(edge(&start(), &invalid));
    let mut ns = NodeState {
        status: NodeStatus::Failed,
        resumed_inputs: Some(obj(json!({"invalid": 1}))),
        ..NodeState::default()
    };
    ns.interrupt_schemas
        .insert("invalid".into(), json!({"type": "integer"}));
    state.nodes.insert("zinvalid".into(), ns);
    let wf = must_new(edges);
    let mut s = wf.resume(top_level_ctx(), state, responses);
    let (mut rejected, mut scheduled) = (false, false);
    while let Some(item) = s.next().await {
        if let Err(e) = item
            && is_kind(&e, WorkflowErrorKind::InvalidResumeResponse)
        {
            rejected = true;
            scheduled = tokio::time::timeout(Duration::from_secs(2), entered_rx.recv())
                .await
                .is_ok();
            break;
        }
    }
    drop(s); // consumer exit
    assert!(
        rejected,
        "fixture did not reject the failed requester's response"
    );
    assert!(
        scheduled,
        "fixture did not exercise a producer scheduled before rejection"
    );
    // Aborted node tasks drop their futures asynchronously; give the runtime a moment.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while ended.load(Ordering::SeqCst) != started.load(Ordering::SeqCst)
        && std::time::Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert_eq!(
        ended.load(Ordering::SeqCst),
        started.load(Ordering::SeqCst),
        "consumer exit left scheduled producers blocked"
    );
}

#[tokio::test]
async fn test_sub_scheduler_wait_for_output_has_no_failure_marker() {
    let child = dynamic_node::<Value, Value, _, _>(
        "wait",
        |_c: Context, _i: Value, _e: Emit| async { Ok(Value::Null) },
        wait_cfg(),
    );
    let (emit, events) = recording_emit();
    let sub = new_dynamic_sub_scheduler(top_level_ctx(), "parent".into(), emit);
    let err = sub
        .run_child(child, Value::Null, RunNodeOptions::default())
        .await
        .expect_err("fixture did not park waiting for output");
    assert!(
        is_kind(&err, WorkflowErrorKind::NodeWaitingForOutput),
        "fixture did not park waiting for output: {err}"
    );
    for ev in events.lock().iter() {
        assert_eq!(
            workflow_node_outcome(ev),
            "",
            "waiting for output was recorded as a failure"
        );
    }
    let inventory = sub.child_failures();
    assert!(
        inventory.known && inventory.paths.is_empty(),
        "waiting for output contaminated the failure inventory"
    );
}

#[rstest::rstest]
#[case::own_complete(false, true, json!("value"))]
#[case::own_uncommitted(false, false, Value::Null)]
#[case::descendant_complete(true, true, Value::Null)]
#[tokio::test]
async fn test_sub_scheduler_delegated_control_own_path(
    #[case] descendant: bool,
    #[case] completed: bool,
    #[case] want: Value,
) {
    let child = dynamic_node::<Value, Value, _, _>(
        "child",
        move |ctx: Context, _i: Value, emit: Emit| async move {
            let mut path = ctx.path();
            if descendant {
                path.push_str("/other@1");
            }
            let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
            ev.node_info = Some(NodeInfo {
                path,
                ..NodeInfo::default()
            });
            ev.response
                .custom_metadata
                .insert(WORKFLOW_NODE_COMPLETED_KEY.into(), json!(completed));
            ev.response
                .custom_metadata
                .insert(WORKFLOW_DELEGATED_OUTPUT_KEY.into(), json!("value"));
            emit(ev).await?;
            Ok(Value::Null)
        },
        NodeConfig::default(),
    );
    let sub = new_dynamic_sub_scheduler(top_level_ctx(), "parent".into(), noop_emit());
    let out = sub
        .run_child(child, Value::Null, RunNodeOptions::default())
        .await;
    assert!(
        matches!(&out, Ok(v) if *v == want),
        "dynamic result did not respect delegated completion ownership: {out:?}"
    );
}

#[tokio::test]
async fn test_workflow_delegated_output_schema_rejected() {
    let child = function_node::<Value, String, _, _>(
        "child",
        |_c: Context, _i: Value| async { Ok("not a number".to_string()) },
        NodeConfig::default(),
    );
    let parent = delegating_with_integer_schema("parent", child);
    let wf = must_new(vec![edge(&start(), &parent)]);
    let mut s = wf.run(mock_ctx());
    let mut failed = false;
    while let Some(item) = s.next().await {
        if let Err(e) = item {
            failed = failed || is_kind(&e, WorkflowErrorKind::NodeFailed);
        }
    }
    assert!(
        failed,
        "invalid delegated result bypassed the parent's output schema"
    );
}

#[rstest::rstest]
#[case::answer_invalidates_delegated_output(&["raised", "delegated", "resolved"], NodeStatus::Pending, None, false)]
#[case::own_output_before_answer_is_not_finished(&["raised", "own_old", "resolved"], NodeStatus::Pending, None, false)]
#[case::delegation_after_answer_is_not_completion(&["raised", "resolved", "delegated"], NodeStatus::Pending, None, true)]
#[case::delegation_does_not_recover_failure(&["raised", "resolved", "failed", "delegated"], NodeStatus::Failed, None, true)]
#[case::completed_with_delegation(&["raised", "resolved", "delegated", "finished"], NodeStatus::Completed, Some(json!("computed")), false)]
#[case::duplicate_answer_keeps_completion(&["raised", "resolved", "delegated", "finished", "resolved"], NodeStatus::Completed, Some(json!("computed")), false)]
fn test_reconstruct_run_state_delegated_completion(
    #[case] kinds: &[&str],
    #[case] status: NodeStatus,
    #[case] output: Option<Value>,
    #[case] cached: bool,
) {
    let parent = silent("p");
    let wf = must_new(vec![edge(&start(), &parent)]);
    let p_event = || Event {
        author: "p".into(),
        ..at("root@1/p@1")
    };
    let events: Vec<Event> = kinds
        .iter()
        .map(|k| match *k {
            "raised" => Event {
                long_running_tool_ids: vec!["approval".into()],
                ..p_event()
            },
            "delegated" => Event {
                output: Some(json!("computed")),
                node_info: Some(NodeInfo {
                    path: "root@1/p@1/compute@1".into(),
                    output_for: vec!["root@1/p@1".into()],
                    ..NodeInfo::default()
                }),
                ..Event::default()
            },
            "resolved" => resolved("approval"),
            "own_old" => Event {
                output: Some(json!("old")),
                ..p_event()
            },
            "finished" => with_md(
                with_md(p_event(), WORKFLOW_NODE_COMPLETED_KEY, json!(true)),
                WORKFLOW_DELEGATED_OUTPUT_KEY,
                json!("computed"),
            ),
            "failed" => with_md(
                p_event(),
                WORKFLOW_NODE_OUTCOME_KEY,
                json!(WORKFLOW_NODE_FAILURE_OUTCOME),
            ),
            other => panic!("unknown event kind {other}"),
        })
        .collect();
    let persisted = round_trip(&events);
    let state = reconstruct(&wf, persisted.clone());
    let ns = node_state(&state, "p");
    assert!(
        ns.status == status && ns.output == output,
        "delegation was confused with execution completion: {ns:?}"
    );
    if status == NodeStatus::Pending {
        let (outputs, _) =
            collect_node_outputs(&persisted, &build_nodes_by_name(&wf.graph), "", "root");
        assert_eq!(
            outputs.contains_key("p"),
            cached,
            "output collection did not track the resume boundary"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_workflow_cancelled_parallel_nested_children() {
    let (emitted_a, emitted_b, paused) = (Gate::new(), Gate::new(), Gate::new());
    let (attempts_a, attempts_b, failures) = (
        Arc::new(AtomicI32::new(0)),
        Arc::new(AtomicI32::new(0)),
        Arc::new(AtomicI32::new(0)),
    );
    let mk_mid = |name: &'static str, emitted: &Arc<Gate>, attempts: &Arc<AtomicI32>| {
        let (emitted, attempts) = (Arc::clone(emitted), Arc::clone(attempts));
        let leaf = dynamic_node::<Value, Value, _, _>(
            "leaf",
            move |ctx: Context, _i: Value, emit: Emit| {
                let n = attempts.fetch_add(1, Ordering::SeqCst) + 1;
                let emitted = Arc::clone(&emitted);
                async move {
                    if n > 1 {
                        return Ok(json!(format!("retried-{name}")));
                    }
                    emit(output_event(&ctx, json!(format!("partial-{name}")))).await?;
                    emitted.release();
                    ctx.cancellation_token().cancelled().await;
                    Ok(Value::Null)
                }
            },
            NodeConfig::default(),
        );
        dynamic_node::<Value, Value, _, _>(
            name,
            move |ctx: Context, input: Value, _e: Emit| {
                let leaf = Arc::clone(&leaf);
                async move { run_node(&ctx, leaf, input, RunNodeOptions::default()).await }
            },
            NodeConfig::default(),
        )
    };
    let (mid_a, mid_b) = (
        mk_mid("midA", &emitted_a, &attempts_a),
        mk_mid("midB", &emitted_b, &attempts_b),
    );
    let c = dynamic_node::<Value, Value, _, _>(
        "c",
        move |ctx: Context, input: Value, _e: Emit| {
            let (mid_a, mid_b) = (Arc::clone(&mid_a), Arc::clone(&mid_b));
            async move {
                // Go runs the two RunNode calls on goroutines; join! polls them concurrently.
                let (ra, rb) = tokio::join!(
                    run_node(&ctx, mid_a, input.clone(), RunNodeOptions::default()),
                    run_node(&ctx, mid_b, input, RunNodeOptions::default()),
                );
                Ok(json!({"a": ra?, "b": rb?}))
            }
        },
        NodeConfig::default(),
    );
    let b = asker("b", "b");
    let (fl, p, ea, eb) = (
        Arc::clone(&failures),
        Arc::clone(&paused),
        Arc::clone(&emitted_a),
        Arc::clone(&emitted_b),
    );
    let f = dynamic_node::<Value, Value, _, _>(
        "f",
        move |_c: Context, _i: Value, _e: Emit| {
            let n = fl.fetch_add(1, Ordering::SeqCst) + 1;
            let (p, ea, eb) = (Arc::clone(&p), Arc::clone(&ea), Arc::clone(&eb));
            async move {
                if n > 1 {
                    return Ok(Value::Null);
                }
                p.wait().await;
                ea.wait().await;
                eb.wait().await;
                Err(test_err("scripted failure"))
            }
        },
        NodeConfig::default(),
    );
    let join: Arc<dyn Node> = JoinNode::new("join");
    let wf = Workflow::new(
        "root",
        vec![
            edge(&start(), &b),
            edge(&start(), &c),
            edge(&start(), &f),
            edge(&b, &join),
            edge(&c, &join),
        ],
        WorkflowOptions::default(),
    )
    .expect("workflow execution failed");
    let mut history = Vec::new();
    let mut s = wf.run(mock_ctx());
    while let Some(item) = s.next().await {
        let Ok(ev) = item else { continue };
        if ev.requested_input.is_some() {
            paused.release();
        }
        history.push(ev);
    }
    drop(s);
    let state = wf
        .reconstruct_run_state(&EventsSession::new(history.clone()), TEST_INVOCATION_ID)
        .expect("workflow execution failed")
        .expect("non-nil state");
    let resume_ctx = ctx_with_events(history).with_delta(gadk_agent::ContextDelta {
        path: Some("root@1".into()),
        ..gadk_agent::ContextDelta::default()
    });
    let (_, err) = collect(wf.resume(resume_ctx, state, obj(json!({"b": "approved"})))).await;
    assert!(err.is_none(), "workflow execution failed: {err:?}");
    assert!(
        attempts_a.load(Ordering::SeqCst) == 2 && attempts_b.load(Ordering::SeqCst) == 2,
        "leaf execution counts a={} b={}; both cancelled leaves must retry",
        attempts_a.load(Ordering::SeqCst),
        attempts_b.load(Ordering::SeqCst)
    );
}

#[tokio::test]
async fn test_reconstruct_run_state_delegated_schema_conversion() {
    let q = function_node::<Value, String, _, _>(
        "q",
        |_c: Context, _i: Value| async { Ok("42".to_string()) },
        NodeConfig::default(),
    );
    let p = delegating_with_integer_schema("p", q);
    let b = asker("b", "b");
    let wf = must_new(vec![edge(&start(), &p), edge(&start(), &b)]);
    let events = drain(wf.run(mock_ctx())).await;
    let state = reconstruct(&wf, events);
    let want = p
        .validate_output(json!("42"))
        .expect("fixture validation failed");
    let ns = node_state(&state, "p");
    assert_eq!(
        ns.output,
        Some(want),
        "restored delegated output type differs from the validated type"
    );
}

#[rstest::rstest]
#[case::both_nil(false)]
#[case::one_output(true)]
#[tokio::test]
async fn test_workflow_node_nil_delegation_is_not_terminal_output(#[case] one_output: bool) {
    let mk = |name: &str, out: Value| {
        let child = dynamic_node::<Value, Value, _, _>(
            "child",
            move |_c: Context, _i: Value, _e: Emit| {
                let out = out.clone();
                async move { Ok(out) }
            },
            NodeConfig::default(),
        );
        delegating(name, child, NodeConfig::default())
    };
    let a = mk("a", Value::Null);
    let b = mk(
        "b",
        if one_output {
            json!("result")
        } else {
            Value::Null
        },
    );
    let node: Arc<dyn Node> =
        WorkflowNode::new("sub", vec![edge(&start(), &a), edge(&start(), &b)])
            .expect("fixture construction failed");
    let mut s = node.run(top_level_ctx(), Value::Null);
    while let Some(item) = s.next().await {
        assert!(
            item.is_ok(),
            "nil delegation was counted as a terminal output: {:?}",
            item.err()
        );
    }
}

#[tokio::test]
async fn test_sub_scheduler_nil_delegation_waits_for_output() {
    let child = delegating("child", silent("silent"), wait_cfg());
    let (emit, history) = recording_emit();
    let sub = new_dynamic_sub_scheduler(top_level_ctx(), "parent".into(), emit);
    let err = sub
        .run_child(child, Value::Null, RunNodeOptions::default())
        .await
        .expect_err("nil delegation bypassed WaitForOutput");
    assert!(
        is_kind(&err, WorkflowErrorKind::NodeWaitingForOutput),
        "nil delegation bypassed WaitForOutput: {err}"
    );
    for ev in history.lock().iter() {
        assert_ne!(
            ev.response.custom_metadata.get(WORKFLOW_NODE_COMPLETED_KEY),
            Some(&json!(true)),
            "nil delegation recorded completion for a parked child"
        );
    }
}

/// A dynamic node counting its calls, emitting `own` (when set) and then delegating to a
/// silent child.
fn counting_delegator(calls: &Arc<AtomicI32>, own: Option<&'static str>) -> Arc<dyn Node> {
    let calls = Arc::clone(calls);
    let silent = silent("silent");
    dynamic_node::<Value, Value, _, _>(
        "child",
        move |ctx: Context, input: Value, emit: Emit| {
            calls.fetch_add(1, Ordering::SeqCst);
            let silent = Arc::clone(&silent);
            async move {
                if let Some(own) = own {
                    emit(output_event(&ctx, json!(own))).await?;
                }
                run_node(&ctx, silent, input, use_as_output()).await?;
                Ok(Value::Null)
            }
        },
        NodeConfig::default(),
    )
}

#[tokio::test]
async fn test_sub_scheduler_nil_delegation_completed_cache() {
    let calls = Arc::new(AtomicI32::new(0));
    let child = counting_delegator(&calls, None);
    let (emit, history) = recording_emit();
    let sub = new_dynamic_sub_scheduler(top_level_ctx(), "parent".into(), emit);
    let out = sub
        .run_child(Arc::clone(&child), Value::Null, RunNodeOptions::default())
        .await;
    assert!(
        matches!(&out, Ok(Value::Null)),
        "nil delegation did not complete successfully: {out:?}"
    );
    let history = history.lock().clone();
    let resumed = new_dynamic_sub_scheduler(ctx_with_events(history), "parent".into(), noop_emit());
    let out = resumed
        .run_child(child, Value::Null, RunNodeOptions::default())
        .await;
    assert!(
        matches!(&out, Ok(Value::Null)) && calls.load(Ordering::SeqCst) == 1,
        "proven nil completion was not replayed: {out:?}"
    );
}

#[test]
fn test_reconstruct_run_state_cancellation_answered_later() {
    let parent = silent("parent");
    let wf = must_new(vec![edge(&start(), &parent)]);
    let raised = Event {
        author: "parent".into(),
        long_running_tool_ids: vec!["approval".into()],
        ..Event::default()
    };
    let cancelled = with_code(
        Event {
            author: "parent".into(),
            ..Event::default()
        },
        WORKFLOW_NODE_CANCELLED_CODE,
    );
    let state = reconstruct(&wf, vec![raised, cancelled, resolved("approval")]);
    assert_eq!(
        node_state(&state, "parent").status,
        NodeStatus::Pending,
        "legacy cancellation overrode the later answer to an outstanding pause"
    );
}

#[rstest::rstest]
#[case::direct(false)]
#[case::workflow_node(true)]
#[tokio::test]
async fn test_workflow_nil_delegation_preserves_own_output(#[case] nested: bool) {
    let silent_node = silent("silent");
    let p = dynamic_node::<Value, Value, _, _>(
        "p",
        move |ctx: Context, input: Value, emit: Emit| {
            let silent_node = Arc::clone(&silent_node);
            async move {
                emit(output_event(&ctx, json!("own-result"))).await?;
                run_node(&ctx, silent_node, input, use_as_output()).await?;
                Ok(Value::Null)
            }
        },
        NodeConfig::default(),
    );
    let producer: Arc<dyn Node> = if nested {
        WorkflowNode::new("sub", vec![edge(&start(), &p)]).expect("fixture construction failed")
    } else {
        p
    };
    let received: Arc<Mutex<Value>> = Arc::new(Mutex::new(Value::Null));
    let r = Arc::clone(&received);
    let d = function_node::<Value, Value, _, _>(
        "d",
        move |_c: Context, input: Value| {
            *r.lock() = input;
            async { Ok(Value::Null) }
        },
        NodeConfig::default(),
    );
    let ask = asker("ask", "approval");
    let wf = must_new(vec![
        edge(&start(), &producer),
        edge(&producer, &d),
        edge(&start(), &ask),
    ]);
    let history = drain(wf.run(mock_ctx())).await;
    assert_eq!(
        *received.lock(),
        json!("own-result"),
        "downstream did not receive the real output"
    );
    let state = reconstruct(&wf, round_trip(&history));
    assert_eq!(
        node_state(&state, producer.name()).output,
        Some(json!("own-result")),
        "history lost the real output after a nil completion"
    );
}

#[tokio::test]
async fn test_sub_scheduler_nil_delegation_preserves_own_output() {
    let calls = Arc::new(AtomicI32::new(0));
    let child = counting_delegator(&calls, Some("own-result"));
    let (emit, history) = recording_emit();
    let sub = new_dynamic_sub_scheduler(top_level_ctx(), "parent".into(), emit);
    let out = sub
        .run_child(Arc::clone(&child), Value::Null, RunNodeOptions::default())
        .await;
    assert!(
        matches!(&out, Ok(v) if *v == json!("own-result")),
        "nil completion discarded the child's real output: {out:?}"
    );
    let history = history.lock().clone();
    let resumed = new_dynamic_sub_scheduler(ctx_with_events(history), "parent".into(), noop_emit());
    let out = resumed
        .run_child(child, Value::Null, RunNodeOptions::default())
        .await;
    assert!(
        matches!(&out, Ok(v) if *v == json!("own-result")) && calls.load(Ordering::SeqCst) == 1,
        "nil completion overwrote the child's cached real output: {out:?}"
    );
}

#[tokio::test]
async fn test_sub_scheduler_nil_completion_control_is_not_output() {
    let child = dynamic_node::<Value, Value, _, _>(
        "child",
        |ctx: Context, _i: Value, emit: Emit| async move {
            let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
            ev.response
                .custom_metadata
                .insert(WORKFLOW_NODE_COMPLETED_KEY.into(), json!(true));
            ev.response
                .custom_metadata
                .insert(WORKFLOW_DELEGATED_OUTPUT_KEY.into(), Value::Null);
            emit(ev).await?;
            Ok(Value::Null)
        },
        wait_cfg(),
    );
    let sub = new_dynamic_sub_scheduler(top_level_ctx(), "parent".into(), noop_emit());
    let err = sub
        .run_child(child, Value::Null, RunNodeOptions::default())
        .await
        .expect_err("nil completion was mistaken for a child output");
    assert!(
        is_kind(&err, WorkflowErrorKind::NodeWaitingForOutput),
        "nil completion was mistaken for a child output: {err}"
    );
}
