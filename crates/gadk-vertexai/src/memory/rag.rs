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

//! [`VertexAiRagMemoryService`]: a [`MemoryService`] backed by a Vertex AI RAG Engine corpus.
//!
//! Each session is stored as one RAG file named `{app_name}.{user_id}.{session_id}`: one JSON
//! line `{"author", "timestamp", "text"}` per event with text. Adding a session again replaces
//! its file. A search runs RAG retrieval over the corpus and keeps the lines of the requesting
//! app and user's files.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use gadk_genai::{Content, ROLE_USER};
use gadk_memory::{MemoryEntry, MemoryError, MemoryService, SearchRequest, SearchResponse};
use gadk_model::HttpClient;
use gadk_platform::env::{EnvProvider, ProcessEnv};
use gadk_session::{Event, Session, zero_time};
use google_cloud_auth::credentials::Credentials;
use http::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::client::VertexClient;
use crate::config::{host_url, resolve};
use crate::error::{BoxError, VertexError, memory_error, wrap};

/// The description every uploaded RAG file carries.
const FILE_DESCRIPTION: &str = "gadk session memory";

/// How a [`VertexAiRagMemoryService`] reaches its RAG corpus: the corpus, the retrieval
/// options, and the `endpoint`, `credentials`, `without_authentication`, `http_client` and `env`
/// settings of the underlying client.
#[derive(Clone)]
#[non_exhaustive]
pub struct VertexAiRagMemoryConfig {
    rag_corpus: String,
    similarity_top_k: Option<u32>,
    vector_distance_threshold: Option<f64>,
    endpoint: Option<String>,
    credentials: Option<Credentials>,
    without_authentication: bool,
    http_client: Option<Arc<dyn HttpClient>>,
    env: Arc<dyn EnvProvider>,
}

impl VertexAiRagMemoryConfig {
    /// A config for the corpus `rag_corpus`
    /// (`projects/{project}/locations/{location}/ragCorpora/{id}`), with the RAG Engine's
    /// default retrieval options, the regional endpoint, and Application Default Credentials.
    pub fn new(rag_corpus: impl Into<String>) -> Self {
        Self {
            rag_corpus: rag_corpus.into(),
            similarity_top_k: None,
            vector_distance_threshold: None,
            endpoint: None,
            credentials: None,
            without_authentication: false,
            http_client: None,
            env: Arc::new(ProcessEnv),
        }
    }

    /// The number of contexts retrieval returns (`topK`); unset uses the RAG Engine default.
    /// Retrieval runs over the whole corpus before the app/user filter, so a shared corpus may
    /// need a larger value.
    #[must_use]
    pub fn with_similarity_top_k(mut self, top_k: u32) -> Self {
        self.similarity_top_k = Some(top_k);
        self
    }

    /// Only contexts within this vector distance are returned
    /// (`filter.vectorDistanceThreshold`); unset sends no filter.
    #[must_use]
    pub fn with_vector_distance_threshold(mut self, threshold: f64) -> Self {
        self.vector_distance_threshold = Some(threshold);
        self
    }

    /// The base URL; unset = `https://{location}-aiplatform.googleapis.com` for the corpus's
    /// location.
    #[must_use]
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = Some(endpoint.into());
        self
    }

    /// The credentials sent with every request; unset resolves Application Default
    /// Credentials with the `cloud-platform` scope.
    #[must_use]
    pub fn with_credentials(mut self, credentials: Credentials) -> Self {
        self.credentials = Some(credentials);
        self
    }

    /// Send no credentials.
    #[must_use]
    pub fn without_authentication(mut self) -> Self {
        self.without_authentication = true;
        self
    }

    /// The HTTP client; unset builds a reqwest client.
    #[must_use]
    pub fn with_http_client(mut self, http_client: Arc<dyn HttpClient>) -> Self {
        self.http_client = Some(http_client);
        self
    }

    /// Where `GOOGLE_APPLICATION_CREDENTIALS` is read; unset reads the process environment.
    #[must_use]
    pub fn with_env(mut self, env: Arc<dyn EnvProvider>) -> Self {
        self.env = env;
        self
    }
}

