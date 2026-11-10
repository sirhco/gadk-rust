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

//! [`VertexAiSessionService`]: a [`SessionService`] backed by Vertex AI Agent Engine sessions
//! (port of ADK-Go `session/vertexai`).

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

mod client;
pub(crate) mod convert;
mod local;

use std::sync::Arc;

use async_trait::async_trait;
use gadk_model::HttpClient;
use gadk_platform::env::{EnvProvider, ProcessEnv};
use gadk_session::{
    CreateRequest, DeleteRequest, Event, GetRequest, ListRequest, Session, SessionError,
    SessionService,
};
use google_cloud_auth::credentials::Credentials;

pub use local::VertexSession;

/// How a [`VertexAiSessionService`] reaches its Agent Engine (Go `VertexAIServiceConfig` plus
/// the `option.ClientOption`s Go passes to `aiplatform.NewSessionClient`).
#[derive(Clone)]
pub struct VertexAiSessionServiceConfig {
    /// The Google Cloud project ID (Go `ProjectID`).
    pub project_id: String,
    /// The engine's location (Go `Location`).
    pub location: String,
    /// The reasoning engine's numeric ID (Go `ReasoningEngine`); `None` resolves the engine
    /// from each request's app name: a numeric app name is the ID, and an app name of the form
    /// `projects/…/locations/…/reasoningEngines/{id}` names it.
    pub reasoning_engine: Option<String>,
    /// The base URL (Go `option.WithEndpoint`); `None` = `https://aiplatform.googleapis.com`,
    /// whatever the location (Go's session client default).
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

impl Default for VertexAiSessionServiceConfig {
    fn default() -> Self {
        Self {
            project_id: String::new(),
            location: String::new(),
            reasoning_engine: None,
            endpoint: None,
            credentials: None,
            without_authentication: false,
            http_client: None,
            env: Arc::new(ProcessEnv),
        }
    }
}

impl std::fmt::Debug for VertexAiSessionServiceConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VertexAiSessionServiceConfig")
            .field("project_id", &self.project_id)
            .field("location", &self.location)
            .field("reasoning_engine", &self.reasoning_engine)
            .field("endpoint", &self.endpoint)
            .field("credentials", &self.credentials.is_some())
            .field("without_authentication", &self.without_authentication)
            .field("http_client", &self.http_client.is_some())
            .finish_non_exhaustive()
    }
}

/// A [`SessionService`] that keeps sessions and events in a Vertex AI Agent Engine (Go
/// `vertexAIService`).
pub struct VertexAiSessionService {
    client: client::SessionClient,
}

impl VertexAiSessionService {
    /// A service for `config` (Go `NewSessionService`). Synchronous; only Application Default
    /// Credentials need a Tokio runtime.
    ///
    /// # Errors
    ///
    /// [`SessionError::Backend`] with Go's text `failed to create Vertex AI client: could not
    /// establish connection to the aiplatform server: {adc error}` when Application Default
    /// Credentials cannot be built.
    pub fn new(config: VertexAiSessionServiceConfig) -> Result<Self, SessionError> {
        todo!()
    }
}

#[async_trait]
impl SessionService for VertexAiSessionService {
    async fn create(&self, req: CreateRequest) -> Result<Arc<dyn Session>, SessionError> {
        todo!()
    }

    async fn get(&self, req: GetRequest) -> Result<Arc<dyn Session>, SessionError> {
        todo!()
    }

    async fn list(&self, req: ListRequest) -> Result<Vec<Arc<dyn Session>>, SessionError> {
        todo!()
    }

    async fn delete(&self, req: DeleteRequest) -> Result<(), SessionError> {
        todo!()
    }

    async fn append_event(
        &self,
        session: &dyn Session,
        event: &mut Event,
    ) -> Result<(), SessionError> {
        todo!()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::collections::HashMap;

    use gadk_platform::env::MapEnv;

    use super::*;

    #[test]
    fn config_default_and_debug() {
        let config = VertexAiSessionServiceConfig::default();
        assert_eq!(config.project_id, "");
        assert_eq!(config.reasoning_engine, None);
        assert_eq!(config.endpoint, None);
        assert!(!config.without_authentication);
        assert_eq!(
            format!("{config:?}"),
            "VertexAiSessionServiceConfig { project_id: \"\", location: \"\", reasoning_engine: \
             None, endpoint: None, credentials: false, without_authentication: false, \
             http_client: false, .. }"
        );
        let config = VertexAiSessionServiceConfig {
            project_id: "p".into(),
            location: "l".into(),
            reasoning_engine: Some("1".into()),
            endpoint: Some("http://127.0.0.1:1".into()),
            without_authentication: true,
            env: Arc::new(MapEnv(HashMap::new())),
            ..Default::default()
        };
        assert_eq!(
            format!("{config:?}"),
            "VertexAiSessionServiceConfig { project_id: \"p\", location: \"l\", reasoning_engine: \
             Some(\"1\"), endpoint: Some(\"http://127.0.0.1:1\"), credentials: false, \
             without_authentication: true, http_client: false, .. }"
        );
    }
}
