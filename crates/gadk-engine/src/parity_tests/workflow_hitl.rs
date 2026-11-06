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

//! Port of Go workflow/hitl_test.go.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::StreamExt;
use gadk_agent::{Context, ContextDelta};
use gadk_session::{Event, RequestInput};
use serde_json::{Map, Value, json};

use crate::error::WorkflowErrorKind;
use crate::workflow::scheduler::Scheduler;
use crate::workflow::scheduler::test_support::*;
use crate::workflow::state::NodeStatus;
use crate::workflow::{
    Emit, NodeConfig, RunNodeOptions, START, Workflow, dynamic_node, new_request_input_event,
    resume_or_request_input, run_node,
};

/// Port of Go findRequestedInputEvent: the first RequestedInput and how many there were.
fn find_requested_input_event(events: &[Event]) -> (Option<&RequestInput>, usize) {
    let reqs: Vec<&RequestInput> = events
        .iter()
        .filter_map(|e| e.requested_input.as_ref())
        .collect();
    (reqs.first().copied(), reqs.len())
}

/// Port of Go newHitlNode with a closure that only flags that it ran.
fn flag_node(name: &str, ran: &Arc<AtomicBool>) -> Arc<dyn crate::workflow::Node> {
    let r = Arc::clone(ran);
    TestNode::arc(name, NodeConfig::default(), move |_, _| {
        r.store(true, Ordering::SeqCst);
        futures::stream::empty().boxed()
    })
}

/// Drives `w`'s scheduler directly (Go `newScheduler` + `scheduleNode(Start)` + `run`) so the
/// test can inspect the final run state; returns the events, the first error and the scheduler.
async fn drive(
    w: &Arc<Workflow>,
    ctx: Context,
) -> (Vec<Event>, Option<gadk_agent::AgentError>, Scheduler) {
    let mut s = Scheduler::new(ctx.clone(), Arc::clone(&w.graph), w.max_concurrency);
    s.workflow_name = w.name.clone();
    let input = Some(json!("seed"));
    s.state.ensure_node(START.name()).input = input.clone();
    s.schedule_node(Arc::clone(&START), input, String::new(), ctx.branch());
    let (mut events, mut first) = (Vec::new(), None);
    while let Some(item) = s.next().await {
        match item {
            Ok(ev) => events.push(ev),
            Err(e) => {
                if first.is_none() {
                    first = Some(e);
                }
            }
        }
    }
    (events, first, s)
}

#[tokio::test]
async fn test_scheduler_hitl_node_pauses_and_forwards_request() {
    let downstream_ran = Arc::new(AtomicBool::new(false));
    let asker = TestNode::arc("asker", NodeConfig::default(), |ctx, _| {
        once(new_request_input_event(
            ctx.as_invocation(),
            RequestInput {
                interrupt_id: "human_review".into(),
                message: "Please approve".into(),
                payload: Some(json!("the draft")),
                ..RequestInput::default()
            },
        ))
    });
    let downstream = flag_node("downstream", &downstream_ran);
    let w = must_new(vec![edge(&start(), &asker), edge(&asker, &downstream)]);
    let events = drain(w.run(seeded_mock_ctx())).await;
    assert!(
        !downstream_ran.load(Ordering::SeqCst),
        "downstream node ran; HITL pause must suppress successor scheduling"
    );
    let (req, count) = find_requested_input_event(&events);
    assert_eq!(
        count,
        1,
        "expected exactly 1 event with RequestedInput (events={})",
        events.len()
    );
    let req = req.expect("request");
    assert_eq!(req.interrupt_id, "human_review");
    assert_eq!(req.message, "Please approve");
    assert_eq!(req.payload, Some(json!("the draft")));
}

#[tokio::test]
async fn test_scheduler_hitl_node_auto_generates_interrupt_id() {
    let asker = TestNode::arc("asker", NodeConfig::default(), |ctx, _| {
        once(new_request_input_event(
            ctx.as_invocation(),
            RequestInput {
                message: "approve?".into(),
                ..RequestInput::default()
            },
        ))
    });
    let w = must_new(vec![edge(&start(), &asker)]);
    let events = drain(w.run(seeded_mock_ctx())).await;
    let (req, count) = find_requested_input_event(&events);
    assert_eq!(count, 1, "expected exactly 1 RequestedInput event");
    assert!(
        !req.expect("request").interrupt_id.is_empty(),
        "InterruptID is empty; engine must auto-generate when caller omits it"
    );
}

