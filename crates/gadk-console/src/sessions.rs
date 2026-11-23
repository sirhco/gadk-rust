// Copyright 2026 Chris Olson
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

//! Sessions and users: the agent activity of the agent-analytics table grouped by session and
//! by user.
//!
//! Activity rows are a session's rows other than `FEEDBACK` rows. A session's user and root
//! agent come from its earliest `INVOCATION_STARTING` row in the window. Every query is
//! parameterized (unset filters are typed NULL parameters), filters `timestamp` to the window
//! and runs through [`UsageSource::run`] under the byte cap. Each per-session aggregate is
//! computed on its own before any join, so joins never multiply counts. Tokens come per model,
//! so the pages can price them.

/// The columns of [`session_rows_sql`].
#[cfg(test)]
pub(crate) const SESSION_FIELDS: [(&str, &str); 14] = [
    ("session_id", "STRING"),
    ("user_id", "STRING"),
    ("agent", "STRING"),
    ("first", "TIMESTAMP"),
    ("last", "TIMESTAMP"),
    ("turns", "INT64"),
    ("errors", "INT64"),
    ("ups", "INT64"),
    ("downs", "INT64"),
    ("model", "STRING"),
    ("input", "INT64"),
    ("cached", "INT64"),
    ("output", "INT64"),
    ("reasoning", "INT64"),
];

/// The columns of [`user_rows_sql`].
#[cfg(test)]
pub(crate) const USER_FIELDS: [(&str, &str); 12] = [
    ("user_id", "STRING"),
    ("sessions", "INT64"),
    ("turns", "INT64"),
    ("errors", "INT64"),
    ("ups", "INT64"),
    ("downs", "INT64"),
    ("last", "TIMESTAMP"),
    ("model", "STRING"),
    ("input", "INT64"),
    ("cached", "INT64"),
    ("output", "INT64"),
    ("reasoning", "INT64"),
];
use chrono::{DateTime, Utc};
use serde_json::{Map, Value};

use crate::prices::Usage;
use crate::traces::{SourceError, TimeRange, param, text, timestamp, ts_param};
use crate::usage::{CALL_COLUMNS, UsageSource, count, window_params};

/// The most sessions `/sessions` lists.
pub(crate) const SESSION_CAP: usize = 100;
/// The most users `/users` lists.
pub(crate) const USER_CAP: usize = 100;
/// The most (session or user) × model rows the sessions and users queries return.
const ROW_CAP: u32 = 5_000;
/// The longest session id, user or agent filter, in bytes.
pub(crate) const MAX_ID_LEN: usize = 128;
/// The text of the 400 page for a bad `after` / `after_id` pair.
pub(crate) const INVALID_CURSOR: &str = "invalid cursor";

/// Whether `s` can be a session id: 1 to [`MAX_ID_LEN`] bytes with no control characters.
pub(crate) fn is_session_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_ID_LEN && !s.chars().any(char::is_control)
}

/// A position in the sessions list: the sort key `(last, session id)` of the session last
/// shown. The next page starts with the largest sort key strictly below it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SessionCursor {
    /// The session's last activity.
    pub(crate) at: DateTime<Utc>,
    /// The session id, as given.
    pub(crate) id: String,
}

impl SessionCursor {
    /// The cursor of the URL values `after` (RFC 3339) and `after_id` (a session id, kept as
    /// given).
    ///
    /// # Errors
    ///
    /// [`INVALID_CURSOR`] when either value is invalid.
    pub(crate) fn parse(after: &str, after_id: &str) -> Result<Self, &'static str> {
        let at = DateTime::parse_from_rfc3339(after)
            .map_err(|_| INVALID_CURSOR)?
            .with_timezone(&Utc);
        if !is_session_id(after_id) {
            return Err(INVALID_CURSOR);
        }
        Ok(Self {
            at,
            id: after_id.to_owned(),
        })
    }
}

/// The sessions of `[end - range, end)` to list.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SessionQuery {
    /// The window length.
    pub(crate) range: TimeRange,
    /// The window end (now).
    pub(crate) end: DateTime<Utc>,
    /// Only sessions of this user.
    pub(crate) user: Option<String>,
    /// Only sessions of this root agent.
    pub(crate) agent: Option<String>,
    /// Only this session.
    pub(crate) session: Option<String>,
    /// Only sessions after this cursor.
    pub(crate) after: Option<SessionCursor>,
}

/// The users of `[end - range, end)` to list.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct UserQuery {
    /// The window length.
    pub(crate) range: TimeRange,
    /// The window end (now).
    pub(crate) end: DateTime<Utc>,
    /// Only sessions of this root agent count.
    pub(crate) agent: Option<String>,
}

/// One session's activity in the window.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SessionRow {
    /// The session id.
    pub(crate) session_id: String,
    /// The user of its earliest `INVOCATION_STARTING` row; empty when none.
    pub(crate) user: String,
    /// The root agent of that row; empty when none.
    pub(crate) agent: String,
    /// The first activity row.
    pub(crate) first: DateTime<Utc>,
    /// The last activity row.
    pub(crate) last: DateTime<Utc>,
    /// Distinct non-empty invocation ids.
    pub(crate) turns: u64,
    /// `LLM_ERROR` and `TOOL_ERROR` rows and rows with status `ERROR`.
    pub(crate) errors: u64,
    /// Latest boolean scores that are `true`.
    pub(crate) ups: u64,
    /// Latest boolean scores that are `false`.
    pub(crate) downs: u64,
    /// The `LLM_RESPONSE` tokens per model.
    pub(crate) tokens: Vec<(String, Usage)>,
}

