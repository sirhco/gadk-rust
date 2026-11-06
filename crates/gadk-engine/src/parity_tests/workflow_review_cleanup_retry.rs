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

//! Port of Go workflow/review_cleanup_retry_test.go.

use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

use gadk_agent::{AgentError, Context};
use gadk_session::{Event, NodeInfo};
use serde_json::{Value, json};

use crate::error::{WorkflowError, WorkflowErrorKind};
use crate::workflow::dynamic_scheduler::new_dynamic_sub_scheduler;
use crate::workflow::failure_inventory::FailureInventory;
use crate::workflow::persistence::{WORKFLOW_FAILED_CHILD_PATHS_KEY, workflow_node_outcome};
use crate::workflow::scheduler::test_support::*;
use crate::workflow::scheduler::{CompletionItem, EventItem, NodeRun, QueueItem, Scheduler};
use crate::workflow::state::{NodeState, NodeStatus};
use crate::workflow::{
    Emit, NodeConfig, RetryConfig, RunNodeOptions, dynamic_node, function_node, run_node,
};

#[rstest::rstest]
#[case::success_without_event(false)]
#[case::retryable_error_without_event(true)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
// parity: adapted: Go loops 32 trials hoping map order schedules the valid node before the
// rejection and waits on a span exporter; Rust's resume iterates node names sorted, so the nodes
// are named "a_valid"/"b_invalid" (interrupt ids stay "valid"/"invalid") to make that order
// deterministic, and the test waits on the node's call counter instead of the exporter.
async fn test_resume_rejected_response_completion_does_not_dispatch(#[case] retry: bool) {
    use std::time::{Duration, Instant};

    use futures::StreamExt;

    use crate::workflow::RunState;

    let (valid_calls, successor_calls) = (Arc::new(AtomicI32::new(0)), Arc::new(AtomicI32::new(0)));
    let vc = Arc::clone(&valid_calls);
    let cfg = if retry {
        NodeConfig {
            retry_config: Some(RetryConfig {
                max_attempts: 2,
                ..RetryConfig::default()
            }),
            ..NodeConfig::default()
        }
    } else {
        NodeConfig::default()
    };
    let valid = dynamic_node::<Value, Value, _, _>(
        "a_valid",
        move |_c: Context, _i: Value, _e: Emit| {
            let n = vc.fetch_add(1, Ordering::SeqCst) + 1;
            async move {
                if n == 1 && retry {
                    Err(test_err("scripted parent failure"))
                } else {
                    Ok(Value::Null)
                }
            }
        },
        cfg,
    );
    let sc = Arc::clone(&successor_calls);
    let next = function_node::<Value, Value, _, _>(
        "next",
        move |_c: Context, _i: Value| {
            sc.fetch_add(1, Ordering::SeqCst);
            async { Ok(Value::Null) }
        },
        NodeConfig::default(),
    );
    let invalid = dynamic_node::<Value, Value, _, _>(
        "b_invalid",
        |_c: Context, _i: Value, _e: Emit| async { Ok(Value::Null) },
        NodeConfig::default(),
    );
    let wf = must_new(vec![
        edge(&start(), &valid),
        edge(&valid, &next),
        edge(&start(), &invalid),
    ]);
    let new_state = || {
        let mut state = RunState::default();
        let mut valid_inputs = serde_json::Map::new();
        valid_inputs.insert("valid".into(), json!("ok"));
        state.nodes.insert(
            "a_valid".into(),
            NodeState {
                status: NodeStatus::Pending,
                resumed_inputs: Some(valid_inputs),
                ..NodeState::default()
            },
        );
        let mut invalid_inputs = serde_json::Map::new();
        invalid_inputs.insert("invalid".into(), json!(1));
        let mut ns = NodeState {
            status: NodeStatus::Failed,
            resumed_inputs: Some(invalid_inputs),
            ..NodeState::default()
        };
        ns.interrupt_schemas
            .insert("invalid".into(), json!({"type": "integer"}));
        state.nodes.insert("b_invalid".into(), ns);
        state
    };
    let responses = |invalid: Value| {
        let mut m = serde_json::Map::new();
        m.insert("valid".into(), json!("ok"));
        m.insert("invalid".into(), invalid);
        m
    };

    let mut s = wf.resume(top_level_ctx(), new_state(), responses(json!("wrong")));
    let mut rejected = false;
    while let Some(item) = s.next().await {
        if let Err(e) = item
            && is_kind(&e, WorkflowErrorKind::InvalidResumeResponse)
        {
            rejected = true;
            // The valid node was scheduled before the rejection; let it run once.
            let deadline = Instant::now() + Duration::from_secs(2);
            while valid_calls.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            break;
        }
    }
    assert!(rejected, "fixture did not reject the invalid response");
    drop(s); // consumer exit
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        valid_calls.load(Ordering::SeqCst),
        1,
        "cleanup dispatched a retry after consumer exit"
    );
    assert_eq!(
        successor_calls.load(Ordering::SeqCst),
        0,
        "cleanup dispatched a successor after consumer exit"
    );

    // Reload the last persisted state: cleanup events were not consumed.
    let (_, err) = collect(wf.resume(top_level_ctx(), new_state(), responses(json!(42)))).await;
    assert!(
        err.is_none(),
        "corrected response could not resume: {err:?}"
    );
    assert_eq!(
        valid_calls.load(Ordering::SeqCst),
        2,
        "corrected response repeated or skipped the valid node"
    );
    assert_eq!(
        successor_calls.load(Ordering::SeqCst),
        1,
        "corrected response repeated or skipped the successor"
    );
}

