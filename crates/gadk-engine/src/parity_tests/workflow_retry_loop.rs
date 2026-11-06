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

//! Port of Go workflow/retry_loop_test.go.

use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Duration;

use serde_json::Value;

use crate::workflow::scheduler::test_support::*;
use crate::workflow::{NodeConfig, StringRoute, default_retry_config};

#[tokio::test]
async fn test_scheduler_retry_loop() {
    let mut retry = default_retry_config();
    retry.max_attempts = 3;
    retry.initial_delay = Duration::ZERO;
    retry.jitter = 0.0;
    let cfg = NodeConfig {
        retry_config: Some(retry),
        ..NodeConfig::default()
    };
    let calls = Arc::new(AtomicI32::new(0));
    let c = Arc::clone(&calls);
    // Port of Go retryLoopTestNode: fails twice per execution, succeeds on the third.
    let node_a = TestNode::arc("A", cfg, move |ctx, input| {
        let n = c.fetch_add(1, Ordering::SeqCst) + 1;
        if n % 3 != 0 {
            return fail(gadk_agent::AgentError::other(format!("fail attempt {n}")));
        }
        let shown = match &input {
            Value::String(s) => s.clone(),
            v => v.to_string(),
        };
        let mut ev = output_event(&ctx, Value::String(format!("{shown}:A")));
        ev.routes = vec![if n >= 6 { "finish" } else { "loop" }.to_string()];
        once(ev)
    });
    let (finish, gate) = recording_node("Finish");
    gate.release();
    let w = must_new(vec![
        edge(&start(), &node_a),
        route_edge(&node_a, &node_a, Arc::new(StringRoute("loop".into()))),
        route_edge(&node_a, &finish, Arc::new(StringRoute("finish".into()))),
    ]);
    let events = drain(w.run(seeded_mock_ctx())).await;
    assert_eq!(
        outputs_of(&events),
        vec!["seed:A", "seed:A:A", "seed:A:A:Finish"]
    );
    assert_eq!(calls.load(Ordering::SeqCst), 6, "node A calls");
}