/// A page of sessions.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SessionPage {
    /// At most [`SESSION_CAP`] sessions, newest last activity first.
    pub(crate) sessions: Vec<SessionRow>,
    /// More sessions follow the last one.
    pub(crate) has_next: bool,
    /// The query hit its row cap: some sessions or models are left out.
    pub(crate) truncated: bool,
}

/// One user's activity in the window.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct UserRow {
    /// The user id; empty for sessions without one.
    pub(crate) user: String,
    /// Distinct sessions.
    pub(crate) sessions: u64,
    /// Turns over those sessions.
    pub(crate) turns: u64,
    /// Errors over those sessions.
    pub(crate) errors: u64,
    /// Latest boolean scores that are `true`.
    pub(crate) ups: u64,
    /// Latest boolean scores that are `false`.
    pub(crate) downs: u64,
    /// The last activity of those sessions.
    pub(crate) last: DateTime<Utc>,
    /// The `LLM_RESPONSE` tokens per model.
    pub(crate) tokens: Vec<(String, Usage)>,
}

/// A page of users.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct UserPage {
    /// At most [`USER_CAP`] users, most recent last activity first.
    pub(crate) users: Vec<UserRow>,
    /// More users matched.
    pub(crate) more: bool,
    /// The query hit its row cap: some users or models are left out.
    pub(crate) truncated: bool,
}

impl UsageSource {
    /// The sessions of `q`, newest last activity first: at most [`SESSION_CAP`], with whether
    /// more follow.
    pub(crate) async fn sessions(&self, q: &SessionQuery) -> Result<SessionPage, SourceError> {
        let sql = session_rows_sql(&self.table_path()?, q.after.is_some());
        let mut params = window_params(q.end - q.range.duration(), q.end);
        params.push(param("user", "STRING", q.user.clone()));
        params.push(param("agent", "STRING", q.agent.clone()));
        params.push(param("session", "STRING", q.session.clone()));
        params.push(param("limit", "INT64", Some((SESSION_CAP + 1).to_string())));
        if let Some(c) = &q.after {
            params.push(param("after", "TIMESTAMP", Some(ts_param(c.at))));
            params.push(param("after_id", "STRING", Some(c.id.clone())));
        }
        let result = self.run(&sql, params, ROW_CAP).await?;
        let mut sessions: Vec<SessionRow> = Vec::new();
        let mut has_next = false;
        for row in &result.rows {
            let Some(id) = text(row, "session_id") else {
                continue;
            };
            if let Some(last) = sessions.last_mut().filter(|s| s.session_id == id) {
                add_tokens(&mut last.tokens, row);
                continue;
            }
            if sessions.len() == SESSION_CAP {
                has_next = true;
                break;
            }
            let mut s = SessionRow {
                session_id: id,
                user: text(row, "user_id").unwrap_or_default(),
                agent: text(row, "agent").unwrap_or_default(),
                first: timestamp(row, "first")?,
                last: timestamp(row, "last")?,
                turns: count(row, "turns"),
                errors: count(row, "errors"),
                ups: count(row, "ups"),
                downs: count(row, "downs"),
                tokens: Vec::new(),
            };
            add_tokens(&mut s.tokens, row);
            sessions.push(s);
        }
        Ok(SessionPage {
            sessions,
            has_next,
            truncated: result.has_more,
        })
    }

    /// The users of `q`, most recent last activity first: at most [`USER_CAP`], with whether
    /// more matched.
    pub(crate) async fn users(&self, q: &UserQuery) -> Result<UserPage, SourceError> {
        let sql = user_rows_sql(&self.table_path()?);
        let mut params = window_params(q.end - q.range.duration(), q.end);
        params.push(param("agent", "STRING", q.agent.clone()));
        params.push(param("limit", "INT64", Some((USER_CAP + 1).to_string())));
        let result = self.run(&sql, params, ROW_CAP).await?;
        let mut users: Vec<UserRow> = Vec::new();
        let mut more = false;
        for row in &result.rows {
            let user = text(row, "user_id").unwrap_or_default();
            if let Some(last) = users.last_mut().filter(|u| u.user == user) {
                add_tokens(&mut last.tokens, row);
                continue;
            }
            if users.len() == USER_CAP {
                more = true;
                break;
            }
            let mut u = UserRow {
                user,
                sessions: count(row, "sessions"),
                turns: count(row, "turns"),
                errors: count(row, "errors"),
                ups: count(row, "ups"),
                downs: count(row, "downs"),
                last: timestamp(row, "last")?,
                tokens: Vec::new(),
            };
            add_tokens(&mut u.tokens, row);
            users.push(u);
        }
        Ok(UserPage {
            users,
            more,
            truncated: result.has_more,
        })
    }
}

/// The `input`, `cached`, `output` and `reasoning` cells of `row`.
fn usage_of(row: &Map<String, Value>) -> Usage {
    Usage {
        input: count(row, "input"),
        cached: count(row, "cached"),
        output: count(row, "output"),
        reasoning: count(row, "reasoning"),
    }
}

