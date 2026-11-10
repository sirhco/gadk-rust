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

//! Pub/Sub and Eventarc push triggers (port of Go `server/adkrest/controllers/triggers`): each
//! delivery runs the agent in a fresh session, retrying a 429 with Go's jittered backoff. The
//! controllers' [`handler`](PubSubController::handler) is an axum `MethodRouter` the caller mounts
//! at a path of its choosing (plan 5a Task 10).

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::sync::Arc;

use gadk_agent::{AgentError, Loader};
use gadk_artifact::ArtifactService;
use gadk_engine::runner::PluginConfig;
use gadk_memory::MemoryService;
use gadk_platform::Platform;
use gadk_session::SessionService;
use gadk_session::compaction::Config as CompactionConfig;

mod config;
mod eventarc;
mod pubsub;

pub use config::TriggerConfig;
pub use eventarc::EventarcController;
pub use pubsub::PubSubController;

/// Port of Go `triggers.ControllerConfig`.
#[derive(Clone)]
pub struct ControllerConfig {
    /// The session service.
    pub session_service: Arc<dyn SessionService>,
    /// The agents served, by application name.
    pub agent_loader: Arc<dyn Loader>,
    /// Optional memory service passed to every runner.
    pub memory_service: Option<Arc<dyn MemoryService>>,
    /// Optional artifact service passed to every runner.
    pub artifact_service: Option<Arc<dyn ArtifactService>>,
    /// Plugins of every runner.
    pub plugin_config: PluginConfig,
    /// Retry and concurrency settings.
    pub trigger_config: TriggerConfig,
    /// Context compaction for every runner.
    pub compaction: Option<CompactionConfig>,
    /// Clock and ID source of every runner.
    pub platform: Platform,
}

/// Port of Go `triggers.RetriableRunner`: runs an agent with retry semantics, creating a new
/// session per delivery.
pub struct RetriableRunner {
    cfg: ControllerConfig,
}

impl RetriableRunner {
    /// Go `RunAgent`: runs the agent of `app_name` for `message_content` as `user_id` and returns
    /// the resulting events.
    pub async fn run_agent(
        &self,
        app_name: &str,
        user_id: &str,
        message_content: &str,
    ) -> Result<Vec<gadk_session::Event>, AgentError> {
        todo!("plan 5a Task 10")
    }
}
