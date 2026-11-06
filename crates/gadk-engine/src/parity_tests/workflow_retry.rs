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

//! Port of Go workflow/retry_test.go.

use std::sync::Arc;
use std::time::Duration;

use gadk_agent::AgentError;
use gadk_platform::Platform;

use crate::workflow::{RetryConfig, calculate_delay, should_retry};

#[test]
fn test_calculate_delay() {
    let cfg = RetryConfig {
        initial_delay: Duration::from_secs(1),
        backoff_factor: 2.0,
        max_delay: Duration::from_secs(10),
        jitter: 0.0, // Deterministic for base testing
        ..Default::default()
    };
    let p = Platform::default();
    for (failed, want_secs) in [(0, 0), (1, 1), (2, 2), (3, 4), (4, 8), (5, 10), (6, 10)] {
        assert_eq!(
            calculate_delay(&cfg, failed, &p),
            Duration::from_secs(want_secs),
            "failed_attempts={failed}"
        );
    }
}

#[test]
fn test_calculate_delay_with_jitter() {
    let cfg = RetryConfig {
        initial_delay: Duration::from_secs(1),
        backoff_factor: 2.0,
        max_delay: Duration::from_secs(10),
        jitter: 0.5,
        ..Default::default()
    };
    let delay = calculate_delay(&cfg, 1, &Platform::default());
    assert!(
        delay >= Duration::from_millis(500) && delay <= Duration::from_millis(1500),
        "calculate_delay with jitter = {delay:?}, want in [500ms, 1500ms]"
    );
    // Reproducible under a deterministic platform.
    let a = calculate_delay(&cfg, 1, &Platform::deterministic(7));
    let b = calculate_delay(&cfg, 1, &Platform::deterministic(7));
    assert_eq!(a, b);
}

#[test]
fn test_calculate_delay_unset_backoff_factor() {
    let cfg = RetryConfig {
        max_attempts: 3,
        initial_delay: Duration::from_secs(1),
        ..Default::default()
    };
    let p = Platform::default();
    for attempt in 1..=3 {
        assert_eq!(
            calculate_delay(&cfg, attempt, &p),
            Duration::from_secs(1),
            "attempt={attempt}"
        );
    }
}

/// A retry predicate (`RetryConfig::should_retry`).
type Pred = Arc<dyn Fn(&AgentError) -> bool + Send + Sync>;

// parity: adapted: Go's nil *RetryConfig case is modelled as Option<RetryConfig> at the call site (None => false), since should_retry takes &RetryConfig
#[test]
fn test_should_retry() {
    let err = AgentError::other("test error");
    let always: Option<Pred> = Some(Arc::new(|_: &AgentError| true));
    let never: Option<Pred> = Some(Arc::new(|_: &AgentError| false));
    let cfg = |max_attempts: i32, pred: Option<Pred>| RetryConfig {
        max_attempts,
        should_retry: pred,
        ..Default::default()
    };

    let nil_cfg: Option<RetryConfig> = None;
    assert!(
        !nil_cfg.as_ref().is_some_and(|c| should_retry(c, &err, 1)),
        "nil config"
    );
    assert!(
        should_retry(&cfg(3, always.clone()), &err, 1),
        "within max attempts"
    );
    assert!(
        should_retry(&cfg(3, None), &err, 1),
        "nil ShouldRetry defaults to true"
    );
    assert!(
        !should_retry(&cfg(3, None), &err, 3),
        "max attempts reached"
    );
    assert!(
        !should_retry(&cfg(3, None), &err, 4),
        "exceeds max attempts"
    );
    assert!(!should_retry(&cfg(0, None), &err, 1), "zero max attempts");
    assert!(
        should_retry(&cfg(3, always), &err, 1),
        "ShouldRetry returns true"
    );
    assert!(
        !should_retry(&cfg(3, never), &err, 1),
        "ShouldRetry returns false"
    );
}
