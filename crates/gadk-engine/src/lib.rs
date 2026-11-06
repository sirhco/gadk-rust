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

// Removed by Task 23 once every module has consumers.
#![allow(dead_code)]

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
pub(crate) mod memory;
pub(crate) mod telemetry;

#[cfg(any(test, feature = "testutil"))]
pub mod testutil;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod parity_tests;

#[allow(unused_imports)]
pub use error::*;
