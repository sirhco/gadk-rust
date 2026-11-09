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

//! The sqlite dialect (Go glebarez/sqlite + gorm): the exact DDL ADK-Go creates, the SQL each
//! service operation issues, and the go-sqlite timestamp codec.

use std::str::FromStr;

use chrono::{DateTime, NaiveDate, NaiveDateTime, Timelike, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteRow};
use sqlx::{Row, SqliteConnection as Conn, SqlitePool};

use crate::migrate::TableDef;
use crate::rows::{
    AppStateRow, ColumnReader, EventRow, SessionRow, UserStateRow, app_state_row, event_row,
    session_row, user_state_row,
};

/// ADK-Go's gorm DDL on glebarez/sqlite, byte for byte, in `AutoMigrate` order.
pub(crate) static TABLES: [TableDef; 4] = [
    TableDef {
        name: "sessions",
        create: "CREATE TABLE `sessions` (`app_name` text,`user_id` text,`id` text,`state` text,`create_time` datetime,`update_time` datetime,PRIMARY KEY (`app_name`,`user_id`,`id`))",
        columns: &[
            ("app_name", "text"),
            ("user_id", "text"),
            ("id", "text"),
            ("state", "text"),
            ("create_time", "datetime"),
            ("update_time", "datetime"),
        ],
    },
    TableDef {
        name: "events",
        create: "CREATE TABLE `events` (`id` text,`app_name` text,`user_id` text,`session_id` text,`invocation_id` text,`author` text,`actions` blob,`long_running_tool_ids_json` text,`routes_json` text,`output_json` text,`node_info_json` text,`requested_input_json` text,`branch` text,`isolation_scope` text,`timestamp` datetime,`content` text,`grounding_metadata` text,`custom_metadata` text,`usage_metadata` text,`citation_metadata` text,`input_transcription` text,`output_transcription` text,`partial` numeric,`turn_complete` numeric,`error_code` text,`error_message` text,`interrupted` numeric,PRIMARY KEY (`id`,`app_name`,`user_id`,`session_id`),CONSTRAINT `fk_sessions_events` FOREIGN KEY (`app_name`,`user_id`,`session_id`) REFERENCES `sessions`(`app_name`,`user_id`,`id`) ON DELETE CASCADE)",
        columns: &[
            ("id", "text"),
            ("app_name", "text"),
            ("user_id", "text"),
            ("session_id", "text"),
            ("invocation_id", "text"),
            ("author", "text"),
            ("actions", "blob"),
            ("long_running_tool_ids_json", "text"),
            ("routes_json", "text"),
            ("output_json", "text"),
            ("node_info_json", "text"),
            ("requested_input_json", "text"),
            ("branch", "text"),
            ("isolation_scope", "text"),
            ("timestamp", "datetime"),
            ("content", "text"),
            ("grounding_metadata", "text"),
            ("custom_metadata", "text"),
            ("usage_metadata", "text"),
            ("citation_metadata", "text"),
            ("input_transcription", "text"),
            ("output_transcription", "text"),
            ("partial", "numeric"),
            ("turn_complete", "numeric"),
            ("error_code", "text"),
            ("error_message", "text"),
            ("interrupted", "numeric"),
        ],
    },
    TableDef {
        name: "app_states",
        create: "CREATE TABLE `app_states` (`app_name` text,`state` text,`update_time` datetime,PRIMARY KEY (`app_name`))",
        columns: &[
            ("app_name", "text"),
            ("state", "text"),
            ("update_time", "datetime"),
        ],
    },
    TableDef {
        name: "user_states",
        create: "CREATE TABLE `user_states` (`app_name` text,`user_id` text,`state` text,`update_time` datetime,PRIMARY KEY (`app_name`,`user_id`))",
        columns: &[
            ("app_name", "text"),
            ("user_id", "text"),
            ("state", "text"),
            ("update_time", "datetime"),
        ],
    },
];

