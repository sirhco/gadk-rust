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

//! The BigQuery trace source: traces read from the agent-analytics table (`agent_events`).
//!
//! Rows that share a `trace_id` are one trace; rows that share a `span_id` are one span; every
//! row is also one log entry of its span. Rows are never joined to Cloud Logging.

use chrono::{DateTime, Utc};
use gadk_bigquery::{BigQueryClient, BigQueryError, QueryOptions};
use serde_json::{Map, Value, json};

use super::{SourceError, TraceId, TracePage, TraceQuery, TraceSource, TraceSummary};

/// The URL form `--traces` accepts.
const URL_FORM: &str = "bigquery://<project>.<dataset>.<table>";

/// A BigQuery table: `project.dataset.table`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct BigQueryTable {
    /// The project id, optionally domain-scoped (`example.com:proj`).
    pub project: String,
    /// The dataset id.
    pub dataset: String,
    /// The table id.
    pub table: String,
}

impl BigQueryTable {
    /// A table reference (not validated here; queries validate it).
    pub fn new(
        project: impl Into<String>,
        dataset: impl Into<String>,
        table: impl Into<String>,
    ) -> Self {
        Self {
            project: project.into(),
            dataset: dataset.into(),
            table: table.into(),
        }
    }

    /// Parses `bigquery://<project>.<dataset>.<table>`, splitting from the right so a
    /// domain-scoped project (`example.com:proj`) keeps its dots.
    ///
    /// # Errors
    ///
    /// A message naming the expected form when the scheme, the parts or their characters are
    /// wrong.
    pub fn parse_url(s: &str) -> Result<Self, String> {
        let bad = || format!("expected {URL_FORM}, got {s:?}");
        let rest = s.strip_prefix("bigquery://").ok_or_else(bad)?;
        let mut parts = rest.rsplitn(3, '.');
        let (Some(table), Some(dataset), Some(project)) =
            (parts.next(), parts.next(), parts.next())
        else {
            return Err(bad());
        };
        let t = Self::new(project, dataset, table);
        if t.is_valid() { Ok(t) } else { Err(bad()) }
    }

    /// Whether every part matches the allowed characters (see `is_project`, `is_dataset_or_table`).
    fn is_valid(&self) -> bool {
        is_project(&self.project)
            && is_dataset_or_table(&self.dataset)
            && is_dataset_or_table(&self.table)
    }

    /// The backquoted SQL path `` `project.dataset.table` ``.
    pub(crate) fn sql_path(&self) -> Result<String, SourceError> {
        if self.is_valid() {
            Ok(format!(
                "`{}.{}.{}`",
                self.project, self.dataset, self.table
            ))
        } else {
            Err(SourceError::BigQuery {
                status: None,
                reason: None,
                detail: format!(
                    "invalid table {}.{}.{}",
                    self.project, self.dataset, self.table
                ),
            })
        }
    }
}

