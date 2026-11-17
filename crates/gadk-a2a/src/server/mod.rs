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

//! Serving a gadk agent over A2A: [`Executor`] implements
//! `a2a_server::AgentExecutor`; [`build_agent_skills`] describes an agent tree for its
//! `a2a::AgentCard`; [`a2a_identity_layer`] and [`identity_header_resolver`] carry the
//! authenticated user to the executor.

pub(crate) mod agent_card;
pub(crate) mod cleanup;
pub(crate) mod context;
pub(crate) mod executor;
pub(crate) mod identity;
pub(crate) mod input_required;
pub(crate) mod plugin;
pub(crate) mod processor;
pub(crate) mod task_artifact;

pub use agent_card::*;
pub use context::ExecutorContext;
pub use executor::*;
pub use identity::*;
pub use input_required::handle_input_required;
