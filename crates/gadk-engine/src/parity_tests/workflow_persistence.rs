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

//! Port of Go workflow/persistence_test.go.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use futures::StreamExt;
use gadk_agent::{AgentError, Context, ContextDelta};
use gadk_genai::{Content, FunctionResponse, Part};
use gadk_session::{Event, NodeInfo, RequestInput};
use serde_json::{Map, Value, json};

use crate::error::WorkflowErrorKind;
use crate::workflow::persistence::{
    WORKFLOW_BASE_PATH_PREFIX, WORKFLOW_NODE_CANCELLED_CODE, WORKFLOW_NODE_COMPLETED_KEY,
    WORKFLOW_NODE_FAILURE_CODE, collect_node_outputs, event_node_name,
};
use crate::workflow::scheduler::Scheduler;
use crate::workflow::scheduler::test_support::*;
use crate::workflow::state::{NodeState, NodeStatus, RunState};
use crate::workflow::{
    Emit, JoinNode, Node, NodeConfig, RunNodeOptions, WORKFLOW_INPUT_FUNCTION_CALL_NAME, Workflow,
    WorkflowOptions, dynamic_node, function_node, resume_or_request_input, run_node,
};

fn nodes_map(names: &[&str]) -> HashMap<String, Arc<dyn Node>> {
    names
        .iter()
        .map(|n| (n.to_string(), dummy_node(n)))
        .collect()
}

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => panic!("not an object"),
    }
}

/// Go `&session.Event{Author: "user"}` whose content is
/// `genai.NewContentFromFunctionResponse(WorkflowInputFunctionCallName, resp, RoleUser)` with
/// the FunctionResponse id set to `id`.
fn user_response(id: &str, resp: Value) -> Event {
    let mut ev = Event {
        author: "user".into(),
        ..Event::default()
    };
    ev.response.content = Some(Content::from_function_response(
        FunctionResponse {
            id: id.into(),
            name: WORKFLOW_INPUT_FUNCTION_CALL_NAME.into(),
            response: obj(resp),
            ..FunctionResponse::default()
        },
        "user",
    ));
    ev
}

fn raised(author: &str, id: &str) -> Event {
    Event {
        author: author.into(),
        long_running_tool_ids: vec![id.into()],
        ..Event::default()
    }
}

fn with_error_code(mut ev: Event, code: &str) -> Event {
    ev.response.error_code = code.into();
    ev
}

/// Session services round-trip events through JSON.
fn round_trip(events: &[Event]) -> Vec<Event> {
    serde_json::from_value(serde_json::to_value(events).expect("history serialization failed"))
        .expect("history deserialization failed")
}

/// Port of Go nodeState: the reconstructed state of `name`, failing the test when absent.
fn node_state<'a>(state: &'a Option<RunState>, name: &str) -> &'a NodeState {
    let state = state
        .as_ref()
        .unwrap_or_else(|| panic!("ReconstructRunState returned nil state, want node {name:?}"));
    state
        .nodes
        .get(name)
        .unwrap_or_else(|| panic!("no NodeState for {name:?}; nodes = {:?}", state.nodes))
}

fn reconstruct(wf: &Arc<Workflow>, events: Vec<Event>, invocation_id: &str) -> Option<RunState> {
    wf.reconstruct_run_state(&EventsSession::new(events), invocation_id)
        .expect("ReconstructRunState")
}

