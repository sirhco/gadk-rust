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
//!
//! [`AnalyticsSource::insert_score`] and [`AnalyticsSource::insert_scores`] are writes: each is
//! one DML statement that inserts `FEEDBACK` rows into the table, needs
//! `roles/bigquery.dataEditor` on it ([`BQ_EDITOR_HINT`]) and is billed at least 10 MiB
//! ([`DML_MIN_BYTES`]), whatever the byte cap.
//!
//! ```
//! use std::sync::Arc;
//!
//! use chrono::{TimeZone, Utc};
//! use gadk_analytics::testutil::{FakeHttp, reply};
//! use gadk_analytics::{AnalyticsSource, BigQueryTable};
//! use gadk_bigquery::{BigQueryClient, NoAuth};
//!
//! # tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
//! // One scripted `jobs.query` reply: one score of the trace.
//! let http = FakeHttp::new(vec![reply(
//!     &[
//!         ("timestamp", "TIMESTAMP"), ("source", "STRING"), ("author", "STRING"),
//!         ("name", "STRING"), ("data_type", "STRING"), ("value", "STRING"),
//!         ("comment", "STRING"),
//!     ],
//!     vec![vec![
//!         "1763640000000000".into(), "console".into(), "chris".into(), "quality".into(),
//!         "bool".into(), "true".into(), serde_json::Value::Null,
//!     ]],
//!     false,
//! )]);
//! let source = AnalyticsSource::new(
//!     BigQueryClient::with_client(http.clone(), Arc::new(NoAuth)),
//!     "billing-proj",
//!     BigQueryTable::new("data-proj", "agent_analytics", "agent_events"),
//!     1_073_741_824,
//! );
//! let start = Utc.with_ymd_and_hms(2026, 11, 20, 12, 0, 0).unwrap();
//! let scores = source
//!     .trace_scores("0123456789abcdef0123456789abcdef", &[], start)
//!     .await?;
//! assert_eq!(scores.rows[0].name, "quality");
//! assert_eq!(scores.rows[0].value, "true");
//! # Ok::<(), gadk_analytics::SourceError>(())
//! # }).unwrap();
//! ```

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::row::{param, text, timestamp, ts_param};
use crate::source::string_array_param;
use crate::{AnalyticsSource, SourceError};

/// The byte cap floor of a DML statement: BigQuery bills every one at least 10 MiB.
pub const DML_MIN_BYTES: i64 = 10_485_760;

/// The most scores `/traces/{id}` shows.
pub const TRACE_SCORES_CAP: usize = 200;

/// The error-banner hint for a score insert refused for lack of permission.
pub const BQ_EDITOR_HINT: &str =
    "needs roles/bigquery.dataEditor on the table and roles/bigquery.jobUser";

/// A score's value.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub enum NewValue {
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
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct NewScore {
    /// The score name, trimmed.
    pub name: String,
    /// The value.
    pub value: NewValue,
    /// `None` when the form's comment is blank.
    pub comment: Option<String>,
}

