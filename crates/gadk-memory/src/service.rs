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

//! The memory service API (port of Go memory/service.go): entities to interact with agent
//! memory (long-term knowledge).

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use gadk_genai::Content;
use gadk_session::{Session, SessionError};
use serde_json::{Map, Value};

/// Port of Go memory.Service: a definition of the memory service.
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
}

/// Port of Go memory.SearchRequest: a request for memory search.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SearchRequest {
    /// The search query.
    pub query: String,
    /// The user whose memories are searched.
    pub user_id: String,
    /// The app whose memories are searched.
    pub app_name: String,
}

/// Port of Go memory.SearchResponse: the response from a memory search.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SearchResponse {
    /// The matching memory entries.
    pub memories: Vec<MemoryEntry>,
}

/// Port of Go memory.Entry: a single memory entry.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MemoryEntry {
    /// The unique identifier of the memory.
    pub id: String,
    /// The main content of the memory.
    pub content: Option<Content>,
    /// Author of the memory.
    pub author: String,
    /// When the original content of this memory happened. It is forwarded to the LLM formatted
    /// as ISO 8601. `None` stands for Go's zero `time.Time`.
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
}