const EVENT_COLUMNS: &str = "`id`, `app_name`, `user_id`, `session_id`, `invocation_id`, `author`, `actions`, `long_running_tool_ids_json`, `routes_json`, `output_json`, `node_info_json`, `requested_input_json`, `branch`, `isolation_scope`, `timestamp`, `content`, `grounding_metadata`, `custom_metadata`, `usage_metadata`, `citation_metadata`, `input_transcription`, `output_transcription`, `partial`, `turn_complete`, `error_code`, `error_message`, `interrupted`";
const SESSION_COLUMNS: &str = "`app_name`, `user_id`, `id`, `state`, `create_time`, `update_time`";

/// Opens a pool like ADK-Go's sqlite dialector: the file is created when missing and foreign
/// keys are not enforced (go-sqlite never turns them on). An in-memory database lives in its
/// single connection, so that connection is never closed.
pub(crate) async fn connect(url: &str) -> sqlx::Result<SqlitePool> {
    let opts = SqliteConnectOptions::from_str(url)?
        .create_if_missing(true)
        .foreign_keys(false);
    let pool = if is_memory(url) {
        SqlitePoolOptions::new()
            .max_connections(1)
            .min_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
    } else {
        SqlitePoolOptions::new()
    };
    pool.connect_with(opts).await
}

fn is_memory(url: &str) -> bool {
    let rest = url.strip_prefix("sqlite:").unwrap_or(url);
    let rest = rest.strip_prefix("//").unwrap_or(rest);
    rest.starts_with(":memory:") || rest.contains("mode=memory")
}

/// gorm's sqlite `AddColumn`.
pub(crate) fn add_column_sql(table: &str, column: &str, ty: &str) -> String {
    format!("ALTER TABLE `{table}` ADD `{column}` {ty}")
}

/// go-sqlite's write layout, `2006-01-02 15:04:05.999999999-07:00`, for a UTC time: trailing
/// fraction zeros trimmed, no fraction when zero, offset `+00:00`. The time is written at the
/// precision it carries.
pub(crate) fn encode_time(t: &DateTime<Utc>) -> String {
    let mut s = t.format("%Y-%m-%d %H:%M:%S").to_string();
    let nanos = t.nanosecond() % 1_000_000_000;
    if nanos != 0 {
        let frac = format!("{nanos:09}");
        s.push('.');
        s.push_str(frac.trim_end_matches('0'));
    }
    s.push_str("+00:00");
    s
}

