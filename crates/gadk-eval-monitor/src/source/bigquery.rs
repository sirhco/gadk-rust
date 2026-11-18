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

//! The BigQuery transcript source: lists runs from the agent-analytics table.
//!
//! Each [`TranscriptSource::list`] call reads the table's schema (`tables.get`), then runs two
//! parameterised queries: the newest invocations that started in the window (from their
//! `INVOCATION_STARTING` rows), then the mapped rows of those invocations (from 10 minutes before
//! the window, since the user message is logged just before the invocation starts), which
//! [`build_run`] turns into transcripts. Payload columns left out of the table by the analytics
//! plugin's `payload_column_denylist` are selected as NULL: missing `content` flags every run
//! incomplete, and missing `attributes` makes the `agent` column the run's agent (an `agent`
//! filter then fails). Rows with equal timestamps (parallel
//! tool calls, for example) are ordered by `event_id`, which is random: the order is
//! deterministic but not causal. Older tables without an `event_id` column leave such ties in
//! no particular order.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use gadk_model::HttpClient;
use http::Method;
use serde_json::{Map, Value, json};

use crate::error::MonitorError;
use crate::gcp::{AdcTokenSource, GcpHttp, TokenSource, encode};
use crate::source::mapping::{EventRow, MAPPED_EVENT_TYPES, RunHeader, build_run};
use crate::source::{ProductionRun, TranscriptQuery, TranscriptSource};
use crate::spec::{is_dataset_or_table, is_project_id};

const ENDPOINT: &str = "https://bigquery.googleapis.com/bigquery/v2";

/// How long one `jobs.query` / `jobs.getQueryResults` call waits for the job, in ms.
const WAIT_MS: u64 = 10_000;

/// The most `jobs.getQueryResults` polls for a job that has not completed.
const MAX_POLLS: usize = 30;

/// Reads production runs from an agent-analytics table.
pub struct BigQueryTranscriptSource {
    project: String,
    dataset: String,
    table_id: String,
    /// The quoted table reference, `` `project.dataset.table` ``.
    table: String,
    gcp: GcpHttp,
}

/// Which optional columns the table has: the payload columns (the analytics plugin can leave
/// each out) and `event_id` (missing from tables created before it was added).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Columns {
    pub(crate) content: bool,
    pub(crate) content_parts: bool,
    pub(crate) attributes: bool,
    pub(crate) event_id: bool,
}

impl Columns {
    /// The optional columns named in a `tables.get` reply's `schema.fields`.
    pub(crate) fn from_schema(table: &Value) -> Self {
        let names: Vec<&str> = table
            .pointer("/schema/fields")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|f| f.get("name").and_then(Value::as_str))
            .collect();
        Self {
            content: names.contains(&"content"),
            content_parts: names.contains(&"content_parts"),
            attributes: names.contains(&"attributes"),
            event_id: names.contains(&"event_id"),
        }
    }
}

impl BigQueryTranscriptSource {
    /// The table `project.dataset.table`, read with Application Default Credentials. Must be
    /// called inside a Tokio runtime.
    ///
    /// # Errors
    ///
    /// [`MonitorError::Source`] for an invalid identifier or when no credentials are found.
    pub fn new(project: &str, dataset: &str, table: &str) -> Result<Self, MonitorError> {
        let auth = AdcTokenSource::adc()
            .map_err(|e| MonitorError::Source(format!("Application Default Credentials: {e}")))?;
        Self::with_client(
            project,
            dataset,
            table,
            GcpHttp::default_http(),
            Arc::new(auth),
        )
    }

    /// Like [`BigQueryTranscriptSource::new`], with an explicit HTTP client and credentials.
    ///
    /// # Errors
    ///
    /// [`MonitorError::Source`] when `project` is not `[A-Za-z0-9_-]+` or `dataset` / `table`
    /// is not `[A-Za-z0-9_]+`.
    pub fn with_client(
        project: &str,
        dataset: &str,
        table: &str,
        http: Arc<dyn HttpClient>,
        auth: Arc<dyn TokenSource>,
    ) -> Result<Self, MonitorError> {
        if !is_project_id(project) {
            return Err(MonitorError::Source(format!(
                "invalid project {project:?} (expected [A-Za-z0-9_-]+)"
            )));
        }
        for (what, name) in [("dataset", dataset), ("table", table)] {
            if !is_dataset_or_table(name) {
                return Err(MonitorError::Source(format!(
                    "invalid {what} {name:?} (expected [A-Za-z0-9_]+)"
                )));
            }
        }
        Ok(Self {
            project: project.to_owned(),
            dataset: dataset.to_owned(),
            table_id: table.to_owned(),
            table: format!("`{project}.{dataset}.{table}`"),
            gcp: GcpHttp::new(http, auth),
        })
    }