impl NewScore {
    /// The score `name` (saved trimmed) with `value` and `comment`, checked by the rules of
    /// [`from_form`](Self::from_form): a non-blank name, a non-blank category and a finite
    /// number.
    ///
    /// # Errors
    ///
    /// `score name is empty`, `score category is empty` or `score number is not finite`.
    pub fn new(
        name: impl Into<String>,
        value: NewValue,
        comment: Option<String>,
    ) -> Result<NewScore, String> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err("score name is empty".to_owned());
        }
        match &value {
            NewValue::Number(n) if !n.is_finite() => {
                return Err("score number is not finite".to_owned());
            }
            NewValue::Category(c) if c.trim().is_empty() => {
                return Err("score category is empty".to_owned());
            }
            _ => {}
        }
        Ok(NewScore {
            name: name.trim().to_owned(),
            value,
            comment,
        })
    }

    /// The score of the form fields: `kind` is `up`, `down`, `number` (with `value` a number)
    /// or `category` (with `value` the category). The name is saved trimmed.
    ///
    /// # Errors
    ///
    /// The text of the 400 page: `score name is empty`, `kind: must be up, down, number or
    /// category`, `value: not a number`, `score number is not finite`, `score category is
    /// empty`.
    pub fn from_form(
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
    pub fn content(&self) -> Value {
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
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct TraceRoot {
    /// The row's `user_id`.
    pub user_id: Option<String>,
    /// The row's `session_id`.
    pub session_id: Option<String>,
    /// The row's `invocation_id`.
    pub invocation_id: String,
    /// `attributes.adk.app_name`.
    pub app_name: Option<String>,
}

impl TraceRoot {
    /// The identity of a trace's invocation `invocation_id` of `app_name`, by `user_id` in
    /// `session_id`.
    pub fn new(
        user_id: Option<String>,
        session_id: Option<String>,
        invocation_id: String,
        app_name: Option<String>,
    ) -> Self {
        Self {
            user_id,
            session_id,
            invocation_id,
            app_name,
        }
    }
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
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct ScoreRow {
    /// When the score was written.
    pub timestamp: DateTime<Utc>,
    /// `app` or `console` (empty when the row has none).
    pub source: String,
    /// Who scored (empty when the row has none).
    pub author: String,
    /// The score name.
    pub name: String,
    /// `bool`, `number` or `category` (empty when the row has none).
    pub data_type: String,
    /// The value as text (`true`, `4.5`, `helpful`).
    pub value: String,
    /// The comment (empty when none).
    pub comment: String,
}

impl ScoreRow {
    /// A score row with every field given.
    pub fn new(
        timestamp: DateTime<Utc>,
        source: String,
        author: String,
        name: String,
        data_type: String,
        value: String,
        comment: String,
    ) -> Self {
        Self {
            timestamp,
            source,
            author,
            name,
            data_type,
            value,
            comment,
        }
    }
}

/// The scores of one trace, newest first.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct TraceScores {
    /// At most [`TRACE_SCORES_CAP`] scores.
    pub rows: Vec<ScoreRow>,
    /// More scores exist than are listed.
    pub truncated: bool,
}

impl TraceScores {
    /// The scores `rows`; `truncated` when more exist.
    pub fn new(rows: Vec<ScoreRow>, truncated: bool) -> Self {
        Self { rows, truncated }
    }
}

impl AnalyticsSource {
    /// The scores of the trace `trace_id`: `FEEDBACK` rows with that `trace_id` or with an
    /// `invocation_id` in `invocation_ids`, written from one day before `trace_start` until now,
    /// newest first, at most [`TRACE_SCORES_CAP`].
    pub async fn trace_scores(
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

impl AnalyticsSource {
    /// The identity of trace `trace_id`: its first row (not a score) with an invocation id in
    /// the last 30 days; `None` when there is none.
    pub async fn trace_root(&self, trace_id: &str) -> Result<Option<TraceRoot>, SourceError> {
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
    pub async fn insert_score(
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
    pub async fn insert_scores(
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
    use crate::BigQueryTable;
    use crate::testutil::{FakeHttp, param_value, reply};

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

    fn source(fake: &Arc<FakeHttp>) -> AnalyticsSource {
        AnalyticsSource::new(
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
    fn new_scores_follow_the_form_rules() {
        assert_eq!(
            NewScore::new(" quality ", NewValue::Bool(true), None),
            Ok(NewScore {
                name: "quality".into(),
                value: NewValue::Bool(true),
                comment: None,
            })
        );
        let err = |name: &str, value: NewValue| NewScore::new(name, value, None).unwrap_err();
        assert_eq!(err(" \t ", NewValue::Bool(true)), "score name is empty");
        assert_eq!(
            err("q", NewValue::Category("  ".into())),
            "score category is empty"
        );
        assert_eq!(
            err("q", NewValue::Number(f64::NAN)),
            "score number is not finite"
        );
        assert_eq!(
            err("q", NewValue::Number(f64::INFINITY)),
            "score number is not finite"
        );
        let s = NewScore::new("rating", NewValue::Number(4.0), Some("fast".into())).unwrap();
        assert_eq!(
            s,
            NewScore::from_form("rating", "number", "4", "fast").unwrap()
        );
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
        let src = AnalyticsSource::new(
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
        let src = AnalyticsSource::new(
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