/// go-sqlite `parseTime`: first the `time.Time.String()` form (`… -0700 MST [m=±…]`), then,
/// with one trailing `Z` removed, each `parseTimeFormats` layout (offset-less ones are UTC).
pub(crate) fn decode_time(s: &str) -> Result<DateTime<Utc>, String> {
    let cut = match s.find("m=") {
        Some(x) if x > 0 => s.get(..x).unwrap_or(s),
        _ => s,
    }
    .trim();
    if let Some((rest, _zone_abbrev)) = cut.rsplit_once(' ')
        && let Ok(t) = DateTime::parse_from_str(rest, "%Y-%m-%d %H:%M:%S%.f %z")
    {
        return Ok(t.with_timezone(&Utc));
    }
    let ts = s.strip_suffix('Z').unwrap_or(s);
    for f in ["%Y-%m-%d %H:%M:%S%.f%:z", "%Y-%m-%dT%H:%M:%S%.f%:z"] {
        if let Ok(t) = DateTime::parse_from_str(ts, f) {
            return Ok(t.with_timezone(&Utc));
        }
    }
    for f in [
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M",
    ] {
        if let Ok(t) = NaiveDateTime::parse_from_str(ts, f) {
            return Ok(t.and_utc());
        }
    }
    if let Some(t) = NaiveDate::parse_from_str(ts, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
    {
        return Ok(t.and_utc());
    }
    Err(format!("unrecognized sqlite time {s:?}"))
}

fn decode_err(col: &str, msg: String) -> sqlx::Error {
    sqlx::Error::ColumnDecode {
        index: col.to_string(),
        source: msg.into(),
    }
}

impl ColumnReader for SqliteRow {
    fn text(&self, col: &str) -> sqlx::Result<String> {
        self.try_get_unchecked(col)
    }

    fn opt_text(&self, col: &str) -> sqlx::Result<Option<String>> {
        self.try_get_unchecked(col)
    }

    fn opt_bytes(&self, col: &str) -> sqlx::Result<Option<Vec<u8>>> {
        self.try_get_unchecked(col)
    }

    /// A `datetime` column in go-sqlite's text layout; NULL is Go's zero time.
    fn time(&self, col: &str) -> sqlx::Result<DateTime<Utc>> {
        match self.opt_text(col)? {
            None => Ok(gadk_session::zero_time()),
            Some(s) => decode_time(&s).map_err(|e| decode_err(col, e)),
        }
    }

    /// A `numeric` boolean column (Go binds bools as 0/1).
    fn flag(&self, col: &str) -> sqlx::Result<Option<bool>> {
        Ok(self
            .try_get_unchecked::<Option<i64>, _>(col)?
            .map(|n| n != 0))
    }
}

pub(crate) async fn select_session(
    c: &mut Conn,
    app_name: &str,
    user_id: &str,
    id: &str,
) -> sqlx::Result<Option<SessionRow>> {
    let sql = format!(
        "SELECT {SESSION_COLUMNS} FROM `sessions` WHERE `app_name` = ? AND `user_id` = ? AND `id` = ? LIMIT 1"
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
            let sql = format!(
                "SELECT {SESSION_COLUMNS} FROM `sessions` WHERE `app_name` = ? AND `user_id` = ?"
            );
            sqlx::query(sqlx::AssertSqlSafe(sql))
                .bind(app_name)
                .bind(user_id)
                .fetch_all(&mut *c)
                .await?
        }
        None => {
            let sql = format!("SELECT {SESSION_COLUMNS} FROM `sessions` WHERE `app_name` = ?");
            sqlx::query(sqlx::AssertSqlSafe(sql))
                .bind(app_name)
                .fetch_all(&mut *c)
                .await?
        }
    };
    rows.iter().map(session_row).collect()
}

pub(crate) async fn insert_session(c: &mut Conn, row: &SessionRow) -> sqlx::Result<()> {
    let sql = format!("INSERT INTO `sessions` ({SESSION_COLUMNS}) VALUES (?, ?, ?, ?, ?, ?)");
    sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(&row.app_name)
        .bind(&row.user_id)
        .bind(&row.id)
        .bind(&row.state)
        .bind(encode_time(&row.create_time))
        .bind(encode_time(&row.update_time))
        .execute(&mut *c)
        .await
        .map(|_| ())
}

/// Go `Save` of a read session: every column rewritten.
pub(crate) async fn update_session(c: &mut Conn, row: &SessionRow) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE `sessions` SET `state` = ?, `create_time` = ?, `update_time` = ? WHERE `app_name` = ? AND `user_id` = ? AND `id` = ?",
    )
    .bind(&row.state)
    .bind(encode_time(&row.create_time))
    .bind(encode_time(&row.update_time))
    .bind(&row.app_name)
    .bind(&row.user_id)
    .bind(&row.id)
    .execute(&mut *c)
    .await
    .map(|_| ())
}

pub(crate) async fn delete_session(
    c: &mut Conn,
    app_name: &str,
    user_id: &str,
    id: &str,
) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM `sessions` WHERE `app_name` = ? AND `user_id` = ? AND `id` = ?")
        .bind(app_name)
        .bind(user_id)
        .bind(id)
        .execute(&mut *c)
        .await
        .map(|_| ())
}

