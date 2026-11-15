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

//! `impl SessionService for DatabaseSessionService` (Go `service.go`), step for step.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use gadk_session::state_util::{
    extract_state_deltas, go_rfc3339_nano, merge_states, require, trim_temp_delta_state,
};
use gadk_session::{
    CreateRequest, DeleteRequest, Event, GetRequest, ListRequest, Session, SessionError,
    SessionService, zero_time,
};

use crate::DatabaseSessionService;
use crate::backend::Tx;
use crate::errors::{self, wrap};
use crate::rows::{self, AppStateRow, SessionRow, StateMap, UserStateRow};
use crate::session::DatabaseSession;

/// A fetched state row's decoded state, empty when there is no row; errors carry `prefix`.
fn fetched_state(
    found: sqlx::Result<Option<String>>,
    prefix: &str,
) -> Result<StateMap, SessionError> {
    match found {
        Ok(Some(state)) => rows::decode_state(&state).map_err(|e| wrap(prefix, e)),
        Ok(None) => Ok(StateMap::new()),
        Err(e) => Err(wrap(prefix, e)),
    }
}

/// Go `fetchStorageAppState`: the app's state, empty when it has no row.
async fn fetch_app_state(
    tx: &mut Tx,
    app_name: &str,
    for_update: bool,
) -> Result<StateMap, SessionError> {
    let found = tx.select_app_state(app_name, for_update).await;
    fetched_state(
        found.map(|row| row.map(|r| r.state)),
        "failed to fetch app state",
    )
}

/// Go `fetchStorageUserState`.
async fn fetch_user_state(
    tx: &mut Tx,
    app_name: &str,
    user_id: &str,
    for_update: bool,
) -> Result<StateMap, SessionError> {
    let found = tx.select_user_state(app_name, user_id, for_update).await;
    fetched_state(
        found.map(|row| row.map(|r| r.state)),
        "failed to fetch user state",
    )
}

/// Go `lockStateRow` for the app row: insert an empty row if absent (on conflict do nothing) so
/// the locked read that follows (`for_update`) finds a row to lock.
async fn lock_app_state_row(
    tx: &mut Tx,
    app_name: &str,
    now: DateTime<Utc>,
) -> Result<(), SessionError> {
    let empty = AppStateRow {
        app_name: app_name.to_string(),
        state: "{}".to_string(),
        update_time: now,
    };
    tx.insert_app_state_if_absent(&empty)
        .await
        .map_err(|e| wrap("failed to lock app state", e))
}

/// Go `lockStateRow` for the user row (see [`lock_app_state_row`]).
async fn lock_user_state_row(
    tx: &mut Tx,
    app_name: &str,
    user_id: &str,
    now: DateTime<Utc>,
) -> Result<(), SessionError> {
    let empty = UserStateRow {
        app_name: app_name.to_string(),
        user_id: user_id.to_string(),
        state: "{}".to_string(),
        update_time: now,
    };
    tx.insert_user_state_if_absent(&empty)
        .await
        .map_err(|e| wrap("failed to lock user state", e))
}

/// Go `fetchAllAppStorageUserState`: every user's state for the app, by user ID.
async fn fetch_all_user_states(
    tx: &mut Tx,
    app_name: &str,
) -> Result<HashMap<String, StateMap>, SessionError> {
    let rows = tx
        .select_user_states(app_name)
        .await
        .map_err(|e| wrap("failed to fetch user states", e))?;
    rows.into_iter()
        .map(|r| {
            rows::decode_state(&r.state)
                .map(|s| (r.user_id, s))
                .map_err(|e| wrap("failed to fetch user states", e))
        })
        .collect()
}

fn encode(state: &StateMap, prefix: &str) -> Result<String, SessionError> {
    rows::encode_state(state).map_err(|e| wrap(prefix, e))
}

/// The owner of a shared state row.
#[derive(Clone, Copy)]
enum Scope<'a> {
    App { app_name: &'a str },
    User { app_name: &'a str, user_id: &'a str },
}