/// `[a-z][a-z0-9-]{4,28}[a-z0-9]`, optionally prefixed by a domain `[a-z0-9.-]+:`.
fn is_project(s: &str) -> bool {
    let id = match s.rsplit_once(':') {
        Some((domain, id)) => {
            if domain.is_empty()
                || !domain
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
            {
                return false;
            }
            id
        }
        None => s,
    };
    let b = id.as_bytes();
    (6..=30).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && (b[b.len() - 1].is_ascii_lowercase() || b[b.len() - 1].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// `[A-Za-z0-9_]{1,1024}`.
fn is_dataset_or_table(s: &str) -> bool {
    (1..=1024).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Whether an event type names a span: `USER_MESSAGE_RECEIVED` or the `INVOCATION_`, `AGENT_`,
/// `LLM_` and `TOOL_` families.
#[allow(dead_code)] // used by BigQueryTraceSource (Tasks 3-4)
pub(crate) fn is_named_family(event_type: &str) -> bool {
    event_type == "USER_MESSAGE_RECEIVED"
        || ["INVOCATION_", "AGENT_", "LLM_", "TOOL_"]
            .iter()
            .any(|p| event_type.starts_with(p))
}

/// The span name for a row: `invocation`, `invoke_agent <agent>`, `call_llm <model>`,
/// `execute_tool <tool>`, else the event type lowercased. A missing or empty suffix is dropped.
pub(crate) fn span_name(
    event_type: &str,
    agent: Option<&str>,
    model: Option<&str>,
    tool: Option<&str>,
) -> String {
    let (base, suffix) =
        if event_type == "USER_MESSAGE_RECEIVED" || event_type.starts_with("INVOCATION_") {
            ("invocation", None)
        } else if event_type.starts_with("AGENT_") {
            ("invoke_agent", agent)
        } else if event_type.starts_with("LLM_") {
            ("call_llm", model)
        } else if event_type.starts_with("TOOL_") {
            ("execute_tool", tool)
        } else {
            return event_type.to_ascii_lowercase();
        };
    match suffix.filter(|s| !s.is_empty()) {
        Some(s) => format!("{base} {s}"),
        None => base.to_owned(),
    }
}

/// A `gadk-bigquery` error as a trace-source error.
pub(crate) fn from_bigquery(e: BigQueryError) -> SourceError {
    match e {
        BigQueryError::Api {
            status,
            message,
            reason,
        } => SourceError::BigQuery {
            status: Some(status),
            reason,
            detail: message,
        },
        BigQueryError::Credentials(text) => SourceError::Credentials(text),
        BigQueryError::Http(text) | BigQueryError::Job(text) | BigQueryError::Decode(text) => {
            SourceError::BigQuery {
                status: None,
                reason: None,
                detail: text,
            }
        }
        other => SourceError::BigQuery {
            status: None,
            reason: None,
            detail: other.to_string(),
        },
    }
}

/// Traces listed per page.
pub(crate) const PAGE_SIZE: usize = 50;

/// The root-name expression of the list query: the SQL form of [`span_name`] over the root row.
const ROOT_NAME_SQL: &str = "CASE \
  WHEN root.event_type = 'USER_MESSAGE_RECEIVED' OR STARTS_WITH(root.event_type, 'INVOCATION_') THEN 'invocation' \
  WHEN STARTS_WITH(root.event_type, 'AGENT_') THEN IF(IFNULL(root.agent, '') = '', 'invoke_agent', CONCAT('invoke_agent ', root.agent)) \
  WHEN STARTS_WITH(root.event_type, 'LLM_') THEN IF(IFNULL(root.model, '') = '', 'call_llm', CONCAT('call_llm ', root.model)) \
  WHEN STARTS_WITH(root.event_type, 'TOOL_') THEN IF(IFNULL(root.tool, '') = '', 'execute_tool', CONCAT('execute_tool ', root.tool)) \
  ELSE LOWER(root.event_type) END";

/// Traces from the agent-analytics BigQuery table. Every query runs (and is billed) in the
/// compute project and stops at the byte cap.
#[non_exhaustive]
pub struct BigQueryTraceSource {
    client: BigQueryClient,
    compute_project: String,
    table: BigQueryTable,
    max_bytes_billed: i64,
    pub(crate) now: fn() -> DateTime<Utc>,
}

impl std::fmt::Debug for BigQueryTraceSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BigQueryTraceSource")
            .field("compute_project", &self.compute_project)
            .field("table", &self.table)
            .field("max_bytes_billed", &self.max_bytes_billed)
            .finish_non_exhaustive()
    }
}

impl BigQueryTraceSource {
    /// A source reading `table`, running queries in `compute_project` with Application Default
    /// Credentials (built on the first call), each capped at `max_bytes_billed` bytes.
    pub fn new(
        compute_project: impl Into<String>,
        table: BigQueryTable,
        max_bytes_billed: i64,
    ) -> Self {
        Self::with_client(
            BigQueryClient::new(),
            compute_project,
            table,
            max_bytes_billed,
        )
    }

    /// [`new`](Self::new) with an explicit BigQuery client.
    pub fn with_client(
        client: BigQueryClient,
        compute_project: impl Into<String>,
        table: BigQueryTable,
        max_bytes_billed: i64,
    ) -> Self {
        Self {
            client,
            compute_project: compute_project.into(),
            table,
            max_bytes_billed,
            now: Utc::now,
        }
    }

    /// The options of every query: the byte cap and `params`.
    pub(crate) fn options(&self, params: Vec<Value>) -> QueryOptions {
        QueryOptions::new()
            .with_maximum_bytes_billed(self.max_bytes_billed)
            .with_query_parameters(params)
    }

    /// Runs `sql` in the compute project.
    pub(crate) async fn run(
        &self,
        sql: &str,
        opts: &QueryOptions,
    ) -> Result<gadk_bigquery::QueryResult, SourceError> {
        self.client
            .query(&self.compute_project, sql, opts)
            .await
            .map_err(from_bigquery)
    }
}

/// A named query parameter; `None` is SQL NULL (`parameterValue` without `value`).
pub(crate) fn param(name: &str, ty: &str, value: Option<String>) -> Value {
    let value = match value {
        Some(v) => json!({ "value": v }),
        None => json!({}),
    };
    json!({ "name": name, "parameterType": { "type": ty }, "parameterValue": value })
}

/// A TIMESTAMP parameter value: `%Y-%m-%d %H:%M:%S%.6f+00:00`.
pub(crate) fn ts_param(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%d %H:%M:%S%.6f+00:00").to_string()
}

/// A non-empty STRING cell.
pub(crate) fn text(row: &Map<String, Value>, key: &str) -> Option<String> {
    row.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// A TIMESTAMP cell (RFC 3339 text from `gadk-bigquery`).
pub(crate) fn timestamp(row: &Map<String, Value>, key: &str) -> Result<DateTime<Utc>, SourceError> {
    row.get(key)
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
        .ok_or_else(|| SourceError::BigQuery {
            status: None,
            reason: None,
            detail: format!("row without a valid {key}"),
        })
}

fn list_sql(table: &str) -> String {
    format!(
        "WITH t AS (\n\
  SELECT trace_id,\n\
    ARRAY_AGG(STRUCT(timestamp, event_type, agent, session_id,\n\
                     JSON_VALUE(attributes, '$.model') AS model,\n\
                     JSON_VALUE(content, '$.tool') AS tool)\n\
              ORDER BY IF(REGEXP_CONTAINS(event_type,\n\
                            r'^(USER_MESSAGE_RECEIVED|INVOCATION_|AGENT_|LLM_|TOOL_)'), 0, 1),\n\
                       timestamp, event_type LIMIT 1)[OFFSET(0)] AS root,\n\
    MIN(timestamp) AS first_ts,\n\
    MAX(timestamp) AS last_ts\n\
  FROM {table}\n\
  WHERE timestamp >= TIMESTAMP_SUB(@start, INTERVAL 1 DAY)\n\
    AND timestamp < TIMESTAMP_ADD(@end, INTERVAL 1 HOUR)\n\
    AND trace_id IS NOT NULL AND trace_id != ''\n\
  GROUP BY trace_id),\n\
r AS (\n\
  SELECT trace_id, first_ts, last_ts, root.event_type AS event_type, root.agent AS agent,\n\
    root.session_id AS session_id, root.model AS model, root.tool AS tool,\n\
    {ROOT_NAME_SQL} AS root_name\n\
  FROM t)\n\
SELECT trace_id, first_ts AS start, last_ts, event_type, agent, session_id, model, tool\n\
FROM r\n\
WHERE first_ts >= @start AND first_ts < @end\n\
  AND (@agent IS NULL OR agent = @agent)\n\
  AND (@session IS NULL OR session_id = @session)\n\
  AND (@name IS NULL OR STARTS_WITH(root_name, @name))\n\
ORDER BY start DESC, trace_id\n\
LIMIT @limit OFFSET @offset"
    )
}

#[async_trait::async_trait]
impl TraceSource for BigQueryTraceSource {
    async fn list(&self, query: &TraceQuery) -> Result<TracePage, SourceError> {
        let offset = match query.page_token.as_deref() {
            None => 0,
            Some(t) => t.parse::<u64>().map_err(|_| SourceError::BigQuery {
                status: None,
                reason: None,
                detail: "bad page token".into(),
            })?,
        };
        let sql = list_sql(&self.table.sql_path()?);
        let end = (self.now)();
        let start = end - query.range.duration();
        let params = vec![
            param("start", "TIMESTAMP", Some(ts_param(start))),
            param("end", "TIMESTAMP", Some(ts_param(end))),
            param("name", "STRING", query.name.clone()),
            param("agent", "STRING", query.agent.clone()),
            param("session", "STRING", query.session.clone()),
            param("limit", "INT64", Some((PAGE_SIZE + 1).to_string())),
            param("offset", "INT64", Some(offset.to_string())),
        ];
        let result = self.run(&sql, &self.options(params)).await?;
        let more = result.rows.len() > PAGE_SIZE;
        let mut traces = Vec::new();
        for row in result.rows.iter().take(PAGE_SIZE) {
            let Some(trace_id) = row
                .get("trace_id")
                .and_then(Value::as_str)
                .and_then(TraceId::parse)
            else {
                continue;
            };
            let start = timestamp(row, "start")?;
            let last = timestamp(row, "last_ts")?;
            let event_type = text(row, "event_type").unwrap_or_default();
            let agent = text(row, "agent");
            traces.push(TraceSummary {
                trace_id,
                root_name: span_name(
                    &event_type,
                    agent.as_deref(),
                    text(row, "model").as_deref(),
                    text(row, "tool").as_deref(),
                ),
                start,
                duration_ms: (last - start).num_microseconds().unwrap_or(0) as f64 / 1000.0,
                agent,
                session: text(row, "session_id"),
            });
        }
        Ok(TracePage {
            traces,
            next: more.then(|| (offset + PAGE_SIZE as u64).to_string()),
        })
    }

    async fn get(&self, _id: &TraceId) -> Result<super::TraceDetail, SourceError> {
        // Task 4 replaces this.
        Err(SourceError::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::{DateTime, TimeZone, Utc};
    use gadk_bigquery::{BigQueryClient, BigQueryError, NoAuth};
    use pretty_assertions::assert_eq;
    use serde_json::{Value, json};

    use super::*;
    use crate::traces::test_http::FakeHttp;
    use crate::traces::{SourceError, TimeRange, TraceQuery, TraceSource};

    const QUERIES: &str =
        "https://bigquery.googleapis.com/bigquery/v2/projects/billing-proj/queries";

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 20, 12, 0, 0).unwrap()
    }

    /// Microseconds since the epoch, as `useInt64Timestamp` sends TIMESTAMP cells.
    fn micros(t: DateTime<Utc>) -> String {
        t.timestamp_micros().to_string()
    }

    /// A completed `jobs.query` reply with `fields` (name, type) and `rows` (one JSON per cell).
    fn reply(
        fields: &[(&str, &str)],
        rows: Vec<Vec<Value>>,
        page_token: bool,
    ) -> Result<(u16, String), String> {
        let schema: Vec<Value> = fields
            .iter()
            .map(|(n, t)| json!({ "name": n, "type": t, "mode": "NULLABLE" }))
            .collect();
        let rows: Vec<Value> = rows
            .into_iter()
            .map(|cells| json!({ "f": cells.into_iter().map(|v| json!({ "v": v })).collect::<Vec<_>>() }))
            .collect();
        let mut body = json!({
            "jobComplete": true,
            "jobReference": { "projectId": "billing-proj", "jobId": "job_1" },
            "schema": { "fields": schema },
            "rows": rows,
        });
        if page_token {
            body["pageToken"] = json!("next");
        }
        Ok((200, body.to_string()))
    }

    fn source(fake: &Arc<FakeHttp>) -> BigQueryTraceSource {
        let client = BigQueryClient::with_client(fake.clone(), Arc::new(NoAuth));
        let mut s = BigQueryTraceSource::with_client(
            client,
            "billing-proj",
            BigQueryTable::new("data-proj", "agent_analytics", "agent_events"),
            1_000,
        );
        s.now = now;
        s
    }

    const LIST_FIELDS: [(&str, &str); 8] = [
        ("trace_id", "STRING"),
        ("start", "TIMESTAMP"),
        ("last_ts", "TIMESTAMP"),
        ("event_type", "STRING"),
        ("agent", "STRING"),
        ("session_id", "STRING"),
        ("model", "STRING"),
        ("tool", "STRING"),
    ];

    fn trace_id(i: usize) -> String {
        format!("{i:032x}")
    }

    fn list_row(i: usize, event: &str) -> Vec<Value> {
        let start = now() - chrono::TimeDelta::minutes(i as i64);
        vec![
            json!(trace_id(i)),
            json!(micros(start)),
            json!(micros(start + chrono::TimeDelta::milliseconds(1500))),
            json!(event),
            json!("weather_bot"),
            json!("s-1"),
            Value::Null,
            Value::Null,
        ]
    }

    fn param_value(body: &Value, name: &str) -> Value {
        body["queryParameters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == json!(name))
            .unwrap_or_else(|| panic!("no parameter {name}: {body}"))
            .clone()
    }

    #[tokio::test]
    async fn list_maps_rows_and_pages_by_offset() {
        let rows: Vec<Vec<Value>> = (1..=51)
            .map(|i| list_row(i, "USER_MESSAGE_RECEIVED"))
            .collect();
        let fake = FakeHttp::new(vec![reply(&LIST_FIELDS, rows, false)]);
        let page = source(&fake).list(&TraceQuery::default()).await.unwrap();
        assert_eq!(page.traces.len(), 50);
        assert_eq!(page.next.as_deref(), Some("50"));
        let first = &page.traces[0];
        assert_eq!(first.trace_id.as_str(), trace_id(1));
        assert_eq!(first.root_name, "invocation");
        assert_eq!(first.start, now() - chrono::TimeDelta::minutes(1));
        assert_eq!(first.duration_ms, 1500.0);
        assert_eq!(first.agent.as_deref(), Some("weather_bot"));
        assert_eq!(first.session.as_deref(), Some("s-1"));

        assert_eq!(fake.uris(), [QUERIES]);
        let body = fake.body(0);
        assert_eq!(body["maximumBytesBilled"], json!("1000"));
        assert_eq!(body["parameterMode"], json!("NAMED"));
        let sql = body["query"].as_str().unwrap();
        assert!(
            sql.contains("FROM `data-proj.agent_analytics.agent_events`"),
            "{sql}"
        );
        assert_eq!(
            param_value(&body, "start")["parameterValue"]["value"],
            json!("2026-11-20 11:00:00.000000+00:00")
        );
        assert_eq!(
            param_value(&body, "end")["parameterValue"]["value"],
            json!("2026-11-20 12:00:00.000000+00:00")
        );
        assert_eq!(
            param_value(&body, "limit")["parameterValue"]["value"],
            json!("51")
        );
        assert_eq!(
            param_value(&body, "offset")["parameterValue"]["value"],
            json!("0")
        );
        assert_eq!(param_value(&body, "agent")["parameterValue"], json!({}));
    }

    #[tokio::test]
    async fn list_last_page_has_no_next_and_skips_bad_ids() {
        let mut rows: Vec<Vec<Value>> = (1..=3).map(|i| list_row(i, "AGENT_STARTING")).collect();
        rows[1][0] = json!("not-a-trace-id");
        let fake = FakeHttp::new(vec![reply(&LIST_FIELDS, rows, false)]);
        let q = TraceQuery {
            page_token: Some("50".into()),
            ..TraceQuery::default()
        };
        let page = source(&fake).list(&q).await.unwrap();
        assert_eq!(page.next, None);
        assert_eq!(page.traces.len(), 2);
        assert_eq!(page.traces[0].root_name, "invoke_agent weather_bot");
        assert_eq!(
            param_value(&fake.body(0), "offset")["parameterValue"]["value"],
            json!("50")
        );
    }

    #[tokio::test]
    async fn list_filters_are_parameters_not_sql() {
        let fake = FakeHttp::new(vec![reply(&LIST_FIELDS, vec![], false)]);
        let q = TraceQuery {
            range: TimeRange::Days7,
            name: Some("invoke_agent x'; --".into()),
            agent: Some("bot\"name".into()),
            session: Some("sess`1".into()),
            page_token: None,
        };
        source(&fake).list(&q).await.unwrap();
        let body = fake.body(0);
        let sql = body["query"].as_str().unwrap();
        for value in ["invoke_agent x'; --", "bot\"name", "sess`1"] {
            assert!(!sql.contains(value), "{value} leaked into SQL: {sql}");
        }
        assert_eq!(
            param_value(&body, "name")["parameterValue"]["value"],
            json!("invoke_agent x'; --")
        );
        assert_eq!(
            param_value(&body, "agent")["parameterValue"]["value"],
            json!("bot\"name")
        );
        assert_eq!(
            param_value(&body, "session")["parameterValue"]["value"],
            json!("sess`1")
        );
        assert_eq!(
            param_value(&body, "start")["parameterValue"]["value"],
            json!("2026-11-13 12:00:00.000000+00:00")
        );
    }

    #[tokio::test]
    async fn list_rejects_a_bad_page_token_without_querying() {
        let fake = FakeHttp::new(vec![]);
        for token in ["x", "-1", "1.5"] {
            let q = TraceQuery {
                page_token: Some(token.into()),
                ..TraceQuery::default()
            };
            let err = source(&fake).list(&q).await.unwrap_err();
            assert_eq!(
                err,
                SourceError::BigQuery {
                    status: None,
                    reason: None,
                    detail: "bad page token".into()
                }
            );
        }
        assert!(fake.uris().is_empty());
    }

    #[tokio::test]
    async fn list_maps_bigquery_failures() {
        let fake = FakeHttp::new(vec![Ok((
            403,
            json!({ "error": { "code": 403, "message": "Access Denied", "errors": [{ "reason": "accessDenied" }] } }).to_string(),
        ))]);
        let err = source(&fake)
            .list(&TraceQuery::default())
            .await
            .unwrap_err();
        assert_eq!(
            err,
            SourceError::BigQuery {
                status: Some(403),
                reason: Some("accessDenied".into()),
                detail: "Access Denied".into()
            }
        );
    }

    #[test]
    fn parse_url_accepts_plain_and_domain_scoped_projects() {
        assert_eq!(
            BigQueryTable::parse_url("bigquery://sirhco-apps.agent_analytics.agent_events"),
            Ok(BigQueryTable::new(
                "sirhco-apps",
                "agent_analytics",
                "agent_events"
            ))
        );
        assert_eq!(
            BigQueryTable::parse_url("bigquery://example.com:proj-1.ds.tbl"),
            Ok(BigQueryTable::new("example.com:proj-1", "ds", "tbl"))
        );
    }

    #[test]
    fn parse_url_rejects_malformed_values() {
        for bad in [
            "",
            "sirhco-apps.ds.tbl",
            "bq://sirhco-apps.ds.tbl",
            "bigquery://sirhco-apps.ds",
            "bigquery://.ds.tbl",
            "bigquery://Sirhco-apps.ds.tbl",
            "bigquery://abc.ds.tbl",
            "bigquery://sirhco-apps.d-s.tbl",
            "bigquery://sirhco-apps.ds.t`bl",
            "bigquery://sirhco-apps.ds.",
            "bigquery://EXAMPLE.com:proj-1.ds.tbl",
        ] {
            let err = BigQueryTable::parse_url(bad).unwrap_err();
            assert!(
                err.contains("bigquery://<project>.<dataset>.<table>"),
                "{bad}: {err}"
            );
        }
    }

    #[test]
    fn sql_path_backquotes_valid_tables_and_rejects_others() {
        assert_eq!(
            BigQueryTable::new("example.com:proj-1", "ds", "tbl").sql_path(),
            Ok("`example.com:proj-1.ds.tbl`".to_owned())
        );
        let err = BigQueryTable::new("p", "ds", "t`; DROP")
            .sql_path()
            .unwrap_err();
        assert_eq!(
            err,
            SourceError::BigQuery {
                status: None,
                reason: None,
                detail: "invalid table p.ds.t`; DROP".into()
            }
        );
    }

    #[test]
    fn span_names_follow_the_event_family() {
        let cases = [
            ("USER_MESSAGE_RECEIVED", "invocation"),
            ("INVOCATION_STARTING", "invocation"),
            ("INVOCATION_COMPLETED", "invocation"),
            ("AGENT_STARTING", "invoke_agent weather_bot"),
            ("LLM_REQUEST", "call_llm gemini-3.8-flash"),
            ("TOOL_STARTING", "execute_tool get_weather"),
            ("STATE_DELTA", "state_delta"),
        ];
        for (event, want) in cases {
            assert_eq!(
                span_name(
                    event,
                    Some("weather_bot"),
                    Some("gemini-3.8-flash"),
                    Some("get_weather")
                ),
                want,
                "{event}"
            );
        }
        assert_eq!(
            span_name("AGENT_COMPLETED", None, None, None),
            "invoke_agent"
        );
        assert_eq!(span_name("LLM_RESPONSE", None, Some(""), None), "call_llm");
        assert_eq!(span_name("TOOL_ERROR", None, None, None), "execute_tool");
        assert!(is_named_family("USER_MESSAGE_RECEIVED"));
        assert!(is_named_family("TOOL_COMPLETED"));
        assert!(!is_named_family("STATE_DELTA"));
    }

    #[test]
    fn bigquery_errors_map_to_source_errors() {
        assert_eq!(
            from_bigquery(BigQueryError::Api {
                status: 403,
                message: "Access Denied".into(),
                reason: Some("accessDenied".into()),
            }),
            SourceError::BigQuery {
                status: Some(403),
                reason: Some("accessDenied".into()),
                detail: "Access Denied".into(),
            }
        );
        assert_eq!(
            from_bigquery(BigQueryError::Credentials("no ADC".into())),
            SourceError::Credentials("no ADC".into())
        );
        for e in [
            BigQueryError::Http("reset".into()),
            BigQueryError::Job(
                "Query exceeded limit for bytes billed: 10. 20 or higher required.".into(),
            ),
            BigQueryError::Decode("no rows".into()),
        ] {
            let text = match &e {
                BigQueryError::Http(t) | BigQueryError::Job(t) | BigQueryError::Decode(t) => {
                    t.clone()
                }
                _ => unreachable!(),
            };
            assert_eq!(
                from_bigquery(e),
                SourceError::BigQuery {
                    status: None,
                    reason: None,
                    detail: text
                }
            );
        }
    }
}
