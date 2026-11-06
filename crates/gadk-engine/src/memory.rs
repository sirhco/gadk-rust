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

//! Scoped memory (port of Go `internal/memory`).

use std::sync::Arc;

use async_trait::async_trait;
use gadk_agent::{AgentError, Memory};
use gadk_memory::{MemoryService, SearchRequest, SearchResponse};
use gadk_session::Session;

/// Port of Go internal/memory Memory: a [`Memory`] scoped to one app and user. Go's `SessionID`
/// field is unused by its methods and is dropped.
#[derive(Clone)]
pub(crate) struct ScopedMemory {
    service: Arc<dyn MemoryService>,
    app_name: String,
    user_id: String,
}

impl ScopedMemory {
    /// Go `&memory.Memory{Service, AppName, UserID}`.
    pub(crate) fn new(service: Arc<dyn MemoryService>, app: &str, user: &str) -> Self {
        Self {
            service,
            app_name: app.to_string(),
            user_id: user.to_string(),
        }
    }
}

#[async_trait]
impl Memory for ScopedMemory {
    /// Port of Go Memory.AddSessionToMemory.
    async fn add_session_to_memory(&self, session: Arc<dyn Session>) -> Result<(), AgentError> {
        Ok(self.service.add_session_to_memory(session.as_ref()).await?)
    }

    /// Port of Go Memory.SearchMemory.
    async fn search_memory(&self, query: &str) -> Result<SearchResponse, AgentError> {
        Ok(self
            .service
            .search_memory(SearchRequest {
                app_name: self.app_name.clone(),
                user_id: self.user_id.clone(),
                query: query.to_string(),
            })
            .await?)
    }
}