/// Merges a non-empty `delta` into `state` and saves the app or user row stamped `now`.
async fn save_delta(
    tx: &mut Tx,
    scope: Scope<'_>,
    state: &mut StateMap,
    delta: StateMap,
    now: DateTime<Utc>,
) -> Result<(), SessionError> {
    if delta.is_empty() {
        return Ok(());
    }
    state.extend(delta);
    let prefix = match scope {
        Scope::App { .. } => "failed to save app state",
        Scope::User { .. } => "failed to save user state",
    };
    let state = encode(state, prefix)?;
    let saved = match scope {
        Scope::App { app_name } => {
            let row = AppStateRow {
                app_name: app_name.to_string(),
                state,
                update_time: now,
            };
            tx.save_app_state(&row).await
        }
        Scope::User { app_name, user_id } => {
            let row = UserStateRow {
                app_name: app_name.to_string(),
                user_id: user_id.to_string(),
                state,
                update_time: now,
            };
            tx.save_user_state(&row).await
        }
    };
    saved.map_err(|e| wrap(prefix, e))
}

/// Go `t.Truncate(time.Microsecond)`.
fn truncate_to_micros(t: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::from_timestamp_micros(t.timestamp_micros()).unwrap_or(t)
}

/// Go `time.UnixMicro(us).Format(time.RFC3339Nano)`, in UTC.
fn micros_rfc3339(us: i64) -> String {
    DateTime::from_timestamp_micros(us)
        .map(|t| go_rfc3339_nano(&t))
        .unwrap_or_default()
}

#[async_trait]
impl SessionService for DatabaseSessionService {
    async fn create(&self, req: CreateRequest) -> Result<Arc<dyn Session>, SessionError> {
        if req.app_name.is_empty() || req.user_id.is_empty() {
            return Err(SessionError::InvalidRequest(
                "app_name and user_id are required".to_string(),
            ));
        }
        let session_id = match req.session_id {
            Some(id) if !id.is_empty() => id,
            _ => self.platform.new_uuid(),
        };
        let now = self.platform.now();
        let (app_delta, user_delta, session_state) =
            extract_state_deltas(&req.state.unwrap_or_default());

        let mut tx = self
            .backend
            .begin_write(self.log.clone())
            .await
            .map_err(errors::raw)?;
        // Lock the shared rows this session's state changes, app before user, for the same
        // reasons and in the same order as `apply_event` (Go #1930).
        if !app_delta.is_empty() {
            lock_app_state_row(&mut tx, &req.app_name, now).await?;
        }
        let mut app_state = fetch_app_state(&mut tx, &req.app_name, !app_delta.is_empty())
            .await
            .map_err(|e| wrap("error on create session", e))?;
        if !user_delta.is_empty() {
            lock_user_state_row(&mut tx, &req.app_name, &req.user_id, now).await?;
        }
        let mut user_state =
            fetch_user_state(&mut tx, &req.app_name, &req.user_id, !user_delta.is_empty())
                .await
                .map_err(|e| wrap("error on create session", e))?;
        let app = Scope::App {
            app_name: &req.app_name,
        };
        save_delta(&mut tx, app, &mut app_state, app_delta, now).await?;
        let user = Scope::User {
            app_name: &req.app_name,
            user_id: &req.user_id,
        };
        save_delta(&mut tx, user, &mut user_state, user_delta, now).await?;
        let row = SessionRow {
            app_name: req.app_name.clone(),
            user_id: req.user_id.clone(),
            id: session_id.clone(),
            state: encode(&session_state, "error creating session on database")?,
            create_time: now,
            update_time: now,
        };
        tx.insert_session(&row)
            .await
            .map_err(|e| wrap("error creating session on database", e))?;
        tx.commit().await.map_err(errors::raw)?;

        let merged = merge_states(Some(&app_state), Some(&user_state), &session_state);
        Ok(Arc::new(DatabaseSession::new(
            req.app_name,
            req.user_id,
            session_id,
            merged,
            Vec::new(),
            now,
        )))
    }

