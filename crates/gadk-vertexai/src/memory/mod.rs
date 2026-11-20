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
//! (the Memory Bank of an Agent Engine).
//!
//! [`rag::VertexAiRagMemoryService`] is a [`MemoryService`] backed by a Vertex AI RAG Engine
//! corpus instead.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use gadk_genai::{Content, ROLE_USER};
use gadk_memory::{MemoryEntry, MemoryError, MemoryService, SearchRequest, SearchResponse};
use gadk_model::HttpClient;
use gadk_platform::env::{EnvProvider, ProcessEnv};
use gadk_platform::{go_parse_rfc3339, json_kind_article};
use gadk_session::Session;
use google_cloud_auth::credentials::Credentials;
use http::Method;
use serde_json::{Value, json};

use crate::AgentEngineData;
use crate::client::VertexClient;
use crate::config::{host_url, resolve};
use crate::error::{BoxError, memory_error, wrap};
use crate::protojson::format_timestamp;

pub mod rag;

pub use rag::{VertexAiRagMemoryConfig, VertexAiRagMemoryService};

#[cfg(test)]
mod rag_live_tests;
#[cfg(test)]
mod rag_tests;

#[cfg(test)]
mod tests;

/// How a [`VertexAiMemoryService`] reaches its Memory Bank: the engine and memory-generation
/// settings, plus the `endpoint`, `credentials`, `without_authentication`, `http_client` and
/// `env` settings of the underlying client.
#[derive(Clone)]
pub struct VertexAiMemoryServiceConfig {
    /// The Agent Engine whose Memory Bank is used.
    pub engine: AgentEngineData,
    /// The session state key holding the time of the session's last memory update: when set,
    /// only events newer than that time are sent;
    /// `None` generates memories from the whole session.
    pub state_key_session_last_update_time: Option<String>,
    /// Wait for memory generation to finish.
    pub wait_for_completion: bool,
    /// The base URL; `None` = `https://{location}-aiplatform.googleapis.com`
    /// (`https://aiplatform.googleapis.com` for an empty or `global` location).
    pub endpoint: Option<String>,
    /// The credentials sent with every request; `None`
    /// resolves Application Default Credentials through [`env`](Self::env) with the
    /// `cloud-platform` scope.
    pub credentials: Option<Credentials>,
    /// Send no credentials.
    pub without_authentication: bool,
    /// The HTTP client; `None` builds a reqwest client.
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
/// Memory Bank.
pub struct VertexAiMemoryService {
    client: VertexClient,
    engine: AgentEngineData,
    state_key_session_last_update_time: Option<String>,
    wait_for_completion: bool,
}

impl VertexAiMemoryService {
    /// A service for `config`. Synchronous; only Application Default
    /// Credentials need a Tokio runtime.
    ///
    /// # Errors
    ///
    /// [`MemoryError::Backend`] with the client-construction
    /// error text (see the literal in this function) when Application Default Credentials cannot
    /// be built.
    pub fn new(config: VertexAiMemoryServiceConfig) -> Result<Self, MemoryError> {
        // The default endpoint is the location's host, served over HTTPS.
        let default_endpoint = format!("https://{}", host_url(&config.engine.location));
        let resolved = resolve(
            config.endpoint.as_deref(),
            &default_endpoint,
            config.credentials,
            config.without_authentication,
            config.http_client,
            config.env.as_ref(),
        )
        .map_err(|e| memory_error(wrap("failed to create the Memory Bank client", e)))?;
        Ok(Self {
            client: VertexClient::new(resolved),
            engine: config.engine,
            state_key_session_last_update_time: config.state_key_session_last_update_time,
            wait_for_completion: config.wait_for_completion,
        })
    }

    /// `GenerateMemories` for the session (`start` = only events after it), then, with
    /// `wait_for_completion`, the operation's wait. The parent is the configured engine as given
    /// (no app-name resolution); the session resource uses the session's ID.
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
            .map_err(|e| wrap("generate memories failed", e))?;
        if self.wait_for_completion {
            // A done operation without a result is success: the
            // `operation finished without a response` error is accepted, on both paths.
            self.client
                .wait_operation(op)
                .await
                .map_err(|e| wrap("waiting for the generate-memories operation failed", e))?;
        }
        Ok(())
    }
}

/// The Memory Bank scope `{"user_id": user_id}`.
fn user_scope(user_id: &str) -> Value {
    json!({ "user_id": user_id })
}

/// The time a state value holds: an RFC 3339 string. Anything else fails with
/// `expected an RFC 3339 timestamp, found <kind> (<value as JSON>)` (just `found null` for null).
fn state_time(value: &Value) -> Result<DateTime<Utc>, BoxError> {
    if let Value::String(s) = value
        && let Some(t) = go_parse_rfc3339(s)
    {
        return Ok(t);
    }
    let found = match value {
        Value::Null => "null".to_owned(),
        other => format!("{} ({other})", json_kind_article(other)),
    };
    Err(format!("expected an RFC 3339 timestamp, found {found}").into())
}

#[async_trait]
impl MemoryService for VertexAiMemoryService {
    /// Adds the session to memory: without a state key (or with an empty one) the whole session
    /// is sent; with one, only the events after the time stored under it.
    ///
    /// # Errors
    ///
    /// [`MemoryError::Backend`] with a wrapper chain (see the literals in this
    /// function): a failed state read, a state value that is not a timestamp, or a failed
    /// whole-session or newer-events add, around a failed memory generation or a failed wait for
    /// the whole-session generation operation.
    async fn add_session_to_memory(&self, session: &dyn Session) -> Result<(), MemoryError> {
        let Some(key) = self
            .state_key_session_last_update_time
            .as_deref()
            .filter(|k| !k.is_empty())
        else {
            return self
                .add_session(session, None)
                .await
                .map_err(|e| memory_error(wrap("adding the whole session to memory failed", e)));
        };
        let value = session
            .state()
            .get(key)
            .map_err(|e| memory_error(wrap(&format!("reading state key {key} failed"), e)))?;
        let start = state_time(&value).map_err(memory_error)?;
        self.add_session(session, Some(start))
            .await
            .map_err(|e| memory_error(wrap("adding new session events to memory failed", e)))
    }

    /// Searches memory: `RetrieveMemories` by similarity to `req.query` within the user's
    /// scope; each retrieved fact becomes a user-role text entry.
    ///
    /// # Errors
    ///
    /// [`MemoryError::Backend`]: `retrieve memories failed: …`.
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
            .map_err(|e| memory_error(wrap("retrieve memories failed", e)))?;
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
