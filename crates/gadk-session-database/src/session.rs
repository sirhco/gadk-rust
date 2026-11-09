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

//! The session handed out by the service (Go `localSession`).
#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use chrono::{DateTime, Utc};
use gadk_session::{Event, Session, SessionError, State};
use parking_lot::RwLock;
use serde_json::Value;

use crate::rows::StateMap;

/// A session read from or created in the database. `append_event` on the service downcasts to
/// it through `Session::as_any`.
pub(crate) struct DatabaseSession {
    app_name: String,
    user_id: String,
    id: String,
    data: SessionData,
}

/// The mutable part of a [`DatabaseSession`] behind one lock (Go `localSession.mu`); also its
/// [`State`].
pub(crate) struct SessionData(RwLock<Inner>);

struct Inner {
    state: StateMap,
    events: Vec<Event>,
    updated_at: DateTime<Utc>,
}

impl DatabaseSession {
    pub(crate) fn new(
        app_name: String,
        user_id: String,
        id: String,
        state: StateMap,
        events: Vec<Event>,
        updated_at: DateTime<Utc>,
    ) -> Self {
        todo!("Task 5")
    }

    /// Go `localSession.appendEvent`: a partial event is ignored; otherwise the whole state
    /// delta (temp: keys included) is copied into the session state and the event, with temp:
    /// keys trimmed from its delta, is appended.
    pub(crate) fn append_event(&self, event: &Event) {
        todo!("Task 5")
    }

    pub(crate) fn updated_at(&self) -> DateTime<Utc> {
        todo!("Task 5")
    }

    pub(crate) fn set_updated_at(&self, t: DateTime<Utc>) {
        todo!("Task 5")
    }

    /// Test hook for the snapshot port: replaces the event at `index`.
    #[cfg(test)]
    pub(crate) fn replace_event(&self, index: usize, event: Event) {
        todo!("Task 5")
    }
}

impl Session for DatabaseSession {
    fn id(&self) -> &str {
        todo!("Task 5")
    }
    fn app_name(&self) -> &str {
        todo!("Task 5")
    }
    fn user_id(&self) -> &str {
        todo!("Task 5")
    }
    fn state(&self) -> &dyn State {
        todo!("Task 5")
    }
    fn events(&self) -> Vec<Event> {
        todo!("Task 5")
    }
    fn last_update_time(&self) -> DateTime<Utc> {
        todo!("Task 5")
    }
    fn as_any(&self) -> &dyn std::any::Any {
        todo!("Task 5")
    }
}

impl State for SessionData {
    fn get(&self, key: &str) -> Result<Value, SessionError> {
        todo!("Task 5")
    }
    fn set(&self, key: &str, value: Value) -> Result<(), SessionError> {
        todo!("Task 5")
    }
    fn all(&self) -> Vec<(String, Value)> {
        todo!("Task 5")
    }
}
