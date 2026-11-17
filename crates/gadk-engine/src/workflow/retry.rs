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

//! `calculate_delay` and `should_retry`.

use std::time::Duration;

use gadk_agent::AgentError;
use gadk_platform::Platform;

use crate::workflow::RetryConfig;
use crate::workflow::config::default_should_retry;

/// The delay before the next attempt. `failed_attempts` = times the node has already failed.
///
/// The jitter source is the platform UUID provider so runs under `Platform::deterministic` are
/// reproducible: a jittered delay consumes exactly one UUID, and a delay with `jitter == 0`
/// consumes none.
pub fn calculate_delay(cfg: &RetryConfig, failed_attempts: i32, platform: &Platform) -> Duration {
    if failed_attempts <= 0 {
        return Duration::ZERO;
    }
    // Unset BackoffFactor would collapse the delay to 0; use constant delay.
    let backoff = if cfg.backoff_factor <= 0.0 {
        1.0
    } else {
        cfg.backoff_factor
    };
    let mut delay = cfg.initial_delay.as_nanos() as f64;
    for _ in 1..failed_attempts {
        delay *= backoff;
    }
    let max_delay = cfg.max_delay.as_nanos() as f64;
    if max_delay > 0.0 && delay > max_delay {
        delay = max_delay;
    }
    if cfg.jitter > 0.0 {
        let rand_val = jitter_fraction(platform);
        delay += (rand_val * 2.0 - 1.0) * cfg.jitter * delay;
        if delay < 0.0 {
            delay = 0.0;
        }
    }
    // The conversion truncates toward zero.
    Duration::from_nanos(delay as u64)
}

/// Uniform fraction in [0,1): the first 8 hex digits of `platform.new_uuid()` divided by 2^32
/// (the jitter source of [`calculate_delay`]).
fn jitter_fraction(platform: &Platform) -> f64 {
    let id = platform.new_uuid();
    let hex: String = id
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .take(8)
        .collect();
    u32::from_str_radix(&hex, 16).map_or(0.0, |n| f64::from(n) / 4_294_967_296.0)
}

/// Whether a node should be retried after a failure. `failed_attempts` is the number of times
/// the node has already failed. A node without a retry config is the caller's `None`.
pub fn should_retry(cfg: &RetryConfig, err: &AgentError, failed_attempts: i32) -> bool {
    if cfg.max_attempts <= 1 || failed_attempts >= cfg.max_attempts {
        return false;
    }
    match &cfg.should_retry {
        // A custom predicate is honored as-is (it may choose to retry validation errors).
        Some(pred) => pred(err),
        // By default, input validation errors are not retried: the input is deterministic per
        // activation, so retrying would loop on the same failure.
        None => default_should_retry(err),
    }
}
