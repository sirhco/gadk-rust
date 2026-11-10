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

//! Test fixtures (feature `testutil`): a port of Go `server/adkrest/internal/fakes`
//! `testsessionservice.go`, the session service the Go controller tests run against. Plan 5a
//! Task 13 adds the wire-scenario builders.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use gadk_session::{
    CreateRequest, DeleteRequest, Event, GetRequest, ListRequest, Session, SessionError,
    SessionService, State,
};
use parking_lot::Mutex;
use serde_json::{Map, Value};

/// Go `fakes.TestState`: a map with Go map reference semantics (clones share it).
#[derive(Debug, Clone, Default)]
pub struct TestState(pub Arc<Mutex<Map<String, Value>>>);

impl From<Map<String, Value>> for TestState {
    fn from(m: Map<String, Value>) -> Self {
        Self(Arc::new(Mutex::new(m)))
    }
}

impl State for TestState {
    /// Go: a missing key reads as `nil` without error.
    fn get(&self, key: &str) -> Result<Value, SessionError> {
        Ok(self.0.lock().get(key).cloned().unwrap_or(Value::Null))
    }

    fn set(&self, key: &str, value: Value) -> Result<(), SessionError> {
        self.0.lock().insert(key.to_owned(), value);
        Ok(())
    }

    fn all(&self) -> Vec<(String, Value)> {
        self.0
            .lock()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

/// Go `fakes.TestEvents`: the session's events (clones share them, as the Go fake's appended
/// session and its stored copy do).
#[derive(Debug, Clone, Default)]
pub struct TestEvents(pub Arc<Mutex<Vec<Event>>>);

impl From<Vec<Event>> for TestEvents {
    fn from(v: Vec<Event>) -> Self {
        Self(Arc::new(Mutex::new(v)))
    }
}

/// Go `fakes.SessionKey`.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionKey {
    /// The application name.
    pub app_name: String,
    /// The user ID.
    pub user_id: String,
    /// The session ID.
    pub session_id: String,
}

/// Go `fakes.TestSession`.
#[derive(Debug, Clone)]
pub struct TestSession {
    /// Go `Id`.
    pub id: SessionKey,
    /// Go `SessionState`.
    pub session_state: TestState,
    /// Go `SessionEvents`.
    pub session_events: TestEvents,
    /// Go `UpdatedAt` (shared by clones, like the events).
    pub updated_at: Arc<Mutex<DateTime<Utc>>>,
}

impl TestSession {
    /// A session with the given parts.
    pub fn new(
        id: SessionKey,
        state: Map<String, Value>,
        events: Vec<Event>,
        updated_at: DateTime<Utc>,
    ) -> Self {
        Self {
            id,
            session_state: state.into(),
            session_events: events.into(),
            updated_at: Arc::new(Mutex::new(updated_at)),
        }
    }
}

impl Session for TestSession {
    fn id(&self) -> &str {
        &self.id.session_id
    }

    fn app_name(&self) -> &str {
        &self.id.app_name
    }

    fn user_id(&self) -> &str {
        &self.id.user_id
    }

    fn state(&self) -> &dyn State {
        &self.session_state
    }

    fn events(&self) -> Vec<Event> {
        self.session_events.0.lock().clone()
    }