/// Port of Go modelEvent.
fn model_event(path: &str, text: &str, message_as_output: bool) -> Event {
    let mut ev = Event {
        node_info: Some(NodeInfo {
            path: path.into(),
            message_as_output,
            ..NodeInfo::default()
        }),
        ..Event::default()
    };
    ev.response.content = Some(Content {
        role: "model".into(),
        parts: vec![Part::from_text(text)],
    });
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

#[rstest::rstest]
#[case::own("root@1/p@1", vec![], false, "", None, json!({"p": "result"}))]
#[case::discarded_child("root@1/p@1/q@1", vec!["root@1/p@1/q@1"], false, "", None, json!({}))]
#[case::delegated_child("root@1/p@1/q@1", vec!["root@1/p@1/q@1", "root@1/p@1"], false, "", None, json!({"p": "result"}))]
#[case::legacy_author("", vec![], false, "", None, json!({"p": "result"}))]
#[case::partial("root@1/p@1", vec![], true, "", None, json!({}))]
#[case::same_workflow_and_node_name("p@1/p@1", vec![], false, "p", None, json!({"p": "result"}))]
#[case::unprefixed_dynamic_child("p@1/q@1", vec![], false, "p", Some(""), json!({}))]
fn test_collect_node_outputs_ownership(
    #[case] path: &str,
    #[case] output_for: Vec<&str>,
    #[case] partial: bool,
    #[case] workflow_name: &str,
    #[case] base_path: Option<&str>,
    #[case] want: Value,
) {
    let nodes = nodes_map(&["p", "q"]);
    let mut ev = Event {
        author: "p".into(),
        output: Some(json!("result")),
        node_info: Some(NodeInfo {
            path: path.into(),
            output_for: output_for.iter().map(|s| s.to_string()).collect(),
            ..NodeInfo::default()
        }),
        ..Event::default()
    };
    ev.response.partial = partial;
    if let Some(base) = base_path {
        ev.response.custom_metadata.insert(
            format!("{WORKFLOW_BASE_PATH_PREFIX}{workflow_name}"),
            json!(base),
        );
    }
    let (outputs, _) = collect_node_outputs(&[ev], &nodes, "", workflow_name);
    let got = Value::Object(outputs.into_iter().collect());
    assert_eq!(got, want, "output was attributed to the wrong graph node");
}

#[rstest::rstest]
#[case::run_node(false)]
#[case::run(true)]
#[tokio::test]
async fn test_reconstruct_run_state_workflow_name_matches_node(#[case] prefixed: bool) {
    let ask = asker("ask", "approval");
    let done = function_node::<Value, String, _, _>(
        "done",
        |_c: Context, _i: Value| async { Ok("result".to_string()) },
        NodeConfig::default(),
    );
    let wf = Workflow::new(
        "done",
        vec![edge(&start(), &ask), edge(&start(), &done)],
        WorkflowOptions::default(),
    )
    .expect("New");
    let ctx = mock_ctx();
    let stream = if prefixed {
        wf.run(ctx)
    } else {
        wf.run_node(ctx.into_context(), Value::Null)
    };
    let events = drain(stream).await;
    let state = reconstruct(&wf, events, "");
    assert_eq!(
        node_state(&state, "ask").status,
        NodeStatus::Waiting,
        "workflow name shadowed a graph node's event ownership"
    );
    assert_eq!(
        node_state(&state, "done").output,
        Some(json!("result")),
        "workflow name shadowed a graph node's event ownership"
    );
}

#[rstest::rstest]
#[case::output_then_failure(&["output", "failure"], NodeStatus::Failed, None)]
#[case::failure_then_result(&["failure", "output"], NodeStatus::Completed, Some(json!("result")))]
#[case::failure_then_nil_success(&["failure", "completed"], NodeStatus::Completed, None)]
#[case::nil_success(&["completed"], NodeStatus::Completed, None)]
#[case::later_nil_success_clears_old_output(&["output", "completed"], NodeStatus::Completed, None)]
fn test_reconstruct_run_state_latest_outcome(
    #[case] kinds: &[&str],
    #[case] status: NodeStatus,
    #[case] output: Option<Value>,
) {
    let (ask, sibling) = (dummy_node("ask"), dummy_node("sibling"));
    let wf = Workflow::new(
        "root",
        vec![edge(&start(), &ask), edge(&start(), &sibling)],
        WorkflowOptions::default(),
    )
    .expect("New");
    let sibling_event = || Event {
        author: "sibling".into(),
        branch: "root.sibling".into(),
        ..Event::default()
    };
    let mut events = vec![raised("ask", "approval")];
    for kind in kinds {
        let mut ev = sibling_event();
        match *kind {
            "output" => ev.output = Some(json!("result")),
            "failure" => ev.response.error_code = WORKFLOW_NODE_FAILURE_CODE.into(),
            _ => {
                ev.response
                    .custom_metadata
                    .insert(WORKFLOW_NODE_COMPLETED_KEY.into(), json!(true));
            }
        }
        events.push(ev);
    }
    let state = reconstruct(&wf, round_trip(&events), "");
    let ns = node_state(&state, "sibling");
    assert!(
        ns.status == status && ns.output == output && ns.branch == "root.sibling",
        "latest outcome was not reconstructed with its output and branch: {ns:?}"
    );
    if status == NodeStatus::Failed {
        assert!(
            !state
                .as_ref()
                .is_some_and(|s| s.completed.contains("sibling")),
            "failed node would suppress a later retry"
        );
    }
}

#[rstest::rstest]
#[case::not_rerun_yet("not_rerun_yet")]
#[case::already_finished("already_finished")]
#[case::partial_result("partial_result")]
fn test_reconstruct_run_state_previously_resumed_reentry(#[case] outcome: &str) {
    let c = function_node::<Value, String, _, _>(
        "c",
        |_c: Context, _i: Value| async { Ok("result".to_string()) },
        NodeConfig {
            rerun_on_resume: Some(true),
            ..NodeConfig::default()
        },
    );
    let wf =
        Workflow::new("root", vec![edge(&start(), &c)], WorkflowOptions::default()).expect("New");
    let mut events = vec![
        raised("c", "approval"),
        user_response("approval", json!({"response": "approved"})),
    ];
    let mut want = NodeStatus::Pending;
    if outcome != "not_rerun_yet" {
        let mut ev = Event {
            author: "c".into(),
            output: Some(json!("result")),
            ..Event::default()
        };
        ev.response.partial = outcome == "partial_result";
        if !ev.response.partial {
            want = NodeStatus::Completed;
        }
        events.push(ev);
    }
    let state = reconstruct(&wf, events, "");
    assert_eq!(
        node_state(&state, "c").status,
        want,
        "previously resolved interrupt changed the latest node outcome"
    );
}

#[tokio::test]
async fn test_resume_failed_sibling_requires_matched_interrupt() {
    let attempts = Arc::new(AtomicI32::new(0));
    let a = Arc::clone(&attempts);
    let ask = dummy_node("ask");
    let failed = function_node::<Value, String, _, _>(
        "failed",
        move |_c: Context, _i: Value| {
            a.fetch_add(1, Ordering::SeqCst);
            async { Ok("retried".to_string()) }
        },
        NodeConfig::default(),
    );
    let wf = Workflow::new(
        "root",
        vec![edge(&start(), &ask), edge(&start(), &failed)],
        WorkflowOptions::default(),
    )
    .expect("New");
    let mut state = RunState::new();
    state.nodes.insert(
        "ask".into(),
        NodeState {
            status: NodeStatus::Waiting,
            interrupts: vec!["approval".into()],
            ..NodeState::default()
        },
    );
    state.nodes.insert(
        "failed".into(),
        NodeState {
            status: NodeStatus::Failed,
            ..NodeState::default()
        },
    );
    let (_, err) =
        collect(wf.resume(top_level_ctx(), state, obj(json!({"unknown": "response"})))).await;
    assert!(
        err.as_ref()
            .is_some_and(|e| is_kind(e, WorkflowErrorKind::NothingToResume))
            && attempts.load(Ordering::SeqCst) == 0,
        "unmatched response retried a failed sibling: {err:?}"
    );
}

#[rstest::rstest]
#[case::pause_not_consumed(false)]
#[case::reentry_cancelled(true)]
fn test_reconstruct_run_state_cancellation_after_interrupt(#[case] answered: bool) {
    let ask = dummy_node("ask");
    let wf = must_new(vec![edge(&start(), &ask)]);
    let mut events = vec![raised("ask", "approval")];
    let mut want = NodeStatus::Waiting;
    if answered {
        events.push(user_response("approval", json!({"response": "approved"})));
        want = NodeStatus::Failed;
    }
    events.push(with_error_code(
        Event {
            author: "ask".into(),
            ..Event::default()
        },
        WORKFLOW_NODE_CANCELLED_CODE,
    ));
    let state = reconstruct(&wf, events, "");
    assert_eq!(
        node_state(&state, "ask").status,
        want,
        "cancellation confused an outstanding pause with failed re-entry"
    );
}

#[tokio::test]
async fn test_resume_failed_requester_receives_response() {
    let (called, received) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );
    let (c, r) = (Arc::clone(&called), Arc::clone(&received));
    let ask = function_node::<Value, String, _, _>(
        "ask",
        move |ctx: Context, _i: Value| {
            c.store(true, Ordering::SeqCst);
            r.store(
                ctx.resumed_input("approval") == Some(json!("corrected")),
                Ordering::SeqCst,
            );
            async { Ok("result".to_string()) }
        },
        NodeConfig::default(),
    );
    let wf = must_new(vec![edge(&start(), &ask)]);
    let mut state = RunState::new();
    state.nodes.insert(
        "ask".into(),
        NodeState {
            status: NodeStatus::Failed,
            resumed_inputs: Some(obj(json!({"approval": "approved"}))),
            ..NodeState::default()
        },
    );
    let (_, err) = collect(wf.resume(
        top_level_ctx(),
        state,
        obj(json!({"approval": "corrected"})),
    ))
    .await;
    assert!(err.is_none(), "unexpected error: {err:?}");
    assert!(
        called.load(Ordering::SeqCst),
        "matched response did not retry the failed requester"
    );
    assert!(
        received.load(Ordering::SeqCst),
        "failed requester did not receive its response on retry"
    );
}

