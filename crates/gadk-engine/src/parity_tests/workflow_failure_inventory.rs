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

//! Port of Go workflow/failure_inventory_test.go.

use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, OnceLock, Weak};

use futures::{FutureExt, StreamExt};
use gadk_agent::{AgentError, Context};
use serde_json::{Value, json};

use crate::workflow::dynamic_scheduler::new_dynamic_sub_scheduler;
use crate::workflow::failure_inventory::{
    FailureInventory, node_producer_key, publish_failure_inventory, scope, with_failure_inventory,
};
use crate::workflow::persistence::{WORKFLOW_FAILED_CHILD_PATHS_KEY, workflow_node_outcome};
use crate::workflow::scheduler::test_support::*;
use crate::workflow::scheduler::{
    CompletionItem, DEFAULT_EVENT_QUEUE_CAPACITY, NodeRun, QueueItem, Scheduler, run_node_task,
};
use crate::workflow::{
    Emit, Node, NodeConfig, RunNodeOptions, WorkflowNode, dynamic_node, function_node, run_node,
};

/// Port of Go panicAfterErrorNode: forwards `inner`, publishes a known inventory on an
/// error, and panics from its cleanup (Go: deferred panic; Rust: Drop of the stream state).
fn panic_after_error_node(inner: Arc<dyn Node>) -> Arc<dyn Node> {
    struct PanicOnDrop;
    impl Drop for PanicOnDrop {
        fn drop(&mut self) {
            if !std::thread::panicking() {
                panic!("scripted iterator cleanup panic");
            }
        }
    }
    let key_cell: Arc<OnceLock<usize>> = Arc::new(OnceLock::new());
    let kc = Arc::clone(&key_cell);
    let node = TestNode::arc("body", NodeConfig::default(), move |ctx, input| {
        let (inner, kc) = (Arc::clone(&inner), Arc::clone(&kc));
        async_stream::stream! {
            let _guard = PanicOnDrop;
            let mut s = inner.run(ctx, input);
            while let Some(item) = s.next().await {
                if item.is_err() {
                    publish_failure_inventory(kc.get().copied().unwrap_or_default(), &FailureInventory { known: true, paths: Vec::new() });
                }
                yield item;
            }
        }
        .boxed()
    });
    let _ = key_cell.set(node_producer_key(&node));
    node
}

// parity: adapted: Go context.WithValue recorders → task-local scopes; the goroutines become tokio tasks that each enter their own recorder scope.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_failure_inventory_activation_isolation() {
    let parent_node = dummy_node("parent");
    let parent_rec = with_failure_inventory(&parent_node);
    let pkey = node_producer_key(&parent_node);
    let mut paths = vec!["parent/stable@1".to_string()];
    let published = paths.clone();
    scope(Arc::clone(&parent_rec), async move {
        publish_failure_inventory(
            pkey,
            &FailureInventory {
                known: true,
                paths: published,
            },
        )
    })
    .await;
    paths[0] = "changed".into();
    assert_eq!(
        parent_rec.snapshot().paths,
        vec!["parent/stable@1"],
        "publishing retained a mutable path slice"
    );
    let mut copy = parent_rec.snapshot();
    copy.paths[0] = "changed".into();
    assert_eq!(
        parent_rec.snapshot().paths[0],
        "parent/stable@1",
        "inventory snapshot aliases the recorder"
    );

    let mut set = tokio::task::JoinSet::new();
    for path in ["parent/left@1", "parent/right@1"] {
        set.spawn(scope(Arc::clone(&parent_rec), async move {
            let child_node = dummy_node("child");
            let rec = with_failure_inventory(&child_node);
            let inv = rec.snapshot();
            assert!(
                !inv.known && inv.paths.is_empty(),
                "new activation inherited its parent's inventory"
            );
            let key = node_producer_key(&child_node);
            scope(Arc::clone(&rec), async move {
                publish_failure_inventory(
                    key,
                    &FailureInventory {
                        known: true,
                        paths: vec![path.to_string()],
                    },
                );
            })
            .await;
            assert_eq!(
                rec.snapshot().paths,
                vec![path.to_string()],
                "parallel activations shared inventory"
            );
        }));
    }
    while let Some(r) = set.join_next().await {
        r.expect("activation task");
    }
    assert_eq!(
        parent_rec.snapshot().paths,
        vec!["parent/stable@1"],
        "child activation overwrote its parent's inventory"
    );
}

