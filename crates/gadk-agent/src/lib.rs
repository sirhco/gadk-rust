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

//! gadk-agent: the agent API — agents, contexts, tools, callbacks and plugins (port of ADK-Go
//! `agent`, `tool`, `plugin`). Task 4 owns this file and adds the crate-root re-exports.

pub mod agent;
pub mod callback;
pub mod context;
pub mod error;
pub mod mode;
pub mod parent_map;
pub mod plugin;
pub mod plugin_manager;
pub mod run_config;
pub mod scheduler;
pub mod services;
pub mod state;
pub mod tool;
pub mod tool_util;
pub mod toolset;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod parity_tests;
