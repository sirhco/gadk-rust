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

//! Scores: the `FEEDBACK` rows of the agent-analytics BigQuery table. Apps write them with
//! `gadk-agentanalytics` (`"source": "app"`); the console reads them for `/traces/{id}` and
//! inserts the ones added there (`"source": "console"`).
//!
//! A score row's `content` is `{"name", "value", "data_type", "comment"}` and its `attributes`
//! carry `source` and `author`. Every statement is parameterized and stops at the byte cap
//! (the insert at no less than 10 MiB); every read filters `timestamp`.

use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};

use crate::traces::{SourceError, param, text, timestamp, ts_param};
use crate::usage::{UsageSource, count, string_array_param, version_of, window_params};

/// The most scores `/traces/{id}` shows.
pub(crate) const TRACE_SCORES_CAP: usize = 200;

/// The most version × score × value rows the feedback query returns.
pub(crate) const FEEDBACK_CAP: usize = 5_000;

/// The scores of one prompt version with one name, source and data type.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FeedbackRow {
    /// The prompt version.
    pub(crate) version: u32,
    /// The score name.
    pub(crate) name: String,
    /// `app` or `console` (empty when the rows have none).
    pub(crate) source: String,
    /// `bool`, `number` or `category` (empty when the rows have none).
    pub(crate) data_type: String,
    /// How many scores (the latest per invocation, name and source).
    pub(crate) count: u64,
    /// `bool` scores that are `true`.
    pub(crate) positive: u64,
    /// The mean of `number` scores.
    pub(crate) mean: Option<f64>,
    /// The most common `category` (ties: the lowest value).
    pub(crate) mode: Option<String>,
}

/// The per-version feedback of one prompt.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct VersionFeedback {
    /// One row per version, name, source and data type: newest version first, then by name,
    /// source and data type.
    pub(crate) rows: Vec<FeedbackRow>,
    /// The query hit its row cap: some scores are left out.
    pub(crate) truncated: bool,
}

/// The error-banner hint for a score insert refused for lack of permission.
pub(crate) const BQ_EDITOR_HINT: &str =
    "needs roles/bigquery.dataEditor on the table and roles/bigquery.jobUser";

/// A score's value.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum NewValue {
    /// Thumbs up (`true`) or down (`false`).
    Bool(bool),
    /// A finite number.
    Number(f64),
    /// A non-blank category.
    Category(String),
}

/// A score submitted on `/traces/{id}`, checked by the rules of `gadk-agentanalytics`'
/// `Score::new` (a non-blank name, a non-blank category, a finite number) and with the same
/// error texts; the console does not depend on that crate.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NewScore {
    pub(crate) name: String,
    pub(crate) value: NewValue,
    /// `None` when the form's comment is blank.
    pub(crate) comment: Option<String>,
}

impl NewScore {
    /// The score of the form fields: `kind` is `up`, `down`, `number` (with `value` a number)
    /// or `category` (with `value` the category). The name is saved trimmed.
    ///
    /// # Errors
    ///
    /// The text of the 400 page: `score name is empty`, `kind: must be up, down, number or
    /// category`, `value: not a number`, `score number is not finite`, `score category is
    /// empty`.
    pub(crate) fn from_form(
        name: &str,
        kind: &str,
        value: &str,
        comment: &str,
    ) -> Result<NewScore, String> {
        if name.trim().is_empty() {
            return Err("score name is empty".to_owned());
        }
        let value = match kind {
            "up" => NewValue::Bool(true),
            "down" => NewValue::Bool(false),
            "number" => {
                let n: f64 = value
                    .trim()
                    .parse()
                    .map_err(|_| "value: not a number".to_owned())?;
                if !n.is_finite() {
                    return Err("score number is not finite".to_owned());
                }
                NewValue::Number(n)
            }
            "category" => {
                if value.trim().is_empty() {
                    return Err("score category is empty".to_owned());
                }
                NewValue::Category(value.to_owned())
            }
            _ => return Err("kind: must be up, down, number or category".to_owned()),
        };
        Ok(NewScore {
            name: name.trim().to_owned(),
            value,
            comment: (!comment.trim().is_empty()).then(|| comment.to_owned()),
        })
    }

    /// The row's `content`: `{"name", "value", "data_type", "comment"}`, `comment` only when
    /// there is one.
    pub(crate) fn content(&self) -> Value {
        let (value, data_type) = match &self.value {
            NewValue::Bool(b) => (Value::Bool(*b), "bool"),
            NewValue::Number(n) => (Value::from(*n), "number"),
            NewValue::Category(c) => (Value::from(c.as_str()), "category"),
        };
        let mut content = Map::new();
        content.insert("name".to_owned(), Value::from(self.name.as_str()));
        content.insert("value".to_owned(), value);
        content.insert("data_type".to_owned(), Value::from(data_type));
        if let Some(comment) = &self.comment {
            content.insert("comment".to_owned(), Value::from(comment.as_str()));
        }
        Value::Object(content)
    }
}

/// The identity of a trace's first row with an invocation id: where a console score goes.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TraceRoot {
    pub(crate) user_id: Option<String>,
    pub(crate) session_id: Option<String>,
    pub(crate) invocation_id: String,
    /// `attributes.adk.app_name`.
    pub(crate) app_name: Option<String>,
}

