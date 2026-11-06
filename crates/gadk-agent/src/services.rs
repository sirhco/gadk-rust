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

//! Scoped service handles (Go `agent.Artifacts` / `agent.Memory` in `agent/agent.go`).

use std::sync::Arc;

use gadk_genai::Part;
use gadk_session::Session;

use crate::error::AgentError;

/// The service and scope behind a scoped [`Artifacts`] handle. The engine's forwarding service
/// (Go `internal/artifact` `forwarding.go`) uses it to reach the real `ArtifactService`.
#[doc(hidden)]
#[derive(Clone)]
pub struct ArtifactBacking {
    /// The backing service.
    pub service: Arc<dyn gadk_artifact::ArtifactService>,
    /// The scope's app name.
    pub app_name: String,
    /// The scope's user id.
    pub user_id: String,
    /// The scope's session id.
    pub session_id: String,
}

/// Go `agent.Artifacts`: methods to work with artifacts of the current session.
#[async_trait::async_trait]
pub trait Artifacts: Send + Sync {
    /// Saves `data` as a new version of the artifact `name`.
    async fn save(&self, name: &str, data: Part)
    -> Result<gadk_artifact::SaveResponse, AgentError>;
    /// Lists the artifact names of the session.
    async fn list(&self) -> Result<gadk_artifact::ListResponse, AgentError>;
    /// Loads the latest version of the artifact `name`.
    async fn load(&self, name: &str) -> Result<gadk_artifact::LoadResponse, AgentError>;
    /// Loads `version` of the artifact `name` (Go takes an `int`).
    async fn load_version(
        &self,
        name: &str,
        version: i64,
    ) -> Result<gadk_artifact::LoadResponse, AgentError>;
    /// The backing service, for engine-internal forwarding; `None` for handles without one.
    #[doc(hidden)]
    fn backing_service(&self) -> Option<ArtifactBacking> {
        None
    }
}

/// Go `agent.Memory`: access to agent memory across the sessions of the current user.
#[async_trait::async_trait]
pub trait Memory: Send + Sync {
    /// Adds `session` to memory.
    async fn add_session_to_memory(&self, session: Arc<dyn Session>) -> Result<(), AgentError>;
    /// Returns memory entries relevant to `query`.
    async fn search_memory(&self, query: &str) -> Result<gadk_memory::SearchResponse, AgentError>;
}