#[rstest::rstest]
#[case::queued_success(false, false)]
#[case::queued_retryable_failure(true, false)]
#[case::queued_event_and_completion(false, true)]
#[tokio::test]
async fn test_scheduler_initial_drain_does_not_dispatch(
    #[case] failure: bool,
    #[case] with_event: bool,
) {
    let successor_calls = Arc::new(AtomicI32::new(0));
    let sc = Arc::clone(&successor_calls);
    let n = dynamic_node::<Value, Value, _, _>(
        "valid",
        |_c: Context, _i: Value, _e: Emit| async { Ok(Value::Null) },
        NodeConfig {
            retry_config: Some(RetryConfig {
                max_attempts: 2,
                ..RetryConfig::default()
            }),
            ..NodeConfig::default()
        },
    );
    let next = function_node::<Value, Value, _, _>(
        "next",
        move |_c: Context, _i: Value| {
            sc.fetch_add(1, Ordering::SeqCst);
            async { Ok(Value::Null) }
        },
        NodeConfig::default(),
    );
    let wf = must_new(vec![edge(&start(), &n), edge(&n, &next)]);
    let ctx = top_level_ctx();
    let mut s = Scheduler::new(ctx.clone(), Arc::clone(&wf.graph), 0);
    s.state.nodes.insert(
        "valid".into(),
        NodeState {
            status: NodeStatus::Running,
            ..NodeState::default()
        },
    );
    s.runs_by_name
        .insert("valid".into(), NodeRun::with_path("valid@1"));
    let (ptx, mut prx) = tokio::sync::oneshot::channel();
    if with_event {
        let ev = Event::new(ctx.platform(), ctx.invocation_id());
        s.tx.send(QueueItem::Event(Box::new(EventItem {
            node_name: "valid".into(),
            ev,
            processed: Some(ptx),
        })))
        .await
        .unwrap_or_else(|_| panic!("queue"));
    }
    let err = failure.then(|| test_err("scripted parent failure"));
    s.tx.send(QueueItem::Completion(CompletionItem {
        node_name: "valid".into(),
        err,
        inventory: FailureInventory::default(),
    }))
    .await
    .unwrap_or_else(|_| panic!("queue"));
    s.cancel_all();
    // `drain` never yields (Go's yield counter stays 0).
    s.drain().await;
    assert!(
        successor_calls.load(Ordering::SeqCst) == 0
            && s.retry_timers.is_empty()
            && s.runs_by_name.is_empty(),
        "initial drain yielded or dispatched new work"
    );
    if with_event {
        assert!(
            prx.try_recv().is_ok(),
            "initial drain did not release the producer handshake"
        );
    }
}

