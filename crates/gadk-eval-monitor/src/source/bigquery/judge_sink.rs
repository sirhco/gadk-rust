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

//! [`ScoreSink`] for the BigQuery source: one dedupe query over the rows' invocations and names
//! (from the report window's start), then one parameterized
//! `INSERT … SELECT … FROM UNNEST(@rows)` of judge `FEEDBACK` rows per call.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use super::{BigQueryTranscriptSource, cell_str, string_array_param, timestamp_param};
use crate::error::MonitorError;
use crate::judge_rows::{JudgeRow, ScoreSink};

/// The least `maximumBytesBilled` of a judge-row `INSERT`: BigQuery bills every DML statement at
/// least 10 MiB. (`gadk-analytics` has the same constant, but this crate cannot depend on it.)
const JUDGE_DML_MIN_BYTES: i64 = 10 * 1024 * 1024;

/// Appended to the error of a failed dedupe query or insert.
const WRITE_HINT: &str = "; writing scores needs roles/bigquery.dataEditor on the table";

#[async_trait::async_trait]
impl ScoreSink for BigQueryTranscriptSource {
    async fn existing(
        &self,
        rows: &[JudgeRow],
        since: DateTime<Utc>,
    ) -> Result<HashSet<(String, String)>, MonitorError> {
        if rows.is_empty() {
            return Ok(HashSet::new());
        }
        let found = self
            .query(
                &existing_judge_sql(&self.table),
                existing_params(rows, since),
            )
            .await
            .map_err(with_hint)?;
        Ok(found
            .iter()
            .filter_map(|r| Some((cell_str(r, "invocation_id")?, cell_str(r, "name")?)))
            .collect())
    }

    async fn insert(&self, rows: &[JudgeRow], author: &str) -> Result<(), MonitorError> {
        if rows.is_empty() {
            return Ok(());
        }
        self.query_capped(
            &insert_judge_sql(&self.table),
            insert_params(rows, author),
            Some(dml_cap(self.max_bytes_billed)),
        )
        .await
        .map_err(with_hint)?;
        Ok(())
    }
}

/// The insert's byte cap: the source's cap raised to [`JUDGE_DML_MIN_BYTES`], or that minimum
/// when the source has none.
fn dml_cap(cap: Option<i64>) -> i64 {
    cap.map_or(JUDGE_DML_MIN_BYTES, |b| b.max(JUDGE_DML_MIN_BYTES))
}

/// `e` with [`WRITE_HINT`] appended to a source error's message.
fn with_hint(e: MonitorError) -> MonitorError {
    match e {
        MonitorError::Source(m) => MonitorError::Source(format!("{m}{WRITE_HINT}")),
        other => other,
    }
}

/// The (invocation_id, name) pairs of judge rows written at or after `@start` for the
/// invocations `@ids` and names `@names`.
fn existing_judge_sql(table: &str) -> String {
    format!(
        r"SELECT DISTINCT invocation_id, JSON_VALUE(content, '$.name') AS name
FROM {table}
WHERE event_type = 'FEEDBACK' AND JSON_VALUE(attributes, '$.source') = 'judge'
  AND timestamp >= @start
  AND invocation_id IN UNNEST(@ids) AND JSON_VALUE(content, '$.name') IN UNNEST(@names)"
    )
}

/// One `FEEDBACK` row per element of `@rows` (a JSON object with `trace_id`, `session_id`,
/// `invocation_id`, and `content` and `attributes` as JSON text); `user_id` is NULL.
fn insert_judge_sql(table: &str) -> String {
    format!(
        r"INSERT INTO {table} (timestamp, event_id, event_type, trace_id, session_id,
  invocation_id, user_id, status, is_truncated, content, attributes)
SELECT CURRENT_TIMESTAMP(), REPLACE(GENERATE_UUID(), '-', ''), 'FEEDBACK',
  JSON_VALUE(r, '$.trace_id'), JSON_VALUE(r, '$.session_id'),
  JSON_VALUE(r, '$.invocation_id'), NULL, 'OK', FALSE,
  PARSE_JSON(JSON_VALUE(r, '$.content'), wide_number_mode => 'round'),
  PARSE_JSON(JSON_VALUE(r, '$.attributes'), wide_number_mode => 'round')
FROM UNNEST(@rows) AS r_text, UNNEST([PARSE_JSON(r_text)]) AS r"
    )
}

/// The distinct `values`, in first-seen order.
fn distinct<'a>(values: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .filter(|v| seen.insert(*v))
        .map(str::to_owned)
        .collect()
}

