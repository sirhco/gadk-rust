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

//! `impl SessionService for DatabaseSessionService` (Go `service.go`).
#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::sync::Arc;

use async_trait::async_trait;
use gadk_session::{
    CreateRequest, DeleteRequest, Event, GetRequest, ListRequest, Session, SessionError,
    SessionService,
};

use crate::DatabaseSessionService;

#[async_trait]
impl SessionService for DatabaseSessionService {
    async fn create(&self, req: CreateRequest) -> Result<Arc<dyn Session>, SessionError> {
        todo!("Task 7")
    }
    async fn get(&self, req: GetRequest) -> Result<Arc<dyn Session>, SessionError> {
        todo!("Task 7")
    }
    async fn list(&self, req: ListRequest) -> Result<Vec<Arc<dyn Session>>, SessionError> {
        todo!("Task 7")
    }
    async fn delete(&self, req: DeleteRequest) -> Result<(), SessionError> {
        todo!("Task 7")
    }
    async fn append_event(
        &self,
        session: &dyn Session,
        event: &mut Event,
    ) -> Result<(), SessionError> {
        todo!("Task 7")
    }
}