#[rstest::rstest]
#[case::known_empty("known_empty")]
#[case::known_child("known_child")]
#[case::unknown_function("unknown_function")]
#[case::panic("panic")]
#[case::panic_after_error("panic_after_error")]
#[tokio::test]
// parity: adapted: Go drives runNode over a raw queue with ctx-carried recorders; Rust drives run_node_task inside the parent's task-local scope; panic_after_error panics from Drop instead of a deferred iterator panic.
async fn test_scheduler_failure_inventory_executors(#[case] mode: &'static str) {
    let parent_node = dummy_node("parent");
    let rec = with_failure_inventory(&parent_node);
    let pkey = node_producer_key(&parent_node);
    scope(Arc::clone(&rec), async move {
        publish_failure_inventory(
            pkey,
            &FailureInventory {
                known: true,
                paths: vec!["parent/stale@1".into()],
            },
        );
    })
    .await;

    let self_key: Arc<OnceLock<usize>> = Arc::new(OnceLock::new());
    let sk = Arc::clone(&self_key);
    let dynamic = dynamic_node::<Value, Value, _, _>(
        "body",
        move |ctx: Context, _i: Value, _e: Emit| {
            let sk = Arc::clone(&sk);
            async move {
                if mode == "known_child" {
                    let child = function_node::<Value, Value, _, _>(
                        "leaf",
                        |_c: Context, _i: Value| async { Err(test_err("scripted body failure")) },
                        NodeConfig::default(),
                    );
                    return run_node(&ctx, child, Value::Null, RunNodeOptions::default()).await;
                }
                if mode == "panic" {
                    publish_failure_inventory(
                        sk.get().copied().unwrap_or_default(),
                        &FailureInventory {
                            known: true,
                            paths: Vec::new(),
                        },
                    );
                    panic!("scripted panic");
                }
                Err::<Value, AgentError>(test_err("scripted body failure"))
            }
        },
        NodeConfig::default(),
    );
    let _ = self_key.set(node_producer_key(&dynamic));
    let node: Arc<dyn Node> = match mode {
        "unknown_function" => function_node::<Value, Value, _, _>(
            "body",
            |_c: Context, _i: Value| async { Err(test_err("scripted body failure")) },
            NodeConfig::default(),
        ),
        "panic_after_error" => panic_after_error_node(dynamic),
        _ => dynamic,
    };

    let (tx, mut rx) = tokio::sync::mpsc::channel(DEFAULT_EVENT_QUEUE_CAPACITY);
    let task = tokio::spawn(scope(
        Arc::clone(&rec),
        run_node_task(tx, "body".into(), node, top_level_ctx(), None, None),
    ));
    let completion = loop {
        match rx.recv().await.expect("queue closed before completion") {
            QueueItem::Event(mut it) => {
                if let Some(p) = it.processed.take() {
                    let _ = p.send(());
                }
            }
            QueueItem::Completion(c) => break c,
            QueueItem::Retry(_) => {}
        }
    };
    task.await.expect("runner task");
    let want_known = matches!(mode, "known_empty" | "known_child");
    assert_eq!(
        completion.inventory.known, want_known,
        "executor lost known inventory or trusted an unknown outcome"
    );
    let err = completion.err.expect("completion error");
    if mode == "known_child" {
        assert_eq!(
            completion.inventory.paths,
            vec!["body/leaf@1"],
            "executor lost the failed child's path"
        );
        assert!(
            find_cause::<TestError>(&err).is_some(),
            "executor lost the original cause"
        );
    } else if !mode.starts_with("panic") {
        assert_eq!(
            find_cause::<TestError>(&err),
            Some(&TestError("scripted body failure")),
            "executor replaced the business error"
        );
    }
    assert_eq!(
        rec.snapshot().paths,
        vec!["parent/stale@1"],
        "executor reused the parent's inventory recorder"
    );
}

