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

//! The dialect-neutral layer the service talks to: one transaction type whose operations
//! dispatch to `sqlite.rs` or `postgres.rs` (identical function names and signatures) and
//! record the state-table statements of plan D5.
#![allow(dead_code)]

use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::migrate::TableDef;
use crate::rows::{AppStateRow, EventRow, SessionRow, UserStateRow};

/// Where `Tx` records state-table statements (tests only; `None` records nothing).
pub(crate) type StatementLog = Option<Arc<parking_lot::Mutex<Vec<String>>>>;

/// The pool behind a [`crate::DatabaseSessionService`].
pub(crate) enum Backend {
    #[cfg(feature = "sqlite")]
    Sqlite(sqlx::SqlitePool),
    #[cfg(feature = "postgres")]
    Postgres(sqlx::PgPool),
}

enum TxInner {
    #[cfg(feature = "sqlite")]
    Sqlite(sqlx::Transaction<'static, sqlx::Sqlite>),
    #[cfg(feature = "postgres")]
    Postgres(sqlx::Transaction<'static, sqlx::Postgres>),
}

/// An open transaction. Dropping it without [`Tx::commit`] rolls back (Go's `Transaction`
/// callback returning an error).
pub(crate) struct Tx {
    inner: TxInner,
    log: StatementLog,
}

/// Calls the same-named function of the transaction's dialect module.
macro_rules! dispatch {
    ($inner:expr, $f:ident ( $($arg:expr),* )) => {
        match $inner {
            #[cfg(feature = "sqlite")]
            TxInner::Sqlite(tx) => crate::sqlite::$f(&mut **tx, $($arg),*).await,
            #[cfg(feature = "postgres")]
            TxInner::Postgres(tx) => crate::postgres::$f(&mut **tx, $($arg),*).await,
        }
    };
}

impl Backend {
    /// `"sqlite"` or `"postgres"`.
    pub(crate) fn name(&self) -> &'static str {
        match self {
            #[cfg(feature = "sqlite")]
            Backend::Sqlite(_) => "sqlite",
            #[cfg(feature = "postgres")]
            Backend::Postgres(_) => "postgres",
        }
    }

    /// A transaction for reads (`get`, `list`): sqlite `BEGIN` (deferred), Postgres `BEGIN`.
    pub(crate) async fn begin_read(&self, log: StatementLog) -> sqlx::Result<Tx> {
        let inner = match self {
            #[cfg(feature = "sqlite")]
            Backend::Sqlite(p) => TxInner::Sqlite(p.begin().await?),
            #[cfg(feature = "postgres")]
            Backend::Postgres(p) => TxInner::Postgres(p.begin().await?),
        };
        Ok(Tx { inner, log })
    }

    /// A transaction for writes: sqlite `BEGIN IMMEDIATE` (takes the write lock up front, so
    /// concurrent writers wait for the busy timeout instead of failing mid-transaction),
    /// Postgres `BEGIN` (rows are locked `FOR UPDATE` where Go locks them).
    pub(crate) async fn begin_write(&self, log: StatementLog) -> sqlx::Result<Tx> {
        let inner = match self {
            #[cfg(feature = "sqlite")]
            Backend::Sqlite(p) => TxInner::Sqlite(p.begin_with("BEGIN IMMEDIATE").await?),
            #[cfg(feature = "postgres")]
            Backend::Postgres(p) => TxInner::Postgres(p.begin().await?),
        };
        Ok(Tx { inner, log })
    }

    /// The four tables in Go's `AutoMigrate` order (sessions, events, app_states, user_states).
    pub(crate) fn tables(&self) -> &'static [TableDef] {
        match self {
            #[cfg(feature = "sqlite")]
            Backend::Sqlite(_) => &crate::sqlite::TABLES,
            #[cfg(feature = "postgres")]
            Backend::Postgres(_) => &crate::postgres::TABLES,
        }
    }

    /// The dialect's `ALTER TABLE … ADD …` for one missing column.
    pub(crate) fn add_column_sql(&self, table: &str, column: &str, ty: &str) -> String {
        match self {
            #[cfg(feature = "sqlite")]
            Backend::Sqlite(_) => crate::sqlite::add_column_sql(table, column, ty),
            #[cfg(feature = "postgres")]
            Backend::Postgres(_) => crate::postgres::add_column_sql(table, column, ty),
        }
    }
}

impl Tx {
    pub(crate) async fn commit(self) -> sqlx::Result<()> {
        match self.inner {
            #[cfg(feature = "sqlite")]
            TxInner::Sqlite(tx) => tx.commit().await,
            #[cfg(feature = "postgres")]
            TxInner::Postgres(tx) => tx.commit().await,
        }
    }

