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

//! Port of Go workflow/config_test.go.

use std::time::Duration;

use gadk_agent::AgentError;

use crate::workflow::default_retry_config;

// parity: adapted: Go calls ShouldRetry(nil); Rust predicates take &AgentError, so a plain error stands in for nil
#[test]
fn test_default_retry_config() {
    let got = default_retry_config();
    assert_eq!(got.max_attempts, 5);
    assert_eq!(got.initial_delay, Duration::from_secs(1));
    assert_eq!(got.max_delay, Duration::from_secs(60));
    assert_eq!(got.backoff_factor, 2.0);
    assert_eq!(got.jitter, 1.0);
    let pred = got.should_retry.as_ref().expect("ShouldRetry is set");
    assert!(
        pred(&AgentError::other("boom")),
        "ShouldRetry returns false"
    );
}
