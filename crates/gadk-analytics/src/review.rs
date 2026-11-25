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

//! The review queue: the recent traces that have no console score yet for every one of a set of
//! score names ([`AnalyticsSource::pending`], [`AnalyticsSource::next_pending`]).
//!
//! A queue is a window and optional filters ([`ReviewQueue`]); the score names are a score, or
//! the criteria of a rubric. The pending query reads the agent-analytics table: one
//! parameterized statement for every filter combination (unset filters are typed NULL
//! parameters), run under the byte cap, every read filtering `timestamp`.
//!
//! ```
//! use std::sync::Arc;
//!
//! use chrono::{TimeZone, Utc};
//! use gadk_analytics::review::ReviewQueue;
//! use gadk_analytics::testutil::{FakeHttp, pending_cells, pending_reply};
//! use gadk_analytics::{AnalyticsSource, BigQueryTable, TimeRange};
//! use gadk_bigquery::{BigQueryClient, NoAuth};
//!
//! # tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
//! let end = Utc.with_ymd_and_hms(2026, 11, 23, 12, 0, 0).unwrap();
//! let trace = "0123456789abcdef0123456789abcdef";
//! // One scripted `jobs.query` reply: one trace still waiting for a `quality` score.
//! let http = FakeHttp::new(vec![pending_reply(vec![pending_cells(
//!     trace,
//!     end - chrono::TimeDelta::hours(2),
//!     &["triage@2"],
//!     0,
//! )])]);
//! let source = AnalyticsSource::new(
//!     BigQueryClient::with_client(http.clone(), Arc::new(NoAuth)),
//!     "billing-proj",
//!     BigQueryTable::new("data-proj", "agent_analytics", "agent_events"),
//!     1_073_741_824,
//! );
//! let q = ReviewQueue::new(TimeRange::Days7).with_prompt("triage");
//! let pending = source.pending(&q, &["quality".to_owned()], end).await?;
//! assert_eq!(pending[0].trace_id, trace);
//! assert_eq!(pending[0].prompts, ["triage@2"]);
//! # Ok::<(), gadk_analytics::SourceError>(())
//! # }).unwrap();
//! ```

use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::row::{param, text, timestamp, ts_param};
use crate::source::{count, string_array_param, window_params};
use crate::{AnalyticsSource, SourceError, TimeRange, TraceId};

/// The most pending traces `/review` lists.
pub const REVIEW_CAP: usize = 100;

/// A review queue's filters: the traces of the last `range` with an LLM response, optionally
/// only those that called `prompt` (at `version`) or that carry an app thumbs-down. The score
/// names the traces wait for are passed to the queries on their own.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ReviewQueue {
    /// Only traces whose LLM requests used this prompt.
    pub prompt: Option<String>,
    /// Only that version of `prompt`.
    pub version: Option<u32>,
    /// Only traces with an app thumbs-down.
    pub flagged: bool,
    /// The window, ending at the queries' `end`.
    pub range: TimeRange,
}

impl ReviewQueue {
    /// Every trace of the last `range`, unfiltered.
    pub fn new(range: TimeRange) -> Self {
        Self {
            prompt: None,
            version: None,
            flagged: false,
            range,
        }
    }

    /// Only traces whose LLM requests used `prompt`.
    pub fn with_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.prompt = Some(prompt.into());
        self
    }

    /// Only traces that used `version` of the prompt (set with [`with_prompt`](Self::with_prompt)).
    /// Without a prompt the version is ignored.
    pub fn with_version(mut self, version: u32) -> Self {
        self.version = Some(version);
        self
    }

    /// Only traces with an app thumbs-down (`true`), or every trace (`false`).
    pub fn with_flagged(mut self, flagged: bool) -> Self {
        self.flagged = flagged;
        self
    }
}

/// A position in a queue: the sort key `(first, trace_id)` of the trace last shown. The next
/// trace is the one with the largest sort key strictly below it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Cursor {
    /// The trace's first LLM response.
    pub at: DateTime<Utc>,
    /// The trace id, lowercase.
    pub trace_id: String,
}

impl Cursor {
    /// The cursor of the trace `trace_id` whose first LLM response is `at`. The id is saved
    /// lowercase, as [`parse`](Self::parse) saves it.
    pub fn new(at: DateTime<Utc>, trace_id: String) -> Self {
        Self {
            at,
            trace_id: trace_id.to_ascii_lowercase(),
        }
    }

