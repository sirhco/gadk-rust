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

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::context::InvocationContext;
use crate::error::AgentError;
use crate::run_config::run_limits::{
    DEFAULT_MAX_LLM_CALLS, LlmCallLimiter, resolve_max_llm_calls_with,
};

// parity: adapted: std::env::set_var is unsafe in edition 2024 (unsafe_code is forbidden), so the env value is passed to the pure resolve_max_llm_calls_with
#[test]
fn test_resolve_max_llm_calls() {
    let cases: [(&str, i32, Option<&str>, i32); 6] = [
        ("zero takes the default", 0, None, DEFAULT_MAX_LLM_CALLS),
        ("zero takes the environment override", 0, Some("7"), 7),
        (
            "zero ignores an unparsable override",
            0,
            Some("lots"),
            DEFAULT_MAX_LLM_CALLS,
        ),
        ("zero honors a negative override", 0, Some("-1"), -1),
        (
            "an explicit value wins over the environment",
            5,
            Some("7"),
            5,
        ),
        ("a negative value is passed through", -1, None, -1),
    ];
    for (name, v, env, want) in cases {
        assert_eq!(resolve_max_llm_calls_with(v, env), want, "{name}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_increment_and_enforce_llm_calls_limit() {
    // a limit of n permits exactly n calls
    let c = LlmCallLimiter::new(2);
    c.increment_and_enforce().unwrap();
    c.increment_and_enforce().unwrap();
    let err = c.increment_and_enforce().unwrap_err();
    assert!(
        matches!(err, AgentError::LlmCallsLimitExceeded { limit: 2 }),
        "{err:?}"
    );
    assert_eq!(
        err.to_string(),
        "max number of llm calls exceeded: limit is 2"
    );
    // non-positive limits do not bound
    for limit in [0, -1] {
        let c = LlmCallLimiter::new(limit);
        for i in 0..100 {
            assert!(
                c.increment_and_enforce().is_ok(),
                "limit {limit} bounded the run at call {i}"
            );
        }
    }
    // a nil run config does not bound: a context without a limiter has none
    assert!(
        InvocationContext::builder()
            .build_invocation()
            .llm_call_limiter()
            .is_none()
    );
    // the counter is shared across concurrent agents
    let c = Arc::new(LlmCallLimiter::new(50));
    let allowed = Arc::new(AtomicUsize::new(0));
    let mut tasks = Vec::new();
    for _ in 0..100 {
        let (c, allowed) = (c.clone(), allowed.clone());
        tasks.push(tokio::spawn(async move {
            if c.increment_and_enforce().is_ok() {
                allowed.fetch_add(1, Ordering::SeqCst);
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    assert_eq!(allowed.load(Ordering::SeqCst), 50);
}