#[tokio::test]
async fn test_scheduler_initial_drain_does_not_finalize() {
    let a = function_node::<Value, Value, _, _>(
        "a",
        |_c: Context, _i: Value| async { Ok(Value::Null) },
        NodeConfig::default(),
    );
    let b = function_node::<Value, Value, _, _>(
        "b",
        |_c: Context, _i: Value| async { Ok(Value::Null) },
        NodeConfig::default(),
    );
    let wf = must_new(vec![edge(&start(), &a), edge(&start(), &b)]);
    let mut s = Scheduler::new(top_level_ctx(), Arc::clone(&wf.graph), 0);
    for name in ["a", "b"] {
        s.state.nodes.insert(
            name.into(),
            NodeState {
                status: NodeStatus::Completed,
                output: Some(Value::String(name.into())),
                ..NodeState::default()
            },
        );
    }
    s.cancel_all();
    // `drain` never yields, so a finalize error (two terminal outputs) can never surface.
    s.drain().await;
    assert!(
        s.next().await.is_none(),
        "initial drain finalized or yielded"
    );
}

#[tokio::test]
async fn test_sub_scheduler_empty_failure_inventory_persists() {
    let ctx = top_level_ctx();
    let sub = new_dynamic_sub_scheduler(ctx.clone(), "parent".into(), noop_emit());
    let inventory = sub.child_failures();
    assert!(
        inventory.known && inventory.paths.is_empty(),
        "known empty inventory was lost"
    );
    let wf = must_new(vec![edge(&start(), &dummy_node("parent"))]);
    let mut sched = Scheduler::new(ctx, Arc::clone(&wf.graph), 0);
    sched.state.nodes.insert(
        "parent".into(),
        NodeState {
            status: NodeStatus::Failed,
            ..NodeState::default()
        },
    );
    let ev = sched
        .completion_event("parent", Some("parent"), &inventory)
        .expect("completion event");
    let data = serde_json::to_string(&ev).expect("failure record serialization failed");
    let persisted: Event =
        serde_json::from_str(&data).expect("failure record deserialization failed");
    assert_eq!(
        persisted
            .response
            .custom_metadata
            .get(WORKFLOW_FAILED_CHILD_PATHS_KEY),
        Some(&json!([])),
        "empty inventory was omitted or persisted as null"
    );
    let unknown = sched
        .completion_event("parent", Some("parent"), &FailureInventory::default())
        .expect("completion event");
    assert!(
        !unknown
            .response
            .custom_metadata
            .contains_key(WORKFLOW_FAILED_CHILD_PATHS_KEY),
        "unknown inventory was recorded as known"
    );
}

#[rstest::rstest]
#[case::recovered(None)]
#[case::known_empty(Some(vec![]))]
#[case::new_failure(Some(vec!["parent/mid@1/other@1"]))]
#[tokio::test]
async fn test_sub_scheduler_recovered_nested_failure_inventory(#[case] paths: Option<Vec<&str>>) {
    const MID: &str = "parent/mid@1";
    const LEAF: &str = "parent/mid@1/leaf@1";
    const SIBLING: &str = "parent/mid@10/leaf@1";
    let sub = new_dynamic_sub_scheduler(top_level_ctx(), "parent".into(), noop_emit());
    sub.state().failed_paths.insert(SIBLING.into());
    let failure = || WorkflowError::new(WorkflowErrorKind::Other, "scripted child failure");
    sub.finish_run(
        MID,
        Err(failure()),
        true,
        &FailureInventory {
            known: true,
            paths: vec![LEAF.into()],
        },
    );
    {
        let st = sub.state();
        assert!(
            st.failed_paths.contains(LEAF) && st.failed_paths.len() == 2,
            "nested inventory was widened or lost"
        );
    }
    let paths: Option<Vec<String>> = paths.map(|p| p.into_iter().map(String::from).collect());
    let (res, inventory) = match &paths {
        None => (Ok(json!("recovered")), FailureInventory::default()),
        Some(p) => (
            Err(failure()),
            FailureInventory {
                known: true,
                paths: p.clone(),
            },
        ),
    };
    sub.finish_run(MID, res, true, &inventory);
    let st = sub.state();
    let want_len = 1 + paths.as_ref().map_or(0, Vec::len);
    assert!(
        st.failed_paths.contains(SIBLING) && st.failed_paths.len() == want_len,
        "new child inventory retained stale failures or cleared another subtree: {:?}",
        st.failed_paths
    );
    for path in paths.iter().flatten() {
        assert!(
            st.failed_paths.contains(path),
            "new child inventory omitted a failed path"
        );
    }
}

