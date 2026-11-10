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

//! `SessionClient`: Go's `vertexAiClient` session methods (create, get, list, delete, append,
//! list events), engine-ID resolution, and the name parsers and filter quoting they use.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use chrono::{DateTime, Utc};
use gadk_session::{CreateRequest, DeleteRequest, Event, GetRequest, ListRequest, SessionError};

use super::local::VertexSession;
use crate::client::VertexClient;

/// Go `vertexAiClient`.
pub(crate) struct SessionClient {
    client: VertexClient,
    project_id: String,
    location: String,
    /// The configured engine ID; `""` resolves it from each request's app name.
    reasoning_engine: String,
}

impl SessionClient {
    pub(crate) fn new(
        client: VertexClient,
        project_id: String,
        location: String,
        reasoning_engine: String,
    ) -> Self {
        todo!()
    }

    /// Go `getReasoningEngineID`.
    pub(crate) fn reasoning_engine_id(&self, app_name: &str) -> Result<String, SessionError> {
        todo!()
    }

    pub(crate) async fn create_session(
        &self,
        req: &CreateRequest,
    ) -> Result<VertexSession, SessionError> {
        todo!()
    }

    pub(crate) async fn get_session(
        &self,
        req: &GetRequest,
    ) -> Result<VertexSession, SessionError> {
        todo!()
    }

    pub(crate) async fn list_session_events(
        &self,
        app_name: &str,
        session_id: &str,
        after: Option<DateTime<Utc>>,
        num_recent_events: usize,
    ) -> Result<Vec<Event>, SessionError> {
        todo!()
    }

    pub(crate) async fn list_sessions(
        &self,
        req: &ListRequest,
    ) -> Result<Vec<VertexSession>, SessionError> {
        todo!()
    }

    pub(crate) async fn delete_session(&self, req: &DeleteRequest) -> Result<(), SessionError> {
        todo!()
    }

    pub(crate) async fn append_event(
        &self,
        app_name: &str,
        session_id: &str,
        event: &Event,
    ) -> Result<(), SessionError> {
        todo!()
    }
}

/// Go `quoteFilterLiteral`.
pub(crate) fn quote_filter_literal(value: &str) -> String {
    todo!()
}

/// Go `sessionIdBySessionName` (error text verbatim).
pub(crate) fn session_id_by_session_name(name: &str) -> Result<String, String> {
    todo!()
}

/// Go `sessionIDByOperationName` (error texts verbatim).
pub(crate) fn session_id_by_operation_name(name: &str) -> Result<String, String> {
    todo!()
}