impl TraceRoot {
    /// The row's `attributes`: `source` `console`, `author`, and `adk` with `app_name` (when
    /// known) and `invocation_id`.
    pub(crate) fn attributes(&self, author: &str) -> Value {
        let mut adk = Map::new();
        if let Some(app) = &self.app_name {
            adk.insert("app_name".to_owned(), Value::from(app.as_str()));
        }
        adk.insert(
            "invocation_id".to_owned(),
            Value::from(self.invocation_id.as_str()),
        );
        json!({ "source": "console", "author": author, "adk": adk })
    }
}

/// One score as `/traces/{id}` lists it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ScoreRow {
    /// When the score was written.
    pub(crate) timestamp: DateTime<Utc>,
    /// `app` or `console` (empty when the row has none).
    pub(crate) source: String,
    /// Who scored (empty when the row has none).
    pub(crate) author: String,
    /// The score name.
    pub(crate) name: String,
    /// `bool`, `number` or `category` (empty when the row has none).
    pub(crate) data_type: String,
    /// The value as text (`true`, `4.5`, `helpful`).
    pub(crate) value: String,
    /// The comment (empty when none).
    pub(crate) comment: String,
}

/// The scores of one trace, newest first.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TraceScores {
    /// At most [`TRACE_SCORES_CAP`] scores.
    pub(crate) rows: Vec<ScoreRow>,
    /// More scores exist than are listed.
    pub(crate) truncated: bool,
}

impl UsageSource {
    /// The scores of the trace `trace_id`: `FEEDBACK` rows with that `trace_id` or with an
    /// `invocation_id` in `invocation_ids`, written from one day before `trace_start` until now,
    /// newest first, at most [`TRACE_SCORES_CAP`].
    pub(crate) async fn trace_scores(
        &self,
        trace_id: &str,
        invocation_ids: &[String],
        trace_start: DateTime<Utc>,
    ) -> Result<TraceScores, SourceError> {
        let sql = trace_scores_sql(&self.table_path()?);
        let params = vec![
            param("trace_id", "STRING", Some(trace_id.to_owned())),
            string_array_param("ids", invocation_ids),
            param("start", "TIMESTAMP", Some(ts_param(trace_start))),
        ];
        let (rows, truncated) = self.run_capped(&sql, params, TRACE_SCORES_CAP).await?;
        let rows = rows.iter().map(score_row).collect::<Result<Vec<_>, _>>()?;
        Ok(TraceScores { rows, truncated })
    }
}

/// A row of [`trace_scores_sql`].
fn score_row(row: &Map<String, Value>) -> Result<ScoreRow, SourceError> {
    Ok(ScoreRow {
        timestamp: timestamp(row, "timestamp")?,
        source: text(row, "source").unwrap_or_default(),
        author: text(row, "author").unwrap_or_default(),
        name: text(row, "name").unwrap_or_default(),
        data_type: text(row, "data_type").unwrap_or_default(),
        value: text(row, "value").unwrap_or_default(),
        comment: text(row, "comment").unwrap_or_default(),
    })
}

impl UsageSource {
    /// The identity of trace `trace_id`: its first row (not a score) with an invocation id in
    /// the last 30 days; `None` when there is none.
    pub(crate) async fn trace_root(
        &self,
        trace_id: &str,
    ) -> Result<Option<TraceRoot>, SourceError> {
        let sql = trace_root_sql(&self.table_path()?);
        let params = vec![param("trace_id", "STRING", Some(trace_id.to_owned()))];
        let result = self.run(&sql, params, 1).await?;
        Ok(result.rows.first().and_then(|row| {
            Some(TraceRoot {
                user_id: text(row, "user_id"),
                session_id: text(row, "session_id"),
                invocation_id: text(row, "invocation_id")?,
                app_name: text(row, "app_name"),
            })
        }))
    }

    /// Inserts `score` by `author` on trace `trace_id` as a `FEEDBACK` row of `root`'s
    /// invocation (one DML statement, billed at least 10 MiB).
    pub(crate) async fn insert_score(
        &self,
        trace_id: &str,
        root: &TraceRoot,
        score: &NewScore,
        author: &str,
    ) -> Result<(), SourceError> {
        let sql = insert_score_sql(&self.table_path()?);
        let params = vec![
            param("trace_id", "STRING", Some(trace_id.to_owned())),
            param("session_id", "STRING", root.session_id.clone()),
            param("invocation_id", "STRING", Some(root.invocation_id.clone())),
            param("user_id", "STRING", root.user_id.clone()),
            param("content", "STRING", Some(score.content().to_string())),
            param(
                "attributes",
                "STRING",
                Some(root.attributes(author).to_string()),
            ),
        ];
        self.run_dml(&sql, params).await
    }

