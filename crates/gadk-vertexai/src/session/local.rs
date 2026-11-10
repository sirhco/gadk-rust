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

//! [`VertexSession`]: the session a [`VertexAiSessionService`](super::VertexAiSessionService)
//! returns (port of Go `localSession`).

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::sync::Mutex;

use chrono::{DateTime, Utc};
use gadk_session::{Event, Session, SessionError, State};
use serde_json::{Map, Value};

/// A session read from or created in an Agent Engine (Go `localSession`).
pub struct VertexSession {
    app_name: String,
    user_id: String,
    session_id: String,
    state: Mutex<Map<String, Value>>,
    events: Mutex<Vec<Event>>,
    updated_at: Mutex<DateTime<Utc>>,
}

impl VertexSession {
    pub(crate) fn new(
        app_name: String,
        user_id: String,
        session_id: String,
        state: Map<String, Value>,
        updated_at: DateTime<Utc>,
    ) -> Self {
        todo!()
    }

    pub(crate) fn set_events(&self, events: Vec<Event>) {
        todo!()
    }

    /// Go `localSession.appendEvent`.
    pub(crate) fn append_event(&self, event: &Event) -> Result<(), SessionError> {
        todo!()
    }
}

impl Session for VertexSession {
    fn id(&self) -> &str {
        todo!()
    }

    fn app_name(&self) -> &str {
        todo!()
    }

    fn user_id(&self) -> &str {
        todo!()
    }

    fn state(&self) -> &dyn State {
        todo!()
    }

    fn events(&self) -> Vec<Event> {
        todo!()
    }

    fn last_update_time(&self) -> DateTime<Utc> {
        todo!()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        todo!()
    }
}
