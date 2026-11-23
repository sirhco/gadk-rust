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

//! The review queue: the recent traces that have no console score of a given name yet.
//!
//! A queue is defined by URL parameters ([`ReviewQueue`]). The pending query reads the
//! agent-analytics table: one parameterized statement for every filter combination (unset
//! filters are typed NULL parameters), run under the byte cap, every read filtering
//! `timestamp`.

use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Map, Value};

use crate::traces::{SourceError, TimeRange, TraceId, param, text, timestamp, ts_param};
use crate::url::percent_encode;
use crate::usage::{UsageSource, count, window_params};

/// The most pending traces `/review` lists.
pub(crate) const REVIEW_CAP: usize = 100;

/// The note shown by `/review` and `/review/next` when the trace source is not BigQuery.
pub(crate) const NEEDS_BIGQUERY_REVIEW: &str = "Review needs the BigQuery trace source: start the console with --traces bigquery://<project>.<dataset>.<table>.";

/// The longest score name, in bytes after trimming (the trace page's score-name input is
/// `maxlength="100"`).
const MAX_SCORE_LEN: usize = 100;

/// A review queue: the traces of the last `range` with an LLM response and no console score
/// named `score`, optionally only those that called `prompt` (at `version`) or that carry an
/// app thumbs-down.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ReviewQueue {
    /// The score name, trimmed (1-100 bytes).
    pub(crate) score: String,
    /// Only traces whose LLM requests used this prompt.
    pub(crate) prompt: Option<String>,
    /// Only that version of `prompt` (never set without `prompt`).
    pub(crate) version: Option<u32>,
    /// Only traces with an app thumbs-down.
    pub(crate) flagged: bool,
    /// The window, ending now.
    pub(crate) range: TimeRange,
}

impl ReviewQueue {
    /// The queue of the URL parameters `score`, `prompt`, `version`, `flagged` and `range`
    /// (the first value of each counts; a blank `prompt`, `version` or `range` is absent).
    ///
    /// `Ok(None)` when `score` is absent or blank, before any other parameter is checked.
    ///
    /// # Errors
    ///
    /// The text of the 400 page: `score: must be at most 100 bytes`, `version: must be a
    /// positive integer`, `version: needs a prompt`, `range: must be 1h, 6h, 24h or 7d`.
    pub(crate) fn from_params(params: &[(String, String)]) -> Result<Option<ReviewQueue>, String> {
        let first = |key: &str| {
            params
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
        };
        let present = |key: &str| first(key).filter(|v| !v.trim().is_empty());
        let Some(score) = present("score").map(str::trim) else {
            return Ok(None);
        };
        if score.len() > MAX_SCORE_LEN {
            return Err(format!("score: must be at most {MAX_SCORE_LEN} bytes"));
        }
        let prompt = present("prompt").map(str::to_owned);
        let version = match present("version") {
            None => None,
            Some(v) => match v.trim().parse::<u32>() {
                Ok(n) if n > 0 => Some(n),
                _ => return Err("version: must be a positive integer".to_owned()),
            },
        };
        if version.is_some() && prompt.is_none() {
            return Err("version: needs a prompt".to_owned());
        }
        let range = match present("range") {
            None => TimeRange::Days7,
            Some(r) => {
                TimeRange::parse(r).ok_or_else(|| "range: must be 1h, 6h, 24h or 7d".to_owned())?
            }
        };
        Ok(Some(ReviewQueue {
            score: score.to_owned(),
            prompt,
            version,
            flagged: first("flagged") == Some("1"),
            range,
        }))
    }

    /// The queue as URL parameters, in the order `score`, `prompt`, `version`, `flagged`,
    /// `range`, each value percent-encoded. Absent filters, `flagged` off and the default
    /// range (7d) are left out.
    pub(crate) fn query_string(&self) -> String {
        let mut out = format!("score={}", percent_encode(&self.score));
        if let Some(prompt) = &self.prompt {
            out.push_str(&format!("&prompt={}", percent_encode(prompt)));
        }
        if let Some(version) = self.version {
            out.push_str(&format!("&version={version}"));
        }
        if self.flagged {
            out.push_str("&flagged=1");
        }
        if self.range != TimeRange::Days7 {
            out.push_str(&format!("&range={}", self.range.as_str()));
        }
        out
    }
}

/// A position in a queue: the sort key `(first, trace_id)` of the trace last shown. The next
/// trace is the one with the largest sort key strictly below it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Cursor {
    /// The trace's first LLM response.
    pub(crate) at: DateTime<Utc>,
    /// The trace id, lowercase.
    pub(crate) trace_id: String,
}