    /// The cursor of the URL values `at` (RFC 3339) and `trace_id` (32 hex digits, either
    /// case).
    ///
    /// # Errors
    ///
    /// The text of the 400 page, naming the `/review/next` parameter: `after: must be an RFC
    /// 3339 time` or `after_id: must be 32 hex digits`.
    pub fn parse(at: &str, trace_id: &str) -> Result<Cursor, String> {
        let at = DateTime::parse_from_rfc3339(at)
            .map_err(|_| "after: must be an RFC 3339 time".to_owned())?
            .with_timezone(&Utc);
        let trace_id =
            TraceId::parse(trace_id).ok_or_else(|| "after_id: must be 32 hex digits".to_owned())?;
        Ok(Cursor {
            at,
            trace_id: trace_id.as_str().to_owned(),
        })
    }
}

/// `t` as a URL value: RFC 3339 with microseconds and `Z` (BigQuery's precision, so a cursor
/// compares exactly).
pub fn at_text(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Micros, true)
}

/// A trace waiting for review.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct PendingTrace {
    /// The trace id, lowercase.
    pub trace_id: String,
    /// The trace's earliest LLM response in the window.
    pub first: DateTime<Utc>,
    /// The trace's prompt versions as `name@version`, sorted.
    pub prompts: Vec<String>,
    /// The distinct app thumbs-down scores on the trace.
    pub thumbs_down: u64,
}

impl PendingTrace {
    /// The pending trace `trace_id` whose first LLM response is `first`, with its prompt
    /// versions `prompts` and `thumbs_down` app thumbs-down scores.
    pub fn new(
        trace_id: String,
        first: DateTime<Utc>,
        prompts: Vec<String>,
        thumbs_down: u64,
    ) -> Self {
        Self {
            trace_id,
            first,
            prompts,
            thumbs_down,
        }
    }
}

impl AnalyticsSource {
    /// The pending traces of `q` in `[end - q.range, end)`, newest first: those that miss a
    /// console score for some name in `names` (sent as `@names`). At most [`REVIEW_CAP`] + 1
    /// rows, so the caller can tell whether the cap was hit. An empty `names` slice returns no
    /// traces (and runs no query).
    pub async fn pending(
        &self,
        q: &ReviewQueue,
        names: &[String],
        end: DateTime<Utc>,
    ) -> Result<Vec<PendingTrace>, SourceError> {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        let sql = pending_sql(&self.table_path()?, false);
        let params = pending_params(q, names, end, None, REVIEW_CAP + 1);
        let result = self.run(&sql, params, (REVIEW_CAP + 1) as u32).await?;
        result.rows.iter().map(pending_row).collect()
    }

    /// The pending trace of `q` and `names` (see [`pending`](Self::pending)) in
    /// `[end - q.range, end)` that comes right after `after` (the largest sort key strictly
    /// below it); `None` when the queue has no more. An empty `names` slice returns no trace
    /// (and runs no query).
    pub async fn next_pending(
        &self,
        q: &ReviewQueue,
        names: &[String],
        end: DateTime<Utc>,
        after: &Cursor,
    ) -> Result<Option<PendingTrace>, SourceError> {
        if names.is_empty() {
            return Ok(None);
        }
        let sql = pending_sql(&self.table_path()?, true);
        let params = pending_params(q, names, end, Some(after), 1);
        let result = self.run(&sql, params, 1).await?;
        result.rows.first().map(pending_row).transpose()
    }
}

/// The parameters of [`pending_sql`]: the window, the score `names` as `@names`, the queue's
/// filters (unset filters are typed NULLs), `@limit` and, with a cursor, `@after` and
/// `@after_id`.
fn pending_params(
    q: &ReviewQueue,
    names: &[String],
    end: DateTime<Utc>,
    after: Option<&Cursor>,
    limit: usize,
) -> Vec<Value> {
    let mut params = window_params(end - q.range.duration(), end);
    params.push(string_array_param("names", names));
    params.push(param("prompt", "STRING", q.prompt.clone()));
    params.push(param("version", "INT64", q.version.map(|v| v.to_string())));
    params.push(param("flagged", "BOOL", Some(q.flagged.to_string())));
    params.push(param("limit", "INT64", Some(limit.to_string())));
    if let Some(c) = after {
        params.push(param("after", "TIMESTAMP", Some(ts_param(c.at))));
        params.push(param("after_id", "STRING", Some(c.trace_id.clone())));
    }
    params
}