    /// The payload columns of the table (`tables.get`).
    async fn columns(&self) -> Result<Columns, MonitorError> {
        let url = format!(
            "{ENDPOINT}/projects/{}/datasets/{}/tables/{}",
            self.project, self.dataset, self.table_id
        );
        let table = self
            .gcp
            .json(Method::GET, &url, None)
            .await
            .map_err(MonitorError::Source)?;
        Ok(Columns::from_schema(&table))
    }

    /// Runs `sql` with `params` and returns every result row as a column-name → cell map.
    async fn query(
        &self,
        sql: &str,
        params: Vec<Value>,
    ) -> Result<Vec<Map<String, Value>>, MonitorError> {
        let source = |m: String| MonitorError::Source(m);
        let url = format!("{ENDPOINT}/projects/{}/queries", self.project);
        let mut resp = self
            .gcp
            .json(Method::POST, &url, Some(&query_body(sql, params)))
            .await
            .map_err(source)?;
        let mut rows = Vec::new();
        let mut names: Vec<String> = Vec::new();
        let mut polls = 0;
        loop {
            let complete = resp.get("jobComplete").and_then(Value::as_bool) == Some(true);
            if let Some(message) = job_error(&resp, complete) {
                return Err(source(format!("query failed: {message}")));
            }
            let page = resp
                .get("pageToken")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if complete {
                if let Some(fields) = resp.pointer("/schema/fields").and_then(Value::as_array) {
                    names = fields
                        .iter()
                        .map(|f| {
                            f.get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_owned()
                        })
                        .collect();
                }
                for row in resp
                    .get("rows")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    rows.push(decode_row(&names, row));
                }
                if page.is_none() {
                    return Ok(rows);
                }
            } else {
                polls += 1;
                if polls > MAX_POLLS {
                    return Err(source(format!(
                        "query did not complete after {MAX_POLLS} polls"
                    )));
                }
            }
            let url = results_url(&resp, page.as_deref()).ok_or_else(|| {
                source("query reply has no jobReference to read results from".into())
            })?;
            resp = self
                .gcp
                .json(Method::GET, &url, None)
                .await
                .map_err(source)?;
        }
    }
}

#[async_trait::async_trait]
impl TranscriptSource for BigQueryTranscriptSource {
    async fn list(&self, q: &TranscriptQuery) -> Result<Vec<ProductionRun>, MonitorError> {
        if q.limit == 0 {
            return Ok(Vec::new());
        }
        let columns = self.columns().await?;
        if q.agent.is_some() && !columns.attributes {
            return Err(MonitorError::Source(format!(
                "the `agent` filter needs the `attributes` column, which {} does not have \
                 (payload_column_denylist)",
                self.table
            )));
        }
        let invocations = self
            .query(
                &invocations_sql(&self.table, q.agent.is_some(), &columns),
                invocation_params(q),
            )
            .await?;
        let mut headers = Vec::with_capacity(invocations.len());
        for r in &invocations {
            headers.push(RunHeader {
                invocation_id: cell_str(r, "invocation_id").unwrap_or_default(),
                session_id: cell_str(r, "session_id").unwrap_or_default(),
                agent: cell_str(r, "agent").unwrap_or_default(),
                started_at: cell_timestamp(r, "started_at")?,
            });
        }
        if headers.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<String> = headers.iter().map(|h| h.invocation_id.clone()).collect();
        let mut rows: HashMap<String, Vec<EventRow>> = HashMap::new();
        for r in self
            .query(&rows_sql(&self.table, &columns), row_params(&ids, q.from))
            .await?
        {
            let id = cell_str(&r, "invocation_id").unwrap_or_default();
            rows.entry(id).or_default().push(EventRow {
                event_type: cell_str(&r, "event_type").unwrap_or_default(),
                timestamp: cell_timestamp(&r, "timestamp")?,
                content: cell_json(&r, "content"),
                content_parts: match cell_json(&r, "content_parts") {
                    Some(Value::Array(a)) => a,
                    _ => Vec::new(),
                },
                attributes: cell_json(&r, "attributes"),
                is_truncated: cell_str(&r, "is_truncated").as_deref() == Some("true"),
                trace_id: cell_str(&r, "trace_id").filter(|t| !t.is_empty()),
            });
        }
        let mut runs: Vec<ProductionRun> = headers
            .into_iter()
            .map(|h| {
                let rows = rows.remove(&h.invocation_id).unwrap_or_default();
                build_run(h, &rows)
            })
            .collect();
        runs.sort_by_key(|r| std::cmp::Reverse(r.started_at));
        runs.truncate(q.limit);
        Ok(runs)
    }
}