/// Adds the tokens of `row`'s `model` (none when NULL) to `tokens`, summing a model seen
/// before.
fn add_tokens(tokens: &mut Vec<(String, Usage)>, row: &Map<String, Value>) {
    let Some(model) = text(row, "model") else {
        return;
    };
    let usage = usage_of(row);
    match tokens.iter_mut().find(|(m, _)| *m == model) {
        Some((_, sum)) => *sum += usage,
        None => tokens.push((model, usage)),
    }
}

/// The CTEs shared by [`session_rows_sql`] and [`user_rows_sql`], each over `[@start, @end)`
/// and a non-empty `session_id`, each aggregated on its own:
///
/// - `head`: per session, the user and agent (`h`) of the earliest `INVOCATION_STARTING` row;
/// - `act`: per session, first and last activity, turns and errors;
/// - `fb`: per session, the latest boolean `FEEDBACK` per (invocation, name, source) that are
///   `true` (`ups`) and `false` (`downs`);
/// - `req`, `c`, `tok`: per session and model, the `LLM_RESPONSE` tokens.
fn session_ctes(table: &str) -> String {
    format!(
        r"WITH head AS (
  SELECT session_id,
    ARRAY_AGG(STRUCT(user_id, agent) ORDER BY timestamp, event_id LIMIT 1)[OFFSET(0)] AS h
  FROM {table}
  WHERE event_type = 'INVOCATION_STARTING' AND timestamp >= @start AND timestamp < @end
    AND session_id IS NOT NULL AND session_id != ''
  GROUP BY session_id),
act AS (
  SELECT session_id, MIN(timestamp) AS first, MAX(timestamp) AS last,
    COUNT(DISTINCT NULLIF(invocation_id, '')) AS turns,
    COUNTIF(event_type IN ('LLM_ERROR', 'TOOL_ERROR') OR status = 'ERROR') AS errors
  FROM {table}
  WHERE event_type != 'FEEDBACK' AND timestamp >= @start AND timestamp < @end
    AND session_id IS NOT NULL AND session_id != ''
  GROUP BY session_id),
fb AS (
  SELECT session_id, COUNTIF(value = 'true') AS ups, COUNTIF(value = 'false') AS downs
  FROM (
    SELECT session_id, JSON_VALUE(content, '$.data_type') AS data_type,
      JSON_VALUE(content, '$.value') AS value
    FROM {table}
    WHERE event_type = 'FEEDBACK' AND timestamp >= @start AND timestamp < @end
      AND session_id IS NOT NULL AND session_id != ''
      AND invocation_id IS NOT NULL AND invocation_id != ''
    QUALIFY ROW_NUMBER() OVER (
      PARTITION BY invocation_id, JSON_VALUE(content, '$.name'), JSON_VALUE(attributes, '$.source')
      ORDER BY timestamp DESC, event_id DESC) = 1)
  WHERE data_type = 'bool'
  GROUP BY session_id),
req AS (
  SELECT trace_id, span_id, ANY_VALUE(JSON_VALUE(attributes, '$.model')) AS model
  FROM {table}
  WHERE event_type = 'LLM_REQUEST' AND timestamp >= @start AND timestamp < @end
  GROUP BY trace_id, span_id),
c AS (
  SELECT res.session_id AS session_id,
    COALESCE(NULLIF(req.model, ''), NULLIF(JSON_VALUE(res.attributes, '$.model_version'), ''), '(unknown)') AS model,
    {CALL_COLUMNS}
  FROM {table} AS res
  LEFT JOIN req ON req.trace_id = res.trace_id AND req.span_id = res.span_id
  WHERE res.event_type = 'LLM_RESPONSE' AND res.timestamp >= @start AND res.timestamp < @end
    AND res.session_id IS NOT NULL AND res.session_id != ''),
tok AS (
  SELECT session_id, model, SUM(input) AS input, SUM(cached) AS cached, SUM(output) AS output,
    SUM(reasoning) AS reasoning
  FROM c
  GROUP BY session_id, model)"
    )
}

/// One row per (session, model) of the `@limit` sessions with activity in `[@start, @end)`
/// matching `@user`, `@agent` and `@session` (typed NULLs when unset), newest last activity
/// first (`last DESC, session_id DESC`); a session without model calls has one row with a NULL
/// model. `with_cursor` keeps only sort keys strictly below `(@after, @after_id)`.
fn session_rows_sql(table: &str, with_cursor: bool) -> String {
    let ctes = session_ctes(table);
    let cursor = if with_cursor {
        "\n  WHERE (s.last < @after OR (s.last = @after AND s.session_id < @after_id))"
    } else {
        ""
    };
    format!(
        r"{ctes},
s AS (
  SELECT act.session_id AS session_id, IFNULL(head.h.user_id, '') AS user_id,
    IFNULL(head.h.agent, '') AS agent, act.first AS first, act.last AS last,
    act.turns AS turns, act.errors AS errors, IFNULL(fb.ups, 0) AS ups,
    IFNULL(fb.downs, 0) AS downs
  FROM act
  LEFT JOIN head ON head.session_id = act.session_id
  LEFT JOIN fb ON fb.session_id = act.session_id
  WHERE (@user IS NULL OR head.h.user_id = @user)
    AND (@agent IS NULL OR head.h.agent = @agent)
    AND (@session IS NULL OR act.session_id = @session)),
page AS (
  SELECT * FROM s{cursor}
  ORDER BY last DESC, session_id DESC
  LIMIT @limit)
SELECT page.session_id AS session_id, page.user_id AS user_id, page.agent AS agent,
  page.first AS first, page.last AS last, page.turns AS turns, page.errors AS errors,
  page.ups AS ups, page.downs AS downs, tok.model AS model, tok.input AS input,
  tok.cached AS cached, tok.output AS output, tok.reasoning AS reasoning
FROM page
LEFT JOIN tok ON tok.session_id = page.session_id
ORDER BY last DESC, session_id DESC, model"
    )
}