#[tokio::test]
async fn test_scheduler_hitl_node_preserves_explicit_interrupt_id() {
    let asker = TestNode::arc("asker", NodeConfig::default(), |ctx, _| {
        once(new_request_input_event(
            ctx.as_invocation(),
            RequestInput {
                interrupt_id: "stable-id-42".into(),
                message: "approve?".into(),
                ..RequestInput::default()
            },
        ))
    });
    let w = must_new(vec![edge(&start(), &asker)]);
    let events = drain(w.run(seeded_mock_ctx())).await;
    let (req, _) = find_requested_input_event(&events);
    let req = req.expect("no RequestedInput event found");
    assert_eq!(
        req.interrupt_id, "stable-id-42",
        "engine must preserve caller-supplied IDs"
    );
}

#[tokio::test]
// parity: adapted: Go runs w.Run and checks the events' LongRunningToolIDs; Rust drives the
// scheduler directly so it can also check the parked NodeState.
async fn test_scheduler_hitl_node_multiple_requests_park() {
    let asker = TestNode::arc("asker", NodeConfig::default(), |ctx, _| {
        let c = ctx.clone();
        async_stream::stream! {
            for id in ["first", "second"] {
                yield Ok(new_request_input_event(c.as_invocation(), RequestInput { interrupt_id: id.into(), ..RequestInput::default() }));
            }
        }
        .boxed()
    });
    let w = must_new(vec![edge(&start(), &asker)]);
    let (events, err, s) = drive(&w, seeded_mock_ctx().into_context()).await;
    assert!(
        err.is_none(),
        "multiple interrupts must park cleanly: {err:?}"
    );
    let ids: Vec<&String> = events
        .iter()
        .flat_map(|e| e.long_running_tool_ids.iter())
        .collect();
    assert!(
        ids.contains(&&"first".to_string()) && ids.contains(&&"second".to_string()),
        "long-running interrupts = {ids:?}, want both first and second"
    );
    let ns = &s.state.nodes["asker"];
    assert_eq!(ns.status, NodeStatus::Waiting);
    assert_eq!(
        ns.interrupts,
        vec!["first".to_string(), "second".to_string()]
    );
}