#[rstest::rstest]
#[case::recorded_inventory(true)]
#[case::rejected_marker(false)]
#[tokio::test]
async fn test_sub_scheduler_unrecorded_child_failure_remains_conservative(#[case] recorded: bool) {
    const PARENT: &str = "parent";
    const MID_PATH: &str = "parent/mid@stable";
    const CHARGE_PATH: &str = "parent/mid@stable/charge@charge";
    let history: Arc<parking_lot::Mutex<Vec<Event>>> = Arc::default();
    let h = Arc::clone(&history);
    let emit: Emit = Arc::new(move |ev: Event| {
        let rejected = !workflow_node_outcome(&ev).is_empty()
            && ev.node_info.as_ref().is_some_and(|i| i.path == MID_PATH)
            && !recorded;
        if !rejected {
            h.lock().push(ev);
        }
        Box::pin(async move {
            if rejected {
                Err(AgentError::other("scripted marker rejection"))
            } else {
                Ok(())
            }
        })
    });
    let sub = new_dynamic_sub_scheduler(ctx_with_events(Vec::new()), PARENT.into(), emit);
    let charge = function_node::<Value, String, _, _>(
        "charge",
        |_c: Context, _i: Value| async { Ok("charged".to_string()) },
        NodeConfig::default(),
    );
    let mid = dynamic_node::<Value, Value, _, _>(
        "mid",
        move |ctx: Context, input: Value, emit: Emit| {
            let charge = Arc::clone(&charge);
            async move {
                run_node(
                    &ctx,
                    charge,
                    input,
                    RunNodeOptions {
                        run_id: Some("charge".into()),
                        ..RunNodeOptions::default()
                    },
                )
                .await?;
                let mut partial = output_event(&ctx, json!("partial"));
                partial.node_info = Some(NodeInfo {
                    path: ctx.path(),
                    ..NodeInfo::default()
                });
                emit(partial).await?;
                Err::<Value, _>(test_err("scripted nested body failure"))
            }
        },
        NodeConfig::default(),
    );
    let err = sub
        .run_child(
            mid,
            Value::Null,
            RunNodeOptions {
                run_id: Some("stable".into()),
                ..RunNodeOptions::default()
            },
        )
        .await
        .expect_err("fixture did not fail");
    assert_eq!(
        find_cause::<TestError>(&err),
        Some(&TestError("scripted nested body failure")),
        "fixture did not preserve the nested failure cause: {err}"
    );
    let wf = must_new(vec![edge(&start(), &dummy_node(PARENT))]);
    let mut sched = Scheduler::new(ctx_with_events(Vec::new()), Arc::clone(&wf.graph), 0);
    sched.state.nodes.insert(
        PARENT.into(),
        NodeState {
            status: NodeStatus::Failed,
            ..NodeState::default()
        },
    );
    let completion = sched
        .completion_event(PARENT, Some(PARENT), &sub.child_failures())
        .expect("completion event");
    history.lock().push(completion);
    let data = serde_json::to_string(&*history.lock()).expect("history serialization failed");
    let persisted: Vec<Event> =
        serde_json::from_str(&data).expect("history deserialization failed");
    let rebuilt = new_dynamic_sub_scheduler(ctx_with_events(persisted), PARENT.into(), noop_emit());
    let st = rebuilt.state();
    assert!(
        !st.result_by_path.contains_key(MID_PATH),
        "failed child's own partial output survived"
    );
    assert_eq!(
        st.result_by_path.contains_key(CHARGE_PATH),
        recorded,
        "unknown failure scope was trusted or known child completion was discarded"
    );
}
