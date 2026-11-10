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

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use gadk_genai::{Content, ROLE_USER};
use gadk_memory::{MemoryEntry, MemoryError, MemoryService, SearchRequest, SearchResponse};
use gadk_model::HttpClient;
use gadk_platform::env::{EnvProvider, ProcessEnv};
use gadk_platform::{go_parse_rfc3339, go_sprint_value};
use gadk_session::Session;
use google_cloud_auth::credentials::Credentials;
use http::Method;
use serde_json::{Value, json};

use crate::AgentEngineData;
use crate::client::VertexClient;
use crate::config::{host_url, resolve};
use crate::error::{BoxError, memory_error, wrap};
use crate::protojson::format_timestamp;

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
        // Go `option.WithEndpoint(aiplatformutil.HostPortURL(location))`: `{host}:443`, served
        // over HTTPS.
        let default_endpoint = format!("https://{}", host_url(&config.engine.location));
        let resolved = resolve(
            config.endpoint.as_deref(),
            &default_endpoint,
            config.credentials,
            config.without_authentication,
            config.http_client,
            config.env.as_ref(),
        )
        .map_err(|e| {
            memory_error(wrap(
                "newVertexAIClient failed",
                wrap("aiplatform.NewMemoryBankClient failed", e),
            ))
        })?;
        Ok(Self {
            client: VertexClient::new(resolved),
            engine: config.engine,
            state_key_session_last_update_time: config.state_key_session_last_update_time,
            wait_for_completion: config.wait_for_completion,
        })
    }

    /// Go `vertexAIClient.addSession`: `GenerateMemories` for the session (`start` = only
    /// events after it), then, with `wait_for_completion`, the operation's wait. Go's parent is
    /// the configured engine as given (no app-name resolution); the session resource uses the
    /// session's ID.
    async fn add_session(
        &self,
        session: &dyn Session,
        start: Option<DateTime<Utc>>,
    ) -> Result<(), BoxError> {
        let parent = self.engine.engine_resource();
        let mut source = json!({ "session": self.engine.session_resource(session.id()) });
        if let Some(start) = start {
            source["startTime"] = Value::String(format_timestamp(&start));
        }
        let body = json!({
            "parent": parent,
            "vertexSessionSource": source,
            "scope": user_scope(session.user_id()),
        });
        let op = self
            .client
            .call(
                Method::POST,
                &format!("/v1beta1/{parent}/memories:generate"),
                &[],
                ("parent", &parent),
                Some(&body),
            )
            .await
            .map_err(|e| wrap("v.client.GenerateMemories failed", e))?;
        if self.wait_for_completion {
            // A done operation without a result is success (Go accepts `op.Wait`'s
            // `unsupported result type <nil>: <nil>`); Go uses this text on both paths.
            self.client
                .wait_operation(op)
                .await
                .map_err(|e| wrap("op.Wait for GenerateMemories (whole session) failed", e))?;
        }
        Ok(())
    }
}

/// Go `createUserScope`: the Memory Bank scope `{"user_id": user_id}`.
fn user_scope(user_id: &str) -> Value {
    json!({ "user_id": user_id })
}

/// The time a state value holds: an RFC 3339 string (Go stores a `time.Time`, whose JSON form
/// this is). Anything else fails with Go's `want type time.Time, got : %T (%v)`, the value
/// rendered as Go renders it after `json.Unmarshal` into `any`.
fn state_time(value: &Value) -> Result<DateTime<Utc>, BoxError> {
    if let Value::String(s) = value
        && let Some(t) = go_parse_rfc3339(s)
    {
        return Ok(t);
    }
    Err(format!(
        "want type time.Time, got : {} ({})",
        go_type_name(value),
        go_sprint_value(value)
    )
    .into())
}

/// Go `%T` of a JSON value decoded into `any`.
fn go_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "<nil>",
        Value::Bool(_) => "bool",
        Value::Number(_) => "float64",
        Value::String(_) => "string",
        Value::Array(_) => "[]interface {}",
        Value::Object(_) => "map[string]interface {}",
    }
}

#[async_trait]
impl MemoryService for VertexAiMemoryService {
    /// Go `AddSessionToMemory`: without a state key (or with an empty one) the whole session
    /// is sent; with one, only the events after the time stored under it.
    ///
    /// # Errors
    ///
    /// [`MemoryError::Backend`] with Go's wrapper chain: `state.Get({key}) failed: …`,
    /// `want type time.Time, got : …`, `v.client.addWholeSession failed: …` or
    /// `v.client.addEventsNewerThan failed: …` around `v.client.GenerateMemories failed: …` or
    /// `op.Wait for GenerateMemories (whole session) failed: …`.
    async fn add_session_to_memory(&self, session: &dyn Session) -> Result<(), MemoryError> {
        let Some(key) = self
            .state_key_session_last_update_time
            .as_deref()
            .filter(|k| !k.is_empty())
        else {
            return self
                .add_session(session, None)
                .await
                .map_err(|e| memory_error(wrap("v.client.addWholeSession failed", e)));
        };
        let value = session
            .state()
            .get(key)
            .map_err(|e| memory_error(wrap(&format!("state.Get({key}) failed"), e)))?;
        let start = state_time(&value).map_err(memory_error)?;
        self.add_session(session, Some(start))
            .await
            .map_err(|e| memory_error(wrap("v.client.addEventsNewerThan failed", e)))
    }

    /// Go `SearchMemory`: `RetrieveMemories` by similarity to `req.query` within the user's
    /// scope; each retrieved fact becomes a user-role text entry.
    ///
    /// # Errors
    ///
    /// [`MemoryError::Backend`]: `v.client.RetrieveMemories failed: …`.
    async fn search_memory(&self, req: SearchRequest) -> Result<SearchResponse, MemoryError> {
        let parent = self.engine.engine_resource();
        // protojson omits an empty `searchQuery`; the set oneof stays as `{}`.
        let mut params = serde_json::Map::new();
        if !req.query.is_empty() {
            params.insert("searchQuery".to_owned(), Value::String(req.query));
        }
        let body = json!({
            "parent": parent,
            "similaritySearchParams": params,
            "scope": user_scope(&req.user_id),
        });
        let resp = self
            .client
            .call(
                Method::POST,
                &format!("/v1beta1/{parent}/memories:retrieve"),
                &[],
                ("parent", &parent),
                Some(&body),
            )
            .await
            .map_err(|e| memory_error(wrap("v.client.RetrieveMemories failed", e)))?;
        let memories = resp
            .get("retrievedMemories")
            .and_then(Value::as_array)
            .map(|retrieved| {
                retrieved
                    .iter()
                    .map(|m| {
                        let fact = m
                            .pointer("/memory/fact")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        MemoryEntry {
                            content: Some(Content::from_text(fact, ROLE_USER)),
                            ..MemoryEntry::default()
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(SearchResponse { memories })
    }
}