    async fn get(&self, req: GetRequest) -> Result<Arc<dyn Session>, SessionError> {
        require(&[
            ("app_name", &req.app_name),
            ("user_id", &req.user_id),
            ("session_id", &req.session_id),
        ])?;
        let mut tx = self
            .backend
            .begin_read(self.log.clone())
            .await
            .map_err(errors::raw)?;
        let row = tx
            .select_session(&req.app_name, &req.user_id, &req.session_id)
            .await
            .map_err(|e| wrap("database error while fetching session", e))?
            .ok_or(SessionError::NotFound)?;
        let after = req.after.filter(|t| *t != zero_time());
        let limit = (req.num_recent_events > 0)
            .then(|| i64::try_from(req.num_recent_events).unwrap_or(i64::MAX));
        let mut event_rows = tx
            .select_events(&req.app_name, &req.user_id, &req.session_id, after, limit)
            .await
            .map_err(|e| wrap("database error while fetching events", e))?;
        let app_state = fetch_app_state(&mut tx, &req.app_name, false)
            .await
            .map_err(|e| wrap("error on get session", e))?;
        let user_state = fetch_user_state(&mut tx, &req.app_name, &req.user_id, false)
            .await
            .map_err(|e| wrap("error on get session", e))?;
        tx.commit().await.map_err(errors::raw)?;

        let session_state = rows::decode_state(&row.state)
            .map_err(|e| wrap("database error while fetching session", e))?;
        // Fetched newest first (for the limit); returned oldest first.
        event_rows.reverse();
        let events = event_rows
            .iter()
            .map(|r| rows::row_to_event(r).map_err(|e| wrap("failed to map storage event", e)))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Arc::new(DatabaseSession::new(
            row.app_name,
            row.user_id,
            row.id,
            merge_states(Some(&app_state), Some(&user_state), &session_state),
            events,
            row.update_time,
        )))
    }

    async fn list(&self, req: ListRequest) -> Result<Vec<Arc<dyn Session>>, SessionError> {
        require(&[("app_name", &req.app_name)])?;
        let user_filter = (!req.user_id.is_empty()).then_some(req.user_id.as_str());
        let mut tx = self
            .backend
            .begin_read(self.log.clone())
            .await
            .map_err(errors::raw)?;
        let mut rows = tx
            .list_sessions(&req.app_name, user_filter)
            .await
            .map_err(|e| wrap("database error while fetching session", e))?;
        // Sort here rather than with ORDER BY: SQLite stores update_time as RFC 3339 text with
        // the value's own offset, so it would compare text, not instants, for rows written in
        // different zones. Same order as `gadk_session::compare_by_last_update` (Go #1919).
        rows.sort_by(|a, b| {
            a.update_time
                .cmp(&b.update_time)
                .then_with(|| a.user_id.cmp(&b.user_id))
                .then_with(|| a.id.cmp(&b.id))
        });
        let app_state = fetch_app_state(&mut tx, &req.app_name, false)
            .await
            .map_err(|e| wrap("error on list sessions", e))?;
        let user_states = match user_filter {
            Some(user_id) => {
                let state = fetch_user_state(&mut tx, &req.app_name, user_id, false)
                    .await
                    .map_err(|e| wrap("error on list sessions", e))?;
                HashMap::from([(user_id.to_string(), state)])
            }
            None => fetch_all_user_states(&mut tx, &req.app_name)
                .await
                .map_err(|e| wrap("error on list sessions", e))?,
        };
        tx.commit().await.map_err(errors::raw)?;

        let empty = StateMap::new();
        rows.into_iter()
            .map(|row| {
                let session_state = rows::decode_state(&row.state).map_err(|e| {
                    wrap(
                        &format!("failed to map storage object for session {}", row.id),
                        e,
                    )
                })?;
                let user_state = user_states.get(&row.user_id).unwrap_or(&empty);
                let merged = merge_states(Some(&app_state), Some(user_state), &session_state);
                Ok(Arc::new(DatabaseSession::new(
                    row.app_name,
                    row.user_id,
                    row.id,
                    merged,
                    Vec::new(),
                    row.update_time,
                )) as Arc<dyn Session>)
            })
            .collect()
    }

    async fn delete(&self, req: DeleteRequest) -> Result<(), SessionError> {
        require(&[
            ("app_name", &req.app_name),
            ("user_id", &req.user_id),
            ("session_id", &req.session_id),
        ])?;
        let mut tx = self
            .backend
            .begin_write(self.log.clone())
            .await
            .map_err(errors::raw)?;
        // Delete the events explicitly instead of relying on the ON DELETE CASCADE foreign key:
        // SQLite enforces foreign keys only when the connection enables them (Go #1774).
        tx.delete_session_events(&req.app_name, &req.user_id, &req.session_id)
            .await
            .map_err(|e| wrap("database error during session events deletion", e))?;
        tx.delete_session(&req.app_name, &req.user_id, &req.session_id)
            .await
            .map_err(|e| wrap("database error during session deletion", e))?;
        tx.commit().await.map_err(errors::raw)
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
        // Give the event an identity if it arrived without one, as the in-memory service does.
        if event.id.is_empty() {
            event.id = self.platform.new_uuid();
        }
        // The database keeps microseconds.
        event.timestamp = truncate_to_micros(event.timestamp);

        let sess = session
            .as_any()
            .downcast_ref::<DatabaseSession>()
            .ok_or_else(|| {
                errors::message(
                    "unexpected session type: the session was not created by DatabaseSessionService",
                )
            })?;
        // Go appends to the caller's session before persisting.
        sess.append_event(event);
        let trimmed = trim_temp_delta_state(event);
        self.apply_event(sess, &trimmed).await?;
        sess.set_updated_at(trimmed.timestamp);
        Ok(())
    }
}

