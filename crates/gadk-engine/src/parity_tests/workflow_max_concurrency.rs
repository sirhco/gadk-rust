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

//! Port of Go workflow/max_concurrency_test.go.

use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

use futures::StreamExt;
use gadk_agent::Context;
use serde_json::{Value, json};

use crate::workflow::scheduler::test_support::*;
use crate::workflow::{
    Edge, JoinNode, Node, NodeConfig, RetryConfig, Workflow, WorkflowOptions, function_node,
};

/// Port of Go bumpPeak (increment half; callers decrement when done).
fn bump_peak(in_flight: &AtomicI32, peak: &AtomicI32) {
    let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
    peak.fetch_max(now, Ordering::SeqCst);
}

/// Port of Go fanOutToJoin.
fn fan_out_to_join(nodes: &[Arc<dyn Node>]) -> Vec<Edge> {
    let join: Arc<dyn Node> = JoinNode::new("join");
    nodes
        .iter()
        .flat_map(|n| [edge(&start(), n), edge(n, &join)])
        .collect()
}

/// Port of Go newBlockingNode with the bumpPeak/release work function.
fn blocking_node(
    name: &str,
    gate: Arc<Gate>,
    in_flight: Arc<AtomicI32>,
    peak: Arc<AtomicI32>,
) -> Arc<dyn Node> {
    let n = name.to_string();
    TestNode::arc(name, NodeConfig::default(), move |ctx: Context, _| {
        let (gate, in_flight, peak, n) = (
            Arc::clone(&gate),
            Arc::clone(&in_flight),
            Arc::clone(&peak),
            n.clone(),
        );
        async_stream::stream! {
            bump_peak(&in_flight, &peak);
            gate.wait().await;
            in_flight.fetch_sub(1, Ordering::SeqCst);
            yield Ok(output_event(&ctx, json!(n)));
        }
        .boxed()
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_max_concurrency_caps() {
    const FAN_OUT: usize = 6;
    const CAP: usize = 2;
    let (in_flight, peak) = (Arc::new(AtomicI32::new(0)), Arc::new(AtomicI32::new(0)));
    let gates: Vec<Arc<Gate>> = (0..FAN_OUT).map(|_| Gate::new()).collect();
    let nodes: Vec<Arc<dyn Node>> = (0..FAN_OUT)
        .map(|i| {
            blocking_node(
                &format!("N{i}"),
                Arc::clone(&gates[i]),
                Arc::clone(&in_flight),
                Arc::clone(&peak),
            )
        })
        .collect();
    let w = Workflow::new(
        "",
        fan_out_to_join(&nodes),
        WorkflowOptions {
            max_concurrency: Some(CAP),
            ..WorkflowOptions::default()
        },
    )
    .expect("New");
    let run = tokio::spawn(async move { drain(w.run(seeded_mock_ctx())).await });
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && in_flight.load(Ordering::SeqCst) != CAP as i32 {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert_eq!(
        in_flight.load(Ordering::SeqCst),
        CAP as i32,
        "after warmup: cap not enforced"
    );
    for g in &gates {
        g.release();
    }
    run.await.expect("run task");
    assert!(
        peak.load(Ordering::SeqCst) <= CAP as i32,
        "peak in-flight = {}, want <= {CAP}",
        peak.load(Ordering::SeqCst)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_max_concurrency_pending_dispatched_fifo() {
    const FAN_OUT: usize = 4;
    let order = Arc::new(AtomicI32::new(0));
    let starts: Arc<Vec<AtomicI32>> = Arc::new((0..FAN_OUT).map(|_| AtomicI32::new(0)).collect());
    let nodes: Vec<Arc<dyn Node>> = (0..FAN_OUT)
        .map(|i| {
            let (order, starts) = (Arc::clone(&order), Arc::clone(&starts));
            function_node::<Value, String, _, _>(
                &format!("N{i}"),
                move |_c: Context, _i: Value| {
                    starts[i].store(order.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
                    async { Ok("ok".to_string()) }
                },
                NodeConfig::default(),
            )
        })
        .collect();
    let w = Workflow::new(
        "",
        fan_out_to_join(&nodes),
        WorkflowOptions {
            max_concurrency: Some(1),
            ..WorkflowOptions::default()
        },
    )
    .expect("New");
    drain(w.run(seeded_mock_ctx())).await;
    for (i, s) in starts.iter().enumerate() {
        assert_eq!(
            s.load(Ordering::SeqCst),
            i as i32 + 1,
            "N{i} start order (FIFO violated)"
        );
    }
}

/// A function node whose body tracks in-flight peak and holds the slot for 20 ms (as Go does).
fn peak_node(
    name: &str,
    cfg: NodeConfig,
    in_flight: &Arc<AtomicI32>,
    peak: &Arc<AtomicI32>,
    calls: &Arc<AtomicI32>,
    fail_first: bool,
) -> Arc<dyn Node> {
    let (in_flight, peak, calls) = (Arc::clone(in_flight), Arc::clone(peak), Arc::clone(calls));
    function_node::<Value, String, _, _>(
        name,
        move |_c: Context, _i: Value| {
            let (in_flight, peak, calls) = (
                Arc::clone(&in_flight),
                Arc::clone(&peak),
                Arc::clone(&calls),
            );
            async move {
                bump_peak(&in_flight, &peak);
                tokio::time::sleep(Duration::from_millis(20)).await;
                let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
                in_flight.fetch_sub(1, Ordering::SeqCst);
                if fail_first && n == 1 {
                    return Err(gadk_agent::AgentError::other("retryable"));
                }
                Ok("ok".to_string())
            }
        },
        cfg,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_max_concurrency_retry_respects_limit() {
    let (in_flight, peak) = (Arc::new(AtomicI32::new(0)), Arc::new(AtomicI32::new(0)));
    let flaky_calls = Arc::new(AtomicI32::new(0));
    let stable_calls = Arc::new(AtomicI32::new(0));
    let retry = RetryConfig {
        max_attempts: 3,
        initial_delay: Duration::from_millis(10),
        max_delay: Duration::from_millis(10),
        backoff_factor: 1.0,
        should_retry: Some(Arc::new(|_| true)),
        ..RetryConfig::default()
    };
    let flaky = peak_node(
        "flaky",
        NodeConfig {
            retry_config: Some(retry),
            ..NodeConfig::default()
        },
        &in_flight,
        &peak,
        &flaky_calls,
        true,
    );
    let stable = peak_node(
        "stable",
        NodeConfig::default(),
        &in_flight,
        &peak,
        &stable_calls,
        false,
    );
    let w = Workflow::new(
        "",
        fan_out_to_join(&[flaky, stable]),
        WorkflowOptions {
            max_concurrency: Some(1),
            ..WorkflowOptions::default()
        },
    )
    .expect("New");
    drain(w.run(seeded_mock_ctx())).await;
    assert!(
        peak.load(Ordering::SeqCst) <= 1,
        "peak in-flight = {}, want <= 1 (retry bypassed cap)",
        peak.load(Ordering::SeqCst)
    );
    assert!(
        flaky_calls.load(Ordering::SeqCst) >= 2,
        "flaky calls = {}, want >= 2 (retry never happened)",
        flaky_calls.load(Ordering::SeqCst)
    );
}