/// Drives `run_node_task` for `node` (no enclosing recorder) and returns its completion.
async fn run_to_completion(node: Arc<dyn Node>) -> CompletionItem {
    let (tx, mut rx) = tokio::sync::mpsc::channel(DEFAULT_EVENT_QUEUE_CAPACITY);
    let task = tokio::spawn(run_node_task(
        tx,
        "body".into(),
        node,
        top_level_ctx(),
        None,
        None,
    ));
    let completion = loop {
        match rx.recv().await.expect("queue closed before completion") {
            QueueItem::Event(mut it) => {
                if let Some(p) = it.processed.take() {
                    let _ = p.send(());
                }
            }
            QueueItem::Completion(c) => break c,
            QueueItem::Retry(_) => {}
        }
    };
    task.await.expect("runner task");
    completion
}

#[rstest::rstest]
#[case::known("known")]
#[case::unknown("unknown")]
#[case::accumulator_error("accumulator_error")]
#[case::consumer_gone("consumer_gone")]
#[tokio::test]
// parity: adapted: Go's ctx-carried recorder is a task-local scope around the scheduler's pull loop; completions are pushed through the scheduler's own queue sender.
async fn test_scheduler_failure_inventory_first_error(#[case] mode: &'static str) {
    let (first, second) = (dummy_node("first"), dummy_node("second"));
    let nested = WorkflowNode::new(
        "nested",
        vec![edge(&start(), &first), edge(&start(), &second)],
    )
    .expect("nested workflow construction failed");
    let nested_dyn: Arc<dyn Node> = nested.clone();
    let recorder = with_failure_inventory(&nested_dyn);
    let mut s = Scheduler::new(top_level_ctx(), Arc::clone(&nested.sub_workflow.graph), 0);
    s.runs_by_name
        .insert("first".into(), NodeRun::with_path("first"));
    s.runs_by_name
        .insert("second".into(), NodeRun::with_path("second"));
    let mut item = CompletionItem {
        node_name: "first".into(),
        err: Some(test_err("scripted first failure")),
        inventory: FailureInventory::default(),
    };
    match mode {
        "known" => {
            item.inventory = FailureInventory {
                known: true,
                paths: vec!["first/leaf@1".into()],
            }
        }
        "accumulator_error" => {
            item.err = None;
            item.inventory = FailureInventory {
                known: true,
                paths: vec!["unrelated".into()],
            };
            s.runs_by_name
                .get_mut("first")
                .expect("first run")
                .record_err(test_err("scripted first failure"));
        }
        _ => {}
    }
    let tx = s.tx.clone();
    tx.send(QueueItem::Completion(item))
        .await
        .expect("queue first");
    tx.send(QueueItem::Completion(CompletionItem {
        node_name: "second".into(),
        err: Some(AgentError::Cancelled),
        inventory: FailureInventory {
            known: true,
            paths: vec!["second/leaf@1".into()],
        },
    }))
    .await
    .expect("queue second");
    scope(Arc::clone(&recorder), async {
        if mode == "consumer_gone" {
            s.drain().await;
        } else {
            let mut got = None;
            while let Some(item) = s.next().await {
                if let Err(e) = item {
                    got = Some(e);
                }
            }
            let got = got.expect("run error");
            assert_eq!(
                find_cause::<TestError>(&got),
                Some(&TestError("scripted first failure")),
                "first error was replaced while draining"
            );
        }
    })
    .await;
    let inventory = recorder.snapshot();
    assert_eq!(
        inventory.known,
        mode == "known",
        "unknown or departed-consumer outcome acquired another node's inventory"
    );
    if mode == "known" {
        assert_eq!(
            inventory.paths,
            vec!["first/leaf@1"],
            "sibling cancellation overwrote the first error's inventory"
        );
    }
}