/// One row per (user, model) of the `@limit` users (the session head's user, `''` when none)
/// with activity in `[@start, @end)` in sessions matching `@agent`, most recent last activity
/// first (`last DESC, user_id`); a user without model calls has one row with a NULL model.
fn user_rows_sql(table: &str) -> String {
    let ctes = session_ctes(table);
    format!(
        r"{ctes},
su AS (
  SELECT act.session_id AS session_id, IFNULL(head.h.user_id, '') AS user_id,
    act.last AS last, act.turns AS turns, act.errors AS errors, IFNULL(fb.ups, 0) AS ups,
    IFNULL(fb.downs, 0) AS downs
  FROM act
  LEFT JOIN head ON head.session_id = act.session_id
  LEFT JOIN fb ON fb.session_id = act.session_id
  WHERE (@agent IS NULL OR head.h.agent = @agent)),
u AS (
  SELECT user_id, COUNT(DISTINCT session_id) AS sessions, SUM(turns) AS turns,
    SUM(errors) AS errors, SUM(ups) AS ups, SUM(downs) AS downs, MAX(last) AS last
  FROM su
  GROUP BY user_id),
page AS (
  SELECT * FROM u
  ORDER BY last DESC, user_id
  LIMIT @limit),
ut AS (
  SELECT su.user_id AS user_id, tok.model AS model, SUM(tok.input) AS input,
    SUM(tok.cached) AS cached, SUM(tok.output) AS output, SUM(tok.reasoning) AS reasoning
  FROM su
  JOIN tok ON tok.session_id = su.session_id
  GROUP BY 1, 2)
SELECT page.user_id AS user_id, page.sessions AS sessions, page.turns AS turns,
  page.errors AS errors, page.ups AS ups, page.downs AS downs, page.last AS last,
  ut.model AS model, ut.input AS input, ut.cached AS cached, ut.output AS output,
  ut.reasoning AS reasoning
FROM page
LEFT JOIN ut ON ut.user_id = page.user_id
ORDER BY last DESC, user_id, model"
    )
}

/// The model cells of a test row: `(model, [input, cached, output, reasoning])`, or `None` for
/// a row without model calls (NULL cells).
#[cfg(test)]
pub(crate) type ModelCells<'a> = Option<(&'a str, [u64; 4])>;

/// A TIMESTAMP cell as `jobs.query` sends it (microseconds since the epoch).
#[cfg(test)]
pub(crate) fn ts_cell(t: DateTime<Utc>) -> Value {
    Value::from(t.timestamp_micros().to_string())
}

/// The model, input, cached, output and reasoning cells of `model`.
#[cfg(test)]
fn model_cells(model: ModelCells<'_>) -> Vec<Value> {
    match model {
        Some((name, counts)) => std::iter::once(Value::from(name))
            .chain(counts.iter().map(|n| Value::from(n.to_string())))
            .collect(),
        None => vec![Value::Null; 5],
    }
}

/// The cells of one [`session_rows_sql`] row; `counts` is `[turns, errors, ups, downs]`.
#[cfg(test)]
pub(crate) fn session_cells(
    id: &str,
    user: &str,
    agent: &str,
    first: DateTime<Utc>,
    last: DateTime<Utc>,
    counts: [u64; 4],
    model: ModelCells<'_>,
) -> Vec<Value> {
    let mut cells = vec![
        Value::from(id),
        Value::from(user),
        Value::from(agent),
        ts_cell(first),
        ts_cell(last),
    ];
    cells.extend(counts.iter().map(|n| Value::from(n.to_string())));
    cells.extend(model_cells(model));
    cells
}

/// The cells of one [`user_rows_sql`] row (`None` is a NULL user); `counts` is
/// `[sessions, turns, errors, ups, downs]`.
#[cfg(test)]
pub(crate) fn user_cells(
    user: Option<&str>,
    counts: [u64; 5],
    last: DateTime<Utc>,
    model: ModelCells<'_>,
) -> Vec<Value> {
    let mut cells = vec![user.map_or(Value::Null, Value::from)];
    cells.extend(counts.iter().map(|n| Value::from(n.to_string())));
    cells.push(ts_cell(last));
    cells.extend(model_cells(model));
    cells
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::{TimeDelta, TimeZone};
    use gadk_bigquery::{BigQueryClient, NoAuth};
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;
    use crate::traces::BigQueryTable;
    use crate::traces::test_http::{FakeHttp, param_value, reply};

    const SESSIONS_SQL: &str = r"WITH head AS (
  SELECT session_id,
    ARRAY_AGG(STRUCT(user_id, agent) ORDER BY timestamp, event_id LIMIT 1)[OFFSET(0)] AS h
  FROM `p.d.t`
  WHERE event_type = 'INVOCATION_STARTING' AND timestamp >= @start AND timestamp < @end
    AND session_id IS NOT NULL AND session_id != ''
  GROUP BY session_id),