    /// Inserts `scores` by `author` on trace `trace_id` as `FEEDBACK` rows of `root`'s
    /// invocation, in order: one DML statement (all rows or none), billed at least 10 MiB.
    pub(crate) async fn insert_scores(
        &self,
        trace_id: &str,
        root: &TraceRoot,
        scores: &[NewScore],
        author: &str,
    ) -> Result<(), SourceError> {
        let sql = insert_scores_sql(&self.table_path()?);
        let contents: Vec<String> = scores.iter().map(|s| s.content().to_string()).collect();
        let params = vec![
            param("trace_id", "STRING", Some(trace_id.to_owned())),
            param("session_id", "STRING", root.session_id.clone()),
            param("invocation_id", "STRING", Some(root.invocation_id.clone())),
            param("user_id", "STRING", root.user_id.clone()),
            string_array_param("contents", &contents),
            param(
                "attributes",
                "STRING",
                Some(root.attributes(author).to_string()),
            ),
        ];
        self.run_dml(&sql, params).await
    }
}

impl UsageSource {
    /// The scores of the calls of prompt `prompt`: `FEEDBACK` rows written since `start` (no
    /// upper bound: scores arrive after the calls), the latest per invocation, name and source,
    /// mapped to versions through the `LLM_REQUEST` rows of `[start, end)` widened by one day
    /// before and one hour after (an invocation that used several versions counts for each).
    pub(crate) async fn version_feedback(
        &self,
        prompt: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<VersionFeedback, SourceError> {
        let sql = version_feedback_sql(&self.table_path()?);
        let mut params = window_params(start, end);
        params.push(param("prompt", "STRING", Some(prompt.to_owned())));
        let (rows, truncated) = self.run_capped(&sql, params, FEEDBACK_CAP).await?;
        Ok(VersionFeedback {
            rows: merge_feedback(&rows),
            truncated,
        })
    }
}

/// A [`FeedbackRow`] being merged: the sum of its numbers and its category counts.
struct Merging {
    row: FeedbackRow,
    total: f64,
    categories: Vec<(String, u64)>,
}

/// The rows of [`version_feedback_sql`] (one per version, name, source, data type and
/// category) merged into one per version, name, source and data type, in first-seen order.
fn merge_feedback(rows: &[Map<String, Value>]) -> Vec<FeedbackRow> {
    let mut out: Vec<Merging> = Vec::new();
    for row in rows {
        let (Some(version), Some(name)) = (version_of(row), text(row, "name")) else {
            continue;
        };
        let source = text(row, "source").unwrap_or_default();
        let data_type = text(row, "data_type").unwrap_or_default();
        let n = count(row, "count");
        let i = match out.iter().position(|m| {
            m.row.version == version
                && m.row.name == name
                && m.row.source == source
                && m.row.data_type == data_type
        }) {
            Some(i) => i,
            None => {
                out.push(Merging {
                    row: FeedbackRow {
                        version,
                        name,
                        source,
                        data_type,
                        count: 0,
                        positive: 0,
                        mean: None,
                        mode: None,
                    },
                    total: 0.0,
                    categories: Vec::new(),
                });
                out.len() - 1
            }
        };
        let m = &mut out[i];
        m.row.count += n;
        m.row.positive += count(row, "positive");
        m.total += row.get("total").and_then(Value::as_f64).unwrap_or(0.0);
        if let Some(category) = text(row, "category") {
            m.categories.push((category, n));
        }
    }
    out.into_iter()
        .map(|m| {
            let mut r = m.row;
            if r.data_type == "number" && r.count > 0 {
                r.mean = Some(m.total / r.count as f64);
            }
            r.mode = m
                .categories
                .into_iter()
                .reduce(|best, c| {
                    if c.1 > best.1 || (c.1 == best.1 && c.0 < best.0) {
                        c
                    } else {
                        best
                    }
                })
                .map(|(category, _)| category);
            r
        })
        .collect()
}

/// Scores of the calls of `@prompt`, per version, name, source, data type and category (NULL
/// unless `category`): count, `true` values and the sum of numbers. Only the latest score per
/// (invocation, name, source) counts, a NULL source counting as `''`.
fn version_feedback_sql(table: &str) -> String {
    format!(
        r"WITH s AS (
  SELECT invocation_id, JSON_VALUE(content, '$.name') AS name,
    JSON_VALUE(attributes, '$.source') AS source,
    JSON_VALUE(content, '$.data_type') AS data_type,
    JSON_VALUE(content, '$.value') AS value
  FROM {table}
  WHERE event_type = 'FEEDBACK' AND timestamp >= @start
    AND invocation_id IS NOT NULL AND invocation_id != ''
  QUALIFY ROW_NUMBER() OVER (
    PARTITION BY invocation_id, JSON_VALUE(content, '$.name'), IFNULL(JSON_VALUE(attributes, '$.source'), '')
    ORDER BY timestamp DESC, event_id DESC) = 1),
v AS (
  SELECT DISTINCT invocation_id,
    SAFE_CAST(JSON_VALUE(attributes, '$.prompt.version') AS INT64) AS version
  FROM {table}
  WHERE event_type = 'LLM_REQUEST'
    AND timestamp >= TIMESTAMP_SUB(@start, INTERVAL 1 DAY)
    AND timestamp < TIMESTAMP_ADD(@end, INTERVAL 1 HOUR)
    AND JSON_VALUE(attributes, '$.prompt.name') = @prompt
    AND SAFE_CAST(JSON_VALUE(attributes, '$.prompt.version') AS INT64) IS NOT NULL)
SELECT v.version AS version, s.name AS name, IFNULL(s.source, '') AS source,
  IFNULL(s.data_type, '') AS data_type,
  IF(s.data_type = 'category', s.value, NULL) AS category,
  COUNT(*) AS count, COUNTIF(s.value = 'true') AS positive,
  SUM(IF(s.data_type = 'number', SAFE_CAST(s.value AS FLOAT64), NULL)) AS total
FROM s JOIN v ON v.invocation_id = s.invocation_id
GROUP BY 1, 2, 3, 4, 5
ORDER BY 1 DESC, 2, 3, 4, 5
LIMIT @limit"
    )
}

