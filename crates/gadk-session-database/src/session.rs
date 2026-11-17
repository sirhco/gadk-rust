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

//! The session handed out by the service.

use chrono::{DateTime, Utc};
use gadk_session::state_util::trim_temp_delta_state;
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

/// The mutable part of a [`DatabaseSession`] behind one lock; also its
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
        Self {
            app_name,
            user_id,
            id,
            data: SessionData(RwLock::new(Inner {
                state,
                events,
                updated_at,
            })),
        }
    }

    /// Appends an event: a partial event is ignored; otherwise the whole state
    /// delta (temp: keys included) is copied into the session state and the event, with temp:
    /// keys trimmed from its delta, is appended.
    pub(crate) fn append_event(&self, event: &Event) {
        if event.response.partial {
            return;
        }
        let mut inner = self.data.0.write();
        if let Some(delta) = &event.actions.state_delta {
            inner
                .state
                .extend(delta.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        inner.events.push(trim_temp_delta_state(event));
    }

    pub(crate) fn updated_at(&self) -> DateTime<Utc> {
        self.data.0.read().updated_at
    }

    pub(crate) fn set_updated_at(&self, t: DateTime<Utc>) {
        self.data.0.write().updated_at = t;
    }

    /// Test hook for the snapshot port: replaces the event at `index`.
    #[cfg(test)]
    pub(crate) fn replace_event(&self, index: usize, event: Event) {
        if let Some(slot) = self.data.0.write().events.get_mut(index) {
            *slot = event;
        }
    }
}

impl Session for DatabaseSession {
    fn id(&self) -> &str {
        &self.id
    }
    fn app_name(&self) -> &str {
        &self.app_name
    }
    fn user_id(&self) -> &str {
        &self.user_id
    }
    fn state(&self) -> &dyn State {
        &self.data
    }
    /// A snapshot: later appends and replacements do not change it.
    fn events(&self) -> Vec<Event> {
        self.data.0.read().events.clone()
    }
    fn last_update_time(&self) -> DateTime<Utc> {
        self.updated_at()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl State for SessionData {
    fn get(&self, key: &str) -> Result<Value, SessionError> {
        self.0
            .read()
            .state
            .get(key)
            .cloned()
            .ok_or(SessionError::StateKeyNotExist)
    }
    fn set(&self, key: &str, value: Value) -> Result<(), SessionError> {
        self.0.write().state.insert(key.to_string(), value);
        Ok(())
    }
    fn all(&self) -> Vec<(String, Value)> {
        self.0
            .read()
            .state
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}
