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

//! The workflow graph engine (port of ADK-Go `workflow`).

pub mod base_node;
#[allow(unused_imports)]
pub use base_node::*;
pub mod config;
#[allow(unused_imports)]
pub use config::*;
pub mod edge_builder;
#[allow(unused_imports)]
pub use edge_builder::*;
pub mod node;
#[allow(unused_imports)]
pub use node::*;
pub mod nodes;
#[allow(unused_imports)]
pub use nodes::*;
pub mod request_input;
#[allow(unused_imports)]
pub use request_input::*;
pub mod retry;
#[allow(unused_imports)]
pub use retry::*;
pub mod route;
#[allow(unused_imports)]
pub use route::*;
pub mod run_node;
#[allow(unused_imports)]
pub use run_node::*;
pub mod state;
#[allow(unused_imports)]
pub use state::*;
#[allow(clippy::module_inception)]
pub mod workflow;
#[allow(unused_imports)]
pub use workflow::*;

pub(crate) mod branch;
pub(crate) mod dynamic_scheduler;
pub(crate) mod failure_inventory;
pub(crate) mod graph;
pub(crate) mod persistence;
pub(crate) mod resume;
pub(crate) mod scheduler;
pub(crate) mod validation;
// Test-only: Go walkAgents' only consumer is the out-of-scope exp/appinfo.
#[cfg(test)]
pub(crate) mod walk;
