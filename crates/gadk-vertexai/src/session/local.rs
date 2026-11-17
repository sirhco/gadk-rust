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
//! returns.

use std::sync::{PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use chrono::{DateTime, Utc};
use gadk_session::state_util::trim_temp_delta_state;
use gadk_session::{Event, Session, SessionError, State};
use serde_json::{Map, Value};

/// A session read from or created in an Agent Engine: a local copy of
/// its state and events. [`Session::events`] returns a snapshot, so callers iterate without
/// holding the session lock.
pub struct VertexSession {
    app_name: String,
    user_id: String,
    session_id: String,
    data: LocalData,
}

/// The mutable part, behind one lock; also the session's [`State`].
struct LocalData(RwLock<Inner>);

struct Inner {
    events: Vec<Event>,
    state: Map<String, Value>,
    updated_at: DateTime<Utc>,
}

impl LocalData {
    // A panic while the lock was held cannot leave `Inner` half-updated in a way that
    // matters to readers, so a poisoned lock is used as it is.
    fn read(&self) -> RwLockReadGuard<'_, Inner> {
        self.0.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> RwLockWriteGuard<'_, Inner> {
        self.0.write().unwrap_or_else(PoisonError::into_inner)
    }
}

impl VertexSession {
    pub(crate) fn new(
        app_name: String,
        user_id: String,
        session_id: String,
        state: Map<String, Value>,
        updated_at: DateTime<Utc>,
    ) -> Self {
        Self {
            app_name,
            user_id,
            session_id,
            data: LocalData(RwLock::new(Inner {
                events: Vec::new(),
                state,
                updated_at,
            })),
        }
    }

    /// Replaces the session's events (assigned after listing them).
    pub(crate) fn set_events(&self, events: Vec<Event>) {
        self.data.write().events = events;
    }

    /// Appends an event locally: ignores a partial event; otherwise applies the event's
    /// whole state delta (`temp:` keys included), stores
    /// the event without its `temp:` keys and takes its timestamp as the last update time.
    pub(crate) fn append_event(&self, event: &Event) -> Result<(), SessionError> {
        if event.response.partial {
            return Ok(());
        }
        let mut inner = self.data.write();
        // Applying the state delta cannot fail.
        if let Some(delta) = &event.actions.state_delta {
            for (k, v) in delta {
                inner.state.insert(k.clone(), v.clone());
            }
        }
        let processed = trim_temp_delta_state(event);
        inner.updated_at = processed.timestamp;
        inner.events.push(processed);
        Ok(())
    }
}

impl Session for VertexSession {
    fn id(&self) -> &str {
        &self.session_id
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

    fn events(&self) -> Vec<Event> {
        self.data.read().events.clone()
    }

    fn last_update_time(&self) -> DateTime<Utc> {
        self.data.read().updated_at
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl State for LocalData {
    fn get(&self, key: &str) -> Result<Value, SessionError> {
        self.read()
            .state
            .get(key)
            .cloned()
            .ok_or(SessionError::StateKeyNotExist)
    }

    fn set(&self, key: &str, value: Value) -> Result<(), SessionError> {
        self.write().state.insert(key.to_owned(), value);
        Ok(())
    }

    fn all(&self) -> Vec<(String, Value)> {
        self.read()
            .state
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use chrono::TimeZone;
    use gadk_genai::Content;
    use gadk_model::LlmResponse;
    use gadk_session::{EventActions, zero_time};
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    fn session() -> VertexSession {
        let state = json!({"k": "v"}).as_object().unwrap().clone();
        VertexSession::new(
            "app".into(),
            "user".into(),
            "sess".into(),
            state,
            zero_time(),
        )
    }

    #[test]
    fn identity_and_initial_state() {
        let s = session();
        assert_eq!((s.id(), s.app_name(), s.user_id()), ("sess", "app", "user"));
        assert_eq!(s.last_update_time(), zero_time());
        assert!(s.events().is_empty());
        assert_eq!(s.state().get("k").unwrap(), json!("v"));
        assert!(matches!(
            s.state().get("missing"),
            Err(SessionError::StateKeyNotExist)
        ));
        assert!(s.as_any().downcast_ref::<VertexSession>().is_some());
    }

    #[test]
    fn state_set_and_all() {
        let s = session();
        s.state().set("k2", json!(2)).unwrap();
        let mut all = s.state().all();
        all.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            all,
            vec![("k".to_string(), json!("v")), ("k2".to_string(), json!(2))]
        );
    }

    #[test]
    fn append_event_applies_whole_delta_and_stores_trimmed_event() {
        let s = session();
        let ts = Utc.with_ymd_and_hms(2025, 1, 2, 3, 4, 5).unwrap();
        let event = Event {
            id: "e1".into(),
            timestamp: ts,
            actions: EventActions {
                state_delta: Some(
                    json!({"temp:scratch": 1, "user:u": "x", "k": null})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
                ..Default::default()
            },
            response: LlmResponse {
                content: Some(Content::from_text("hi", "user")),
                ..Default::default()
            },
            ..Default::default()
        };
        s.append_event(&event).unwrap();

        // Go's updateSessionState copies every key, temp: included, nulls included.
        assert_eq!(s.state().get("temp:scratch").unwrap(), json!(1));
        assert_eq!(s.state().get("user:u").unwrap(), json!("x"));
        assert_eq!(s.state().get("k").unwrap(), Value::Null);
        // The stored event has no temp: keys; the caller's event is untouched.
        let stored = s.events();
        assert_eq!(stored.len(), 1);
        assert_eq!(
            stored[0].actions.state_delta,
            Some(
                json!({"user:u": "x", "k": null})
                    .as_object()
                    .unwrap()
                    .clone()
            )
        );
        assert!(
            event
                .actions
                .state_delta
                .as_ref()
                .unwrap()
                .contains_key("temp:scratch")
        );
        assert_eq!(s.last_update_time(), ts);
    }

    #[test]
    fn append_event_ignores_partial_events() {
        let s = session();
        let event = Event {
            timestamp: Utc.with_ymd_and_hms(2025, 1, 2, 3, 4, 5).unwrap(),
            actions: EventActions {
                state_delta: Some(json!({"k": "new"}).as_object().unwrap().clone()),
                ..Default::default()
            },
            response: LlmResponse {
                partial: true,
                ..Default::default()
            },
            ..Default::default()
        };
        s.append_event(&event).unwrap();
        assert!(s.events().is_empty());
        assert_eq!(s.state().get("k").unwrap(), json!("v"));
        assert_eq!(s.last_update_time(), zero_time());
    }

    #[test]
    fn append_event_without_delta_keeps_state() {
        let s = session();
        s.append_event(&Event::default()).unwrap();
        assert_eq!(s.state().all(), vec![("k".to_string(), json!("v"))]);
        assert_eq!(s.events().len(), 1);
    }

    #[test]
    fn set_events_replaces_events() {
        let s = session();
        s.append_event(&Event {
            id: "old".into(),
            ..Default::default()
        })
        .unwrap();
        s.set_events(vec![Event {
            id: "a".into(),
            ..Default::default()
        }]);
        let ids: Vec<String> = s.events().into_iter().map(|e| e.id).collect();
        assert_eq!(ids, vec!["a".to_string()]);
    }
}
