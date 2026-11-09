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

//! Port of ADK-Go `session/inmemory.go` and `internal/sessionutils`.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use gadk_platform::{Platform, go_quote};
use parking_lot::RwLock;
use serde_json::{Map, Value};

use crate::error::SessionError;
use crate::event::{Event, zero_time};
use crate::service::{
    CreateRequest, DeleteRequest, GetRequest, KEY_PREFIX_APP, KEY_PREFIX_USER, ListRequest,
    Session, SessionService, State,
};
use crate::state_util::{extract_state_deltas, merge_states, require, trim_temp_delta_state};

type StateMap = Map<String, Value>;
/// `(app_name, user_id, session_id)`, ordered like Go's `ordered.Encode` key.
type SessionKey = (String, String, String);

/// An in-memory [`SessionService`] (ADK-Go `session.InMemoryService`). Thread-safe.
///
/// State keys are routed by prefix: `app:` keys are shared by every session of the app,
/// `user:` keys by every session of the user, and the rest belong to the session. `temp:`
/// keys in appended event deltas are never persisted; as in ADK-Go, `temp:` keys in the
/// initial state given to `create` are kept in session state. Sessions handed out are
/// copies of the service's record.
///
/// An event appended with an empty ID is given one:
///
/// ```
/// # tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async {
/// use gadk_session::*;
/// use std::sync::Arc;
/// let svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
/// let s = svc.create(CreateRequest { app_name: "app".into(), user_id: "u".into(), ..Default::default() }).await.unwrap();
/// let mut ev = Event::new(&gadk_platform::Platform::default(), "inv");
/// ev.id.clear();
/// svc.append_event(s.as_ref(), &mut ev).await.unwrap();
/// assert!(!ev.id.is_empty());
/// # });
/// ```
pub struct InMemorySessionService {
    platform: Platform,
    inner: RwLock<Inner>,
}

#[derive(Default)]
struct Inner {
    sessions: BTreeMap<SessionKey, StoredSession>,
    app_state: HashMap<String, StateMap>,
    user_state: HashMap<(String, String), StateMap>,
}

/// The service's canonical record of a session.
struct StoredSession {
    state: StateMap,
    events: Vec<Event>,
    updated_at: DateTime<Utc>,
}

impl InMemorySessionService {
    /// A new, empty service using [`Platform::default`].
    pub fn new() -> Self {
        Self::with_platform(Platform::default())
    }

    /// A new, empty service taking time and session/event IDs from `platform`.
    pub fn with_platform(platform: Platform) -> Self {
        Self {
            platform,
            inner: RwLock::new(Inner::default()),
        }
    }
}

impl Default for InMemorySessionService {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for InMemorySessionService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InMemorySessionService")
            .finish_non_exhaustive()
    }
}

impl Inner {
    /// Go `mergeStates`: session state plus the app and user state, prefixed back.
    fn merged_state(&self, state: &StateMap, app_name: &str, user_id: &str) -> StateMap {
        merge_states(
            self.app_state.get(app_name),
            self.user_state
                .get(&(app_name.to_string(), user_id.to_string())),
            state,
        )
    }

    /// Applies a state delta to the app, user and session maps (Go `ExtractStateDeltas`
    /// followed by `updateAppState`/`updateUserState`/`maps.Copy`).
    fn apply_delta(
        app_state: &mut HashMap<String, StateMap>,
        user_state: &mut HashMap<(String, String), StateMap>,
        session_state: &mut StateMap,
        delta: &StateMap,
        app_name: &str,
        user_id: &str,
    ) {
        let (app, user, session) = extract_state_deltas(delta);
        app_state
            .entry(app_name.to_string())
            .or_default()
            .extend(app);
        user_state
            .entry((app_name.to_string(), user_id.to_string()))
            .or_default()
            .extend(user);
        session_state.extend(session);
    }
}

#[async_trait]
impl SessionService for InMemorySessionService {
    async fn create(&self, req: CreateRequest) -> Result<Arc<dyn Session>, SessionError> {
        if req.app_name.is_empty() || req.user_id.is_empty() {
            return Err(SessionError::InvalidRequest(format!(
                "app_name and user_id are required, got app_name: {}, user_id: {}",
                go_quote(&req.app_name),
                go_quote(&req.user_id)
            )));
        }
        let session_id = match req.session_id {
            Some(id) if !id.is_empty() => id,
            _ => self.platform.new_uuid(),
        };
        let key = (req.app_name, req.user_id, session_id);

        let mut inner = self.inner.write();
        if inner.sessions.contains_key(&key) {
            return Err(SessionError::InvalidRequest(format!(
                "session {} already exists",
                key.2
            )));
        }

        let Inner {
            sessions,
            app_state,
            user_state,
        } = &mut *inner;
        // Go stores `MergeStates(app, user, req.State)` with the raw request state: app: and
        // user: keys go to the shared maps (and are merged back on read), and every other
        // key, temp: included, stays in session state. Only appended deltas are trimmed.
        let mut state = StateMap::new();
        if let Some(initial) = &req.state {
            let (app, user, _) = extract_state_deltas(initial);
            app_state.entry(key.0.clone()).or_default().extend(app);
            user_state
                .entry((key.0.clone(), key.1.clone()))
                .or_default()
                .extend(user);
            state.extend(
                initial
                    .iter()
                    .filter(|(k, _)| {
                        !k.starts_with(KEY_PREFIX_APP) && !k.starts_with(KEY_PREFIX_USER)
                    })
                    .map(|(k, v)| (k.clone(), v.clone())),
            );
        }
        let updated_at = self.platform.now();
        let merged = merge_states(
            app_state.get(&key.0),
            user_state.get(&(key.0.clone(), key.1.clone())),
            &state,
        );
        sessions.insert(
            key.clone(),
            StoredSession {
                state,
                events: Vec::new(),
                updated_at,
            },
        );
        drop(inner);

        Ok(Arc::new(InMemorySession::new(
            key,
            merged,
            Vec::new(),
            updated_at,
        )))
    }

