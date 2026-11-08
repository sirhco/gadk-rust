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

//! The SDK retry policy (port of anthropic-sdk-go v1.81.0 `internal/requestconfig`:
//! `shouldRetry`, `retryHeader`, `shouldRetryDroppedBody`, `parseRetryAfterHeader`,
//! `retryDelay`).
//!
//! Placeholder signatures written by Task 1; Task 5 implements them.

use std::time::Duration;

use chrono::{DateTime, Utc};
use http::{HeaderMap, StatusCode};

/// The SDK's default retry count (`MaxRetries` 2: up to 3 attempts).
pub(crate) const DEFAULT_MAX_RETRIES: u32 = 2;

/// SDK `retryHeader`: `Some(true)` / `Some(false)` for `x-should-retry: true` / `false`, else
/// `None`.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn retry_header(headers: &HeaderMap) -> Option<bool> {
    todo!("Task 5")
}

/// SDK `shouldRetry` for a received response: the `x-should-retry` header wins, else 408, 409,
/// 429 and every status ≥ 500. (A connection error, with no response, is always retried.)
#[allow(unused_variables, clippy::todo)]
pub(crate) fn should_retry(status: StatusCode, headers: &HeaderMap) -> bool {
    todo!("Task 5")
}

/// SDK `shouldRetryDroppedBody`: a body that could not be read is retried like a connection
/// error unless the response said `x-should-retry: false`.
#[allow(unused_variables, clippy::todo)]
pub(crate) fn should_retry_dropped_body(headers: &HeaderMap) -> bool {
    todo!("Task 5")
}

/// SDK `parseRetryAfterHeader`: `Retry-After-Ms` (float milliseconds), then `Retry-After`
/// (float seconds, or an RFC 1123 date measured from `now`). A negative wait is
/// `Duration::ZERO` (the SDK's `retryDelay` clamps it with `max(0, …)`).
#[allow(unused_variables, clippy::todo)]
pub(crate) fn parse_retry_after(headers: &HeaderMap, now: DateTime<Utc>) -> Option<Duration> {
    todo!("Task 5")
}

/// SDK `retryDelay` before retry number `retry_count` (0-based): the server's wait when
/// [`parse_retry_after`] finds one (`headers` is `None` after a connection error), else
/// `min(0.5s·2^retry_count, 8s)` minus a uniform jitter in `[0, delay/4)` (`rand`).
#[allow(unused_variables, clippy::todo)]
pub(crate) fn retry_delay(headers: Option<&HeaderMap>, retry_count: u32) -> Duration {
    todo!("Task 5")
}