act AS (
  SELECT session_id, MIN(timestamp) AS first, MAX(timestamp) AS last,
    COUNT(DISTINCT NULLIF(invocation_id, '')) AS turns,
    COUNTIF(event_type IN ('LLM_ERROR', 'TOOL_ERROR') OR status = 'ERROR') AS errors
  FROM `p.d.t`
  WHERE event_type != 'FEEDBACK' AND timestamp >= @start AND timestamp < @end
    AND session_id IS NOT NULL AND session_id != ''
  GROUP BY session_id),
fb AS (
  SELECT session_id, COUNTIF(value = 'true') AS ups, COUNTIF(value = 'false') AS downs
  FROM (
    SELECT session_id, JSON_VALUE(content, '$.data_type') AS data_type,
      JSON_VALUE(content, '$.value') AS value
    FROM `p.d.t`
    WHERE event_type = 'FEEDBACK' AND timestamp >= @start AND timestamp < @end
      AND session_id IS NOT NULL AND session_id != ''
      AND invocation_id IS NOT NULL AND invocation_id != ''
    QUALIFY ROW_NUMBER() OVER (
      PARTITION BY invocation_id, JSON_VALUE(content, '$.name'), JSON_VALUE(attributes, '$.source')
      ORDER BY timestamp DESC, event_id DESC) = 1)
  WHERE data_type = 'bool'
  GROUP BY session_id),
req AS (
  SELECT trace_id, span_id, ANY_VALUE(JSON_VALUE(attributes, '$.model')) AS model
  FROM `p.d.t`
  WHERE event_type = 'LLM_REQUEST' AND timestamp >= @start AND timestamp < @end
  GROUP BY trace_id, span_id),
c AS (
  SELECT res.session_id AS session_id,
    COALESCE(NULLIF(req.model, ''), NULLIF(JSON_VALUE(res.attributes, '$.model_version'), ''), '(unknown)') AS model,
    JSON_VALUE(res.attributes, '$.prompt.name') AS prompt_name,
    SAFE_CAST(JSON_VALUE(res.attributes, '$.prompt.version') AS INT64) AS prompt_version,
    IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.prompt_token_count') AS INT64), 0)
      + IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.tool_use_prompt_token_count') AS INT64), 0) AS input,
    IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.cached_content_token_count') AS INT64), 0) AS cached,
    IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.candidates_token_count') AS INT64), 0) AS output,
    IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.thoughts_token_count') AS INT64), 0) AS reasoning,
    SAFE_CAST(JSON_VALUE(res.latency_ms, '$.total_ms') AS FLOAT64) AS total_ms
  FROM `p.d.t` AS res
  LEFT JOIN req ON req.trace_id = res.trace_id AND req.span_id = res.span_id
  WHERE res.event_type = 'LLM_RESPONSE' AND res.timestamp >= @start AND res.timestamp < @end
    AND res.session_id IS NOT NULL AND res.session_id != ''),
tok AS (
  SELECT session_id, model, SUM(input) AS input, SUM(cached) AS cached, SUM(output) AS output,
    SUM(reasoning) AS reasoning
  FROM c
  GROUP BY session_id, model),
s AS (
  SELECT act.session_id AS session_id, IFNULL(head.h.user_id, '') AS user_id,
    IFNULL(head.h.agent, '') AS agent, act.first AS first, act.last AS last,
    act.turns AS turns, act.errors AS errors, IFNULL(fb.ups, 0) AS ups,
    IFNULL(fb.downs, 0) AS downs
  FROM act
  LEFT JOIN head ON head.session_id = act.session_id
  LEFT JOIN fb ON fb.session_id = act.session_id
  WHERE (@user IS NULL OR head.h.user_id = @user)
    AND (@agent IS NULL OR head.h.agent = @agent)
    AND (@session IS NULL OR act.session_id = @session)),
page AS (
  SELECT * FROM s
  ORDER BY last DESC, session_id DESC
  LIMIT @limit)
SELECT page.session_id AS session_id, page.user_id AS user_id, page.agent AS agent,
  page.first AS first, page.last AS last, page.turns AS turns, page.errors AS errors,
  page.ups AS ups, page.downs AS downs, tok.model AS model, tok.input AS input,
  tok.cached AS cached, tok.output AS output, tok.reasoning AS reasoning
FROM page
LEFT JOIN tok ON tok.session_id = page.session_id
ORDER BY last DESC, session_id DESC, model";

    const USERS_SQL: &str = r"WITH head AS (
  SELECT session_id,
    ARRAY_AGG(STRUCT(user_id, agent) ORDER BY timestamp, event_id LIMIT 1)[OFFSET(0)] AS h
  FROM `p.d.t`
  WHERE event_type = 'INVOCATION_STARTING' AND timestamp >= @start AND timestamp < @end
    AND session_id IS NOT NULL AND session_id != ''
  GROUP BY session_id),
act AS (
  SELECT session_id, MIN(timestamp) AS first, MAX(timestamp) AS last,
    COUNT(DISTINCT NULLIF(invocation_id, '')) AS turns,
    COUNTIF(event_type IN ('LLM_ERROR', 'TOOL_ERROR') OR status = 'ERROR') AS errors
  FROM `p.d.t`
  WHERE event_type != 'FEEDBACK' AND timestamp >= @start AND timestamp < @end
    AND session_id IS NOT NULL AND session_id != ''
  GROUP BY session_id),