/// `for_update` is ignored: sqlite has no row locks; `BEGIN IMMEDIATE` serializes writers.
pub(crate) async fn select_app_state(
    c: &mut Conn,
    app_name: &str,
    _for_update: bool,
) -> sqlx::Result<Option<AppStateRow>> {
    sqlx::query(
        "SELECT `app_name`, `state`, `update_time` FROM `app_states` WHERE `app_name` = ? ORDER BY `app_name` LIMIT 1",
    )
    .bind(app_name)
    .fetch_optional(&mut *c)
    .await?
    .as_ref()
    .map(app_state_row)
    .transpose()
}

/// `for_update` is ignored: sqlite has no row locks; `BEGIN IMMEDIATE` serializes writers.
pub(crate) async fn select_user_state(
    c: &mut Conn,
    app_name: &str,
    user_id: &str,
    _for_update: bool,
) -> sqlx::Result<Option<UserStateRow>> {
    sqlx::query(
        "SELECT `app_name`, `user_id`, `state`, `update_time` FROM `user_states` WHERE `app_name` = ? AND `user_id` = ? ORDER BY `app_name` LIMIT 1",
    )
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
    sqlx::query(
        "SELECT `app_name`, `user_id`, `state`, `update_time` FROM `user_states` WHERE `app_name` = ?",
    )
    .bind(app_name)
    .fetch_all(&mut *c)
    .await?
    .iter()
    .map(user_state_row)
    .collect()
}

pub(crate) async fn insert_app_state_if_absent(
    c: &mut Conn,
    row: &AppStateRow,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO `app_states` (`app_name`, `state`, `update_time`) VALUES (?, ?, ?) ON CONFLICT DO NOTHING",
    )
    .bind(&row.app_name)
    .bind(&row.state)
    .bind(encode_time(&row.update_time))
    .execute(&mut *c)
    .await
    .map(|_| ())
}

pub(crate) async fn insert_user_state_if_absent(
    c: &mut Conn,
    row: &UserStateRow,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO `user_states` (`app_name`, `user_id`, `state`, `update_time`) VALUES (?, ?, ?, ?) ON CONFLICT DO NOTHING",
    )
    .bind(&row.app_name)
    .bind(&row.user_id)
    .bind(&row.state)
    .bind(encode_time(&row.update_time))
    .execute(&mut *c)
    .await
    .map(|_| ())
}

/// Go `Save`: insert, or rewrite every column of the existing row.
pub(crate) async fn save_app_state(c: &mut Conn, row: &AppStateRow) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO `app_states` (`app_name`, `state`, `update_time`) VALUES (?, ?, ?) ON CONFLICT (`app_name`) DO UPDATE SET `state` = excluded.`state`, `update_time` = excluded.`update_time`",
    )
    .bind(&row.app_name)
    .bind(&row.state)
    .bind(encode_time(&row.update_time))
    .execute(&mut *c)
    .await
    .map(|_| ())
}

/// Go `Save`: insert, or rewrite every column of the existing row.
pub(crate) async fn save_user_state(c: &mut Conn, row: &UserStateRow) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO `user_states` (`app_name`, `user_id`, `state`, `update_time`) VALUES (?, ?, ?, ?) ON CONFLICT (`app_name`, `user_id`) DO UPDATE SET `state` = excluded.`state`, `update_time` = excluded.`update_time`",
    )
    .bind(&row.app_name)
    .bind(&row.user_id)
    .bind(&row.state)
    .bind(encode_time(&row.update_time))
    .execute(&mut *c)
    .await
    .map(|_| ())
}

pub(crate) async fn insert_event(c: &mut Conn, row: &EventRow) -> sqlx::Result<()> {
    let sql = format!(
        "INSERT INTO `events` ({EVENT_COLUMNS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
    );
    sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(&row.id)
        .bind(&row.app_name)
        .bind(&row.user_id)
        .bind(&row.session_id)
        .bind(&row.invocation_id)
        .bind(&row.author)
        .bind(&row.actions)
        .bind(&row.long_running_tool_ids_json)
        .bind(&row.routes_json)
        .bind(&row.output_json)
        .bind(&row.node_info_json)
        .bind(&row.requested_input_json)
        .bind(&row.branch)
        .bind(&row.isolation_scope)
        .bind(encode_time(&row.timestamp))
        .bind(&row.content)
        .bind(&row.grounding_metadata)
        .bind(&row.custom_metadata)
        .bind(&row.usage_metadata)
        .bind(&row.citation_metadata)
        .bind(&row.input_transcription)
        .bind(&row.output_transcription)
        .bind(row.partial)
        .bind(row.turn_complete)
        .bind(&row.error_code)
        .bind(&row.error_message)
        .bind(row.interrupted)
        .execute(&mut *c)
        .await
        .map(|_| ())
}

