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

#![cfg(feature = "sqlite")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `DatabaseSessionService::connect` on sqlite.

mod common;

use gadk_session::SessionError;
use gadk_session_database::DatabaseSessionService;

#[tokio::test]
async fn memory_pool_keeps_its_one_connection() {
    let svc = DatabaseSessionService::connect("sqlite::memory:")
        .await
        .unwrap();
    let opts = svc.sqlite_pool().unwrap().options();
    assert_eq!(opts.get_max_connections(), 1);
    assert_eq!(opts.get_min_connections(), 1);
    assert_eq!(opts.get_idle_timeout(), None);
    assert_eq!(opts.get_max_lifetime(), None);
}

#[tokio::test]
async fn connect_creates_no_tables() {
    let svc = DatabaseSessionService::connect("sqlite::memory:")
        .await
        .unwrap();
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM sqlite_master WHERE type = 'table'")
        .fetch_one(svc.sqlite_pool().unwrap())
        .await
        .unwrap();
    assert_eq!(n, 0);
}

#[tokio::test]
async fn connect_leaves_foreign_keys_off_like_go() {
    let svc = DatabaseSessionService::connect("sqlite::memory:")
        .await
        .unwrap();
    let fk: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(svc.sqlite_pool().unwrap())
        .await
        .unwrap();
    assert_eq!(fk, 0);
}

#[tokio::test]
async fn connect_creates_a_missing_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.db");
    let svc = DatabaseSessionService::connect(&format!("sqlite://{}", path.display()))
        .await
        .unwrap();
    svc.auto_migrate().await.unwrap();
    assert!(path.exists());
}

#[tokio::test]
async fn connect_rejects_an_unknown_scheme() {
    let err = DatabaseSessionService::connect("mysql://localhost/x")
        .await
        .unwrap_err();
    let SessionError::InvalidRequest(msg) = err else {
        panic!("want InvalidRequest, got {err}")
    };
    assert_eq!(
        msg,
        "unsupported database URL scheme \"mysql\": want sqlite, postgres or postgresql"
    );
}

#[cfg(not(feature = "postgres"))]
#[tokio::test]
async fn connect_postgres_needs_the_feature() {
    let err = DatabaseSessionService::connect("postgres://localhost/x")
        .await
        .unwrap_err();
    let SessionError::InvalidRequest(msg) = err else {
        panic!("want InvalidRequest, got {err}")
    };
    assert_eq!(
        msg,
        "database URL scheme \"postgres\" needs the gadk-session-database feature \"postgres\""
    );
}