#[tokio::test]
async fn test_resume_invalid_failed_response_does_not_retry() {
    let called = Arc::new(AtomicBool::new(false));
    let c = Arc::clone(&called);
    let ask = dummy_node("ask");
    let failed = function_node::<Value, Value, _, _>(
        "failed",
        move |_c: Context, _i: Value| {
            c.store(true, Ordering::SeqCst);
            async { Ok(Value::Null) }
        },
        NodeConfig::default(),
    );
    let wf = must_new(vec![edge(&start(), &ask), edge(&start(), &failed)]);
    let mut state = RunState::new();
    state.nodes.insert(
        "ask".into(),
        NodeState {
            status: NodeStatus::Waiting,
            interrupts: vec!["ask".into()],
            ..NodeState::default()
        },
    );
    let mut ns = NodeState {
        status: NodeStatus::Failed,
        resumed_inputs: Some(obj(json!({"approval": 1}))),
        ..NodeState::default()
    };
    ns.interrupt_schemas
        .insert("approval".into(), json!({"type": "integer"}));
    state.nodes.insert("failed".into(), ns);
    let mut s = wf.resume(
        top_level_ctx(),
        state,
        obj(json!({"ask": "approved", "approval": "invalid"})),
    );
    let mut rejected = false;
    while let Some(item) = s.next().await {
        if let Err(e) = item
            && is_kind(&e, WorkflowErrorKind::InvalidResumeResponse)
        {
            rejected = true;
        }
    }
    assert!(
        rejected && !called.load(Ordering::SeqCst),
        "invalid corrected response retried a failed requester"
    );
}