/// `after` is compared as text in the stored layout, as Go binds it.
pub(crate) async fn select_events(
    c: &mut Conn,
    app_name: &str,
    user_id: &str,
    session_id: &str,
    after: Option<DateTime<Utc>>,
    limit: Option<i64>,
) -> sqlx::Result<Vec<EventRow>> {
    let mut sql = format!(
        "SELECT {EVENT_COLUMNS} FROM `events` WHERE `app_name` = ? AND `user_id` = ? AND `session_id` = ?"
    );
    if after.is_some() {
        sql.push_str(" AND `timestamp` >= ?");
    }
    sql.push_str(" ORDER BY `timestamp` DESC, `id` DESC");
    if limit.is_some() {
        sql.push_str(" LIMIT ?");
    }
    let mut q = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(app_name)
        .bind(user_id)
        .bind(session_id);
    if let Some(after) = after {
        q = q.bind(encode_time(&after));
    }
    if let Some(limit) = limit {
        q = q.bind(limit);
    }
    q.fetch_all(&mut *c).await?.iter().map(event_row).collect()
}

pub(crate) async fn existing_columns(c: &mut Conn, table: &str) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar::<_, String>("SELECT `name` FROM pragma_table_info(?) ORDER BY `cid`")
        .bind(table)
        .fetch_all(&mut *c)
        .await
}

