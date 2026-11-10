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

//! [`VertexAiMemoryService`]: a [`MemoryService`] backed by Vertex AI Agent Engine Memory Bank
//! (port of ADK-Go `memory/vertexai`).

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::sync::Arc;

use async_trait::async_trait;
use gadk_memory::{MemoryError, MemoryService, SearchRequest, SearchResponse};
use gadk_model::HttpClient;
use gadk_platform::env::{EnvProvider, ProcessEnv};
use gadk_session::Session;
use google_cloud_auth::credentials::Credentials;

use crate::AgentEngineData;
use crate::client::VertexClient;

#[cfg(test)]
mod tests;

/// How a [`VertexAiMemoryService`] reaches its Memory Bank (Go `ServiceConfig` plus the
/// `option.ClientOption`s Go passes to `aiplatform.NewMemoryBankClient`).
#[derive(Clone)]
pub struct VertexAiMemoryServiceConfig {
    /// The Agent Engine whose Memory Bank is used (Go's embedded `AgentEngineData`).
    pub engine: AgentEngineData,
    /// The session state key holding the time of the session's last memory update (Go
    /// `StateKeySessionLastUpdateTime`): when set, only events newer than that time are sent;
    /// `None` generates memories from the whole session.
    pub state_key_session_last_update_time: Option<String>,
    /// Wait for memory generation to finish (Go `WaitForCompletion`).
    pub wait_for_completion: bool,
    /// The base URL (Go `option.WithEndpoint`); `None` = `https://{location}-aiplatform.googleapis.com`
    /// (`https://aiplatform.googleapis.com` for an empty or `global` location).
    pub endpoint: Option<String>,
    /// The credentials sent with every request (Go `option.WithAuthCredentials`); `None`
    /// resolves Application Default Credentials through [`env`](Self::env) with the
    /// `cloud-platform` scope.
    pub credentials: Option<Credentials>,
    /// Send no credentials (Go `option.WithoutAuthentication`).
    pub without_authentication: bool,
    /// The HTTP client (Go `option.WithHTTPClient`); `None` builds a reqwest client.
    pub http_client: Option<Arc<dyn HttpClient>>,
    /// Where `GOOGLE_APPLICATION_CREDENTIALS` is read.
    pub env: Arc<dyn EnvProvider>,
}

impl Default for VertexAiMemoryServiceConfig {
    fn default() -> Self {
        Self {
            engine: AgentEngineData::default(),
            state_key_session_last_update_time: None,
            wait_for_completion: false,
            endpoint: None,
            credentials: None,
            without_authentication: false,
            http_client: None,
            env: Arc::new(ProcessEnv),
        }
    }
}

impl std::fmt::Debug for VertexAiMemoryServiceConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VertexAiMemoryServiceConfig")
            .field("engine", &self.engine)
            .field(
                "state_key_session_last_update_time",
                &self.state_key_session_last_update_time,
            )
            .field("wait_for_completion", &self.wait_for_completion)
            .field("endpoint", &self.endpoint)
            .field("credentials", &self.credentials.is_some())
            .field("without_authentication", &self.without_authentication)
            .field("http_client", &self.http_client.is_some())
            .finish_non_exhaustive()
    }
}

/// A [`MemoryService`] that generates and retrieves memories with a Vertex AI Agent Engine
/// Memory Bank (Go `vertexAIService`).
pub struct VertexAiMemoryService {
    client: VertexClient,
    engine: AgentEngineData,
    state_key_session_last_update_time: Option<String>,
    wait_for_completion: bool,
}

impl VertexAiMemoryService {
    /// A service for `config` (Go `NewMemoryService`). Synchronous; only Application Default
    /// Credentials need a Tokio runtime.
    ///
    /// # Errors
    ///
    /// [`MemoryError::Backend`] with Go's text `newVertexAIClient failed:
    /// aiplatform.NewMemoryBankClient failed: {adc error}` when Application Default Credentials
    /// cannot be built.
    pub fn new(config: VertexAiMemoryServiceConfig) -> Result<Self, MemoryError> {
        todo!()
    }
}

#[async_trait]
impl MemoryService for VertexAiMemoryService {
    async fn add_session_to_memory(&self, session: &dyn Session) -> Result<(), MemoryError> {
        todo!()
    }

    async fn search_memory(&self, req: SearchRequest) -> Result<SearchResponse, MemoryError> {
        todo!()
    }
}
