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

//! The memory service API: entities to interact with agent
//! memory (long-term knowledge).

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use gadk_genai::Content;
use gadk_session::{Event, Session, SessionError};
use serde_json::{Map, Value};

/// The memory service.
///
/// The service ingests sessions into memory so that it can be used for user queries across
/// user-scoped sessions.
#[async_trait]
pub trait MemoryService: Send + Sync {
    /// Adds a session to the memory service.
    ///
    /// A session can be added multiple times during its lifetime.
    async fn add_session_to_memory(&self, session: &dyn Session) -> Result<(), MemoryError>;

    /// Returns memory entries relevant to the given query.
    /// Empty slice is returned if there are no matches.
    async fn search_memory(&self, req: SearchRequest) -> Result<SearchResponse, MemoryError>;

    /// The service's [`EventAdder`], when it can ingest an incremental delta of events;
    /// `None` by default.
    ///
    /// A type that wraps a service forwards this method itself, or its wrapped adder is not
    /// reachable through [`add_events_to_memory`].
    ///
    /// EXPERIMENTAL: its API and behavior may change or be removed later.
    fn as_event_adder(&self) -> Option<&dyn EventAdder> {
        None
    }
}

/// An optional capability of a [`MemoryService`] that can ingest
/// an incremental delta of events without re-ingesting a full session. A service exposes it
/// through [`MemoryService::as_event_adder`]; [`crate::InMemoryMemoryService`] implements it.
///
/// EXPERIMENTAL: its API and behavior may change or be removed later.
#[async_trait]
pub trait EventAdder: Send + Sync {
    /// Appends an incremental batch of events to the memory service for the scope named by
    /// `req`. Events already stored for the scope are kept: the batch
    /// never replaces them. Implementations document how they treat events that repeat an ID
    /// already stored.
    async fn add_events(&self, req: AddEventsRequest) -> Result<(), MemoryError>;
}

/// The parameter of [`EventAdder::add_events`] and
/// [`add_events_to_memory`].
///
/// EXPERIMENTAL: its API and behavior may change or be removed later.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AddEventsRequest {
    /// The application name for memory scope.
    pub app_name: String,
    /// The user ID for memory scope.
    pub user_id: String,
    /// The optional session ID for memory partitioning. In the in-memory service an empty ID
    /// places the events in a shared default bucket per (app, user), distinct from the bucket
    /// of every session, including a session whose ID is `""` added with
    /// [`MemoryService::add_session_to_memory`]. Implementations for which session
    /// partitioning does not apply may ignore it.
    pub session_id: String,
    /// The ordered events to add to memory.
    pub events: Vec<Event>,
    /// Optional, implementation-defined metadata for memory generation. The in-memory service
    /// ignores it.
    pub custom_metadata: Map<String, Value>,
}

/// Adds `req.events` to `svc` through its
/// [`EventAdder`], or returns [`MemoryError::AddEventsUnsupported`] when `svc` has none.
///
/// EXPERIMENTAL: its API and behavior may change or be removed later.
pub async fn add_events_to_memory(
    svc: &dyn MemoryService,
    req: AddEventsRequest,
) -> Result<(), MemoryError> {
    let adder = svc
        .as_event_adder()
        .ok_or(MemoryError::AddEventsUnsupported)?;
    adder.add_events(req).await
}

/// A request for memory search.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SearchRequest {
    /// The search query.
    pub query: String,
    /// The user whose memories are searched.
    pub user_id: String,
    /// The app whose memories are searched.
    pub app_name: String,
}

/// The response from a memory search.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SearchResponse {
    /// The matching memory entries.
    pub memories: Vec<MemoryEntry>,
}

/// A single memory entry.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MemoryEntry {
    /// The unique identifier of the memory.
    pub id: String,
    /// The main content of the memory.
    pub content: Option<Content>,
    /// Author of the memory.
    pub author: String,
    /// When the original content of this memory happened. It is forwarded to the LLM formatted
    /// as ISO 8601. `None` when the event's timestamp is unset (the zero time).
    pub timestamp: Option<DateTime<Utc>>,
    /// Optional custom metadata associated with the memory.
    pub custom_metadata: Map<String, Value>,
}

/// Errors returned by a [`MemoryService`].
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    /// The request was invalid.
    #[error("{0}")]
    InvalidRequest(String),
    /// A session operation failed.
    #[error(transparent)]
    Session(#[from] SessionError),
    /// The storage backend failed.
    #[error(transparent)]
    Backend(Box<dyn std::error::Error + Send + Sync>),
    /// The service does not implement [`EventAdder`]. A caller that receives it can ingest the
    /// full session with [`MemoryService::add_session_to_memory`] instead, as its message says.
    #[error(
        "memory: service does not support adding event deltas; call Service.AddSessionToMemory to ingest the full session"
    )]
    AddEventsUnsupported,
}