#[tokio::test]
// parity: adapted: Go breaks the range loop after the first event; Rust drops the stream.
async fn test_scheduler_no_output_completion_consumer_exit() {
    let n = dynamic_node::<Value, Value, _, _>(
        "silent",
        |_c: Context, _i: Value, _e: Emit| async { Ok(Value::Null) },
        NodeConfig::default(),
    );
    let wf = must_new(vec![edge(&start(), &n)]);
    let mut s = wf.run(mock_ctx());
    let first = s.next().await.expect("an event").expect("no error");
    drop(s);
    assert_eq!(
        first
            .response
            .custom_metadata
            .get(WORKFLOW_NODE_COMPLETED_KEY),
        Some(&json!(true)),
        "silent node did not produce a completion record"
    );
}

#[tokio::test]
async fn test_scheduler_no_output_activation_clears_previous_result() {
    let n = dynamic_node::<Value, Value, _, _>(
        "silent",
        |_c: Context, _i: Value, _e: Emit| async { Ok(Value::Null) },
        NodeConfig::default(),
    );
    let wf = must_new(vec![edge(&start(), &n)]);
    let mut s = Scheduler::new(top_level_ctx(), Arc::clone(&wf.graph), 0);
    s.state.nodes.insert(
        "silent".into(),
        NodeState {
            status: NodeStatus::Completed,
            output: Some(json!("old result")),
            ..NodeState::default()
        },
    );
    s.schedule_node(Arc::clone(&n), None, String::new(), String::new());
    let mut saw_completion = false;
    let mut events = Vec::new();
    while let Some(item) = s.next().await {
        let ev = item.expect("unexpected error");
        if ev.response.custom_metadata.get(WORKFLOW_NODE_COMPLETED_KEY) == Some(&json!(true)) {
            saw_completion = true;
        }
        events.push(ev);
    }
    let ns = &s.state.nodes["silent"];
    assert!(
        saw_completion && ns.status == NodeStatus::Completed && ns.output.is_none(),
        "silent activation retained a previous activation's result"
    );
    let (outputs, _) = collect_node_outputs(
        &events,
        &crate::workflow::scheduler::build_nodes_by_name(&wf.graph),
        "",
        "",
    );
    assert_eq!(outputs.get("silent"), Some(&Value::Null));
}

