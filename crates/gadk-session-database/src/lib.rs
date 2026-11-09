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

//! A [`SessionService`](gadk_session::SessionService) backed by a relational database (port of
//! ADK-Go `session/database`): sqlite (feature `sqlite`, default) and PostgreSQL (feature
//! `postgres`), through sqlx.
//!
//! The tables, their columns and the rows written are the ones ADK-Go's gorm models produce, so
//! a gadk service and an ADK-Go service can share one database.
//!
//! The service never creates or alters its tables on its own. Call
//! [`DatabaseSessionService::auto_migrate`] after constructing it, on every startup: a release
//! of this crate may add columns, and writes fail until they exist.
//!
//! As in ADK-Go, a sqlite connection opened by [`DatabaseSessionService::connect`] does not
//! enforce foreign keys, so deleting a session leaves its event rows in the `events` table;
//! on PostgreSQL the foreign key's `ON DELETE CASCADE` removes them.
//!
//! ```no_run
//! # async fn demo() -> Result<(), gadk_session::SessionError> {
//! use gadk_session_database::DatabaseSessionService;
//! let svc = DatabaseSessionService::connect("sqlite://sessions.db").await?;
//! svc.auto_migrate().await?;
//! # Ok(()) }
//! ```
#![allow(dead_code)]

#[cfg(not(any(feature = "sqlite", feature = "postgres")))]
compile_error!("gadk-session-database needs feature `sqlite` or `postgres`");

mod backend;
mod errors;
mod migrate;
#[cfg(feature = "postgres")]
mod postgres;
mod rows;
mod service;
mod session;
#[cfg(feature = "sqlite")]
mod sqlite;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod parity_tests;

/// The sqlx version this crate is built on, for building pools to pass to
/// [`DatabaseSessionService::from_sqlite_pool`] / [`DatabaseSessionService::from_pg_pool`].
pub use sqlx;

use gadk_platform::{Platform, go_quote};
use gadk_session::SessionError;

use crate::backend::{Backend, StatementLog};

/// A [`SessionService`](gadk_session::SessionService) storing sessions, events and app/user
/// state in a relational database (ADK-Go `database.NewSessionService`).
pub struct DatabaseSessionService {
    pub(crate) backend: Backend,
    pub(crate) platform: Platform,
    pub(crate) log: StatementLog,
}

impl DatabaseSessionService {
    /// Opens a pool for `url` (Go `NewSessionService` with a dialector): `sqlite:` URLs (feature
    /// `sqlite`; the file is created when missing; `sqlite::memory:` keeps its single connection
    /// open for the pool's lifetime) and `postgres:` / `postgresql:` URLs (feature `postgres`).
    /// Creates no tables: see [`Self::auto_migrate`].
    pub async fn connect(url: &str) -> Result<Self, SessionError> {
        let scheme = url.split_once(':').map_or("", |(s, _)| s);
        let backend = match scheme {
            "sqlite" => sqlite_backend(url).await?,
            "postgres" | "postgresql" => postgres_backend(url).await?,
            _ => {
                return Err(SessionError::InvalidRequest(format!(
                    "unsupported database URL scheme {}: want sqlite, postgres or postgresql",
                    go_quote(scheme)
                )));
            }
        };
        Ok(Self::from_backend(backend))
    }

    /// Wraps an existing sqlite pool, shared with the caller (Go `NewSessionServiceFromDB`).
    /// The pool is used as configured.
    #[cfg(feature = "sqlite")]
    pub fn from_sqlite_pool(pool: sqlx::SqlitePool) -> Self {
        Self::from_backend(Backend::Sqlite(pool))
    }

    /// Wraps an existing PostgreSQL pool, shared with the caller (Go `NewSessionServiceFromDB`).
    #[cfg(feature = "postgres")]
    pub fn from_pg_pool(pool: sqlx::PgPool) -> Self {
        Self::from_backend(Backend::Postgres(pool))
    }

    /// Takes time and session/event IDs from `platform` (default [`Platform::default`]), as
    /// `InMemorySessionService::with_platform` does.
    pub fn with_platform(mut self, platform: Platform) -> Self {
        self.platform = platform;
        self
    }

    /// Creates any missing table and adds any missing column (Go `AutoMigrate`, restricted to
    /// additions: existing columns are never altered or dropped). Idempotent; meant to run on
    /// every startup. Errors are `auto migrate failed: <cause>`.
    pub async fn auto_migrate(&self) -> Result<(), SessionError> {
        migrate::auto_migrate(&self.backend).await
    }

    /// The sqlite pool, when this service runs on sqlite.
    #[cfg(feature = "sqlite")]
    pub fn sqlite_pool(&self) -> Option<&sqlx::SqlitePool> {
        match &self.backend {
            Backend::Sqlite(p) => Some(p),
            #[allow(unreachable_patterns)]
            _ => None,
        }
    }

    /// The PostgreSQL pool, when this service runs on PostgreSQL.
    #[cfg(feature = "postgres")]
    pub fn pg_pool(&self) -> Option<&sqlx::PgPool> {
        match &self.backend {
            Backend::Postgres(p) => Some(p),
            #[allow(unreachable_patterns)]
            _ => None,
        }
    }

    /// Test hook: records the app/user state statements the service issues (see D5 of the 4a
    /// plan) into the returned log.
    #[cfg(feature = "testutil")]
    #[doc(hidden)]
    pub fn with_statement_log(mut self) -> (Self, std::sync::Arc<parking_lot::Mutex<Vec<String>>>) {
        let log = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
        self.log = Some(log.clone());
        (self, log)
    }

    fn from_backend(backend: Backend) -> Self {
        Self {
            backend,
            platform: Platform::default(),
            log: None,
        }
    }
}

impl std::fmt::Debug for DatabaseSessionService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DatabaseSessionService")
            .field("backend", &self.backend.name())
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "sqlite")]
async fn sqlite_backend(url: &str) -> Result<Backend, SessionError> {
    Ok(Backend::Sqlite(
        sqlite::connect(url).await.map_err(errors::raw)?,
    ))
}

#[cfg(not(feature = "sqlite"))]
async fn sqlite_backend(_url: &str) -> Result<Backend, SessionError> {
    Err(missing_feature("sqlite", "sqlite"))
}

#[cfg(feature = "postgres")]
async fn postgres_backend(url: &str) -> Result<Backend, SessionError> {
    Ok(Backend::Postgres(
        postgres::connect(url).await.map_err(errors::raw)?,
    ))
}

#[cfg(not(feature = "postgres"))]
async fn postgres_backend(url: &str) -> Result<Backend, SessionError> {
    let scheme = url.split_once(':').map_or("", |(s, _)| s);
    Err(missing_feature(scheme, "postgres"))
}

#[allow(dead_code)] // unused when both features are on
fn missing_feature(scheme: &str, feature: &str) -> SessionError {
    SessionError::InvalidRequest(format!(
        "database URL scheme {} needs the gadk-session-database feature {}",
        go_quote(scheme),
        go_quote(feature)
    ))
}