    async fn get(&self, req: GetRequest) -> Result<Arc<dyn Session>, SessionError> {
        require(&[
            ("app_name", &req.app_name),
            ("user_id", &req.user_id),
            ("session_id", &req.session_id),
        ])?;
        let key = (req.app_name, req.user_id, req.session_id);

        let inner = self.inner.read();
        let stored = inner.sessions.get(&key).ok_or(SessionError::NotFound)?;
        let state = inner.merged_state(&stored.state, &key.0, &key.1);

        // Go order: the recent-events limit first, then the timestamp filter.
        let mut events: &[Event] = &stored.events;
        if req.num_recent_events > 0 {
            let start = events.len().saturating_sub(req.num_recent_events);
            events = &events[start..];
        }
        if let Some(after) = req.after.filter(|t| *t != zero_time()) {
            // Events are sorted by timestamp: keep from the first one not before `after`.
            let first = events.partition_point(|e| e.timestamp < after);
            events = &events[first..];
        }
        let events = events.to_vec();
        let updated_at = stored.updated_at;
        drop(inner);

        Ok(Arc::new(InMemorySession::new(
            key, state, events, updated_at,
        )))
    }

    async fn list(&self, req: ListRequest) -> Result<Vec<Arc<dyn Session>>, SessionError> {
        require(&[("app_name", &req.app_name)])?;

        let inner = self.inner.read();
        let sessions = inner
            .sessions
            .iter()
            .filter(|((app, user, _), _)| {
                *app == req.app_name && (req.user_id.is_empty() || *user == req.user_id)
            })
            .map(|(key, stored)| {
                let state = inner.merged_state(&stored.state, &key.0, &key.1);
                Arc::new(InMemorySession::new(
                    key.clone(),
                    state,
                    Vec::new(),
                    stored.updated_at,
                )) as Arc<dyn Session>
            })
            .collect();
        Ok(sessions)
    }

    async fn delete(&self, req: DeleteRequest) -> Result<(), SessionError> {
        require(&[
            ("app_name", &req.app_name),
            ("user_id", &req.user_id),
            ("session_id", &req.session_id),
        ])?;
        let key = (req.app_name, req.user_id, req.session_id);
        self.inner.write().sessions.remove(&key);
        Ok(())
    }

    async fn append_event(
        &self,
        session: &dyn Session,
        event: &mut Event,
    ) -> Result<(), SessionError> {
        // Partial events are not stored.
        if event.response.partial {
            return Ok(());
        }
        // Give the event an identity if it arrived without one, the same way a missing
        // session ID is filled in on create.
        if event.id.is_empty() {
            event.id = self.platform.new_uuid();
        }

        let live = session
            .as_any()
            .downcast_ref::<InMemorySession>()
            .ok_or_else(|| {
                SessionError::InvalidRequest(
                    "session not created by InMemorySessionService".to_string(),
                )
            })?;

        let mut inner = self.inner.write();
        let Inner {
            sessions,
            app_state,
            user_state,
        } = &mut *inner;
        let stored = sessions.get_mut(&live.key).ok_or(SessionError::NotFound)?;

        let trimmed = trim_temp_delta_state(event);

        // Update the caller's session: Go `session.appendEvent` copies the whole delta
        // (temp: keys included) into its state and records the trimmed event.
        {
            let mut data = live.data.write();
            if let Some(delta) = &event.actions.state_delta {
                data.state
                    .extend(delta.iter().map(|(k, v)| (k.clone(), v.clone())));
            }
            data.events.push(trimmed.clone());
            data.updated_at = event.timestamp;
        }

        // Update the canonical record.
        stored.events.push(trimmed);
        stored.updated_at = event.timestamp;
        if let Some(delta) = event.actions.state_delta.as_ref().filter(|d| !d.is_empty()) {
            Inner::apply_delta(
                app_state,
                user_state,
                &mut stored.state,
                delta,
                &live.key.0,
                &live.key.1,
            );
        }
        Ok(())
    }
}

/// A session handed out by [`InMemorySessionService`]: a copy of the service's record.
struct InMemorySession {
    key: SessionKey,
    data: LiveState,
}

/// The mutable part of an [`InMemorySession`], behind one lock (Go `session.mu`).
struct LiveState(RwLock<LiveData>);

struct LiveData {
    state: StateMap,
    events: Vec<Event>,
    updated_at: DateTime<Utc>,
}

impl LiveState {
    fn write(&self) -> parking_lot::RwLockWriteGuard<'_, LiveData> {
        self.0.write()
    }
}

impl InMemorySession {
    fn new(
        key: SessionKey,
        state: StateMap,
        events: Vec<Event>,
        updated_at: DateTime<Utc>,
    ) -> Self {
        Self {
            key,
            data: LiveState(RwLock::new(LiveData {
                state,
                events,
                updated_at,
            })),
        }
    }
}

impl Session for InMemorySession {
    fn id(&self) -> &str {
        &self.key.2
    }

    fn app_name(&self) -> &str {
        &self.key.0
    }

    fn user_id(&self) -> &str {
        &self.key.1
    }

    fn state(&self) -> &dyn State {
        &self.data
    }

    fn events(&self) -> Vec<Event> {
        self.data.0.read().events.clone()
    }

    fn last_update_time(&self) -> DateTime<Utc> {
        self.data.0.read().updated_at
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl State for LiveState {
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
        // A snapshot taken under the lock, so iterating it never races a writer.
        self.0
            .read()
            .state
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}