pub(crate) async fn execute(c: &mut Conn, sql: &str) -> sqlx::Result<()> {
    sqlx::query(sqlx::AssertSqlSafe(sql.to_string()))
        .execute(&mut *c)
        .await
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use chrono::TimeZone;
    use pretty_assertions::assert_eq;

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32, nanos: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
            + chrono::Duration::nanoseconds(i64::from(nanos))
    }

    #[test]
    fn encode_time_uses_gos_layout() {
        assert_eq!(
            encode_time(&utc(2026, 3, 4, 5, 6, 7, 0)),
            "2026-03-04 05:06:07+00:00"
        );
        assert_eq!(
            encode_time(&utc(2026, 3, 4, 5, 6, 7, 123_456_000)),
            "2026-03-04 05:06:07.123456+00:00"
        );
        assert_eq!(
            encode_time(&utc(2026, 3, 4, 5, 6, 7, 123_456_789)),
            "2026-03-04 05:06:07.123456789+00:00"
        );
        assert_eq!(
            encode_time(&utc(2026, 3, 4, 5, 6, 7, 500_000_000)),
            "2026-03-04 05:06:07.5+00:00"
        );
        assert_eq!(
            encode_time(&gadk_session::zero_time()),
            "0001-01-01 00:00:00+00:00"
        );
    }

    #[test]
    fn decode_time_accepts_every_go_sqlite_form() {
        let want = utc(2026, 3, 4, 5, 6, 7, 123_456_000);
        for s in [
            "2026-03-04 05:06:07.123456+00:00",
            "2026-03-04 07:06:07.123456+02:00",
            "2026-03-04T05:06:07.123456+00:00",
            "2026-03-04 05:06:07.123456",
            "2026-03-04T05:06:07.123456Z",
            "2026-03-04 05:06:07.123456Z",
            "2026-03-04 07:06:07.123456 +0200 CEST",
            "2026-03-04 07:06:07.123456 +0200 CEST m=+0.000123",
        ] {
            assert_eq!(decode_time(s).unwrap(), want, "{s}");
        }
        assert_eq!(
            decode_time("2026-03-04 05:06").unwrap(),
            utc(2026, 3, 4, 5, 6, 0, 0)
        );
        assert_eq!(
            decode_time("2026-03-04T05:06").unwrap(),
            utc(2026, 3, 4, 5, 6, 0, 0)
        );
        assert_eq!(
            decode_time("2026-03-04").unwrap(),
            utc(2026, 3, 4, 0, 0, 0, 0)
        );
        assert!(decode_time("yesterday").is_err());
    }

    #[test]
    fn time_codec_round_trips() {
        for t in [
            utc(2026, 3, 4, 5, 6, 7, 0),
            utc(1999, 12, 31, 23, 59, 59, 999_999_000),
            gadk_session::zero_time(),
        ] {
            assert_eq!(decode_time(&encode_time(&t)).unwrap(), t);
        }
    }

    #[test]
    fn table_columns_match_their_create_statements() {
        for t in &TABLES {
            let mut at = 0;
            for (name, ty) in t.columns {
                let needle = format!("`{name}` {ty}");
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
    fn add_column_sql_is_gorms() {
        assert_eq!(
            add_column_sql("events", "interrupted", "numeric"),
            "ALTER TABLE `events` ADD `interrupted` numeric"
        );
    }

    /// Every row of every table as `quote()`d column text, in primary-key order.
    async fn snapshot(c: &mut Conn) -> Vec<String> {
        let mut out = Vec::new();
        for t in &TABLES {
            let cols: Vec<String> = t
                .columns
                .iter()
                .map(|(n, _)| format!("quote(`{n}`)"))
                .collect();
            let q = format!(
                "SELECT {} FROM `{}` ORDER BY 1",
                cols.join(" || ',' || "),
                t.name
            );
            let rows: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(q))
                .fetch_all(&mut *c)
                .await
                .unwrap();
            out.extend(rows.into_iter().map(|r| format!("{}: {r}", t.name)));
        }
        out.sort();
        out
    }

    /// Go's rows read through this module's selects and written back through its inserts and
    /// saves come out byte-equal.
    #[tokio::test]
    async fn go_rows_round_trip_through_reads_and_writes() {
        use sqlx::ConnectOptions;
        let mut c = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .foreign_keys(false)
            .connect()
            .await
            .unwrap();
        for t in &TABLES {
            execute(&mut c, t.create).await.unwrap();
        }
        let fixture = include_str!("../tests/testdata/rows.sqlite.sql");
        for line in fixture.lines().filter(|l| !l.is_empty()) {
            execute(&mut c, line).await.unwrap();
        }
        let want = snapshot(&mut c).await;
        assert!(want.len() > 4, "{want:?}");

        let sessions = list_sessions(&mut c, "app", None).await.unwrap();
        let mut events = Vec::new();
        for (u, s) in [("u1", "s1"), ("u1", "s3"), ("u2", "s2")] {
            events.extend(
                select_events(&mut c, "app", u, s, None, None)
                    .await
                    .unwrap(),
            );
        }
        let app = select_app_state(&mut c, "app", false).await.unwrap();
        let users = select_user_states(&mut c, "app").await.unwrap();
        assert_eq!(
            select_session(&mut c, "app", "u1", "s1")
                .await
                .unwrap()
                .as_ref(),
            sessions.iter().find(|s| s.id == "s1")
        );

        for t in &TABLES {
            execute(&mut c, &format!("DELETE FROM `{}`", t.name))
                .await
                .unwrap();
        }
        for s in &sessions {
            insert_session(&mut c, s).await.unwrap();
        }
        for e in &events {
            insert_event(&mut c, e).await.unwrap();
        }
        if let Some(a) = &app {
            save_app_state(&mut c, a).await.unwrap();
        }
        for u in &users {
            save_user_state(&mut c, u).await.unwrap();
        }
        assert_eq!(snapshot(&mut c).await, want);
    }
}
