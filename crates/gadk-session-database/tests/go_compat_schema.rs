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
//! `auto_migrate` against ADK-Go's sqlite schema.

mod common;

use common::{dump_rows, dump_schema, load_rows, memory_service, testdata};
use gadk_session_database::DatabaseSessionService;
use pretty_assertions::assert_eq;

/// The `CREATE TABLE` statements of the Go fixture, in fixture order.
fn go_tables() -> Vec<String> {
    testdata("schema.sqlite.txt")
        .lines()
        .filter(|l| l.starts_with("table\t"))
        .map(|l| l.splitn(4, '\t').nth(3).unwrap().to_string())
        .collect()
}

async fn bare_memory() -> DatabaseSessionService {
    DatabaseSessionService::connect("sqlite::memory:")
        .await
        .unwrap()
}

#[tokio::test]
async fn sqlite_schema_matches_go() {
    let svc = memory_service().await;
    assert_eq!(
        dump_schema(svc.sqlite_pool().unwrap()).await,
        testdata("schema.sqlite.txt")
    );
}

#[tokio::test]
async fn auto_migrate_is_idempotent() {
    let svc = memory_service().await;
    svc.auto_migrate().await.unwrap();
    svc.auto_migrate().await.unwrap();
    assert_eq!(
        dump_schema(svc.sqlite_pool().unwrap()).await,
        testdata("schema.sqlite.txt")
    );
}

#[tokio::test]
async fn auto_migrate_noop_on_go_schema() {
    let svc = bare_memory().await;
    let pool = svc.sqlite_pool().unwrap();
    for create in go_tables() {
        sqlx::query(sqlx::AssertSqlSafe(create))
            .execute(pool)
            .await
            .unwrap();
    }
    svc.auto_migrate().await.unwrap();
    assert_eq!(dump_schema(pool).await, testdata("schema.sqlite.txt"));
}

#[tokio::test]
async fn go_rows_dump_round_trips() {
    let svc = memory_service().await;
    let pool = svc.sqlite_pool().unwrap();
    let go_rows = testdata("rows.sqlite.sql");
    load_rows(pool, &go_rows).await;
    assert_eq!(dump_rows(pool).await, go_rows);
}

#[tokio::test]
async fn auto_migrate_adds_missing_column() {
    let svc = bare_memory().await;
    let pool = svc.sqlite_pool().unwrap();
    for create in go_tables() {
        let create = create.replace(",`interrupted` numeric", "");
        sqlx::query(sqlx::AssertSqlSafe(create))
            .execute(pool)
            .await
            .unwrap();
    }
    sqlx::query("INSERT INTO `sessions` VALUES ('app','u','s','{}','2026-01-01 00:00:00+00:00','2026-01-01 00:00:00+00:00')")
        .execute(pool)
        .await
        .unwrap();
    svc.auto_migrate().await.unwrap();
    let cols: Vec<(String, String)> =
        sqlx::query_as("SELECT name, type FROM pragma_table_info('events') ORDER BY cid")
            .fetch_all(pool)
            .await
            .unwrap();
    assert_eq!(
        cols.last().unwrap(),
        &("interrupted".to_string(), "numeric".to_string())
    );
    let kept: i64 = sqlx::query_scalar("SELECT count(*) FROM `sessions`")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(kept, 1);
}
