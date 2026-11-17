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

//! Retry and concurrency settings of the trigger controllers.

use std::time::Duration;

/// Retry and concurrency settings of the trigger controllers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TriggerConfig {
    /// The maximum number of times to retry a failed agent execution.
    pub max_retries: i32,
    /// The base delay between retries.
    pub base_delay: Duration,
    /// The maximum delay between retries.
    pub max_delay: Duration,
    /// The maximum number of concurrent runs; zero or negative means no limit.
    pub max_concurrent_runs: i32,
}