fb AS (
  SELECT session_id, COUNTIF(value = 'true') AS ups, COUNTIF(value = 'false') AS downs
  FROM (
    SELECT session_id, JSON_VALUE(content, '$.data_type') AS data_type,
      JSON_VALUE(content, '$.value') AS value
    FROM `p.d.t`
    WHERE event_type = 'FEEDBACK' AND timestamp >= @start AND timestamp < @end
      AND session_id IS NOT NULL AND session_id != ''
      AND invocation_id IS NOT NULL AND invocation_id != ''
    QUALIFY ROW_NUMBER() OVER (
      PARTITION BY invocation_id, JSON_VALUE(content, '$.name'), JSON_VALUE(attributes, '$.source')
      ORDER BY timestamp DESC, event_id DESC) = 1)
  WHERE data_type = 'bool'
  GROUP BY session_id),
req AS (
  SELECT trace_id, span_id, ANY_VALUE(JSON_VALUE(attributes, '$.model')) AS model
  FROM `p.d.t`
  WHERE event_type = 'LLM_REQUEST' AND timestamp >= @start AND timestamp < @end
  GROUP BY trace_id, span_id),
c AS (
  SELECT res.session_id AS session_id,
    COALESCE(NULLIF(req.model, ''), NULLIF(JSON_VALUE(res.attributes, '$.model_version'), ''), '(unknown)') AS model,
    JSON_VALUE(res.attributes, '$.prompt.name') AS prompt_name,
    SAFE_CAST(JSON_VALUE(res.attributes, '$.prompt.version') AS INT64) AS prompt_version,
    IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.prompt_token_count') AS INT64), 0)
      + IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.tool_use_prompt_token_count') AS INT64), 0) AS input,
    IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.cached_content_token_count') AS INT64), 0) AS cached,
    IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.candidates_token_count') AS INT64), 0) AS output,
    IFNULL(SAFE_CAST(JSON_VALUE(res.attributes, '$.usage_metadata.thoughts_token_count') AS INT64), 0) AS reasoning,
    SAFE_CAST(JSON_VALUE(res.latency_ms, '$.total_ms') AS FLOAT64) AS total_ms
  FROM `p.d.t` AS res
  LEFT JOIN req ON req.trace_id = res.trace_id AND req.span_id = res.span_id
  WHERE res.event_type = 'LLM_RESPONSE' AND res.timestamp >= @start AND res.timestamp < @end
    AND res.session_id IS NOT NULL AND res.session_id != ''),
tok AS (
  SELECT session_id, model, SUM(input) AS input, SUM(cached) AS cached, SUM(output) AS output,
    SUM(reasoning) AS reasoning
  FROM c
  GROUP BY session_id, model),
su AS (
  SELECT act.session_id AS session_id, IFNULL(head.h.user_id, '') AS user_id,
    act.last AS last, act.turns AS turns, act.errors AS errors, IFNULL(fb.ups, 0) AS ups,
    IFNULL(fb.downs, 0) AS downs
  FROM act
  LEFT JOIN head ON head.session_id = act.session_id
  LEFT JOIN fb ON fb.session_id = act.session_id
  WHERE (@agent IS NULL OR head.h.agent = @agent)),
u AS (
  SELECT user_id, COUNT(DISTINCT session_id) AS sessions, SUM(turns) AS turns,
    SUM(errors) AS errors, SUM(ups) AS ups, SUM(downs) AS downs, MAX(last) AS last
  FROM su
  GROUP BY user_id),
page AS (
  SELECT * FROM u
  ORDER BY last DESC, user_id
  LIMIT @limit),
ut AS (
  SELECT su.user_id AS user_id, tok.model AS model, SUM(tok.input) AS input,
    SUM(tok.cached) AS cached, SUM(tok.output) AS output, SUM(tok.reasoning) AS reasoning
  FROM su
  JOIN tok ON tok.session_id = su.session_id
  GROUP BY 1, 2)
SELECT page.user_id AS user_id, page.sessions AS sessions, page.turns AS turns,
  page.errors AS errors, page.ups AS ups, page.downs AS downs, page.last AS last,
  ut.model AS model, ut.input AS input, ut.cached AS cached, ut.output AS output,
  ut.reasoning AS reasoning