/// A row of [`pending_sql`]; a NULL `prompts` array is empty.
fn pending_row(row: &Map<String, Value>) -> Result<PendingTrace, SourceError> {
    let trace_id = text(row, "trace_id")
        .ok_or_else(|| SourceError::bigquery(None, None, "row without a valid trace_id"))?;
    let prompts = row
        .get("prompts")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    Ok(PendingTrace {
        trace_id,
        first: timestamp(row, "first")?,
        prompts,
        thumbs_down: count(row, "thumbs_down"),
    })
}

/// The pending traces of a queue, newest first (`first DESC, trace_id DESC`), at most
/// `@limit`. `with_cursor` keeps only sort keys strictly below `(@after, @after_id)`.
///
/// - `t`: per lowercase trace id, the earliest `LLM_RESPONSE` in `[@start, @end)`.
/// - `p`: per trace, its `name@version` prompt versions and whether one matches `@prompt`
///   (and `@version` when set).
/// - `inv`: the trace's invocations in the window.
/// - `f`, `fm`: `FEEDBACK` rows since a day before `@start`, mapped to traces by `trace_id`
///   or by invocation.
/// - `agg`: per trace, whether it has a console score for every name in `@names` and how many
///   distinct app thumbs-down scores there are.
fn pending_sql(table: &str, with_cursor: bool) -> String {
    let cursor = if with_cursor {
        "\n  AND (t.first < @after OR (t.first = @after AND t.trace_id < @after_id))"
    } else {
        ""
    };
    format!(
        r"WITH t AS (
  SELECT LOWER(trace_id) AS trace_id, MIN(timestamp) AS first
  FROM {table}
  WHERE event_type = 'LLM_RESPONSE' AND timestamp >= @start AND timestamp < @end
    AND REGEXP_CONTAINS(trace_id, r'^[0-9a-fA-F]{{32}}$')
  GROUP BY 1),
p AS (
  SELECT trace_id,
    ARRAY_AGG(DISTINCT CONCAT(name, '@', CAST(version AS STRING))
      ORDER BY CONCAT(name, '@', CAST(version AS STRING))) AS prompts,
    LOGICAL_OR(name = @prompt AND (@version IS NULL OR version = @version)) AS matches
  FROM (
    SELECT LOWER(trace_id) AS trace_id, JSON_VALUE(attributes, '$.prompt.name') AS name,
      SAFE_CAST(JSON_VALUE(attributes, '$.prompt.version') AS INT64) AS version
    FROM {table}
    WHERE event_type = 'LLM_REQUEST' AND timestamp >= @start AND timestamp < @end
      AND REGEXP_CONTAINS(trace_id, r'^[0-9a-fA-F]{{32}}$'))
  WHERE name IS NOT NULL AND version IS NOT NULL
  GROUP BY 1),
inv AS (
  SELECT DISTINCT LOWER(trace_id) AS trace_id, invocation_id
  FROM {table}
  WHERE event_type != 'FEEDBACK' AND timestamp >= @start AND timestamp < @end
    AND REGEXP_CONTAINS(trace_id, r'^[0-9a-fA-F]{{32}}$')
    AND invocation_id IS NOT NULL AND invocation_id != ''),
f AS (
  SELECT event_id, trace_id, invocation_id, JSON_VALUE(attributes, '$.source') AS source,
    JSON_VALUE(content, '$.name') AS name, JSON_VALUE(content, '$.data_type') AS data_type,
    JSON_VALUE(content, '$.value') AS value
  FROM {table}
  WHERE event_type = 'FEEDBACK'
    AND timestamp >= TIMESTAMP_SUB(@start, INTERVAL 1 DAY)
    AND timestamp <= CURRENT_TIMESTAMP()),
fm AS (
  SELECT LOWER(trace_id) AS tid, event_id, source, name, data_type, value
  FROM f
  WHERE trace_id != ''
  UNION ALL
  SELECT inv.trace_id AS tid, f.event_id, f.source, f.name, f.data_type, f.value
  FROM f JOIN inv USING (invocation_id)),
agg AS (
  SELECT tid,
    COUNT(DISTINCT IF(source = 'console' AND name IN UNNEST(@names), name, NULL))
      = ARRAY_LENGTH(@names) AS scored,
    COUNT(DISTINCT IF(source = 'app' AND data_type = 'bool' AND value = 'false', event_id, NULL))
      AS thumbs_down
  FROM fm
  GROUP BY tid)
SELECT t.trace_id AS trace_id, t.first AS first, p.prompts AS prompts,
  IFNULL(agg.thumbs_down, 0) AS thumbs_down
FROM t LEFT JOIN p USING (trace_id) LEFT JOIN agg ON agg.tid = t.trace_id
WHERE NOT IFNULL(agg.scored, FALSE)
  AND (NOT @flagged OR IFNULL(agg.thumbs_down, 0) > 0)
  AND (@prompt IS NULL OR IFNULL(p.matches, FALSE)){cursor}
ORDER BY first DESC, trace_id DESC
LIMIT @limit"
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::{TimeZone, Utc};
    use gadk_bigquery::{BigQueryClient, NoAuth};
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;
    use crate::BigQueryTable;
    use crate::testutil::{FakeHttp, param_value, pending_cells, pending_reply};

    const TRACE: &str = "0123456789abcdef0123456789abcdef";

    fn source(fake: &Arc<FakeHttp>) -> AnalyticsSource {
        AnalyticsSource::new(
            BigQueryClient::with_client(fake.clone(), Arc::new(NoAuth)),
            "billing-proj",
            BigQueryTable::new("data-proj", "agent_analytics", "agent_events"),
            1_000,
        )
    }

    fn end() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 23, 12, 0, 0).unwrap()
    }

    /// The score names `names`, owned.
    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_owned()).collect()
    }

    #[test]
    fn cursors_take_rfc3339_and_a_lowercased_trace_id() {
        let at = Utc.with_ymd_and_hms(2026, 11, 23, 11, 0, 0).unwrap()
            + chrono::TimeDelta::microseconds(123_456);
        assert_eq!(at_text(at), "2026-11-23T11:00:00.123456Z");
        assert_eq!(
            Cursor::parse("2026-11-23T11:00:00.123456Z", &TRACE.to_uppercase()),
            Ok(Cursor {
                at,
                trace_id: TRACE.into()
            })
        );
        assert_eq!(
            Cursor::parse("yesterday", TRACE),
            Err("after: must be an RFC 3339 time".to_owned())
        );
        assert_eq!(
            Cursor::parse("2026-11-23T11:00:00Z", "abc"),
            Err("after_id: must be 32 hex digits".to_owned())
        );
    }

    const PENDING_SQL: &str = r"WITH t AS (
  SELECT LOWER(trace_id) AS trace_id, MIN(timestamp) AS first
  FROM `p.d.t`
  WHERE event_type = 'LLM_RESPONSE' AND timestamp >= @start AND timestamp < @end
    AND REGEXP_CONTAINS(trace_id, r'^[0-9a-fA-F]{32}$')
  GROUP BY 1),