/// The first row of `@trace_id` (not a score) with an invocation id, in the last 30 days.
fn trace_root_sql(table: &str) -> String {
    format!(
        r"SELECT user_id, session_id, invocation_id,
  JSON_VALUE(attributes, '$.adk.app_name') AS app_name
FROM {table}
WHERE trace_id = @trace_id AND event_type != 'FEEDBACK'
  AND invocation_id IS NOT NULL AND invocation_id != ''
  AND timestamp >= TIMESTAMP_SUB(CURRENT_TIMESTAMP(), INTERVAL 30 DAY)
ORDER BY timestamp
LIMIT 1"
    )
}

/// One `FEEDBACK` row: the ids and JSON columns are parameters, the rest is fixed.
fn insert_score_sql(table: &str) -> String {
    format!(
        r"INSERT INTO {table} (timestamp, event_id, event_type, trace_id, session_id,
  invocation_id, user_id, status, is_truncated, content, attributes)
VALUES (CURRENT_TIMESTAMP(), REPLACE(GENERATE_UUID(), '-', ''), 'FEEDBACK', @trace_id,
  @session_id, @invocation_id, @user_id, 'OK', FALSE,
  PARSE_JSON(@content, wide_number_mode => 'round'),
  PARSE_JSON(@attributes, wide_number_mode => 'round'))"
    )
}

/// One `FEEDBACK` row per element of `@contents` (`ARRAY<STRING>`); the other columns are
/// those of [`insert_score_sql`].
fn insert_scores_sql(table: &str) -> String {
    format!(
        r"INSERT INTO {table} (timestamp, event_id, event_type, trace_id, session_id,
  invocation_id, user_id, status, is_truncated, content, attributes)
SELECT CURRENT_TIMESTAMP(), REPLACE(GENERATE_UUID(), '-', ''), 'FEEDBACK', @trace_id,
  @session_id, @invocation_id, @user_id, 'OK', FALSE,
  PARSE_JSON(c, wide_number_mode => 'round'),
  PARSE_JSON(@attributes, wide_number_mode => 'round')
FROM UNNEST(@contents) AS c"
    )
}