    fn last_update_time(&self) -> DateTime<Utc> {
        *self.updated_at.lock()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Go `fakes.FakeSessionService`: sessions in a map keyed by [`SessionKey`] (iterated in key
/// order; Go's map order is unspecified). Errors carry Go's texts (`session already exists`,
/// `not found`, `invalid session type`) as [`SessionError::Backend`].
#[derive(Debug, Default)]
pub struct FakeSessionService {
    /// Go `Sessions`.
    pub sessions: Mutex<BTreeMap<SessionKey, TestSession>>,
}

impl FakeSessionService {
    /// A service holding `sessions`.
    pub fn new(sessions: impl IntoIterator<Item = TestSession>) -> Self {
        Self {
            sessions: Mutex::new(sessions.into_iter().map(|s| (s.id.clone(), s)).collect()),
        }
    }

    /// A snapshot of the stored session `key`.
    pub fn session(&self, key: &SessionKey) -> Option<TestSession> {
        self.sessions.lock().get(key).cloned()
    }
}

fn backend(msg: &str) -> SessionError {
    SessionError::Backend(msg.into())
}

#[async_trait]
impl SessionService for FakeSessionService {
    async fn create(&self, req: CreateRequest) -> Result<Arc<dyn Session>, SessionError> {
        let mut sessions = self.sessions.lock();
        let mut key = SessionKey {
            app_name: req.app_name,
            user_id: req.user_id,
            session_id: req.session_id.unwrap_or_default(),
        };
        if sessions.contains_key(&key) {
            return Err(backend("session already exists"));
        }
        if key.session_id.is_empty() {
            key.session_id = "testID".into();
        }
        let session = TestSession::new(
            key.clone(),
            req.state.unwrap_or_default(),
            Vec::new(),
            Utc::now(),
        );
        sessions.insert(key, session.clone());
        Ok(Arc::new(session))
    }

    async fn get(&self, req: GetRequest) -> Result<Arc<dyn Session>, SessionError> {
        let key = SessionKey {
            app_name: req.app_name,
            user_id: req.user_id,
            session_id: req.session_id,
        };
        match self.sessions.lock().get(&key) {
            Some(s) => Ok(Arc::new(s.clone())),
            None => Err(backend("not found")),
        }
    }

    async fn list(&self, req: ListRequest) -> Result<Vec<Arc<dyn Session>>, SessionError> {
        Ok(self
            .sessions
            .lock()
            .values()
            .filter(|s| s.id.app_name == req.app_name && s.id.user_id == req.user_id)
            .map(|s| Arc::new(s.clone()) as Arc<dyn Session>)
            .collect())
    }

    async fn delete(&self, req: DeleteRequest) -> Result<(), SessionError> {
        let key = SessionKey {
            app_name: req.app_name,
            user_id: req.user_id,
            session_id: req.session_id,
        };
        match self.sessions.lock().remove(&key) {
            Some(_) => Ok(()),
            None => Err(backend("not found")),
        }
    }

    async fn append_event(
        &self,
        session: &dyn Session,
        event: &mut Event,
    ) -> Result<(), SessionError> {
        let mut sessions = self.sessions.lock();
        let Some(s) = session.as_any().downcast_ref::<TestSession>() else {
            return Err(backend("invalid session type"));
        };
        s.session_events.0.lock().push(event.clone());
        *s.updated_at.lock() = event.timestamp;
        sessions.insert(s.id.clone(), s.clone());
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn key(id: &str) -> SessionKey {
        SessionKey {
            app_name: "app".into(),
            user_id: "u".into(),
            session_id: id.into(),
        }
    }

    #[tokio::test]
    async fn create_get_append_delete_follow_the_go_fake() {
        let svc = FakeSessionService::default();
        let created = svc
            .create(CreateRequest {
                app_name: "app".into(),
                user_id: "u".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(created.id(), "testID");
        let err = svc
            .create(CreateRequest {
                app_name: "app".into(),
                user_id: "u".into(),
                session_id: Some("testID".into()),
                ..Default::default()
            })
            .await
            .err()
            .unwrap();
        assert_eq!(err.to_string(), "session already exists");

        let got = svc
            .get(GetRequest {
                app_name: "app".into(),
                user_id: "u".into(),
                session_id: "testID".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let mut ev = Event {
            id: "e1".into(),
            timestamp: DateTime::from_timestamp(5, 0).unwrap(),
            ..Default::default()
        };
        svc.append_event(&*got, &mut ev).await.unwrap();
        // The appended session and the stored one both see the event, as in Go.
        assert_eq!(got.events().len(), 1);
        let stored = svc.session(&key("testID")).unwrap();
        assert_eq!(stored.events()[0].id, "e1");
        assert_eq!(stored.last_update_time().timestamp(), 5);

        let list = svc
            .list(ListRequest {
                app_name: "app".into(),
                user_id: "u".into(),
            })
            .await
            .unwrap();
        assert_eq!(list.len(), 1);
        svc.delete(DeleteRequest {
            app_name: "app".into(),
            user_id: "u".into(),
            session_id: "testID".into(),
        })
        .await
        .unwrap();
        let err = svc
            .get(GetRequest {
                app_name: "app".into(),
                user_id: "u".into(),
                session_id: "testID".into(),
                ..Default::default()
            })
            .await
            .err()
            .unwrap();
        assert_eq!(err.to_string(), "not found");
    }

    #[tokio::test]
    async fn state_reads_missing_keys_as_null() {
        let s = TestSession::new(key("s"), Map::new(), vec![], Utc::now());
        assert_eq!(s.state().get("nope").unwrap(), Value::Null);
        s.state().set("k", Value::from(1)).unwrap();
        assert_eq!(s.clone().state().get("k").unwrap(), Value::from(1));
    }
}