p AS (
  SELECT trace_id,
    ARRAY_AGG(DISTINCT CONCAT(name, '@', CAST(version AS STRING))
      ORDER BY CONCAT(name, '@', CAST(version AS STRING))) AS prompts,
    LOGICAL_OR(name = @prompt AND (@version IS NULL OR version = @version)) AS matches
  FROM (
    SELECT LOWER(trace_id) AS trace_id, JSON_VALUE(attributes, '$.prompt.name') AS name,
      SAFE_CAST(JSON_VALUE(attributes, '$.prompt.version') AS INT64) AS version
    FROM `p.d.t`
    WHERE event_type = 'LLM_REQUEST' AND timestamp >= @start AND timestamp < @end
      AND REGEXP_CONTAINS(trace_id, r'^[0-9a-fA-F]{32}$'))
  WHERE name IS NOT NULL AND version IS NOT NULL
  GROUP BY 1),
inv AS (
  SELECT DISTINCT LOWER(trace_id) AS trace_id, invocation_id
  FROM `p.d.t`
  WHERE event_type != 'FEEDBACK' AND timestamp >= @start AND timestamp < @end
    AND REGEXP_CONTAINS(trace_id, r'^[0-9a-fA-F]{32}$')
    AND invocation_id IS NOT NULL AND invocation_id != ''),
f AS (
  SELECT event_id, trace_id, invocation_id, JSON_VALUE(attributes, '$.source') AS source,
    JSON_VALUE(content, '$.name') AS name, JSON_VALUE(content, '$.data_type') AS data_type,
    JSON_VALUE(content, '$.value') AS value
  FROM `p.d.t`
  WHERE event_type = 'FEEDBACK'
    AND timestamp >= TIMESTAMP_SUB(@start, INTERVAL 1 DAY)
    AND timestamp <= CURRENT_TIMESTAMP()),
