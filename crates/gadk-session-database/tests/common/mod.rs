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

//! Helpers shared by the integration tests (each test binary uses a subset).
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use gadk_session_database::DatabaseSessionService;
#[cfg(feature = "sqlite")]
use sqlx::{Row, SqlitePool};

/// A migrated service on a fresh in-memory sqlite database.
#[cfg(feature = "sqlite")]
pub async fn memory_service() -> DatabaseSessionService {
    let svc = DatabaseSessionService::connect("sqlite::memory:")
        .await
        .unwrap();
    svc.auto_migrate().await.unwrap();
    svc
}

/// The text of `tests/testdata/<name>`.
pub fn testdata(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/testdata")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Schema dump in the fixture format: `type\tname\ttbl_name\tsql` per `sqlite_master` row,
/// ordered by type then name, empty `sql` for automatic indexes.
#[cfg(feature = "sqlite")]
pub async fn dump_schema(pool: &SqlitePool) -> String {
    let rows = sqlx::query(
        "SELECT type, name, tbl_name, coalesce(sql, '') FROM sqlite_master ORDER BY type, name",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    rows.iter()
        .map(|r| {
            format!(
                "{}\t{}\t{}\t{}\n",
                r.get::<String, _>(0),
                r.get::<String, _>(1),
                r.get::<String, _>(2),
                r.get::<String, _>(3)
            )
        })
        .collect()
}

/// Rows dump in the fixture format: per table, ``INSERT INTO `<t>` VALUES(<quote()…>);`` in
/// primary-key order.
#[cfg(feature = "sqlite")]
pub async fn dump_rows(pool: &SqlitePool) -> String {
    let mut out = String::new();
    for t in ["sessions", "events", "app_states", "user_states"] {
        let cols: Vec<(i64, String, i64)> =
            sqlx::query_as("SELECT cid, name, pk FROM pragma_table_info(?) ORDER BY cid")
                .bind(t)
                .fetch_all(pool)
                .await
                .unwrap();
        let vals: Vec<String> = cols
            .iter()
            .map(|(_, n, _)| format!("quote(`{n}`)"))
            .collect();
        let mut pk: Vec<&(i64, String, i64)> = cols.iter().filter(|c| c.2 > 0).collect();
        pk.sort_by_key(|c| c.2);
        let pks: Vec<String> = pk.iter().map(|c| format!("`{}`", c.1)).collect();
        let q = format!(
            "SELECT {} FROM `{t}` ORDER BY {}",
            vals.join(" || ',' || "),
            pks.join(", ")
        );
        for row in sqlx::query(sqlx::AssertSqlSafe(q))
            .fetch_all(pool)
            .await
            .unwrap()
        {
            let line: String = row.get(0);
            out.push_str(&format!("INSERT INTO `{t}` VALUES({line});\n"));
        }
    }
    out
}

/// Executes each line of a rows dump.
#[cfg(feature = "sqlite")]
pub async fn load_rows(pool: &SqlitePool, dump: &str) {
    for line in dump.lines().filter(|l| !l.is_empty()) {
        sqlx::query(sqlx::AssertSqlSafe(line.to_string()))
            .execute(pool)
            .await
            .unwrap();
    }
}

/// A migrated service on a fresh PostgreSQL schema `gadk_test_<uuid>`; every pooled
/// connection's `search_path` is that schema.
#[cfg(feature = "postgres")]
pub async fn pg_service(url: &str) -> (DatabaseSessionService, String) {
    let schema = format!(
        "gadk_test_{}",
        gadk_platform::Platform::default()
            .new_uuid()
            .replace('-', "_")
    );
    let admin = sqlx::PgPool::connect(url).await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    let path = schema.clone();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .after_connect(move |conn, _| {
            let sql = format!("SET search_path TO {path}");
            Box::pin(async move {
                sqlx::query(sqlx::AssertSqlSafe(sql))
                    .execute(&mut *conn)
                    .await?;
                Ok(())
            })
        })
        .connect(url)
        .await
        .unwrap();
    let svc = DatabaseSessionService::from_pg_pool(pool);
    svc.auto_migrate().await.unwrap();
    (svc, schema)
}

/// Drops a schema made by [`pg_service`].
#[cfg(feature = "postgres")]
pub async fn drop_pg_schema(url: &str, schema: &str) {
    let admin = sqlx::PgPool::connect(url).await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP SCHEMA IF EXISTS {schema} CASCADE"
    )))
    .execute(&admin)
    .await
    .unwrap();
    admin.close().await;
}
