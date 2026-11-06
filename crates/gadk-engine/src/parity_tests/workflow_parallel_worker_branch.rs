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

//! Port of Go workflow/parallel_worker_branch_test.go.

use std::collections::HashMap;
use std::sync::Arc;

use gadk_agent::{Context, ContextDelta};
use parking_lot::Mutex;
use serde_json::json;

use crate::workflow::scheduler::test_support::*;
use crate::workflow::{Node, NodeConfig, ParallelWorker, RetryConfig, function_node};

fn branch_peeker(
    name: &str,
    seen: &Arc<Mutex<Vec<String>>>,
    suffix: &'static str,
) -> Arc<dyn Node> {
    let s = Arc::clone(seen);
    function_node::<String, String, _, _>(
        name,
        move |ctx: Context, input: String| {
            s.lock().push(ctx.branch());
            async move { Ok(format!("{input}{suffix}")) }
        },
        NodeConfig::default(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_parallel_worker_per_item_sub_branch() {
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let wrapped = branch_peeker("summarize", &seen, "-done");
    let pw: Arc<dyn Node> =
        ParallelWorker::new("pw", wrapped, 0, NodeConfig::default()).expect("new");
    drain(pw.run(top_level_ctx(), json!(["a", "b", "c"]))).await;
    let mut got = seen.lock().clone();
    got.sort();
    assert_eq!(got, vec!["summarize@1", "summarize@2", "summarize@3"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_parallel_worker_sub_branch_under_non_root_parent() {
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let wrapped = branch_peeker("worker", &seen, "");
    let pw: Arc<dyn Node> =
        ParallelWorker::new("pw", wrapped, 0, NodeConfig::default()).expect("new");
    let ctx = top_level_ctx().with_delta(ContextDelta {
        branch: Some("outer@1".into()),
        ..ContextDelta::default()
    });
    drain(pw.run(ctx, json!(["x", "y"]))).await;
    let mut got = seen.lock().clone();
    got.sort();
    assert_eq!(got, vec!["outer@1.worker@1", "outer@1.worker@2"]);
}

#[tokio::test]
async fn test_parallel_worker_retry_keeps_same_branch() {
    let attempts_by_branch: Arc<Mutex<HashMap<String, usize>>> = Arc::default();
    let a = Arc::clone(&attempts_by_branch);
    let wrapped = function_node::<String, String, _, _>(
        "flaky",
        move |ctx: Context, _input: String| {
            *a.lock().entry(ctx.branch()).or_default() += 1;
            async { Err(test_err("force retry")) }
        },
        NodeConfig::default(),
    );
    let cfg = NodeConfig {
        retry_config: Some(RetryConfig {
            max_attempts: 3,
            ..RetryConfig::default()
        }),
        ..NodeConfig::default()
    };
    let pw: Arc<dyn Node> = ParallelWorker::new("pw", wrapped, 0, cfg).expect("new");
    let (_, got_err) = collect(pw.run(top_level_ctx(), json!(["a"]))).await;
    assert!(
        got_err.is_some(),
        "expected wrapped node to ultimately fail after retries, got nil error"
    );
    let attempts = attempts_by_branch.lock();
    assert_eq!(
        attempts.len(),
        1,
        "retries must share one branch; got {attempts:?}"
    );
    for (branch, count) in attempts.iter() {
        assert_eq!(branch, "flaky@1", "retry branch");
        assert!(
            *count >= 2,
            "retry attempts under {branch:?} = {count}, want ≥2"
        );
    }
}