    fn record(&self, stmt: String) {
        if let Some(log) = &self.log {
            log.lock().push(stmt);
        }
    }

    pub(crate) async fn select_session(
        &mut self,
        app_name: &str,
        user_id: &str,
        id: &str,
    ) -> sqlx::Result<Option<SessionRow>> {
        dispatch!(&mut self.inner, select_session(app_name, user_id, id))
    }

    pub(crate) async fn list_sessions(
        &mut self,
        app_name: &str,
        user_id: Option<&str>,
    ) -> sqlx::Result<Vec<SessionRow>> {
        dispatch!(&mut self.inner, list_sessions(app_name, user_id))
    }

    pub(crate) async fn insert_session(&mut self, row: &SessionRow) -> sqlx::Result<()> {
        dispatch!(&mut self.inner, insert_session(row))
    }

    pub(crate) async fn update_session(&mut self, row: &SessionRow) -> sqlx::Result<()> {
        dispatch!(&mut self.inner, update_session(row))
    }

    pub(crate) async fn delete_session(
        &mut self,
        app_name: &str,
        user_id: &str,
        id: &str,
    ) -> sqlx::Result<()> {
        dispatch!(&mut self.inner, delete_session(app_name, user_id, id))
    }

    /// Records `select app_states` (`… for update` when `for_update`).
    pub(crate) async fn select_app_state(
        &mut self,
        app_name: &str,
        for_update: bool,
    ) -> sqlx::Result<Option<AppStateRow>> {
        self.record(select_stmt("app_states", for_update));
        dispatch!(&mut self.inner, select_app_state(app_name, for_update))
    }

    /// Records `select user_states` (`… for update` when `for_update`).
    pub(crate) async fn select_user_state(
        &mut self,
        app_name: &str,
        user_id: &str,
        for_update: bool,
    ) -> sqlx::Result<Option<UserStateRow>> {
        self.record(select_stmt("user_states", for_update));
        dispatch!(
            &mut self.inner,
            select_user_state(app_name, user_id, for_update)
        )
    }

    pub(crate) async fn select_user_states(
        &mut self,
        app_name: &str,
    ) -> sqlx::Result<Vec<UserStateRow>> {
        dispatch!(&mut self.inner, select_user_states(app_name))
    }

    /// Records `insert app_states on conflict do nothing`.
    pub(crate) async fn insert_app_state_if_absent(
        &mut self,
        row: &AppStateRow,
    ) -> sqlx::Result<()> {
        self.record("insert app_states on conflict do nothing".to_string());
        dispatch!(&mut self.inner, insert_app_state_if_absent(row))
    }

    /// Records `insert user_states on conflict do nothing`.
    pub(crate) async fn insert_user_state_if_absent(
        &mut self,
        row: &UserStateRow,
    ) -> sqlx::Result<()> {
        self.record("insert user_states on conflict do nothing".to_string());
        dispatch!(&mut self.inner, insert_user_state_if_absent(row))
    }

    pub(crate) async fn save_app_state(&mut self, row: &AppStateRow) -> sqlx::Result<()> {
        dispatch!(&mut self.inner, save_app_state(row))
    }

    pub(crate) async fn save_user_state(&mut self, row: &UserStateRow) -> sqlx::Result<()> {
        dispatch!(&mut self.inner, save_user_state(row))
    }

    pub(crate) async fn insert_event(&mut self, row: &EventRow) -> sqlx::Result<()> {
        dispatch!(&mut self.inner, insert_event(row))
    }

    /// Events of one session, `ORDER BY timestamp DESC, id DESC`, `timestamp >= after` when
    /// set, at most `limit` rows when set.
    pub(crate) async fn select_events(
        &mut self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
        after: Option<DateTime<Utc>>,
        limit: Option<i64>,
    ) -> sqlx::Result<Vec<EventRow>> {
        dispatch!(
            &mut self.inner,
            select_events(app_name, user_id, session_id, after, limit)
        )
    }

    /// Column names of `table` in the current schema; empty when the table does not exist.
    pub(crate) async fn existing_columns(&mut self, table: &str) -> sqlx::Result<Vec<String>> {
        dispatch!(&mut self.inner, existing_columns(table))
    }

    /// Runs one DDL statement.
    pub(crate) async fn execute(&mut self, sql: &str) -> sqlx::Result<()> {
        dispatch!(&mut self.inner, execute(sql))
    }
}

fn select_stmt(table: &str, for_update: bool) -> String {
    if for_update {
        format!("select {table} for update")
    } else {
        format!("select {table}")
    }
}
