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

//! gadk-engine: the LLM agent and flow, workflow engine, workflow agents, runner and built-in
//! tools (port of ADK-Go `agent/llmagent`, `internal/llminternal`, `workflow`, `runner`, …).

pub mod agents;
pub mod error;
pub mod llm;
pub mod plugin;
pub mod runner;
pub mod tool;
pub mod workflow;

pub(crate) mod artifacts;
// consumed by the request processor (Task 9) and the runner (Task 10); removed by Task 11
#[allow(dead_code)]
pub(crate) mod compaction;
pub(crate) mod memory;
pub(crate) mod telemetry;

#[cfg(any(test, feature = "testutil"))]
pub mod testutil;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod parity_tests;

pub use agents::{loop_agent, parallel_agent, sequential_agent, workflow_agent};
#[allow(unused_imports)]
pub use error::*;
pub use llm::agent as llm_agent;
pub use runner::{RunOptions, Runner, RunnerConfig};