fm AS (
  SELECT LOWER(trace_id) AS tid, event_id, source, name, data_type, value
  FROM f
  WHERE trace_id != ''
  UNION ALL
  SELECT inv.trace_id AS tid, f.event_id, f.source, f.name, f.data_type, f.value
  FROM f JOIN inv USING (invocation_id)),
agg AS (
  SELECT tid,
    COUNT(DISTINCT IF(source = 'console' AND name IN UNNEST(@names), name, NULL))
      = ARRAY_LENGTH(@names) AS scored,
    COUNT(DISTINCT IF(source = 'app' AND data_type = 'bool' AND value = 'false', event_id, NULL))
      AS thumbs_down
  FROM fm
  GROUP BY tid)
SELECT t.trace_id AS trace_id, t.first AS first, p.prompts AS prompts,
  IFNULL(agg.thumbs_down, 0) AS thumbs_down
FROM t LEFT JOIN p USING (trace_id) LEFT JOIN agg ON agg.tid = t.trace_id
WHERE NOT IFNULL(agg.scored, FALSE)
  AND (NOT @flagged OR IFNULL(agg.thumbs_down, 0) > 0)
  AND (@prompt IS NULL OR IFNULL(p.matches, FALSE))";

    #[test]
    fn the_pending_sql_is_byte_identical() {
        let tail = "\nORDER BY first DESC, trace_id DESC\nLIMIT @limit";
        assert_eq!(
            pending_sql("`p.d.t`", false),
            format!("{PENDING_SQL}{tail}")
        );
        assert_eq!(
            pending_sql("`p.d.t`", true),
            format!(
                "{PENDING_SQL}\n  AND (t.first < @after OR (t.first = @after AND t.trace_id < @after_id)){tail}"
            )
        );
    }

    /// The value of parameter `name` of request `i`.
    fn value_of(fake: &FakeHttp, i: usize, name: &str) -> Value {
        param_value(&fake.body(i), name)["parameterValue"].clone()
    }

    #[tokio::test]
    async fn pending_sends_the_window_queue_and_cap_and_reads_the_rows() {
        let first = end() - chrono::TimeDelta::hours(2);
        let fake = FakeHttp::new(vec![pending_reply(vec![
            pending_cells(TRACE, first, &["greet@1", "triage@2"], 2),
            pending_cells(
                &"1".repeat(32),
                first - chrono::TimeDelta::seconds(1),
                &[],
                0,
            ),
        ])]);
        let rows = source(&fake)
            .pending(
                &ReviewQueue::new(TimeRange::Days7),
                &names(&["quality"]),
                end(),
            )
            .await
            .unwrap();
        assert_eq!(
            rows,
            vec![
                PendingTrace {
                    trace_id: TRACE.into(),
                    first,
                    prompts: vec!["greet@1".into(), "triage@2".into()],
                    thumbs_down: 2,
                },
                PendingTrace {
                    trace_id: "1".repeat(32),
                    first: first - chrono::TimeDelta::seconds(1),
                    prompts: vec![],
                    thumbs_down: 0,
                },
            ]
        );
        let body = fake.body(0);
        assert_eq!(
            body["query"],
            json!(pending_sql(
                "`data-proj.agent_analytics.agent_events`",
                false
            ))
        );
        assert_eq!(body["maximumBytesBilled"], json!("1000"));
        assert_eq!(body["maxResults"], json!(101));
        for (name, ty, value) in [
            (
                "start",
                "TIMESTAMP",
                json!({ "value": "2026-11-16 12:00:00.000000+00:00" }),
            ),
            (
                "end",
                "TIMESTAMP",
                json!({ "value": "2026-11-23 12:00:00.000000+00:00" }),
            ),
            (
                "names",
                "ARRAY",
                json!({ "arrayValues": [{ "value": "quality" }] }),
            ),
            ("prompt", "STRING", json!({})),
            ("version", "INT64", json!({})),
            ("flagged", "BOOL", json!({ "value": "false" })),
            ("limit", "INT64", json!({ "value": "101" })),
        ] {
            let p = param_value(&body, name);
            assert_eq!(p["parameterType"]["type"], json!(ty), "{name}");
            assert_eq!(p["parameterValue"], value, "{name}");
        }
        let names: Vec<&str> = body["queryParameters"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["name"].as_str().unwrap())
            .collect();
        assert!(
            !names.contains(&"after") && !names.contains(&"after_id"),
            "{names:?}"
        );
    }

    #[tokio::test]
    async fn filters_are_parameters_and_never_sql() {
        let fake = FakeHttp::new(vec![pending_reply(vec![])]);
        let q = ReviewQueue::new(TimeRange::Hour1)
            .with_prompt("p' OR 1=1 --")
            .with_version(7)
            .with_flagged(true);
        assert!(
            source(&fake)
                .pending(&q, &names(&["<script>\"q\"&"]), end())
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            value_of(&fake, 0, "names"),
            json!({ "arrayValues": [{ "value": "<script>\"q\"&" }] })
        );
        assert_eq!(
            value_of(&fake, 0, "prompt"),
            json!({ "value": "p' OR 1=1 --" })
        );
        assert_eq!(value_of(&fake, 0, "version"), json!({ "value": "7" }));
        assert_eq!(value_of(&fake, 0, "flagged"), json!({ "value": "true" }));
        assert_eq!(
            value_of(&fake, 0, "start"),
            json!({ "value": "2026-11-23 11:00:00.000000+00:00" })
        );
        let body = fake.body(0);
        let sql = body["query"].as_str().unwrap();
        for value in ["<script>", "OR 1=1", "'7'"] {
            assert!(!sql.contains(value), "{value} is a parameter: {sql}");
        }
    }

    #[tokio::test]
    async fn next_pending_sends_the_cursor_and_reads_one_row() {
        let first = end() - chrono::TimeDelta::hours(3);
        let fake = FakeHttp::new(vec![
            pending_reply(vec![pending_cells(TRACE, first, &["triage@2"], 0)]),
            pending_reply(vec![]),
        ]);
        let src = source(&fake);
        let after = Cursor::parse("2026-11-23T10:00:00.000001Z", &"A".repeat(32)).unwrap();
        let q = ReviewQueue::new(TimeRange::Days7);
        let names = names(&["quality"]);
        let next = src.next_pending(&q, &names, end(), &after).await.unwrap();
        assert_eq!(
            next,
            Some(PendingTrace {
                trace_id: TRACE.into(),
                first,
                prompts: vec!["triage@2".into()],
                thumbs_down: 0,
            })
        );
        let body = fake.body(0);
        assert_eq!(
            body["query"],
            json!(pending_sql(
                "`data-proj.agent_analytics.agent_events`",
                true
            ))
        );
        assert_eq!(body["maxResults"], json!(1));
        assert_eq!(value_of(&fake, 0, "limit"), json!({ "value": "1" }));
        assert_eq!(
            value_of(&fake, 0, "after"),
            json!({ "value": "2026-11-23 10:00:00.000001+00:00" })
        );
        assert_eq!(
            value_of(&fake, 0, "after_id"),
            json!({ "value": "a".repeat(32) })
        );
        assert_eq!(
            src.next_pending(&q, &names, end(), &after).await.unwrap(),
            None
        );
    }

    #[test]
    fn a_new_cursor_lowercases_its_trace_id() {
        let at = Utc.with_ymd_and_hms(2026, 11, 23, 11, 0, 0).unwrap();
        assert_eq!(Cursor::new(at, TRACE.to_uppercase()).trace_id, TRACE);
        assert_eq!(
            Cursor::new(at, TRACE.to_uppercase()),
            Cursor::parse("2026-11-23T11:00:00Z", TRACE).unwrap()
        );
    }

    #[tokio::test]
    async fn no_score_names_is_no_pending_traces_and_no_query() {
        let fake = FakeHttp::new(vec![]);
        let src = source(&fake);
        let q = ReviewQueue::new(TimeRange::Days7);
        assert_eq!(src.pending(&q, &[], end()).await.unwrap(), vec![]);
        let after = Cursor::parse("2026-11-23T10:00:00Z", TRACE).unwrap();
        assert_eq!(
            src.next_pending(&q, &[], end(), &after).await.unwrap(),
            None
        );
        assert!(fake.uris().is_empty());
    }

    #[tokio::test]
    async fn a_failed_pending_query_is_the_source_error() {
        let fake = FakeHttp::new(vec![Ok((
            403,
            json!({ "error": { "code": 403, "message": "Access Denied", "errors": [{ "reason": "accessDenied" }] } }).to_string(),
        ))]);
        let err = source(&fake)
            .pending(&ReviewQueue::new(TimeRange::Days7), &names(&["q"]), end())
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "BigQuery: HTTP 403: Access Denied");
    }
}