impl Cursor {
    /// The cursor of the URL values `at` (RFC 3339) and `trace_id` (32 hex digits, either
    /// case).
    ///
    /// # Errors
    ///
    /// The text of the 400 page, naming the `/review/next` parameter: `after: must be an RFC
    /// 3339 time` or `after_id: must be 32 hex digits`.
    pub(crate) fn parse(at: &str, trace_id: &str) -> Result<Cursor, String> {
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
pub(crate) fn at_text(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Micros, true)
}

/// A trace waiting for review.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PendingTrace {
    /// The trace id, lowercase.
    pub(crate) trace_id: String,
    /// The trace's earliest LLM response in the window.
    pub(crate) first: DateTime<Utc>,
    /// The trace's prompt versions as `name@version`, sorted.
    pub(crate) prompts: Vec<String>,
    /// The distinct app thumbs-down scores on the trace.
    pub(crate) thumbs_down: u64,
}

impl UsageSource {
    /// The pending traces of `q` in `[end - q.range, end)`, newest first: at most
    /// [`REVIEW_CAP`] + 1 rows, so the caller can tell whether the cap was hit.
    pub(crate) async fn pending(
        &self,
        q: &ReviewQueue,
        end: DateTime<Utc>,
    ) -> Result<Vec<PendingTrace>, SourceError> {
        let sql = pending_sql(&self.table_path()?, false);
        let params = pending_params(q, end, None, REVIEW_CAP + 1);
        let result = self.run(&sql, params, (REVIEW_CAP + 1) as u32).await?;
        result.rows.iter().map(pending_row).collect()
    }

    /// The pending trace of `q` in `[end - q.range, end)` that comes right after `after`
    /// (the largest sort key strictly below it); `None` when the queue has no more.
    pub(crate) async fn next_pending(
        &self,
        q: &ReviewQueue,
        end: DateTime<Utc>,
        after: &Cursor,
    ) -> Result<Option<PendingTrace>, SourceError> {
        let sql = pending_sql(&self.table_path()?, true);
        let params = pending_params(q, end, Some(after), 1);
        let result = self.run(&sql, params, 1).await?;
        result.rows.first().map(pending_row).transpose()
    }
}