/// `@start` (`since`), `@ids` (the rows' invocation ids) and `@names` (their names), each
/// distinct in first-seen order.
fn existing_params(rows: &[JudgeRow], since: DateTime<Utc>) -> Vec<Value> {
    vec![
        timestamp_param("start", since),
        string_array_param(
            "ids",
            &distinct(rows.iter().map(|r| r.invocation_id.as_str())),
        ),
        string_array_param("names", &distinct(rows.iter().map(|r| r.name.as_str()))),
    ]
}

/// `@rows`: one JSON object per row, its `content` and `attributes` (with `author`) as JSON text.
fn insert_params(rows: &[JudgeRow], author: &str) -> Vec<Value> {
    let rows: Vec<String> = rows
        .iter()
        .map(|r| {
            json!({
                "trace_id": r.trace_id,
                "session_id": r.session_id,
                "invocation_id": r.invocation_id,
                "content": r.content().to_string(),
                "attributes": r.attributes(author).to_string(),
            })
            .to_string()
        })
        .collect();
    vec![string_array_param("rows", &rows)]
}

#[cfg(test)]
mod tests {
    use super::super::query_body;
    use super::super::tests::{QUERIES, reply, source};
    use super::*;
    use crate::testutil::{at, exchange, replay};

    const TABLE: &str = "`proj.analytics.agent_events`";

    const INSERT_SQL: &str = r"INSERT INTO `p.d.t` (timestamp, event_id, event_type, trace_id, session_id,
  invocation_id, user_id, status, is_truncated, content, attributes)
SELECT CURRENT_TIMESTAMP(), REPLACE(GENERATE_UUID(), '-', ''), 'FEEDBACK',
  JSON_VALUE(r, '$.trace_id'), JSON_VALUE(r, '$.session_id'),
  JSON_VALUE(r, '$.invocation_id'), NULL, 'OK', FALSE,
  PARSE_JSON(JSON_VALUE(r, '$.content'), wide_number_mode => 'round'),
  PARSE_JSON(JSON_VALUE(r, '$.attributes'), wide_number_mode => 'round')