FROM page
LEFT JOIN ut ON ut.user_id = page.user_id
ORDER BY last DESC, user_id, model";

    fn end() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 16, 12, 0, 0).unwrap()
    }

    /// `end()` minus `secs` seconds and 250 microseconds.
    fn ago(secs: i64) -> DateTime<Utc> {
        end() - TimeDelta::seconds(secs) - TimeDelta::microseconds(250)
    }

    fn source(fake: &Arc<FakeHttp>) -> UsageSource {
        UsageSource::with_client(
            BigQueryClient::with_client(fake.clone(), Arc::new(NoAuth)),
            "billing-proj",
            BigQueryTable::new("data-proj", "agent_analytics", "agent_events"),
            1_000,
        )
    }

    fn query() -> SessionQuery {
        SessionQuery {
            range: TimeRange::Hours24,
            end: end(),
            user: None,
            agent: None,
            session: None,
            after: None,
        }
    }

    fn usage(input: u64, cached: u64, output: u64, reasoning: u64) -> Usage {
        Usage {
            input,
            cached,
            output,
            reasoning,
        }
    }

    #[test]
    fn session_ids_are_short_and_free_of_control_characters() {
        assert!(is_session_id("s-1"));
        assert!(is_session_id("Mixed Case/é"));
        assert!(is_session_id(&"x".repeat(128)));
        assert!(!is_session_id(""));
        assert!(!is_session_id(&"x".repeat(129)));
        assert!(!is_session_id("a\nb"));
        assert!(!is_session_id("a\u{7f}b"));
    }

    #[test]
    fn cursors_take_an_rfc3339_time_and_a_session_id_as_given() {
        assert_eq!(
            SessionCursor::parse("2026-11-16T11:58:59.999750Z", "Sess-AB"),
            Ok(SessionCursor {
                at: ago(60),
                id: "Sess-AB".into(),
            })
        );
        for (after, after_id) in [
            ("", "s-1"),
            ("2026-11-16", "s-1"),
            ("2026-11-16T11:00:00Z", ""),
            ("2026-11-16T11:00:00Z", "a\tb"),
        ] {
            assert_eq!(
                SessionCursor::parse(after, after_id),
                Err("invalid cursor"),
                "{after} {after_id}"
            );
        }
    }

    #[test]
    fn the_sessions_sql_is_byte_identical() {
        assert_eq!(session_rows_sql("`p.d.t`", false), SESSIONS_SQL);
        assert_eq!(
            session_rows_sql("`p.d.t`", true),
            SESSIONS_SQL.replace(
                "  SELECT * FROM s\n",
                "  SELECT * FROM s\n  WHERE (s.last < @after OR (s.last = @after AND s.session_id < @after_id))\n"
            )
        );
    }

    #[test]
    fn the_users_sql_is_byte_identical() {
        assert_eq!(user_rows_sql("`p.d.t`"), USERS_SQL);
    }

    #[tokio::test]
    async fn sessions_fold_model_rows_and_send_filters_as_parameters() {
        let fake = FakeHttp::new(vec![reply(
            &SESSION_FIELDS,
            vec![
                session_cells(
                    "s-a",
                    "u-1",
                    "triage",
                    ago(600),
                    ago(60),
                    [3, 1, 2, 1],
                    Some(("gemini-3.8-flash", [1000, 200, 300, 50])),
                ),
                session_cells(
                    "s-a",
                    "u-1",
                    "triage",
                    ago(600),
                    ago(60),
                    [3, 1, 2, 1],
                    Some(("other-model", [10, 0, 5, 0])),
                ),
                session_cells("s-b", "", "", ago(900), ago(120), [1, 0, 0, 0], None),
            ],
            false,
        )]);
        let q = SessionQuery {
            user: Some("o'brien <b>".into()),
            agent: Some("triage".into()),
            session: Some("s-a".into()),
            ..query()
        };
        let page = source(&fake).sessions(&q).await.unwrap();
        assert_eq!(
            page,
            SessionPage {
                sessions: vec![
                    SessionRow {
                        session_id: "s-a".into(),
                        user: "u-1".into(),
                        agent: "triage".into(),
                        first: ago(600),
                        last: ago(60),
                        turns: 3,
                        errors: 1,
                        ups: 2,
                        downs: 1,
                        tokens: vec![
                            ("gemini-3.8-flash".into(), usage(1000, 200, 300, 50)),
                            ("other-model".into(), usage(10, 0, 5, 0)),
                        ],
                    },
                    SessionRow {
                        session_id: "s-b".into(),
                        user: String::new(),
                        agent: String::new(),
                        first: ago(900),
                        last: ago(120),
                        turns: 1,
                        errors: 0,
                        ups: 0,
                        downs: 0,
                        tokens: Vec::new(),
                    },
                ],
                has_next: false,
                truncated: false,
            }
        );
        let body = fake.body(0);
        assert_eq!(body["maxResults"], json!(5000));
        assert_eq!(body["maximumBytesBilled"], json!("1000"));
        let sql = body["query"].as_str().unwrap();
        assert_eq!(
            sql,
            session_rows_sql("`data-proj.agent_analytics.agent_events`", false)
        );
        for value in ["o'brien", "<b>", "triage", "s-a"] {
            assert!(!sql.contains(value), "{value} is a parameter: {sql}");
        }
        for (name, value) in [
            ("start", "2026-11-15 12:00:00.000000+00:00"),
            ("end", "2026-11-16 12:00:00.000000+00:00"),
            ("user", "o'brien <b>"),
            ("agent", "triage"),
            ("session", "s-a"),
            ("limit", "101"),
        ] {
            assert_eq!(
                param_value(&body, name)["parameterValue"]["value"],
                json!(value),
                "{name}"
            );
        }
    }

    #[tokio::test]
    async fn unset_filters_are_typed_nulls_and_the_cursor_is_sent_as_given() {
        let fake = FakeHttp::new(vec![reply(&SESSION_FIELDS, vec![], false)]);
        let q = SessionQuery {
            after: Some(SessionCursor::parse("2026-11-16T11:58:59.999750Z", "Sess-AB").unwrap()),
            ..query()
        };
        let page = source(&fake).sessions(&q).await.unwrap();
        assert!(page.sessions.is_empty() && !page.has_next && !page.truncated);
        let body = fake.body(0);
        for name in ["user", "agent", "session"] {
            assert_eq!(
                param_value(&body, name),
                json!({ "name": name, "parameterType": { "type": "STRING" }, "parameterValue": {} })
            );
        }
        assert_eq!(
            param_value(&body, "after")["parameterValue"]["value"],
            json!("2026-11-16 11:58:59.999750+00:00")
        );
        assert_eq!(
            param_value(&body, "after_id")["parameterValue"]["value"],
            json!("Sess-AB")
        );
        assert!(
            body["query"].as_str().unwrap().contains(
                "WHERE (s.last < @after OR (s.last = @after AND s.session_id < @after_id))"
            )
        );
    }

    #[tokio::test]
    async fn a_101st_session_marks_a_next_page_and_more_rows_mark_truncation() {
        let mut rows = vec![session_cells(
            "s-000",
            "u",
            "a",
            ago(10),
            ago(0),
            [1, 0, 0, 0],
            Some(("m1", [1, 0, 1, 0])),
        )];
        rows.extend((0..101).map(|i| {
            session_cells(
                &format!("s-{i:03}"),
                "u",
                "a",
                ago(10 + i),
                ago(i),
                [1, 0, 0, 0],
                Some(("m2", [1, 0, 1, 0])),
            )
        }));
        let fake = FakeHttp::new(vec![
            reply(&SESSION_FIELDS, rows.clone(), false),
            reply(&SESSION_FIELDS, rows[..101].to_vec(), true),
        ]);
        let src = source(&fake);
        let page = src.sessions(&query()).await.unwrap();
        assert_eq!(page.sessions.len(), 100);
        assert!(page.has_next && !page.truncated);
        assert_eq!(page.sessions[0].tokens.len(), 2);
        assert_eq!(page.sessions[99].session_id, "s-099");

        let page = src.sessions(&query()).await.unwrap();
        assert_eq!(page.sessions.len(), 100);
        assert!(!page.has_next && page.truncated);
    }

    #[tokio::test]
    async fn users_fold_model_rows_and_read_a_null_user_as_empty() {
        let fake = FakeHttp::new(vec![reply(
            &USER_FIELDS,
            vec![
                user_cells(
                    Some("u-1"),
                    [2, 5, 1, 3, 0],
                    ago(60),
                    Some(("gemini-3.8-flash", [1000, 0, 100, 0])),
                ),
                user_cells(
                    Some("u-1"),
                    [2, 5, 1, 3, 0],
                    ago(60),
                    Some(("other-model", [10, 0, 1, 0])),
                ),
                user_cells(None, [1, 1, 0, 0, 1], ago(120), None),
            ],
            false,
        )]);
        let q = UserQuery {
            range: TimeRange::Days7,
            end: end(),
            agent: Some("triage".into()),
        };
        let page = source(&fake).users(&q).await.unwrap();
        assert_eq!(
            page,
            UserPage {
                users: vec![
                    UserRow {
                        user: "u-1".into(),
                        sessions: 2,
                        turns: 5,
                        errors: 1,
                        ups: 3,
                        downs: 0,
                        last: ago(60),
                        tokens: vec![
                            ("gemini-3.8-flash".into(), usage(1000, 0, 100, 0)),
                            ("other-model".into(), usage(10, 0, 1, 0)),
                        ],
                    },
                    UserRow {
                        user: String::new(),
                        sessions: 1,
                        turns: 1,
                        errors: 0,
                        ups: 0,
                        downs: 1,
                        last: ago(120),
                        tokens: Vec::new(),
                    },
                ],
                more: false,
                truncated: false,
            }
        );
        let body = fake.body(0);
        assert_eq!(body["maxResults"], json!(5000));
        let sql = body["query"].as_str().unwrap();
        assert_eq!(
            sql,
            user_rows_sql("`data-proj.agent_analytics.agent_events`")
        );
        assert!(!sql.contains("triage"), "{sql}");
        for (name, value) in [
            ("start", "2026-11-09 12:00:00.000000+00:00"),
            ("end", "2026-11-16 12:00:00.000000+00:00"),
            ("agent", "triage"),
            ("limit", "101"),
        ] {
            assert_eq!(
                param_value(&body, name)["parameterValue"]["value"],
                json!(value),
                "{name}"
            );
        }
    }

    #[tokio::test]
    async fn a_101st_user_marks_more_users() {
        let rows = (0..101)
            .map(|i| user_cells(Some(&format!("u-{i:03}")), [1, 1, 0, 0, 0], ago(i), None))
            .collect();
        let fake = FakeHttp::new(vec![reply(&USER_FIELDS, rows, true)]);
        let q = UserQuery {
            range: TimeRange::Days7,
            end: end(),
            agent: None,
        };
        let page = source(&fake).users(&q).await.unwrap();
        assert_eq!(page.users.len(), 100);
        assert!(page.more && page.truncated);
        assert_eq!(
            param_value(&fake.body(0), "agent")["parameterValue"],
            json!({})
        );
    }

    #[tokio::test]
    async fn a_failed_query_is_the_source_error() {
        let fake = FakeHttp::new(vec![Ok((
            500,
            json!({ "error": { "code": 500, "message": "backend error", "errors": [{ "reason": "backendError" }] } }).to_string(),
        ))]);
        let err = source(&fake).sessions(&query()).await.unwrap_err();
        assert_eq!(
            err,
            SourceError::BigQuery {
                status: Some(500),
                reason: Some("backendError".into()),
                detail: "backend error".into()
            }
        );
    }
}
