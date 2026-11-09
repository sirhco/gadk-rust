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

//! The PostgreSQL dialect (gorm postgres): DDL with `JSONB` and `timestamptz(6)` columns, SQL
//! text, and `SELECT … FOR UPDATE` row locking.

use chrono::{DateTime, Utc};
use sqlx::postgres::{PgPoolOptions, PgRow};
use sqlx::{PgConnection as Conn, PgPool, Row};

use crate::migrate::TableDef;
use crate::rows::{
    AppStateRow, ColumnReader, EventRow, SessionRow, UserStateRow, app_state_row, event_row,
    session_row, user_state_row,
};

/// gorm postgres DDL for ADK-Go's models: `text` strings, `JSONB` for `stateMap`/`dynamicJSON`,
/// `bytea` for `[]byte`, `timestamptz(6)` for `precision:6` times, `boolean` for `*bool`.
pub(crate) static TABLES: [TableDef; 4] = [
    TableDef {
        name: "sessions",
        create: "CREATE TABLE \"sessions\" (\"app_name\" text,\"user_id\" text,\"id\" text,\"state\" JSONB,\"create_time\" timestamptz(6),\"update_time\" timestamptz(6),PRIMARY KEY (\"app_name\",\"user_id\",\"id\"))",
        columns: &[
            ("app_name", "text"),
            ("user_id", "text"),
            ("id", "text"),
            ("state", "JSONB"),
            ("create_time", "timestamptz(6)"),
            ("update_time", "timestamptz(6)"),
        ],
    },
    TableDef {
        name: "events",
        create: "CREATE TABLE \"events\" (\"id\" text,\"app_name\" text,\"user_id\" text,\"session_id\" text,\"invocation_id\" text,\"author\" text,\"actions\" bytea,\"long_running_tool_ids_json\" JSONB,\"routes_json\" JSONB,\"output_json\" JSONB,\"node_info_json\" JSONB,\"requested_input_json\" JSONB,\"branch\" text,\"isolation_scope\" text,\"timestamp\" timestamptz(6),\"content\" JSONB,\"grounding_metadata\" JSONB,\"custom_metadata\" JSONB,\"usage_metadata\" JSONB,\"citation_metadata\" JSONB,\"input_transcription\" JSONB,\"output_transcription\" JSONB,\"partial\" boolean,\"turn_complete\" boolean,\"error_code\" text,\"error_message\" text,\"interrupted\" boolean,PRIMARY KEY (\"id\",\"app_name\",\"user_id\",\"session_id\"),CONSTRAINT \"fk_sessions_events\" FOREIGN KEY (\"app_name\",\"user_id\",\"session_id\") REFERENCES \"sessions\"(\"app_name\",\"user_id\",\"id\") ON DELETE CASCADE)",
        columns: &[
            ("id", "text"),
            ("app_name", "text"),
            ("user_id", "text"),
            ("session_id", "text"),
            ("invocation_id", "text"),
            ("author", "text"),
            ("actions", "bytea"),
            ("long_running_tool_ids_json", "JSONB"),
            ("routes_json", "JSONB"),
            ("output_json", "JSONB"),
            ("node_info_json", "JSONB"),
            ("requested_input_json", "JSONB"),
            ("branch", "text"),
            ("isolation_scope", "text"),
            ("timestamp", "timestamptz(6)"),
            ("content", "JSONB"),
            ("grounding_metadata", "JSONB"),
            ("custom_metadata", "JSONB"),
            ("usage_metadata", "JSONB"),
            ("citation_metadata", "JSONB"),
            ("input_transcription", "JSONB"),
            ("output_transcription", "JSONB"),
            ("partial", "boolean"),
            ("turn_complete", "boolean"),
            ("error_code", "text"),
            ("error_message", "text"),
            ("interrupted", "boolean"),
        ],
    },
    TableDef {
        name: "app_states",
        create: "CREATE TABLE \"app_states\" (\"app_name\" text,\"state\" JSONB,\"update_time\" timestamptz(6),PRIMARY KEY (\"app_name\"))",
        columns: &[
            ("app_name", "text"),
            ("state", "JSONB"),
            ("update_time", "timestamptz(6)"),
        ],
    },
    TableDef {
        name: "user_states",
        create: "CREATE TABLE \"user_states\" (\"app_name\" text,\"user_id\" text,\"state\" JSONB,\"update_time\" timestamptz(6),PRIMARY KEY (\"app_name\",\"user_id\"))",
        columns: &[
            ("app_name", "text"),
            ("user_id", "text"),
            ("state", "JSONB"),
            ("update_time", "timestamptz(6)"),
        ],
    },
];