FROM UNNEST(@rows) AS r_text, UNNEST([PARSE_JSON(r_text)]) AS r";

    const DENIED: &str = r#"{"error":{"code":403,"message":"Access Denied: Table proj:analytics.agent_events","status":"PERMISSION_DENIED"}}"#;

    fn row(name: &str, inv: &str, trace: Option<&str>, pass: bool) -> JudgeRow {
        JudgeRow::new(
            name.into(),
            4,
            pass,
            "polite".into(),
            trace.map(str::to_owned),
            format!("s-{inv}"),
            inv.into(),
        )
    }

    #[test]
    fn the_existing_query_text_and_parameters() {
        assert_eq!(
            existing_judge_sql("`p.d.t`"),
            "SELECT DISTINCT invocation_id, JSON_VALUE(content, '$.name') AS name\n\
             FROM `p.d.t`\n\
             WHERE event_type = 'FEEDBACK' AND JSON_VALUE(attributes, '$.source') = 'judge'\n  \
             AND timestamp >= @start\n  \
             AND invocation_id IN UNNEST(@ids) AND JSON_VALUE(content, '$.name') IN UNNEST(@names)"
        );
        let rows = [
            row("m/llm_judge", "inv-1", None, true),
            row("m/reference", "inv-1", None, false),
            row("m/llm_judge", "inv-2", None, true),
        ];
        assert_eq!(
            Value::Array(existing_params(&rows, at(1_763_337_600))),
            json!([
                {"name": "start", "parameterType": {"type": "TIMESTAMP"},
                 "parameterValue": {"value": "2025-11-17 00:00:00.000000+00:00"}},
                {"name": "ids",
                 "parameterType": {"type": "ARRAY", "arrayType": {"type": "STRING"}},
                 "parameterValue": {"arrayValues": [{"value": "inv-1"}, {"value": "inv-2"}]}},
                {"name": "names",
                 "parameterType": {"type": "ARRAY", "arrayType": {"type": "STRING"}},
                 "parameterValue": {"arrayValues": [{"value": "m/llm_judge"}, {"value": "m/reference"}]}},
            ])
        );
    }

    #[test]
    fn the_insert_text_and_its_one_json_object_per_row() {
        assert_eq!(insert_judge_sql("`p.d.t`"), INSERT_SQL);
        let rows = [
            row("m/llm_judge", "inv-1", Some("trace-1"), true),
            row("m/reference", "inv-2", None, false),
        ];
        let params = insert_params(&rows, "m");
        assert_eq!(params.len(), 1);
        assert_eq!(params[0]["name"], "rows");
        assert_eq!(
            params[0]["parameterType"],
            json!({"type": "ARRAY", "arrayType": {"type": "STRING"}})
        );
        let values: Vec<Value> = params[0]["parameterValue"]["arrayValues"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| serde_json::from_str(v["value"].as_str().unwrap()).unwrap())
            .collect();
        assert_eq!(
            values[0],
            json!({
                "trace_id": "trace-1",
                "session_id": "s-inv-1",
                "invocation_id": "inv-1",
                "content": rows[0].content().to_string(),
                "attributes": rows[0].attributes("m").to_string(),
            })
        );
        assert_eq!(values[1]["trace_id"], Value::Null);
        let content: Value = serde_json::from_str(values[1]["content"].as_str().unwrap()).unwrap();
        assert_eq!(
            content,
            json!({"name": "m/reference", "value": 4, "data_type": "number",
                   "comment": "fail: polite"})
        );
        let attributes: Value =
            serde_json::from_str(values[1]["attributes"].as_str().unwrap()).unwrap();
        assert_eq!(
            attributes,
            json!({"source": "judge", "author": "m", "adk": {"invocation_id": "inv-2"}})
        );
    }

    #[tokio::test]
    async fn existing_runs_one_query_under_the_sources_cap_and_reads_the_pairs() {
        let rows = [
            row("m/llm_judge", "inv-1", None, true),
            row("m/llm_judge", "inv-2", None, true),
        ];
        let since = at(1_763_337_600);
        let mut body = query_body(&existing_judge_sql(TABLE), existing_params(&rows, since));
        body["maximumBytesBilled"] = json!("5000");
        let dir = tempfile::tempdir().unwrap();
        let http = replay(
            dir.path(),
            &[exchange(
                "POST",
                QUERIES,
                &body.to_string(),
                200,
                &reply(
                    &["invocation_id", "name"],
                    &[vec![json!("inv-2"), json!("m/llm_judge")]],
                    None,
                ),
            )],
        );
        let found = source(http)
            .with_max_bytes_billed(5000)
            .existing(&rows, since)
            .await
            .unwrap();
        assert_eq!(
            found,
            HashSet::from([("inv-2".to_owned(), "m/llm_judge".to_owned())])
        );
    }

    #[tokio::test]
    async fn insert_runs_one_statement_billed_at_least_10_mib() {
        let rows = [row("m/llm_judge", "inv-1", Some("trace-1"), true)];
        let done = r#"{"jobComplete":true,"jobReference":{"projectId":"proj","jobId":"job_2","location":"US"},"numDmlAffectedRows":"1"}"#;
        for (cap, billed) in [
            (None, "10485760"),
            (Some(5_000), "10485760"),
            (Some(20_971_520), "20971520"),
        ] {
            let mut body = query_body(&insert_judge_sql(TABLE), insert_params(&rows, "m"));
            body["maximumBytesBilled"] = json!(billed);
            let dir = tempfile::tempdir().unwrap();
            let http = replay(
                dir.path(),
                &[exchange("POST", QUERIES, &body.to_string(), 200, done)],
            );
            let mut src = source(http);
            if let Some(c) = cap {
                src = src.with_max_bytes_billed(c);
            }
            src.insert(&rows, "m").await.unwrap();
        }
        assert_eq!(JUDGE_DML_MIN_BYTES, 10 * 1024 * 1024);
    }

    #[tokio::test]
    async fn failures_name_the_role_writing_needs() {
        let rows = [row("m/llm_judge", "inv-1", None, true)];
        let mut body = query_body(&insert_judge_sql(TABLE), insert_params(&rows, "m"));
        body["maximumBytesBilled"] = json!("10485760");
        let dir = tempfile::tempdir().unwrap();
        let http = replay(
            dir.path(),
            &[exchange("POST", QUERIES, &body.to_string(), 403, DENIED)],
        );
        let err = source(http).insert(&rows, "m").await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "transcript source: HTTP 403: Access Denied: Table proj:analytics.agent_events; \
             writing scores needs roles/bigquery.dataEditor on the table"
        );

        let body = query_body(&existing_judge_sql(TABLE), existing_params(&rows, at(0)));
        let dir = tempfile::tempdir().unwrap();
        let http = replay(
            dir.path(),
            &[exchange("POST", QUERIES, &body.to_string(), 403, DENIED)],
        );
        let err = source(http).existing(&rows, at(0)).await.unwrap_err();
        assert!(
            matches!(&err, MonitorError::Source(m) if m.ends_with(WRITE_HINT)),
            "{err}"
        );
    }

    #[tokio::test]
    async fn no_rows_send_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let src = source(replay(dir.path(), &[]));
        assert!(src.existing(&[], at(0)).await.unwrap().is_empty());
        src.insert(&[], "m").await.unwrap();
    }
}