/// The first query: the newest `@limit` invocations whose first `INVOCATION_STARTING` row is in
/// `[@from, @to)`, optionally only those of root agent `@agent` (which needs `attributes`). The
/// run's agent is `attributes.root_agent_name`, or the `agent` column without `attributes`.
pub(crate) fn invocations_sql(table: &str, with_agent: bool, columns: &Columns) -> String {
    let agent_expr = if columns.attributes {
        "ANY_VALUE(JSON_VALUE(attributes, '$.root_agent_name'))"
    } else {
        "ANY_VALUE(agent)"
    };
    let agent = if with_agent {
        "\n  AND JSON_VALUE(attributes, '$.root_agent_name') = @agent"
    } else {
        ""
    };
    format!(
        "SELECT invocation_id, MIN(timestamp) AS started_at, \
         ANY_VALUE(session_id) AS session_id, {agent_expr} AS agent\n\
         FROM {table}\n\
         WHERE event_type = 'INVOCATION_STARTING'\n  \
         AND timestamp >= @from AND timestamp < @to{agent}\n\
         GROUP BY invocation_id\n\
         ORDER BY started_at DESC\n\
         LIMIT @limit"
    )
}

/// The second query: the mapped rows of `@invocation_ids` from 10 minutes before `@from` on, in
/// log order (ties broken by `event_id` when the table has it); payload columns the table lacks
/// are NULL.
pub(crate) fn rows_sql(table: &str, columns: &Columns) -> String {
    let types: Vec<String> = MAPPED_EVENT_TYPES
        .iter()
        .map(|t| format!("'{t}'"))
        .collect();
    let pick = |present: bool, expr: &str, name: &str| {
        if present {
            expr.to_owned()
        } else {
            format!("NULL AS {name}")
        }
    };
    format!(
        "SELECT invocation_id, event_type, timestamp, {}, {}, {}, \
         IFNULL(is_truncated, FALSE) AS is_truncated, trace_id\n\
         FROM {table}\n\
         WHERE invocation_id IN UNNEST(@invocation_ids)\n  \
         AND timestamp >= TIMESTAMP_SUB(@from, INTERVAL 10 MINUTE)\n  \
         AND event_type IN ({})\n\
         ORDER BY invocation_id, timestamp{}",
        pick(columns.content, "content", "content"),
        pick(
            columns.content_parts,
            "TO_JSON_STRING(content_parts) AS content_parts",
            "content_parts"
        ),
        pick(columns.attributes, "attributes", "attributes"),
        types.join(", "),
        if columns.event_id { ", event_id" } else { "" }
    )
}

