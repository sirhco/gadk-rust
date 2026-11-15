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

//! Vertex AI Agent Engine services (port of ADK-Go `session/vertexai`, `memory/vertexai` and
//! `tool/exampletool/vertexai`).
//!
//! [`session::VertexAiSessionService`] keeps sessions and their events in an Agent Engine
//! (reasoning engine) session store; [`memory::VertexAiMemoryService`] generates and retrieves
//! memories with the engine's Memory Bank; [`reasoning_engine::ReasoningEngineClient`] creates
//! and updates the engine itself (the calls `adkgo deploy agentengine` makes);
//! [`example_store::new_provider`] builds a `gadk_tools::example::Provider` that searches a
//! Vertex AI Example Store for few-shot examples. All speak the Vertex AI `v1beta1` REST API
//! through gadk's own REST transport, with the resource names, request bodies and error texts
//! ADK-Go uses, so a gadk service and an ADK-Go service can share one engine. Requests are
//! authenticated with Application Default Credentials unless credentials are given.
//!
//! ```no_run
//! # fn demo() -> Result<(), gadk_session::SessionError> {
//! use gadk_vertexai::session::{VertexAiSessionService, VertexAiSessionServiceConfig};
//! let sessions = VertexAiSessionService::new(VertexAiSessionServiceConfig {
//!     project_id: "my-project".into(),
//!     location: "us-central1".into(),
//!     reasoning_engine: Some("1234567890".into()),
//!     ..Default::default()
//! })?;
//! # Ok(()) }
//! ```

pub mod example_store;
pub mod memory;
pub mod reasoning_engine;
pub mod session;

mod client;
mod config;
mod error;
mod protojson;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod parity_tests;

#[cfg(any(test, feature = "testutil"))]
#[doc(hidden)]
pub mod testutil;

pub use config::AgentEngineData;
pub use error::VertexError;