/// Reads JSONB columns back as text.
const EVENT_SELECT: &str = "SELECT \"id\", \"app_name\", \"user_id\", \"session_id\", \"invocation_id\", \"author\", \"actions\", \"long_running_tool_ids_json\"::text AS long_running_tool_ids_json, \"routes_json\"::text AS routes_json, \"output_json\"::text AS output_json, \"node_info_json\"::text AS node_info_json, \"requested_input_json\"::text AS requested_input_json, \"branch\", \"isolation_scope\", \"timestamp\", \"content\"::text AS content, \"grounding_metadata\"::text AS grounding_metadata, \"custom_metadata\"::text AS custom_metadata, \"usage_metadata\"::text AS usage_metadata, \"citation_metadata\"::text AS citation_metadata, \"input_transcription\"::text AS input_transcription, \"output_transcription\"::text AS output_transcription, \"partial\", \"turn_complete\", \"error_code\", \"error_message\", \"interrupted\" FROM \"events\"";
const SESSION_SELECT: &str = "SELECT \"app_name\", \"user_id\", \"id\", \"state\"::text AS state, \"create_time\", \"update_time\" FROM \"sessions\"";

pub(crate) async fn connect(url: &str) -> sqlx::Result<PgPool> {
    PgPoolOptions::new().connect(url).await
}

/// gorm's postgres `AddColumn`.
pub(crate) fn add_column_sql(table: &str, column: &str, ty: &str) -> String {
    format!("ALTER TABLE \"{table}\" ADD \"{column}\" {ty}")
}

impl ColumnReader for PgRow {
    fn text(&self, col: &str) -> sqlx::Result<String> {
        self.try_get(col)
    }

    fn opt_text(&self, col: &str) -> sqlx::Result<Option<String>> {
        self.try_get(col)
    }

    fn opt_bytes(&self, col: &str) -> sqlx::Result<Option<Vec<u8>>> {
        self.try_get(col)
    }

    /// A `timestamptz` column; NULL is Go's zero time.
    fn time(&self, col: &str) -> sqlx::Result<DateTime<Utc>> {
        Ok(self
            .try_get::<Option<DateTime<Utc>>, _>(col)?
            .unwrap_or_else(gadk_session::zero_time))
    }

    fn flag(&self, col: &str) -> sqlx::Result<Option<bool>> {
        self.try_get(col)
    }
}

fn locking(for_update: bool) -> &'static str {
    if for_update { " FOR UPDATE" } else { "" }
}

pub(crate) async fn select_session(
    c: &mut Conn,
    app_name: &str,
    user_id: &str,
    id: &str,
) -> sqlx::Result<Option<SessionRow>> {
    let sql = format!(
        "{SESSION_SELECT} WHERE \"app_name\" = $1 AND \"user_id\" = $2 AND \"id\" = $3 LIMIT 1"
    );
    sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(app_name)
        .bind(user_id)
        .bind(id)
        .fetch_optional(&mut *c)
        .await?
        .as_ref()
        .map(session_row)
        .transpose()
}

pub(crate) async fn list_sessions(
    c: &mut Conn,
    app_name: &str,
    user_id: Option<&str>,
) -> sqlx::Result<Vec<SessionRow>> {
    let rows = match user_id {
        Some(user_id) => {
            let sql = format!("{SESSION_SELECT} WHERE \"app_name\" = $1 AND \"user_id\" = $2");
            sqlx::query(sqlx::AssertSqlSafe(sql))
                .bind(app_name)
                .bind(user_id)
                .fetch_all(&mut *c)
                .await?
        }
        None => {
            let sql = format!("{SESSION_SELECT} WHERE \"app_name\" = $1");
            sqlx::query(sqlx::AssertSqlSafe(sql))
                .bind(app_name)
                .fetch_all(&mut *c)
                .await?
        }
    };
    rows.iter().map(session_row).collect()
}

pub(crate) async fn insert_session(c: &mut Conn, row: &SessionRow) -> sqlx::Result<()> {
    sqlx::query("INSERT INTO \"sessions\" (\"app_name\", \"user_id\", \"id\", \"state\", \"create_time\", \"update_time\") VALUES ($1, $2, $3, $4::jsonb, $5, $6)")
        .bind(&row.app_name).bind(&row.user_id).bind(&row.id).bind(&row.state)
        .bind(row.create_time).bind(row.update_time)
        .execute(&mut *c).await.map(|_| ())
}