#[rstest::rstest]
#[case::known_empty("known_empty")]
#[case::leaf_failure("leaf_failure")]
#[case::recovered_leaf("recovered_leaf")]
#[tokio::test]
async fn test_workflow_node_failure_inventory(#[case] mode: &'static str) {
    const INNER_PATH: &str = "parent/inner@stable";
    const LEAF_PATH: &str = "parent/inner@stable/body@1/leaf@stable";
    let leaf_calls = Arc::new(AtomicI32::new(0));
    let lc = Arc::clone(&leaf_calls);
    let leaf = function_node::<Value, Value, _, _>(
        "leaf",
        move |_c: Context, _i: Value| {
            let first = lc.fetch_add(1, Ordering::SeqCst) == 0;
            async move {
                if first {
                    Err(test_err("scripted leaf failure"))
                } else {
                    Ok(json!("recovered"))
                }
            }
        },
        NodeConfig::default(),
    );
    let body = dynamic_node::<Value, Value, _, _>(
        "body",
        move |ctx: Context, _i: Value, _e: Emit| {
            let leaf = Arc::clone(&leaf);
            async move {
                if mode != "known_empty" {
                    let stable = || RunNodeOptions {
                        run_id: Some("stable".into()),
                        ..RunNodeOptions::default()
                    };
                    let err = run_node(&ctx, Arc::clone(&leaf), Value::Null, stable())
                        .await
                        .expect_err("leaf fixture did not fail");
                    if mode == "leaf_failure" {
                        return Err(err);
                    }
                    assert_eq!(
                        find_cause::<TestError>(&err),
                        Some(&TestError("scripted leaf failure")),
                        "leaf fixture did not fail"
                    );
                    run_node(&ctx, Arc::clone(&leaf), Value::Null, stable()).await?;
                }
                Err(test_err("scripted body failure"))
            }
        },
        NodeConfig::default(),
    );
    let inner: Arc<dyn Node> = WorkflowNode::new("inner", vec![edge(&start(), &body)])
        .expect("nested workflow construction failed");
    let (emit, log) = recording_emit();
    let sub = new_dynamic_sub_scheduler(top_level_ctx(), "parent".into(), emit);
    let opts = RunNodeOptions {
        run_id: Some("stable".into()),
        ..RunNodeOptions::default()
    };
    let err = sub
        .run_child(inner, Value::Null, opts)
        .await
        .expect_err("nested workflow must fail");
    let want_cause = if mode == "leaf_failure" {
        "scripted leaf failure"
    } else {
        "scripted body failure"
    };
    assert_eq!(
        find_cause::<TestError>(&err),
        Some(&TestError(want_cause)),
        "nested workflow lost its original failure"
    );
    let marker = log
        .lock()
        .iter()
        .rfind(|ev| {
            ev.node_info.as_ref().is_some_and(|i| i.path == INNER_PATH)
                && !workflow_node_outcome(ev).is_empty()
        })
        .cloned()
        .expect("nested workflow lost its outcome marker");
    let want: Vec<Value> = if mode == "leaf_failure" {
        vec![json!(LEAF_PATH)]
    } else {
        Vec::new()
    };
    assert_eq!(
        marker
            .response
            .custom_metadata
            .get(WORKFLOW_FAILED_CHILD_PATHS_KEY),
        Some(&Value::Array(want.clone())),
        "WorkflowNode lost precise inventory or widened it to the containing child"
    );
    let inventory = sub.child_failures();
    assert!(
        inventory.known && inventory.paths.len() == want.len(),
        "outer scheduler retained a recovered leaf or lost known-empty inventory"
    );
    if mode == "leaf_failure" {
        assert_eq!(
            inventory.paths[0], LEAF_PATH,
            "outer scheduler did not inherit the precise failed leaf"
        );
    }
}

