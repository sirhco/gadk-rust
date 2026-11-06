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

//! The LLM agent and its flow (port of ADK-Go `agent/llmagent` and `internal/llminternal`).

pub mod agent;
pub mod config;
#[allow(unused_imports)]
pub use config::*;

pub(crate) mod flow;
pub(crate) mod functions;
#[allow(dead_code)] // consumed by the live flow (Task 8); removed by Task 12
pub(crate) mod live;
pub(crate) mod mode;
pub(crate) mod node_wrapper;
pub(crate) mod processors;
pub(crate) mod state;
pub(crate) mod transfer;