pub(crate) async fn update_session(c: &mut Conn, row: &SessionRow) -> sqlx::Result<()> {
    sqlx::query("UPDATE \"sessions\" SET \"state\" = $1::jsonb, \"create_time\" = $2, \"update_time\" = $3 WHERE \"app_name\" = $4 AND \"user_id\" = $5 AND \"id\" = $6")
        .bind(&row.state).bind(row.create_time).bind(row.update_time)
        .bind(&row.app_name).bind(&row.user_id).bind(&row.id)
        .execute(&mut *c).await.map(|_| ())
}

pub(crate) async fn delete_session(
    c: &mut Conn,
    app_name: &str,
    user_id: &str,
    id: &str,
) -> sqlx::Result<()> {
    sqlx::query(
        "DELETE FROM \"sessions\" WHERE \"app_name\" = $1 AND \"user_id\" = $2 AND \"id\" = $3",
    )
    .bind(app_name)
    .bind(user_id)
    .bind(id)
    .execute(&mut *c)
    .await
    .map(|_| ())
}

pub(crate) async fn select_app_state(
    c: &mut Conn,
    app_name: &str,
    for_update: bool,
) -> sqlx::Result<Option<AppStateRow>> {
    let sql = format!(
        "SELECT \"app_name\", \"state\"::text AS state, \"update_time\" FROM \"app_states\" WHERE \"app_name\" = $1 ORDER BY \"app_name\" LIMIT 1{}",
        locking(for_update)
    );
    sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(app_name)
        .fetch_optional(&mut *c)
        .await?
        .as_ref()
        .map(app_state_row)
        .transpose()
}

pub(crate) async fn select_user_state(
    c: &mut Conn,
    app_name: &str,
    user_id: &str,
    for_update: bool,
) -> sqlx::Result<Option<UserStateRow>> {
    let sql = format!(
        "SELECT \"app_name\", \"user_id\", \"state\"::text AS state, \"update_time\" FROM \"user_states\" WHERE \"app_name\" = $1 AND \"user_id\" = $2 ORDER BY \"app_name\" LIMIT 1{}",
        locking(for_update)
    );
    sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(app_name)
        .bind(user_id)
        .fetch_optional(&mut *c)
        .await?
        .as_ref()
        .map(user_state_row)
        .transpose()
}

pub(crate) async fn select_user_states(
    c: &mut Conn,
    app_name: &str,
) -> sqlx::Result<Vec<UserStateRow>> {
    sqlx::query("SELECT \"app_name\", \"user_id\", \"state\"::text AS state, \"update_time\" FROM \"user_states\" WHERE \"app_name\" = $1")
        .bind(app_name).fetch_all(&mut *c).await?.iter().map(user_state_row).collect()
}

pub(crate) async fn insert_app_state_if_absent(
    c: &mut Conn,
    row: &AppStateRow,
) -> sqlx::Result<()> {
    sqlx::query("INSERT INTO \"app_states\" (\"app_name\", \"state\", \"update_time\") VALUES ($1, $2::jsonb, $3) ON CONFLICT DO NOTHING")
        .bind(&row.app_name).bind(&row.state).bind(row.update_time)
        .execute(&mut *c).await.map(|_| ())
}

pub(crate) async fn insert_user_state_if_absent(
    c: &mut Conn,
    row: &UserStateRow,
) -> sqlx::Result<()> {
    sqlx::query("INSERT INTO \"user_states\" (\"app_name\", \"user_id\", \"state\", \"update_time\") VALUES ($1, $2, $3::jsonb, $4) ON CONFLICT DO NOTHING")
        .bind(&row.app_name).bind(&row.user_id).bind(&row.state).bind(row.update_time)
        .execute(&mut *c).await.map(|_| ())
}

pub(crate) async fn save_app_state(c: &mut Conn, row: &AppStateRow) -> sqlx::Result<()> {
    sqlx::query("INSERT INTO \"app_states\" (\"app_name\", \"state\", \"update_time\") VALUES ($1, $2::jsonb, $3) ON CONFLICT (\"app_name\") DO UPDATE SET \"state\" = excluded.\"state\", \"update_time\" = excluded.\"update_time\"")
        .bind(&row.app_name).bind(&row.state).bind(row.update_time)
        .execute(&mut *c).await.map(|_| ())
}

pub(crate) async fn save_user_state(c: &mut Conn, row: &UserStateRow) -> sqlx::Result<()> {
    sqlx::query("INSERT INTO \"user_states\" (\"app_name\", \"user_id\", \"state\", \"update_time\") VALUES ($1, $2, $3::jsonb, $4) ON CONFLICT (\"app_name\", \"user_id\") DO UPDATE SET \"state\" = excluded.\"state\", \"update_time\" = excluded.\"update_time\"")
        .bind(&row.app_name).bind(&row.user_id).bind(&row.state).bind(row.update_time)
        .execute(&mut *c).await.map(|_| ())
}