// parity: adapted: Go recover() of the propagated panic is AssertUnwindSafe(run_child(..)).catch_unwind(); the cleanup panic comes from Drop of the stream state.
#[tokio::test]
async fn test_sub_scheduler_panic_after_error_inventory() {
    const CHILD_PATH: &str = "parent/body@stable";
    let inner = dynamic_node::<Value, Value, _, _>(
        "body",
        |_c: Context, _i: Value, _e: Emit| async {
            Err::<Value, _>(test_err("scripted body failure"))
        },
        NodeConfig::default(),
    );
    let child = panic_after_error_node(inner);
    let (emit, log) = recording_emit();
    let sub = new_dynamic_sub_scheduler(top_level_ctx(), "parent".into(), emit);
    let opts = RunNodeOptions {
        run_id: Some("stable".into()),
        ..RunNodeOptions::default()
    };
    let panicked = AssertUnwindSafe(sub.run_child(child, Value::Null, opts))
        .catch_unwind()
        .await
        .is_err();
    let marker = log
        .lock()
        .iter()
        .find(|ev| {
            ev.node_info.as_ref().is_some_and(|i| i.path == CHILD_PATH)
                && !workflow_node_outcome(ev).is_empty()
        })
        .cloned();
    assert!(
        panicked && marker.is_some(),
        "child panic was swallowed or its failure marker was lost"
    );
    let marker = marker.expect("marker");
    assert!(
        !marker
            .response
            .custom_metadata
            .contains_key(WORKFLOW_FAILED_CHILD_PATHS_KEY),
        "iterator cleanup panic trusted the preceding error's inventory"
    );
    assert!(
        sub.state().failed_paths.contains(CHILD_PATH),
        "panicked child was not conservatively invalidated"
    );
}

/// The composite body of Go TestFailureInventory_DirectRunsRemainUnknown: a direct
/// `child.run(..)` composition (no run_node) that then fails for its own reason.
async fn direct_body(
    ctx: Context,
    input: Value,
    mode: &'static str,
    self_node: Arc<OnceLock<Weak<dyn Node>>>,
) -> Result<Value, AgentError> {
    let child_msg = if mode == "handled_same_sentinel" {
        "scripted composite failure"
    } else {
        "scripted direct child failure"
    };
    if !input.is_null() {
        return Err(test_err(child_msg));
    }
    let child: Arc<dyn Node> = match mode {
        "same_producer" => self_node.get().and_then(Weak::upgrade).expect("self node"),
        _ => dynamic_node::<Value, Value, _, _>(
            if mode == "same_name" { "body" } else { "child" },
            move |_c: Context, _i: Value, _e: Emit| async move { Err::<Value, _>(test_err(child_msg)) },
            NodeConfig::default(),
        ),
    };
    let consume = || {
        let (child, ctx) = (Arc::clone(&child), ctx.clone());
        async move {
            let (_, err) = collect(child.run(ctx, json!(1))).await;
            assert_eq!(
                err.as_ref().and_then(find_cause::<TestError>),
                Some(&TestError(child_msg)),
                "direct child error was replaced"
            );
        }
    };
    if mode == "parallel" || mode == "same_producer" {
        futures::join!(consume(), consume());
    } else {
        consume().await;
    }
    Err(test_err("scripted composite failure"))
}

#[rstest::rstest]
#[case::handled("handled")]
#[case::handled_same_sentinel("handled_same_sentinel")]
#[case::same_name("same_name")]
#[case::parallel("parallel")]
#[case::same_producer("same_producer")]
#[tokio::test]
// parity: adapted: Go's goroutine pair (sync.WaitGroup) is futures::join! of two drains on the activation's task; Go's self-referencing node variable is a Weak in a OnceLock.
async fn test_failure_inventory_direct_runs_remain_unknown(#[case] mode: &'static str) {
    let slot: Arc<OnceLock<Weak<dyn Node>>> = Arc::default();
    let s = Arc::clone(&slot);
    let node: Arc<dyn Node> = if mode == "same_producer" {
        dynamic_node::<Value, Value, _, _>(
            "body",
            move |ctx: Context, input: Value, _e: Emit| {
                direct_body(ctx, input, mode, Arc::clone(&s))
            },
            NodeConfig::default(),
        )
    } else {
        function_node::<Value, Value, _, _>(
            "body",
            move |ctx: Context, input: Value| direct_body(ctx, input, mode, Arc::clone(&s)),
            NodeConfig::default(),
        )
    };
    let _ = slot.set(Arc::downgrade(&node));
    let completion = run_to_completion(Arc::clone(&node)).await;
    let err = completion.err.expect("completion error");
    assert_eq!(
        find_cause::<TestError>(&err),
        Some(&TestError("scripted composite failure")),
        "direct node composition supplied an unrelated failure"
    );
    assert!(
        !completion.inventory.known,
        "direct node composition supplied an ambiguous failure inventory"
    );
}