#[rstest::rstest]
#[case::child(false, false)]
#[case::nested_child(true, false)]
#[case::nested_context_error(true, true)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_workflow_cancelled_dynamic_child_does_not_replay_output(
    #[case] nested: bool,
    #[case] context_error: bool,
) {
    let (paused, child_emitted) = (Gate::new(), Gate::new());
    let attempts = Arc::new(AtomicI32::new(0));
    let failures = Arc::new(AtomicI32::new(0));
    let stable_attempts = Arc::new(AtomicI32::new(0));
    let sa = Arc::clone(&stable_attempts);
    let stable = function_node::<Value, String, _, _>(
        "stable",
        move |_c: Context, _i: Value| {
            sa.fetch_add(1, Ordering::SeqCst);
            async { Ok("stable".to_string()) }
        },
        NodeConfig::default(),
    );
    let (at, ce) = (Arc::clone(&attempts), Arc::clone(&child_emitted));
    let mut q = dynamic_node::<Value, Value, _, _>(
        "q",
        move |ctx: Context, _i: Value, emit: Emit| {
            let n = at.fetch_add(1, Ordering::SeqCst) + 1;
            let ce = Arc::clone(&ce);
            async move {
                if n > 1 {
                    return Ok(json!("retried"));
                }
                emit(output_event(&ctx, json!("partial"))).await?;
                ce.release();
                ctx.cancellation_token().cancelled().await;
                Ok(Value::Null)
            }
        },
        NodeConfig::default(),
    );
    if nested {
        let leaf = Arc::clone(&q);
        q = dynamic_node::<Value, Value, _, _>(
            "mid",
            move |ctx: Context, input: Value, _e: Emit| {
                let leaf = Arc::clone(&leaf);
                async move {
                    let out = run_node(&ctx, leaf, input, RunNodeOptions::default()).await;
                    if context_error && ctx.cancellation_token().is_cancelled() {
                        return Err(AgentError::Cancelled);
                    }
                    out
                }
            },
            NodeConfig::default(),
        );
    }
    let c = dynamic_node::<Value, Value, _, _>(
        "c",
        move |ctx: Context, input: Value, _e: Emit| {
            let (stable, q) = (Arc::clone(&stable), Arc::clone(&q));
            async move {
                run_node(&ctx, stable, input.clone(), RunNodeOptions::default()).await?;
                run_node(&ctx, q, input, RunNodeOptions::default()).await
            }
        },
        NodeConfig::default(),
    );
    let b = asker("b", "b");
    let (fl, p, ce) = (
        Arc::clone(&failures),
        Arc::clone(&paused),
        Arc::clone(&child_emitted),
    );
    let f = dynamic_node::<Value, Value, _, _>(
        "f",
        move |_c: Context, _i: Value, _e: Emit| {
            let n = fl.fetch_add(1, Ordering::SeqCst) + 1;
            let (p, ce) = (Arc::clone(&p), Arc::clone(&ce));
            async move {
                if n > 1 {
                    return Ok(Value::Null);
                }
                p.wait().await;
                ce.wait().await;
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
    .expect("New");
    let mut events = Vec::new();
    let mut saw_failure = false;
    let mut s = wf.run(mock_ctx());
    while let Some(item) = s.next().await {
        match item {
            Err(e) => saw_failure = find_cause::<TestError>(&e).is_some(),
            Ok(ev) => {
                if ev.requested_input.is_some() {
                    paused.release();
                }
                events.push(ev);
            }
        }
    }
    drop(s);
    assert!(saw_failure, "fixture did not cancel the dynamic child");
    let state = reconstruct(&wf, events.clone(), TEST_INVOCATION_ID).expect("non-nil state");
    let resume_ctx = ctx_with_events(events).with_delta(ContextDelta {
        path: Some("root@1".into()),
        ..ContextDelta::default()
    });
    let mut s = wf.resume(resume_ctx, state, obj(json!({"b": "approved"})));
    let mut saw_join = false;
    while let Some(item) = s.next().await {
        let ev = item.expect("resume error");
        if let Some(out @ Value::Object(_)) = &ev.output {
            saw_join = true;
            assert_eq!(
                out,
                &json!({"b": "approved", "c": "retried"}),
                "cancelled child output was replayed as a successful result"
            );
        }
    }
    assert!(
        saw_join
            && attempts.load(Ordering::SeqCst) == 2
            && stable_attempts.load(Ordering::SeqCst) == 1,
        "cancelled child was not executed again on resume (join={saw_join}, attempts={}, stable={})",
        attempts.load(Ordering::SeqCst),
        stable_attempts.load(Ordering::SeqCst)
    );
}

#[test]
fn test_collect_node_outputs_message_as_output() {
    let nodes = nodes_map(&["talky"]);
    let events = vec![model_event("talky", "Hello, world!", true)];
    let (outputs, completed) = collect_node_outputs(&events, &nodes, "", "");
    assert_eq!(outputs.get("talky"), Some(&json!("Hello, world!")));
    assert!(
        completed.contains("talky"),
        "completed[talky] = false, want true"
    );
}

#[test]
fn test_collect_node_outputs_message_not_flagged() {
    let nodes = nodes_map(&["talky"]);
    let events = vec![model_event("talky", "Hello, world!", false)];
    let (outputs, _) = collect_node_outputs(&events, &nodes, "", "");
    assert!(
        !outputs.contains_key("talky"),
        "outputs[talky] = {:?}, want absent",
        outputs.get("talky")
    );
}

#[test]
fn test_collect_node_outputs_explicit_output_wins() {
    let nodes = nodes_map(&["talky"]);
    let mut ev = model_event("talky", "from message", true);
    ev.output = Some(json!("explicit"));
    let (outputs, _) = collect_node_outputs(&[ev], &nodes, "", "");
    assert_eq!(outputs.get("talky"), Some(&json!("explicit")));
}

#[test]
fn test_collect_node_outputs_output_for_attributes_ancestors() {
    let nodes = nodes_map(&["child", "outer"]);
    let ev = Event {
        output: Some(json!("delegated")),
        node_info: Some(NodeInfo {
            path: "child/gc@1".into(),
            output_for: vec!["child/gc@1".into(), "child".into(), "outer".into()],
            ..NodeInfo::default()
        }),
        ..Event::default()
    };
    let (outputs, _) = collect_node_outputs(&[ev], &nodes, "", "");
    assert_eq!(outputs.get("child"), Some(&json!("delegated")));
    assert_eq!(
        outputs.get("outer"),
        Some(&json!("delegated")),
        "ancestor not attributed"
    );
}

#[rstest::rstest]
#[case::nil_node_info_falls_back_to_author(None, "authorNode")]
#[case::empty_path_falls_back_to_author(Some(""), "authorNode")]
#[case::static_node_path(Some("nodeA"), "nodeA")]
#[case::node_path_with_invocation_id(Some("nodeA@1"), "nodeA")]
#[case::hierarchical_path_matches_static_parent(Some("parent/child@1"), "parent")]
#[case::hierarchical_path_matches_child_when_parent_unknown(Some("unknown/child@2"), "child")]
#[case::path_segments_not_in_nodes_map_falls_back_to_author(Some("unknown/other@1"), "authorNode")]
fn test_event_node_name(#[case] path: Option<&str>, #[case] want: &str) {
    let nodes = nodes_map(&["nodeA", "parent", "child"]);
    let ev = Event {
        author: "authorNode".into(),
        node_info: path.map(|p| NodeInfo {
            path: p.into(),
            ..NodeInfo::default()
        }),
        ..Event::default()
    };
    assert_eq!(event_node_name(&ev, &nodes), want);
}

#[test]
fn test_reconstruct_run_state_invocation_scope() {
    let ask = dummy_node("ask");
    let wf = Workflow::new(
        "hitl-rerun",
        vec![edge(&start(), &ask)],
        WorkflowOptions::default(),
    )
    .expect("New");
    let raise = |inv: &str| Event {
        invocation_id: inv.into(),
        ..raised("ask", "iid")
    };
    let resolve = |inv: &str| {
        let mut ev = Event {
            author: "user".into(),
            invocation_id: inv.into(),
            ..Event::default()
        };
        ev.response.content = Some(Content {
            parts: vec![Part {
                function_response: Some(FunctionResponse {
                    id: "iid".into(),
                    response: obj(json!({"payload": "first"})),
                    ..FunctionResponse::default()
                }),
                ..Part::default()
            }],
            ..Content::default()
        });
        ev
    };
    let events = vec![raise("inv1"), resolve("inv1"), raise("inv2")];

    let state = reconstruct(&wf, events.clone(), "inv2");
    let ns = node_state(&state, "ask");
    assert!(
        ns.status == NodeStatus::Waiting && ns.interrupts == vec!["iid".to_string()],
        "inv2 ask = {ns:?}, want NodeWaiting on interrupt [iid]"
    );

    let state = reconstruct(&wf, events, "inv1");
    let ns = node_state(&state, "ask");
    assert!(
        ns.status == NodeStatus::Completed && ns.interrupts.is_empty(),
        "inv1 ask = {ns:?}, want NodeCompleted with no pending interrupts"
    );
}

#[rstest::rstest]
#[case::explicit_output(false)]
#[case::message_output(true)]
fn test_reconstruct_run_state_completed_join_predecessors(#[case] message_output: bool) {
    let (ask, done, old, streaming) = (
        dummy_node("ask"),
        dummy_node("done"),
        dummy_node("old"),
        dummy_node("streaming"),
    );
    let join: Arc<dyn Node> = JoinNode::new("join");
    let wf = Workflow::new(
        "root",
        vec![
            edge(&start(), &ask),
            edge(&start(), &done),
            edge(&start(), &old),
            edge(&start(), &streaming),
            edge(&ask, &join),
            edge(&done, &join),
        ],
        WorkflowOptions::default(),
    )
    .expect("New");
    let mut completed = model_event("done", "saved", message_output);
    completed.invocation_id = "current".into();
    completed.branch = "outer@1.done@1".into();
    if !message_output {
        completed.output = Some(json!("saved"));
    }
    let pending = Event {
        invocation_id: "current".into(),
        output: Some(json!("intermediate")),
        ..raised("ask", "approval")
    };
    let previous = Event {
        author: "old".into(),
        invocation_id: "previous".into(),
        output: Some(json!("obsolete")),
        ..Event::default()
    };
    let mut unfinished = model_event("streaming", "partial", false);
    unfinished.invocation_id = "current".into();
    let branch = completed.branch.clone();
    let state = reconstruct(
        &wf,
        vec![previous, completed, unfinished, pending],
        "current",
    );
    let ns = node_state(&state, "done");
    assert!(
        ns.status == NodeStatus::Completed
            && ns.output == Some(json!("saved"))
            && ns.branch == branch,
        "completed sibling lost its output, branch or completed status"
    );
    assert_eq!(
        node_state(&state, "ask").status,
        NodeStatus::Waiting,
        "interrupted node was marked completed"
    );
    for name in ["old", "streaming"] {
        assert!(
            !state.as_ref().unwrap().nodes.contains_key(name),
            "{name} incorrectly restored as completed"
        );
    }
}