pub(crate) async fn insert_event(c: &mut Conn, row: &EventRow) -> sqlx::Result<()> {
    sqlx::query("INSERT INTO \"events\" (\"id\", \"app_name\", \"user_id\", \"session_id\", \"invocation_id\", \"author\", \"actions\", \"long_running_tool_ids_json\", \"routes_json\", \"output_json\", \"node_info_json\", \"requested_input_json\", \"branch\", \"isolation_scope\", \"timestamp\", \"content\", \"grounding_metadata\", \"custom_metadata\", \"usage_metadata\", \"citation_metadata\", \"input_transcription\", \"output_transcription\", \"partial\", \"turn_complete\", \"error_code\", \"error_message\", \"interrupted\") VALUES ($1, $2, $3, $4, $5, $6, $7, $8::jsonb, $9::jsonb, $10::jsonb, $11::jsonb, $12::jsonb, $13, $14, $15, $16::jsonb, $17::jsonb, $18::jsonb, $19::jsonb, $20::jsonb, $21::jsonb, $22::jsonb, $23, $24, $25, $26, $27)")
        .bind(&row.id).bind(&row.app_name).bind(&row.user_id).bind(&row.session_id)
        .bind(&row.invocation_id).bind(&row.author).bind(&row.actions)
        .bind(&row.long_running_tool_ids_json).bind(&row.routes_json).bind(&row.output_json)
        .bind(&row.node_info_json).bind(&row.requested_input_json)
        .bind(&row.branch).bind(&row.isolation_scope).bind(row.timestamp)
        .bind(&row.content).bind(&row.grounding_metadata).bind(&row.custom_metadata)
        .bind(&row.usage_metadata).bind(&row.citation_metadata)
        .bind(&row.input_transcription).bind(&row.output_transcription)
        .bind(row.partial).bind(row.turn_complete)
        .bind(&row.error_code).bind(&row.error_message).bind(row.interrupted)
        .execute(&mut *c).await.map(|_| ())
}

pub(crate) async fn select_events(
    c: &mut Conn,
    app_name: &str,
    user_id: &str,
    session_id: &str,
    after: Option<DateTime<Utc>>,
    limit: Option<i64>,
) -> sqlx::Result<Vec<EventRow>> {
    let mut sql = format!(
        "{EVENT_SELECT} WHERE \"app_name\" = $1 AND \"user_id\" = $2 AND \"session_id\" = $3"
    );
    let mut n = 3;
    if after.is_some() {
        n += 1;
        sql.push_str(&format!(" AND \"timestamp\" >= ${n}"));
    }
    sql.push_str(" ORDER BY \"timestamp\" DESC, \"id\" DESC");
    if limit.is_some() {
        n += 1;
        sql.push_str(&format!(" LIMIT ${n}"));
    }
    let mut q = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(app_name)
        .bind(user_id)
        .bind(session_id);
    if let Some(after) = after {
        q = q.bind(after);
    }
    if let Some(limit) = limit {
        q = q.bind(limit);
    }
    q.fetch_all(&mut *c).await?.iter().map(event_row).collect()
}

pub(crate) async fn existing_columns(c: &mut Conn, table: &str) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar::<_, String>("SELECT column_name::text FROM information_schema.columns WHERE table_schema = current_schema() AND table_name = $1 ORDER BY ordinal_position")
        .bind(table).fetch_all(&mut *c).await
}

pub(crate) async fn execute(c: &mut Conn, sql: &str) -> sqlx::Result<()> {
    sqlx::query(sqlx::AssertSqlSafe(sql.to_string()))
        .execute(&mut *c)
        .await
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn table_columns_match_their_create_statements() {
        for t in &TABLES {
            let mut at = 0;
            for (name, ty) in t.columns {
                let needle = format!("\"{name}\" {ty}");
                let pos = t.create[at..]
                    .find(&needle)
                    .unwrap_or_else(|| panic!("{}: {needle}", t.name));
                at += pos + needle.len();
            }
        }
        let names: Vec<&str> = TABLES.iter().map(|t| t.name).collect();
        assert_eq!(names, ["sessions", "events", "app_states", "user_states"]);
    }

    #[test]
    fn events_cascade_on_session_delete() {
        assert!(TABLES[1].create.ends_with(
            "REFERENCES \"sessions\"(\"app_name\",\"user_id\",\"id\") ON DELETE CASCADE)"
        ));
    }

    #[test]
    fn add_column_sql_is_gorms() {
        assert_eq!(
            add_column_sql("events", "interrupted", "boolean"),
            "ALTER TABLE \"events\" ADD \"interrupted\" boolean"
        );
    }
}