#[tokio::test]
async fn test_scheduler_hitl_node_error_after_request_fails() {
    let asker = TestNode::arc("asker", NodeConfig::default(), |ctx, _| {
        let c = ctx.clone();
        async_stream::stream! {
            yield Ok(new_request_input_event(c.as_invocation(), RequestInput { interrupt_id: "ignored".into(), ..RequestInput::default() }));
            yield Err(test_err("downstream of request"));
        }
        .boxed()
    });
    let w = must_new(vec![edge(&start(), &asker)]);
    let (_, err, s) = drive(&w, seeded_mock_ctx().into_context()).await;
    let err = err.expect("expected an error from Run, got none");
    assert_eq!(
        find_cause::<TestError>(&err),
        Some(&TestError("downstream of request")),
        "Run error = {err}; failure must take precedence over the recorded request"
    );
    assert_eq!(s.state.nodes["asker"].status, NodeStatus::Failed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_scheduler_hitl_node_concurrent_branches_pauses_only_when_all_non_running() {
    let hitl_downstream_ran = Arc::new(AtomicBool::new(false));
    let plain_downstream_ran = Arc::new(AtomicBool::new(false));
    let hitl_asker = TestNode::arc("hitl_asker", NodeConfig::default(), |ctx, _| {
        once(new_request_input_event(
            ctx.as_invocation(),
            RequestInput {
                interrupt_id: "ask".into(),
                ..RequestInput::default()
            },
        ))
    });
    let hitl_downstream = flag_node("hitl_downstream", &hitl_downstream_ran);
    let plain = TestNode::arc("plain", NodeConfig::default(), |ctx, _| {
        once(output_event(&ctx, json!("done")))
    });
    let plain_downstream = flag_node("plain_downstream", &plain_downstream_ran);
    let w = must_new(vec![
        edge(&start(), &hitl_asker),
        edge(&start(), &plain),
        edge(&hitl_asker, &hitl_downstream),
        edge(&plain, &plain_downstream),
    ]);
    let events = drain(w.run(seeded_mock_ctx())).await;
    assert!(
        !hitl_downstream_ran.load(Ordering::SeqCst),
        "hitl_downstream ran; HITL branch must not schedule successors before resume"
    );
    assert!(
        plain_downstream_ran.load(Ordering::SeqCst),
        "plain_downstream did not run; non-HITL branch must complete normally even while a sibling pauses"
    );
    assert_eq!(
        find_requested_input_event(&events).1,
        1,
        "expected exactly 1 RequestedInput event in the stream"
    );
}

#[tokio::test]
async fn test_resume_or_request_input() {
    // first pass emits request and pauses
    let ctx = top_level_ctx();
    let (emit, emitted) = recording_emit();
    let req = RequestInput {
        interrupt_id: "ask_name".into(),
        message: "What's your name?".into(),
        ..RequestInput::default()
    };
    let err = resume_or_request_input(&ctx, &emit, req)
        .await
        .expect_err("want ErrNodeInterrupted");
    assert!(
        is_kind(&err, WorkflowErrorKind::NodeInterrupted),
        "err = {err}, want ErrNodeInterrupted"
    );
    {
        let emitted = emitted.lock();
        assert_eq!(emitted.len(), 1, "expected one RequestInput event");
        let ri = emitted[0].requested_input.as_ref().expect("RequestInput");
        assert_eq!(ri.interrupt_id, "ask_name");
    }

    // resume returns reply without emitting
    let mut inputs = Map::new();
    inputs.insert("ask_name".into(), json!("Alice"));
    let ctx = top_level_ctx().with_delta(ContextDelta {
        resumed_inputs: Some(Arc::new(inputs)),
        ..ContextDelta::default()
    });
    let (emit, emitted) = recording_emit();
    let req = RequestInput {
        interrupt_id: "ask_name".into(),
        ..RequestInput::default()
    };
    let reply = resume_or_request_input(&ctx, &emit, req)
        .await
        .expect("err on resume");
    assert_eq!(reply, Some(json!("Alice")));
    assert!(
        emitted.lock().is_empty(),
        "emit called on resume; the request must not be re-sent"
    );

    // emit failure is returned instead of ErrNodeInterrupted
    let ctx = top_level_ctx();
    let emit: Emit = Arc::new(|_ev| Box::pin(async { Err(test_err("emit failed")) }));
    let req = RequestInput {
        interrupt_id: "ask_name".into(),
        ..RequestInput::default()
    };
    let err = resume_or_request_input(&ctx, &emit, req)
        .await
        .expect_err("want emit failure");
    assert_eq!(
        find_cause::<TestError>(&err),
        Some(&TestError("emit failed"))
    );
    assert!(
        !is_kind(&err, WorkflowErrorKind::NodeInterrupted),
        "emit failure must not be reported as ErrNodeInterrupted"
    );
}

#[tokio::test]
// parity: adapted: Go runs w.Run; Rust drives the scheduler directly so it can also check that
// orch is parked Waiting.
async fn test_scheduler_wait_for_output_pause_suspends_workflow() {
    let cfg = NodeConfig {
        wait_for_output: Some(true),
        ..NodeConfig::default()
    };
    let child = TestNode::arc("waiter", cfg, |_, _| once(Event::default()));
    let orch = dynamic_node::<Value, Value, _, _>(
        "orch",
        move |ctx: Context, _i: Value, _e: Emit| {
            let child = Arc::clone(&child);
            async move { run_node(&ctx, child, Value::Null, RunNodeOptions::default()).await }
        },
        NodeConfig::default(),
    );
    let downstream_ran = Arc::new(AtomicBool::new(false));
    let downstream = flag_node("downstream", &downstream_ran);
    let w = must_new(vec![edge(&start(), &orch), edge(&orch, &downstream)]);
    let (_, err, s) = drive(&w, seeded_mock_ctx().into_context()).await;
    assert!(err.is_none(), "unexpected error: {err:?}");
    assert!(
        !downstream_ran.load(Ordering::SeqCst),
        "downstream ran; a WaitForOutput pause must suspend the workflow, not complete the parent"
    );
    assert_eq!(s.state.nodes["orch"].status, NodeStatus::Waiting);
}