/// The parameters of [`pending_sql`]: the window, the queue (unset filters are typed NULLs),
/// `@limit` and, with a cursor, `@after` and `@after_id`.
fn pending_params(
    q: &ReviewQueue,
    end: DateTime<Utc>,
    after: Option<&Cursor>,
    limit: usize,
) -> Vec<Value> {
    let mut params = window_params(end - q.range.duration(), end);
    params.push(param("score", "STRING", Some(q.score.clone())));
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
    let trace_id = text(row, "trace_id").ok_or_else(|| SourceError::BigQuery {
        status: None,
        reason: None,
        detail: "row without a valid trace_id".to_owned(),
    })?;
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
/// - `agg`: per trace, whether a console score named `@score` exists and how many distinct app
///   thumbs-down scores there are.
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
  SELECT tid, LOGICAL_OR(source = 'console' AND name = @score) AS scored,
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

/// The columns of [`pending_sql`] (`prompts` is `ARRAY<STRING>`).
#[cfg(test)]
pub(crate) const PENDING_FIELDS: [(&str, &str); 4] = [
    ("trace_id", "STRING"),
    ("first", "TIMESTAMP"),
    ("prompts", "STRING"),
    ("thumbs_down", "INT64"),
];

/// A `jobs.query` reply of [`pending_sql`] rows (see [`pending_cells`]), with `prompts`
/// `REPEATED`.
#[cfg(test)]
pub(crate) fn pending_reply(rows: Vec<Vec<Value>>) -> Result<(u16, String), String> {
    let (status, body) = crate::traces::test_http::reply(&PENDING_FIELDS, rows, false)?;
    let mut body: Value = serde_json::from_str(&body).unwrap();
    body["schema"]["fields"][2]["mode"] = Value::from("REPEATED");
    Ok((status, body.to_string()))
}

/// The cells of one pending row; no `prompts` is a NULL array.
#[cfg(test)]
pub(crate) fn pending_cells(
    trace_id: &str,
    first: DateTime<Utc>,
    prompts: &[&str],
    thumbs_down: u64,
) -> Vec<Value> {
    let prompts = if prompts.is_empty() {
        Value::Null
    } else {
        Value::Array(
            prompts
                .iter()
                .map(|p| serde_json::json!({ "v": p }))
                .collect(),
        )
    };
    vec![
        Value::from(trace_id),
        Value::from(first.timestamp_micros().to_string()),
        prompts,
        Value::from(thumbs_down.to_string()),
    ]
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::{TimeZone, Utc};
    use gadk_bigquery::{BigQueryClient, NoAuth};
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;
    use crate::traces::BigQueryTable;
    use crate::traces::test_http::{FakeHttp, param_value};

    const TRACE: &str = "0123456789abcdef0123456789abcdef";

    fn params(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    /// The pairs of a query string, percent-decoded.
    fn decode(qs: &str) -> Vec<(String, String)> {
        let unescape = |s: &str| {
            let b = s.as_bytes();
            let mut out = Vec::new();
            let mut i = 0;
            while i < b.len() {
                if b[i] == b'%' {
                    out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
                    i += 3;
                } else {
                    out.push(b[i]);
                    i += 1;
                }
            }
            String::from_utf8(out).unwrap()
        };
        qs.split('&')
            .map(|pair| {
                let (k, v) = pair.split_once('=').unwrap();
                (unescape(k), unescape(v))
            })
            .collect()
    }

    fn queue(pairs: &[(&str, &str)]) -> ReviewQueue {
        ReviewQueue::from_params(&params(pairs)).unwrap().unwrap()
    }

    fn source(fake: &Arc<FakeHttp>) -> UsageSource {
        UsageSource::with_client(
            BigQueryClient::with_client(fake.clone(), Arc::new(NoAuth)),
            "billing-proj",
            BigQueryTable::new("data-proj", "agent_analytics", "agent_events"),
            1_000,
        )
    }

    fn end() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 23, 12, 0, 0).unwrap()
    }

    #[test]
    fn a_queue_takes_its_filters_and_defaults_to_seven_days() {
        assert_eq!(
            queue(&[("score", "  quality ")]),
            ReviewQueue {
                score: "quality".into(),
                prompt: None,
                version: None,
                flagged: false,
                range: TimeRange::Days7,
            }
        );
        assert_eq!(
            queue(&[
                ("range", "24h"),
                ("flagged", "1"),
                ("version", "3"),
                ("prompt", "triage"),
                ("score", "quality"),
                ("score", "ignored"),
            ]),
            ReviewQueue {
                score: "quality".into(),
                prompt: Some("triage".into()),
                version: Some(3),
                flagged: true,
                range: TimeRange::Hours24,
            }
        );
        // Blank optional values are absent; any `flagged` other than 1 is off.
        assert_eq!(
            queue(&[
                ("score", "q"),
                ("prompt", " "),
                ("version", ""),
                ("range", ""),
                ("flagged", "yes"),
            ]),
            queue(&[("score", "q")])
        );
        assert_eq!(queue(&[("score", &"x".repeat(100))]).score.len(), 100);
    }

    #[test]
    fn no_score_is_no_queue_before_anything_else_is_checked() {
        for pairs in [
            &[][..],
            &[("score", "")][..],
            &[("score", "  "), ("version", "x"), ("range", "1y")][..],
        ] {
            assert_eq!(
                ReviewQueue::from_params(&params(pairs)),
                Ok(None),
                "{pairs:?}"
            );
        }
    }

    #[test]
    fn invalid_values_are_errors() {
        let err = |pairs: &[(&str, &str)]| ReviewQueue::from_params(&params(pairs)).unwrap_err();
        assert_eq!(
            err(&[("score", "q"), ("version", "2")]),
            "version: needs a prompt"
        );
        for v in ["0", "x", "-1", "1.5"] {
            assert_eq!(
                err(&[("score", "q"), ("prompt", "p"), ("version", v)]),
                "version: must be a positive integer",
                "{v}"
            );
        }
        assert_eq!(
            err(&[("score", "q"), ("range", "30d")]),
            "range: must be 1h, 6h, 24h or 7d"
        );
        for len in [101, 129] {
            assert_eq!(
                err(&[("score", &"x".repeat(len))]),
                "score: must be at most 100 bytes",
                "{len}"
            );
        }
    }

    #[test]
    fn the_query_string_is_ordered_encoded_and_round_trips() {
        assert_eq!(queue(&[("score", "q")]).query_string(), "score=q");
        let q = queue(&[
            ("range", "1h"),
            ("flagged", "1"),
            ("version", "12"),
            ("prompt", "team/greet"),
            ("score", "a&b#c dé"),
        ]);
        let qs = q.query_string();
        assert_eq!(
            qs,
            "score=a%26b%23c%20d%C3%A9&prompt=team%2Fgreet&version=12&flagged=1&range=1h"
        );
        assert_eq!(ReviewQueue::from_params(&decode(&qs)), Ok(Some(q)));
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
  SELECT tid, LOGICAL_OR(source = 'console' AND name = @score) AS scored,
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
            .pending(&queue(&[("score", "quality")]), end())
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
            ("score", "STRING", json!({ "value": "quality" })),
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
        let q = queue(&[
            ("score", "<script>\"q\"&"),
            ("prompt", "p' OR 1=1 --"),
            ("version", "7"),
            ("flagged", "1"),
            ("range", "1h"),
        ]);
        assert!(source(&fake).pending(&q, end()).await.unwrap().is_empty());
        assert_eq!(
            value_of(&fake, 0, "score"),
            json!({ "value": "<script>\"q\"&" })
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
        let q = queue(&[("score", "quality")]);
        let next = src.next_pending(&q, end(), &after).await.unwrap();
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
        assert_eq!(src.next_pending(&q, end(), &after).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_failed_pending_query_is_the_source_error() {
        let fake = FakeHttp::new(vec![Ok((
            403,
            json!({ "error": { "code": 403, "message": "Access Denied", "errors": [{ "reason": "accessDenied" }] } }).to_string(),
        ))]);
        let err = source(&fake)
            .pending(&queue(&[("score", "q")]), end())
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "BigQuery: HTTP 403: Access Denied");
    }
}