/// The error of a `jobs.query` / `jobs.getQueryResults` reply: `status.errorResult` or a top-level
/// `error`, or the first of `errors` when a completed job returned no schema. Other `errors`
/// entries are warnings and are ignored.
fn job_error(resp: &Value, complete: bool) -> Option<String> {
    let message = |v: &Value| {
        v.get("message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    if let Some(e) = resp
        .pointer("/status/errorResult")
        .or_else(|| resp.get("error"))
    {
        return Some(message(e));
    }
    let first = resp.get("errors").and_then(Value::as_array)?.first()?;
    (complete && resp.get("schema").is_none()).then(|| message(first))
}

fn param(name: &str, kind: &str, value: String) -> Value {
    json!({"name": name, "parameterType": {"type": kind}, "parameterValue": {"value": value}})
}

fn timestamp_param(name: &str, t: DateTime<Utc>) -> Value {
    param(
        name,
        "TIMESTAMP",
        t.format("%Y-%m-%d %H:%M:%S%.6f+00:00").to_string(),
    )
}

pub(crate) fn invocation_params(q: &TranscriptQuery) -> Vec<Value> {
    let mut params = vec![
        timestamp_param("from", q.from),
        timestamp_param("to", q.to),
        param("limit", "INT64", q.limit.to_string()),
    ];
    if let Some(agent) = &q.agent {
        params.push(param("agent", "STRING", agent.clone()));
    }
    params
}

pub(crate) fn row_params(ids: &[String], from: DateTime<Utc>) -> Vec<Value> {
    let values: Vec<Value> = ids.iter().map(|id| json!({"value": id})).collect();
    vec![
        json!({
            "name": "invocation_ids",
            "parameterType": {"type": "ARRAY", "arrayType": {"type": "STRING"}},
            "parameterValue": {"arrayValues": values},
        }),
        timestamp_param("from", from),
    ]
}

/// The `jobs.query` request body.
pub(crate) fn query_body(sql: &str, params: Vec<Value>) -> Value {
    json!({
        "query": sql,
        "useLegacySql": false,
        "parameterMode": "NAMED",
        "queryParameters": params,
        "timeoutMs": WAIT_MS,
        "formatOptions": {"useInt64Timestamp": true},
    })
}

/// The `jobs.getQueryResults` URL for the job in `resp`, at `page` when given.
fn results_url(resp: &Value, page: Option<&str>) -> Option<String> {
    let job = resp.get("jobReference")?;
    let project = job.get("projectId")?.as_str()?;
    let id = job.get("jobId")?.as_str()?;
    let mut url = format!(
        "{ENDPOINT}/projects/{}/queries/{}?timeoutMs={WAIT_MS}&formatOptions.useInt64Timestamp=true",
        encode(project),
        encode(id)
    );
    if let Some(location) = job.get("location").and_then(Value::as_str) {
        url.push_str(&format!("&location={}", encode(location)));
    }
    if let Some(page) = page {
        url.push_str(&format!("&pageToken={}", encode(page)));
    }
    Some(url)
}

/// A result row (`{"f": [{"v": …}, …]}`) as a map from the column `names` to the cell values.
fn decode_row(names: &[String], row: &Value) -> Map<String, Value> {
    let cells = row.get("f").and_then(Value::as_array);
    names
        .iter()
        .zip(cells.into_iter().flatten())
        .map(|(n, c)| (n.clone(), c.get("v").cloned().unwrap_or(Value::Null)))
        .collect()
}

fn cell_str(row: &Map<String, Value>, name: &str) -> Option<String> {
    row.get(name).and_then(Value::as_str).map(str::to_owned)
}

/// A TIMESTAMP cell: integer microseconds (`useInt64Timestamp`) or floating-point seconds.
fn cell_timestamp(row: &Map<String, Value>, name: &str) -> Result<DateTime<Utc>, MonitorError> {
    let raw = cell_str(row, name).unwrap_or_default();
    let micros = raw
        .parse::<i64>()
        .ok()
        .or_else(|| raw.parse::<f64>().ok().map(|s| (s * 1e6).round() as i64));
    micros
        .and_then(DateTime::from_timestamp_micros)
        .ok_or_else(|| MonitorError::Source(format!("invalid TIMESTAMP {raw:?} in `{name}`")))
}

/// A JSON cell: the JSON text decoded (`None` for NULL or JSON `null`); text that is not JSON
/// stays a string.
fn cell_json(row: &Map<String, Value>, name: &str) -> Option<Value> {
    match row.get(name)? {
        Value::Null => None,
        Value::String(s) => match serde_json::from_str::<Value>(s) {
            Ok(Value::Null) => None,
            Ok(v) => Some(v),
            Err(_) => Some(Value::String(s.clone())),
        },
        other => Some(other.clone()),
    }
}

#[cfg(test)]
mod tests {
    use crate::gcp::NoAuth;
    use crate::testutil::{Exchange, at, exchange, replay};

    use super::*;

    const QUERIES: &str = "https://bigquery.googleapis.com/bigquery/v2/projects/proj/queries";
    const TABLE: &str = "https://bigquery.googleapis.com/bigquery/v2/projects/proj/datasets/analytics/tables/agent_events";

    const ALL: Columns = Columns {
        content: true,
        content_parts: true,
        attributes: true,
        event_id: true,
    };

    /// A `tables.get` reply whose schema has the analytics columns minus `denied`.
    fn table_reply(denied: &[&str]) -> Exchange {
        let fields: Vec<Value> = [
            "timestamp",
            "event_type",
            "agent",
            "session_id",
            "invocation_id",
            "user_id",
            "trace_id",
            "span_id",
            "parent_span_id",
            "content",
            "content_parts",
            "attributes",
            "latency_ms",
            "status",
            "error_message",
            "is_truncated",
            "event_id",
        ]
        .iter()
        .filter(|n| !denied.contains(n))
        .map(|n| json!({"name": n}))
        .collect();
        exchange(
            "GET",
            TABLE,
            "",
            200,
            &json!({"tableReference": {"projectId": "proj", "datasetId": "analytics",
                    "tableId": "agent_events"}, "schema": {"fields": fields}})
            .to_string(),
        )
    }

    fn query(agent: Option<&str>, limit: usize) -> TranscriptQuery {
        TranscriptQuery {
            from: at(1_763_337_600),
            to: at(1_763_424_000),
            agent: agent.map(str::to_owned),
            limit,
        }
    }

    fn source(http: Arc<dyn HttpClient>) -> BigQueryTranscriptSource {
        BigQueryTranscriptSource::with_client(
            "proj",
            "analytics",
            "agent_events",
            http,
            Arc::new(NoAuth),
        )
        .unwrap()
    }

    /// A completed `jobs.query` / `jobs.getQueryResults` reply with `rows` of `names` columns.
    fn reply(names: &[&str], rows: &[Vec<Value>], page: Option<&str>) -> String {
        let fields: Vec<Value> = names.iter().map(|n| json!({"name": n})).collect();
        let rows: Vec<Value> = rows
            .iter()
            .map(|r| json!({"f": r.iter().map(|v| json!({"v": v})).collect::<Vec<_>>()}))
            .collect();
        let mut v = json!({
            "jobComplete": true,
            "jobReference": {"projectId": "proj", "jobId": "job_1", "location": "US"},
            "schema": {"fields": fields},
            "rows": rows,
        });
        if let Some(p) = page {
            v["pageToken"] = json!(p);
        }
        v.to_string()
    }

    const HEAD: [&str; 4] = ["invocation_id", "started_at", "session_id", "agent"];
    const ROW: [&str; 8] = [
        "invocation_id",
        "event_type",
        "timestamp",
        "content",
        "content_parts",
        "attributes",
        "is_truncated",
        "trace_id",
    ];

    /// A row-query result row: `micros` since the epoch, `content` as JSON text.
    fn row(id: &str, event_type: &str, micros: i64, content: Value, trace: &str) -> Vec<Value> {
        vec![
            json!(id),
            json!(event_type),
            json!(micros.to_string()),
            if content.is_null() {
                Value::Null
            } else {
                json!(content.to_string())
            },
            json!(r#"[{"storage_mode":"INLINE","part_index":0}]"#),
            json!(r#"{"root_agent_name":"support"}"#),
            json!("false"),
            json!(trace),
        ]
    }

    #[test]
    fn invocation_query_text_and_parameters() {
        let body = query_body(
            &invocations_sql("`proj.analytics.agent_events`", true, &ALL),
            invocation_params(&query(Some("support"), 100)),
        );
        assert_eq!(
            body,
            json!({
                "query": "SELECT invocation_id, MIN(timestamp) AS started_at, \
                          ANY_VALUE(session_id) AS session_id, \
                          ANY_VALUE(JSON_VALUE(attributes, '$.root_agent_name')) AS agent\n\
                          FROM `proj.analytics.agent_events`\n\
                          WHERE event_type = 'INVOCATION_STARTING'\n  \
                          AND timestamp >= @from AND timestamp < @to\n  \
                          AND JSON_VALUE(attributes, '$.root_agent_name') = @agent\n\
                          GROUP BY invocation_id\n\
                          ORDER BY started_at DESC\n\
                          LIMIT @limit",
                "useLegacySql": false,
                "parameterMode": "NAMED",
                "queryParameters": [
                    {"name": "from", "parameterType": {"type": "TIMESTAMP"},
                     "parameterValue": {"value": "2025-11-17 00:00:00.000000+00:00"}},
                    {"name": "to", "parameterType": {"type": "TIMESTAMP"},
                     "parameterValue": {"value": "2025-11-18 00:00:00.000000+00:00"}},
                    {"name": "limit", "parameterType": {"type": "INT64"},
                     "parameterValue": {"value": "100"}},
                    {"name": "agent", "parameterType": {"type": "STRING"},
                     "parameterValue": {"value": "support"}},
                ],
                "timeoutMs": 10000,
                "formatOptions": {"useInt64Timestamp": true},
            })
        );
        let no_agent = invocations_sql("`p.d.t`", false, &ALL);
        assert!(!no_agent.contains("@agent"), "{no_agent}");
        assert_eq!(invocation_params(&query(None, 5)).len(), 3);
    }

    #[test]
    fn row_query_text_and_parameters() {
        assert_eq!(
            rows_sql("`proj.analytics.agent_events`", &ALL),
            "SELECT invocation_id, event_type, timestamp, content, \
             TO_JSON_STRING(content_parts) AS content_parts, attributes, \
             IFNULL(is_truncated, FALSE) AS is_truncated, trace_id\n\
             FROM `proj.analytics.agent_events`\n\
             WHERE invocation_id IN UNNEST(@invocation_ids)\n  \
             AND timestamp >= TIMESTAMP_SUB(@from, INTERVAL 10 MINUTE)\n  \
             AND event_type IN ('USER_MESSAGE_RECEIVED', 'AGENT_RESPONSE', 'TOOL_STARTING', \
             'AGENT_ERROR', 'INVOCATION_ERROR')\n\
             ORDER BY invocation_id, timestamp, event_id"
        );
        assert_eq!(
            Value::Array(row_params(&["a".into(), "b".into()], at(0))),
            json!([
                {"name": "invocation_ids",
                 "parameterType": {"type": "ARRAY", "arrayType": {"type": "STRING"}},
                 "parameterValue": {"arrayValues": [{"value": "a"}, {"value": "b"}]}},
                {"name": "from", "parameterType": {"type": "TIMESTAMP"},
                 "parameterValue": {"value": "1970-01-01 00:00:00.000000+00:00"}},
            ])
        );
    }

    #[test]
    fn identifiers_are_validated() {
        let http: Arc<dyn HttpClient> = Arc::new(gadk_model::ReqwestClient::default());
        for (p, d, t) in [
            ("p.q", "d", "t"),
            ("p", "d-x", "t"),
            ("p", "d", "t`; DROP TABLE x; --"),
            ("", "d", "t"),
        ] {
            let r = BigQueryTranscriptSource::with_client(p, d, t, http.clone(), Arc::new(NoAuth));
            assert!(matches!(r, Err(MonitorError::Source(_))), "{p} {d} {t}");
        }
    }

    #[tokio::test]
    async fn lists_runs_through_a_paged_query() {
        let q = query(None, 10);
        let table = "`proj.analytics.agent_events`";
        let invocations = query_body(&invocations_sql(table, false, &ALL), invocation_params(&q));
        let ids = ["inv-new".to_owned(), "inv-old".to_owned()];
        let rows = query_body(&rows_sql(table, &ALL), row_params(&ids, q.from));
        let results = "https://bigquery.googleapis.com/bigquery/v2/projects/proj/queries/job_1\
                       ?timeoutMs=10000&formatOptions.useInt64Timestamp=true&location=US";
        let dir = tempfile::tempdir().unwrap();
        let http = replay(
            dir.path(),
            &[
                table_reply(&[]),
                exchange(
                    "POST",
                    QUERIES,
                    &invocations.to_string(),
                    200,
                    &reply(
                        &HEAD,
                        &[
                            vec![
                                json!("inv-new"),
                                json!("1763350000000000"),
                                json!("s-2"),
                                json!("support"),
                            ],
                            vec![
                                json!("inv-old"),
                                json!("1763340000000000"),
                                json!("s-1"),
                                json!("support"),
                            ],
                        ],
                        None,
                    ),
                ),
                exchange(
                    "POST",
                    QUERIES,
                    &rows.to_string(),
                    200,
                    r#"{"jobComplete":false,"jobReference":{"projectId":"proj","jobId":"job_1","location":"US"}}"#,
                ),
                exchange(
                    "GET",
                    results,
                    "",
                    200,
                    &reply(
                        &ROW,
                        &[
                            row(
                                "inv-new",
                                "USER_MESSAGE_RECEIVED",
                                1_763_350_000_000_001,
                                json!({"text_summary": "Where is order 7?"}),
                                "trace-n",
                            ),
                            row(
                                "inv-new",
                                "TOOL_STARTING",
                                1_763_350_000_000_002,
                                json!({"tool": "lookup_order", "args": {"id": 7}, "tool_origin": "LOCAL"}),
                                "trace-n",
                            ),
                        ],
                        Some("page 2"),
                    ),
                ),
                exchange(
                    "GET",
                    &format!("{results}&pageToken=page%202"),
                    "",
                    200,
                    &reply(
                        &ROW,
                        &[
                            row(
                                "inv-new",
                                "AGENT_RESPONSE",
                                1_763_350_000_000_003,
                                json!({"response": "text: 'It ships today.'"}),
                                "trace-n",
                            ),
                            row(
                                "inv-old",
                                "USER_MESSAGE_RECEIVED",
                                1_763_340_000_000_001,
                                json!({"text_summary": "hi"}),
                                "",
                            ),
                            row(
                                "inv-old",
                                "AGENT_RESPONSE",
                                1_763_340_000_000_002,
                                Value::Null,
                                "",
                            ),
                        ],
                        None,
                    ),
                ),
            ],
        );
        let runs = source(http).list(&q).await.unwrap();
        let ids: Vec<&str> = runs.iter().map(|r| r.invocation_id.as_str()).collect();
        assert_eq!(ids, ["inv-new", "inv-old"]);
        let new = &runs[0];
        assert_eq!(new.session_id, "s-2");
        assert_eq!(new.agent, "support");
        assert_eq!(new.trace_id.as_deref(), Some("trace-n"));
        assert_eq!(
            new.started_at,
            DateTime::from_timestamp_micros(1_763_350_000_000_000).unwrap()
        );
        assert!(new.content_complete, "{:?}", new.notes);
        let turn = &new.transcript.turns[0];
        assert_eq!(turn.user, "Where is order 7?");
        assert_eq!(turn.reply, "It ships today.");
        assert_eq!(turn.calls[0].name, "lookup_order");
        assert_eq!(Value::Object(turn.calls[0].args.clone()), json!({"id": 7}));
        let old = &runs[1];
        assert_eq!(old.trace_id, None);
        assert!(!old.content_complete);
        assert!(old.notes[0].ends_with("content is NULL"), "{:?}", old.notes);
    }

    #[tokio::test]
    async fn no_invocations_means_one_query() {
        let q = query(Some("support"), 3);
        let body = query_body(
            &invocations_sql("`proj.analytics.agent_events`", true, &ALL),
            invocation_params(&q),
        );
        let dir = tempfile::tempdir().unwrap();
        let http = replay(
            dir.path(),
            &[
                table_reply(&[]),
                exchange(
                    "POST",
                    QUERIES,
                    &body.to_string(),
                    200,
                    &reply(&HEAD, &[], None),
                ),
            ],
        );
        assert!(source(http).list(&q).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_zero_limit_sends_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let http = replay(dir.path(), &[]);
        assert!(source(http).list(&query(None, 0)).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn http_and_job_errors_are_source_errors() {
        let q = query(None, 3);
        let body = query_body(
            &invocations_sql("`proj.analytics.agent_events`", false, &ALL),
            invocation_params(&q),
        )
        .to_string();
        let dir = tempfile::tempdir().unwrap();
        let http = replay(
            dir.path(),
            &[
                table_reply(&[]),
                exchange(
                    "POST",
                    QUERIES,
                    &body,
                    403,
                    r#"{"error":{"code":403,"message":"Access Denied: Table proj:analytics.agent_events","status":"PERMISSION_DENIED"}}"#,
                ),
            ],
        );
        let err = source(http).list(&q).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "transcript source: HTTP 403: Access Denied: Table proj:analytics.agent_events"
        );
        let dir = tempfile::tempdir().unwrap();
        let http = replay(
            dir.path(),
            &[
                table_reply(&[]),
                exchange(
                    "POST",
                    QUERIES,
                    &body,
                    200,
                    r#"{"jobComplete":true,"errors":[{"reason":"invalidQuery","message":"Unrecognized name: attributes"}]}"#,
                ),
            ],
        );
        let err = source(http).list(&q).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "transcript source: query failed: Unrecognized name: attributes"
        );
    }

    #[test]
    fn denied_payload_columns_are_selected_as_null() {
        let none = Columns {
            content: false,
            content_parts: false,
            attributes: false,
            event_id: true,
        };
        assert_eq!(
            rows_sql("`p.d.t`", &none),
            "SELECT invocation_id, event_type, timestamp, NULL AS content, \
             NULL AS content_parts, NULL AS attributes, \
             IFNULL(is_truncated, FALSE) AS is_truncated, trace_id\n\
             FROM `p.d.t`\n\
             WHERE invocation_id IN UNNEST(@invocation_ids)\n  \
             AND timestamp >= TIMESTAMP_SUB(@from, INTERVAL 10 MINUTE)\n  \
             AND event_type IN ('USER_MESSAGE_RECEIVED', 'AGENT_RESPONSE', 'TOOL_STARTING', \
             'AGENT_ERROR', 'INVOCATION_ERROR')\n\
             ORDER BY invocation_id, timestamp, event_id"
        );
        let sql = invocations_sql("`p.d.t`", false, &none);
        assert!(sql.contains("ANY_VALUE(agent) AS agent"), "{sql}");
        assert!(!sql.contains("attributes"), "{sql}");
        assert_eq!(
            Columns::from_schema(&json!({"schema": {"fields": [
                {"name": "content"}, {"name": "attributes"}, {"name": "timestamp"}
            ]}})),
            Columns {
                content: true,
                content_parts: false,
                attributes: true,
                event_id: false,
            }
        );
    }

    #[test]
    fn tables_without_event_id_order_by_timestamp_only() {
        let old = Columns {
            event_id: false,
            ..ALL
        };
        let sql = rows_sql("`p.d.t`", &old);
        assert!(sql.ends_with("ORDER BY invocation_id, timestamp"), "{sql}");
        assert!(!sql.contains("event_id"), "{sql}");
        assert!(rows_sql("`p.d.t`", &ALL).ends_with("ORDER BY invocation_id, timestamp, event_id"));
        assert!(
            !Columns::from_schema(&json!({"schema": {"fields": [{"name": "content"}]}})).event_id
        );
    }

    #[tokio::test]
    async fn the_agent_filter_needs_the_attributes_column() {
        let dir = tempfile::tempdir().unwrap();
        let http = replay(dir.path(), &[table_reply(&["attributes"])]);
        let err = source(http)
            .list(&query(Some("support"), 3))
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "transcript source: the `agent` filter needs the `attributes` column, which \
             `proj.analytics.agent_events` does not have (payload_column_denylist)"
        );
    }

    #[tokio::test]
    async fn denied_columns_read_as_null_and_flag_the_run() {
        let q = query(None, 3);
        let tbl = "`proj.analytics.agent_events`";
        let denied = Columns {
            content: false,
            content_parts: false,
            attributes: false,
            event_id: true,
        };
        let invocations = query_body(&invocations_sql(tbl, false, &denied), invocation_params(&q));
        let rows = query_body(
            &rows_sql(tbl, &denied),
            row_params(&["inv".to_owned()], q.from),
        );
        let dir = tempfile::tempdir().unwrap();
        let http = replay(
            dir.path(),
            &[
                table_reply(&["content", "content_parts", "attributes"]),
                exchange(
                    "POST",
                    QUERIES,
                    &invocations.to_string(),
                    200,
                    &reply(
                        &HEAD,
                        &[vec![
                            json!("inv"),
                            json!("1763340000000000"),
                            json!("s"),
                            json!("support_agent"),
                        ]],
                        None,
                    ),
                ),
                exchange(
                    "POST",
                    QUERIES,
                    &rows.to_string(),
                    200,
                    &reply(
                        &ROW,
                        &[vec![
                            json!("inv"),
                            json!("USER_MESSAGE_RECEIVED"),
                            json!("1763340000000001"),
                            Value::Null,
                            Value::Null,
                            Value::Null,
                            json!("false"),
                            json!("t"),
                        ]],
                        None,
                    ),
                ),
            ],
        );
        let runs = source(http).list(&q).await.unwrap();
        assert_eq!(runs[0].agent, "support_agent");
        assert!(!runs[0].content_complete);
        assert!(
            runs[0].notes[0].ends_with("content is NULL"),
            "{:?}",
            runs[0].notes
        );
    }

    #[tokio::test]
    async fn job_warnings_are_ignored_and_error_results_are_fatal() {
        let q = query(None, 3);
        let body = query_body(
            &invocations_sql("`proj.analytics.agent_events`", false, &ALL),
            invocation_params(&q),
        )
        .to_string();
        let mut warned: Value = serde_json::from_str(&reply(&HEAD, &[], None)).unwrap();
        warned["errors"] = json!([{"reason": "warning", "message": "slot contention"}]);
        let dir = tempfile::tempdir().unwrap();
        let http = replay(
            dir.path(),
            &[
                table_reply(&[]),
                exchange("POST", QUERIES, &body, 200, &warned.to_string()),
            ],
        );
        assert!(source(http).list(&q).await.unwrap().is_empty());
        let mut failed: Value = serde_json::from_str(&reply(&HEAD, &[], None)).unwrap();
        failed["status"] = json!({"errorResult": {"reason": "invalidQuery", "message": "boom"}});
        let dir = tempfile::tempdir().unwrap();
        let http = replay(
            dir.path(),
            &[
                table_reply(&[]),
                exchange("POST", QUERIES, &body, 200, &failed.to_string()),
            ],
        );
        assert_eq!(
            source(http).list(&q).await.unwrap_err().to_string(),
            "transcript source: query failed: boom"
        );
    }

    #[test]
    fn timestamp_cells_accept_micros_and_float_seconds() {
        let row: Map<String, Value> = [
            ("a".to_owned(), json!("1763350000000123")),
            ("b".to_owned(), json!("1.763350000000123E9")),
            ("c".to_owned(), json!("soon")),
        ]
        .into_iter()
        .collect();
        let want = DateTime::from_timestamp_micros(1_763_350_000_000_123).unwrap();
        assert_eq!(cell_timestamp(&row, "a").unwrap(), want);
        assert_eq!(
            cell_timestamp(&row, "b").unwrap().timestamp(),
            want.timestamp()
        );
        assert!(cell_timestamp(&row, "c").is_err());
    }

    #[test]
    fn json_cells() {
        let row: Map<String, Value> = [
            ("obj".to_owned(), json!(r#"{"a":1}"#)),
            ("null_json".to_owned(), json!("null")),
            ("sql_null".to_owned(), Value::Null),
            ("text".to_owned(), json!("plain text")),
        ]
        .into_iter()
        .collect();
        assert_eq!(cell_json(&row, "obj"), Some(json!({"a": 1})));
        assert_eq!(cell_json(&row, "null_json"), None);
        assert_eq!(cell_json(&row, "sql_null"), None);
        assert_eq!(cell_json(&row, "missing"), None);
        assert_eq!(cell_json(&row, "text"), Some(json!("plain text")));
    }
}