impl std::fmt::Debug for VertexAiRagMemoryConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VertexAiRagMemoryConfig")
            .field("rag_corpus", &self.rag_corpus)
            .field("similarity_top_k", &self.similarity_top_k)
            .field("vector_distance_threshold", &self.vector_distance_threshold)
            .field("endpoint", &self.endpoint)
            .field("credentials", &self.credentials.is_some())
            .field("without_authentication", &self.without_authentication)
            .field("http_client", &self.http_client.is_some())
            .finish_non_exhaustive()
    }
}

/// A [`MemoryService`] that stores sessions as files of a Vertex AI RAG Engine corpus and
/// searches them with RAG retrieval.
///
/// Each session becomes one RAG file, `{app_name}.{user_id}.{session_id}`, holding one JSON
/// line per event with text; adding the session again replaces the file. Search retrieves
/// contexts from the whole corpus, then keeps those of the requesting app and user. Indexing is
/// asynchronous, so a session may not be searchable right after it is added. Incremental event
/// adds are not supported.
#[non_exhaustive]
pub struct VertexAiRagMemoryService {
    client: VertexClient,
    /// `projects/{p}/locations/{l}/ragCorpora/{id}`.
    corpus: String,
    /// `projects/{p}/locations/{l}`.
    parent: String,
    similarity_top_k: Option<u32>,
    vector_distance_threshold: Option<f64>,
}

impl VertexAiRagMemoryService {
    /// A service for `config`. Synchronous; only Application Default Credentials need a Tokio
    /// runtime.
    ///
    /// # Errors
    ///
    /// [`MemoryError::InvalidRequest`] when the corpus name is not
    /// `projects/{project}/locations/{location}/ragCorpora/{id}`; [`MemoryError::Backend`]
    /// (`failed to create the RAG memory client: …`) when Application Default Credentials
    /// cannot be built.
    pub fn new(config: VertexAiRagMemoryConfig) -> Result<Self, MemoryError> {
        let Some((project, location)) = corpus_parts(&config.rag_corpus) else {
            return Err(MemoryError::InvalidRequest(format!(
                "invalid RAG corpus name {}: expected \
                 projects/{{project}}/locations/{{location}}/ragCorpora/{{id}}",
                config.rag_corpus
            )));
        };
        let parent = format!("projects/{project}/locations/{location}");
        let default_endpoint = format!("https://{}", host_url(location));
        let resolved = resolve(
            config.endpoint.as_deref(),
            &default_endpoint,
            config.credentials,
            config.without_authentication,
            config.http_client,
            config.env.as_ref(),
        )
        .map_err(|e| memory_error(wrap("failed to create the RAG memory client", e)))?;
        Ok(Self {
            client: VertexClient::new(resolved),
            corpus: config.rag_corpus,
            parent,
            similarity_top_k: config.similarity_top_k,
            vector_distance_threshold: config.vector_distance_threshold,
        })
    }