impl DatabaseSessionService {
    /// Go `applyEvent`: one transaction that checks staleness, locks and updates the shared
    /// state rows the event changes (app before user), inserts the event and saves the session.
    async fn apply_event(&self, sess: &DatabaseSession, event: &Event) -> Result<(), SessionError> {
        let (app_name, user_id, id) = (sess.app_name(), sess.user_id(), sess.id());
        let mut tx = self
            .backend
            .begin_write(self.log.clone())
            .await
            .map_err(errors::raw)?;
        let mut stored = tx
            .select_session(app_name, user_id, id)
            .await
            .map_err(|e| wrap("failed to get session", e))?
            .ok_or(SessionError::NotFound)?;

        let storage_us = stored.update_time.timestamp_micros();
        let session_us = sess.updated_at().timestamp_micros();
        if storage_us > session_us {
            return Err(errors::message(format!(
                "stale session error: last update time from request ({}) is older than in database ({})",
                micros_rfc3339(session_us),
                micros_rfc3339(storage_us)
            )));
        }

        let delta = event.actions.state_delta.clone().unwrap_or_default();
        let (app_delta, user_delta, session_delta) = extract_state_deltas(&delta);

        // Lock and read the app row before touching the user row: Create writes app then user.
        if !app_delta.is_empty() {
            lock_app_state_row(&mut tx, app_name, event.timestamp).await?;
        }
        let mut app_state = fetch_app_state(&mut tx, app_name, !app_delta.is_empty()).await?;
        if !user_delta.is_empty() {
            lock_user_state_row(&mut tx, app_name, user_id, event.timestamp).await?;
        }
        let mut user_state =
            fetch_user_state(&mut tx, app_name, user_id, !user_delta.is_empty()).await?;

        let now = event.timestamp;
        let app = Scope::App { app_name };
        save_delta(&mut tx, app, &mut app_state, app_delta, now).await?;
        let user = Scope::User { app_name, user_id };
        save_delta(&mut tx, user, &mut user_state, user_delta, now).await?;

        let mut session_state =
            rows::decode_state(&stored.state).map_err(|e| wrap("failed to get session", e))?;
        session_state.extend(session_delta);

        let event_row = rows::event_to_row(app_name, user_id, id, event)
            .map_err(|e| wrap("failed to map event to storage model", e))?;
        tx.insert_event(&event_row)
            .await
            .map_err(|e| wrap("failed to save event", e))?;

        stored.state = encode(&session_state, "failed to save session state")?;
        stored.update_time = event.timestamp;
        tx.update_session(&stored)
            .await
            .map_err(|e| wrap("failed to save session state", e))?;
        tx.commit().await.map_err(errors::raw)?;
        sess.set_updated_at(stored.update_time);
        Ok(())
    }
}