/// The `FEEDBACK` rows of `@trace_id` or of the invocations in `@ids`, newest first.
fn trace_scores_sql(table: &str) -> String {
    format!(
        r"SELECT timestamp, JSON_VALUE(attributes, '$.source') AS source,
  JSON_VALUE(attributes, '$.author') AS author, JSON_VALUE(content, '$.name') AS name,
  JSON_VALUE(content, '$.data_type') AS data_type, JSON_VALUE(content, '$.value') AS value,
  JSON_VALUE(content, '$.comment') AS comment
FROM {table}
WHERE event_type = 'FEEDBACK'
  AND timestamp >= TIMESTAMP_SUB(@start, INTERVAL 1 DAY)
  AND timestamp <= CURRENT_TIMESTAMP()
  AND (trace_id = @trace_id OR invocation_id IN UNNEST(@ids))
ORDER BY timestamp DESC
LIMIT @limit"
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::{TimeZone, Utc};
    use gadk_bigquery::{BigQueryClient, NoAuth};
    use pretty_assertions::assert_eq;
    use serde_json::{Value, json};

    use super::*;
    use crate::traces::BigQueryTable;
    use crate::traces::test_http::{FakeHttp, param_value, reply};

    const TRACE: &str = "0123456789abcdef0123456789abcdef";

    const SCORE_FIELDS: [(&str, &str); 7] = [
        ("timestamp", "TIMESTAMP"),
        ("source", "STRING"),
        ("author", "STRING"),
        ("name", "STRING"),
        ("data_type", "STRING"),
        ("value", "STRING"),
        ("comment", "STRING"),
    ];

    const ROOT_FIELDS: [(&str, &str); 4] = [
        ("user_id", "STRING"),
        ("session_id", "STRING"),
        ("invocation_id", "STRING"),
        ("app_name", "STRING"),
    ];

    /// A completed DML reply.
    fn dml_reply() -> Result<(u16, String), String> {
        Ok((
            200,
            json!({
                "jobComplete": true,
                "numDmlAffectedRows": "1",
                "jobReference": { "projectId": "billing-proj", "jobId": "job_2" }
            })
            .to_string(),
        ))
    }

    fn source(fake: &Arc<FakeHttp>) -> UsageSource {
        UsageSource::with_client(
            BigQueryClient::with_client(fake.clone(), Arc::new(NoAuth)),
            "billing-proj",
            BigQueryTable::new("data-proj", "agent_analytics", "agent_events"),
            1_000,
        )
    }

    #[tokio::test]
    async fn trace_scores_read_feedback_rows_of_the_trace_and_its_invocations() {
        let at = Utc.with_ymd_and_hms(2026, 11, 20, 12, 0, 0).unwrap();
        let fake = FakeHttp::new(vec![reply(
            &SCORE_FIELDS,
            vec![
                vec![
                    json!(at.timestamp_micros().to_string()),
                    json!("console"),
                    json!("chris"),
                    json!("quality"),
                    json!("category"),
                    json!("helpful"),
                    json!("clear answer"),
                ],
                vec![
                    json!((at.timestamp_micros() - 60_000_000).to_string()),
                    json!("app"),
                    json!("user-1"),
                    json!("thumbs"),
                    json!("bool"),
                    json!("true"),
                    Value::Null,
                ],
            ],
            false,
        )]);
        let ids = vec!["e-1".to_owned(), "e-2".to_owned()];
        let start = Utc.with_ymd_and_hms(2026, 11, 20, 11, 0, 0).unwrap();
        let scores = source(&fake)
            .trace_scores(TRACE, &ids, start)
            .await
            .unwrap();
        assert!(!scores.truncated);
        assert_eq!(
            scores.rows,
            vec![
                ScoreRow {
                    timestamp: at,
                    source: "console".into(),
                    author: "chris".into(),
                    name: "quality".into(),
                    data_type: "category".into(),
                    value: "helpful".into(),
                    comment: "clear answer".into(),
                },
                ScoreRow {
                    timestamp: at - chrono::TimeDelta::seconds(60),
                    source: "app".into(),
                    author: "user-1".into(),
                    name: "thumbs".into(),
                    data_type: "bool".into(),
                    value: "true".into(),
                    comment: String::new(),
                },
            ]
        );
        let body = fake.body(0);
        assert_eq!(body["maximumBytesBilled"], json!("1000"));
        assert_eq!(body["maxResults"], json!(201));
        let sql = body["query"].as_str().unwrap();
        for piece in [
            "FROM `data-proj.agent_analytics.agent_events`",
            "WHERE event_type = 'FEEDBACK'",
            "AND timestamp >= TIMESTAMP_SUB(@start, INTERVAL 1 DAY)",
            "AND timestamp <= CURRENT_TIMESTAMP()",
            "AND (trace_id = @trace_id OR invocation_id IN UNNEST(@ids))",
            "ORDER BY timestamp DESC",
            "LIMIT @limit",
        ] {
            assert!(sql.contains(piece), "{piece}: {sql}");
        }
        assert!(!sql.contains(TRACE) && !sql.contains("e-1"), "{sql}");
        assert_eq!(
            param_value(&body, "trace_id")["parameterValue"]["value"],
            json!(TRACE)
        );
        assert_eq!(
            param_value(&body, "ids")["parameterValue"],
            json!({ "arrayValues": [{ "value": "e-1" }, { "value": "e-2" }] })
        );
        assert_eq!(
            param_value(&body, "start")["parameterValue"]["value"],
            json!("2026-11-20 11:00:00.000000+00:00")
        );
        assert_eq!(
            param_value(&body, "limit")["parameterValue"]["value"],
            json!("201")
        );
    }

    #[test]
    fn form_scores_follow_the_agentanalytics_rules() {
        let ok = |kind: &str, value: &str| NewScore::from_form("q", kind, value, "").unwrap().value;
        assert_eq!(ok("up", ""), NewValue::Bool(true));
        assert_eq!(ok("down", "ignored"), NewValue::Bool(false));
        assert_eq!(ok("number", " 3.75 "), NewValue::Number(3.75));
        assert_eq!(
            ok("category", "helpful"),
            NewValue::Category("helpful".into())
        );
        let s = NewScore::from_form("quality", "up", "", " note ").unwrap();
        assert_eq!(s.comment.as_deref(), Some(" note "));
        assert_eq!(
            NewScore::from_form("q", "up", "", "  ").unwrap().comment,
            None
        );
        let err = |name: &str, kind: &str, value: &str| {
            NewScore::from_form(name, kind, value, "").unwrap_err()
        };
        assert_eq!(err(" ", "up", ""), "score name is empty");
        assert_eq!(
            err("q", "maybe", ""),
            "kind: must be up, down, number or category"
        );
        assert_eq!(err("q", "number", "abc"), "value: not a number");
        assert_eq!(err("q", "number", "inf"), "score number is not finite");
        assert_eq!(err("q", "number", "NaN"), "score number is not finite");
        assert_eq!(err("q", "category", " "), "score category is empty");
    }

    #[test]
    fn form_score_names_are_saved_trimmed() {
        let s = NewScore::from_form(" quality ", "up", "", "").unwrap();
        assert_eq!(s.content()["name"], json!("quality"));
        assert_eq!(
            NewScore::from_form(" \t ", "up", "", "").unwrap_err(),
            "score name is empty"
        );
    }

    #[test]
    fn content_and_attributes_have_the_feedback_shape() {
        let s = NewScore::from_form("rating", "number", "4", "fast").unwrap();
        assert_eq!(
            s.content(),
            json!({"name": "rating", "value": 4.0, "data_type": "number", "comment": "fast"})
        );
        let s = NewScore::from_form("thumbs", "down", "", "").unwrap();
        assert_eq!(
            s.content(),
            json!({"name": "thumbs", "value": false, "data_type": "bool"})
        );
        let root = TraceRoot {
            user_id: Some("u-1".into()),
            session_id: Some("s-1".into()),
            invocation_id: "e-1".into(),
            app_name: Some("shop".into()),
        };
        assert_eq!(
            root.attributes("chris"),
            json!({"source": "console", "author": "chris", "adk": {"app_name": "shop", "invocation_id": "e-1"}})
        );
        let no_app = TraceRoot {
            app_name: None,
            ..root
        };
        assert_eq!(
            no_app.attributes("chris")["adk"],
            json!({"invocation_id": "e-1"})
        );
    }

    #[tokio::test]
    async fn trace_root_reads_the_first_row_with_an_invocation() {
        let fake = FakeHttp::new(vec![
            reply(
                &ROOT_FIELDS,
                vec![vec![
                    json!("u-1"),
                    json!("s-1"),
                    json!("e-1"),
                    json!("shop"),
                ]],
                false,
            ),
            reply(&ROOT_FIELDS, vec![], false),
        ]);
        let src = source(&fake);
        assert_eq!(
            src.trace_root(TRACE).await.unwrap(),
            Some(TraceRoot {
                user_id: Some("u-1".into()),
                session_id: Some("s-1".into()),
                invocation_id: "e-1".into(),
                app_name: Some("shop".into()),
            })
        );
        assert_eq!(src.trace_root(TRACE).await.unwrap(), None);
        let body = fake.body(0);
        assert_eq!(body["maximumBytesBilled"], json!("1000"));
        assert_eq!(body["maxResults"], json!(1));
        let sql = body["query"].as_str().unwrap();
        for piece in [
            "SELECT user_id, session_id, invocation_id,\n  JSON_VALUE(attributes, '$.adk.app_name') AS app_name",
            "WHERE trace_id = @trace_id AND event_type != 'FEEDBACK'",
            "AND invocation_id IS NOT NULL AND invocation_id != ''",
            "AND timestamp >= TIMESTAMP_SUB(CURRENT_TIMESTAMP(), INTERVAL 30 DAY)",
            "ORDER BY timestamp\nLIMIT 1",
        ] {
            assert!(sql.contains(piece), "{piece}: {sql}");
        }
        assert!(!sql.contains(TRACE), "{sql}");
        assert_eq!(
            param_value(&body, "trace_id")["parameterValue"]["value"],
            json!(TRACE)
        );
    }

    #[tokio::test]
    async fn insert_score_is_one_parameterized_dml_billed_at_least_10_mib() {
        let fake = FakeHttp::new(vec![dml_reply()]);
        let root = TraceRoot {
            user_id: None,
            session_id: Some("s-1".into()),
            invocation_id: "e-1".into(),
            app_name: Some("shop".into()),
        };
        let score = NewScore::from_form("quality", "category", "helpful", "").unwrap();
        source(&fake)
            .insert_score(TRACE, &root, &score, "chris")
            .await
            .unwrap();
        let body = fake.body(0);
        assert_eq!(body["maximumBytesBilled"], json!("10485760"));
        let sql = body["query"].as_str().unwrap();
        for piece in [
            "INSERT INTO `data-proj.agent_analytics.agent_events` (timestamp, event_id, event_type, trace_id, session_id,",
            "VALUES (CURRENT_TIMESTAMP(), REPLACE(GENERATE_UUID(), '-', ''), 'FEEDBACK', @trace_id,",
            "@session_id, @invocation_id, @user_id, 'OK', FALSE,",
            "PARSE_JSON(@content, wide_number_mode => 'round')",
            "PARSE_JSON(@attributes, wide_number_mode => 'round'))",
        ] {
            assert!(sql.contains(piece), "{piece}: {sql}");
        }
        for value in [TRACE, "s-1", "e-1", "helpful", "chris", "shop"] {
            assert!(!sql.contains(value), "{value} is a parameter: {sql}");
        }
        assert_eq!(
            param_value(&body, "user_id")["parameterValue"],
            json!({}),
            "an unknown user id is NULL"
        );
        assert_eq!(
            param_value(&body, "session_id")["parameterValue"]["value"],
            json!("s-1")
        );
        let content: Value = serde_json::from_str(
            param_value(&body, "content")["parameterValue"]["value"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            content,
            json!({"name": "quality", "value": "helpful", "data_type": "category"})
        );
        let attributes: Value = serde_json::from_str(
            param_value(&body, "attributes")["parameterValue"]["value"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(attributes["author"], json!("chris"));
        assert_eq!(attributes["source"], json!("console"));
    }

    #[tokio::test]
    async fn a_larger_byte_cap_is_kept_for_the_insert() {
        let fake = FakeHttp::new(vec![dml_reply()]);
        let src = UsageSource::with_client(
            BigQueryClient::with_client(fake.clone(), Arc::new(NoAuth)),
            "billing-proj",
            BigQueryTable::new("data-proj", "agent_analytics", "agent_events"),
            50_000_000,
        );
        let root = TraceRoot {
            user_id: None,
            session_id: None,
            invocation_id: "e-1".into(),
            app_name: None,
        };
        let score = NewScore::from_form("thumbs", "up", "", "").unwrap();
        src.insert_score(TRACE, &root, &score, "chris")
            .await
            .unwrap();
        assert_eq!(fake.body(0)["maximumBytesBilled"], json!("50000000"));
    }

    #[test]
    fn the_insert_scores_sql_is_byte_identical() {
        assert_eq!(
            insert_scores_sql("`p.d.t`"),
            r"INSERT INTO `p.d.t` (timestamp, event_id, event_type, trace_id, session_id,
  invocation_id, user_id, status, is_truncated, content, attributes)
SELECT CURRENT_TIMESTAMP(), REPLACE(GENERATE_UUID(), '-', ''), 'FEEDBACK', @trace_id,
  @session_id, @invocation_id, @user_id, 'OK', FALSE,
  PARSE_JSON(c, wide_number_mode => 'round'),
  PARSE_JSON(@attributes, wide_number_mode => 'round')
FROM UNNEST(@contents) AS c"
        );
    }

    #[tokio::test]
    async fn insert_scores_is_one_dml_with_the_contents_in_order() {
        let fake = FakeHttp::new(vec![dml_reply()]);
        let root = TraceRoot {
            user_id: Some("u-1".into()),
            session_id: Some("s-1".into()),
            invocation_id: "e-1".into(),
            app_name: Some("shop".into()),
        };
        let scores = [
            NewScore::from_form("support/accuracy", "number", "4", "kind").unwrap(),
            NewScore::from_form("support/tone", "number", "2", "kind").unwrap(),
        ];
        source(&fake)
            .insert_scores(TRACE, &root, &scores, "chris")
            .await
            .unwrap();
        assert_eq!(fake.uris().len(), 1, "one statement");
        let body = fake.body(0);
        assert_eq!(body["maximumBytesBilled"], json!("10485760"));
        assert_eq!(
            body["query"],
            json!(insert_scores_sql(
                "`data-proj.agent_analytics.agent_events`"
            ))
        );
        let contents = param_value(&body, "contents");
        assert_eq!(
            contents["parameterType"],
            json!({ "type": "ARRAY", "arrayType": { "type": "STRING" } })
        );
        assert_eq!(
            contents["parameterValue"],
            json!({ "arrayValues": [
                { "value": r#"{"comment":"kind","data_type":"number","name":"support/accuracy","value":4.0}"# },
                { "value": r#"{"comment":"kind","data_type":"number","name":"support/tone","value":2.0}"# },
            ] })
        );
        for (name, want) in [
            ("trace_id", TRACE),
            ("session_id", "s-1"),
            ("invocation_id", "e-1"),
            ("user_id", "u-1"),
        ] {
            assert_eq!(
                param_value(&body, name)["parameterValue"]["value"],
                json!(want),
                "{name}"
            );
        }
        let attributes: Value = serde_json::from_str(
            param_value(&body, "attributes")["parameterValue"]["value"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            attributes,
            json!({"source": "console", "author": "chris", "adk": {"app_name": "shop", "invocation_id": "e-1"}})
        );
        let sql = body["query"].as_str().unwrap();
        for value in [TRACE, "s-1", "e-1", "u-1", "support", "kind", "chris"] {
            assert!(!sql.contains(value), "{value} is a parameter: {sql}");
        }
    }

    #[tokio::test]
    async fn insert_scores_keeps_a_larger_byte_cap() {
        let fake = FakeHttp::new(vec![dml_reply()]);
        let src = UsageSource::with_client(
            BigQueryClient::with_client(fake.clone(), Arc::new(NoAuth)),
            "billing-proj",
            BigQueryTable::new("data-proj", "agent_analytics", "agent_events"),
            50_000_000,
        );
        let root = TraceRoot {
            user_id: None,
            session_id: None,
            invocation_id: "e-1".into(),
            app_name: None,
        };
        let scores = [NewScore::from_form("r/c", "number", "1", "").unwrap()];
        src.insert_scores(TRACE, &root, &scores, "chris")
            .await
            .unwrap();
        let body = fake.body(0);
        assert_eq!(body["maximumBytesBilled"], json!("50000000"));
        assert_eq!(
            param_value(&body, "user_id")["parameterValue"],
            json!({}),
            "an unknown user id is NULL"
        );
    }

    const FEEDBACK_FIELDS: [(&str, &str); 8] = [
        ("version", "INT64"),
        ("name", "STRING"),
        ("source", "STRING"),
        ("data_type", "STRING"),
        ("category", "STRING"),
        ("count", "INT64"),
        ("positive", "INT64"),
        ("total", "FLOAT64"),
    ];

    /// A category row of `FEEDBACK_FIELDS` for version 2 of score `quality` from the app.
    fn category(value: &str, count: &str) -> Vec<Value> {
        vec![
            json!("2"),
            json!("quality"),
            json!("app"),
            json!("category"),
            json!(value),
            json!(count),
            json!("0"),
            Value::Null,
        ]
    }

    /// A `bool` row of `FEEDBACK_FIELDS` for score `thumbs`.
    fn thumbs(version: &str, source: &str, count: &str, positive: &str) -> Vec<Value> {
        vec![
            json!(version),
            json!("thumbs"),
            json!(source),
            json!("bool"),
            Value::Null,
            json!(count),
            json!(positive),
            Value::Null,
        ]
    }

    #[tokio::test]
    async fn version_feedback_summarizes_each_version_name_and_source() {
        let rating = vec![
            json!("2"),
            json!("rating"),
            json!("console"),
            json!("number"),
            Value::Null,
            json!("2"),
            json!("0"),
            json!("7.5"),
        ];
        let fake = FakeHttp::new(vec![reply(
            &FEEDBACK_FIELDS,
            vec![
                category("clear", "2"),
                category("helpful", "2"),
                category("vague", "1"),
                rating,
                thumbs("2", "app", "3", "2"),
                thumbs("2", "console", "1", "0"),
                thumbs("1", "app", "1", "1"),
            ],
            false,
        )]);
        let start = Utc.with_ymd_and_hms(2026, 11, 13, 12, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2026, 11, 20, 12, 0, 0).unwrap();
        let f = source(&fake)
            .version_feedback("greet", start, end)
            .await
            .unwrap();
        assert!(!f.truncated);
        let row = |version, name: &str, source: &str, data_type: &str| FeedbackRow {
            version,
            name: name.into(),
            source: source.into(),
            data_type: data_type.into(),
            count: 0,
            positive: 0,
            mean: None,
            mode: None,
        };
        assert_eq!(
            f.rows,
            vec![
                FeedbackRow {
                    count: 5,
                    mode: Some("clear".into()),
                    ..row(2, "quality", "app", "category")
                },
                FeedbackRow {
                    count: 2,
                    mean: Some(3.75),
                    ..row(2, "rating", "console", "number")
                },
                FeedbackRow {
                    count: 3,
                    positive: 2,
                    ..row(2, "thumbs", "app", "bool")
                },
                FeedbackRow {
                    count: 1,
                    ..row(2, "thumbs", "console", "bool")
                },
                FeedbackRow {
                    count: 1,
                    positive: 1,
                    ..row(1, "thumbs", "app", "bool")
                },
            ]
        );
        let body = fake.body(0);
        assert_eq!(body["maximumBytesBilled"], json!("1000"));
        let sql = body["query"].as_str().unwrap();
        for piece in [
            "WHERE event_type = 'FEEDBACK' AND timestamp >= @start\n    AND invocation_id IS NOT NULL",
            "QUALIFY ROW_NUMBER() OVER (\n    PARTITION BY invocation_id, JSON_VALUE(content, '$.name'), IFNULL(JSON_VALUE(attributes, '$.source'), '')\n    ORDER BY timestamp DESC, event_id DESC) = 1",
            "WHERE event_type = 'LLM_REQUEST'",
            "AND timestamp >= TIMESTAMP_SUB(@start, INTERVAL 1 DAY)",
            "AND timestamp < TIMESTAMP_ADD(@end, INTERVAL 1 HOUR)",
            "AND JSON_VALUE(attributes, '$.prompt.name') = @prompt",
            "FROM s JOIN v ON v.invocation_id = s.invocation_id",
            "GROUP BY 1, 2, 3, 4, 5",
            "LIMIT @limit",
        ] {
            assert!(sql.contains(piece), "{piece}: {sql}");
        }
        assert!(!sql.contains("greet"), "{sql}");
        assert_eq!(
            param_value(&body, "prompt")["parameterValue"]["value"],
            json!("greet")
        );
        assert_eq!(
            param_value(&body, "start")["parameterValue"]["value"],
            json!("2026-11-13 12:00:00.000000+00:00")
        );
        assert_eq!(
            param_value(&body, "end")["parameterValue"]["value"],
            json!("2026-11-20 12:00:00.000000+00:00")
        );
        assert_eq!(
            param_value(&body, "limit")["parameterValue"]["value"],
            json!("5001")
        );
    }

    #[test]
    fn the_mode_breaks_ties_by_the_lowest_value_in_any_row_order() {
        // Decoded rows, as `gadk-bigquery` returns them: INT64 cells are numbers.
        let cells = |category: &str, count: u64| -> Map<String, Value> {
            let row = json!({
                "version": 1, "name": "q", "source": "app", "data_type": "category",
                "category": category, "count": count, "positive": 0, "total": null,
            });
            row.as_object().unwrap().clone()
        };
        let rows = merge_feedback(&[cells("zeta", 2), cells("alpha", 2), cells("mid", 1)]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].mode.as_deref(), Some("alpha"));
        assert_eq!(rows[0].count, 5);
        assert_eq!(rows[0].mean, None);
    }

    #[tokio::test]
    async fn trace_scores_note_more_than_the_cap() {
        let at = Utc.with_ymd_and_hms(2026, 11, 20, 12, 0, 0).unwrap();
        let row = vec![
            json!(at.timestamp_micros().to_string()),
            json!("app"),
            json!("u"),
            json!("thumbs"),
            json!("bool"),
            json!("false"),
            Value::Null,
        ];
        let fake = FakeHttp::new(vec![reply(&SCORE_FIELDS, vec![row], true)]);
        let scores = source(&fake).trace_scores(TRACE, &[], at).await.unwrap();
        assert!(scores.truncated);
        assert_eq!(scores.rows.len(), 1);
        assert_eq!(
            param_value(&fake.body(0), "ids")["parameterValue"],
            json!({ "arrayValues": [] })
        );
    }
}
