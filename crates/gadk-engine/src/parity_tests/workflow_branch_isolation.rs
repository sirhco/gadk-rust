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

//! Port of Go workflow/branch_isolation_test.go.

use std::sync::Arc;

use gadk_agent::{AgentError, Context};
use parking_lot::Mutex;
use serde_json::{Value, json};

use crate::workflow::scheduler::test_support::*;
use crate::workflow::{Emit, NodeConfig, RunNodeOptions, dynamic_node, function_node, run_node};

/// Port of Go runSequentialFanOut: three sequential RunNode calls with WithRunID("custom-N").
async fn run_sequential_fan_out(child_name: &str, use_sub_branch: bool) -> Vec<String> {
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let s = Arc::clone(&seen);
    let peeker = function_node::<String, String, _, _>(
        child_name,
        move |ctx: Context, input: String| {
            let s = Arc::clone(&s);
            async move {
                s.lock().push(ctx.branch());
                Ok(input)
            }
        },
        NodeConfig::default(),
    );
    let orch = dynamic_node::<Value, Vec<String>, _, _>(
        "orch",
        move |ctx: Context, _input: Value, _emit: Emit| {
            let peeker = Arc::clone(&peeker);
            async move {
                let mut results = Vec::new();
                for (i, item) in ["a", "b", "c"].into_iter().enumerate() {
                    let opts = RunNodeOptions {
                        run_id: Some(format!("custom-{}", i + 1)),
                        use_sub_branch,
                        ..RunNodeOptions::default()
                    };
                    let out = run_node(&ctx, Arc::clone(&peeker), json!(item), opts).await?;
                    results.push(out.as_str().unwrap_or_default().to_string());
                }
                Ok::<_, AgentError>(results)
            }
        },
        NodeConfig::default(),
    );
    let (_, err) = drain_dynamic_with_err(&orch, Value::Null).await;
    assert!(err.is_none(), "orchestrator: {err:?}");
    let got = seen.lock().clone();
    assert_eq!(got.len(), 3, "got {} peeker invocations, want 3", got.len());
    got
}

#[rstest::rstest]
#[case::with_use_sub_branch_distinct_branches(true, vec!["peeker@custom-1", "peeker@custom-2", "peeker@custom-3"])]
#[case::no_option_still_shares_branch(false, vec!["", "", ""])]
#[tokio::test]
async fn test_run_node_sequential_fan_out_branches_from_options(
    #[case] use_sub_branch: bool,
    #[case] want: Vec<&str>,
) {
    let mut seen = run_sequential_fan_out("peeker", use_sub_branch).await;
    seen.sort();
    assert_eq!(seen, want);
}