    /// Deletes the RAG file `name` and waits for the delete operation; a 404 reply means the
    /// file is already gone.
    async fn delete_file(&self, name: &str) -> Result<(), BoxError> {
        let op = match self
            .client
            .call(
                Method::DELETE,
                &format!("/v1/{name}"),
                &[],
                ("name", name),
                None,
            )
            .await
        {
            Ok(op) => op,
            Err(e)
                if e.downcast_ref::<VertexError>()
                    .is_some_and(|v| v.status == 404) =>
            {
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        self.client.wait_operation(op).await.map(|_| ())
    }
}

#[async_trait]
impl MemoryService for VertexAiRagMemoryService {
    /// Stores the session as its RAG file: deletes every file with the session's display name
    /// (waiting for each delete), then uploads the new file when the session has text.
    ///
    /// # Errors
    ///
    /// [`MemoryError::InvalidRequest`] when the app name or user id is empty or contains `.`
    /// (nothing is sent); [`MemoryError::Backend`] with `list rag files failed: …`,
    /// `delete rag file <name> failed: …` or `upload rag file failed: …`.
    async fn add_session_to_memory(&self, session: &dyn Session) -> Result<(), MemoryError> {
        let display = display_name(session.app_name(), session.user_id(), session.id())?;
        let file = session_file(&session.events());
        let files = self
            .client
            .list_all(
                &format!("/v1/{}/ragFiles", self.corpus),
                &[],
                ("parent", &self.corpus),
                "ragFiles",
            )
            .await
            .map_err(|e| memory_error(wrap("list rag files failed", e)))?;
        for found in &files {
            if found.get("displayName").and_then(Value::as_str) != Some(display.as_str()) {
                continue;
            }
            let name = found
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            self.delete_file(name)
                .await
                .map_err(|e| memory_error(wrap(&format!("delete rag file {name} failed"), e)))?;
        }
        if file.is_empty() {
            return Ok(());
        }
        let metadata = json!({
            "rag_file": {"display_name": display, "description": FILE_DESCRIPTION},
        });
        let reply = self
            .client
            .upload_multipart(
                &format!("/v1/{}/ragFiles:upload", self.corpus),
                ("parent", &self.corpus),
                &metadata,
                &format!("{display}.txt"),
                "text/plain",
                file.as_bytes(),
            )
            .await
            .map_err(|e| memory_error(wrap("upload rag file failed", e)))?;
        if let Some(error) = reply.get("error").filter(|e| e.is_object()) {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            return Err(memory_error(wrap("upload rag file failed", message)));
        }
        Ok(())
    }

    /// Retrieves contexts for `req.query` from the corpus and returns the lines of the
    /// requesting app and user's files, one entry per line, without repeats. An empty query
    /// returns no memories and sends nothing.
    ///
    /// # Errors
    ///
    /// [`MemoryError::InvalidRequest`] when the app name or user id is empty or contains `.`
    /// (nothing is sent); [`MemoryError::Backend`]: `retrieve contexts failed: …`.
    async fn search_memory(&self, req: SearchRequest) -> Result<SearchResponse, MemoryError> {
        if req.query.is_empty() {
            return Ok(SearchResponse::default());
        }
        let prefix = scope_prefix(&req.app_name, &req.user_id)?;
        let mut retrieval = Map::new();
        if let Some(top_k) = self.similarity_top_k {
            retrieval.insert("topK".to_owned(), json!(top_k));
        }
        if let Some(threshold) = self.vector_distance_threshold {
            retrieval.insert(
                "filter".to_owned(),
                json!({ "vectorDistanceThreshold": threshold }),
            );
        }
        let mut query = json!({ "text": req.query });
        if !retrieval.is_empty() {
            query["ragRetrievalConfig"] = Value::Object(retrieval);
        }
        let body = json!({
            "vertexRagStore": {"ragResources": [{"ragCorpus": self.corpus}]},
            "query": query,
        });
        let resp = self
            .client
            .call(
                Method::POST,
                &format!("/v1/{}:retrieveContexts", self.parent),
                &[],
                ("parent", &self.parent),
                Some(&body),
            )
            .await
            .map_err(|e| memory_error(wrap("retrieve contexts failed", e)))?;
        Ok(SearchResponse {
            memories: memories_from_contexts(&resp, &prefix),
        })
    }
}

/// `(project, location)` of `projects/{project}/locations/{location}/ragCorpora/{id}` (no part
/// empty), else `None`.
fn corpus_parts(name: &str) -> Option<(&str, &str)> {
    let parts: Vec<&str> = name.split('/').collect();
    match parts.as_slice() {
        ["projects", project, "locations", location, "ragCorpora", id]
            if !project.is_empty() && !location.is_empty() && !id.is_empty() =>
        {
            Some((project, location))
        }
        _ => None,
    }
}

/// The display-name prefix `{app}.{user}.` of an app and user's RAG files; an empty app or
/// user, or one holding `.`, is rejected so the prefix stays unambiguous (a dotted user id
/// could otherwise match another user's dotted session id).
fn scope_prefix(app: &str, user: &str) -> Result<String, MemoryError> {
    if app.is_empty() || user.is_empty() || app.contains('.') || user.contains('.') {
        return Err(MemoryError::InvalidRequest(format!(
            "invalid RAG memory scope: app name {app:?} and user id {user:?} must be non-empty \
             and contain no '.'"
        )));
    }
    Ok(format!("{app}.{user}."))
}

/// The RAG file display name `{app}.{user}.{session}`, with the scope checked as
/// [`scope_prefix`] does.
fn display_name(app: &str, user: &str, session: &str) -> Result<String, MemoryError> {
    Ok(format!("{}{session}", scope_prefix(app, user)?))
}

/// One line of a session file.
#[derive(Serialize, Deserialize)]
struct Line {
    #[serde(default)]
    author: String,
    /// Seconds since the Unix epoch; `0` for an unset time.
    #[serde(default)]
    timestamp: f64,
    text: String,
}

/// The session file: for each event with at least one non-empty, non-thought text part, one
/// JSON line `{"author", "timestamp", "text"}` (the parts joined with `\n`), lines separated
/// by `\n`. Empty when no event has text.
pub(super) fn session_file(events: &[Event]) -> String {
    let mut out = String::new();
    for event in events {
        let Some(content) = &event.response.content else {
            continue;
        };
        let texts: Vec<&str> = content
            .parts
            .iter()
            .filter(|p| !p.thought && !p.text.is_empty())
            .map(|p| p.text.as_str())
            .collect();
        if texts.is_empty() {
            continue;
        }
        let line = Line {
            author: event.author.clone(),
            timestamp: unix_seconds(event.timestamp),
            text: texts.join("\n"),
        };
        // A struct of strings and a float always serializes.
        let Ok(json) = serde_json::to_string(&line) else {
            continue;
        };
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&json);
    }
    out
}

/// Fractional seconds since the Unix epoch; `0` for [`zero_time`].
fn unix_seconds(t: DateTime<Utc>) -> f64 {
    if t == zero_time() {
        return 0.0;
    }
    t.timestamp() as f64 + f64::from(t.timestamp_subsec_nanos()) / 1e9
}

/// The time `secs` seconds after the Unix epoch; `None` for `0` (or a value out of range).
fn time_from_seconds(secs: f64) -> Option<DateTime<Utc>> {
    if secs == 0.0 || !secs.is_finite() {
        return None;
    }
    let whole = secs.floor();
    let nanos = ((secs - whole) * 1e9).round().min(999_999_999.0) as u32;
    DateTime::from_timestamp(whole as i64, nanos)
}

/// The memories in a `retrieveContexts` reply: the contexts whose `sourceDisplayName` starts
/// with `prefix`, split into lines, each line that parses as a session-file line (with `text`)
/// becoming one user-role entry (empty `id`, `custom_metadata.source_display_name`); repeats of
/// the same author, timestamp and text are dropped; retrieval order is kept.
pub(super) fn memories_from_contexts(resp: &Value, prefix: &str) -> Vec<MemoryEntry> {
    let contexts = resp
        .pointer("/contexts/contexts")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut seen = HashSet::new();
    let mut memories = Vec::new();
    for context in contexts {
        let display = context
            .get("sourceDisplayName")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !display.starts_with(prefix) {
            continue;
        }
        let text = context
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        for raw in text.split('\n') {
            let Ok(line) = serde_json::from_str::<Line>(raw) else {
                continue;
            };
            if !seen.insert((
                line.author.clone(),
                line.timestamp.to_bits(),
                line.text.clone(),
            )) {
                continue;
            }
            let mut custom_metadata = Map::new();
            custom_metadata.insert(
                "source_display_name".to_owned(),
                Value::String(display.to_owned()),
            );
            memories.push(MemoryEntry {
                id: String::new(),
                content: Some(Content::from_text(line.text, ROLE_USER)),
                author: line.author,
                timestamp: time_from_seconds(line.timestamp),
                custom_metadata,
            });
        }
    }
    memories
}
