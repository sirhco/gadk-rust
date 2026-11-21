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
use serde_json::{Map, Value};

use crate::traces::{SourceError, param, text, timestamp, ts_param};
use crate::usage::{UsageSource, string_array_param};

/// The most scores `/traces/{id}` shows.
pub(crate) const TRACE_SCORES_CAP: usize = 200;

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
